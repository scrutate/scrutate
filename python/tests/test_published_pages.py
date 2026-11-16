"""docs/LEADERBOARD.md, docs/BENCHMARKS.md and docs/DEPENDENCIES.md are what their scripts write from the results or the tree they come from."""
import json
import os
import sys

HERE = os.path.dirname(__file__)
sys.path.insert(0, os.path.join(HERE, "..", "..", "tools"))
sys.path.insert(0, os.path.join(HERE, "..", "..", "bench"))
import gen_doc_assets as D  # noqa: E402
import make_leaderboard_md as L  # noqa: E402
import make_benchmarks_md as B  # noqa: E402

ROOT = os.path.join(HERE, "..", "..")


def page(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        return f.read()


def test_the_published_pages_are_what_their_inputs_give():
    for path in D.PAGES:
        assert D.regenerated_problems(path, page(path)) == [], path


def test_a_number_edited_by_hand_is_named():
    # the first measured number in the first table, one digit changed, as the audit changed 581 to 281
    lines = page("docs/LEADERBOARD.md").split("\n")
    row = next(i for i, line in enumerate(lines) if "lines) |" in line)
    at = next(k for k in range(lines[row].index("lines) |") + 8, len(lines[row])) if lines[row][k].isdigit())
    lines[row] = lines[row][:at] + ("2" if lines[row][at] != "2" else "3") + lines[row][at + 1:]
    problems = D.regenerated_problems("docs/LEADERBOARD.md", "\n".join(lines))
    assert problems == [f"docs/LEADERBOARD.md:{row + 1}: differs from what bench/make_leaderboard_md.py writes from the inputs the page names; run it again"]


def test_a_page_that_names_no_inputs_is_named():
    text = "\n".join(l for l in page("docs/BENCHMARKS.md").split("\n") if not l.startswith("<!-- inputs:"))
    assert D.regenerated_problems("docs/BENCHMARKS.md", text) == ["docs/BENCHMARKS.md: names no inputs (<!-- inputs: ... -->): write it with bench/make_benchmarks_md.py"]


def test_a_results_file_moved_under_superseded_keeps_its_platform():
    path = os.path.join(ROOT, "bench", "results", "linux-aarch64", "superseded", "leaderboard-warm-261e.json")
    with open(path, encoding="utf-8") as f:
        assert L.platform_of(json.load(f), path) == "linux-aarch64"
    assert L.platform_of({}, "/x/macos-arm64/leaderboard-warm.json") == "macos-arm64"


def test_the_readme_counts_are_the_leaderboards_pass_counts():
    readme, board = page("README.md"), page("docs/LEADERBOARD.md")
    assert D.readme_count_problems(readme, board) == []
    planted = D.readme_count_problems(readme.replace("all 37 warm-cache", "all 99 warm-cache"), board)
    assert len(planted) == 1 and "says 99" in planted[0] and "has 37 PASS of 37" in planted[0], planted
    words = D.readme_count_problems(readme.replace("leading all eight indexed", "leading all nine indexed"), board)
    assert len(words) == 1 and "says 9" in words[0] and "has 8 PASS of 8" in words[0], words


def test_a_reworded_count_sentence_fails_rather_than_drop_out():
    readme, board = page("README.md"), page("docs/LEADERBOARD.md")
    problems = D.readme_count_problems(readme.replace("leads Zoekt and livegrep on all 8 indexed metrics", "leads Zoekt and livegrep everywhere"), board)
    assert len(problems) == 1 and problems[0].startswith("README.md: no sentence matches"), problems


def test_the_generators_take_their_words_from_the_run_they_print():
    # an index build is "cold" only when the run dropped the page cache for every row
    assert B.build_header([{"page_cache_dropped": True}, {"page_cache_dropped": True}]) == "Index build s (from scratch, page cache dropped)"
    assert B.build_header([{"page_cache_dropped": True}, {"page_cache_dropped": False}]) == "Index build s (from scratch, warm page cache)"
    assert B.build_header([{}]) == "Index build s (from scratch, warm page cache)"
    # the throughput line names the runs the settings hold; a file from before the option ran three
    assert L.throughput_runs({"throughput_runs": 10}) == 10
    assert L.throughput_runs({}) == 3


def test_a_lockfile_page_edited_by_hand_is_named():
    # DEPENDENCIES.md comes from Cargo.toml and `cargo metadata --locked`: a row changed by hand, or a
    # lock changed without the script, differs from what it writes
    assert D.regenerated_problems("docs/DEPENDENCIES.md", page("docs/DEPENDENCIES.md")) == []
    lines = page("docs/DEPENDENCIES.md").split("\n")
    row = next(i for i, line in enumerate(lines) if line.startswith("| anyhow | "))
    lines[row] = lines[row].replace("| anyhow | 1.", "| anyhow | 9.", 1)
    problems = D.regenerated_problems("docs/DEPENDENCIES.md", "\n".join(lines))
    assert problems == [f"docs/DEPENDENCIES.md:{row + 1}: differs from what tools/gen_dependencies_md.py writes; run it again"]


def test_a_generator_given_check_compares_and_never_writes(tmp_path):
    import generated_page as G
    page = tmp_path / "page.md"
    page.write_text("one\ntwo\n")
    assert G.write_or_check(str(page), "one\ntwo\n", "gen.py", ["--check"]) == 0
    assert G.write_or_check(str(page), "one\nthree\n", "gen.py", ["--check"]) == 1
    assert page.read_text() == "one\ntwo\n"
    out = tmp_path / "out.md"
    assert G.write_or_check(str(page), "one\nthree\n", "gen.py", ["--out", str(out)]) == 0
    assert out.read_text() == "one\nthree\n" and page.read_text() == "one\ntwo\n"
