#!/usr/bin/env python3
"""Write the documentation hub, the head of every document, and the banners, icons and diagrams.

Run from anywhere (Python 3.9 or newer, standard library only):

    python3 tools/gen_doc_assets.py            # write docs/README.md, the heads and docs/assets/banner-*, diagram-*
    python3 tools/gen_doc_assets.py --check    # fail when a committed file differs from what this script writes,
                                               # an SVG is malformed or too large, a link or #anchor is broken,
                                               # or a content revision in the docs differs from index.rs

The images are drawn in the README's system (tools/gen_assets.py: its tokens, parts and outlined type):
flat ink or paper panels, one file each, and the highlighter colour only on what a diagram is about.
A drawing names colour tokens (@fg@, @dim@, @rule@, @mark@ ...) and its panel, ink or paper, fills them in.
The heads of the generated documents are written by their own generators (DOC_HEAD in each); --check
compares those with the head this script would write.
"""

import argparse
import ast
import os
import re
import sys
from xml.sax.saxutils import escape

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
ASSETS = os.path.join(ROOT, "docs", "assets")

# the palette, the type, the parts and the image rules are the README's: one source, so the two sets cannot drift apart
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_assets as G  # noqa: E402
from gen_assets import INK, PAPER, MARK, ALERT, ON, W, PAD, SANS, MONO, REDUCED  # noqa: E402

WRITTEN = "<!-- written by tools/gen_doc_assets.py; edit that, not this file -->"

# Icons on a 24 px grid, strokes only.
ICONS = {
    "docs": ["M4 5.5C4 4.7 4.7 4 5.5 4H10a2 2 0 0 1 2 2v14a2 2 0 0 0-2-2H4z",
             "M20 5.5c0-.8-.7-1.5-1.5-1.5H14a2 2 0 0 0-2 2v14a2 2 0 0 1 2-2h6z"],
    "install": ["M12 4v11", "M7.5 10.5 12 15l4.5-4.5", "M5 19.5h14"],
    "cli": ["M5 4.5h14a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-11a2 2 0 0 1 2-2z", "M7 9.5l3 2.5-3 2.5", "M12.5 15h4.5"],
    "plug": ["M9 3v5", "M15 3v5", "M6.5 8h11v3a5.5 5.5 0 0 1-11 0z", "M12 16.5V21"],
    "editor": ["M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z", "M3 8.5h18",
               "M10 12l-2 2 2 2", "M14 12l2 2-2 2"],
    "play": ["M7 3h10a4 4 0 0 1 4 4v10a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4V7a4 4 0 0 1 4-4z", "M10 8.5v7l6-3.5z"],
    "commit": ["M3 12h5.5", "M15.5 12H21", "M12 8.5a3.5 3.5 0 1 1 0 7 3.5 3.5 0 0 1 0-7z"],
    "layers": ["M12 3 3 7.5l9 4.5 9-4.5z", "M3 12l9 4.5 9-4.5", "M3 16.5 12 21l9-4.5"],
    "braces": ["M8.5 4C6.5 4 6 5 6 7v2c0 1.5-1 3-2.5 3C5 12 6 13.5 6 15v2c0 2 .5 3 2.5 3",
               "M15.5 4c2 0 2.5 1 2.5 3v2c0 1.5 1 3 2.5 3-1.5 0-2.5 1.5-2.5 3v2c0 2-.5 3-2.5 3"],
    "book": ["M6 3h11.5a1 1 0 0 1 1 1v16a1 1 0 0 1-1 1H6a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z", "M8 7.5h6.5", "M8 11h6.5", "M4 17.5a2 2 0 0 1 2-2h12.5"],
    "podium": ["M4 20v-7h5v7", "M9 20V8h6v12", "M15 20v-5h5v5", "M3 20h18"],
    "stopwatch": ["M12 5.5a7.5 7.5 0 1 1 0 15 7.5 7.5 0 0 1 0-15z", "M12 13V9.5", "M10 2.5h4", "M12 2.5v3"],
    "target": ["M12 3.5a8.5 8.5 0 1 1 0 17 8.5 8.5 0 0 1 0-17z", "M12 7.5a4.5 4.5 0 1 1 0 9 4.5 4.5 0 0 1 0-9z",
               "M12 11.25a.75.75 0 1 1 0 1.5.75.75 0 0 1 0-1.5z"],
    "checklist": ["M4 6.5l1.5 1.5 3-3", "M11.5 7H20", "M4 13.5l1.5 1.5 3-3", "M11.5 14H20", "M11.5 19.5H20"],
    "shield": ["M12 3 4.5 6v5.5c0 4.5 3.2 8 7.5 9.5 4.3-1.5 7.5-5 7.5-9.5V6z", "M9 12l2 2 4-4"],
    "link": ["M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1", "M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"],
    "flag": ["M5 21V4", "M5 4h11.5l-2.5 4 2.5 4H5"],
    "scales": ["M12 4v16", "M7.5 20h9", "M5 7h14", "M5 7l-3 6a3 3 0 0 0 6 0z", "M19 7l-3 6a3 3 0 0 0 6 0z"],
    "pull": ["M6 3.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5z", "M6 15.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5z", "M6 8.5v7",
             "M18 15.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5z", "M18 15.5V10a3 3 0 0 0-3-3h-3", "M14 5l-2 2 2 2"],
    "tag": ["M3 12V4.5A1.5 1.5 0 0 1 4.5 3H12l9 9-9 9z", "M7.5 6.5a1 1 0 1 1 0 2 1 1 0 0 1 0-2z"],
    "box": ["M12 3l8 4.5v9L12 21l-8-4.5v-9z", "M4 7.5l8 4.5 8-4.5", "M12 12v9"],
    "graph": ["M6 3.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5z", "M18 3.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5z",
              "M12 15.5a2.5 2.5 0 1 1 0 5 2.5 2.5 0 0 1 0-5z", "M7.2 8.3l3.6 7.4", "M16.8 8.3l-3.6 7.4"],
    "map": ["M9 4 3 6.5V20l6-2.5 6 2.5 6-2.5V4l-6 2.5z", "M9 4v13.5", "M15 6.5V20"],
    "flask": ["M9.5 3h5", "M10 3v6l-5.5 9.5A1.7 1.7 0 0 0 6 21h12a1.7 1.7 0 0 0 1.5-2.5L14 9V3", "M7 15h10"],
    "type": ["M5 7.5V5h14v2.5", "M12 5v14", "M9 19h6"],
    "chart": ["M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z", "M7 15l3-3 3 2 4-5"],
    "archive": ["M4 4h16a1 1 0 0 1 1 1v3a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1z", "M5 9v10h14V9", "M10 13h4"],
    "dice": ["M7 4h10a3 3 0 0 1 3 3v10a3 3 0 0 1-3 3H7a3 3 0 0 1-3-3V7a3 3 0 0 1 3-3z",
             "M9 8.25a.75.75 0 1 1 0 1.5.75.75 0 0 1 0-1.5z", "M12 11.25a.75.75 0 1 1 0 1.5.75.75 0 0 1 0-1.5z",
             "M15 14.25a.75.75 0 1 1 0 1.5.75.75 0 0 1 0-1.5z"],
    "code": ["M8 8l-4 4 4 4", "M16 8l4 4-4 4", "M13.5 6l-3 12"],
    "history": ["M3.5 12a8.5 8.5 0 1 0 2.5-6", "M3 3.5V8h4.5", "M12 8v4l3 2"],
    "question": ["M12 3.5a8.5 8.5 0 1 1 0 17 8.5 8.5 0 0 1 0-17z", "M9.5 9.5a2.5 2.5 0 1 1 3.4 2.3c-.6.3-.9.8-.9 1.4v.8",
                 "M12 16.25a.75.75 0 1 1 0 1.5.75.75 0 0 1 0-1.5z"],
}

