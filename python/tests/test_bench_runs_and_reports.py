"""What a benchmark result may claim: a verdict rests on at least 30 runs per tool, every result names
the tree it measured, the timing loops warm up and report outliers, a competitor that is not the
latest stable release is named, and the leaderboard shows each tool's interval, scrutate's rank and
the raw file of every row."""
import json
import os
import subprocess
import sys


BENCH = os.path.join(os.path.dirname(__file__), "..", "..", "bench")
sys.path.insert(0, BENCH)
sys.path.insert(0, os.path.join(BENCH, "competitors"))
import compare_tools as C  # noqa: E402
import indexed as I  # noqa: E402
import latest as V  # noqa: E402
import leaderboard as L  # noqa: E402
import make_benchmarks_md as MB  # noqa: E402
import make_leaderboard_md as ML  # noqa: E402

from test_bench_fairness import bench_data, indexed_data, table, text_row  # noqa: E402


# ---- indexed.py: under 30 runs a figure is measured, not ranked
def test_an_indexed_row_under_thirty_runs_is_measured_not_ranked():
    d = indexed_data()
    for k, v in d["comparison"].items():
        v["runs"] = 30 if k.startswith("latency") else 5
    s = I.settle(d["comparison"], d["tools"])
    assert s["latency p50"]["status"] == "PASS"
    assert s["full indexing time"]["status"] == "measured"
    assert "5 runs per tool" in s["full indexing time"]["why"]
    # a wrong answer still fails the latency rows, whatever their runs
    d = indexed_data(scrutate_wrong=["PM_RESUME"])
    for v in d["comparison"].values():
        v["runs"] = 20
    s = I.settle(d["comparison"], d["tools"])
    assert s["latency p50"]["status"] == "FAIL" and s["full indexing time"]["status"] == "measured"


def test_a_file_whose_rows_carry_no_run_count_is_judged_by_its_settings():
    d = indexed_data()  # settings: 20 query runs, 5 index runs, no --throughput-runs (3)
    s = I.settle(d["comparison"], d["tools"], d["settings"])
    assert {k: v["status"] for k, v in s.items()} == {
        "latency p50": "measured", "latency p95": "measured", "latency p99": "measured", "full indexing time": "measured",
        "incremental update time": "n/a", "concurrent throughput": "measured"}
    assert s["concurrent throughput"]["why"].startswith("3 runs per tool")
    assert "| measured: 20 runs per tool, under the 30 a verdict needs |" in table(ML.indexed(d, "label"))["latency p50"]


def test_indexed_takes_thirty_query_and_throughput_runs_by_default():
    a = I.parser().parse_args(["--out", "x.json"])
    assert a.runs >= 30 and a.throughput_runs >= 30 and a.warmup >= 1


def test_an_indexed_result_carries_the_tree_it_measured(tmp_path):
    corpus = tmp_path / "linux-nodot"
    corpus.mkdir()
    (corpus / "a.c").write_text("int a;\n")
    digest, n, size = I.corpora.tree_hash(str(corpus))
    entry = {"kind": "generated", "tree_sha256": digest, "files": n, "bytes": size}
    (tmp_path / "manifest.json").write_text(json.dumps({"linux-nodot": entry}))
    assert I.check_corpus(str(corpus)) == entry
    d = indexed_data()
    d["corpus_manifest"] = entry
    assert f"tree SHA-256 `{digest}`" in "\n".join(ML.indexed(d, "label"))


# ---- outliers: flagged, never dropped
def test_a_run_far_from_the_median_is_counted_as_an_outlier():
    assert L.outliers([1.0, 1.1, 0.9, 1.05, 0.95] * 6) == 0
    assert L.outliers([1.0, 1.1, 0.9, 1.05, 0.95] * 6 + [50.0]) == 1
    assert L.outliers([2.0] * 30) == 0 and L.outliers([]) == 0


def test_compare_tools_warms_each_tool_up_and_reports_outliers(monkeypatch):
    order = []
    tools = {"scrutate": {"exe": "/s"}, "rg": {"exe": "/rg"}, "git": {"exe": None}}
    walls = iter([0.01] * 5 + [5.0] + [0.01] * 100)

    def run(cmd, cwd, executable=None, count_only=False):
        order.append(cmd[0])
        return {"wall": next(walls), "out": b"x.py:1:import\n", "err": b"", "rc": 0, "cpu": 0.01, "maxrss_kb": 1}

    monkeypatch.setattr(C, "QUERIES", [C.QUERIES[0]])
    monkeypatch.setattr(C, "timed_rusage", run)
    monkeypatch.setattr(C, "peak_rss_mb", lambda *a, **k: 1.0)
    monkeypatch.setattr(C, "corpus_stats", lambda d: {"bytes": 1})
    rows = C.bench_text_search(tools, {"c": "/nowhere"}, 30, False)
    # one answer-checking run each, then one untimed warm-up run each, then 30 timed runs each
    assert order[2:4] == ["/s", "/rg"] and len(order) == 2 + 2 + 60
    assert all(len(r["wall_ms"]) == 30 for r in rows)
    assert [r["outliers"] for r in rows] == [0, 1]


