#!/usr/bin/env python3
"""Reproducible comparison of scrutate's instant engine with grep-class tools, plus index metrics.

usage: compare_tools.py --corpus NAME=DIR [--corpus NAME=DIR ...] [--runs N] [--cold] [--out results.json]

For every (corpus, query, tool) the harness
  1. runs the command once and records the number of matching lines (all tools must agree, or the
     row is flagged: a benchmark of a tool that returns different results is not a benchmark);
  2. runs it N more times, warm cache, taking the median and minimum wall time, CPU time
     (user+sys) and peak RSS from the child's rusage (os.wait4);
  3. optionally drops the page cache (root only) and measures one cold run.
Startup cost is measured on a directory with one file; concurrency as K simultaneous processes, each with
its own output file checked against the reference count, N rounds per cell.

Only tools found on this machine are measured; the others are reported as "not available" so the
table never implies a comparison that did not happen. Tool paths are absolute: shell functions and
aliases (some environments alias `grep` to another program) are bypassed.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

from leaderboard import drop_scrutate_settings

HERE = os.path.dirname(os.path.abspath(__file__))
SCRUTATE = os.environ.get("SCRUTATE_BIN", os.path.join(HERE, "..", "target", "release", "scrutate"))
WORK = os.environ.get("SCRUTATE_WORK", os.path.join(HERE, "..", "..", "work"))


# --------------------------------------------------------------------------- tool discovery
def find(name: str) -> str | None:
    for cand in [f"/usr/bin/{name}", f"/usr/local/bin/{name}", f"/opt/homebrew/bin/{name}"]:
        if os.path.isfile(cand) and os.access(cand, os.X_OK):
            return cand
    return shutil.which(name)


def discover() -> dict:
    tools: dict[str, dict] = {}
    tools["scrutate"] = {"exe": SCRUTATE, "version": run_version([SCRUTATE, "--version"])}
    for name in ["rg", "grep", "git", "ugrep", "ack", "ag", "ast-grep"]:
        # GNU grep where it is installed under its Homebrew name (macOS ships BSD grep as grep)
        p = (find("ggrep") or find(name)) if name == "grep" else find(name)
        tools[name] = {"exe": p, "version": run_version([p, "--version"]) if p else None}
    # ugrep bundled inside another binary (dispatches on argv[0]); used only when no native ugrep exists
    bundled = os.environ.get("UGREP_BUNDLED")
    if tools["ugrep"]["exe"] is None and bundled and os.path.isfile(bundled):
        v = run_version(["ugrep", "--version"], executable=bundled)
        if v and "ugrep" in v:
            tools["ugrep"] = {"exe": bundled, "argv0": "ugrep", "version": v, "note": "bundled build (argv0 dispatch); see caveats"}
    return tools


def run_version(cmd, executable=None) -> str | None:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=30, executable=executable, stdin=subprocess.DEVNULL)
        line = (r.stdout or r.stderr).strip().splitlines()
        return line[0] if line else None
    except Exception:
        return None


# --------------------------------------------------------------------------- commands
def command(tool: str, t: dict, q: dict, corpus: str) -> list[str] | None:
    """argv for `tool` running query `q` over `corpus`, or None when the tool cannot express it."""
    pat = q["pattern"]
    mode = q["mode"]  # literal | regex | icase | word | count | files | multiline | identifier | fuzzy
    if tool == "scrutate":
        c = [t["exe"], "grep"]
        if mode == "literal":
            c += ["-F", pat]
        elif mode == "regex":
            c += [pat]
        elif mode == "icase":
            c += ["-F", "-i", pat]
        elif mode == "word":
            c += ["-F", "-w", pat]
        elif mode == "count":
            c += ["-F", "-c", pat]
        elif mode == "files":
            c += ["-F", "-l", pat]
        elif mode == "multiline":
            c += ["-U", pat]
        elif mode == "identifier":
            c += ["--ident", pat]
        elif mode == "fuzzy":
            c += ["--fuzzy=1", pat]
        return c + ["."]
    if tool == "rg":
        c = [t["exe"], "--no-messages"]
        if mode == "literal":
            c += ["-n", "-F", pat]
        elif mode == "regex":
            c += ["-n", pat]
        elif mode == "icase":
            c += ["-n", "-F", "-i", pat]
        elif mode == "word":
            c += ["-n", "-F", "-w", pat]
        elif mode == "count":
            c += ["-c", "-F", pat]
        elif mode == "files":
            c += ["-l", "-F", pat]
        elif mode == "multiline":
            c += ["-n", "-U", pat]
        elif mode == "identifier":
            c += ["-n", q["ident_regex"]]
        else:
            return None
        return c + ["."]
    if tool == "grep":
        # the files the others search: nothing hidden (GNU grep knows no ignore rules)
        c = [t["exe"], "-r", "-I", "--exclude=.*", "--exclude-dir=.?*"]
        if mode == "literal":
            c += ["-n", "-F", pat]
        elif mode == "regex":
            c += ["-n", "-E", pat]
        elif mode == "icase":
            c += ["-n", "-F", "-i", pat]
        elif mode == "word":
            c += ["-n", "-F", "-w", pat]
        elif mode == "count":
            c += ["-c", "-F", pat]
        elif mode == "files":
            c += ["-l", "-F", pat]
        elif mode == "identifier":
            c += ["-n", "-E", "-i", q["ident_regex_posix"]]
        else:
            return None  # no multiline mode
        return c + ["."]
    if tool == "git":
        c = [t["exe"], "grep", "-I"]
        if mode == "literal":
            c += ["-n", "-F", pat]
        elif mode == "regex":
            c += ["-n", "-P", pat]  # \s and \w are not POSIX: PCRE, as in bench/leaderboard.py
        elif mode == "icase":
            c += ["-n", "-F", "-i", pat]
        elif mode == "word":
            c += ["-n", "-F", "-w", pat]
        elif mode == "count":
            c += ["-c", "-F", pat]
        elif mode == "files":
            c += ["-l", "-F", pat]
        elif mode == "identifier":
            c += ["-n", "-P", "-i", q["ident_regex"]]
        else:
            return None
        return c
    if tool == "ugrep":
        exe = t.get("argv0", t["exe"])
        c = [exe, "-r", "-I", "--ignore-files", "--exclude-dir=.git", "--exclude-dir=.scrutate"]
        if mode == "literal":
            c += ["-n", "-F", pat]
        elif mode == "regex":
            c += ["-n", pat]
        elif mode == "icase":
            c += ["-n", "-F", "-i", pat]
        elif mode == "word":
            c += ["-n", "-F", "-w", pat]
        elif mode == "count":
            c += ["-c", "-F", pat]
        elif mode == "files":
            c += ["-l", "-F", pat]
        elif mode == "identifier":
            c += ["-n", "-i", q["ident_regex"]]
        elif mode == "fuzzy":
            c += ["-n", "-Z1", "-F", pat]
        else:
            return None
        return c + ["."]
    if tool == "ack":
        # ack 3.10 refuses --nogroup together with -c or -l
        c = [t["exe"], "--noenv", "--nocolor"] + ([] if mode in ("count", "files") else ["--nogroup"])
        if mode == "literal":
            c += ["-Q", pat]
        elif mode == "regex":
            c += [pat]
        elif mode == "icase":
            c += ["-Q", "-i", pat]
        elif mode == "word":
            c += ["-Q", "-w", pat]
        elif mode == "count":
            c += ["-c", "-Q", pat]
        elif mode == "files":
            c += ["-l", "-Q", pat]
        elif mode == "identifier":
            c += ["-i", q["ident_regex"]]
        else:
            return None
        return c + ["."]
    if tool == "ag":
        c = [t["exe"], "--nocolor", "--nogroup", "--silent"]
        if mode == "literal":
            c += ["-Q", pat]
        elif mode == "regex":
            c += [pat]
        elif mode == "icase":
            c += ["-Q", "-i", pat]
        elif mode == "word":
            c += ["-Q", "-w", pat]
        elif mode == "count":
            c += ["-c", "-Q", pat]
        elif mode == "files":
            c += ["-l", "-Q", pat]
        elif mode == "identifier":
            c += ["-i", q["ident_regex"]]
        else:
            return None
        return c + ["."]
    return None


def shlex_quote(s: str) -> str:
    import shlex
    return shlex.quote(s)


def count_lines(mode: str, out: bytes) -> int:
    """Matching lines reported by a run (per-file counts are summed in count mode)."""
    if mode == "count":
        total = 0
        for line in out.splitlines():
            try:
                total += int(line.rsplit(b":", 1)[1])
            except (IndexError, ValueError):
                pass
        return total
    return sum(1 for ln in out.splitlines() if ln.strip())


# --------------------------------------------------------------------------- measurement
def env_of(cmd: list[str]) -> dict | None:
    """GNU grep gets the C locale for plain fixed-string searches, where it is fastest and the
    locale cannot change a match (bench/leaderboard.py does the same); the rest keep the caller's."""
    if os.path.basename(cmd[0]) in ("grep", "ggrep") and "-F" in cmd and "-i" not in cmd and "-w" not in cmd:
        return {**os.environ, "LC_ALL": "C"}
    return None


