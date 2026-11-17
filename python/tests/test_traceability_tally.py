"""tools/traceability_tally.py counts every row's status, wherever its table puts it."""
import os
import shutil
import subprocess
import sys

TALLY = os.path.join(os.path.dirname(__file__), "..", "..", "tools", "traceability_tally.py")

PAGE = """Counts: PASS 0 · PARTIAL 0 · BLOCKED 0 · FAIL 0 over 0 rows (computed by `tools/traceability_tally.py`).

| ID | Requirement | Implementation | Evidence | Status |
|---|---|---|---|---|
| R-1.1 | a | b | c | PASS |

| Gate | Status | Evidence |
|---|---|---|
| Release | BLOCKED | x |

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| R-8.10 | a section-8 row | PARTIAL | its evidence |

## Tally

PASS 0 · PARTIAL 0 · BLOCKED 0 · FAIL 0 over 0 rows.
"""


def test_a_section_8_row_with_its_status_in_the_third_cell_is_counted(tmp_path):
    (tmp_path / "tools").mkdir()
    (tmp_path / "docs").mkdir()
    shutil.copy(TALLY, tmp_path / "tools" / "traceability_tally.py")
    (tmp_path / "docs" / "TRACEABILITY.md").write_text(PAGE, encoding="utf-8")
    out = subprocess.run([sys.executable, str(tmp_path / "tools" / "traceability_tally.py")], capture_output=True, text=True, check=True).stdout
    assert out.strip() == "PASS 1 · PARTIAL 1 · BLOCKED 1 · FAIL 0 over 3 rows"
    assert "## Tally\n\nPASS 1 · PARTIAL 1 · BLOCKED 1 · FAIL 0 over 3 rows." in (tmp_path / "docs" / "TRACEABILITY.md").read_text(encoding="utf-8")
