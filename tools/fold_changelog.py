#!/usr/bin/env python3
"""Fold CHANGELOG.md for a release (docs/RELEASING.md, step 1).

The version's CHANGELOG section becomes the description in docs/changes/VERSION-summary.md (what
the release is, kept by hand), dated, with one sentence that links the full list; the release date's other carriers
(check_version.DATED: DELIVERY.md, SECURITY.md, CITATION.cff) take the same date. The full list,
every entry under `## [Unreleased]` and the version's old section as they stood, moves verbatim
to docs/changes/VERSION.md, which the archives pack. `## [Unreleased]` stays as an empty heading,
and the two link references go at the end. Nothing is deleted. Links are rebased for where each
text lands, so they resolve in the tree and, after tools/release_notes.py, on the release page.

usage: fold_changelog.py VERSION DATE [--root DIR] [--repo URL]
"""
import argparse
import os
import re
import sys

LINK_REF = re.compile(r"^\[[^\]]+\]: \S")
RELATIVE = re.compile(r"\]\((?![a-zA-Z][a-zA-Z0-9+.-]*:|#|/)([^)\s]+)\)")


def rebase(text: str, from_dir: str, to_dir: str) -> str:
    """`text`'s links to files of the tree, written from `from_dir`, rewritten for `to_dir` (both
    relative to the root); inline code is left as written."""
    def one(m):
        path, _, anchor = m.group(1).partition("#")
        moved = os.path.relpath(os.path.normpath(os.path.join(from_dir, path)), to_dir or ".").replace(os.sep, "/")
        return f"]({moved}{'#' + anchor if anchor else ''})"
    parts = text.split("`")
    for i in range(0, len(parts), 2):
        parts[i] = RELATIVE.sub(one, parts[i])
    return "`".join(parts)


def trim(lines: list[str]) -> list[str]:
    while lines and not lines[0].strip():
        lines = lines[1:]
    while lines and not lines[-1].strip():
        lines = lines[:-1]
    return lines


def fold(changelog: str, summary: str, version: str, date: str, repo: str) -> tuple[str, str]:
    """(the new CHANGELOG.md, docs/changes/VERSION.md) from the CHANGELOG and the summary's text."""
    lines = changelog.split("\n")
    heads = [i for i, line in enumerate(lines) if line.startswith("## ")]
    unrel = next((i for i in heads if re.match(r"^## \[?unreleased\]?\s*$", lines[i], re.I)), None)
    ver = next((i for i in heads if re.match(rf"^## \[{re.escape(version)}\]", lines[i])), None)
    if unrel is None or ver is not None and ver < unrel:
        raise SystemExit(f"CHANGELOG.md needs `## [Unreleased]`, above `## [{version}]` if it has one")
    # a version without a section of its own (every release after the first) folds Unreleased alone
    stop = ver if ver is not None else next((i for i in heads if i > unrel), len(lines))
    end = next((i for i in heads if i > ver), len(lines)) if ver is not None else stop
    refs = [line for line in lines if LINK_REF.match(line)]
    unreleased = trim([line for line in lines[unrel + 1:stop] if not LINK_REF.match(line)])
    old = trim([line for line in lines[ver + 1:end] if not LINK_REF.match(line)]) if ver is not None else []
    stood = (f"then its section as it stood ({lines[ver][3:]})." if ver is not None else "").strip()
    later = trim([line for line in lines[end:] if not LINK_REF.match(line)])
    full = [
        f"# Every change in {version}",
        "",
        f"The full list behind the {version} section of [CHANGELOG.md](../../CHANGELOG.md), moved here",
        "verbatim when the release was folded: the entries written under Unreleased while it was",
        f"prepared{', ' + stood if stood else '.'}",
        "",
        f"## Written under Unreleased before {version}",
        "",
        *rebase("\n".join(unreleased), "", "docs/changes").split("\n"),
        "",
    ]
    if ver is not None:
        full += [f"## {lines[ver][3:]}", "", *rebase("\n".join(old), "", "docs/changes").split("\n"), ""]
    described = trim(re.sub(r"<!--.*?-->", "", summary, flags=re.S).split("\n"))
    section = [
        f"## [{version}] - {date}",
        "",
        *rebase("\n".join(described), "docs/changes", "").split("\n"),
        "",
        f"Every change made on the way to {version} is listed in [docs/changes/{version}.md](docs/changes/{version}.md).",
    ]
    kept = [r for r in refs if not re.match(rf"^\[(unreleased|{re.escape(version)})\]:", r, re.I)]
    out = lines[:unrel] + ["## [Unreleased]", ""] + section + ([""] + later if later else []) + [""] + kept + [
        f"[Unreleased]: {repo}/compare/v{version}...HEAD",
        f"[{version}]: {repo}/releases/tag/v{version}",
        "",
    ]
    return "\n".join(out), "\n".join(full)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("version")
    ap.add_argument("date")
    ap.add_argument("--root", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    ap.add_argument("--repo", default="https://github.com/scrutate/scrutate")
    a = ap.parse_args(argv)
    if not re.fullmatch(r"\d{4}-\d\d-\d\d", a.date):
        raise SystemExit(f"the date is YYYY-MM-DD, not {a.date}")
    path = os.path.join(a.root, "CHANGELOG.md")
    summary_path = os.path.join(a.root, "docs", "changes", f"{a.version}-summary.md")
    full_path = os.path.join(a.root, "docs", "changes", f"{a.version}.md")
    if os.path.exists(full_path):
        raise SystemExit(f"{full_path} exists: {a.version} is folded already")
    with open(path, encoding="utf-8") as f, open(summary_path, encoding="utf-8") as g:
        changelog, full = fold(f.read(), g.read(), a.version, a.date, a.repo)
    with open(full_path, "w", encoding="utf-8") as f:
        f.write(full)
    with open(path, "w", encoding="utf-8") as f:
        f.write(changelog)
    # the release date's other carriers, as check_version.py reads them
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import check_version
    for rel, pattern in check_version.DATED:
        p = os.path.join(a.root, rel)
        with open(p, encoding="utf-8") as f:
            text = f.read()
        text, n = re.subn(pattern, lambda m: m.group(0).replace(m.group(1), a.date), text, flags=re.M)
        if n == 0:
            raise SystemExit(f"{rel}: no release date where check_version.py reads one")
        with open(p, "w", encoding="utf-8") as f:
            f.write(text)
    print(f"folded: CHANGELOG.md's {a.version} section from {os.path.relpath(summary_path, a.root)}, the full list in {os.path.relpath(full_path, a.root)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
