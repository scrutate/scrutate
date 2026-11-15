"""docs/LEADERBOARD.md and docs/BENCHMARKS.md are what their scripts write from the results they name."""
import json
import os
import sys

HERE = os.path.dirname(__file__)
sys.path.insert(0, os.path.join(HERE, "..", "..", "tools"))
sys.path.insert(0, os.path.join(HERE, "..", "..", "bench"))
import gen_doc_assets as D  # noqa: E402
import make_leaderboard_md as L  # noqa: E402

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
