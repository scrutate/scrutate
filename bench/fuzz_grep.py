#!/usr/bin/env python3
"""Random differential test: `scrutate grep` against ripgrep on small random trees.

usage: fuzz_grep.py [--bin SCRUTATE] [--seconds 60] [--seed 1] [--ordered]

Each tree has a `.git` directory, CRLF and BOM files, NUL bytes (some past the first 64 KB), invalid UTF-8, hidden files,
`.gitignore`, `.ignore` and `.rgignore` files and a symlink. Six random flag sets (now and then
with `-t rust` or `-T rust`, which match after the ignore rules) run on it with
both tools, each on `.`, one file, one directory, a file and a directory, or paths that repeat or
overlap (a file twice, a directory twice, a directory and one inside it, `.` and a directory or
`plain.txt`), with each tool's default for printing file names unless -H or -I is drawn; sorted
output and exit codes must agree. The file given by name is `plain.txt`, text no ignore rule
names, and no glob is drawn with named paths: an explicitly named binary file and globs under a
named directory are ripgrep differences of their own. A tree that differs is kept, with its arguments in
ARGS.txt, under the temporary directory printed at the end. One difference is ripgrep's and is
counted apart: with `-v` or `^$` it prints one line before a NUL byte and stops, so differences
in the lines of the files that got a NUL byte are excused. Runs whose arguments ripgrep rejects
(exit 2 with a message) are not compared; on any other exit 2 the output is. Exit status 1 when
anything else differs, or when no compared run had any output. With --ordered, a run draws
`--sort path` (scrutate's `--sort`) for both tools four times in ten, and then their lines are
compared in the order printed. Every run first replays the saved cases of bench/fuzz_grep_cases.json
(trees and arguments that once differed), so a short run still covers them. ripgrep is `SCRUTATE_RG` or `rg` on PATH, and must be 15.2.0 or
newer: an older one searches an ignored directory twice when path arguments overlap.
"""
import argparse, base64, json, os, random, shutil, subprocess, sys, tempfile, time
from collections import Counter

ap = argparse.ArgumentParser()
ap.add_argument("--bin", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "release", "scrutate"))
ap.add_argument("--seconds", type=float, default=60)
ap.add_argument("--seed", type=int, default=1)
ap.add_argument("--ordered", action="store_true", help="accepted as before: every case now also runs with --sort")
a_ = ap.parse_args()
BIN, SECONDS, SEED, ORDERED = os.path.abspath(a_.bin), a_.seconds, a_.seed, a_.ordered
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from differential import require_rg  # noqa: E402

RG = require_rg()
HERE = tempfile.mkdtemp(prefix="scrutate-fuzz-")
WORK = os.path.join(HERE, f"work{SEED}")
KEEP = os.path.join(HERE, f"diffs{SEED}")
rnd = random.Random(SEED)
WORDS = ["foo", "Foo", "FOO", "bar", "baz", "foobar", "foo_bar", "x", "été", "ÉTÉ", "token", "Token", "a.b", "(x)", "end", ""]
NAMES = ["a.txt", "b.txt", "c.log", "d.rs", "e.tmp", "keep.log", ".h.txt", "A.TXT", "long name.txt"]
DIRS = ["", "sub", "sub/deep", "dir1", "dir2", ".hid", "tmp"]
IGN = ["*.log", "sub/", "/a.txt", "!keep.log", "**/tmp", "*.tmp", "dir*/", "b?.txt", "[ab].txt", "deep", "/sub/deep/", "*.TXT"]

def line():
    return " ".join(rnd.choice(WORDS) for _ in range(rnd.randint(0, 5)))

STATE = {"nul": set()}


