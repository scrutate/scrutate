"""bench/musl_vs_glibc.py: the worst ratio is the largest one measured, not floored at x1.00."""
import os
import stat
import subprocess
import sys

ROOT = os.path.join(os.path.dirname(__file__), "..", "..")


def fake(path, pause):
    with open(path, "w") as f:
        f.write(f"#!/bin/sh\nsleep {pause}\necho same\n")
    os.chmod(path, os.stat(path).st_mode | stat.S_IEXEC)


def test_a_musl_binary_faster_on_every_search_reports_a_worst_ratio_under_one(tmp_path):
    fake(tmp_path / "glibc", 0.2)
    fake(tmp_path / "musl", 0.01)
    out = subprocess.run([sys.executable, os.path.join(ROOT, "bench", "musl_vs_glibc.py"), str(tmp_path / "glibc"), str(tmp_path / "musl"), str(tmp_path), "--runs", "1"],
                         capture_output=True, text=True, check=True).stdout
    worst = float(out.strip().splitlines()[-1].split("x")[-1])
    assert worst < 0.5, out