# One row per document: key, path from the root, banner title, the question it answers, icon, group, contents line.
# A row whose key starts with "+" is on the hub only (no head is written into that file).
DOCS = [
    ("faq", "docs/FAQ.md", "Questions", "What is scrutate, and what do people ask first?", "question", "use", False),
    ("install","docs/INSTALL.md", "Install", "How do I install, update and remove scrutate?", "install", "use", False),
    ("cli", "docs/CLI.md", "Command line", "What does each command do, and what does it print?", "cli", "use", True),
    ("mcp", "integrations/mcp/README.md", "MCP server", "How do I give an agent scrutate's tools?", "plug", "use", False),
    ("vscode", "integrations/vscode/README.md", "VS Code", "How do I use the language server in VS Code?", "editor", "use", False),
    ("neovim", "integrations/neovim/README.md", "Neovim", "How do I use scrutate from Neovim?", "editor", "use", False),
    ("jetbrains", "integrations/jetbrains/README.md", "JetBrains IDEs", "How do I run scrutate from a JetBrains IDE?", "editor", "use", False),
    ("github", "integrations/github/README.md", "GitHub Actions", "How do I run scrutate in a GitHub workflow?", "play", "use", False),
    ("precommit", "integrations/pre-commit/README.md", "pre-commit hook", "How do I stop a commit that adds duplicate functions?", "commit", "use", False),
    ("+python", "crates/scrutate-py/README.md", "Python bindings", "How do I call both engines from Python?", "code", "use", False),
    ("architecture", "docs/ARCHITECTURE.md", "Architecture", "How do the two engines work inside?", "layers", "understand", True),
    ("languages", "docs/LANGUAGES.md", "Languages", "What does the index find in each language?", "braces", "understand", False),
    ("glossary", "GLOSSARY.md", "Glossary", "What does each word of this codebase mean?", "book", "understand", False),
    ("leaderboard", "docs/LEADERBOARD.md", "Leaderboard", "Where is scrutate faster or slower, and how sure is that?", "podium", "trust", False),
    ("benchmarks", "docs/BENCHMARKS.md", "Benchmarks", "What do time, memory and start-up cost, tool by tool?", "stopwatch", "trust", False),
    ("quality", "docs/QUALITY.md", "Ranking quality", "How good are ranked answers on code it was not fitted on?", "target", "trust", True),
    ("test-results", "docs/TEST_RESULTS.md", "Test results", "Did every quality gate pass, and what did it print?", "checklist", "trust", False),
    ("security", "docs/SECURITY.md", "Security", "What can a hostile repository, query or client do?", "shield", "trust", True),
    ("traceability", "docs/TRACEABILITY.md", "Traceability", "Which requirement is met, and what is the evidence?", "link", "trust", True),
    ("release-readiness", "docs/RELEASE_READINESS.md", "Release readiness", "Is 0.1.0 ready, and what is still missing?", "flag", "trust", False),
    ("competitive", "docs/COMPETITIVE.md", "Competitive position", "What does the evidence say against other tools?", "scales", "trust", False),
    ("+dashboard", "docs/dashboard.html", "Benchmark dashboard", "The benchmark results as charts (open it in a browser)", "chart", "trust", False),
    ("contributing", "CONTRIBUTING.md", "Contributing", "What must pass before a change is merged?", "pull", "work", False),
    ("releasing", "docs/RELEASING.md", "Releasing", "Which steps does a release take, in order?", "tag", "work", True),
    ("delivery", "docs/DELIVERY.md", "Delivery index", "Where is each delivered item in this repository?", "box", "work", False),
    ("dependencies", "docs/DEPENDENCIES.md", "Dependencies", "Which crates and data are compiled in, and why?", "graph", "work", False),
    ("roadmap", "docs/ROADMAP.md", "Roadmap", "What is done, what is blocked and what is next?", "map", "work", True),
    ("research", "docs/RESEARCH.md", "Research", "Would a small transformer model rank code better?", "flask", "work", False),
    ("naming", "docs/NAMING.md", "Naming", "Why is it called scrutate, and how was that checked?", "type", "work", False),
    ("+changelog", "CHANGELOG.md", "Changelog", "What changed in each version?", "history", "work", False),
    ("+packaging", "packaging/README.md", "Packaging", "Which package definitions exist, and how was each tested?", "archive", "work", False),
    ("+fuzz", "fuzz/README.md", "Fuzz targets", "What does each fuzz target check?", "dice", "work", False),
    ("+cla", "CLA.md", "Contributor licence agreement", "What rights do I give with a contribution, and what do I keep?", "scales", "work", False),
    ("+trademarks", "TRADEMARKS.md", "Trademark policy", "What may a fork, a package or a product do with the name and the logo?", "tag", "work", False),
]
GROUPS = [
    ("use", "Use it", "Install it, run it, connect it to an editor, an agent or CI."),
    ("understand", "Understand it", "How the engines work and what the words mean."),
    ("trust", "Trust it", "Measurements, tests, security and the evidence behind every claim."),
    ("work", "Work on it", "How changes are made, released and planned."),
]
GROUP_ON = {"use": "paper", "understand": "ink", "trust": "paper", "work": "ink"}   # the panel each group's banners take
GENERATORS = {
    "docs/LEADERBOARD.md": "bench/make_leaderboard_md.py",
    "docs/BENCHMARKS.md": "bench/make_benchmarks_md.py",
    "docs/LANGUAGES.md": "tools/gen_languages_md.py",
    "docs/TEST_RESULTS.md": "tools/gen_test_results.py",
    "docs/DEPENDENCIES.md": "tools/gen_dependencies_md.py",
}
HUB = "docs/README.md"
HEAD_START = "<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->"
HEAD_END = "<!-- End of head. -->"


# ---------------------------------------------------------------- text measuring and SVG pieces

def spans(text):
    """Split `code` marks: a list of (text, is_code)."""
    parts = text.split("`")
    return [(p, i % 2 == 1) for i, p in enumerate(parts) if p]


def text_width(text, size):
    # Generous per-character widths, so a wider fallback font still fits.
    return sum(len(t) * (0.61 if code else 0.53) * size for t, code in spans(text))


def wrap(text, width, size):
    """Greedy wrap at spaces outside code marks; every line fits width with 10% to spare."""
    words, cur, inside = [], "", False
    for ch in text:
        if ch == "`":
            inside = not inside
        if ch == " " and not inside:
            words.append(cur)
            cur = ""
        else:
            cur += ch
    words.append(cur)
    lines, line = [], ""
    for w in words:
        cand = (line + " " + w) if line else w
        if text_width(cand, size) <= width * 0.9 or not line:
            line = cand
        else:
            lines.append(line)
            line = w
    lines.append(line)
    for ln in lines:
        if text_width(ln, size) > width * 0.9:
            raise SystemExit(f"text does not fit {width}px at {size}px: {ln!r}")
    return lines


def rich(x, y, text, cls, anchor=None):
    out = [f'<text x="{x:g}" y="{y:g}" class="{cls}"' + (f' text-anchor="{anchor}"' if anchor else "") + ">"]
    for t, code in spans(text):
        out.append(f'<tspan class="m">{escape(t)}</tspan>' if code else escape(t))
    out.append("</text>")
    return "".join(out)


STYLE = f"""
text {{ font-family: {SANS}; }}
.m {{ font-family: {MONO}; }}
.pre {{ white-space: pre; }}
.h {{ font-size: 15px; font-weight: 700; fill: @fg@; }}
.hm {{ font-size: 15px; font-weight: 700; fill: {INK}; }}
.b {{ font-size: 14px; fill: @dim@; }}
.bi {{ font-size: 14px; fill: @fg@; }}
.b .m, .bi .m {{ fill: @fg@; }}
.lbl {{ font-family: {MONO}; font-size: 11.5px; letter-spacing: 1.6px; fill: @dim@; }}
.card, .inset {{ fill: @bg@; stroke: @rule@; stroke-width: 1; }}
.store {{ fill: @bg@; stroke: @fg@; stroke-width: 1.25; }}
.out {{ fill: @bg@; stroke: @fg@; stroke-width: 1.5; }}
.arrow {{ fill: none; stroke: @dim@; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }}
.gate {{ fill: @bg@; stroke: @dim@; stroke-width: 1.25; stroke-dasharray: 6 5; }}
"""

# A diagram's loop: a block of the highlighter runs along each arrow in the order data flows, the card the
# flow ends in is marked when the last block arrives, it holds, the mark lifts and the flow starts again.
TRAVEL, HOLD = 1.1, 2.6

R_CARD = 4          # corner of a card inside a diagram
MARGIN, TOP = 32, 72  # where a diagram's drawing sits inside its panel, under its label


def tokens(on):
    c = ON[on]
    return {"fg": c["fg"], "dim": c["dim"], "rule": c["rule"], "bg": c["bg"], "mark": MARK, "alert": ALERT, "ink": INK}


def fill(src, on):
    t = tokens(on)
    out = re.sub(r"@(\w+)@", lambda m: t[m.group(1)], src)
    if re.search(r"@\w+@", out):
        raise SystemExit("unfilled colour token")
    return out


def document(w, h, title, desc, body, style):
    animated = "@keyframes" in style
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" '
            f'aria-labelledby="t d">\n{WRITTEN}\n<title id="t">{escape(title)}</title>\n<desc id="d">{escape(desc)}</desc>\n'
            f"<style>{style}{REDUCED + chr(10) if animated else ''}</style>\n{body}\n</svg>\n")


def outlined(text):
    """True when the headline font has every letter of `text` in capitals."""
    return "`" not in text and all(ch == " " or ch in G.font()["glyphs"] for ch in text.upper())


def lbl(x, y, text, anchor=None):
    """A small label in spaced mono capitals."""
    return f'<text x="{x:g}" y="{y:g}" class="lbl"' + (f' text-anchor="{anchor}"' if anchor else "") + f">{escape(text.upper())}</text>"


