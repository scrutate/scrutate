"""The benchmark harness publishes no win it has not earned: wrong answers fail a row, rows measured
with other tools are not kept, the corpus and the source tree are the ones recorded, and a tool is
compared only where it gave the same answer."""
import json
import os
import subprocess
import sys

import pytest

BENCH = os.path.join(os.path.dirname(__file__), "..", "..", "bench")
sys.path.insert(0, BENCH)
import compare_tools as C  # noqa: E402
import indexed as I  # noqa: E402
import leaderboard as L  # noqa: E402
import make_benchmarks_md as MB  # noqa: E402
import make_dashboard as MD  # noqa: E402
import make_leaderboard_md as ML  # noqa: E402

BUILT_FROM = os.path.join(BENCH, "competitors", "built_from.sh")


def indexed_data(scrutate_wrong=(), rival_mode="full rebuild (no incremental mode for a directory)", throughput=None):
    row = lambda: {"best": "zoekt", "ratio": 0.5, "ci95": [0.4, 0.6], "status": "PASS"}
    tool = lambda mode, wrong: {"version": "v", "index": {"median": 1.0, "bytes": 1e6, "rss_max": 1e6},
                                "update": {"median": 0.1, "mode": mode}, "latency": {"p50": 0.01, "p95": 0.02, "p99": 0.03, "rss_median": 1e6,
                                                                                  "wrong_queries": list(wrong)},
                                "throughput": {"median": 10.0}}
    comparison = {k: row() for k in ("latency p50", "latency p95", "latency p99", "full indexing time", "incremental update time")}
    comparison["concurrent throughput"] = throughput or row()
    return {"environment": {"platform": "macOS", "cpus": 8, "cpu": "x"}, "corpus": "/c/linux-nodot",
            "settings": {"index_runs": 5, "runs": 20, "queries": 20, "concurrency": 8, "seconds": 15},
            "tools": {"scrutate": tool("incremental", scrutate_wrong), "zoekt": tool(rival_mode, ())},
            "comparison": comparison}


def table(lines):
    return {l.split(" | ")[0].lstrip("| ").split(" (vs")[0]: l for l in lines if l.startswith("| ") and " | " in l}


# ---- leaderboard page: a comparison row without a ratio (scrutate's queries failed, or every rival's)
def test_a_throughput_row_with_no_ratio_is_printed_with_its_reason():
    d = indexed_data(throughput={"runs": 10, "status": "FAIL", "why": "2 of scrutate's queries exited with an error during the runs"})
    rows = table(ML.indexed(d, "label"))
    assert rows["concurrent throughput"] == "| concurrent throughput | - | - | FAIL: 2 of scrutate's queries exited with an error during the runs |"


# ---- indexed: a query scrutate answered wrong fails every latency and throughput row
def test_a_query_scrutate_answered_wrong_fails_the_latency_and_throughput_rows():
    s = I.settle(indexed_data(scrutate_wrong=["PM_RESUME"])["comparison"], indexed_data(scrutate_wrong=["PM_RESUME"])["tools"])
    for k in ("latency p50", "latency p95", "latency p99", "concurrent throughput"):
        assert s[k]["status"] == "FAIL" and "1 of the queries" in s[k]["why"], k
    assert s["full indexing time"]["status"] == "PASS"
    page = table(ML.indexed(indexed_data(scrutate_wrong=["PM_RESUME"]), "label"))
    assert page["latency p50"].endswith("| FAIL: scrutate answered 1 of the queries differently from the reference |")


def test_with_every_answer_right_the_rows_keep_their_status():
    s = I.settle(indexed_data(rival_mode="incremental")["comparison"], indexed_data(rival_mode="incremental")["tools"])
    assert {v["status"] for v in s.values()} == {"PASS"}


# ---- indexed: an update is not compared with a rival's full rebuild
def test_the_update_row_is_not_a_win_when_every_rival_rebuilds():
    d = indexed_data()
    assert I.settle(d["comparison"], d["tools"])["incremental update time"]["status"] == "n/a"
    assert table(ML.indexed(d, "label"))["incremental update time"].endswith("| n/a: every rival's update is a full rebuild |")


