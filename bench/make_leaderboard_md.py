#!/usr/bin/env python3
"""docs/LEADERBOARD.md from the JSON written by bench/leaderboard.py and bench/indexed.py.

usage: make_leaderboard_md.py --engine-a FILE.json [--engine-a FILE2.json] [--indexed FILE.json]... [--out docs/LEADERBOARD.md]

Every number in the page comes from those files; the page states where they came from.
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import statistics
import sys

# The page head (banner and navigation); tools/gen_doc_assets.py --check compares it with its table.
DOC_HEAD = """<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-leaderboard.svg" alt="Leaderboard. Where is scrutate faster or slower, and how sure is that?" width="880">
</p>

# Benchmark leaderboard

[Documentation](README.md) &nbsp;·&nbsp; Next: [Benchmarks](BENCHMARKS.md)

<!-- End of head. -->
"""

def walk_floor(r):
    """The walk-and-read floor of a row, or "-" where walkread read other files than the row searches
    (untracked or ignored files, which it does not leave out)."""
    w = r.get("walkread", {})
    if w.get("files") != r["reference"].get("files_listed"):
        return "-"
    return ms(w.get("median"))


TOOLS = ["scrutate", "ripgrep", "ugrep", "GNU grep", "git grep", "ag", "ack", "ast-grep"]

# what closes each kind of gap; the leaderboard states the measured gap next to it
PLANS = {
    "small": ("below ~20 ms the walk of the directories, on one thread, is the critical path: Scrutate starts 1.4 ms sooner than "
              "ugrep but takes about 1.7 ms longer to walk and match ignore rules (ugrep compiles no glob sets and reads neither "
              "the global git excludes nor .git/info/exclude). Measured next steps: the parallel walker's idle threads sleep 1 ms "
              "between checks for work, and a wait that starts at 10 us took the parallel walk of the ripgrep repository from "
              "8.5 to 6.2 ms; then read each directory's ignore files from its listing and build glob sets lazily"),
    "output": "output-bound: per-file buffers already avoid per-line allocation; next, write straight from the search buffer (no UTF-8 re-validation) and reuse buffers across files",
    "regex": "regex-bound: profile the matcher configuration against ripgrep's (same regex crate) and check literal prefilters",
    "default": "profile this workload (instrumented build) and compare with the leader's strategy before changing code",
}


def ms(x):
    if x is None:
        return "-"
    v = x * 1000
    return f"{v:,.0f}" if v >= 100 else f"{v:.1f}"


def plan_for(r):
    us = r["tools"].get("scrutate", {}).get("median") or 0
    if us < 0.02:
        return PLANS["small"]
    if r["qtype"] in ("very many matches", "whole-word") or (r["reference"]["lines"] > 50_000):
        return PLANS["output"]
    if "regex" in r["qtype"]:
        return PLANS["regex"]
    return PLANS["default"]


STRUCTURAL_CACHED = ("scrutate's `tsquery` ran with its capture cache (`.scrutate/tsquery/`) filled by earlier runs, so its timed "
                     "runs read and hashed every file and parsed none, while ast-grep, with no cache, parses every file each run. "
                     "This measurement has no parse-for-parse run (`--no-cache`).")

STOP_NOTE = ("- Stop rule (`leaderboard.py`, on by default; the Haybittle-Peto boundary: Haybittle 1971, Peto et al. 1976): a row whose wall-time verdict and four CPU "
             "and memory verdicts are all PASS, FAIL or n/a at alpha 0.001 after its second block, with no tool's block medians 25% "
             "apart, stops there with 20 runs and shows its 99.9% interval. Every other row takes all its runs and is judged at "
             "alpha 0.049, a 95.1% interval. The alphas sum to 0.05 (Bonferroni), so either interval clearing 1 meets the 95% claim "
             "above, whatever the load did between the blocks.")

# method notes the page states once, above the sections that use them
LEGEND = ("- Engine A tables: medians in ms (runs in parentheses when fewer than the full count); `wrong` = answer differs from "
          "the reference (disqualified), `n/a` = the tool cannot express the query or filter, `timeout` = did not finish. The "
          "`scrutate --index` column (Engine B's text index, another category) is published but never ranked.")
CPU_TESTED = ("- CPU time (user + system seconds) and peak memory (resident set) come from the same runs as the wall time and are "
              "tested like it: the 95% interval of median(scrutate) / median(the competitor lowest on that metric), bootstrapped over "
              "the runs, gives PASS below 1, FAIL above 1, PARTIAL when it contains 1 (no BLOCKED: no hardware ceiling). The "
              "competitors lowest on CPU time and on memory in the pilot get full runs too, whatever their wall time. Rank is the "
              "place among tools with a correct answer, lowest first, 3-run pilots included. The default mode (`scrutate grep` with "
              "its default thread count) is the product default because it leads wall time; `scrutate grep -j1` (one search "
              "thread) is the mode for least CPU time and memory: checked against the reference like any tool, timed in the same "
              "blocks, never ranked for wall time.")
CPU_UNTESTED = ("- CPU time and peak memory tables without a status give medians over the same runs (user + system CPU seconds, "
                "peak resident set in MB); rank is scrutate's place among tools with a correct answer, lowest first. These rows "
                "predate significance tests for CPU time and memory.")

METRICS = {"cpu": "CPU time", "rss": "peak memory"}
MODES = ["default", "-j1"]


def count_line(statuses):
    counts = {}
    for s in statuses:
        counts[s] = counts.get(s, 0) + 1
    return ", ".join(f"**{k}** {v}" for k, v in sorted(counts.items())) + f" (of {len(statuses)})"


def metric_value(key, v):
    return "-" if v is None else f"{ms(v)} ms" if key == "cpu" else f"{v / 1e6:.1f} MB"


def level(r):
    """The interval's level where the row's own alpha is not 0.05 (a row judged by the stop rule)."""
    alpha = r.get("alpha", 0.05)
    return "" if alpha == 0.05 else f" ({(1 - alpha) * 100:g}%)"


def metric_cell(key, m, lvl=""):
    """One mode's verdict on one metric: median, ratio and interval, rank, status (with the gap on FAIL)."""
    if m["status"] == "n/a":
        return "n/a"
    parts = [metric_value(key, m.get("median"))] if m.get("median") is not None else []
    if "ratio" in m:
        parts.append(f"{m['ratio']:.3f} [{m['ci95'][0]:.3f}, {m['ci95'][1]:.3f}]{lvl}")
    if m.get("rank"):
        parts.append(f"rank {m['rank']} of {m['of']}")
    st = m["status"]
    if st == "FAIL" and "ratio" in m:
        st += f" ({(m['ratio'] - 1) * 100:.0f}% more)"
    elif m.get("why"):
        st += f" ({m['why']})"
    return ", ".join(parts + [f"**{st}**" if m["status"] == "PASS" else st])


