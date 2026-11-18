#!/usr/bin/env python3
"""Write README's "Compared against" table from the result files docs/LEADERBOARD.md names: one row
per tool, with its version, the machines and benchmarks it was measured on, and scrutate against
it. Only the lines between `<!-- compared-against: begin -->` and `<!-- compared-against: end -->`
are written; the rest of the page stays.

usage: make_compared_md.py [--engine-a RESULT.json ...] [--indexed RESULT.json ...] [--out README.md]

With no results named, it reads the ones docs/LEADERBOARD.md's inputs line names, so one run after
make_leaderboard_md.py brings the table up to date.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import sys
from collections import Counter

from indexed import settle
from make_leaderboard_md import MIN_RUNS, VM, hardware, platform_of, published

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, ".."))
BEGIN, END = "<!-- compared-against: begin -->", "<!-- compared-against: end -->"
ORDER = ["GNU grep", "ripgrep", "ack", "ag", "git grep", "ugrep", "ast-grep", "zoekt", "livegrep"]
NAME = {"zoekt": "Zoekt"}
INDEXED = {"zoekt", "livegrep"}
VERDICTS = ["PASS", "FAIL", "PARTIAL", "BLOCKED", "measured"]


def machine(data: dict, path: str) -> str:
    plat = platform_of(data, path)
    return plat + " VM" if hardware(data.get("environment", {})).startswith(VM) else plat


def version(text: str) -> str:
    """The version a tool printed: its number, or for a build from a commit (a Go pseudo-version, a
    bare hash) that commit."""
    text = text or "?"
    if m := re.search(r"v\d+\.\d+\.\d+-(\d{4})(\d\d)(\d\d)\d{6}-([0-9a-f]{12})", text):
        return f"commit {m.group(4)[:7]} ({m.group(1)}-{m.group(2)}-{m.group(3)})"
    if m := re.search(r"\b[0-9a-f]{12,40}\b", text):
        return f"commit {m.group(0)[:7]}"
    m = re.search(r"v?\d+(?:\.\d+)+", text)
    return m.group(0) if m else text


def span(xs: list[float]) -> str:
    lo, hi = f"{min(xs):.2f}", f"{max(xs):.2f}"
    return lo if lo == hi else f"{lo} to {hi}"


def tool_cell(tool: str, versions: dict) -> str:
    """The tool and its version, with the machine beside each version when they differ."""
    seen = list(dict.fromkeys(versions.values()))
    if len(seen) == 1:
        return f"{NAME.get(tool, tool)} {seen[0]}"
    return f"{NAME.get(tool, tool)} " + ", ".join(f"{v} ({m})" for m, v in versions.items())


def shares(rows) -> str:
    xs = [s for s, _ in rows]
    return f"{span(xs)} of its median; lower median on {sum(s < 1 for s in xs)} of {len(xs)}"


def engine_a_row(tool: str, runs) -> str:
    versions, counts, machines, rows, cached, parsed, verdicts, wrong = {}, {}, [], [], [], [], [], 0
    for path, data in runs:
        m = machine(data, path)
        machines.append(m)
        for r in data["benchmarks"].values():
            x, us = r["tools"].get(tool, {}), r["tools"].get("scrutate", {})
            wrong += x.get("status") == "wrong"
            if x.get("status") != "ok" or not x.get("median") or us.get("status") != "ok" or not us.get("median"):
                continue
            versions[m] = version(data["environment"].get("tools", {}).get(tool))
            counts.setdefault(m, Counter())[data["settings"]["cache"]] += 1
            share = (us["median"] / x["median"], r)
            rows.append(share)
            if r.get("qtype") == "structural":
                (parsed if "--no-cache" in us.get("argv", []) else cached).append(share)
            if r.get("best_competitor") == tool:
                verdicts.append(published(r)["status"])
    if not rows:
        return f"| {NAME.get(tool, tool)} | no committed result file | - | - |"
    on = "; ".join(f"{m}: " + ", ".join(f"{n} {c}" for c, n in counts[m].items()) for m in dict.fromkeys(machines) if m in counts)
    on += "".join(f"; not compared on {m}" for m in dict.fromkeys(machines) if m not in counts)
    if tool == "ast-grep":
        against = (shares(cached) + " with scrutate's capture cache filled; " if cached else "") + "parse for parse (`tsquery --no-cache`): " + (
            shares(parsed) if parsed else "no committed result file")
    else:
        against = shares(rows)
    if wrong:
        # the other tool's wrong answers, never scrutate's
        against += f"; {NAME.get(tool, tool)} wrong on {wrong} more, left out"
    if verdicts:
        n = Counter(verdicts)
        ranked = ", ".join(f"{v} {n[v]}" for v in VERDICTS if n[v]) + f" of the {len(verdicts)} where it was the fastest other tool"
    else:
        ranked = "never the fastest other tool, so not ranked"
    return f"| {tool_cell(tool, versions)} | {on} | {against} | {ranked} |"


# the figures of an indexed rival, scrutate's over its: (name, how to read it from a tool's record)
FIGURES = [
    ("index build", lambda t: t["index"]["median"]),
    ("index size", lambda t: t["index"]["bytes"]),
    ("query p50", lambda t: t["latency"]["p50"]),
    ("query memory", lambda t: t["latency"]["rss_median"]),
]


def indexed_row(tool: str, runs) -> str:
    versions, machines, figures, ranked, timed = {}, [], {}, {}, False
    for path, data in runs:
        m = machine(data, path)
        machines.append(m)
        if tool not in data["tools"]:
            continue
        us, x = data["tools"]["scrutate"], data["tools"][tool]
        versions[m] = version(x.get("version"))
        for name, get in FIGURES:
            try:
                figures.setdefault(name, []).append(get(us) / get(x))
            except (KeyError, TypeError, ZeroDivisionError):
                pass
        for k, v in settle(data.get("comparison", {}), data["tools"], data.get("settings")).items():
            # the best rival is the one ahead of the others: beating it beats this one too
            if v["status"] == "PASS" or (v["status"] == "FAIL" and v.get("best") == tool):
                ranked.setdefault(k, Counter())[v["status"]] += 1
            timed |= v["status"] == "measured"
    if not versions:
        return f"| {NAME.get(tool, tool)} | no committed result file | - | - |"
    on = ", ".join(versions) + ": the text index, a process per query" + "".join(f"; not measured on {m}" for m in dict.fromkeys(machines) if m not in versions)
    against = ", ".join(f"{name} {span(xs)}" for name, xs in figures.items() if xs) + f" of {NAME.get(tool, tool)}'s"
    n = len(versions)
    cells = [f"{k} " + ", ".join(f"{v} {c[v]}" for v in VERDICTS if c[v]) + f" of {n}" for k, c in ranked.items()]
    if timed:
        cells.append(f"the timed metrics are measured, not ranked (under {MIN_RUNS} runs)")
    cells.append("kept running as a server: no committed result file")
    return f"| {tool_cell(tool, versions)} | {on} | {against} | {'; '.join(cells)} |"


def table(engine_a: list, indexed: list) -> list[str]:
    out = ["| Tool | Compared on | Scrutate against it | Ranked at 95% |", "|---|---|---|---|"]
    for tool in ORDER:
        out.append(indexed_row(tool, indexed) if tool in INDEXED else engine_a_row(tool, engine_a))
    return out


def leaderboard_inputs() -> list[str]:
    with open(os.path.join(ROOT, "docs", "LEADERBOARD.md"), encoding="utf-8") as f:
        m = re.search(r"^<!-- inputs: (.*) -->$", f.read(), re.M)
    return shlex.split(m.group(1)) if m else []


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine-a", action="append", default=[])
    ap.add_argument("--indexed", action="append", default=[])
    ap.add_argument("--out", default=os.path.join(ROOT, "README.md"))
    a = ap.parse_args()
    if not a.engine_a and not a.indexed:
        a = ap.parse_args(leaderboard_inputs() + ["--out", a.out])
    at = lambda p: p if os.path.isabs(p) else os.path.join(ROOT, p)
    load = lambda paths: [(p, json.load(open(at(p), encoding="utf-8"))) for p in paths]
    named = [f"--engine-a {os.path.relpath(at(p), ROOT)}" for p in a.engine_a] + [f"--indexed {os.path.relpath(at(p), ROOT)}" for p in a.indexed]
    with open(a.out, encoding="utf-8") as f:
        page = f.read()
    if page.count(BEGIN) != 1 or page.count(END) != 1 or page.index(BEGIN) > page.index(END):
        raise SystemExit(f"the page needs one {BEGIN} line and one {END} line after it")
    head, rest = page.split(BEGIN, 1)
    _, tail = rest.split(END, 1)
    body = [f"<!-- inputs: {' '.join(named)} -->", "", *table(load(a.engine_a), load(a.indexed)), ""]
    with open(a.out, "w", encoding="utf-8") as f:
        f.write(head + BEGIN + "\n" + "\n".join(body) + "\n" + END + tail)
    print(f"wrote {a.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
