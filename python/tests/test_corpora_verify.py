"""bench/corpora.py --verify checks each corpus on disk against the checksum its manifest pins."""
import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import corpora  # noqa: E402


def test_verify_passes_on_the_pinned_tree_and_fails_on_one_changed_byte(tmp_path, monkeypatch, capsys):
    tree = tmp_path / "tiny"
    (tree / "src").mkdir(parents=True)
    (tree / "src" / "a.py").write_text("def a():\n    return 1\n")
    (tree / "b.txt").write_text("hello\n")
    digest, n, size = corpora.tree_hash(str(tree))
    (tmp_path / "manifest.json").write_text(json.dumps({"tiny": {"kind": "generated", "tree_sha256": digest, "files": n, "bytes": size}}))

    monkeypatch.setattr(sys, "argv", ["corpora.py", "--root", str(tmp_path), "--verify"])
    assert corpora.main() == 0

    (tree / "b.txt").write_text("hellp\n")
    assert corpora.main() == 1
    assert "tiny" in capsys.readouterr().out


def test_verify_reports_a_corpus_the_manifest_names_but_the_disk_lacks(tmp_path, monkeypatch, capsys):
    (tmp_path / "manifest.json").write_text(json.dumps({"gone": {"kind": "generated", "tree_sha256": "0" * 64, "files": 1, "bytes": 1}}))
    monkeypatch.setattr(sys, "argv", ["corpora.py", "--root", str(tmp_path), "--verify"])
    assert corpora.main() == 1
    assert "gone" in capsys.readouterr().out


def test_verify_fails_when_it_checks_nothing(tmp_path, monkeypatch, capsys):
    # no manifest: no corpus was ever fetched, which is no pass
    monkeypatch.setattr(sys, "argv", ["corpora.py", "--root", str(tmp_path), "--verify"])
    assert corpora.main() == 1
    assert "manifest.json" in capsys.readouterr().out


def test_verify_fails_for_a_named_corpus_the_manifest_lacks(tmp_path, monkeypatch, capsys):
    tree = tmp_path / "tiny"
    tree.mkdir()
    (tree / "a.txt").write_text("a\n")
    digest, n, size = corpora.tree_hash(str(tree))
    (tmp_path / "manifest.json").write_text(json.dumps({"tiny": {"kind": "generated", "tree_sha256": digest, "files": n, "bytes": size}}))
    monkeypatch.setattr(sys, "argv", ["corpora.py", "--root", str(tmp_path), "--verify", "--only", "tiny", "hugo"])
    assert corpora.main() == 1
    out = capsys.readouterr().out
    assert "hugo: not in" in out and "tiny: ok" in out