def make():
    STATE["nul"] = set()
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(os.path.join(WORK, ".git"))
    for d in rnd.sample(DIRS, rnd.randint(1, 5)):
        os.makedirs(os.path.join(WORK, d), exist_ok=True)
        for n in rnd.sample(NAMES, rnd.randint(1, 4)):
            nl = rnd.choice(["\n", "\n", "\n", "\r\n"])
            body = nl.join(line() for _ in range(rnd.randint(0, 12)))
            if rnd.random() < 0.7:
                body += nl
            data = body.encode()
            r = rnd.random()
            if r < 0.04:
                # half of them past the first 64 KB read, where ripgrep prints the lines before it
                pad = (("zz" * 40 + nl) * rnd.choice([0, 1000])).encode()
                data = data[: len(data) // 2] + pad + b"\x00bin" + data[len(data) // 2 :]
                STATE["nul"].add(os.path.join(d, n))
            elif r < 0.08:
                data = b"\xff\xfe bad " + data
            elif r < 0.11 and data:
                # a Latin-1 byte inside a line (\xff\xfe above is UTF-16's byte-order mark, decoded):
                # plain output prints it as it is
                at = rnd.randrange(len(data))
                data = data[:at] + b"\xe9" + data[at:]
            elif r < 0.14:
                data = b"\xef\xbb\xbf" + data
            elif r < 0.20:
                # a NUL where the reader's first reads end: in its 3-byte first read, just past it, in
                # the first 64 KB or past them, as a binary file has it
                pad = (("zz" * 40 + nl) * rnd.choice([0, 0, 1000])).encode()
                data += pad
                at = rnd.choice([0, 1, 2, 3, 4, rnd.randrange(len(data) + 1)])
                data = data[:at] + b"\x00" + data[at:]
                STATE["nul"].add(os.path.join(d, n))
            with open(os.path.join(WORK, d, n), "wb") as f:
                f.write(data)
        if rnd.random() < 0.3:
            with open(os.path.join(WORK, d, rnd.choice([".gitignore", ".ignore", ".rgignore"])), "w") as f:
                f.write("\n".join(rnd.sample(IGN, rnd.randint(1, 3))) + "\n")
    with open(os.path.join(WORK, "plain.txt"), "w") as f:
        f.write("\n".join(line() for _ in range(rnd.randint(0, 8))) + "\n")
    if rnd.random() < 0.2 and os.path.isfile(os.path.join(WORK, "a.txt")):
        try:
            os.symlink("a.txt", os.path.join(WORK, "link.txt"))
        except OSError:
            pass
    # by what is on disk: where case does not tell names apart, A.TXT and a.txt are one file
    STATE["nul"] = set()
    for d, _, names in os.walk(WORK):
        for n in names:
            p = os.path.join(d, n)
            if os.path.isfile(p) and b"\0" in open(p, "rb").read():
                STATE["nul"].add(os.path.relpath(p, WORK))

def pattern():
    w = [x for x in WORDS if x and x.isalnum()]
    a, b = rnd.choice(w), rnd.choice(w)
    return rnd.choice([a, a, f"{a}|{b}", f"{a}\\w+", f"^{a}", f"{b}$", "[a-f]+", f"\\b{a}\\b", f"{a}.*{b}", f"(?i){a}", f"{a} {b}", "^$", f"\\s{a}", f"{a}?{b}"])

def args():
    out = []
    lit = rnd.random() < 0.2
    if lit:
        out.append("-F")
    out += rnd.choice([["-i"], ["-S"], ["-i", "-S"], ["-S", "-i"], [], [], [], []])
    for flag, p in [ ("-w", 0.2), ("-v", 0.12), ("--hidden", 0.25), ("--no-ignore", 0.2), ("-L", 0.15)]:
        if rnd.random() < p:
            out.append(flag)
    shape = rnd.random()
    if shape < 0.15:
        out.append("-c")
    elif shape < 0.3:
        out.append("-l")
    elif shape < 0.5:
        out += [rnd.choice(["-A", "-B", "-C"]) + str(rnd.randint(1, 3))]
    elif shape < 0.6:
        out += ["-m", str(rnd.randint(1, 3))]
    if rnd.random() < 0.15:
        out += ["--max-depth", str(rnd.randint(1, 3))]
    if rnd.random() < 0.15:
        out += ["-g", rnd.choice(["*.txt", "!*.log", "sub/**", "!sub/**", "*.rs", "!dir1/", "*.{txt,log}"])]
    if rnd.random() < 0.12:
        out += [rnd.choice(["-t", "-t", "-T"]), "rust"]
    if rnd.random() < 0.08:
        out += ["--max-filesize", str(rnd.choice([10, 50, 200]))]
    if rnd.random() < 0.15:
        out.append(rnd.choice(["-H", "-I", "--with-filename", "--no-filename"]))
    if rnd.random() < 0.1 and not lit:
        w = [x for x in WORDS if x and x.isalnum()]
        out += ["-U", f"{rnd.choice(w)}\\n{rnd.choice(w)}"]
    elif rnd.random() < 0.12:
        w = [x for x in WORDS if x and x.isalnum()]
        out += ["-e", rnd.choice(w), "-e", rnd.choice(w)]
    else:
        p = pattern()
        out.append(rnd.choice([x for x in WORDS if x]) if lit else p)
    return out

def paths(a):
    """`.` mostly; otherwise `plain.txt`, one directory, both, or repeated and overlapping paths
    (with `a`'s globs dropped)."""
    dirs = sorted(os.path.relpath(d, WORK) for d, _, _ in os.walk(WORK) if ".git" not in d.split(os.sep))[1:]
    r = rnd.random()
    if r < 0.6:
        return a + ["."]
    while "-g" in a:
        i = a.index("-g")
        del a[i : i + 2]
    if r < 0.7 and STATE["nul"]:
        # a file with a NUL byte named on the command line: searched as ripgrep searches a named file
        return a + [rnd.choice(sorted(STATE["nul"]))]
    if r < 0.8 or not dirs:
        return a + ["plain.txt"]
    if r < 0.85:
        return a + [rnd.choice(dirs)]
    if r < 0.9:
        return a + ["plain.txt", rnd.choice(dirs)]
    d = rnd.choice(dirs)
    inner = [x for x in dirs if x.startswith(d + os.sep)]
    return a + rnd.choice([["plain.txt", "plain.txt"], [d, d], [".", d], [d, "."], ["plain.txt", "."], [d] + inner[:1], inner[:1] + [d]])


def ours(a):
    """The arguments as scrutate spells them: ripgrep's `-T NAME` is `-t !NAME`."""
    out = []
    for x in a:
        if out[-1:] == ["-T"]:
            out[-1:] = ["-t", "!" + x]
        else:
            out.append(x)
    return out


def run(tool, a):
    cmd = [RG, "-n", "--no-heading", "--no-messages"] + a if tool == "rg" else [BIN, "grep"] + ours(a)
    if tool == "rg" and "--sort" in a:
        cmd.insert(cmd.index("--sort") + 1, "path")
    try:
        p = subprocess.run(cmd, cwd=WORK, capture_output=True, timeout=20)
    except subprocess.TimeoutExpired:
        return ["<timeout>"], -9, b""
    # lossless: rg's `\xff` and a U+FFFD in its place must differ (plain output prints the bytes)
    out = p.stdout.decode("utf-8", "surrogateescape").split("\n")
    out = out[:-1] if out and out[-1] == "" else out
    lines = [l[2:] if l.startswith("./") else l for l in out]
    return (lines if "--sort" in a else sorted(lines)), p.returncode, p.stderr

def in_nul_file(l):
    return any(l == f or l.startswith((f + ":", f + "-")) for f in STATE["nul"])

def only(x, y):
    left = Counter(x)
    left.subtract(Counter(y))
    return [l for l, c in left.items() if c > 0]

t0, n, compared, nonempty, diffs, known, kinds = time.time(), 0, 0, 0, 0, 0, {}
CASES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fuzz_grep_cases.json")


def load(case):
    """A saved case's tree as WORK (its files, empty directories and the NUL files it holds)."""
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK)
    for d in case["dirs"]:
        os.makedirs(os.path.join(WORK, d), exist_ok=True)
    STATE["nul"] = set()
    for rel, b64 in case["files"].items():
        data = base64.b64decode(b64)
        os.makedirs(os.path.dirname(os.path.join(WORK, rel)) or WORK, exist_ok=True)
        with open(os.path.join(WORK, rel), "wb") as f:
            f.write(data)
        if b"\0" in data:
            STATE["nul"].add(rel)