# ---- leaderboard.py: rows of an earlier run are kept only when it was measured the same way
def test_a_resumed_run_names_every_change_that_would_mix_its_rows():
    env = {"scrutate_sha256": "a", "tools": {"ugrep": "7.1"}, "kernel": "6.8", "cpu": "x"}
    settings = {"runs": 30, "cache": "warm"}
    data = {"environment": dict(env), "corpora": {"linux": {"tree_sha256": "1"}}, "settings": dict(settings)}
    assert L.run_changes(data, env, data["corpora"], settings) == []
    assert L.run_changes(data, {**env, "tools": {"ugrep": "7.2"}}, data["corpora"], settings) == ["tools"]
    assert L.run_changes(data, {**env, "kernel": "6.9"}, data["corpora"], settings) == ["kernel"]
    assert L.run_changes(data, env, {"linux": {"tree_sha256": "2"}}, settings) == ["corpora"]
    assert L.run_changes(data, env, data["corpora"], {**settings, "runs": 20}) == ["settings"]
    assert L.run_changes(data, {**env, "scrutate_sha256": "b"}, data["corpora"], settings) == ["scrutate_sha256"]


# ---- built_from.sh: the binary is built from the working tree, so it must be a commit's
def _git(repo, *args):
    env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, env=env)


@pytest.mark.parametrize("change", ["edit", "stage", "untracked"])
def test_a_tree_with_changes_no_commit_holds_is_refused(tmp_path, change):
    src, built = tmp_path / "src", tmp_path / "built"
    src.mkdir()
    built.mkdir()
    _git(src, "init", "-q")
    (src / "a.rs").write_text("one\n")
    _git(src, "add", "a.rs")
    _git(src, "commit", "-q", "-m", "one")
    assert subprocess.run(["bash", BUILT_FROM, "write", str(src), str(built)]).returncode == 0
    if change == "untracked":
        (src / "b.rs").write_text("new\n")
    else:
        (src / "a.rs").write_text("two\n")
        if change == "stage":
            _git(src, "add", "a.rs")
    for mode in ("write", "check"):
        r = subprocess.run(["bash", BUILT_FROM, mode, str(src), str(built)], capture_output=True, text=True)
        assert r.returncode == 1 and "no commit holds" in r.stderr, (mode, r.stderr)


def test_results_and_host_facts_written_into_the_tree_are_not_changes(tmp_path):
    src, built = tmp_path / "src", tmp_path / "built"
    src.mkdir()
    built.mkdir()
    _git(src, "init", "-q")
    _git(src, "commit", "-q", "--allow-empty", "-m", "one")
    assert subprocess.run(["bash", BUILT_FROM, "write", str(src), str(built)]).returncode == 0
    (src / "bench" / "results" / "linux-aarch64").mkdir(parents=True)
    (src / "bench" / "results" / "linux-aarch64" / "leaderboard-warm.json").write_text("{}")
    (src / "host-facts.json").write_text("{}")
    assert subprocess.run(["bash", BUILT_FROM, "check", str(src), str(built)]).returncode == 0


# ---- indexed.py: the corpus it edits is put back whatever happens, and checked before a run
def test_the_edited_files_are_restored_when_the_update_phase_fails(tmp_path):
    (tmp_path / "a.c").write_bytes(b"int a;\n")
    (tmp_path / "b.c").write_bytes(b"int b;\n")

    def body(backup):
        for f in backup:
            with open(tmp_path / f, "ab") as fh:
                fh.write(b"/* benchmark edit 0 */\n")
        raise KeyboardInterrupt

    with pytest.raises(KeyboardInterrupt):
        I.edit_and_restore(str(tmp_path), ["a.c", "b.c"], body)
    assert (tmp_path / "a.c").read_bytes() == b"int a;\n" and (tmp_path / "b.c").read_bytes() == b"int b;\n"


def test_the_edited_files_are_restored_when_the_run_gets_sigterm(tmp_path):
    (tmp_path / "a.c").write_bytes(b"int a;\n")
    script = (
        "import os, signal, sys, time\n"
        f"sys.path.insert(0, {BENCH!r})\n"
        "import indexed as I\n"
        "I.exit_on_sigterm()\n"
        "def body(backup):\n"
        "    open(os.path.join(sys.argv[1], 'a.c'), 'ab').write(b'/* benchmark edit 0 */\\n')\n"
        "    os.kill(os.getpid(), signal.SIGTERM)\n"
        "    time.sleep(10)\n"
        "I.edit_and_restore(sys.argv[1], ['a.c'], body)\n"
    )
    p = subprocess.run([sys.executable, "-c", script, str(tmp_path)], capture_output=True, timeout=60)
    assert p.returncode == 128 + 15, p.stderr
    assert (tmp_path / "a.c").read_bytes() == b"int a;\n"


