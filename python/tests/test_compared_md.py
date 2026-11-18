"""bench/make_compared_md.py writes README's "Compared against" table from the result files
docs/LEADERBOARD.md names: one row per tool, every figure from those files."""
import os
import sys

HERE = os.path.dirname(__file__)
ROOT = os.path.join(HERE, "..", "..")
sys.path.insert(0, os.path.join(ROOT, "bench"))
import make_compared_md as C  # noqa: E402

MAC = {"platform": "macOS-27.2.1-arm64-arm-64bit", "machine": "arm64", "cpu": "Apple M3 Pro", "cpus": 11,
       "tools": {"scrutate": "scrutate 0.1.0", "ripgrep": "ripgrep 15.2.0", "GNU grep": "ggrep (GNU grep) 3.12", "ast-grep": "ast-grep 0.51.1"}}
LINUX = {"platform": "Linux-6.8.0-117-generic-aarch64-with-glibc2.39", "machine": "aarch64", "cpu": "0x000", "cpus": 5,
         "tools": {"scrutate": "scrutate 0.1.0", "ripgrep": "ripgrep 15.1.0"}}


def row(tools, best, status, structural=False, cached=True):
    out = {"tools": {t: {"status": "ok", "median": m, "n": n, "argv": ["scrutate", "tsquery"] + ([] if cached else ["--no-cache"])}
                     for t, (m, n) in tools.items()},
           "best_competitor": best, "status": status, "ratio": 0.5, "ci95": [0.4, 0.6]}
    if structural:
        out["qtype"] = "structural"
    return out


def engine_a(env, cache, rows):
    return {"environment": env, "settings": {"runs": 30, "cache": cache}, "benchmarks": dict(enumerate(rows))}


def by_tool(lines):
    rows = [line for line in lines if line.startswith("| ") and not line.startswith("| Tool")]
    return {next(t for t in C.ORDER if r.startswith(f"| {C.NAME.get(t, t)}")): r for r in rows}


def test_each_tool_gets_its_share_range_and_its_verdicts_where_it_was_the_fastest_other_tool():
    mac_warm = engine_a(MAC, "warm", [
        row({"scrutate": (1.0, 30), "ripgrep": (2.0, 30), "GNU grep": (10.0, 3)}, "ripgrep", "PASS"),
        row({"scrutate": (1.0, 30), "ripgrep": (1.25, 3), "GNU grep": (4.0, 3)}, "ugrep", "PASS"),
    ])
    mac_cold = engine_a(MAC, "cold", [row({"scrutate": (3.0, 30), "ripgrep": (2.0, 30)}, "ripgrep", "FAIL")])
    lin = engine_a(LINUX, "warm", [row({"scrutate": (1.0, 30), "ripgrep": (4.0, 30)}, "ripgrep", "PASS")])
    # an answer that differs from the reference is left out of the figures, and counted
    lin["benchmarks"][0]["tools"]["GNU grep"] = {"status": "wrong"}
    rows = by_tool(C.table([("a/mac-warm.json", mac_warm), ("a/mac-cold.json", mac_cold), ("a/lin.json", lin)], []))
    rg = rows["ripgrep"]
    assert rg.startswith("| ripgrep 15.2.0 (macos-arm64), 15.1.0 (linux-aarch64 VM) | ")
    assert "macos-arm64: 2 warm, 1 cold; linux-aarch64 VM: 1 warm" in rg
    # 1/2, 1/1.25, 3/2, 1/4
    assert "0.25 to 1.50 of its median; faster on 3 of 4" in rg
    assert "PASS 2, FAIL 1 of the 3 where it was the fastest other tool" in rg
    grep = rows["GNU grep"]
    assert "macos-arm64: 2 warm; not measured on linux-aarch64 VM" in grep
    assert "0.10 to 0.25 of its median; faster on 2 of 2; GNU grep wrong on 1, left out" in grep
    assert "never the fastest other tool, so not ranked" in grep


def test_a_verdict_on_too_few_runs_is_measured_not_ranked():
    mac = engine_a(MAC, "warm", [row({"scrutate": (1.0, 30), "ripgrep": (2.0, 20)}, "ripgrep", "PASS")])
    assert "measured 1 of the 1 where it was the fastest other tool" in by_tool(C.table([("a/mac.json", mac)], []))["ripgrep"]


def test_a_tool_no_file_measured_and_a_comparison_with_no_file_say_so():
    mac = engine_a(MAC, "warm", [row({"scrutate": (0.1, 30), "ast-grep": (1.0, 30)}, "ast-grep", "PASS", structural=True)])
    rows = by_tool(C.table([("a/mac.json", mac)], []))
    assert set(rows) == set(C.ORDER) and len(rows) == 9
    assert rows["ack"].endswith("| no committed result file | - | - |")
    # only cache hits were measured: parse for parse has no file
    assert "with scrutate's capture cache filled; parse for parse (`tsquery --no-cache`): no committed result file" in rows["ast-grep"]


def indexed(env, rivals):
    tools = {"scrutate": {"version": "scrutate 0.1.0", "index": {"median": 2.0, "bytes": 100e6}, "latency": {"p50": 0.01, "rss_median": 10e6}}}
    for name, (version, build, size, p50, rss) in rivals.items():
        tools[name] = {"version": version, "index": {"median": build, "bytes": size}, "latency": {"p50": p50, "rss_median": rss}}
    return {"environment": env, "settings": {"runs": 20}, "tools": tools,
            "comparison": {"index size": {"best": "zoekt", "ratio": 0.1, "status": "PASS"},
                           "latency p50": {"best": "zoekt", "ratio": 0.4, "ci95": [0.3, 0.5], "status": "measured", "why": "20 runs per tool, under the 30 a verdict needs"}}}


def test_an_indexed_rival_gets_its_figures_and_the_servers_have_no_file(monkeypatch):
    monkeypatch.setattr(C, "settle", lambda comparison, tools, settings: comparison)
    mac = indexed(MAC, {"zoekt": ("zoekt v0.0.0-20261103080450-a5a933bc043e", 50.0, 1000e6, 0.04, 200e6)})
    lin = indexed(LINUX, {"zoekt": ("zoekt v0.0.0-20261103080450-a5a933bc043e", 8.0, 1000e6, 0.02, 100e6), "livegrep": ("livegrep 923d5ad71dfe60900e6c2017b2fa4a5ff902ad71", 20.0, 500e6, 0.05, 400e6)})
    rows = by_tool(C.table([], [("a/mac-indexed.json", mac), ("a/lin-indexed.json", lin)]))
    z = rows["zoekt"]
    assert z.startswith("| Zoekt commit a5a933b (2026-11-03) | macos-arm64, linux-aarch64 VM: the text index, a process per query | ")
    # build 2/50 and 2/8, size 0.1 both, p50 0.25 and 0.5, memory 0.05 and 0.1
    assert "index build 0.04 to 0.25, index size 0.10, query p50 0.25 to 0.50, query memory 0.05 to 0.10 of Zoekt's" in z
    assert "index size PASS 2 of 2; the timed metrics are measured, not ranked (under 30 runs); kept running as a server: no committed result file" in z
    lg = rows["livegrep"]
    assert lg.startswith("| livegrep commit 923d5ad | ")
    assert "linux-aarch64 VM: the text index, a process per query; not measured on macos-arm64" in lg
    # zoekt is the smaller rival: beating it beats livegrep too
    assert "index size PASS 1 of 1" in lg
