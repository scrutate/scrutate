#!/usr/bin/env python3
"""docs/dashboard.html: one self-contained page (inline CSS and SVG) from the JSON in bench/results/.

usage: make_dashboard.py [--results bench/results] [--out docs/dashboard.html]

For each benchmark it plots scrutate's ratio to the best competitor, with the 95% interval and the
status, for every run in time order, and counts the statuses per run. Runs under `superseded/` come
first, in the order of HISTORY; the current files follow.
"""
from __future__ import annotations

import argparse
import glob
import html
import json
import math
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tools"))
import gen_assets as G  # noqa: E402  (the design tokens)
from indexed import settle  # noqa: E402  (the indexed rows as LEADERBOARD.md publishes them)

# the runs carry no timestamp; this is the order they were measured in (superseded/README.md)
HISTORY = ["leaderboard-warm-prelim", "leaderboard-warm-blocks", "leaderboard-warm-prevbin", "leaderboard-warm-eeab",
           "leaderboard-warm-a82a", "leaderboard-warm-9da7-partial", "indexed-eeab", "leaderboard-warm-fb5c", "leaderboard-cold-fb5c",
           "indexed-fb5c", "leaderboard-warm-fb3b", "leaderboard-cold-fb3b", "indexed-fb3b", "leaderboard-warm-6602",
           "leaderboard-cold-6602", "indexed-6602", "leaderboard-warm-b57d", "leaderboard-cold-b57d", "indexed-b57d",
           "leaderboard-warm-558e", "leaderboard-cold-558e", "indexed-558e", "leaderboard-warm-e49f", "leaderboard-cold-e49f",
           "indexed-e49f", "leaderboard-warm-587a", "leaderboard-cold-587a-partial", "leaderboard-warm-261e", "leaderboard-cold-261e",
           "indexed-261e", "leaderboard-warm-9aa0", "leaderboard-cold-9aa0", "indexed-9aa0"]
STATUSES = ["PASS", "PARTIAL", "BLOCKED", "FAIL", "n/a"]
MARK = {"PASS": "P", "PARTIAL": "~", "BLOCKED": "B", "FAIL": "F", "n/a": "-"}


def css(status):
    """The class of a status: `n/a` is `na`."""
    return status.lower().replace("/", "")


def load_runs(results):
    """Runs grouped by (platform, kind), each group in time order."""
    paths = glob.glob(os.path.join(results, "*", "leaderboard-*.json")) + glob.glob(os.path.join(results, "*", "indexed*.json"))
    paths += glob.glob(os.path.join(results, "*", "superseded", "leaderboard-*.json"))
    paths += glob.glob(os.path.join(results, "*", "superseded", "indexed*.json"))
    groups = {}
    for p in paths:
        name = os.path.splitext(os.path.basename(p))[0]
        rel = os.path.relpath(p, results)
        platform = rel.split(os.sep)[0]
        old = "superseded" in rel.split(os.sep)
        try:
            d = json.load(open(p))
        except (OSError, ValueError) as e:
            print(f"skipping {p}: {e}", file=sys.stderr)
            continue
        env = d.get("environment", {})
        if name.startswith("indexed"):
            kind = "indexed"
            items = settle(d.get("comparison", {}), d.get("tools", {}), d.get("settings"))
        else:
            kind = f"leaderboard, {d.get('settings', {}).get('cache', '?')} cache"
            items = d.get("benchmarks", {})
        if not items:
            continue
        order = (0 if old else 1, HISTORY.index(name) if name in HISTORY else len(HISTORY), name)
        groups.setdefault((platform, kind), []).append({
            "order": order, "name": name, "path": rel, "old": old, "items": items,
            "sha": env.get("scrutate_sha256"), "date": env.get("date"),
        })
    for runs in groups.values():
        runs.sort(key=lambda r: r["order"])
    return dict(sorted(groups.items()))


def norm(item):
    """(ratio, lo, hi, status, best) for a leaderboard benchmark or an indexed comparison."""
    ci = item.get("ci95") or [None, None]
    return item.get("ratio"), ci[0], ci[1], item.get("status", "?"), item.get("best_competitor") or item.get("best"), item.get("why")


def fmt(x):
    return "-" if x is None else f"{x:.3f}" if x >= 0.01 else f"{x:.2g}"


