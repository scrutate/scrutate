#!/usr/bin/env python3
"""Fail when a release version differs from the one any shipped package declares, or a release
date from the date of the version's CHANGELOG section.

usage: check_version.py VERSION   (release.yml passes the tag without its leading v)
"""
import json
import os
import re
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")


# every other place a release writes its version: (file, pattern whose group 1 is the version)
WRITTEN = [
    ("integrations/vscode/package-lock.json", r'^  "version": "([^"]+)"'),
    ("integrations/vscode/package-lock.json", r'^    "": \{\n      "name": "[^"]+",\n      "version": "([^"]+)"'),
    ("packaging/scoop/scrutate.json", r'^    "version": "([^"]+)"'),
    ("packaging/scoop/scrutate.json", r"/releases/download/v([0-9][^/]*)/"),
    ("packaging/scoop/scrutate.json", r"scrutate-([0-9][^-]*)-[a-z0-9_]+-pc-windows-msvc"),
    ("packaging/homebrew/scrutate.rb", r"/refs/tags/v([^/]+)\.tar\.gz"),
    ("integrations/gitlab/.gitlab-ci.yml", r'^  SCRUTATE_VERSION: "([^"]+)"'),
    ("integrations/bitbucket/bitbucket-pipelines.yml", r"export V=(\S+)"),
    ("integrations/github/README.md", r"scrutate@v(\S+)"),
    ("integrations/github/README.md", r"version: v(\S+)"),
    ("integrations/pre-commit/README.md", r"rev: v(\S+)"),
    ("CITATION.cff", r'^version: "?([^"\s]+)'),
]
# every place a release writes its date, read against the date of the version's CHANGELOG section
DATED = [
    ("docs/DELIVERY.md", r"^# Delivery index: Scrutate \S+ \((\d{4}-\d\d-\d\d)\)"),
    ("docs/SECURITY.md", r"built from this repository on (\d{4}-\d\d-\d\d)"),
    ("CITATION.cff", r'^date-released: "?(\d{4}-\d\d-\d\d)'),
]
WINGET = "packaging/winget/manifests/c/crisnahine/Scrutate"


def declared() -> dict[str, str]:
    out = {}
    for f in ("crates/scrutate/Cargo.toml", "crates/scrutate-py/Cargo.toml"):
        with open(os.path.join(ROOT, f)) as fh:
            # the first `version = "..."` of a manifest is the package's own
            out[f] = re.search(r'^version = "([^"]+)"', fh.read(), re.M).group(1)
    with open(os.path.join(ROOT, "integrations/vscode/package.json")) as fh:
        out["integrations/vscode/package.json"] = json.load(fh)["version"]
    for f, pattern in WRITTEN:
        with open(os.path.join(ROOT, f)) as fh:
            found = re.findall(pattern, fh.read(), re.M)
        # a pattern that finds nothing means the file changed shape: that fails too
        for i, v in enumerate(found or ["(no version found)"]):
            out[f"{f} ({pattern}, {i + 1})"] = v
    for d in sorted(os.listdir(os.path.join(ROOT, WINGET))):
        out[f"{WINGET}/{d}/"] = d
        for name in sorted(os.listdir(os.path.join(ROOT, WINGET, d))):
            with open(os.path.join(ROOT, WINGET, d, name)) as fh:
                text = fh.read()
            for i, v in enumerate(re.findall(r"^PackageVersion: (\S+)", text, re.M) + re.findall(r"/releases/download/v([0-9][^/]*)/", text)
                                  + re.findall(r"scrutate-([0-9][^-]*)-[a-z0-9_]+-pc-windows-msvc", text) or ["(no version found)"]):
                out[f"{WINGET}/{d}/{name} ({i + 1})"] = v
    return out


def dated(version: str, root: str = ROOT) -> list[str]:
    """What disagrees with the date of the version's CHANGELOG section (none when it has none)."""
    with open(os.path.join(root, "CHANGELOG.md"), encoding="utf-8") as fh:
        m = re.search(rf"^## \[{re.escape(version)}\] - (\d{{4}}-\d\d-\d\d)", fh.read(), re.M)
    if not m:
        return [f"CHANGELOG.md has no dated ## [{version}] section"]
    bad = []
    for f, pattern in DATED:
        with open(os.path.join(root, f), encoding="utf-8") as fh:
            found = re.findall(pattern, fh.read(), re.M) or ["(no date found)"]
        bad += [f"{f} says {d}, CHANGELOG.md's {version} section {m.group(1)}" for d in found if d != m.group(1)]
    return bad


def main() -> int:
    want = sys.argv[1]
    bad = {f: v for f, v in declared().items() if v != want}
    for f, v in bad.items():
        print(f"{f} declares {v}, the tag says {want}")
    dates = dated(want)
    for d in dates:
        print(d)
    if not bad and not dates:
        print(f"every package declares {want}, and every release date agrees")
    return 1 if bad or dates else 0


if __name__ == "__main__":
    sys.exit(main())
