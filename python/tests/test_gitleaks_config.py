"""tools/gitleaks_config.py: the config may hold gitleaks' own rules and nothing more."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
import gitleaks_config as G  # noqa: E402

ROOT = os.path.join(os.path.dirname(__file__), "..", "..")
HEAD = "title = 't'\n[extend]\nuseDefault = true\n"


def test_the_repository_config_passes():
    assert G.check(os.path.join(ROOT, ".gitleaks.toml")) == []


def test_anything_beyond_the_default_rules_fails_and_is_named(tmp_path):
    cases = {
        # a rule narrowed to the files the gate plants in: every other file escapes it
        HEAD + "[[rules]]\nid = 'github-pat'\npath = '''crates/scrutate/src/(intel|secrets)\\.rs'''\n": "rules",
        # a default rule turned off: no plant can see it
        HEAD + "disabledRules = ['aws-access-token']\n": "extend.disabledrules",
        HEAD + "[[allowlists]]\npaths = ['''x''']\n": "allowlists",
        HEAD + "[[AllowLists]]\nPaths = ['''x''']\n": "allowlists",
        HEAD + "[allowlist]\nregexes = ['''x''']\n": "allowlist",
        "unknown = 1\n" + HEAD: "unknown",
    }
    c = tmp_path / "c.toml"
    for text, key in cases.items():
        c.write_text(text)
        assert G.check(str(c)) == [f"{c}: {key}"], text
        assert G.main(["gitleaks_config.py", str(c)]) == 1


def test_the_default_rules_must_be_extended(tmp_path):
    c = tmp_path / "c.toml"
    missing = "extend.usedefault (gitleaks' own rules) missing"
    for text, keys in {
        "title = 't'\n": [missing],
        "title = 't'\n[extend]\nuseDefault = false\n": ["extend.usedefault", missing],
        "title = 't'\n[Extend]\nPath = 'other.toml'\nuseDefault = true\n": ["extend.path"],
        "title = 't'\n[extend]\nuseDefault = true\n[Extend]\nUseDefault = true\n": ["extend.usedefault", missing],
    }.items():
        c.write_text(text)
        assert G.check(str(c)) == [f"{c}: {k}" for k in keys], text


def test_a_gitleaksignore_beside_the_config_fails(tmp_path, capsys):
    c = tmp_path / "c.toml"
    c.write_text(HEAD)
    assert G.main(["gitleaks_config.py", str(c)]) == 0
    (tmp_path / ".gitleaksignore").write_text("crates/scrutate/src/secrets.rs:github-pat:1360\n")
    assert G.main(["gitleaks_config.py", str(c)]) == 1
    assert ".gitleaksignore exempts findings by fingerprint" in capsys.readouterr().out
