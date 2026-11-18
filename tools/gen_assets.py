#!/usr/bin/env python3
"""The README's images in docs/assets/: one file per panel, built from the tokens and parts below.

usage:
  python3 tools/gen_assets.py            write every image
  python3 tools/gen_assets.py --check    exit 1 when a committed image differs from what this writes, when an
                                         SVG breaks a rule below, or when a link or image in README.md is missing
  python3 tools/gen_assets.py beats      print the hero's timeline, beat by beat
  python3 tools/gen_assets.py capture --bin PATH/TO/scrutate --repo DIR
                                         run the commands in docs/assets/demo.json for real in DIR and keep
                                         their output there (the social preview draws it)
  python3 tools/gen_assets.py capture-hero --bin PATH/TO/scrutate --repo crates/scrutate
                                         run the hero's searches (docs/assets/hero.json) with --json in DIR and
                                         keep the exact spans on its block of lines; edit the list there to change them
  python3 tools/gen_assets.py font --otf PATH/TO/GeistMono-Black.otf
                                         write tools/fonts/geist-mono-black.json, the outlines headlines are drawn with

The system: black ink, cream paper and one highlighter colour (MARK) that marks only what was found: a match,
the winning bar, the one word that matters. Every panel carries its own background, so one file serves
GitHub's light and dark pages. Rules each SVG keeps, checked by --check: flat colour only (no gradient, no
filter), no script, event handler or SMIL element, nothing loaded from outside, a title and a desc, valid XML,
under 150 KB, and complete with motion off (the still state is the finished state).

Motion is CSS inside each SVG (GitHub shows them through <img>: no script). Each animated image runs on one
master clock (Clock below): every element's keyframes are percentages of one loop, so it repeats for ever
without a seam. Only transform, opacity and stroke-dashoffset are animated. An element's attributes are its
finished state; its class says whether its motion travels (`mv`, stopped under reduced motion) or stays in
place (`sm`, kept), and --check refuses an animated element that has neither.

Headlines are outlines of Geist Mono Black (SIL Open Font License 1.1, tools/fonts/OFL.txt), read from
tools/fonts/geist-mono-black.json, so they look the same whatever fonts a reader has.

docs/assets/social-preview.png is rendered by hand from social-preview.svg:
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu \
    --hide-scrollbars --window-size=1280,640 --screenshot=docs/assets/social-preview.png \
    docs/assets/social-preview.svg
Plain Python 3.9, standard library only (the font mode too).
"""
from __future__ import annotations

import argparse
import html
import json
import os
import re
import struct
import subprocess
import sys
import xml.dom.minidom

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
ASSETS = os.path.join(ROOT, "docs", "assets")
FONT_JSON = os.path.join(ROOT, "tools", "fonts", "geist-mono-black.json")

# ---------------------------------------------------------------- tokens: every colour and size, once

INK, PAPER, MARK, ALERT = "#0c0c0e", "#f2eee3", "#d9ff3d", "#ff5a36"
ON = {  # what sits on each panel colour
    "ink": {"bg": INK, "fg": PAPER, "rule": "#2a2a2f", "dim": "#8f8f98"},
    "paper": {"bg": PAPER, "fg": INK, "rule": "#cfc9ba", "dim": "#6f6a5e"},
}
W, RADIUS, INSET, PAD = 880, 14, 10, 40        # panel width, corner, frame inset, content margin
SANS = '-apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif'
MONO = 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace'
EASE = "cubic-bezier(.2,.7,.2,1)"          # out: fast start, long settle (a pen stroke, a bar)
EASE_IN_OUT = "cubic-bezier(.6,0,.25,1)"   # a reticle travelling to its target
# Motion that travels across the image or grows (a reticle, letters dropping in, a block running along an arrow,
# a pulse) carries the class `mv`; motion that stays in place (typing, a count, a mark wiped in, a cursor, a fade)
# carries `sm`. A reader who asks for reduced motion gets every `mv` element in its finished state, and the rest
# keeps running, so the page still shows the searches being done.
REDUCED = "@media (prefers-reduced-motion: reduce) {\n  .mv { animation: none !important; }\n}"
BANNER = "<!-- written by tools/gen_assets.py; edit that, not this file -->"
MAX_BYTES = 150 * 1024


def esc(s: str) -> str:
    return html.escape(s, quote=False)


def svg(w, h, title, desc, body, style="", animated=False):
    css = f".s{{font-family:{SANS}}} .m{{font-family:{MONO}}} .pre{{white-space:pre}}\n" + style
    if animated:
        css += "\n" + REDUCED
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" '
            f'aria-labelledby="t d">\n{BANNER}\n<title id="t">{esc(title)}</title>\n<desc id="d">{esc(desc)}</desc>\n'
            f"<style>\n{css}\n</style>\n{body}\n</svg>\n")


def text_w(s: str, size: float, mono=False, spacing=0.0) -> float:
    """A generous width estimate for system-font text, so a wider fallback font still fits."""
    return len(s) * size * ((0.62 if mono else 0.56) + spacing)


def kf(name: str, total: float, points, prop="opacity") -> str:
    """@keyframes from (seconds, value[, timing function for the next segment]) points."""
    out = [f"@keyframes {name} {{"]
    for p in points:
        t, v = p[0], p[1]
        tf = f" animation-timing-function: {p[2]};" if len(p) > 2 else ""
        out.append(f"  {100 * t / total:.2f}% {{ {prop}: {v};{tf} }}")
    out.append("}")
    return "\n".join(out)


class Clock:
    """One master clock per image. A beat is a name, a start and a length in seconds; each animated element gets
    keyframes as (seconds, value[, timing function of the segment that follows]) and they are written as
    percentages of the one loop, so nothing drifts and the loop has no seam: the value at 0 and at the end is the
    element's `rest`. An element's SVG attributes are its finished state, which reduced motion shows for an `mv`."""

    def __init__(self, total=None):
        self.total, self.beats, self.tracks = total, [], []

    def beat(self, name, start, length):
        self.beats.append((name, round(start, 3), round(length, 3)))
        return start + length

    def anim(self, points, prop="transform", rest=None, travel=False):
        """The class string for an element animated through `points`; a value may be a dict of properties.
        `travel` marks motion across the image or growing, which reduced motion stops."""
        self.tracks.append((list(points), prop, rest))
        return ("mv" if travel else "sm") + f" k{len(self.tracks)}"

    def more(self, cls, points):
        """Add keyframes to the element whose class string `anim` returned."""
        self.tracks[int(cls.rsplit("k", 1)[1]) - 1][0].extend(points)

    def sheet(self):
        """The beats as text: what `python3 tools/gen_assets.py beats` prints."""
        return "\n".join(f"{s:7.2f} s  {s + n:7.2f} s  {name}" for name, s, n in self.beats) + f"\nloop {self.total:.2f} s"

    def css(self):
        T = self.total
        out = []
        for i, (points, prop, rest) in enumerate(self.tracks, 1):
            pts = sorted(points, key=lambda p: p[0])
            if pts[0][0] > 0:
                assert rest is not None, f"k{i}: a track that starts after 0 needs its rest value"
                pts.insert(0, (0, rest))
            if pts[-1][0] < T:
                # holding the last value to the end is implicit; drifting back to rest is not
                assert rest is None or pts[-1][1] == rest, f"k{i}: ends at {pts[-1][1]}, not at its rest {rest}"
                pts.append((T, pts[-1][1]))
            assert pts[-1][0] <= T + 1e-6, f"k{i} runs past the loop ({pts[-1][0]:.2f} s of {T:.2f} s)"
            assert pts[0][1] == pts[-1][1], f"k{i}: the value at 0 and at the end differ, so the loop would jump"
            assert all(b[0] > a[0] for a, b in zip(pts, pts[1:])), f"k{i}: two keyframes at one time"
            out.append(f".k{i}{{animation:k{i} {T:.2f}s linear infinite}}")
            frames = []
            for j, p in enumerate(pts):
                v = p[1] if isinstance(p[1], dict) else {prop: p[1]}
                tf = f" animation-timing-function: {p[2]};" if len(p) > 2 else ""
                # a hold is a jump at its end: Chrome repaints an image every frame while a smooth segment runs,
                # even one between two equal values, and not at all during a step
                if j + 1 < len(pts) and pts[j + 1][1] == p[1]:
                    tf = " animation-timing-function: steps(1, end);"
                frames.append(f"{100 * p[0] / T:.3f}% {{ " + " ".join(f"{k}: {x};" for k, x in v.items()) + tf + " }")
            out.append(f"@keyframes k{i} {{ " + " ".join(frames) + " }")
        return "\n".join(out)


def wipe(t, d=0.32, over=1.03):
    """A marker stroke landing: it decelerates and overshoots by a hair. Keyframe points for scaleX."""
    return [(t, "scaleX(0)", EASE), (t + d * 0.78, f"scaleX({over})", "ease-in-out"), (t + d, "scaleX(1)")]


def blink(clk, t0, t1, period=1.06):
    """A cursor blinking from t0 to t1 on the clock: visible, hidden, visible ..."""
    pts, t, on = [], t0, True
    while t < t1 - 1e-6:
        pts.append((t, 1 if on else 0, "steps(1, end)"))
        t, on = t + period / 2, not on
    pts.append((t1, 1))
    return clk.anim(pts, "opacity", rest=1)


def typing(t0, text, cps=26.0):
    """When each character of `text` appears, typed in bursts of a word with a short pause at each space.
    Returns [(t, characters shown, timing function)] and the end time."""
    pace = [1.0, 0.82, 1.18, 0.9, 1.06, 0.95]
    pts, t, n, end = [], t0, 0, t0
    for i, w in enumerate(re.findall(r"\S+\s*", text)):
        pts.append((t, n, f"steps({len(w)}, start)"))
        n += len(w)
        t += len(w) / cps * pace[i % len(pace)]
        pts.append((t, n))
        end = t
        t += 0.07 if i % 3 else 0.14
    return pts, end


# ---------------------------------------------------------------- headline type: outlines from the font JSON

_FONT: dict = {}


def font() -> dict:
    if not _FONT:
        with open(FONT_JSON, encoding="utf-8") as f:
            _FONT.update(json.load(f))
    return _FONT


def type_w(s: str, cap: float, track=0.0) -> float:
    """Width of outlined text with capital height `cap` px; `track` is extra space per letter, in em."""
    f = font()
    return (len(s) * f["advance"] + (len(s) - 1) * track * f["upm"]) * cap / f["cap"]


def words(s, x, y, cap, fill, track=0.0, anchor="start", hollow=0.0, knock=None, cls="", extra=""):
    """Outlined text: baseline `y`, capital height `cap`. `hollow` draws only the outline, that many px wide;
    `knock` = (colour, px) paints a band of that colour round each letter, so it reads over anything."""
    f = font()
    k = cap / f["cap"]
    step = f["advance"] + track * f["upm"]
    x0 = x - type_w(s, cap, track) * {"start": 0, "middle": 0.5, "end": 1}[anchor]
    if hollow:
        paint = f'fill="none" stroke="{fill}" stroke-width="{hollow:g}" stroke-linejoin="round" vector-effect="non-scaling-stroke"'
    else:
        paint = f'fill="{fill}"'
    if knock:
        paint += f' stroke="{knock[0]}" stroke-width="{knock[1]:g}" stroke-linejoin="round" paint-order="stroke" vector-effect="non-scaling-stroke"'
    c = f' class="{cls}"' if cls else ""
    out = [f'<g{c} transform="translate({x0:.1f} {y:.1f}) scale({k:.5f} {-k:.5f})" {paint}{extra}>']
    for i, ch in enumerate(s):
        if ch == " ":
            continue
        if ch not in f["glyphs"]:
            raise SystemExit(f"the headline font JSON has no {ch!r}; add it to FONT_CHARS and run the font mode")
        tr = f' transform="translate({i * step:g} 0)"' if i else ""
        out.append(f'<path{tr} d="{f["glyphs"][ch]}"/>')
    out.append("</g>")
    return "".join(out)


# ---------------------------------------------------------------- parts

