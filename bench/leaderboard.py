#!/usr/bin/env python3
"""Benchmark leaderboard for Engine A and structural search (mission section 14).

usage: leaderboard.py --corpora DIR --out FILE.json [--only ID_PREFIX...] [--runs 30]
                      [--cache warm|cold] [--cold-prepare CMD] [--max-load X] [--no-stop-early] [--force]

Per benchmark:
 1. Correctness. Every tool runs the query once; its output is normalised to the set of
    (path, line) pairs and compared with a reference computed independently in Python over an
    explicit file list (text files of the corpus under the mode's ignore rules). A tool whose
    answer differs is disqualified for that benchmark, scrutate included.
 2. Pilot. Three runs per valid tool; the fastest competitor, and any tool within 1.25x of the
    fastest (`--contender`), are contenders. So are the competitors with the lowest pilot CPU time
    and the lowest pilot peak memory (metric contenders).
 3. Measurement. hyperfine, output piped (no /dev/null shortcuts), 30 runs (20 under the stop
    rule below) for scrutate and every contender, taken in interleaved blocks (A B, A B, A B; one warm-up run per block) so load
    drift on a shared machine does not bias the comparison (cold cache: the prepare command runs
    before every run). `scrutate -j1` (one search thread, the documented mode for the least CPU
    time and memory) runs scrutate's command lines with -j1 in the same blocks; it is ranked on CPU
    time and memory only. With SCRUTATE_BASE_BIN set, `scrutate base` runs scrutate's command lines
    with that binary in the same blocks, for bench/leaderboard_regression.py; it is never ranked.
    A concurrent row times each tool's verified command started 4 times at once by one shell.
    Stop rule (on by default, off with --no-stop-early or SCRUTATE_BASE_BIN): after the second block a row stops when
    its wall-time verdict and its four CPU and memory verdicts are PASS, FAIL or n/a at alpha 0.001
    and no tool's block medians differ by more than 25%; every other row takes all its blocks and is
    judged at alpha 0.049 (the Haybittle-Peto boundary; the alphas sum to 0.05). The row records its
    run count (`runs`) and the alpha of its interval (`alpha`).
 4. Status. The 95% confidence interval of median(scrutate) / median(best competitor), by a
    bootstrap over the runs: PASS if it lies below 1, FAIL above 1, PARTIAL if it contains 1;
    BLOCKED (a tie at the hardware limit) when it contains 1 and both medians are within 15% of
    the time to merely read the same files (bench/rawread.c, the fastest of several thread
    counts). The same rule, without BLOCKED, gives a status for CPU time (user + system) and for
    peak resident memory, against the competitor lowest on that metric, for the default mode and
    for -j1 (`metrics`).
 5. Stability. A contender whose block medians differ by more than 25% was measured while the
    machine did something else. `--redo-unstable` measures such benchmarks again, once, whatever
    their status; the first measurement's status, ratio, interval and load stay in the row
    (`first`). A warm-cache row records how much of its corpus was in the page cache when it
    ended (`resident`); a second measurement with less than 95% there is not kept.
Results are written after every benchmark, so an interrupted run resumes where it stopped.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import random
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, ".."))
SCRUTATE = os.environ.get("SCRUTATE_BIN", os.path.join(ROOT, "target", "release", "scrutate"))
SCRUTATE_BASE = os.environ.get("SCRUTATE_BASE_BIN") or None


def drop_scrutate_settings():
    """Drop every SCRUTATE_ variable, so no tool a run starts reads one as a setting (scrutate then
    reads its arguments the slower way). Each benchmark's main() calls it; an import does not."""
    for name in [k for k in os.environ if k.startswith("SCRUTATE_")]:
        del os.environ[name]


def which(*names):
    for n in names:
        p = n if os.path.isabs(n) else shutil.which(n)
        if p and os.path.exists(p):
            return p
    return None


BIN = {
    "scrutate": SCRUTATE,
    "ripgrep": which("/opt/homebrew/bin/rg", "rg"),
    "ugrep": which("/opt/homebrew/bin/ugrep", "ugrep"),
    "GNU grep": which("/opt/homebrew/bin/ggrep", "ggrep") or (which("grep") if platform.system() == "Linux" else None),
    "git grep": which("/opt/homebrew/bin/git", "git"),
    "ag": which("/opt/homebrew/bin/ag", "ag"),
    "ack": which("/opt/homebrew/bin/ack", "ack"),
    "ast-grep": which("/opt/homebrew/bin/ast-grep", "ast-grep"),
    "scrutate --index": SCRUTATE,
    "scrutate -j1": SCRUTATE,
    **({"scrutate base": SCRUTATE_BASE} if SCRUTATE_BASE else {}),
}
BASE = ["scrutate base"] if SCRUTATE_BASE else []
ENGINE_A = ["scrutate", *BASE, "ripgrep", "ugrep", "GNU grep", "git grep", "ag", "ack"]
# never ranked for wall time: an index answers from work done in advance, the base is the change's
# yardstick, and -j1 is the documented mode for the least CPU time and memory (ranked on those only)
CROSS = ["scrutate --index", *BASE, "scrutate -j1"]
# the modes ranked on CPU time and peak memory; the default is the product's entry
MODES = {"default": "scrutate", "-j1": "scrutate -j1"}


def version(tool):
    b = BIN.get(tool)
    if not b:
        return None
    arg = {"ag": "--version", "git grep": "--version"}.get(tool, "--version")
    try:
        out = subprocess.run([b, arg], capture_output=True, text=True, timeout=20).stdout.strip().splitlines()
        return out[0] if out else None
    except Exception:
        return None


# ------------------------------------------------------------------------- the suite
def Q(pats, fixed=False, icase=False, word=False, multiline=False, ident=None, unicode=False, ref=None):
    """`ref`: the same language written for Python's backtracking `re`, which computes the reference answer."""
    q = {"pats": pats if isinstance(pats, list) else [pats], "fixed": fixed, "icase": icase, "word": word, "multiline": multiline, "ident": ident,
         "unicode": unicode}
    if ref:
        q["ref"] = ref  # only when set: the query dict is part of the reference cache key
    return q


QT = {
    "literal": Q("PM_RESUME", fixed=True),
    "multiple literals": Q(["ERR_PTR", "PTR_ERR", "IS_ERR_OR_NULL"], fixed=True),
    "case-insensitive": Q("pm_resume", fixed=True, icase=True),
    "whole-word": Q("ERR_PTR", fixed=True, word=True),
    "simple regex": Q("[A-Z]+_RESUME"),
    "complex regex": Q(r"\w+_(suspend|resume)\("),
    "unicode": Q("KÖNIG", icase=True, unicode=True),
    "no match": Q("zqxjkvbw_never_present_symbol", fixed=True),
    "very many matches": Q("return", fixed=True),
    "multiline": Q(r"struct file_operations \w+ = \{\n\s+\.owner", multiline=True),
    "identifier": Q(None, ident="kernelThread"),
}