def strip_chart(cells, lo_d, hi_d, labels):
    """Small SVG: x = run, y = ratio on a log scale, with a line at 1."""
    n = len(cells)
    w, h, pad = 24 + 44 * n, 64, 10
    step = (w - 2 * pad) / max(n, 1)
    y = lambda v: pad + (h - 2 * pad) * (math.log(hi_d) - math.log(min(max(v, lo_d), hi_d))) / (math.log(hi_d) - math.log(lo_d))
    out = [f'<svg viewBox="0 0 {w} {h}" class="strip" role="img" aria-label="ratio per run">',
           f'<line x1="{pad}" x2="{w - pad}" y1="{y(1):.1f}" y2="{y(1):.1f}" class="one"/>',
           f'<text x="0" y="{y(1) + 4:.1f}" class="axis">1</text>']
    for i, c in enumerate(cells):
        cx = pad + step * (i + 0.5)
        if c is None:
            out.append(f'<text x="{cx:.1f}" y="{h - 2}" class="miss">.</text>')
            continue
        r, lo, hi, st, best, why = c
        tip = html.escape(f"{labels[i]}: " + (f"{fmt(r)} [{fmt(lo)}, {fmt(hi)}] vs {best}, {st}" if r is not None else f"{st}: {why or 'no ratio'}"))
        cls = css(st) if st in STATUSES else "other"
        out.append(f'<g class="{cls}"><title>{tip}</title>')
        out.append(f'<rect x="{cx - step / 2:.1f}" y="0" width="{step:.1f}" height="{h}" class="hit"/>')
        if r is None:
            out.append(f'<text x="{cx:.1f}" y="{h / 2 + 4:.1f}" class="glyph">x</text></g>')
            continue
        if lo is not None and hi is not None:
            out.append(f'<line x1="{cx:.1f}" x2="{cx:.1f}" y1="{y(hi):.1f}" y2="{y(lo):.1f}" class="ci"/>')
        out.append(f'<circle cx="{cx:.1f}" cy="{y(r):.1f}" r="4" class="dot"/></g>')
    out.append("</svg>")
    return "".join(out)


def section(platform, kind, runs):
    labels = [r["name"] + (f" ({r['sha'][:8]})" if r["sha"] else "") for r in runs]
    keys = []
    for r in runs:
        keys += [k for k in r["items"] if k not in keys]
    vals = [v for r in runs for it in r["items"].values() for v in norm(it)[:3] if isinstance(v, (int, float)) and v > 0]
    lo_d, hi_d = min(vals + [1.0]) / 1.15, max(vals + [1.0]) * 1.15
    o = [f'<section><h2>{html.escape(platform)}: {html.escape(kind)}</h2>']
    o.append('<ol class="runs">' + "".join(
        f'<li><code>{html.escape(r["path"])}</code> binary {("<code>" + r["sha"][:12] + "</code>") if r["sha"] else "not recorded"}'
        + (f', {html.escape(r["date"])}' if r["date"] else "") + "</li>" for r in runs) + "</ol>")
    o.append("<h3>Status counts per run</h3><div class='counts'>")
    for r, lab in zip(runs, labels):
        c = {s: 0 for s in STATUSES}
        for it in r["items"].values():
            st = norm(it)[3]
            c[st if st in c else "FAIL"] += 1
        total = sum(c.values())
        bar = "".join(f'<span class="seg {css(s)}" style="flex:{v}" title="{s} {v}"></span>' for s, v in c.items() if v)
        text = ", ".join(f"{s} {v}" for s, v in c.items() if v)
        o.append(f'<div class="crow"><div class="clab">{html.escape(lab)}</div><div class="bar">{bar}</div>'
                 f'<div class="ctext">{text} (of {total})</div></div>')
    o.append("</div>")
    o.append(f"<h3>Ratio to the best competitor, run by run</h3><p class='note'>Log scale from {fmt(lo_d)} to {fmt(hi_d)}; "
             "below the line at 1 scrutate is faster (or smaller). Vertical bar: 95% interval. A dot means no run measured it; "
             "x means the run has a status but no ratio. Hover a mark for its numbers.</p><div class='bench'>")
    for k in keys:
        cells = [norm(r["items"][k]) if k in r["items"] else None for r in runs]
        last = next(c for c in reversed(cells) if c)
        st = last[3]
        o.append(f'<div class="brow"><div class="bname">{html.escape(k)}</div>{strip_chart(cells, lo_d, hi_d, labels)}'
                 f'<div class="blast"><span class="tag {css(st)}">{MARK.get(st, "?")}</span> {st} {fmt(last[0])}</div></div>')
    o.append("</div><details><summary>Table view</summary><div class='scroll'><table><thead><tr><th>Benchmark</th>"
             + "".join(f"<th>{html.escape(l)}</th>" for l in labels) + "</tr></thead><tbody>")
    for k in keys:
        o.append(f"<tr><th>{html.escape(k)}</th>")
        for r in runs:
            if k not in r["items"]:
                o.append("<td>-</td>")
                continue
            rr, lo, hi, st, best, _ = norm(r["items"][k])
            ci = f" [{fmt(lo)}, {fmt(hi)}]" if lo is not None else ""
            o.append(f"<td>{fmt(rr)}{ci} {html.escape(str(best or ''))} <b>{st}</b></td>")
        o.append("</tr>")
    o.append("</tbody></table></div></details></section>")
    return "\n".join(o)


