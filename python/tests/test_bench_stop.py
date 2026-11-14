"""The leaderboard stops a row early only when every verdict holds at alpha 0.001."""
import os
import random
import statistics
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import leaderboard as L  # noqa: E402


def _tool(factor, load, rnd, per=10):
    times = [0.010 * busy * factor * rnd.uniform(0.98, 1.02) for busy in load for _ in range(per)]
    return {"status": "ok", "n": len(times), "blocks": len(load), "times": times, "cpu": [t * 2 for t in times], "rss": [5e6] * len(times),
            "median": statistics.median(times)}


def _row(tools):
    return {"tools": tools, "load": []}


def test_the_boundary_spends_0_001_before_the_last_look_and_0_05_in_all():
    interim, last = L.stop_rule(3)
    assert interim == [2] and last == 0.049
    assert L.INTERIM_ALPHA * len(interim) + last <= 0.05 + 1e-12
    # at alpha 0.05 the interval is the one the published runs were judged by
    a, b = [1.0, 1.1, 0.9] * 10, [2.0, 2.1, 1.9] * 10
    assert L.boot_ratio(a, b) == L.boot_ratio(a, b, B=10000, alpha=0.05)


def test_a_clear_lead_stops_after_two_blocks():
    rnd = random.Random(3)
    tools = {"scrutate": _tool(0.8, [1.0, 1.0], rnd), "ripgrep": _tool(1.0, [1.0, 1.0], rnd)}
    tools["ripgrep"]["rss"] = [9e6] * 20
    r = _row(tools)
    L.decide(r, ["scrutate", "ripgrep"], 20, L.INTERIM_ALPHA)
    assert r["status"] == "PASS" and r["ci95"][1] < 1
    # CPU is 2x the wall time for both, so their CPU ratio is the wall ratio; -j1 was not run
    assert r["metrics"]["cpu"]["default"]["status"] == "PASS" and r["metrics"]["cpu"]["-j1"]["status"] == "n/a"
    assert L.settled(r)


def test_a_tie_does_not_stop():
    rnd = random.Random(4)
    r = _row({"scrutate": _tool(1.0, [1.0, 1.0], rnd), "ripgrep": _tool(1.0, [1.0, 1.0], rnd)})
    L.decide(r, ["scrutate", "ripgrep"], 20, L.INTERIM_ALPHA)
    assert r["status"] == "PARTIAL" and not L.settled(r)


def test_blocks_measured_under_different_load_do_not_stop():
    rnd = random.Random(5)
    tools = {"scrutate": _tool(0.8, [1.0, 1.4], rnd), "ripgrep": _tool(1.0, [1.0, 1.4], rnd)}
    tools["ripgrep"]["rss"] = [9e6] * 20
    r = _row(tools)
    L.decide(r, ["scrutate", "ripgrep"], 20, L.INTERIM_ALPHA)
    assert L.unstable(r) and not L.settled(r)


def test_an_unsettled_memory_verdict_holds_the_row():
    rnd = random.Random(6)
    r = _row({"scrutate": _tool(0.8, [1.0, 1.0], rnd), "ripgrep": _tool(1.0, [1.0, 1.0], rnd)})
    L.decide(r, ["scrutate", "ripgrep"], 20, L.INTERIM_ALPHA)
    # both use 5 MB in every run: a tie
    assert r["status"] == "PASS" and r["metrics"]["rss"]["default"]["status"] == "PARTIAL" and not L.settled(r)


def test_import_keeps_the_environment_and_a_run_drops_scrutate_settings():
    # a test that imports leaderboard must still see SCRUTATE_BIN; the tools a run starts must not
    code = (
        "import os, subprocess, sys; os.environ['SCRUTATE_PROBE'] = '1'; sys.path.insert(0, sys.argv[1]); import leaderboard as L; "
        "assert os.environ.get('SCRUTATE_PROBE') == '1', 'import dropped it'; L.drop_scrutate_settings(); "
        "env = subprocess.run([sys.executable, '-c', 'import os; print(sorted(os.environ))'], capture_output=True, text=True).stdout; "
        "assert 'SCRUTATE_' not in env, env"
    )
    import subprocess
    r = subprocess.run([sys.executable, "-c", code, os.path.join(os.path.dirname(__file__), "..", "..", "bench")], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr


def test_c_helpers_build_where_no_target_dir_exists(tmp_path, monkeypatch):
    # the CI benchmark jobs get scrutate as an artifact and never run cargo, so target/ is missing
    monkeypatch.setattr(L, "ROOT", str(tmp_path))
    for out in (L.rawread_bin(), L.c_helper("evict")):
        assert os.access(out, os.X_OK), out