def suite():
    B = []

    def add(bid, corpus, qtype, q=None, mode="ignore"):
        B.append({"id": bid, "corpus": corpus, "qtype": qtype, "q": q or QT[qtype], "mode": mode})

    for t in QT:
        add(f"linux/{t}", "linux", t)
    add("linux/unrestricted", "linux", "literal (unrestricted)", Q("kmalloc", fixed=True), "unrestricted")
    add("linux/file-type filter", "linux", "literal (*.c, *.h)", Q("kmalloc", fixed=True), "types")
    # the same command line as linux/literal, started 4 times at once: the same answer and verdict
    B.append({"id": "linux/4 at once (literal)", "corpus": "linux", "qtype": "literal, 4 at once", "q": QT["literal"], "mode": "ignore", "concurrent": 4})
    for t in ["literal", "case-insensitive", "complex regex", "very many matches", "no match", "identifier"]:
        q = dict(QT[t])
        if t == "literal":
            q = Q("getOperandAsUnsigned", fixed=True)
        if t == "case-insensitive":
            q = Q("isascii", fixed=True, icase=True)
        if t == "identifier":
            q = Q(None, ident="getTypeSize")
        add(f"llvm/{t}", "llvm", t, q)
    add("ripgrep/literal", "ripgrep", "literal", Q("Searcher", fixed=True))
    add("ripgrep/simple regex", "ripgrep", "simple regex", Q(r"[A-Z][a-z]+Builder"))
    add("ripgrep/case-insensitive", "ripgrep", "case-insensitive", Q("error", fixed=True, icase=True))
    B.append({"id": "ripgrep/4 at once (literal)", "corpus": "ripgrep", "qtype": "literal, 4 at once", "q": Q("Searcher", fixed=True), "mode": "ignore",
              "concurrent": 4})
    for t in ["literal", "case-insensitive", "complex regex", "very many matches", "no match", "unicode"]:
        add(f"bigfile/{t}", "bigfile", t)
    add("smallfiles/literal", "smallfiles", "literal", Q("spin_lock_irqsave", fixed=True))
    add("smallfiles/simple regex", "smallfiles", "simple regex", Q(r"[A-Z]+_MAX\b"))
    add("binheavy/literal", "binheavy", "literal", Q("spin_lock", fixed=True))
    add("patho/long line", "patho", "literal", Q("PATHOLOGICAL_NEEDLE", fixed=True))
    add("patho/one-byte lines", "patho", "very many matches", Q("x", fixed=True))
    # every run of one or more a's splits into a and aa pieces, so ^a+$ is the same language
    add("patho/backtracking trap", "patho", "simple regex", Q(r"^(a|aa)+$", ref=r"^a+$"))
    add("rgbuilt/git-aware", "rgbuilt", "literal (git-ignored build output present)", Q("Searcher", fixed=True), "git")
    # start-up: one small file and no match, so the time is starting, walking and exiting
    add("startup/no match", "startup", "no match")
    # any argument list: a two-argument pattern is not the same query in both tools (tree-sitter's
    # anchors count a comment between arguments as an argument, ast-grep's $A, $B does not)
    B.append({"id": "linux/structural", "corpus": "linux", "qtype": "structural", "mode": "ignore", "structural": {
        # alone, `kmalloc($$$A)` parses as a type macro in C; as a statement with a selector it is a call
        "lang": "c", "ast_grep": "kmalloc($$$A);", "ast_grep_selector": "call_expression",
        "tsquery": '(call_expression function: (identifier) @f (#eq? @f "kmalloc")) @call'}})
    B.append({"id": "ripgrep/structural", "corpus": "ripgrep", "qtype": "structural", "mode": "ignore", "structural": {
        "lang": "rust", "ast_grep": "format!($$$A)",
        "tsquery": '(macro_invocation macro: (identifier) @m (#eq? @m "format")) @call'}})
    return B


# ------------------------------------------------------------------------- command lines
def ident_re(word):
    parts = re.findall(r"[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|\d+", word)
    return r"\b" + "[_-]?".join(p.lower() for p in parts) + r"\b"


def alternation(q):
    parts = [re.escape(p) if q["fixed"] else p for p in q["pats"]]
    return parts[0] if len(parts) == 1 else "|".join(f"(?:{p})" for p in parts)


def unicode_sensitive(q):
    """Queries whose answer depends on Unicode word, class or case rules (vs bytes / ASCII)."""
    return bool(q["unicode"] or q["word"] or q["icase"] or q["ident"] or (not q["fixed"] and any(re.search(r"\\[wWbB]|\.", p) for p in q["pats"])))


def is_git(path):
    return os.path.isdir(os.path.join(path, ".git"))