def shown(cmd: list[str]) -> str:
    return ("LC_ALL=C " if env_of(cmd) else "") + " ".join(shlex_quote(c) for c in cmd)


def timed_rusage(cmd: list[str], cwd: str, executable: str | None = None, count_only: bool = False) -> dict:
    """Run once with per-process CPU time and peak RSS from wait4; stdout goes to a temp file.
    With `count_only` the output is not loaded into memory: only its line count is returned
    (structural searches over the massive corpus print hundreds of MB)."""
    with tempfile.TemporaryFile() as fo, tempfile.TemporaryFile() as fe:
        t0 = time.perf_counter()
        p = subprocess.Popen(cmd, cwd=cwd, stdout=fo, stderr=fe, stdin=subprocess.DEVNULL, executable=executable, env=env_of(cmd))
        _, status, ru = os.wait4(p.pid, 0)
        wall = time.perf_counter() - t0
        fo.seek(0)
        if count_only:
            n = 0
            for chunk in iter(lambda: fo.read(1 << 20), b""):
                n += chunk.count(b"\n")
            out = n
        else:
            out = fo.read()
        fe.seek(0)
        err = fe.read(4096)
    rc = os.waitstatus_to_exitcode(status)
    return {"wall": wall, "out": out, "err": err, "rc": rc, "cpu": ru.ru_utime + ru.ru_stime, "maxrss_kb": ru.ru_maxrss}


