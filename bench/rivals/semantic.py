"""Semantic code search rivals for W8, all local: ck (its own bundled model), grepai (Ollama) and
claude-context (Ollama embeddings, Milvus standalone), each with the embedding model in its
version so a row says what was measured. An answer is a ranked list of (path, start, end)."""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import time
import urllib.request

from . import Adapter, Setup

OLLAMA = os.environ.get("OLLAMA_HOST", "http://127.0.0.1:11434")
OLLAMA_MODEL = os.environ.get("YARDSTICK_OLLAMA_MODEL", "nomic-embed-text")
HERE = os.path.dirname(os.path.abspath(__file__))


def relpath(corpus: str, p: str) -> str:
    p = p.removeprefix("./")
    return os.path.relpath(p, corpus).replace(os.sep, "/") if os.path.isabs(p) else p


def ollama_missing() -> str | None:
    try:
        with urllib.request.urlopen(OLLAMA + "/api/tags", timeout=5) as r:
            names = [m["name"] for m in json.load(r).get("models", [])]
    except OSError as e:
        return f"Ollama is not answering at {OLLAMA} ({e})"
    if not any(n.split(":")[0] == OLLAMA_MODEL for n in names):
        return f"Ollama has no {OLLAMA_MODEL} (ollama pull {OLLAMA_MODEL})"
    return None


class Ck(Adapter):
    name = "ck"
    workloads = frozenset({"W8"})

    def missing(self):
        return None if shutil.which("ck") else "ck is not on PATH"

    def version(self):
        v = subprocess.run(["ck", "--version"], capture_output=True, text=True).stdout.strip()
        return f"{v} (embedding bge-small, its default)"

    def start(self, corpus, work):
        self.corpus = corpus
        t0 = time.perf_counter()
        subprocess.run(["ck", "--index", "."], cwd=corpus, capture_output=True, check=True)
        return Setup(seconds=time.perf_counter() - t0)

    def ask(self, workload, query):
        # --threshold 0: semantic search otherwise drops hits scoring under 0.6, short of the top ten
        p = subprocess.run(["ck", "--jsonl", "--no-snippet", "--sem", "--topk", "10", "--threshold", "0", query["query"], "."],
                           cwd=self.corpus, capture_output=True, text=True, check=True)
        rows = [json.loads(line) for line in p.stdout.splitlines() if line.strip()]
        return [(relpath(self.corpus, r["path"]), r["span"]["line_start"], r["span"]["line_end"]) for r in rows]


class Grepai(Adapter):
    name = "grepai"
    workloads = frozenset({"W8"})

    def missing(self):
        return ("grepai is not on PATH" if not shutil.which("grepai") else None) or ollama_missing()

    def version(self):
        v = subprocess.run(["grepai", "version"], capture_output=True, text=True).stdout.strip()
        return f"{v} (Ollama {OLLAMA_MODEL}; its default path boosts and per-file dedup on)"

    def start(self, corpus, work):
        self.corpus = corpus
        t0 = time.perf_counter()
        subprocess.run(["grepai", "init"], cwd=corpus, capture_output=True, stdin=subprocess.DEVNULL, check=True)
        subprocess.run(["grepai", "index"], cwd=corpus, capture_output=True, check=True)
        return Setup(seconds=time.perf_counter() - t0)

    def ask(self, workload, query):
        p = subprocess.run(["grepai", "search", query["query"], "--json", "--compact", "--limit", "10"], cwd=self.corpus, capture_output=True, text=True,
                           check=True)
        res = json.loads(p.stdout)
        if isinstance(res, dict) and "error" in res:
            raise RuntimeError(res["error"])
        return [(relpath(self.corpus, r["file_path"]), r["start_line"], r["end_line"]) for r in res]


class ClaudeContext(Adapter):
    """claude-context's core API in one Node process (bench/competitors/claude_context.mjs): it
    indexes the corpus, then answers one JSON query per line."""

    name = "claude-context"
    form = "resident"
    workloads = frozenset({"W8"})
    SCRIPT = os.path.join(HERE, "..", "competitors", "claude_context.mjs")

    def missing(self):
        if not shutil.which("node"):
            return "node is not on PATH"
        if not os.path.isdir(os.path.join(os.environ.get("CLAUDE_CONTEXT_DIR", "/nonexistent"), "node_modules", "@zilliz", "claude-context-core")):
            return "CLAUDE_CONTEXT_DIR has no node_modules/@zilliz/claude-context-core (npm install @zilliz/claude-context-core@0.1.16 there)"
        if not os.environ.get("MILVUS_ADDRESS"):
            return "MILVUS_ADDRESS is not set (Milvus standalone, bench/competitors/claude-context.compose.yml)"
        return ollama_missing()

    def version(self):
        out = subprocess.run(["node", self.SCRIPT, "--version"], capture_output=True, text=True).stdout.strip()
        return f"claude-context {out or '?'} (Ollama {OLLAMA_MODEL}, Milvus at {os.environ.get('MILVUS_ADDRESS')})"

    def start(self, corpus, work):
        self.corpus = corpus
        env = {**os.environ, "EMBEDDING_PROVIDER": "Ollama", "EMBEDDING_MODEL": OLLAMA_MODEL, "OLLAMA_HOST": OLLAMA, "HYBRID_MODE": "true"}
        t0 = time.perf_counter()
        self.p = subprocess.Popen(["node", self.SCRIPT, corpus], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=env)
        ready = self.p.stdout.readline()
        if json.loads(ready or "{}").get("ready") is not True:
            raise RuntimeError(f"claude_context.mjs did not get ready: {ready.strip() or self.p.poll()}")
        return Setup(seconds=time.perf_counter() - t0)

    def ask(self, workload, query):
        self.p.stdin.write(json.dumps({"query": query["query"], "limit": 10}) + "\n")
        self.p.stdin.flush()
        res = json.loads(self.p.stdout.readline())
        if "error" in res:
            raise RuntimeError(res["error"])
        return [(r["relativePath"], r["startLine"], r["endLine"]) for r in res["results"]]

    def stop(self):
        if getattr(self, "p", None):
            self.p.stdin.close()
            self.p.wait(30)