def panel(h, on, frame=True, dots=False, w=W, r=RADIUS):
    """The background every image carries: a flat ink or paper card, a hairline frame, optional dot grid."""
    c = ON[on]
    out = [f'<rect width="{w}" height="{h}" rx="{r}" fill="{c["bg"]}"/>']
    if dots:
        out.insert(0, f'<defs><pattern id="dots" width="22" height="22" patternUnits="userSpaceOnUse">'
                      f'<rect x="10" y="10" width="2" height="2" fill="{c["rule"]}"/></pattern></defs>')
        out.append(f'<rect x="{INSET}" y="{INSET}" width="{w - 2 * INSET}" height="{h - 2 * INSET}" rx="{r - 6}" fill="url(#dots)"/>')
    if frame:
        out.append(f'<rect x="{INSET + .5}" y="{INSET + .5}" width="{w - 2 * INSET - 1}" height="{h - 2 * INSET - 1}" '
                   f'rx="{r - 6}" fill="none" stroke="{c["rule"]}"/>')
    return "".join(out)


def mono(x, y, s, size, fill, spacing=0.0, anchor="start", weight="", cls=""):
    """System mono text; capitals with spacing make the small labels."""
    ls = f' letter-spacing="{spacing * size:.2f}"' if spacing else ""
    an = f' text-anchor="{anchor}"' if anchor != "start" else ""
    wt = f' font-weight="{weight}"' if weight else ""
    return f'<text x="{x:.1f}" y="{y:.1f}" class="m pre {cls}" font-size="{size}" fill="{fill}"{ls}{an}{wt}>{esc(s)}</text>'


def label(x, y, n, s, on, x_end):
    """`// 01  WHAT THIS IS` in spaced capitals, then a hairline to x_end."""
    c = ON[on]
    t = f"// {n}  {s.upper()}" if n else f"// {s.upper()}"
    end = x + text_w(t, 12, True, 0.16) + 14
    out = mono(x, y, t, 12, c["fg"], 0.16)
    if end < x_end:
        out += f'<path d="M{end:.1f} {y - 4:.1f} H{x_end}" stroke="{c["rule"]}"/>'
    return out


def marker(x, y, w, h, cls="", style="", colour=MARK):
    """A highlighter stroke: a solid block whose two ends lean like a chisel tip. Flat, no blur."""
    j = min(4.0, h * 0.14)
    c = f' class="{cls}"' if cls else ""
    st = f' style="{style}"' if style else ""
    return f'<path{c}{st} d="M{x + j:.1f} {y:.1f} H{x + w + j:.1f} L{x + w:.1f} {y + h:.1f} H{x:.1f} Z" fill="{colour}"/>'


def crosshair(cx, cy, r, colour, cls=""):
    c = f' class="{cls}"' if cls else ""
    return (f'<g{c} stroke="{colour}" fill="none" stroke-width="1.5"><circle cx="{cx}" cy="{cy}" r="{r}"/>'
            f'<path d="M{cx - r * 1.7:g} {cy} H{cx - r * .45:g} M{cx + r * .45:g} {cy} H{cx + r * 1.7:g} '
            f'M{cx} {cy - r * 1.7:g} V{cy - r * .45:g} M{cx} {cy + r * .45:g} V{cy + r * 1.7:g}"/></g>')


def ticks(x, y, w, h, colour, n=10):
    """Corner ticks: four small L marks framing a box."""
    return (f'<path d="M{x} {y + n} V{y} H{x + n} M{x + w - n} {y} H{x + w} V{y + n} M{x + w} {y + h - n} V{y + h} '
            f'H{x + w - n} M{x + n} {y + h} H{x} V{y + h - n}" stroke="{colour}" stroke-width="1.5" fill="none"/>')


def side(s, x, y, colour):
    """Tiny capitals running down the panel's right edge."""
    return (f'<text transform="translate({x} {y}) rotate(90)" class="m pre" font-size="10" fill="{colour}" '
            f'letter-spacing="2">{esc(s.upper())}</text>')


def tag(x, y, s, on, h=24):
    """`SCRUTATE // GREP`: a hairline box of spaced capitals; returns (svg, width)."""
    c = ON[on]
    w = text_w(s, 11, True, 0.14) + 18
    return (f'<rect x="{x + .5:.1f}" y="{y + .5:.1f}" width="{w:.0f}" height="{h - 1}" fill="none" stroke="{c["rule"]}"/>'
            + mono(x + 10, y + h / 2 + 4, s.upper(), 11, c["dim"], 0.14)), w


def cut(x, y, w, h, fill, k=12):
    """A block with its top right corner cut off."""
    return f'<path d="M{x} {y} H{x + w - k} L{x + w} {y + k} V{y + h} H{x} Z" fill="{fill}"/>'


def statement(x, y, line1, line2, cap, on, slab_cls=""):
    """Two heavy lines; the second sits on a highlighter slab."""
    c = ON[on]
    lh = cap * 1.55
    w2 = type_w(line2, cap) + cap * 0.7
    slab = marker(x - cap * 0.3, y + lh - cap * 1.32, w2, cap * 1.62, slab_cls)
    return words(line1, x, y, cap, c["fg"]) + slab + words(line2, x, y + lh, cap, INK)


WIPE_CSS = ".wipe{transform-box:fill-box;transform-origin:left center}"


# ---------------------------------------------------------------- the wordmark and the logo

WORD = "SCRUTATE"
WORD_TRACK = -0.03


def wordmark(x, y, cap, fill, bg, on_slab=MARK, cursor=True, knock=None, cls_cursor="", cls_letters=None, cls_bar="",
             cls_blink=""):
    """SCRUTATE in Geist Mono Black; the bar of the A is a highlighter stroke, and a block cursor follows.
    `bg` (or `knock`) is a band round the letters, so they read over whatever lies behind. `cls_letters` (one class
    string per letter) puts each letter in its own group, to animate; the knock band is then left out.
    Returns (svg, width including the cursor)."""
    f = font()
    k = cap / f["cap"]
    knock = knock or (bg, cap * 0.09)
    step = (f["advance"] + WORD_TRACK * f["upm"]) * k
    ax = x + WORD.index("A") * step
    # the A's counter runs to the baseline; a stroke across both legs takes the place of its bar
    bar = marker(ax + 40 * k, y - 360 * k, 500 * k, 170 * k, cls_bar, colour=on_slab)

    def glyph(i, ch):
        d = A_OPEN if ch == "A" else f["glyphs"][ch]
        return f'<path transform="translate({x + i * step:.1f} {y:.1f}) scale({k:.5f} {-k:.5f})" d="{d}"/>'

    if cls_letters:
        body = [f'<g fill="{fill}">' + "".join(f'<g class="{c}">{glyph(i, ch)}</g>' for i, (ch, c) in enumerate(zip(WORD, cls_letters)))
                + "</g>", bar]
    else:
        body = [f'<g fill="{knock[0]}" stroke="{knock[0]}" stroke-width="{knock[1]:g}" stroke-linejoin="round" '
                'vector-effect="non-scaling-stroke">' + "".join(glyph(i, ch) for i, ch in enumerate(WORD)) + "</g>",
                f'<g fill="{fill}">' + "".join(glyph(i, ch) for i, ch in enumerate(WORD)) + "</g>", bar]
    width = len(WORD) * step
    if cursor:
        cw = step * 0.62
        cl = f' class="{cls_cursor}"' if cls_cursor else ""
        rect = f'<rect x="{x + width + step * 0.12:.1f}" y="{y - cap:.1f}" width="{cw:.1f}" height="{cap:.1f}" fill="{on_slab}"/>'
        if cls_blink:
            rect = rect.replace("<rect ", f'<rect class="{cls_blink}" ', 1)
        body.append(f"<g{cl}>{rect}</g>" if cl else rect)
        width += step * 0.12 + cw
    return "".join(body), width


A_OPEN = "M14 0L192 0L300 476L408 0L586 0L406 710L194 710Z"   # Geist Mono's A without its bar


def lens(dx, dy, s, ring, beam):
    """The mark on a 64 grid: a lens crossed by a highlighter beam."""
    return (f'<g transform="translate({dx} {dy}) scale({s})">'
            f'{marker(2, 22, 52, 10, colour=beam)}'
            f'<circle cx="27" cy="27" r="16" fill="none" stroke="{ring}" stroke-width="7"/>'
            f'<path d="M39 39 L55 55" stroke="{ring}" stroke-width="9" stroke-linecap="square"/></g>')


def logo_mark() -> str:
    body = f'<rect width="64" height="64" rx="12" fill="{INK}"/>' + lens(5, 5, 0.86, PAPER, MARK)
    return svg(64, 64, "Scrutate", "The Scrutate mark: a lens crossed by a highlighter beam, on an ink square.", body)


def logo() -> str:
    h, cap = 96, 44
    wm, ww = wordmark(96, 70, cap, PAPER, INK)
    w = int(96 + ww + 30)
    body = (f'<rect width="{w}" height="{h}" rx="14" fill="{INK}"/>' + lens(18, 16, 1.0, PAPER, MARK) + wm)
    return svg(w, h, "Scrutate", "The Scrutate mark, a lens crossed by a highlighter beam, and the wordmark SCRUTATE "
               "whose A has a highlighter stroke for its bar, on ink.", body)


# ---------------------------------------------------------------- the hero: four real searches over one block of code

HERO_LINE = ("INSTANT SEARCH, NO INDEX", "CODE INTELLIGENCE FROM AN INDEX")
HERO_SMALL = ("TWO ENGINES", "ONE RUST BINARY")
HERO_DESC = "the lines 'Instant search, no index. Code intelligence from an index. Two engines, one Rust binary.'"
DEMO_JSON = os.path.join(ASSETS, "demo.json")
HERO_JSON = os.path.join(ASSETS, "hero.json")


def demo_spec():
    with open(DEMO_JSON, encoding="utf-8") as f:
        return json.load(f)


def hero_spec():
    with open(HERO_JSON, encoding="utf-8") as f:
        return json.load(f)


def capture_hero(binary: str, repo: str) -> int:
    """Run the hero's searches in `repo` with --json and keep the spans that fall on the block of lines shown."""
    spec = hero_spec()
    first, last = spec["first"], spec["last"]
    with open(os.path.join(repo, spec["file"]), encoding="utf-8") as f:
        spec["lines"] = f.read().split("\n")[first - 1:last]
    env = dict(os.environ, NO_COLOR="1")
    for q in spec["queries"]:
        argv = [binary, q["argv"][1], "--json"] + q["argv"][2:]
        p = subprocess.run(argv, cwd=repo, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
        rows = [json.loads(ln) for ln in p.stdout.splitlines()]
        spans = []
        for r in rows:
            if r["type"] == "match" and r["path"] == spec["file"] and first <= r["line"] <= last:
                assert r["text"] == spec["lines"][r["line"] - first], r
                b = r["text"].encode()
                spans += [[r["line"], len(b[:s["start"]].decode()), len(b[:s["end"]].decode())] for s in r["spans"]]
        s = rows[-1]
        assert s["type"] == "summary", s
        q.update(spans=spans, lines_matched=s["matches"], files_matched=s["files_matched"], files_searched=s["files_searched"])
    spec["version"] = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.strip()
    spec["source"] = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=repo, capture_output=True, text=True).stdout.strip()
    with open(HERO_JSON, "w", encoding="utf-8") as f:
        json.dump(spec, f, indent=2)
        f.write("\n")
    print(f"wrote {HERO_JSON}", file=sys.stderr)
    return 0


def code_text(x, y, s, size, fill, cw, cls=""):
    """One line of code in system mono, each character exactly `cw` apart whatever mono font the reader has,
    so a highlighter slab drawn at a computed column sits on the right letters."""
    lead = len(s) - len(s.lstrip(" "))
    s = s.strip(" ")
    if not s:
        return ""
    return (f'<text class="m {cls}" x="{x + lead * cw:.1f}" y="{y:.1f}" font-size="{size}" fill="{fill}" '
            f'textLength="{len(s) * cw:.1f}" lengthAdjust="spacing" xml:space="preserve">{esc(s)}</text>').replace('"m "', '"m"')


