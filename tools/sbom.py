#!/usr/bin/env python3
"""Software bill of materials (CycloneDX 1.5 JSON) from the resolved Cargo dependency graph.

Uses `cargo metadata` (the authoritative, locked graph) so the SBOM lists exactly what the binary
was built from: name, version, license expression, source registry and purl for every crate, and
one `data` component per data file from outside the project (`third_party_notices.data_files`).
The timestamp is SOURCE_DATE_EPOCH when that is set, else now; the serial number is new each run.

usage: sbom.py [--out FILE] [--package scrutate]
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
import uuid

from third_party_notices import data_files, graph, ships

HERE = os.path.dirname(os.path.abspath(__file__))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=None)
    ap.add_argument("--package", default="scrutate")
    a = ap.parse_args()
    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    if epoch and not epoch.isdigit():
        sys.exit(f"SOURCE_DATE_EPOCH must be whole seconds since 1970, got {epoch!r}")
    meta, seen, root_id = graph(a.package)
    by_id = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    comps = []
    for nid in sorted(seen - {root_id}, key=lambda i: (by_id[i]["name"], by_id[i]["version"])):
        p = by_id[nid]
        comps.append({
            "type": "library",
            "bom-ref": f"pkg:cargo/{p['name']}@{p['version']}",
            "name": p["name"],
            "version": p["version"],
            "purl": f"pkg:cargo/{p['name']}@{p['version']}",
            "licenses": [{"expression": p["license"]}] if p.get("license") else [],
            "externalReferences": [{"type": "distribution", "url": p["source"] or "path"}] + ([{"type": "website", "url": p["repository"]}] if p.get("repository") else []),
        })
        if not p["source"]:
            # a crate vendored under vendor/ with local changes, listed in its README
            rel = os.path.relpath(os.path.dirname(p["manifest_path"]), os.path.join(HERE, ".."))
            comps[-1]["pedigree"] = {"notes": f"modified copy of an upstream crates.io release, published as {p['name']}; source and changes in {rel}"}
            comps[-1]["externalReferences"][0]["url"] = rel
    for d in data_files():
        comps.append({
            "type": "data",
            "bom-ref": f"data:{d['name']}@{d['version']}",
            "name": d["name"],
            "version": d["version"],
            "description": d["purpose"],
            "licenses": [{"expression": d["license"]}],
            "externalReferences": [{"type": "distribution", "url": d["url"]}],
            "pedigree": {"notes": f"derived file, compiled in from {d['file']}"},
        })
        if d.get("sha256"):
            comps[-1]["externalReferences"][0]["hashes"] = [{"alg": "SHA-256", "content": d["sha256"]}]
    root = by_id[root_id]
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid4()}",
        "version": 1,
        "metadata": {
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(int(epoch) if epoch else None)),
            "tools": [{"name": "scrutate/tools/sbom.py", "version": root["version"]}],
            "component": {"type": "application", "bom-ref": f"pkg:cargo/{root['name']}@{root['version']}", "name": root["name"], "version": root["version"],
                          "licenses": [{"expression": root["license"]}] if root.get("license") else []},
        },
        "components": comps,
        "dependencies": [
            {"ref": f"pkg:cargo/{by_id[nid]['name']}@{by_id[nid]['version']}",
             "dependsOn": sorted({f"pkg:cargo/{by_id[d['pkg']]['name']}@{by_id[d['pkg']]['version']}" for d in nodes[nid]["deps"] if d["pkg"] in seen and ships(d)})}
            for nid in sorted(seen, key=lambda i: (by_id[i]["name"], by_id[i]["version"]))
        ],
    }
    text = json.dumps(bom, indent=1)
    if a.out:
        with open(a.out, "w") as f:
            f.write(text)
        print(f"{len(comps)} components -> {a.out}", file=sys.stderr)
    else:
        print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
