"""bench/rivals/lsp.py: a timed request asks the server alone, with each file opened once."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
from rivals.lsp import Lsp  # noqa: E402


class Recorder:
    def __init__(self):
        self.sent = []

    def notify(self, method, params):
        self.sent.append(method)

    def request(self, method, params):
        self.sent.append(method)
        return []


def test_each_file_is_opened_once_however_often_it_is_asked(tmp_path):
    (tmp_path / "a.c").write_text("int f(void) { return 0; }\n")
    (tmp_path / "b.c").write_text("int g(void) { return f(); }\n")
    s = Lsp("fake", ["true"])
    s.c, s.corpus = Recorder(), str(tmp_path)
    for path in ["a.c", "a.c", "a.c", "b.c", "a.c"]:
        s.ask("W5", {"kind": "refs", "at": [path, 1, 5], "language": "c"})
    assert s.c.sent.count("textDocument/didOpen") == 2
    assert s.c.sent.count("textDocument/references") == 5
