#!/usr/bin/env python3
"""Indexed code search benchmark: scrutate's text index (Engine B, `grep --index`) against Zoekt
and livegrep (mission section 14, "Engine B workloads").

usage: indexed.py --corpus DIR --work DIR --out FILE.json [--tools scrutate,zoekt,livegrep]
                  [--runs 20] [--index-runs 5] [--throughput-runs 10] [--concurrency N] [--seconds 15]
                  [--max-load X]

Measured per tool: full indexing time and peak memory, incremental update time (100 files edited),
index size, query latency (p50 / p95 / p99 over every run of every query, process start to exit),
query memory, and concurrent throughput (N clients issuing the query list for a fixed time).
Every query's answer is first checked against the Python reference of bench/leaderboard.py on the
same files; a tool that answers differently, or exits with a code other than 0 or 1, is
disqualified for that query. Every measurement takes the tools in turn (A B, A B, ...), so load
drift on the machine does not turn into a difference between them; each comparison row says how
many runs per tool it rests on.

Like-for-like setup: the corpus is the Linux tree without hidden files (`linux-nodot`, no
`.gitignore` rules), so every engine indexes the same files; Zoekt runs with its size and trigram
limits raised so it does not skip files the others search. Zoekt and scrutate are measured as
command-line clients that open the index for each query; livegrep's design is a server holding the
index in memory, so it is measured through its `lg` client against a running `codesearch`.
"""
from __future__ import annotations

import argparse
import json
import os
import platform
import random
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import leaderboard as lb  # noqa: E402

QUERIES = [
    ("literal", lb.Q("PM_RESUME", fixed=True)),
    ("literal", lb.Q("kernel_thread", fixed=True)),
    ("literal", lb.Q("syzbot", fixed=True)),
    ("literal", lb.Q("EXPORT_SYMBOL_NS_GPL", fixed=True)),
    ("literal", lb.Q("rcu_read_lock_bh", fixed=True)),
    ("literal", lb.Q("__GFP_NOWARN", fixed=True)),
    ("literal", lb.Q("devm_kzalloc", fixed=True)),
    ("literal", lb.Q("sk_buff_head", fixed=True)),
    ("common literal", lb.Q("printk", fixed=True)),
    ("common literal", lb.Q("mutex_lock", fixed=True)),
    ("common literal", lb.Q("spin_lock_irqsave", fixed=True)),
    ("phrase", lb.Q("return -EINVAL;", fixed=True)),
    ("case-insensitive", lb.Q("pm_resume", fixed=True, icase=True)),
    ("case-insensitive", lb.Q("spinlock_t", fixed=True, icase=True)),
    ("regex", lb.Q("[A-Z]+_RESUME")),
    ("regex", lb.Q(r"devm_[a-z]+_alloc")),
    ("regex", lb.Q(r"(suspend|resume)_noirq")),
    ("multiple literals", lb.Q(["ERR_PTR", "IS_ERR_OR_NULL"], fixed=True)),
    ("no match", lb.Q("zqxjkvbw_never_present_symbol", fixed=True)),
    ("no match", lb.Q("qqq_absent_identifier_zzz", fixed=True)),
]

RE2_META = set(r"\.+*?()|[]{}^$")


def re2_escape(s):
    return "".join("\\" + c if c in RE2_META else c for c in s)


ZOEKT = os.environ.get("ZOEKT_BIN_DIR", os.path.expanduser("~/.cache/scrutate-bench/go/bin"))


