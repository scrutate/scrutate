#!/usr/bin/env python3
"""Whether each competitor a run recorded is the latest stable release.

The latest releases are pinned in latest.json (what linux_bench.sh installs), so the check needs no
network. With online=True (`SCRUTATE_BENCH_LATEST_ONLINE=1` in the harness) it also asks GitHub for
each tool's latest release and names a pinned entry upstream has passed; without a network that part
is skipped and the result says so.

usage: latest.py [--online] TOOL=VERSION...   (prints the check as JSON)
"""
from __future__ import annotations

import json
import os
import re
import sys
import urllib.request

PINNED = os.path.join(os.path.dirname(os.path.abspath(__file__)), "latest.json")


def load(path: str = PINNED) -> dict:
    return json.load(open(path))


def number(s: str | None) -> str | None:
    """The release number in a tool's --version line (`ripgrep 15.2.0 (rev ..)` -> `15.2.0`)."""
    m = re.search(r"\d+(?:\.\d+)+", s or "")
    return m.group(0) if m else None


def key(v: str) -> tuple[int, ...]:
    return tuple(int(x) for x in v.split("."))


def fetch_latest(repo: str) -> str | None:
    """The number of the latest release GitHub marks for `repo` (not a pre-release)."""
    req = urllib.request.Request(f"https://api.github.com/repos/{repo}/releases/latest", headers={"Accept": "application/vnd.github+json", "User-Agent": "bench"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return number(json.load(r).get("tag_name"))


def check(recorded: dict, pinned: dict | None = None, online: bool = False) -> dict:
    """`recorded` maps a tool to the version line a run recorded. -> the tools older than the pinned
    latest release (`stale`), the ones the list cannot judge (`unchecked`), and with `online` the pinned
    entries upstream has passed (`newer_upstream`)."""
    pinned = pinned or load()
    tools, aliases = pinned["tools"], pinned.get("aliases", {})
    out = {"checked": pinned.get("checked"), "stale": [], "unchecked": [], "online": "not asked"}
    for tool, line in recorded.items():
        name = aliases.get(tool, tool)
        latest, got = tools.get(name, {}).get("latest"), number(line if isinstance(line, str) else (line or {}).get("version"))
        if tool.startswith("scrutate"):
            continue
        if not latest or not got:
            out["unchecked"].append({"tool": name, "recorded": line if isinstance(line, str) else None})
        elif key(got) < key(latest):
            out["stale"].append({"tool": name, "recorded": got, "latest": latest})
    if online:
        newer, failed = [], []
        for name, t in tools.items():
            if not t.get("repo") or not t.get("latest"):
                continue
            try:
                up = fetch_latest(t["repo"])
            except Exception as e:  # the network is optional
                failed.append(f"{name}: {e}")
                continue
            if up and key(up) > key(t["latest"]):
                newer.append({"tool": name, "pinned": t["latest"], "upstream": up})
        out["online"] = "skipped (" + "; ".join(failed) + ")" if failed and not newer else "asked"
        out["newer_upstream"] = newer
    return out


def warn(result: dict) -> None:
    """One stderr line per competitor that is not the latest stable release."""
    for s in result["stale"]:
        print(f"[bench] {s['tool']} {s['recorded']} is not the latest stable release ({s['latest']}, pinned {result['checked']})", file=sys.stderr)
    for s in result.get("newer_upstream", []):
        print(f"[bench] {s['tool']}: upstream released {s['upstream']}, latest.json pins {s['pinned']}", file=sys.stderr)


def run_check(recorded: dict) -> dict:
    """The check as a harness runs it: online when SCRUTATE_BENCH_LATEST_ONLINE=1, warnings on stderr."""
    r = check(recorded, online=os.environ.get("SCRUTATE_BENCH_LATEST_ONLINE") == "1")
    warn(r)
    return r


if __name__ == "__main__":
    args = sys.argv[1:]
    online = "--online" in args
    rec = dict(a.split("=", 1) for a in args if "=" in a)
    r = check(rec, online=online)
    warn(r)
    print(json.dumps(r, indent=1))
