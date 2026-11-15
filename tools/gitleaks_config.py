#!/usr/bin/env python3
"""Fail when the gitleaks config exempts files: an allow list with `paths`, the config's own or a
rule's, in either spelling (`[allowlist]`, `[[allowlists]]`), here or in a config it extends
(`[extend] path`, whose allow lists gitleaks appends), its keys in any case (gitleaks reads them
so); or a `.gitleaksignore` beside the config, whose fingerprints exempt findings. gitleaks 8.30.1
applies an entry's paths alone, whatever its condition, so one such entry passes every secret in
those files. A line the rules take for a secret that is none carries `gitleaks:allow` and its
reason instead.

    python3 tools/gitleaks_config.py [CONFIG]   # default .gitleaks.toml
"""
import os
import sys
import tomllib


def join(a, b):
    """Two values whose keys differ only in case, kept both: lists and tables merged, else a list."""
    if isinstance(a, dict) and isinstance(b, dict):
        out = dict(a)
        for k, v in b.items():
            out[k] = join(out[k], v) if k in out else v
        return out
    return (a if isinstance(a, list) else [a]) + (b if isinstance(b, list) else [b])


def lowered(node):
    """`node` with every table key in lower case, as gitleaks (viper) reads them; keys that differ
    only in case are joined, so no spelling hides an entry."""
    if isinstance(node, list):
        return [lowered(n) for n in node]
    if not isinstance(node, dict):
        return node
    out: dict = {}
    for key, value in node.items():
        k, v = key.lower(), lowered(value)
        out[k] = join(out[k], v) if k in out else v
    return out


def tables(value) -> list[dict]:
    """The tables a key holds: one table, or the tables of a list."""
    if isinstance(value, dict):
        return [value]
    return [t for t in value if isinstance(t, dict)] if isinstance(value, list) else []


def exempting(config: dict) -> list[str]:
    """Where `config` (keys lowered) itself lists paths to exempt: `allowlists[0]`, `rules[2].allowlist[0]`, ..."""
    def lists(node: dict, at: str):
        for key in ("allowlist", "allowlists"):
            for i, a in enumerate(tables(node.get(key))):
                yield f"{at}{key}[{i}]", a

    found = [where for where, a in lists(config, "") if a.get("paths")]
    for r, rule in enumerate(tables(config.get("rules"))):
        found += [where for where, a in lists(rule, f"rules[{r}].") if a.get("paths")]
    return found


def check(path: str, depth: int = 0) -> list[str]:
    """`path: where` of each exempting allow list in `path` and the configs it extends."""
    with open(path, "rb") as f:
        config = lowered(tomllib.load(f))
    found = [f"{path}: {where}" for where in exempting(config)]
    # gitleaks reads an extended path from where it runs, as this does, two levels deep at most
    for extend in tables(config.get("extend")):
        extended = extend.get("path")
        for p in extended if isinstance(extended, list) else [extended]:
            if isinstance(p, str) and p and depth < 2:
                found += check(p, depth + 1)
    return found


def main(argv: list[str]) -> int:
    config = argv[1] if len(argv) > 1 else ".gitleaks.toml"
    found = check(config)
    for f in found:
        print(f"{f} has paths, which exempt whole files from the scan; mark the line with gitleaks:allow and its reason instead")
    ignore = os.path.join(os.path.dirname(config), ".gitleaksignore")
    if os.path.exists(ignore):
        found.append(ignore)
        print(f"{ignore} exempts findings by fingerprint, away from the line; mark the line with gitleaks:allow and its reason instead")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