def test_benchmarks_md_names_who_timed_what_and_the_outliers():
    rows = [text_row("scrutate"), dict(text_row("rg"), outliers=2)]
    page = "\n".join(MB.section(bench_data(rows), "compare.json"))
    assert "one untimed warm-up run" in page and "os.wait4" in page
    assert "ripgrep 2 of 30" in page


# ---- the latest stable release of each competitor
def test_a_competitor_older_than_the_pinned_latest_release_is_named():
    pinned = {"checked": "2026-11-17", "tools": {"ripgrep": {"latest": "15.2.0"}, "GNU grep": {"latest": "3.12"}, "zoekt": {"latest": None}}}
    got = V.check({"ripgrep": "ripgrep 14.1.1 (rev 4649aa9700)", "GNU grep": "grep (GNU grep) 3.12", "zoekt": "zoekt v0.0.0-x", "ack": "ack v3.10.0"}, pinned)
    assert got["stale"] == [{"tool": "ripgrep", "recorded": "14.1.1", "latest": "15.2.0"}]
    assert got["checked"] == "2026-11-17" and got["online"] == "not asked"
    assert {u["tool"] for u in got["unchecked"]} == {"zoekt", "ack"}


def test_the_online_check_is_optional(monkeypatch):
    pinned = {"checked": "d", "tools": {"ripgrep": {"latest": "15.2.0", "repo": "BurntSushi/ripgrep"}}}
    monkeypatch.setattr(V, "fetch_latest", lambda repo: (_ for _ in ()).throw(OSError("no network")))
    got = V.check({"ripgrep": "ripgrep 15.2.0"}, pinned, online=True)
    assert got["stale"] == [] and got["online"].startswith("skipped")
    monkeypatch.setattr(V, "fetch_latest", lambda repo: "15.3.0")
    got = V.check({"ripgrep": "ripgrep 15.2.0"}, pinned, online=True)
    assert got["newer_upstream"] == [{"tool": "ripgrep", "pinned": "15.2.0", "upstream": "15.3.0"}]


def test_the_pinned_list_matches_what_the_linux_harness_installs():
    pinned = V.load()
    script = open(os.path.join(BENCH, "competitors", "linux_bench.sh")).read()
    for tool, want in (("ripgrep", "ripgrep 15.2.0"), ("ugrep", "ugrep 7.8.5"), ("GNU grep", "GNU grep 3.12"), ("git grep", "git 2.56.1"),
                       ("ack", "ack 3.10.0"), ("ast-grep", "ast-grep 0.51.3")):
        assert pinned["tools"][tool]["latest"] == want.split()[-1], tool
        assert want in script, want


def test_the_pages_name_a_stale_competitor():
    stale = {"checked": "2026-11-17", "stale": [{"tool": "ripgrep", "recorded": "14.1.1", "latest": "15.2.0"}], "unchecked": [], "online": "not asked"}
    d = indexed_data()
    d["version_check"] = stale
    assert "ripgrep `14.1.1`, latest stable `15.2.0`" in "\n".join(ML.indexed(d, "label"))
    b = bench_data([text_row("scrutate"), text_row("rg")])
    b["version_check"] = stale
    assert "ripgrep `14.1.1`, latest stable `15.2.0`" in "\n".join(MB.section(b, "compare.json"))


# ---- the leaderboard page: each tool's interval, scrutate's rank, the raw file of each row
def engine_a_data():
    t = lambda med, n=30: {"status": "ok", "n": n, "times": [med + (i % 5 - 2) * 0.001 for i in range(n)], "median": med}
    row = {"id": "r/literal", "qtype": "literal", "reference": {"lines": 7}, "rawread": {"median": 0.001},
           "tools": {"scrutate": t(0.010), "ripgrep": t(0.020), "ugrep": t(0.005), "ack": t(0.5, n=3), "ag": {"status": "wrong", "missing": 1, "extra": 0}},
           "best_competitor": "ugrep", "ratio": 2.0, "ci95": [1.8, 2.2], "status": "FAIL"}
    return {"environment": {"cpus": 8, "platform": "macOS", "tools": {}, "cpu": "x"}, "settings": {"runs": 30, "warmup": 1, "cache": "warm", "max_load": 4.0},
            "benchmarks": {"r/literal": row}}


