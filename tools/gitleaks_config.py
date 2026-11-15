#!/usr/bin/env python3
"""Fail when the gitleaks config exempts files: an allow list with `paths`, the config's own or a
rule's, in either spelling (`[allowlist]`, `[[allowlists]]`), here or in a config it extends
(`[extend] path`, whose allow lists gitleaks appends). gitleaks 8.30.1 applies an entry's paths
alone, whatever its condition, so one such entry passes every secret in those files. A line the
rules take for a secret that is none carries `gitleaks:allow` and its reason instead.

    python3 tools/gitleaks_config.py [CONFIG]   # default .gitleaks.toml
"""
import sys
import tomllib


def exempting(config: dict) -> list[str]:
    """Where `config` itself lists paths to exempt: `allowlists[0]`, `rules[2].allowlist`, ..."""
    def lists(node: dict, at: str):
        if isinstance(node.get("allowlist"), dict):
            yield f"{at}allowlist", node["allowlist"]
        for i, a in enumerate(node.get("allowlists") or []):
            yield f"{at}allowlists[{i}]", a

    found = [where for where, a in lists(config, "") if a.get("paths")]
    for r, rule in enumerate(config.get("rules") or []):
        found += [where for where, a in lists(rule, f"rules[{r}].") if a.get("paths")]
    return found


def check(path: str, depth: int = 0) -> list[str]:
    """`path: where` of each exempting allow list in `path` and the configs it extends."""
    with open(path, "rb") as f:
        config = tomllib.load(f)
    found = [f"{path}: {where}" for where in exempting(config)]
    # gitleaks reads an extended path from where it runs, as this does, two levels deep at most
    extended = (config.get("extend") or {}).get("path")
    if extended and depth < 2:
        found += check(extended, depth + 1)
    return found


def main(argv: list[str]) -> int:
    found = check(argv[1] if len(argv) > 1 else ".gitleaks.toml")
    for f in found:
        print(f"{f} has paths, which exempt whole files from the scan; mark the line with gitleaks:allow and its reason instead")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
