#!/usr/bin/env python3
"""Ranking quality of a scrutate binary on this repository's own code, against a recorded floor.

usage: quality.py BINARY            measure; exit 1 when a measure is below its floor
       quality.py BINARY --record   measure and write the floor (the measured value less MARGIN)
       quality.py --make            derive the query set from the corpus again

The corpus is a frozen copy of the code under CORPUS with every doc comment and docstring removed
(variant A of bench/make_bench.py and bench/make_bench_code.py), kept in quality/corpus.tar.gz.
The queries are the benchmark's five types made from those doc comments (the first sentence is
the query, the definition it documents is the answer), kept in quality/queries.json. Both are
written by --make and committed, so the measure moves only when the engine does: an edit to the
repository's code does not move it. MRR@10 and hit@5 (recall at 5: one answer per query) are
defined in evalcore.py. After --make (a newer copy of the code), run --record in the same commit.
"""
from __future__ import annotations

import datetime
import gzip
import hashlib
import io
import json
import os
import random
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time

sys.path.insert(0, os.path.dirname(__file__))
from evalcore import first_relevant_rank, summarize  # noqa: E402

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
CORPUS = {"crates/scrutate/src": ".rs", "vendor/ignore/src": ".rs", "bench": ".py", "tools": ".py"}
QUERIES = os.path.join(REPO, "bench", "quality", "queries.json")
FLOOR = os.path.join(REPO, "bench", "quality", "floor.json")
FROZEN = os.path.join(REPO, "bench", "quality", "corpus.tar.gz")
PER_TYPE = 150  # queries of each type and target kind, so a run stays well under two minutes
MARGIN = 0.02  # a floor is the measured value less this


def corpus_files():
    for top, ext in CORPUS.items():
        for dp, dns, fns in os.walk(os.path.join(REPO, top)):
            dns[:] = sorted(d for d in dns if d not in ("__pycache__", "results", "competitors"))
            for fn in sorted(fns):
                if fn.endswith(ext):
                    yield os.path.relpath(os.path.join(dp, fn), REPO).replace(os.sep, "/")


def variant_a(rel: str, src: str, with_targets: bool, chunks: list[dict]):
    """The file with its doc comments removed, and the targets they document."""
    import make_bench
    import make_bench_code

    if rel.endswith(".py"):
        a, _, targets = make_bench.process_file(src, rel, with_targets)
    else:
        a, _, targets = make_bench_code.process_file(src, rel, "rust", with_targets, chunks)
    for t in targets:
        t["path"] = rel
    return a, targets


def make() -> None:
    import make_bench
    import make_bench_code
    from groups import is_test_path

    files = []
    for rel in corpus_files():
        with open(os.path.join(REPO, rel), encoding="utf8") as f:
            files.append((rel, f.read()))
    code = [(rel, src) for rel, src in files if not rel.endswith(".py")]
    chunks = {rel: rec.get("chunks", []) for (rel, _), rec in zip(code, make_bench_code.engine_files(code))}
    targets = []
    os.makedirs(os.path.dirname(FROZEN), exist_ok=True)
    # the same bytes for the same code: no times, owners or compression date in the archive
    with open(FROZEN, "wb") as raw, gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, filename="") as gz, tarfile.open(fileobj=gz, mode="w") as tar:
        for rel, src in files:
            text, found = variant_a(rel, src, not is_test_path(rel), chunks.get(rel, []))
            targets += found
            data = text.encode("utf8")
            info = tarfile.TarInfo(rel)
            info.size, info.mode = len(data), 0o644
            tar.addfile(info, io.BytesIO(data))
    qs = make_bench.make_queries(targets, random.Random(1234), random.Random("types quality"))
    rng = random.Random(7)
    out = []
    for key in dict.fromkeys((q["type"], q["target_kind"]) for q in qs):
        group = [q for q in qs if (q["type"], q["target_kind"]) == key]
        out += sorted(rng.sample(group, min(PER_TYPE, len(group))), key=qs.index)
    os.makedirs(os.path.dirname(QUERIES), exist_ok=True)
    with open(QUERIES, "w") as f:
        json.dump(out, f, indent=0)
        f.write("\n")
    print(f"{len(targets)} targets, {len(qs)} queries, {len(out)} kept in {os.path.relpath(QUERIES, REPO)}; corpus in {os.path.relpath(FROZEN, REPO)}")


class Hit:
    __slots__ = ("path", "qualname", "name", "kind")

    def __init__(self, h: dict):
        # a hit names its definition by the qualified name, or by the name when it has none
        self.path, self.qualname, self.kind = h["path"], h["name"], h["kind"]
        # shortcut: the last dotted part is the name, wrong for a name with a dot in its generics
        self.name = h["name"].rsplit(".", 1)[-1]


