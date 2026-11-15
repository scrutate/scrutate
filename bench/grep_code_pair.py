#!/usr/bin/env python3
"""MCP grep_code of two scrutate builds on one box: the yardstick's W3 queries to two resident
servers on one index, the two in turn (ABBA) on every round, through the yardstick's own adapter
(bench/rivals/scrutate.py Mcp), so each call is the one W3 times. Without --queries the queries
are those yardstick.py's W3 builds: one per entry of bench/indexed.py's QUERIES, in order.

usage: grep_code_pair.py --corpus DIR --before BIN --after BIN [--queries FILE] [--runs 20] [--warmup 2]

A query whose answer passes grep_code's 5,000-line cap is not answered (as in W3) and not timed.
The two servers' answers must be equal on every answered query, or the run stops.
"""
import argparse
import json
import os
import statistics
import sys
import time

ap = argparse.ArgumentParser()
ap.add_argument("--corpus", required=True)
ap.add_argument("--queries")
ap.add_argument("--before", required=True)
ap.add_argument("--after", required=True)
ap.add_argument("--runs", type=int, default=20)
ap.add_argument("--warmup", type=int, default=2)
a = ap.parse_args()
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from rivals import Unsupported  # noqa: E402
from rivals.scrutate import Mcp  # noqa: E402

if a.queries:
    queries = json.load(open(a.queries))
else:
    from indexed import QUERIES  # noqa: E402

    queries = [{"id": f"W3/{i:02d}-{kind.replace(' ', '-')}", "workload": "W3", "grep": q} for i, (kind, q) in enumerate(QUERIES)]
side = {"before": Mcp(a.before), "after": Mcp(a.after)}
for name, s in side.items():
    t0 = time.perf_counter()
    s.start(a.corpus, None)
    print(f"{name}: {s.version()}, started in {time.perf_counter() - t0:.1f} s", flush=True)
times = {n: {q["id"]: [] for q in queries} for n in side}
answered = {}
try:
    for q in queries:
        got = {}
        for n, s in side.items():
            try:
                got[n] = s.ask("W3", q)
            except Unsupported as e:
                got[n] = f"not answered: {e}"
        if got["before"] != got["after"]:
            sys.exit(f"{q['id']}: the two answers differ")
        answered[q["id"]] = not isinstance(got["before"], str)
    for r in range(a.warmup + a.runs):
        order = ["before", "after"] if r % 2 == 0 else ["after", "before"]
        for q in queries:
            if not answered[q["id"]]:
                continue
            for n in order:
                t0 = time.perf_counter()
                side[n].ask("W3", q)
                ms = (time.perf_counter() - t0) * 1000
                if r >= a.warmup:
                    times[n][q["id"]].append(ms)
finally:
    for s in side.values():
        s.stop()
ok = [q["id"] for q in queries if answered[q["id"]]]
print(f"answered {len(ok)} of {len(queries)}: {' '.join(ok)}")
for q in ok:
    print(f"{q:24} before {statistics.median(times['before'][q]):9.2f} ms   after {statistics.median(times['after'][q]):8.2f} ms")
for n in side:
    meds = [statistics.median(times[n][q]) for q in ok]
    print(f"{n}: median of the per-query medians {statistics.median(meds):.2f} ms over {len(ok)} queries")
