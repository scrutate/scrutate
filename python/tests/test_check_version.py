"""tools/check_version.py: every version and every release date agree, or the release stops."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
import check_version as C  # noqa: E402


def test_the_tree_agrees_with_its_own_version():
    version = C.declared()["crates/scrutate/Cargo.toml"]
    assert {f: v for f, v in C.declared().items() if v != version} == {}
    assert C.dated(version) == []


def test_a_date_the_fold_left_behind_stops_the_release(tmp_path):
    (tmp_path / "docs").mkdir()
    (tmp_path / "CHANGELOG.md").write_text("## [Unreleased]\n\n## [0.1.0] - 2026-11-30\n\n- The first release.\n")
    (tmp_path / "docs/DELIVERY.md").write_text("# Delivery index: Scrutate 0.1.0 (2026-11-30)\n")
    (tmp_path / "docs/SECURITY.md").write_text("Scope: the binary, as\nbuilt from this repository on 2026-11-11. More.\n")
    assert C.dated("0.1.0", str(tmp_path)) == ["docs/SECURITY.md says 2026-11-11, CHANGELOG.md's 0.1.0 section 2026-11-30"]
    assert C.dated("0.2.0", str(tmp_path)) == ["CHANGELOG.md has no dated ## [0.2.0] section"]
