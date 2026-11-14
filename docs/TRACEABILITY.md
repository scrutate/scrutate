<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-traceability.svg" alt="Traceability. Which requirement is met, and what is the evidence?" width="880">
</p>

# Requirement traceability matrix

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Security](SECURITY.md) &nbsp;·&nbsp; Next: [Release readiness](RELEASE_READINESS.md)

**Contents**: [2A. Engine A: universal instant search](#2a-engine-a-universal-instant-search) &nbsp;·&nbsp; [2B. Engine B: deep code intelligence](#2b-engine-b-deep-code-intelligence) &nbsp;·&nbsp; [2C. Unified query layer](#2c-unified-query-layer) &nbsp;·&nbsp; [3. Universal support](#3-universal-support) &nbsp;·&nbsp; [4. Extreme performance](#4-extreme-performance) &nbsp;·&nbsp; [5. World-class Rust engineering](#5-world-class-rust-engineering) &nbsp;·&nbsp; [6. Enterprise and future-proofing](#6-enterprise-and-future-proofing) &nbsp;·&nbsp; [7. Project identity](#7-project-identity) &nbsp;·&nbsp; [8. Actually build the product](#8-actually-build-the-product) &nbsp;·&nbsp; [9. Zero-gap quality assurance](#9-zero-gap-quality-assurance) &nbsp;·&nbsp; [10. Release gates (0.1.0)](#10-release-gates-010) &nbsp;·&nbsp; [12. Edge cases, reliability, ecosystem](#12-edge-cases-reliability-ecosystem) &nbsp;·&nbsp; [13. Hybrid Rust + Python stack](#13-hybrid-rust--python-stack) &nbsp;·&nbsp; [14. Benchmark leadership](#14-benchmark-leadership) &nbsp;·&nbsp; [Tally](#tally)

<!-- End of head. -->

Each mission requirement (sections 1 to 14) with its implementation, source, verification,
evidence and status. The mission's status words:

- **PASS**: implemented and verified by the evidence named.
- **PARTIAL**: implemented, verified in part.
- **FAIL**: not implemented, or verification failed.
- **BLOCKED**: a real, documented technical limit of the environment or the problem.

Evidence: generated `docs/TEST_RESULTS.md` (tests, audits) and `docs/LEADERBOARD.md`
(section 14, from `bench/results/`), `docs/BENCHMARKS.md`, `docs/QUALITY.md`, `docs/SECURITY.md`,
`docs/LANGUAGES.md`, `docs/NAMING.md`, `bench/results/platforms/` (per-platform logs). Tests are
in `crates/scrutate/tests/*.rs`, `crates/scrutate/src/*.rs` and `python/tests/*.py`.
Counts: PASS 127 · PARTIAL 8 · BLOCKED 1 · FAIL 0 over 136 rows (computed by `tools/traceability_tally.py`).

## 2A. Engine A: universal instant search

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-2A.1 | Universal plain-text search, no index | `instant.rs` (`search`, `build_walker`, `search_one`, `Collector`) | `cli.rs::exit_codes_and_plain_output`, `json_lines_schema_and_modes`; differential vs ripgrep (`bench/differential.py`); every Engine A row of LEADERBOARD.md (each answer checked against an independent reference) | PASS |
| R-2A.2 | Literal, regex, fuzzy, approximate matching | `Mode::{Literal,Regex,Fuzzy,FuzzyRegex,Pcre2}`; `FuzzyMatcher` (Myers, counted in characters: one word up to 128, 64-bit blocks up to 4096, pigeonhole prefilter); `fuzzyre.rs` (approximate regex, bit-parallel Glushkov automaton); PCRE2 for look-around and backreferences within its match limit; `-z` for archives and compressed files; `-e` for several patterns | unit tests `instant::tests` (blocks against the dynamic programme), `fuzzyre::tests` (against Myers and the regex engine), proptest `fuzzy_finds_edited_patterns`, `cli.rs::pcre2_*`, `search_zip_*`, `fuzzy_regex_*`, differential against `rg -P`, fuzz target `fuzzy_matcher` | PASS |
| R-2A.3 | Multiline and identifier searching | `-U` (each line of a multi-line match printed with its number), `--ident` (`ident_parts`, `identifier_regex`) | unit tests, proptest, `cli.rs::multiline_matches_print_every_line`, LEADERBOARD rows "multiline", "identifier" | PASS |
| R-2A.4 | SIMD acceleration where beneficial | via `regex`/`aho-corasick`/`memchr` (vectorised literal and prefilter scanning), `half` F16C conversion; 128-bit bit-parallel Myers; word-at-a-time UTF-8 validation before the replacing decoder | LEADERBOARD.md; no hand-written intrinsics | PASS |
| R-2A.5 | Parallel filesystem traversal | `ignore::WalkParallel` (work stealing), threads = min(cores, 12), on macOS the performance cores (`policy.rs`), or `-j` | robustness symlink tests; LEADERBOARD (5 threads, one per performance core, on the M3 Pro) | PASS |
| R-2A.6 | Optimized memory and I/O | `grep-searcher` buffered line search; plain output formatted by the search threads into per-file buffers (`PlainSink`), lines over 64 KB written from where they are | peak RSS column of LEADERBOARD.md; `cli.rs` output tests | PASS |
| R-2A.7 | Efficient pattern matching | linear-time regex with literal optimisations; literal fast path; word matcher | LEADERBOARD rows; proptest | PASS |
| R-2A.8 | Streaming results and cancellation | per-file buffers or batched channel; `AtomicBool` cancel; Ctrl-C handler through `signal()` / `SetConsoleCtrlHandler` (exit 130); broken-pipe stop; `--max-results` | `cli.rs::interrupt_exits_130`, robustness `regex_limits_and_errors_are_clean` | PASS |
| R-2A.9 | Git-aware and ignore-aware | `.gitignore`/`.ignore`/global/exclude, nested ignore files, `--no-ignore`, `--hidden` | robustness `symlinks_ignores_and_nested_repos` (no longer needs a git binary); LEADERBOARD `rgbuilt/git-aware` | PASS |
| R-2A.10 | Advanced filtering and traversal | `-g`, `-t/--type-list`, `--max-filesize`, `--max-depth`, `-L`, `--hidden`, `-m`, `--max-results` | `cli.rs::json_lines_schema_and_modes`; LEADERBOARD `linux/file-type filter`, `linux/unrestricted` | PASS |
| R-2A.11 | Works on unknown languages immediately | no parser involved; any readable file | robustness (noext, UPPER.PY, Makefile), differential | PASS |
| R-2A.12 | Never depends on Engine B / index / parsers / cloud / background processes | `instant.rs` imports nothing from index/parse; `grep` never opens `.scrutate` unless `--index` or `--symbols` is asked for | code review; every `grep` test runs without an index | PASS |

## 2B. Engine B: deep code intelligence

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-2B.1 | Incremental repository indexing | `index.rs::update` (mtime/size fast path, SHA-1, per-file delete+insert, segment per update, compaction); `textindex.rs::update` (size + mtime, new segments, liveness per generation, k-way merge); `watch` / `serve --watch` run them on file changes; `grep --index --update` | robustness `incremental_updates_track_edits_renames_and_deletes`; `cli.rs::indexed_grep_equals_walk` (edits, additions, deletions, renames, merges); `cli.rs::watch_keeps_the_code_and_text_indexes_fresh`, `grep_index_update_answers_for_the_tree_as_it_is_now`; LEADERBOARD Engine B "update after 100 edits" | PASS |
| R-2B.2 | AST-aware and structural searching | tree-sitter grammars (`langs.rs`, `parse.rs`); `tsquery` (`intel::tsquery`) with a per-file capture cache by content hash (`tscache.rs`) | samples snapshot tests; `cli.rs::tsquery_cache_output_equals_a_fresh_parse`; LEADERBOARD `linux/structural`, `ripgrep/structural` (answers checked against py-tree-sitter) | PASS |
| R-2B.3 | Symbol and reference discovery | chunks with kinds/qualnames; `symbol`, `refs` | `cli.rs::index_search_symbol_refs_outline_deps`, bindings tests | PASS |
| R-2B.4 | Find definitions and usages | `find_symbol`, `references()`; `resolve/` resolves each occurrence to its definition through scopes, imports and qualified names (Python, JavaScript/TypeScript, Rust, Go, Java with its fields, PHP, C and C++ through `#include`, C#) | bindings `test_repo_index_and_intelligence`; `cli.rs::index_search_symbol_refs_outline_deps`; `tests/resolve.rs` (two same-named definitions per language; C, C++ and C# trees and their snapshot samples); `tests/resolve_java.rs`; `tests/resolve_php.rs`; 60 of 60 hand-checked resolutions right on networkx and ripgrep | PASS |
| R-2B.5 | Function, method, class, type discovery | definition kinds per grammar | `tests/samples/expected.json`, `tests/samples_extra/expected.json` | PASS |
| R-2B.6 | Import and dependency analysis | `imports` table; `deps` and `rdeps` through `names.rs` (`Names::imports`, `Names::importers`) | unit test `imports_are_extracted_and_normalised`; bindings test | PASS |
| R-2B.7 | Call-site discovery | `refs` table, `callers`, `callees`, `refs` | bindings test; `tests/intel_refs.rs` | PASS |
| R-2B.8 | Cross-file and cross-repository intelligence | call graph and imports across files; `search --also ROOT` | `cli.rs::search_also_merges_repositories` | PASS |
| R-2B.9 | Duplicate and similar-code discovery | `intel::dupes` (exact hash + MinHash/LSH), `intel::similar` | bindings tests | PASS |
| R-2B.10 | Language-aware navigation | `outline`, `symbol`, `callers`, `callees`, `refs`, `deps` | bindings tests, `cli.rs` | PASS |
| R-2B.11 | Evaluate tree-sitter / compilers / language servers | tree-sitter chosen; compiler front-ends and LSP servers rejected for 0.1 (per-language toolchains, repository code execution) (ARCHITECTURE.md) | design record | PASS |
| R-2B.12 | Ranking quality | BM25 + exact + semantic + LightGBM ranker (v8: fitted on Python, Rust, Go, Java, JavaScript, TypeScript) | QUALITY.md: held-out Python MRR@10 (variant A, function answers) 0.635 (doc) to 1.000 (id), above the lexical baseline with every interval of the difference above 0; held-out repositories in five more languages, above BM25 on every query type except Java keyword queries with a function answer, where the two tie | PASS |
| R-2B.13 | Indexed text search (the job of Zoekt and livegrep) | `textindex.rs` (trigram postings over the default walk's files, HIR-derived trigram conditions, candidates searched by Engine A's per-file searcher), `grep --index` | `cli.rs::indexed_grep_equals_walk` (17 query kinds, 4 stages), `bench/differential.py` (index vs walk on random corpora, before and after an update), unit and property tests `textindex::tests`, fuzz target `trigram_query` (1.17 M runs, no failure), LEADERBOARD Engine B section | PASS |

## 2C. Unified query layer

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-2C.1 | One consistent CLI and query interface | `cli.rs` (clap), shared conventions (exit codes, `--json`, JSON diagnostics) | `cli.rs` end-to-end tests | PASS |
| R-2C.2 | Automatic strategy selection without sacrificing correctness | `find` (regex to Engine A; indexed/small repo to ranked; else identifier grep); `grep --symbols`; `grep --index` answers like the walk | `cli.rs::find_routes_by_query_shape`, `indexed_grep_equals_walk` | PASS |
| R-2C.3 | Engines independently operational and testable | separate modules and tests | test suite layout | PASS |

## 3. Universal support

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-3.1 | Every readable text file searchable | Engine A: all files; Engine B: windows tier for everything not excluded | robustness `hostile_text_shapes_never_panic` | PASS |
| R-3.2 | Unknown languages without configuration | heuristic tier + windows | `heuristics_find_keyword_definitions` | PASS |
| R-3.3 | Extensible language adapters | data-driven `LangSpec` registry (`langs.rs`) | 61 default + 13 optional grammars; CONTRIBUTING.md procedure | PASS |
| R-3.4 | Verified language capability registry | `scrutate languages --json`; `tools/gen_languages_md.py` | LANGUAGES.md: all 74 grammars verified by samples, 61 in the default build and 13 optional ones in `tests/samples_extra` (writing them found and fixed broken rules for D, LLVM IR, AutoHotkey, VHDL architectures, OCaml modules and OCaml interfaces) | PASS |
| R-3.5 | Monorepos, multiple repositories, Git and non-Git, mixed languages, legacy, config, generated code | ignore-aware walk, `--also`, config files indexed, `exclude_dirs` | LEADERBOARD corpora: Linux, llvm-project (180,631 files), generated and non-Git trees | PASS |

## 4. Extreme performance

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-4.1 | SIMD, algorithms, parallelism, cache-efficient structures, mmap, buffered I/O, low allocation, adaptive concurrency, incremental indexing, optimised storage | see 2A/2B; mmapped immutable segments; trigram postings as delta + LEB128; per-file output buffers; ranges of one large file on every thread; suffix-literal prefilter | LEADERBOARD.md | PASS. Concurrency follows the work: up to min(cores, 12) threads (the performance cores on macOS), three search threads behind a one-thread walk for roots with no directory to enter, more threads as reads turn out to come from the disk (one per 16 slow reads, up to 64 on Linux and twice the cores elsewhere), one large file spread over every thread, indexed searches capped at the candidate count; no load-based scaling (not shown to help) |
| R-4.2 | Benchmark against grep, ripgrep, ack, ag, git grep, ugrep, ast-grep | `bench/leaderboard.py` | LEADERBOARD.md (all seven, plus Zoekt and livegrep for Engine B) | PASS |
| R-4.3 | Measure latency, throughput, CPU, memory, startup, indexing time, index size, concurrency | `leaderboard.py` (wall, CPU, peak RSS per run via hyperfine), `indexed.py` (index time, size, update, p50/p95/p99, memory, concurrent throughput) | LEADERBOARD.md | PASS |
| R-4.4 | Small and massive repositories, cold and warm caches, query types, realistic workloads | 9 corpora; warm and cold (macOS: page eviction with `bench/evict.c`; Linux VM: `drop_caches`) | LEADERBOARD.md: warm and cold sections for macOS and the Linux VM; BENCHMARKS.md: three Python corpora from 600 to 54,500 files | PASS |
| R-4.5 | Reproducible benchmarks with versions, hardware, configuration, datasets, commands | results JSON records tool versions, hardware, settings, every argv; corpora pinned (`bench/corpora.py`, manifest with tree checksums) | LEADERBOARD.md "Exact command lines" | PASS |
| R-4.6 | Outperform competitors across every workload; report where they remain superior | per-thread output held for up to a millisecond and written in 64 KB pieces, no Foundation/libobjc at start-up, system functions bound on first use, long-line writes, fast UTF-8 validation, one large file searched on every thread and a one-line file read on several, suffix-literal and line-start-literal candidate lines, walk threads chosen by platform and grown by the speed of the reads, a vendored `ignore` with a regex-free excludes parser, regex-free gitignore lines and a short idle wait, a text index with position and next-byte masks | LEADERBOARD.md statuses: macOS 37 of 37 warm and 5 of 5 cold PASS; Linux VM 14 of 14 warm and 4 of 4 cold PASS; indexed: macOS 8 of 8 PASS against Zoekt, Linux VM 8 of 8 PASS against Zoekt and livegrep; COMPETITIVE.md gives the narrowest leads and what other tools do that Scrutate does not. No run on Linux x86_64 or on dedicated hardware | PARTIAL |

## 5. World-class Rust engineering

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-5.1 | Stable Rust, evaluated dependencies | pinned stable 1.99.1, `rust-version` 1.91; DEPENDENCIES.md with purpose per crate | `cargo build` on stable; `cargo audit` | PASS |
| R-5.2 | Modular, cohesive, independently testable | 29 modules (`lib.rs`); tests per module | test layout | PASS |
| R-5.3 | Correctness, memory safety, predictable resources, concurrency, maintainability | pinned ranking features (`tests/ranking_pins.rs`) and a ranking quality floor (`bench/quality.py`); unsafe inventory (SECURITY.md); fuzzing with AddressSanitizer; bounded buffers | ranking pins, quality floor, robustness, fuzzing | PASS |
| R-5.4 | Minimal dependencies | 212 crates in the release binary (the `ctrlc` crate and its objc2 chain removed for 50 lines of signal handling) | DEPENDENCIES.md | PASS |
| R-5.5 | No dead code, duplicated logic, unnecessary allocations, unjustified abstractions | `-D warnings`, clippy clean; Engine A's per-file search shared by the walk and the indexed path | `cargo clippy --all-targets -D warnings` | PASS |
| R-5.6 | Async only where beneficial | none used | design note | PASS |
| R-5.7 | `cargo fmt`, `cargo clippy`, `cargo test`, dependency auditing | all gates | TEST_RESULTS.md | PASS |

## 6. Enterprise and future-proofing

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-6.1 | Target Linux, macOS, Windows | portable code; CI matrix for 6 runners | macOS arm64 natively; Linux aarch64 and x86_64 in clean containers (x86_64 under emulation); macOS x86_64 and Windows x86_64 cross-built (`bench/results/platforms/`, INSTALL.md); in CI run 43013926055 (`9384c18`) the Rust tests and a smoke test of the release binary passed on all six runners: macOS arm64, Intel macOS, Linux x86_64, Linux arm64, Windows x64 and Windows arm64 | PASS |
| R-6.2 | Secure filesystem access | path confinement in `read`, no symlink following by default, index only under root, index directory owner-only (0700) on Unix | SECURITY.md; robustness; bindings; cli `the_index_directory_is_private_to_its_owner_under_umask_022` | PASS |
| R-6.3 | Permission-aware indexing | unreadable files counted and skipped (text index: searched by Engine A, retried on the next update) | robustness `unreadable_files_are_reported_not_fatal` | PASS |
| R-6.4 | Repository isolation | one `.scrutate/` per repository | design; concurrency tests | PASS |
| R-6.5 | Resource exhaustion protection | regex size limits, file size caps, binary skip, bounded buffers, overflow-checked array sizes | robustness; fuzz target `npy` | PASS |
| R-6.6 | Index corruption detection and recovery | triage on open, segment validation, `check`, auto rebuild; text index: structural checks on open, deep checks in `check`, rebuild with `--force` | robustness `corrupted_index_is_detected_and_rebuilt`; `cli.rs::indexed_grep_equals_walk` (damaged segment) | PASS |
| R-6.7 | Versioned storage and migration | `schema_version` 14 (the artifact store and the path map), text index `format` 3, model signature; mismatch → rebuild; a different `content_rev` (parsing rules) reads every file again | robustness; `Triage::Rebuild` | PASS (migration policy = rebuild the cache) |
| R-6.8 | Offline and air-gapped operation | no network code; models embedded | code review | PASS |
| R-6.9 | No unauthorized source-code transmission | nothing leaves the machine | code review | PASS |
| R-6.10 | Dependency and supply-chain security | `Cargo.lock`, `cargo audit`, SBOM (CycloneDX), licences reviewed | TEST_RESULTS.md, `tools/sbom.py` | PASS |
| R-6.11 | Reproducible builds | pinned toolchain, lock file, release profile | two clean builds on macOS give the same hash (TEST_RESULTS.md); two different Linux images (`rust:1-slim-bookworm`, `golang:1.26-bookworm` with rustup) give the same SHA-256 (`bench/results/platforms/repro/`) | PASS |
| R-6.12 | CI/CD integration | `.github/workflows/{ci,release,benchmarks}.yml`, GitLab/Bitbucket templates | YAML validated; CI run 43013926055 (`9384c18`) passed all 12 jobs of a push; the release and benchmark workflows have not run. `release.yml` takes a `dry_run` dispatch input that runs every job, `ci.yml` included, attests, and makes, checks and deletes a draft release `rehearsal-<run id>` (docs/RELEASING.md section 7) | PARTIAL |
| R-6.13 | JSON and JSONL output | `--json` everywhere; JSONL schema 1; JSON error records | `cli.rs::json_lines_schema_and_modes`, `errors_are_json_when_asked` | PASS |
| R-6.14 | Stable, versioned integration interfaces | JSONL `schema`, MCP protocol versions, SemVer policy | docs | PASS |
| R-6.15 | IDE, automation, Git-platform, MCP integrations | MCP server (`serve --watch` keeps its index fresh); `scrutate lsp` language server; VS Code extension as its client; JetBrains through LSP4IJ; Neovim, GitHub action, GitLab/Bitbucket templates, pre-commit | MCP: `cli.rs::mcp_protocol_round_trip`, `serve_watch_updates_the_index_while_serving`; LSP: `tests/lsp.rs` (every request, UTF-16 columns); in their hosts, CI run 43471722734 (commit `5369956`): VS Code 1.85.0 and stable on Linux, macOS and Windows, from the folder and from the packaged .vsix (vsce 4.1.1) installed with `--install-extension`; Neovim 0.10.4, 0.11.7 and 0.12.6 on the same three; the GitHub action of the commit on all six runners; the GitLab and Bitbucket templates in gitlab-ci-local 4.77.1 and pipeline-runner 0.6.4; JetBrains: IntelliJ IDEA 2026.2.3 headless with LSP4IJ 0.21.0 from the Marketplace, Go to Declaration (`jetbrains.yml`, run 43462992805); pre-commit 4.7.0 runs the hook (CI run 43484020876) | MCP, LSP, VS Code, Neovim, JetBrains, the GitHub action and pre-commit PASS; GitLab and Bitbucket PASS in emulators, PARTIAL on gitlab.com and bitbucket.org |
| R-6.16 | 10 to 20-year maintenance horizon | versioned formats, replaceable components, compatibility policy, automated tests | design record | PASS (as a design; time will verify) |

## 7. Project identity

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-7.1 | Original, memorable, commercially suitable name | **Scrutate** | NAMING.md | PASS |
| R-7.2 | At least 15 candidates; GitHub, crates.io, products, registries, domains, trademarks | 30 candidates; crates.io, PyPI, npm, GitHub, Homebrew, WHOIS, web; trademark knockout search on 2026-11-13 of USPTO, TMview (EUIPO and national offices), WIPO Global Brand Database, UKIPO and IP Australia: no SCRUTATE or SCRUTAT* mark in any class; near marks in classes 9 and 42 (SCRUTINY US 99613386, SCRUT UK, SCRUTINIZER, Scrutor, Scrutoir, ScrutIA) listed for counsel | NAMING.md, `docs/name_research.json` | PASS (knockout search; legal clearance by counsel is the owner's decision) |
| R-7.3 | Selection by documented evidence; no false uniqueness/legal claims | NAMING.md | review | PASS |

## 8. Actually build the product

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| R-8.1 to 8.10 | Inspect, research, foundation, Engine A, Engine B, unify, benchmark/optimise, security/reliability testing, documentation/installation, deliver 0.1.0 | PASS | this repository; `git log`; TEST_RESULTS.md; LEADERBOARD.md; docs/ |
| R-8.11 | Preserve existing user work | PASS | earlier work untouched; the prototype's training code lives on as `python/scrutate_ref` |
| R-8.12 | Decide autonomously | PASS | decisions recorded in CHANGELOG/ARCHITECTURE; one question asked (publishing to GitHub: declined on 2026-11-08, asked for on 2026-11-11 once every install and usage path is verified; the repository is on GitHub now, private) |

## 9. Zero-gap quality assurance

| ID | Requirement | Implementation | Status |
|---|---|---|---|
| R-9.1 | Unit tests | `cargo test` lib tests | PASS |
| R-9.2 | Integration tests | `tests/samples.rs`, `tests/samples_extra.rs`, `tests/robustness.rs`, `tests/cli.rs` | PASS |
| R-9.3 | End-to-end tests | `tests/cli.rs` runs the real binary | PASS |
| R-9.4 | Differential tests | `bench/differential.py` (vs ripgrep; `--index` vs the walk); `bench/fuzz_grep.py` (`scrutate grep` vs ripgrep on random trees) and `bench/fuzz_incremental.py` (an updated index vs a fresh one after random edits), 20 seconds each in CI, failing when nothing was compared; every leaderboard answer vs a Python reference | PASS |
| R-9.5 | Property-based tests | `tests/properties.rs`, `textindex::tests::property_condition_never_rejects_a_matching_text` | PASS |
| R-9.6 | Fuzz tests | four cargo-fuzz targets (`fuzz/`), AddressSanitizer, ten-minute rounds, and 45 minutes per target every week in CI (`fuzz-weekly.yml`); four defects found and fixed, crash inputs replayed by the test gate | PASS |
| R-9.7 | Regression tests | snapshot `expected.json`; fuzz regressions; `bench/leaderboard_regression.py` | PASS |
| R-9.8 | Stress tests | robustness (5 MB line, 200k lines, deep nesting, 3 writers + 2 readers x 30); 12 concurrent text-index updates on SMB | PASS |
| R-9.9 | Security tests | regex limits, path confinement, symlink escape, corruption, panic containment, fuzzing | PASS |
| R-9.10 | Cross-platform tests | full test suite on macOS arm64, Linux aarch64 and Linux x86_64 (`bench/results/platforms/`); in CI run 43013926055 (`9384c18`) the Rust tests passed on macOS arm64, Intel macOS, Linux x86_64, Linux arm64, Windows x64 and Windows arm64 (five tests of files removed while open and of file names Windows forbids run on Unix only) | PASS |
| R-9.11 | Traceability matrix | this document | PASS |
| R-9.12 | Continuous gap analysis | RELEASE_READINESS.md; ROADMAP.md | PASS |

## 10. Release gates (0.1.0)

| Gate | Status | Evidence |
|---|---|---|
| Functional universal instant search | PASS | 2A rows |
| Unknown-language generic search | PASS | R-3.1/3.2 |
| Reliable filesystem traversal | PASS | robustness tests, differential, network FS check |
| Ignore rules and advanced filtering | PASS | R-2A.9/2A.10 |
| Literal and regex search | PASS | R-2A.2 |
| Machine-readable output | PASS | R-6.13 |
| Functioning, tested code-intelligence foundation | PASS | 2B rows, QUALITY.md |
| Automated tests | PASS | TEST_RESULTS.md |
| Reproducible competitive benchmarks | PASS | LEADERBOARD.md, `bench/` |
| Verified builds for declared supported platforms | PASS | INSTALL.md (only verified platforms are declared) |
| Security assessment | PASS (self-assessment; no external audit) | SECURITY.md |
| Professional documentation | PASS | docs/ |
| Installation instructions | PASS | INSTALL.md |
| Do not declare production readiness until criteria pass | respected: **0.1.0 is not declared production-ready** | RELEASE_READINESS.md |

## 12. Edge cases, reliability, ecosystem

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-12.1 | Filesystem edge cases | long names, spaces/unicode names, dotfiles, no extension, uppercase extensions, case-colliding paths (benchmark corpora) | robustness | PASS |
| R-12.2 | Operating-system differences | portable code, CRLF/CR handling, `/`-normalised paths, SMB lock semantics | tests pass on macOS (APFS, SMB share) and Linux (ext4 in a container); the Windows-only code (console handler, path handling) compiles and links for x86_64-pc-windows-gnu; the tests pass on the Windows x64 and arm64 runners (CI run 43013926055); the three runs before it found a stream search that read from a moved file cursor, a CRLF checkout of compiled-in data and the printed form of verbatim paths, all fixed | PASS |
| R-12.3 | Unicode | lossy decoding, boundary safety, Unicode case folding in the trigram conditions; search tokens for words of every script (scripts written without spaces as character bigrams) and names compared in NFC, in both engines | robustness, proptest, `textindex::tests::unicode_case_folding_is_covered`, `tests/unicode_text.rs` | PASS |
| R-12.4 | Binary files | NUL sniff (Engine B), `BinaryDetection::quit` (Engine A), text index keeps the bytes before the first NUL | robustness; LEADERBOARD `binheavy/literal` | PASS |
| R-12.5 | Invalid encodings | invalid UTF-8 lossy; UTF-16 with a BOM transcoded by both engines | `cli.rs::utf16_bom_is_searched_by_engine_a_and_indexed_by_engine_b` | PASS |
| R-12.6 | Symlinks | not followed by default, loop-safe with `-L` | robustness | PASS |
| R-12.7 | Git worktrees and submodules | `.git` files skipped as VCS metadata; nested repos walked; a linked work tree's new index starts from its main checkout's, the link read from git's files and confirmed both ways | robustness, `cli.rs::git_file_of_worktrees_and_submodules_is_not_indexed`, `tests/artifacts.rs` | PASS |
| R-12.8 | Permissions | counted as errors, never fatal | robustness | PASS |
| R-12.9 | Huge files | Engine A searches any size (100 MB single-line file in LEADERBOARD); Engine B skips > 1 MB; the text index maps files over 64 MB | robustness; LEADERBOARD `patho/long line` | PASS |
| R-12.10 | Network filesystems | text-index lock polled with `try_lock` (SMB answers a blocking lock with EAGAIN) | `tools/netfs_check.sh` over SMB (Samba 4.23, macOS smbfs): identical Engine A output, Engine B index, concurrent updates, `check --files`; `bench/results/platforms/netfs-smb-macos.txt` | PASS for one host; an index shared by several machines over NFS/SMB is not supported (documented) |
| R-12.11 | Files changing during execution | content read once per file; concurrent writers test | robustness | PASS |
| R-12.12 | Index lifecycle | `index.rs`, `lex.rs`, `textindex.rs` (manifest swaps, immutable segments, generations, GC, compaction, rebuild, `Triage`) | robustness, `cli.rs` | PASS |
| R-12.13 | Professional CLI: completion, configuration, env vars, piping, deterministic output, structured diagnostics, progress, cancellation, exit codes | `completions`, `config.rs`, env on every option, BrokenPipe handling, `--sort`, JSON error records, progress on TTY, Ctrl-C, 0/1/2/130 | `cli.rs::config_file_sets_defaults_but_command_line_wins`, `errors_are_json_when_asked`, `interrupt_exits_130` | PASS |
| R-12.14 | Protections: regex exhaustion, unauthorized access, unsafe temp files, vulnerabilities, untrusted repository execution | limits; confinement; temp files inside `.scrutate/` + rename; audit; never executes repository content | SECURITY.md | PASS |
| R-12.15 | Verification: unit, integration, differential, property, fuzz, stress, security, regression, cross-platform | section 9 | TEST_RESULTS.md (unit, integration, property, robustness, differential, ranking quality, fuzz regressions, audit, reproducible build); `bench/results/platforms/` (cross-platform) | PASS (cross-platform part as R-9.10: the CI run of `9384c18`; a release commit passes CI on the six platforms again, docs/RELEASING.md) |
| R-12.16 | Real integrations | `integrations/` + `.github/` | MCP and LSP automated; VS Code (folder and packaged), Neovim, the GitHub action (CI run 43471722734), JetBrains (`jetbrains.yml` run 43462992805) and pre-commit (CI run 43484020876) run in their hosts in CI; the GitLab and Bitbucket templates run in local emulators in CI, not yet on gitlab.com or bitbucket.org | MCP, LSP, VS Code, Neovim, JetBrains, the GitHub action and pre-commit PASS; GitLab and Bitbucket PARTIAL |
| R-12.17 | Installation, updates, uninstallation, packaging, signed releases, checksums, SBOMs, reproducible builds | INSTALL.md; release workflow; `tools/sbom.py`; `tools/third_party_notices.py`; `packaging/` (Homebrew, Scoop, winget, `.deb`, `.rpm`); checksums for delivered artifacts | SBOM, notices and checksums generated; reproducible across two build images; `.deb` and `.rpm` installed, run and removed in clean Debian and Fedora containers, the Homebrew formula in a Linux Homebrew (bench/results/platforms/packages/). "Signed" means: every file on the release carries a Sigstore-signed SLSA build provenance attestation (SLSA v1.0 Build Level 2) and is listed in `SHA256SUMS`, the SBOM is attested to the archives, and INSTALL.md's check pins the signer workflow and the tag (`--signer-workflow scrutate/scrutate/.github/workflows/release.yml --source-ref refs/tags/v0.1.0`). Platform code signing (Apple notarization, Windows Authenticode) is a known limit with INSTALL.md's Gatekeeper and SmartScreen workarounds; a Homebrew formula install gets no quarantine attribute (Homebrew sets it on casks only). The attestations have not run yet; publishing needs the owner's accounts | PARTIAL (signing and publishing) |
| R-12.18 | Reproducible benchmarks and regression tracking | `leaderboard.py`, `leaderboard_regression.py`, `benchmarks.yml` | LEADERBOARD.md; `benchmarks.yml` has not completed a run | PARTIAL |
| R-12.19 | Every edge case/integration/requirement has explicit status and evidence | this matrix | | PASS |

## 13. Hybrid Rust + Python stack

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-13.1 | Python only in permitted roles | benchmarks, differential scripts, ranking quality, optional PyO3 package, JSONL consumers, model training | repository layout | PASS |
| R-13.2 | Engines and CLI in Rust, no Python at runtime | no Python dependency; models embedded | `cli.rs::runs_without_python_on_path`; Linux images without Python (`linux-aarch64/verify.log`, `linux-x86_64/verify.log`) | PASS |
| R-13.3 | Core builds and passes `cargo test` without Python; bindings optional | bindings are a separate workspace member outside `default-members` | `cargo test` on `rust:1-slim-bookworm`, which has no Python (`linux-*/verify.log`) | PASS (an optional crate rather than a feature flag, so the core cannot even optionally depend on Python) |
| R-13.4 | Bindings release the GIL during long work | `Python::detach` around index/search/grep waits | `test_gil_is_released_during_rust_work` | PASS (four threads searching at once take 0.34 of the sequential time with the abi3 wheel on macOS arm64; on a busy machine the test skips instead of measuring) |
| R-13.5 | Minimise Rust/Python conversion; streaming results | `GrepIter` (bounded channel, per-batch conversion) | bindings tests | PASS |
| R-13.6 | abi3 wheels for standard Python | `pyo3/abi3-py39`, maturin | `cp39-abi3` wheel installed and tested on CPython 3.10 to 3.13 on macOS arm64 and on 3.10, 3.12, 3.13 on Linux aarch64 and x86_64 (`wheels-macos-arm64.json`, `linux-*/wheels.json`) | PASS |
| R-13.7 | Free-threaded `cp314t` wheel; `gil_used = false` after review | `#[pymodule(gil_used = false)]` with the review in `lib.rs` | `cp314t` wheel on free-threaded 3.14: the abi3 wheel is refused, the GIL stays disabled after import with warnings as errors, bindings tests pass (macOS arm64, Linux aarch64 and x86_64) | PASS |
| R-13.8 | Linux static musl binaries, identical output, measured difference | `tools/linux_verify.sh` + `bench/musl_vs_glibc.py` | static musl binaries for aarch64 and x86_64; byte-identical JSON output on three queries; timing on aarch64 0.85x to 1.25x of glibc on 3 to 9 ms runs, inside the run-to-run noise measured with two copies of one binary (`linux-*/musl-*.txt`) | PASS |
| R-13.9 | Wheels and binaries for Linux/macOS/Windows in CI; declare a platform only after testing there | `release.yml`; INSTALL.md declares only tested platforms | `ci.yml` builds and tests the binary and builds, installs and tests the abi3 and cp314t wheels on all six runners (CI run 43408907893); platforms also verified locally (R-6.1, CI run 43013926055); `release.yml`, which builds the release archives, has not run | PASS |
| R-13.10 | Never cite unverified external claims | external claims limited to primary sources and measured numbers | review | PASS |
| R-13.11 | Hybrid requirements carry PASS/PARTIAL/FAIL/BLOCKED | this section | | PASS |

## 14. Benchmark leadership

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-14.1 | Competitors by category, each verified against its primary source | grep 3.12 (gnu.org), ripgrep 15.2.0, ack 3.10.0 (beyondgrep.com), ag 2.2.0 (last release 2018, still widely packaged), git grep 2.56, ugrep 7.8.5, ast-grep 0.51.1, Zoekt (sourcegraph/zoekt, commit of 2026-11-03), livegrep (livegrep/livegrep 923d5ad, 2026-02-10) | versions recorded in each results file | PASS |
| R-14.2 | Cross-category comparisons published, not counted | `scrutate --index` column in the Engine A table; never ranked | LEADERBOARD.md | PASS |
| R-14.3 | Query types: literal, multiple literals, case-insensitive, whole-word, simple and complex regex, Unicode, no-match, very many matches, multiline, identifier, structural | `leaderboard.py::QT`, structural benchmarks | LEADERBOARD.md (`linux/*` rows) | PASS |
| R-14.4 | Filtering modes: ignore-aware, unrestricted, file-type, Git-aware | `argv()` per tool and mode, reference file lists per mode | LEADERBOARD rows `linux/unrestricted`, `linux/file-type filter`, `rgbuilt/git-aware` | PASS |
| R-14.5 | Corpora pinned: Linux kernel, large multi-language monorepo, small repo, single large file, many small files, binary-heavy, pathological | `bench/corpora.py` (Linux v7.3, llvm-project 23.1.5, ripgrep 15.2.0, generated sets) with `manifest.json` tree checksums | manifest in each results file | PASS |
| R-14.6 | Cold and warm cache | `leaderboard.py --cache cold --cold-prepare`: `bench/evict.c` evicts the benchmark's files from the page cache before every run on macOS (no root needed), `drop_caches` on Linux | LEADERBOARD.md cold sections: macOS 5 of 5 PASS; Linux VM 4 of 4 PASS | PASS |
| R-14.7 | Engine B workloads: indexing time, incremental update, size, p50/p95/p99, memory, concurrent throughput | `bench/indexed.py` | LEADERBOARD.md Engine B section: all eight metrics measured on the Linux tree, against Zoekt on macOS (`macos-arm64/superseded/indexed-9aa0.json`) and against Zoekt and livegrep in the Linux VM (`linux-aarch64/superseded/indexed-261e.json`) | PASS (measured; the ranking is R-14.16) |
| R-14.8 | Hardware ceiling baseline | `bench/rawread.c` per corpus and cache state: 64 neighbouring paths per thread, the fastest of several thread counts | "read files" column of LEADERBOARD.md | PASS |
| R-14.9 | Fairness: equivalent work verified against a reference, exact commands, latest stable versions, hyperfine with warm-ups and preparation, no special-casing, no benchmark removed for losing | `leaderboard.py` (reference in Python over explicit file lists; disqualification on any difference, scrutate included); tool-specific ignore files removed from corpora so every tool shares one ignore definition | LEADERBOARD.md "Disqualified answers" and "Exact command lines" | PASS |
| R-14.10 | Definition of #1: 95% CI of the ratio of medians over at least 20 runs per tool. A row stops at 20 only when every verdict holds at alpha 0.001 (Haybittle-Peto), else 30, and the run count is recorded per row | `boot_ratio` (10,000 bootstrap resamples, 50,000 at alpha 0.001), `bench/leaderboard.py` stop rule (on by default) | LEADERBOARD.md ratio column; replay of the 383 recorded 30-run rows: 266 stop at 20, no verdict changes (docs/RELEASING.md section 2) | PASS |
| R-14.11 | Publishable results from dedicated, otherwise idle hardware | runs wait for a low load average and record it | the only machine available is a developer laptop shared with other workloads (load 4 to 7 during the runs) | BLOCKED (no dedicated machine; the results are measured and reproducible but not publishable as #1 claims under this rule) |
| R-14.12 | Trade-off modes documented and benchmarked | text index stores no contents (small, needs the source tree) | ARCHITECTURE.md, LEADERBOARD.md Engine B section: the one configuration leads all eight metrics on both platforms, size and latency included, so no pair of metrics is left for a second mode to trade | PASS |
| R-14.13 | Leaderboard with medians, CIs and our rank; status per benchmark | `bench/make_leaderboard_md.py` | LEADERBOARD.md | PASS |
| R-14.14 | Claims of #1 only for PASS rows, linked to raw data | docs reviewed | README.md, COMPETITIVE.md | PASS |
| R-14.15 | Every benchmark tracked in CI; regressions beyond noise block the change | `benchmarks.yml`: the change and its base timed in one interleaved `leaderboard.py` run (`SCRUTATE_BASE_BIN`) on the small corpora, compared by `leaderboard_regression.py --within`, the ratio taken inside each block; every benchmark counted under cachegrind by `bench/instructions.py`, failing past 1% more instructions; one `benchmark gate` check | `benchmarks.yml` has not completed a run | PARTIAL |
| R-14.16 | Rank #1 on every benchmark, every metric, every declared platform | everything in R-4.6 | LEADERBOARD.md: #1 with significance on every row measured: 37 of 37 warm and 5 of 5 cold benchmarks on macOS arm64, 14 of 14 warm and 4 of 4 cold in the Linux aarch64 VM, 8 of 8 indexed metrics against Zoekt on macOS and 8 of 8 against Zoekt and livegrep in the Linux VM. Linux x86_64, the third declared platform, has no benchmark run (it is tested under emulation), and no result comes from dedicated idle hardware, which section 14 requires for a published #1 | PARTIAL |

## Tally

PASS 127 · PARTIAL 8 · BLOCKED 1 · FAIL 0 over 136 rows. [RELEASE_READINESS](RELEASE_READINESS.md#remaining-defects-and-incomplete-requirements)
lists the others with what closes each.
