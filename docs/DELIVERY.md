<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-delivery.svg" alt="Delivery index. Where is each delivered item in this repository?" width="880">
</p>

# Delivery index: Scrutate 0.1.0 (2026-11-11)

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Releasing](RELEASING.md) &nbsp;·&nbsp; Next: [Dependencies](DEPENDENCIES.md)

<!-- End of head. -->

The fifteen items of the mission (section 11) and where each lives.

| # | Item | Location |
|---|---|---|
| 1 | Project name and naming research | docs/NAMING.md, docs/name_research.json, tools/name_research.py |
| 2 | Source code and architecture | crates/scrutate (engines, CLI, MCP), crates/scrutate-py (bindings), python/scrutate_ref (model training code), crates/scrutate/models/; docs/ARCHITECTURE.md |
| 3 | Implemented capabilities | README.md, docs/CLI.md, docs/TRACEABILITY.md (each requirement and its status) |
| 4 | Rust toolchain and dependencies | docs/DEPENDENCIES.md, Cargo.lock, rust-toolchain.toml, the CycloneDX SBOM in `dist/` (tools/sbom.py) |
| 5 | Platforms and language capabilities | docs/INSTALL.md (all six supported: macOS arm64, Linux aarch64 and x86_64 tested here; macOS x86_64, Windows x86_64 and Windows ARM64 tested on GitHub's runners), docs/LANGUAGES.md (61 verified grammars, 13 more optional, heuristic tier) |
| 6 | Installation and usage | docs/INSTALL.md, docs/CLI.md, README.md, integrations/*/README.md |
| 7 | Automated test results | docs/TEST_RESULTS.md (generated), bench/results/platforms/ (per-platform logs, wheel matrices, musl checks, reproducible build), bench/results/fuzz/ |
| 8 | Security assessment | docs/SECURITY.md |
| 9 | Reproducible competitive benchmarks | docs/LEADERBOARD.md (generated), bench/leaderboard.py, bench/indexed.py, bench/yardstick.py with bench/rivals/ (every workload against its rivals, rivals pinned by bench/competitors/install_rivals.sh), bench/corpora.py (pinned corpora), bench/results/ (raw JSON and logs) |
| 10 | Performance and memory | docs/LEADERBOARD.md (wall time, CPU time, peak memory per benchmark and tool; indexed latency percentiles, index time and size), docs/BENCHMARKS.md (start-up, concurrency, Engine B index metrics) |
| 11 | Documentation | docs/ (release checklist in docs/RELEASING.md, first questions in docs/FAQ.md), README.md, CHANGELOG.md, CONTRIBUTING.md, GLOSSARY.md, CITATION.cff |
| 12 | Remaining defects and incomplete requirements | docs/RELEASE_READINESS.md, docs/TRACEABILITY.md (PARTIAL, FAIL and BLOCKED rows) |
| 13 | Release-readiness assessment | docs/RELEASE_READINESS.md |
| 14 | Engineering roadmap | docs/ROADMAP.md |
| 15 | Evidence-based competitive positioning | docs/COMPETITIVE.md |

## Release artifacts (built locally, not published)

`tools/release_local.sh` on the release commit writes them to `dist/` (macOS) and, in a clean
`rust:1-slim-bookworm` container, to `dist-linux/`. Neither is in git; `SHA256SUMS` in each holds
the checksums. Every archive holds the binary, LICENSE-APACHE, LICENSE-MIT, README.md,
CHANGELOG.md, THIRD_PARTY_NOTICES.txt, docs/ and integrations/; the `.deb` and `.rpm` packages
carry the licence files too ([Releasing](RELEASING.md#6-platform-checks-and-artifacts)).

| Artifact | Platform | Binary SHA-256 |
|---|---|---|
| `scrutate-0.1.0-aarch64-apple-darwin.tar.gz` | macOS arm64 | `9aa0a14ec4b0e10b...`, the binary docs/LEADERBOARD.md measured on macOS |
| `scrutate-0.1.0-cp39-abi3-macosx_13_0_arm64.whl` | macOS arm64 (macOS 13 or newer), standard CPython 3.9 to 3.14 | |
| `scrutate-0.1.0-cp314-cp314t-macosx_13_0_arm64.whl` | macOS arm64, free-threaded CPython 3.14 | |
| `scrutate-0.1.0-cp315-abi3.abi3t-macosx_13_0_arm64.whl` | macOS arm64, CPython 3.15 and newer, standard and free-threaded | |
| `scrutate-0.1.0-aarch64-unknown-linux-gnu.tar.gz` | Linux aarch64 (glibc) | `261ed1a2a4613e19...`, identical from two build images (`bench/results/platforms/repro/`), and the binary measured in the Linux VM |
| `scrutate-0.1.0-cp39-abi3-manylinux_2_34_aarch64.whl` | Linux aarch64, standard CPython 3.9 to 3.14 | |
| `scrutate-0.1.0-cp314-cp314t-manylinux_2_34_aarch64.whl` | Linux aarch64, free-threaded CPython 3.14 | |
| `scrutate-0.1.0-cp315-abi3.abi3t-manylinux_2_34_aarch64.whl` | Linux aarch64, CPython 3.15 and newer, standard and free-threaded | |
| `scrutate-0.1.0.cdx.json` | | CycloneDX SBOM of the binary's dependencies |

With `uv` installed, `tools/release_local.sh` builds one wheel of each kind (interpreters 3.12,
3.14t and 3.15) and stops if `uv` cannot find one of them; without `uv` it builds only the wheel
for `python3`. `tools/linux_verify.sh` builds and tests the static musl binary, which is not
packaged here. The archives of the other platforms, the x86_64 `.deb` and `.rpm`, the `.vsix`
and the attestations come from `.github/workflows/release.yml` when a version is tagged; a dry
run of it rehearses them ([Releasing](RELEASING.md#7-what-a-published-release-adds)).

The Python prototype ("Titan") stays untouched in its sibling directory. Its training code lives
on as `python/scrutate_ref`, fitting the bundled models on rows the Rust engine writes; its
engine mirrored the Rust one bit for bit until that mirror ended.
