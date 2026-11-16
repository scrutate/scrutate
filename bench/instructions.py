#!/usr/bin/env python3
"""Instruction-count regression gate: each leaderboard benchmark's scrutate command line, run once
under Valgrind's cachegrind (callgrind for the start-up row) for the change and once for its base.

usage: instructions.py --corpora DIR --new BIN --base BIN --out FILE.json
                       [--only PREFIX...] [--skip PREFIX...] [--tolerance T] [--base-commit SHA]

Text searches run with -j1 and tsquery with RAYON_NUM_THREADS=1: Valgrind runs one thread at a
time, so with more threads the count would depend on how they were scheduled. A "4 at once" row
counts its four copies and the shell that starts them. Both binaries should sit at paths of the
same length (argv[0] lands on the stack), and address randomisation is off.

The start-up row counts every instruction but the dynamic loader's own, from callgrind's cost by
object in place of cachegrind's total: the loader's share follows the binary's relocations, which
grow with every static table, not with what scrutate does at start. The loader's count is reported
as its own row (LOADER_ROWS), with its own limit, so that growth is still seen.

A row regresses when new / base instructions exceeds 1 + tolerance, is faster below 1 - tolerance,
and is the same otherwise. A command that exits other than 0 or 1 fails its row. Exit status 1
when anything failed, 2 when nothing was compared.

bench/instructions_accepted.json holds rises accepted on review: a row's `limit` replaces
1 + tolerance, only while --base-commit is the `base` the rise was measured against, so it lapses
once that base moves on (a later rise then fails at 1%). A row within its limit reads "accepted".
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import platform
import re
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


# a row counted without the dynamic loader's own instructions -> the row that reports them
LOADER_ROWS = {"startup/no match": "startup/loader"}
LOADER = re.compile(r"^ld(-linux.*)?\.so")


def by_object(text):
    """-> ({object: its own instructions}, total) of one callgrind output file."""
    names, own, ob, call = {}, {}, "?", False
    for line in text.splitlines():
        m = re.match(r"^(c?ob)=\((\d+)\)(?: (.*))?$", line)
        if m:
            kind, n, name = m.groups()
            if name:
                names[n] = os.path.basename(name)
            if kind == "ob":
                ob = names[n]
            continue
        if line.startswith("calls="):
            call = True
        elif line[:1].isdigit() or line[:1] in "+-*":
            parts = line.split()
            # the line after calls= is the call's inclusive cost, the callee's, not this object's
            if not call and len(parts) > 1:
                own[ob] = own.get(ob, 0) + int(parts[1])
            call = False
    total = re.search(r"^(?:summary|totals): (\d+)", text, re.M)
    return own, int(total.group(1)) if total else sum(own.values())


def count_split(cmd, cwd, env):
    """(instructions over every process but the dynamic loader's own, the loader's own, exit status,
    output lines), from one callgrind run."""
    with tempfile.TemporaryDirectory() as d:
        vg = ["setarch", platform.machine(), "-R", "valgrind", "--tool=callgrind", "--trace-children=yes", "-q",
              f"--callgrind-out-file={d}/cg.%p"]
        p = subprocess.run(vg + cmd, cwd=cwd, env={**os.environ, **env}, stdin=subprocess.DEVNULL, capture_output=True)
        total = loader = 0
        for f in glob.glob(os.path.join(d, "cg.*")):
            own, t = by_object(open(f).read())
            total += t
            loader += sum(v for k, v in own.items() if LOADER.match(k))
        return total - loader, loader, p.returncode, p.stdout.count(b"\n")


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


def verdict(new, base, tolerance, limit=None):
    r = new / base
    if limit is not None and 1 + tolerance < r <= limit:
        return r, "accepted"
    return r, "REGRESSION" if r > max(1 + tolerance, limit or 0) else "faster" if r < 1 - tolerance else "same"


ACCEPTED = os.path.join(os.path.dirname(os.path.abspath(__file__)), "instructions_accepted.json")


def accepted_limits(base_commit, path=ACCEPTED):
    """-> {row: limit} of the accepted rises measured against `base_commit`."""
    entries = json.load(open(path)) if os.path.exists(path) else {}
    return {row: e["limit"] for row, e in entries.items() if base_commit and e["base"] == base_commit}


def _selfcheck():
    assert verdict(1011, 1000, 0.01)[1] == "REGRESSION"
    assert verdict(1010, 1000, 0.01)[1] == "same" and verdict(1000, 1000, 0.01) == (1.0, "same")
    assert verdict(989, 1000, 0.01)[1] == "faster"
    assert verdict(1014, 1000, 0.01, 1.02)[1] == "accepted" and verdict(1021, 1000, 0.01, 1.02)[1] == "REGRESSION"
    assert verdict(1005, 1000, 0.01, 1.02)[1] == "same"
    # self cost by object: a call's line is the callee's; a name given once is reused by number
    own, total = by_object("events: Ir\nob=(1) /lib/ld-linux-x86-64.so.2\nfn=(1) _dl_start\n0 700\ncob=(2) /bin/x\ncfn=(2) main\n"
                           "calls=1 0\n0 250\n+1 50\nob=(2)\nfn=(2)\n0 250\ntotals: 1000\n")
    assert own == {"ld-linux-x86-64.so.2": 750, "x": 250} and total == 1000, own
    assert LOADER.match("ld-linux-x86-64.so.2") and LOADER.match("ld-linux-aarch64.so.1") and LOADER.match("ld.so")
    assert not LOADER.match("libc.so.6") and not LOADER.match("scrutate")


def judge(rid, row, tolerance, limit):
    """Sets row's verdict (and ratio) and prints it."""
    if bad_exit([row["new"]["exit"]]) or not row["new"]["instructions"]:
        row["verdict"] = "FAIL"
        print(f"FAIL       {rid}: exit {row['new']['exit']}, {row['new']['instructions']} instructions")
    elif bad_exit([row["base"]["exit"]]) or not row["base"]["instructions"]:
        row["verdict"] = "skip"
        print(f"skip       {rid}: the base exits {row['base']['exit']}, {row['base']['instructions']} instructions")
    else:
        row["ratio"], row["verdict"] = verdict(row["new"]["instructions"], row["base"]["instructions"], tolerance, limit)
        differs = "" if row["new"]["lines"] == row["base"]["lines"] else f"; output {row['base']['lines']} -> {row['new']['lines']} lines"
        if limit is not None:
            differs += f"; accepted up to {limit} against this base (bench/instructions_accepted.json)"
        print(f"{row['verdict']:10} {rid}: {row['base']['instructions']:,} -> {row['new']['instructions']:,} instructions (ratio {row['ratio']:.4f}){differs}",
              flush=True)


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
    ap.add_argument("--base-commit", help="the base's commit; selects the accepted rises measured against it")
    a = ap.parse_args()
    limits = accepted_limits(a.base_commit)
    rows, bad = {}, 0
    for b in suite():
        bid = b["id"]
        if (a.only and not bid.startswith(tuple(a.only))) or bid.startswith(tuple(a.skip)):
            continue
        cdir = os.path.join(a.corpora, b["corpus"])
        if not os.path.isdir(cdir):
            raise SystemExit(f"{bid}: no corpus at {cdir}")
        row, loader = {}, {}
        for side in ("base", "new"):
            cmd, env = command(b, getattr(a, side), cdir)
            if bid in LOADER_ROWS:
                n, ld, code, lines = count_split(cmd, cdir, env)
                loader[side] = {"argv": cmd, "env": env, "instructions": ld, "exit": code, "lines": lines}
            else:
                n, code, lines = count(cmd, cdir, env)
            row[side] = {"argv": cmd, "env": env, "instructions": n, "exit": code, "lines": lines}
        judged = [(bid, row)] + ([(LOADER_ROWS[bid], loader)] if loader else [])
        for rid, r in judged:
            judge(rid, r, a.tolerance, limits.get(rid))
            bad += r["verdict"] in ("FAIL", "REGRESSION")
            rows[rid] = r
        json.dump({"tolerance": a.tolerance, "benchmarks": rows}, open(a.out, "w"), indent=1)
    if not any("ratio" in r for r in rows.values()):
        print("nothing compared")
        return 2
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