def peak_rss_mb(cmd: list[str], cwd: str, executable: str | None = None) -> float:
    """Output goes to a file in both branches: on /dev/null GNU grep stops reading a file at its
    first match and ugrep stops at the first match of all, which is less work than the search."""
    if not os.path.isdir("/proc"):
        # macOS: /usr/bin/time -l reports the child's own maximum resident set size, in bytes
        with tempfile.TemporaryFile() as fo:
            r = subprocess.run(["/usr/bin/time", "-l", executable or cmd[0], *cmd[1:]], cwd=cwd, stdout=fo,
                               stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, text=True, env=env_of(cmd))
        for line in r.stderr.splitlines():
            if "maximum resident set size" in line:
                return int(line.split()[0]) / 2**20
        return 0.0
    return _peak_rss_proc(cmd, cwd, executable)


def _peak_rss_proc(cmd: list[str], cwd: str, executable: str | None = None) -> float:
    """Peak resident set size of one run, sampled from /proc/<pid>/status (VmHWM) every millisecond.

    `wait4`'s ru_maxrss cannot be used: on Linux the kernel records the pre-exec image's high-water
    mark into the child's accounting, so every program forked from this (Python) process reports at
    least the harness's own RSS. Sampling the child's own VmHWM avoids that floor; a peak shorter
    than the sampling interval at the very end of a run could be missed."""
    import threading
    fo = tempfile.TemporaryFile()
    p = subprocess.Popen(cmd, cwd=cwd, stdout=fo, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, executable=executable, env=env_of(cmd))
    peak = [0]
    stop = threading.Event()

    def poll():
        path = f"/proc/{p.pid}/status"
        while not stop.is_set():
            try:
                with open(path) as f:
                    for line in f:
                        if line.startswith("VmHWM:"):
                            peak[0] = max(peak[0], int(line.split()[1]))
                            break
            except OSError:
                return
            time.sleep(0.001)

    t = threading.Thread(target=poll, daemon=True)
    t.start()
    p.wait()
    stop.set()
    t.join()
    fo.close()
    return peak[0] / 1024


