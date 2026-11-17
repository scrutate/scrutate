"""The Linux benchmark harness: a run refuses a binary built from another commit, and the scripts parse."""
import os
import subprocess

HERE = os.path.dirname(__file__)
COMP = os.path.join(HERE, "..", "..", "bench", "competitors")
BUILT_FROM = os.path.join(COMP, "built_from.sh")


def _git(repo, *args):
    env = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, env=env)


def test_a_run_refuses_a_build_from_another_commit(tmp_path):
    src, built = tmp_path / "src", tmp_path / "built"
    src.mkdir()
    built.mkdir()
    _git(src, "init", "-q")
    _git(src, "commit", "-q", "--allow-empty", "-m", "one")
    assert subprocess.run(["bash", BUILT_FROM, "write", str(src), str(built)]).returncode == 0
    assert subprocess.run(["bash", BUILT_FROM, "check", str(src), str(built)]).returncode == 0
    _git(src, "commit", "-q", "--allow-empty", "-m", "two")
    r = subprocess.run(["bash", BUILT_FROM, "check", str(src), str(built)], capture_output=True, text=True)
    assert r.returncode == 1 and "was built from" in r.stderr, r.stderr


def test_a_run_refuses_a_build_that_recorded_no_commit(tmp_path):
    src = tmp_path / "src"
    src.mkdir()
    _git(src, "init", "-q")
    _git(src, "commit", "-q", "--allow-empty", "-m", "one")
    assert subprocess.run(["bash", BUILT_FROM, "check", str(src), str(tmp_path)], capture_output=True).returncode == 1


def test_on_linux_compare_tools_takes_the_harness_builds_first_on_path(monkeypatch):
    import sys
    sys.path.insert(0, os.path.join(HERE, "..", "..", "bench"))
    import compare_tools as C
    monkeypatch.setattr(C.platform, "system", lambda: "Linux")
    monkeypatch.setattr(C.shutil, "which", lambda name: f"/bench/bin/{name}")
    monkeypatch.setattr(C.os.path, "isfile", lambda p: True)
    assert C.find("git") == "/bench/bin/git"


def test_the_linux_scripts_parse():
    for name in ("linux_bench.sh", "linux_indexed.sh", "built_from.sh"):
        assert subprocess.run(["bash", "-n", os.path.join(COMP, name)]).returncode == 0, name
