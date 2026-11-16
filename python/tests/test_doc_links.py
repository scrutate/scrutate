"""tools/gen_doc_assets.py --check: links and anchors resolve in every tracked Markdown file."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
import gen_doc_assets as D  # noqa: E402


def test_files_without_a_generated_head_are_checked_too():
    files = D.linked_files()
    for path in ("CHANGELOG.md", "README.md", "CONTRIBUTING.md", D.HUB):
        assert path in files, path
    assert not [f for f in files if f.startswith(("docs/research/", "docs/superpowers/"))]


def test_a_link_to_a_heading_that_does_not_exist_is_named():
    problems = []
    D.check_links("CHANGELOG.md", "- A change ([CLI](docs/CLI.md#bash-hook)).\n- Fine ([CLI](docs/CLI.md#setup-and-the-grep-hook)).\n", problems)
    assert problems == ["CHANGELOG.md:1: docs/CLI.md#bash-hook: no such heading"]


def test_outside_a_checkout_the_check_stops_rather_than_read_fewer_files(tmp_path, monkeypatch):
    import pytest
    monkeypatch.setattr(D, "ROOT", str(tmp_path))
    with pytest.raises(SystemExit, match="git ls-files"):
        D.linked_files()