def test_each_engine_a_row_has_scrutate_s_rank_each_tool_s_interval_and_its_raw_file():
    lines = ML.engine_a(engine_a_data(), "label", raw="../bench/results/x/leaderboard-warm.json")
    head = next(l for l in lines if l.startswith("| Benchmark"))
    row = next(l for l in lines if l.startswith("| [r/literal]"))
    assert "| rank |" in head
    assert row.startswith("| [r/literal](../bench/results/x/leaderboard-warm.json) (7 lines) |")
    cells = dict(zip([c.strip() for c in head.split("|")], [c.strip() for c in row.split("|")]))
    assert cells["rank"] == "2 of 4"
    # 30 runs: the order-statistic interval of the median; 3 runs are too few for a 95% interval
    assert cells["scrutate"].startswith("10.0 [") and cells["ack"] == "500 (3)"


def test_an_engine_a_row_timed_over_the_run_s_load_limit_says_so():
    # the bounded wait for an idle machine can end with the load still over the run's max_load,
    # far under the CPU count on a large machine (limit 2 on 96 CPUs)
    for load, mark in [(5.2, " (load 5.2, over the 4 limit)"), (9.0, " (load 9.0, over the 4 limit)"), (3.0, "")]:
        d = engine_a_data()
        d["benchmarks"]["r/literal"]["max_load"] = load
        row = next(l for l in ML.engine_a(d, "label") if l.startswith("| r/literal"))
        status = row.rstrip(" |").rsplit("|", 1)[1]
        assert status == " FAIL" + mark, (load, row)


def test_an_engine_a_verdict_under_thirty_runs_is_measured_not_counted():
    # a committed row cut to 20 runs per tool, as leaderboard.py's stop rule can leave one
    path = os.path.join(BENCH, "results", "macos-arm64", "superseded", "leaderboard-warm-9aa0.json")
    with open(path) as f:
        d = json.load(f)
    bid, r = next(iter(d["benchmarks"].items()))
    assert r["status"] == "PASS"
    r["runs"] = 20
    for x in r["tools"].values():
        if x.get("times"):
            x["times"], x["n"] = x["times"][:20], 20
    second = dict(d["benchmarks"][list(d["benchmarks"])[1]])
    d["benchmarks"] = {bid: r, "full": second}
    lines = ML.engine_a(d, "label")
    row = next(l for l in lines if l.startswith(f"| {bid} "))
    assert row.endswith("| measured: 20 runs per tool, under the 30 a verdict needs |"), row
    counts = next(l for l in lines if l.startswith("Status counts, wall time:"))
    assert counts == f"Status counts, wall time: **{second['status']}** 1 (of 1); 1 measured, under 30 runs per tool.\n"
    sys.path.insert(0, os.path.join(BENCH, "..", "tools"))
    import gen_doc_assets as D
    assert D.pass_counts(lines) == ((1, 1) if second["status"] == "PASS" else (0, 1))


def test_the_median_interval_covers_at_least_95_percent():
    assert ML.median_ci([float(i) for i in range(30)]) == (9.0, 20.0)
    assert ML.median_ci([1.0, 2.0, 3.0]) is None


def test_each_indexed_comparison_row_links_its_raw_file():
    lines = ML.indexed(indexed_data(), "label", raw="../bench/results/x/indexed.json")
    assert any(l.startswith("| [latency p50](../bench/results/x/indexed.json) (vs zoekt)") for l in lines)


# ---- run_macos.sh measures a binary built from the commit it names
def test_run_macos_builds_from_a_clean_head_before_it_times_anything():
    script = open(os.path.join(BENCH, "run_macos.sh")).read()
    check = script.index("built_from.sh write")
    build = script.index("cargo build --release -p scrutate --locked")
    assert check < build < script.index("python3 bench/wait_aged.py") < script.index("python3 bench/leaderboard.py")
    assert "set -euo pipefail" in script or "|| exit" in script[check:build + 60]


def test_run_macos_refuses_a_tree_with_changes_no_commit_holds(tmp_path):
    repo = tmp_path / "r"
    (repo / "bench" / "competitors").mkdir(parents=True)
    for f in ("run_macos.sh", "competitors/built_from.sh"):
        (repo / "bench" / f).write_text(open(os.path.join(BENCH, f)).read())
    git = lambda *a: subprocess.run(["git", "-C", str(repo), *a], check=True, capture_output=True)
    git("init", "-q")
    git("add", ".")
    git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "y")
    (repo / "bench" / "edited.txt").write_text("x")
    r = subprocess.run(["bash", str(repo / "bench" / "run_macos.sh")], capture_output=True, text=True, env=dict(os.environ, PATH="/usr/bin:/bin"))
    assert r.returncode != 0 and "changes no commit holds" in r.stderr


def test_publish_scripts_take_every_run():
    for f in ("run_macos.sh", "competitors/linux_bench.sh"):
        s = open(os.path.join(BENCH, f)).read()
        assert "--no-stop-early" in s, f
