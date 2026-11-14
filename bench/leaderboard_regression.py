#!/usr/bin/env python3
"""Regression gate for the leaderboard, benchmark by benchmark.

usage: leaderboard_regression.py FILE.json [--tolerance T]              (or --within FILE.json)
       leaderboard_regression.py BASELINE.json NEW.json [--tolerance T]

With one file (the gate): `scrutate` against `scrutate base`, both measured in the same run, in the
same interleaved blocks (bench/leaderboard.py with SCRUTATE_BASE_BIN set). A slow minute on a shared
machine slows both, so their ratio stays put; two runs minutes apart cannot be compared that way.
The ratio is taken inside each block and the interval comes from resampling inside each block, so
a block that ran on a busier machine drops out of both.

With two files (recorded runs, same machine): scrutate's runs in NEW against those in BASELINE.

A benchmark regresses when the 95% bootstrap confidence interval of median(new) / median(base)
lies entirely above 1 + tolerance. A benchmark whose answer became wrong always fails. Exit
status 1 when anything regressed, 2 when the file holds nothing to compare.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from leaderboard import boot_ratio  # noqa: E402

MIN_RUNS = 10


def block_ratio(new, base, blocks, B=10000, seed=1):
    """The median over the blocks of median(new) / median(base) in that block, with its 95% interval."""
    rng, med = random.Random(seed), statistics.median
    per = len(new) // blocks
    nb = [(new[i * per:(i + 1) * per], base[i * per:(i + 1) * per]) for i in range(blocks)]
    rs = sorted(med(med(rng.choices(n, k=per)) / med(rng.choices(b, k=per)) for n, b in nb) for _ in range(B))
    return med(med(n) / med(b) for n, b in nb), rs[int(0.025 * B)], rs[int(0.975 * B) - 1]


def compare(pairs, tolerance, paired=False):
    """pairs: (id, base tool record, new tool record); paired: both were measured in the same blocks.
    Returns (failures, compared)."""
    bad = compared = 0
    for bid, b, n in pairs:
        if n.get("status") != "ok":
            if b.get("status") not in (None, "ok"):
                print(f"skip       {bid}: answer is {n.get('status')}, as the base's was ({b.get('status')})")
                continue
            print(f"FAIL       {bid}: answer is {n.get('status')}")
            bad += 1
            continue
        if b.get("status") == "ok" and len(b.get("times", [])) >= MIN_RUNS and len(n.get("times", [])) < MIN_RUNS:
            # leaderboard.py cuts a tool more than 20x slower than the fastest to one run
            print(f"FAIL       {bid}: {len(n.get('times', []))} run(s) against the base's {len(b['times'])}: {n.get('note', 'too slow to time')}")
            bad += 1
            continue
        if b.get("status") != "ok" or len(b.get("times", [])) < MIN_RUNS or len(n.get("times", [])) < MIN_RUNS:
            print(f"skip       {bid}: not enough runs to compare (base {b.get('status')}, {len(b.get('times', []))} runs)")
            continue
        blocks = n.get("blocks") if paired else None
        if blocks and blocks == b.get("blocks") and len(n["times"]) == len(b["times"]) and len(n["times"]) % blocks == 0:
            r, lo, hi = block_ratio(n["times"], b["times"], blocks)
        else:
            r, lo, hi = boot_ratio(n["times"], b["times"])
        verdict = "REGRESSION" if lo > 1 + tolerance else "faster" if hi < 1 else "same"
        bad += verdict == "REGRESSION"
        compared += 1
        print(f"{verdict:10} {bid}: {b['median'] * 1000:.2f} -> {n['median'] * 1000:.2f} ms (ratio {r:.3f}, CI {lo:.3f} to {hi:.3f})")
    return bad, compared


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("files", nargs="*", help="one file (scrutate against scrutate base), or BASELINE NEW")
    ap.add_argument("--within", metavar="FILE", help="one file: scrutate against scrutate base from the same run")
    # a copy of one binary against itself on a busy machine: interval lower bounds up to 1.05
    ap.add_argument("--tolerance", type=float, default=0.10)
    a = ap.parse_args()
    files = ([a.within] if a.within else []) + a.files
    if len(files) not in (1, 2):
        ap.error("give one file (or --within FILE), or BASELINE and NEW")
    if len(files) == 1:
        bench = json.load(open(files[0]))["benchmarks"]
        pairs = [(bid, r["tools"].get("scrutate base", {}), r["tools"].get("scrutate", {})) for bid, r in sorted(bench.items())]
        if not any("scrutate base" in r["tools"] for r in bench.values()):
            print(f"{files[0]}: no `scrutate base` runs (measure with SCRUTATE_BASE_BIN set)")
            return 2
    else:
        base, new = (json.load(open(f))["benchmarks"] for f in files)
        pairs = [(bid, base[bid]["tools"].get("scrutate", {}), new[bid]["tools"].get("scrutate", {})) for bid in sorted(set(base) & set(new))]
    bad, compared = compare(pairs, a.tolerance, paired=len(files) == 1)
    if not bad and not compared:
        print("nothing compared")
        return 2
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