def argv(tool, q, mode, corpus_dir):
    """The command line for `tool`, or None when the tool cannot express the query or mode."""
    b = BIN.get(tool)
    if not b:
        return None, {}
    env = {}
    git = is_git(corpus_dir)
    if tool in ("scrutate", "scrutate --index", "scrutate base", "scrutate -j1"):
        if tool == "scrutate --index" and mode != "ignore":
            return None, env
        a = [b, "grep"] + {"scrutate --index": ["--index"], "scrutate -j1": ["-j1"]}.get(tool, [])
        if q["ident"]:
            a += ["--ident", q["ident"]]
        else:
            a += (["-F"] if q["fixed"] else []) + (["-i"] if q["icase"] else []) + (["-w"] if q["word"] else []) + (["-U"] if q["multiline"] else [])
            for p in q["pats"]:
                a += ["-e", p]
        a += {"ignore": [], "git": [], "unrestricted": ["--no-ignore", "--hidden", "-g", "!.git", "-g", "!.scrutate"], "types": ["-g", "*.c", "-g", "*.h"]}[mode]
        return a, env
    if tool == "ripgrep":
        a = [b, "--no-config", "-n", "--no-heading", "--color", "never"]
        if q["ident"]:
            a += ["-i", "-e", ident_re(q["ident"])]
        else:
            a += (["-F"] if q["fixed"] else []) + (["-i"] if q["icase"] else []) + (["-w"] if q["word"] else []) + (["-U"] if q["multiline"] else [])
            for p in q["pats"]:
                a += ["-e", p]
        a += {"ignore": [], "git": [], "unrestricted": ["-uu", "-g", "!.git", "-g", "!.scrutate"], "types": ["-g", "*.c", "-g", "*.h"]}[mode]
        return a + ["."], env
    if tool == "ugrep":
        a = [b, "-rn", "-I", "--color=never"]
        a += {"ignore": ["--ignore-files"], "git": ["--ignore-files"], "unrestricted": ["--hidden", "--exclude-dir=.git", "--exclude-dir=.scrutate"],
              "types": ["--ignore-files", "-O", "c,h"]}[mode]
        if q["ident"]:
            a += ["-i", "-e", ident_re(q["ident"])]
        else:
            a += (["-F"] if q["fixed"] else []) + (["-i"] if q["icase"] else []) + (["-w"] if q["word"] else [])
            for p in q["pats"]:
                a += ["-e", p]
        return a + ["."], env
    if tool == "GNU grep":
        if q["multiline"] or mode == "git":
            return None, env
        a = [b, "-rn", "-I", "--color=never"]
        a += {"ignore": ["--exclude=.*", "--exclude-dir=.?*"], "unrestricted": ["--exclude-dir=.git", "--exclude-dir=.scrutate"],
              "types": ["--exclude=.*", "--exclude-dir=.?*", "--include=*.c", "--include=*.h"]}[mode]
        if q["ident"]:
            a += ["-i", "-E", "-e", ident_re(q["ident"])]
        else:
            a += (["-F"] if q["fixed"] else ["-E"]) + (["-i"] if q["icase"] else []) + (["-w"] if q["word"] else [])
            for p in q["pats"]:
                a += ["-e", p]
        # a UTF-8 locale gives grep Unicode word and case rules; C is used where bytes suffice
        env = {"LC_ALL": "en_US.UTF-8" if unicode_sensitive(q) else "C"}
        return a + ["."], env
    if tool == "git grep":
        if q["multiline"] or (mode in ("unrestricted",) and not git):
            return None, env
        a = [b, "--no-pager", "grep", "-n", "-I", "--no-color"] + ([] if git else ["--no-index"])
        if mode == "unrestricted":
            a += ["--untracked", "--no-exclude-standard"]
        if q["ident"]:
            a += ["-i", "-P", "-e", ident_re(q["ident"])]
        elif q["word"]:
            # git's own -w uses ASCII word characters; PCRE look-arounds under a UTF-8 locale are Unicode
            a += ["-P"] + (["-i"] if q["icase"] else []) + ["-e", rf"(?<!\w)(?:{alternation(q)})(?!\w)"]
        else:
            a += (["-F"] if q["fixed"] else ["-P"]) + (["-i"] if q["icase"] else [])
            for p in q["pats"]:
                a += ["-e", p]
        if unicode_sensitive(q):
            env = {"LC_ALL": "en_US.UTF-8"}
        hidden = [":(exclude).*", ":(exclude)**/.*"]
        a += {"ignore": ["--", "."] + hidden, "git": ["--", "."] + hidden, "unrestricted": ["--", ".", ":(exclude).scrutate"],
              "types": ["--", "*.c", "*.h"] + hidden}[mode]
        return a, env
    if tool == "ag":
        a = [b, "--nocolor", "--nogroup", "-m", "1000000000"]
        a += {"ignore": [], "git": [], "unrestricted": ["-U", "--hidden", "--ignore", ".git", "--ignore", ".scrutate"], "types": ["-G", r"\.(c|h)$"]}[mode]
        if q["ident"]:
            return a + ["-i", ident_re(q["ident"]), "."], env
        a += ["-i"] if q["icase"] else ["-s"]
        a += ["-w"] if q["word"] else []
        if len(q["pats"]) == 1 and q["fixed"]:
            a += ["-Q"]
            pat = q["pats"][0]
        else:
            pat = alternation(q)
        return a + [pat, "."], env
    if tool == "ack":
        if q["multiline"] or mode == "git":
            return None, env
        a = [b, "--noenv", "--nocolor", "--nogroup", "-H"]
        a += {"ignore": ["--ignore-dir=match:/^\\./", "--ignore-file=match:/^\\./"], "unrestricted": ["--ignore-dir=.scrutate"],
              "types": ["--ignore-dir=match:/^\\./", "--ignore-file=match:/^\\./", "--type-set=chx:ext:c,h", "--type=chx"]}[mode]
        if q["ident"]:
            return a + ["-i", ident_re(q["ident"]), "."], env
        a += (["-i"] if q["icase"] else []) + (["-w"] if q["word"] else [])
        if len(q["pats"]) == 1 and q["fixed"]:
            return a + ["-Q", q["pats"][0], "."], env
        return a + [alternation(q), "."], env
    return None, env


def structural_argv(tool, spec):
    if tool in ("scrutate", "scrutate base") and BIN.get(tool):
        # --no-cache: every file parsed on each run, as ast-grep does; a kept capture cache would skip the parse
        return [BIN[tool], "tsquery", "--json", "--no-cache", spec["lang"], spec["tsquery"], ".", "--limit", "100000000"]
    if tool == "ast-grep" and BIN["ast-grep"]:
        sel = ["--selector", spec["ast_grep_selector"]] if spec.get("ast_grep_selector") else []
        return [BIN["ast-grep"], "run", "-p", spec["ast_grep"], *sel, "-l", spec["lang"], "--json=stream", "."]
    return None


def group_argv(cmd, k):
    """`cmd` started k times at once by one shell, which exits when all of them have; every tool pays
    the same shell start. The environment of `cmd` reaches the copies through the shell's."""
    one = " ".join(shlex.quote(x) for x in cmd)
    # the shell waits for each copy by its id and exits with a failed copy's status, so a copy that
    # fails only when several run at once is seen like a failed single run
    starts = " ".join(f"{one} & p{i}=$!;" for i in range(k))
    waits = " ".join(f"wait $p{i} || s=$?;" for i in range(k))
    return ["/bin/sh", "-c", f"{starts} s=0; {waits} exit $s"]


# ------------------------------------------------------------------------- output normalisation
LINE = re.compile(rb"^(?:\./)?(.*?)[:|](\d+)[:|]")


def normalise(out: bytes, structural=None):
    got = set()
    if structural == "scrutate":
        for line in out.splitlines():
            r = json.loads(line)
            if r.get("capture") == "call":
                got.add((r["path"], r["line"]))
        return got
    if structural == "ast-grep":
        for line in out.splitlines():
            r = json.loads(line)
            got.add((r["file"].removeprefix("./"), r["range"]["start"]["line"] + 1))
        return got
    for line in out.splitlines():
        m = LINE.match(line)
        if m:
            got.add((m.group(1).decode("utf-8", "surrogateescape"), int(m.group(2))))
    return got


# ------------------------------------------------------------------------- the reference
def hidden(rel):
    return any(p.startswith(".") for p in rel.split("/"))


def whitelisted_hidden(corpus_dir, paths):
    """Hidden paths the repository's .gitignore explicitly un-ignores (`!.mailmap`): ignore-aware
    walkers search those although they skip other hidden files. A path qualifies when every hidden
    component of it (directories as `dir/`) is decided by a negation rule."""
    prefixes = set()
    for p in paths:
        parts = p.split("/")
        for i, part in enumerate(parts):
            if part.startswith("."):
                prefixes.add("/".join(parts[: i + 1]) + ("/" if i < len(parts) - 1 else ""))
    if not prefixes:
        return set()
    out = subprocess.run(["git", "-c", "core.ignorecase=false", "check-ignore", "--no-index", "-v", "-n", "--stdin"], cwd=corpus_dir,
                         input="\n".join(sorted(prefixes)) + "\n", capture_output=True, text=True).stdout
    ok = set()
    for line in out.splitlines():
        rule, _, path = line.partition("\t")
        if rule.split(":", 2)[-1].startswith("!"):
            ok.add(path)
    keep = set()
    for p in paths:
        parts = p.split("/")
        if all(("/".join(parts[: i + 1]) + ("/" if i < len(parts) - 1 else "")) in ok for i, part in enumerate(parts) if part.startswith(".")):
            keep.add(p)
    return keep


