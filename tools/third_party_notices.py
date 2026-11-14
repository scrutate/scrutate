#!/usr/bin/env python3
"""The licence and notice files of every crate compiled into the binary, in one text file.

MIT, BSD and Apache-2.0 all ask that their notices travel with the binary; release archives
include this file next to LICENSE-APACHE and LICENSE-MIT. The crate set is the one
`tools/sbom.py` lists (normal and build dependencies of the package, from the locked
`cargo metadata` graph). A crate that ships no licence file is listed with its declared licence
expression, so the gap is visible. Data files
from outside the project (`data_files`: Unihan and OpenCC, the sources of the Han fold) come first, with their licence.

usage: third_party_notices.py [--out FILE] [--package scrutate]
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

from gen_han_fold import OPENCC, OPENCC_SHA256, OPENCC_URL

HERE = os.path.dirname(os.path.abspath(__file__))
NAMES = ("license", "licence", "copying", "notice", "unlicense", "copyright")
MIT = """Permission is hereby granted, free of charge, to any person obtaining a copy of this software
and associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or
substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES
OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE."""


def ships(dep: dict) -> bool:
    """Whether a `cargo metadata` dependency edge is compiled into the package: normal or build."""
    return any(k["kind"] in (None, "build") for k in dep["dep_kinds"])


def graph(package: str) -> tuple[dict, set[str], str]:
    """The locked `cargo metadata`, the ids compiled into `package` (itself included) and its id.

    The one walk behind this file, `sbom.py` and `gen_dependencies_md.py`: normal and build
    dependencies, never dev dependencies.
    """
    meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1", "--locked"], cwd=os.path.join(HERE, ".."),
                                     capture_output=True, text=True, check=True).stdout)
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    root = next(p["id"] for p in meta["packages"] if p["name"] == package)
    seen, stack = set(), [root]
    while stack:
        nid = stack.pop()
        if nid in seen:
            continue
        seen.add(nid)
        for dep in nodes[nid]["deps"]:
            if ships(dep):
                stack.append(dep["pkg"])
    return meta, seen, root


def shipped(package: str) -> list[dict]:
    meta, seen, root = graph(package)
    by_id = {p["id"]: p for p in meta["packages"]}
    return sorted((by_id[i] for i in seen - {root}), key=lambda p: (p["name"], p["version"]))


# C libraries compiled into the binary whose licence lives only in their source headers
BUNDLED = {"pcre2-sys": ("PCRE2", "upstream/src/pcre2_compile.c")}

UNICODE_V3 = """UNICODE LICENSE V3

COPYRIGHT AND PERMISSION NOTICE

Copyright © 1991-2026 Unicode, Inc.

NOTICE TO USER: Carefully read the following legal agreement. BY
DOWNLOADING, INSTALLING, COPYING OR OTHERWISE USING DATA FILES, AND/OR
SOFTWARE, YOU UNEQUIVOCALLY ACCEPT, AND AGREE TO BE BOUND BY, ALL OF THE
TERMS AND CONDITIONS OF THIS AGREEMENT. IF YOU DO NOT AGREE, DO NOT
DOWNLOAD, INSTALL, COPY, DISTRIBUTE OR USE THE DATA FILES OR SOFTWARE.

Permission is hereby granted, free of charge, to any person obtaining a
copy of data files and any associated documentation (the "Data Files") or
software and any associated documentation (the "Software") to deal in the
Data Files or Software without restriction, including without limitation
the rights to use, copy, modify, merge, publish, distribute, and/or sell
copies of the Data Files or Software, and to permit persons to whom the
Data Files or Software are furnished to do so, provided that either (a)
this copyright and permission notice appear with all copies of the Data
Files or Software, or (b) this copyright and permission notice appear in
associated Documentation.

THE DATA FILES AND SOFTWARE ARE PROVIDED "AS IS", WITHOUT WARRANTY OF ANY
KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF
THIRD PARTY RIGHTS.

IN NO EVENT SHALL THE COPYRIGHT HOLDER OR HOLDERS INCLUDED IN THIS NOTICE
BE LIABLE FOR ANY CLAIM, OR ANY SPECIAL INDIRECT OR CONSEQUENTIAL DAMAGES,
OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS,
WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION,
ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THE DATA
FILES OR SOFTWARE.