def count_up(final: str):
    """The values a counter shows on its way to `final` (a number as written, such as 17, 12.3 or 1,234)."""
    dec = len(final.split(".")[1]) if "." in final else 0
    v = float(final.replace(",", ""))
    out = []
    for f in (0.1, 0.27, 0.46, 0.63, 0.78, 0.9, 0.97):
        s = f"{v * f:,.{dec}f}"
        if s not in out and s != final and float(s.replace(",", "")) > 0:
            out.append(s)
    return out + [final]


def counter(clk, x, y, n, t0, t1, t_clear, size, fill, bold=True, suffix=""):
    """A number that runs up from blank to `n` between t0 and t1 and is blank again from t_clear, right-aligned
    at x, with `suffix` after each value: a strip of values behind a one-row window. Its finished state is `n`."""
    rows = [""] + [v + suffix for v in count_up(str(n))]
    wide = len(str(n) + suffix) * size * 0.62 + size
    dy = size * 1.5
    top = (len(rows) - 1) * dy
    cls = clk.anim([(t0, "translateY(0px)", f"steps({len(rows) - 1}, end)"), (t1, f"translateY({-top:.1f}px)"),
                    (t_clear, f"translateY({-top:.1f}px)", "steps(1, end)"), (t_clear + 0.01, "translateY(0px)")],
                   rest="translateY(0px)")
    tspans = "".join(f'<tspan x="{x:.1f}" y="{y + i * dy:.1f}" xml:space="preserve">{esc(v)}</tspan>' for i, v in enumerate(rows) if v)
    wt = ' font-weight="700"' if bold else ""
    window = (f'<clipPath id="n{cls.split("k")[-1]}"><rect x="{x - wide:.1f}" y="{y - size:.1f}" width="{wide + 2:.1f}" '
              f'height="{size * 1.3:.1f}"/></clipPath>')
    return (f'<defs>{window}</defs><g clip-path="url(#n{cls.split("k")[-1]})"><text class="m {cls}" transform="translate(0 {-top:.1f})" '
            f'font-size="{size}" fill="{fill}" text-anchor="end"{wt}>{tspans}</text></g>')


def reticle(cls, colour, r=11):
    return (f'<g class="{cls}" opacity="0"><g stroke="{colour}" fill="none" stroke-width="1.5"><circle r="{r}"/>'
            f'<path d="M{-r * 1.8:g} 0 H{-r * .5:g} M{r * .5:g} 0 H{r * 1.8:g} M0 {-r * 1.8:g} V{-r * .5:g} M0 {r * .5:g} V{r * 1.8:g}"/>'
            "</g></g>")


def hero(phone=False):
    """The poster: a block of real code, four real searches run over it one after another (typed, each match
    found by a reticle and struck by the highlighter, the count run up), and the wordmark below.
    Its beats: `python3 tools/gen_assets.py beats`."""
    return _hero(phone)[0]


def wrap_rows(lines, cols):
    """Wraps lines at `cols` characters, after a space where there is one: (line index, first column, text) rows.
    No character is dropped or changed, so a span of the line maps onto the rows."""
    rows = []
    for i, ln in enumerate(lines):
        k = 0
        while True:
            e = len(ln) if len(ln) - k <= cols else (ln.rfind(" ", k + 1, k + cols) + 1 or k + cols)
            rows.append((i, k, ln[k:e]))
            if e >= len(ln):
                break
            k = e
    return rows


def segments(rows, i, s0, s1):
    """The pieces of columns s0..s1 of line i on the rows it wraps onto: (row number, first column, start, end)."""
    return [(r, k, max(s0, k), min(s1, k + len(t))) for r, (j, k, t) in enumerate(rows)
            if j == i and max(s0, k) < min(s1, k + len(t))]


# the hero's two layouts: the poster at 880, and a tall one for a phone (the wordmark first, the code wrapped)
HERO_LAYOUT = {
    "wide": {"w": W, "fs": 11.2, "cw": 6.72, "lh": 21, "ps": 15, "pw": 9.0, "cap": 104, "st": 23, "count": 18},
    "phone": {"w": 400, "fs": 11.2, "cw": 6.72, "lh": 17, "ps": 13, "pw": 7.8, "cap": 50, "st": 12.5, "count": 15},
}


