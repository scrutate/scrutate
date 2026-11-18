"""bench/make_yardstick_md.py writes docs/COMPETITIVE.md's yardstick section from result files, and the doc check sees a hand edit."""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(__file__)
ROOT = os.path.join(HERE, "..", "..")
sys.path.insert(0, os.path.join(ROOT, "bench"))
sys.path.insert(0, os.path.join(ROOT, "tools"))
sys.path.insert(0, HERE)
import gen_doc_assets as D  # noqa: E402
import yardstick as Y  # noqa: E402
from test_yardstick import RIGHT, Fake, QUERY  # noqa: E402

SCRIPT = os.path.join(ROOT, "bench", "make_yardstick_md.py")
PAGE = "intro\n\n<!-- yardstick: begin -->\n<!-- inputs:  -->\n<!-- yardstick: end -->\n\nafter\n"


def _result(tmp_path):
    out = Y.measure([Fake("scrutate", RIGHT, 0.001, form="process"), Fake("scrutate-mcp", RIGHT, 0.001), Fake("zoekt", RIGHT, 0.002),
                     Fake("wrong rival", set())], [QUERY], runs=3, warmup=0, corpus="/c", work="/w")
    out["environment"] = {"cpu": "Test CPU", "cpus": 4, "platform": "Linux-6.8-x86_64", "tools": {}, "scrutate_sha256": "ab" * 32,
                          "virtualization": "none", "virtualization_source": "systemd-detect-virt", "kernel": "6.8", "cpu_governor": None,
                          "disk": None, "container": None}
    out["settings"] = {"runs": 3, "warmup": 0, "max_load": 1.2, "max_wait": 60}
    out["summary"] = Y.summary(out["rows"])
    d = tmp_path / "results"
    d.mkdir()
    (d / "w4.json").write_text(json.dumps(out))
    return out, str(d / "w4.json")


def _write(page, *args, check=False):
    return subprocess.run([sys.executable, SCRIPT, *args, "--out", str(page), *(["--check"] if check else [])], capture_output=True, text=True)


def test_the_section_holds_each_rival_against_each_scrutate_form(tmp_path):
    out, res = _result(tmp_path)
    page = tmp_path / "page.md"
    page.write_text(PAGE)
    assert _write(page, res).returncode == 0
    text = page.read_text()
    assert text.startswith("intro\n") and text.endswith("\nafter\n")
    zoekt = next(r for r in out["rows"] if r["tool"] == "zoekt")
    v = zoekt["vs"]["scrutate"]
    assert f"{v['ratio']:.2f} [{v['ci_low']:.2f}, {v['ci_high']:.2f}]" in text
    assert "| W4/point | zoekt | resident | ok |" in text
    assert "| W4/point | wrong rival | resident | wrong: missing" in text
    assert "virtualization `none` (systemd-detect-virt)" in text and "Test CPU" in text
    assert f"<!-- inputs: {os.path.relpath(res, os.path.abspath(ROOT))} -->" in text
    assert _write(page, res, check=True).returncode == 0
    page.write_text(text.replace("| W4/point | zoekt", "| W4/point | Zoekt", 1))
    r = _write(page, res, check=True)
    assert r.returncode == 1 and "differs" in r.stderr


def test_without_results_the_section_is_empty_and_competitive_md_is_what_the_script_writes(tmp_path):
    page = tmp_path / "page.md"
    page.write_text(PAGE.replace("<!-- inputs:  -->\n", "<!-- inputs:  -->\nstale\n"))
    assert _write(page).returncode == 0
    assert page.read_text() == PAGE
    with open(os.path.join(ROOT, "docs", "COMPETITIVE.md"), encoding="utf-8") as f:
        text = f.read()
    assert D.regenerated_problems("docs/COMPETITIVE.md", text) == []
    edited = text.replace("<!-- inputs:  -->\n", "<!-- inputs:  -->\n| W3 | zoekt | 0.50 |\n")
    assert len(D.regenerated_problems("docs/COMPETITIVE.md", edited)) == 1


def test_the_method_says_which_run_p99_is_and_a_row_over_the_load_limit_says_so(tmp_path):
    import make_yardstick_md as YM
    out, res = _result(tmp_path)
    out["settings"] = {**out["settings"], "runs": 30}
    out["rows"][0]["over_load"], out["rows"][0]["load"] = True, 3.5
    text = "\n".join(YM.section(out, res))
    assert "With 30 runs, p95 is the 29th fastest and p99 the 30th, the slowest" in text, text
    row = next(line for line in text.split("\n") if line.startswith(f"| W4/point | {out['rows'][0]['tool']} |"))
    assert "ok, timed at load 3.5, over the 1.2 limit" in row, row
