"""bench/indexed.py names every missing tool binary before its hour-long index phase starts."""
import os
import sys
import types

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import indexed  # noqa: E402


def test_a_missing_binary_is_named_with_the_script_that_builds_it(tmp_path):
    lg = tmp_path / "livegrep"
    lg.mkdir()
    (lg / "codesearch").write_text("#!/bin/sh\n")
    (lg / "codesearch").chmod(0o755)
    defs = indexed.tool_defs(types.SimpleNamespace(work=str(tmp_path), corpus=str(tmp_path), livegrep_bin=str(lg)))
    missing = indexed.missing_binaries(defs, ["livegrep"])
    assert missing == [f"livegrep: {lg}/lgq (bench/competitors/livegrep_query/build.sh builds it)"]
    (lg / "lgq").write_text("#!/bin/sh\n")
    (lg / "lgq").chmod(0o755)
    assert indexed.missing_binaries(defs, ["livegrep"]) == []