def bench_memory(tools: dict, corpora: dict, res: dict) -> None:
    """Fill `maxrss_mb` of the text-search, structural and index rows with sampled peaks."""
    for r in res.get("text_search", []):
        if r.get("status") != "ok":
            continue
        t = tools[r["tool"]]
        q = next(x for x in QUERIES if x["name"] == r["query"])
        cmd = command(r["tool"], t, q, corpora[r["corpus"]])
        exe = t["exe"] if t.get("argv0") else None
        r["maxrss_mb"] = max(peak_rss_mb(cmd, corpora[r["corpus"]], exe) for _ in range(2))
        print(f"  mem {r['corpus']:8} {r['query']:26} {r['tool']:8} {r['maxrss_mb']:7.1f} MB", file=sys.stderr)
    for r in res.get("structural", []):
        cmd = structural_commands(tools)[r["tool"]]
        r["maxrss_mb"] = peak_rss_mb(cmd, corpora[r["corpus"]])
        print(f"  mem structural {r['corpus']} {r['tool']}: {r['maxrss_mb']:.0f} MB", file=sys.stderr)


def structural_commands(tools: dict) -> dict[str, list[str]]:
    # --no-cache: every file parsed on each run, as ast-grep does
    cmds = {"scrutate tsquery": [SCRUTATE, "tsquery", "--no-cache", "python", "(function_definition) @f", "--limit", "100000000", "."]}
    sg = tools.get("ast-grep", {}).get("exe")
    if sg:
        # the same node kind through ast-grep's rule engine (a `def $F($$$A): $$$B` pattern would
        # miss annotated or decorated definitions); --json=stream prints one match per line
        cmds["ast-grep"] = [sg, "scan", "--inline-rules", "id: fn\nlanguage: python\nrule:\n  kind: function_definition\n", "--json=stream", "."]
    return cmds


def drop_caches() -> bool:
    try:
        subprocess.run(["sync"], check=False)
        with open("/proc/sys/vm/drop_caches", "w") as f:
            f.write("3\n")
        return True
    except OSError:
        return False


def corpus_stats(d: str) -> dict:
    files = lines = size = 0
    for dp, dns, fns in os.walk(d):
        dns[:] = [x for x in dns if x not in (".git", ".scrutate")]
        for fn in fns:
            p = os.path.join(dp, fn)
            try:
                b = open(p, "rb").read()
            except OSError:
                continue
            files += 1
            size += len(b)
            lines += b.count(b"\n")
    return {"files": files, "lines": lines, "bytes": size}


def ensure_git(d: str) -> bool:
    if os.path.isdir(os.path.join(d, ".git")):
        return True
    try:
        subprocess.run(["git", "init", "-q"], cwd=d, check=True)
        subprocess.run(["git", "add", "-A", "."], cwd=d, check=True)
        subprocess.run(["git", "-c", "user.email=bench@x", "-c", "user.name=bench", "commit", "-q", "-m", "bench"], cwd=d, check=True)
        return True
    except subprocess.CalledProcessError:
        return False