def hardware(env):
    """The CPU as recorded; a Linux guest of the Apple Virtualization framework reports no name."""
    cpu = env.get("cpu", "?")
    if env.get("platform", "").startswith("Linux") and (not cpu or cpu.startswith("0x")):
        if env.get("cpus", 0) <= 5:
            # one virtual CPU per performance core of the host: none has to run on an efficiency core
            return f"{VM}, one virtual CPU per performance core"
        return f"{VM}, its virtual CPUs on performance or efficiency cores"
    return cpu


VM = "a Linux virtual machine on the Apple M3 Pro above"
VM_NOTE = ("- The Linux virtual machine is Colima (Apple Virtualization); macOS schedules its virtual CPUs, so its runs vary "
           "more than the host's.")


def platform_of(data, path):
    """The machine a results file was measured on, as its directory was named when it was written
    (`macos-arm64`, `linux-aarch64`): from its environment record, so a file moved under
    superseded/ keeps its label."""
    env = data.get("environment", {})
    system, machine = env.get("platform", "").split("-")[0].lower(), env.get("machine", "")
    return f"{system}-{machine}" if system and machine else os.path.basename(os.path.dirname(os.path.abspath(path)))


def engine_a(data, label):
    env = data["environment"]
    out = []
    out.append(f"## Engine A and structural search: {label}\n")
    mem = env.get("memory_bytes", 0) / 2**30
    out.append(f"- Hardware: {hardware(env)} ({env.get('cpus')} logical CPUs"
               + (f"; {env.get('perf_cores')} performance + {env.get('efficiency_cores')} efficiency cores" if env.get("perf_cores") else "")
               + f"), {mem:.0f} GB RAM; {env['platform']}")
    st = data["settings"]
    stop = st.get("stop")
    runs = (f"{st['runs']} timed runs per tool in {st.get('blocks', 1)} interleaved blocks" if not stop else
            f"up to {st['runs']} timed runs per tool in up to {st['blocks']} interleaved blocks (a row stops after block "
            f"{' or '.join(map(str, stop['after_blocks']))} once every verdict holds at alpha {stop['interim_alpha']:g}, else is judged "
            f"at alpha {stop['last_alpha']:g}: see the stop rule above)")
    out.append(f"- Method: hyperfine (`{env.get('hyperfine', '?')}`), output piped, {runs}"
               f" ({st['warmup']} warm-up run each), {st['cache']} cache"
               + (f" (prepare: `{st['cold_prepare']}`)" if st.get("cold_prepare") else "")
               + f"; each block first waits (bounded) for a 1-minute load average below {st['max_load']:.0f}.")
    loads = [r.get("max_load", 0) for r in data["benchmarks"].values()]
    loads = [x for r in data["benchmarks"].values() for x in r.get("load", [])] or loads
    out.append(f"- 1-minute load average as each block started: median {statistics.median(loads):.1f}, max {max(loads):.1f} on "
               f"{env.get('cpus')} logical CPUs.")
    out.append("- Tool versions: " + "; ".join(f"{t}: `{v}`" for t, v in env["tools"].items() if v and t not in ("scrutate --index", "scrutate base", "scrutate -j1")) + ".")
    if env.get("scrutate_sha256"):
        out.append(f"- scrutate binary measured: SHA-256 `{env['scrutate_sha256']}`.")
    out.append("")
    out.append("Status counts, wall time: " + count_line([r["status"] for r in data["benchmarks"].values()]) + ".\n")
    if any("metrics" in r for r in data["benchmarks"].values()):
        for key, name in METRICS.items():
            out.append(f"Status counts, {name}: " + "; ".join(
                f"{mode} mode " + count_line([r["metrics"][key][mode]["status"] for r in data["benchmarks"].values() if "metrics" in r]) for mode in MODES) + ".\n")
    # results from before bench/walkread.c have no floor to show
    walk = any("walkread" in r for r in data["benchmarks"].values())
    head = "| Benchmark | " + " | ".join(TOOLS) + " | scrutate --index | read files" + (" | walk and read" if walk else "") + " | ratio vs best (95% CI) | status |"
    out.append(head)
    out.append("|" + "---|" * (len(TOOLS) + 5 + walk))
    for bid, r in data["benchmarks"].items():
        cells = []
        for t in TOOLS:
            x = r["tools"].get(t)
            if x is None:
                cells.append("")
            elif x.get("status") != "ok":
                cells.append(x.get("status", "?"))
            else:
                n = x.get("n", 0)
                v = ms(x.get("median"))
                cells.append(f"**{v}**" if t == r.get("best_competitor") or (t == "scrutate" and r["status"] == "PASS") else v)
                if n and n < r.get("runs", data["settings"]["runs"]):
                    cells[-1] += f" ({n})"
        xi = r["tools"].get("scrutate --index", {})
        idx = ms(xi.get("median")) if xi.get("status") == "ok" else xi.get("status", "")
        ratio = f"{r['ratio']:.3f} [{r['ci95'][0]:.3f}, {r['ci95'][1]:.3f}]{level(r)} vs {r['best_competitor']}" if "ratio" in r else r.get("why", "")
        cpus = data["environment"].get("cpus") or 1
        flag = f" (load {r.get('max_load', 0):.0f})" if r.get("max_load", 0) > cpus else ""
        first = r.get("first")
        if first:
            flag += f" (measured again; first: {first['status']}" + (f" {first['ratio']:.3f}" if first.get("ratio") else "") + ")"
        if r.get("resident") is not None and r["resident"] < 0.95:
            flag += f" ({r['resident']:.0%} of the files in the page cache)"
        floor = f" | {walk_floor(r)}" if walk else ""
        out.append(f"| {bid} ({r['reference']['lines']:,} lines) | " + " | ".join(cells) + f" | {idx} | {ms(r.get('rawread', {}).get('median'))}{floor} | {ratio} | {r['status']}{flag} |")
    out.append("")
    for bid, r in data["benchmarks"].items():
        n = r["reference"].get("files_not_compared")
        if n:
            out.append(f"- **{bid}**: every tool searches every file, but {n:,} of the {r['reference']['files_searched']:,} files are not "
                       "compared: the reference's own parse of them has errors, and inside an error the node a tool reports depends on "
                       "tree-sitter's error recovery, which differs between versions.")
    cached = [bid for bid, r in data["benchmarks"].items()
              if r.get("qtype") == "structural" and "--no-cache" not in r.get("tools", {}).get("scrutate", {}).get("argv", ["--no-cache"])]
    if cached:
        out.append(f"- {', '.join(f'**{b}**' for b in cached)}: {STRUCTURAL_CACHED}")
    out.append("")
    fails = [(bid, r) for bid, r in data["benchmarks"].items() if r["status"] == "FAIL"]
    if fails:
        out.append("### Where a competitor is ahead (FAIL), with the measured gap and the plan\n")
        for bid, r in fails:
            if "ratio" in r:
                out.append(f"- **{bid}**: scrutate {ms(r['tools']['scrutate']['median'])} ms vs {r['best_competitor']} "
                           f"{ms(r['tools'][r['best_competitor']]['median'])} ms ({(r['ratio'] - 1) * 100:.0f}% slower, CI {r['ci95'][0]:.3f} to {r['ci95'][1]:.3f}). Plan: {plan_for(r)}.")
            else:
                out.append(f"- **{bid}**: {r.get('why', '')}.")
        out.append("")
    out.append("### CPU time and peak memory\n")
    tested = [(bid, r) for bid, r in data["benchmarks"].items() if "metrics" in r]
    if tested:
        out.append("| Benchmark | CPU, default | CPU, -j1 | memory, default | memory, -j1 | lowest CPU competitor | lowest memory competitor |")
        out.append("|---|---|---|---|---|---|---|")
        for bid, r in tested:
            m = r["metrics"]
            cells = [metric_cell(key, m[key][mode], level(r)) for key in METRICS for mode in MODES]
            for key in METRICS:
                lead = m[key]["default"].get("best_competitor") or m[key]["-j1"].get("best_competitor")
                med = r["tools"].get(lead, {}).get({"cpu": "cpu_median", "rss": "rss_median"}[key]) if lead else None
                cells.append(f"{lead} {metric_value(key, med)}" if lead else "-")
            out.append(f"| {bid} | " + " | ".join(cells) + " |")
        out.append("")
    untested = [(bid, r) for bid, r in data["benchmarks"].items() if "metrics" not in r]
    if untested:
        # rows from the harness before the metrics had a status
        out.append("| Benchmark | scrutate CPU s | lowest competitor CPU s | CPU rank | scrutate MB | lowest competitor MB | memory rank |")
        out.append("|---|---|---|---|---|---|---|")
    for bid, r in untested:
        ok = {t: x for t, x in r["tools"].items() if x.get("status") == "ok" and x.get("cpu_median") is not None and t not in ("scrutate --index", "scrutate base")}
        us = ok.get("scrutate")
        if not us:
            continue
        cells = []
        for key, scale, fmt in (("cpu_median", 1, "{:.2f}"), ("rss_median", 1e6, "{:.0f}")):
            comp = {t: x[key] for t, x in ok.items() if t != "scrutate"}
            best = min(comp, key=comp.get) if comp else None
            rank = 1 + sum(v < us[key] for v in comp.values())
            cells += [fmt.format(us[key] / scale), f"{fmt.format(comp[best] / scale)} ({best})" if best else "-", f"{rank} of {len(ok)}"]
        out.append(f"| {bid} | " + " | ".join(cells) + " |")
    out.append("")
    wrong = [(bid, t, x) for bid, r in data["benchmarks"].items() for t, x in r["tools"].items() if x.get("status") == "wrong"]
    if wrong:
        out.append("### Disqualified answers\n")
        out.append("| Benchmark | Tool | missing | extra | example |\n|---|---|---|---|---|")
        for bid, t, x in wrong:
            ex = (x.get("missing_sample") or x.get("extra_sample") or [["", ""]])[0]
            out.append(f"| {bid} | {t} | {x['missing']} | {x['extra']} | `{ex[0]}:{ex[1]}` |")
        out.append("")
    out.append("### Exact command lines\n")
    out.append("<details><summary>every tool, every benchmark (run from the corpus root)</summary>\n")
    for bid, r in data["benchmarks"].items():
        out.append(f"**{bid}**" + (f" (each command line started {r['concurrent']} times at once by `/bin/sh -c '... & ... & wait'`)" if r.get("concurrent") else "") + "\n")
        out.append("```")
        for t, x in r["tools"].items():
            if x.get("argv"):
                env = " ".join(f"{k}={v}" for k, v in (x.get("env") or {}).items())
                argv = [os.path.basename(x["argv"][0])] + x["argv"][1:]
                out.append(f"{t:16} {env + ' ' if env else ''}{' '.join(shlex.quote(a) for a in argv)}")
        out.append("```\n")
    out.append("</details>\n")
    return out