class Svg:
    def __init__(self, w, h, title, desc, on="ink"):
        self.w, self.h, self.title, self.desc, self.on = w, h, title, desc, on
        self.body, self.style, self.defs, self.flows, self.moves = [], [STYLE], [], [], {}
        self.lead = 0.6   # when the flow has arrived, for a diagram whose own motion takes longer

    def add(self, s):
        self.body.append(s)

    def move(self, name, fn):
        """An element of its own motion: its class is `@m:name@`, filled by fn(clock, the time the flow
        arrives, the loop's length) once the loop is known."""
        self.moves[name] = fn
        return f"@m:{name}@"

    def heading(self, x, y, text, marked=False, mono_title=False, room=None):
        """A card's title at baseline y: outlined capitals, or bold system text when it holds code.
        `marked` puts it on a highlighter slab that wipes in when the last block arrives."""
        cap = 11 if room is None else min(11, room / G.type_w(text.upper(), 1))   # a long title is set a little smaller
        plain = not mono_title and outlined(text) and cap >= 9.5
        t = f"`{text}`" if mono_title else text
        if plain:
            w, top, h = G.type_w(text.upper(), cap), y - cap - 6, cap + 12
        else:
            w, top, h = text_width(t, 15) * 0.93, y - 16, 22
        if marked:
            self.add(G.marker(x - 7, top, w + 14, h, "wipe @cls:slab@"))
        # a marked title is drawn twice: in the panel's colour, and in ink over it while the slab is there
        if plain:
            self.add(G.words(text.upper(), x, y, cap, "@fg@"))
            if marked:
                self.add(G.words(text.upper(), x, y, cap, INK, cls="@cls:ink@"))
        else:
            self.add(rich(x, y, t, "h"))
            if marked:
                self.add(rich(x, y, t, "hm @cls:ink@"))

    def box(self, x, y, w, h, cls):
        if cls == "store":   # a store has its top right corner cut, like the README's cards
            k = 12
            self.add(f'<path d="M{x:g} {y:g} H{x + w - k:g} L{x + w:g} {y + k:g} V{y + h:g} H{x:g} Z" class="store"/>')
        else:
            self.add(f'<rect x="{x:g}" y="{y:g}" width="{w:g}" height="{h:g}" rx="{R_CARD}" class="{cls}"/>')

    def card(self, x, y, w, title, text, cls="card", h=None, title_mono=False, marked=None):
        """A card with a title and wrapped body paragraphs; returns its height. An `out` card is marked."""
        lines = []
        for para in ([text] if isinstance(text, str) else text):
            lines += wrap(para, w - 32, 14)
        need = 30 + 20 * len(lines) + 12 if title else 14 + 20 * len(lines) + 4
        h = h or need
        if h < need:
            raise SystemExit(f"card {title!r} needs {need}px, has {h}")
        self.box(x, y, w, h, cls)
        ty = y + 26
        if title:
            self.heading(x + 16, y + 27, title, marked=(cls == "out") if marked is None else marked,
                         mono_title=title_mono, room=w - 32)
            ty += 22
        else:
            ty = y + 24
        for ln in lines:
            self.add(rich(x + 16, ty, ln, "b"))
            ty += 20
        return h

    @staticmethod
    def need(w, title, text):
        n = sum(len(wrap(p, w - 32, 14)) for p in ([text] if isinstance(text, str) else text))
        return 30 + 20 * n + 12

    def arrow(self, pts, flow=None, head=True, both=False):
        d = "M" + " L".join(f"{x:g} {y:g}" for x, y in pts)
        marks = (' marker-end="url(#ah)"' if head else "") + (' marker-start="url(#ah)"' if both else "")
        self.add(f'<path d="{d}" class="arrow"{marks}/>')
        if flow is not None:
            length = sum(abs(x1 - x0) + abs(y1 - y0) for (x0, y0), (x1, y1) in zip(pts, pts[1:]))
            dash = min(30.0, 10 / length * 100)  # a 10 px block, whatever the arrow's length
            self.flows.append((flow, dash))
            self.add(G.dash_path(d, f"@flow{len(self.flows)}@", f"{dash:.1f} 300", 9).replace(MARK, "@mark@"))

    def svg(self):
        """The drawing, scaled into a panel under a `// TITLE` label, with its loop."""
        clk = G.Clock()
        arrive = max([f + TRAVEL for f, _ in self.flows] + [self.lead])
        clk.total = round(arrive + 0.35 + HOLD + 0.7, 2)
        T = clk.total
        inner = "\n".join(self.body)
        for n, (flow, dash) in enumerate(self.flows, 1):
            inner = inner.replace(f"@flow{n}@", G.dash_run(clk, flow, flow + TRAVEL, dash=dash)[0])
        if self.flows:
            clk.beat("a block of highlighter runs along each arrow, in the order data flows", 0, arrive)
        if "@cls:slab@" in inner:
            inner = inner.replace("@cls:slab@", clk.anim(G.wipe(arrive, 0.32) + [(T - 0.6, "scaleX(1)", "ease-in"), (T - 0.3, "scaleX(0)")],
                                                   rest="scaleX(0)"))
            inner = inner.replace("@cls:ink@", clk.anim([(arrive + 0.2, 0, "steps(1, end)"), (arrive + 0.21, 1),
                                                   (T - 0.6, 1, "steps(1, end)"), (T - 0.59, 0)], "opacity", rest=0))
            clk.beat("the card the flow ends in is marked", arrive, 0.35)
        for name, fn in self.moves.items():
            inner = inner.replace(f"@m:{name}@", fn(clk, arrive, T))
        clk.beat("hold, then the marks lift", arrive + 0.35, T - arrive - 0.35)
        if clk.tracks:
            self.style.append("\n" + G.WIPE_CSS + "\n" + G.SPOT_CSS + "\n" + clk.css() + "\n")
        sc = (W - 2 * MARGIN) / self.w
        H = round(TOP + self.h * sc + 30)
        head = ('<marker id="ah" viewBox="0 0 10 10" refX="8.5" refY="5" markerWidth="6.5" markerHeight="6.5" '
                'orient="auto-start-reverse"><path d="M1.5 1.5 8.5 5 1.5 8.5" fill="none" stroke="@dim@" '
                'stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></marker>')
        body = "\n".join([
            G.panel(H, self.on, dots=self.on == "ink"),
            G.label(MARGIN + 8, 46, "", self.title, self.on, W - MARGIN - 8),
            "<defs>" + head + "".join(self.defs) + "</defs>",
            f'<g transform="translate({MARGIN} {TOP}) scale({sc:.5f})">',
            inner,
            "</g>",
        ])
        src = document(W, H, self.title, self.desc, body, "".join(self.style))
        return fill(src, self.on)


def marked_at(t):
    """A slab of a diagram's own that wipes in at t and lifts with the loop's other marks."""
    return lambda clk, arrive, T: "wipe " + clk.anim(G.wipe(t, 0.3) + [(T - 0.6, "scaleX(1)", "ease-in"), (T - 0.3, "scaleX(0)")],
                                                     rest="scaleX(0)")


def hops(stops, t0, step):
    """A reticle that appears on the first of `stops`, settles on each in turn, rests on the last and goes."""
    def fn(clk, arrive, T):
        def at(p, s, o=1):
            return {"transform": f"translate({p[0]:g}px, {p[1]:g}px) scale({s})", "opacity": o}
        pts = [(t0 - 0.2, at(stops[0], 1.7, 0), G.EASE), (t0 + 0.2, at(stops[0], 1))]
        for k, p in enumerate(stops[1:], 1):
            t = t0 + step * k
            pts += [(t - 0.28, at(stops[k - 1], 1), G.EASE_IN_OUT), (t, at(p, 1.25), G.EASE), (t + 0.14, at(p, 1))]
        pts += [(T - 0.6, at(stops[-1], 1), "ease-in"), (T - 0.3, at(stops[-1], 1.4, 0))]
        return clk.anim(pts, rest=at(stops[-1], 1.4, 0), travel=True)
    return fn


def reticle_box(cls, w, h, colour="@fg@"):
    """Corner ticks round a w by h box centred on 0 0; hidden until its motion shows it."""
    return f'<g class="{cls}" opacity="0">' + G.ticks(-w / 2, -h / 2, w, h, colour, n=min(10, w / 3)) + "</g>"


def icon_paths(name, stroke, width=1.75):
    return "".join(f'<path d="{d}" fill="none" stroke="{stroke}" stroke-width="{width}" '
                   'stroke-linecap="round" stroke-linejoin="round"/>' for d in ICONS[name])


# ---------------------------------------------------------------- banners, group strips and icons

def group_no(g):
    return f"{[k for k, _, _ in GROUPS].index(g) + 1:02d}"


