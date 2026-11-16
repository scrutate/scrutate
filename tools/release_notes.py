#!/usr/bin/env python3
"""The release notes of VERSION: its section of CHANGELOG.md, without the link references. It fails,
so that no release goes out with notes that leave its changes out, when:
- `## [Unreleased]` (in any case, bracketed or not) still holds entries: they would be missing
  from the notes and stay "Unreleased" in every archive (fold them into the version's section
  first, with the tag's date: docs/RELEASING.md, step 1);
- there is no `## [VERSION]` section with entries, or there are two: a fold that added a heading
  and kept the old one would publish one of them alone;
- the version has no link reference (`[VERSION]: URL`).

    python3 tools/release_notes.py 0.1.0 [--out notes.md] [--changelog CHANGELOG.md]
"""
import argparse
import re
import sys

LINK = re.compile(r"^\[[^\]]+\]: \S")


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
    return "\n".join(body).strip() + "\n", problems


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
