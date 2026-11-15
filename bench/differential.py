#!/usr/bin/env python3
"""Differential testing of Engine A against ripgrep: random corpora x random patterns, exact
agreement on (path, line number, line text) for every match, plus -c and -l modes; and of Engine
B's text index (`grep --index`) against Engine A on the same cases, before and after an update.

usage: differential.py [--cases N] [--seed S] [--keep]
exit status 0 when every case agrees; the first disagreement is printed with a reproduction.

Both tools are run with equivalent flags (line numbers, no colour, same case rules, same word
semantics, multiline when the pattern contains a newline escape). Known, documented semantic
differences are excluded from the generator rather than tolerated in the comparison.

Each corpus is a git repository, so both tools apply its .gitignore; a quarter of the cases add a
file type (`-t py`, `-t rust` or their negation), which both match after the ignore rules, so a
gitignored `.rs` file stays out and a hidden `.py` one comes in. Most patterns are drawn from
the corpus's own text, so most cases have matches; the share that matched nothing is printed.
Every case runs scrutate twice: as is, and in one of the modes the engine picks on its own for
some inputs (SCRUTATE_SMALL_TREE: the one-thread path for a small tree, the parallel walk, or a
hand-over between them; SCRUTATE_SPLIT_FILE_BYTES: one file searched in parallel pieces). A file
is split only when it is over 64 KiB, so about one file in five is 64 to 300 KiB, and now and
then one is a single line of that size (the path that reads a file with no line break whole).
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SCRUTATE = os.environ.get("SCRUTATE_BIN", os.path.join(HERE, "..", "target", "release", "scrutate"))
RG = os.environ.get("SCRUTATE_RG") or shutil.which("rg") or "/usr/bin/rg"
# ripgrep before 15.2.0 matched a parent directory's ignore rules against the root that first
# compiled them, so with overlapping path arguments (`sub/deep sub`) it searched an ignored
# directory twice, by argument order or by a race between threads (BurntSushi/ripgrep 43e2f08ede)
RG_MIN = (15, 2, 0)


def rg_version(text: str) -> tuple[int, ...]:
    """(15, 2, 0) from `ripgrep 15.2.0 (rev ...)`; () when the first line does not say."""
    words = text.split()
    try:
        return tuple(int(x) for x in words[1].split(".")[:3]) if words[:1] == ["ripgrep"] else ()
    except (IndexError, ValueError):
        return ()


def require_rg(rg: str = RG) -> str:
    """`rg` when it is ripgrep 15.2.0 or newer, else exit naming the fix the comparison needs."""
    try:
        out = subprocess.run([rg, "--version"], capture_output=True, text=True).stdout
    except OSError as e:
        sys.exit(f"{rg} does not run ({e.strerror}): the comparison needs ripgrep 15.2.0 or newer; set SCRUTATE_RG")
    if rg_version(out) < RG_MIN:
        sys.exit(f"{rg} is {out.splitlines()[0] if out else 'not ripgrep'}: the comparison needs ripgrep 15.2.0 or newer "
                 "(43e2f08ede fixed overlapping path arguments); set SCRUTATE_RG")
    return rg

WORDS = ["alpha", "beta", "gamma", "delta", "token", "refresh", "Token", "REFRESH_TOKEN", "refreshToken", "foo_bar", "x", "y1", "self", "def",
         "class", "import", "return", "value", "été", "日本語", "naïve", "Ωmega", "tab\there", "a.b", "a*b", "(x)", "[y]", "end$", "^start", "back\\slash"]


def random_corpus(rng: random.Random, root: str) -> list[str]:
    """Write a corpus and return its non-empty lines."""
    written = []
    nfiles = rng.randint(1, 8)
    for i in range(nfiles):
        sub = rng.choice(["", "src/", "src/deep/dir/", "docs/"])
        name = rng.choice([f"f{i}.py", f"f{i}.txt", f"f{i}.rs", f"F{i}.MD", f"noext{i}", f"sp ace{i}.txt"])
        p = os.path.join(root, sub, name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        lines = []
        shape = rng.random()
        if shape < 0.04:
            # one line past the size where a file is split
            lines.append(" ".join(rng.choice(WORDS) for _ in range(rng.randint(14_000, 50_000))))
        else:
            # past that size for about one file in five
            size, target = 0, rng.randint(64 << 10, 300 << 10) if shape < 0.24 else 0
            count = rng.randint(0, 60)
            while len(lines) < count or size < target:
                k = rng.randint(0, 8)
                line = " ".join(rng.choice(WORDS) for _ in range(k))
                if rng.random() < 0.05:
                    line = "x" * rng.randint(200, 5000)  # a long line
                lines.append(line)
                size += len(line) + 1
        # a pattern is drawn from these: a sample of a large file is enough
        written += [x for x in lines[:200] if x.strip()]
        sep = rng.choice(["\n", "\n", "\n", "\r\n"])
        text = sep.join(lines)
        if rng.random() < 0.7:
            text += sep
        data = text.encode("utf8")
        if rng.random() < 0.05:
            data = b"\xef\xbb\xbf" + data  # BOM
        elif rng.random() < 0.05 and data:
            # a Latin-1 byte: plain output prints it as it is, JSON as U+FFFD
            at = rng.randrange(len(data))
            data = data[:at] + b"\xe9" + data[at:]
        with open(p, "wb") as f:
            f.write(data)
    if rng.random() < 0.3:
        with open(os.path.join(root, ".gitignore"), "w") as f:
            f.write("docs/\n*.rs\n")
    if rng.random() < 0.3:
        os.makedirs(os.path.join(root, ".hidden"), exist_ok=True)
        # a hidden directory stays out whatever its files' type; a hidden file of the type does not
        for name in (os.path.join(".hidden", "h.txt"), ".h.py"):
            with open(os.path.join(root, name), "w") as f:
                f.write("alpha beta\n")
    subprocess.run(["git", "init", "-q", root], check=True)
    return written


META = set(r"\.+*?()|[]{}^$")


def escape(s: str) -> str:
    return "".join("\\" + c if c in META else c for c in s)


def fragment(rng: random.Random, line: str) -> str:
    start = rng.randrange(len(line))
    return line[start:start + rng.randint(1, 16)]


def random_pattern(rng: random.Random, lines: list[str]) -> tuple[str, list[str], list[str], str]:
    """-> (pattern, scrutate flags, rg flags, mode)"""
    mode = rng.choice(["literal", "literal", "regex", "regex", "icase", "word", "multiline", "pcre2"])
    present = [x for x in WORDS if any(x in line for line in lines)]
    # a quarter from the whole list, so patterns with no match stay covered
    w = rng.choice(present) if present and rng.random() < 0.75 else rng.choice(WORDS)
    if mode == "literal":
        if lines and rng.random() < 0.5:
            return fragment(rng, rng.choice(lines)), ["-F"], ["-F"], mode
        pat = rng.choice([w, w[: max(1, len(w) // 2)], w + " " + rng.choice(WORDS)])
        return pat, ["-F"], ["-F"], mode
    if mode == "icase":
        pat = rng.choice([w.upper(), w.lower(), w.capitalize()])
        return pat, ["-F", "-i"], ["-F", "-i"], mode
    if mode == "word":
        return w, ["-F", "-w"], ["-F", "-w"], mode
    if mode == "pcre2":
        w2 = rng.choice(WORDS)
        pat = rng.choice([rf"(?<={w[:2]}){w[2:] or w}", rf"{w}(?=\s+{w2})", rf"(?<!x)\b{w}\b", r"(\w)\1", rf"(?i){w}(?!{w2})", rf"({w}).*\1"])
        return pat, ["-P"], ["-P"], mode
    if mode == "multiline":
        if len(lines) > 1 and rng.random() < 0.6:
            # the end of one line and the start of the next (the same file, unless it was the last)
            i = rng.randrange(len(lines) - 1)
            return escape(lines[i][-rng.randint(1, 8):]) + r"\r?\n\s*" + escape(lines[i + 1][:rng.randint(1, 8)]), ["-U"], ["-U"], mode
        return rng.choice([r"alpha\n\s*beta", r"def\s*\n", r"(?s)gamma.{0,20}delta"]), ["-U"], ["-U"], mode
    atoms = [r"\w+", r"\balpha\b", "(alpha|beta)", "ga+mma", r"\d", "[a-c]+", "tok(en)?", r"^\s*def", r"value$", "é", ".", r"x{3,}", r"refresh[_-]?token", "(?i)TOKEN"]
    # patterns anchored at a line start with a literal prefix take their own path in the engine
    anchored = [r"^alpha", r"^(alpha|beta)\b", r"^[a-c]", r"^(?i)value", r"^(a|aa)+$", r"^été"]
    if lines and rng.random() < 0.5:
        pat = escape(fragment(rng, rng.choice(lines))) + rng.choice(["", r"\w*", r"\s*\S+", ".*", r"\b", "(?:" + rng.choice(atoms) + ")?"])
        return pat, [], [], "regex"
    pat = "".join(rng.choice(atoms) for _ in range(rng.randint(1, 3)))
    if rng.random() < 0.25:
        pat = rng.choice(anchored) + pat
    return pat, [], [], "regex"


def run_json(cmd: list[str], cwd: str, env: dict | None = None) -> tuple[int, bytes]:
    r = subprocess.run(cmd, cwd=cwd, env={**os.environ, **(env or {})}, capture_output=True, stdin=subprocess.DEVNULL, timeout=120)
    return r.returncode, r.stdout


def random_mode(rng: random.Random) -> dict:
    """One of the engine's internal search paths, forced through its environment variable."""
    return rng.choice([
        {"SCRUTATE_SMALL_TREE": "100000"},
        {"SCRUTATE_SMALL_TREE": "0"},
        {"SCRUTATE_SMALL_TREE": str(rng.randint(1, 4))},
        {"SCRUTATE_SPLIT_FILE_BYTES": str(rng.choice([1, 16, 100, 1000, 4096]))},
    ])


