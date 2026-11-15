#!/usr/bin/env python3
"""The numbers docs/SECURITY.md gives for secret-safe answers: for each ROOT, every line of every
text file through the redactor (its redactions by kind, per 10,000 lines, and the same without the
private keys, whose armour names them), then what building the detectors and redacting a
24,000-byte page cost. ROOT is only read: no index is built there.

usage: tools/secret_redactions.py ROOT...
"""
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def run(test, env):
    cmd = ["cargo", "test", "-q", "--release", "-p", "scrutate", "--lib", "--", "--ignored", "--nocapture", "--test-threads=1", test]
    out = subprocess.run(cmd, cwd=os.path.join(HERE, ".."), env={**os.environ, **env}, capture_output=True, text=True)
    if out.returncode != 0:
        sys.exit(f"{' '.join(cmd)} failed:\n{out.stdout}{out.stderr}")
    return out.stdout


def main():
    roots = sys.argv[1:]
    if not roots:
        sys.exit(__doc__)
    for root in roots:
        lines = run("secrets::tests::false_redactions_per_ten_thousand_lines", {"SCRUTATE_FP_ROOT": os.path.abspath(root)}).splitlines()
        kinds = {}
        for line in lines:
            for kind in re.findall(r"\[redacted:([a-z0-9-]+)\]", line):
                kinds[kind] = kinds.get(kind, 0) + 1
        summary = next((l for l in lines if " marks in " in l), "no summary line")
        print(f"{root}: {summary}")
        for kind, n in sorted(kinds.items()):
            print(f"  {kind}: {n}")
        # a private key's armour names it, so its marks are counted apart from the rest
        total = re.search(r" in (\d+) lines", summary)
        if total:
            rest = sum(n for kind, n in kinds.items() if kind != "private-key")
            print(f"  not private-key: {rest} marks, {rest * 10_000 / int(total.group(1)):.3f} per 10,000 lines")
    for line in run("secrets::tests::the_cost_of_redacting_large_answers", {}).splitlines():
        if "ms per answer" in line or "once a process" in line:
            print(line)


if __name__ == "__main__":
    main()
