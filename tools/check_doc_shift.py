#!/usr/bin/env python3
"""Fail when a change puts a new Rust item between an unchanged `///` block and the item it
documented, so the doc comment silently moves onto the new item.

usage: check_doc_shift.py [REV | A..B | --each A..B | --diff FILE]   (default HEAD: the diff of REV^..REV)

`A..B` checks every commit the range holds on its own and reports a shift only while it is still in
the tree at B: the item that lost its doc comment has no doc lines directly above it, whatever
became of the newcomer or of the stolen lines; once it has some again, or is gone, the shift passes. The diff of A..B alone can hide one: an item removed above it in the same range makes
the doc line read as added. `--each A..B` reports every commit's shifts, fixed later or not.

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


def doc_run(above: list[str]) -> list[str]:
    """The `///` lines directly above, attributes and blank lines aside, stripped, in file order."""
    run = []
    for line in reversed([c for c in code_lines(above) if c.strip()]):
        if not line.lstrip().startswith("///"):
            break
        run.append(line.strip())
    return run[::-1]


def shifted(diff: str) -> list[str]:
    """`path:line: text` of each added item that takes over the doc block above it."""
    return [f"{path}:{n}: {t.strip()}" for path, n, t, _ in shifted_items(diff)]


def shifted_items(diff: str) -> list[tuple[str, int, str, list[str]]]:
    """(path, line, text, the doc lines it went in beneath) of each such item."""
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
            found.append((path, n, t, doc_run(before)))

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


def lines_at(rev: str, path: str) -> list[str] | None:
    shown = subprocess.run(["git", "show", f"{rev}:{path}"], capture_output=True, text=True)
    return shown.stdout.splitlines() if shown.returncode == 0 else None


def under(lines: list[str], i: int, docs: list[str]) -> bool:
    """Whether `docs` are among the doc lines directly above line `i`."""
    run = doc_run(lines[max(0, i - 200):i])
    return any(run[j:j + len(docs)] == docs for j in range(len(run) - len(docs) + 1))


def lost_doc(rev: str, path: str, docs: list[str]) -> tuple[str, str] | None:
    """(kind, name) of the item `docs` documented before commit `rev` took them."""
    lines = lines_at(f"{rev}^", path)
    if not lines or not docs:
        return None
    return next((item(l) for i, l in enumerate(lines) if item(l) and under(lines, i, docs)), None)


def still_lost(tip: str, path: str, text: str, docs: list[str], gone: tuple[str, str]) -> int | None:
    """The line at `tip` of `gone`, the item that lost `docs` to `text`, while it has no doc lines
    directly above it; None once it has some again or is gone. Of several items of that name, the
    one nearest the newcomer (or, renamed, the stolen doc lines) is meant."""
    lines = lines_at(tip, path)
    if lines is None:
        return None
    named = [i for i, l in enumerate(lines) if item(l) == gone]
    anchor = [i for i, l in enumerate(lines) if item(l) == item(text)] or [i for i, l in enumerate(lines) if l.strip() == docs[0]]
    if not named or (len(named) > 1 and not anchor):
        return None
    at = min(named, key=lambda i: min(abs(i - a) for a in anchor)) if anchor else named[0]
    return None if doc_run(lines[max(0, at - 200):at]) else at + 1


def still_in_tree(rng: str) -> list[str]:
    """`path:line: text` at the range's end of each shift a commit of `rng` made and none fixed: the
    item that lost its doc comment still has none."""
    tip = rng.split("..", 1)[1] or "HEAD"
    revs = subprocess.run(["git", "rev-list", "--reverse", rng], capture_output=True, text=True, check=True).stdout.split()
    found = []
    for r in revs:
        diff = subprocess.run(["git", "show", "--format=", "-U5", "--diff-merges=first-parent", r, "--", "*.rs"], capture_output=True, text=True, check=True).stdout
        for path, _, text, docs in shifted_items(diff):
            gone = lost_doc(r, path, docs)
            at = still_lost(tip, path, text, docs, gone) if gone and docs else None
            line = f"{path}:{at}: {gone[0] if gone else ''} {gone[1] if gone else ''} has no doc comment since {text.strip()} took it (from {r[:8]})"
            if at and line not in found:
                found.append(line)
    return found


def main(argv: list[str]) -> int:
    if argv[:1] == ["--each"]:
        revs = subprocess.run(["git", "rev-list", "--reverse", argv[1]], capture_output=True, text=True, check=True).stdout.split()
        return max((main([r]) for r in revs), default=0)
    if argv[:1] == ["--diff"]:
        diff = sys.stdin.read() if argv[1:2] in ([], ["-"]) else open(argv[1], encoding="utf-8").read()
    elif argv and ".." in argv[0]:
        diff = None
    else:
        # `git show` reads a commit against its parent (a merge against the first), a root against nothing
        cmd = ["git", "show", "--format=", "-U5", "--diff-merges=first-parent", argv[0] if argv else "HEAD"]
        diff = subprocess.run([*cmd, "--", "*.rs"], capture_output=True, text=True, check=True).stdout
    bad = still_in_tree(argv[0]) if diff is None else shifted(diff)
    for b in bad:
        print(f"{b}\n    goes between a doc comment and the item it documented: insert it above the doc block")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
