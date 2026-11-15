#!/usr/bin/env python3
"""Fail unless the gitleaks config is gitleaks' own rules with no change: `title` and `[extend]`
with `useDefault = true`, nothing else, its keys read in any case as gitleaks reads them. Any other
key or table (rules, disabledRules, an allow list, an extended path, ...) can narrow or exempt
what the scan sees, so it fails and is named; so does a `.gitleaksignore` beside the config, whose
fingerprints exempt findings. A line the rules take for a secret that is none carries
`gitleaks:allow` and its reason instead.

    python3 tools/gitleaks_config.py [CONFIG]   # default .gitleaks.toml
"""
import os
import sys
import tomllib


def join(a, b):
    """Two values whose keys differ only in case, kept both: tables merged, else a list."""
    if isinstance(a, dict) and isinstance(b, dict):
        out = dict(a)
        for k, v in b.items():
            out[k] = join(out[k], v) if k in out else v
        return out
    return (a if isinstance(a, list) else [a]) + (b if isinstance(b, list) else [b])


def lowered(node):
    """`node` with every table key in lower case, as gitleaks (viper) reads them; keys that differ
    only in case are joined, so a second spelling never passes for the first."""
    if isinstance(node, list):
        return [lowered(n) for n in node]
    if not isinstance(node, dict):
        return node
    out: dict = {}
    for key, value in node.items():
        k, v = key.lower(), lowered(value)
        out[k] = join(out[k], v) if k in out else v
    return out


def unexpected(config: dict) -> list[str]:
    """What `config` (keys lowered) holds beyond `title` and `[extend] useDefault = true`."""
    found = []
    for key, value in config.items():
        if key == "title" and isinstance(value, str):
            continue
        if key == "extend" and isinstance(value, dict):
            found += [f"extend.{k}" for k, v in value.items() if not (k == "usedefault" and v is True)]
            continue
        found.append(key)
    if not (isinstance(config.get("extend"), dict) and config["extend"].get("usedefault") is True):
        found.append("extend.usedefault (gitleaks' own rules) missing")
    return found


def check(path: str) -> list[str]:
    """`path: key` of each key the config may not hold, and the .gitleaksignore beside it."""
    with open(path, "rb") as f:
        found = [f"{path}: {key}" for key in unexpected(lowered(tomllib.load(f)))]
    ignore = os.path.join(os.path.dirname(path), ".gitleaksignore")
    return found + ([ignore] if os.path.exists(ignore) else [])


def main(argv: list[str]) -> int:
    found = check(argv[1] if len(argv) > 1 else ".gitleaks.toml")
    for f in found:
        what = "exempts findings by fingerprint" if f.endswith(".gitleaksignore") else "is not allowed: the config is gitleaks' own rules, unchanged"
        print(f"{f} {what}; mark the line with gitleaks:allow and its reason instead")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
