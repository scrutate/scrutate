"""bench/yardstick.py checks every answer before it times one, and names the best rival per workload."""
import os
import subprocess
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


def test_a_grep_query_becomes_grep_code_arguments_or_is_unsupported():
    from rivals import scrutate
    g = {"pats": ["a.b"], "fixed": True, "icase": False, "word": False, "multiline": False, "ident": None}
    assert scrutate.grep_args(g, "ignore") == {"pattern": "a.b", "mode": "literal", "case": "sensitive", "word": False, "max_results": 5000}
    two = scrutate.grep_args({**g, "pats": ["a.b", "c|d"], "icase": True}, "ignore")
    assert (two["pattern"], two["mode"], two["case"]) == (r"a\.b|c\|d", "regex", "insensitive")
    assert scrutate.grep_args({**g, "ident": "make_point"}, "ignore")["mode"] == "identifier"
    for bad, mode in [({**g, "multiline": True}, "ignore"), (g, "unrestricted")]:
        try:
            scrutate.grep_args(bad, mode)
            raise AssertionError(f"{bad} {mode} was accepted")
        except Y.Unsupported:
            pass


def test_a_right_rival_without_a_scrutate_row_of_its_form_says_so():
    class Process(Fake):
        workloads = frozenset({"W4"})
    reason = Y.summary(run(Process("scrutate", RIGHT, form="process"), Fake("resident rival", RIGHT))["rows"])["W4"]["reason"]
    assert reason == "no scrutate row of form resident, so no rival has a time to compare"


class Starts(Adapter):
    """An adapter that records its start, which may run a command first."""

    def __init__(self, name, workloads, command=None):
        self.name, self.workloads, self.command = name, frozenset(workloads), command
        self.started = self.stopped = False

    def start(self, corpus, work):
        self.started = True
        if self.command:
            subprocess.run(self.command, check=True)
        return super().start(corpus, work)

    def stop(self):
        self.stopped = True


def test_only_a_tool_a_selected_query_needs_is_started():
    unused, used = Starts("unused", {"W9"}), Starts("used", {"W1"})
    out = Y.measure([unused, used], [{"workload": "W1", "id": "q"}], 1, 0, ".", ".")
    assert not unused.started and used.started
    assert "unused" not in out["tools"]


def test_a_start_past_its_limit_is_that_tools_error_and_the_run_goes_on():
    slow, other = Starts("slow", {"W1"}, ["sleep", "30"]), Starts("other", {"W1"})
    t0 = time.perf_counter()
    out = Y.measure([slow, other], [{"workload": "W1", "id": "q"}], 1, 0, ".", ".", start_limit=1)
    assert time.perf_counter() - t0 < 10
    assert "over 1 s" in out["tools"]["slow"]["error"]
    assert slow.stopped and other.started
    assert [r["status"] for r in out["rows"] if r["tool"] == "slow"] == ["error: StartTimeout: start took over 1 s"]


def test_every_rival_of_a_form_is_timed_against_one_named_scrutate_base():
    # scrutate-lsp comes after scrutate-mcp in adapters(), and is slower here
    out = run(Fake("scrutate-mcp", RIGHT, 0.002), Fake("rival", RIGHT, 0.006), Fake("scrutate-lsp", RIGHT, 0.012), runs=3)
    rival = next(r for r in out["rows"] if r["tool"] == "rival")
    assert rival["base"] == "scrutate-mcp" and rival["ratio"] > 1


def test_a_query_whose_answer_cannot_be_checked_is_never_timed():
    for q in ({"id": "W4/none", "workload": "W4", "args": {"name": "Point"}},
              {"id": "W4/empty", "workload": "W4", "args": {"name": "Point"}, "expect": []}):
        out = Y.measure([Fake("scrutate-mcp", RIGHT), Fake("rival", RIGHT)], [q], runs=2, warmup=0, corpus="/c", work="/w")
        assert all(r["status"].startswith("unchecked") and "times" not in r for r in out["rows"]), out["rows"]
    # an exact empty answer is a check: "no match"
    exact = {"id": "W4/nothing", "workload": "W4", "args": {"name": "Nope"}, "expect": [], "exact": True}
    out = Y.measure([Fake("scrutate-mcp", set())], [exact], runs=2, warmup=0, corpus="/c", work="/w")
    assert out["rows"][0]["status"] == "ok" and len(out["rows"][0]["times"]) == 2