def scrutate_matches(out: bytes) -> set:
    s = set()
    for line in out.splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get("type") == "match":
            s.add((r["path"], r["line"], r["text"]))
    return s


def rg_matches(out: bytes) -> set:
    s = set()
    for line in out.splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get("type") == "match":
            d = r["data"]
            text = d["lines"].get("text")
            if text is None:
                # rg gives a line that is not UTF-8 as base64
                text = base64.b64decode(d["lines"]["bytes"]).decode("utf8", "replace") if "bytes" in d["lines"] else ""
            text = text.rstrip("\n").rstrip("\r")
            path = d["path"].get("text", "")
            path = path[2:] if path.startswith("./") else path
            s.add((path, d["line_number"], text))
    return s


def indexed_agrees(rng: random.Random, root: str, pat: str, sf: list[str], mode: str) -> str | None:
    """Engine B's text index must give Engine A's answer, before and after an incremental update."""
    for stage in ("fresh", "updated"):
        if stage == "updated":
            files = [os.path.join(d, f) for d, _, fs in os.walk(root) if not {".scrutate", ".git"} & set(os.path.relpath(d, root).split(os.sep)) for f in fs]
            if not files:
                return None
            victim = rng.choice(sorted(files))
            with open(victim, "a", encoding="utf8") as f:
                f.write(" ".join(rng.choice(WORDS) for _ in range(6)) + "\n")
        if subprocess.run([SCRUTATE, "index", "--text"], cwd=root, capture_output=True).returncode != 0:
            return f"index --text failed (corpus {root})"
        rc1, o1 = run_json([SCRUTATE, "grep", "--sort", "--json", *sf, pat], root)
        rc2, o2 = run_json([SCRUTATE, "grep", "--index", "--sort", "--json", *sf, pat], root)
        walk, index = scrutate_matches(o1), scrutate_matches(o2)
        if "-t" in sf:
            # the index holds no hidden file, and the walk takes a hidden file of a type
            walk = {m for m in walk if not m[0].startswith(".")}
            rc1 = rc1 if walk or rc1 != 0 else 1
        if rc1 != rc2 or walk != index:
            only_walk = sorted(walk - index)[:3]
            only_index = sorted(index - walk)[:3]
            return f"--index differs ({stage}) for {pat!r} ({mode}, flags {sf}): exit {rc1}/{rc2}; only walk {only_walk}; only index {only_index} (corpus {root})"
    return None


