#!/usr/bin/env python3
"""The yardstick: each workload W1 to W10 against its best rival, every answer checked first.

    python3 bench/yardstick.py --corpus DIR --queries FILE --out FILE.json [--runs 20] [--only W3,W4]
    python3 bench/yardstick.py --smoke          # the tiny fixture: scrutate's rows must pass

A tool is an Adapter (bench/rivals): one request per `ask`. A query names its workload and what
the answer must hold (`expect`, or exactly that with `exact: true`). An answer that does not hold
it is reported as wrong and never timed; a tool that cannot run here says why. Passing tools are
timed in turn (A B C, B C A, ...), so load drift does not turn into a difference between them, and
each is compared with scrutate's adapter of the same form: resident with resident (a server that
holds its index), process with process (a program per request).
"""
from __future__ import annotations

import dataclasses
import math
import statistics
import time

import leaderboard as lb
from rivals import Unsupported


def check(workload: str, answer, query: dict) -> str | None:
    """None when `answer` holds what `query` expects, else what is wrong with it."""
    got = {tuple(x) if isinstance(x, (list, tuple)) else x for x in answer}
    want = {tuple(x) if isinstance(x, (list, tuple)) else x for x in query.get("expect", [])}
    if want - got:
        return f"missing {sorted(want - got, key=str)[:3]}"
    if query.get("exact") and got - want:
        return f"extra {sorted(got - want, key=str)[:3]}"
    return None


def rank_in(hits, relevant) -> int | None:
    """1-based rank among the first ten of the first hit in a relevant file whose lines overlap a
    relevant definition's, or None."""
    for i, (path, start, end) in enumerate(list(hits)[:10], 1):
        if any(path == p and start <= e and s <= end for p, s, e in relevant):
            return i
    return None


def measure(adapters: list, queries: list[dict], runs: int, warmup: int, corpus: str, work: str) -> dict:
    """Check, then time, every adapter on every query of a workload it serves."""
    tools: dict = {}
    started = []
    for a in adapters:
        why = a.missing()
        tools[a.name] = {"form": a.form, "missing": why}
        if why:
            continue
        tools[a.name]["version"] = a.version()
        try:
            tools[a.name]["setup"] = dataclasses.asdict(a.start(corpus, work))
            started.append(a)
        except Exception as e:  # noqa: BLE001 - a tool that cannot start is reported, the run goes on
            tools[a.name]["error"] = f"{type(e).__name__}: {e}"
    rows: list[dict] = []
    try:
        for q in queries:
            w = q["workload"]
            if "grep" in q and "expect" not in q:
                # a grep query is checked against leaderboard.py's Python reference, line for line
                found, _ = lb.reference(corpus, lb.file_list(corpus, q.get("mode", "ignore")), q["grep"])
                q = {**q, "expect": sorted(found), "exact": True}
            live = []
            for a in (a for a in adapters if w in a.workloads and (a.languages is None or q.get("language") in a.languages)):
                row = {"workload": w, "query": q["id"], "tool": a.name, "form": a.form}
                rows.append(row)
                if tools[a.name]["missing"]:
                    row["status"] = f"missing: {tools[a.name]['missing']}"
                elif a not in started:
                    row["status"] = f"error: {tools[a.name]['error']}"
                else:
                    try:
                        answer = a.ask(w, q)
                        if w == "W8":
                            # ranked, not right or wrong: the rank of the first relevant hit is the result
                            row["rank"], bad = rank_in(answer, q["relevant"]), None
                        else:
                            bad = check(w, answer, q)
                        row["status"] = f"wrong: {bad}" if bad else "ok"
                    except Unsupported as e:
                        row["status"] = f"missing: {e}"
                    except Exception as e:  # noqa: BLE001
                        row["status"] = f"error: {e}"
                    if row["status"] == "ok":
                        live.append((a, row))
            for i in range(warmup + runs):
                k = i % len(live) if live else 0
                for a, row in live[k:] + live[:k]:
                    if row["status"] != "ok":
                        continue
                    t0 = time.perf_counter()
                    try:
                        a.ask(w, q)
                    except Exception as e:  # noqa: BLE001
                        row["status"] = f"error: {e}"
                        row.pop("times", None)
                        continue
                    if i >= warmup:
                        row.setdefault("times", []).append(time.perf_counter() - t0)
            for _, row in live:
                if row.get("times"):
                    row["p50_ms"] = 1000 * statistics.median(row["times"])
                    row["p95_ms"] = 1000 * sorted(row["times"])[max(0, math.ceil(0.95 * len(row["times"])) - 1)]
            ours = {row["form"]: row for _, row in live if row["tool"].startswith("scrutate") and row.get("times")}
            for _, row in live:
                base = ours.get(row["form"])
                if base and base is not row and row.get("times"):
                    row["ratio"], row["ci_low"], row["ci_high"] = lb.boot_ratio(row["times"], base["times"], B=2000)
    finally:
        for a in started:
            a.stop()
    return {"tools": tools, "rows": rows}


