#!/usr/bin/env python3
"""Write docs/COMPETITIVE.md's yardstick section from bench/yardstick.py result files: per query,
each tool's status and p50 / p95 / p99, and each rival's time against each scrutate form with its
95% interval, with the machine, the load and the versions the files record. Only the lines between
`<!-- yardstick: begin -->` and `<!-- yardstick: end -->` are written; the rest of the page stays.

usage: make_yardstick_md.py [RESULT.json ...] [--out docs/COMPETITIVE.md] [--check]

--check writes nothing and exits 1 when the section differs from what the files give.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import statistics
import sys

from make_leaderboard_md import hardware, machine_line

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, ".."))
BEGIN, END = "<!-- yardstick: begin -->", "<!-- yardstick: end -->"


def ms(x) -> str:
    return "-" if x is None else f"{x:,.0f}" if x >= 100 else f"{x:.1f}" if x >= 10 else f"{x:.2f}"


def ordinal(k: int) -> str:
    """1st, 2nd, 3rd, 4th, ... 11th, 12th, 13th, ... 21st."""
    return f"{k}{'th' if 10 <= k % 100 <= 20 else {1: 'st', 2: 'nd', 3: 'rd'}.get(k % 10, 'th')}"


def section(d: dict, path: str) -> list[str]:
    env, st = d.get("environment", {}), d.get("settings", {})
    out = [f"#### `{os.path.relpath(os.path.abspath(path), ROOT)}`", ""]
    out.append(f"- Machine: {hardware(env)} ({env.get('cpus')} logical CPUs); {env.get('platform', '?')}")
    out += machine_line(env)
    loads = [r["load"] for r in d["rows"] if r.get("load") is not None]
    load = f"; 1-minute load average before each query: median {statistics.median(loads):.1f}, max {max(loads):.1f}" if loads else ""
    wait = f", each query first waiting up to {st['max_wait']:g} s for a load under {st['max_load']:g}" if st.get("max_wait") else ""
    n = st.get("runs")
    ranks = ""
    if isinstance(n, int) and n > 0:
        p95, p99 = (math.ceil(p / 100 * n) for p in (95, 99))
        ranks = f" With {n} runs, p95 is the {ordinal(p95)} fastest and p99 the {ordinal(p99)}" + (", the slowest." if p99 == n else ".")
    out.append(f"- Method: {st.get('runs', '?')} timed runs per tool and query after {st.get('warmup', '?')} warm-up, the tools in turn{wait}{load}. "
               "Every answer is checked before it is timed; a wrong one is never timed." + ranks)
    versions = "; ".join(f"{t}: `{v['version']}`" for t, v in d["tools"].items() if v.get("version"))
    if versions:
        out.append(f"- Versions: {versions}.")
    if env.get("scrutate_sha256"):
        out.append(f"- scrutate binary: SHA-256 `{env['scrutate_sha256']}`.")
    bases = list(dict.fromkeys(b for r in d["rows"] for b in r.get("vs", {})))
    out += ["", "| Query | Tool | Form | Status | p50 / p95 / p99 ms | " + " | ".join(f"vs {b}" for b in bases) + " |",
            "|---|---|---|---|---|" + "---|" * len(bases)]
    for r in d["rows"]:
        times = " / ".join(ms(r.get(k)) for k in ("p50_ms", "p95_ms", "p99_ms")) if r.get("times") else "-"
        vs = [f"{v['ratio']:.2f} [{v['ci_low']:.2f}, {v['ci_high']:.2f}]" if (v := r.get("vs", {}).get(b)) else "-" for b in bases]
        status = r["status"] + (f", timed at load {r['load']:.1f}, over the {st.get('max_load', 0):g} limit" if r.get("over_load") else "")
        out.append(f"| {r['query']} | {r['tool']} | {r['form']} | {status.replace('|', '/')} | {times} | " + " | ".join(vs) + " |")
    if bases:
        out += ["", "A ratio is the rival's median time over the scrutate form's, with its 95% bootstrap interval: under 1 the rival "
                "is faster, and only an interval wholly on one side of 1 shows a difference."]
    for w, s in d.get("summary", {}).items():
        out.append(f"- {w}: " + (f"the closest rival is {s['rival']} ({s['form']}), {s['ratio']:.2f} of scrutate's time (geometric mean)"
                                 if "rival" in s else s["reason"]) + ".")
    return out + [""]


def render(page: str, paths: list[str]) -> str:
    if page.count(BEGIN) != 1 or page.count(END) != 1 or page.index(BEGIN) > page.index(END):
        raise SystemExit(f"the page needs one {BEGIN} line and one {END} line after it")
    head, rest = page.split(BEGIN, 1)
    _, tail = rest.split(END, 1)
    body = [f"<!-- inputs: {' '.join(os.path.relpath(os.path.abspath(p), ROOT) for p in paths)} -->"]
    for p in paths:
        with open(p, encoding="utf-8") as f:
            body += section(json.load(f), p)
    return head + BEGIN + "\n" + "\n".join(body) + "\n" + END + tail


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("results", nargs="*")
    ap.add_argument("--out", default=os.path.join(ROOT, "docs", "COMPETITIVE.md"))
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    with open(a.out, encoding="utf-8") as f:
        page = f.read()
    new = render(page, a.results)
    if a.check:
        if new != page:
            print(f"{a.out}: the yardstick section differs from what bench/make_yardstick_md.py writes; run it again", file=sys.stderr)
            return 1
        return 0
    with open(a.out, "w", encoding="utf-8") as f:
        f.write(new)
    return 0


if __name__ == "__main__":
    sys.exit(main())