def file_list(corpus_dir, mode):
    if mode in ("ignore", "git", "types") and is_git(corpus_dir):
        def ls(*args):
            # case-sensitive ignore matching, as the tools apply it (git turns it off on
            # case-insensitive file systems, where `*.s` would then hide every `.S` file)
            return [p for p in subprocess.run(["git", "-c", "core.ignorecase=false", "ls-files", "-z", *args], cwd=corpus_dir, capture_output=True, text=True,
                                              check=True).stdout.split("\0") if p]
        tracked_ignored = set(ls("-ci", "--exclude-standard"))
        files = [p for p in ls("--cached", "--others", "--exclude-standard") if p not in tracked_ignored]
    else:
        files = []
        for d, dirs, fs in os.walk(corpus_dir):
            dirs[:] = [x for x in dirs if x not in (".git", ".scrutate")]
            for f in fs:
                files.append(os.path.relpath(os.path.join(d, f), corpus_dir).replace(os.sep, "/"))
    if mode != "unrestricted":
        allowed = whitelisted_hidden(corpus_dir, [p for p in files if hidden(p)]) if is_git(corpus_dir) and mode != "types" else set()
        files = [p for p in files if (not hidden(p) or p in allowed) and not p.startswith(".scrutate/")]
    if mode == "types":
        files = [p for p in files if p.endswith((".c", ".h"))]
    out = []
    for p in sorted(set(files)):
        full = os.path.join(corpus_dir, p)
        if os.path.isfile(full) and not os.path.islink(full):
            out.append(p)
    return out


def py_pattern(q):
    if q["ident"]:
        pat, flags = ident_re(q["ident"]), re.IGNORECASE
    elif q.get("ref"):
        pat, flags = q["ref"], 0
    else:
        pat = alternation(q)
        if q["word"]:
            pat = rf"(?<!\w)(?:{pat})(?!\w)"
        flags = re.IGNORECASE if q["icase"] else 0
    return re.compile(pat, flags | re.MULTILINE)


def blocks(data, multiline, size=16 << 20):
    """(text, first line number) pieces split at line boundaries; one piece for multi-line queries."""
    if multiline or len(data) <= size:
        yield data.decode("utf-8", "surrogateescape"), 1
        return
    start, line = 0, 1
    while start < len(data):
        end = data.find(b"\n", min(start + size, len(data)) - 1)
        end = len(data) if end < 0 else end + 1
        piece = data[start:end]
        yield piece.decode("utf-8", "surrogateescape"), line
        line += piece.count(b"\n")
        start = end


def reference(corpus_dir, files, q):
    rx = py_pattern(q)
    hits = set()
    nfiles = 0
    for rel in files:
        with open(os.path.join(corpus_dir, rel), "rb") as f:
            data = f.read()
        if b"\0" in data:
            continue  # binary: not searched by any tool in these modes
        nfiles += 1
        for text, base in blocks(data, q["multiline"]):
            pos, line = 0, base
            for m in rx.finditer(text):
                line += text.count("\n", pos, m.start())
                pos = m.start()
                last = line + (text.count("\n", m.start(), max(m.start(), m.end() - 1)) if q["multiline"] else 0)
                for ln in range(line, last + 1):
                    hits.add((rel, ln))
    return hits, nfiles


# ------------------------------------------------------------------------- measurement
def loadavg():
    try:
        return os.getloadavg()[0]
    except OSError:
        return 0.0


def wait_idle(max_load, max_wait):
    t0 = time.time()
    while loadavg() > max_load and time.time() - t0 < max_wait:
        time.sleep(15)
    return loadavg()


def c_helper(name, *flags):
    """bench/NAME.c built into target/NAME, again when the source is newer; a job that never ran
    cargo has no target/ yet."""
    out, src = os.path.join(ROOT, "target", name), os.path.join(HERE, f"{name}.c")
    if not os.path.exists(out) or os.path.getmtime(out) < os.path.getmtime(src):
        os.makedirs(os.path.dirname(out), exist_ok=True)
        subprocess.run(["cc", "-O2", *flags, "-o", out, src], check=True)
    return out


def resident(lst, cwd):
    """Fraction of the listed files' pages that are in the page cache (bench/evict.c --resident);
    None where that cannot be measured."""
    try:
        out = c_helper("evict")
        m = re.search(r"pages: (\d+) total, (\d+) resident", subprocess.run([out, lst, "--resident"], cwd=cwd, capture_output=True, text=True).stdout)
        return int(m.group(2)) / max(int(m.group(1)), 1) if m else None
    except (OSError, subprocess.CalledProcessError):
        return None


def hyperfine(cmd, cwd, env, runs, warmup, prepare=None):
    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tf:
        path = tf.name
    a = ["hyperfine", "--output=pipe", "--style", "none", "--runs", str(runs), "--warmup", str(warmup), "--export-json", path, "-i"]
    if prepare:
        a += ["--prepare", prepare]
    else:
        a += ["-N"]
    a += [" ".join(shlex.quote(x) for x in cmd)]
    subprocess.run(a, cwd=cwd, env={**os.environ, **env}, capture_output=True, check=True)
    r = json.load(open(path))["results"][0]
    os.unlink(path)
    ms = r["measurements"]
    return {
        "n": len(ms),
        "times": [m["time_wall_clock"]["value"] for m in ms],
        "cpu": [m["time_cpu"]["value"] for m in ms],
        "rss": [m["memory_peak_resident"]["value"] for m in ms],
        "exit": sorted({m["exit_code"] for m in ms}),
    }


def boot_ratio(a, b, B=None, seed=1, alpha=0.05):
    """median(a) / median(b) and its 1 - alpha percentile bootstrap interval."""
    # at least 25 resamples beyond each end of the interval: 10,000 at alpha 0.05, 50,000 at 0.001
    B = B or max(10000, math.ceil(25 / (alpha / 2)))
    rng = random.Random(seed)
    med = statistics.median
    rs = []
    for _ in range(B):
        rs.append(med(rng.choices(a, k=len(a))) / med(rng.choices(b, k=len(b))))
    rs.sort()
    return med(a) / med(b), rs[round(alpha / 2 * B)], rs[round((1 - alpha / 2) * B) - 1]


# Stopping by evidence, the Haybittle-Peto boundary (Haybittle 1971; Peto et al. 1976): a look before
# the last stops a row only at alpha 0.001, the last look spends the rest, and the alphas sum to 0.05
# (Bonferroni), whatever the load did between the blocks.
ALPHA, INTERIM_ALPHA, FIRST_LOOK = 0.05, 0.001, 2


def stop_rule(blocks):
    """(blocks after which a row may stop early, the alpha of the last look)."""
    interim = list(range(FIRST_LOOK, blocks))
    return interim, round(ALPHA - INTERIM_ALPHA * len(interim), 6)


