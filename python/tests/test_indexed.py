"""bench/indexed.py: a server is used once it answers, and only scrutate's own failed queries fail its throughput row."""
import os
import subprocess
import sys
import threading
import time

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import indexed as I  # noqa: E402


def tools(failed, qps):
    return {n: {"throughput": {"failed_queries": failed[n], "qps": qps[n], "median": sorted(qps[n])[len(qps[n]) // 2]}} for n in failed}


def test_a_rival_whose_queries_failed_is_left_out_and_scrutate_is_not_failed():
    t = tools({"scrutate": 0, "zoekt": 7, "livegrep": 0}, {"scrutate": [90, 91, 92], "zoekt": [500, 510, 520], "livegrep": [40, 41, 42]})
    v = I.throughput_verdict(t, ["scrutate", "zoekt", "livegrep"], 3)
    assert v["best"] == "livegrep" and v["status"] == "PASS"
    assert v["left_out"] == {"zoekt": "7 queries exited with an error"}


def test_scrutate_s_own_failed_queries_fail_its_row():
    t = tools({"scrutate": 2, "zoekt": 0}, {"scrutate": [90, 91, 92], "zoekt": [40, 41, 42]})
    v = I.throughput_verdict(t, ["scrutate", "zoekt"], 3)
    assert v["status"] == "FAIL" and "2 of scrutate's queries" in v["why"]


def test_with_every_rival_failed_there_is_no_verdict():
    t = tools({"scrutate": 0, "zoekt": 1}, {"scrutate": [90, 91, 92], "zoekt": [40, 41, 42]})
    assert I.throughput_verdict(t, ["scrutate", "zoekt"], 3)["status"] == "n/a"


def test_a_server_is_used_once_its_tool_answers(tmp_path):
    flag = tmp_path / "up"
    server = subprocess.Popen(["sleep", "30"])
    try:
        threading.Timer(0.5, flag.touch).start()
        t0 = time.monotonic()
        I.wait_ready(server, ["sh", "-c", f"test -e {flag} || exit 2"], str(tmp_path), limit=10)
        assert 0.4 < time.monotonic() - t0 < 5
    finally:
        server.kill()


def test_a_server_that_exits_or_never_answers_stops_the_run(tmp_path):
    gone = subprocess.Popen(["true"])
    gone.wait()
    with pytest.raises(SystemExit, match="exited"):
        I.wait_ready(gone, ["sh", "-c", "exit 2"], str(tmp_path), limit=10)
    server = subprocess.Popen(["sleep", "30"])
    try:
        with pytest.raises(SystemExit, match="within 1 s"):
            I.wait_ready(server, ["sh", "-c", "exit 2"], str(tmp_path), limit=1)
    finally:
        server.kill()