def shown(env: dict) -> str:
    return " ".join(f"{k}={v}" for k, v in env.items()) or "default mode"


def one_case(rng: random.Random, keep: bool, stats: dict) -> str | None:
    root = tempfile.mkdtemp(prefix="scrutate_diff_")
    try:
        lines = random_corpus(rng, root)
        pat, sf, rf, mode = random_pattern(rng, lines)
        if rng.random() < 0.25:
            # a type matches after the ignore rules: an ignored `.rs` stays out, a hidden `.py` comes in
            name, negated = rng.choice(["py", "rust"]), rng.random() < 0.3
            sf, rf = sf + ["-t", "!" + name if negated else name], rf + ["-T" if negated else "-t", name]
        envs = [{}, random_mode(rng)]
        if mode == "multiline":
            # rg's -U with --json reports multi-line matches as one record spanning lines; compare counts of files instead
            rc2, o2 = run_json([RG, "--no-messages", "-l", *rf, pat, "."], root)
            b = sorted(l.decode()[2:] if l.startswith(b"./") else l.decode() for l in o2.splitlines())
            stats["empty"] += not b
            for env in envs:
                rc1, o1 = run_json([SCRUTATE, "grep", "-l", *sf, pat], root, env)
                if rc1 not in (0, 1) or rc2 not in (0, 1):
                    if rc1 == 2 and rc2 == 2:
                        return None  # both reject the pattern
                    return f"exit codes differ for {pat!r} ({mode}, {shown(env)}): scrutate={rc1} rg={rc2} (corpus {root})"
                a = sorted(l.decode() for l in o1.splitlines())
                if a != b:
                    return f"multiline -l mismatch for {pat!r} ({shown(env)}): scrutate={a} rg={b} (corpus {root})"
            return indexed_agrees(rng, root, pat, sf, mode)
        rc2, o2 = run_json([RG, "--json", "--no-messages", *rf, pat, "."], root)
        b = rg_matches(o2)
        stats["empty"] += not b
        rc2c, o2c = run_json([RG, "--no-messages", "-c", *rf, pat, "."], root)
        cb = {}
        for l in o2c.splitlines():
            if b":" in l:
                p, n = l.rsplit(b":", 1)
                p = p.decode()
                cb[p[2:] if p.startswith("./") else p] = int(n)
        for env in envs:
            rc1, o1 = run_json([SCRUTATE, "grep", "--json", *sf, pat], root, env)
            if rc1 not in (0, 1) or rc2 not in (0, 1):
                if rc1 == 2 and rc2 == 2:
                    return None  # both reject the pattern
                return f"exit codes differ for {pat!r} ({mode}, {shown(env)}): scrutate={rc1} rg={rc2} (corpus {root})"
            a = scrutate_matches(o1)
            if a != b:
                only_a = sorted(a - b)[:3]
                only_b = sorted(b - a)[:3]
                return f"match sets differ for {pat!r} ({mode}, flags {sf}, {shown(env)}): only scrutate {only_a}; only rg {only_b} (corpus {root})"
            # plain output, byte for byte: the text as the file holds it
            _, p1 = run_json([SCRUTATE, "grep", "-n", "-H", *sf, pat], root, env)
            _, p2 = run_json([RG, "--no-messages", "--no-heading", "-n", "-H", *rf, pat, "."], root)
            pa = sorted(p1.split(b"\n"))
            pb = sorted(l[2:] if l.startswith(b"./") else l for l in p2.split(b"\n"))
            if pa != pb:
                diff = sorted(set(pa) ^ set(pb))[:3]
                return f"plain output differs for {pat!r} ({mode}, flags {sf}, {shown(env)}): {diff} (corpus {root})"
            rc1, o1 = run_json([SCRUTATE, "grep", "-c", *sf, pat], root, env)
            ca = {l.rsplit(b":", 1)[0].decode(): int(l.rsplit(b":", 1)[1]) for l in o1.splitlines() if b":" in l}
            if ca != cb:
                return f"-c differs for {pat!r} ({mode}, {shown(env)}): scrutate={ca} rg={cb} (corpus {root})"
        return indexed_agrees(rng, root, pat, sf, mode)
    finally:
        if not keep:
            shutil.rmtree(root, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", type=int, default=200)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--keep", action="store_true", help="keep the corpus of a failing case")
    a = ap.parse_args()
    require_rg()
    if not os.path.exists(SCRUTATE) or not os.path.exists(RG):
        print(f"need {SCRUTATE} and ripgrep", file=sys.stderr)
        return 2
    if not shutil.which("git"):
        print("need git: every corpus is a git repository", file=sys.stderr)
        return 2
    rng = random.Random(a.seed)
    failures = 0
    stats = {"empty": 0}
    ran = 0
    for i in range(a.cases):
        ran += 1
        problem = one_case(rng, a.keep, stats)
        if problem:
            failures += 1
            print(f"case {i}: {problem}")
            if failures >= 5:
                break
    print(f"{ran} cases, {failures} disagreements (seed {a.seed}); ripgrep found nothing in {stats['empty']} ({100 * stats['empty'] / max(ran, 1):.0f}%)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
