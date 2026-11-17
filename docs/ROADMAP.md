<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-roadmap.svg" alt="Roadmap. What is done, what is blocked and what is next?" width="880">
</p>

# Roadmap

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Dependencies](DEPENDENCIES.md) &nbsp;·&nbsp; Next: [Research](RESEARCH.md)

**Contents**: [Done in this release](#done-in-this-release) &nbsp;·&nbsp; [Blocked: needs the first release](#blocked-needs-the-first-release) &nbsp;·&nbsp; [Next](#next)

<!-- End of head. -->

The plan after 0.1.0: done in this release, blocked on publishing, and next. Most items name the
gap they close in docs/TRACEABILITY.md.

## Done in this release

| Item | Where |
|---|---|
| Searching inside archives (zip, tar) and compressed files (R-2A.2) | `grep -z`, `src/archive.rs`, docs/CLI.md |
| Look-around and backreferences with a step budget (R-2A.2) | `grep -P` (PCRE2, stopped by its match limit with a warning) |
| Fuzzy patterns longer than 128 bytes (R-2A.2) | `--fuzzy`, up to 4096 characters (64-bit blocks with carries) |
| Fuzzy regex (R-2A.2) | `--fuzzy-regex[=N]`, `src/fuzzyre.rs` (bit-parallel Glushkov automaton, up to 64 positions) |
| Small trees (R-4.6) | the parallel walk starts at the first directory of any root that holds directories (one thread used to read all 60 directories of a 211-file repository while the search threads waited); a sequential walk feeding three search threads only for a flat root of up to 256 files; ignore files matched without a regex (each built a `GlobSet`: 100 to 400 µs, the global excludes file 0.4 ms of every run); 133 system functions bound on first use (0.4 ms); a grep-only argument parser |
| One very long line (R-4.6) | read into one exactly sized buffer, each thread filling its own part (100 MB: 12.6 ms on one thread, 4.0 ms on five), its UTF-8 checked a piece at a time as it is written: 32 ms on a 100 MB line in the final run (ugrep 38.8 ms) |
| Incremental re-search for editors (R-4.1) | `grep --index --update` (refresh the text index, then search only candidate files); `watch` and `serve --watch` keep the index fresh |
| Structural search without re-parsing (R-2B.2) | `tsquery` keeps captures per file by content hash: the Linux tree took about 25 s, the repeated query 1.6 s |
| Cross-file reference resolution with scope awareness (R-2B.4) | `refs` resolves occurrences through scopes, imports and qualified names for Python, JavaScript/TypeScript, Rust, Go, Java, PHP, C and C++ through `#include`, and C# (60 of 60 hand-checked resolutions right on networkx and ripgrep; `tests/resolve.rs`, `tests/resolve_java.rs`, `tests/resolve_php.rs`) |
| Language server (R-6.15) | `scrutate lsp`; the VS Code extension starts it; JetBrains through LSP4IJ |
| Sample for `ocaml_interface` (R-3.4) | all 74 grammars verified by samples (docs/LANGUAGES.md) |
| int8 semantic vectors (R-4.3) | schema 13: index about a fifth smaller, ranking quality within 0.001 MRR (docs/QUALITY.md) |
| `scrutate watch` (R-2B.1) | inotify on Linux (directories past the watch limit checked on a timer), ReadDirectoryChangesW on Windows (an overflowed buffer walks the tree), FSEvents on macOS loaded only by `watch` (the index's own check on a timer when it cannot load) |
| Benchmark dashboard (R-4.5) | `bench/make_dashboard.py` writes `docs/dashboard.html` from the results in `bench/results/` |
| Package definitions (R-12.17) | `packaging/`: Homebrew formula (installed and tested in a Linux Homebrew), Scoop and winget manifests (schema-validated), `.deb` and `.rpm` (built and install-tested in clean Debian and Fedora containers) |
| Third-party notices in releases | `tools/third_party_notices.py`; archives, wheels and the `.deb` and `.rpm` packages include `THIRD_PARTY_NOTICES.txt` |
| Incremental parsing of edited files (R-2B.1) | a long-running index (`watch`, `serve`, `lsp`) reparses an edited file against its previous tree, for the 21 grammars without an external scanner (with one, a reparse was seen to differ from a fresh parse); a 66 KB Go file: 5.5 ms to 2.0 ms per edit; `tests/incremental.rs` |
| Ranking quality beyond Python (R-2B.12) | the ranker is fitted on Python plus Rust, Go, Java, JavaScript and TypeScript repositories, with functions and types as answers; held-out results per language in docs/QUALITY.md |
| Indexed search precision (R-2B.1, R-4.6) | text index format 2: next-byte and position masks per posting; concurrent throughput against Zoekt went from 4% behind to 5% ahead (20% ahead, with p50 latency 0.37 of Zoekt's, in the final run) |
| Many files on macOS (R-4.6) | as many walking threads as performance cores (kernel contention made 11 threads slower than 5); the Linux and LLVM trees, 200,000 small files and the binary-heavy tree went from behind or level with ugrep to ahead of it (in the final run 15 to 30% on every row but two: 45% on the Linux search with 1.3 million matching lines, 8.5% on LLVM's case-insensitive one) |
| Regexes anchored at line starts (R-4.6) | only line starts beginning with the regex's prefix literals are examined: `^(a|aa)+$` on the pathological corpus, 52 ms to 18 ms (13 ms since its one-line file is read on several threads) |
| Start-up in git repositories (R-4.6) | `vendor/ignore` reads the global excludes path without compiling a regex: 0.4 ms of a 6 ms search of a 220-file repository |
| One definition of every index operation (R-6.15, R-13) | `ops.rs`: the CLI, the MCP server, `batch` and the Python bindings share arguments, defaults, limits, text and JSON; they had drifted (three limits for `callers`, references textual in MCP and resolved in the CLI) |
| An MCP server that fits how agents work today (R-6.15) | read-only and local hints on every tool, server instructions, answers within a byte budget with an offset to continue, `grep_code` on Engine A, no tool call blocked for more than 20 s by a first index build; from docs/research/agent-facing-search-2026-11.md |
| Index upkeep in one place (R-12.12) | `Repo`: update, and rebuild when the database is corrupt, for every front end that stays open |
| The index never disagrees with the walk about a pattern (R-2B.1) | one translation of the pattern for Engine A's matcher and the text index's trigram condition, checked by an in-process test |
| Linux-tree searches against ripgrep (R-4.6) | a busy `.gitignore` compiles its globs; files up to 1 MB read whole; no size lookup per file: in the test VM 7 to 16% ahead of ripgrep on 11 of the 13 text rows, 40% ahead with very many matches, 59% ahead on the complex regex |
| Cold trees on Linux (R-4.6) | a read counts as a disk read at 30 µs there (the macOS value, 75 µs, never fired), and a walk reading from the disk grows by a thread for every 16 slow reads, up to 64: the four cold Linux-tree rows went from level with git grep (two lost by under 1%) to 0.28 to 0.30 of the best competitor's time, 18 to 25% above the read-files ceiling |
| Heavy output through a pipe (R-4.6) | output that follows other output within a millisecond waits in a 64 KB buffer and leaves in one write, where every matching file used to cost a write and a wake-up of the reader: `return` in the Linux tree (1.3 million lines) in the Linux VM, from 0.95 of ripgrep's time to 0.60 |
| A read-files ceiling that is one (R-14.8) | `bench/rawread.c` gives each thread 64 neighbouring paths at a time, and the harness keeps the fastest of several thread counts; with one path at a time, its threads shared a directory and read a cold tree at a third of the disk's speed |
| Measurements a busy machine spoiled (R-14.9) | a benchmark where some tool's block medians differ by more than 25% is measured again, once, whatever its status, and keeps its first result beside the second; a warm row records how much of its corpus was in the page cache (on this 18 GB laptop the LLVM tree, 4.9 GB in 16 KB pages, no longer fits once a browser and an editor are open) |
| What agents ask of an MCP server now (R-6.15) | revision 2026-07-28 beside the handshake revisions (`server/discover`, a version in each request, `resultType`, cache hints on lists); with `serve --structured`, an `outputSchema` on every tool and the `--json` form of each answer in `structuredContent` (off by default: Claude Code shows its model that JSON in place of the text); `notifications/cancelled` honoured, each call on a thread of its own; progress reports with files read against the total while the first index is built, `reindex` runs or `structural_search` reads; large structured answers paged with the text (`next_offset`); `read_code` of a whole definition by name; the location at the start of every result accepted wherever a tool takes a symbol |
| Method calls on variables (R-2B.4) | `x.m()` resolves through the type the declaration of `x` states, as an annotation or as the thing it is built from, in Python, JavaScript/TypeScript, Rust, Go, Java, PHP and C#; Go methods are found by their receiver type across the files of a package |
| TypeScript path aliases (R-2B.4) | an import through `paths` or `baseUrl` of the nearest `tsconfig.json` or `jsconfig.json` (comments, trailing commas and a relative `extends` included) resolves to its file in `refs`, `deps` and `rdeps`; so does an import of a workspace package by its `package.json` name (`exports`, `module`, `main`, a build output standing for its source under `src/`) |
| Limits left in 0.1.0's first draft (R-2A, R-12.12) | smart case sees a letter written by its code point (`\x41`); the LSP server replaces a damaged index, from a save or from a request; the watcher's test of one path decides in the walk's order (a `.ignore` at any level before every `.gitignore`) |
| Limits left in 0.1.0's second draft (R-2A, R-2B.1, R-2B.4) | calls inside Rust macro arguments are call edges; an MCP answer checks the files it cites and re-reads a changed one; files on `/proc`, `/sys` and the other kernel pseudo filesystems are read to the end; on Unix a `chmod` alone is seen (the index keeps the status-change time); an emptied `chunks` table is damage and is rebuilt; a JavaScript `var` belongs to its function; one file argument prints without its name, as in ripgrep, with `-H` and `-I`; C++ constructors, destructors, member templates and conversion operators declared in a class body are methods; `\,` keeps a comma in `SCRUTATE_INDEX_EXCLUDE_DIRS` |
| Training reads the engine, and the mirror ended (R-5.3) | the ranker and the vector model are fitted on rows the engine writes (`batch --features`, the hidden `views` command), the benchmark builders read files and words through it, and the Python reference engine that mirrored it bit for bit on 7 of the 73 grammars is gone with `bench/parity.py`; `tests/ranking_pins.rs` and `bench/quality.py` watch ranking (docs/QUALITY.md) |
| Function order on glibc Linux (R-4.6) | release builds for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` link twice, the second time with lld's `--symbol-ordering-file` from `crates/scrutate/startup.order` under the build's own names (`tools/order_file.py --elf`); docs/RELEASING.md section 2 |
| An LSP4IJ template for JetBrains IDEs | `integrations/jetbrains/lsp4ij-template` fills the Language Servers dialog in one import (name, `scrutate lsp`, a file name pattern per language); the `jetbrains` workflow imports it with LSP4IJ 0.21.0 and runs Go to Declaration through it |
| Benchmark corpora without hard links (R-14) | `bench/corpora.py` copies (APFS clones) where it linked: a hard-linked file costs every tool twice as long to open on macOS |

## Blocked: needs the first release

Publishing was declined on 2026-11-08 and asked for on 2026-11-11, on one condition: every way to
install and use the tool works first. The repository is public on GitHub
(`github.com/scrutate/scrutate`); docs/RELEASE_READINESS.md lists what ran
here and what ran on GitHub. The order: one green run of `ci.yml` (done), the settings in
docs/RELEASING.md, public (done), tag `v0.1.0`.

- The release workflow. `ci.yml` is green since run 43013926055
  ([Releasing](RELEASING.md#7-what-a-published-release-adds)).
- Signed releases with Sigstore attestations (`release.yml` has the steps and a dry run that
  rehearses them; they need one run on GitHub). The free-threaded `cp314t` wheels are built,
  installed and tested in CI on all six runners (`ci.yml`, run 43408907893).
- The packages: tap, bucket and winget pull requests with the release checksums (steps in
  packaging/README.md).
- `cargo install scrutate` from crates.io: `scrutate-ignore`, then `scrutate` with the int8 model;
  the upload needs the owner's crates.io account at the release ([Releasing](RELEASING.md#7-what-a-published-release-adds)).
  No `cargo publish --dry-run` of the current layout is recorded: the one log,
  `bench/results/platforms/packages/cargo-package.log`, is of an older layout and failed to verify
  the package.
- A comment on GitLab merge requests (R-12.16; on GitHub the Action's `comment: true` posts
  `scrutate review` on a pull request, and the GitLab template saves it as a Code Quality report),
  and weekly benchmark runs for the dashboard (R-4.5).
- Now that `ci.yml` is green, one job or setting each (docs/research/github-ci-2026-11.md): wheels
  to PyPI by trusted publishing (the jobs are in `release.yml` and wait for the `PYPI_PUBLISH`
  variable), CodeQL default setup for Rust, OpenSSF Scorecard.

## Next

Measurements in this section that name no committed result file or CI run were taken on the
development Mac and are not recorded in the repository.

- Python tooling to Rust (R-13.1): one `xtask` crate in the workspace, not a default member and never
  shipped, run as `cargo xtask NAME`, replacing the 20 maintenance scripts in `tools/` and
  `packaging/validate_manifests.py` (their 11 tests in `python/tests/` become the xtask's
  specification). After 0.1.0, since the release scripts are the path the first `release.yml`
  run proves. In order: delete `name_research.py` (its output,
  docs/name_research.json, is committed); the CI checks (`check_doc_shift`, `unsafe_count`,
  `gate_parity`, `gitleaks_config`, `fuzz_lock`, `jump_tables`), their Python tests as the
  specification; `order_file`; the release scripts (`check_version`, `release_notes`,
  `fold_changelog`, `sbom`, `third_party_notices`), each against golden files of the Python
  output; last the docs generators (`gen_assets`, `gen_doc_assets`, `gen_dependencies_md`,
  `gen_test_results`, `traceability_tally`, `generated_page`), byte for byte.
- Small trees and start-up, what is left (the measured state is in
  [Architecture](ARCHITECTURE.md#engine-a-universal-instant-search-cratesscrutatesrcinstantrs)).
  A one-file directory search runs fewer instructions than GNU grep (17 M against its 20.8 M) in the
  same CPU time but touches 20 more pages (a Rust hello-world: 291 page reclaims, 1.9 MB):
  anonymous constant tables the linker cannot order by name (12 pages), relocated data (6) and
  the 64 KB line buffer the searcher zeroes. Wall time against GNU grep depends on load (1.87 ms
  against 1.97 ms in steady runs, behind in interleaved blocks under other work); an idle-machine
  benchmark settles it. Ruled out: binary size (0.07 ms at 85 MB; a file written minutes ago
  costs 0.2 ms more) and a condition variable in place of the idle walker threads' short sleeps
  (3.9 ms against 3.7). A linker order file did nothing under ThinLTO (585 page faults against
  586), since ld64 cannot place the functions ThinLTO promotes; with one object it took 131 page
  reclaims out. Profile-guided optimisation gained 2 to 3.5% on the 220-file tree, not adopted:
  it needs a 13 MB profile in the repository and every release build.
- Peak memory against GNU grep. `-j1` holds less than ugrep and ripgrep on every tree and more
  than GNU grep, a 235 KB C program (macOS):

  | Tree | `-j1` | GNU grep |
  |---|---|---|
  | a one-file directory | 2.5 MB | 1.95 MB |
  | the ripgrep sources | 3.3 MB | 2.3 MB |
  | py-big | 4.6 MB | 2.8 MB |
  | the Linux tree | 6.5 MB | 3.4 MB |
  | LLVM | 8.3 MB | 6.4 MB |

  On a large tree the rest is mostly allocator memory freed and kept (per-CPU magazines:
  `MallocMaxMagazines=1` in the environment takes 1.1 MB off the Linux tree; a process cannot set
  it for itself), the ignore matchers (1.2 MB there) and whole directories read at once. A
  one-line file is held whole (103 MB for 100 MB) for a regex, `-i`, `-w`, `--json` or color; a
  plain `-F` literal with plain output or `-c` reads it in two passes and holds 3 to 5 MB. Under
  the 64 MiB split size such a file still goes through the stream reader, whose buffer grows to
  the line; the two passes could serve it. The default mode holds ordered output on one large file
  with many matches (23 MB); 2 MB ranges in place of 8 MB halve that for 2.4 to 3.5% more
  instructions, not taken.
- The function order on glibc Linux, measured. Of 5.8 MB resident at the start of a small search
  before it, 3.5 MB was the executable's text, spread over 85 MB with the kernel mapping 64 KB
  around each fault; 0.5 MB is data relocated at load. The release build now links the macOS
  order with rust-lld (docs/RELEASING.md section 2); ci.yml's `rust` job prints the minor page
  faults of `--version` and a one-file `grep` without and with it. First run (CI run 43463286517,
  median of 7): a one-file `grep` takes 334 to 321 faults on x86_64 and 322 to 297 on aarch64
  (4% and 8% fewer); `--version` stays where it was (365 to 369, 371 to 372), since it touches
  little code past the start. The musl builds keep GNU ld and no order.
- Large trees on macOS are bound by `open` (docs/ARCHITECTURE.md): the Linux tree takes 0.52 s just to
  open and read on the five performance cores, a search 0.58 s; the rest above that floor is the
  search. The corpora `linux` and `linux-nodot` shared files by hard links, doubling every open's
  cost on macOS for every tool; `bench/corpora.py` now copies them (APFS clones). `openat`
  through the directory's descriptor does not lower the floor: 13.3 µs a file against 12.4 µs by
  path.
- Linux on dedicated hardware: every Linux leaderboard run (`bench/results/linux-aarch64/`)
  comes from a VM on the development Mac (5 CPUs; 8 for the `*-6602` run), on a virtual disk on its SSD. The x86_64
  server runs COMPETITIVE.md quotes have no committed result, and `bench/results/final.json`, an
  early run of 2026-11-08 on a 2-core x86_64 Xeon, is used by no document. Two values fitted on
  the VM need checking on NVMe and network
  disks: the read time that counts as a disk read (30 µs) and the most threads for a cold tree
  (64). A cold tree there also costs about 90 MB on 64 threads against 16 MB warm (`-j` bounds
  both).
- Windows: nothing is measured. It takes the macOS values (threads: the cores, at most 12; cold:
  twice the cores, at most 24; a disk read: 75 µs) from the `OTHER` row in
  `crates/scrutate/src/policy.rs`, where a measured row goes.
- Benchmark leadership on dedicated idle hardware (R-14.11): every leaderboard number comes from a
  developer laptop shared with other work, and no leaderboard run is on Linux x86_64; in the last
  macOS run six of 37 warm
  rows were measured twice for that reason. An x86_64 run emulated on this laptop was dropped:
  gcc crashes building the grammar sources there, so it would
  time the emulator. A run of `bench/run_macos.sh` and
  `bench/competitors/linux_bench.sh` (which takes an x86_64 host as it is) on dedicated machines
  closes it (docs/research/known-limits-2026-11.md prices the hardware).

### Known limits that stay in 0.1.0

- A method call on a call's result resolves through the callee's declared return type, and in
  Rust through a table of standard methods ([CLI](CLI.md#refs)). It stays undecided when the
  callee declares no type, returns a type parameter, takes a closure or lets the caller pick its
  type (`collect`, `into`), or is a method of an outside type the table does not list that holds
  a repository type or whose type arguments are not written (`let m = HashMap::new()`, then
  `m.get(&k).unwrap().ok()`); and in every language but Rust, when the callee is a method of an
  outside type. A precise answer needs a type checker.
- In `refs`, a plain name in the arguments of a Rust macro other than the standard expression
  macros is `macro argument`, undecided: only expanding the macro says what `text` in
  `mymac!(text, 3)` is.
- C and C++: `refs` reads no include paths of the build (`-I`, `compile_commands.json`): an
  include is found beside the including file or by its path suffix. A call on a value
  (`s.area()`, `p->next()`) stays undecided: the value's type is not followed
  ([CLI](CLI.md#refs)).
- `watch` takes the `notify` crate at 9.0.0-rc.9, pinned (only 9.0 reports an overflowed Windows
  buffer as a rescan, and 8.x has no backport); 9.0.0 itself is not out (crates.io on 2026-11-16). Moving to it is a
  version bump once it is.
- Call forms the grammars do not tell apart: a Verilog task call on a member (`obj.m(1);`), which
  tree-sitter-verilog 1.0.3 parses as an error, records no call edge, and a VHDL indexed signal
  `a(i)` declared in another file than the use is recorded as a call.
- Windows and Intel macOS are tested on GitHub's runners only ([Install](INSTALL.md) has the
  runners and the Unix-only tests): `release.yml` builds their archives, but no release has
  run yet, so none has been installed on a machine or benchmarked there.
- No leaderboard benchmark has run on x86_64 Linux, and none on dedicated idle hardware (R-14.11
  above).
- The release binaries carry no platform code signature: no Apple notarization, no Windows
  Authenticode. The integrity check is the Sigstore attestation and `SHA256SUMS`;
  [Install](INSTALL.md#unsigned-binaries-on-macos-and-windows) has the Gatekeeper and SmartScreen
  workarounds. Next: apply to SignPath Foundation (free for open source) after 0.1.0.
