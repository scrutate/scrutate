#!/usr/bin/env python3
"""Instruction-count regression gate: each leaderboard benchmark's scrutate command line, run once
under Valgrind's cachegrind for the change and once for its base.

usage: instructions.py --corpora DIR --new BIN --base BIN --out FILE.json
                       [--only PREFIX...] [--skip PREFIX...] [--tolerance T]

Text searches run with -j1 and tsquery with RAYON_NUM_THREADS=1: Valgrind runs one thread at a
time, so with more threads the count would depend on how they were scheduled. A "4 at once" row
counts its four copies and the shell that starts them. Both binaries should sit at paths of the
same length (argv[0] lands on the stack), and address randomisation is off.

A row regresses when new / base instructions exceeds 1 + tolerance, is faster below 1 - tolerance,
and is the same otherwise. A command that exits other than 0 or 1 fails its row. Exit status 1
when anything failed, 2 when nothing was compared.
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import platform
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from leaderboard import argv, bad_exit, drop_scrutate_settings, group_argv, structural_argv, suite  # noqa: E402


def command(b, binary, cdir):
    """(argv, extra environment) of scrutate for benchmark `b`, run by `binary`."""
    if b.get("structural"):
        cmd, env = structural_argv("scrutate", b["structural"]), {"RAYON_NUM_THREADS": "1"}
    else:
        cmd, env = argv("scrutate -j1", b["q"], b["mode"], cdir)
    cmd = [binary, *cmd[1:]]
    return (group_argv(cmd, b["concurrent"]) if b.get("concurrent") else cmd), env


def count(cmd, cwd, env):
    """(instructions over every process, exit status, output lines)."""
    with tempfile.TemporaryDirectory() as d:
        vg = ["setarch", platform.machine(), "-R", "valgrind", "--tool=cachegrind", "--cache-sim=no", "--trace-children=yes", "-q",
              f"--cachegrind-out-file={d}/cg.%p"]
        p = subprocess.run(vg + cmd, cwd=cwd, env={**os.environ, **env}, stdin=subprocess.DEVNULL, capture_output=True)
        total = 0
        for f in glob.glob(os.path.join(d, "cg.*")):
            for line in open(f):
                if line.startswith("summary:"):
                    total += int(line.split()[1])
        return total, p.returncode, p.stdout.count(b"\n")


def verdict(new, base, tolerance):
    r = new / base
    return r, "REGRESSION" if r > 1 + tolerance else "faster" if r < 1 - tolerance else "same"


def _selfcheck():
    assert verdict(1011, 1000, 0.01)[1] == "REGRESSION"
    assert verdict(1010, 1000, 0.01)[1] == "same" and verdict(1000, 1000, 0.01) == (1.0, "same")
    assert verdict(989, 1000, 0.01)[1] == "faster"


def main() -> int:
    drop_scrutate_settings()
    _selfcheck()
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpora", required=True)
    ap.add_argument("--new", required=True)
    ap.add_argument("--base", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--only", nargs="*")
    ap.add_argument("--skip", nargs="*", default=[])
    # a same-binary run reads 1.000 on every row; 1% is under the 2.66% wall-time noise of shared runners
    ap.add_argument("--tolerance", type=float, default=0.01)
    a = ap.parse_args()
    rows, bad = {}, 0
    for b in suite():
        bid = b["id"]
        if (a.only and not bid.startswith(tuple(a.only))) or bid.startswith(tuple(a.skip)):
            continue
        cdir = os.path.join(a.corpora, b["corpus"])
        if not os.path.isdir(cdir):
            raise SystemExit(f"{bid}: no corpus at {cdir}")
        row = {}
        for side in ("base", "new"):
            cmd, env = command(b, getattr(a, side), cdir)
            n, code, lines = count(cmd, cdir, env)
            row[side] = {"argv": cmd, "env": env, "instructions": n, "exit": code, "lines": lines}
        if bad_exit([row["new"]["exit"]]) or not row["new"]["instructions"]:
            row["verdict"] = "FAIL"
            print(f"FAIL       {bid}: exit {row['new']['exit']}, {row['new']['instructions']} instructions")
        elif bad_exit([row["base"]["exit"]]) or not row["base"]["instructions"]:
            row["verdict"] = "skip"
            print(f"skip       {bid}: the base exits {row['base']['exit']}")
        else:
            row["ratio"], row["verdict"] = verdict(row["new"]["instructions"], row["base"]["instructions"], a.tolerance)
            differs = "" if row["new"]["lines"] == row["base"]["lines"] else f"; output {row['base']['lines']} -> {row['new']['lines']} lines"
            print(f"{row['verdict']:10} {bid}: {row['base']['instructions']:,} -> {row['new']['instructions']:,} instructions (ratio {row['ratio']:.4f}){differs}",
                  flush=True)
        bad += row["verdict"] in ("FAIL", "REGRESSION")
        rows[bid] = row
        json.dump({"tolerance": a.tolerance, "benchmarks": rows}, open(a.out, "w"), indent=1)
    if not any("ratio" in r for r in rows.values()):
        print("nothing compared")
        return 2
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
