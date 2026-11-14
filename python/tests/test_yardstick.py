"""bench/yardstick.py checks every answer before it times one, and names the best rival per workload."""
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import yardstick as Y  # noqa: E402
from rivals import Adapter  # noqa: E402

CALLS: list = []


class Fake(Adapter):
    workloads = frozenset({"W4"})

    def __init__(self, name, answer, delay=0.0, form="resident", missing=None, fails=False):
        self.name, self.answer, self.delay, self.form, self._missing, self.fails = name, answer, delay, form, missing, fails
        self.stopped = False

    def missing(self):
        return self._missing

    def version(self):
        return f"{self.name} 1.0"

    def ask(self, workload, query):
        CALLS.append(self.name)
        if self.fails:
            raise RuntimeError("crashed")
        time.sleep(self.delay)
        return self.answer

    def stop(self):
        self.stopped = True


QUERY = {"id": "W4/point", "workload": "W4", "args": {"name": "Point"}, "expect": [["geo.py", "Point"]]}
RIGHT = {("geo.py", "Point")}


def run(*adapters, runs=4):
    CALLS.clear()
    return Y.measure(list(adapters), [QUERY], runs=runs, warmup=1, corpus="/c", work="/w")


def test_a_wrong_answer_is_reported_and_never_timed():
    out = run(Fake("scrutate-mcp", RIGHT), Fake("rival", {("other.py", "Point")}))
    rows = {r["tool"]: r for r in out["rows"]}
    assert rows["scrutate-mcp"]["status"] == "ok" and len(rows["scrutate-mcp"]["times"]) == 4
    assert rows["rival"]["status"].startswith("wrong") and "times" not in rows["rival"]
    assert "geo.py" in rows["rival"]["status"]


def test_a_missing_tool_says_why_and_a_crash_is_an_error_that_stops_nothing():
    crash = Fake("crashy", RIGHT, fails=True)
    out = run(Fake("scrutate-mcp", RIGHT), Fake("absent", RIGHT, missing="no binary on PATH"), crash)
    rows = {r["tool"]: r for r in out["rows"]}
    assert rows["absent"]["status"] == "missing: no binary on PATH"
    assert rows["crashy"]["status"] == "error: crashed" and crash.stopped
    assert rows["scrutate-mcp"]["status"] == "ok"
    assert out["tools"]["crashy"]["version"] == "crashy 1.0"


def test_runs_alternate_between_the_tools_and_the_fastest_passing_rival_is_named():
    out = run(Fake("scrutate-mcp", RIGHT, 0.004), Fake("slow", RIGHT, 0.008), Fake("quick", RIGHT, 0.001), runs=3)
    timed = CALLS[3:]  # after the three correctness calls: warm-up 1 + 3 runs, each round one call per tool
    assert len(timed) == 12 and all(len(set(timed[i:i + 3])) == 3 for i in range(0, 12, 3))
    best = Y.summary(out["rows"])["W4"]
    assert best["rival"] == "quick" and best["form"] == "resident" and best["ratio"] < 1
    assert Y.summary(run(Fake("scrutate-mcp", RIGHT), Fake("bad", set()))["rows"])["W4"]["reason"].startswith("no rival answered right")


def test_answers_are_compared_as_the_query_says():
    assert Y.check("W4", {("a.py", "f"), ("b.py", "f")}, {"expect": [["a.py", "f"]]}) is None
    assert Y.check("W4", {("a.py", "f"), ("b.py", "f")}, {"expect": [["a.py", "f"]], "exact": True}).startswith("extra")
    assert Y.check("W4", set(), {"expect": [["a.py", "f"]]}).startswith("missing")


class Ranked(Fake):
    workloads = frozenset({"W8"})


def test_a_semantic_tool_is_ranked_by_where_its_first_relevant_hit_falls():
    q = {"id": "W8/1", "workload": "W8", "query": "make a point", "relevant": [["geo.py", 6, 7]]}
    hit = [("other.py", 1, 3), ("geo.py", 7, 9)]  # the second hit overlaps the target's lines
    miss = [("other.py", 1, 3)]
    CALLS.clear()
    out = Y.measure([Ranked("scrutate-mcp", [("geo.py", 6, 7)]), Ranked("finds it second", hit), Ranked("misses", miss)], [q], runs=2, warmup=0, corpus="/c", work="/w")
    ranks = {r["tool"]: r["rank"] for r in out["rows"]}
    assert ranks == {"scrutate-mcp": 1, "finds it second": 2, "misses": None}
    assert all(r["status"] == "ok" and len(r["times"]) == 2 for r in out["rows"])
    quality = Y.summary(out["rows"])["W8"]["quality"]
    assert quality == {"scrutate-mcp": {"mrr10": 1.0, "hit5": 1.0}, "finds it second": {"mrr10": 0.5, "hit5": 1.0}, "misses": {"mrr10": 0.0, "hit5": 0.0}}


def test_a_run_passes_only_when_every_scrutate_form_could_run():
    absent = run(Fake("scrutate-mcp", RIGHT, missing="no binary at /x"), Fake("rival", RIGHT))
    assert not Y.passed(absent)
    assert not Y.passed(run(Fake("scrutate-mcp", RIGHT, fails=True)))
    assert Y.passed(run(Fake("scrutate-mcp", RIGHT), Fake("rival", RIGHT, missing="not on PATH")))


class NoCalls(Fake):
    def ask(self, workload, query):
        raise Y.Unsupported("no call hierarchy")


def test_a_query_scrutate_cannot_express_still_passes():
    assert Y.passed(run(Fake("scrutate-mcp", RIGHT), NoCalls("scrutate-lsp", RIGHT)))


def test_when_scrutate_fails_the_summary_says_so_and_does_not_blame_the_rivals():
    reason = Y.summary(run(Fake("scrutate-mcp", RIGHT, fails=True), Fake("rival", RIGHT))["rows"])["W4"]["reason"]
    assert reason == "scrutate-mcp error: crashed, so no rival has a time to compare"
