#!/usr/bin/env python3
"""Fail when a change puts a new Rust item between an unchanged `///` block and the item it
documented, so the doc comment silently moves onto the new item.

usage: check_doc_shift.py [REV | A..B | --each A..B | --diff FILE]   (default HEAD: the diff of REV^..REV)

`--each A..B` checks every commit the range holds on its own, as a push of several commits needs:
the diff of A..B alone can hide a commit whose shift a later one touched.

A change block (added and removed lines between context lines) that follows unchanged `///`
lines (attributes, over one line or several, and blank lines between them aside) and adds an item (fn, struct, enum, type,
const, static, impl, mod, trait, union, macro_rules!) is reported, unless it also removes the
documented item and does not add it back below another (an edit, a rename or a replacement in
place). The fix is to insert the new item above the doc block.
"""
from __future__ import annotations

import re
import subprocess
import sys

ITEM = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|const|unsafe|default|extern(?:\s+\"[^\"]*\")?)\s+)*"
    r"(fn|struct|enum|type|const|static|impl|mod|trait|union|macro_rules!)(?=[\s<])\s*"
)


def item(line: str) -> tuple[str, str] | None:
    """(kind, name) when `line` opens a Rust item; an `impl`'s generics are passed over whole, nested
    ones too (`impl<T: A<B>> Name`), and generics that go on past the line leave the name empty."""
    if line.lstrip().startswith("//"):
        return None
    m = ITEM.match(line)
    if not m:
        return None
    rest = line[m.end():]
    if rest.startswith("<"):
        depth, i = 0, 0
        while i < len(rest) and (depth or i == 0):
            if rest.startswith("->", i):
                i += 2
                continue
            depth += {"<": 1, ">": -1}.get(rest[i], 0)
            i += 1
        rest = rest[i:].lstrip() if depth == 0 else ""
    name = re.match(r"\w+", rest)
    return (m.group(1), name.group(0) if name else "") if name or not rest.strip() else None


def code_lines(lines: list[str]) -> list[str]:
    """`lines` without attributes, one spread over several lines (`#[cfg(any(` to `))]`) included."""
    out, depth = [], 0
    for line in lines:
        if depth == 0 and line.strip().startswith("#["):
            depth = line.count("[") - line.count("]")
        elif depth > 0:
            depth += line.count("[") - line.count("]")
        else:
            out.append(line)
    return out


def shifted(diff: str) -> list[str]:
    """`path:line: text` of each added item that takes over the doc block above it."""
    found, path, new_line, ctx, block = [], None, 0, [], []

    def close():
        nonlocal ctx
        if not block or not path or not path.endswith(".rs"):
            return
        # what the new file holds above the next block: this block's added lines count too
        before, ctx = ctx, (ctx + [t for kind, _, t in block if kind == "+"])[-20:]
        above = [c for c in code_lines(before) if c.strip()]
        if not above or not above[-1].lstrip().startswith("///"):
            return
        added = [(n, t, item(t)) for kind, n, t in block if kind == "+" and item(t)]
        removed = [item(t) for kind, _, t in block if kind == "-" and item(t)]
        # the documented item gone for good is a replacement; back below a new item, it lost its doc
        if added and (not removed or (added[0][2] != removed[0] and removed[0] in [a[2] for a in added])):
            n, t, _ = added[0]
            found.append(f"{path}:{n}: {t.strip()}")

    for raw in diff.splitlines():
        if raw.startswith("diff --git"):
            close()
            block, ctx, path = [], [], None
        elif raw.startswith("+++ "):
            path = raw[4:].removeprefix("b/") if raw[4:] != "/dev/null" else None
        elif raw.startswith("--- "):
            continue
        elif raw.startswith("@@"):
            close()
            block, ctx = [], []
            new_line = int(re.match(r"@@ -\d+(?:,\d+)? \+(\d+)", raw).group(1))
        elif raw.startswith("+"):
            block.append(("+", new_line, raw[1:]))
            new_line += 1
        elif raw.startswith("-"):
            block.append(("-", new_line, raw[1:]))
        elif raw.startswith(" ") or raw == "":
            close()
            block = []
            ctx = (ctx + [raw[1:]])[-20:]
            new_line += 1
    close()
    return found


def main(argv: list[str]) -> int:
    if argv[:1] == ["--each"]:
        revs = subprocess.run(["git", "rev-list", "--reverse", argv[1]], capture_output=True, text=True, check=True).stdout.split()
        return max((main([r]) for r in revs), default=0)
    if argv[:1] == ["--diff"]:
        diff = sys.stdin.read() if argv[1:2] in ([], ["-"]) else open(argv[1], encoding="utf-8").read()
    else:
        rev = argv[0] if argv else "HEAD"
        # `git show` reads a commit against its parent (a merge against the first), a root against nothing
        cmd = ["git", "diff", "-U5", *rev.split("..", 1)] if ".." in rev else ["git", "show", "--format=", "-U5", "--diff-merges=first-parent", rev]
        diff = subprocess.run([*cmd, "--", "*.rs"], capture_output=True, text=True, check=True).stdout
    bad = shifted(diff)
    for b in bad:
        print(f"{b}\n    goes between a doc comment and the item it documented: insert it above the doc block")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