# The page is one ink panel in both themes, as the README's panels are; the tokens are those of tools/gen_assets.py.
_ON = G.ON["ink"]
CSS = (f":root{{--bg:{G.INK};--ink:{G.PAPER};--muted:{_ON['dim']};--grid:{_ON['rule']};--axis:{_ON['dim']};"
       f"--pass:{G.MARK};--partial:{G.PAPER};--blocked:{_ON['dim']};--fail:{G.ALERT};--card:{_ON['rule']};--mono:{G.MONO}}}") + """
*{box-sizing:border-box}
body{margin:0;padding:16px;background:var(--bg);color:var(--ink);font:15px/1.45 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:980px;margin:0 auto}
h1,h2,h3{font-family:var(--mono);text-transform:uppercase;letter-spacing:.08em}
h1{font-size:1.5rem;margin:.2em 0}h2{font-size:1.05rem;margin:2.4em 0 .4em;border-bottom:1px solid var(--grid);padding-bottom:.4em}
h2::before{content:"// ";color:var(--muted)}
h3{font-size:.8rem;margin:1.6em 0 .4em;color:var(--muted)}
.note,.runs,.legend{color:var(--muted);font-size:.88rem}
code{font-size:.85em;overflow-wrap:anywhere}
.legend span{margin-right:1em;white-space:nowrap}
.crow,.brow{display:grid;grid-template-columns:minmax(0,15rem) minmax(0,1fr) minmax(0,11rem);gap:.6rem;align-items:center;padding:.25rem 0;
border-bottom:1px solid var(--grid)}
.clab,.bname{font-size:.85rem;overflow-wrap:anywhere}
.ctext,.blast{font-size:.82rem;color:var(--muted)}
.bar{display:flex;gap:2px;height:14px}
.seg{border-radius:3px}
.seg.pass,.tag.pass{background:var(--pass)}.seg.partial,.tag.partial{background:var(--partial)}
.seg.blocked,.tag.blocked{background:var(--blocked)}.seg.fail,.tag.fail{background:var(--fail)}
.seg.na,.tag.na{background:var(--grid);color:var(--ink)}
.tag{display:inline-block;min-width:1.4em;text-align:center;border-radius:4px;color:var(--bg);font-weight:600;font-size:.75rem}
.strip{height:64px;width:auto;max-width:100%;display:block}
.strip .one{stroke:var(--axis);stroke-width:1;stroke-dasharray:3 3}
.strip .hit{fill:transparent}
.strip g:hover .hit{fill:var(--grid)}
.strip .ci{stroke-width:2;stroke-linecap:round;opacity:.55}
.strip .dot{stroke:var(--bg);stroke-width:2}
.strip .pass .ci{stroke:var(--pass)}.strip .pass .dot{fill:var(--pass)}
.strip .partial .ci{stroke:var(--partial)}.strip .partial .dot{fill:var(--partial)}
.strip .blocked .ci{stroke:var(--blocked)}.strip .blocked .dot{fill:var(--blocked)}
.strip .fail .ci{stroke:var(--fail)}.strip .fail .dot{fill:var(--fail)}
.strip .na .ci{stroke:var(--muted)}.strip .na .dot{fill:var(--muted)}
.strip .axis{fill:var(--muted);font-size:10px}
.strip .glyph,.strip .miss{fill:var(--muted);font-size:12px;text-anchor:middle}
.scroll{overflow-x:auto}
table{border-collapse:collapse;font-size:.8rem}td,th{border:1px solid var(--grid);padding:.25rem .4rem;text-align:left;vertical-align:top}
thead th{background:var(--card)}
details{margin-top:1rem}summary{cursor:pointer}
@media (max-width:640px){.crow,.brow{grid-template-columns:minmax(0,1fr)}}
"""


def main() -> int:
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", default=os.path.join(here, "results"))
    ap.add_argument("--out", default=os.path.join(here, "..", "docs", "dashboard.html"))
    a = ap.parse_args()
    groups = load_runs(a.results)
    if not groups:
        raise SystemExit(f"no leaderboard or indexed results under {a.results}")
    legend = "".join(f'<span><span class="tag {css(s)}">{MARK[s]}</span> {s}</span>' for s in STATUSES)
    body = [
        "<h1>Scrutate benchmark dashboard</h1>",
        "<p class='note'>Generated by <code>bench/make_dashboard.py</code> from the raw results in <code>bench/results/</code>. "
        "Ratio = median(scrutate) / median(best competitor). PASS: the 95% interval lies below 1. FAIL: above 1. PARTIAL: it contains 1. "
        "BLOCKED: a tie at the hardware limit. n/a: not compared (an indexed update where every rival rebuilds). Runs are listed oldest first; runs under <code>superseded/</code> used older binaries "
        "or an older harness. The machine was shared, so no row is a #1 claim.</p>",
        f"<p class='legend'>{legend}</p>",
    ]
    body += [section(p, k, runs) for (p, k), runs in groups.items()]
    page = ("<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>"
            f"<title>Scrutate benchmarks</title><style>{CSS}</style></head><body><main>{''.join(body)}</main></body></html>\n")
    with open(a.out, "w") as f:
        f.write(page)
    print(f"wrote {a.out} ({sum(len(r) for r in groups.values())} runs in {len(groups)} groups)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
