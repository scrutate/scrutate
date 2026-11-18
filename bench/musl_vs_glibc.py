#!/usr/bin/env python3
"""Measure the musl static binary against the glibc binary on the same searches (median of N runs)
and verify the outputs are byte-identical. usage: musl_vs_glibc.py GLIBC_BIN MUSL_BIN CORPUS [--runs 11]
"""
from __future__ import annotations

import argparse
import os
import statistics
import subprocess
import sys
import time


def timed(cmd, cwd):
    t0 = time.perf_counter()
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, stdin=subprocess.DEVNULL)
    # several threads print in no fixed order: compare the set of lines
    return time.perf_counter() - t0, sorted(r.stdout.splitlines())


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("glibc")
    ap.add_argument("musl")
    ap.add_argument("corpus")
    ap.add_argument("--runs", type=int, default=11)
    a = ap.parse_args()
    a.glibc, a.musl = os.path.abspath(a.glibc), os.path.abspath(a.musl)
    queries = [["grep", "-F", "fn ", "."], ["grep", r"\w+::\w+\(", "."], ["grep", "-F", "-c", "use ", "."], ["grep", "--ident", "parseFile", "."]]
    worst = 0.0
    for q in queries:
        tg, tm, out_g, out_m = [], [], None, None
        for _ in range(a.runs):
            t, out_g = timed([a.glibc, *q], a.corpus)
            tg.append(t)
            t, out_m = timed([a.musl, *q], a.corpus)
            tm.append(t)
        if out_g != out_m:
            print(f"OUTPUT DIFFERS for {q}")
            return 1
        ratio = statistics.median(tm) / statistics.median(tg)
        worst = max(worst, ratio)
        print(f"{' '.join(q):32} glibc {1000*statistics.median(tg):7.1f} ms  musl {1000*statistics.median(tm):7.1f} ms  musl/glibc x{ratio:.2f}  (identical output, {len(out_g)} bytes)")
    print(f"worst musl/glibc ratio: x{worst:.2f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
