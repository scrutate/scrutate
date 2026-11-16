#!/usr/bin/env python3
"""Differential testing of Engine A against ripgrep: random corpora x random patterns, exact
agreement on (path, line number, line text) for every match, plus -c and -l modes; and of Engine
B's text index (`grep --index`) against Engine A on the same cases, before and after an update.

usage: differential.py [--cases N] [--seed S] [--keep]
exit status 0 when every case agrees; the first disagreement is printed with a reproduction.

Both tools are run with equivalent flags (line numbers, no colour, same case rules, same word
semantics, multiline when the pattern contains a newline escape) and no path, so both search the
current directory: a case whose filters leave no file is exit 2 with a note for both, where a `.`
given to one of them alone would make it exit 1. Two exit codes of 2 agree only when both tools
give the same reason (no file searched, or a bad pattern); a panic beside the other's note is a
disagreement. Known, documented semantic
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
        elif rng.random() < 0.06:
            # a NUL byte, as a binary file has it: in the reader's 3-byte first read, just past it,
            # or anywhere (a file past 64 KB holds it past the first 64 KB about as often)
            at = rng.choice([0, 1, 2, 3, 4, rng.randrange(len(data) + 1)])
            data = data[:at] + b"\x00" + data[at:]
            # a line that ends in the reader's 3-byte first read is searched before the NUL
            if rng.random() < 0.5:
                data = rng.choice([b"\n", b"\n\n", b"a\n", b"zz\n", b"\r\n"]) + data
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


def run_err(cmd: list[str], cwd: str, env: dict | None = None) -> tuple[int, bytes, bytes]:
    r = subprocess.run(cmd, cwd=cwd, env={**os.environ, **(env or {})}, capture_output=True, stdin=subprocess.DEVNULL, timeout=120)
    return r.returncode, r.stdout, r.stderr


def run_json(cmd: list[str], cwd: str, env: dict | None = None) -> tuple[int, bytes]:
    rc, out, _ = run_err(cmd, cwd, env)
    return rc, out


def reason(err: bytes) -> str | None:
    """Why a tool exited 2, when it is a reason both tools give: no file searched, or a bad pattern."""
    e = err.lower()
    if b"no files were searched" in e:
        return "no file searched"
    if any(k in e for k in (b"regex parse error", b"error compiling pattern", b"invalid pattern")):
        return "bad pattern"
    return None


def exits_differ(pat: str, mode: str, env: dict, root: str, rc1: int, e1: bytes, rc2: int, e2: bytes) -> str | None:
    """None when both exit codes say the same: 0 or 1 each, or 2 for the same reason; else why not."""
    if rc1 in (0, 1) and rc2 in (0, 1):
        return None
    if rc1 == 2 and rc2 == 2 and reason(e1) is not None and reason(e1) == reason(e2):
        return None
    first = lambda e: e.decode("utf-8", "replace").strip().splitlines()[:1] or [""]  # noqa: E731
    return f"exit codes differ for {pat!r} ({mode}, {shown(env)}): scrutate={rc1} ({first(e1)[0]!r}) rg={rc2} ({first(e2)[0]!r}) (corpus {root})"


def late_nul(root: str) -> list[str]:
    """The files whose first NUL byte is past the reader's first fill: there ripgrep's output stops
    at a read whose start depends on how far earlier long lines on its thread grew its buffer."""
    out = []
    for d, dirs, fs in os.walk(root):
        dirs[:] = [x for x in dirs if x not in (".git", ".scrutate")]
        for f in fs:
            p = os.path.join(d, f)
            if not os.path.isfile(p) or os.path.islink(p):
                continue
            try:
                with open(p, "rb") as fh:
                    data = fh.read()
            except OSError:
                # not late: rg reports the read error itself, in the run without the late files
                continue
            if data.find(b"\0") >= (64 << 10) - 8:
                out.append(os.path.relpath(p, root).replace(os.sep, "/"))
    return out


def searched_none(cmd: list[str], cwd: str, err: bytes) -> bool:
    """Whether rg's run of `cmd` in `cwd`, which exited 2 writing `err`, searched no file. rg says so
    only without --no-messages, so a run with it is asked again; a read error exits 2 as well, and
    --json prints a summary either way."""
    if b"No files were searched" in err:
        return True
    if "--no-messages" not in cmd:
        return False
    r = subprocess.run([x for x in cmd if x != "--no-messages"], cwd=cwd, capture_output=True, stdin=subprocess.DEVNULL, timeout=120)
    return r.returncode == 2 and b"No files were searched" in r.stderr


def run_rg(cmd: list[str], root: str, late: list[str], by_path: bool = False, errors: list | None = None) -> tuple[int, bytes]:
    """`cmd` (rg, its flags, then PATTERN), with each file of `late` answered as rg answers
    it searched alone, with a fresh buffer: scrutate's rule, which no walk order can change. The
    rest is rg's run without those files. `by_path`: put the output back in path order (`--sort`).
    `errors`, when given, collects what each run whose exit code counts wrote to stderr (all of
    them when none searched a file), so a reason read from it is the combined exit code's."""
    if not late:
        rc, out, err = run_err(cmd, root)
        if errors is not None:
            errors.append(err)
        return rc, out
    # one rg over the tree exits as its searched parts do: 2 on an error even beside a match (rg
    # without -q), else 0 on a match, else 1; a part that searched no file (all late) adds nothing
    runs: list[tuple[int, bytes, bool]] = []
    rest = [*cmd[:-1], *(x for rel in late for x in ("-g", "!/" + rel)), cmd[-1]]
    rc, out, err = run_err(rest, root)
    runs.append((rc, err, rc == 2 and searched_none(rest, root, err)))
    parts = [out]
    for rel in late:
        with tempfile.TemporaryDirectory() as t:
            # the ignore files decide whether the file is searched at all
            for d, dirs, fs in os.walk(root):
                dirs[:] = [x for x in dirs if x not in (".git", ".scrutate")]
                for f in fs:
                    src = os.path.join(d, f)
                    if f in (".gitignore", ".ignore", ".rgignore") or os.path.relpath(src, root) == rel:
                        dst = os.path.join(t, os.path.relpath(src, root))
                        os.makedirs(os.path.dirname(dst), exist_ok=True)
                        shutil.copy2(src, dst)
            if os.path.isdir(os.path.join(root, ".git")):
                os.makedirs(os.path.join(t, ".git"), exist_ok=True)
            rc1, o1, e1 = run_err(cmd, t)
            runs.append((rc1, e1, rc1 == 2 and searched_none(cmd, t, e1)))
        parts.append(o1)
    counted = [(c, e) for c, e, none in runs if not none]
    codes = [c for c, _ in counted]
    rc = 2 if 2 in codes or not codes else 0 if 0 in codes else 1
    if errors is not None:
        errors.extend([e for _, e in counted] if counted else [e for _, e, _ in runs])
    if not by_path:
        return rc, b"".join(parts)
    groups: dict[bytes, list[bytes]] = {}
    for line in b"".join(parts).split(b"\n"):
        if line:
            groups.setdefault(line.split(b":", 1)[0], []).append(line)
    order = sorted(groups, key=lambda p: p.split(b"/"))
    return rc, b"".join(l + b"\n" for p in order for l in groups[p])


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
        late = late_nul(root)
        if mode == "multiline":
            # rg's -U with --json reports multi-line matches as one record spanning lines; compare counts of files instead
            e2: list[bytes] = []
            rc2, o2 = run_rg([RG, "-l", *rf, pat], root, late, errors=e2)
            b = sorted(l.decode()[2:] if l.startswith(b"./") else l.decode() for l in o2.splitlines())
            stats["empty"] += not b
            for env in envs:
                rc1, o1, e1 = run_err([SCRUTATE, "grep", "-l", *sf, pat], root, env)
                if why := exits_differ(pat, mode, env, root, rc1, e1, rc2, b"".join(e2)):
                    return why
                if rc1 == 2:
                    return None  # both reject the pattern, or both searched no file
                a = sorted(l.decode() for l in o1.splitlines())
                if a != b:
                    return f"multiline -l mismatch for {pat!r} ({shown(env)}): scrutate={a} rg={b} (corpus {root})"
            return indexed_agrees(rng, root, pat, sf, mode)
        e2: list[bytes] = []
        rc2, o2 = run_rg([RG, "--json", *rf, pat], root, late, errors=e2)
        b = rg_matches(o2)
        stats["empty"] += not b
        rc2c, o2c = run_rg([RG, "--no-messages", "-c", *rf, pat], root, late)
        cb = {}
        for l in o2c.splitlines():
            if b":" in l:
                p, n = l.rsplit(b":", 1)
                p = p.decode()
                cb[p[2:] if p.startswith("./") else p] = int(n)
        for env in envs:
            rc1, o1, e1 = run_err([SCRUTATE, "grep", "--json", *sf, pat], root, env)
            if why := exits_differ(pat, mode, env, root, rc1, e1, rc2, b"".join(e2)):
                return why
            if rc1 == 2:
                return None  # both reject the pattern, or both searched no file
            a = scrutate_matches(o1)
            if a != b:
                only_a = sorted(a - b)[:3]
                only_b = sorted(b - a)[:3]
                return f"match sets differ for {pat!r} ({mode}, flags {sf}, {shown(env)}): only scrutate {only_a}; only rg {only_b} (corpus {root})"
            # plain output, byte for byte: the text as the file holds it
            _, p1 = run_json([SCRUTATE, "grep", "-n", "-H", *sf, pat], root, env)
            _, p2 = run_rg([RG, "--no-messages", "--no-heading", "-n", "-H", *rf, pat], root, late)
            pa = sorted(p1.split(b"\n"))
            pb = sorted(l[2:] if l.startswith(b"./") else l for l in p2.split(b"\n"))
            if pa != pb:
                diff = sorted(set(pa) ^ set(pb))[:3]
                return f"plain output differs for {pat!r} ({mode}, flags {sf}, {shown(env)}): {diff} (corpus {root})"
            # sorted output is printed from records, a path of its own: in its order, byte for byte
            _, s1 = run_json([SCRUTATE, "grep", "--sort", "-n", "-H", *sf, pat], root, env)
            _, s2 = run_rg([RG, "--no-messages", "--no-heading", "--sort", "path", "-n", "-H", *rf, pat], root, late, by_path=True)
            sb = [l[2:] if l.startswith(b"./") else l for l in s2.split(b"\n")]
            if s1.split(b"\n") != sb:
                diff = [x for x in zip(s1.split(b"\n"), sb) if x[0] != x[1]][:2]
                return f"sorted output differs for {pat!r} ({mode}, flags {sf}, {shown(env)}): {diff} (corpus {root})"
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
