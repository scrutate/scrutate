"""bench/compare_tools.py reports a tool that failed and never counts it as agreeing."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import compare_tools as C  # noqa: E402


def test_a_structural_run_that_fails_is_reported_and_does_not_agree(monkeypatch):
    # ast-grep absent: scrutate alone, and its tsquery exits 2 with no output
    monkeypatch.setattr(C, "structural_commands", lambda tools: {"scrutate": ["scrutate", "tsquery"]})
    monkeypatch.setattr(C, "timed_rusage", lambda *a, **k: {"wall": 0.1, "out": 0, "err": b"error: bad query", "rc": 2, "cpu": 0.1, "maxrss_kb": 1})
    monkeypatch.setattr(C, "peak_rss_mb", lambda *a, **k: 1.0)
    rows = C.bench_structural({}, {"c": "/nowhere"}, 3)
    assert [(r["tool"], r["status"], r["agrees"]) for r in rows] == [("scrutate", "exit 2: error: bad query", False)]
