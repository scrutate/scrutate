"""Resident indexed grep rivals for W3: zoekt-webserver and livegrep, asked over HTTP.

The indexes are built with indexed.py's own command lines (its limits raised so no file is
skipped), so the setup matches the indexed benchmark's; the servers run on free local ports and
are ready when their own readiness endpoints say so.
"""
from __future__ import annotations

import json
import os
import socket
import subprocess
import time
import types
import urllib.parse
import urllib.request

import indexed

from . import Adapter, Setup


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_ok(url: str, deadline: float) -> None:
    end = time.monotonic() + deadline
    while time.monotonic() < end:
        try:
            with urllib.request.urlopen(url, timeout=5) as r:
                if r.status == 200:
                    return
        except OSError:
            pass
        time.sleep(0.2)
    raise TimeoutError(f"not ready after {deadline} s: {url}")


def defs(corpus: str, work: str, livegrep_bin: str = "") -> dict:
    return indexed.tool_defs(types.SimpleNamespace(corpus=corpus, work=work, livegrep_bin=livegrep_bin))


class ZoektWeb(Adapter):
    name = "zoekt-webserver"
    form = "resident"
    workloads = frozenset({"W3"})

    def __init__(self, deadline: float = 600):
        self.dir, self.deadline, self.p = indexed.ZOEKT, deadline, None

    def missing(self):
        absent = [b for b in ("zoekt-index", "zoekt-webserver") if not os.access(os.path.join(self.dir, b), os.X_OK)]
        return f"no {', '.join(absent)} in {self.dir} (ZOEKT_BIN_DIR)" if absent else None

    def version(self):
        return "zoekt " + indexed.zoekt_version()

    def start(self, corpus, work):
        d = defs(corpus, work)["zoekt"]
        t0 = time.perf_counter()
        subprocess.run(d["index"], cwd=corpus, capture_output=True, check=True)
        built = time.perf_counter() - t0
        self.url = f"http://127.0.0.1:{free_port()}"
        self.p = subprocess.Popen([os.path.join(self.dir, "zoekt-webserver"), "-index", d["size_dir"], "-listen", self.url.removeprefix("http://"), "-rpc"],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait_ok(self.url + "/readyz", self.deadline)
        return Setup(seconds=built, index_bytes=indexed.du(d["size_dir"]))

    def ask(self, workload, query):
        big = 10 ** 9
        body = {"Q": indexed.zoekt_query(query["grep"]),
                "Opts": {"ChunkMatches": True, "ShardMaxMatchCount": big, "TotalMaxMatchCount": big, "MaxDocDisplayCount": big,
                         "MaxMatchDisplayCount": big, "MaxWallTime": 600 * 10 ** 9}}
        req = urllib.request.Request(self.url + "/api/search", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=600) as r:
            res = json.load(r)["Result"]
        out = set()
        for f in res.get("Files") or []:
            for c in f.get("ChunkMatches") or []:
                for rg in c["Ranges"]:
                    out.update((f["FileName"], n) for n in range(rg["Start"]["LineNumber"], rg["End"]["LineNumber"] + 1))
        return out

    def stop(self):
        if self.p:
            self.p.kill()
            self.p.wait()


class LivegrepWeb(Adapter):
    """codesearch holds the index; the livegrep frontend answers /api/v1/search over HTTP."""

    name = "livegrep"
    form = "resident"
    workloads = frozenset({"W3"})

    def __init__(self, deadline: float = 600):
        self.bin = os.environ.get("LIVEGREP_BIN_DIR", os.path.expanduser("~/bench-tools/livegrep"))
        self.deadline, self.procs = deadline, []

    def missing(self):
        absent = [b for b in ("codesearch", "livegrep", "web") if not os.path.exists(os.path.join(self.bin, b))]
        return f"no {', '.join(absent)} in {self.bin} (LIVEGREP_BIN_DIR)" if absent else None

    def version(self):
        rev = os.path.join(self.bin, "REVISION")
        return "livegrep " + (open(rev).read().strip() if os.path.exists(rev) else "?")

    def start(self, corpus, work):
        indexed.livegrep_config(corpus, work)
        d = defs(corpus, work, self.bin)["livegrep"]
        t0 = time.perf_counter()
        subprocess.run(d["index"], cwd=corpus, capture_output=True, check=True)
        built = time.perf_counter() - t0
        grpc = f"localhost:{free_port()}"
        backend = [grpc if x == "localhost:9999" else x for x in d["server"][0]]
        self.url = f"http://127.0.0.1:{free_port()}"
        front = [os.path.join(self.bin, "livegrep"), "-connect", grpc, "-listen", self.url.removeprefix("http://"), "-docroot", os.path.join(self.bin, "web")]
        for cmd in (backend, front):
            self.procs.append(subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        wait_ok(self.url + "/debug/healthcheck", self.deadline)
        return Setup(seconds=built, index_bytes=os.path.getsize(d["size_file"]))

    def ask(self, workload, query):
        q = query["grep"]
        params = {"q": indexed.to_regex(q) + " max_matches:100000000", "fold_case": "true" if q["icase"] else "false", "regex": "true"}
        with urllib.request.urlopen(self.url + "/api/v1/search/?" + urllib.parse.urlencode(params), timeout=600) as r:
            res = json.load(r)
        return {(m["path"], m["lno"]) for m in res.get("results") or []}

    def stop(self):
        for p in self.procs:
            p.kill()
            p.wait()
