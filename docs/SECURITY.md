<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-security.svg" alt="Security. What can a hostile repository, query or client do?" width="880">
</p>

# Security assessment (0.1.0)

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Test results](TEST_RESULTS.md) &nbsp;·&nbsp; Next: [Traceability](TRACEABILITY.md)

**Contents**: [Threat model](#threat-model) &nbsp;·&nbsp; [Controls and evidence](#controls-and-evidence) &nbsp;·&nbsp; [Running git](#running-git) &nbsp;·&nbsp; [The session socket](#the-session-socket) &nbsp;·&nbsp; [The main checkout's index in a linked work tree](#the-main-checkouts-index-in-a-linked-work-tree) &nbsp;·&nbsp; [A repository's language tables](#a-repositorys-language-tables) &nbsp;·&nbsp; [Setup and the Grep hook](#setup-and-the-grep-hook) &nbsp;·&nbsp; [The structural rewrite](#the-structural-rewrite) &nbsp;·&nbsp; [The review comment](#the-review-comment) &nbsp;·&nbsp; [Secrets in answers](#secrets-in-answers) &nbsp;·&nbsp; [Verified symbol edits](#verified-symbol-edits) &nbsp;·&nbsp; [Dependency source](#dependency-source) &nbsp;·&nbsp; [Known gaps and residual risks](#known-gaps-and-residual-risks) &nbsp;·&nbsp; [Reporting](#reporting)

<!-- End of head. -->

Scope: the `scrutate` binary (Engine A, Engine B, CLI, MCP server) and the Python bindings, as
built from this repository on 2026-11-11. A self-assessment by the authors with the tests named
below; it is **not** an independent audit or penetration test.

## Threat model

Scrutate reads source trees the user chooses and writes one cache directory (`.scrutate/`) inside
each indexed repository, which on Unix only its owner can enter (mode 0700). Outside it, it writes package copies and their index under the user cache for `--deps` and
`scope: "deps"` ([Dependency source](#dependency-source)), and one small state file, `frecency.json` under
`$XDG_STATE_HOME/scrutate/`, the read counts `files` ranks by ([CLI](CLI.md#files)): each
repository's canonical root and the paths of files read in it, with a count and a time. Nothing in
it is run or opened: a path in it only ranks a file the walk already found. Delete it at any time
to forget the counts. In a linked git work tree
scrutate also reads the index of the repository's main checkout (below).

The assets: (1) the confidentiality of the source code, (2) the integrity of the machine running
the tool, (3) availability (no hangs or resource exhaustion on hostile inputs). Attackers: a
**hostile repository** (crafted files, names, symlinks, `.gitignore`, a corrupted or planted
`.scrutate/` directory), a **hostile query** (patterns), a **hostile MCP client** (malformed
JSON-RPC, path escapes), and **another local process** that reaches the socket a running `serve`
offers (below). Writes outside a repository happen on the user's word only: `scrutate setup`
writes an agent's configuration files, and the Grep hook it installs appends to a counter file
(below). Only `scrutate tsquery --rewrite` with `--write`, and the MCP `edit_symbol` of a server
the user starts with `serve --allow-edits`, write source files ([below](#the-structural-rewrite),
[Verified symbol edits](#verified-symbol-edits)).

<p align="center">
<img src="assets/diagram-trust-boundaries.svg" alt="Trust boundaries: repository content, patterns, MCP requests and the .scrutate directory, with the controls on each boundary; no network." width="880">
<br><sub>What crosses into scrutate, and the controls from the table below on each boundary. The network is not used.</sub>
</p>

## Controls and evidence

| Area | Control | Evidence |
|---|---|---|
| No code execution from repositories | Files are parsed as data (tree-sitter, regex); nothing is evaluated, imported or run. There are no build systems, repository hooks, `.scrutate.toml` scripts or plugins. The binary starts another program in three cases: `grep -z` decompresses bzip2, xz, lzma, lz4, zstd, brotli and `.Z` files by running the system's `bzip2`, `xz`, `lz4`, `zstd`, `brotli` or `uncompress` from `PATH` on the file (through `grep-cli`, as ripgrep does; gzip, zip and tar are read in process); and `impact` (the command and the MCP tool) and `review` run `git` from `PATH` in the root to read the diff, as `read --history` (and `read_code` with `history`) does to read a definition's commits (see [Running git](#running-git)); and `scrutate hook run` runs the `rg`, `grep` or `find` it was given, from `PATH`, or this binary's own `grep` for an `rg` whose arguments `scrutate grep` reads the same way ([below](#setup-and-the-grep-hook)). Without `-z`, `impact`, `review`, a history or `hook run`, no search starts a program. `scrutate setup claude` runs `claude mcp add-json` or `claude mcp remove` from `PATH` when it is there ([below](#setup-and-the-grep-hook)). | Code review: the engine's process spawns are `grep_cli::DecompressionReaderBuilder` in `archive.rs`, `Git::command` in `git.rs` and `command` in `shellhook.rs` (from `run_main`), and `setup.rs` runs `claude` as above; `tests/impact.rs` (a revision written as an option is refused, and no file is written). |
| No network | The binary makes no network connections and has no HTTP/DNS dependency. | `cargo tree` (docs/DEPENDENCIES.md) has no networking crate; offline build and run verified in a proxy-restricted environment. |
| Filesystem confinement | `read_code` / `read()` reject paths that resolve outside the repository root, and `read_code` refuses a path that is not a regular file. The index is written only under `<root>/.scrutate/`; another checkout's index is only read, in a linked git work tree, from its main checkout ([below](#the-main-checkouts-index-in-a-linked-work-tree)). Segment files are written to a temp directory *inside* `.scrutate/seg/` and renamed into place (no `/tmp`, no predictable world-writable paths). A repository's `exclude_dirs` (`.scrutate.toml` at the root) can only leave directories out by name: the file is read as other repository files are (a regular file inside the root, at most 1 MB; through a link only when the resolved file is one), and its names are never read as patterns. A `.scrutate` that is a symbolic link, or holds one (checked three levels deep, no link followed), is refused, and files of fixed names are written with remove and `create_new`, so nothing outside the repository is written or deleted through a link. A `--root` that does not exist or is not a directory is an error, and nothing is created. `tsconfig.json`, `jsconfig.json`, `Cargo.toml` and `go.mod` are read only as regular files inside the repository, of at most 1 MB. A path a tool call names, a file an answer cites and a file a watch event names are indexed only when the path resolves inside the root: a linked folder on the way that leads outside makes the file count as missing. A tool-call path that leaves the root by its own parts (`..`, an absolute path elsewhere, a share on another machine) is refused with nothing looked at, so on Windows no connection is made to a machine a call names. The language server reads a document from disk only under the root, links on the way included, and resolves no path on another machine's share than the root's: for a document outside it (another folder, a URI that leaves through `..`) it uses the text the editor sent, or nothing. | `tests/robustness.rs::symlinks_ignores_and_nested_repos` (symlink escape not followed by default), bindings test `r.read("../etc/passwd")` → error, lsp `a_document_that_is_not_under_the_root_is_never_read_from_disk`, mcp_tools `a_path_through_a_linked_folder_reads_nothing_outside_the_repository`. |
| Symlinks | Not followed by default (Engine A and B); `-L` follows with loop detection (the `ignore` walker). | robustness: loop + dangling + escaping symlinks. |
| Permission-aware | Unreadable files are counted as errors (`--stats`) and skipped, never fatal. | robustness `unreadable_files_are_reported_not_fatal` (skipped when running as root, as in CI containers; the error counter exercises the code path). |
| Resource exhaustion: regex | Rust `regex` (linear time, no backtracking) with compiled-size limits (20 MB regex, 10 MB DFA); oversized patterns are an error. | robustness `regex_limits_and_errors_are_clean`: `(a*)*b` on a 10 000-char line finishes; `(a{1000}){1000}` is an error. |
| Resource exhaustion: files | Engine B skips files > 1 MB and binary files (NUL in the first 4 KB); chunk text and body are capped at 200 lines (40 for a container) and 32 KiB, and the definitions of one file share a budget of 1 MiB plus 8 bytes per byte of the file, past which each keeps its first line, so a one-line file of 8,000 nested functions keeps 0.34 MB of compressed chunk text and indexes with a 44 MB peak; tree-sitter parse per file; Engine A streams with bounded batches; `--max-filesize`, `--max-depth`, `--max-results`, `--max-count`. With `-z`, an archive or compressed file stops at 8 GiB expanded or a million members, a member at a line longer than 64 MiB, and a tar member's name from a long-name or PAX header is read up to 64 KiB, so a small archive cannot make `grep -z` hold gigabytes. Index time stays linear in nesting depth: a file of 60,000 nested Lisp calls, nested lists or 50,000 nested HTML elements indexes in under a second. The resolver does not read a file nested more than 2,000 levels deep, so such a file cannot overflow the stack of `refs`, the language server or the MCP server, or hold them for minutes. | robustness `hostile_text_shapes_never_panic` (5 MB line, 200k lines, 20 000-deep nesting); parse_index `deeply_nested_lisp_and_html_parse_in_linear_time` (Lisp and Scheme to 60,000 levels, bare lists to 80,000, HTML to 50,000) and `chunk_text_has_a_byte_cap_on_a_one_line_file_of_nested_functions`. |
| Hostile text | Invalid UTF-8 (lossy decode), BOM, CRLF/CR, UTF-16 with a byte-order mark (transcoded by both engines), multibyte boundaries, emoji identifiers, 255-byte names, spaces/unicode in paths. This suite found a real UTF-8 boundary panic, now fixed (`text::prefix_bytes`). | same test; `parser_fuzz_smoke` (400 random inputs); proptest `parser_yields_well_formed_chunks`. |
| Index corruption | `PRAGMA quick_check` on open for a database up to 64 MiB (`SCRUTATE_QUICK_CHECK_MAX_BYTES`; a larger one is checked by `scrutate check` and rebuilt when an operation meets damage), once per state of the database file (a passing check is recorded with the file's identity and data version, and any change to the file runs it again); every segment array validated for dtype/shape/length on open; `scrutate check` runs `integrity_check` + deep validation (sorted ids, posting ranges, IVF permutation) and optional file re-hashing. Damaged indexes are rebuilt automatically, also when a search first meets the damage in a segment array (a posting row, a pointer or a cluster table that points outside the segment); a planted non-database file is replaced; locked ≠ corrupt (a live database is never deleted). The artifact store is part of the index: an artifact that does not decode, kept texts that do not decode, a file that names an artifact the store does not hold, and rows that disagree with their artifact are found where an update reads them (and by `scrutate check`) and rebuilt by the same rule. | `tests/artifacts.rs::damage_to_the_store_is_found_and_the_index_built_again`; robustness `corrupted_index_is_detected_and_rebuilt` (5 corruption scenarios); cli `a_posting_row_outside_its_segment_triggers_a_rebuild`; `concurrent_indexers_and_readers_do_not_corrupt` (3 writers + 2 readers, 30× repeated, `integrity_check` ok). |
| Index privacy | `.scrutate/` holds a compressed copy of every indexed chunk's text, including chunks of files the owner made private (`chmod 600`). On Unix it is made with mode 0700, whatever the umask, so another user can read nothing inside it; an existing one with any other mode (an earlier version's, made under umask 022) is set to 0700 by the next command that writes it. The files inside keep the umask's mode, which does not matter while the directory above shuts others out. A `.scrutate` another user owns is left as it is. Only `.scrutate` itself changes mode: no file of the repository, and nothing outside it. On Windows the directory inherits the repository directory's inheritable permissions, as every new directory does. | cli `the_index_directory_is_private_to_its_owner_under_umask_022` (`index`, `index --text`, `callers` and the `tsquery` cache each make it 0700; a 0755 one is tightened). |
| Cache files | `.scrutate/tsquery/` (captures by content) and `.scrutate/answers/` (what `callers` and `callees` learned) are read only as regular files within their size limit (128 MB and 64 MB), never through a symbolic link, and decoded with every length checked; a file that does not decode, or whose header names another scrutate, executable, index database or root, counts as no file. They are written whole to a temporary name and renamed into place: a tsquery file only after the index directory check refuses a symbolic link anywhere in `.scrutate/`, an answers file after a check that `.scrutate/` and `.scrutate/answers/` are directories and no links (the whole check runs when either is made; one answer is written per definition a walk asks about, and the whole check on each made an impact walk's time grow with the square of the change). A planted cache file that does decode is trusted as the checkout's own index is: it can make the answers it names wrong until it is deleted (deleting `.scrutate/` is always safe). | `files::tests::a_cache_file_is_read_only_as_a_regular_file_within_its_limit`; `answers::tests`; the tsquery tests in `tests/cli.rs`. |
| Panic containment | A panic in the CLI prints one diagnostic line and exits 2. A panic inside an MCP tool call is reported as a tool error and the server keeps running: the session closes the reader the call ran on and opens another. The Python bindings and the language server keep a panic inside its call the same way. An index update that panics rolls back at once, and the next update clears a write transaction left open. A panic in the MCP server's first index build is reported on stderr and in each tool call's answer; calls stop waiting for it, and `serve` still exits when its input closes. | `cli::main_exit`, `session.rs`; `session::tests::a_panic_inside_one_call_is_that_calls_error_and_the_session_goes_on`; mcp_protocol `a_first_build_that_panics_is_reported_and_the_server_still_answers_and_exits`. |
| Supply chain | 213 crates in the binary's dependency graph (every platform's), each with a permissive licence (MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD, ISC, Zlib, Unlicense, 0BSD, Unicode-3.0, CC0-1.0, BSL-1.0), plus one data file compiled in, a Han fold table derived from Unihan 18.0.0 (Unicode-3.0) and OpenCC ver.1.4.2's `JPShinjitaiCharacters.txt` (Apache-2.0, pinned by SHA-256 in `tools/gen_han_fold.py`), `crates/scrutate/data/han_fold.txt`; `Cargo.lock` committed; `cargo audit` over the 255 locked crates: **0 vulnerabilities, 0 warnings** (RustSec advisory DB of 1,509 advisories, updated 2026-11-15). The bundled SQLite is 3.54.1, from `libsqlite3-sys` 0.40.0 under `rusqlite` 0.43.0; it was first upgraded for RUSTSEC-2026-0459 at `rusqlite` 0.41 and `libsqlite3-sys` 0.39. `ignore` is vendored with eleven documented changes (`vendor/ignore`, listed in its README) and published as its own crate, `scrutate-ignore`, so a crate from crates.io builds with the same walker; the SBOM marks it as a modified copy of `ignore`. `zip` 9, used only by `grep -z`, has no open advisory; `pyo3` was upgraded to 0.30 (2 advisories in 0.27). SBOM generated per release (`tools/sbom.py`, CycloneDX 1.5). `cargo deny check` runs on every push to main, every pull request, once a week and on the tagged commit before a release against `deny.toml`: an advisory (vulnerable, unmaintained, unsound or yanked), a licence outside the list above, a wildcard version, or a crate from a registry other than crates.io or from git fails it. | `cargo audit` over `Cargo.lock` (docs/TEST_RESULTS.md holds an earlier run); SBOM in release artifacts; `deny.toml` and the `deny` job of `.github/workflows/security.yml`. |
| Reproducible builds | Release profile pins codegen; `Cargo.lock` and a pinned toolchain (`rust-toolchain.toml`: 1.100.0). Two clean builds in two build images (`rust:1-slim-bookworm` and `golang:1.26-bookworm` with rustup, fresh containers, linux/arm64) produced the same SHA-256 on 2026-11-16 with `rustc 1.100.0 (0ef1bc386 2026-11-09)`: `a3a0549d...` (`bench/results/platforms/repro/`). The binary the Linux leaderboard measured, `261ed1a2...`, is the Rust 1.99.1 build from before the toolchain moved to 1.100.0, so it is not this hash; the benchmarks rerun on the release binary at release time (docs/RELEASING.md). Reproducibility on other operating systems, or across source paths in general, is not claimed. | `tools/repro_docker.sh`, `bench/results/platforms/repro/`; `tools/reproducible_check.sh` (same machine). |
| Signed releases | "Signed" means: every file on the release carries a SLSA build provenance attestation (`actions/attest`), signed through Sigstore with the workflow's OIDC identity and logged in Sigstore's public transparency log (SLSA v1.0 Build Level 2), and is listed in `SHA256SUMS`; the SBOM is attested to the archives. `gh attestation verify FILE --repo scrutate/scrutate --signer-workflow scrutate/scrutate/.github/workflows/release.yml --source-ref refs/tags/v0.1.0` checks a file ([Install](INSTALL.md#from-a-release-archive)). No platform code signature (see Known gaps). | `.github/workflows/release.yml`, **not yet executed**; no release has been signed yet. |
| Workflows | Every action is pinned to a commit, checkout keeps no credentials, and each job gets only the permissions it names. `zizmor` 1.32.0 with `--persona pedantic` and its online audits checks `.github` on every push to main, every pull request, once a week and on the tagged commit before a release, and `actionlint` 1.7.12 with shellcheck on every `run:` script checks the workflows on the same runs; a finding fails the job. | the `zizmor` and `actionlint` jobs of `.github/workflows/security.yml`. |
| `unsafe` count | Each file's count of the `unsafe` keyword (line comments and plain string literals left out) must equal the table in the [`unsafe` inventory](#known-gaps-and-residual-risks), checked on every push to main, every pull request, once a week and on the tagged commit before a release, so a new `unsafe` cannot land without its line there. | `tools/unsafe_count.py`; the `unsafe` job of `.github/workflows/security.yml`. |
| Secrets in the tree | No tracked file holds a secret: gitleaks 8.30.1 with its own rules, installed by the SHA-256 it publishes, reads the tree on every push to main, every pull request, once a week and on the tagged commit before a release, against `.gitleaks.toml` and no allow list: a line its rules flag that holds no secret carries `gitleaks:allow` with the reason, so no file is exempt as a whole (gitleaks 8.30.1 applies an allow list's paths alone, whatever its condition). `tools/gitleaks_config.py` fails the job and the gate unless the config holds `title` and `[extend] useDefault = true` alone, its keys read in any case as gitleaks reads them (a rule, a disabled rule, an allow list or an extended path can each narrow the scan), and when a `.gitleaksignore` is committed beside it. The gate also plants a token in `intel.rs` and in `secrets.rs` in a copy of the tree and requires gitleaks to find each. The tests run scrutate's own redactor over every tracked file too, so test samples are built at run time; a `format!` template that builds one (`sk-ant-api03-{}`) holds no secret, and neither scanner flags it. | the `gitleaks` job of `.github/workflows/security.yml`; `tools/gitleaks_config.py` and python/tests/test_gitleaks_config.py; tests/secrets.rs `no_tracked_file_holds_a_secret`. |
| Coverage-guided fuzzing | Four libFuzzer targets (`fuzz/`, nightly toolchain, AddressSanitizer): the `.npy` reader on arbitrary bytes; the text-index soundness property (a text a pattern matches must satisfy the pattern's trigram condition); the edit-distance matcher; every parser tier on arbitrary text under 60 file extensions. They found four real defects, each fixed with a regression test and its crash input kept in `fuzz/regressions/`: chunk line numbers shifted after form feeds (Python-style line splitting); a truncated string array panicked the `.npy` reader; a shape whose element count overflowed wrapped in release builds and could have produced an over-long slice view (undefined behaviour, now a saturating count and checked sizes); a zero-width string array with a huge shape requested a multi-terabyte allocation. Every week each target fuzzes for 45 minutes from the corpus the last run kept, and a crash, timeout or out-of-memory input fails the run and is kept as an artifact. | `bench/results/fuzz/` (run logs), `.github/workflows/fuzz-weekly.yml`, `fuzz/regressions/`, unit tests `npy::header_tests::damaged_headers_are_errors_not_panics`, `parse::tests::form_feeds_and_lone_cr_do_not_shift_line_numbers`. |
| Network file systems | A repository on an SMB share (Samba 4.23 server, macOS `smbfs` client): Engine A output identical to the local copy; Engine B indexes, answers, survives three concurrent `index` processes with readers, and passes `check --files`. Found and fixed: a blocking file lock fails at once on SMB (EAGAIN), so the text index polls `try_lock`. One host only: several machines sharing one index over a network file system is not supported (SQLite's documentation warns that its locking is unreliable there). | `tools/netfs_check.sh`, `bench/results/platforms/netfs-smb-macos.txt`. |
| Untrusted `.scrutate.toml` | The configuration file sets option defaults (booleans, numbers, sizes, globs, file type names), directory names to leave out and language tables (extensions and regex patterns); it cannot name programs or paths to execute. Unknown keys are rejected. `follow = true` in a repository's own file is ignored with a warning, so a cloned repository cannot make a search leave it through a symbolic link; the flag, the environment and the user's own file still can. | `config.rs` tests; `tests/cli.rs` (a link out of the repository). |
| MCP server | Reads the repository and writes `.scrutate/` and the frecency file above, the package cache for `scope: "deps"`, and, only under `serve --allow-edits`, the file an `edit_symbol` call names ([Verified symbol edits](#verified-symbol-edits)); path arguments are confined. It takes requests on stdio, and offers its open index to command line calls of the same user through `.scrutate/session.sock` on Unix or a named pipe on Windows (no network port; see [The session socket](#the-session-socket)). Malformed input gets the JSON-RPC error the specification names, and the server keeps reading: a line that is not JSON or not UTF-8 gets -32700; a message that is not an object, an empty batch and a request without `method` get -32600; an unknown method gets -32601; an unknown tool and a missing tool name get -32602; an argument of the wrong type, or a negative number, is a tool error that names the argument. Notifications are never answered, and a client's own responses are ignored. An error quotes at most 200 characters of an argument. Each tool call has a thread of its own, at most 32 at once (further calls run on the reading thread), and at most 8 read the index at once. | `mcp.rs` and its tests; `tests/mcp_protocol.rs`, `tests/mcp_tools.rs`, `tests/cli.rs`; the -32700 for a line that is not JSON and the 200-character quote (`fmt.rs`) are checked by reading the code. |
| Python bindings | Engine behind a session (calls side by side, writes one at a time, a panic kept inside its call); `GrepIter` borrow-checked by PyO3; free-threaded declaration (`gil_used = false`) made after this review; stable ABI build. | `python/tests/test_bindings.py`. |

## Running git

`impact` and `review` run the `git` program found on `PATH` (on Windows the current directory is
not searched first) in the root: `rev-parse`, `merge-base`, `config --get-regexp`, `diff-index`
(`diff-tree` between two commits; raw and `-U0`), `ls-files --others` and one `cat-file --batch`.
`read --history` runs `--version`, `config --get-regexp`, `rev-parse --is-shallow-repository` (and in a shallow clone
`rev-parse --git-path shallow`, whose file of commit ids it reads), `cat-file -e`, `diff-index -p
-U0 HEAD` with `--literal-pathspecs` after `--`, and `log -L` with `-s`, each on the path of an indexed
file, never a path the caller writes: inside `-L start,end:path`, `HEAD:./path` and after `--`, a
path that starts with `-` or holds a colon reads as a path. Nothing in the repository names this
program or its arguments. Every call goes through one builder, `Git::command`, which runs only
these commands and refuses any other (`tests/git_spawns.rs` fails on a git started anywhere else),
and every call only reads: `--no-optional-locks` (git writes no index file),
the diff by plumbing that never refreshes the index (`git diff` writes `.git/index` back when a
file's stat changed, holding `index.lock` against your own `git add` meanwhile), `-c
core.fsmonitor=false` (no file system monitor program), `-c log.showSignature=false` (no
`gpg.program` to check a signed commit), `--no-ext-diff` and `--no-textconv` on every diff and log
(no external diff or text conversion program from git's configuration), and `GIT_NO_LAZY_FETCH=1` (a
partial clone fetches no object it lacks, which would write `.git/objects` and run the remote's
upload program or `core.sshCommand`; `impact` then says to fetch them first. git 2.44 and later
read it; an older git fetches).

The one argument a caller gives is a revision (`--base`, the tool's `base`, `review --head`). An
empty one, or one starting with `-`, is refused before git runs, so `--base=--output=x` cannot
become git's `--output` and write a file. It goes as one argument, `REV^{commit}`, to
`git rev-parse --verify`, and later calls get the commit id that prints. Each side of `A...B` is
checked the same way.

Of the environment, only a relative `GIT_DIR`, `GIT_WORK_TREE`, `GIT_OBJECT_DIRECTORY` or
`GIT_COMMON_DIR` changes: it is made absolute against the command's working directory, where git
itself reads it, so starting git at the root does not point it at another repository. The three
pathspec variables git refuses beside `--literal-pathspecs` (`GIT_GLOB_PATHSPECS`,
`GIT_NOGLOB_PATHSPECS`, `GIT_ICASE_PATHSPECS`) are removed.

No filter program runs either. `.gitattributes` can assign a working tree file to a filter that
git's configuration defines (Git LFS's, for example), and git would run its clean command to
compare that file. So every driver `git config --get-regexp '^filter\.'` lists gets
`-c filter.<name>.clean= -c filter.<name>.smudge= -c filter.<name>.process= -c
filter.<name>.required=false` on the diff, and the file is read as it is on disk. One touched
since git last looked at it (by a build or a checkout) is then compared with its committed text
unfiltered: an LFS file shows as a changed binary file. A driver whose name holds `=` cannot be
emptied that way, and `impact` stops with an error rather than run it.

## The session socket

While `scrutate serve` runs, index commands use its open index through a socket file on Unix,
`<root>/.scrutate/session.sock`, or a named pipe on Windows ([CLI](CLI.md#serve-mcp) lists the
commands and settings). This local boundary is reviewed here; the Windows choices are in
[ADR 0006](adr/0006-the-session-on-windows.md).

What it is. A Unix-domain stream socket, mode 0600, in the index directory, which only its owner
can enter (mode 0700, see [Controls and evidence](#controls-and-evidence)), made when `serve`
starts and removed when it exits; no TCP or UDP port. The command line never starts a server,
and the listening thread ends with `serve`.

On Windows: a named pipe, `\\.\pipe\scrutate-session-<hash>`, the hash a BLAKE2b-128 of the
user's SID and the canonical root. It is created with a protected DACL of one entry, full access
for that SID, a mandatory label at the server's own integrity level that no lower process may
write through (`D:P(A;;GA;;;<sid>)S:(ML;;NW;;;S-1-16-<level>)`), and `PIPE_REJECT_REMOTE_CLIENTS`; the first instance with
`FILE_FLAG_FIRST_PIPE_INSTANCE`, so a name already held (a second `serve`, or another account
that made it first) makes the server offer nothing. The name goes away with the server's last
handle: nothing is left behind to clean up.

Checks, at both ends:

- The server asks the system who the peer is (`getpeereid` on macOS and the BSDs, `SO_PEERCRED`
  on Linux) and closes a connection from another user before it reads or writes a byte. On
  Windows the client writes one byte first, and the server reads the connection's own token
  (`ImpersonateNamedPipeClient` at the identification level the client allows, then
  `RevertToSelf`) and closes a connection from another user, from below its own integrity level,
  or from a restricted or AppContainer token before it writes a byte; a process that opened the
  pipe for reading alone cannot send that byte and gets nothing.
- The command line connects only to a socket, in a `.scrutate` directory, both owned by its own
  user and neither a symbolic link (`lstat`, no link followed), then drops a peer of another user.
  On Windows it opens the pipe for identification only (`SECURITY_SQOS_PRESENT |
  SECURITY_IDENTIFICATION`: the server can learn who calls, never act as the caller) and drops a
  server whose process token is another user's, so a pipe of that name made by another account
  is passed over, or is at a lower integrity level than its own, or restricted, or an AppContainer
  one, so a sandboxed process of the same user that made the name first sees no request; and it
  drops a pipe whose label is below its own level (read with `GetSecurityInfo`), since a process
  id can name another process by the time it is read, and a pipe's label stays as its maker set it.
  A server whose process id or token cannot be read is dropped too.
- The server greets with its identity: the executable file (device, inode, size, modification
  and status-change times; on Windows its path, size and times), the version, the index schema and
  content revisions, the model signatures and the `SCRUTATE_RANKER` file's id taken the same way,
  every `SCRUTATE_*` setting that can change an answer, and its root. The command line uses it only
  when all match its own.
- A request is one line of at most 1 MiB, read within 2 seconds of the greeting (a caller that
  sends a byte now and then is closed then too); on Windows the server first waits up to 2 seconds
  for the caller's first byte, so a request may take up to about 4 seconds there. The command line
  gives a session 2 seconds to take it, and each write of the server's answer waits at most 2
  seconds for the caller to take it (on Windows each step of at most 64 KB), so a caller that
  reads a long answer slowly keeps its connection longer; at most 16 connections at once, a
  further one closed. A greeting slower than 20 ms (`SCRUTATE_SESSION_WAIT_MS`), an answer slower
  than 60 s, a closed connection or any error makes the command line do the work itself.

What a process of the same user gains: nothing. A request is an operation the command line could
run itself (`ops::Op`, ranked search included, with how to print it: `--json`, the byte limit and
offset, `--why`), run through the same `Repo` path and path confinement, and the answer is what
`scrutate` would print in that directory. The socket reads only the server's repository (and, for a `--deps` request, the package stores)
and writes only its `.scrutate/`, that user's frecency file and, for `--deps`, that user's package
cache, which the user can already write. It
can keep the server busy, as it could any process of its own; the limits above bound one
connection.

What another user can do: nothing. The file mode stops them where the system checks a socket's
mode (Linux), and everywhere the server checks the peer before it reads a byte. A socket another
user plants at the path (only if they can write the repository's `.scrutate/`) fails the command
line's owner and peer checks. On Windows the pipe's DACL refuses their open, and a pipe they make
first under the name fails the command line's server check.

A hostile repository cannot point the socket elsewhere. The path is fixed, and a `.scrutate` that
is or holds a symbolic link is refused for the socket too (`files::index_dir` refuses the index,
the server will not bind there, the command line will not connect through a link). git stores no
sockets; a regular file planted at the path is not removed, and neither side uses it.

Edge cases:

- A path too long for a socket address (103 bytes on macOS, 107 on Linux): the process enters the
  socket's directory before it starts any thread and goes back through a descriptor of the
  directory it left (`fchdir`), even if that was renamed. If that fails, the command line stops
  with an error and a server offers no socket.
- Two servers on one root: the first keeps the socket. A server removes it on exit only if it is
  still the one it made (device and inode). On Windows the second cannot make the pipe's first
  instance and offers nothing.
- A killed server's stale socket: a connect is refused at once, and the next `serve` replaces it.
- A moved root no longer matches the greeting; a deleted root takes the socket with it.
- A session still making its first index, with no file in it yet, is not used, nor is one for a
  call whose root is not the tree the session now indexes (its root moved to another). A call
  that meets a write of the session's own process waits for it up to 250 ms, and one that meets
  another process's write lock goes on with the index as it is; either answer says what is still
  pending.
  With every reader busy, a call waits for one, within the command line's 60 seconds.
- `SCRUTATE_NO_SESSION=1` turns the use off and `SCRUTATE_SESSION_DEBUG=1` says on stderr why a
  call did not use a session.

In the last four cases the command line does its own work.

Evidence: `tests/session.rs` (the same bytes and exit codes as a fresh process for every command,
and for ranked search and find with each option; an edit seen at once; another build, another
setting, the switch, a stale socket, a file that is no socket, two servers, a moved root, a deep
root, also reached from another directory with relative arguments; the socket's mode; on Windows
the same answers, the switch, another build and setting, two servers, the pipe's DACL read
back, and an open by a local account the test adds, refused) and unit tests in `session.rs` of the return to a renamed working
directory, the SID's text and the pipe name, and on Windows of a session and a caller at the same
integrity level, the level the pipe carries, a low-integrity reader that gets no greeting, a
low-integrity process of the same user that cannot answer as the session, a call in the old tree
once the root moves, and a request a slow session takes whole or never reads. A peer of another user on Unix needs a second account
to fake, so it is checked by code review only.

## The main checkout's index in a linked work tree

In a linked git work tree (`git worktree add`), a new index starts as a copy of the main
checkout's index, and an update looks in the main checkout's artifact store for content it has not
seen before it parses the file. Both read files outside the root.

How the main checkout is found: from git's own files, with no program run. The root's `.git` must
be a regular file of at most 4 KiB whose first line is `gitdir: <dir>`, with `<dir>` equal to
`<common dir>/worktrees/<name>`; `<dir>/gitdir` must name this `.git` file back and
`<dir>/commondir` the common directory (compared after links are resolved), which must be the
`.git` directory, no link, of its parent: the main checkout. Anything else (a `.git` file that
merely points somewhere, a bare repository, a main checkout without an index) is not followed,
and the index is built as always. The main checkout's `.scrutate` is checked as this checkout's
own is (one that is or holds a symbolic link is refused), and its `index.db` must be a regular
file.

Read, not write. Nothing under the main checkout's `.scrutate/` is written. Its database is opened
read-only (SQLite marks its read lock in `index.db-shm`, as for every reader), copied with
`VACUUM INTO` into this checkout's `.scrutate/`, and the segments it lists are copied, not linked.
New artifacts are not written into the main checkout's store: that would save a parse in other
work trees, but break the rule that scrutate writes only its own `.scrutate/` and let one work
tree damage another's index.

What a hostile repository can make it read: a work tree's `.git` file is no file git checks out,
so it is not repository content. An attacker who can write it, and the `gitdir` file it names that
must name it back, makes scrutate read one more `.scrutate/` they wrote: the same power as
planting a `.scrutate/` in the repository, which the threat model covers (decoded as data, with
bounds checks, never run). An artifact whose counts do not fit its bytes, a segment whose arrays
do not validate, or a panic while reading the other index counts as a miss, and the file is
parsed.

What is verified and what is trusted: an artifact's key is a 128-bit BLAKE2b hash of the artifact
format, the content revision, the SHA-1 of the file's bytes, its file name, and for an `index`
file its directory's name, all from this process's own read. So an artifact is used only for bytes
that hash to its key, under a name that parses the same. That it holds what a parse of those bytes
gives is not verified: its store is trusted, as a checkout's own index is. A planted store can make
answers about the content it names wrong in the work tree that reads it, until
`scrutate index --force` (which parses every file and reads no store) or a rebuild after damage.
Whether an artifact exists tells the reader nothing new: the same user can read that index
already.

Edge cases:

- The main checkout deleted or moved: the link is not confirmed, and the index is built as usual.
  A seeded index needs nothing of the main checkout afterwards.
- On another file system the copy is a real copy; on APFS it is a clone.
- Being written at that moment: the reads see the last commit, never half of one (`VACUUM INTO`
  reads in one transaction, an update's lookups in one snapshot). The main checkout removes a
  segment it replaced right after its commit; if a listed segment is gone or half gone, the seed
  fails and leaves no database (every copied segment must open before the copy is taken), the
  index is built as usual, and an artifact whose vectors were in that segment gets vectors made
  from its word counts. The segments the seed copied stay, since another first open at the same
  moment may list them; an update removes a copy no database lists once it is 10 minutes old.
  The main checkout's writer is never blocked: a WAL reader only holds back the
  checkpoint until the read ends.
- Two plain clones of one repository (no git link between them) do not find each other, and a
  second checkout that is no git work tree builds its index as before.

Evidence: `tests/artifacts.rs` (a seeded work tree equals a fresh build, with and without a vector
model; links not confirmed both ways, a main index directory that is or holds a link, and a plain
copy not followed; the main checkout's uncommitted write not read; its build beside lookups in the
work tree; damage to the store found and rebuilt).

## A repository's language tables

A `[[language]]` table in a repository's `.scrutate.toml` adds a language to that repository's
index ([CLI](CLI.md#custom-languages)). It is data. It runs nothing: the `regex` crate compiles
its patterns (no backtracking, linear in the line) with `grep`'s size limits. It reads no file: no
key names a path, an unknown key is an error, and the file is read as other repository files are.
It cannot change another language: a built-in name or extension is refused. The worst a hostile
table does is give the repository's files of its extensions wrong symbols, or stop every command
there with its error.

Evidence: `tests/custom_lang.rs` (an `include` or `path` key refused with its line, a link to a
table outside the repository not read, a pattern that does not compile, a built-in name or
extension refused).

## Setup and the Grep hook

`scrutate setup <agent>` writes only the files [docs/CLI.md](CLI.md#setup-and-the-grep-hook) names
for that agent and scope (under the home directory, `$CODEX_HOME` or `$XDG_CONFIG_HOME`, or the
repository's top with `--project`), and only when it is run; no other command touches them. Each
file gets one entry named `scrutate`, which runs this binary by its absolute path with `serve`
(with `--project`, `--root <repository> serve`) and nothing else. Only with `--hook` do Claude
Code's settings get two `PreToolUse` groups (Grep and Bash) and Codex's `hooks.json` one (Bash), and only with `--instructions` does the agent's
instruction file get one marked block. A file that does not parse is left as it is, and a crash leaves the old file or the new one, never half of one. For Claude Code's
user file, `setup` runs `claude` from `PATH` (`claude mcp add-json`, `claude mcp remove`), because
running Claude Code sessions rewrite that file themselves. An entry in a project file (`.mcp.json`,
`.cursor/mcp.json`) is a command line the agent runs for anyone who opens the repository and
accepts it; Claude Code asks before it starts one.

`scrutate hook grep` answers each Claude Code Grep call it reads on standard input. It reads files
as Engine A does (ignore rules followed, no link followed, nothing executed), and only below the
project directory (`CLAUDE_PROJECT_DIR`, else the session's directory); a search elsewhere is one
Claude Code would ask about, so the hook lets Grep run. It cannot apply the `Read` and `Grep`
rules Claude Code filters Grep's results by, so it lets Grep run when any settings file it reads
has a `deny` or `ask` rule for either or does not parse, and whenever managed policy it does not
read exists (a macOS `com.anthropic.claudecode.plist`, the Windows key
`SOFTWARE\Policies\ClaudeCode`): an administrator's read rule is never bypassed. It also fails open
on any argument it does not map, an error, a binary or unreadable file and after 3 seconds
([CLI](CLI.md#setup-and-the-grep-hook) lists every case): no output, exit 0, Grep runs. It never
exits 2, which would block the call. Its one file is the counter
`$XDG_STATE_HOME/scrutate/hook-grep.jsonl`: per answered call the session id, two byte counts and
the time, and no file name or text of the repository.

`scrutate hook bash` rewrites a shell command only by putting `scrutate hook run --` in front of
its first command, when that command is a read-only search by `rg`, `grep` or `find`; the rest of
the line stays as written. `scrutate hook run` is the boundary, as an agent can type it and a user
may allow it without a prompt: it runs only a bare `rg`, `grep` or `find` looked up in the absolute
directories of `PATH` (or, for an `rg` whose arguments `scrutate grep` reads the same way, this
binary's own `grep`), and its arguments, as the shell expanded them, must be on an allow-list:
each program has a table of the flags that only choose what is read and how it is printed, with
whether each takes a value, and the arguments are read as the program reads them (a flag's value
is skipped whatever it looks like, so `rg -g -- --pre=./p` gives `-g` the value `--`, and
`--pre` is a flag). Any other flag, a known one or a future release's, is refused (exit 2, nothing
run; the hook leaves such a line alone). On top of that, a word that names a flag that runs a
program or writes a file (`rg --pre`, `--pre-glob`, `-z` (`--search-zip`) or a cluster with `z`, `--hostname-bin`,
the text index's `--index` and `--update`) is refused wherever it sits, as a value or after `--`
too, and `find` takes only its read-only expressions (never `-exec`, `-execdir`, `-ok`, `-okdir`,
`-delete`, `-fprint`, `-fprint0`, `-fprintf` or `-fls`), so a file named `--pre=x` that a glob
expands into is refused too. A command with a redirection to anything but the null
device (`2>/dev/null`, `2>&1`, `>/dev/null`, `&>/dev/null`) is never rewritten: `2> ~/.bashrc`
would truncate a file. Codex takes a rewrite only with `allow`, which approves the whole
line, so for Codex the hook rewrites only a search with at most `| head` or `| tail` after it;
Claude Code gets no decision and checks each command of the line by its own rules: on that
path, what follows the first command (`;`, `&&`, `| tee`) is approved or not by Claude Code's own
permission parser under the user's rules, the same as without the hook. A line with `\r` or
another control character but tab and line feed is never rewritten (bash reads them as part of a
word). `hook run` takes no program from a `PATH` directory inside the current directory or its git
work tree, and runs the program with `LD_PRELOAD`, `LD_LIBRARY_PATH`, `LD_AUDIT` and every
`DYLD_*` variable removed.

## The structural rewrite

`scrutate tsquery LANG QUERY [PATHS] --rewrite TEMPLATE` prints a diff and writes nothing. With
`--write` it changes only files under the given paths whose canonical path lies inside `--root`
([CLI](CLI.md#tsquery---rewrite) lists every file it leaves alone, a link among them). Each is
written to a new temporary file in its own directory (`create_new`, so nothing planted there is
opened) and renamed over the original only if the original's bytes are unchanged, so an edit made
meanwhile is never lost. The root is opened once when the plan starts, and every read and write
goes through that handle and the directories opened from it one name at a time, a name never
followed: with `O_NOFOLLOW` on Linux and macOS (`openat`, `renameat`), and on Windows with
`FILE_OPEN_REPARSE_POINT` (`NtCreateFile` and a `FileRenameInformation` rename relative to the
directory handle), where a directory that is a junction or link is refused. The temporary file,
the check and the rename are relative to the file's directory handle. The temporary file is made
new, never opened when its name is taken: a file already of that name is left as it is, and the
write of that file is refused with its name. A directory swapped for a
link or junction after the plan starts, the root and its ancestors included, therefore cannot send
a read, the write or a temporary file outside the root. The template is text: nothing in it is run. No MCP tool rewrites; the one MCP tool that writes a source file is `edit_symbol`, offered only by a server started with `serve --allow-edits` ([Verified symbol edits](#verified-symbol-edits)).
Tests: `tests/rewrite.rs` (a link out of the root, an edit between the read and the write, a
directory swapped for a link after the read, the root swapped for one after the plan, the diff
mode writing nothing, and on Windows the same with junctions) and `rewrite::tests` on Windows (each
step meeting a junction as itself).

## The review comment

`scrutate review` reads the repository through git (above), writes only its index and never
posts. The GitHub Action posts its Markdown when `comment: true` is set, with its `github-token`
input (`github.token` by default), which needs only `pull-requests: write`. How it finds its own
comment, checks its inputs and never fails the job is in
[GitHub Actions](../integrations/github/README.md). Details that page leaves out: a
person's token is matched by the login `gh api user` gives (GITHUB_TOKEN cannot ask, so then only
the bot's comment counts), so another person's comment that quotes the marker
`<!-- scrutate-review -->` is left alone; the inputs reach `comment.sh` through the environment and are checked before anything runs, the commit
id as 40 or 64 hexadecimal digits, and used only in double quotes; the body goes to `gh api` as a
file (`-F body=@FILE`), never on a command line. Tests: `tests/review.rs`
(`comment_script`: a value that would run a command is refused with nothing run, a failed call is
a warning with exit 0, the comment is posted once and then edited, a person's comment with the
marker is not).

## Secrets in answers

What an agent reads is secret-safe by default: the MCP tools, the Grep hook and `lsp` replace each
secret they find with `[redacted:KIND]`, keeping the key name, URL scheme or PEM armour around it.
A text page ends with `(N secrets redacted: SCRUTATE_REDACT=off shows them)`, N counting the
redactions that page shows; the structured form carries `"redacted": {KIND: count}` for what it
holds, which may be more items than the page. `SCRUTATE_REDACT=off` turns it off and `on` turns it
on for the CLI's index answers (`--json` redacted as values, the closing line on stderr); any other
value keeps the default and says so on stderr. No tool argument turns it off, so an agent cannot.

Where it runs (`secrets.rs`): an answer's items are redacted before the budget pages them, so no
page edge cuts a secret in two, in the detailed and the concise form (only the items a page can
show); then the page as assembled is read once more, so a form written later cannot skip it. A
rule that reads a line as a whole reads the file's text, after the line number or path an answer
writes before it (`    7  `, `a.py:3:`, `a.py-4-`). What a page cannot see is redacted in the
answer first: a structural search capture as its whole source line reads it, so a value alone
(`"key-..."`) is found by the name its line gives it; a line of a private key's body as its whole
file reads it, at any size and in UTF-16 too, wherever `read_code` (from the middle of a file),
`grep_code`, `references`, the Grep hook or `hook run` (by the file and line grep names) show it,
armour on the page or not, reading only the files of what the page can show and each only as far
as its last line shown. Every line of a private key's body in a file the text index holds is also
recorded there by its hash, so whatever shows that line has it redacted by its text alone, with or
without its file, its line number or its armour (`grep -r` and `rg` through `hook run`, a file
name with a colon); and a line too long for `references` to show whole before the window
around a use is cut from it, so the window never shows a value without its name. The Grep hook
writes its lines through `hook::redacted`; `hook run` redacts what the search program prints, line
by line as it streams (with colour off, and refusing an `rg` with `--replace` or `--json`, which
rewrite a line before the redactor can see a secret in it whole), and leaves `.scrutate/` out of a `grep` or `rg` it runs: the index and the
`tsquery` cache hold the repository's text in binary records, where a secret runs straight into
the next field's bytes (`...ScEknc4\0\0`) and no detector can see where it ends. `hook run` decides before the search runs, by file identity (`files::IndexFiles`), so the
shape of the output (colour, headings, separators, JSON, odd names) cannot matter: it walks what
the search could read, hidden and ignored entries taken and links followed where the search
follows them, and refuses the search, running nothing, when a path it names lies inside an index directory or the walk reaches
one of the index directories' files or directories by device and inode (on Windows by the file
handle's volume and index: a link, a hard link
under another name, a nested repository's index), or when the walk is too large to check
(over 1,000,000 entries or 5 seconds); `read_code`, `grep_code`, the Grep hook and `edit_symbol`
(its dry run too, which reads the definition as `read_code` does) refuse such a path the same way. A path that is an index file under another name (a hard link,
either way round) is refused with a line that names the index file and says to remove the hard
link, by its path, to search it again. The searches scrutate walks itself (`scrutate grep`, the
index's walk and so `search`, `refs` and the MCP tools, the Grep hook) leave such a file out: on
Unix the inode comes with each directory entry, so a file that is one of the index files by
device and inode is in no answer, whichever of its names was there first. Only a file with a
second link (or one reached through a symbolic link) is looked up, and a match counts only while
the index file's own path still names that device and inode, so a new file that the file system
gave the inode number of an index file deleted since is searched as any other. The walk says so once
on stderr, and `scrutate check` lists each one with the index file it is. On Windows the walks
would have to open every entry to know it, so they do not. There, what keeps an index file's
text out of their answers is that every file scrutate writes under `.scrutate/` that can hold a
file's text has a NUL byte in its first kilobyte, so the walks see a binary file and skip it (a
test holds every file there to that), and a repository file hard-linked into `.scrutate/` is
still found by the walks while being refused by name. A file a watch or a session takes in by
its name, with no walk (`watch`, `serve --watch`), is held to the rule on every platform: one
that is an index file under another name is dropped from the indexes as gone. That costs nothing
for a file with one link on Unix, and on Windows a handle per index file once per change set that
names a file, since only the named files are asked about. The walks, and `hook run`'s walk before a search, know the index directories of
the searched paths and of the directories above where each path really is (a path through a
symbolic link, with `..` or a trailing slash, is climbed from its target), not that of a nested
repository inside the tree, where the binary header is what keeps its text out. Not covered: a link or a hard link made while the search runs
(the walk is done before it starts); a hard link to an index file of a repository outside the
searched tree; a search that prints only part of a secret (`-o` with a pattern that matches
inside it), since the redactor sees a line as printed and a piece of a token gives it nothing to
recognise; and, not the hook's to stop, a command it does not answer (`cat`, `strings` of those files), and the file names `find`
prints. A path that holds a secret (a file named with a token) is redacted like any
other text, so that path cannot be opened from the answer.

The detectors, ports of gitleaks v8.30.1 and secretlint v13.1.0 rules where the table names one,
each regex run only on text holding one of its keywords (an Aho-Corasick pass), with gitleaks'
entropy thresholds; JSON's `\/` is read as `/`:

| Kind | Rule it comes from |
|---|---|
| `aws-access-key-id` | gitleaks aws-access-token |
| `aws-secret-access-key` | secretlint-rule-aws |
| `github-token` | gitleaks github-pat, github-oauth, github-app-token, github-refresh-token |
| `github-fine-grained-token` | gitleaks github-fine-grained-pat |
| `gitlab-token` | gitleaks gitlab-pat, gitlab-pat-routable |
| `slack-token` | gitleaks slack-bot-token, slack-user-token, slack-app-token, slack-config-*, slack-legacy-* |
| `slack-webhook` | gitleaks slack-webhook-url |
| `stripe-key` | gitleaks stripe-access-token (secretlint-rule-stripe's length) |
| `google-api-key` | gitleaks gcp-api-key |
| `openai-api-key` | gitleaks openai-api-key |
| `anthropic-api-key` | gitleaks anthropic-api-key, anthropic-admin-api-key |
| `npm-token` | gitleaks npm-access-token |
| `pypi-token` | gitleaks pypi-upload-token (any index's prefix) |
| `jwt` | gitleaks jwt |
| `twilio-api-key` | gitleaks twilio-api-key |
| `sendgrid-api-key` | gitleaks sendgrid-api-token (SendGrid's 22.43 layout) |
| `mailgun-api-key` | gitleaks mailgun-private-api-token |
| `huggingface-token` | gitleaks huggingface-access-token, huggingface-organization-api-token |
| `shopify-token` | gitleaks shopify-access-token, shopify-custom-access-token, shopify-private-app-access-token, shopify-shared-secret |
| `digitalocean-token` | gitleaks digitalocean-pat, digitalocean-access-token, digitalocean-refresh-token |
| `vault-token` | gitleaks vault-service-token (the `hvs.` form), vault-batch-token |
| `telegram-bot-token` | gitleaks telegram-bot-api-token |
| `database-password` | secretlint-rule-database-connection-string |
| `url-token` | scrutate |
| `url-password` | scrutate |
| `bearer-token` | scrutate |
| `curl-user` | scrutate |
| `basic-auth` | scrutate |
| `env-secret` | scrutate |
| `password` | scrutate |
| `xml-secret` | scrutate |
| `key-material` | scrutate |
| `generic-secret` | gitleaks generic-api-key (without its stopwords) |
| `private-key` | gitleaks private-key, secretlint-rule-privatekey (line by line between the armour) |

Where a rule differs from its gitleaks v8.30.1 source, it marks more: `digitalocean-token` reads
all three forms without regard to case (gitleaks only the refresh token), and `vault-token` takes
a batch token above entropy 3.5 (gitleaks 4).

`url-token` takes a URL's user part of 20 or more characters shaped like a token, as a clone URL
carries a token (`https://<token>@github.com/...`); `url-password` takes the password of any other
URL that carries one (`https://user:pass@host`, also with no user; a password holding `@` runs to
the last `@` before the host, as does `database-password`'s), with a digit or a symbol in it
or eight letters or more, and no variable (`$PASS`, `${...}`) or placeholder (a placeholder
word, the user name again as in `anonymous:anonymous`, or the word password in letters and `_` alone as in
`username:mypassword`, where `MyPassword2024` is a real one); `bearer-token` an
opaque token after `Bearer` or an `Authorization: Token` scheme; `curl-user` the password that
`curl -u` or `--user` gives after the user (also joined to `-u`, and a proxy's through `-U` or
`--proxy-user`), on curl's own command (up to a `|`, `;` or `&`), with
url-password's test of the value, so a variable and a doc's example (`admin:admin`,
`elastic:changeme`) stay. A token here mixes upper
case, lower case and digits, or runs letters and digits through each other (one case with digits, a
change between them three times or more), or is a UUID or 32 or more hex digits, so a word with a
number after it (`accesstoken12345678`) is left alone. `basic-auth` takes the base64 after `Basic` when it decodes to a printable
`user:password` whose password is no doc's example word (`password`, `pass`, `secret`,
`changeme`). `env-secret`
takes `NAME=value` lines whose upper-case name holds `SECRET`, `PASSWORD`, `TOKEN`,
`KEY` or `CREDENTIALS`, or is `PASS` or ends in `_PASS` (`DB_PASS`, not `BYPASS`); `password` a quoted value of a name holding `password`, `secret` or
`token`, any symbol in it; `xml-secret` the text of an XML element whose name ends in
`password`, `passwd`, `pwd`, `secret`, `secretKey`, `token` or `apiKey` (Maven's `<password>`,
`<ns:accessToken>`), up to a quote or its closing tag, and all of it to a closing tag on its
line, quotes and spaces included, unless it reads as a sentence (a space, no digit, no symbol
but punctuation: `Forgot your password?`) or an escaped `&lt;...&gt;`; and the `value` of a .NET `<add key="..." value="...">` whose key ends so,
with `password`'s test of the value, so an empty element and a placeholder (`${env.X}`) stay, as
do the closed deploy-time forms of names alone (`#{db.password}`, `@@DB_PASSWORD@@`,
`[ClientSecret]`, and Razor's `@Model.`, `@ViewBag.`, `@ViewData.`, `@Html.` or `@Configuration.`
before a name);
`key-material` a line of base64 alone as wide as a key's body and near
random, a key read without its armour (lockfile checksums such as go.sum's `h1:` aside, and public
material: an SSH key after its type, as `known_hosts` writes it, and the body of a public key, a
certificate or a certificate request between its armour, read as a private key's is: armour
that ends its line opens it, and the first line that is not base64 ends it. Such a body is left
as written only when an END line of the same label comes later in the same answer and the body
is then exactly the one DER element its first bytes give the length of; `hook run` holds at most
64 lines (8 KiB) of it for that END. A body under a public key label is shown only when it is a
DER public key; any other body is marked: one that is no DER element, one longer than that hold,
one that runs past its element's length, or one shown without its own END line, since base64
after it may be another file's private key. An OpenPGP public key block and an RFC 4716 SSH2
public key are marked as any key material, since nothing here proves their bodies public);
`generic-secret` gitleaks' assignment rule without its stopword list, which drops real secrets
holding a common word; after a name holding `password`, `passwd`, `secret` or `token` it also takes
an unquoted value with other symbols (a YAML `password:` or a `.properties`
`spring.datasource.password=` whose value holds `@` or `!`), up to a space, quote, `;`, `&`, `:`, `,`, a bracket or the
line's end, and no URL. `aws-secret-access-key`, `password` and `generic-secret` also take a typed
assignment with a type of up to 80 characters (`const KEY: &'static str = "..."`, `apiToken?:
string | undefined = "..."`, `x!: string = "..."`, `: HashMap<String, Vec<u8>> = "..."`), a value
in up to two constructors (`Some(String::from("..."))`, `Optional.of("...")`) and a Rust raw
string (`r"..."`, `r#"..."#`). The rules that end a secret at what follows it take a quote, a
space, `;`, an escaped line end, the line's end, or `&` before a URL's next query parameter
(`?api_key=...&format=json`, `jdbc:...&password=...&useSSL=false`). An unquoted value goes on
past a `&` or `;` (`client_secret = ...&Rr`) unless what follows starts another item: a
`name=`, spaces in the name allowed (`;Initial Catalog=`), also behind `&amp;` or `&#38;`; a
second `&`; a cookie's `HttpOnly`, `Secure` or `Partitioned`; or a word and a space (`;echo
done`), which leaves a tail shaped so shown. A private key's
body is redacted line by line between its armour, which is matched in any case (`Rsa Private
Key`); armour that does not end its line (`const HDR = "-----BEGIN ...-----";`) starts no key, and
a line that is not base64 ends one. Placeholders (`${VAR}`, `{{ x }}`, `<value>`), numbers as code
writes them and identifiers in one case (`NL80211_AUTHTYPE_SAE`) are left alone, and after a
password, secret or token name so are a chain of names through `?.` or `!.`
(`this.form?.value?.password`), Razor's `@Model.Password` and a `~/` path (`~/.aws/credentials`):
a value with a digit or another symbol, or words joined by dots alone (a passphrase), is still read
as a secret; hex values are kept as secrets.

`tools/secret_redactions.py ROOT` gives the numbers. On Linux at c52356211790 (43,843,823 lines)
it redacts 199 strings (`generic-secret` 128, `key-material` 32, `password` 1, `url-password` 1,
`private-key` 37): the private keys are the test keys the kernel ships, and the other 162, 0.037
per 10,000 lines, were read one by one and none is a secret; in this repository's tracked files,
none (a test holds it there). A rule's regex is built the first time a text holds one of its
keywords, so a process pays for the rules its answers need. The page measured is the first 24,000
bytes of the files directly under `crates/scrutate/src/`, in name order, which needs 5 of the 34
rules. On a 16-vCPU AMD EPYC 9R14 server (three runs, load 6), a process takes 0.3 ms before any
text and 2.1 ms more for that page; a text that needs every rule pays what building them all costs,
10.3 ms. Redacting the page again then takes 0.45 ms (median of 101 runs). The source a page holds
changes which rules it needs; `cargo test --release -p scrutate --lib the_cost_of_redacting --
--ignored --nocapture` measures it again.

`tests/secrets.rs` plants three secrets of each kind (a `.env`, private keys with a last line
shorter than any rule for base64 alone, a typed Rust constant, a line too long to show whole and a
file named with a token among them) and asks every MCP tool that returns text, in both forms, at
pages of 24,000 and 700 bytes, a page from inside a key among them, and the Grep hook, its context
lines too; no byte of any secret comes back, each kind is marked, and with `SCRUTATE_REDACT=off`
every one does. The CLI's `--json` stays one JSON document with it on.

Not detected: token families outside the table (any
vendor's format scrutate has no rule for), and Vault's legacy `s.` service tokens, whose keyword
would send nearly every line of code to the regex.

Not covered, in assignments: a quoted value that reads as a name (a file path with a common
file extension, none of whose parts mixes both cases with digits, or capitalized words run
together with up to four digits after them, so a password such as `WinterIsComing2024` too); two on one line; `=` inside a type's generics; a type over 80
characters; a value built in a closure (`Lazy::new(|| ...)`) or a Kotlin `by lazy`; an f-string;
a wrapper other than `Some`, `Ok` and a type's `from`, `new` or `of` (`Cow::Borrowed`). Not
covered elsewhere: a private key's short last line in a `search_code` or definition answer that
starts inside the key (no armour on the page, and these answers are not read back from the file),
or in a file that holds a key's body without its armour; a key's short last line in a file the
text index does not hold yet (new since its last update, or one it skips) or when the text index
cannot be opened, shown by `hook run` with no line number or with a colon in its file name;
secrets in base64, hex or URL encoding (gitleaks decodes them; scrutate does not); formats outside
the table; a secret split over two lines of code or built at run time; a value alone with neither
a vendor prefix nor a name on its line (a name on the line before it); the CLI and the Python
bindings, which print what the files hold unless `SCRUTATE_REDACT=on` (the CLI's index answers
only); and the index itself: `.scrutate/` keeps chunk text, secrets included, and the hash of
every key body line (mode 0700 on Unix).

## Verified symbol edits

The MCP server writes no source file unless the user starts it with `serve --allow-edits`; no
environment variable or configuration file turns that on, so a repository cannot. The flag adds
one tool, `edit_symbol`, marked `readOnlyHint: false` and `destructiveHint: true`, so a client
that asks before a write asks for it. It writes one file per call, only one the index holds,
never under `.git/` or `.scrutate/` at any depth: by name under every spelling git's own rules
take for its directory (case, the code points HFS+ ignores, trailing dots and spaces, the short
names), which the index skips too, and by file identity with the root's own for each directory on
the way, so a link to either counts. A directory mounted into the tree from a `.git` (a bind
mount) is not recognised. Through the structural rewrite's writer: the root opened
once, each directory below it by name with no link followed, the file a regular file, its bytes
checked again just before a new file in the same directory is renamed over it, its permissions
kept. A dry run writes nothing in the repository, not even a temporary file; the read it makes is
counted, as every read is, in the frecency state file under the user's state directory. Only an MCP call reaches it: the
operation is never serialized, so the session socket that command line calls use cannot carry
one to a server, and `batch` refuses the tool by name (the Python bindings have no call that edits). A call writes only
with `apply: true` and the hash of the file as it is, so an agent that
read an older version is refused; only when the definition has its lines to itself (one that
shares its first or last line with other code that is not a comment is refused, a dry run too, since the edit replaces
whole lines); and only when the new file parses as well as the old one and
keeps every definition outside the edit ([ADR 0009](adr/0009-verified-symbol-edits.md)). Tests:
`tests/edits.rs` (the tool absent without the flag, a stale hash refused with the file kept as
another writer left it, an edit that breaks the parse or adds a definition refused, a file
reached through a link never written).

## Dependency source

`symbol --deps` and `read --deps` (the MCP `find_symbol` and `read_code` with `scope: "deps"`)
read outside the repository root; apart from them, only the main checkout's index in a linked work
tree ([above](#the-main-checkouts-index-in-a-linked-work-tree)), the settings the Grep hook reads and
the user's own configuration file (`$SCRUTATE_CONFIG`, or `config.toml` under
`$XDG_CONFIG_HOME/scrutate/` or `~/.config/scrutate/`) do. The read boundary then holds the
packages the root's lockfiles (`Cargo.lock`, `package-lock.json`, `uv.lock`, `poetry.lock`,
`go.mod`) name, in four stores (`$CARGO_HOME/registry/src`, the root's `node_modules`, `.venv`'s
`site-packages`, `$GOMODCACHE`), at the locked versions, and nothing more. A lockfile is
untrusted: a name or version that is not one of its ecosystem's (a `/`, a `..`, a character
outside letters, digits and `-._~+`) is refused by name, and what an answer or an error shows of a
lockfile has either passed that check or is escaped, so no control character of it reaches a
terminal. A store outside the repository (the cargo
registry, the Go module cache) is the user's: the directory built from a name is canonicalized and
refused when it resolves outside it. A store inside the repository (`node_modules`, `.venv`'s
`site-packages`) is read through no link: every directory from the root down to the package must
be one and none a link, so a `node_modules` or a `site-packages` that links out, or a package in
`node_modules` that does, is refused before a file is read. Of a Python distribution only the
files its `RECORD` lists are copied, none that is a link or lies under one. A path dependency is read from the repository's own index when it is
inside the root and refused when it is outside; one that names a drive, a share or a root (a `\`,
a `:`, a leading `/`) is refused by its text before any file is looked at, so Windows never
connects to a share a lockfile names. The package is copied to the user cache
(`deps/` under `SCRUTATE_CACHE_DIR` or the platform's cache directory; on Unix mode 0700, set
also on directories that were there before, and a cache root that belongs to another user or that
others may write to is refused, so no one else can swap a copy; on Windows under the user's own
`%LOCALAPPDATA%`, which only that user can read by default): a
linked file is copied only when it resolves inside the package, a linked directory is never
followed, and a copy over 20,000 files or 256 MiB is refused with nothing kept. Its answers are
redacted as the repository's are. A copy from a store the repository holds (`node_modules`,
`.venv`) is keyed by that repository too, so what one repository installed is never read for
another; only copies from the user's own stores (the cargo registry, the Go module cache) are
shared. A package edited in place after it was copied is answered as it was
([ADR 0005](adr/0005-dependency-source.md)).
Tests: `tests/packages.rs` (`../../x` and the like as a name and a version in each lockfile, a
link to a file outside the package and a package directory that is a link out of
`node_modules`, path dependencies inside and outside), and a planted secret in a dependency in
`tests/secrets.rs`.

## Known gaps and residual risks

- **Not independently audited** (no external review or penetration test). The fuzzing campaign
  (above) ran each target for ten minutes on one machine. Since then each target fuzzes for 45
  minutes every week in CI (`.github/workflows/fuzz-weekly.yml`), from the corpus the last run kept; that still
  finds shallow defects more often than deep ones.
- **`unsafe` inventory** (every site in the Rust crates, its count checked in CI):
  - `npy.rs`: memory-mapped array views (`Mmap::map` in `npy::map`, which the text index's
    segment files use too, read with bounds-checked slicing only; `from_raw_parts` after dtype,
    length and alignment checks; the `Pod` trait documents the invariants; a byte view of a `Pod`
    slice when saving);
  - `lex.rs`: one `matrixmultiply::sgemm` call with lengths derived from the slices it is given;
  - `text.rs`: `from_utf8_unchecked` on a run of ASCII word bytes (ASCII boundaries are UTF-8
    boundaries);
  - `source.rs`: `grep_searcher::MmapChoice::auto` for the searcher of the files named to a
    one-shot `scrutate grep` or the Grep hook (`Files::map_named`), as ripgrep takes it, so such a
    file is searched through a memory map where grep-searcher maps one (not on macOS);
  - `config.rs`: `std::env::set_var` during startup before any thread exists;
  - `cli.rs`: `signal(SIGINT, ...)` (Unix) and `SetConsoleCtrlHandler` (Windows) declarations,
    whose handler only swaps an atomic flag; on a second Ctrl-C the Unix handler restores the
    default action with `signal` and sends `raise(SIGINT)`, both async-signal-safe, and the
    Windows one passes the event on to the system's handler; `GetFileType` (Windows) of the
    process's own standard input handle, to tell whether `watch` was started with a pipe;
  - `hook.rs` (Windows): `RegOpenKeyExW` and `RegCloseKey` to see whether a policy key exists,
    with a nul-terminated name and the key closed;
  - `shellhook.rs` (`hook run`, which ends the program it runs when it dies itself): on Unix a
    `pipe` and `fcntl(FD_CLOEXEC)` on its two ends, so neither reaches the program; one
    `getrlimit(RLIMIT_NOFILE)` into the two 64-bit words of an `rlimit`, read before the fork; and
    a `Command::pre_exec` closure that calls only async-signal-safe functions (`getpid`, `fork`,
    `close` or the `close_range` system call on Linux, `read`, `getppid`, `kill`, `_exit`). Its
    `fork`, and the two steps of `pipe` then `fcntl` (not atomic), are safe because `hook run`
    has one thread there (a debug build asserts it from `/proc/self/task` on Linux). The forked
    watcher closes every descriptor but the pipe's read end and waits for end of file, which
    comes when `hook run` dies or drops the write end after the program finished; a read error
    ends the wait too. It then sends `SIGKILL` to the program only while the program is still its
    parent, so after a normal finish it kills nothing. The two descriptors it made are then taken
    into `File`s once each. On Windows: `CreateJobObjectW`, `SetInformationJobObject` with a
    zeroed `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` whose size it passes and
    `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and `AssignProcessToJobObject` with `hook run`'s own
    pseudo handle from `GetCurrentProcess` before it starts the program, so the program is in the
    job from its first instruction; the job's handle stays open until the process ends. When
    any of these fails (a job `hook run` is in already may refuse it), a second job takes the
    child's own handle once it started. Limit: in that case only, the child runs for a moment
    before it is in the job, and if `hook run` dies in that moment, or this assignment fails too,
    the program outlives it. A Windows test asks `IsProcessInJob` of a program it started suspended;
  - `index.rs` (Windows): the status change time of a file, from `GetFileInformationByName`
    (looked up once with `LoadLibraryW` of the API set `api-ms-win-core-file-l2-1-4` and
    `GetProcAddress`, then transmuted to the signature winbase.h declares, as CPython does) with a
    nul-terminated path, or `GetFileInformationByHandleEx` on a handle `std` opened for attributes
    only; and of a directory's entries, from `GetFileInformationByHandleEx` with
    `FileIdBothDirectoryInfo` into an 8-byte aligned buffer whose size it passes, each entry read
    from a byte copy with bounds-checked slicing; and on macOS one `fcntl(F_BARRIERFSYNC)` per
    file the index flushes (`sync_file`), on a descriptor the `File` it borrows keeps open, with
    no third argument;
  - `memory.rs` (Linux with glibc): one `malloc_trim(0)` after an answer of `serve`, `lsp` or
    `batch`, which takes no pointer and only gives free pages back;
  - `policy.rs`: one `sysctlbyname("hw.perflevel0.physicalcpu")` call on macOS, writing an `i32`
    whose size it passes;
  - `instant.rs`: on Linux one `statfs` per search root, and per directory named `proc` or `sys`
    below one, into a buffer larger than `struct statfs` on every Linux ABI, of which only the
    first field (`f_type`) is read;
  - `running.rs` (macOS only): `getuid`, the System V semaphore calls `semget`, `semctl` and
    `semop` on one semaphore, `IPC_STAT` into a buffer larger than `struct semid_ds` of which the
    first 16 bytes are read, `sigaction`, `pthread_sigmask` and `raise` for `SIGTSTP`, and file
    descriptor 1 borrowed for one `fstat` and never closed;
  - `foldfind.rs` (x86_64 only): one call to a function compiled for AVX2, made after the CPU
    reports AVX2;
  - `rewrite.rs` (Windows): `NtCreateFile` relative to a directory handle it holds, with one name
    in a `UNICODE_STRING` and an `OBJECT_ATTRIBUTES` that live for the call, the returned handle
    taken into an `OwnedHandle` once; and `NtSetInformationFile` with `FileRenameInformation` from
    a zeroed, 8-byte aligned buffer at least the size of `FILE_RENAME_INFORMATION`, the name copied
    after the `FileName` offset and the length passed;
  - `session.rs`: `geteuid`, and `getpeereid` (macOS, the BSDs) or `getsockopt(SO_PEERCRED)`
    (Linux) into a `struct ucred` whose size it passes and checks, on a connected socket it owns,
    and `fchdir` back to a directory descriptor it holds open; on Windows, the named pipe:
    `CreateNamedPipeW` with a security descriptor from
    `ConvertStringSecurityDescriptorToSecurityDescriptorW`, freed by `LocalFree` after the call,
    and `CreateFileW` of a nul-terminated name, each returned handle taken into an `OwnedHandle`
    once; `ConnectNamedPipe`, `ReadFile` and `WriteFile` overlapped, with an `OVERLAPPED` on the
    stack and an event the `Pipe` owns, always waited out with `GetOverlappedResult` (after
    `CancelIoEx` when the wait ran out) before the `OVERLAPPED` or the buffer goes away, the length
    passed being the buffer's; `CreateEventW`, `WaitForSingleObject`, `WaitNamedPipeW`,
    `GetLastError`, and `GetNamedPipeServerProcessId` into a `u32`; `ImpersonateNamedPipeClient`
    on a connected pipe after a byte was read from it, `OpenThreadToken(GetCurrentThread(),
    TOKEN_QUERY, open as self)` taken into an `OwnedHandle` once, and `RevertToSelf` before any
    other use of the thread (the process aborts when it fails); a unit test's `CreateFileW` of the
    pipe for reading alone, from a Low copy; `GetCurrentProcess`, `OpenProcess`, `OpenProcessToken` and `GetTokenInformation`
    (`TokenUser`, then `TokenIntegrityLevel`) into an 8-byte aligned 512-byte buffer whose size it
    passes, the SID read as `GetLengthSid` bytes from the pointer the `TOKEN_USER` holds inside
    that buffer and the level as the last of `GetSidSubAuthorityCount` sub-authorities of the
    label's SID (`GetSidSubAuthority`), each read before the buffer is reused;
    `GetTokenInformation(TokenIsAppContainer)` into a `u32` and `IsTokenRestricted` on the same
    token; `GetSecurityInfo(SE_KERNEL_OBJECT, LABEL_SECURITY_INFORMATION)` of the pipe into a SACL
    pointer and a security descriptor freed by `LocalFree`, each of the SACL's `AceCount` entries
    read through `GetAce`, the label's SID at the `SidStart` of a `SYSTEM_MANDATORY_LABEL_ACE`
    (type 0x11) and its last sub-authority read as above; a unit test makes a Low copy of its own process (`OpenProcessToken`, `DuplicateTokenEx`,
    `ConvertStringSidToSidW` of `S-1-16-4096` freed by `LocalFree` after `SetTokenInformation`
    takes it with the size of the label and the SID, `CreateProcessAsUserW` with a zeroed
    `STARTUPINFOW` whose size it sets, both returned handles taken into `OwnedHandle`s once, and
    `GetExitCodeProcess` into a `u32`); three unit tests' `ConnectNamedPipe` for a
    server that greets and then reads nothing or reads slowly, and for the low-integrity process
    that takes the pipe's name first, through the same `Pipe::io`;
  - `kq.rs` (macOS only): `kqueue`; `open` of a nul-terminated path with `O_EVTONLY |
    O_CLOEXEC`, each returned descriptor taken into an `OwnedFd` once; `kevent` with one change
    from a live `struct kevent` and no events asked back, or with no changes and a zeroed array of
    256 events whose length it passes and a zero timeout, reading only the first `n` it returns;
    `sysctlbyname("kern.maxfilesperproc")` into an `int` with its size, `getrlimit`/`setrlimit`
    of `RLIMIT_NOFILE` within the hard limit; and in its tests `fcntl(F_GETPATH)` into a
    `PATH_MAX` buffer. `kq::tests` write, replace, create and remove files and move directories
    under a registry and count the descriptors it holds;
  - `lazy_bind.rs` (the executable on macOS arm64 only): assembly stubs for 159 libSystem
    functions, each jumping to the address `dlsym` returned for its own name, with every argument
    register and the stack left as the caller set them; `fork` binds all of them first, so a child
    process never calls `dlsym`; `tests/lazy_bind.rs` runs variadic, floating-point, callback,
    panic and `fork` calls through them;
  - `watch.rs` (macOS only, `fsevents`): `dlopen` of CoreServices by its full system path and
    `dlsym` of twelve CoreFoundation, libdispatch and FSEvents functions (and the address of
    `kCFTypeArrayCallBacks`, passed to `CFArrayCreate` as is), each function transmuted to the
    signature the macOS SDK headers declare and used only when no lookup returned null; the
    stream is created, scheduled, started and torn down (stop, invalidate, release) in the order
    FSEvents.h gives, every returned pointer checked for null; the callback reads exactly the `n`
    paths and flags FSEvents passes, as nul-terminated strings that live for the call, and its
    `info` is a boxed channel sender that the stream's release callback frees once, or the
    caller frees when no stream was made. A session's cookie barrier (on macOS only where a
    test turns it on: reads there look at the tree) calls `FSEventStreamFlushSync` from
    its own thread through `Flush` (declared `Send`: the stream pointer is only passed to that
    function), which `watch::Notes` holds under a lock and drops before the stream is torn down.
    `watch::tests` starts a stream and receives a file's event, every `tests/watch_files.rs` test
    runs on it, and the session tests of the change set flush it;
  - `watch.rs` `local` (macOS and Windows): `statfs` (`statfs$INODE64` on Intel macOS) of a
    nul-terminated path into a buffer larger than `struct statfs` and 8-byte aligned as its
    64-bit fields, `f_flags` read as the four bytes at offset 64; `GetDriveTypeW` of a nul-terminated drive root. `watch::tests` asks both about
    the temporary directory;
  - the tests: `kill` (`tests/cli.rs`, `tests/running.rs`), `geteuid` (`tests/robustness.rs`),
    `clock_gettime` (`tests/parse_index.rs`) and `wait4` of a child the test spawned
    (`tests/intel_refs.rs`, its CPU time) with arguments they own, `std::env::set_var` in
    `tests/index_quota.rs`, the one test of its binary, so no other thread reads the environment, and in
    `tests/lazy_bind.rs` C library calls (`snprintf`, `powf`, `qsort` with its callback, `fchdir`,
    `fork`, `getpid`, `_exit`, `waitpid`) made through the stubs above.

  Edition 2024 writes `unsafe` on each `extern` block (`unsafe extern "C"`), on
  `#[unsafe(no_mangle)]`, and as a block around the body of an `unsafe fn`
  (`unsafe_op_in_unsafe_fn`): these mark the sites listed above and add none.

  `vendor/ignore` has none. `tools/unsafe_count.py` counts the `unsafe` keywords outside comments
  and strings in every file under `crates/` and `vendor/ignore`, and the security workflow fails
  when the counts differ from this table, so a new `unsafe` needs a line above:

  | File | `unsafe` |
  |---|---|
  | `crates/scrutate/src/cli.rs` | 8 |
  | `crates/scrutate/src/config.rs` | 1 |
  | `crates/scrutate/src/foldfind.rs` | 2 |
  | `crates/scrutate/src/hook.rs` | 3 |
  | `crates/scrutate/src/index.rs` | 7 |
  | `crates/scrutate/src/instant.rs` | 3 |
  | `crates/scrutate/src/kq.rs` | 11 |
  | `crates/scrutate/src/lazy_bind.rs` | 14 |
  | `crates/scrutate/src/lex.rs` | 1 |
  | `crates/scrutate/src/memory.rs` | 1 |
  | `crates/scrutate/src/npy.rs` | 14 |
  | `crates/scrutate/src/policy.rs` | 2 |
  | `crates/scrutate/src/rewrite.rs` | 2 |
  | `crates/scrutate/src/running.rs` | 13 |
  | `crates/scrutate/src/session.rs` | 59 |
  | `crates/scrutate/src/shellhook.rs` | 6 |
  | `crates/scrutate/src/source.rs` | 1 |
  | `crates/scrutate/src/text.rs` | 1 |
  | `crates/scrutate/src/watch.rs` | 28 |
  | `crates/scrutate/tests/cli.rs` | 2 |
  | `crates/scrutate/tests/index_quota.rs` | 1 |
  | `crates/scrutate/tests/intel_refs.rs` | 2 |
  | `crates/scrutate/tests/lazy_bind.rs` | 12 |
  | `crates/scrutate/tests/parse_index.rs` | 2 |
  | `crates/scrutate/tests/robustness.rs` | 2 |
  | `crates/scrutate/tests/running.rs` | 2 |

  tree-sitter grammars and SQLite are C code built from crates.io sources.
- **Where the system cannot map a file** (`Mmap::map` fails with `Unsupported`, as on wasi),
  `npy::map` reads the whole file into memory: an index's arrays and text index segments are
  held in memory there, and a change on disk after the read is not seen. Any
  other mapping error is an error.
- **Memory-mapped files can change underneath a reader** if another process rewrites a segment in
  place. scrutate never does (temp dir + rename; deletion only after the manifest no longer
  references them), so this needs an external actor; on Windows a mapped file cannot be deleted
  and GC retries later. A file the user names (a `SCRUTATE_SEMANTIC` model) is read whole, never
  mapped: replacing it in place under a running process would end that process with SIGBUS. A file
  named to a one-shot `scrutate grep` or the Grep hook is mapped (not on macOS), as ripgrep maps
  it: another process that shortens it while it is searched can end that process with `SIGBUS`,
  the risk ripgrep takes for the same speed. A process that stays up (`serve`, `lsp`, a session),
  where agents edit the files it searches, maps none of them: it reads a named file whole (up to
  64 MiB) instead. `tests/maps.rs` lists every call that maps a file with why its file may be
  mapped (built-in data, segment files scrutate writes once, or a file named to a one-shot grep),
  and a new one fails until it is listed.
- **Index privacy on Windows**: `.scrutate/` inherits the repository directory's permissions there,
  so a file whose own permissions are narrower than its directory's has its chunks readable by
  whoever can read the repository directory. On Unix the directory is the owner's alone (above).
  A checkout several users share on Unix has one index each user cannot open but its owner's:
  the others search with Engine A, or the owner widens it with `chmod`, which the next write
  undoes.
- **Denial of service by index size**: millions of files build a multi-gigabyte index unless
  `index.max_files` or `index.max_bytes` is set ([CLI](CLI.md#conventions)); by default there is
  no limit. The quota counts the files an index holds and their size, not the bytes `.scrutate/`
  takes on disk. Engine A has no such state.
- **Read rules the hook cannot see**: a `Read` or `Grep` rule given only on Claude Code's command
  line (`--disallowedTools`, `--settings`) or by an SDK host is in no file the hook reads, so the
  hook answers a Grep that rule would have filtered. Put such rules in a settings file, or leave
  the hook out (`scrutate setup claude --uninstall`, then `scrutate setup claude` without
  `--hook`).
- **No platform code signing**: the macOS binaries are not notarized (Apple's Developer Program
  is 99 USD a year) and the Windows ones carry no Authenticode signature. Gatekeeper prompts for
  a quarantined download, SmartScreen asks for "Run anyway", and Smart App Control blocks an
  unsigned executable; [Install](INSTALL.md#unsigned-binaries-on-macos-and-windows) has the ways
  around it. The Sigstore attestation and `SHA256SUMS` are the integrity check.
- **Timing side channels, multi-tenant hosting, encryption at rest**: out of scope for a local
  developer tool.

## Reporting

Report vulnerabilities privately to the maintainers (see README), not in public issues; include
the version (`scrutate --version`), platform and a reproduction.
