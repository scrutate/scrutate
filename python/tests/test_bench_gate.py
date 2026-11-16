"""The benchmark gate compares a change with its base inside each block of one run."""
import os
import random
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
from leaderboard_regression import compare  # noqa: E402


def _runs(factor, load, rnd):
    return [0.010 * busy * factor * rnd.uniform(0.98, 1.02) for busy in load for _ in range(5)]


def test_a_regression_shows_through_a_machine_whose_load_drifts():
    rnd = random.Random(7)
    load = [1.0, 1.6, 1.0, 2.2, 1.3, 1.0]
    base = {"status": "ok", "blocks": 6, "median": 0.010, "times": _runs(1.0, load, rnd)}
    same = {**base, "times": _runs(1.0, load, rnd)}
    slower = {**base, "times": _runs(1.15, load, rnd)}
    assert compare([("same", base, same)], 0.10, paired=True) == (0, 1)
    assert compare([("slower", base, slower)], 0.10, paired=True) == (1, 1)
    # runs pooled over the blocks cannot tell a 15% regression from that drift
    assert compare([("slower", base, slower)], 0.10, paired=False) == (0, 1)


def test_a_change_too_slow_to_be_timed_fails_the_gate():
    rnd = random.Random(3)
    load = [1.0] * 6
    base = {"status": "ok", "blocks": 6, "median": 0.010, "times": _runs(1.0, load, rnd)}
    # leaderboard.py keeps one run of a tool more than 20x slower than the fastest
    cut = {"status": "ok", "n": 1, "median": 6.0, "times": [6.0], "note": "not a contender (more than 20x slower in the correctness run)"}
    assert compare([("cut", base, cut)], 0.10, paired=True) == (1, 0)
    # both cut, as the base was: nothing to compare, and no failure
    assert compare([("both", {**cut, "median": 5.5, "times": [5.5]}, cut)], 0.10, paired=True) == (0, 0)


def test_the_instruction_gate_counts_one_thread_and_fails_past_one_percent():
    from instructions import command, verdict
    from leaderboard import suite

    b = {x["id"]: x for x in suite()}
    cmd, env = command(b["ripgrep/literal"], "/n/scrutate", "/c")
    assert cmd[:3] == ["/n/scrutate", "grep", "-j1"] and env == {}
    cmd, env = command(b["ripgrep/structural"], "/n/scrutate", "/c")
    assert cmd[:2] == ["/n/scrutate", "tsquery"] and env == {"RAYON_NUM_THREADS": "1"}
    cmd, _ = command(b["ripgrep/4 at once (literal)"], "/n/scrutate", "/c")
    assert cmd[:2] == ["/bin/sh", "-c"] and cmd[2].count("/n/scrutate grep -j1") == 4
    assert [verdict(n, 1000, 0.01)[1] for n in (1011, 1010, 1000, 989)] == ["REGRESSION", "same", "same", "faster"]


def test_the_start_up_row_leaves_out_the_dynamic_loader_which_is_a_row_of_its_own():
    from instructions import LOADER, LOADER_ROWS, accepted_limits, by_object
    from leaderboard import suite

    ids = {x["id"] for x in suite()}
    assert LOADER_ROWS == {"startup/no match": "startup/loader"} and "startup/no match" in ids and "startup/loader" not in ids
    # callgrind's cost lines: the one after calls= is the callee's inclusive cost, not the caller's
    text = ("events: Ir\nob=(1) /usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2\nfn=(1) _dl_relocate_object\n0 9000\n"
            "cob=(2) /tmp/bin/scrutate\ncfn=(2) planted\ncalls=1 0\n0 400\nob=(2)\nfn=(2)\n0 400\n+2 600\ntotals: 10000\n")
    own, total = by_object(text)
    assert own == {"ld-linux-x86-64.so.2": 9000, "scrutate": 1000} and total == 10000
    assert sum(v for k, v in own.items() if LOADER.match(k)) == 9000
    limits = accepted_limits("e7d6c9d92da5674dcc169d60ce960978c75ab6c3")
    assert limits["startup/no match"] == 1.05 and limits["startup/loader"] == 1.10