def test_the_dashboard_shows_the_update_row_as_published(tmp_path):
    os.makedirs(tmp_path / "linux-x")
    (tmp_path / "linux-x" / "indexed.json").write_text(json.dumps(indexed_data()))
    (group,) = MD.load_runs(str(tmp_path)).items()
    assert group[1][0]["items"]["incremental update time"]["status"] == "n/a"
    page = MD.section(*group[0], group[1])
    assert "n/a 1 (of" in page and 'class="tag na"' in page and "PASS 8" not in page


def test_a_corpus_that_is_not_the_pinned_tree_stops_the_run(tmp_path, capsys):
    corpus = tmp_path / "linux-nodot"
    corpus.mkdir()
    (corpus / "a.c").write_text("int a;\n")
    digest, n, size = I.corpora.tree_hash(str(corpus))
    (tmp_path / "manifest.json").write_text(json.dumps({"linux-nodot": {"kind": "generated", "tree_sha256": digest, "files": n, "bytes": size}}))
    I.check_corpus(str(corpus))
    (corpus / "a.c").write_text("int a;\n/* benchmark edit 0 */\n")
    with pytest.raises(SystemExit, match="not the tree"):
        I.check_corpus(str(corpus))
    (tmp_path / "manifest.json").unlink()
    with pytest.raises(SystemExit, match="not the tree"):
        I.check_corpus(str(corpus))


# ---- compare_tools.py and BENCHMARKS.md: a tool is compared only where it gave the same answer
def test_two_answers_with_the_same_line_count_do_not_agree():
    a = b"./x.py:1:import a\n./y.py:4:import b\n"
    b = b"x.py:1:import a\ny.py:5:import b\n"
    assert C.count_lines("literal", a) == C.count_lines("literal", b)
    assert C.answer_of("literal", a) != C.answer_of("literal", b)
    assert C.answer_of("literal", a) == C.answer_of("literal", b"x.py:1:import a\ny.py:4:import b\n")
    assert C.answer_of("count", b"./x.py:2\nz.py:0\n") == C.answer_of("count", b"x.py:2\n")
    assert C.answer_of("files", b"./x.py\n") == C.answer_of("files", b"x.py\n")