def summary(rows: list[dict]) -> dict:
    """Per workload: the rival whose time against scrutate's (geometric mean over the queries) is
    lowest among those that answered every query right, or why there is none."""
    out: dict = {}
    for w in sorted({r["workload"] for r in rows}):
        mine = [r for r in rows if r["workload"] == w and not r["tool"].startswith("scrutate")]
        good = {}
        for tool in {r["tool"] for r in mine}:
            rs = [r for r in mine if r["tool"] == tool]
            if all(r["status"] == "ok" and "ratio" in r for r in rs):
                good[tool] = (math.exp(statistics.mean(math.log(r["ratio"]) for r in rs)), rs[0]["form"])
        if good:
            tool, (ratio, form) = min(good.items(), key=lambda kv: kv[1][0])
            out[w] = {"rival": tool, "form": form, "ratio": ratio}
        else:
            # a right rival with no ratio had no scrutate row of its form to be timed against
            forms = {r["form"] for r in mine if r["status"] == "ok" and "ratio" not in r}
            ours = sorted({f"{r['tool']} {r['status']}" for r in rows if r["workload"] == w and r["tool"].startswith("scrutate")
                           and r["form"] in forms and r["status"] != "ok"})
            seen = sorted({f"{r['tool']} {r['status']}" for r in mine})
            out[w] = {"reason": "; ".join(ours) + ", so no rival has a time to compare" if ours
                      else "no rival answered right: " + ("; ".join(seen) if seen else "none adapted")}
        if w == "W8":
            import evalcore  # numpy: only a semantic run needs it

            ranked = [r for r in rows if r["workload"] == w and r["status"] == "ok"]
            out[w]["quality"] = {t: {"mrr10": round(s["MRR"], 4), "hit5": round(s["hit@5"], 4)}
                                 for t in dict.fromkeys(r["tool"] for r in ranked)
                                 for s in [evalcore.summarize([r["rank"] for r in ranked if r["tool"] == t])]}
    return out


def passed(out: dict) -> bool:
    """Every scrutate form ran, and answered right whatever it can express (a query it cannot,
    such as the language server's callers, is missing, not failed)."""
    ours = {t: v for t, v in out["tools"].items() if t.startswith("scrutate")}
    return (all(not v["missing"] and "error" not in v for v in ours.values())
            and all(r["status"] == "ok" or r["status"].startswith("missing") for r in out["rows"] if r["tool"] in ours))


def adapters(binary: str | None = None) -> list:
    """Every adapter: scrutate's forms first, then the rivals (each says why when it cannot run)."""
    from rivals import index, lsp, scrutate, semantic, text
    b = binary or lb.SCRUTATE

    def server(name, cmd, version, languages, opens=()):
        s = lsp.Lsp(name, cmd, version, opens=opens)
        s.languages = frozenset(languages)
        return s

    return [
        scrutate.Cli(b), scrutate.Mcp(b), scrutate.McpCold(b), lsp.Lsp("scrutate-lsp", [b, "lsp"], [b, "--version"], calls=False),
        *(text.Grep(t) for t in ("ripgrep", "ugrep", "GNU grep", "git grep")),
        index.ZoektWeb(), index.LivegrepWeb(), semantic.Ck(), semantic.Grepai(), semantic.ClaudeContext(),
        text.Ctags(), text.AstGrep(), text.EmptyProgram(),
        server("rust-analyzer", ["rust-analyzer"], ["rust-analyzer", "--version"], {"rust"}),
        server("pyright", ["pyright-langserver", "--stdio"], ["pyright", "--version"], {"python"}),
        server("clangd", ["clangd"], ["clangd", "--version"], {"c", "cpp"}, opens=(".c", ".cc", ".cpp")),
        server("gopls", ["gopls"], ["gopls", "version"], {"go"}),
    ]


