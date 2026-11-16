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
