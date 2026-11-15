"""tools/gate_parity.py: every CI step is keyed by what decides whether and how it runs."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
import gate_parity as G  # noqa: E402

JOB = """jobs:
  check:
    runs-on: ubuntu-latest
    steps:
"""


def keys(tmp_path, monkeypatch, steps: str) -> dict:
    (tmp_path / ".github" / "workflows").mkdir(parents=True, exist_ok=True)
    (tmp_path / ".github" / "workflows" / "ci.yml").write_text(JOB + steps)
    monkeypatch.setattr(G, "ROOT", str(tmp_path))
    monkeypatch.setattr(G, "WORKFLOWS", ("ci.yml",))
    return dict(G.steps())


def test_a_uses_step_is_keyed(tmp_path, monkeypatch):
    found = keys(tmp_path, monkeypatch, "      - uses: actions/checkout@v7\n      - run: make\n")
    assert sorted(found.values()) == ["actions/checkout@v7", "make"]
    other = keys(tmp_path, monkeypatch, "      - uses: actions/checkout@v7\n        with:\n          fetch-depth: 0\n      - run: make\n")
    assert set(found) != set(other)


def test_if_shell_working_directory_and_continue_on_error_move_the_key(tmp_path, monkeypatch):
    plain = set(keys(tmp_path, monkeypatch, "      - run: make\n"))
    for extra in ("if: false", "shell: sh", "working-directory: sub", "continue-on-error: true"):
        changed = set(keys(tmp_path, monkeypatch, f"      - run: make\n        {extra}\n"))
        assert changed != plain, extra
    job_if = JOB.replace("    runs-on:", "    if: false\n    runs-on:")
    (tmp_path / ".github" / "workflows" / "ci.yml").write_text(job_if + "      - run: make\n")
    assert set(dict(G.steps())) != plain


def test_a_comment_names_no_gate_step(tmp_path, monkeypatch):
    (tmp_path / "tools").mkdir()
    (tmp_path / "tools" / "ci_gate.sh").write_text(
        "# the step below covers it; every step here\n"
        "step fmt cargo fmt --all --check\n"
        "  [ -f deny.toml ] && step cargo-deny deny check\n"
        '  step "msrv-$msrv" cargo check\n'
        "  if a; then step doc-shift x\n"
        "  else step doc-shift y; fi\n"
        '  say "SKIP secret-scan (no step here)"\n'
    )
    monkeypatch.setattr(G, "ROOT", str(tmp_path))
    assert G.gate_steps() == {"fmt", "cargo-deny", "msrv-$msrv", "doc-shift"}


def test_needs_container_services_and_the_triggers_move_the_key(tmp_path, monkeypatch):
    plain = set(keys(tmp_path, monkeypatch, "      - run: make\n"))
    for job_key in ("needs: other", "container: alpine:3", "services:\n      db:\n        image: postgres"):
        (tmp_path / ".github" / "workflows" / "ci.yml").write_text(JOB.replace("    runs-on:", f"    {job_key}\n    runs-on:") + "      - run: make\n")
        assert set(dict(G.steps())) != plain, job_key
    (tmp_path / ".github" / "workflows" / "ci.yml").write_text("on:\n  pull_request:\n" + JOB + "      - run: make\n")
    only_pr = set(dict(G.steps()))
    (tmp_path / ".github" / "workflows" / "ci.yml").write_text("on:\n  push:\n  pull_request:\n" + JOB + "      - run: make\n")
    assert set(dict(G.steps())) != only_pr
