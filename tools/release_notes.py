#!/usr/bin/env python3
"""The release notes of VERSION: its section of CHANGELOG.md, without the link references, its links
to the repository's files pointing at the tag's tree. It fails,
so that no release goes out with notes that leave its changes out, when:
- `## [Unreleased]` (in any case, bracketed or not) still holds entries: they would be missing
  from the notes and stay "Unreleased" in every archive (fold them into the version's section
  first, with the tag's date: docs/RELEASING.md, step 1);
- there is no `## [VERSION]` section with entries, or there are two: a fold that added a heading
  and kept the old one would publish one of them alone;
- the version has no link reference (`[VERSION]: URL`);
- the notes are longer than a GitHub release body takes (MAX_CHARS).

    python3 tools/release_notes.py 0.1.0 [--out notes.md] [--changelog CHANGELOG.md]
"""
import argparse
import re
import sys

LINK = re.compile(r"^\[[^\]]+\]: \S")
# GitHub refuses a release body over 125,000 characters ("body is too long"): no GitHub page
# states it, the figure is from tool reports (intuit/auto#2418; release-it cuts at 124,000)
MAX_CHARS = 124_000
# a link to a file of the repository (`](docs/CLI.md#x)`), not a URL, an anchor or an absolute path
RELATIVE = re.compile(r"\]\((?![a-zA-Z][a-zA-Z0-9+.-]*:|#|/|\.\./)([^)\s]+)\)")
# what is left of a link after the rewrite that does not resolve on a release page
UNRESOLVED = re.compile(r"\]\((?!https?://|#)")


def link_problems(notes: str) -> list[str]:
    """What the rewrite could not make resolve on a release page: a link it left relative (a
    title, an angle link, `../`), an image (a blob URL serves a page, not the image), or an odd
    number of backticks (code spans the rewrite cannot tell apart)."""
    parts = notes.split("`")
    if len(parts) % 2 == 0:
        return ["the notes have an odd number of backticks: a code span is not closed, so their links cannot be checked"]
    prose = "`".join(parts[0::2])
    bad = [f"a link that does not resolve on a release page: ...{prose[m.start() - 30:m.end() + 40]}..." for m in UNRESOLVED.finditer(prose)]
    bad += [f"an image, which a release page cannot show from the repository: ...{prose[m.start():m.end() + 40]}..." for m in re.finditer(r"!\[", prose)]
    return bad


def absolute(text: str, base: str) -> str:
    """`text` with each link to a file of the repository pointing at `base` (the tag's tree), so it
    resolves on the release page too; inline code is left as written."""
    parts = text.split("`")
    for i in range(0, len(parts), 2):
        parts[i] = RELATIVE.sub(lambda m: f"]({base}/{m.group(1)})", parts[i])
    return "`".join(parts)


def sections(text: str) -> list[tuple[str, list[str]]]:
    """(NAME, its lines, the heading first) of each `## [NAME]` or `## NAME` section, in order."""
    out: list[tuple[str, list[str]]] = []
    for line in text.splitlines():
        m = re.match(r"^## (?:\[([^\]]+)\]|(\S+))", line)
        if m:
            out.append((m.group(1) or m.group(2), [line]))
        elif out:
            out[-1][1].append(line)
    return out


def entries(lines: list[str]) -> list[str]:
    """The lines of a section that say something: not blank, not a `###` heading, not a link."""
    return [line for line in lines if line.strip() and not line.startswith("###") and not LINK.match(line)]


def notes(text: str, version: str) -> tuple[str, list[str]]:
    """(the notes, the reasons they cannot be released) for `version` in CHANGELOG text `text`."""
    s = sections(text)
    problems = []
    unreleased = [line for name, lines in s if name.lower() == "unreleased" for line in entries(lines[1:])]
    if unreleased:
        problems.append(f"## [Unreleased] holds {len(unreleased)} lines of entries: fold them into ## [{version}] with the tag's date (docs/RELEASING.md, step 1)")
    ours = [lines for name, lines in s if name == version]
    if len(ours) > 1:
        problems.append(f"CHANGELOG.md has {len(ours)} ## [{version}] sections: one version, one section")
    body = [line for line in (ours[0] if ours else []) if not LINK.match(line)]
    if not entries(body[1:]):
        problems.append(f"CHANGELOG.md has no ## [{version}] section with entries")
    if not re.search(rf"^\[{re.escape(version)}\]: \S", text, re.M):
        problems.append(f"CHANGELOG.md has no link reference [{version}]: URL")
    out = "\n".join(body).strip() + "\n"
    # a release page resolves a relative link under /releases/tag/, where no file is
    repo = re.search(rf"^\[{re.escape(version)}\]: (\S+)/releases/tag/", text, re.M)
    if repo:
        out = absolute(out, f"{repo.group(1)}/blob/v{version}")
        problems += link_problems(out)
    if len(out) > MAX_CHARS:
        problems.append(f"the notes of {version} are {len(out):,} characters, {len(out) - MAX_CHARS:,} over the {MAX_CHARS:,} a release body takes: shorten its section")
    return out, problems


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("version")
    ap.add_argument("--out")
    ap.add_argument("--changelog", default="CHANGELOG.md")
    a = ap.parse_args(argv)
    with open(a.changelog, encoding="utf-8") as f:
        text, problems = notes(f.read(), a.version)
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        return 1
    if a.out:
        with open(a.out, "w", encoding="utf-8") as f:
            f.write(text)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
