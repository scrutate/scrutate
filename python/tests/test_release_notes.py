"""tools/release_notes.py: a release's notes hold every change, or the release stops."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
import release_notes as R  # noqa: E402

HEAD = "# Changelog\n\nIntro.\n\n"
OLD = "## [0.1.0] - 2026-11-11\n\n### Added\n- The first release.\n"
LINKS = "\n[Unreleased]: https://example.org/compare/v0.1.0...HEAD\n[0.1.0]: https://example.org/releases/tag/v0.1.0\n"


def test_entries_left_under_unreleased_stop_the_release():
    text = HEAD + "## [Unreleased]\n\n### Changed\n- A change since.\n- Another.\n\n" + OLD + LINKS
    _, problems = R.notes(text, "0.1.0")
    assert problems == ["## [Unreleased] holds 2 lines of entries: fold them into ## [0.1.0] with the tag's date (docs/RELEASING.md, step 1)"]
    assert R.main(["0.1.0", "--changelog", os.devnull]) == 1


def test_a_folded_changelog_gives_the_section_without_its_links():
    text = HEAD + "## [Unreleased]\n\n### Added\n\n" + OLD + LINKS
    out, problems = R.notes(text, "0.1.0")
    assert problems == []
    assert out == "## [0.1.0] - 2026-11-11\n\n### Added\n- The first release.\n"


def test_a_missing_section_or_link_reference_stops_the_release():
    assert R.notes(HEAD + OLD + LINKS, "0.2.0")[1] == ["CHANGELOG.md has no ## [0.2.0] section with entries", "CHANGELOG.md has no link reference [0.2.0]: URL"]
    assert R.notes(HEAD + OLD, "0.1.0")[1] == ["CHANGELOG.md has no link reference [0.1.0]: URL"]


def test_a_fold_that_keeps_the_old_heading_stops_the_release():
    text = HEAD + "## [Unreleased]\n\n## [0.1.0] - 2026-11-17\n\n### Changed\n- The whole train.\n\n" + OLD + LINKS
    assert R.notes(text, "0.1.0")[1] == ["CHANGELOG.md has 2 ## [0.1.0] sections: one version, one section"]


def test_unreleased_in_any_spelling_with_entries_stops_the_release():
    for heading in ("## Unreleased", "## [unreleased]", "## UNRELEASED"):
        text = HEAD + heading + "\n\n- A change since.\n\n" + OLD + LINKS
        assert R.notes(text, "0.1.0")[1] == ["## [Unreleased] holds 1 lines of entries: fold them into ## [0.1.0] with the tag's date (docs/RELEASING.md, step 1)"], heading


def test_notes_longer_than_a_release_body_takes_stop_the_release():
    line = "- " + "x" * 97 + "\n"
    def changelog(n):
        return HEAD + "## [Unreleased]\n\n## [0.1.0] - 2026-11-17\n\n### Added\n" + line * n + LINKS
    fits = (R.MAX_CHARS - len("## [0.1.0] - 2026-11-17\n\n### Added\n")) // len(line)
    out, problems = R.notes(changelog(fits), "0.1.0")
    assert problems == [] and len(out) <= R.MAX_CHARS
    out, problems = R.notes(changelog(fits + 1), "0.1.0")
    assert len(out) > R.MAX_CHARS
    assert problems == [f"the notes of 0.1.0 are {len(out):,} characters, {len(out) - R.MAX_CHARS:,} over the 124,000 a release body takes: shorten its section"]