def banner(key, title, line, icon, group=None):
    """A document's banner: its group label, its title in outlined capitals with a highlighter cursor, the
    question it answers and its icon. The hub's banner (no group) is taller."""
    on = GROUP_ON[group] if group else "ink"
    c = ON[on]
    hub_banner = group is None
    H = 168 if hub_banner else 150
    b = [G.panel(H, on)]
    if hub_banner:
        tag_text = "SCRUTATE // DOCS"
        tw = G.text_w(tag_text, 11, True, 0.14) + 18
        b.append(G.label(PAD, 44, "", f"{len(DOCS)} documents // {len(GROUPS)} groups", on, W - PAD - tw - 16))
        b.append(G.tag(W - PAD - tw, 30, tag_text, on)[0])
    else:
        b.append(G.label(PAD, 44, group_no(group), next(n for g, n, _ in GROUPS if g == group), on, W - PAD))
    t = title.upper()
    room = W - 2 * PAD - 110
    cap = min(46 if hub_banner else 34, room / (G.type_w(t, 1) + 0.8))
    base = 114 if hub_banner else 98
    b.append(G.words(t, PAD, base, cap, c["fg"]))
    cx = PAD + G.type_w(t, cap) + cap * 0.2
    # the loop: the reticle round the icon closes on it and settles, holds, and lets go; the cursor blinks
    clk = G.Clock(5.2)
    edge = f' stroke="{INK}" stroke-width="1.5"' if on == "paper" else ""   # on paper an ink edge keeps the highlighter visible
    b.append(f'<rect class="{G.blink(clk, 0, clk.total, clk.total / 5)}" x="{cx:.1f}" y="{base - cap:.1f}" '
             f'width="{cap * 0.58:.1f}" height="{cap:.1f}" fill="{MARK}"{edge}/>')
    if text_width(line, 15) > room:
        raise SystemExit(f"banner line too long: {key}")
    b.append(rich(PAD, base + 31, line, "q"))
    s = 60
    ix, iy = W - PAD - s, (H - s) / 2 + 12
    settle = clk.anim([(0.4, {"transform": "scale(1.45)", "opacity": 0}, G.EASE), (0.95, {"transform": "scale(1)", "opacity": 1}),
                       (4.5, {"transform": "scale(1)", "opacity": 1}, "ease-in"), (4.9, {"transform": "scale(1.2)", "opacity": 0})],
                      rest={"transform": "scale(1.2)", "opacity": 0}, travel=True)
    clk.beat("the reticle closes on the icon and settles", 0.4, 0.55)
    clk.beat("it holds, then lets go; the cursor blinks throughout", 0.95, 3.95)
    b.append(f'<g class="{settle} spot">' + G.ticks(ix, iy, s, s, c["dim"]) + "</g>")
    k = 1.9
    b.append(f'<g transform="translate({ix + (s - 24 * k) / 2:.1f} {iy + (s - 24 * k) / 2:.1f}) scale({k})">{icon_paths(icon, c["fg"])}</g>')
    style = (f"\n.m {{ font-family: {MONO}; }} .pre {{ white-space: pre; }}\n"
             f"text {{ font-family: {SANS}; }} .q {{ font-size: 15px; fill: {c['dim']}; }}\n"
             + G.SPOT_CSS + "\n" + clk.css() + "\n")
    return document(W, H, f"{title}: scrutate documentation", f"Banner for the {title} page. {line}", "\n".join(b), style)


def group_strip(g, name):
    """The band before each group on the hub: `// 01  USE IT`."""
    on = GROUP_ON[g]
    c = ON[on]
    H, n = 84, group_no(g)
    b = [G.panel(H, on, frame=False), G.words("//", PAD, 56, 26, c["dim"]), G.words(n, PAD + 58, 56, 26, c["fg"])]
    x = PAD + 58 + G.type_w(n, 26) + 26
    b.append(G.words(name.upper(), x, 53, 19, c["fg"]))
    end = x + G.type_w(name.upper(), 19) + 22
    b.append(f'<path d="M{end:.1f} 44 H{W - PAD - 34}" stroke="{c["rule"]}"/>')
    clk = G.Clock(4.8)
    b.append(G.seek(clk, end, W - PAD - 24, 44, W - PAD - 12, 8, c["dim"], t0=0.3 + 0.3 * (int(n) % 3), on=on))
    style = "\n" + G.SPOT_CSS + "\n" + clk.css() + "\n"
    return document(W, H, f"{n}: {name}", f"Group {n} of the documentation hub: {name.lower()}.", "\n".join(b), style)


def icon_svg(name, title):
    """An ink chip with the icon in paper strokes, as the README's icons: one file reads on both page colours."""
    body = (f'<rect width="32" height="32" rx="6" fill="{INK}"/><g transform="translate(5 5) scale(.9167)">'
            f'{icon_paths(name, PAPER, 1.9)}</g>')
    return document(32, 32, title, f"Icon for {title}.", body, "")


# ---------------------------------------------------------------- diagrams

def diagram_engine_a():
    s = Svg(880, 510, "Engine A: from the command line to output",
            "The command line is parsed, the ignore-aware parallel walker visits the files, each file's first read "
            "decides its path (small file, stream reader, split file, long lines), and the output is batched per file.")
    w3, top = 262, 1
    cards = [
        ("Command line", ["`scrutate grep PATTERN`", "Arguments of the plain shapes are read by hand, anything else by clap."]),
        ("Walk", ["The `ignore` parallel walker: `.gitignore`, `.ignore`, global excludes; hidden files off.",
                  "Threads: the cores (at most 12); on macOS the performance cores; more when files come from the disk."]),
        ("First read of a file", ["64 KB and one byte. A NUL byte or a byte-order mark sends the file to the stream reader."]),
    ]
    h1 = max(Svg.need(w3, t, b) for t, b in cards)
    xs = [1, 309, 617]
    for x, (t, b) in zip(xs, cards):
        s.card(x, top, w3, t, b, h=h1)
    my = top + h1 / 2
    s.arrow([(xs[0] + w3 + 2, my), (xs[1] - 4, my)], flow=0)
    s.arrow([(xs[1] + w3 + 2, my), (xs[2] - 4, my)], flow=1.2)
    paths = [
        ("Small file", "A short first read is the whole file: searched in the thread's own buffer."),
        ("Larger file", "The stream reader, up to 1 MB with a searcher made for that file."),
        ("Split file", "Ranges of whole lines (8 MB each); one window of ranges per thread."),
        ("One long line", "No line break before the last byte: a plain -F literal reads it in two passes through small buffers; else one buffer of the file's size."),
    ]
    w4, y2 = 206, top + h1 + 84
    h2 = max(Svg.need(w4, t, b) for t, b in paths)
    px = [1, 225, 449, 673]
    for x, (t, b) in zip(px, paths):
        s.card(x, y2, w4, t, b, h=h2)
    # bracket over the two paths for files of 64 MB or more
    by = y2 - 12
    s.add(f'<path d="M{px[2] + 8} {by + 6} V{by} H{px[3] + w4 - 8} V{by + 6}" fill="none" stroke="@rule@" stroke-width="1.5"/>')
    s.add(lbl((px[2] + px[3] + w4) / 2, by - 8, "64 MB or more", "middle"))
    bus = top + h1 + 26
    cx3 = xs[2] + w3 / 2
    s.arrow([(cx3, top + h1 + 2), (cx3, bus)], head=False, flow=2.4)
    s.add(f'<path d="M{px[0] + w4 / 2} {bus} H{px[3] + w4 / 2}" class="arrow"/>')
    for i, x in enumerate(px):
        c = x + w4 / 2
        s.arrow([(c, bus), (c, y2 - 4)], flow=3.4)
    y3 = y2 + h2 + 50
    out = ["Matches are batched per file and sent to the caller's sink while other threads keep searching; "
           "plain piped output shares one 64 KB buffer, so the lines of many files leave in one write.",
           "Ctrl-C, `--max-results` or a closed pipe cancels the walk between files. `--sort` gives path order."]
    h3 = s.card(1, y3, 878, "Output", out, cls="out")
    for x in px:
        c = x + w4 / 2
        s.arrow([(c, y2 + h2 + 2), (c, y3 - 4)], flow=4.6)
    s.h = int(y3 + h3 + 2)
    return s


