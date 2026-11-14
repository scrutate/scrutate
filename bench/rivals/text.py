"""Rivals that are programs run per request: the grep-class tools (W1, W2), universal-ctags (W4),
ast-grep (W6) and an empty program, the floor any process start pays (W9)."""
from __future__ import annotations

import os
import shutil
import subprocess
import time

import leaderboard as lb

from . import Adapter, Setup, Unsupported


def run(cmd, cwd, env=None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, stdin=subprocess.DEVNULL, env={**os.environ, **(env or {})})


class Grep(Adapter):
    """A leaderboard.py grep tool, on the same command line and reference."""

    workloads = frozenset({"W1", "W2"})

    def __init__(self, tool: str):
        self.name = tool
        self.corpus = ""

    def missing(self):
        return None if lb.BIN.get(self.name) else f"{self.name} is not installed"

    def version(self):
        return lb.version(self.name)

    def start(self, corpus, work):
        self.corpus = corpus
        return Setup()

    def ask(self, workload, query):
        cmd, env = lb.argv(self.name, query["grep"], query.get("mode", "ignore"), self.corpus)
        if cmd is None:
            raise Unsupported(f"{self.name} cannot express this query")
        p = run(cmd, self.corpus, env)
        if lb.bad_exit([p.returncode]):
            raise RuntimeError(f"exit {p.returncode}: {p.stderr[-200:].decode(errors='replace')}")
        return lb.normalise(p.stdout)


class Ctags(Adapter):
    """universal-ctags builds a tags file once; `readtags` looks a name up in it per request."""

    name = "universal-ctags"
    workloads = frozenset({"W4"})

    def __init__(self):
        self.tags = ""
        self.corpus = ""

    def missing(self):
        ctags = shutil.which("ctags")
        if not ctags or "Universal Ctags" not in run([ctags, "--version"], "/").stdout.decode(errors="replace"):
            return "universal-ctags is not installed (the ctags on PATH is another one)"
        return None if shutil.which("readtags") else "readtags (universal-ctags) is not on PATH"

    def version(self):
        return run(["ctags", "--version"], "/").stdout.decode().splitlines()[0]

    def start(self, corpus, work):
        self.corpus, self.tags = corpus, os.path.join(work, "tags")
        t0 = time.perf_counter()
        run(["ctags", "-R", "--fields=+n", "--kinds-all=*", "--extras=-F", "-f", self.tags, "."], corpus).check_returncode()
        return Setup(seconds=time.perf_counter() - t0, index_bytes=os.path.getsize(self.tags))

    def ask(self, workload, query):
        p = run(["readtags", "-t", self.tags, "-e", "-n", query["name"]], self.corpus)
        p.check_returncode()
        out = set()
        for line in p.stdout.decode(errors="replace").splitlines():
            name, path = line.split("\t")[:2]
            out.add((path.removeprefix("./"), name))
        return out


class AstGrep(Adapter):
    name = "ast-grep"
    workloads = frozenset({"W6"})

    def missing(self):
        return None if lb.BIN.get("ast-grep") else "ast-grep is not installed"

    def version(self):
        return lb.version("ast-grep")

    def start(self, corpus, work):
        self.corpus = corpus
        return Setup()

    def ask(self, workload, query):
        p = run(lb.structural_argv("ast-grep", query["structural"]), self.corpus)
        if lb.bad_exit([p.returncode]):
            raise RuntimeError(f"exit {p.returncode}: {p.stderr[-200:].decode(errors='replace')}")
        return lb.normalise(p.stdout, "ast-grep")


class EmptyProgram(Adapter):
    """`fn main() {}` built with the release profile's opt-level and LTO: what starting any Rust
    program costs on this machine, the floor under W9."""

    name = "empty program"
    workloads = frozenset({"W9"})

    def missing(self):
        return None if shutil.which("rustc") else "rustc is not on PATH"

    def version(self):
        return run(["rustc", "--version"], "/").stdout.decode().strip()

    def start(self, corpus, work):
        src, self.bin = os.path.join(work, "empty.rs"), os.path.join(work, "empty")
        with open(src, "w") as f:
            f.write("fn main() {}\n")
        run(["rustc", "-C", "opt-level=3", "-C", "lto=fat", "-C", "codegen-units=1", "-C", "strip=symbols", "-o", self.bin, src], work).check_returncode()
        return Setup()

    def ask(self, workload, query):
        return {run([self.bin], "/").returncode}
