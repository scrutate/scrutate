"""tools/fold_changelog.py: a fold of this tree's CHANGELOG gives notes that release, links that
resolve, the full list moved whole, and every release date the same."""
import os
import shutil
import sys

import pytest

ROOT = os.path.normpath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, os.path.join(ROOT, "tools"))
import check_version as C  # noqa: E402
import fold_changelog as F  # noqa: E402
import gen_doc_assets as G  # noqa: E402
import release_notes as R  # noqa: E402

VERSION, DATE = "0.1.0", "2026-12-01"


@pytest.fixture
def folded(tmp_path):
    # after the release the tree holds the fold itself, checked by release_notes.py and check_version.py
    if os.path.exists(os.path.join(ROOT, "docs", "changes", f"{VERSION}.md")):
        pytest.skip(f"{VERSION} is folded in this tree; these tests fold its CHANGELOG before the release")
    for rel in ("CHANGELOG.md", f"docs/changes/{VERSION}-summary.md", "docs/DELIVERY.md", "docs/SECURITY.md", "CITATION.cff"):
        (tmp_path / os.path.dirname(rel)).mkdir(parents=True, exist_ok=True)
        shutil.copy(os.path.join(ROOT, rel), tmp_path / rel)
    assert F.main([VERSION, DATE, "--root", str(tmp_path)]) == 0
    return tmp_path


def read(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def test_the_folded_notes_release_and_the_link_references_are_at_the_end(folded):
    changelog = read(folded / "CHANGELOG.md")
    notes, problems = R.notes(changelog, VERSION)
    assert problems == [] and len(notes) <= R.MAX_CHARS
    assert notes.startswith(f"## [{VERSION}] - {DATE}\n")
    assert changelog.rstrip("\n").split("\n")[-2:] == [
        f"[Unreleased]: https://github.com/scrutate/scrutate/compare/v{VERSION}...HEAD",
        f"[{VERSION}]: https://github.com/scrutate/scrutate/releases/tag/v{VERSION}",
    ]
    assert C.dated(VERSION, str(folded)) == []


def test_the_links_of_both_texts_resolve(folded):
    full = f"docs/changes/{VERSION}.md"
    problems = []
    G.check_links(full, read(folded / full), problems)
    G.check_links("CHANGELOG.md", read(folded / "CHANGELOG.md"), problems)
    # the full list exists only after the fold, in the folded tree
    assert [p for p in problems if full not in p] == []
    assert (folded / full).is_file()


def test_nothing_is_deleted_and_a_second_fold_is_refused(folded):
    before = read(os.path.join(ROOT, "CHANGELOG.md")).split("\n")
    start = before.index("## [Unreleased]")
    moved = read(folded / f"docs/changes/{VERSION}.md")
    # rebased as one text: a code span can run over a line break
    old = F.rebase("\n".join(line for line in before[start + 1:] if not F.LINK_REF.match(line)), "", "docs/changes")
    kept = [line for line in old.split("\n") if line.strip()]
    assert [line for line in kept if line not in moved] == []
    with pytest.raises(SystemExit, match="folded already"):
        F.main([VERSION, DATE, "--root", str(folded)])


def test_a_release_after_the_first_folds_from_unreleased_alone(folded):
    # after 0.1.0, the next release's entries go under Unreleased and it has no section of its own
    path = folded / "CHANGELOG.md"
    after = read(path).replace("## [Unreleased]\n", "## [Unreleased]\n\n### Added\n- `grep --frob` frobs ([CLI](docs/CLI.md)).\n", 1)
    path.write_text(after, encoding="utf-8")
    (folded / "docs/changes/0.2.0-summary.md").write_text("A second release.\n\n### Added\n- Frobbing ([CLI](../CLI.md)).\n", encoding="utf-8")
    assert F.main(["0.2.0", "2026-12-15", "--root", str(folded)]) == 0
    changelog = read(path)
    heads = [line for line in changelog.split("\n") if line.startswith("## ")]
    assert heads[:3] == ["## [Unreleased]", "## [0.2.0] - 2026-12-15", f"## [{VERSION}] - {DATE}"]
    notes, problems = R.notes(changelog, "0.2.0")
    assert problems == [] and "Frobbing" in notes and "grep --frob" not in notes
    assert R.notes(changelog, VERSION)[1] == [], "the earlier release's section stays as it was"
    full = read(folded / "docs/changes/0.2.0.md")
    assert "`grep --frob` frobs ([CLI](../CLI.md))" in full and f"## [{VERSION}]" not in full
    assert changelog.rstrip("\n").split("\n")[-3:] == [
        f"[{VERSION}]: https://github.com/scrutate/scrutate/releases/tag/v{VERSION}",
        "[Unreleased]: https://github.com/scrutate/scrutate/compare/v0.2.0...HEAD",
        "[0.2.0]: https://github.com/scrutate/scrutate/releases/tag/v0.2.0",
    ]


def test_a_missing_summary_is_one_line_and_nothing_is_written(tmp_path):
    shutil.copy(os.path.join(ROOT, "CHANGELOG.md"), tmp_path / "CHANGELOG.md")
    with pytest.raises(SystemExit) as e:
        F.main(["0.2.0", "2026-12-15", "--root", str(tmp_path)])
    assert str(e.value) == "docs/changes/0.2.0-summary.md is missing: write what 0.2.0 is there first (docs/RELEASING.md)"
    assert read(tmp_path / "CHANGELOG.md") == read(os.path.join(ROOT, "CHANGELOG.md"))
    assert not (tmp_path / "docs/changes/0.2.0.md").exists()


def test_a_release_with_nothing_under_unreleased_is_refused(folded):
    # right after a fold, Unreleased is an empty heading: a second release now would describe nothing
    (folded / "docs/changes/0.2.0-summary.md").write_text("A second release.\n", encoding="utf-8")
    before = read(folded / "CHANGELOG.md")
    with pytest.raises(SystemExit, match=r"nothing under `## \[Unreleased\]` to fold into 0.2.0"):
        F.main(["0.2.0", "2026-12-15", "--root", str(folded)])
    assert read(folded / "CHANGELOG.md") == before and not (folded / "docs/changes/0.2.0.md").exists()


def test_an_angle_link_is_left_as_written_for_the_notes_check_to_refuse():
    assert F.rebase("see [c](<../CLI.md>) and [d](../CLI.md)", "docs/changes", "") == "see [c](<../CLI.md>) and [d](docs/CLI.md)"
