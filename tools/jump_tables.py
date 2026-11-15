#!/usr/bin/env python3
"""Whether the tree-sitter grammars built in a cargo profile directory kept their lexers' jump
tables, and which grammar crates already carry tree-sitter's fix (#5984) in their parser.c. GCC 15
drops the tables at the -O0 a generated parser.c asks for, unless -fjump-tables is given
(.cargo/config.toml, docs/INSTALL.md).

usage: tools/jump_tables.py PROFILE_DIR   (target/release, say)

Exits 1 when tree-sitter-c's lexer, which has tables whenever they are on, has none. Linux only.
"""
import glob
import json
import os
import re
import shutil
import subprocess
import sys

REPO = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
# x86_64 jumps through a table with `jmp *`, aarch64 branches to a register with `br`
INDIRECT = re.compile(r"\b(?:jmp\s+\*|br\s+x\d+)")


def pragma(crate_dir):
    """How the crate's generated parser.c sets GCC's optimization: fixed, old or none."""
    kinds = set()
    for path in glob.glob(os.path.join(crate_dir, "**", "parser.c"), recursive=True):
        text = open(path, encoding="utf-8", errors="replace").read()
        if '#pragma GCC optimize ("O0", "jump-tables")' in text:
            kinds.add("fixed upstream")
        elif '#pragma GCC optimize ("O0")' in text:
            kinds.add("old pragma")
    return "old pragma" if "old pragma" in kinds else (kinds.pop() if kinds else "no O0 pragma")


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    if not sys.platform.startswith("linux") or shutil.which("objdump") is None:
        print("skip: needs Linux and objdump")
        return 0
    meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=REPO, capture_output=True, text=True, check=True).stdout)
    crates = {p["name"]: p for p in meta["packages"] if p["name"].startswith("tree-sitter-")}
    newest = {}
    for obj in glob.glob(os.path.join(sys.argv[1], "build", "tree-sitter-*", "out", "*parser.o")):
        name = re.sub(r"-[0-9a-f]{16}$", "", os.path.basename(os.path.dirname(os.path.dirname(obj))))
        if name in crates and (name not in newest or os.path.getmtime(obj) > os.path.getmtime(newest[name])):
            newest[name] = obj
    if "tree-sitter-c" not in newest:
        print(f"FAIL no tree-sitter-c object under {sys.argv[1]}/build: build the profile first")
        return 1
    old = 0
    for name in sorted(newest):
        tables = len(INDIRECT.findall(subprocess.run(["objdump", "-d", newest[name]], capture_output=True, text=True).stdout))
        state = pragma(os.path.dirname(crates[name]["manifest_path"]))
        old += state == "old pragma"
        print(f"{name} {crates[name]['version']}: {tables} indirect jumps, parser.c {state}")
    if old == 0:
        print("every grammar's parser.c carries the fix: the CFLAGS lines in .cargo/config.toml can go")
    tables_c = len(INDIRECT.findall(subprocess.run(["objdump", "-d", newest["tree-sitter-c"]], capture_output=True, text=True).stdout))
    if tables_c == 0:
        print("FAIL tree-sitter-c lost its lexer's jump tables: -fjump-tables did not reach the build (docs/INSTALL.md)")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