def diagram_engine_b():
    s = Svg(880, 560, "Engine B: one index update",
            "Walk, hash, parse and chunk the files; the chunks give rows for SQLite (symbols, call edges, imports) "
            "and arrays for a new segment (BM25 postings, int8 vectors); one transaction commits both.")
    w, xs = 194, [1, 229, 457, 685]
    row1 = [
        ("Walk", "Files the ignore rules let through; binary files and files over 1 MB are left out."),
        ("Hash", "Size, mtime and `ctime_ns` match: the fast path. Otherwise a SHA-1 of the content."),
        ("Parse", "Tree-sitter rules, a keyword heuristic, or line windows."),
        ("Chunks", "One per definition; every other line in overlapping 50-line windows."),
    ]
    h1 = max(Svg.need(w, t, b) for t, b in row1)
    for x, (t, b) in zip(xs, row1):
        s.card(x, 1, w, t, b, h=h1)
    my = 1 + h1 / 2
    for i in range(3):
        s.arrow([(xs[i] + w + 2, my), (xs[i + 1] - 4, my)], flow=i * 0.9)
    w2, x2 = 270, [1, 304, 607]
    row2 = [
        ("Symbols and call edges", "Chunk metadata and text, call edges (`refs`) and `imports`."),
        ("BM25 postings", "Per field, over a shared 63-bit token-hash vocabulary."),
        ("Vectors", "int8, two views per chunk (description, implementation); IVF cluster order from 20,000 rows."),
    ]
    y2 = 1 + h1 + 72
    h2 = max(Svg.need(w2, t, b) for t, b in row2)
    for x, (t, b) in zip(x2, row2):
        s.card(x, y2, w2, t, b, h=h2)
    bus = 1 + h1 + 34
    cx = xs[3] + w / 2
    s.arrow([(cx, 1 + h1 + 2), (cx, bus)], head=False, flow=2.7)
    s.add(f'<path d="M{x2[0] + w2 / 2} {bus} H{cx}" class="arrow"/>')
    for x in x2:
        s.arrow([(x + w2 / 2, bus), (x + w2 / 2, y2 - 4)], flow=3.6)
    y3 = y2 + h2 + 60
    sql = ["`files`, `chunks`, `refs`, `imports`, `meta` (with the segment manifest); WAL.",
           "One `BEGIN IMMEDIATE` transaction per update: readers never see a half state."]
    seg = ["`.scrutate/seg/sNNNNNN/`: immutable, memory-mapped `.npy` arrays.",
           "Written to a temporary directory, then renamed."]
    h3 = s.card(1, y3, w2, "SQLite `index.db`", sql, cls="store")
    hs = s.card(304, y3, 573, "Segment", seg, cls="store")
    s.arrow([(x2[0] + w2 / 2, y2 + h2 + 2), (x2[0] + w2 / 2, y3 - 4)], flow=4.5)
    for x in x2[1:]:
        s.arrow([(x + w2 / 2, y2 + h2 + 2), (x + w2 / 2, y3 - 4)], flow=4.5)
    ay = y3 + hs / 2
    s.arrow([(302, ay), (x2[0] + w2 + 4, ay)], flow=5.4)
    s.h = int(y3 + h3 + 2)
    return s


def diagram_query():
    s = Svg(880, 440, "Engine B: a ranked query",
            "A query gets candidates from field-weighted BM25, an exact name lookup and semantic nearest neighbours; "
            "48 features per candidate go through a LightGBM LambdaRank ensemble, which orders the results.")
    srcs = [
        ("BM25 by field", "Weights: name 10, qualified name 3, signature 3, docs 5, body 1, path 2."),
        ("Exact lookup", "The name or the qualified name."),
        ("Semantic neighbours", "The query encoded into the chunks' 256-d space; in a large segment, 48 IVF clusters probed."),
    ]
    sx, sw = 214, 330
    hs = [Svg.need(sw, t, b) for t, b in srcs]
    y, ys = 1, []
    for (t, b), h in zip(srcs, hs):
        s.card(sx, y, sw, t, b, h=h)
        ys.append((y, h))
        y += h + 16
    total = y - 16
    qh = Svg.need(170, "Query", "Words, a name or an identifier.")
    qy = 1 + (total - qh) / 2
    s.card(1, qy, 170, "Query", "Words, a name or an identifier.", h=qh)
    fx, fw = 590, 289
    feat = ("48 features per candidate", "Lexical scores and ranks per field, the z-scored semantic similarity of both "
            "views, literal hits, name coverage, the name as typed, kind, lengths, test-path flags.")
    s.card(fx, 1, fw, feat[0], feat[1], h=total)
    for i, (yy, h) in enumerate(ys):
        c = yy + h / 2
        s.arrow([(173, qy + qh / 2), (192, qy + qh / 2), (192, c), (sx - 4, c)], flow=0)
        s.arrow([(sx + sw + 2, c), (fx - 4, c)], flow=1.4)
    y2 = total + 1 + 56
    rk = "A LightGBM LambdaRank ensemble, exported to plain arrays and evaluated in Rust."
    res = "Ties broken by id. A test pins the features and scores of a small tree, bit for bit."
    h2 = max(Svg.need(fw, "Ranker", rk), Svg.need(sw, "Ranked results", res))
    s.card(fx, y2, fw, "Ranker", rk, h=h2)
    s.card(sx, y2, sw, "Ranked results", res, cls="out", h=h2)
    s.arrow([(fx + fw / 2, total + 3), (fx + fw / 2, y2 - 4)], flow=2.8)
    s.arrow([(fx - 2, y2 + h2 / 2), (sx + sw + 4, y2 + h2 / 2)], flow=4.0)
    s.h = int(y2 + h2 + 2)
    return s


def diagram_text_index():
    s = Svg(880, 600, "The text index: trigrams narrow the files, Engine A confirms",
            "When indexing, each file's byte trigrams and mask bytes go into immutable segments. When searching, "
            "the pattern becomes a trigram condition, the postings give candidate files, and Engine A's per-file "
            "searcher reads only those.")
    w, xs = 270, [1, 305, 609]
    s.add(lbl(1, 16, "When indexing"))
    r1 = [
        ("Files", "Exactly the files Engine A's default walk visits: the same walker, the same ignore rules.", "card"),
        ("Trigrams per file", "Byte trigrams, ASCII case folded. Each posting has two mask bytes: the bytes that "
         "follow, the positions modulo 8.", "card"),
        ("Segments", "Sorted trigram keys, delta + LEB128 posting lists. No file contents.", "store"),
    ]
    y1 = 30
    h1 = max(Svg.need(w, t, b) for t, b, _ in r1)
    for x, (t, b, c) in zip(xs, r1):
        s.card(x, y1, w, t, b, cls=c, h=h1)
    for i in range(2):
        s.arrow([(xs[i] + w + 2, y1 + h1 / 2), (xs[i + 1] - 4, y1 + h1 / 2)], flow=i * 1.0)
    y2 = y1 + h1 + 64
    s.add(lbl(1, y2 - 14, "When searching"))
    r2 = [
        ("Pattern", "Its regex syntax tree is walked: literals, small classes and alternations become string sets.", "card"),
        ("Trigram condition", "A boolean condition: necessary, never sufficient. Evaluated over the postings as bitsets.", "card"),
        ("Candidate files", "Strings of four bytes or more must line up. `kernel_thread` on the Linux tree: 164 "
         "files instead of 1,781.", "card"),
    ]
    h2 = max(Svg.need(w, t, b) for t, b, _ in r2)
    for x, (t, b, c) in zip(xs, r2):
        s.card(x, y2, w, t, b, cls=c, h=h2)
    for i in range(2):
        s.arrow([(xs[i] + w + 2, y2 + h2 / 2), (xs[i + 1] - 4, y2 + h2 / 2)], flow=2.2 + i * 1.0)
    c3 = xs[2] + w / 2
    s.arrow([(c3, y1 + h1 + 2), (c3, y2 - 4)], flow=2.2)
    y3 = y2 + h2 + 56
    fin = ["Engine A's own per-file searcher reads the candidate files from the source tree, with the same translated pattern.",
           "Results equal Engine A's on the indexed snapshot."]
    h3 = s.card(xs[1], y3, 878 - xs[1] + 1, "Engine A confirms", fin, cls="out")
    s.arrow([(c3, y2 + h2 + 2), (c3, y3 - 4)], flow=4.4)
    s.h = int(y3 + h3 + 2)
    return s