def extract(root: str) -> None:
    """The frozen corpus, written under `root`."""
    with tarfile.open(FROZEN) as tar:
        for m in tar.getmembers():
            # the archive is this repository's own, but a path that leaves the folder is still refused
            if not m.isfile() or m.name.startswith("/") or ".." in m.name.split("/"):
                sys.exit(f"{FROZEN}: unexpected member {m.name!r}")
            os.makedirs(os.path.dirname(os.path.join(root, m.name)), exist_ok=True)
            with open(os.path.join(root, m.name), "wb") as f:
                f.write(tar.extractfile(m).read())


def measure(binary: str) -> dict:
    with open(QUERIES) as f:
        qs = json.load(f)
    work = tempfile.mkdtemp(prefix="scrutate_quality_")
    try:
        root = os.path.join(work, "corpus")
        extract(root)
        t0 = time.time()
        env = {k: v for k, v in os.environ.items() if not k.startswith("SCRUTATE_")}
        ix = subprocess.run([binary, "--root", root, "index", "--no-text"], capture_output=True, text=True, env=env)
        if ix.returncode != 0:
            sys.exit(f"index failed: {ix.stderr[-2000:]}")
        qfile = os.path.join(work, "q.jsonl")
        with open(qfile, "w") as f:
            f.write("".join(json.dumps({"query": q["query"], "k": 10}) + "\n" for q in qs))
        r = subprocess.run([binary, "--root", root, "batch", qfile], capture_output=True, text=True, env=env)
        if r.returncode != 0:
            sys.exit(f"batch failed: {r.stderr[-2000:]}")
        recs = [json.loads(line) for line in r.stdout.splitlines()]
        assert len(recs) == len(qs), (len(recs), len(qs))
        db = sqlite3.connect(os.path.join(root, ".scrutate", "index.db"))
        sig = db.execute("SELECT value FROM meta WHERE key = 'model_sig'").fetchone()[0]
        db.close()
        seconds = time.time() - t0
    finally:
        shutil.rmtree(work, ignore_errors=True)
    ranks = [first_relevant_rank(q, [Hit(h) for h in rec["hits"]]) for q, rec in zip(qs, recs)]
    by_type = {}
    for q, rank in zip(qs, ranks):
        by_type.setdefault(f"{q['type']} {q['target_kind']}", []).append(rank)
    s = summarize(ranks)
    return {"queries": len(qs), "mrr10": round(s["MRR"], 4), "hit5": round(s["hit@5"], 4),
            "by_type": {k: round(summarize(v)["MRR"], 4) for k, v in sorted(by_type.items())},
            "model_signature": sig, "seconds": round(seconds, 1)}


def main() -> None:
    a = sys.argv[1:]
    if a == ["--make"]:
        return make()
    binary = os.path.abspath(a[0])
    m = measure(binary)
    print(f"{m['queries']} queries: MRR@10 {m['mrr10']:.4f}, hit@5 {m['hit5']:.4f} ({m['seconds']} s, models {m['model_signature']})")
    for k, v in m["by_type"].items():
        print(f"  {k:<14} MRR@10 {v:.4f}")
    if "--record" in a:
        head = subprocess.run(["git", "-C", REPO, "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
        with open(os.path.join(REPO, "crates", "scrutate", "models", "ranker.npz"), "rb") as f:
            ranker = hashlib.sha256(f.read()).hexdigest()
        rec = {"floor": {"mrr10": round(m["mrr10"] - MARGIN, 4), "hit5": round(m["hit5"] - MARGIN, 4)}, "margin": MARGIN,
               "measured": {"mrr10": m["mrr10"], "hit5": m["hit5"], "by_type": m["by_type"]}, "queries": m["queries"],
               "date": datetime.date.today().isoformat(), "commit": head, "model_signature": m["model_signature"],
               "ranker_sha256": ranker}
        with open(FLOOR, "w") as f:
            json.dump(rec, f, indent=1)
            f.write("\n")
        print(f"floor written to {os.path.relpath(FLOOR, REPO)}: {rec['floor']}")
        return
    with open(FLOOR) as f:
        floor = json.load(f)["floor"]
    low = [f"{k} {m[k]:.4f} < floor {v:.4f}" for k, v in floor.items() if not m[k] >= v]
    if low:
        print("RANKING QUALITY BELOW ITS FLOOR: " + "; ".join(low))
        sys.exit(1)
    print(f"QUALITY OK (floor MRR@10 {floor['mrr10']:.4f}, hit@5 {floor['hit5']:.4f})")


if __name__ == "__main__":
    main()
