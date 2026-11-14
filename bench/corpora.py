#!/usr/bin/env python3
"""Fetch and generate the benchmark corpora, each pinned to a commit or a content checksum.

usage: corpora.py --root DIR [--only NAME...] [--verify]

Real corpora are shallow clones of release tags; a tag given with its commit must still name it. Paths that differ only by letter case are removed
(and the removal committed locally) so every tool sees the same files on case-insensitive file
systems. Generated corpora are built deterministically from the Linux tree and a seeded generator.
`manifest.json` records, per corpus, its source, commit, file count, bytes and tree checksum
(SHA-256 over the sorted list of `path NUL sha256(content)`; `.git` and `.scrutate` excluded).
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import shutil
import subprocess
import sys

REAL = {
    "linux": ("https://github.com/torvalds/linux.git", "v7.3"),
    "llvm": ("https://github.com/llvm/llvm-project.git", "llvmorg-23.1.5"),
    "ripgrep": ("https://github.com/BurntSushi/ripgrep.git", "15.2.0"),
    # a Go tree for gopls (bench/yardstick.py W4, W5); a third value is the commit the tag must name
    "hugo": ("https://github.com/gohugoio/hugo.git", "v0.170.0", "f7aa05f0f521611aa960b2da56e1870ea04f57b2"),
}
GENERATED = ["bigfile", "smallfiles", "binheavy", "patho", "rgbuilt", "linux-nodot", "startup"]


def own_copy(src, dst):
    """A copy with an inode of its own. A hard link would be cheaper to make, but on macOS opening
    a file with two links costs every tool about twice as much (19 us against 10 on APFS), here
    and in the tree it links to. APFS clones share the data blocks, so they cost no space."""
    if sys.platform == "darwin":
        import ctypes

        libc = ctypes.CDLL(None, use_errno=True)
        if libc.clonefile(os.fsencode(src), os.fsencode(dst), 0) == 0:
            return
    shutil.copy2(src, dst)


def sh(cmd, cwd=None):
    return subprocess.run(cmd, cwd=cwd, check=True, capture_output=True, text=True).stdout


def files_of(root):
    out = []
    for d, dirs, files in os.walk(root):
        dirs[:] = sorted(x for x in dirs if x not in (".git", ".scrutate"))
        for f in files:
            p = os.path.join(d, f)
            if os.path.isfile(p) and not os.path.islink(p):
                out.append(os.path.relpath(p, root).replace(os.sep, "/"))
    return sorted(out)


def tree_hash(root):
    h = hashlib.sha256()
    n = size = 0
    for rel in files_of(root):
        with open(os.path.join(root, rel), "rb") as f:
            data = f.read()
        h.update(rel.encode() + b"\0" + hashlib.sha256(data).hexdigest().encode() + b"\n")
        n += 1
        size += len(data)
    return h.hexdigest(), n, size


def fetch_real(root, name):
    url, tag, *pinned = REAL[name]
    d = os.path.join(root, name)
    if not os.path.isdir(d):
        subprocess.run(["git", "clone", "-q", "--depth", "1", "--branch", tag, url, d], check=True)
    tagged = sh(["git", "rev-list", "-n", "1", tag], cwd=d).strip()
    if pinned and tagged != pinned[0]:
        sys.exit(f"{name}: tag {tag} names {tagged}, not the pinned {pinned[0]}")
    tracked = sh(["git", "ls-files", "-z"], cwd=d).split("\0")
    seen, coll = {}, set()
    for p in tracked:
        if not p:
            continue
        k = p.lower()
        if k in seen:
            coll.update([p, seen[k]])
        seen[k] = p
    # search-tool configuration inside a repository (`.ignore`, `.rgignore`, `.agignore`) is read by
    # some tools only; removing it gives every tool the same ignore rules (.gitignore + hidden)
    tool_cfg = [p for p in tracked if p and os.path.basename(p) in (".ignore", ".rgignore", ".agignore")]
    coll.update(tool_cfg)
    if coll:
        subprocess.run(["git", "rm", "-q", "--cached", "--"] + sorted(coll), cwd=d, check=True)
        for p in coll:
            try:
                os.remove(os.path.join(d, p))
            except FileNotFoundError:
                pass
        subprocess.run(["git", "-c", "user.name=bench", "-c", "user.email=bench@localhost", "commit", "-q", "-m",
                        "bench: drop case-colliding paths and search-tool ignore files"], cwd=d, check=True)
    return {"kind": "git", "url": url, "tag": tag, "tag_commit": tagged, "head": sh(["git", "rev-parse", "HEAD"], cwd=d).strip(),
            "removed_paths": sorted(coll)}


def text_chunks(linux, exts, lines_per=24):
    """Deterministic stream of text chunks from the Linux tree (files in sorted order)."""
    buf = []
    for rel in sorted(sh(["git", "ls-files", "-z"], cwd=linux).split("\0")):
        if not rel.endswith(exts):
            continue
        try:
            with open(os.path.join(linux, rel), "rb") as f:
                for line in f:
                    buf.append(line)
                    if len(buf) == lines_per:
                        yield b"".join(buf)
                        buf = []
        except FileNotFoundError:
            continue


def generate(root, name):
    linux = os.path.join(root, "linux")
    d = os.path.join(root, name)
    if os.path.isdir(d):
        return {"kind": "generated"}
    tmp = d + ".tmp"
    subprocess.run(["rm", "-rf", tmp], check=True)
    os.makedirs(tmp)
    if name == "bigfile":
        # one large file: every C source file of the Linux tree, concatenated in path order
        with open(os.path.join(tmp, "linux-c.txt"), "wb") as out:
            for rel in sorted(sh(["git", "ls-files", "-z"], cwd=linux).split("\0")):
                if rel.endswith(".c") and os.path.isfile(os.path.join(linux, rel)):
                    with open(os.path.join(linux, rel), "rb") as f:
                        out.write(f.read())
        info = "all *.c files of linux (tracked, path order) concatenated"
    elif name == "smallfiles":
        n = 0
        for chunk in text_chunks(linux, (".h",)):
            sub = os.path.join(tmp, f"d{n // 1000:04d}")
            if n % 1000 == 0:
                os.makedirs(sub)
            with open(os.path.join(sub, f"f{n % 1000:04d}.txt"), "wb") as f:
                f.write(chunk)
            n += 1
            if n == 200_000:
                break
        info = "200,000 files of 24 lines each from the linux headers"
    elif name == "binheavy":
        rng = random.Random(20261108)
        os.makedirs(os.path.join(tmp, "bin"))
        for i in range(10_000):
            sub = os.path.join(tmp, "bin", f"b{i // 500:03d}")
            os.makedirs(sub, exist_ok=True)
            with open(os.path.join(sub, f"blob{i:05d}.dat"), "wb") as f:
                f.write(rng.randbytes(32 * 1024))
        os.makedirs(os.path.join(tmp, "src"))
        for n, chunk in enumerate(text_chunks(linux, (".c",), lines_per=200)):
            with open(os.path.join(tmp, "src", f"s{n:05d}.c"), "wb") as f:
                f.write(chunk)
            if n == 2999:
                break
        info = "10,000 random 32 KB binary files + 3,000 text files of 200 lines from linux"
    elif name == "patho":
        with open(os.path.join(tmp, "longline.txt"), "wb") as f:
            unit = b"abcdefghij klmnopqrst uvwxyz0123 "
            reps = (100 << 20) // len(unit)
            for i in range(reps):
                f.write(unit)
                if i in (reps // 4, reps // 2, 3 * reps // 4):
                    f.write(b"PATHOLOGICAL_NEEDLE ")
        with open(os.path.join(tmp, "manylines.txt"), "wb") as f:
            f.write(b"x\n" * 10_000_000)
        with open(os.path.join(tmp, "redos.txt"), "wb") as f:
            f.write((b"a" * 40 + b"!\n") * 50_000)
        info = "a 100 MB single line, 10 M one-byte lines, 50,000 lines that defeat backtracking engines"
    elif name == "rgbuilt":
        # a Rust repository after a build: ripgrep's sources plus a git-ignored target/ directory
        src = os.path.join(root, "ripgrep")
        subprocess.run(["git", "clone", "-q", src, tmp + "/r"], check=True)
        for e in os.listdir(tmp + "/r"):
            os.rename(os.path.join(tmp, "r", e), os.path.join(tmp, e))
        os.rmdir(tmp + "/r")
        n = 0
        for chunk in text_chunks(linux, (".c",), lines_per=200):
            sub = os.path.join(tmp, "target", "debug", "build", f"g{n // 500:03d}")
            os.makedirs(sub, exist_ok=True)
            with open(os.path.join(sub, f"out{n:05d}.rs"), "wb") as f:
                f.write(chunk)
            n += 1
            if n == 20_000:
                break
        info = "ripgrep 15.2.0 checkout + 20,000 generated files under the git-ignored target/"
    elif name == "linux-nodot":
        # the Linux tree without hidden files: the file set every indexed engine indexes alike
        for rel in sh(["git", "ls-files", "-z"], cwd=linux).split("\0"):
            if not rel or any(p.startswith(".") for p in rel.split("/")):
                continue
            s = os.path.join(linux, rel)
            if not os.path.isfile(s):
                continue
            t = os.path.join(tmp, rel)
            os.makedirs(os.path.dirname(t), exist_ok=True)
            own_copy(s, t)
        info = "linux tracked files without hidden files and directories (copies; clones where the file system has them)"
    elif name == "startup":
        with open(os.path.join(tmp, "a.txt"), "wb") as f:
            f.write(b"hello\n")
        info = "one file of one line (start-up time)"
    else:
        raise SystemExit(f"unknown corpus {name}")
    os.rename(tmp, d)
    return {"kind": "generated", "recipe": info}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=os.path.expanduser("~/.cache/scrutate-bench/corpora"))
    ap.add_argument("--only", nargs="*")
    a = ap.parse_args()
    os.makedirs(a.root, exist_ok=True)
    mpath = os.path.join(a.root, "manifest.json")
    manifest = json.load(open(mpath)) if os.path.exists(mpath) else {}
    for name in list(REAL) + GENERATED:
        if a.only and name not in a.only:
            continue
        print(f"[corpora] {name}", file=sys.stderr)
        entry = fetch_real(a.root, name) if name in REAL else generate(a.root, name)
        prev = manifest.get(name, {})
        entry = {**prev, **entry}
        digest, n, size = tree_hash(os.path.join(a.root, name))
        entry.update({"tree_sha256": digest, "files": n, "bytes": size})
        manifest[name] = entry
        json.dump(manifest, open(mpath, "w"), indent=1, sort_keys=True)
    print(json.dumps({k: {x: v[x] for x in ("files", "bytes", "tree_sha256")} for k, v in manifest.items()}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
