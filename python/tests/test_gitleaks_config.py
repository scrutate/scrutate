"""tools/gitleaks_config.py: an allow list that exempts files fails the check."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
import gitleaks_config as G  # noqa: E402

ROOT = os.path.join(os.path.dirname(__file__), "..", "..")
HEAD = "title = 't'\n[extend]\nuseDefault = true\n"


def test_the_repository_config_exempts_nothing():
    assert G.check(os.path.join(ROOT, ".gitleaks.toml")) == []


def test_paths_in_any_allow_list_fail(tmp_path):
    cases = {
        "[[allowlists]]\ncondition = 'AND'\nregexes = ['''y''']\npaths = ['''^crates/scrutate/src/secrets\\.rs$''']\n": "allowlists[0]",
        "[allowlist]\npaths = ['''x''']\n": "allowlist",
        "[[rules]]\nid = 'github-pat'\n[[rules.allowlists]]\npaths = ['''x''']\n": "rules[0].allowlists[0]",
        "[[rules]]\nid = 'r'\n[rules.allowlist]\npaths = ['''x''']\n": "rules[0].allowlist",
    }
    c = tmp_path / "c.toml"
    for body, where in cases.items():
        c.write_text(HEAD + body)
        assert G.check(str(c)) == [f"{c}: {where}"], body
        assert G.main(["gitleaks_config.py", str(c)]) == 1


def test_a_config_it_extends_is_checked_too(tmp_path):
    base = tmp_path / "base.toml"
    base.write_text("[[allowlists]]\npaths = ['''x''']\n")
    c = tmp_path / "c.toml"
    c.write_text(f"[extend]\npath = '{base}'\n")
    assert G.check(str(c)) == [f"{base}: allowlists[0]"]


def test_regexes_and_stopwords_alone_pass(tmp_path):
    c = tmp_path / "c.toml"
    c.write_text(HEAD + "[[allowlists]]\nregexes = ['''x''']\nstopwords = ['y']\n")
    assert G.check(str(c)) == []
    assert G.main(["gitleaks_config.py", str(c)]) == 0
