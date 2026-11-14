#!/usr/bin/env python3
"""Wait until macOS starts a just-built binary at its settled speed, at most an hour.

usage: wait_aged.py BIN [--cap 3600] [--every 120]

macOS starts a new executable file about 0.2 ms slower than the same file later (identical copies,
the same instructions executed), which is 5% of a small-tree search. Each round writes a fresh copy
of BIN, a new file and so slow, and times `--version` of both in alternating hyperfine batches. BIN
has settled once it starts faster than its fresh copy (the 95% interval of the ratio of medians
wholly below 1) in two rounds in a row, with its median within 2% of the round before. Where new
files start no slower, that never happens and the wait ends at the cap, as the fixed hour did.
Exit 0 either way; the last line says which.
"""
from __future__ import annotations

import argparse
import os
import shutil
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from leaderboard import boot_ratio, drop_scrutate_settings, hyperfine  # noqa: E402


def settled(rounds):
    """rounds: (ratio interval upper end, median of BIN) per round, oldest first."""
    if len(rounds) < 2:
        return False
    (hi1, m1), (hi2, m2) = rounds[-2:]
    return hi1 < 1 and hi2 < 1 and abs(m2 - m1) <= 0.02 * m1


def main() -> int:
    drop_scrutate_settings()
    ap = argparse.ArgumentParser()
    ap.add_argument("bin")
    ap.add_argument("--cap", type=float, default=3600)
    ap.add_argument("--every", type=float, default=120, help="seconds between the starts of two rounds")
    a = ap.parse_args()
    fresh = os.path.join(os.path.dirname(os.path.abspath(a.bin)), "..", "startup-probe")
    t0, rounds = time.time(), []
    while time.time() - t0 < a.cap:
        start = time.time()
        # a new file each round: a copy that stayed would settle too
        if os.path.exists(fresh):
            os.unlink(fresh)
        shutil.copyfile(a.bin, fresh)
        shutil.copymode(a.bin, fresh)
        ours, theirs = [], []
        for i in range(4):
            for path, into in ((a.bin, ours), (fresh, theirs)) if i % 2 == 0 else ((fresh, theirs), (a.bin, ours)):
                into += hyperfine([path, "--version"], os.getcwd(), {}, 30, 3)["times"]
        ratio, _, hi = boot_ratio(ours, theirs)
        rounds.append((hi, statistics.median(ours)))
        print(f"[wait_aged] {(time.time() - t0) / 60:.0f} min: {statistics.median(ours) * 1000:.3f} ms against a fresh copy's "
              f"{statistics.median(theirs) * 1000:.3f} ms, ratio {ratio:.3f}, interval up to {hi:.3f}", flush=True)
        if settled(rounds):
            os.unlink(fresh)
            print(f"[wait_aged] settled after {(time.time() - t0) / 60:.0f} minutes", flush=True)
            return 0
        time.sleep(max(0.0, a.every - (time.time() - start)))
    if os.path.exists(fresh):
        os.unlink(fresh)
    print(f"[wait_aged] not settled; stopped at the cap of {a.cap / 60:.0f} minutes", flush=True)
    return 0


def _selfcheck() -> None:
    assert not settled([(0.9, 1.0)])
    assert settled([(0.95, 1.00), (0.96, 1.01)])
    assert not settled([(1.01, 1.00), (0.96, 1.00)]) and not settled([(0.95, 1.00), (0.96, 1.05)])


if __name__ == "__main__":
    _selfcheck()
    sys.exit(main())