def _hero(phone=False):
    spec = hero_spec()
    c = ON["ink"]
    L = HERO_LAYOUT["phone" if phone else "wide"]
    PW, fs, cw, lh, ps, pw, cap, count = L["w"], L["fs"], L["cw"], L["lh"], L["ps"], L["pw"], L["cap"], L["count"]
    M = 24 if phone else PAD
    first, last = spec["first"], spec["last"]
    caption = (f"{spec['file']}:{first}-{last}" if phone else f"{spec['file']}, lines {first} to {last}").upper()
    licence = f"MIT OR APACHE-2.0 // {spec['version'].upper()}"
    st_lh = L["st"] * 1.55
    if phone:
        wy = 30 + cap                       # the wordmark leads, then the statement, then the search
        sy = wy + 46
        tag_y = sy + st_lh + 34
        rule0 = tag_y + 34
        px, py = M, rule0 + 34
        lab_y = py + 28
        cap_y = lab_y + 22
        rule1 = cap_y + 14
        lx, nx = PW - M, PW - M - 82        # the FILES and LINES counters, on the caption's row
        cx0 = M + 22
        cols = int((PW - M - cx0) // cw)
    else:
        px, py = PAD, 54
        lab_y = cap_y = 82
        rule1 = 96
        lx, nx = PW - PAD, PW - PAD - 106
        cx0 = 54
        cols = 200
    rows = wrap_rows(spec["lines"], cols)
    cy0 = rule1 + 26
    code_end = cy0 + (len(rows) - 1) * lh + (16 if phone else 22)
    if phone:
        H = int(code_end + 12)
    else:
        wy = code_end + 122
        sy = wy + 62
        H = 586

    clk = Clock()
    b = [panel(H, "ink")]
    b.append(mono(px, py, "$", ps, MARK))
    b.append(mono(M if phone else PW - PAD, cap_y, caption, 9 if phone else 10.5, c["dim"], 0.14, "start" if phone else "end"))
    if phone:
        b.append(f'<path d="M{M} {rule0} H{PW - M}" stroke="{c["rule"]}"/>')
    b.append(f'<path d="M{M} {rule1} H{PW - M}" stroke="{c["rule"]}"/>')
    under = len(b)   # the searches' slabs go here, under the code, so a lifting slab uncovers the letters
    for r, (i, k, t) in enumerate(rows):
        y = cy0 + r * lh
        if not k:
            b.append(mono(cx0 - 12, y, str(first + i), 10, c["rule"], anchor="end"))
        b.append(code_text(cx0, y, t, fs, c["dim"], cw))
    if not phone:
        b.append(f'<path d="M{PAD} {code_end} H{PW - PAD}" stroke="{c["rule"]}"/>')
        b.append(side(licence, PW - 24, 112, c["dim"]))
    # the counters' labels: the numbers are each search's own
    small = 9.5 if phone else 10.5
    b.append(mono(lx - (36 if phone else 44), cap_y if phone else py, "FILES", small, c["dim"], 0.14, "end"))
    b.append(mono(nx - (40 if phone else 44), cap_y if phone else py, "LINES", small, c["dim"], 0.14, "end"))

    # the wordmark: the letters cut in, the A's highlighter bar is drawn last, the cursor lands, then it stands
    lift = cap * 0.22
    letters = []
    for i in range(len(WORD)):
        t = 0.1 + 0.075 * i
        letters.append(clk.anim([(t, {"transform": f"translateY({lift:.0f}px)", "opacity": 0}, "steps(1, start)"),
                                 (t + 0.01, {"transform": f"translateY({lift:.0f}px)", "opacity": 1}, EASE),
                                 (t + 0.34, {"transform": "translateY(0px)", "opacity": 1})],
                                rest={"transform": f"translateY({lift:.0f}px)", "opacity": 0}, travel=True))
    clk.beat("wordmark: the letters cut in", 0.1, 0.075 * 7 + 0.34)
    t_bar = clk.beat("wordmark: the A's bar is drawn", 0.78, 0.34) - 0.34
    t_cur = clk.beat("wordmark: the cursor lands", 1.1, 0.3) - 0.3
    bar_cls = "wipe " + clk.anim(wipe(t_bar, 0.34), rest="scaleX(0)")
    cur_cls = clk.anim([(t_cur, {"transform": f"translateY({-cap * 0.6:.0f}px)", "opacity": 0}, EASE),
                        (t_cur + 0.3, {"transform": "translateY(0px)", "opacity": 1})],
                       rest={"transform": f"translateY({-cap * 0.6:.0f}px)", "opacity": 0}, travel=True)
    slab_cls = "wipe " + clk.anim(wipe(1.3, 0.42), rest="scaleX(0)")
    clk.beat(f"statement: {HERO_LINE[1]} is marked", 1.3, 0.42)

    # the searches: `scrutate grep ` stays at the prompt, the rest of each command is typed
    head = "scrutate grep "
    b.append(code_text(px + 2 * pw, py, head.strip(), ps, PAPER, pw))
    x0 = px + (2 + len(head)) * pw
    t = 1.3
    queries = []
    for qi, q in enumerate(spec["queries"]):
        assert q["argv"][:2] == ["scrutate", "grep"], q["argv"]
        cmd = shell_join(q["argv"][2:])
        assert x0 + len(cmd) * pw < PW - M, f"the command {cmd!r} does not fit the {'phone' if phone else 'wide'} hero"
        pts, t_typed = typing(t, cmd, 24)
        clk.beat(f"search {qi + 1}: `{cmd}` is typed", t, t_typed - t)
        t_run = t_typed + 0.25
        marks = [(ln, s0, s1, t_run + 0.1 + 0.42 * j) for j, (ln, s0, s1) in enumerate(sorted(q["spans"]))]
        t_landed = marks[-1][3] + 0.5
        clk.beat(f"search {qi + 1}: the reticle finds the {len(marks)} match{'es' * (len(marks) > 1)} on these lines, "
                 "the highlighter strikes each, the counts run up", t_run + 0.1, t_landed - t_run - 0.1)
        t_lift = t_landed + 2.1
        clk.beat(f"search {qi + 1}: hold", t_landed, t_lift - t_landed)
        queries.append((q, cmd, pts, t, t_typed, t_run, marks, t_landed, t_lift))
        clk.beat(f"search {qi + 1}: the marks lift, the line is cleared", t_lift, 0.3)
        t = t_lift + 0.45
    t_end = queries[-1][8] + 1.0
    clk.total = t_end
    t_exit = queries[-1][8] + 0.1
    clk.beat("wordmark leaves; the loop starts again", t_exit, t_end - t_exit)

    # the letters, the bar and the cursor leave at the end of the loop, so the next one starts clean
    for i, cls in enumerate(letters):
        te = t_exit + 0.03 * i
        clk.more(cls, [(te, {"transform": "translateY(0px)", "opacity": 1}, "ease-in"),
                       (te + 0.22, {"transform": f"translateY({-lift / 2:.0f}px)", "opacity": 0}),
                       (te + 0.23, {"transform": f"translateY({lift:.0f}px)", "opacity": 0})])
    for cls in (bar_cls, slab_cls):
        clk.more(cls, [(t_exit, "scaleX(1)", "ease-in"), (t_exit + 0.25, "scaleX(0)")])
    clk.more(cur_cls, [(t_exit + 0.25, {"transform": "translateY(0px)", "opacity": 1}, "ease-in"),
                       (t_exit + 0.45, {"transform": "translateY(0px)", "opacity": 0}),
                       (t_exit + 0.46, {"transform": f"translateY({-cap * 0.6:.0f}px)", "opacity": 0})])
    blink_cls = blink(clk, t_cur + 0.3, t_exit + 0.2)
    wm, ww = wordmark(M, wy, cap, PAPER, INK, cls_letters=letters, cls_bar=bar_cls, cls_cursor=cur_cls, cls_blink=blink_cls)
    assert M + ww <= PW - M + 1, "the wordmark does not fit"
    b.append(wm)
    b.append(statement(M, sy, HERO_LINE[0], HERO_LINE[1], L["st"], "ink", slab_cls=slab_cls))
    assert M + type_w(HERO_LINE[1], L["st"]) * 1.05 < (PW - M if phone else PW - PAD - 140), "the statement does not fit"
    if phone:
        b.append(mono(M, tag_y, " // ".join(HERO_SMALL), 10, PAPER, 0.14))
        b.append(mono(M, tag_y + 17, licence, 10, c["dim"], 0.14))
    else:
        b.append(mono(PW - PAD, sy, HERO_SMALL[0], 11, PAPER, 0.16, "end"))
        b.append(mono(PW - PAD, sy + st_lh, HERO_SMALL[1], 11, PAPER, 0.16, "end"))

    # each search is a group: its label, its command, its marks and its counts; the groups cross-fade
    cur_pts, ret_pts = [], []
    prev = None
    n = len(queries)
    for qi, (q, cmd, pts, t0, t_typed, t_run, marks, t_landed, t_lift) in enumerate(queries):
        # a search's group is cut in and out while its line is empty and its marks are gone
        cut_out = [(t_lift + 0.3, 1, "steps(1, end)"), (t_lift + 0.31, 0)]
        p = queries[qi - 1][8]   # the lift of the search before (for the first, the last one's)
        if qi:
            g_cls = clk.anim([(p + 0.3, 0, "steps(1, end)"), (p + 0.31, 1)] + cut_out, "opacity", rest=0)
        else:
            g_cls = clk.anim(cut_out + [(p + 0.3, 0, "steps(1, end)"), (p + 0.31, 1)], "opacity", rest=1)
        g_open = f'<g class="{g_cls}"' + (' opacity="0"' if qi else "") + ">"
        slabs = [g_open]
        b.append(g_open)
        b.append(mono(M if phone else PAD, lab_y if phone else 82, f"SEARCH {qi + 1} OF {n} // {q['label']}", small, c["fg"], 0.14))
        b.append(code_text(x0, py, cmd, ps, PAPER, pw))
        # typing: a cover over the command shrinks toward its right end, a letter at a time; at the lift it
        # grows back from the right, as if the line were deleted
        Lc = len(cmd) * pw + 2
        cov = [(tt, f"scaleX({1 - k * pw / Lc:.4f})", *rest) for tt, k, *rest in pts]
        cov_cls = clk.anim(cov + [(t_lift, "scaleX(0)", f"steps({len(cmd)}, end)"), (t_lift + 0.25, "scaleX(1)")], rest="scaleX(1)")
        b.append(f'<rect class="{cov_cls} cover" x="{x0 - 1:.1f}" y="{py - ps:.1f}" width="{Lc:.1f}" height="{ps + 6}" fill="{INK}" '
                 f'transform="scale(0 1)"/>')
        cur_pts += [(t0 - 0.31, {"transform": f"translateX({x0:.1f}px)", "opacity": 0}, "steps(1, end)"),
                    (t0 - 0.3, {"transform": f"translateX({x0:.1f}px)", "opacity": 1})]
        cur_pts += [(tt, {"transform": f"translateX({x0 + k * pw:.1f}px)", "opacity": 1}, *rest) for tt, k, *rest in pts]
        cur_pts += [(t_run, {"transform": f"translateX({x0 + len(cmd) * pw:.1f}px)", "opacity": 1}, "steps(1, end)"),
                    (t_run + 0.01, {"transform": f"translateX({x0 + len(cmd) * pw:.1f}px)", "opacity": 0})]
        # the counts run up while the marks land
        t_c1 = max(t_landed, t_run + 0.9)
        cy_n = cap_y + 1 if phone else py + 1
        b.append(counter(clk, lx, cy_n, q["files_matched"], t_run + 0.2, t_c1, t_lift + 0.32, count, PAPER))
        b.append(counter(clk, nx, cy_n, q["lines_matched"], t_run + 0.2, t_c1, t_lift + 0.32, count, PAPER))
        # the marks: a slab wipes in under each match (each piece of it, where a line wraps), the matched
        # letters turn ink on it, then they lift
        for (ln, s0, s1, tm) in marks:
            segs = segments(rows, ln - first, s0, s1)
            assert segs, (ln, s0, s1)
            text = spec["lines"][ln - first]
            for r, k, a, e in segs:
                y = cy0 + r * lh
                mx, mw = cx0 + (a - k) * cw - 2, (e - a) * cw + 3
                slab = wipe(tm + 0.22, 0.28) + [(t_lift, "scaleX(1)", "ease-in"), (t_lift + 0.3, "scaleX(0)")]
                slabs.append(marker(mx, y - fs * 0.98, mw, fs * 1.34, "wipe " + clk.anim(slab, rest="scaleX(0)")))
                ink = [(tm + 0.41, 0, "steps(1, end)"), (tm + 0.42, 1), (t_lift, 1, "steps(1, end)"), (t_lift + 0.01, 0)]
                b.append(code_text(cx0 + (a - k) * cw, y, text[a:e], fs, INK, cw, clk.anim(ink, "opacity", rest=0)))
            r, k, a, e = segs[0]
            tx, ty = cx0 + ((a + e) / 2 - k) * cw, cy0 + r * lh - fs * 0.36
            start = prev or (x0 + len(cmd) * pw, py - 5)

            def at(p, s, o=1):
                return {"transform": f"translate({p[0]:.1f}px, {p[1]:.1f}px) scale({s})", "opacity": o}
            if prev is None:   # it appears at the end of the command and sets off
                ret_pts += [(tm - 0.12, at(start, 1.8, 0), EASE), (tm, at(start, 1.3), EASE_IN_OUT)]
            else:
                ret_pts.append((tm, at(start, 1), EASE_IN_OUT))
            ret_pts += [(tm + 0.22, at((tx, ty), 1.35), EASE), (tm + 0.34, at((tx, ty), 1))]
            prev = (tx, ty)
        if prev:
            ret_pts.append((t_lift, {"transform": f"translate({prev[0]:.1f}px, {prev[1]:.1f}px) scale(1)", "opacity": 1}, "ease-in"))
            ret_pts.append((t_lift + 0.25, {"transform": f"translate({prev[0]:.1f}px, {prev[1]:.1f}px) scale(1.6)", "opacity": 0}))
            prev = None
        b.append("</g>")
        b.insert(under, "".join(slabs) + "</g>")
        under += 1
    # the prompt's cursor and the reticle are hidden between searches; they rest where they were last seen
    b.append(f'<g class="{clk.anim(cur_pts, rest=cur_pts[-1][1])}" opacity="0"><rect y="{py - ps + 1:.1f}" width="{pw:.1f}" '
             f'height="{ps + 3}" fill="{MARK}"/></g>')
    b.append(reticle(clk.anim(ret_pts, rest=ret_pts[-1][1], travel=True), PAPER, 9 if phone else 11))
    style = [WIPE_CSS, ".cover{transform-box:fill-box;transform-origin:right center}", clk.css()]
    qs = "; ".join(f"`{cmd}` marks {len(q['spans'])} on these lines and reports {q['lines_matched']} matching lines in "
                   f"{q['files_matched']} of {q['files_searched']} files" for q, cmd, *_ in queries)
    where = ("Above: the wordmark SCRUTATE, whose A has a highlighter stroke for its bar, and " if phone else
             "Below: the wordmark SCRUTATE, whose A has a highlighter stroke for its bar, and ")
    desc = (f"Lines {first} to {last} of {spec['file']} (scrutate's own source, crates/scrutate at {spec['source']}), and four "
            f"searches run over them with {spec['version']}, one after another: {qs}. Each search is typed, a reticle finds each "
            f"match the binary reported, a highlighter marks it and the counts run up to the real numbers. {where}"
            f"{HERO_DESC}. With reduced motion the searches still run; the reticle and the wordmark stand still.")
    return svg(PW, H, "Scrutate: instant search with no index, code intelligence from an index", desc, "\n".join(b),
               "\n".join(style), animated=True), clk


# ---------------------------------------------------------------- the social preview's terminal output

LOC = re.compile(r"^(\s*\S+?:\d+(?:-\d+)?(?::\d+)?)(:|\s\s)")


def capture(binary: str, repo: str) -> int:
    spec = demo_spec()
    env = dict(os.environ, NO_COLOR="1")
    for setup in spec.get("setup", []):
        subprocess.run([binary] + setup[1:], cwd=repo, env=env, check=True, stdin=subprocess.DEVNULL, capture_output=True)
    for c in spec["commands"]:
        argv = [binary] + c["argv"][1:]
        p = subprocess.run(argv, cwd=repo, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
        c["output"] = p.stdout.splitlines()
    spec["version"] = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.strip()
    with open(DEMO_JSON, "w") as f:
        json.dump(spec, f, indent=2)
        f.write("\n")
    print(f"wrote {DEMO_JSON}", file=sys.stderr)
    return 0


def shell_join(argv) -> str:
    return " ".join(a if re.fullmatch(r"[\w./=:-]+", a) else '"' + a + '"' for a in argv)


def marked_line(x, y, s, rx, size, fg, pfx):
    """One line of real output, still (the social card): the location in `pfx`, each match of `rx` on a
    highlighter slab in ink. Returns (svg, slabs)."""
    cw = size * 0.6
    m = LOC.match(s)
    head = m.group(1) if m else ""
    rest = s[len(head):]
    slabs, spans = [], [f'<tspan fill="{pfx}">{esc(head)}</tspan>'] if head else []
    last = 0
    for mm in rx.finditer(rest):
        spans += [esc(rest[last:mm.start()]), f'<tspan fill="{INK}">{esc(mm.group(0))}</tspan>']
        slabs.append(marker(x + (len(head) + mm.start()) * cw - 3, y - size * 0.95, len(mm.group(0)) * cw + 4, size * 1.3))
        last = mm.end()
    spans.append(esc(rest[last:]))
    return "".join(slabs) + f'<text x="{x}" y="{y}" class="m pre" font-size="{size}" fill="{fg}">{"".join(spans)}</text>', len(slabs)


# ---------------------------------------------------------------- two engines

ENGINES = {
    "asks": [("CLI", "scrutate ..."), ("PYTHON", "import scrutate"), ("MCP", "for agents"), ("LSP", "for editors")],
    "a": ("A", "NO INDEX", "no index, no parsers, no background process", ["walk", "ignore rules", "match", "stream"],
          [("MATCHES", "grep, streamed")]),
    "b": ("B", "THE INDEX", "persistent, incremental, on disk", ["parse", "symbols", "call edges", "text index", "ranker"],
          [("RANKED RESULTS", "search"), ("DEFINITIONS, CALLERS", "symbol, callers"), ("REFERENCES", "refs")]),
}


def dash_run(clk, t0, t1, ease=EASE_IN_OUT, dash=6.0):
    """A short block of highlighter running along a path drawn with pathLength=100, from before its start to past
    its end, between t0 and t1; hidden the rest of the loop. Returns the class and the dash array."""
    return clk.anim([(t0, f"{dash:.1f}", ease), (t1, "-100", "steps(1, end)"), (t1 + 0.01, f"{dash:.1f}")], "stroke-dashoffset",
                    rest=f"{dash:.1f}", travel=True), f"{dash:.1f} 300"


def dash_path(d, cls, dash, width=3.5, colour=MARK):
    return (f'<path class="{cls}" d="{d}" pathLength="100" fill="none" stroke="{colour}" stroke-width="{width}" '
            f'stroke-dasharray="{dash}" stroke-dashoffset="{dash.split()[0]}"/>')


def path_len(pts):
    return sum(abs(x1 - x0) + abs(y1 - y0) for (x0, y0), (x1, y1) in zip(pts, pts[1:]))


def engines(phone=False) -> str:
    """What you ask goes to one engine or the other: a packet leaves one of the four front ends, runs along the
    wires into Engine A or Engine B, each stage is marked as it passes, and what comes back is marked. Four runs
    in turn (the CLI to A, MCP to B, Python to A, LSP to B), then again. On a phone the same parts stack: the four
    front ends in a row, then each engine with what it returns under it."""
    c = ON["ink"]
    PW = 400 if phone else W
    M = 24 if phone else PAD
    clk = Clock()
    b = []
    asks, cards, packets_to = {}, {}, {}
    if phone:
        b.append(label(M, 38, "", "Which engine answers", "ink", PW - M))
        b.append(mono(M, 70, "YOU ASK", 10.5, c["dim"], 0.16))
        bw, bh, gap, by = (PW - 2 * M - 24) / 4, 38, 8, 82
        bus = by + bh + 18
        trunk = M + 6
        for i, (t, _) in enumerate(ENGINES["asks"]):
            x = M + i * (bw + gap)
            b.append(f'<rect x="{x + .5:.1f}" y="{by + .5}" width="{bw - 1:.1f}" height="{bh - 1}" fill="{INK}" stroke="{c["rule"]}"/>')
            b.append(words(t, x + bw / 2, by + 24, 11, PAPER, anchor="middle"))
            asks[t] = (x, by, bw, bh)
            packets_to[t] = [(x + bw / 2, by + bh), (x + bw / 2, bus), (trunk, bus)]
        b.append(f'<path d="M{trunk} {bus} H{M + 3 * (bw + gap) + bw / 2:.1f}" stroke="{c["dim"]}" stroke-width="1.5" fill="none"/>')
        b.append("".join(f'<path d="M{x + bw / 2:.1f} {y + bh} V{bus}" stroke="{c["dim"]}" stroke-width="1.5"/>'
                         for x, y, _, _ in asks.values()))
        ex, ew = M + 22, PW - 2 * M - 22
        ey = bus + 26
    else:
        b.append(label(PAD, 46, "", "What you ask, and the engine that answers", "ink", W - PAD))
        b.append(mono(PAD, 92, "YOU ASK", 11, c["dim"], 0.16))
        for i, (t, sub) in enumerate(ENGINES["asks"]):
            y = 108 + i * 64
            b.append(f'<rect x="{PAD + .5}" y="{y + .5}" width="139" height="49" fill="{INK}" stroke="{c["rule"]}"/>')
            b.append(words(t, PAD + 12, y + 24, 13, PAPER))
            b.append(mono(PAD + 12, y + 40, sub, 11, c["dim"]))
            asks[t] = (PAD, y, 140, 50)
            packets_to[t] = [(PAD + 141, y + 25), (PAD + 160, y + 25)]
        b.append(f'<path d="M{PAD + 140} {108 + 25} H{PAD + 160} M{PAD + 140} {172 + 25} H{PAD + 160} M{PAD + 140} {236 + 25} H{PAD + 160} '
                 f'M{PAD + 140} {300 + 25} H{PAD + 160} M{PAD + 160} 133 V325" stroke="{c["dim"]}" stroke-width="1.5" fill="none"/>')
        ex, ew = 224, 360
        b.append(mono(ex + ew + 22, 92, "YOU GET", 11, c["dim"], 0.16))
    for key in ("a", "b"):
        letter, big, line, steps, gets = ENGINES[key]
        if phone:
            entry = (trunk, ey + 30)
            b.append(f'<path d="M{trunk} {ey + 30} H{ex - 4}" stroke="{c["dim"]}" stroke-width="1.5"/>'
                     f'<path d="M{ex - 9} {ey + 25} l5 5 -5 5" stroke="{c["dim"]}" stroke-width="1.5" fill="none"/>')
            x, y = ex + 14, ey + 88
        else:
            ey = 108 if key == "a" else 262
            entry = (PAD + 160, ey + 46)
            b.append(f'<path d="M{PAD + 160} {ey + 46} H{ex - 4}" stroke="{c["dim"]}" stroke-width="1.5"/>'
                     f'<path d="M{ex - 9} {ey + 41} l5 5 -5 5" stroke="{c["dim"]}" stroke-width="1.5" fill="none"/>')
            x, y = ex + 18, ey + 92
        tw = [text_w(st.upper(), 10.5, True, 0.1) + 14 for st in steps]
        boxes, seps = [], []
        for i, st in enumerate(steps):
            if x + tw[i] > ex + ew - 14:
                x, y = ex + (14 if phone else 18), y + 34
            elif i:
                seps.append(f'<path d="M{x - 15:.1f} {y + 13} H{x - 3:.1f}" stroke="{INK}" stroke-width="1.5"/>')
            boxes.append((x, y, tw[i], st))
            x += tw[i] + 18
        eh = (y + 26 + 16 - ey) if phone else (136 if key == "a" else 190)
        b.append(cut(ex, ey, ew, eh, PAPER, 16))
        b += seps
        if phone:
            b.append(words(letter, ex + 14, ey + 48, 36, INK))
            b.append(words(big, ex + 56, ey + 36, 18, INK))
            b.append(f'<text x="{ex + 14}" y="{ey + 70}" class="s" font-size="12" fill="{ON["paper"]["dim"]}">{esc(line)}</text>')
        else:
            b.append(words(letter, ex + 18, ey + 66, 50, INK))
            b.append(words(big, ex + 72, ey + 40, 26, INK))
            b.append(f'<text x="{ex + 72}" y="{ey + 62}" class="s" font-size="13" fill="{ON["paper"]["dim"]}">{esc(line)}</text>')
        outs, out_paths = [], []
        for j, (g, sub) in enumerate(gets):
            if phone:
                wx, gx, gy = ex + 22, ex + 44, ey + eh + 14 + j * 52
                b.append(f'<path d="M{wx} {ey + eh} V{gy + 15} H{gx - 6}" stroke="{c["dim"]}" stroke-width="1.5" fill="none"/>')
                b.append(mono(gx + 2, gy + 46, sub, 11, c["dim"]))
                out_paths.append([(wx, ey + eh), (wx, gy + 15), (gx, gy + 15)])
            else:
                gx, gy = ex + ew + 22, ey + 6 + j * 62
                b.append(f'<path d="M{ex + ew + 2} {gy + 28} H{gx - 6}" stroke="{c["dim"]}" stroke-width="1.5"/>')
                b.append(mono(gx + 2, gy + 52, sub, 12, c["dim"]))
                out_paths.append([(ex + ew + 2, gy + 28), (gx, gy + 28)])
            outs.append((g, gx, gy))
        cards[key] = (ey, entry, boxes, outs, out_paths)
        if phone:
            ey = outs[-1][2] + 52 + 22
    H = (cards["b"][3][-1][2] + 64) if phone else 480
    if phone:
        b.append(f'<path d="M{trunk} {bus} V{cards["b"][1][1]}" stroke="{c["dim"]}" stroke-width="1.5" fill="none"/>')
    b.insert(0, panel(H, "ink", dots=True, w=PW))

    # the runs: (front end, engine, which outputs it asks for)
    runs = [("CLI", "a", [0]), ("MCP", "b", [0]), ("PYTHON", "a", [0]), ("LSP", "b", [1, 2])]
    step_marks = {(k, i): [] for k in cards for i in range(len(cards[k][2]))}
    out_marks = {(k, j): [] for k in cards for j in range(len(cards[k][3]))}
    ask_on = {t: [] for t in asks}
    packets = []
    t = 0.4
    for front, key, which in runs:
        ey, entry, boxes, outs, out_paths = cards[key]
        t_in = t + 0.15
        t_at = t_in + 0.6
        t_out = t_at + 0.15 * len(boxes) + 0.1
        t_lift = t_out + 0.5 + 1.7
        clk.beat(f"{front}: the packet runs to Engine {key.upper()}", t_in, 0.6)
        clk.beat(f"{front}: each stage of Engine {key.upper()} is marked as it passes", t_at, t_out - t_at)
        clk.beat(f"{front}: what comes back is marked", t_out, 0.5)
        clk.beat(f"{front}: hold, then the marks lift", t_out + 0.5, t_lift + 0.3 - t_out - 0.5)
        for i in range(len(boxes)):
            step_marks[(key, i)].append((t_at + 0.15 * i, t_lift))
        for n, j in enumerate(which):
            out_marks[(key, j)].append((t_out + 0.18 + 0.12 * n, t_lift))
        ask_on[front].append((t, t_lift))
        packets.append((packets_to[front] + [entry, (ex - 4, entry[1])], t_in, t_at))
        for j in which:
            packets.append((out_paths[j], t_out, t_out + 0.2))
        t = t_lift + 0.55
    clk.total = t

    def lit(times):
        """scaleX points of a slab marked at each (t, lift) of `times`."""
        pts = []
        for tm, tl in times:
            pts += wipe(tm, 0.22) + [(tl, "scaleX(1)", "ease-in"), (tl + 0.3, "scaleX(0)")]
        return pts
    for (key, i), times in step_marks.items():
        x, y, w, st = cards[key][2][i]
        b.append(marker(x - 1, y - 1, w + 3, 27, "wipe " + clk.anim(lit(times), rest="scaleX(0)")).replace("<path ", '<path transform="scale(0 1)" ', 1))
        b.append(f'<rect x="{x + .5:.1f}" y="{y + .5}" width="{w:.0f}" height="25" fill="none" stroke="{INK}"/>')
        b.append(mono(x + 7, y + 17, st.upper(), 10.5, INK, 0.1))
    for (key, j), times in out_marks.items():
        g, gx, gy = cards[key][3][j]
        b.append(marker(gx, gy, type_w(g, 13) + 20, 30, "wipe " + clk.anim(lit(times), rest="scaleX(0)")))
        b.append(words(g, gx + 10, gy + 21, 13, PAPER))
        ink = []
        for tm, tl in times:
            ink += [(tm + 0.12, 0, "steps(1, end)"), (tm + 0.13, 1), (tl, 1, "steps(1, end)"), (tl + 0.01, 0)]
        b.append(words(g, gx + 10, gy + 21, 13, INK, cls=clk.anim(ink, "opacity", rest=0) if ink else ""))
    for front, spans in ask_on.items():
        pts = []
        for t0, t1 in spans:
            pts += [(t0, 0, "steps(1, end)"), (t0 + 0.01, 1), (t1, 1, "steps(1, end)"), (t1 + 0.01, 0)]
        x, y, w, h = asks[front]
        b.append(f'<rect class="{clk.anim(pts, "opacity", rest=0)}" opacity="0" x="{x + .5:.1f}" y="{y + .5}" width="{w - 1:.1f}" '
                 f'height="{h - 1}" fill="none" stroke="{PAPER}" stroke-width="1.5"/>')
    for pts, t0, t1 in packets:
        cls, dash = dash_run(clk, t0, t1, dash=14 / path_len(pts) * 100)
        b.append(dash_path("M" + " L".join(f"{x:g} {y:g}" for x, y in pts), cls, dash))
    desc = ("What you ask (the CLI, Python, an MCP client for agents, an LSP client for editors) goes to two engines in one "
            "binary. Engine A, no index: no index, no parsers, no background process; walk, ignore rules, match, stream; it "
            "returns matches, grep, streamed. Engine B, the index: persistent, incremental, on disk; parse, symbols, call "
            "edges, text index, ranker; it returns ranked results (search), definitions and callers (symbol, callers) and "
            "references (refs). In a loop, a request runs from the CLI to Engine A, from MCP to Engine B, from Python to "
            "Engine A and from LSP to Engine B; each stage is marked as it passes, and what comes back is marked with a highlighter.")
    return svg(PW, H, "Two engines, one binary", desc, "\n".join(b), WIPE_CSS + "\n" + clk.css(), animated=True)


# ---------------------------------------------------------------- section strips

# (file, number, words[, title]): a slim ink kicker above each README section's heading; the words are
# outlined, so they read on a phone. One colour and one height, so the headings below them lead.
STRIPS = [
    ("strip-engines", "01", "NO INDEX, OR AN INDEX"),
    ("strip-quick-start", "02", "EIGHTEEN COMMANDS"),
    ("strip-what-it-does", "03", "FIFTEEN THINGS IT DOES"),
    ("strip-benchmarks", "04", "EVERYTHING HERE IS MEASURED"),
    ("strip-agents", "05", "MCP AND LSP ON STDIO"),
    ("strip-install", "06", "BUILD IT FROM SOURCE"),
    ("strip-python", "07", "IMPORT SCRUTATE"),
    ("strip-status", "08", "NOT DECLARED PRODUCTION-READY"),
    ("strip-documentation", "09", "EVERY DOCUMENT"),
    ("strip-sponsor", "10", "PAID SUPPORT AND HIRE"),
    ("strip-licence", "11", "MIT OR APACHE-2.0", "MIT OR Apache-2.0"),
]


def seek(clk, x0, x1, y, cx, r, colour, t0=0.6, on="ink"):
    """The strips' quiet life: a block runs along the hairline to the crosshair, the crosshair settles on its
    target and a mark lands in its centre, holds, and goes. Its finished state is that mark. On paper the
    block is ink and the mark has an ink edge, since the highlighter alone does not read on paper."""
    cls, dash = dash_run(clk, t0, t0 + 0.95, dash=22 / (x1 - x0) * 100)
    t_hit = t0 + 0.9
    ring = clk.anim([(t_hit, "scale(1)", EASE), (t_hit + 0.12, "scale(1.5)", EASE), (t_hit + 0.5, "scale(1)")], rest="scale(1)",
                    travel=True)
    dot = clk.anim([(t_hit + 0.15, "scale(0)", EASE), (t_hit + 0.45, "scale(1)"), (clk.total - 0.8, "scale(1)", "ease-in"),
                    (clk.total - 0.45, "scale(0)")], rest="scale(0)")
    clk.beat("a block runs along the hairline to the crosshair", t0, 0.95)
    clk.beat("the crosshair settles and its mark lands", t_hit, 0.5)
    clk.beat("hold, then the mark goes", t_hit + 0.5, clk.total - t_hit - 0.5)
    paper = on == "paper"
    edge = f' stroke="{INK}" stroke-width="1.25"' if paper else ""
    return (dash_path(f"M{x0:.1f} {y} H{x1:.1f}", cls, dash, 3, INK if paper else MARK)
            + f'<g transform="translate({cx} {y})"><g class="{ring} spot">{crosshair(0, 0, r, colour)}</g>'
            + f'<rect class="{dot} spot" x="{-r * 0.5:.1f}" y="{-r * 0.5:.1f}" width="{r:.1f}" height="{r:.1f}" fill="{MARK}"{edge}/></g>')


def strip(n, s, i=0, title=None):
    on = "ink"
    c = ON[on]
    H, cap, y = 56, 16, 36
    b = [panel(H, on, frame=False, r=10)]
    b.append(words("//", PAD - 8, y, cap, c["dim"]))
    b.append(words(n, PAD + 26, y, cap, c["dim"]))
    x = PAD + 26 + type_w(n, cap) + 22
    b.append(words(s, x, y, cap, c["fg"]))
    end = x + type_w(s, cap) + 20
    mid = y - cap / 2
    b.append(f'<path d="M{end:.1f} {mid} H{W - PAD - 34}" stroke="{c["rule"]}"/>')
    clk = Clock(4.8)
    b.append(seek(clk, end, W - PAD - 24, mid, W - PAD - 12, 7, c["dim"], t0=0.3 + 0.3 * (i % 3), on=on))
    return svg(W, H, f"{n}: {title or s.capitalize()}", f"Section {n} of the README: {title or s.lower()}.", "\n".join(b),
               SPOT_CSS + "\n" + clk.css(), animated=True)


SPOT_CSS = ".spot{transform-box:fill-box;transform-origin:center}"


PLAYGROUND = "https://scrutate.github.io/playground/"


def cta(phone=False):
    """The README's way into the playground: both engines run in the browser, with no install."""
    on = "ink"
    c = ON[on]
    w = 400 if phone else W
    cap = 24 if phone else 30
    x = 28 if phone else PAD
    y = 52 if phone else 60
    T = 6.4
    clk = Clock(T)
    # the slab lands, holds while the line is read, and lifts just before the loop starts again
    slab_cls = "wipe " + clk.anim(wipe(0.35, 0.42) + [(T - 0.6, "scaleX(1)", "ease-in"), (T - 0.3, "scaleX(0)")],
                                  rest="scaleX(0)")
    clk.beat("YOUR BROWSER is marked", 0.35, 0.42)
    notes = ("BOTH ENGINES, NO INSTALL", "YOUR CODE STAYS ON YOUR MACHINE")
    go = "OPEN THE PLAYGROUND \u2192"
    size = 11 if phone else 12.5
    if phone:
        H = 244
        nx, ny, anchor, step = x, y + cap * 1.55 + 46, "start", 20
    else:
        H = 176
        nx, ny, anchor, step = w - PAD, 58, "end", 24
    b = [panel(H, on, w=w, r=12), statement(x, y, "TRY IT IN", "YOUR BROWSER", cap, on, slab_cls)]
    for i, s in enumerate(notes):
        b.append(mono(nx, ny + i * step, s, size, c["dim"], 0.14, anchor))
    gy = ny + len(notes) * step + (24 if phone else 20)
    gw = text_w(go, size, True, 0.14)
    padx, bh = 14, size + 18
    bw = gw + 2 * padx
    bx = nx - bw if anchor == "end" else nx
    by = gy - size - 7

    def label(fill):
        return mono(bx + padx, gy, go, size, fill, 0.14, weight="700")

    b.append(f'<rect x="{bx + .5:.1f}" y="{by + .5:.1f}" width="{bw - 1:.1f}" height="{bh - 1:.1f}" rx="6" fill="none" stroke="{c["fg"]}"/>')
    b.append(label(c["fg"]))
    # a hand glides in and presses the button: the button lights up and a ring spreads from the fingertip
    t_click = 2.15
    lit = clk.anim([(t_click, 0, "steps(1, end)"), (t_click + 0.01, 1), (T - 1.0, 1, "ease-in"), (T - 0.75, 0)], "opacity", rest=0)
    b.append(f'<g class="{lit}" opacity="0"><rect x="{bx:.1f}" y="{by:.1f}" width="{bw:.1f}" height="{bh:.1f}" rx="6" '
             f'fill="{MARK}"/>{label(INK)}</g>')
    px, py = bx + bw * 0.62, by + bh * 0.58
    still = {"transform": "scale(0.3)", "opacity": 0}
    ring = clk.anim([(t_click, still, "steps(1, end)"), (t_click + 0.01, {"transform": "scale(0.3)", "opacity": 0.9}, EASE),
                     (t_click + 0.55, {"transform": "scale(1.7)", "opacity": 0}, "steps(1, end)"), (t_click + 0.56, still)],
                    rest=still, travel=True)
    b.append(f'<circle class="spot {ring}" cx="{px:.1f}" cy="{py:.1f}" r="16" fill="none" stroke="{MARK}" stroke-width="2" opacity="0"/>')
    sx, sy = px + 70, py + 60

    def at(hx, hy, s=1, o=1):
        return {"transform": f"translate({hx:.1f}px, {hy:.1f}px) scale({s})", "opacity": o}

    hand_cls = clk.anim([(1.05, at(sx, sy, 1, 0), "steps(1, end)"), (1.1, at(sx, sy), EASE_IN_OUT), (1.95, at(px, py)),
                         (t_click - 0.1, at(px, py), "ease-in"), (t_click, at(px, py, 0.86), EASE), (t_click + 0.22, at(px, py)),
                         (T - 1.0, at(px, py), "ease-in"), (T - 0.7, at(px, py, 1, 0), "steps(1, end)"), (T - 0.69, at(sx, sy, 1, 0))],
                        rest=at(sx, sy, 1, 0), travel=True)
    clk.beat("the hand glides to OPEN THE PLAYGROUND", 1.1, 0.85)
    clk.beat("it presses: the button lights up", t_click - 0.1, 0.32)
    # still, the hand rests on the button
    b.append(f'<g class="hand {hand_cls}" transform="translate({px:.1f} {py:.1f})">{hand(1.3 if phone else 1.4)}</g>')
    desc = (f"A link to the playground at {PLAYGROUND.split('//')[1].rstrip('/')}: TRY IT IN YOUR BROWSER, with YOUR BROWSER on a highlighter mark, "
            "and a hand that presses the OPEN THE PLAYGROUND button, which lights up. Both engines run in the browser with "
            "no install, and the code you search stays on your machine.")
    return svg(w, H, "Try scrutate in your browser", desc, "\n".join(b),
               "\n".join([WIPE_CSS, SPOT_CSS, ".hand{transform-box:view-box;transform-origin:0 0}", clk.css()]), animated=True)


def hand(s):
    """A pointing hand, its fingertip at the origin: an ink outline under a paper fill, so the shapes merge."""
    parts = [(-2.6, 0, 5.2, 15, 2.6), (2.2, 7, 4.6, 10, 2.3), (6.4, 8.5, 4.4, 9, 2.2), (10.4, 10, 4, 8, 2),
             (-2.6, 13, 17, 11, 4.5), (-8, 12.5, 8, 4.6, 2.3)]

    def layer(extra):
        return "".join(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}"{extra}/>' for x, y, w, h, r in parts)

    return (f'<g transform="scale({s})">'
            f'<g fill="{INK}" stroke="{INK}" stroke-width="3.2" stroke-linejoin="round">{layer("")}</g>'
            f'<g fill="{PAPER}">{layer("")}</g></g>')


# ---------------------------------------------------------------- icons, badges, social preview

ICONS = {
    # 24 grid, 1.75 stroke, round caps and joins, no fill
    "regex": '<path d="M5.5 18.5h.01" stroke-width="3.2"/><path d="M14.5 4v12M9.3 7l10.4 6M19.7 7 9.3 13"/>',
    "ident": '<path d="M7 4.5c-1.7 0-2.5.8-2.5 2.5v3c0 1.2-.6 2-1.8 2 1.2 0 1.8.8 1.8 2v3c0 1.7.8 2.5 2.5 2.5"/><path d="M10 7h10M10 12h7M10 17h9"/>',
    "fuzzy": '<path d="M3 9c2-2.5 4-2.5 6 0s4 2.5 6 0 4-2.5 6 0"/><path d="M3 15c2-2.5 4-2.5 6 0s4 2.5 6 0 4-2.5 6 0"/>',
    "archive": '<rect x="3.5" y="4" width="17" height="5" rx="1.5"/><path d="M5 9v9.5c0 .8.7 1.5 1.5 1.5h11c.8 0 1.5-.7 1.5-1.5V9"/><path d="M10 13h4"/>',
    "textindex": '<rect x="3" y="4" width="5" height="5" rx="1"/><rect x="9.5" y="4" width="5" height="5" rx="1"/><rect x="16" y="4" width="5" height="5" rx="1"/><path d="M5.5 12.5v7M12 12.5v4M18.5 12.5v6"/>',
    "ranked": '<path d="M4 6h.01M4 12h.01M4 18h.01"/><path d="M8 6h12M8 12h8.5M8 18h5"/>',
    "callgraph": '<circle cx="12" cy="5" r="2.5"/><circle cx="5.5" cy="18.5" r="2.5"/><circle cx="18.5" cy="18.5" r="2.5"/><path d="M10.6 7.2 6.9 16.3M13.4 7.2l3.7 9.1M8 18.5h8"/>',
    "refs": '<circle cx="12" cy="12" r="3"/><path d="M12 3v4M12 17v4M3 12h4M17 12h4"/><circle cx="12" cy="12" r="8"/>',
    "dupes": '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5.5c0-.8-.7-1.5-1.5-1.5h-9C4.7 4 4 4.7 4 5.5v9c0 .8.7 1.5 1.5 1.5H8"/>',
    "structural": '<rect x="9" y="3" width="6" height="5" rx="1.2"/><rect x="3" y="16" width="6" height="5" rx="1.2"/><rect x="15" y="16" width="6" height="5" rx="1.2"/><path d="M12 8v4M6 16v-4h12v4"/>',
    "mcp": '<rect x="4" y="4" width="16" height="7" rx="2"/><rect x="4" y="13" width="16" height="7" rx="2"/><path d="M8 7.5h.01M8 16.5h.01M12 7.5h4M12 16.5h4"/>',
    "lsp": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 8.5h18"/><path d="M7 13h5M7 16.5h8"/><path d="M14.5 11.5v3"/>',
    "python": '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
    "watch": '<path d="M20 11a8 8 0 0 0-14.6-4.4L4 8.5"/><path d="M4 4v4.5h4.5"/><path d="M4 13a8 8 0 0 0 14.6 4.4L20 15.5"/><path d="M20 20v-4.5h-4.5"/>',
    "repos": '<path d="M3 7.5V18c0 .8.7 1.5 1.5 1.5h7"/><path d="M7 4h4l1.5 2H19c.8 0 1.5.7 1.5 1.5V14c0 .8-.7 1.5-1.5 1.5H8.5C7.7 15.5 7 14.8 7 14V4Z"/>',
    "doc": '<path d="M14 3H7c-.8 0-1.5.7-1.5 1.5v15c0 .8.7 1.5 1.5 1.5h10c.8 0 1.5-.7 1.5-1.5V7.5Z"/><path d="M14 3v4.5h4.5M9 12h6M9 15.5h6"/>',
    "terminal": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9.5 3 2.5-3 2.5M12.5 15h4.5"/>',
    "chart": '<path d="M4 4v16h16"/><path d="M8 16v-4M12 16V8M16 16v-6"/>',
    "shield": '<path d="M12 3 5 6v5.5c0 4.3 3 7.9 7 9.5 4-1.6 7-5.2 7-9.5V6Z"/><path d="m9 12 2 2 4-4"/>',
    "check": '<rect x="3.5" y="3.5" width="17" height="17" rx="2.5"/><path d="m8 12.5 2.8 2.7L16.5 9"/>',
    "flag": '<path d="M5 21V4"/><path d="M5 4.5c4-2 6 2 10 0s4 0 4 0v8.5s-1-2-4 0-6-2-10 0"/>',
    "book": '<path d="M4 5.5C4 4.7 4.7 4 5.5 4H11v16H5.5c-.8 0-1.5-.7-1.5-1.5Z"/><path d="M20 5.5c0-.8-.7-1.5-1.5-1.5H13v16h5.5c.8 0 1.5-.7 1.5-1.5Z"/>',
    "plug": '<path d="M9 3v4M15 3v4"/><path d="M6 7h12v3a6 6 0 0 1-12 0Z"/><path d="M12 16v5"/>',
    "language": '<path d="m8 7-5 5 5 5M16 7l5 5-5 5M13.5 4.5l-3 15"/>',
    "tag": '<path d="M3.5 12.6V5c0-.8.7-1.5 1.5-1.5h7.6l8 8a1.5 1.5 0 0 1 0 2.1l-7.5 7.5a1.5 1.5 0 0 1-2.1 0Z"/><path d="M8 8h.01"/>',
    "map": '<circle cx="12" cy="12" r="8.5"/><path d="m15.5 8.5-2 5-5 2 2-5Z"/>',
}
ICON_NAMES = {
    "regex": "literal and regex search", "ident": "identifier search in all casings", "fuzzy": "fuzzy search",
    "archive": "search inside archives", "textindex": "text index", "ranked": "ranked search", "callgraph": "call graph",
    "refs": "references", "dupes": "duplicates", "structural": "structural query", "mcp": "MCP server", "lsp": "language server",
    "python": "Python bindings", "watch": "an index kept fresh", "repos": "cross-repository search", "doc": "document",
    "terminal": "command line", "chart": "benchmarks", "shield": "security", "check": "checked results", "flag": "release",
    "book": "glossary", "plug": "integrations", "language": "languages", "tag": "name", "map": "documentation map",
}


def icon(name: str) -> str:
    """An ink chip with the icon in paper strokes, so one file reads on both page colours."""
    body = (f'<rect width="32" height="32" rx="6" fill="{INK}"/><g transform="translate(5 5) scale(.9167)" stroke="{PAPER}" '
            f'stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" fill="none">{ICONS[name]}</g>')
    return svg(32, 32, ICON_NAMES[name].capitalize(), f"Icon: {ICON_NAMES[name]}.", body)


def rust_version() -> str:
    """The workspace's `rust-version`, so the badge cannot say another one."""
    with open(os.path.join(ROOT, "Cargo.toml"), encoding="utf8") as f:
        return re.search(r'^rust-version = "([^"]+)"', f.read(), re.M).group(1)


BADGES = {"licence": ("licence", "MIT OR Apache-2.0"), "rust": ("rust", f"{rust_version()} or newer"), "platforms": ("platforms", "macOS  Linux  Windows")}


def badge(key: str) -> str:
    left, right = BADGES[key]
    lw = text_w(left.upper(), 11, True, 0.12) + 20
    rw = text_w(right, 12, True) + 18
    w, h = lw + rw, 24
    body = (f'<rect width="{w:.0f}" height="{h}" fill="{INK}"/>'
            f'<path d="M{lw:.0f} 6 V18" stroke="{ON["ink"]["rule"]}"/>'
            + mono(10, 16, left.upper(), 11, ON["ink"]["dim"], 0.12)
            + mono(lw + 9, 16.5, right, 12, PAPER))
    return svg(round(w), h, f"{left}: {right}", f"Badge: {left} {right}.", body)


def social() -> str:
    """The 1280 by 640 card a link to the repository shows."""
    SW, SH = 1280, 640
    spec = demo_spec()
    cmd = next(c for c in spec["commands"] if c.get("highlight"))
    rx = re.compile(cmd["highlight"])
    c = ON["ink"]
    b = [f'<rect width="{SW}" height="{SH}" fill="{INK}"/>']
    none = re.compile(r"(?!)")
    for i, ln in enumerate(cmd["output"][:16]):
        y = 70 + i * 36
        # no marks behind the wordmark: they would show through its counters
        s, _ = marked_line(64, y, ln, none if 160 < y < 350 else rx, 21, c["rule"], c["rule"])
        b.append(s)
    wm, ww = wordmark(0, 0, 150, PAPER, INK, knock=(INK, 26))
    b.append(f'<g transform="translate({(SW - ww) / 2:.1f} 330)">{wm}</g>')
    b.append(cut(48, 400, 760, 176, INK, 22))
    b.append(statement(72, 460, HERO_LINE[0], HERO_LINE[1], 40, "ink"))
    foot = " // ".join(HERO_SMALL + ("MIT OR APACHE-2.0",))
    fw = round(text_w(foot, 18, True, 0.16)) + 64 + 32
    b.append(f'<rect x="{SW - fw}" y="{SH - 78}" width="{fw}" height="78" fill="{INK}"/>')
    b.append(mono(SW - 64, SH - 40, foot, 18, c["dim"], 0.16, "end"))
    return svg(SW, SH, "Scrutate", f"Social preview: the wordmark SCRUTATE over real grep output with every match marked, and {HERO_DESC}.",
               "\n".join(b))


# ---------------------------------------------------------------- the font mode: Geist Mono Black to JSON

FONT_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/.,:;-+%$#()'_?&=<>*"


def _tables(data):
    n = struct.unpack(">H", data[4:6])[0]
    out = {}
    for i in range(n):
        tag, _, off, ln = struct.unpack(">4sIII", data[12 + 16 * i:28 + 16 * i])
        out[tag.decode()] = data[off:off + ln]
    return out


def _cmap(tab):
    n = struct.unpack(">H", tab[2:4])[0]
    for i in range(n):
        off = struct.unpack(">I", tab[8 + 8 * i:12 + 8 * i])[0]
        if struct.unpack(">H", tab[off:off + 2])[0] != 4:
            continue
        seg = struct.unpack(">H", tab[off + 6:off + 8])[0] // 2
        e = off + 14
        ends = struct.unpack(f">{seg}H", tab[e:e + 2 * seg])
        starts = struct.unpack(f">{seg}H", tab[e + 2 * seg + 2:e + 4 * seg + 2])
        deltas = struct.unpack(f">{seg}h", tab[e + 4 * seg + 2:e + 6 * seg + 2])
        ro = e + 6 * seg + 2
        ranges = struct.unpack(f">{seg}H", tab[ro:ro + 2 * seg])
        out = {}
        for s in range(seg):
            for ch in range(starts[s], min(ends[s], 0xFFFE) + 1):
                if ranges[s] == 0:
                    g = (ch + deltas[s]) & 0xFFFF
                else:
                    a = ro + 2 * s + ranges[s] + 2 * (ch - starts[s])
                    g = struct.unpack(">H", tab[a:a + 2])[0]
                    g = (g + deltas[s]) & 0xFFFF if g else 0
                if g:
                    out[ch] = g
        return out
    raise SystemExit("the font has no format 4 cmap")


def _index(d, p):
    count = struct.unpack(">H", d[p:p + 2])[0]
    if not count:
        return [], p + 2
    osz = d[p + 2]
    offs = [int.from_bytes(d[p + 3 + i * osz:p + 3 + (i + 1) * osz], "big") for i in range(count + 1)]
    base = p + 2 + (count + 1) * osz
    return [d[base + offs[i]:base + offs[i + 1]] for i in range(count)], base + offs[-1]


def _dict(b):
    out, ops, i = {}, [], 0
    while i < len(b):
        v = b[i]
        if v <= 21:
            key, i = (1200 + b[i + 1], i + 2) if v == 12 else (v, i + 1)
            out[key], ops = ops, []
        elif v == 28:
            ops.append(struct.unpack(">h", b[i + 1:i + 3])[0]); i += 3
        elif v == 29:
            ops.append(struct.unpack(">i", b[i + 1:i + 5])[0]); i += 5
        elif v == 30:   # a real number: skip its nibbles, these keys are not needed
            i += 1
            while (b[i] & 15) != 15 and (b[i] >> 4) != 15:
                i += 1
            i += 1
            ops.append(0)
        elif v <= 246:
            ops.append(v - 139); i += 1
        elif v <= 250:
            ops.append((v - 247) * 256 + b[i + 1] + 108); i += 2
        else:
            ops.append(-(v - 251) * 256 - b[i + 1] - 108); i += 2
    return out


def _charstring(cs, gsubrs, lsubrs):
    """A CFF Type 2 charstring to SVG path data in font units (y up)."""
    st, out, pos, stems = [], [], [0.0, 0.0], [0]

    def go(op, *pts):
        if op == "M" and out:
            out.append("Z")
        out.append(op + " ".join(f"{v:.0f}" for v in pts))

    def move(dx, dy):
        pos[0] += dx; pos[1] += dy; go("M", *pos)

    def line(dx, dy):
        pos[0] += dx; pos[1] += dy; go("L", *pos)

    def curve(a, b, c, d, e, f):
        x1, y1 = pos[0] + a, pos[1] + b
        x2, y2 = x1 + c, y1 + d
        pos[0], pos[1] = x2 + e, y2 + f
        go("C", x1, y1, x2, y2, *pos)

    def bias(subrs):
        return 107 if len(subrs) < 1240 else 1131 if len(subrs) < 33900 else 32768

    def run(code):
        i = 0
        while i < len(code):
            v = code[i]
            if v == 28:
                st.append(struct.unpack(">h", code[i + 1:i + 3])[0]); i += 3; continue
            if v >= 32:
                if v <= 246:
                    st.append(v - 139); i += 1
                elif v <= 250:
                    st.append((v - 247) * 256 + code[i + 1] + 108); i += 2
                elif v <= 254:
                    st.append(-(v - 251) * 256 - code[i + 1] - 108); i += 2
                else:
                    st.append(struct.unpack(">i", code[i + 1:i + 5])[0] / 65536); i += 5
                continue
            i += 1
            a = st[:]
            st.clear()
            if v in (1, 3, 18, 23, 19, 20):            # stems and hint masks: only their count matters
                stems[0] += len(a) // 2
                if v in (19, 20):
                    i += (stems[0] + 7) // 8
            elif v == 21:
                move(*a[-2:])
            elif v == 22:
                move(a[-1], 0)
            elif v == 4:
                move(0, a[-1])
            elif v == 5:
                for k in range(0, len(a), 2):
                    line(a[k], a[k + 1])
            elif v in (6, 7):
                h = v == 6
                for d in a:
                    line(d, 0) if h else line(0, d)
                    h = not h
            elif v == 8:
                for k in range(0, len(a), 6):
                    curve(*a[k:k + 6])
            elif v == 24:
                for k in range(0, len(a) - 2, 6):
                    curve(*a[k:k + 6])
                line(*a[-2:])
            elif v == 25:
                for k in range(0, len(a) - 6, 2):
                    line(a[k], a[k + 1])
                curve(*a[-6:])
            elif v in (26, 27):
                k, first = (1, a[0]) if len(a) % 4 else (0, 0)
                while k < len(a):
                    if v == 26:
                        curve(first, a[k], a[k + 1], a[k + 2], 0, a[k + 3])
                    else:
                        curve(a[k], first, a[k + 1], a[k + 2], a[k + 3], 0)
                    first, k = 0, k + 4
            elif v in (30, 31):
                h, k = v == 31, 0
                while k + 4 <= len(a):
                    last = len(a) - k == 5
                    e = a[k + 4] if last else 0
                    if h:
                        curve(a[k], 0, a[k + 1], a[k + 2], e, a[k + 3])
                    else:
                        curve(0, a[k], a[k + 1], a[k + 2], a[k + 3], e)
                    h, k = not h, k + (5 if last else 4)
            elif v in (10, 29):
                subrs = lsubrs if v == 10 else gsubrs
                st.extend(a[:-1])
                if run(subrs[int(a[-1]) + bias(subrs)]):
                    return True
            elif v == 11:
                st.extend(a)
                return False
            elif v == 14:
                return True
            else:
                raise SystemExit(f"charstring operator {v} is not handled")
        return False

    run(cs)
    return "".join(out) + "Z"


def font_json(otf: str) -> int:
    data = open(otf, "rb").read()
    t = _tables(data)
    upm = struct.unpack(">H", t["head"][18:20])[0]
    cff = t["CFF "]
    _, p = _index(cff, cff[2])
    tops, p = _index(cff, p)
    _, p = _index(cff, p)
    gsubrs, _ = _index(cff, p)
    top = _dict(tops[0])
    if 1230 in top:
        raise SystemExit("a CID-keyed CFF font is not handled")
    chars, _ = _index(cff, top[17][0])
    psize, poff = top[18]
    priv = _dict(cff[poff:poff + psize])
    lsubrs = _index(cff, poff + priv[19][0])[0] if 19 in priv else []
    cm = _cmap(t["cmap"])
    nh = struct.unpack(">H", t["hhea"][34:36])[0]
    ga = min(cm[ord("A")], nh - 1)
    adv = struct.unpack(">H", t["hmtx"][4 * ga:4 * ga + 2])[0]   # a monospace font: every letter's advance
    glyphs = {ch: _charstring(chars[cm[ord(ch)]], gsubrs, lsubrs) for ch in FONT_CHARS}
    out = {"name": "Geist Mono Black 1.700", "source": "https://github.com/vercel/geist-font (GeistMono-Black.otf)",
           "licence": "SIL Open Font License 1.1, tools/fonts/OFL.txt", "upm": upm, "cap": 710, "advance": adv,
           "glyphs": glyphs}
    os.makedirs(os.path.dirname(FONT_JSON), exist_ok=True)
    with open(FONT_JSON, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=0, sort_keys=True)
        f.write("\n")
    print(f"wrote {FONT_JSON}: {len(glyphs)} glyphs", file=sys.stderr)
    return 0


# ---------------------------------------------------------------- write / check

def outputs():
    out = {}
    out["logo-mark.svg"] = logo_mark()
    out["logo.svg"] = logo()
    out["hero.svg"] = hero()
    out["hero-phone.svg"] = hero(phone=True)
    out["engines.svg"] = engines()
    out["engines-phone.svg"] = engines(phone=True)
    for i, (name, n, s, *title) in enumerate(STRIPS):
        out[f"{name}.svg"] = strip(n, s, i, *title)
    out["social-preview.svg"] = social()
    out["cta-playground.svg"] = cta()
    out["cta-playground-phone.svg"] = cta(phone=True)
    for n in ICONS:
        out[f"icon-{n}.svg"] = icon(n)
    for k in BADGES:
        out[f"badge-{k}.svg"] = badge(k)
    return out


MOVED = {"transform", "opacity", "stroke-dashoffset"}
MAX_ANIMATED = 80


def motion_problems(text: str):
    """An animated image keeps the reduced-motion rule word for word; runs on one clock (one duration, no
    delays); animates only transform, opacity and stroke-dashoffset; each animated element is `mv` or `sm`;
    an `sm` element neither travels (no 2-D translate, no stroke-dashoffset) nor grows (no scale above 1.05);
    and it has at most MAX_ANIMATED animated elements."""
    if "@keyframes" not in text and "animation" not in text:
        return []
    bad = []
    if REDUCED not in text:
        bad.append("animated but without the reduced-motion rule")
    css = "".join(re.findall(r"<style>(.*?)</style>", text, re.S)).replace(REDUCED, "")
    if "animation-delay" in css:
        bad.append("uses animation-delay: every element runs on the one clock")
    frames = {m.group(1): m.group(2) for m in re.finditer(r"@keyframes\s+([\w-]+)\s*\{((?:[^{}]|\{[^{}]*\})*)\}", css)}
    for name, body in frames.items():
        props = {p.strip() for p in re.findall(r"([\w-]+)\s*:", body)} - {"animation-timing-function"}
        if props - MOVED:
            bad.append(f"@keyframes {name} animates {', '.join(sorted(props - MOVED))}")
    rules = re.findall(r"\.([\w-]+)\s*\{([^{}]*\banimation\s*:[^{}]*)\}", css)
    if len({re.search(r"animation\s*:\s*[\w-]+\s+([\d.]+)s", r).group(1) for _, r in rules}) > 1:
        bad.append("more than one loop length: one master clock per image")
    animated = {cls: r for cls, r in rules}
    used = 0
    for m in re.finditer(r'class="([^"]*)"', text):
        names = m.group(1).split()
        hit = [n for n in names if n in animated]
        if not hit:
            continue
        used += 1
        kind = {"mv", "sm"} & set(names)
        if len(kind) != 1:
            bad.append(f"animated element of class {m.group(1)!r} is neither mv nor sm (or both)")
        if "sm" in kind:
            for n in hit:
                body = frames.get(re.search(r"animation\s*:\s*([\w-]+)", animated[n]).group(1), "")
                grows = [float(s) for s in re.findall(r"\bscale\(([\d.]+)", body)]
                if "stroke-dashoffset" in body or re.search(r"\btranslate\(", body) or any(s > 1.05 for s in grows):
                    bad.append(f".{n} travels or grows but is sm: reduced motion would not stop it")
    if any(not re.fullmatch(r"k\d+", c) for c in animated):
        bad.append("an animation rule on a class the clock did not write")
    if used > MAX_ANIMATED:
        bad.append(f"{used} animated elements, over {MAX_ANIMATED}")
    return bad


def svg_problems(name: str, text: str):
    """The rules every image keeps."""
    bad = []
    try:
        xml.dom.minidom.parseString(text.encode())
    except Exception as e:  # noqa: BLE001
        bad.append(f"not valid XML ({e})")
    if len(text.encode()) > MAX_BYTES:
        bad.append(f"{len(text.encode()) // 1024} KB, over {MAX_BYTES // 1024} KB")
    if "<title" not in text or "<desc" not in text:
        bad.append("no title or desc")
    low = text.lower()
    for word in ("gradient", "<filter", "<script", "<set", "<animate", "@import", "@font-face", "<image", "<foreignobject"):
        if word in low:
            bad.append(f"contains {word!r}")
    if re.search(r"filter\s*[:=]", low):
        bad.append("contains a filter")
    if re.search(r"<[^<>]*\son[a-z]+\s*=", low):
        bad.append("has an event handler attribute")
    css = re.findall(r"<style[^>]*>(.*?)</style>", low, re.S) + re.findall(r"""\sstyle\s*=\s*(?:"[^"]*"|'[^']*')""", low)
    if any("\\" in c for c in css):
        bad.append("has a CSS escape, which can hide a reference")
    if re.search(r"""(?:href|src)\s*=\s*["'](?!#)""", low) or re.search(r"url\((?!#)", low) or \
            re.search(r"https?:", low.replace('xmlns="http://www.w3.org/2000/svg"', "")):
        bad.append("refers to something outside the file")
    bad += motion_problems(text)
    return [f"{name}: {p}" for p in bad]


# shapes svg_problems must refuse; --check feeds them to it, so a loosened rule fails the check
REFUSED = ['<rect onload="x()"/>', '<set attributeName="href" to="//e.com/x"/>', '<style>@IMPORT "x.css";</style>',
           '<rect style="fill:URL(x.svg)"/>', '<rect style="fill:u\\72l(x)"/>', '<rect filter="blur(2px)"/>',
           '<rect style="filter : blur(2px)"/>', '<a href="https://e.com">x</a>', "<use href='x.svg#a'/>"]
# motion that reduced motion would not stop: travel and growth under `sm`
REFUSED_MOTION = ["transform: translate(9px, 9px)", "transform: scale(1.5)", "stroke-dashoffset: -100"]


def checker_problems():
    head = '<svg xmlns="http://www.w3.org/2000/svg"><title>t</title><desc>d</desc>'
    bad = [f"the checker passes {b}" for b in REFUSED if not svg_problems("probe", head + b + "</svg>")]
    for v in REFUSED_MOTION:
        probe = (f"{head}<style>.k1{{animation:k1 2.00s linear infinite}} @keyframes k1 {{ 0% {{ {v}; }} }}\n{REDUCED}</style>"
                 '<rect class="sm k1"/></svg>')
        if not motion_problems(probe):
            bad.append(f"the checker passes an sm element with {v}")
    return bad


def readme_problems():
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import gen_doc_assets  # noqa: E402  (imports this module; its link checker is the one the documents use)
    problems = []
    with open(os.path.join(ROOT, "README.md"), encoding="utf-8") as f:
        gen_doc_assets.check_links("README.md", f.read(), problems)
    return problems


def write_or_check(files: dict, directory: str, check: bool) -> int:
    bad = 0
    for name, text in files.items():
        p = os.path.join(directory, name)
        for prob in svg_problems(name, text) if name.endswith(".svg") else []:
            print(prob, file=sys.stderr)
            bad = 1
        if check:
            cur = open(p, encoding="utf-8").read() if os.path.exists(p) else None
            if cur != text:
                print(f"differs: {os.path.relpath(p, ROOT)}", file=sys.stderr)
                bad = 1
        else:
            os.makedirs(directory, exist_ok=True)
            with open(p, "w", encoding="utf-8") as f:
                f.write(text)
    if not check:
        print(f"wrote {len(files)} files in {os.path.relpath(directory, ROOT)}", file=sys.stderr)
    return bad


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("what", nargs="?", choices=["capture", "capture-hero", "font", "beats"])
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--bin")
    ap.add_argument("--repo")
    ap.add_argument("--otf")
    a = ap.parse_args()
    if a.what in ("capture", "capture-hero"):
        if not (a.bin and a.repo):
            ap.error(f"{a.what} needs --bin and --repo")
        # the commands run inside --repo, so a relative --bin must not be read from there
        return (capture if a.what == "capture" else capture_hero)(os.path.abspath(a.bin), a.repo)
    if a.what == "font":
        if not a.otf:
            ap.error("font needs --otf")
        return font_json(a.otf)
    if a.what == "beats":
        print(_hero()[1].sheet())
        return 0
    bad = write_or_check(outputs(), ASSETS, a.check)
    if a.check:
        for p in checker_problems() + readme_problems():
            print(p, file=sys.stderr)
            bad = 1
    return bad


if __name__ == "__main__":
    sys.exit(main())
