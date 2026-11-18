<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-delivery.svg" alt="Delivery index. Where is each delivered item in this repository?" width="880">
</p>

# Delivery index: Scrutate 0.1.0 (2026-11-18)

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

`tools/release_local.sh` writes them to `dist/` (macOS) and, in a clean `rust:1-slim-bookworm`
container, to `dist-linux/`. Neither is in git; `SHA256SUMS` in each holds the checksums. Run on
the release commit, every archive it writes holds the binary, LICENSE-APACHE, LICENSE-MIT,
README.md, CHANGELOG.md, THIRD_PARTY_NOTICES.txt, docs/ and integrations/, and the `.deb` and
`.rpm` packages carry the licence files too
([Releasing](RELEASING.md#6-platform-checks-and-artifacts)).

No release commit exists yet. The files in `dist/` and `dist-linux/` now were built on 2026-11-11,
before the project became MIT OR Apache-2.0 (2026-11-13): their archives, wheels and packages hold
one `LICENSE` (Apache-2.0) in place of the two licence files, and no free-threaded or abi3t wheel
was built. They are rebuilt on the release commit, and this table with them. Every hash in it,
with the binary inside each archive, is in
`bench/results/platforms/packages/dist-2026-11-11-SHA256SUMS.txt`, recomputed from the files.

| Artifact | Platform | SHA-256 of the file | Binary inside |
|---|---|---|---|
| `scrutate-0.1.0-aarch64-apple-darwin.tar.gz` | macOS arm64 | `3c4b51d27c64807d...` | `9aa0a14ec4b0e10b...`, the binary docs/LEADERBOARD.md measured on macOS |
| `scrutate-0.1.0-cp39-abi3-macosx_13_0_arm64.whl` | macOS arm64 (macOS 13 or newer), standard CPython 3.9 to 3.14 | `cd62b27ec0d5b9c0...` | |
| `scrutate-0.1.0.cdx.json` in `dist/` | | `4ba77d65110d0251...` | CycloneDX SBOM of the binary's dependencies |
| `scrutate_0.1.0-1_arm64.deb` in `dist/` | Debian and Ubuntu, aarch64 | `cecae2e9943bc0f0...` | from `packaging/linux/build-packages.sh`, not in `SHA256SUMS` |
| `scrutate-0.1.0-1.aarch64.rpm` in `dist/` | Fedora and RHEL, aarch64 | `0c1d8f46b07b0f2f...` | from `packaging/linux/build-packages.sh`, not in `SHA256SUMS` |
| `scrutate-0.1.0-aarch64-unknown-linux-gnu.tar.gz` | Linux aarch64 (glibc) | `be103d4605fa6c5a...` | `1b8dbc6797ee7359...` (Rust 1.99.1); the Linux VM run in docs/LEADERBOARD.md measured another build, `261ed1a2a4613e19...`; the reproducible-build proof on the pinned 1.100.0 is in docs/SECURITY.md's row |
| `scrutate-0.1.0-cp39-abi3-manylinux_2_34_aarch64.whl` | Linux aarch64, standard CPython 3.9 to 3.14 | `81528e21aaa6458a...` | |
| `scrutate-0.1.0.cdx.json` in `dist-linux/` | | `1a750da0ebcb832d...` | CycloneDX SBOM of the binary's dependencies |

With `uv` installed, `tools/release_local.sh` builds one wheel of each kind (interpreters 3.12,
3.14t and 3.15) and stops if `uv` cannot find one of them; without `uv` it builds only the wheel
for `python3`, and without `maturin` no wheel, so `SHA256SUMS` holds the archive and the SBOM. `tools/linux_verify.sh` builds and tests the static musl binary, which is not
packaged here. The archives of the other platforms, the `.deb` and `.rpm` of both architectures
(the aarch64 pair was also built and installed here by `packaging/linux/build-packages.sh`, not by
this script: packaging/README.md), the `.vsix` and the attestations come from
`.github/workflows/release.yml` when a version is tagged; a dry run of it rehearses them ([Releasing](RELEASING.md#7-what-a-published-release-adds)).

The Python prototype ("Titan") stays untouched in its sibling directory. Its training code lives
on as `python/scrutate_ref`, fitting the bundled models on rows the Rust engine writes; its
engine mirrored the Rust one bit for bit until that mirror ended.
