"""scrutate's own forms: the CLI (a process per request) and the MCP server (resident).

Answers are normalised as every rival's are: W1-W3 and W6 (path, line); W4 and callers (path, bare
name); refs (path, line) of each use; W9 the exit code; W10 as W4, through MCP.
"""
from __future__ import annotations

import json
import os
import subprocess
import threading
import time

import leaderboard as lb

from . import Adapter, Setup


def bare(name: str) -> str:
    return name.rsplit(".", 1)[-1]


def hits(result) -> set:
    return {(h["path"], bare(h["name"])) for h in result}


def refs(result) -> set:
    return {(o["path"], o["line"]) for d in result.get("definitions", []) for o in d.get("references", []) if o.get("via") != "definition"}


class Cli(Adapter):
    name = "scrutate"
    form = "process"
    workloads = frozenset({"W1", "W2", "W3", "W4", "W5", "W6", "W8", "W9"})

    def __init__(self, binary: str | None = None):
        self.bin = os.path.abspath(binary or lb.SCRUTATE)
        self.corpus = ""

    def missing(self):
        return None if os.access(self.bin, os.X_OK) else f"no scrutate binary at {self.bin}"

    def version(self):
        return subprocess.run([self.bin, "--version"], capture_output=True, text=True).stdout.strip()

    def start(self, corpus, work):
        self.corpus = corpus
        t0 = time.perf_counter()
        subprocess.run([self.bin, "--root", corpus, "index"], capture_output=True, check=True)
        return Setup(seconds=time.perf_counter() - t0)

    def run(self, args: list[str]) -> subprocess.CompletedProcess:
        return subprocess.run([self.bin, "--root", self.corpus, *args], cwd=self.corpus, capture_output=True, stdin=subprocess.DEVNULL)

    def ask(self, workload, query):
        if workload in ("W1", "W2", "W3"):
            tool = "scrutate --index" if workload == "W3" else "scrutate"
            cmd, env = lb.argv(tool, query["grep"], query.get("mode", "ignore"), self.corpus)
            out = subprocess.run([self.bin, *cmd[1:]], cwd=self.corpus, capture_output=True, env={**os.environ, **env}, stdin=subprocess.DEVNULL).stdout
            return lb.normalise(out)
        if workload == "W6":
            cmd = lb.structural_argv("scrutate", query["structural"])
            return lb.normalise(subprocess.run([self.bin, *cmd[1:]], cwd=self.corpus, capture_output=True, stdin=subprocess.DEVNULL).stdout, "scrutate")
        if workload == "W9":
            return {self.run(["--version"]).returncode}
        if workload == "W8":
            res = json.loads(self.run(["search", "-k", "10", "--json", query["query"]]).stdout)["result"]
            return [(h["path"], h["start"], h["end"]) for h in res]
        kind = query.get("kind", "symbol")
        out = self.run([kind, "--json", query["name"]] + (["--limit", "500"] if kind != "refs" else []))
        result = json.loads(out.stdout)["result"] if out.stdout else []
        return refs(result) if kind == "refs" else hits(result)


class Mcp(Adapter):
    """`scrutate serve --structured`: one JSON-RPC line per message on stdio."""

    name = "scrutate-mcp"
    form = "resident"
    workloads = frozenset({"W4", "W5", "W8", "W10"})
    TOOL = {"symbol": "find_symbol", "refs": "references", "callers": "callers"}

    def __init__(self, binary: str | None = None):
        self.bin = os.path.abspath(binary or lb.SCRUTATE)
        self.p = None
        self.next = 0
        self.lock = threading.Lock()

    def missing(self):
        return None if os.access(self.bin, os.X_OK) else f"no scrutate binary at {self.bin}"

    def version(self):
        return subprocess.run([self.bin, "--version"], capture_output=True, text=True).stdout.strip()

    def open(self, corpus):
        self.p = subprocess.Popen([self.bin, "--root", corpus, "serve", "--structured"], cwd=corpus, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, text=True)
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "yardstick", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")

    def start(self, corpus, work):
        t0 = time.perf_counter()
        self.open(corpus)
        self.call("find_symbol", {"name": "__yardstick_warm__"})  # the first call builds or opens the index
        return Setup(seconds=time.perf_counter() - t0)

    def rpc(self, method, params):
        with self.lock:
            self.next += 1
            self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}) + "\n")
            self.p.stdin.flush()
            while True:
                line = self.p.stdout.readline()
                if not line:
                    raise RuntimeError(f"serve exited ({self.p.poll()})")
                msg = json.loads(line)
                if msg.get("id") == self.next:
                    if "error" in msg:
                        raise RuntimeError(msg["error"].get("message"))
                    return msg["result"]

    def call(self, tool, args):
        r = self.rpc("tools/call", {"name": tool, "arguments": args})
        if r.get("isError"):
            raise RuntimeError(r["content"][0]["text"])
        return r

    def ask(self, workload, query):
        if workload == "W8":
            res = self.call("search_code", {"query": query["query"], "k": 10})["structuredContent"]["result"]
            return [(h["path"], h["start"], h["end"]) for h in res]
        kind = query.get("kind", "symbol")
        r = self.call(self.TOOL[kind], {"name": query["name"], **({"limit": 500} if kind != "refs" else {})})
        result = r["structuredContent"]["result"]
        return refs(result) if kind == "refs" else hits(result)

    def stop(self):
        if self.p:
            self.p.stdin.close()
            try:
                self.p.wait(10)
            except subprocess.TimeoutExpired:
                self.p.kill()


class McpCold(Mcp):
    """One MCP call from nothing: start `serve`, initialize, call, exit (what a client that
    starts the server per task pays)."""

    name = "scrutate-mcp-cold"
    form = "process"
    workloads = frozenset({"W10"})

    def start(self, corpus, work):
        self.corpus = corpus
        return Setup()

    def ask(self, workload, query):
        one = Mcp(self.bin)
        one.open(self.corpus)
        try:
            return Mcp.ask(one, workload, query)
        finally:
            one.stop()
