"""The ripgrep comparisons refuse a ripgrep older than the fix for overlapping path arguments."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import differential  # noqa: E402


def test_the_version_is_read_from_ripgreps_first_line():
    assert differential.rg_version("ripgrep 15.2.0 (rev e89fff89ac)\n\nfeatures:+pcre2") == (15, 2, 0)
    assert differential.rg_version("ripgrep 14.1.1\n") == (14, 1, 1)
    assert differential.rg_version("grep (GNU grep) 3.11") == ()
    assert differential.rg_version("") == ()


def test_an_older_ripgrep_is_refused_by_name(tmp_path):
    fake = tmp_path / "rg"
    fake.write_text("#!/bin/sh\necho 'ripgrep 15.1.0'\n")
    fake.chmod(0o755)
    try:
        differential.require_rg(str(fake))
        raise AssertionError("ripgrep 15.1.0 was accepted")
    except SystemExit as e:
        assert "15.1.0" in str(e) and "43e2f08ede" in str(e)
    fake.write_text("#!/bin/sh\necho 'ripgrep 15.2.0 (rev e89fff89ac)'\n")
    assert differential.require_rg(str(fake)) == str(fake)


def test_a_missing_ripgrep_says_how_to_name_one(tmp_path):
    try:
        differential.require_rg(str(tmp_path / "no-such-rg"))
        raise AssertionError("a missing ripgrep was accepted")
    except SystemExit as e:
        assert "no-such-rg" in str(e) and "SCRUTATE_RG" in str(e)