def settled(res):
    """True when the wall-time verdict and all four CPU and memory verdicts can no longer change
    and the blocks agree (no tool's block medians differ by more than 25%)."""
    return (res["status"] in ("PASS", "FAIL") and not unstable(res)
            and all(m["status"] in ("PASS", "FAIL", "n/a") for ms in res["metrics"].values() for m in ms.values()))


def bad_exit(codes):
    """Exit codes other than 0 (results) and 1 (none): the tool failed, whatever it printed."""
    return sorted(set(codes) - {0, 1})


def corpus_digest(corpora, name):
    """The corpus content hash bench/corpora.py records, or None for a corpus it did not make."""
    try:
        with open(os.path.join(corpora, "manifest.json")) as f:
            return json.load(f).get(name, {}).get("tree_sha256")
    except (OSError, ValueError):
        return None


RAWREAD: dict = {}


def rawread_bin():
    return c_helper("rawread", "-pthread")


def run_bench(b, corpora, a, cache_dir):
    cdir = os.path.join(corpora, b["corpus"])
    res = {"id": b["id"], "corpus": b["corpus"], "qtype": b["qtype"], "mode": b["mode"], "cache": a.cache, "tools": {}, "load": []}
    structural = b.get("structural")
    conc = b.get("concurrent")
    if conc:
        res["concurrent"] = conc
    # tsquery has no thread option, so the -j1 mode exists for text search only
    tools = ["scrutate", *BASE, "ast-grep"] if structural else ENGINE_A + ["scrutate --index", "scrutate -j1"]
    # 1. reference
    files = file_list(cdir, b["mode"])
    digest = corpus_digest(corpora, b["corpus"])
    if structural:
        ref, nref, unclear = structural_reference(cdir, files, structural, cache_dir, digest)
        # where the reference's own parse has errors, error recovery (runtime code that differs between
        # tree-sitter versions) decides what is a call: those files are searched but not compared
        ref = {h for h in ref if h[0] not in unclear}
    else:
        key = hashlib.sha1(json.dumps([b["corpus"], b["mode"], b["q"], len(files), files[:50], digest], sort_keys=True).encode()).hexdigest()[:16]
        cpath = os.path.join(cache_dir, f"ref-{key}.json")
        if os.path.exists(cpath):
            d = json.load(open(cpath))
            ref, nref = {tuple(x) for x in d["hits"]}, d["files"]
        else:
            t = time.time()
            ref, nref = reference(cdir, files, b["q"])
            json.dump({"hits": sorted(ref), "files": nref, "seconds": time.time() - t}, open(cpath, "w"))
    res["reference"] = {"lines": len(ref), "files_searched": nref, "files_listed": len(files)}
    if structural:
        res["reference"]["files_not_compared"] = len(unclear)
    if not structural and b["mode"] == "ignore":
        # the cross-category column needs the text index of this snapshot
        subprocess.run([SCRUTATE, "index", "--text"], cwd=cdir, capture_output=True, check=True)
    # 2. correctness + one timed run each
    for t in tools:
        cmd, env = (structural_argv(t, structural), {}) if structural else argv(t, b["q"], b["mode"], cdir)
        if not cmd:
            res["tools"][t] = {"status": "n/a"}
            continue
        t0 = time.time()
        try:
            p = subprocess.run(cmd, cwd=cdir, env={**os.environ, **env}, stdin=subprocess.DEVNULL, capture_output=True, timeout=a.timeout)
            took = time.time() - t0
        except subprocess.TimeoutExpired:
            res["tools"][t] = {"status": "timeout", "argv": cmd, "env": env, "timeout_s": a.timeout}
            continue
        got = normalise(p.stdout, ("scrutate" if t == "scrutate base" else t) if structural else None)
        if structural:
            got = {h for h in got if h[0] not in unclear}
        missing, extra = ref - got, got - ref
        # a list, as the timed runs give it: a tool that is too slow to be timed keeps this one
        rec = {"argv": cmd, "env": env, "exit": [p.returncode], "verify_s": took, "lines": len(got)}
        if bad_exit([p.returncode]):
            rec.update({"status": "error", "stderr": p.stderr.decode("utf-8", "replace")[-500:]})
        elif missing or extra:
            rec.update({"status": "wrong", "missing": len(missing), "extra": len(extra), "missing_sample": sorted(missing)[:5], "extra_sample": sorted(extra)[:5]})
        else:
            rec["status"] = "ok"
        if conc:
            # the single command's answer is the verdict; the timed command starts it k times at once
            rec["group_argv"] = group_argv(cmd, conc)
        res["tools"][t] = rec
    valid = [t for t in tools if res["tools"][t].get("status") == "ok"]
    lst = os.path.join(cache_dir, f"list-{hashlib.sha1((b['corpus'] + b['mode']).encode()).hexdigest()[:12]}.bin")
    with open(lst, "wb") as f:
        f.write(b"\0".join(p.encode("utf-8", "surrogateescape") for p in files))
    prepare = a.cold_prepare.replace("{list}", lst) if a.cold_prepare else None
    # 3. pilot, contenders, full runs
    if valid:
        # the cross-category column answers from an index: it never sets the pace for the others
        ranked = [t for t in valid if t not in CROSS] or valid
        fastest_verify = min(res["tools"][t]["verify_s"] for t in ranked)
        for t in valid:
            r = res["tools"][t]
            if r["verify_s"] > max(5.0, 20 * fastest_verify):
                # a concurrent row's correctness run is one copy, not the group: no time to show
                r.update({"n": 1, "note": "not a contender (more than 20x slower in the correctness run)", **({} if conc else {"times": [r["verify_s"]]})})
                continue
            res["load"].append(wait_idle(a.max_load, a.max_wait))
            r.update(hyperfine(r.get("group_argv") or r["argv"], cdir, r["env"], 3, 1, prepare))
        best_pilot = min(statistics.median(res["tools"][t]["times"]) for t in ranked if res["tools"][t].get("n", 0) >= 3)
        piloted = [t for t in valid if res["tools"][t].get("n", 0) >= 3 and not t.startswith("scrutate")]
        fastest_comp = min(piloted, key=lambda t: statistics.median(res["tools"][t]["times"]), default=None)
        contenders = [t for t in valid if res["tools"][t].get("n", 0) >= 3
                      and (t.startswith("scrutate") or t == fastest_comp or statistics.median(res["tools"][t]["times"]) <= a.contender * best_pilot)]
        # metric contenders: the pilot's lowest CPU time and lowest peak memory, often slower tools
        target = {t: a.runs for t in contenders}
        for key in ("cpu", "rss"):
            lead = min(piloted, key=lambda t: statistics.median(res["tools"][t][key]), default=None)
            if lead and lead not in target:
                slow = a.short_metric_runs and statistics.median(res["tools"][lead]["times"]) > 2.0
                target[lead] = a.runs // 2 if slow else a.runs
                contenders.append(lead)
            if lead:
                res["tools"][lead].setdefault("metric_contender", []).append(key)
        # interleaved blocks (A B C, A B C, ...): on a shared machine the load drifts, and one long
        # block per tool would turn that drift into a difference between the tools
        acc = {t: {"n": 0, "times": [], "cpu": [], "rss": [], "exit": set()} for t in contenders}
        per = -(-a.runs // a.blocks)
        interim, res["alpha"] = stop_rule(a.blocks) if a.stop else ([], ALPHA)
        for i in range(a.blocks):
            # alternate the order, so of two tools neither always runs first in a block
            for t in (contenders if i % 2 == 0 else contenders[::-1]):
                r = res["tools"][t]
                res["load"].append(wait_idle(a.max_load, a.max_wait))
                h = hyperfine(r.get("group_argv") or r["argv"], cdir, r["env"], -(-target[t] // a.blocks), 1, prepare)
                for k in ("times", "cpu", "rss"):
                    acc[t][k] += h[k]
                acc[t]["n"] += h["n"]
                acc[t]["exit"] |= set(h["exit"])
            res["runs"] = per * (i + 1)
            fill(res, acc, target, valid, a.runs, a.blocks, i + 1)
            if i + 1 in interim:
                decide(res, [t for t in valid if res["tools"][t]["status"] == "ok"], res["runs"], INTERIM_ALPHA)
                if settled(res):
                    res["alpha"] = INTERIM_ALPHA
                    break
        res["load"].append(loadavg())
    # 4. hardware ceiling and status (none for a concurrent row: it reads the files k times)
    if not structural and not conc:
        # the ceiling depends only on the file set and the cache state: measured once per run
        key = f"{b['corpus']}|{b['mode']}|{a.cache}|{len(files)}"
        if key not in RAWREAD:
            cpus = os.cpu_count() or 4
            # the ceiling is the fastest of a few thread counts: cold reads wait on the disk, which
            # takes more of them at once than there are cores, and warm reads on macOS are fastest
            # on the performance cores alone
            for threads in sorted({cpus, max(1, cpus // 2)} if a.cache == "warm" else {cpus, 24, 48, 96}):
                rr = hyperfine([rawread_bin(), lst, str(threads)], cdir, {}, a.runs if a.cache == "warm" else min(a.runs, 10), 3, prepare)
                cur = {"median": statistics.median(rr["times"]), "n": rr["n"], "threads": threads, "times": rr["times"]}
                if key not in RAWREAD or cur["median"] < RAWREAD[key]["median"]:
                    RAWREAD[key] = cur
        res["rawread"] = {k: v for k, v in RAWREAD[key].items() if k != "times"}
    if not structural and a.cache == "warm":
        # a warm run is one whose files were in memory: on a machine short of it they are not
        res["resident"] = resident(lst, cdir)
    valid = [t for t in valid if res["tools"][t]["status"] == "ok"]
    decide(res, valid, res.get("runs", a.runs), res.get("alpha", ALPHA))
    res["max_load"] = max(res["load"]) if res["load"] else loadavg()
    return res


def fill(res, acc, target, valid, runs, blocks, done):
    """The contenders' runs of the first `done` blocks into their rows, with medians and exit checks."""
    for t, x in acc.items():
        res["tools"][t].update({**x, "exit": sorted(x["exit"]), "blocks": done})
        if target[t] != runs:
            res["tools"][t]["runs_target"] = target[t] if done == blocks else -(-target[t] // blocks) * done
    for t in valid:
        r = res["tools"][t]
        if bad_exit(r.get("exit", [])):
            r.update({"status": "error", "note": f"exit {bad_exit(r['exit'])} in a timed run"})
        if "times" in r:
            r["median"] = statistics.median(r["times"])
            r["cpu_median"] = statistics.median(r["cpu"]) if r.get("cpu") else None
            r["rss_median"] = statistics.median(r["rss"]) if r.get("rss") else None


def decide(res, valid, runs, alpha=ALPHA):
    """Status of one benchmark from its stored runs (see the module docstring); `runs` and `alpha`
    are the row's own: the run count of its look and the alpha spent there."""
    for k in ("best_competitor", "ratio", "ci95", "why"):
        res.pop(k, None)
    us = res["tools"].get("scrutate", {})
    comps = [t for t in valid if not t.startswith("scrutate") and res["tools"][t].get("n", 0) >= runs]
    if us.get("status") != "ok":
        res["status"] = "FAIL"
        res["why"] = f"scrutate's answer is {us.get('status')}"
    elif not comps:
        # every competitor was wrong, timed out, cannot express the query, or was too slow to time
        others = [t for t in valid if not t.startswith("scrutate")]
        res["status"] = "PASS" if others else "PARTIAL"
        res["why"] = "every correct competitor is more than 20x slower" if others else "no competitor answers this query correctly"
    else:
        best = min(comps, key=lambda t: res["tools"][t]["median"])
        ratio, lo, hi = boot_ratio(us["times"], res["tools"][best]["times"], alpha=alpha)
        res.update({"best_competitor": best, "ratio": ratio, "ci95": [lo, hi], "status": ci_status(lo, hi)})
        if res["status"] == "PARTIAL":
            raw = res.get("rawread", {}).get("median")
            if raw and us["median"] <= 1.15 * raw and res["tools"][best]["median"] <= 1.15 * raw:
                res["status"] = "BLOCKED"
                res["why"] = "tie at the hardware limit (both within 15% of reading the files)"
    res["metrics"] = {key: {mode: metric_verdict(res["tools"], tool, valid, key, runs, alpha) for mode, tool in MODES.items()} for key in ("cpu", "rss")}


def ci_status(lo, hi):
    return "PASS" if hi < 1 else "FAIL" if lo > 1 else "PARTIAL"


def metric_verdict(tools, us_tool, valid, key, runs, alpha=ALPHA):
    """CPU time (`cpu`) or peak memory (`rss`) of one scrutate mode against the competitor lowest on
    that metric, by the wall-time rule; there is no hardware ceiling, so no BLOCKED. The rank counts
    every correct competitor with a median, the 3-run pilots included."""
    us = tools.get(us_tool)
    if not us:
        return {"status": "n/a", "why": f"{us_tool} cannot run this benchmark"}
    if us.get("status") != "ok" or us_tool not in valid:
        return {"status": "FAIL", "why": f"{us_tool}'s answer is {us.get('status')}"}
    med = statistics.median
    comps = {t: tools[t][key] for t in valid if not t.startswith("scrutate") and tools[t].get(key)}
    if not us.get(key):
        return {"status": "PARTIAL", "why": f"{us_tool} was not timed"}
    out = {"median": med(us[key]), "rank": 1 + sum(med(v) < med(us[key]) for v in comps.values()), "of": len(comps) + 1}
    full = {t: v for t, v in comps.items() if len(v) >= tools[t].get("runs_target", runs)}
    if not full or len(us[key]) < runs:
        return {**out, "status": "PARTIAL", "why": "no correct competitor was measured in full on this metric" if not full else f"{us_tool} has fewer than {runs} runs"}
    best = min(full, key=lambda t: med(full[t]))
    if min(full[best]) <= 0:
        return {**out, "status": "PARTIAL", "best_competitor": best, "why": f"{best} reads 0 in some runs"}
    ratio, lo, hi = boot_ratio(us[key], full[best], alpha=alpha)
    return {**out, "best_competitor": best, "best_median": med(full[best]), "ratio": ratio, "ci95": [lo, hi], "status": ci_status(lo, hi)}


# a warm-cache measurement needs this much of the corpus in the page cache
WARM = 0.95


def unstable(res):
    """True when the block medians of a fully measured tool differ by more than 25%."""
    for t in res["tools"].values():
        times, blocks = t.get("times"), t.get("blocks")
        if times and blocks and len(times) >= 3 * blocks:
            per = len(times) // blocks
            medians = [statistics.median(times[i * per:(i + 1) * per]) for i in range(blocks)]
            if max(medians) > 1.25 * min(medians):
                return True
    return False


def structural_reference(cdir, files, spec, cache_dir, digest=None):
    """Python tree-sitter over the same files: nodes matched by the query's outer capture."""
    key = hashlib.sha1(json.dumps([cdir, spec, len(files), digest]).encode()).hexdigest()[:16]
    cpath = os.path.join(cache_dir, f"sref-{key}.json")
    d = json.load(open(cpath)) if os.path.exists(cpath) else {}
    if "parse_errors" in d:
        return {tuple(x) for x in d["hits"]}, d["files"], set(d["parse_errors"])
    exts = {"c": (".c", ".h"), "rust": (".rs",)}[spec["lang"]]
    sel = [p for p in files if p.endswith(exts)]
    lst = os.path.join(cache_dir, "sref-files.txt")
    open(lst, "w").write("\n".join(sel))
    code = r'''
import json, sys, tree_sitter as ts
lang_mod = __import__(sys.argv[3])
lang = ts.Language(lang_mod.language())
q = ts.Query(lang, sys.argv[2])
cur = ts.QueryCursor(q)
parser = ts.Parser(lang)
hits = set(); n = 0; errors = []
for rel in open(sys.argv[1]).read().split("\n"):
    if not rel: continue
    src = open(rel, "rb").read()
    if b"\0" in src: continue
    n += 1
    tree = parser.parse(src)
    if tree.root_node.has_error:
        errors.append(rel)
    for _, caps in cur.matches(tree.root_node):
        for node in caps.get("call", []):
            hits.add((rel, node.start_point[0] + 1))
print(json.dumps({"hits": sorted(hits), "files": n, "parse_errors": errors}))
'''
    mod = {"c": "tree_sitter_c", "rust": "tree_sitter_rust"}[spec["lang"]]
    pkg = {"c": "tree-sitter-c", "rust": "tree-sitter-rust"}[spec["lang"]]
    # a Python the grammar packages publish wheels for (a newer one would try to build them)
    p = subprocess.run(["uv", "run", "--no-project", "-q", "--python", "3.13", "--with", "tree-sitter>=0.25", "--with", pkg, "python", "-c", code, lst, spec["tsquery"], mod],
                       cwd=cdir, capture_output=True, text=True, check=True)
    d = json.loads(p.stdout)
    json.dump(d, open(cpath, "w"))
    return {tuple(x) for x in d["hits"]}, d["files"], set(d["parse_errors"])


def environment():
    env = {"date": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "platform": platform.platform(), "machine": platform.machine(), "python": platform.python_version(), "cpus": os.cpu_count()}
    if platform.system() == "Darwin":
        q = lambda k: subprocess.run(["sysctl", "-n", k], capture_output=True, text=True).stdout.strip()
        env.update({"cpu": q("machdep.cpu.brand_string"), "memory_bytes": int(q("hw.memsize") or 0), "perf_cores": q("hw.perflevel0.physicalcpu"),
                    "efficiency_cores": q("hw.perflevel1.physicalcpu")})
    elif os.path.exists("/proc/cpuinfo"):
        lines = open("/proc/cpuinfo").read().splitlines()
        env["cpu"] = next((l.split(":", 1)[1].strip() for l in lines if l.startswith(("model name", "CPU part"))), "")
        env["memory_bytes"] = int(open("/proc/meminfo").read().split()[1]) * 1024
    env["tools"] = {t: version(t) for t in BIN}
    env["scrutate_sha256"] = hashlib.sha256(open(SCRUTATE, "rb").read()).hexdigest()
    if SCRUTATE_BASE:
        env["scrutate_base_sha256"] = hashlib.sha256(open(SCRUTATE_BASE, "rb").read()).hexdigest()
    env["hyperfine"] = subprocess.run(["hyperfine", "--version"], capture_output=True, text=True).stdout.strip() if shutil.which("hyperfine") else None
    return env


def check_hyperfine():
    """The harness reads each run from hyperfine 2's JSON (`measurements`); 1.x writes none."""
    try:
        out = subprocess.run(["hyperfine", "--version"], capture_output=True, text=True).stdout.strip()
    except OSError:
        raise SystemExit("hyperfine is not installed (version 2 or newer is needed)")
    m = re.search(r"(\d+)\.\d+", out)
    if not m or int(m.group(1)) < 2:
        raise SystemExit(f"hyperfine 2 or newer is needed (its JSON lists each run); found: {out or 'no version'}")


def _selfcheck() -> None:
    g = group_argv(["/x/rg", "-e", "it's", "."], 2)
    copy = ["/x/rg", "-e", "it's", ".", "&"]
    assert g[:2] == ["/bin/sh", "-c"] and shlex.split(g[2])[:12] == copy + ["p0=$!;"] + copy + ["p1=$!;"], g
    run = lambda c: subprocess.run(group_argv(["/bin/sh", "-c", c], 3)).returncode
    assert (run("exit 0"), run("exit 2")) == (0, 2)
    ok = lambda xs: {"status": "ok", "cpu": xs}
    tools = {"scrutate": ok([1.0, 1.1, 0.9] * 10), "ugrep": ok([2.0, 2.1, 1.9] * 10), "ack": ok([0.5] * 3), "ag": {"status": "wrong"}}
    m = metric_verdict(tools, "scrutate", ["scrutate", "ugrep", "ack"], "cpu", 30)
    # ack is lower but has only its pilot runs: it counts for the rank, not for the status
    assert (m["status"], m["best_competitor"], m["rank"], m["of"]) == ("PASS", "ugrep", 2, 3) and abs(m["ratio"] - 0.5) < 1e-9, m
    tools["ugrep"] = ok([0.5, 0.6, 0.4] * 10)
    assert metric_verdict(tools, "scrutate", ["scrutate", "ugrep"], "cpu", 30)["status"] == "FAIL"
    assert metric_verdict(tools, "scrutate -j1", ["scrutate", "ugrep"], "cpu", 30)["status"] == "n/a"
    assert metric_verdict({**tools, "scrutate -j1": {"status": "wrong"}}, "scrutate -j1", ["scrutate"], "cpu", 30)["status"] == "FAIL"
    assert ci_status(0.9, 1.1) == "PARTIAL" and ci_status(1.01, 1.2) == "FAIL" and ci_status(0.8, 0.99) == "PASS"


def main() -> int:
    drop_scrutate_settings()
    _selfcheck()
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpora", default=os.path.expanduser("~/.cache/scrutate-bench/corpora"))
    ap.add_argument("--out", required=True)
    ap.add_argument("--only", nargs="*")
    ap.add_argument("--runs", type=int, default=30)
    ap.add_argument("--cache", choices=["warm", "cold"], default="warm")
    ap.add_argument("--cold-prepare", default=None, help="command run before every timed run (cold cache); {list} = the benchmark's file list "
                    "(NUL-separated), e.g. 'target/evict {list}' (bench/evict.c, no root needed on macOS) or 'sync; echo 3 > /proc/sys/vm/drop_caches'")
    ap.add_argument("--max-load", type=float, default=(os.cpu_count() or 4) * 0.3, help="wait until the 1-minute load average is below this")
    ap.add_argument("--max-wait", type=float, default=1800)
    ap.add_argument("--timeout", type=float, default=300)
    ap.add_argument("--blocks", type=int, default=3, help="the full runs of the contenders are taken in this many interleaved blocks")
    ap.add_argument("--contender", type=float, default=1.25, help="besides scrutate and the fastest competitor, tools whose pilot median is within this "
                    "factor of the fastest also get the full run count")
    ap.add_argument("--short-metric-runs", action="store_true", help="a metric contender that is not a wall-time contender and whose pilot "
                    "median is over 2 s gets half the runs (shortens a full cycle; its CPU and memory statuses then rest on fewer runs)")
    ap.add_argument("--stop-early", action=argparse.BooleanOptionalAction, default=True, help="a row may stop after its second block "
                    "once its wall-time and CPU and memory verdicts are settled at alpha 0.001 (20 runs, the R-14.10 floor); "
                    "--no-stop-early takes every block; ignored with SCRUTATE_BASE_BIN")
    ap.add_argument("--force", action="store_true")
    ap.add_argument("--redo-unstable", action="store_true", help="measure again, once, the benchmarks in --out where a tool's block medians "
                    "differ by more than 25%% (the machine was disturbed); each keeps its first status, ratio, interval and load as `first`")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--restatus", action="store_true", help="no measurement: refresh each benchmark's hardware ceiling from the run's shared "
                    "measurements and recompute every status")
    a = ap.parse_args()
    # the regression gate's block ratio needs every block of both binaries
    a.stop = a.stop_early and not SCRUTATE_BASE
    if a.cache == "cold" and not a.cold_prepare:
        raise SystemExit("--cache cold needs --cold-prepare (e.g. 'sync; echo 3 > /proc/sys/vm/drop_caches')")
    benches = suite()
    if a.list:
        for b in benches:
            print(b["id"])
        return 0
    if SCRUTATE_BASE and not os.access(SCRUTATE_BASE, os.X_OK):
        raise SystemExit(f"SCRUTATE_BASE_BIN is not an executable: {SCRUTATE_BASE}")
    if not a.restatus:
        check_hyperfine()
    cache_dir = os.path.join(os.path.dirname(os.path.abspath(a.out)), "cache")
    os.makedirs(cache_dir, exist_ok=True)
    data = json.load(open(a.out)) if os.path.exists(a.out) else {"benchmarks": {}}
    RAWREAD.update(data.get("rawread", {}))
    if a.restatus:
        for r in data["benchmarks"].values():
            n = r["reference"]["files_listed"]
            raw = RAWREAD.get(f"{r['corpus']}|{r['mode']}|{r['cache']}|{n}")
            if raw and not r.get("concurrent"):
                r["rawread"] = {k: v for k, v in raw.items() if k != "times"}
            decide(r, [t for t, x in r["tools"].items() if x.get("status") == "ok"], r.get("runs", data["settings"]["runs"]), r.get("alpha", ALPHA))
            print(f"{r['id']}: {r['status']}", file=sys.stderr)
        json.dump(data, open(a.out, "w"), indent=1)
        return 0
    env, was = environment(), data.get("environment", {})
    same = all(env.get(k) == was.get(k) for k in ("scrutate_sha256", "scrutate_base_sha256"))
    if a.redo_unstable:
        # the same run, continued: its environment and settings stand, so the binaries must be the same
        if not same:
            raise SystemExit("--redo-unstable: the scrutate binary, or its base, is not the one this run measured")
    else:
        # rows kept from an earlier run would get this binary's hash over another binary's timings
        if data["benchmarks"] and not same and not (a.force and not a.only):
            raise SystemExit(f"{a.out} holds rows that another scrutate binary, or base, measured: move the file away, or measure every row again with --force")
        if not same:
            # a run that dies part way must not leave the other binary's rows under this one's hash
            data["benchmarks"] = {}
        data["environment"] = env
        data["corpora"] = json.load(open(os.path.join(a.corpora, "manifest.json")))
        data["settings"] = {"runs": a.runs, "cache": a.cache, "cold_prepare": a.cold_prepare, "max_load": a.max_load, "warmup": 1,
                            "contender": a.contender, "blocks": a.blocks, "short_metric_runs": a.short_metric_runs}
        if a.stop:
            interim, last = stop_rule(a.blocks)
            data["settings"]["stop"] = {"after_blocks": interim, "interim_alpha": INTERIM_ALPHA, "last_alpha": last}
    for b in benches:
        old = data["benchmarks"].get(b["id"])
        if a.only and not any(b["id"].startswith(p) for p in a.only):
            continue
        if a.redo_unstable:
            if not old or "first" in old or not unstable(old):
                continue
        elif old and not a.force:
            continue
        print(f"[leaderboard] {b['id']}" + (" (again: unstable)" if a.redo_unstable else ""), file=sys.stderr, flush=True)
        r = run_bench(b, a.corpora, a, cache_dir)
        if a.redo_unstable:
            if r.get("resident") is not None and r["resident"] < WARM:
                print(f"  not kept: {r['resident']:.0%} of the files were in the page cache", file=sys.stderr, flush=True)
                continue
            r["first"] = {k: old.get(k) for k in ("status", "ratio", "ci95", "max_load")}
        data["benchmarks"][b["id"]] = r
        data["rawread"] = RAWREAD
        json.dump(data, open(a.out, "w"), indent=1)
        best = r.get("best_competitor")
        print(f"  {r['status']}: scrutate {r['tools'].get('scrutate', {}).get('median', float('nan')) * 1000:.1f} ms; best competitor {best} "
              f"{(r['tools'][best]['median'] * 1000) if best else float('nan'):.1f} ms; ratio {r.get('ratio', float('nan')):.3f} {r.get('ci95', '')}",
              file=sys.stderr, flush=True)
        print("  " + "; ".join(f"{key} {mode} {m['status']} {m.get('ratio', float('nan')):.3f} vs {m.get('best_competitor')}"
                               for key, ms in r["metrics"].items() for mode, m in ms.items()), file=sys.stderr, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
