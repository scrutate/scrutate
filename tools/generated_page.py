"""The write or `--check` of a page a script generates: `--check` compares and never writes."""
import argparse
import os
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")


def write_or_check(page: str, text: str, script: str, argv: list[str]) -> int:
    """With `--check`, compare `text` with the committed page and write nothing; else write it to
    `--out PATH`, by default the page."""
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(ROOT, page))
    ap.add_argument("--check", action="store_true", help="fail when the committed page differs; write nothing")
    a = ap.parse_args(argv)
    if a.check:
        with open(os.path.join(ROOT, page), encoding="utf-8") as f:
            old = f.read()
        if old == text:
            return 0
        x, y = old.splitlines(), text.splitlines()
        n = next((i for i, (p, q) in enumerate(zip(x, y)) if p != q), min(len(x), len(y)))
        print(f"{page}:{n + 1}: differs from what {script} writes; run it again", file=sys.stderr)
        return 1
    with open(a.out, "w", encoding="utf-8") as f:
        f.write(text)
    return 0