def tool_defs(a):
    cores = os.cpu_count() or 4
    S = lb.SCRUTATE
    idx = os.path.join(a.work, "zoekt-index")
    lg_idx = os.path.join(a.work, "livegrep.idx")
    return {
        "scrutate": {
            "index": [S, "index", "--text", "--force"],
            "update": [S, "index", "--text"],
            "size_dir": os.path.join(a.corpus, ".scrutate", "text"),
            "clean": lambda: shutil.rmtree(os.path.join(a.corpus, ".scrutate"), ignore_errors=True),
            "query": lambda q: lb.argv("scrutate --index", q, "ignore", a.corpus)[0],
            "version": lb.version("scrutate"),
        },
        "zoekt": {
            "index": [os.path.join(ZOEKT, "zoekt-index"), "-disable_ctags", "-file_limit", str(1 << 40), "-max_trigram_count", str(1 << 30),
                      "-parallelism", str(cores), "-ignore_dirs", ".git,.hg,.svn,.scrutate", "-index", idx, "."],
            "update": None,  # zoekt-index has no incremental mode for a directory: an update re-runs the build
            "size_dir": idx,
            "clean": lambda: shutil.rmtree(idx, ignore_errors=True),
            "query": lambda q: [os.path.join(ZOEKT, "zoekt"), "-index_dir", idx, zoekt_query(q)],
            "version": "zoekt " + zoekt_version(),
        },
        "livegrep": {
            # limits raised so it indexes every line and returns every match, like the other tools
            "index": [a.livegrep_bin + "/codesearch", "-line_limit", "1048576", "-index_only", "-dump_index", lg_idx, os.path.join(a.work, "livegrep.json")],
            "update": None,
            "size_file": lg_idx,
            "clean": lambda: os.path.exists(lg_idx) and os.remove(lg_idx),
            # the backend holds the index in memory; lgq (bench/competitors/livegrep_query) is a
            # minimal gRPC client printing path:line:text, so no web frontend sits in between
            "server": [[a.livegrep_bin + "/codesearch", "-load_index", lg_idx, "-grpc", "localhost:9999", "-max_matches", "100000000", "-timeout", "600000",
                        "-threads", str(cores)]],
            "query": lambda q: [a.livegrep_bin + "/lgq", "-addr", "localhost:9999"] + (["-i"] if q["icase"] else []) + [to_regex(q)],
            "version": "livegrep " + (open(a.livegrep_bin + "/REVISION").read().strip() if os.path.exists(a.livegrep_bin + "/REVISION") else "?"),
        },
    }


def zoekt_version():
    try:
        out = subprocess.run(["go", "version", "-m", os.path.join(ZOEKT, "zoekt")], capture_output=True, text=True).stdout
        m = re.search(r"mod\s+github.com/sourcegraph/zoekt\s+(\S+)", out)
        return m.group(1) if m else "?"
    except Exception:
        return "?"


def to_regex(q):
    parts = [re2_escape(p) if q["fixed"] else p for p in q["pats"]]
    pat = parts[0] if len(parts) == 1 else "(" + "|".join(parts) + ")"
    return rf"\b{pat}\b" if q["word"] else pat


def livegrep_config(corpus, work):
    """livegrep's index config in `work`: exactly the files the other engines index (never
    .scrutate/). -> those files."""
    files = lb.file_list(corpus, "ignore")
    with open(os.path.join(work, "livegrep-files.txt"), "w") as f:
        f.write("\n".join(files) + "\n")
    json.dump({"name": "linux", "fs_paths": [{"name": "linux", "path": corpus, "ordered_contents": os.path.join(work, "livegrep-files.txt")}]},
              open(os.path.join(work, "livegrep.json"), "w"))
    return files


def zoekt_query(q):
    return f'case:{"no" if q["icase"] else "yes"} content:"{to_regex(q).replace(chr(34), chr(92) + chr(34))}"'


def normalise(out):
    got = set()
    for line in out.splitlines():
        m = lb.LINE.match(line)
        if m and int(m.group(2)) > 0:
            got.add((m.group(1).decode("utf-8", "surrogateescape"), int(m.group(2))))
    return got


def timed(cmd, cwd):
    """Run once: wall seconds, peak RSS bytes, exit code, stdout (rusage of this child only)."""
    t0 = time.perf_counter()
    p = subprocess.Popen(cmd, cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    out = p.stdout.read()
    _, status, ru = os.wait4(p.pid, 0)
    wall = time.perf_counter() - t0
    rss = ru.ru_maxrss if platform.system() == "Darwin" else ru.ru_maxrss * 1024
    return wall, rss, os.waitstatus_to_exitcode(status), out


def query_rss(cmd, cwd, measured):
    """Peak RSS of one run of `cmd`. On Linux a child's ru_maxrss also counts the forked copy of this
    harness from before exec, so there the figure comes from GNU time, which forks the command from
    a small process; elsewhere `measured` (from wait4) is already the command's own."""
    gnu_time = "/usr/bin/time"
    if platform.system() != "Linux" or not os.path.exists(gnu_time):
        return measured
    with tempfile.NamedTemporaryFile(suffix=".rss", delete=False) as f:
        path = f.name
    subprocess.run([gnu_time, "-f", "%M", "-o", path] + cmd, cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    kb = open(path).read().split()
    os.unlink(path)
    return int(kb[-1]) * 1024 if kb else measured


def server_rss(proc):
    """Resident bytes of a server process now (Linux /proc; elsewhere `ps`)."""
    try:
        for line in open(f"/proc/{proc.pid}/status"):
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) * 1024
    except OSError:
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(proc.pid)], capture_output=True, text=True).stdout.strip()
        return int(out) * 1024 if out else 0
    return 0


