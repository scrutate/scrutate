"""tools/gen_doc_assets.py: RELEASE_READINESS.md's verdict names every platform INSTALL.md supports."""
import os
import sys

ROOT = os.path.join(os.path.dirname(__file__), "..", "..")
sys.path.insert(0, os.path.join(ROOT, "tools"))
import gen_doc_assets as G  # noqa: E402


def read(path):
    with open(os.path.join(ROOT, path), encoding="utf-8") as f:
        return f.read()


def test_the_verdict_names_every_supported_platform():
    assert G.platform_problems(read("docs/INSTALL.md"), read("docs/RELEASE_READINESS.md")) == []


def test_a_verdict_that_kept_three_of_six_platforms_fails():
    old = "**Verdict: 0.1.0 is a verified first release on the platforms INSTALL.md declares (macOS arm64,\nLinux aarch64, Linux x86_64).**\n\n"
    problems = G.platform_problems(read("docs/INSTALL.md"), old)
    assert [p.split(" name ")[1].split(",")[0] for p in problems] == ["macOS x86_64", "Windows x86_64", "Windows ARM64"]
    assert G.platform_problems("no table", old) == ["docs/INSTALL.md or docs/RELEASE_READINESS.md: no platform table or no verdict to compare"]


def test_a_verdict_that_names_a_platform_only_to_rule_it_out_fails():
    install = read("docs/INSTALL.md")
    good = read("docs/RELEASE_READINESS.md")
    for said in ("and Windows x86_64 (not on Windows ARM64)", "and Windows x86_64, but not yet Windows ARM64", "and Windows x86_64, except Windows ARM64"):
        bad = good.replace("Windows x86_64 and Windows\nARM64", said, 1)
        assert bad != good
        assert G.platform_problems(install, bad) == ["docs/RELEASE_READINESS.md: the verdict does not name Windows ARM64, which docs/INSTALL.md supports"], said


def test_every_platform_row_names_each_wheel_the_release_builds():
    assert G.wheel_problems(read("docs/INSTALL.md"), read(".github/workflows/release.yml")) == []


def test_a_row_without_the_abi3t_wheel_or_a_kind_with_no_tag_fails():
    install, release = read("docs/INSTALL.md"), read(".github/workflows/release.yml")
    row = next(line for line in install.split("\n") if line.startswith("| Windows ARM64 "))
    bad = install.replace(row, row.replace("abi3t", "abi3"))
    assert G.wheel_problems(bad, release) == ["docs/INSTALL.md: the Windows ARM64 row names no `abi3t` wheel, which release.yml builds"]
    more = release.replace("kind: [abi3, free-threaded, abi3t]", "kind: [abi3, free-threaded, abi3t, pypy]")
    assert more != release
    assert G.wheel_problems(install, more) == ["tools/gen_doc_assets.py: no wheel tag known for release.yml's kind `pypy`"]