def compare(a, label=None):
    """Run both tools on `a` in WORK and count the outcome; a difference is kept and printed."""
    global n, compared, nonempty, diffs, known
    n += 1
    r, rc, rerr = run("rg", a)
    if rc == 2 and rerr.strip():
        return  # rg rejects the arguments
    s, sc, _ = run("sc", a)
    compared += 1
    nonempty += bool(r)
    # ripgrep's other exit 2 is an error it met while searching: only the output is compared
    if r == s and (rc == sc or rc == 2):
        return
    changed = only(r, s) + only(s, r)
    if ("-v" in a or "^$" in a) and changed and any(x in a for x in ("-I", "--no-filename")):
        # lines without their file's path: ask both again with it, to see which files differ
        named = ["-H" if x in ("-I", "--no-filename") else x for x in a]
        changed = only(run("rg", named)[0], run("sc", named)[0]) + only(run("sc", named)[0], run("rg", named)[0])
    if ("-v" in a or "^$" in a) and changed and all(in_nul_file(l) for l in changed):
        known += 1
        return
    diffs += 1
    key = " ".join(x for x in a if x.startswith("-") and not x[1:].isdigit())
    kinds[key] = kinds.get(key, 0) + 1
    if kinds[key] <= 2 and diffs <= 400:
        dst = os.path.join(KEEP, f"case{diffs}")
        shutil.copytree(WORK, dst, symlinks=True)
        with open(os.path.join(dst, "ARGS.txt"), "w") as f:
            f.write(repr(a) + f"\nrg exit {rc}, scrutate exit {sc}\n")
        print(f"DIFF case{diffs}{f' (saved: {label})' if label else ''} {a!r}: rg {len(r)} lines exit {rc}; scrutate {len(s)} lines exit {sc}")
        for l in [x for x in r if x not in s][:2]:
            print("    only rg:      ", l[:100])
        for l in [x for x in s if x not in r][:2]:
            print("    only scrutate:", l[:100])
        sys.stdout.flush()


shutil.rmtree(KEEP, ignore_errors=True)
os.makedirs(KEEP, exist_ok=True)
# the saved cases every run replays, before the random ones: differences found before
for case in json.load(open(CASES, encoding="utf-8")):
    load(case)
    compare(case["args"], case["name"])
while time.time() - t0 < SECONDS:
    make()
    for _ in range(6):
        a = paths(args())
        compare(a)
        # sorted output is printed from records, a path of its own: each case runs both ways
        if "--sort" not in a:
            compare(["--sort", *a])
shutil.rmtree(WORK, ignore_errors=True)
print(f"{n} runs in {time.time() - t0:.0f} s (seed {SEED}), {compared} compared: {diffs} differ, {known} are ripgrep's stop at a NUL byte" + (f"; by flags: {sorted(kinds.items(), key=lambda kv: -kv[1])[:12]}; kept under {KEEP}" if diffs else ""))
if not diffs:
    shutil.rmtree(HERE, ignore_errors=True)
if not nonempty:
    print("no compared run had any ripgrep output: nothing was really compared")
sys.exit(1 if diffs or not nonempty else 0)