def indexed(data, label):
    others = [t for t in data["tools"] if t != "scrutate"]
    out = [f"## Engine B: indexed search against {' and '.join(others)}: {label}\n"]
    env = data["environment"]
    out.append(f"- Hardware: {hardware(env)}, {env.get('cpus')} logical CPUs; {env['platform']}. Corpus: `{os.path.basename(data['corpus'])}`.")
    st = data["settings"]
    out.append(f"- Method: {st['index_runs']} full index builds and {st['index_runs']} incremental updates (100 files edited) per tool; "
               f"{st['runs']} runs of each of {st['queries']} queries, round-robin across tools, each in a fresh process; "
               f"{st['concurrency']} concurrent clients for {st['seconds']:.0f} s, three times, for throughput.")
    out.append("- Versions: " + "; ".join(f"{t}: `{v['version']}`" for t, v in data["tools"].items()) + ".\n")
    out.append("| Metric | " + " | ".join(data["tools"]) + " |")
    out.append("|---|" + "---|" * len(data["tools"]))
    rows = [
        ("full indexing time (median, s)", lambda t: f"{t['index']['median']:.2f}"),
        ("index size (MB)", lambda t: f"{t['index']['bytes'] / 1e6:,.0f}"),
        ("indexing peak memory (MB)", lambda t: f"{t['index']['rss_max'] / 1e6:,.0f}"),
        ("update after 100 edits (median, s)", lambda t: f"{t['update']['median']:.2f} ({t['update']['mode'].split(' (')[0]})"),
        ("query latency p50 (ms)", lambda t: ms(t["latency"]["p50"])),
        ("query latency p95 (ms)", lambda t: ms(t["latency"]["p95"])),
        ("query latency p99 (ms)", lambda t: ms(t["latency"]["p99"])),
        ("memory to answer a query (median MB; a server's resident memory included)", lambda t: f"{t['latency']['rss_median'] / 1e6:.0f}"),
        ("concurrent throughput (queries/s)", lambda t: f"{t['throughput']['median']:.0f}"),
        ("queries answered differently from the reference", lambda t: str(len(t["latency"]["wrong_queries"]))),
    ]
    for name, f in rows:
        cells = []
        for t in data["tools"].values():
            try:
                cells.append(f(t))
            except (KeyError, TypeError):
                cells.append("-")
        out.append(f"| {name} | " + " | ".join(cells) + " |")
    out.append("")
    if "comparison" in data:
        out.append("| Comparison (scrutate / best other) | ratio | 95% CI | status |\n|---|---|---|---|")
        for k, v in data["comparison"].items():
            ci = f"{v['ci95'][0]:.3f} to {v['ci95'][1]:.3f}" if "ci95" in v else "(deterministic)"
            out.append(f"| {k} (vs {v['best']}) | {v['ratio']:.3f} | {ci} | {v['status']} |")
        out.append("")
    wrong = [(e["q"]["pats"], t, x) for e in data.get("queries", []) for t, x in e["tools"].items() if not x["valid"]]
    if wrong:
        out.append("Queries a tool answered differently (excluded from that tool's latency):\n")
        for pats, t, x in wrong:
            ex = (x.get("missing_sample") or x.get("extra_sample") or [["", ""]])[0]
            out.append(f"- {t}: `{' | '.join(pats)}`: {x['missing']} missing, {x['extra']} extra (e.g. `{ex[0]}:{ex[1]}`)")
        out.append("")
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine-a", action="append", default=[])
    ap.add_argument("--indexed", action="append", default=[])
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "docs", "LEADERBOARD.md"))
    a = ap.parse_args()
    datas = [json.load(open(p)) for p in a.engine_a]
    fewest = min((r.get("runs", d["settings"]["runs"]) for d in datas for r in d["benchmarks"].values()), default=30)
    root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
    named = [f"--engine-a {os.path.relpath(os.path.abspath(p), root)}" for p in a.engine_a] + [f"--indexed {os.path.relpath(os.path.abspath(p), root)}" for p in a.indexed]
    # what tools/gen_doc_assets.py --check writes the page from again, to compare
    lines = [DOC_HEAD, f"<!-- inputs: {' '.join(named)} -->",
             "Generated by `bench/make_leaderboard_md.py` from the raw results in `bench/results/`; nothing is typed by hand. "
             "A benchmark is **PASS** only when the 95% confidence interval of median(scrutate) / median(best competitor), bootstrapped over at "
             f"least {fewest} runs of each, lies wholly below 1; **FAIL** when above 1; **PARTIAL** when it contains 1; **BLOCKED** for a tie "
             "at the hardware limit (both within 15% of the time to read the same files). \"Fastest\" claims apply only to PASS rows.", ""]
    if any(d["settings"].get("stop") for d in datas):
        lines.append(STOP_NOTE)
    lines += [
        "- Every machine was shared with other work, so no result is a publishable #1 claim (section 14 requires dedicated idle "
        "hardware); a row whose load exceeded the CPU count shows it in the status column.",
        "- The read-files ceiling (`bench/rawread.c`) is measured once per file set, before its first benchmark, as the fastest of several "
        "thread counts: one per CPU and half that with a warm cache, up to 96 with a cold one (a disk takes more reads at once than "
        "there are cores). A row measured later under heavier load can read PARTIAL where a quieter run reads BLOCKED; PARTIAL is "
        "the weaker claim."]
    if any("walkread" in r for d in datas for r in d["benchmarks"].values()):
        lines.append(
            "- The walk-and-read floor (`bench/walkread.c`) lists the tree and reads every file in it, skipping hidden entries and "
            "reading no ignore rule, with the same thread counts: the least a search that has to find its files does, where the "
            "read-files ceiling is handed the list. It is measured once per corpus and cache state, for the rows that search every "
            "non-hidden file, and shows only where it read as many files as the row lists: it reads no ignore rule, so a checkout with "
            "untracked or ignored files gives it more than the tools search.")
    lines += [
        "- A benchmark where some tool's block medians differed by more than 25% was disturbed by other work and measured again once, "
        "whatever its status (`leaderboard.py --redo-unstable`); its status column also gives the first measurement, and the second "
        "reflects the machine as it was by then."]
    if any(r.get("resident") is not None for d in datas for r in d["benchmarks"].values()):
        lines.append("- A warm-cache benchmark records how much of its corpus was in the page cache at its end (`bench/evict.c --resident`); "
                     "a row under 95% says so in its status column.")
    indexed_datas = [json.load(open(p)) for p in a.indexed]
    if any(hardware(d["environment"]).startswith(VM) for d in datas + indexed_datas):
        lines.append(VM_NOTE)
    rows = [r for d in datas for r in d["benchmarks"].values()]
    if rows:
        lines.append(LEGEND)
    if any("metrics" in r for r in rows):
        lines.append(CPU_TESTED)
    if any("metrics" not in r for r in rows):
        lines.append(CPU_UNTESTED)
    lines.append("")
    for p, d in zip(a.engine_a, datas):
        lines += engine_a(d, f"{platform_of(d, p)}, {d['settings']['cache']} cache")
    for p, d in zip(a.indexed, indexed_datas):
        lines += indexed(d, platform_of(d, p))
    with open(a.out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {a.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