QUERIES = [
    {"name": "common literal", "pattern": "import", "mode": "literal"},
    {"name": "rare literal", "pattern": "refresh_token", "mode": "literal"},
    {"name": "regex", "pattern": r"def\s+\w+_test\w*\(", "mode": "regex"},
    {"name": "case-insensitive", "pattern": "warning", "mode": "icase"},
    {"name": "whole word", "pattern": "self", "mode": "word"},
    {"name": "count only", "pattern": "return", "mode": "count"},
    {"name": "files with matches", "pattern": "numpy", "mode": "files"},
    {"name": "multiline", "pattern": r"try:\n\s+import", "mode": "multiline"},
    {"name": "identifier (all casings)", "pattern": "addNode", "mode": "identifier",
     "ident_regex": r"(?i)\badd[_\-]?node\b", "ident_regex_posix": r"\badd[_-]?node\b"},
    {"name": "fuzzy (1 edit)", "pattern": "shortest_pth", "mode": "fuzzy"},
]


def bench_text_search(tools: dict, corpora: dict, runs: int, cold: bool) -> list[dict]:
    rows = []
    for cname, cdir in corpora.items():
        n_runs = max(2, runs // 2) if corpus_stats(cdir)["bytes"] > 500e6 else runs  # the massive corpus: fewer repetitions
        has_git = ensure_git(cdir) if tools["git"]["exe"] else False
        for q in QUERIES:
            counts: dict[str, int] = {}
            for tool, t in tools.items():
                if not t.get("exe") or tool == "ast-grep":
                    continue
                if tool == "git" and not has_git:
                    continue
                cmd = command(tool, t, q, cdir)
                if cmd is None:
                    rows.append({"corpus": cname, "query": q["name"], "tool": tool, "status": "unsupported"})
                    continue
                exe = t["exe"] if t.get("argv0") else None
                first = timed_rusage(cmd, cdir, executable=exe)
                if first["rc"] not in (0, 1):
                    rows.append({"corpus": cname, "query": q["name"], "tool": tool, "status": f"exit {first['rc']}: {first['err'][:200].decode(errors='replace')}"})
                    continue
                n = count_lines(q["mode"], first["out"])
                counts[tool] = n
                walls, cpus = [], []
                for _ in range(n_runs):
                    r = timed_rusage(cmd, cdir, executable=exe)
                    walls.append(r["wall"])
                    cpus.append(r["cpu"])
                row = {
                    "corpus": cname, "query": q["name"], "tool": tool, "status": "ok", "matches": n,
                    "wall_median_ms": 1000 * statistics.median(walls), "wall_min_ms": 1000 * min(walls), "wall_ms": [1000 * w for w in walls],
                    "cpu_median_s": statistics.median(cpus), "maxrss_mb": peak_rss_mb(cmd, cdir, exe),
                    "cmd": shown(cmd),
                }
                if cold:
                    if drop_caches():
                        r = timed_rusage(cmd, cdir, executable=exe)
                        row["cold_wall_ms"] = 1000 * r["wall"]
                    else:
                        row["cold_wall_ms"] = None
                rows.append(row)
                print(f"  {cname:8} {q['name']:26} {tool:8} {row['wall_median_ms']:9.1f} ms  matches={n}", file=sys.stderr)
            # agreement
            ref = counts.get("rg", counts.get("scrutate"))
            for r in rows:
                if r["corpus"] == cname and r["query"] == q["name"] and r.get("status") == "ok":
                    r["agrees"] = (r["matches"] == ref)
    return rows


def bench_startup(tools: dict, runs: int) -> list[dict]:
    rows = []
    with tempfile.TemporaryDirectory() as d:
        open(os.path.join(d, "a.txt"), "w").write("hello\n")
        q = {"pattern": "zzzzqqqq", "mode": "literal"}
        for tool, t in tools.items():
            if not t.get("exe") or tool in ("ast-grep", "git"):
                continue
            cmd = command(tool, t, q, d)
            exe = t["exe"] if t.get("argv0") else None
            walls = []
            status = "ok"
            for _ in range(runs * 3):
                r = timed_rusage(cmd, d, executable=exe)
                # the file was searched and nothing matched: exit 1 and no output
                if r["rc"] != 1 or r["out"]:
                    status = f"exit {r['rc']}, {len(r['out'])} bytes printed"
                    break
                walls.append(r["wall"])
            if status != "ok":
                rows.append({"tool": tool, "status": status})
                continue
            rows.append({"tool": tool, "status": "ok", "startup_median_ms": 1000 * statistics.median(walls), "startup_min_ms": 1000 * min(walls)})
    return rows


def reference_count(tools: dict, text_rows: list[dict], cname: str, cdir: str, q: dict) -> int | None:
    """Matching lines of `q` by the text phase's rule (ripgrep's count, else scrutate's), from its
    rows when it ran, else from one run of that tool."""
    counts = {r["tool"]: r["matches"] for r in text_rows if r["corpus"] == cname and r["query"] == q["name"] and r.get("status") == "ok"}
    if "rg" in counts or "scrutate" in counts:
        return counts.get("rg", counts.get("scrutate"))
    for tool in ("rg", "scrutate"):
        if tools.get(tool, {}).get("exe"):
            r = timed_rusage(command(tool, tools[tool], q, cdir), cdir)
            return count_lines(q["mode"], r["out"]) if r["rc"] in (0, 1) else None
    return None


def bench_concurrency(tools: dict, corpora: dict, runs: int, text_rows: list[dict], k: int = 4) -> list[dict]:
    """K identical searches at once: aggregate wall time (how a shared CI box or an editor firing
    several searches behaves). Each process writes to a file of its own, and every round checks each
    output's line count against the reference; a tool that disagrees is `wrong` and not timed further."""
    rows = []
    q = next(x for x in QUERIES if x["name"] == "common literal")
    for cname, cdir in corpora.items():
        expected = reference_count(tools, text_rows, cname, cdir, q)
        for tool, t in tools.items():
            if not t.get("exe") or tool in ("ast-grep", "git"):
                continue
            cmd = command(tool, t, q, cdir)
            exe = t["exe"] if t.get("argv0") else None
            row = {"corpus": cname, "tool": tool, "k": k, "expected": expected, "status": "ok"}
            walls = []
            for _ in range(runs):
                outs = [tempfile.TemporaryFile() for _ in range(k)]
                t0 = time.perf_counter()
                procs = [subprocess.Popen(cmd, cwd=cdir, stdout=fo, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL, executable=exe, env=env_of(cmd))
                         for fo in outs]
                for p in procs:
                    p.wait()
                walls.append(time.perf_counter() - t0)
                got = []
                for fo in outs:
                    fo.seek(0)
                    # output lines are never empty (path:line:text), so newlines count them as count_lines does
                    got.append(sum(chunk.count(b"\n") for chunk in iter(lambda: fo.read(1 << 20), b"")))
                    fo.close()
                if expected is not None and any(n != expected for n in got):
                    row.update({"status": "wrong", "matches": got})
                    break
            if row["status"] == "ok":
                row.update({"rounds": len(walls), "wall_ms": 1000 * statistics.median(walls), "wall_min_ms": 1000 * min(walls), "walls_ms": [1000 * w for w in walls]})
            rows.append(row)
            print(f"  concurrency {cname:8} {tool:8} {row['status']} {row.get('wall_ms', float('nan')):9.1f} ms", file=sys.stderr)
    return rows


def bench_index(corpora: dict, runs: int, cold: bool) -> list[dict]:
    """Engine B: cold index build, warm no-op update, incremental update after one edit, index size,
    ranked-query latency (in-process via `batch --timing`, and whole-process via `scrutate search`)."""
    rows = []
    qfile = os.path.join(WORK, "bench", "queries", "networkx.json")
    for cname, cdir in corpora.items():
        shutil.rmtree(os.path.join(cdir, ".scrutate"), ignore_errors=True)
        if cold:
            drop_caches()
        t0 = time.perf_counter()
        build_rss = peak_rss_mb([SCRUTATE, "index"], cdir)
        build_s = time.perf_counter() - t0
        r2 = timed_rusage([SCRUTATE, "index"], cdir)
        noop_s = r2["wall"]
        # incremental: touch one file's content
        some = None
        for dp, dns, fns in os.walk(cdir):
            dns[:] = [x for x in dns if x not in (".git", ".scrutate")]
            for fn in fns:
                if fn.endswith(".py"):
                    some = os.path.join(dp, fn)
                    break
            if some:
                break
        inc_s = None
        if some:
            with open(some, "a") as f:
                f.write("\n# bench touch\n")
            inc_s = timed_rusage([SCRUTATE, "index"], cdir)["wall"]
            with open(some) as f:
                s = f.read()
            with open(some, "w") as f:
                f.write(s.replace("\n# bench touch\n", ""))
            timed_rusage([SCRUTATE, "index"], cdir)
        size = 0
        for dp, _, fns in os.walk(os.path.join(cdir, ".scrutate")):
            for fn in fns:
                size += os.path.getsize(os.path.join(dp, fn))
        cs = corpus_stats(cdir)
        row = {"corpus": cname, "files": cs["files"], "lines": cs["lines"], "corpus_mb": cs["bytes"] / 1e6, "index_build_s": build_s,
               "index_build_maxrss_mb": build_rss, "index_noop_s": noop_s, "index_incremental_s": inc_s, "index_mb": size / 1e6,
               "index_ratio": size / max(cs["bytes"], 1)}
        # ranked search latency: in-process timings over the benchmark queries
        if os.path.exists(qfile):
            qs = [x["query"] for x in json.load(open(qfile))][:150]
            with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
                for q in qs:
                    f.write(json.dumps({"query": q, "k": 10}) + "\n")
                qpath = f.name
            r = timed_rusage([SCRUTATE, "batch", qpath, "--timing"], cdir)
            os.unlink(qpath)
            ms = []
            for line in r["out"].decode(errors="replace").splitlines():
                try:
                    ms.append(json.loads(line)["ms"])
                except (ValueError, KeyError):
                    pass
            if ms:
                ms.sort()
                row["search_p50_ms"] = ms[len(ms) // 2]
                row["search_p95_ms"] = ms[int(len(ms) * 0.95)]
                row["search_queries"] = len(ms)
            # whole process: start, open index, load models, search, print
            walls = [timed_rusage([SCRUTATE, "search", "shortest path between two nodes"], cdir)["wall"] for _ in range(runs)]
            row["search_process_median_ms"] = 1000 * statistics.median(walls)
            if cold and drop_caches():
                row["search_process_cold_ms"] = 1000 * timed_rusage([SCRUTATE, "search", "shortest path between two nodes"], cdir)["wall"]
        rows.append(row)
        print(f"  index {cname}: build {build_s:.1f}s, {size/1e6:.0f} MB, incremental {inc_s}", file=sys.stderr)
    return rows


def bench_structural(tools: dict, corpora: dict, runs: int) -> list[dict]:
    """Structural search: ast-grep pattern vs scrutate tree-sitter query (same semantics: every Python
    function definition). Counts must agree."""
    rows = []
    for cname, cdir in corpora.items():
        cmds = structural_commands(tools)
        counts = {}
        for name, cmd in cmds.items():
            first = timed_rusage(cmd, cdir, count_only=True)
            if first["rc"] not in (0, 1):
                counts[name] = None
                rows.append({"corpus": cname, "tool": name, "status": f"exit {first['rc']}: {first['err'][:200].decode(errors='replace')}",
                             "cmd": " ".join(shlex_quote(c) for c in cmd)})
                print(f"  structural {cname} {name}: {rows[-1]['status']}", file=sys.stderr)
                continue
            counts[name] = first["out"]
            walls = [timed_rusage(cmd, cdir, count_only=True)["wall"] for _ in range(runs)]
            rows.append({"corpus": cname, "tool": name, "status": "ok", "matches": counts[name], "wall_median_ms": 1000 * statistics.median(walls),
                         "maxrss_mb": peak_rss_mb(cmd, cdir), "cmd": " ".join(shlex_quote(c) for c in cmd)})
            print(f"  structural {cname} {name}: {counts[name]} in {rows[-1]['wall_median_ms']:.0f} ms", file=sys.stderr)
        for r in rows:
            if r["corpus"] == cname:
                r["agrees"] = None not in counts.values() and len(set(counts.values())) == 1
    return rows


def environment(tools: dict) -> dict:
    cpu = ""
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True).stdout.strip()
    return {
        "date": time.strftime("%Y-%m-%d %H:%M:%S %z"),
        "platform": platform.platform(),
        "cpu": cpu,
        "cores": os.cpu_count(),
        "mem_gb": round(os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES") / 1e9, 1),
        "tools": {k: {"version": v.get("version"), "path": v.get("exe"), "note": v.get("note")} for k, v in tools.items()},
        "scrutate_sha256": hashlib.sha256(open(SCRUTATE, "rb").read()).hexdigest(),
    }


def main() -> int:
    drop_scrutate_settings()
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", action="append", required=True, help="NAME=DIR")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--cold", action="store_true", help="also measure one cold-cache run per cell (needs root)")
    ap.add_argument("--skip-index", action="store_true")
    ap.add_argument("--only", default=None, help="comma-separated phases to (re)run: text,startup,concurrency,structural,index; other phases are kept from --out if it exists")
    ap.add_argument("--out", default=None)
    a = ap.parse_args()
    corpora = dict(c.split("=", 1) for c in a.corpus)
    tools = discover()
    print("tools:", {k: (v.get("version") or "not available") for k, v in tools.items()}, file=sys.stderr)
    out = a.out or os.path.join(HERE, "..", "work", "bench_results", time.strftime("%Y%m%d_%H%M%S") + ".json")
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    phases = set(a.only.split(",")) if a.only else {"text", "startup", "concurrency", "structural", "index"}
    # "memory" re-samples the peak RSS of rows already in --out (used after a run that predates sampling)
    if a.skip_index:
        phases.discard("index")
    res = json.load(open(out)) if a.only and os.path.exists(out) else {}
    env = environment(tools)
    # phases kept from the file would stand under this binary's hash
    if res and res.get("environment", {}).get("scrutate_sha256") != env["scrutate_sha256"]:
        raise SystemExit(f"{out} holds phases that another scrutate binary measured: move the file away, or run every phase")
    res.update({"environment": env, "corpora": {k: corpus_stats(v) for k, v in corpora.items()}, "runs": a.runs})

    def save():
        json.dump(res, open(out, "w"), indent=1)

    if "text" in phases:
        res["text_search"] = bench_text_search(tools, corpora, a.runs, a.cold)
        save()
    if "startup" in phases:
        res["startup"] = bench_startup(tools, a.runs)
        save()
    if "concurrency" in phases:
        res["concurrency"] = bench_concurrency(tools, corpora, a.runs, res.get("text_search", []))
        save()
    if "structural" in phases:
        res["structural"] = bench_structural(tools, corpora, max(1, a.runs // 2))
        save()
    if "index" in phases:
        res["index"] = bench_index(corpora, a.runs, a.cold)
        save()
    if "memory" in phases:
        bench_memory(tools, corpora, res)
        save()
    print(f"wrote {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