def diagram_front_ends():
    s = Svg(880, 520, "Front ends share one index directory",
            "The CLI, the Python bindings, the MCP server, the language server and the watcher go through the same "
            "operations to one .scrutate directory; writers take turns, readers answer from the last commit.")
    fe = [
        ("CLI", "Index commands and `batch`. `grep` never opens the index."),
        ("Python bindings", "Both engines, the GIL released."),
        ("MCP server", "`scrutate serve`: nineteen tools on stdio."),
        ("Language server", "`scrutate lsp`: a save updates the index on a second connection."),
        ("Watch", "`scrutate watch`, `serve --watch`: one more writer."),
    ]
    lw = 240
    hs = [Svg.need(lw, t, b) for t, b in fe]
    y, mids = 1, []
    for (t, b), h in zip(fe, hs):
        s.card(1, y, lw, t, b, h=h)
        mids.append(y + h / 2)
        y += h + 12
    total = y - 12
    mx, mw = 296, 224
    mid = ["Operations (`ops.rs`): every front end gets the same arguments, defaults and answers.",
           "`Repo` (`lib.rs`) keeps the index up to date and rebuilds a corrupt one."]
    mh = Svg.need(mw, "One engine", mid)
    my = (total - mh) / 2
    s.card(mx, my, mw, "One engine", mid, h=mh)
    for i, c in enumerate(mids):
        s.arrow([(lw + 3, c), (268, c), (268, my + mh / 2), (mx - 4, my + mh / 2)], flow=i * 0.4)
    rx, rw = 566, 313
    s.box(rx, 1, rw, total - 1, "store")
    s.heading(rx + 16, 27, "`.scrutate/`", marked=True)
    parts = [("`index.db`", "SQLite, WAL"), ("`seg/`", "memory-mapped arrays"),
             ("`text/`", "trigram segments"), ("`tsquery/`", "the `tsquery` cache")]
    py = 42
    for name, what in parts:
        s.add(f'<rect x="{rx + 12}" y="{py}" width="{rw - 24}" height="34" rx="2" class="inset"/>')
        if text_width(what, 14) > (rw - 24 - 110) * 0.9:
            raise SystemExit(f"front ends: {what} does not fit")
        s.add(rich(rx + 26, py + 22, name, "bi"))
        s.add(rich(rx + 122, py + 22, what, "b"))
        py += 42
    lock = ["Writers take turns: the SQLite transaction and the text index lock file.",
            "A read never waits for a writer: it answers from the last committed index."]
    ly = py + 8
    for para in lock:
        for ln in wrap(para, rw - 32, 14):
            ly += 20
            s.add(rich(rx + 16, ly, ln, "b"))
        ly += 6
    if ly + 12 > total:
        raise SystemExit("front ends: the .scrutate card is too short")
    s.arrow([(mx + mw + 2, my + mh / 2), (rx - 4, my + mh / 2)], flow=2.4)
    s.h = int(total + 2)
    return s