def test_each_query_waits_for_an_idle_machine_and_records_its_load(monkeypatch):
    waits = []
    monkeypatch.setattr(Y.lb, "wait_idle", lambda m, w: waits.append((m, w)) or 0.25)
    q2 = {**QUERY, "id": "W4/point2"}
    out = Y.measure([Fake("scrutate-mcp", RIGHT)], [QUERY, q2], runs=2, warmup=0, corpus="/c", work="/w", max_load=2.0, max_wait=60)
    assert waits == [(2.0, 60), (2.0, 60)]
    assert [r["load"] for r in out["rows"]] == [0.25, 0.25]


def test_rows_give_p99_and_the_interval_uses_the_bootstrap_rule(monkeypatch):
    seen = []
    real = Y.lb.boot_ratio
    monkeypatch.setattr(Y.lb, "boot_ratio", lambda a, b, **kw: seen.append(kw) or real(a, b, B=200))
    out = run(Fake("scrutate-mcp", RIGHT, 0.001), Fake("rival", RIGHT, 0.002), runs=3)
    assert all("p99_ms" in r for r in out["rows"])
    # no B given: boot_ratio's own rule, 10,000 resamples at alpha 0.05
    assert seen and all("B" not in kw for kw in seen)


def test_a_rival_is_compared_with_every_scrutate_form():
    out = run(Fake("scrutate", RIGHT, 0.001, form="process"), Fake("scrutate-mcp", RIGHT, 0.001), Fake("zoekt", RIGHT, 0.002), runs=3)
    zoekt = next(r for r in out["rows"] if r["tool"] == "zoekt")
    assert zoekt["base"] == "scrutate-mcp"
    assert set(zoekt["vs"]) == {"scrutate", "scrutate-mcp"}
    for v in zoekt["vs"].values():
        assert v["ci_low"] <= v["ratio"] <= v["ci_high"] and v["n_base"] == 3 and v["n"] == 3


def test_the_committed_w3_queries_are_indexed_queries():
    import json
    path = os.path.join(os.path.dirname(__file__), "..", "..", "bench", "yardstick_queries", "w3-linux.json")
    with open(path) as f:
        assert json.load(f) == Y.w3_queries()
    assert len(Y.w3_queries()) == 20 and Y.w3_queries()[0]["id"] == "W3/00-literal"


def test_a_caller_the_resolver_could_not_decide_is_no_answer():
    from rivals import scrutate
    result = [{"path": "a.py", "name": "make"}, {"path": "a.py", "name": "go", "undecided": True}]
    assert scrutate.hits(result) == {("a.py", "make")}


def test_the_w5_query_files_are_callers_and_refs_queries_with_evidence():
    import json
    d = os.path.join(os.path.dirname(__file__), "..", "..", "bench", "yardstick_queries")
    for corpus in ("ripgrep", "networkx", "hugo"):
        with open(os.path.join(d, f"w45-{corpus}.json")) as f:
            qs = json.load(f)
        with open(os.path.join(d, f"evidence-{corpus}.json")) as f:
            evidence = {e["id"] for e in json.load(f)}
        w5 = [q for q in qs if q["workload"] == "W5"]
        assert w5 and all(q["kind"] in ("refs", "callers") and q["expect"] for q in w5), corpus
        assert {q["id"] for q in w5} <= evidence, corpus


def test_an_unchecked_query_waits_for_no_idle_machine(monkeypatch):
    waits = []
    monkeypatch.setattr(Y.lb, "wait_idle", lambda m, w: waits.append((m, w)) or 0.25)
    monkeypatch.setattr(Y.lb, "loadavg", lambda: 0.5)
    q = {"id": "W4/none", "workload": "W4", "args": {"name": "Point"}}
    out = Y.measure([Fake("scrutate-mcp", RIGHT)], [q], runs=2, warmup=0, corpus="/c", work="/w", max_load=2.0, max_wait=60)
    assert waits == [] and out["rows"][0]["load"] == 0.5 and out["rows"][0]["status"].startswith("unchecked")


def test_a_row_timed_over_the_load_limit_is_marked(monkeypatch):
    for load, marked in ((3.0, True), (1.0, False)):
        monkeypatch.setattr(Y.lb, "wait_idle", lambda m, w, load=load: load)
        out = Y.measure([Fake("scrutate-mcp", RIGHT)], [QUERY], runs=2, warmup=0, corpus="/c", work="/w", max_load=2.0, max_wait=60)
        row = out["rows"][0]
        assert row["status"] == "ok" and row.get("over_load", False) == marked, row
