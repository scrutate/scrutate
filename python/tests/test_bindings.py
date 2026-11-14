"""Tests for the PyO3 bindings (`pip install` the wheel from crates/scrutate-py first).

Skipped when the native module is not installed, so the pure-Python reference tests still run.
"""
import json
import os
import signal
import subprocess
import sys
import threading
import time

import pytest

scrutate = pytest.importorskip("scrutate")

CLI = os.environ.get("SCRUTATE_BIN", os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "scrutate"))


@pytest.fixture()
def repo(tmp_path):
    (tmp_path / "pkg").mkdir()
    for i in range(20):
        (tmp_path / "pkg" / f"m{i}.py").write_text(
            f'"""Module {i}: helpers for widget {i}."""\n\nimport os\n\n\ndef make_widget_{i}(size):\n    """Build widget number {i}."""\n    return os.path.join("w", str(size))\n\n\ndef use_{i}():\n    return make_widget_{i}(1)\n'
        )
    (tmp_path / "README.md").write_text("# demo\nneedle in the docs\n")
    return tmp_path


def test_grep_streams_and_matches_cli(repo):
    it = scrutate.grep("import os", [str(repo)], mode="literal")
    first = next(it)
    assert isinstance(first, scrutate.Match)
    rest = list(it)
    got = sorted((m.path, m.line, m.text) for m in [first] + rest)
    assert len(got) == 20
    st = it.stats()
    assert st["matches"] == 20 and st["files_matched"] == 20 and not st["cancelled"]
    # a binary the caller named must be there: a missing one fails, it does not skip the comparison
    if "SCRUTATE_BIN" in os.environ or os.path.exists(CLI):
        out = subprocess.run([os.path.abspath(CLI), "grep", "-F", "--json", "import os"], cwd=repo, capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout
        cli = sorted((r["path"], r["line"], r["text"]) for r in map(json.loads, out.splitlines()) if r.get("type") == "match")
        assert cli == got



def test_grep_types_are_the_custom_languages_of_the_index_it_starts_in(tmp_path, monkeypatch):
    # another Repo of the process loaded its own tables last: a custom -t still names this tree's
    a, b = tmp_path / "a", tmp_path / "b"
    for d in (a, b):
        (d / ".git").mkdir(parents=True)
    (a / ".scrutate.toml").write_text('[[language]]\nname = "onlya"\nextensions = ["oa"]\n')
    (a / "x.oa").write_text("needle\n")
    (a / "y.txt").write_text("needle\n")
    (b / "z.py").write_text("x = 1\n")
    scrutate.Repo(str(b)).symbol("x")
    monkeypatch.chdir(a)
    found = [m.path for m in scrutate.grep("needle", ["."], types=["onlya"])]
    assert len(found) == 1 and found[0].endswith("x.oa"), found

def test_grep_modes_and_errors(repo):
    assert len(list(scrutate.grep(r"def make_widget_\d+", [str(repo)]))) == 20
    # identifier mode: one spelling finds every casing of the whole identifier (def + call in m3.py)
    assert len(list(scrutate.grep("makeWidget3", [str(repo)], mode="identifier"))) == 2
    assert len(list(scrutate.grep("MAKE_WIDGET_3", [str(repo)], mode="identifier"))) == 2
    assert len(list(scrutate.grep("neddle", [str(repo)], mode="fuzzy", fuzzy_edits=1))) == 1
    with pytest.raises(ValueError):
        scrutate.grep("(unclosed", [str(repo)])
    with pytest.raises(ValueError):
        scrutate.grep("x", [str(repo)], mode="nope")


def test_grep_cancel_and_max_results(repo):
    it = scrutate.grep("e", [str(repo)])
    next(it)
    it.cancel()
    assert list(it) == []
    hits = list(scrutate.grep("e", [str(repo)], max_results=5))
    assert len(hits) == 5


def test_repo_index_and_intelligence(repo):
    r = scrutate.Repo(str(repo))
    st = r.index()
    assert st["files_total"] == 21 and st["chunks"] > 40
    assert r.index()["files_changed"] == 0  # incremental: nothing to do
    hits = r.search("build a widget", k=5)
    assert hits and all(isinstance(h, scrutate.Hit) for h in hits)
    assert any(h.name.startswith("make_widget_") for h in hits)
    sym = r.symbol("make_widget_3")
    assert sym and sym[0].path == "pkg/m3.py"
    callers = r.callers("make_widget_3")
    assert [c.name for c in callers] == ["use_3"]
    res, ext = r.callees("use_3")
    assert [h.name for h in res] == ["make_widget_3"]
    outline = r.outline("pkg/m3.py")
    assert [o["kind"] for o in outline] == ["module", "function", "function"]
    deps = r.dependencies("pkg/m3.py")
    assert deps == [(3, "os", None)]
    assert len(r.dependents("os")) == 20
    sim = r.similar("make_widget_3", k=3)
    assert sim and all(-1.0 <= c <= 1.0 for _, c, _ in sim)
    dup = r.duplicates(min_tokens=3, threshold=0.5, limit=5)
    assert isinstance(dup, list)
    s = r.stats()
    assert s["files"] == 21 and s["symbols"] == 41  # 40 functions + the README heading
    assert "Module 3" in r.read("pkg/m3.py", 1, 1)
    with pytest.raises(FileNotFoundError):
        r.read("nope.py")
    with pytest.raises(ValueError):
        r.read("/etc/passwd")


def test_paths_and_locations_read_as_the_mcp_tools_read_them(repo):
    r = scrutate.Repo(str(repo))
    r.index()
    # a location an earlier answer printed, and another spelling of a path
    sym = r.symbol("make_widget_3")[0]
    at = r.symbol(f"{sym.path}:{sym.start}-{sym.end}")
    assert [(h.path, h.name) for h in at] == [("pkg/m3.py", "make_widget_3")]
    assert r.outline("./pkg/m3.py") == r.outline("pkg/m3.py") and r.outline("pkg/m3.py")
    assert r.outline("pkg/../pkg/m3.py") == r.outline("pkg/m3.py")
    assert r.dependencies("./pkg/m3.py") == [(3, "os", None)]
    assert r.dependents("./pkg/m3.py") == r.dependents("pkg/m3.py")

    def similar(target):
        return [(h.path, h.start) for h, _, _ in r.similar(target, k=3)]

    want = similar("pkg/m3.py:6")
    assert want
    for target in ["pkg/../pkg/m3.py:6", "./pkg/m3.py:6", os.path.join(str(repo), "pkg", "m3.py") + ":6"]:
        assert similar(target) == want, target
    # a path that leaves the root and comes back by its parts is read from the root; one that
    # leaves and stays out is not looked at
    assert similar(f"../{repo.name}/pkg/m3.py:6") == want
    with pytest.raises(RuntimeError, match="no symbol or location matches"):
        r.similar(f"../{repo.name}_elsewhere/pkg/m3.py:6")
    # an outside path answers the same whether or not a file is there, a missing one inside differently
    with pytest.raises(ValueError):
        r.read("../no_such_file_outside_the_repo")
    with pytest.raises(ValueError):
        r.read("/no/such/file/outside/the/repo")
    with pytest.raises(FileNotFoundError):
        r.read("pkg/nope.py")
    os.symlink("/etc/hosts", os.path.join(str(repo), "leaves.txt"))
    with pytest.raises(FileNotFoundError):
        r.read("leaves.txt")


def test_a_cut_answer_says_where_it_stopped(repo):
    (repo / "cut.py").write_text(
        "def target():\n    return 1\n\n\n"
        + "".join(f"def caller_{i}():\n    return target()\n\n\n" for i in range(3))
        + "def fan():\n    return caller_0() + caller_1() + caller_2()\n"
    )
    shapes = [
        "    total = compute_alpha(x)\n    shifted = adjust_beta(total)\n    return finalize_gamma(shifted)\n",
        "    items = load_records(x)\n    kept = filter_entries(items)\n    return store_results(kept)\n",
        "    conn = open_socket(x)\n    reply = send_request(conn)\n    return parse_payload(reply)\n",
    ]
    for i, body in enumerate(shapes):
        for copy in "ab":
            (repo / f"dup_{i}{copy}.py").write_text(f"def dup(x):\n{body}")
    r = scrutate.Repo(str(repo))
    r.index()
    calls = {
        "symbol": lambda n: r.symbol("dup", limit=n),
        "callers": lambda n: r.callers("target", limit=n),
        "callees": lambda n: r.callees("fan", limit=n)[0],
        "duplicates": lambda n: r.duplicates(min_tokens=3, threshold=0.95, limit=n),
    }
    def key(items):
        return [x if isinstance(x, dict) else (x.path, x.start) for x in items]

    for name, call in calls.items():
        whole = call(1000)
        assert isinstance(whole, list) and len(whole) >= 3 and whole.stopped_at is None, name
        cut = call(2)
        assert len(cut) == 2 and cut.stopped_at == 2, name
        assert key(cut) == key(whole[:2]), name
        exact = call(len(whole))
        assert key(exact) == key(whole) and exact.stopped_at is None, name


def test_similar_with_k_zero_leaves_the_repo_usable(repo):
    r = scrutate.Repo(str(repo))
    r.index()
    assert r.similar("make_widget_3", 0) == []
    assert r.symbol("make_widget_3")


def test_the_largest_counts_do_not_wrap_around(tmp_path):
    near = "    total = compute_alpha(x)\n    shifted = adjust_beta(total)\n    kept = filter_entries(shifted)\n    more = store_results(kept)\n    again = parse_payload(more)\n    final = send_request(again)\n"
    (tmp_path / "a.py").write_text(f"def f_a(x):\n{near}    return finalize_gamma(final, alpha_one)\n")
    (tmp_path / "b.py").write_text(f"def f_b(x):\n{near}    return finalize_gamma(final, omega_two)\n")
    r = scrutate.Repo(str(tmp_path))
    r.index()
    assert r.duplicates(min_tokens=3)
    assert r.duplicates(min_tokens=2**64 - 1) == [] and r.duplicates(min_tokens=2**63) == []
    assert r.read("a.py", 1, max_lines=-(2**63)) == ""
    assert r.read("a.py", 1, max_lines=2**63 - 1).splitlines()[0].endswith("def f_a(x):")


def test_exclude_dirs_of_the_root_s_file_applies_to_every_process(tmp_path, monkeypatch):
    monkeypatch.delenv("SCRUTATE_INDEX_EXCLUDE_DIRS", raising=False)
    (tmp_path / ".git").mkdir()
    (tmp_path / ".git" / "HEAD").write_text("ref: refs/heads/main\n")
    (tmp_path / ".scrutate.toml").write_text('[index]\nexclude_dirs = ["gen"]\n')
    for rel, name in [("gen/g.py", "g_fn"), ("src/s.py", "s_fn")]:
        (tmp_path / rel).parent.mkdir(exist_ok=True)
        (tmp_path / rel).write_text(f"def {name}():\n    return 1\n")
    sub = tmp_path / "sub"
    sub.mkdir()
    (sub / ".scrutate.toml").write_text('[index]\nexclude_dirs = ["src"]\n')
    r = scrutate.Repo(str(tmp_path))
    r.index()
    assert r.symbol("g_fn") == [] and r.symbol("s_fn")
    # another process started in a folder with its own file updates the same index the same way
    code = f"import scrutate; r = scrutate.Repo({str(tmp_path)!r}); r.index(); print(len(r.symbol('g_fn')), len(r.symbol('s_fn')))"
    out = subprocess.run([sys.executable, "-c", code], cwd=sub, capture_output=True, text=True, check=True).stdout
    assert out.split() == ["0", "1"]
    if "SCRUTATE_BIN" in os.environ or os.path.exists(CLI):
        cli = subprocess.run([os.path.abspath(CLI), "symbol", "g_fn"], cwd=sub, capture_output=True, text=True, stdin=subprocess.DEVNULL)
        assert cli.returncode == 1, cli.stdout + cli.stderr
        assert subprocess.run([os.path.abspath(CLI), "symbol", "s_fn"], cwd=sub, capture_output=True, stdin=subprocess.DEVNULL).returncode == 0
    assert r.symbol("g_fn") == [] and r.symbol("s_fn")


def test_spans_and_column_count_bytes_of_the_text(tmp_path):
    (tmp_path / "bad.txt").write_bytes(b"\xff abc and \xfe abc\r\n")
    (m,) = scrutate.grep("abc", [str(tmp_path)], mode="literal")
    raw = m.text.encode()
    assert [raw[a:b] for a, b in m.spans] == [b"abc", b"abc"]
    assert raw[m.column - 1 : m.column + 2] == b"abc"


def test_grep_argument_errors_at_call_time(repo):
    with pytest.raises(ValueError):
        scrutate.grep("x", [])
    with pytest.raises(ValueError):
        scrutate.grep("x", [str(repo)], globs=["a[b"])
    with pytest.raises(ValueError):
        scrutate.grep("x", [str(repo)], types=["no-such-type"])
    with pytest.raises(FileNotFoundError):
        scrutate.grep("x", [str(repo / "missing")])


def test_grep_sort_gives_path_and_line_order(repo):
    got = [(m.path, m.line) for m in scrutate.grep("e", [str(repo)], sort=True)]
    assert len(got) > 100 and got == sorted(got)


def test_grep_sort_compares_paths_part_by_part(tmp_path):
    # as `rg --sort path` and the CLI: a/b.txt before a-b.txt, though "-" sorts before "/"
    (tmp_path / "a").mkdir()
    (tmp_path / "a" / "b.txt").write_text("x\n")
    (tmp_path / "a-b.txt").write_text("x\n")
    got = [m.path.replace("\\", "/") for m in scrutate.grep("x", [str(tmp_path)], sort=True)]
    assert got == ["a/b.txt", "a-b.txt"]


def test_grep_sort_with_max_results_is_the_first_in_path_order(tmp_path):
    for i in range(300):
        (tmp_path / f"f{i:03}.txt").write_text("needle\n")
    want = [f"f{i:03}.txt" for i in range(5)]
    for _ in range(10):
        got = [m.path for m in scrutate.grep("needle", [str(tmp_path)], sort=True, max_results=5, threads=8)]
        assert got == want


def test_cancel_lets_other_python_threads_run(tmp_path):
    # one big file with no match: the search checks for a stop only between files and at matches
    words = ["alpha", "beta", "gamma", "delta", "omega", "sigma", "kappa", "lambda"]
    line = " ".join(words) + " " + " ".join(reversed(words)) + "\n"
    (tmp_path / "big.txt").write_text(line * (10 * 2**20 // len(line)))
    it = scrutate.grep(r"zqxjvkwpyhgb\w+", [str(tmp_path)], mode="fuzzy_regex", fuzzy_edits=3, threads=1)
    turns, stop = [0], threading.Event()

    def spin():
        while not stop.is_set():
            turns[0] += 1

    t = threading.Thread(target=spin)
    s0 = time.perf_counter()
    t.start()
    time.sleep(0.05)
    before, t0 = turns[0], time.perf_counter()
    it.cancel()
    took, during = time.perf_counter() - t0, turns[0] - before
    stop.set()
    t.join()
    if took < 0.05:
        pytest.skip(f"the search ended before cancel() had to wait ({took:.3f} s)")
    expected = before / (t0 - s0) * took
    assert during >= 0.2 * expected, f"cancel() took {took:.3f} s; the other thread ran {during} turns of {expected:.0f}"


def _interrupt_latency(call, delay):
    """Seconds from a SIGINT sent `delay` s into `call()` to the KeyboardInterrupt; None if it finished first."""
    timer = threading.Timer(delay, os.kill, (os.getpid(), signal.SIGINT))
    t0 = time.perf_counter()
    timer.start()
    try:
        call()
    except KeyboardInterrupt:
        return time.perf_counter() - t0 - delay
    finally:
        timer.cancel()
    return None


@pytest.mark.skipif(not hasattr(signal, "SIGINT") or os.name == "nt", reason="needs POSIX signals")
def test_ctrl_c_interrupts_index_and_grep(tmp_path):
    body = "".join(f"def f_{i}_{{n}}(a, b):\n    return [a * {i}, b + {i}, str(a) + str(b)]\n\n" for i in range(60))
    for n in range(3000):  # about 2 s to index on an M-series laptop
        (tmp_path / f"m{n}.py").write_text(body.format(n=n))
    r = scrutate.Repo(str(tmp_path))
    late = _interrupt_latency(lambda: r.index(force=True), 0.2)
    if late is None:
        pytest.skip("index finished before the signal")
    assert late < 1.0, f"KeyboardInterrupt came {late:.2f}s after the signal"
    # the interrupted update stopped and was rolled back: the next call starts from the index as it was
    assert r.index()["files_changed"] == 3000
    assert r.symbol("f_3_7")
    # each line costs PCRE2 about 15 ms of backtracking: a few seconds in all
    slow = tmp_path / "slow"
    slow.mkdir()
    (slow / "s.txt").write_text("".join(f"line {i} with some words to scan\n" for i in range(200)))
    late = _interrupt_latency(lambda: list(scrutate.grep(r"(\w+\s?)*\d{12}$", [str(slow)], mode="pcre2", threads=1)), 0.3)
    if late is None:
        pytest.skip("grep finished before the signal")
    assert late < 1.0, f"KeyboardInterrupt came {late:.2f}s after the signal"


def test_gil_is_released_during_rust_work(repo):
    # one big file (searched by one thread, below the size where a file is split across threads)
    # and a single match, so the time is Rust scanning, not Python building result objects
    big = repo / "big.txt"
    big.write_text("".join(f"line {i} with some words to scan\n" for i in range(1_500_000)))

    counts = []

    def work():
        counts.append(len(list(scrutate.grep("line 1499999 with", [str(repo)], mode="literal"))))

    t0 = time.perf_counter()
    work()  # warm the cache
    # enough rounds per thread that thread start-up is noise (one search takes 17 ms on some runners)
    rounds = max(1, int(0.1 / max(time.perf_counter() - t0, 1e-3)) + 1)

    def job():
        for _ in range(rounds):
            work()

    t0 = time.perf_counter()
    for _ in range(4):
        job()
    seq = time.perf_counter() - t0
    threads = [threading.Thread(target=job) for _ in range(4)]
    t0 = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    par = time.perf_counter() - t0
    # an assert inside a thread only warns, so the counts are checked here
    assert counts == [1] * (1 + 8 * rounds), counts
    cpus = os.cpu_count() or 1
    load = os.getloadavg()[0] if hasattr(os, "getloadavg") else 0.0
    if cpus < 2 or load > cpus * 0.5:
        pytest.skip(f"needs idle cores to measure concurrency (cpus={cpus}, load={load:.1f}); sequential {seq:.3f}s parallel {par:.3f}s")
    assert par < seq * 0.9, f"threads did not run concurrently: sequential {seq:.3f}s parallel {par:.3f}s"


def test_the_operations_reach_python(repo):
    r = scrutate.Repo(str(repo))
    r.index()
    refs = r.references("make_widget_3")
    assert [d["path"] for d in refs["definitions"]] == ["pkg/m3.py"]
    assert [o["line"] for o in refs["definitions"][0]["references"] if o["via"] != "definition"] == [12]
    assert r.references("make_widget_3", textual=True)["references"]
    callers = r.callers("make_widget_3")
    assert callers.undecided == 0 and not hasattr(callers, "unchecked")
    # any operation by its MCP tool name, with the same arguments, text and paging
    a = r.call("grep_code", {"pattern": "MAKE_WIDGET_3", "mode": "identifier", "path": "pkg"})
    assert a["schema"] == 2 and "pkg/m3.py" in a["text"]
    assert {g["path"] for g in a["result"]["groups"]} == {"pkg/m3.py"}
    assert r.call("grep_code", {"pattern": "make_widget_3", "globs": ["*.md"]})["result"]["groups"] == []
    whole = r.call("find_symbol", {"name": "use_3"})
    assert whole["result"][0]["path"] == "pkg/m3.py" and whole["text"].startswith("pkg/m3.py:")
    page = r.call("dependents", {"target": "os"}, max_bytes=200)
    assert len(page["text"].encode()) <= 200 and "offset=" in page["text"]
    assert r.call("dependents", {"target": "os"}, offset=3)["text"].count("\n") + 1 == 17
    with pytest.raises(ValueError):
        r.call("no_such_tool")
    with pytest.raises(ValueError):
        r.call("grep_code", {"pattern": "x", "mode": "nope"})


def test_a_note_on_a_file_reaches_the_stats(tmp_path):
    filler = "".join(f"x = {i}  # filler line to push the NUL past the first read\n" for i in range(3000))
    (tmp_path / "nul.py").write_text("needle = 1\n" + filler + "\x00\nneedle()\n")
    it = scrutate.grep("needle", [str(tmp_path)], mode="literal")
    assert [m.line for m in it] == [1]
    notes = it.stats()["notes"]
    assert len(notes) == 1 and notes[0][0].endswith("nul.py") and "binary" in notes[0][1], notes


def test_calls_from_several_threads_run_side_by_side_and_see_an_edit(repo):
    r = scrutate.Repo(str(repo))
    r.index()
    want = [h.path for h in r.symbol("make_widget_3")]
    out, errors = [], []

    def call():
        try:
            for _ in range(20):
                out.append([h.path for h in r.symbol("make_widget_3")])
        except Exception as e:  # a failure in a thread must fail the test
            errors.append(e)

    threads = [threading.Thread(target=call) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert not errors and len(out) == 160 and all(o == want for o in out)
    (repo / "pkg" / "late.py").write_text("def arrived_late():\n    return 1\n")
    r.index()
    assert [h.path for h in r.symbol("arrived_late")] == ["pkg/late.py"]
    assert r.stats()["files"] >= 21
