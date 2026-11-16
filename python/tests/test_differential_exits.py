"""bench/differential.py: two exit codes of 2 agree only when both tools give the same reason."""
import os
import random
import shlex
import shutil
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import differential as D  # noqa: E402

NO_FILES_RG = "rg: No files were searched, which means ripgrep probably applied a filter you didn't expect."


def fake(tmp_path, name: str, err: str, rc: int) -> str:
    """A tool that writes `err` to stderr and exits `rc`, whatever it is asked."""
    p = tmp_path / name
    p.write_text(f"#!/bin/sh\nprintf '%s\\n' {shlex.quote(err)} >&2\nexit {rc}\n")
    p.chmod(0o755)
    return str(p)


def test_the_reasons_both_tools_give():
    assert D.reason(b"scrutate: error: no files were searched: a filter left out every file") == "no file searched"
    assert D.reason(NO_FILES_RG.encode()) == "no file searched"
    assert D.reason(b'scrutate: error: invalid pattern "(": regex parse error:') == "bad pattern"
    assert D.reason(b"rg: regex parse error:") == "bad pattern"
    assert D.reason(b"rg: PCRE2: error compiling pattern at offset 3") == "bad pattern"
    assert D.reason(b"scrutate: internal error: attempt to subtract with overflow") is None


def test_a_scrutate_that_exits_2_for_another_reason_is_a_disagreement(tmp_path, monkeypatch):
    monkeypatch.setattr(D, "RG", fake(tmp_path, "rg", NO_FILES_RG, 2))
    monkeypatch.setattr(D, "SCRUTATE", fake(tmp_path, "panics", "scrutate: internal error: attempt to subtract with overflow", 2))
    why = D.one_case(random.Random(3), False, {"empty": 0})
    assert why and "exit codes differ" in why and "internal error" in why, why
    # the same reason on both sides is no disagreement
    monkeypatch.setattr(D, "SCRUTATE", fake(tmp_path, "nofiles", "scrutate: error: no files were searched: a filter left out every file", 2))
    assert D.one_case(random.Random(3), False, {"empty": 0}) is None


@pytest.mark.skipif(os.name != "posix" or os.geteuid() == 0, reason="a mode of 0 stops no read here")
def test_an_unreadable_file_is_not_late_and_rg_reports_it(tmp_path):
    (tmp_path / "late.bin").write_bytes(b"x\n" * 40_000 + b"\0 tail\n")
    locked = tmp_path / "locked.txt"
    locked.write_bytes(b"tail\n")
    locked.chmod(0)
    try:
        assert D.late_nul(str(tmp_path)) == ["late.bin"]
        if shutil.which(D.RG) or os.path.isfile(D.RG):
            for form in (["-c"], ["-l"], ["--json"]):
                errors: list[bytes] = []
                rc, _ = D.run_rg([D.RG, *form, "tail"], str(tmp_path), ["late.bin"], errors=errors)
                one, _, err = D.run_err([D.RG, *form, "tail"], str(tmp_path))
                assert (rc, D.reason(b"".join(errors))) == (one, D.reason(err)), form
    finally:
        locked.chmod(0o644)
