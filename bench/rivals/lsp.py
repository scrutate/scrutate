"""Language servers as yardstick adapters: workspace/symbol (W4), references and incoming calls (W5).

One client for every server (scrutate lsp, rust-analyzer, pyright, clangd, gopls). A server is
ready when its `$/progress` work has been quiet for two seconds after `initialized` (indexing
reports progress there), within a deadline; positions come back 0-based and are made 1-based here.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import threading
import time
import urllib.parse

from . import Adapter, Setup, Unsupported


def uri(path: str) -> str:
    return "file://" + urllib.parse.quote(os.path.abspath(path))


def rel(corpus: str, u: str) -> str:
    p = urllib.parse.unquote(urllib.parse.urlparse(u).path)
    return os.path.relpath(os.path.realpath(p), os.path.realpath(corpus)).replace(os.sep, "/")


class Client:
    """JSON-RPC over stdio with Content-Length frames; answers are matched to requests by id."""

    def __init__(self, cmd: list[str], cwd: str):
        self.p = subprocess.Popen(cmd, cwd=cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        self.next, self.answers, self.progress, self.last_progress = 0, {}, set(), time.monotonic()
        self.cv = threading.Condition()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        f = self.p.stdout
        while True:
            n = None
            while True:
                line = f.readline()
                if not line:
                    with self.cv:
                        self.cv.notify_all()
                    return
                if line in (b"\r\n", b"\n"):
                    break
                if line.lower().startswith(b"content-length:"):
                    n = int(line.split(b":")[1])
            msg = json.loads(f.read(n))
            with self.cv:
                if "id" in msg and "method" in msg:  # a request from the server: accept it
                    self.send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
                elif "id" in msg:
                    self.answers[msg["id"]] = msg
                elif msg.get("method") == "$/progress":
                    kind = msg["params"]["value"].get("kind")
                    (self.progress.add if kind == "begin" else self.progress.discard if kind == "end" else (lambda _: None))(msg["params"]["token"])
                    self.last_progress = time.monotonic()
                self.cv.notify_all()

    def send(self, msg: dict):
        body = json.dumps(msg).encode()
        self.p.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.p.stdin.flush()

    def request(self, method: str, params, timeout: float = 600) -> dict:
        with self.cv:
            self.next += 1
            i = self.next
        self.send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        with self.cv:
            if not self.cv.wait_for(lambda: i in self.answers or self.p.poll() is not None, timeout):
                raise TimeoutError(f"{method}: no answer in {timeout} s")
            if i not in self.answers:
                raise RuntimeError(f"{method}: the server exited ({self.p.poll()})")
            msg = self.answers.pop(i)
        if "error" in msg:
            raise RuntimeError(f"{method}: {msg['error'].get('message')}")
        return msg.get("result")

    def notify(self, method: str, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def quiet(self, seconds: float, deadline: float) -> bool:
        """Wait until no progress is open and none came for `seconds`."""
        end = time.monotonic() + deadline
        with self.cv:
            while time.monotonic() < end:
                if not self.progress and time.monotonic() - self.last_progress >= seconds:
                    return True
                self.cv.wait(0.2)
        return False

    def close(self):
        try:
            self.request("shutdown", None, timeout=10)
            self.notify("exit", None)
            self.p.wait(10)
        except Exception:  # noqa: BLE001 - a server that will not stop is killed
            self.p.kill()


def first_file(root: str, suffixes: tuple[str, ...]) -> str | None:
    """The first file (in sorted walk order, hidden directories skipped) ending in one of `suffixes`."""
    for d, dirs, files in os.walk(root):
        dirs[:] = sorted(x for x in dirs if not x.startswith("."))
        for f in sorted(files):
            if f.endswith(suffixes):
                return os.path.relpath(os.path.join(d, f), root)
    return None


class Lsp(Adapter):
    """One language server: `cmd` started in the corpus."""

    form = "resident"
    workloads = frozenset({"W4", "W5"})

    def __init__(self, name: str, cmd: list[str], version_cmd: list[str] | None = None, deadline: float = 900, calls: bool = True,
                 opens: tuple[str, ...] = ()):
        """`opens`: suffixes of a file to open before waiting, for a server that indexes only once
        a file is open (clangd finds its compilation database from the first one)."""
        self.name, self.cmd, self.version_cmd, self.deadline, self.calls, self.opens = name, cmd, version_cmd, deadline, calls, opens
        self.c: Client | None = None
        self.corpus = ""

    def missing(self):
        return None if shutil.which(self.cmd[0]) else f"{self.cmd[0]} is not on PATH"

    def version(self):
        if not self.version_cmd:
            return "?"
        out = subprocess.run(self.version_cmd, capture_output=True, text=True)
        return (out.stdout or out.stderr).strip().splitlines()[0] if (out.stdout or out.stderr).strip() else "?"

    def start(self, corpus, work):
        t0 = time.perf_counter()
        self.corpus = corpus
        self.c = Client(self.cmd, corpus)
        caps = {"workspace": {"symbol": {}}, "textDocument": {"references": {}, "callHierarchy": {}}, "window": {"workDoneProgress": True}}
        self.c.request("initialize", {"processId": os.getpid(), "rootUri": uri(corpus), "capabilities": caps,
                                      "workspaceFolders": [{"uri": uri(corpus), "name": os.path.basename(corpus)}]})
        self.c.notify("initialized", {})
        first = first_file(corpus, self.opens) if self.opens else None
        if first:
            self.at({"at": [first, 1, 1]})
        if not self.c.quiet(2.0, self.deadline):
            raise TimeoutError(f"not ready after {self.deadline} s")
        return Setup(seconds=time.perf_counter() - t0)

    def at(self, query):
        """The query's position (path, 1-based line, 1-based column) as LSP params; opens the file."""
        path, line, col = query["at"]
        full = os.path.join(self.corpus, path)
        with open(full, encoding="utf-8", errors="replace") as f:
            text = f.read()
        self.c.notify("textDocument/didOpen", {"textDocument": {"uri": uri(full), "languageId": query.get("language", ""), "version": 1, "text": text}})
        return {"textDocument": {"uri": uri(full)}, "position": {"line": line - 1, "character": col - 1}}

    def ask(self, workload, query):
        kind = query.get("kind", "symbol")
        if kind == "symbol":
            got = self.c.request("workspace/symbol", {"query": query["name"]}) or []
            return {(rel(self.corpus, s["location"]["uri"]), s["name"].rsplit(".", 1)[-1]) for s in got if s["name"].rsplit(".", 1)[-1] == query["name"]}
        if kind == "refs":
            got = self.c.request("textDocument/references", {**self.at(query), "context": {"includeDeclaration": False}}) or []
            return {(rel(self.corpus, r["uri"]), r["range"]["start"]["line"] + 1) for r in got}
        if kind == "callers":
            if not self.calls:
                raise Unsupported(f"{self.name} has no call hierarchy")
            items = self.c.request("textDocument/prepareCallHierarchy", self.at(query)) or []
            calls = [c for it in items for c in (self.c.request("callHierarchy/incomingCalls", {"item": it}) or [])]
            return {(rel(self.corpus, c["from"]["uri"]), c["from"]["name"].rsplit(".", 1)[-1].split("(")[0]) for c in calls}
        raise ValueError(f"unknown kind {kind}")

    def stop(self):
        if self.c:
            self.c.close()