def pct(xs, p):
    xs = sorted(xs)
    k = (len(xs) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def boot(a, b, stat, B=5000, seed=3):
    rng = random.Random(seed)
    rs = sorted(stat(rng.choices(a, k=len(a))) / stat(rng.choices(b, k=len(b))) for _ in range(B))
    return stat(a) / stat(b), rs[int(0.025 * B)], rs[int(0.975 * B) - 1]


def status(lo, hi, lower_is_better=True):
    if lower_is_better:
        return "PASS" if hi < 1 else "FAIL" if lo > 1 else "PARTIAL"
    return "PASS" if lo > 1 else "FAIL" if hi < 1 else "PARTIAL"


def du(path, apparent=False):
    """Disk space used (allocated blocks, so sparse files count what they occupy), or the sum of
    file sizes with apparent=True."""
    size = (lambda p: os.path.getsize(p)) if apparent else (lambda p: os.stat(p).st_blocks * 512)
    if os.path.isfile(path):
        return size(path)
    total = 0
    for d, _, fs in os.walk(path):
        for f in fs:
            total += size(os.path.join(d, f))
    return total


def main() -> int:
    lb.drop_scrutate_settings()
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=os.path.expanduser("~/.cache/scrutate-bench/corpora/linux-nodot"))
    ap.add_argument("--work", default=os.path.expanduser("~/.cache/scrutate-bench/indexed-work"))
    ap.add_argument("--out", required=True)
    ap.add_argument("--tools", default="scrutate,zoekt")
    ap.add_argument("--runs", type=int, default=20)
    ap.add_argument("--index-runs", type=int, default=5)
    ap.add_argument("--throughput-runs", type=int, default=10)
    ap.add_argument("--concurrency", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--seconds", type=float, default=15)
    ap.add_argument("--max-load", type=float, default=(os.cpu_count() or 4) * 0.3)
    ap.add_argument("--livegrep-bin", default=os.path.expanduser("~/.cache/scrutate-bench/livegrep"))
    a = ap.parse_args()
    os.makedirs(a.work, exist_ok=True)
    files = livegrep_config(a.corpus, a.work)
    names = a.tools.split(",")
    defs = tool_defs(a)
    res = {"environment": lb.environment(), "corpus": a.corpus, "settings": vars(a) | {"queries": len(QUERIES)}, "tools": {n: {"version": defs[n]["version"]} for n in names}}
    save = lambda: json.dump(res, open(a.out, "w"), indent=1)
    # ---- indexing: full builds, size, peak memory
    times, rss = {n: [] for n in names}, {n: [] for n in names}
    for _ in range(a.index_runs):
        for n in names:
            defs[n]["clean"]()
            lb.wait_idle(a.max_load, 1800)
            w, m, code, _ = timed(defs[n]["index"], a.corpus)
            if code != 0:
                raise SystemExit(f"{n} index failed ({code})")
            times[n].append(w)
            rss[n].append(m)
    for n in names:
        where = defs[n].get("size_dir") or defs[n].get("size_file")
        r = res["tools"][n]
        r["index"] = {"times": times[n], "median": statistics.median(times[n]), "rss_max": max(rss[n]), "bytes": du(where), "apparent_bytes": du(where, apparent=True)}
        print(f"[indexed] {n}: index {r['index']['median']:.2f}s, {r['index']['bytes'] / 1e6:.0f} MB, peak {max(rss[n]) / 1e6:.0f} MB", file=sys.stderr)
    save()
    # ---- incremental update: 100 files edited, then restored
    rng = random.Random(7)
    picked = rng.sample([f for f in files if f.endswith((".c", ".h"))], 100)
    backup = {f: open(os.path.join(a.corpus, f), "rb").read() for f in picked}
    for f, data in backup.items():
        # the corpus is hard-linked to the Linux tree: give the edited files their own inodes
        p = os.path.join(a.corpus, f)
        os.remove(p)
        open(p, "wb").write(data)
    ups = {n: [] for n in names}
    for i in range(a.index_runs):
        # each tool sees the same edit on a tree its index last saw restored
        for n in names:
            cmd = defs[n]["update"] or defs[n]["index"]
            for f in picked:
                with open(os.path.join(a.corpus, f), "ab") as fh:
                    fh.write(f"\n/* benchmark edit {i} */\n".encode())
            w, _, code, _ = timed(cmd, a.corpus)
            if code != 0:
                raise SystemExit(f"{n} update failed ({code})")
            ups[n].append(w)
            for f, data in backup.items():
                open(os.path.join(a.corpus, f), "wb").write(data)
            if timed(cmd, a.corpus)[2] != 0:
                raise SystemExit(f"{n} update after the restore failed")
    for n in names:
        res["tools"][n]["update"] = {"times": ups[n], "median": statistics.median(ups[n]), "files_changed": len(picked),
                                     "mode": "incremental" if defs[n]["update"] else "full rebuild (no incremental mode for a directory)"}
    save()
    # ---- servers
    servers = []
    owner = {}
    for n in names:
        for cmd in defs[n].get("server", []):
            servers.append(subprocess.Popen(cmd, cwd=a.corpus, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
            owner[servers[-1].pid] = n
            time.sleep(10)
    try:
        # ---- correctness per query
        cache = os.path.join(os.path.dirname(os.path.abspath(a.out)), "cache")
        os.makedirs(cache, exist_ok=True)
        res["queries"] = []
        for kind, q in QUERIES:
            key = lb.hashlib.sha1(json.dumps(["indexed", a.corpus, q, len(files)], sort_keys=True).encode()).hexdigest()[:16]
            cp = os.path.join(cache, f"iref-{key}.json")
            if os.path.exists(cp):
                ref = {tuple(x) for x in json.load(open(cp))}
            else:
                ref, _ = lb.reference(a.corpus, files, q)
                json.dump(sorted(ref), open(cp, "w"))
            entry = {"kind": kind, "q": q, "reference_lines": len(ref), "tools": {}}
            for n in names:
                cmd = defs[n]["query"](q)
                _, _, code, out = timed(cmd, a.corpus)
                got = normalise(out)
                miss, extra = ref - got, got - ref
                entry["tools"][n] = {"argv": cmd, "valid": not miss and not extra and not lb.bad_exit([code]), "exit": code, "lines": len(got),
                                     "missing": len(miss), "extra": len(extra),
                                     "missing_sample": sorted(miss)[:3], "extra_sample": sorted(extra)[:3], "times": [], "rss": []}
            res["queries"].append(entry)
        save()
        # ---- latency: round-robin over tools and queries
        for rep in range(a.runs):
            lb.wait_idle(a.max_load, 1800)
            for entry in res["queries"]:
                for n in names:
                    t = entry["tools"][n]
                    if not t["valid"]:
                        continue
                    w, m, code, _ = timed(t["argv"], a.corpus)
                    if lb.bad_exit([code]):
                        t.update({"valid": False, "exit": code})
                        continue
                    t["times"].append(w)
                    t["rss"].append(m)
            if rep % 5 == 4:
                save()
        # ---- memory: one run per query measured on its own (see query_rss), plus the resident
        # memory of a tool's server, which holds its index for every query
        for entry in res["queries"]:
            for n in names:
                t = entry["tools"][n]
                if t["valid"] and t["rss"]:
                    t["rss"] = [query_rss(t["argv"], a.corpus, statistics.median(t["rss"]))]
        for p in servers:
            n = owner[p.pid]
            res["tools"][n]["server_rss"] = res["tools"][n].get("server_rss", 0) + server_rss(p)
        for n in names:
            # the percentiles compare tools over the queries every tool answered correctly
            common = [e for e in res["queries"] if all(e["tools"][x]["valid"] for x in names)]
            samples = [x for e in common for x in e["tools"][n]["times"]]
            res["tools"][n]["latency"] = {"queries": len(common), "samples": len(samples), "p50": pct(samples, .5), "p95": pct(samples, .95),
                                          "p99": pct(samples, .99),
                                          "rss_median": statistics.median(x for e in common for x in e["tools"][n]["rss"]) + res["tools"][n].get("server_rss", 0),
                                          "wrong_queries": [e["q"]["pats"][0] for e in res["queries"] if not e["tools"][n]["valid"]]}
        save()
        # ---- concurrent throughput
        common_q = [e for e in res["queries"] if all(e["tools"][x]["valid"] for x in names)]
        qps, errors = {n: [] for n in names}, {n: 0 for n in names}
        for _ in range(a.throughput_runs):
            for n in names:
                cmds = [e["tools"][n]["argv"] for e in common_q]
                lb.wait_idle(a.max_load, 1800)
                done, failed = [0] * a.concurrency, [0] * a.concurrency
                stop = time.time() + a.seconds

                def worker(i):
                    k = i
                    while time.time() < stop:
                        rc = subprocess.run(cmds[k % len(cmds)], cwd=a.corpus, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode
                        if lb.bad_exit([rc]):
                            failed[i] += 1
                        else:
                            done[i] += 1
                        k += 1

                th = [threading.Thread(target=worker, args=(i,)) for i in range(a.concurrency)]
                t0 = time.time()
                for t in th:
                    t.start()
                for t in th:
                    t.join()
                qps[n].append(sum(done) / (time.time() - t0))
                errors[n] += sum(failed)
            save()
        for n in names:
            res["tools"][n]["throughput"] = {"clients": a.concurrency, "seconds": a.seconds, "qps": qps[n], "median": statistics.median(qps[n]), "failed_queries": errors[n]}
        save()
    finally:
        for p in servers:
            p.terminate()
    # ---- comparisons (scrutate against the best competitor per metric)
    comps = [n for n in names if n != "scrutate"]
    if "scrutate" in names and comps:
        s = res["tools"]["scrutate"]
        out = {}
        common = [e for e in res["queries"] if all(e["tools"][x]["valid"] for x in names)]
        for metric, p in (("p50", .5), ("p95", .95), ("p99", .99)):
            best = min(comps, key=lambda n: res["tools"][n]["latency"][metric])
            us = [x for e in common for x in e["tools"]["scrutate"]["times"]]
            them = [x for e in common for x in e["tools"][best]["times"]]
            r, lo, hi = boot(us, them, lambda v: pct(v, p))
            out[f"latency {metric}"] = {"best": best, "ratio": r, "ci95": [lo, hi], "status": status(lo, hi), "runs": a.runs, "samples": [len(us), len(them)]}
        best = min(comps, key=lambda n: res["tools"][n]["index"]["median"])
        r, lo, hi = boot(s["index"]["times"], res["tools"][best]["index"]["times"], statistics.median)
        out["full indexing time"] = {"best": best, "ratio": r, "ci95": [lo, hi], "status": status(lo, hi), "runs": a.index_runs}
        best = min(comps, key=lambda n: res["tools"][n]["update"]["median"])
        r, lo, hi = boot(s["update"]["times"], res["tools"][best]["update"]["times"], statistics.median)
        out["incremental update time"] = {"best": best, "ratio": r, "ci95": [lo, hi], "status": status(lo, hi), "runs": a.index_runs}
        best = min(comps, key=lambda n: res["tools"][n]["index"]["bytes"])
        r = s["index"]["bytes"] / res["tools"][best]["index"]["bytes"]
        out["index size"] = {"best": best, "ratio": r, "status": "PASS" if r < 1 else "FAIL" if r > 1 else "PARTIAL"}
        best = max(comps, key=lambda n: res["tools"][n]["throughput"]["median"])
        failed = sum(res["tools"][n]["throughput"]["failed_queries"] for n in names)
        if failed:
            out["concurrent throughput"] = {"best": best, "runs": a.throughput_runs, "status": "FAIL",
                                            "why": f"{failed} queries exited with an error during the runs"}
        else:
            r, lo, hi = boot(s["throughput"]["qps"], res["tools"][best]["throughput"]["qps"], statistics.median)
            out["concurrent throughput"] = {"best": best, "ratio": r, "ci95": [lo, hi], "runs": a.throughput_runs,
                                            "status": status(lo, hi, lower_is_better=False)}
        best = min(comps, key=lambda n: res["tools"][n]["latency"]["rss_median"])
        r = s["latency"]["rss_median"] / res["tools"][best]["latency"]["rss_median"]
        out["query memory"] = {"best": best, "ratio": r, "status": "PASS" if r < 0.95 else "FAIL" if r > 1.05 else "PARTIAL"}
        res["comparison"] = out
        save()
        for k, v in out.items():
            print(f"[indexed] {k}: {v}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