def quality_queries(n: int, corpus: str, binary: str) -> list[dict]:
    """`n` of bench/quality/queries.json (a fixed sample), each with the definitions it asks for
    (path, start, end) as scrutate's index of the frozen corpus places them; a query whose target
    the index does not hold is left out."""
    import json
    import random
    import subprocess

    import evalcore
    import quality

    with open(quality.QUERIES) as f:
        qs = json.load(f)
    out = []
    for q in sorted(random.Random(9).sample(qs, min(n, len(qs))), key=qs.index):
        look = q["qualname"] if q["match"] != "name" else q["name"]
        res = json.loads(subprocess.run([binary, "--root", corpus, "symbol", "--json", "--limit", "500", look], capture_output=True, text=True).stdout or "{}")
        spans = [[h["path"], h["start"], h["end"]] for h in res.get("result", [])
                 if evalcore.is_relevant(q, h["path"], h["name"], h["name"].rsplit(".", 1)[-1], h["kind"])]
        if spans:
            out.append({"id": f"W8/{q['type']} {q['target_kind']}: {q['query'][:60]}", "workload": "W8", "query": q["query"], "relevant": spans})
    return out


def main() -> int:
    import argparse
    import json
    import os
    import shutil
    import subprocess
    import sys
    import tempfile

    lb.drop_scrutate_settings()
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--smoke", action="store_true", help="the tiny fixture in bench/yardstick_smoke; exit 1 unless scrutate's rows pass")
    ap.add_argument("--corpus")
    ap.add_argument("--queries")
    ap.add_argument("--out")
    ap.add_argument("--runs", type=int, default=20)
    ap.add_argument("--warmup", type=int, default=2)
    ap.add_argument("--only", help="workloads, comma-separated (W3,W4)")
    ap.add_argument("--quality", type=int, metavar="N", help="W8 alone: N of the QUALITY queries on the frozen corpus (bench/quality)")
    a = ap.parse_args()
    if a.smoke:
        a.queries, a.runs, a.warmup = os.path.join(here, "yardstick_smoke", "queries.json"), 3, 1
    elif not (a.corpus and a.queries) and not a.quality:
        ap.error("give --corpus and --queries, --quality N, or --smoke")
    queries = json.load(open(a.queries)) if a.queries else []
    if a.only:
        queries = [q for q in queries if q["workload"] in a.only.split(",")]
    with tempfile.TemporaryDirectory() as work:
        corpus = a.corpus
        if a.quality:
            import quality

            corpus = os.path.join(work, "quality")
            quality.extract(corpus)
            subprocess.run([lb.SCRUTATE, "--root", corpus, "index", "--no-text"], capture_output=True, check=True)
            queries = quality_queries(a.quality, corpus, lb.SCRUTATE)
        if a.smoke:
            # a copy: the tools write their indexes into the tree they search
            corpus = os.path.join(work, "tree")
            shutil.copytree(os.path.join(here, "yardstick_smoke", "tree"), corpus)
            # clangd indexes the files a compilation database lists, by absolute path
            with open(os.path.join(corpus, "compile_commands.json"), "w") as f:
                json.dump([{"directory": os.path.realpath(corpus), "file": "point.c", "command": "cc -c point.c"}], f)
        out = measure(adapters(), queries, a.runs, a.warmup, os.path.realpath(corpus), work)
    # environment() hashes the binary; without one, the scrutate rows say missing and passed() fails the run
    out["environment"] = lb.environment() if os.path.isfile(lb.SCRUTATE) else {"scrutate": f"missing: {lb.SCRUTATE}"}
    # the pinned rivals' versions, as bench/competitors/install_rivals.sh wrote them
    rivals = os.path.join(os.path.dirname(shutil.which("ck") or shutil.which("ctags") or "/nonexistent/bin/x"), "..", "versions.json")
    if os.path.exists(rivals):
        out["environment"]["rivals"] = json.load(open(rivals))
    out["summary"] = summary(out["rows"])
    for r in out["rows"]:
        print(f"{r['workload']:4} {r['query'][:34]:34} {r['tool']:18} {r['form']:8} {r['status'][:60]:60} {r.get('p50_ms', float('nan')):9.2f} ms", file=sys.stderr)
    for w, s in out["summary"].items():
        print(f"{w}: " + (f"best rival {s['rival']} ({s['form']}), {s['ratio']:.2f}x scrutate's time" if "rival" in s else s["reason"]), file=sys.stderr)
    if a.out:
        with open(a.out, "w") as f:
            json.dump(out, f, indent=1)
    return 0 if passed(out) else 1


if __name__ == "__main__":
    raise SystemExit(main())