Except as contained in this notice, the name of a copyright holder shall
not be used in advertising or otherwise to promote the sale, use or other
dealings in these Data Files or Software without prior written
authorization of the copyright holder."""


def data_files() -> list[dict]:
    """Data from outside the project compiled into the scrutate crate (and so into every package built on it)."""
    path = os.path.join(HERE, "..", "crates", "scrutate", "data", "han_fold.txt")
    with open(path, encoding="utf-8") as f:
        version = f.readline().split()[2]
    fold = "crates/scrutate/data/han_fold.txt"
    return [{"name": "unihan-kSimplifiedVariant", "version": version, "release": f"Unicode {version}", "license": "Unicode-3.0",
             "licence_text": UNICODE_V3, "url": "https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip", "file": fold,
             "purpose": "traditional-to-simplified Han fold of ranked search, from the Unihan field kSimplifiedVariant (tools/gen_han_fold.py)"},
            {"name": "opencc-JPShinjitaiCharacters", "version": OPENCC, "release": f"OpenCC {OPENCC}",
             "license": "Apache-2.0", "sha256": OPENCC_SHA256, "url": OPENCC_URL, "file": fold,
             "licence_text": f"Copyright the OpenCC authors (https://github.com/BYVoid/OpenCC/blob/{OPENCC}/AUTHORS).\n"
                             "Licensed under the Apache License, Version 2.0; the full text is at the end of this file.\n"
                             f"Read as input, unmodified; SHA-256 of the file: {OPENCC_SHA256}.",
             "purpose": "Japanese shinjitai joined to the Han fold of ranked search, from OpenCC's JPShinjitaiCharacters.txt (tools/gen_han_fold.py)"}]


def header_licence(path: str) -> str:
    """The comment of a C source file that holds its licence (copyright lines included)."""
    text = open(path, encoding="utf-8", errors="replace").read()
    at = text.find("Redistribution and use")
    if at < 0:
        return ""
    start = text.rfind("/*", 0, at)
    end = text.find("*/", at)
    return text[start:end + 2]


def licence_files(manifest_path: str) -> list[str]:
    d = os.path.dirname(manifest_path)
    found = []
    for f in sorted(os.listdir(d)):
        full = os.path.join(d, f)
        if os.path.isfile(full) and f.lower().split(".")[0].split("-")[0].split("_")[0] in NAMES:
            found.append(full)
    return found


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=None)
    ap.add_argument("--package", default="scrutate")
    a = ap.parse_args()
    pkgs = shipped(a.package)
    data = data_files()
    out = [f"Third-party software in {a.package}: {len(pkgs)} crates and {len(data)} data sources, with their licence and notice files.",
           "Scrutate's own code is under MIT OR Apache-2.0, at your option (LICENSE-MIT, LICENSE-APACHE); this file",
           "covers only what it compiles in from others.", ""]
    for d in data:
        out.append("=" * 78)
        out.append(f"{d['name']} {d['version']}  ({d['license']}), data compiled in as {d['file']}")
        out.append(d["url"])
        out.append(d["purpose"])
        out.append("-" * 78)
        out.append(d["licence_text"])
        out.append("")
    missing = 0
    for p in pkgs:
        out.append("=" * 78)
        out.append(f"{p['name']} {p['version']}  ({p.get('license') or 'no licence expression'})")
        if p.get("repository"):
            out.append(p["repository"])
        files = licence_files(p["manifest_path"])
        if not files:
            missing += 1
            holders = ", ".join(p.get("authors") or []) or f"the {p['name']} authors (see the repository)"
            out.append(f"The crate ships no licence file. Copyright (c) {holders}. Licensed under {p.get('license') or 'the terms in its repository'};")
            out.append("the standard texts of those licences are at the end of this file.")
        for f in files:
            out.append("-" * 78)
            out.append(os.path.basename(f))
            out.append("")
            out.append(open(f, encoding="utf-8", errors="replace").read().rstrip())
        if p["name"] in BUNDLED:
            lib, rel = BUNDLED[p["name"]]
            src = os.path.join(os.path.dirname(p["manifest_path"]), rel)
            out.append("-" * 78)
            out.append(f"{lib}, the C library this crate compiles in (licence from {rel})")
            out.append("")
            out.append(header_licence(src) if os.path.exists(src) else f"({rel} not found; see the {lib} project)")
        out.append("")
    out.append("=" * 78)
    out.append("Standard licence texts, for the crates above that ship no licence file, and for OpenCC")
    out.append("-" * 78)
    out.append("MIT License")
    out.append("")
    out.append(MIT)
    out.append("-" * 78)
    out.append("Apache License 2.0 (the full text, as in this project's LICENSE-APACHE)")
    out.append("")
    out.append(open(os.path.join(HERE, "..", "LICENSE-APACHE"), encoding="utf-8").read().rstrip())
    text = "\n".join(out) + "\n"
    if a.out:
        with open(a.out, "w") as f:
            f.write(text)
        print(f"wrote {a.out}: {len(pkgs)} crates, {missing} without a licence file, {len(data)} data sources", file=sys.stderr)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