def diagram_cli_map():
    s = Svg(880, 700, "Command map: scrutate's commands by family",
            "Search without an index (grep, tsquery, files), search the index (search, find, grep --index, batch), "
            "code intelligence (symbol, callers, callees, refs, outline, deps, rdeps, similar, dupes, impact), "
            "maintenance (index, check, stats, languages, config, completions) and servers (serve, lsp, watch, setup, hook).")
    fams = [
        ("Search, no index", "fg", 130, [
            ("grep", "alias `g`: regex, literal, identifier, fuzzy"),
            ("tsquery", "tree-sitter structural query"),
            ("files", "a file by a misspelt name or path"),
        ]),
        ("Search the index", "fg", 130, [
            ("search", "alias `s`: ranked by meaning, name, identifier"),
            ("find", "regex-looking: `grep`; otherwise ranked"),
            ("grep --index", "`grep` through the text index"),
            ("batch", "JSON-lines requests, one index open"),
        ]),
        ("Code intelligence", "fg", 200, [
            ("symbol", "where it is defined"),
            ("callers / callees", "call graph edges resolved to definitions"),
            ("refs", "every whole-word occurrence, resolved to its definition"),
            ("outline", "symbols of a file in source order"),
            ("deps / rdeps", "imports of a file / files importing a module or file"),
            ("similar", "code similar to a symbol or `path:line`"),
            ("dupes", "exact and near-duplicate functions"),
            ("impact", "what a git diff changes, its callers and the tests to run"),
        ]),
        ("Maintenance", "fg", 130, [
            ("index", "build or update `.scrutate/`"),
            ("check", "verify the index"),
            ("stats", "index statistics"),
            ("languages", "the capability registry"),
            ("config", "the configuration file in effect"),
            ("completions", "shell completion scripts"),
        ]),
        ("Servers", "fg", 130, [
            ("serve", "MCP server on stdio"),
            ("lsp", "language server on stdio"),
            ("watch", "keep the index up to date"),
            ("setup", "the MCP entry in an agent's configuration"),
            ("hook", "Claude Code's Grep hook and its counter"),
        ]),
    ]
    boxes = [(1, 431), (447, 432), (1, 878), (1, 431), (447, 432)]
    rows = [(0, 1), (2,), (3, 4)]
    def laid(i):
        name, col, ncol, cmds = fams[i]
        w = boxes[i][1]
        out = []
        for cmd, what in cmds:
            if text_width(f"`{cmd}`", 14) > ncol - 12:
                raise SystemExit(f"cli map: {cmd} does not fit")
            out.append((cmd, wrap(what, w - 32 - ncol, 14)))
        return out

    y, stops = 1, []
    for row in rows:
        h = max(48 + sum(8 + 20 * len(ls) for _, ls in laid(i)) + 6 for i in row)
        for i in row:
            name, col, ncol, cmds = fams[i]
            x, w = boxes[i]
            s.add(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{R_CARD}" class="card"/>')
            s.add(f'<rect x="{x + 16}" y="{y + 16}" width="11" height="11" fill="@{col}@"/>')
            s.add(f'<rect class="{s.move(f"fam{i}", marked_at(0.55 + 0.5 * i))}" x="{x + 16}" y="{y + 16}" width="11" height="11" '
                  'fill="@mark@" transform="scale(0 1)"/>')
            stops.append((x + 21.5, y + 21.5))
            s.heading(x + 36, y + 27, name)
            cy = y + 56
            for cmd, ls in laid(i):
                s.add(rich(x + 16, cy, f"`{cmd}`", "bi"))
                for ln in ls:
                    s.add(rich(x + 16 + ncol, cy, ln, "b"))
                    cy += 20
                cy += 8
        y += h + 16
    s.add(reticle_box(s.move("seek", hops(sorted(stops, key=lambda p: (p[1], p[0])), 0.4, 0.5)), 30, 30))
    s.lead = 0.4 + 0.5 * (len(stops) - 1) + 0.5
    s.h = int(y - 16 + 2)
    return s


def diagram_exit_codes():
    s = Svg(880, 150, "Exit codes",
            "0: results found. 1: no results. 2: error. 130: interrupted.", on="paper")
    tiles = [("0", "mark", "results found", ""),
             ("1", "dim", "no results", ""),
             ("2", "alert", "error", "Bad arguments, an unreadable index, an invalid pattern..."),
             ("130", "alert", "interrupted", "Ctrl-C, or stopped early for a reason other than `--max-results`.")]
    w = 208
    hs = []
    for t in tiles:
        hs.append(70 + 20 * len(wrap(t[3], w - 32, 14)) + 10 if t[3] else 80)
    h = max(hs)
    for i, (code, col, label, text) in enumerate(tiles):
        x = 1 + i * (w + 15.67)
        s.add(f'<rect x="{x:g}" y="1" width="{w}" height="{h}" rx="{R_CARD}" class="card"/>')
        if col == "mark":   # the one code that means something was found
            mw = G.type_w(code, 22) + 20
            s.add(G.marker(x + 7, 13, mw, 34, s.move("zero", marked_at(0.75))))
            ret = reticle_box(s.move("seek", hops([(x + 7 + mw / 2, 30)], 0.35, 0)), mw + 16, 44)
        s.add(G.words(code, x + 16, 40, 22, "@ink@" if col == "mark" else f"@{col}@"))
        s.add(rich(x + 16, 62, label, "h"))
        yy = 86
        for ln in (wrap(text, w - 32, 14) if text else []):
            s.add(rich(x + 16, yy, ln, "b"))
            yy += 20
    s.add(ret)
    s.lead = 1.1
    s.h = int(h + 2)
    return s


def diagram_security():
    s = Svg(880, 800, "Trust boundaries",
            "What crosses into scrutate (repository content, patterns, MCP requests, a command line call to a running server) "
            "and what it writes (.scrutate), with the controls the security assessment names on the boundary each one guards. "
            "The network is not used.")
    sw, cx, cw, ex, ew = 196, 236, 414, 690, 189
    rows = [
        ("Repository content", "a hostile repository: crafted files, names, symlinks, `.gitignore`",
         ["Parsed as data: nothing is evaluated, imported or run. One exception: `grep -z` runs the system's "
          "decompressors on compressed files.",
          "Symlinks are not followed by default; `-L` follows with loop detection.",
          "Engine B skips binary files and files over 1 MB; the resolver does not read a file nested more than 2,000 levels.",
          "`follow = true` in the repository's own `.scrutate.toml` is ignored with a warning."]),
        ("Patterns", "a hostile query",
         ["Rust `regex`: linear time, no backtracking, size limits (20 MB regex, 10 MB DFA); an oversized pattern is an error."]),
        ("MCP client", "a hostile client: malformed JSON-RPC, path escapes",
         ["stdio only, no listening socket.",
          "A path that leaves the root by its own parts is refused with nothing looked at.",
          "Malformed input gets the JSON-RPC error the specification names; at most 32 call threads at once."]),
        ("Session socket", "another process on this machine: of this user, or of another",
         ["`.scrutate/session.sock`, mode 0600, while `serve` runs; no network port; the command line never starts a server.",
          "Both ends check the peer is the same user before a byte is read; a link or a file of another owner is refused.",
          "The command line does the work itself unless the server is the same build, settings and root."]),
        ("`.scrutate/`", "the index directory, possibly corrupted or planted",
         ["Written only under `<root>/.scrutate/`; segments go to a temporary directory there and are renamed.",
          "A `.scrutate` that is a link, or holds one, is refused.",
          "`PRAGMA quick_check` on open, arrays validated; a damaged index is rebuilt."]),
        ("Main checkout's index", "in a linked git work tree: another checkout's `.scrutate/`, read",
         ["Found from git's own files, no program run; the link must be confirmed both ways.",
          "Read only, in one snapshot; the same link checks; never written.",
          "An artifact is used only for bytes that hash to its key; anything that does not decode is a miss."]),
    ]
    y = 34
    s.add(lbl(1, 18, "Outside: untrusted"))
    s.add(lbl(cx, 18, "The boundary: the controls on it"))
    s.add(lbl(ex, 18, "Inside"))
    spans_y = []
    for title, who, ctrls in rows:
        lines = []
        for c in ctrls:
            lines.append(wrap(c, cw - 32, 14))
        ch = 16 + sum(20 * len(l) for l in lines) + 8 * (len(lines) - 1) + 12
        sh = Svg.need(sw, title, who)
        h = max(ch, sh)
        s.card(1, y, sw, title, who, h=h, cls="card")
        s.add(f'<rect x="{cx}" y="{y}" width="{cw}" height="{h}" rx="{R_CARD}" class="gate"/>')
        ty = y + 26
        for l in lines:
            for i, ln in enumerate(l):
                if i == 0:
                    s.add(f'<rect x="{cx + 15}" y="{ty - 8}" width="6" height="6" fill="@mark@"/>')
                s.add(rich(cx + 30, ty, ln, "bi" if False else "b"))
                ty += 20
            ty += 8
        mid = y + h / 2
        t = 0.6 * len(spans_y)
        if title == "`.scrutate/`":   # scrutate writes it, and reads it back through the same checks
            s.arrow([(ex - 4, mid), (cx + cw + 4, mid)], both=True, flow=t)
            s.arrow([(cx - 4, mid), (sw + 5, mid)], both=True, flow=t + 0.5)
        else:
            s.arrow([(sw + 5, mid), (cx - 4, mid)], flow=t)
            s.arrow([(cx + cw + 4, mid), (ex - 4, mid)], flow=t + 0.5)
        spans_y.append((y, h))
        y += h + 16
    top, bottom = spans_y[0][0], y - 16
    eng = ["Engine A, Engine B, the CLI, the MCP server and the Python bindings."]
    eh = bottom - top
    s.card(ex, top, ew, "scrutate", eng, cls="out", h=eh, title_mono=True)
    y = bottom + 24
    net = ["No network connections and no HTTP or DNS dependency: `cargo tree` has no networking crate."]
    nh = Svg.need(sw, "Network", "not used")
    nh = max(nh, 16 + 20 * len(wrap(net[0], cw - 32, 14)) + 12)
    s.add(f'<rect x="1" y="{y}" width="{sw}" height="{nh}" rx="{R_CARD}" fill="none" stroke="@rule@" stroke-dasharray="5 5"/>')
    s.heading(17, y + 27, "Network")
    s.add(rich(17, y + 48, "not used", "b"))
    s.add(f'<rect x="{cx}" y="{y}" width="{cw}" height="{nh}" rx="{R_CARD}" class="gate"/>')
    ty = y + 26
    for i, ln in enumerate(wrap(net[0], cw - 32, 14)):
        if i == 0:
            s.add(f'<rect x="{cx + 15}" y="{ty - 8}" width="6" height="6" fill="@mark@"/>')
        s.add(rich(cx + 30, ty, ln, "b"))
        ty += 20
    s.h = int(y + nh + 2)
    return s


def diagram_platforms():
    s = Svg(880, 420, "Platforms: what is built, tested and supported",
            "All six are supported, with binaries and wheels built, installed and tested: macOS arm64, Linux aarch64 "
            "and Linux x86_64 here, macOS x86_64, Windows x86_64 and Windows ARM64 on GitHub runners.", on="paper")
    rows = [
        ("macOS arm64", "Built and tested natively.", "cp39-abi3, cp314t", True),
        ("Linux aarch64", "Built and tested in a clean container; glibc and static musl.", "cp39-abi3, cp314t", True),
        ("Linux x86_64", "Cross-built on arm64; the Linux tests run in an amd64 container.", "cp39-abi3, cp314t", True),
        ("macOS x86_64", "Built and tested on GitHub's `macos-15-intel` runner.", "cp39-abi3, cp314t", True),
        ("Windows x86_64", "Built and tested on GitHub's `windows-latest` runner.", "cp39-abi3, cp314t", True),
        ("Windows ARM64", "Built and tested on GitHub's `windows-11-arm` runner.", "cp39-abi3, cp314t", True),
    ]
    cols = [(18, "Platform"), (176, "Binary"), (556, "Wheels"), (716, "Status")]
    for x, t in cols:
        s.add(lbl(x, 22, t))
    y = 36
    heights = [max(48, 14 + 20 * len(wrap(r[1], 360, 14)) + 14) for r in rows]
    total = sum(heights)
    s.add(f'<rect x="1" y="{y}" width="878" height="{total}" rx="{R_CARD}" class="card"/>')
    for (plat, binary, wheels, ok), h in zip(rows, heights):
        ty = y + 29 if h == 48 else y + 28
        s.add(rich(18, ty, plat, "h"))
        bl = wrap(binary, 360, 14)
        by = y + (h - 20 * len(bl)) / 2 + 14
        for ln in bl:
            s.add(rich(176, by, ln, "b"))
            by += 20
        s.add(rich(556, ty, wheels, "b" if ok else "b"))
        if ok:
            s.add(G.marker(712, y + h / 2 - 13, 112, 26, s.move(f"ok{y}", marked_at(0.4 + 0.3 * len(s.moves)))))
            s.add(f'<path d="M724 {y + h / 2} l4 4 7-8" fill="none" stroke="@ink@" stroke-width="1.9" stroke-linecap="square"/>')
            s.add(rich(742, y + h / 2 + 5, "supported", "bi").replace('class="bi"', 'class="bi" style="fill: @ink@"'))
        else:
            s.add(f'<rect x="712" y="{y + h / 2 - 13}" width="150" height="26" fill="none" stroke="@rule@" stroke-width="1.25"/>')
            s.add(rich(726, y + h / 2 + 5, "not supported yet", "b"))
        y += h
        if y < 36 + total:
            s.add(f'<line x1="2" y1="{y}" x2="878" y2="{y}" stroke="@rule@"/>')
    s.lead = 0.4 + 0.3 * len(s.moves)
    s.h = int(y + 2)
    return s


DIAGRAMS = {
    "engine-a": diagram_engine_a,
    "engine-b-update": diagram_engine_b,
    "ranked-query": diagram_query,
    "text-index": diagram_text_index,
    "front-ends": diagram_front_ends,
    "cli-map": diagram_cli_map,
    "exit-codes": diagram_exit_codes,
    "trust-boundaries": diagram_security,
    "platforms": diagram_platforms,
}


# ---------------------------------------------------------------- documents

def rel(frm, to):
    return os.path.relpath(os.path.join(ROOT, to), os.path.dirname(os.path.join(ROOT, frm))).replace(os.sep, "/")


def headed():
    return [d for d in DOCS if not d[0].startswith("+")]


def strip_code(md):
    """Lines outside fenced code blocks, with their numbers."""
    out, fence = [], None
    for i, ln in enumerate(md.split("\n")):
        m = re.match(r"^\s*(```+|~~~+)", ln)
        if m:
            if fence is None:
                fence = m.group(1)[0] * 3
            elif ln.strip().startswith(fence):
                fence = None
            continue
        if fence is None:
            out.append((i, ln))
    return out


def slug(heading):
    t = re.sub(r"<[^>]+>", "", heading).strip().lower()
    t = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", t)
    t = re.sub(r"[^\w\- ]", "", t, flags=re.UNICODE)
    return t.replace(" ", "-")


def headings(md):
    seen, out = {}, []
    for _, ln in strip_code(md):
        m = re.match(r"^(#{1,6})\s+(.*?)\s*#*\s*$", ln)
        if m:
            base = slug(m.group(2))
            n = seen.get(base, 0)
            seen[base] = n + 1
            out.append((len(m.group(1)), m.group(2), base if n == 0 else f"{base}-{n}"))
    return out


def banner_block(path, key, title, line):
    a = rel(path, "docs/assets")
    return ["<p align=\"center\">",
            f"<img src=\"{a}/banner-{key}.svg\" alt=\"{escape(title)}. {escape(line)}\" width=\"880\">",
            "</p>"]


def nav_line(doc):
    key, path, *_ = doc
    group = [d for d in headed() if d[5] == doc[5]]
    i = [d[0] for d in group].index(key)
    parts = [f"[Documentation]({rel(path, HUB)})"]
    if i > 0:
        p = group[i - 1]
        parts.append(f"Previous: [{p[2]}]({rel(path, p[1])})")
    if i + 1 < len(group):
        n = group[i + 1]
        parts.append(f"Next: [{n[2]}]({rel(path, n[1])})")
    return " &nbsp;·&nbsp; ".join(parts)


def toc_label(heading):
    # A heading's trailing file list, "(`ops.rs`, ...)", is left out of the contents line.
    return re.sub(r"\s*\([^)]*`[^)]*\)\s*$", "", heading).replace("`", "")


def head(doc, h1, body):
    key, path, title, line, icon, group, toc = doc
    out = [HEAD_START] + banner_block(path, key, title, line) + ["", h1, "", nav_line(doc)]
    if toc:
        hs = [(t, a) for lvl, t, a in headings(h1 + "\n" + body) if lvl == 2]
        out += ["", "**Contents**: " + " &nbsp;·&nbsp; ".join(f"[{toc_label(t)}](#{a})" for t, a in hs)]
    out += ["", HEAD_END]
    return "\n".join(out) + "\n"


def split_doc(text):
    """(h1, body) of a document with or without a head."""
    if text.startswith(HEAD_START):
        end = text.index(HEAD_END) + len(HEAD_END) + 1
        h1 = next(ln for ln in text[:end].split("\n") if ln.startswith("# "))
        return h1, text[end:]
    first, _, rest = text.partition("\n")
    if not first.startswith("# "):
        raise SystemExit("a document must start with its # title")
    return first, rest


def with_head(doc, text):
    h1, body = split_doc(text)
    return head(doc, h1, body) + body


def hub():
    out = [HEAD_START] + banner_block(HUB, "docs", "Documentation", "Every document, grouped by what you came to do") + [
        "", "# Documentation", "",
        "Each document links back here and to its neighbours in its group. A document written by a script "
        "says so at the top: change the script, not the page.",
        "", HEAD_END, "", "<table>"]
    # One table, so the columns line up from group to group; each group opens with its band.
    for g, name, line in GROUPS:
        out += ["<tr>", f'<th colspan="2" align="left"><img src="assets/banner-group-{g}.svg" width="100%" '
                f'alt="{group_no(g)} {escape(name)}"><br><sub>{escape(line)}</sub></th>', "</tr>"]
        docs = [d for d in DOCS if d[5] == g]
        for i in range(0, len(docs), 2):
            out.append("<tr>")
            for d in docs[i:i + 2]:
                key, path, title, q, icon = d[:5]
                ikey = key.lstrip("+")
                out += ['<td width="50%" valign="top">',
                        f'<img src="assets/banner-icon-{ikey}.svg" width="20" height="20" alt=""> '
                        f'<a href="{rel(HUB, path)}"><b>{escape(title)}</b></a><br>',
                        escape(q),
                        "</td>"]
            if len(docs[i:i + 2]) == 1:
                out.append('<td width="50%"></td>')
            out.append("</tr>")
    out += ["</table>", ""]
    return "\n".join(out)


# ---------------------------------------------------------------- writing and checking

def planned():
    """Every file this script owns, path -> content."""
    files = {}
    for d in DOCS:
        key, path, title, line, icon = d[:5]
        k = key.lstrip("+")
        files[f"docs/assets/banner-icon-{k}.svg"] = icon_svg(icon, title)
        if not key.startswith("+"):
            files[f"docs/assets/banner-{k}.svg"] = banner(k, title, line, icon, d[5])
    files["docs/assets/banner-docs.svg"] = banner("docs", "Documentation", "Every document, grouped by what you came to do", "docs")
    for g, name, _ in GROUPS:
        files[f"docs/assets/banner-group-{g}.svg"] = group_strip(g, name)
    for name, fn in DIAGRAMS.items():
        files[f"docs/assets/diagram-{name}.svg"] = fn().svg()
    files[HUB] = hub()
    for d in headed():
        if d[1] in GENERATORS:
            continue
        with open(os.path.join(ROOT, d[1]), encoding="utf-8") as f:
            files[d[1]] = with_head(d, f.read())
    return files


def generator_head(gen):
    with open(os.path.join(ROOT, gen), encoding="utf-8") as f:
        tree = ast.parse(f.read())
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(getattr(t, "id", "") == "DOC_HEAD" for t in node.targets):
            return ast.literal_eval(node.value)
    return None


def check_svg(path, txt, problems):
    """The README's image rules (gen_assets.svg_problems): valid XML, a title and a desc, under 150 KB,
    no gradient, filter, script or outside reference, and a reduced-motion rule wherever there is motion."""
    problems += G.svg_problems(path, txt)


def check_links(path, text, problems):
    base = os.path.dirname(os.path.join(ROOT, path))
    own = {a for _, _, a in headings(text)}
    for n, ln in strip_code(text):
        ln = re.sub(r"`[^`]*`", "", ln)
        targets = re.findall(r"\]\(([^)\s]+)\)", ln) + re.findall(r"""(?:href|src|srcset)=["']([^"']+)["']""", ln)
        for t in targets:
            if re.match(r"^[a-z]+:", t):
                continue
            file, _, anchor = t.partition("#")
            if file:
                full = os.path.normpath(os.path.join(base, file))
                if not os.path.exists(full):
                    problems.append(f"{path}:{n + 1}: {t}: no such file")
                    continue
                if anchor:
                    if not full.endswith(".md"):
                        continue
                    with open(full, encoding="utf-8") as f:
                        anchors = {a for _, _, a in headings(f.read())}
                    if anchor not in anchors:
                        problems.append(f"{path}:{n + 1}: {t}: no such heading")
            elif anchor and anchor not in own:
                problems.append(f"{path}:{n + 1}: #{anchor}: no such heading")


def check_content_revision(problems):
    """RELEASING.md and the CHANGELOG's Unreleased section name the content revision index.rs
    declares, wherever they state one: the number has drifted twice behind the code."""
    with open(os.path.join(ROOT, "crates", "scrutate", "src", "index.rs"), encoding="utf-8") as f:
        code = int(re.search(r"pub const CONTENT_REVISION: u32 = (\d+);", f.read()).group(1))
    for path in ("docs/RELEASING.md", "CHANGELOG.md"):
        with open(os.path.join(ROOT, path), encoding="utf-8") as f:
            text = f.read()
        if path == "CHANGELOG.md":
            text = text[: text.find("\n## [", text.find("## [Unreleased]") + 1)]
        for m in re.finditer(r"content revision(?:\s+is)?\s+(\d+)", text, re.I):
            if int(m.group(1)) != code:
                problems.append(f"{path}:{text.count(chr(10), 0, m.start()) + 1}: says content revision {m.group(1)}; "
                                f"crates/scrutate/src/index.rs declares CONTENT_REVISION = {code}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true", help="change nothing; fail when a file differs or a link is broken")
    a = ap.parse_args()
    files = planned()
    problems = []
    for path, txt in sorted(files.items()):
        full = os.path.join(ROOT, path)
        if path.endswith(".svg"):
            check_svg(path, txt, problems)
        try:
            with open(full, encoding="utf-8") as f:
                cur = f.read()
        except FileNotFoundError:
            cur = None
        if cur != txt:
            if a.check:
                problems.append(f"{path}: differs from what tools/gen_doc_assets.py writes")
            else:
                os.makedirs(os.path.dirname(full), exist_ok=True)
                with open(full, "w", encoding="utf-8") as f:
                    f.write(txt)
                print(f"wrote {path}", file=sys.stderr)
    owned = {os.path.basename(p) for p in files if p.startswith("docs/assets/")}
    for name in sorted(os.listdir(ASSETS)):
        if name.startswith(("banner-", "diagram-")) and name not in owned:
            problems.append(f"docs/assets/{name}: not written by this script (remove it)")
    for doc_path, gen in GENERATORS.items():
        d = next(x for x in DOCS if x[1] == doc_path)
        with open(os.path.join(ROOT, doc_path), encoding="utf-8") as f:
            text = f.read()
        h1, body = split_doc(text)
        want = head(d, h1, body)
        if generator_head(gen) != want:
            problems.append(f"{gen}: DOC_HEAD differs from the head of {doc_path} this script would write")
        if not text.startswith(want):
            problems.append(f"{doc_path}: its head differs from the one {gen} writes")
    listed = {d[1] for d in DOCS}
    for name in sorted(os.listdir(os.path.join(ROOT, "docs"))):
        p = f"docs/{name}"
        if (name.endswith(".md") or name.endswith(".html")) and p not in listed and p != HUB:
            problems.append(f"{p}: not on the hub (add it to DOCS)")
    for path in [HUB] + [d[1] for d in headed()]:
        text = files.get(path)
        if text is None:
            with open(os.path.join(ROOT, path), encoding="utf-8") as f:
                text = f.read()
        check_links(path, text, problems)
    check_content_revision(problems)
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        return 1
    print(f"{len(files)} files {'checked' if a.check else 'up to date'}; links and anchors resolve", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
