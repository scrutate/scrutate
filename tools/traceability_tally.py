#!/usr/bin/env python3
"""Count PASS / PARTIAL / BLOCKED / FAIL rows in docs/TRACEABILITY.md (a row with a mixed status
counts as its weakest) and rewrite the tally lines, so the summary can never drift from the table."""
import os
import re

p = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "docs", "TRACEABILITY.md")
s = open(p).read()
counts = {"PASS": 0, "PARTIAL": 0, "BLOCKED": 0, "FAIL": 0}
for line in s.splitlines():
    if not line.startswith("| ") or line.startswith("| ID") or line.startswith("| Gate") or line.startswith("|---"):
        continue
    cells = [c.strip() for c in line.strip().strip("|").split("|")]
    # the release-gate table carries the status in the second cell, section 8 in the third
    for cell in (cells[-1], *cells[1:3]):
        found = [k for k in ("FAIL", "BLOCKED", "PARTIAL", "PASS") if re.search(rf"\b{k}\b", cell)]
        if found:
            counts[found[0]] += 1
            break
total = sum(counts.values())
line = f"PASS {counts['PASS']} · PARTIAL {counts['PARTIAL']} · BLOCKED {counts['BLOCKED']} · FAIL {counts['FAIL']}"
s = re.sub(r"Counts: PASS \d+ · PARTIAL \d+ · BLOCKED \d+ · FAIL \d+ (\(tallied at the end\)|over \d+ rows \(computed by `tools/traceability_tally.py`\))\.",
           f"Counts: {line} over {total} rows (computed by `tools/traceability_tally.py`).", s)
s = re.sub(r"## Tally\n\nPASS \d+ · PARTIAL \d+ · BLOCKED \d+ · FAIL \d+( over \d+ rows)?\.", f"## Tally\n\n{line} over {total} rows.", s)
open(p, "w").write(s)
print(line, "over", total, "rows")