def text_row(tool, corpus="small", query="common literal", walls=(10.0,) * 30, agrees=True, cold=None):
    r = {"corpus": corpus, "query": query, "tool": tool, "status": "ok", "matches": 3, "wall_median_ms": sorted(walls)[len(walls) // 2],
         "wall_ms": list(walls), "cpu_median_s": 0.5, "maxrss_mb": 9.0, "agrees": agrees}
    if cold is not None:
        r["cold_wall_ms"] = cold
    return r


def bench_data(rows):
    return {"environment": {"cpu": "x", "cores": 8, "mem_gb": 16, "platform": "macOS-15", "date": "d",
                            "tools": {t: {"version": t} for t in MB.TOOL_ORDER}},
            "corpora": {"small": {"files": 1, "lines": 1, "bytes": 1}}, "runs": 30, "text_search": rows, "startup": [], "concurrency": []}


def test_a_tool_with_another_answer_is_left_out_of_the_cpu_memory_and_cold_tables():
    rows = [text_row("scrutate", cold=5.0), text_row("rg", cold=6.0), text_row("ag", agrees=False, cold=1.0)]
    for q in ("rare literal", "regex"):
        rows += [text_row("scrutate", query=q, cold=5.0), text_row("rg", query=q, cold=6.0), text_row("ag", query=q, agrees=False, cold=1.0)]
    page = "\n".join(MB.section(bench_data(rows), "compare.json"))
    cpu = page.split("## CPU time and peak memory")[1].split("##")[0]
    assert "| small | ag |" not in cpu and "| small | ripgrep |" in cpu
    cold = page.split("## Cold cache")[1].split("##")[0]
    assert "| small | common literal | 5.0 | 6.0 | ≠ |" in cold


def summary(page):
    return [l for l in page.split("### Summary across all comparable rows")[1].split("##")[0].splitlines() if l.startswith("| ripgrep")][0]


def test_a_row_counts_as_faster_only_when_its_interval_clears_one():
    import random
    rng = random.Random(1)
    noisy = lambda m: [m + rng.uniform(-3, 3) for _ in range(30)]
    clear = [text_row("scrutate", walls=noisy(10)), text_row("rg", walls=noisy(20))]
    drift = [text_row("scrutate", query="regex", walls=noisy(10)), text_row("rg", query="regex", walls=noisy(10.2))]
    few = [text_row("scrutate", query="rare literal", walls=[10.0] * 6), text_row("rg", query="rare literal", walls=[20.0] * 6)]
    line = summary("\n".join(MB.section(bench_data(clear + drift + few), "compare.json")))
    # 3 rows compared, 1 faster, 0 slower, 2 undecided (one within noise, one under 30 runs)
    assert line.startswith("| ripgrep | 3 | 1 | 0 | 2 |"), line


def test_the_tools_take_turns_within_each_repetition(monkeypatch):
    order = []
    tools = {"scrutate": {"exe": "/s"}, "rg": {"exe": "/rg"}, "grep": {"exe": "/g"}, "git": {"exe": None}}

    def run(cmd, cwd, executable=None, count_only=False):
        order.append(cmd[0])
        return {"wall": 0.01, "out": b"x.py:1:import\n", "err": b"", "rc": 0, "cpu": 0.01, "maxrss_kb": 1}

    monkeypatch.setattr(C, "QUERIES", [C.QUERIES[0]])
    monkeypatch.setattr(C, "timed_rusage", run)
    monkeypatch.setattr(C, "peak_rss_mb", lambda *a, **k: 1.0)
    monkeypatch.setattr(C, "corpus_stats", lambda d: {"bytes": 1})
    rows = C.bench_text_search(tools, {"c": "/nowhere"}, 4, False)
    assert order[3:6] == ["/s", "/rg", "/g"]  # one untimed warm-up run per tool after the answer-checking runs
    timed = order[6:]
    assert [timed[i:i + 3] for i in range(0, 12, 3)] == [["/s", "/rg", "/g"], ["/rg", "/g", "/s"], ["/g", "/s", "/rg"], ["/s", "/rg", "/g"]]
    assert all(r["agrees"] and len(r["wall_ms"]) == 4 for r in rows)


def test_compare_tools_takes_thirty_runs_by_default(monkeypatch):
    seen = {}
    monkeypatch.setattr(sys, "argv", ["compare_tools.py", "--corpus", "c=/nowhere"])
    monkeypatch.setattr(C, "discover", lambda: (_ for _ in ()).throw(SystemExit(0)))
    real = C.argparse.ArgumentParser.parse_args

    def parse(self, *a, **k):
        ns = real(self, *a, **k)
        seen["runs"] = ns.runs
        return ns

    monkeypatch.setattr(C.argparse.ArgumentParser, "parse_args", parse)
    monkeypatch.setattr(C, "drop_scrutate_settings", lambda: None)
    with pytest.raises(SystemExit):
        C.main()
    assert seen["runs"] >= 30


# ---- indexed.py: an index build's peak memory is its own on Linux
def test_an_index_build_s_peak_memory_comes_from_gnu_time_on_linux(tmp_path, monkeypatch):
    fake = tmp_path / "time"
    fake.write_text('#!/bin/sh\n# -f %M -o FILE CMD...\nout=$4; shift 4; "$@"; rc=$?; echo 4321 > "$out"; exit $rc\n')
    fake.chmod(0o755)
    monkeypatch.setattr(I.platform, "system", lambda: "Linux")
    monkeypatch.setattr(I, "GNU_TIME", str(fake))
    w, rss, code, out = I.timed_build(["sh", "-c", "echo built"], str(tmp_path))
    assert (rss, code, out) == (4321 * 1024, 0, b"built\n")


# ---- ripgrep runs without the machine's configuration file
def test_ripgrep_is_run_without_its_config_file():
    for q in C.QUERIES:
        cmd = C.command("rg", {"exe": "/rg"}, q, ".")
        if cmd:
            assert cmd[1] == "--no-config", cmd
