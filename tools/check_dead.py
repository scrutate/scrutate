#!/usr/bin/env python3
"""Check `scrutate dead` on a tree: every definition it calls dead has no whole-word occurrence of
its name outside its own lines, by ripgrep (hidden files included, the ignore files obeyed), in
any case for a name `dead` matches in any case.

    python3 tools/check_dead.py BINARY [ROOT]

Prints the count and a sample; exits 1 and names each one that has an occurrence elsewhere.
"""

import json
import subprocess
import sys


def any_case_suffixes(binary: str) -> dict[str, str]:
    """File suffix -> language, for the languages whose names this build matches in any case
    (langs::ignores_case). shortcut: a suffix shared by several languages (`.pp`) is left out, as
    the language there comes from the text; check those case-sensitively until the CLI says it."""
    rows = json.loads(subprocess.run([binary, "languages", "--json"], capture_output=True, text=True, check=True).stdout)
    return {ext: r["id"] for r in rows if r["id"] in ("fortran", "pascal", "ada", "php") for ext in r["extensions"]}


def ignores_case(d: dict, suffixes: dict[str, str]) -> bool:
    lang = next((lang for ext, lang in suffixes.items() if d["path"].endswith(ext)), None)
    if lang is None or not d["name"].isascii():
        return False
    return lang != "php" or d["kind"] in ("function", "method", "class", "interface", "trait", "enum")


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    binary, root = sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "."
    out = subprocess.run([binary, "--root", root, "dead", "--json"], capture_output=True, text=True, stdin=subprocess.DEVNULL)
    if out.returncode not in (0, 1):
        print(out.stderr, file=sys.stderr)
        return 2
    result = json.loads(out.stdout)["result"]
    suffixes = any_case_suffixes(binary)
    bad = []
    for d in result["dead"]:
        rg = subprocess.run(
            ["rg", "--hidden", "-g", "!.git", "-g", "!.scrutate", "-n", "-w", "-F", *(["-i"] if ignores_case(d, suffixes) else []),
             "--no-heading", "--with-filename", "-e", d["name"], "."],
            cwd=root, capture_output=True, text=True, stdin=subprocess.DEVNULL,
        )
        for line in rg.stdout.splitlines():
            path, num, _ = line.split(":", 2)
            path = path.removeprefix("./")
            if not (path == d["path"] and d["start"] <= int(num) <= d["end"]):
                bad.append(f"{d['id']} {d['qualname']}: {line}")
    print(f"{len(result['dead'])} dead, {len(result['undecided'])} undecided")
    for d in result["dead"][:10]:
        print(f"  {d['id']}  [{d['kind']}] {d['qualname']}")
    for b in bad:
        print(f"NAMED ELSEWHERE: {b}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
