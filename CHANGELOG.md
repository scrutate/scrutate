# Changelog

All notable changes to Scrutate. The format follows Keep a Changelog; versions follow SemVer.
Versioned interfaces: the CLI, the JSONL `schema` number, the MCP tool names and the Python API.
Past 1.0, an incompatible change to one bumps the major version and is listed under "Breaking".
The on-disk index is a cache with its own `schema_version`: a new version rebuilds an
incompatible index by itself, and that is never breaking.

## [Unreleased]

### Added
- `dupes` and `duplicates` under a lowered `--threshold` say how much of what was asked they
  find: near pairs come from MinHash buckets, 16 bands of 4 rows, so a pair at similarity J is
  found with probability 1 - (1 - J^4)^16. Under the similarity at which that reaches 99%, a last
  line says so (`(near pairs are found by sampling: about 64% of pairs at 0.50 are found)`), with
  `sampling` in the JSON and the MCP result and `sampled` in the Python bindings; the default
  threshold has no such line ([CLI](docs/CLI.md)).
- On Windows an index command uses a running `serve`'s session too, through a named pipe
  (`\\.\pipe\scrutate-session-` and a hash of the user's SID and the root), with the bytes and exit
  code a fresh process prints, as through the socket on Unix. Only a local process of the same
  user, at the server's integrity level or above and not restricted or in an AppContainer, is
  greeted; the command line in turn takes no answer from a server process or pipe of lower trust.
  The first `serve` on a root holds the name, which goes with the process, or when the root's
  link moves to another tree, once the calls in flight end ([Security](docs/SECURITY.md),
  [ADR 0006](docs/adr/0006-the-session-on-windows.md)).
- `serve --allow-edits` lists `edit_symbol`, which replaces one whole definition: a dry run gives
  the diff and the file's hash, and `apply: true` writes only with that hash, while the file still
  has it, and only when the new file parses and the definitions outside the edit keep their names,
  kinds, text and order (a grown edit shifts their lines). The edit replaces the definition's lines
  whole, so one that shares its first or last line with other code that is not a comment is
  refused, a dry run too; a comment there, or a lone `;` after it (`struct P {...};`), is replaced
  with it. A
  file under `.git/` or `.scrutate/` at any
  depth, under any spelling a file system takes for them (`.GIT`, `git~1`), a link, and a file that
  is not UTF-8 text are never written. Without the flag the server stays read-only, and no
  environment variable or configuration file turns it on
  ([CLI](docs/CLI.md#serve---allow-edits-and-edit_symbol)).
- `symbol --deps PACKAGE.NAME` and `read --deps --symbol PACKAGE.NAME`, and the MCP `find_symbol`
  and `read_code` with `scope: "deps"`, answer from a dependency's source at the version the
  lockfile pins (`Cargo.lock`, `package-lock.json`, `uv.lock`, `poetry.lock`, `go.mod`), read
  where that version is installed. An installed package of another version is an error that names
  both and the command that installs the locked one; a git or URL source is an error, and a path
  dependency outside the repository too ([CLI](docs/CLI.md#--deps-a-dependency-at-its-locked-version)).
- `impact` and `review` list a renamed file's definitions whose callers still import it by the
  old path, as deleted at the old path with the rename note right above them, and those callers
  as calling a deleted definition, where a pure rename listed nothing though it broke the
  importer. In JSON a rename note has `renamed_from`, the old path ([CLI](docs/CLI.md)).
- `check` notes each entry of `.scrutate/seg` no segment list names, with its size and age (`unlisted`
  in `--json`), where it said OK and showed nothing; such an entry is a merge in progress or
  what a stopped writer left, and an update removes it once it is older than 10 minutes, a stray
  file too, where it kept one. It is
  no problem and does not change the exit code ([CLI](docs/CLI.md)).
- The language server answers every workspace folder from the index of its own repository, where
  it answered only the first folder's and looked up a name from another folder in that index. It
  takes `workspace/didChangeWorkspaceFolders`, so a folder added later is answered and a removed
  one closed, and a file outside every folder's repository gets no answer; `workspace/symbol`
  leaves out a folder that cannot answer and answers from the others ([CLI](docs/CLI.md#lsp)).
- `scrutate hook bash`, the `PreToolUse` hook of Claude Code's Bash and Codex's shell: when the
  first command of a line is a read-only search by `rg`, `grep` or `find`, it puts
  `scrutate hook run --` in front of it and keeps the rest of the line as written; `hook run`
  answers `rg` through `scrutate grep` with `rg`'s own output and runs `grep` and `find` as
  given, redacts secrets, and bounds the output to 24,000 bytes when no pipe takes it. It runs no
  program other than a bare `rg`, `grep` or `find` from `PATH`, and only the flags on each one's
  allow-list of flags that read and print, read as the program reads them; any other flag leaves
  the line alone. Codex, which takes a rewrite only with `allow`, gets one only for a search
  with at most `| head` or `| tail` after it. `setup claude --hook` adds it beside the Grep hook,
  `setup codex --hook` to `$CODEX_HOME/hooks.json`; `hook stats` counts its searches apart;
  `SCRUTATE_HOOK_BASH=off` turns it off ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- `hook run` refuses, before anything runs, a `grep` or `rg` that could read a file of
  scrutate's index, decided by file identity, with a line naming the path; a search too wide to
  check that way (over 1,000,000 entries or 5 seconds) is refused with a line asking for narrower
  paths ([Security](docs/SECURITY.md)).
- Searches through `hook run` run with colour off, and an `rg` with `--json` or `--replace`
  (`-r`) is refused, pointing at `scrutate grep --json`, which gives JSON with secrets redacted;
  so is an `rg` with `--stats`, whose statistics `scrutate grep` prints in another form.
- What `hook run` adds to an `rg` goes before the `--` that really ends its flags, read as `rg`
  reads them, so it always stays a flag.
- `read --symbol NAME --history [N]` and the MCP `read_code` tool's `history` list the last
  commits that changed each definition's lines, newest first, through renames and uncommitted
  edits, for the first 10 definitions shown; in a shallow clone it marks where the history is
  cut. It needs git 2.25 or later.
- `search`, `symbol` and `refs` keep the files a glob (`-g`) or a type (`-t`) names, the files
  parsed as a language (`--lang`) and the definitions of a kind (`--kind`), and the MCP tools
  `search_code`, `find_symbol` and `references` take the same as `globs`, `types`, `langs` and
  `kinds`. A filtered search returns `-k` results whenever that many pass among its candidates
  widened up to 64 times their usual number; past that it says it stopped and why. `symbol`
  filters inside the name lookup, so `--kind` finds a definition past the first 100,000 of its
  name; `refs` with kinds or `--def` resolves at most 10,000 occurrences and a filtered `symbol`
  reads at most 100,000 near matches, and an answer stopped there says why.
- `impact`, its `--json` and the MCP `impact` tool list the change's findings: the duplicates it
  adds of code the repository has and the changed definitions no test reaches, which only
  `review` named before. `review` renders the same list, and its Markdown and GitLab reports are
  unchanged.
- Secret-safe answers: the MCP tools, the Grep hook and `lsp` replace a secret in what they
  return with `[redacted:KIND]`, its name or armour kept, in both response forms and in a
  structural search capture of a value alone, and say how many they redacted; a key's body lines
  are recorded in the text index by their hash, so any answer that shows one has it redacted. The
  detectors are ports of gitleaks and secretlint rules and scrutate's own, one kind each: AWS
  access key ids and secret keys, GitHub tokens (classic and fine-grained), GitLab tokens, Slack
  tokens and webhooks, Stripe keys, Google API keys, OpenAI and Anthropic keys, npm and PyPI
  tokens, JWTs, Twilio, SendGrid and Mailgun keys, Hugging Face, Shopify, DigitalOcean, Vault and
  Telegram bot tokens, database URL passwords, a token as a URL's
  user, other URL passwords, `Bearer` and `Authorization: Token` tokens, `curl -u` passwords,
  HTTP Basic credentials, `.env` lines, quoted passwords, a key's base64 body read without its
  armour, a generic assignment rule, and private keys ([the table](docs/SECURITY.md#secrets-in-answers)).
  `SCRUTATE_REDACT=off`
  turns it off and `on` turns it on for every CLI answer that prints file text, in text and
  `--json`, leaving file names and counts as they are ([CLI](docs/CLI.md), [Security](docs/SECURITY.md#secrets-in-answers)).
- Vendored code ranks lower than the project's own in `search` and the MCP `search_code` by a
  fixed margin, so a clearly better vendored match (`WalkBuilder.build_matchers` defined only
  under `vendor/`) still comes first, and it is never left out: `vendor/`, `third_party/`,
  `node_modules/`, `Pods/`, `Carthage/`, minified files and their kin, a subset of GitHub
  linguist's `vendor.yml`. A name only vendored code defines, searched for as itself, keeps its
  place. A directory the root manifest builds as the
  project's own package (Cargo workspace members and path dependencies, a `go.mod` replace
  into the repository, npm, Yarn and pnpm workspaces) is not vendored, unless a workspace glob
  with `!` takes it out again (`'!vendor/x'`) and no other manifest owns it. Every JSON item whose
  `path` is vendored code, in any answer, has `"vendored": true`, and so does every text line
  whose location names such a path: `[function, vendored]` after a location with its kind,
  `(vendored)` at the end of any other line. A mark never shares a line with a file's text: a
  file's lines and the code an answer quotes (a hit's snippet, a definition's body) are never
  marked, so `read_code` shows a file as it is, and a line that quotes code after its location
  (`path:12:5: code`, as `refs` writes an occurrence) keeps that shape and is named instead in one
  `vendored: PATH, ...` line at the end of the answer
  ([CLI](docs/CLI.md#engine-b)).
- `symbol` lists near matches when no definition has the name: names that start with the query,
  names whose first words its parts start (`BkSvc` finds `BookingsService`), then names that hold
  it (three characters or more), each saying how it matched (`match` in JSON, MCP and Python).
  `--exact` (MCP `exact`, Python `exact=True`) keeps the old answer. The language server's
  workspace symbols rank the same way ([CLI](docs/CLI.md#engine-b)).
- `SCRUTATE_COUNT_KEY=N` keeps a count of big searches apart from the one the user's other
  searches join; the tests of that count use it, so another suite's searches cannot change what
  they see ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `scrutate review --base REV [--head REV]` reports a pull request: `impact` between the two
  commits and the duplicates the head adds, as a PR comment (`--format markdown`), JSON or a
  GitLab Code Quality report, which the GitLab template saves as the job's `codequality` report
  ([CLI](docs/CLI.md)).
- The GitHub Action's `comment: true` posts that report as one pull request comment and edits it
  on each push, with only the `pull-requests: write` permission and never failing the job;
  `args` is now optional.
- Every MCP tool but `edit_symbol` takes `response_format`: `detailed` (the default) or
  `concise`, which keeps every location and name and drops code, snippets and explanations: a
  concise answer over 1 KB is at most half of the detailed one where that carries code or
  snippets, and at most two thirds where it is already only places and names (similar code, an
  outline of long names) ([CLI](docs/CLI.md)). The
  Python `Repo.call` reads it too; `batch` checks it.
- `serve --short-tools` (or `SCRUTATE_MCP_SHORT_TOOLS=1`) sends a short tool list with the same
  tools, arguments and checks, in at most half the bytes of the full one (the `tools/list` result
  as compact JSON; [CLI](docs/CLI.md) gives the budgets the tests hold).
- `scrutate setup <claude|codex|opencode|cursor|gemini>` writes an MCP server entry that runs this
  binary into the agent's user file (or with `--project` the repository's), editing it in place;
  `--uninstall` restores it byte for byte, `--dry-run` writes nothing, `--instructions` adds a
  block to `CLAUDE.md`, `AGENTS.md` or `GEMINI.md`. Checked with Claude Code, Codex,
  cursor-agent, opencode and Gemini CLI ([CLI](docs/CLI.md)).
- `scrutate setup claude --hook` installs a `PreToolUse` hook that answers Claude Code's Grep with
  Engine A's matches, bounded as an MCP answer is, and `scrutate hook stats` shows the bytes it
  saved. A plain `--uninstall` removes the hook too. It works on Windows, with Grep's paths there
  (`src\a.rs`).
- `scrutate tsquery LANG QUERY [PATHS] --rewrite TEMPLATE` rewrites each match from a template of
  its captures and prints a unified diff; `--write` writes the files. No MCP tool
  ([CLI](docs/CLI.md)).
- A `[[language]]` table in `.scrutate.toml` or your configuration file adds a language without a
  build: extensions, comment marks and one regular expression per definition kind. Its files get
  symbols, outlines, documentation and ranked hits, and `grep --type` and `languages` know it; a
  bad table is one error with the file and line, exit 2 ([LANGUAGES](docs/LANGUAGES.md)).
- A configuration file that does not parse is one line naming the file and the line.
- docs/FAQ.md answers the first questions about scrutate, with links to the evidence;
  `CITATION.cff` gives GitHub's "Cite this repository".
- The website https://scrutate.github.io (repository `scrutate/scrutate.github.io`), with a
  playground that runs scrutate compiled to WebAssembly in the browser; the README's top links
  and `CITATION.cff`'s `url` point to it. The README opens with its tagline and badges, then a
  "Try it in your browser" image, where a hand presses the button that opens the playground, and
  its top links; the terminal demo image is gone, since the hero already shows real searches and
  the playground runs them live.
- `.github/FUNDING.yml` shows GitHub's Sponsor button (GitHub Sponsors and Buy Me a Coffee), and
  the README's "Sponsor and hire" section links both.
- A security workflow runs `cargo deny check` (`deny.toml`: advisories, the licence list,
  crates.io only), `zizmor --persona pedantic` on `.github`, and a check that each file's count
  of `unsafe` matches the inventory in docs/SECURITY.md, on every push to main, every pull request
  and once a week ([SECURITY](docs/SECURITY.md#controls-and-evidence)).
- Every fuzz target runs for 45 minutes once a week in CI, from the corpus the last run kept; a
  crash fails the run and is uploaded ([fuzz](fuzz/README.md)).
- The release workflow takes a `dry_run` input: a manual run with it runs every job, `ci.yml`
  included, attests every file, and makes, checks and deletes a draft release
  `rehearsal-<run id>`, so the tag path is tested before the first tag
  ([Releasing](docs/RELEASING.md#7-what-a-published-release-adds)).
- The release workflow attaches the VS Code extension (`.vsix`) to the release and attests it,
  and uploads the wheels to TestPyPI and PyPI and the extension to the VS Code Marketplace and
  Open VSX, each by trusted publishing or token and each only once the owner sets it up
  ([Releasing](docs/RELEASING.md#7-what-a-published-release-adds)).
- A release carries a `.deb` and an `.rpm` for x86_64 and aarch64 Linux, built from the tested
  release binary, listed in `SHA256SUMS` and attested ([Install](docs/INSTALL.md#from-a-release-archive)).
- `packaging/homebrew/tap/` and `packaging/scoop/bucket/` hold the files of the Homebrew tap and
  the Scoop bucket repositories, with their test, bottle, autobump and Excavator workflows;
  INSTALL.md shows `brew trust --tap scrutate/tap`, which Homebrew 7 needs
  ([packaging](packaging/README.md)).
- The `jetbrains` workflow runs the language server in a headless IntelliJ IDEA 2026.3 with
  LSP4IJ 0.21.0 from the Marketplace: it registers `scrutate lsp` with the values the Language
  Servers settings page saves and checks that Go to Declaration on the fixture repository opens
  `src/geo.py` at line 6. It runs when the JetBrains files change and by hand
  ([JetBrains](integrations/jetbrains/README.md#the-automated-run)).

- `[index] max_files` and `max_bytes` in the configuration file (`SCRUTATE_INDEX_MAX_FILES`,
  `SCRUTATE_INDEX_MAX_BYTES`) cap how many files, and bytes of them, each index holds; an update
  that would pass a cap stops with exit 2 and leaves the last index as it was. No cap by default
  ([CLI](docs/CLI.md)).
- `refs`, `callers`, `callees`, `deps`, `rdeps` and the language server resolve C and C++
  (`#include` to repository files, namespaces and `::`, members of the class around and its
  bases, a header's declaration as the definition it declares) and C# (namespaces, `using`,
  aliases, typed locals, members, partial classes) ([CLI](docs/CLI.md#refs)).
- Java fields and record components are symbols of a new kind, `field`, and enum and interface
  constants are constants: `refs` ties a field read, `this.f`, `obj.f`, `C.F`, a static import
  and a record accessor `p.x()` to them, where it listed `field or key`
  ([CLI](docs/CLI.md#refs)). An index made before reads every file again.
- `bench/agent_eval.py` runs Claude Code on 20 tasks over three pinned corpora with and without
  the MCP server, and records per run whether the answer checks out, tokens, cost, tool calls and
  tool errors. Each run is paid, so it starts only with `--yes-spend`; no round has run yet
  ([Quality](docs/QUALITY.md#agent-eval)).
- `bench/yardstick.py` measures each workload against its rivals: grep tools, zoekt and livegrep,
  universal-ctags, ast-grep, rust-analyzer, pyright, clangd and gopls, and on the QUALITY queries
  (`--quality N`) ck, grepai and claude-context with local embeddings, with each tool's MRR@10
  and hit@5. It checks every answer before it times one, reports a wrong answer instead of a
  time, and records each rival's version. A rival is timed against scrutate in the same form:
  the zoekt and livegrep servers against scrutate's MCP server, which answers indexed grep too. `bench/competitors/install_rivals.sh` installs every
  rival pinned by version and checksum; `yardstick.Dockerfile` and `claude-context.compose.yml`
  run them in containers. `--only` starts only the tools its workloads have, and a tool's start
  past `--start-timeout` (30 min) is that tool's error. CI runs `--smoke` on a small tree in
  Python, Rust, C and Go ([Releasing](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).
- On Linux the benchmark results record the machine they ran on: kernel, virtualization, CPU
  frequency governor and the disk under the corpora, and `bench/indexed.py` records the load
  before each tool's runs as `bench/leaderboard.py` does. In a container they name the container
  and take the host's facts from a file `bench/host_facts.py` writes on the host
  (`SCRUTATE_BENCH_HOST_FACTS`). LEADERBOARD.md heads a section "bare
  metal" when its results record no virtualization and lists those facts under it, and marks a
  virtual-machine run filed as superseded beside a bare-metal run of its platform. BENCHMARKS.md
  takes several results files, one section per machine
  ([Releasing](docs/RELEASING.md#3-generated-documents)).
- The Linux benchmark harness installs the latest release of each competitor: ast-grep 0.51.3,
  uv 0.13.9, Go 1.27.4 and Zoekt at `33ce1d04d373`, and git 2.56.1 built from source in place of
  Debian's, which `bench/compare_tools.py` now finds first on PATH on Linux as the leaderboard does.
  A run refuses a scrutate binary built from another commit than the harness it runs
  (`bench/competitors/built_from.sh`), and `linux_indexed.sh` takes its idle threshold from
  `INDEXED_MAX_LOAD`.
- `bench/yardstick.py` never times a query whose answer it cannot check, waits for an idle machine
  before each query and records its load, gives p99 beside p50 and p95, compares each rival with
  every scrutate form timed on the query (a resident server against `scrutate grep --index` too),
  and bootstraps its intervals at 10,000 resamples where it took 2,000. The W3 queries for the
  Linux tree are committed (`bench/yardstick_queries/w3-linux.json`), and
  `bench/make_yardstick_md.py` writes a marked section of docs/COMPETITIVE.md from the result
  files it names, which `tools/gen_doc_assets.py --check` checks
  ([Releasing](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).
- An OpenSSF Scorecard workflow (`.github/workflows/scorecard.yml`, ossf/scorecard-action 2.5.0
  pinned by commit) runs on every push to main and once a week, and publishes its score and its
  findings to code scanning ([Security](docs/SECURITY.md)).
- An LSP4IJ template for JetBrains IDEs (`integrations/jetbrains/lsp4ij-template`): *Import from
  custom template...* in the Language Servers settings fills the name, the command `scrutate lsp` and
  a file name pattern per language; the `jetbrains` workflow imports it with LSP4IJ 0.21.0 and
  runs Go to Declaration through it ([JetBrains](integrations/jetbrains/README.md)).
- README's Benchmarks section has a Compared against table, one row per tool measured (GNU grep,
  ripgrep, ack, ag, git grep, ugrep, ast-grep, Zoekt, livegrep): its version, the machines and
  benchmarks it ran on, scrutate's median as a share of its, and the verdicts ranked at 95%.
  `bench/make_compared_md.py` writes it from the result files docs/LEADERBOARD.md names, and
  `tools/gen_doc_assets.py --check` fails on a hand edit or on a table left on older files.

### Changed
- The tree is on Rust edition 2024 and builds with the pinned Rust 1.100.0 (`rust-toolchain.toml`,
  where it was 1.99.1). The oldest compiler that builds it is still 1.91 (`rust-version`). What the
  tool does is unchanged.
- A body under a public key label is shown only when it is a DER public key: exactly the one DER
  element its first bytes give the length of, then its END line. Any other body under any key
  label is marked as key material, an OpenPGP public key block and an RFC 4716 SSH2 public key
  among them ([Security](docs/SECURITY.md#secrets-in-answers)).
- `refs`, `callers` and `dead` hold a bounded set of parsed files: 256 MiB a request, and 256 MiB
  kept across the requests of `serve`, `lsp` and `batch`. The index stores each file's import
  bindings, so a C name looked up through included headers parses only the files whose trees it
  reads, so `refs` and `dead` on the Linux tree take far less time and memory (by how much waits for a recorded run).
  The index's schema goes to 18, so an older index is rebuilt once.
- An answer that a bounded read stopped short says why, in its own line and in `stopped_reason`
  beside `stopped_at` (`--json`, the MCP tools), where it read "stopped at 0 results".
- An SSH public key after its type (`ssh-ed25519 AAAA...`, as in `known_hosts` and
  `authorized_keys`) is left as written. A PEM public key's or certificate's body is left as
  written only when the END line of the same label follows in the same answer, and a DER body
  only at exactly the length it states; any other body stays redacted as before.
- `check` reports a definition without its duplicate keys, and duplicate keys, calls or imports
  left behind by a chunk or file that is gone.
- Outside a repository, the climb to an index in a directory above stops short of the system's
  temporary directory (and `/tmp` and `/var/tmp` on Unix), the home directory (also one kept in
  git) and a filesystem root: an index there answers only for that directory itself, and a
  command in a project below it runs as with no index above and says in one line which index it
  is not using, where it answered from that index, for a sibling project too
  ([CLI](docs/CLI.md#conventions)).
- tree-sitter 0.27.2 (was 0.26.13), tree-sitter-scala 0.26.13 (was 0.26.9) and
  tree-sitter-jsonnet 0.1.1 (was 0.0.1): Jsonnet's `for` object comprehensions parse as their own
  node, and Scala's case bodies by their indentation. A file with a syntax error can yield other
  definitions, since 0.27 recovers from errors elsewhere. Over 1,766 such files in LLVM and Linux,
  341 definitions were gained and 64 lost, and clang's `test/SemaCXX/attr-print.cpp` fell from 12
  to 1 (docs/ARCHITECTURE.md, Parsing).
- `grep --max-results N` with after-context (`-A`, `-C`) prints the context of the Nth match
  before it stops, as `-m N` does, where it stopped at the match; a match inside that context
  prints as a match, as ripgrep prints one after `-m`'s last, and does not count toward N
  ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `grep` with no path that searched no file, because `-g`, `-t`, `--max-depth` or an ignore file
  left out every one, prints `no files were searched` on stderr and exits 2, as ripgrep 15.2.0
  does, where it exited 1 in silence; a path given still exits 1
  ([CLI](docs/CLI.md#conventions)).
- A command on a large tree brings its index up to date sooner: the look at every file runs on
  every core, beside the read of what the index recorded, and a file's recorded hash is read only
  when its stat changed, so a warm `symbol` is faster (its time on the Linux tree waits for a recorded run)
  ([ADR 0012](docs/adr/0012-a-fresh-process-look.md)).
- `grep` prints a CRLF line with its `\r`, matched and context lines alike, in streamed, sorted
  (`--sort`) and cut (`--max-bytes`) output and in colour, as ripgrep 15.2.0 does, where it
  dropped the `\r`; `--json` and MCP text still carry no line end
  ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- blake2 0.11 and sha1 0.11 (digest 0.11): token keys, model signatures and file hashes are the
  same bytes as before, so an existing index stays valid.
- The MCP `grep_code` tool and `references` of a running server read the tree's files from a look
  the server keeps up to date with its watch, where each call walked the whole tree first: on the
  Linux tree a call no longer pays that walk, a query that matches nothing included (on Linux and
  Windows, and on macOS while its kqueue is in place; how much faster that makes a call
  waits for a recorded run) ([CLI](docs/CLI.md#serve-mcp)).
- A build from this repository with GCC 15 on x86_64 or aarch64 Linux compiles the tree-sitter
  grammars with `-fjump-tables`, which GCC 15 drops at the `-O0` their `parser.c` asks for:
  parsing was slower (by how much waits for a recorded run); a build outside the
  repository needs it in `CFLAGS`, which the repository's `CFLAGS_x86_64_unknown_linux_gnu` and
  `CFLAGS_aarch64_unknown_linux_gnu` are joined with ([Install](docs/INSTALL.md)).
- `tsquery` does not parse a file that holds none of the strings every match of the query needs
  (`#eq?`, `#any-of?`): on Linux's tree a `kmalloc` call query takes 21 to 34 s against 95 to
  146 s on four threads, the same answer.
- The text index is format 4, as it also records the hash of every line of a private key's body:
  one an earlier version wrote is rebuilt once, the cost of a first build (about 12 s on Linux's
  tree on an 8-vCPU server, 8% more CPU than format 3).
- `search`, `symbol` and every other answer read the index's statistics without counting the
  chunks table each time: the writer records the counts with each change, and the damage count
  runs once per state of the database. A fresh process on the Linux tree's index (5.2 million
  chunks) no longer spends about a second there ([Architecture](docs/ARCHITECTURE.md#storage-indexrs-lexrs)).
- `review` marks as no test reaching it only a changed function, method, class, struct, trait,
  interface, type, enum or impl, the kinds with call edges: an edited constant, macro, key,
  field or Markdown section is no longer counted under "no test reaches", and is no GitLab issue.
- `dupes` and `review` read the duplicate keys the index keeps for each definition, made once per
  text when it is parsed, and read a definition's text only when it is in a group or shares a
  bucket, so they no longer read every definition's text, and find the same groups. The index's
  schema goes up, so an older index is built again once; the keys make the index larger and a full
  index slower (by how much waits for a recorded run: no script in `bench/` reruns it).
- On macOS an index update flushes its files with a write barrier (`F_BARRIERFSYNC`) in place of
  a flush of the drive cache: the update behind a read of one changed file takes about 30 ms in
  place of about 150 ms, and the files still reach the disk before the database names them.
- A read in the MCP server, the language server or the Python bindings waits up to 250 ms for
  the process's own index update, and past that answers from the last committed index and says
  which changed files the answer does not hold yet: a line in the tool's text and `pending` in
  `structuredContent`, a language server log warning, a Python `UserWarning`. Before, it answered
  at once and said nothing. A command `serve` answers is no longer handed back to the command
  while an update runs, and says the same on stderr
  ([ADR 0002](docs/adr/0002-reads-during-a-write.md), [CLI](docs/CLI.md#the-index-directory)).
- The MCP server, the language server and the Python bindings watch a tree on a local disk, and
  a walk in the background finds a change that makes no event within 60 s, or 50 times what one
  walk takes when that is longer. On Linux and Windows
  each call looks only at the files the watch says changed since the last call, one `stat` each,
  where it looked at every file of the tree: a cookie file the call writes and waits for keeps
  every write made before the call in its answer. On macOS, where FSEvents can give a write a
  later event than a cookie made after it, the server watches every file and directory its walk lists (an ignored build output takes no watch) through a
  kqueue instead, whose notes the system posts inside the call that makes the change: a call reads
  them at once and looks at the files they name, where it looked at the tree (the time saved
  waits for a recorded run). While those watches are not in place, for a
  tree, or the trees of all of a process's sessions together, past half the process's descriptors less 1,024 (`kern.maxfilesperproc`; about 29,700 files
  and directories where it is 61,440), or on a volume that gives no kqueue notes, every macOS
  call looks at every file. When the watch cannot vouch for fewer files, or
  cannot start, the call looks at every file as before. `SCRUTATE_SESSION_DEBUG=1` says why
  ([ADR 0001](docs/adr/0001-change-sets-and-the-cookie-barrier.md), [CLI](docs/CLI.md)).
- A fresh process opens an index whose database it saw pass a check as fast as a running one:
  the `quick_check` walk and the schema work run once per state of the file, recorded in
  `.scrutate/checked`, and an update that finds nothing changed no longer rewrites the database.
  The semantic model's tables are read where they lie in the binary instead of being copied.
  `symbol` and `search` are faster for it (by how much waits for a recorded run) ([Architecture](docs/ARCHITECTURE.md#storage-indexrs-lexrs)).
- The index keeps a table of the definition names, one row per spelling, with their word
  initials and a trigram index over them, for `symbol`'s near matches; its schema version goes
  up, so an index made before is rebuilt on its next use ([CLI](docs/CLI.md#engine-b)).
- The `scrutate` crate is ready for crates.io: it holds its models and data and nothing else
  (7.8 MiB compressed, `bench/results/platforms/packages/cargo-package.log`), and `cargo publish --dry-run -p scrutate-ignore -p scrutate` verifies both
  crates; publishing waits for the release ([INSTALL](docs/INSTALL.md)).
- The bundled semantic model stores its word tables as int8 with one scale per row: 7.6 MB in
  place of 14.6 MB, and the binary is that much smaller. `bench/quality.py` measured MRR@10 0.5873
  before the change (`bench/quality/floor.json`); the run after it is not committed. Its revision is 2, so an index makes its vectors again on the next update; a model
  file named by `SCRUTATE_SEMANTIC` must be in the new format ([QUALITY](docs/QUALITY.md)).
- The vendored walker is its own crate, `scrutate-ignore` (a modified copy of `ignore` 0.4.34,
  `Unlicense OR MIT`, its eleven changes listed in `vendor/ignore/README.md`), and the workspace
  `[patch]` is gone, so a crate built from crates.io walks with the same code
  ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- The macOS benchmark chain waits for the new binary to start at its settled speed
  (`bench/wait_aged.py`, at most an hour) in place of a fixed hour
  ([RELEASING](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).
- `bench/leaderboard.py` stops a row at 20 runs once every verdict holds at alpha 0.001
  (Haybittle-Peto boundary). On by default, off with `--no-stop-early`; a published verdict needs
  30 runs per tool, so CI's gate is what the stop rule serves (see the entry on verdicts below)
  ([RELEASING](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).
- The release binaries for glibc Linux (x86_64 and aarch64) lay out the functions a small search
  runs side by side, as the macOS one does: a second link with rust-lld and the order file under
  that build's own names ([Releasing](docs/RELEASING.md#2-benchmarks-on-the-release-binary)).
- With `serve --structured`, a large answer pages in `structuredContent` too: each page carries
  the JSON of the items its text page shows, with `offset`, `next_offset` and `total`, in place
  of a `null` result ([CLI](docs/CLI.md#serve-mcp)).
- An MCP call with a `progressToken` hears the files read so far against the total, while the
  first index is built and while `reindex`, a large scan or `structural_search` runs, at most
  twice a second; it no longer hears the seconds waited. A cancelled `structural_search` stops
  reading files ([CLI](docs/CLI.md#serve-mcp)).
- The crate builds and runs on `wasm32-wasip1`, which starts no thread: the thread rules take
  one thread, and Engine A, a parallel walk of the vendored `ignore`, `index`, `search`, `symbol`
  and `callers` run on the calling thread. `serve`, `lsp` and `tsquery` fail there with a message
  that says they need threads, `watch` says wasi has no file events (`notify` is not built
  there), and the Grep hook lets Grep answer. Files the system cannot map are read into memory,
  and the text index updates without its lock, which wasi does not have. The vendored `ignore`
  gains two changes for it, eleven in all. A CI job checks the build
  ([ARCHITECTURE](docs/ARCHITECTURE.md)). Native builds are unchanged.
- INSTALL.md's attestation check pins the release workflow and the tag
  (`--signer-workflow`, `--source-ref`): with `--owner` alone any workflow of the organization
  passed. It says what "signed" means and how to run the binaries, which carry no Apple or
  Windows code signature ([Install](docs/INSTALL.md#unsigned-binaries-on-macos-and-windows)).
- A release waits for `security.yml` on the tagged commit as well as `ci.yml`: a crate under a
  licence outside `deny.toml`, a git dependency, a zizmor finding or an unlisted `unsafe` now
  stops it. `ci.yml` no longer runs `cargo audit`; `cargo deny` checks the same advisories.
- PCRE2 is the `pcre2` cargo feature, on by default; a build without it refuses `-P`
  ([CLI](docs/CLI.md)).
- The repository is now github.com/scrutate/scrutate, under the `scrutate` organization; the old
  address redirects, and the install commands, Action, CI templates, packages and
  `CITATION.cff` name the new one.
- With reduced motion on, the README and document images stop only what travels or grows; before,
  they showed only slow cross-fades.
- Ranked search folds Japanese shinjitai with traditional and simplified Han, so `関係` finds
  `關係` and `关系` and the reverse (OpenCC ver.1.4.2 `JPShinjitaiCharacters.txt`, Apache-2.0).
  An older index reads every file again ([ARCHITECTURE](docs/ARCHITECTURE.md#tokens-textrs)).
- `watch` on macOS follows FSEvents with file-level events, loaded with `dlopen` only by `watch`,
  so a save updates that file alone with no walk of the tree (one walk at most every 30 s), much
  sooner than the timer did (the time from a save to the update waits for a recorded run). The timer stays as the fallback when FSEvents cannot
  load ([CLI](docs/CLI.md#watch)).
- The `notify` crate is 9.0.0-rc.9, pinned (it needs Rust 1.91, the existing minimum).
- Three tool errors say how to fix the call: `missing required argument name` (was `missing
  name`, and `missing to` for `call_path`), `missing path or symbol` names what each takes, and
  `path outside repo` says to give a path relative to the root. `batch` reports the same text
  ([CLI](docs/CLI.md#serve-mcp)).
- The benchmark harness publishes a verdict only on 30 runs per tool. `bench/indexed.py` takes
  30 query runs and 30 throughput runs by default, and a comparison under 30 (its 5 index builds,
  or a committed result's 20 query runs) is shown as measured, not ranked, so docs/LEADERBOARD.md
  and README.md now rank the indexed results on index size and query memory only; an Engine A row
  whose scrutate or best competitor has fewer than 30 runs reads measured as well. The publish
  scripts take every run (`--no-stop-early`), and `bench/run_macos.sh` refuses a tree with changes
  no commit holds and builds the binary from it. `indexed.py` records the corpus's tree checksum,
  `indexed.py` and `compare_tools.py` make one untimed warm-up run per tool and count outliers,
  and every harness checks each competitor against the latest stable release pinned in
  `bench/competitors/latest.json`, warning on an older one. Each LEADERBOARD.md row now gives
  every tool's 95% interval, scrutate's rank and a link to its raw result file
  ([Leaderboard](docs/LEADERBOARD.md)).
- CI builds the test binaries at opt-level 3 without cross-crate LTO and with 16 codegen units, as
  `tools/ci_gate.sh` does; the binary is still linked with the release profile (fat LTO, one
  unit). The two write-then-read session tests run 200 loops, 1000 with `SCRUTATE_TEST_STRESS`
  set ([Contributing](CONTRIBUTING.md#quality-gates-all-must-pass-before-a-change-is-merged)).
- `grep` without `--max-results` counts a file's matches in the file's own search and adds them
  to the shared total once per file, where every matching line added to it, and the counters every
  search thread adds to each sit on a cache line of their own. On many-core x86 the threads had
  waited on each other for those lines when a search prints many lines. The counts printed do not
  change.
- Plain `grep` output past its first 64 KiB is written by a thread of its own, fed full 64 KiB
  buffers in the order they filled, so no search thread waits on a write. While one search thread
  held the output for a write, the others slept behind it, and on Linux x86 the writes added to the
  search's time (what the writing thread saves waits for a recorded run). A smaller
  output starts no thread; a write error still stops the search and is reported once
  ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- `refs`, `callers`, `references` and `dead` read a binary file the text index holds only when
  its first NUL byte is past its first 4 KiB, where they read it to its end; one with a NUL in that
  head (an image, a jar, a wasm module) they pass by its trigrams, as `grep --index` does. They
  read every file with a NUL byte on every call, and read a file's first 4 KiB a second time
  when the whole file was already in memory. An index built before records no NUL position, and
  its binary files are read as before until they are indexed again.

### Fixed
- `grep --sort` and Python's `sort=True` print each file's lines once the files before it are
  done, where every matching line was held until the search ended: 4 files of 500,000 matching
  lines took 1 GB, and take 7 MB.
- A Rust `use` of another workspace crate resolves when its `Cargo.toml` names it with a comment
  after the name, in single quotes, under `[package] # ...`, or through `[lib] name`: the manifest
  was read line by line, kept `"  # comment` or the quotes in the name, and missed those headers
  and the library's own name.
- `watch` and `serve --watch` on macOS and Windows no longer walk the whole tree for an ignore
  file or `.scrutate.toml` written in a directory the walks skip, such as the `.gitignore`
  `python3 -m venv` writes in `.venv/` or a fresh tool cache: the watcher reports events inside
  those directories, and any such file under the root counted as a rule change.
- Redaction takes a secret followed by `&`, as in a URL's query string
  (`?api_key=...&format=json`, `jdbc:...&password=...&useSSL=false`, a JWT before `&x=1`): such
  lines reached MCP answers and hook output as written ([Security](docs/SECURITY.md)).
- Redaction takes an unquoted password, secret or token whose value holds a symbol such as `@` or
  `!` (a YAML `password:`, a `.properties` `spring.datasource.password=`), and a `.env` value named
  `PASS` or `..._PASS` (`DB_PASS`, `SMTP_PASS`; not `BYPASS`): these were shown as written
  ([Security](docs/SECURITY.md)).
- Redaction takes a credential in XML, a new `xml-secret` rule: the text of an element named for a
  password, secret, token or API key (Maven's `settings.xml` `<password>`) and the `value` of a
  .NET `<add key="ApiKey" value="...">`, which were shown as written ([Security](docs/SECURITY.md)).
- Redaction takes a base64 key that starts with `/`, such as about one Azure storage
  `AccountKey` in 32: it was left out as an absolute path. A value of base64 characters that ends
  in `=` padding is no path; a path such as `/usr/include/c++/v1` still is.
- Redaction leaves code and home paths after a password, secret or token name as written:
  `password: this.form?.value?.password`, `token: ctx.auth!.accessToken`, Razor's
  `password: @Model.Password` and `secret_file: ~/.aws/credentials` were redacted. Only a chain of
  letter names with `?.` or `!.` in it, `@` before a dotted chain of letter names, and `~/` and a
  path of path characters whose first segment starts with a dot or is followed by another are
  spared; a chain with a digit or another symbol, and `~/` and one segment (`~/Prod2024key`), are
  redacted as before. Words joined by dots alone (a passphrase) are redacted only quoted or under
  an `env-secret` name: unquoted, `password: Correct.Horse.Battery.Staple` is shown.
- `xml-secret` leaves a deploy-time placeholder as written, so the variable it names stays
  readable: Octopus `#{db.password}`, token replacement `@@DB_PASSWORD@@`, a bracketed
  `[ClientSecret]` and Razor's `@Model.Password`. Only those closed forms of names alone are
  spared, and only in XML.
- A render or build stage's setting named `..._PASS` is shown as written (`RENDER_PASS=shadowmap2`,
  `SHADOW_PASS=enabled-v2`, `LINT_PASS=strict_mode`): the name must hold a render pass's or a
  build stage's word, and the value be made of that stage's setting words alone, with short
  versions. `DB_PASS`, `TEST_PASS`, and a weak password or a generated value under a stage's name
  (`RENDER` is also a hosting platform, `CI_BUILD_PASS` a CI credential) are still redacted.
- `--deps PACKAGE.NAME` lists the package's own `NAME` before a method of that name, in Go and Python (`assert.Equal` is testify's function, `assert.Assertions.Equal` its method), and a bare Go package name many packages share is refused naming the first three import paths and how many more, where client-go's 59 packages named `fake` made one line of 3,604 bytes.
- `--deps` finds a Go package in a module's subdirectory, by its import path (`go.uber.org/mock/gomock.NewController`) or by its name (`gomock.NewController`), where both said "no package of go.mod names" and only the module's own names answered: testify's `assert`, `golang.org/x/sync/errgroup` and most Go imports are such packages. Two packages of one name answer the one the repository imports, or are refused naming both import paths.
- A source file the code index leaves out (over 1 MB, or not text or could not be parsed) is said
  wherever an answer depends on it, where `symbol`, `callers`, `read --symbol`, `dupes` and the
  rest answered as if it held nothing: `index`, `check` and `stats` name it, a lookup that found
  nothing or names it says why it is not in the index and, for a text file, what reads it,
  `dupes`, `dead` and `map` always say it, `refs` labels an occurrence in it as in a file that is not indexed where
  it said "not an identifier", and `impact` and `review` list it as not indexed; `--json` and the
  MCP tools carry a `not_indexed` field ([CLI](docs/CLI.md#files-the-index-leaves-out)).
- `tsquery` and `tsquery --rewrite` say a source file over 1 MB, which they do not search: named
  on the command line it gets `PATH: over 1 MB, left alone` and exit 2, where it exited 1 with
  nothing printed as if it held no match; met in a walk it is one note with the count and the
  first names, a `skipped` record in `--json` and `skipped` in the `structural_search` envelope
  ([CLI](docs/CLI.md)).
- `--deps` reaches each distribution under a pkgutil-style namespace (`zope.interface`,
  `backports.tarfile`) by its own package, where two distributions that both ship the same
  `ns/__init__.py` made every name under it "names two packages" ([CLI](docs/CLI.md#--deps-a-dependency-at-its-locked-version)).
- `--deps` finds a Go module that go.mod replaces with a fork of another name by the path the code
  imports and by its package name (`yaml.Marshal` for a fork of github.com/a/yaml), where only the
  fork's own path found it ([CLI](docs/CLI.md#--deps-a-dependency-at-its-locked-version)).
- `dead` no longer lists a definition that another one calls from the same line (`static int
  sq(int v) { return v * v; } int area(int v) { return sq(v); }`, a one-line class whose public
  method calls its private one, two Go functions on a line): a use on its own first or last line
  counts as its own only inside the span its grammar gives it. Where no grammar gives a span, a
  definition whose name stands again on that line is undecided, with why
  ([CLI](docs/CLI.md#engine-b)).
- `dead` reads a definition's modifiers from its own text where another definition shares its
  line: a Kotlin, Scala or PHP public method after a `private` one on the same line was listed as
  dead, and a private method or function after a `public`, `export`, `pub` or `extern` one (Java,
  C#, TypeScript, Rust) was taken for an entry point and never listed; a one-line class's methods
  now have that class as their holder ([CLI](docs/CLI.md#engine-b)).
- A crate Cargo.lock locks at one version from two sources (the registry and a git copy a git
  dependency pulls in) is read by `symbol --deps` from the source the repository's own packages
  depend on,
  where the lockfile's first entry won: "comes from git" for a crate the root takes from crates.io,
  or the registry copy for one it takes from git.
- `symbol --deps` and `read --deps --symbol` (MCP `scope: "deps"`) look a path dependency's name up
  inside its directory before the limit counts, where a Cargo workspace member, an npm workspace
  link or a uv editable source answered "(no results)" once more than 20 other files of the
  repository defined the name, or one hit marked as cut; a read past 20 of the dependency's own
  definitions says to name one by its location, where it said to pass a path `--deps` does not take.
- `refs NAME --lang c` (and any file filter) counts `--max-results` over the occurrences in the
  files the filter keeps, where a C++ header's uses could fill the count and be dropped after it,
  answering none of the C header's ([CLI](docs/CLI.md#engine-b)).
- `dupes` counts a MinHash bucket in distinct bodies, the copies of one body standing as one
  member, so a near copy of a body copied 64 times or more is found, where it was in no answer: a
  body with 64 exact copies and one copy changed in one word gave the exact group alone. Such a
  pair is reported once, against the first copy, which the text marks `(one of 64 exact copies)`
  and `--json` gives as `copies` (`[64, 1]`), where a body with fewer copies gave a pair per copy.
  A bucket of more than 64 distinct bodies is still not compared pair by pair, and the answer now
  says so: a last line counts the functions in such a bucket, says that a near pair among them
  that shares no other bucket is not in the answer, and names the first five (`crowded` in the
  JSON, the MCP result and the Python bindings) ([CLI](docs/CLI.md)).
- `watch`, `serve --watch` and the watch behind every running server's reads (on Linux) see
  every file under a directory moved, removed or made while their watches were being set: the
  tree is walked again when a directory it listed was gone before its watch or one came, at most
  3 times, after which every directory is checked on the timer. A directory moved in that window
  was watched under neither name, and files made under it afterwards reached neither the index
  nor `grep_code` ([CLI](docs/CLI.md#watch)).
- `refs --kind` (the MCP `references` with `kinds`) counts `--max-results` over the occurrences
  of the kinds it keeps, where it counted every kind, dropped the others and could answer none.
  When no definition of the name has a kept kind it answers at once and says so; otherwise it
  resolves at most 10,000 occurrences (or `--max-results` when that is larger) and says when that
  left it short.
- A name written with its owner (`zz::Holder::push`, `Holder.push`) finds its definition however
  many other definitions share its last part, where past 10,000 it answered "(no results)".
- `read_code`, `read --symbol` and `edit_symbol` with a path find the name's definition in that
  file however many other files define it, where past 21 they answered "no definition matches".
- The password and generic secret rules leave a quoted value that reads as a name as written: a
  path with a common extension and no part mixing upper case, lower case and digits
  (`config/secret_settings.toml`), or capitalised words run together with up to four digits
  (`"TokenRefreshed2024"`).
- `impact --help` describes `--json`, and `review --help` describes `--format`; both lines were
  blank.
- One tree gives the same answers however its index was built (at once, in steps, with files
  rewritten and restored, or in reverse): definitions of one name that rank equal, ranked search
  results and `similar` neighbours of equal score are ordered by path and line, where they came
  in the order their rows were written. `symbol a0 --limit 5` over 40 files defining `a0` alike
  listed files 30 to 34 after an index built in reverse batches, and 00 to 04 after one built at
  once; `find_symbol`, `read_code` with a symbol and no path, `callees`, the language server and
  `edit_symbol`'s list of candidates followed it ([CLI](docs/CLI.md#conventions)).
- `read --history`'s note for a git that failed says why (`git could not follow these lines: git
  log: ended by signal 11 (a crash) and gave no reason`), where it said only that git could not;
  and a git that dies while scrutate lists the filter drivers or reads the base copies, or a base
  copy git does not have, is an error that says so, where the copies read as empty files and the
  filters as none.
- `impact --staged` and `review` name the window that holds a changed line outside every
  definition (`b.py:3-3 [window] b`), as the working-tree form does, where they named the whole
  file (`b.py [file]`).
- `refs` (MCP `references`), resolved or `--textual`, says it stopped only when more occurrences
  exist, where an answer of exactly `--max-results` occurrences said it was cut.
- `refs Stack::push` (MCP `references` with a qualified name) keeps the definitions `symbol`
  finds for that name and counts `--max-results` over their uses, where it read the name as its
  last part, listed every `push` and could fill the answer with another one's uses before any of
  `Stack.push`'s ([CLI](docs/CLI.md#refs)).
- The MCP `references` tool refuses `def` with `textual`, as the command line does, where it
  answered the textual form and dropped `def` without a word.
- `refs NAME --def PATH` (MCP `references` with `def`) counts `--max-results` over the occurrences
  that resolve to that definition, where it counted every occurrence of the name first and could
  answer "0 references" for a definition in use. It keeps only that definition's occurrences,
  reading at most 10,000 (or `--max-results` when larger) and saying so when it stops there,
  and still counts those that resolve to other definitions and lists the unresolved ones, as
  `--kind` now does too
  ([CLI](docs/CLI.md)).
- `impact --staged` with a root below the repository's top answers with a conflict outside that
  root, where git 2.53 and 2.54 crash on the `--cached --relative` diff it ran: the staged diff
  there names the root as its pathspec and scrutate takes the root's part off the paths.
- `check` names a segment of 20,000 chunks or more with vectors that lacks its inverted-file lists
  (a removal that stopped midway), and exits 1, where it passed such an index while every search
  read all of that segment's vectors; the answers were right, only slower, and `scrutate index
  --force` writes the lists again ([CLI](docs/CLI.md#engine-b)).
- An error from a git that crashed or failed without a word names how it ended (`git diff-index:
  ended by signal 11 (a crash) and gave no reason`, or its exit status), where `impact` and
  `review` said `git diff-index: ` and nothing after it.
- Plain `impact` refuses a file a merge left in conflict ("is unmerged: finish the merge before
  asking what it changes", exit 2), as `impact --staged` did, where it named the conflict-marked
  text as an edit.
- `SCRUTATE_NO_SESSION`, `SCRUTATE_SESSION_DEBUG`, `SCRUTATE_GREP_DEBUG`, `SCRUTATE_WATCH_DEBUG`
  and `SCRUTATE_PROFILE` are off for `false`, `f`, `no`, `n` and `off` as for `0` or empty, as every other
  on/off variable is, where `SCRUTATE_NO_SESSION=false` turned the use of a running `serve` off,
  `SCRUTATE_SESSION_DEBUG=off` turned its lines on and `SCRUTATE_PROFILE=0` printed timings
  ([CLI](docs/CLI.md#conventions)).
- A directory named as git's or scrutate's own under another spelling (`.GIT`, `.Git.`,
  `.Scrutate`, the short name `git~1`), which a case-insensitive file system such as APFS or NTFS
  takes for `.git` or `.scrutate`, is no longer indexed, nor searched by `grep_code` and the
  index's searches (plain `scrutate grep` walks as ripgrep does), at any depth, where a nested
  `vendor/lib/.GIT/hooks` was indexed and `symbol` answered from its files ([CLI](docs/CLI.md)).
- An index that holds no files and cannot be filled (a rebuild cut short after it cleared the
  index, opened where it cannot be written) answers with an error, exit 2, where every search and
  lookup answered "(no results)" as if nothing matched ([CLI](docs/CLI.md)).
- A compaction whose merged segment another update's clean-up removed, in whole or in part,
  before the compaction committed (the clock moved more than 10 minutes meanwhile: a suspend, a
  clock step) gives up its plan, where it committed a list naming that segment and the next read rebuilt the whole index.
- `watch` started with a pipe for its input ends when the pipe does, with exit code 0, so it no
  longer outlives an agent or script that started it and was killed; from a terminal or with no
  input it runs as before.
- `serve --watch`, the MCP server's watch, the language server and the Python bindings follow a
  root reached through a link when the link is pointed at another tree (or replaced by a
  directory, or a link it leads through is pointed elsewhere), as `watch` does, where they kept
  answering from the old tree. Off a local disk only `serve --watch` follows, since the others
  watch nothing there; a command line call made in the old tree is no longer answered by
  that session.
- `impact` and `review` take time in proportion to the change, where it grew with its square
  (their times on a large change waits for a recorded run). Each answer the caller walk stores no longer lists the answers directory twice.
- A reader that closes early (`| head`) ends `impact`, `review` (every format) and every other
  command quietly with exit 0, as it ended a search, where they printed "internal error: failed
  printing to stdout: Broken pipe" and exited 2 ([CLI](docs/CLI.md)).
- `impact` and `review` read a file moved where the index's rules leave it out (a directory
  `.ignore` or `.gitignore` names) as deleted, with its callers and tests as `git rm` of it gives,
  where they listed nothing ([CLI](docs/CLI.md)).
- A file that is one of scrutate's index files under another name, a hard link either way round,
  is left out of every search scrutate walks itself on Unix (`grep`, the index and so `search`,
  `refs` and the MCP tools, the Grep hook), where only a path naming it was refused. The walk says
  so once on stderr and `check` lists each one (`left_out` in `--json`). `grep` still searches such
  a path when you name it; the Grep hook and the MCP tools (`read_code`, `grep_code`) refuse it, naming the index file
  it is and saying that removing the hard link, by its path, lets it be searched again. Every file under `.scrutate/` that can hold a file's text reads as binary,
  which keeps such a file out of the searches on Windows too ([Security](docs/SECURITY.md)).
- `watch` and `serve --watch` drop from the indexes a file that is one of scrutate's index files
  under another name when it comes in as a change, on every platform, where a hard link made while
  they ran to an index file written after they started was indexed under its new name.
- A command line call gives up a running `serve` that takes no request for 2 s once it greeted and
  does the work itself, where a `serve` stopped or stuck at that point and a request over the
  socket's buffer (a long search with several `--alt`) left the call waiting for good.
- A save a read makes, which must not wait, no longer waits for another process's write lock to
  compact the index, up to 10 minutes while that process indexed: it tries once and leaves the
  compaction to the same process's next update, which does it even when nothing changed.
- Under `serve --refresh-interval S` a session saw no edit at all until the interval passed: once
  the watches were set after the first call, the walk that follows them was held back by the
  interval, and every call's change set waited with it, the files the watch named included. Now
  that walk is part of start-up and the interval never holds it, and a call whose walk the
  interval holds back still looks at the files the watch named; a new directory still waits for
  the interval, as documented. That start-up walk is one walk: new directories made before a call
  took it no longer each get a walk of every file the interval does not hold back.
- Under `serve --refresh-interval S` on Linux an edit was lost until the interval passed when a
  directory was made, removed or moved before the next call: setting the watches again for that
  directory dropped the events noted so far, so the held-back walk had nothing to apply. They are
  kept now, and dropped only when the watch lost events, reported an error or noted too many.
- The language server answers definition and references in the spelling of the request's
  document when the editor names one repository in two ways (two folders, one through a link, or
  a file named through one), where every answer named the first folder's spelling.
- `impact` and `review` with a `GIT_DIR` that is not the root's own git directory, no
  `GIT_WORK_TREE` and no `core.worktree` stop with exit 2 and say to set `GIT_WORK_TREE`
  (`read --history` gives that as its note), where they read that repository's history against
  this tree; one whose `core.worktree` is another tree (a submodule's, from its superproject)
  names that tree, where the answer said the root was not in a git repository. Only the git
  directory's own `core.worktree` counts, as for git: one in an included file or the global
  configuration let another repository's history through.
- An idle `serve` or `lsp` lets go of the index segments another process
  replaced within 5 s, where it kept the deleted files mapped, their disk space held, until its
  next call.
- On Windows `hook run` goes into its job before it starts the program, so the program is in the
  job from its first instruction and cannot outlive `hook run` killed in the moment after the
  start; the child is put in a job after its start only when `hook run` cannot go in itself.
- `grep --sort` with context prints the `--` of a group a NUL byte ended before its first line,
  ahead of the `binary file matches` note, as ripgrep and the unsorted output do
  ([CLI](docs/CLI.md#conventions)).
- `grep` gives a walked file with a NUL byte past its first 64 KiB the lines ripgrep prints for it
  searched alone, whatever was searched before it, where a file with a line longer than 64 KiB
  earlier on the same thread (`-j1`) made it stop sooner and print nothing
  ([CLI](docs/CLI.md#conventions)).
- `tsquery --rewrite --write` leaves alone, and names, a file reached through a link to a
  directory of the root (`dl/b.py`, or `dl` named, with `dl` a link to `real`), where it wrote the
  file the link leads to; a link was already left alone only as the file itself
  ([CLI](docs/CLI.md#tsquery---rewrite)).
- A call that saves an edit to the index no longer waits up to a second to cut the index's log
  while another process reads an older snapshot; it tries once, and the next update cuts it.
- Freeing the word caches after a saved edit no longer holds the call until every pool thread
  is free: while another call's search kept one busy, the call waited as long as that search ran
  (4 to 25 s while other index builds ran in the same process). Each thread frees them when it is
  next free.
- The language server's `workspace/symbol` lists the symbols of a repository opened as its own
  folder inside another folder once, from the deepest folder that answered (the inner one, or
  while its first build runs the nearest one above it), where each folder whose index held them
  listed them again and each took several of the 200 places
  ([CLI](docs/CLI.md#lsp)).
- The Bash hook reads `rg --sort path PATTERN DIR` as ripgrep does, where it took `path` for the
  pattern and answered another search with exit 0; `--sort none` and the last `--sort` given are
  honoured, and `--sortr` or an order other than path runs `rg` itself
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- The language server takes `$/cancelRequest`: the request it names stops its wait for the first
  build and its search and is answered RequestCancelled (-32800), so a long `references` no
  longer holds back every request after it, `shutdown` included ([CLI](docs/CLI.md#lsp)).
- `callees` of a function whose every callee is outside the repository no longer prints
  `(no results)` above the `external:` list, in the CLI and in either MCP form.
- `impact`, `--staged` and `review` no longer list a test file the change deletes as a test to run.
- `review`'s comment puts a renamed file's old path in a code span, as every other path: a name
  such as `x<!--.py` hid the rest of the comment, and `@name` notified a user.
- `review`'s cut comment keeps the line that says the walk stopped, and its last line names the command that prints the whole report, the same
  base, head and depth with `--format json` (`--head` and `--depth` when they are not the
  default), in a code span.
- `impact --base A..B` and `review --base A..B` say a two-dot range is not a revision and point at
  `A...B` for the merge base, where they said the repository lacked it; a `--head`
  given as a range (`A...B` or `A..B`) says to name one commit.
- A walk stopped at 2,000 callers lists them by distance, path and line, and names no
  `untested-change`, which its partial calls could not decide.
- `dupes` lists each group's members by path and line, and cuts near pairs at `--limit` after
  ordering them, so an index brought up to date answers as a fresh one of the same tree.
- `impact` and `review` read the diff with git's plumbing (`diff-index`, `diff-tree`), which never
  writes `.git/index`, and with every filter driver emptied, so a touched file no longer has git
  rewrite the index (holding `index.lock` against your own `git add`) or run a clean filter such as
  Git LFS's ([Security](docs/SECURITY.md#running-git)). `read --history` reads a file's uncommitted
  edits the same way, and every git call goes through one builder that runs only commands that
  read.
- `impact` and `review` fetch nothing in a partial clone: a blob the base needs and the clone lacks
  is an error that says to fetch it first, where git fetched it from the remote, writing
  `.git/objects` and running the remote's upload program (git 2.44 and later). `read --history`
  fetches nothing either, and its note says the same (`this partial clone lacks objects that git
  log needs: ...`), where it said that git could not follow the lines.
  A remote's promisor mark is read as git reads a boolean: `promisor = no` is no partial clone and
  a bare `promisor` key is one, as `impact` and `review` see them.
- `read --history` runs no `gpg.program` when the configuration sets `log.showSignature`, and every
  diff and log git runs for scrutate is told to run no external diff or text conversion driver.
- The language server names the files of its answers under the folder as the editor sent it, also
  when that folder is reached through a link (macOS `/tmp` and `/var/folders`, a linked `~/code`),
  where it named them under the resolved path and the editor opened a second tab of an open file
  ([CLI](docs/CLI.md#lsp)).
- `watch` and `serve --watch` notice a directory put in place of the root (`mv repo repo.old; mv
  repo.new repo`, or the root removed and made again), say `root replaced; watching the new tree`,
  and index and watch the new tree, where they went on watching the old one and indexed nothing.
- On Linux, `watch` goes on indexing a root renamed away and back within 2 s, where it kept
  running and indexed nothing after it, with no message: the watch of a directory above the
  root saw the rename and every watch of the tree was dropped. A root renamed away with a link
  to it put where it was is followed through the link (`root replaced; watching the new tree`),
  where the watch was set on the link and saw nothing.
- The yardstick times every rival of a form against one named scrutate adapter (resident:
  `scrutate-mcp`), and each row names its `base`, where the last scrutate adapter of the form won,
  so W5's refs rows were against `scrutate-lsp`, its callers rows against `scrutate-mcp`, and the
  workload's geometric mean mixed the two.
- The yardstick's language server adapter (`bench/rivals/lsp.py`) opens each file once, on the
  checked request before the timed ones, where every timed request sent `didOpen` again and clangd
  parsed the file again inside the timing (its callers rows about 4 times too slow).
- `bench/indexed.py` fails scrutate's concurrent throughput row only when scrutate's own queries
  fail, and leaves out a rival whose queries failed, where any tool's failures failed scrutate's
  row; a tool's server is used once its first query answers, in place of a fixed 10 s wait.
- `grep` exits 2 when it could not search a path, as ripgrep does, after any matches: a named or
  walked file or directory it cannot read, a dangling link or a loop `-L` follows, an archive
  member it stops in, and PCRE2's match limit. A named unreadable file exited 1, which reads as
  "no match", and a walk with unreadable files exited 0 ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `grep`'s plain output (piped, no colour) prints a line that is not UTF-8 as its own bytes, as
  ripgrep does (`foo \xff\xfe bar`), where it printed U+FFFD for each bad byte, with or without
  `--sort`; `--json` text keeps U+FFFD ([CLI](docs/CLI.md#conventions)).
- `dead` no longer lists as dead a Fortran definition called in another case in a build without
  the Fortran grammar (`call dowork()` for `subroutine DoWork`): a Fortran file ignores case by
  its extension there too, so such a definition is undecided at worst, and `refs` finds its uses
  in any case ([CLI](docs/CLI.md#engine-b)).
- `dead` and `refs` read a name as its file's language writes it: `foo'` in Haskell, `$init` in
  JavaScript and `push!` in Julia are other names, so an unused `foo`, `init` or `push` is dead
  and `refs` no longer lists the longer name's lines as its unresolved uses. Lua's `width-margin`
  is a use of `width` where no definition has that name; `$version` in a Scala or Groovy string and
  Haskell's quote `'helper` still name `version` and `helper` ([CLI](docs/CLI.md#engine-b)).
- `impact` lists a caller of a deleted definition that calls it through an import alias
  (`from util import helper as h`, JavaScript `import { helper as h }`) or, where names ignore
  case, in another case (PHP `dothing()` for a deleted `DoThing`), as `callers` does for a live
  one; it listed none, and the tests behind it were missed ([CLI](docs/CLI.md#engine-b)).
- `refs`, `callers` and `callees` take one decision per call site, so they agree: `refs` of a
  constructor lists its class's instantiations (`new Box(1)`, `Point(1)`, Ruby `Invoice.new`,
  JavaScript `super(..)`), which `callers` already listed; in a language the resolver does not
  read, and in a file nested too deeply, `refs` ties a use to the one definition of its name it
  can reach, marked `(by name)`, where `callers` kept the call and `refs` left it unresolved;
  `callers` keeps a caller decided when one of its calls ties to the definition through an
  alias (PHP `geoArea()` beside an undecided `area()`) ([CLI](docs/CLI.md#engine-b)).
- `callees` of Ruby's `Invoice.new` lists the class `Invoice`, where it listed `new` as external
  ([CLI](docs/CLI.md#engine-b)).
- `callers` of a PHP class lists `new self()` and `new static()` in its methods, and of the base
  class `new parent()`, in any case, which `callees` already listed; `refs` lists them too
  ([CLI](docs/CLI.md#engine-b)).
- `refs` of a PHP use alias finds it in any case: `refs pf` lists `PF()` after `use function
  Lib\phpfn as pf`, and `refs T` lists `t::make()` ([CLI](docs/CLI.md#engine-b)).
- A JavaScript or TypeScript quoted key holding a function (`"get": function () {}`,
  `"q": () => 5`) is a method of what holds the object, as `get: function () {}` is; a call in it
  had only the module chunk as caller. The content revision goes up so an older index reads its
  files again ([CLI](docs/CLI.md#engine-b)).
- The clauses of a Haskell function (`double 0 = 0` then `double x = x * 2`) are one definition
  with its signature, and each clause head is that definition in `refs` and the LSP, where the
  second clause was a definition of its own and a clause head a by-name use; the LSP's
  references without the declaration leave out every line of it. The content revision goes up so
  an older index reads its files again ([CLI](docs/CLI.md#engine-b)).
- `callers` and `refs` follow an element and a returned value to its declared type: Go
  `xs[0].Save()`, `m["a"].Save()` and `f().Save()` for `xs []Store`, `m map[string]Store`,
  `f func() Store`, and Python `xs[0].save()` and `d['a'].save()` for `list[Store]` and
  `dict[str, Store]` are calls of `Store.save`, where they were undecided, and so are an
  element of Rust `&[Store]`, `[Store; 2]`, `Vec<Store>` and `HashMap<K, Store>`, of TypeScript
  `Store[]`, `Array<Store>` and `Record<string, Store>`, and a place of a tuple by its number
  (Python `tuple[Other, Store]` and `tuple[Store, ...]`, TypeScript `[Other, Store]`); the undecided note no
  longer speaks of another definition of the name when there is none ([CLI](docs/CLI.md#engine-b)).
- `grep` on a file with a NUL byte prints what ripgrep 15.2.0 prints on the same platform. A walked
  file is read as ripgrep reads it, so a match in its first 3 bytes, ripgrep's first read, prints
  with the stop warning. A named file is one slice where ripgrep maps it (Linux, Windows): a NUL in
  its first 64 KiB shows only the binary message, and a NUL ends no line. On macOS, where ripgrep
  reads a named file, so does scrutate ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- On Windows the paths scrutate prints or hands to another program have no `\\?\` in front: the
  index and glob roots it reports, `watch`'s root, `search --also`'s `root`, the binary and project
  paths `setup` writes, Engine A's paths (a share as `//server/share`, where it printed
  `//?/UNC/server/share`), and the paths it hands git and the Grep hook. The recent-files ranking
  of `find` keeps each root in that form and reads the form it kept before.
- `impact` and `review` run from a subdirectory read a relative `GIT_DIR`, `GIT_WORK_TREE`,
  `GIT_OBJECT_DIRECTORY` or `GIT_COMMON_DIR` from that directory, as git does: they started git at
  the root with the value unchanged, which found no repository or another one.
- `tsquery --rewrite --write` on Windows reads and writes through the root it opened and each
  directory opened from it, refusing a junction or link at every name: a directory swapped for a
  junction after the plan, the root included, sent the write outside `--root`
  ([Security](docs/SECURITY.md#the-structural-rewrite)).
- `callees` of PHP code that writes `new self()`, `new static()` or `new parent()` lists the
  class, where it listed `self`, `static` or `parent` as an external name
  ([CLI](docs/CLI.md#engine-b)).
- The language server's references under an import alias span the alias as the line writes it:
  `h()` after `from util import helper as h` is 1 character wide, where it took the width of
  `helper` and ran past the end of a short line ([CLI](docs/CLI.md#lsp)).
- `callers` lists a call the resolver could not place, tied by its name only (`tx.foo.update()`
  on an untyped parameter), as undecided also when no other definition has the name, where
  `callers update` listed it as decided and `callers ProviderProfile.update` as undecided;
  `impact` marks such callers undecided too ([CLI](docs/CLI.md#engine-b)).
- A name a Python class body binds, `h = 5` beside `from util import helper as h` (also in an
  `if`, a tuple or a `for` target), is a local of the class body once bound, where `refs helper`
  listed it as a use of the import; `h = h` still reads the import on its right side, as does a
  comprehension
  ([CLI](docs/CLI.md#engine-b)).
- `callers`, `callees` and `refs` follow a PHP alias written in another case: `PF()` after
  `use function Lib\phpfn as pf` is a call of `phpfn`, and `t::make()` after `use Lib\Tool as T`
  a use of `Tool`, where callers and refs missed them and callees said `external: PF` ([CLI](docs/CLI.md#engine-b)).
- `refs` through an import alias lists only uses of the aliased definition: a local of the
  alias's name (`const g = () => 2; g()` beside `import { jsfn as g }`) no longer adds its own
  group to `refs jsfn` ([CLI](docs/CLI.md#engine-b)).
- A Java class of the default package (a file with no `package` line) is looked for in its own
  directory only: `Util.help()` in `ex/b/Main.java` is `ex/b/Util.java`'s, where every
  default-package directory was one package and it could go to `ex/a/Util.java`; with no
  `Util.java` in the directory, `refs` and `callees` both answer `external`
  ([CLI](docs/CLI.md#engine-b)).
- A call on a PHP static property, `User::$current->save()`, goes through the property's declared
  type to `User::save`, and `current` there is `field or key`, where `save` was `receiver` and
  `current` a local variable ([CLI](docs/CLI.md#engine-b)).
- `$this->word()` in a PHP trait that has no `word` is `receiver` in `refs`, since the class using
  the trait decides, where it said `not found` ([CLI](docs/CLI.md#engine-b)).
- `this` in a JavaScript method under a quoted key (`"get": function () {}`, `"del"() {}`) is
  the object literal, so `this.put()` there is `api.put`, where it was `receiver`
  ([CLI](docs/CLI.md#engine-b)).
- A PHP variable a closure takes with `function () use ($u)`, or `use (&$u)`, has the type it
  has around the closure, so `$u->save()` inside is `User::save`, where it was `receiver`
  ([CLI](docs/CLI.md#engine-b)).
- `const x = require('./n').x`, `const { later } = await import('./lazy')` and TypeScript
  `import m = require('./n')` bind their names to the module's definitions, where `refs` read
  them as local variables or receivers ([CLI](docs/CLI.md#engine-b)).
- A JavaScript or TypeScript `super()` in a subclass constructor is a call of the base class:
  `callers Base` and `impact` list `Kid.constructor`, `refs Base` lists the `super()`, and
  `callees Kid.constructor` gives `Base` where it said `external: super` ([CLI](docs/CLI.md#engine-b)).
- A field of a Java anonymous class is what a use of its name inside the class reads, where a
  local of that name in the method around it hid the field and the field had no references
  ([CLI](docs/CLI.md#engine-b)).
- `Outer.this.tick()` in a Java inner class is a call of `Outer.tick`, where it was `receiver`
  and `dead` left `Outer.tick` undecided ([CLI](docs/CLI.md#engine-b)).
- A local variable of a Java instance initializer block (`{ int count = 3; count++; }` in a
  class or enum body) is a local there, where it and its uses were read as the field of that name
  ([CLI](docs/CLI.md#engine-b)).
- PHP class, interface, trait, enum, method, namespace and `use` alias names, and `self::`,
  `static::` and `parent::`, match in any case,
  as PHP matches them: `s::SLUG()` after `use App\Util\Str as S`, `\app\util\str::slug()` and
  `$x->Slug()` are calls of `Str::slug` in `refs`, `callers` and `dead`, where they were
  `external` or not found; variables, properties and constants still compare as written
  ([CLI](docs/CLI.md#engine-b)).
- `refs` finds a Go method promoted through an embedded struct field: `v.Hello()` on a `Wrap`
  that embeds `Base` (or `*Base`, `base.Base` from another package, also two levels down) is
  `Base.Hello`, where it said `not found` ([CLI](docs/CLI.md#engine-b)).
- `refs` ties a call of a Rust trait's default method on a type that implements the trait
  without overriding it (`s.hello()` after `impl Greet for S {}`, also `impl Greet for inner::S`
  and `impl super::Greet for S` inside `mod inner { }`) to that method, where it said
  `not found` ([CLI](docs/CLI.md#engine-b)).
- A Rust `use` of an item of an inline `mod other { }` reaches it: `use other::work` and the
  calls `work()` after it were `external` ([CLI](docs/CLI.md#engine-b)).
- `refs` lists a use under an import alias also in a file whose import never spells the
  definition's name: `h()` after `const h = require("./m")` or `import h from "./m"`, which
  `callers` already found ([CLI](docs/CLI.md#engine-b)).
- The name of a JavaScript generator expression (`function* load() {}`), class expression
  (`class Box {}` as a value) or named function expression is its own inside it, where uses there
  went to an outer `load` or `Box`, and in `var h = function load() { return load(); }` the inner
  call went to an outer `function load`, which `callers load` listed `h` for; a generator
  expression is also a scope for its parameters and locals ([CLI](docs/CLI.md#engine-b)).
- `refs` ties a JavaScript or TypeScript shorthand property, the `helper` in `{ helper }` or
  `module.exports = { helper }`, to the `helper` it reads, where it listed it as `field or key`.
- A Rust path inside a macro's arguments resolves as the same path outside a macro: with `use
  other::work`, the `util::work()` in `println!("{}", util::work())` or `vec![util::work()]` was
  read as the bare `work` and tied to `other::work` ([CLI](docs/CLI.md#engine-b)).
- `callers` and `callees` follow a CommonJS `module.exports = helper` or `module.exports =
  function named() {}` to the call `h()` of `const h = require("./m")`, and an ES default import
  `import h from "./m"` to the definition it reaches, also through a file that re-exports it
  with `const helper = require("./m"); module.exports = helper`: they matched the local name
  to the imported one, so `h` reached nothing ([CLI](docs/CLI.md#engine-b)).
- `dead` counts a Ruby writer (`def color=`) as used only where it is called (`x.color = 1`,
  `send(:color=, 1)`): reading `@color` kept a writer nothing calls out of the dead
  ([CLI](docs/CLI.md#engine-b)).
- A code index segment write that is stopped or fails removes its half-written directory, which
  stayed in `.scrutate` until a later write took its name ([Architecture](docs/ARCHITECTURE.md#storage-indexrs-lexrs)).
- A path that names the repository's root from a directory below it (`dead ..`, `map ..` from
  `repo/pkg`) is the whole repository: it was an error, "no such file or directory in the
  repository" ([CLI](docs/CLI.md#engine-b)).
- `grep -c -U` counts as ripgrep does when a match ends at the end of its lines (`b|\n$`,
  `a|b\n\B`, `-w a|b\n`): `$`, `\B` and word edges saw the end of the matched lines as the end
  of the text, and counted a match too many ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- The language server answers requests queued behind one that waited out the first build at
  once, so `shutdown` behind four of them comes after one 20-second wait, not four
  ([CLI](docs/CLI.md#lsp)).
- A `serve --watch` tool call no longer waits for the server's own update of a saved file, which
  may itself wait for another process's write lock: past 250 ms it answers from the last committed
  index, where it waited until the lock was free (37 s for a lock held 40 s)
  ([CLI](docs/CLI.md#the-index-directory)).
- Ctrl-C ends a wait for the text index's lock that another process holds, under `watch` and
  `grep --index --update` (exit 130): the wait went on up to 10 minutes
  ([CLI](docs/CLI.md#watch)).
- Ctrl-C stops a code index update while a large segment's vectors are clustered (`watch`, a
  cancelled `reindex`): it waited for the clustering to end, two minutes in a debug build of a
  20,000-file tree ([CLI](docs/CLI.md#watch)).
- A path that leaves the repository and comes back by its parts (`outline ../../repo/pkg/a.py`
  from `repo/pkg`, an MCP `path` of `../repo/pkg`) names the repository's file or directory:
  `outline` said the file had no symbols, `dead_code` and `repo_map` found nothing, and
  `grep_code` named files by the detour ([CLI](docs/CLI.md#answers-from-the-index-one-definition-for-every-front-end)).
- `setup --uninstall` keeps text added after the scrutate block on its own line: with a file that
  had no last line break, it joined that text to the line above the block
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- A `grep` option given twice takes its last value, as in ripgrep (`-m1 -m2`, `-i -i`,
  `-C1 -C3`): it was an error ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- The Arabic letter mark (U+061C) in a path or name is shown as `\u{61c}`, as the other bidi
  marks are: it was printed raw ([CLI](docs/CLI.md#conventions)).
- `grep --index` skips the file standard output writes to, as the walk does: with an earlier
  search's output in the text index, `grep --index foo > out.txt` searched its own output and grew
  it until the disk filled ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `dead` finds a use of a definition named with its owner (Pascal's `procedure TFoo.Tidy`, used
  as `o.Tidy`) and of a Ruby writer (`def label=`, used as `v.label = "x"`) by its own name: both
  were called dead while code used them. A method's declaration in its class and its body no
  longer count as uses of each other ([CLI](docs/CLI.md#engine-b)).
- Two `Repo`s of the Python module with different custom languages each work under their own
  root's tables: an update, a tool call (`impact` parses files itself) and `grep(types=)` could use
  the tables another `Repo` loaded last, keep chunks parsed with them or call a custom type
  unknown ([CLI](docs/CLI.md#custom-languages)).
- `grep -t cpp` takes `.h` files, as ripgrep's `cpp` type does, `-t objc` takes `.h` and `.m`, and `-t matlab` and
  `-t puppet` find `.m` and `.pp` files, where they found none: a type now includes the extensions its language shares with another, every file of them
  whatever its content, as ripgrep's types do ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `tsquery --rewrite --write` on Linux and macOS reads and writes through the root as it was
  opened when the plan started and each directory opened from it, refusing links: a directory
  swapped for a link between the read and the write, the root included, sent the write and its
  temporary file outside `--root`
  ([Security](docs/SECURITY.md#the-structural-rewrite)).
- The keyword tier (OCaml and F# without a grammar, unknown languages) names `let rec fact` as
  `fact`, finds `let inline square` and `let private helper`, no longer takes `let mutable x` for a
  function named `mutable`, and keeps a function named after a modifier word (`let data x`,
  `proc inner()`), and takes only OCaml's and F#'s own words after `let` for modifiers
  (`let data x y` binds `data`, `let inline (+.)` gives no definition named `inline`); the
  content revision goes up so an older index reads its files again
  ([Architecture](docs/ARCHITECTURE.md#storage-indexrs-lexrs)).
- `grep -z` on a compressed file named on the command line whose text has a NUL reports a
  binary match, as ripgrep does, where it reported nothing and exit 1
  ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- A custom language's extension is refused when a built-in language claims a file name that
  ends with it (`txt` for `CMakeLists.txt`): such a table took those files from the built-in
  language ([CLI](docs/CLI.md#custom-languages)).
- `check` reports a segment whose vectors are gone or not as wide as the model's, and an IVF row
  out of range, where it said OK or stopped with a bare error; a search finds a segment of the
  wrong width damaged and rebuilds it, where it panicked ([CLI](docs/CLI.md#engine-b)).
- A `SCRUTATE_SEMANTIC` model whose tables do not agree in size is refused with an error: it
  panicked while indexing, and every file stayed skipped after the model was fixed
  ([CLI](docs/CLI.md#environment-variables)).
- `watch` on a disk that ignores case (macOS's default) walks the tree after a rename that
  changes only the case of a name: the text index kept the old name beside the new one
  ([CLI](docs/CLI.md#watch)).
- A concise tool answer too long for `max_bytes` is the detailed page, which says how to go on:
  it was cut in the middle of an item and lost the line with the next offset, so items between
  pages were never shown ([CLI](docs/CLI.md#serve-mcp)).
- The language server counts columns as the editor does in a file with a byte order mark, so
  definition on its first line and its document symbols land on the name, and a definition that
  starts on the first or last line of another (`} function c() {`) is its sibling in the
  document symbols, not inside it
  ([CLI](docs/CLI.md#lsp)).
- `grep --json` spans and colour leave out the empty matches ripgrep leaves out: one right where
  a match ended and one past the line's end (`\s*$` on `foo  ` gives one span, not three), and an
  empty match inside a character stays empty instead of covering the character, at ripgrep's byte
  offset when the line is valid UTF-8 (`x*` on `é€x`: 0, 1, 2, 3, 4).
- Ctrl-C stops a `watch` update at its next batch of files, the first update of a large tree
  included, where it ran on to its end; a second Ctrl-C ends any command at once
  ([CLI](docs/CLI.md#watch)).
- A C++ header with a Doxygen `@class` (or `@interface`, `@protocol`) in a comment is parsed as
  C++: only a line that starts with an Objective-C keyword makes a `.h` file Objective-C; an index
  reads its files again.
- Text answers show bidi marks, embeddings, overrides and isolates and the line and paragraph
  separators in a path as escapes (`\u{202e}`), as they show control characters: a file named
  `bidi<U+202E>yp.txt` displayed as `biditxt.py` ([CLI](docs/CLI.md#serve-mcp)).
- `files` and `tsquery` with a `--root` that does not exist or is a file stop with "no such
  directory" or "not a directory" and exit 2, as the other commands do; they answered "no file
  matches" with exit 1.
- The definition patterns of every `[[language]]` table share one regex size budget, so a
  repository's `.scrutate.toml` with many large patterns is refused at once with the pattern's
  line; each pattern had the whole budget, and ten took 340 MB and three seconds in every command,
  `serve` and `lsp` included ([CLI](docs/CLI.md#custom-languages)).
- The index's write-ahead log stays small while readers overlap: after an update it is cut to
  nothing once readers on older commits finish (waited for about a second, else by the next
  update), and a cut log keeps at most 4 MB of its file. Under four MCP servers searching and a
  `watch` editing every second, the log stays small (its peak waits for a recorded run) ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- A Free Pascal unit in a `.pp` file that starts with compiler directives (`{$mode objfpc}{$H+}`,
  the Lazarus default) is parsed as Pascal, not as one Puppet module; an index reads its files
  again.
- The resolver takes a file's language as the parser does, from its text where the extension is
  shared: an Objective-C `.h` header no longer goes through the C rules, which could not place its
  calls and left `dead` asking about them in place of a C file's call.
- A `serve` reader left idle lets go of segments a write replaced, so their disk space is freed
  on Unix and they can be removed on Windows, where idle readers kept them open until reused.
- `setup claude` replacing an old entry through `claude mcp` adds the old one back when the new
  `add-json` fails, where the user was left with no scrutate server
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- `serve` reads a message of at most 256 MiB, as `lsp` does: a longer line, or one with no end,
  is dropped as it is read and answered with a parse error, where it was held in memory whole
  ([CLI](docs/CLI.md#serve-mcp)).
- `callers` and `refs` through a C# or C++ type whose bases form a wide diamond take linear time:
  each base type is read once per depth, as for Java, where 9 levels of 6 interfaces took 35 s for
  one call.
- `setup` makes its temporary file with the permissions of the file it replaces before writing to
  it: a `config.toml` only its owner may read was readable by others while it was written
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- A language server request during the first build waits for it up to 20 seconds, then answers
  -32801 "the index is still being built": it waited for the whole build on the one thread that
  reads requests, so `shutdown` and cancellations queued behind it ([CLI](docs/CLI.md#lsp)).
- Each search, tool call and language server request reads the code index from one commit: a
  `watch` that committed between its reads could drop a hit whose rows it had already picked
  ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- A failed write of `grep`'s output other than a closed pipe (a full disk, a file size limit)
  is an error with exit code 2: it stopped the search and exited 0 or 1 without a word. ripgrep
  exits 2 too once its output buffer fills; on a small answer it exits 0 without a word.
- `grep foo . > out.txt` skips `out.txt`, as ripgrep does: it searched its own output and grew
  it until the disk or a file size limit stopped it ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- A new segment's files and directory are flushed to disk before the database names them, and
  the text index's before its manifest does, so power lost after an update can no longer leave
  an index naming segments of empty or zeroed files that pass their shape checks; an update of 20 files takes about 60 ms longer on macOS
  ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- A Pascal statement that is a name alone (`DoWork;`, `Obj.Tidy;`) is a call edge, so `callers`,
  `callees` and `impact` see a procedure called without parentheses; an index reads its files
  again ([CLI](docs/CLI.md#engine-b)).
- `refs` of a name a Fortran, Pascal or Ada definition or a PHP function has finds its uses in any
  case in those languages' files: `refs DoWork` missed `dowork()`. Fortran needs a build with
  `--features extra-langs` ([CLI](docs/CLI.md#engine-b)).
- `grep --fuzzy-regex` reports where a match starts: the reversed pattern is run back from its
  end. The column, the `--json` spans and the colour took a lower bound (`zzz a12b` gave column 1
  for `a[0-9]*b`) ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `grep --fuzzy` and `--fuzzy-regex` choose a line's spans by one rule: the parts within N edits,
  the fewest edits first, then the leftmost, then the longest, none overlapping, each beginning
  and ending on a character matched or a word character substituted. An exact match is its own
  span (`--fuzzy=0 aa` on `aaaa`: two, as `rg -F`), a slip lights the whole word (`kmallod`) and a
  bracket beside it stays out (`kmallo(`); the first end found and the start before it lit text
  past N edits (`--fuzzy=2 b2日` on `a123 b`: ` b`) or part of a word (`abc` of `abcd`). Which
  lines match is unchanged, and finding them costs 2.7 to 10.6% fewer instructions on the Linux
  tree's `kernel/`; the spans of `--json` and colour cost up to 15.5% more there (`--fuzzy=2
  return`) ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `grep --fuzzy` reads a long line full of matches once: each match read on to the line's end
  again, so its work grew with the square of the matches (the instructions saved waits for a recorded run)
  ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- An internal error advises `scrutate index --force` only in a process that opened an index:
  `grep` without `--index`, `hook`, `setup` and the other commands without one now only ask for a
  report; with `--index` the text index counts.
- The MCP tools `repo_map` and `dead_code` take an absolute directory under the root, which
  gave no files or "no such file", and `repo_map` and `map` refuse a path that is not there;
  `grep_code` reads a path whose name holds a backslash (`x\ty`) as written when no file has the
  escape's reading. The tools also take a path spelled through the root as the server was given
  it (`--root <dir>/link`, macOS's `/tmp`). The command line, the language server and the tools
  turn paths into repository paths in one place ([CLI](docs/CLI.md#serve-mcp)).
- `index`, `watch`, `serve --watch` and `reindex` wait for another writer for up to 10 minutes,
  or until cancelled, and so do the first open of a new index, the rebuild of an index of an
  older schema and the merge of segments after an update: they gave up with "database is locked"
  after 30 s. Ctrl-C ends `watch` during such a wait. A `watch` burst whose update failed is tried
  again with a walk of the tree, with the next burst or after a pause of 1 s doubling up to 30 s,
  so its files are indexed with no further change ([CLI](docs/CLI.md#watch)).
- An open of the index that finds another process writing no longer takes the lock for damage:
  it replaced the database and deleted that process's work.
- `grep -c -U` no longer counts the empty match after each matched group's line end, at the end
  of a file with no final line end, or right where a match before it ended, as ripgrep: `^$` on
  `a`, a blank line and `b` counts 1, not 2, and `b?$` on the same text with no final line end 3,
  not 4.
- `grep -A` and `-B` win over `-C` in either order, as in ripgrep: `-C1 -A3` showed one line
  after each match, not three ([CLI](docs/CLI.md#engine-a-grep-alias-g)).
- `grep -i` with a class that matches nothing (`ab[^\s\S]`) finds nothing and exits 1, as
  ripgrep does; it stopped with an internal error.
- `dead`, `callers` and `callees` match a Fortran, Pascal or Ada name, and a PHP function outside a class,
  in any case, as those languages do: `call compute_total(y)` is a call of `Compute_Total`, and
  `FormatPrice(3)` of `formatPrice`, also in a Pascal unit of a `.pp` file, whose language its
  text decides; an Ada `Main` is an entry point. Fortran needs a build with `--features
  extra-langs` ([CLI](docs/CLI.md#engine-b)).
- `dead` no longer lists Ruby methods that Ruby calls itself (`method_missing`,
  `respond_to_missing?`, `coerce`, `to_str`, `inherited` and their kin) or operator methods
  (`<=>`) ([CLI](docs/CLI.md#engine-b)).
- `callers`, `callees`, `impact` and `refs` follow an import alias: with `from util import helper
  as h` or `const { lone: l } = require("./u")`, a call `h()` was no caller of `helper`, so
  `impact` found no callers and no tests; and through a package that re-exports the name
  (`from pkg import helper as h` with `pkg/__init__.py` importing `helper` from `.util`), where
  the alias was looked for only in imports naming `helper` or `util` ([CLI](docs/CLI.md#engine-b)).
- Several first opens of a new linked work tree at once (an editor starting its language server
  and MCP server) no longer delete each other's copied segments: the losing open removed the
  segments the winner's database listed, and `check` found the new index damaged.
- `setup --instructions` and `--uninstall` take a scrutate marker only as a whole line: a
  sentence quoting `<!-- scrutate:begin -->` got a second block, and uninstall then cut from that
  sentence to the block's end. A begin line with no end line stops setup, and `--uninstall`
  with or without `--instructions`, with no file changed
  ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- On Windows an index update that a virus scanner or the search indexer briefly blocked failed with
  "Access is denied" or "being used by another process" when it renamed a new segment or the text
  index manifest into place; the rename is now tried again for up to about two seconds.
- On Windows the indexes keep each file's status change time, as on Unix: a file edited back to
  its old size and modification time is read again by the next update
  instead of answering from its old chunks until `index --force`. The walk reads the change times
  with one more listing per directory ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- `watch` on Windows walks the tree when a burst overflows the watch buffer, now 64 KiB: before,
  its files waited for a later change 30 s after the last walk ([CLI](docs/CLI.md#watch)).
- `watch` on Linux checks the directories that got no inotify watch on a timer, so a change there
  is indexed within 1 to 30 s, and the burst no longer walks the tree while one has none; the
  message names `fs.inotify.max_user_watches` when that limit is the cause
  ([CLI](docs/CLI.md#watch)).
- A one-line file of thousands of nested functions indexes in far less time and memory (by how
  much waits for a recorded run): the definitions of a file share a text budget of 1 MiB plus 8 bytes per byte of the
  file, past which each keeps its first line. Normal files keep at most 5.1 times their size, so
  they index as before ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- On Unix `.scrutate/` is made with mode 0700, and an existing one is set to it by the next
  write: under umask 022 it was readable by every user, the text of files made private with
  `chmod 600` included ([SECURITY](docs/SECURITY.md)).
- In Rust, a call on the result of a method of a standard or outside type goes through a table
  of standard methods: `v.first().unwrap().ok()` on a `Vec<Report>` is `Report.ok`, and
  `file.metadata().ok()` is external, so `callers` and `dead` decide more; a value built by
  `Type::new()` with its type arguments unwritten stays undecided ([CLI](docs/CLI.md#refs)).
- A plain name in a Rust macro's arguments, outside the standard expression macros such as
  `println!` and `vec!`, is `macro argument` and undecided, no longer tied by name to a
  definition ([CLI](docs/CLI.md#refs)).
- A Verilog task call without parentheses (`h;`) and a VHDL entity or component instance are
  call edges, and `a(i)` of a VHDL signal, variable, constant or port the file declares is not;
  an index reads its files again ([CLI](docs/CLI.md#engine-b)).
- The Grep hook reads the `managed-settings.d` drop-ins and lets Grep run wherever managed policy
  it does not read exists, so an administrator's read rule is never bypassed
  ([SECURITY](docs/SECURITY.md)).
- `path` takes a function named like its file (`def main` in `main.py`) over the file's module
  chunk, as `callers` and `callees` do.
- A `tsquery` predicate outside every pattern's parentheses is an error: it matched every node,
  so `--rewrite --write` would have rewritten all of them.
- `grep -t TYPE` (and `-t !TYPE`, `SCRUTATE_GREP_TYPES`, the configuration file's `types`)
  matches after the ignore rules, as ripgrep's `--type` does: a gitignored file of the type is no
  longer searched, and `-g '*.txt' -t py` searches only the `.txt` files.
- VS Code's symbol, callers and references commands ask for the word under the cursor without
  what comes before a dot when the name with its dots finds nothing, as the language server
  does: on `norm` in `p.norm()` they asked only for `p.norm` and found nothing. R's `is.ready` is
  still asked whole, and a word is a name as Unicode's identifier rule reads one, as the server
  does: on `ß` in `größe(2)` they found no word, and on `हिंदी` they asked only for `ह`, as a
  vowel sign is not a letter.
- Every reader of names (the call graph, `callers`, `refs`, `dead`, the language server) takes a
  name's characters by one rule, Unicode's identifier rule (UAX #31), with the extra characters
  a language allows (`$` in JavaScript, Java and PHP, `'` in OCaml, F# and Haskell, `\` in a PHP
  qualified name). A call of a name that starts outside ASCII, such as `हिन्दी()`, was not
  recorded, so `callers` and `callees` missed it, and the language server cut `हिन्दी` at its
  virama. The same holds for definitions read line by line in languages without a grammar, GNU
  make's `$(call NAME)`, a C# file-scoped `namespace`, and the dotted names and literal words a
  `search` query is ranked by, and R's dotted names. The content revision goes up so an older index
  reads its files again ([Architecture](docs/ARCHITECTURE.md#storage-indexrs-lexrs)).
- The GitHub Action keeps a backslash in `args` as written: `grep -c \bTODO\b .` searched for
  `bTODOb` and `--root C:\repo` named `C:repo`, so a gate could pass on the wrong search.
- Python `grep(sort=True)` gives the CLI's `--sort` order (`a/b` before `a-b`), and with
  `max_results=N` the first N matches in it, the same on every run: it capped the search at
  whichever N matches came first and sorted those ([bindings](crates/scrutate-py/README.md)).
- Redaction takes an XML password element's whole text, a quote or a space in it: such a
  password was cut at the quote, its tail shown, or not redacted at all. Text a quote ends with no
  closing tag on its line is redacted as before, and a sentence (`Forgot your password?`) or an
  escaped `&lt;...&gt;` as an element's text is left as written.
- Redaction takes an unquoted secret that holds a `&` or `;` whole: `client_secret = ...&Rr` and
  `password=...;k3` were redacted only up to it. What starts another item still ends a value: a
  `name=` (`;Initial Catalog=`, `&amp;format=`), `&&`, a cookie's `HttpOnly` or `Secure`, or
  another command (`;echo done`).
- On macOS, a program `hook run` started ends within milliseconds of `hook run` being killed:
  at a high open-file limit (1048576) it ran on for seconds, as the watcher closed every
  descriptor number up to the limit before it waited; it now closes only those up to the highest
  one open plus 64.
- An unquoted password, secret or token with a symbol that holds a `:` or `,` is redacted
  whole: `password: ...@...:yZk3` came out unchanged. A `,` before a `name=` or `name:` still ends
  it, and code (a ternary, `@user.field`, a method chain ending in `!`, an image digest) and an
  address (`git@github.com:acme/secrets.git`, `vault@prod:8200`) are left as written.
  docs/SECURITY.md says what ends a value and what is not covered.
- Two `serve` processes of one root starting together leave one offering its socket to command
  line calls: each could remove the socket the other had just bound, and a start that found the
  other still starting gave up at once, so none offered it when the other could not; it now waits
  up to a second for the other and checks again.
- Redaction shows a value with `@`, `~` or `!` after a name that holds `password`, `secret` or
  `token` but is no secret: `tokenizer: bert-base@v2.1`, `token.cache.dir=~/.cache/tokens`,
  `secret_header = X-Secret-Header@v1` and `password_label = Enter@your@password` were redacted.
  The keyword run on into a different word (`tokenizer`, `secretary`), or followed by a part such
  as `label`, `header`, `hint`, `cache` or `path`, spares the value; every other name with the
  keyword (`password_prod`, `token_eu`, `secret_value`, `secretkey`, `tokenvalue`) still hides it.
- A private key's body under a public label (`PUBLIC KEY`, `CERTIFICATE`, `CERTIFICATE REQUEST`,
  `X509 CRL`) is redacted: any SEQUENCE of the right length was shown as public. A public key's
  body must now be a SubjectPublicKeyInfo or a PKCS#1 RSAPublicKey, and a certificate's, a
  request's or a CRL's a SEQUENCE of a SEQUENCE, a SEQUENCE and a BIT STRING. Any other body
  under those labels, a cut one or one without its END too, is marked line by line, its short
  last line included, and a body line's trailing `+` no longer drops it to the ordinary rules
  ([Security](docs/SECURITY.md)).
- On Windows the dependency cache of `--deps` and `scope: "deps"` stays under `%LOCALAPPDATA%`
  when `XDG_CACHE_HOME` is set: an `XDG_CACHE_HOME` from MSYS or Git Bash moved it to a place no
  check covered ([Security](docs/SECURITY.md#dependency-source)).
- A `serve` tool call after the repository root was deleted answers with an error the client can
  read (`no such directory`), and so does every call after it, with or without `--watch`. The
  reader whose index the failed rebuild had closed went back to the pool, and handing it back
  panicked (`the index was not reopened after a rebuild`), so the call never answered.
- A path in a Python `RECORD` written with backslashes (`pkg\..\..\x`) no longer reaches
  outside `site-packages` on Unix: its `..` parts passed the check as one name, so `--deps` and
  `scope: "deps"` read a file outside the repository and wrote over one outside the cache,
  emptying it when the two met. Separators are made `/` before the check, which drops the entry
  ([Security](docs/SECURITY.md#dependency-source)).
- The benchmark harness publishes no win it has not earned. An indexed run where scrutate answered
  a query differently fails its latency and throughput rows, and the update row is n/a when every
  rival rebuilds; the leaderboard page computes both from the result file, and prints a
  comparison row with no ratio (scrutate's own failed throughput) instead of crashing. A resumed
  leaderboard run keeps earlier rows only when the tool versions, machine, corpora and settings
  match. `built_from.sh` refuses a source tree with uncommitted changes. The indexed run checks
  its corpus against the manifest and puts the files it edits back when it ends, also on an error,
  Ctrl-C or SIGTERM (after a SIGKILL or a power loss the next run's corpus check refuses the
  edited tree), and takes an index build's peak memory from GNU time on Linux. `compare_tools.py` runs ripgrep without its
  config file, takes the tools in turn in each repetition, 30 runs by default, and agrees on the
  lines printed, not their count; BENCHMARKS.md counts a row faster or slower only past 30 runs
  with its 95% interval clear of 1, and leaves tools with another answer out of its CPU, memory
  and cold-cache tables ([Leaderboard](docs/LEADERBOARD.md), [Benchmarks](docs/BENCHMARKS.md)).
- docs/LANGUAGES.md's Calls and Imports columns are now checked by `cargo test`, as its tier text
  said: `crates/scrutate/tests/samples.rs` fails when a language that declares call edges or imports has a sample
  with none, and the 22 samples (two of them extra-langs) that lacked one now hold a call or an
  import. The Docstrings column is named Body docstrings, since every grammar keeps the comment
  above a definition as its documentation ([Languages](docs/LANGUAGES.md)).
- `refs`, and the `references` of the MCP server and the language server, find the uses in a
  file with a NUL byte past its first 4 KiB, which the index holds: the search read the file as
  `grep` does, which stops at the read that meets the NUL, so a small such file gave none, those
  before the NUL included, and no note. It now reads a file as the index does, to its end, so
  such a file no longer ends with a `stopped searching binary file` note; one with a NUL in its
  first 4 KiB is still binary and left out ([CLI](docs/CLI.md#refs)).
- `grep` takes ripgrep's long names `--after-context`, `--before-context` and `--word-regexp` for
  `--after`, `--before` and `--word`: they were refused, so an `rg` call the shell hook answered
  with them failed ([CLI](docs/CLI.md)).
- In the home directory or at a filesystem root with no index, a command that would build one
  fails with `scrutate: error: ...`, and with `--json` or `SCRUTATE_DIAGNOSTICS=json` an error
  record; it printed a `[scrutate]` line with no record.
- An invalid pattern's error quotes at most 200 characters of it, as other errors quote an
  argument; `grep`, `grep --index`, `--fuzzy-regex` and `grep_code` quoted it whole.
- `tsquery` under `--max-bytes` counts its `(stopped at N matches ...)` line in the budget and
  prints it on the last page only; it was added after the page, past the budget, beside the
  page's own `more not shown` line.
- A repository's own `.scrutate.toml` is read for its settings only as it is for its languages: a
  regular file inside the root, at most 1 MB, through a link only to a file inside the root. One
  outside those bounds stops the command with a line naming it; it was read anyway, so its
  settings applied and TOML that did not parse stopped every command ([Security](docs/SECURITY.md)).
- A symbolic link to the index directory that a search does not follow gets no stderr note, and
  `check` does not list it; the note called it a hard link, and `rg` prints nothing there.
- `--no-color`'s help says an empty `NO_COLOR` turns colour off too, as it does.
- `hook stats` no longer shows a negative saving for a search `hook run` cut at the byte bound:
  such a search counts only the bytes read before the program was ended, so its row now has
  `"cut": true`, is counted apart (`cut_calls`, `of which cut at the byte bound: N`) and saves at
  least 0. `hook run`'s closing line counts only the redactions in the lines sent; it counted
  the line that did not fit too ([CLI](docs/CLI.md#setup-and-the-grep-hook)).
- `hook run` no longer lets a secret out whole across the 1 MiB cut of a long line: each piece
  was redacted on its own, so a token crossing the cut matched no rule and both halves went out
  back to back. A piece is now cut at least 64 KiB before its end and 4 KiB before any match of
  a rule near the cut, and the rest is redacted with the next piece; a match crossing the cut that
  runs past 4 MiB goes out as its marker with the rest of its line withheld. The key-line read
  back of `hook run` and the Grep hook opens only a regular file: a printed line naming
  `/dev/zero` spun forever and one naming a FIFO blocked ([Security](docs/SECURITY.md)).
- A Rust `#[path = "impls/h.rs"] mod helper;` names `src/impls/h.rs`: `helper::compute()` in the
  declaring file resolves there, `deps` shows the file and `rdeps` of it lists the declaration.
  The attribute was not read, so the call was unresolved and `callees` listed every `compute` in
  the crate ([CLI](docs/CLI.md#refs)).
- `setup --help` names the hooks `--hook` adds: those that answer the agent's search tools with
  scrutate, Grep and Bash for Claude Code and Bash for Codex. It named only Claude Code's Grep
  hook.
- A `grep --json` record whose text was redacted gives its `column` in bytes of the redacted
  text: it kept the column of the line as the file has it, so a match after a secret pointed
  inside the marker. A match inside a secret points at its marker ([CLI](docs/CLI.md)).
- A one-file `index --text` no longer writes a large part of the text index again every third
  update. With more than four segments only the largest stayed out of the merge, so a build of
  more than one batch had its second segment rewritten with the small ones each time. The merge
  now takes the smallest segments, and a larger one only once those taken reach a quarter of its
  size, so one-file updates write far less (how much waits for a recorded run) ([Architecture](docs/ARCHITECTURE.md)).
- Go: a file of an external test package (`package a_test` in `a/`) is no longer part of package
  `a`, and a `_test.go` file is seen only from the package's own test files, and through an import
  only from the package's external test (`export_test.go`); a method a `_test.go` file of the
  package declares is still found by its receiver type. A call
  in `a/use.go`, or `a.Exported()` from another package, tied to a namesake in `a/a_test.go`, the
  first file of the directory by name, so `refs`, `callers`, `impact` and `dead` missed the real
  callers ([CLI](docs/CLI.md#refs)).
- `grep -z` on an archive of more than a million members warns and exits 2, as the 8 GiB stop
  does: it stopped there in silence and exited 1. Rust `self::` in an inline module written on
  the same line as the module around it is that module: the resolver took the first module of
  the line. PHP `NEW self()`, `NEW static()` and `NEW parent()` are uses of the constructor in
  `refs`, as `new self()` was. In a custom language, no line inside a block comment defines,
  even one that does not start with the opener ([CLI](docs/CLI.md#custom-languages)).
- That read back opens the file without waiting (`O_NONBLOCK`) and then checks the handle, so a
  FIFO put in place of the file between the check of its path and the open no longer blocks it.
- A GitHub, GitLab, Slack, PyPI or Twilio token glued to a word character (`qghp_...`, as in
  minified code) is redacted, as gitleaks marks it: these rules had a word boundary their
  gitleaks source does not have ([Security](docs/SECURITY.md)).
- docs/SECURITY.md and README name the private reporting channel: the repository's Security tab,
  "Report a vulnerability".
- `review --format gitlab` gives overloads that share a qualified name (Java's `f(int)` and
  `f(String)`) a fingerprint each: they shared one, and GitLab, which takes fingerprints as
  unique, showed one of the two issues. A finding no other shares keeps its fingerprint
  ([CLI](docs/CLI.md)).
- A file whose name holds a secret no longer reaches an agent through what names files beside an
  answer: the MCP tools redact the note and `pending` list of files an update has not reached yet
  and their error text, and `lsp` its errors and log messages, as they redact answers.
  `SCRUTATE_REDACT=on` redacts the hidden `sexp` command's output too. `hook run` refuses an `rg`
  with `--max-columns-preview`, which prints part of a line and may cut a secret where no rule
  finds it. `npm-token` takes its prefix in any case, as gitleaks does
  ([Security](docs/SECURITY.md#secrets-in-answers)).
- A watched session's reads keep their change set when the directory above the root changes a
  lot (`/tmp` or the home directory, watched for its `.ignore` and `.rgignore`): each other entry
  made or removed there counted as a change, so busy siblings filled the 10,000-event bound and
  every read walked the tree ([Architecture](docs/ARCHITECTURE.md)).
- On Windows with `XDG_STATE_HOME` set, the Grep hook's counter (`hook-grep.jsonl`) goes there,
  beside `frecency.json`: it went under `%LOCALAPPDATA%` alone, so the two state files were in two
  directories. Both now take one rule: `XDG_STATE_HOME` when it is an absolute path, else
  `~/.local/state` (on Windows `%LOCALAPPDATA%`) ([Install](docs/INSTALL.md)).
- `map` and the MCP repo map no longer build an edge per calling file and defining file for a
  name defined in more than 32 files: with 3,000 files defining `get` and 3,000 calling it they
  built 9 million edges. Such a name's calls are now summed once per caller and once per definer,
  with the same scores but for rounding in the last digits of a `--json` score
  ([CLI](docs/CLI.md#engine-b)).
- `callers --json` and the MCP `callers` result mark each caller the resolver could not decide
  `"undecided": true`, as `impact` does; the JSON dropped the count the text form prints, so a
  reader took guesses for answers. In Rust, `self.name` read as a field is no use of a method
  `name` (Rust names a method only to call it), and `(*self).m()` in an impl for a reference,
  `Box`, `Rc` or `Arc` stays undecided: both were tied to the method of the enclosing impl
  (ripgrep's `WalkBuilder.follow_links`, `Matcher.shortest_match_at`, `Sink.binary_data`)
  ([CLI](docs/CLI.md#refs)).
- A `.scrutate.toml` in a subdirectory that links to a file elsewhere in the repository is read
  again: its link was held to the subdirectory, so every command run there stopped with "not
  read". The bound is the repository, the nearest directory up holding `.git` or an index; a link
  out of it still stops the command. One with a NUL byte in its first 4 KiB is now refused as
  not text, where the message said it was out of bounds.
- `grep --fuzzy` and `--fuzzy-regex` with `--json` or colour keep where each character of a
  matched line starts as one bit per byte, not eight bytes per character, so a long line takes
  less memory at its peak with the same spans (how much waits for a recorded run).
- docs/INSTALL.md's platform table names the abi3t wheel (`cp315-abi3.abi3t`, CPython 3.15 and
  free-threaded 3.15t) on every platform, as `release.yml` builds it and CI tests it (run
  44069673980); `tools/gen_doc_assets.py --check` now fails a row that leaves out a wheel kind the
  release builds. `bench/musl_vs_glibc.py` gave a worst musl/glibc ratio of x1.00 when musl was
  faster on every search; it gives the largest ratio measured. `bench/musl_vs_glibc.sh` measures
  the musl build against the glibc one on idle x86_64 hardware by the leaderboard's method; its
  first result is pending ([Install](docs/INSTALL.md)).
- In a custom language, a block comment opener after the line comment mark (`// matches
  lib/*.rs`) no longer opens a block that hides every later definition. `grep -z`'s million-member
  cap on a tar counts members, not their PAX or long-name headers: a tar of 600,000 files with a
  PAX header each stopped as "more than 1000000 members" ([CLI](docs/CLI.md#custom-languages)).
- A link planted in the state directory is no longer written through: `frecency.json`'s temporary
  file had a name anyone could guess (the process id) and was opened through whatever was there,
  and the Grep hook's counter was appended to through a link at its path. The temporary file now
  has a random name and is created only where nothing is, the counter is opened without following
  a link (`O_NOFOLLOW`; on Windows a link is refused), and a state directory another user owns is
  not used while one others may write to is closed to them first, with one stderr line
  ([Security](docs/SECURITY.md#threat-model)).
- On Windows with `LOCALAPPDATA` unset, the dependency cache is under
  `%USERPROFILE%\AppData\Local\scrutate`, where `LOCALAPPDATA` points by default, and with no
  `USERPROFILE` either it is an error naming `SCRUTATE_CACHE_DIR`; it fell back to `HOME`, which
  MSYS and Git Bash set ([Security](docs/SECURITY.md)).
- `--deps` and `scope: "deps"` read a lockfile, `Cargo.toml`, `go.mod`, a package's
  `package.json` and a distribution's `RECORD` only as a regular file, never through a link, of at
  most 64 MiB (1 MB for the manifests); a lockfile or manifest that is a link or larger is refused
  by name. They were read with no size limit and through a link, so a planted `Cargo.lock` could
  point outside the repository or be gigabytes long ([Security](docs/SECURITY.md)).
- `setup claude` runs `claude` only from an absolute `PATH` entry outside the current directory and
  its work tree, as `hook run` takes its programs: it took the first one on `PATH`, so with `.` or a
  directory of the repository there it ran a `claude` the repository held ([Security](docs/SECURITY.md)).
- Rust: a `#[path]` mod inside an inline `mod` block no longer takes over a top-level `mod` of
  the same name, so `foo::bar()` at the top of `lib.rs` stays in `src/foo.rs`. A file a `#[path]`
  loads finds its own `mod fd;` beside it, as rustc does, in `refs`, `deps` and `rdeps`. In
  modules written on one line, `self::` in `a::b` is `a::b`, not an `x::a::b` before it
  ([CLI](docs/CLI.md#refs)).
- `grep` on a terminal writes each match of standard input as it is found, and each file's lines
  as soon as that file is done, as ripgrep does: matches read from a pipe (`tail -f log |
  scrutate grep ERROR`) waited for 512 of them or the end of the input, and plain output for 64
  KiB or a millisecond's hold. Lines of files searched at once stay together by file. A pipe or a
  file still gets its lines in batches ([CLI](docs/CLI.md#conventions)).
- `grep -U -c --max-results N` stops at N matches: a merged group counted once against the cap but
  added all its matches to the file's count (`--max-results 1` printed 3 for three matches), and
  `--stats` and the JSON summary counted the groups. Each match now counts in all three, as
  `--sort` and ripgrep count it.
- `.scrutate/model.sig`, the cached hash of a semantic model file, is removed and made anew as
  the index's other files of fixed names are, so it is never written through a link. The MCP
  server gives a JSON-RPC batch a thread of its own only while fewer than 32 batches have one, and
  runs it on the reading thread past that: each batch line had its own thread, so 200 one-call
  batches during the first build took 304 threads ([Security](docs/SECURITY.md)).

## [0.1.0] - 2026-11-11

First release. How each part works is in [ARCHITECTURE](docs/ARCHITECTURE.md), the flags and
outputs in [CLI](docs/CLI.md), the measurements in [BENCHMARKS](docs/BENCHMARKS.md) and
[LEADERBOARD](docs/LEADERBOARD.md).

### Engine A: universal instant search
- Literal, regex (linear time, size-limited), identifier (all casings, digit/acronym aware),
  fuzzy/approximate (Myers bit-parallel, pigeonhole prefilter), multiline, inverted, whole-word,
  smart-case matching; context lines; `-c`, `-l`, `--max-count`, `--max-results`.
- Parallel ignore-aware traversal (`.gitignore`, `.ignore`, global excludes, hidden, symlinks,
  depth and size limits, type globs); per-file batched streaming; Ctrl-C and broken-pipe
  cancellation; deterministic `--sort`; JSONL output (schema 1); exit codes 0/1/2/130.
- Differentially tested against ripgrep on random corpora and patterns: zero disagreements.
- Text not on disk can be searched (`Paths::Memory`), with the output a file of the same bytes
  gives; one module (`source.rs`) reads every kind of source.
- One scheduler with one module of thread rules runs every walk and file list; a list of more
  than 256 files takes its share of the searches running on macOS, and any search adds threads
  when its reads come from the disk.
- `-e PATTERN` (repeatable) for several patterns (`-e '->'` works); each line of a multi-line
  match gets its own line number, and in colour its path.
- `grep -i` and `grep -F -i` with a literal use 37 to 50% less CPU on one thread (a 740 MB file:
  100.6 to 63.3 ms of user time for `pm_resume`, 96.2 to 48.4 ms for `KÖNIG`).
- Search threads format plain output into per-file buffers; lines over 64 KB are written
  uncopied, their UTF-8 checked 256 KB at a time.
- Ctrl-C is handled through `signal()` / `SetConsoleCtrlHandler` directly, saving the `ctrlc`
  crate's start-up cost on macOS.
- Gitignore lines of three common shapes (`*.log`, `**/a/b`, `/build*`) match without building a
  regex, which cost more than a small repository's whole walk.
- Plain output arriving within a millisecond of other output waits in a 64 KB buffer and leaves
  in one write: `return` in the Linux tree (88,000 files) went from 302 ms to 190 ms on Linux
  (ripgrep 319 ms), 675 ms to 650 ms on macOS.
- Start-up on macOS on Apple silicon is 1.9 ms (a Rust hello-world 1.5 ms): system functions a
  search never calls are bound lazily, and a grep-only argument parser skips clap.
- Faster start: a one-file `scrutate grep` uses 2 M fewer instructions, 131 fewer page reclaims
  and 2.2 MB less memory; `-F` with one literal skips the regex engine, and the Apple silicon
  executable is linked with an order file (`crates/scrutate/startup.order`, from
  `tools/order_file.py`).
- macOS builds need macOS 13 or newer (chained fixups).
- A `.gitignore` used on many paths compiles its globs into one set: the Linux tree in a Linux VM,
  144 ms to 120 ms (ripgrep: 135 ms).
- Smart case counts only letters that stand for themselves (`-S '\Sfoo'` is case-insensitive, as
  in ripgrep) and folds only a pattern with a literal and no uppercase letter; of `-i` and `-S`
  the later wins, and `SCRUTATE_GREP_SMART_CASE` applies when neither is given. `-c` with
  `--max-count` stops at the limit; `-w` with `--fuzzy` or `--fuzzy-regex` is an error.
- A file of 64 MB or more is searched on every search thread as ranges of whole lines, output in
  file order.
- A regex whose matches all end in one of a few literals (`\w+_(suspend|resume)\(`) finds
  candidate lines by those literals.
- Small roots are walked on the calling thread and a one-file search starts no thread: 21.1 M to
  19.2 M instructions (GNU grep in the C locale: 20.8 M).
- A large file with no line breaks is read into one buffer, each search thread filling its own
  part: 100 MB in one line in 4.0 ms on five threads, 12.6 ms on one; `^(a|aa)+$` over it and two
  more files, 15.3 ms to 11.0 ms (ugrep 16.5 ms).
- `-P` / `--pcre2`: PCRE2 syntax (look-around, backreferences), stopped by PCRE2's match limit
  on exponential patterns.
- `-z` / `--search-zip`: zip and tar members (`archive.zip{member}`) and compressed files, with a
  64 MiB cap on one expanded line and escaped member names ([SECURITY](docs/SECURITY.md)).
- `--fuzzy` patterns up to 4096 characters, an edit being one character (`naïve` is one edit from
  `naive`); `grep --index --fuzzy` agrees with the walk. `--fuzzy-regex[=N]`: a regex within N
  edits.
- `grep --index --update`: refresh the text index (changed files only), then search it.
- Files that could not be searched, or not to the end, are named in warnings.
- `^` and `$` are line anchors in every mode, as in ripgrep: `^(a|aa)+$` over ten million one-byte
  lines took 198 ms, now 8 ms.
- A regex whose every match starts a line with one of a few literals looks only at those line
  starts: 52 ms to 18 ms.
- On macOS a many-file search runs one thread per performance core, and a walk whose reads come
  from the disk adds threads up to 24: the Linux tree with a cold cache, 2.9 s on 5 threads,
  1.4 s this way, ripgrep 1.8 s.
- `scrutate grep` uses less memory on big trees (peak resident memory: the ripgrep tree 9.9 MB to
  5.9 MB, py-big 12.6 MB to 8.2 MB, the Linux tree 14.3 MB to 9.6 MB; ripgrep: 7.1, 8.5 and
  14.2 MB).
- On macOS, concurrent searches of one user share the threads, since the kernel serves about four
  file-reading threads at full speed; `SCRUTATE_GREP_DEBUG=1` shows the count.
- On Linux a tree read from the disk gets up to 64 threads: the Linux tree cold in a 5-CPU VM,
  1.22 s on 5 threads, 0.37 s this way (git grep 1.23 s, ugrep 0.64 s); 231 files cold: 5.8 ms
  (ripgrep 7.5 ms).
- Paths print as given, as in grep and ripgrep (`./a.txt` for `.`); with one file as the only
  path, lines leave its name out; `-H` and `-I` choose.
- On Unix a backslash in a file name stays in the printed path; only Windows turns `\` into `/`
  and prints `\\?\C:\x` as `C:/x`.
- On Linux, files and directories on `/proc`, `/sys` and the other kernel pseudo filesystems are
  read to the end.
- A `-g` glob is taken whole, commas included (`-g '*.{txt,log}'`); `SCRUTATE_GREP_GLOBS` and
  the configuration file's `globs` split at commas outside braces.
- With `-A`, `-B` or `-C`, a line `--` separates groups, as in ripgrep.
- `-c` with `-U` counts as ripgrep does.
- A pattern or option value that is not UTF-8 is an error (a path may be any bytes).
- `ignore` 0.4.34 is vendored (`vendor/ignore`, the `scrutate-ignore` crate) with the eleven
  changes `vendor/ignore/README.md` lists; an idle walker thread first waits 10 µs instead of 1 ms (a 598-file package:
  10.7 ms to 8.1 ms).
- `--max-results N` stops at exactly N matching lines; `-m N` is a per-file limit.
- A search path that does not exist is a warning and exit code 2; the other paths are searched.
- With no path, a pipe or file on standard input is searched (`-` names it; label `<stdin>`).
- A file with a NUL byte is searched as ripgrep does: named on the command line, line output
  prints `binary file matches (found "\0" byte around offset N)` (a `binary` record with
  `--json`). A walked file with a NUL byte is left out of `-c` and `-l`.
- `-c` and `-l` give one record per path given, as ripgrep does.
- `--sort` prints in the order of `rg --sort path`, also with `--max-results N`.
- `-n` is accepted and `-N` drops line numbers; `-j`, `SCRUTATE_THREADS` and `threads` stop at
  256.
- `grep`, the code index, `watch` and `tsquery` read `.rgignore` files, as ripgrep does.
- `--type-list` and `--version` need no pattern; a bad `.scrutate.toml` no longer stops
  `--help`, `--version`, `config` and `completions`.
- On Windows a file that falls back to the stream reader is searched from its first line.
- Colour or `--json` output of a file with millions of matches holds megabytes (one 20 MB file
  with 2 million matches: 341 MB, now 12 MB).
- In `--json`, `spans` and `column` count bytes of the printed `text`, as do the Python bindings'
  `Match.spans` and `Match.column`.
- On Unix a non-UTF-8 file name prints as its own bytes in piped output, and `--json` adds
  `path_bytes`; control characters in names print as escapes in the index commands, the MCP
  tools and `scrutate grep` on a terminal, and as they are in piped `scrutate grep`
  ([CLI](docs/CLI.md#conventions)).
- With `-l`, `--stats` says `N of M files match` and the JSON summary gives `"matches": null`.
- The MCP `grep_code` tool and `grep --symbols` end a file whose search a NUL byte stopped with a
  `note:` line (it was printed as line 18446744073709551615, or dropped), and `refs` and
  `references` say so too (`notes` in JSON).
- A cancelled `grep_code` or `references` call stops its Engine A search between files.
- `references` and `refs` search the files `grep_code` searches: hidden files included,
  `node_modules` and the like left out.
- Engine A has one way in, `instant::search`; the Python bindings' `grep` no longer yields a
  binary-file note as a `Match`.

### Engine B: code intelligence
- `scrutate map [PATH]` and the MCP tool `repo_map`: the files that define code, ranked by
  PageRank over calls and imports, each with its 5 most called definitions.
  `scrutate path FROM TO [--depth N]` and the MCP tool `call_path`: the shortest call chain
  between two definitions; no chain exits 1 ([CLI](docs/CLI.md)).
- `scrutate dead [PATH]` and the MCP tool `dead_code` list the definitions nothing in the
  repository uses, leaving out each language's entry points (`main`, tests, `#[no_mangle]`,
  Python `__x__` names, exports and the like); a name some line holds that the resolver ties to
  nothing is listed as undecided, with the reason.
- GDScript (`.gd`) has a grammar in the default build (`tree-sitter-gdscript` 6.1.0, MIT, 0.27 MB
  of the binary): functions, classes, signals, enums, constants and variables, `##` comments as
  documentation, a file with `class_name Player` as that class, and calls (`f()`, `obj.f()`,
  `super.f()`) for `callers` and `callees`; `grep --type gdscript` searches `.gd` files
  ([LANGUAGES](docs/LANGUAGES.md)).
- `scrutate impact [--base REV] [--staged] [--depth N]` and the MCP tool `impact`: from the git
  diff to the definitions it adds, edits and deletes, their callers with distances (3 steps by
  default) and the test files to run; a bad revision is exit code 2 ([CLI](docs/CLI.md)).
- `scrutate files QUERY [--limit N]` and the MCP tool `find_files`: the repository's paths ranked
  for a query that may be misspelt (`srch.rs` finds `search.rs`), then by frecency; no index
  needed, about 8 ms on this repository.
- Windows: a damaged index is rebuilt while another handle holds its files (the rebuild could
  fail on a segment it had deleted), and `grep_code` and textual references print paths from the
  root, not absolute paths.
- A linked work tree no longer takes a segment its main checkout removed during the copy, seen
  once on a Linux runner as an update that reported success over a broken index.
- `callers` lists callers in path and line order; an index updated through renames answered 63
  of 120 `callers` questions differently from a fresh one.
- `callers` asks the resolver about every call site (it stopped at the 400 likeliest), and
  `callers` and `callees` keep what they learned under `.scrutate/answers/` while nothing they
  read changes: `callers size` on Guava costs 1.4 s of CPU on a first run and 0.43 s after (the
  capped answer cost 0.6 s). The bindings' callers list has no `unchecked` any more.
- What parsing makes of a file is an artifact kept by content (`artifacts.rs`), so a parsed and a
  stored file give the same index and vectors for another model need no parse. Schema 14 rebuilds
  older indexes once. The store adds 7.5% to `.scrutate/` here and 9.5% on Guava; a fresh build
  costs 18% less on Guava ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- The `tsquery` cache names files by their paths from the root, so a copied tree answers from
  the first tree's cache.
- A look for changes walks the tree once for both indexes, and `grep_code` and `references` read
  the text index for the files it holds as they are now: a rare word on 12,001 files, 405 ms of
  CPU to 90 ms; resolved `refs` of a rare name on 12,000 files, 0.54 s to 0.05 s.
- One session module (`session.rs`) holds a repository open for the MCP server, the language
  server, the bindings and `watch`, with reads side by side on up to 8 readers: eight
  `references` calls sent together no longer queue ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- The resolver is one walker plus a rule set per language (`resolve/`); no answer changed.
- `refs`, `callers`, `callees`, `deps`, `rdeps` and the language server resolve PHP
  (`resolve/php.rs`): namespaces and `use`, classes, traits, `$x->m()` through the type of `$x`,
  `self::`, `static::`, `parent::` and `C::m()` ([CLI](docs/CLI.md)).
- `search` and the ranked part of `find` use a running `serve` session too, with the same bytes
  and exit code as a fresh process in about half the time or less.
- The models are trained on rows the engine writes (`bench/gen_features.py`, `bench/train_semantic.py`);
  on six packages the rows equal the reference's bit for bit (7,836 queries, 1,171,940
  candidates).
- The benchmark builders (`bench/make_bench.py`, `bench/make_bench_code.py`, `bench/quality.py
  --make`) and the ranker's trainer read the engine too: the hidden `views` command. Before the
  reference engine was removed they wrote the same files byte for byte on 13
  packages (1,877 files, 12,039 queries) and the same ranker ([QUALITY](docs/QUALITY.md)).
- `batch --features` names the features (`feature_names`); no feature, score or ranking changed.
- The index keeps its tables to itself: other modules ask `Index`; answers are unchanged.
- The language server's go to definition names the definition the resolver picks: on
  `item.total()` after `item = Order()`, with a class `Item` in the file, it answered
  `Item.total` and now `Order.total`.
- One module decides what a name refers to (`names.rs`); a `callers` request reads each file once
  (450 call sites in one file: 33 ms CPU to 19 ms).
- The models are one set (`models.rs`) loaded on first use: `symbol` on this crate's index takes
  6.8 ms CPU (25.8 ms before), a ranked search 20.5 ms (30.2 ms).
- A change of vector model remakes the vectors without rebuilding the index: to
  `SCRUTATE_SEMANTIC=none` 22 ms CPU instead of 507 ms, and back 1.39 s instead of 3.47 s here,
  2.90 s instead of 8.34 s on Guava. A ranker whose features differ makes `search` fail saying
  so; before, it was silently dropped.
- Incremental index (SQLite + memory-mapped segments), content hashing, atomic updates,
  compaction, GC, corruption detection and self-healing rebuild, schema/model versioning,
  multi-process safety, `check` command.
- 61 tree-sitter grammars in the default build and 13 more with `--features extra-langs`, each
  verified by a snapshot sample (writing the optional ones' samples fixed the D, LLVM IR,
  AutoHotkey, VHDL and OCaml rules); keyword heuristics for grammar-less languages, windows for
  everything else; imports and call edges ([LANGUAGES](docs/LANGUAGES.md)).
- Text index for repeated grep-style queries (`scrutate index --text`, `grep --index`): trigram
  postings whose output equals the walk's; incremental and safe for concurrent readers.
- UTF-16 files with a byte-order mark are transcoded and indexed (they were skipped as binary).
- Text index format 2 adds position masks to the postings, so fewer files are candidates for a
  string of four bytes or more; concurrent throughput against Zoekt on the Linux tree went from
  3% behind to 5% ahead, and the index from 154 MB to 428 MB
  (`bench/results/macos-arm64/superseded/indexed-eeab.json` and `indexed-fb5c.json`).
- Chunk line numbers follow grep's line counting (`\n` only), found by fuzzing. Index schema 12.
- Ranked search: field-weighted BM25 + exact lookup + semantic vectors (IVF) + LightGBM
  LambdaRank ranker; bit-identical with the Python reference implementation.
- The Rust engine and the Python reference cluster a large segment (20,000 chunks or more)
  bit for bit the same, checked by `bench/parity.py` and `bench/parity_repos.sh`.
- The ranker is trained with documented types as answers too, and a feature tells a name typed
  with its case (`BurstFiles`) from the same words in another case: a type is found from its
  documentation with MRR 0.478 (0.017 before), functions lose 0.031 ([QUALITY](docs/QUALITY.md)).
  `bench/retrain_ranker.sh` refuses a ranker that does worse; `bench/eval_sealed.sh` keeps a
  sealed hold-out.
- Ranked search reads every line of an indexed file: code outside definitions is in 50-line
  windows, and a definition starts at its attribute lines (`#[get("/users")]` above a handler is
  found with it).
- Lock files with an ordinary extension (`package-lock.json`, `pnpm-lock.yaml` and the like) are
  left out of the index like `*.lock`; `grep` still searches them ([CLI](docs/CLI.md)).
- Less memory in `grep -j1` (141 MB to 103 MB for a 100 MB line), and faster (the Linux tree
  2.44 s to 2.36 s, py-big 132 ms to 126 ms); the default mode on one 740 MB file holds 6.1 MB,
  down from 14.3 MB.
- `refs`, `deps` and `rdeps`:
  - `rdeps` of a module with thousands of importers stays small (3.8 GB to 103 MB for 5,000
    importers).
  - A relative import that climbs above the root (`../../lib` from `src/a.js`, `from ...x` in
    `src/b.py`) resolves to no file.
  - `refs` places uses right after a byte-order mark or an invalid UTF-8 byte.
  - A file past 2,000 levels of nesting is listed as too deep to read, where `refs` overflowed the
    stack.
  - The resolver no longer panics on a file that starts with a byte-order mark and holds a NUL
    byte.
  - `refs` through a typed variable takes linear time: 5,000 uses from 23 to 32 s to under
    0.25 s.
  - A Java class in front of a call (`Util.foo()`) is a use of the class: every static call on a
    class was "not found".
  - Rust binaries, tests, examples, benches and `build.rs` are crate roots (`crate::` in
    `src/bin/tool.rs` starts at that file).
  - A variable's type comes from the declaration in scope at the use.
  - A Java diamond of interfaces no longer takes exponential time (over 100 s to 0.04 s at 57
    interfaces).
  - A tsconfig `extends` written as a list is read.
  - An inherited method is the one the method resolution order reaches first, up to 8 levels:
    `class D(B, C)` calls `C.m` where it named `A.m`, and a chain of more than 3 classes no
    longer answers not found.
  - A variable inside a closure has the type of its enclosing declaration
    (`xs.map(x => p.norm())` calls `Point.norm`), unless the closure binds the name
    (`xs.map(p => p.norm())`, a bare `p => ...`).
  - A Python base class from outside the repository keeps its place in the method resolution
    order: `class T(unittest.TestCase, Helper)` no longer calls `Helper.setUp` for
    `self.setUp()`, which stays undecided because `TestCase` may define it (`object` is not
    counted).
  - tsconfig `paths` and `${configDir}` are read as TypeScript 5.5 reads them.
  - `refs` and `callers` stay fast where many blocks bind one name (2,000 blocks: 11 to 13 s
    before, 0.14 s).
  - `refs` counts every use on a line (it stopped at 64); `--max-results N` counts uses.
  - `refs` in one large function takes linear time: 4,000 calls from 28 s to 0.12 s.
  - A use on a line longer than 400 bytes shows the part around it: 16 MB of output to under
    1 MB.
- `grep -z` reads at most 64 KiB of a tar member's long name: a 306 KB archive with a 300 MiB
  name took 1.27 GB and takes 5.6 MB.
- Context output on several threads no longer runs one file's lines into another's.
- A named pipe, `<(...)` or `/dev/stdin` given as a path is searched, and a link to a directory
  is walked without an `Is a directory` warning.
- `grep -z` finds the members after an old-GNU sparse member (`tar --sparse`); they were lost.
- A large file of very long lines is streamed: 100 MB of 4 MB lines peaked at 103 MB, now 15 to
  18 MB.
- A case-insensitive search for a long literal no longer slows on near misses (lines of 79 `=`
  against a pattern of 80: 1.2 s, now 0.01 s).
- A large one-line file searched for a `-F` literal peaks under 6 MB instead of 103 MB.
- With `-e`, a first path that is not UTF-8 is searched as a path.
- A home directory kept in git is no longer indexed from a folder below it when `HOME` is a
  symbolic link.
- `--stats` and the JSON summary of a search stopped by `--max-results` count only printed
  matches.
- MCP tool calls no longer read a file outside the repository through a linked folder
  ([SECURITY](docs/SECURITY.md)).
- `serve` and `lsp` open an index another process rebuilt on their next call; before, the two
  processes rebuilt in turn.
- A panic in the first index build no longer leaves `serve` answering "still building" for good.
- `index` keeps an update and exits 0 when the segment merge after it fails.
- The text index survives more: a file shortened while read, a manifest cut by a crash, many
  small updates, and an edit that keeps size and modification time (`cp -p`, `rsync -t`,
  `touch -r`); its format is 3 ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- A full index build of a C, Go or Java tree no longer keeps every syntax tree in memory: 40 MB
  of C in 600 files took 2.1 GB and takes 0.56 GB.
- Haskell type synonyms, Puppet functions, LLVM named types and OCaml classes are definitions.
- Deep nesting no longer costs more than linear time: a 1 MB Perl file of nested `sub` lines
  0.83 s to 0.2 s of CPU, 16,000 nested PHP includes 20 s to 0.06 s, declarations inside C `#if`
  blocks, and a call chain such as `a()()()` or `x.a().a()` (a 256 KB file, 22 s to 0.25 s). A
  C++ `operator` call such as `a.operator()()()` no longer stores names as long as the chain.
- A damaged segment found during a merge rebuilds the index; `serve --watch` and `watch` keep
  watching after an update panics.

- A tool call path that leaves the repository by its own parts (`..`, an absolute path
  elsewhere) is refused before anything is looked at, and an
  absolute path elsewhere (`/a.py`) no longer names the repository's file of that name. The
  language server reads no file behind a link that leaves the root and works on a Windows share or
  in WSL (`file://wsl.localhost/...`); MCP `callees` says how many outside names it left out past
  50 ([SECURITY](docs/SECURITY.md)).
- `read_code` gives one answer for a path outside the repository whether or not a file is there.
  The Python bindings read a path spelling (`./pkg/a.py`) and a location (`pkg/a.py:3-9`) as the
  MCP tools do.
- An older scrutate no longer rebuilds an index a newer one built: it reads it when only the
  parsing rules are newer and otherwise refuses it with exit 2; `scrutate index --force`
  rebuilds it.
- A documentation comment whose lines carry no comment mark (a GNU style C block, Lua
  `--[[ ]]`, Ruby `=begin` and the like) is in its definition's documentation only, and without a
  grammar a language's own comment marks are read ([LANGUAGES](docs/LANGUAGES.md)).
- The language server goes to the definition a receiver call names, not the plain namesake:
  `o.valid?()` to `valid?`, `self.save!` to `save!`, `M.ok?(x)` to `ok?`, `Main.update!(x)` to
  `update!`, `step' y` to `step'`, and `is` in R `is.ready(x)` finds `is.ready`. References of
  `valid` no longer list `valid?`.
- `follow = true` in a repository's own `.scrutate.toml` is ignored with a warning, so a clone
  cannot make `scrutate grep` leave it through a link ([SECURITY](docs/SECURITY.md)).
- A name typed with its punctuation is found by that name first: `search 'valid?'`,
  `search Get-Greeting`, `search 'list->vector'`, `search 'Rec.valid?'`. More test file names are
  told (`button.test.jsx`, `parser_test.rs`, `parser_spec.rb`, `WidgetTest.kt`, a directory
  `MyApp.Tests`). `similar` takes a window's location.
- `callers` lists calls written outside every definition, and `callees FILE:LINE` answers for
  such a line.
- Elixir `@doc false` leaves a function undocumented, PowerShell help in a function body and an
  Erlang `-doc` attribute are documentation, and `this["k"] = function` is the member `k`.
- A Go type on its own line (`type Server struct`) takes the comment above it, and a Go type alias
  (`type Celsius = float64`) is a definition.
- `symbol`, `callers`, `callees`, `refs`, `outline`, `deps`, `rdeps`, `similar`, `dupes`
  (exact + MinHash/LSH near-duplicates), `tsquery` (tree-sitter structural search),
  cross-repository `search --also`.
- `refs` resolves each occurrence to its definition for Python, JavaScript/TypeScript, Rust, Go
  and Java; `--def` keeps one definition and `--textual` gives every whole-word occurrence.
- Java in `refs`, `callers`, `callees` and the language server: packages, imports, members,
  nested classes, records, method references, and overloads picked by the argument count
  ([CLI](docs/CLI.md)).
- `callers` of an override lists a call through its base type as undecided, a `super` call is a
  decided call of the nearest base method (also `super(C, self)`), TypeScript follows `extends`,
  and a bare Python import finds the module beside the importing file (`from helpers import x`
  in `tests/`).
- A JavaScript or TypeScript import of a workspace package by its `package.json` name resolves to
  the package's file through `exports`, `module`, `main` or `index`; a `.d.ts` beside its
  implementation no longer takes the import ([CLI](docs/CLI.md)).
- Imports are recorded as their statements write them, in more languages; a Java star import is
  one `a.b.*` record that `deps` resolves to the package's files.
- In a language the resolver does not read, a call without a receiver is a call of the
  definition in its own file (`callees` listed the index's first three namesakes), and a MATLAB `.m` file is no longer in the C family.
- A search that meets a damaged segment array rebuilds the index and answers; it panicked.
- `callers` of a constructor lists the calls of its class (`Point(1)` for `Point.__init__`,
  `new Widget()`, `Invoice.new`); it printed "(no results)". `refs` gives the reason
  `not resolved in this language` for a language with a grammar and no resolver rules.
- A call is recorded under its definition's name (a trailing `?` or `!`, a prime, `r#`):
  `callers valid?` and `callers save!` printed "(no results)", and a Haskell `step'` was indexed
  as `step`.
- The comment above a definition is its documentation in more languages, and survives what
  stands between (Rust attributes after `/// doc`: 172 items of this repository had lost theirs;
  decorators, `@spec` and the like) ([LANGUAGES](docs/LANGUAGES.md)).
- More forms are definitions: a function assigned to a name, member or table field in Lua,
  JavaScript and TypeScript (`exports.a.b = function`, a call through
  `const app = require("./app")` resolves to it), Julia methods on another module's function
  (`Base.:(==)`), R6 methods (`R6::R6Class`) and the C++ call operator
  ([LANGUAGES](docs/LANGUAGES.md)).
- `search` looks a one-word query up as a name: `search find` and `search get` printed
  "(no results)".
- A Java file counts as a test by `Test.java` or `Tests.java` with the capital (`Contest.java`
  and `Latest.java` were ranked as tests), and a Django `tests.py` is a test file.
- `languages --json` lists every kind of definition the index gives a language, and a Racket
  `define-syntax-rule` is a macro.
- `refs` finds a name in its composed and decomposed Unicode spelling.
- `tsquery` caches each file's captures in `.scrutate/tsquery/`: a repeated query on the Linux
  tree, about 25 s then 1.6 s.
- Semantic vectors stored as int8 (schema 13): the index about a fifth smaller, quality within
  0.001 MRR.
- `scrutate lsp`: a language server for editors; the VS Code extension starts it, JetBrains uses
  it through LSP4IJ.
- `scrutate watch` and `serve --watch` keep the indexes fresh as files change.
- A long-running index reparses an edited file incrementally for the 21 grammars without an
  external scanner: a 66 KB Go file, 5.5 ms to 2.0 ms per edit.
- Ranker v8: better than v7 on held-out code in 56 of 58 cells, never worse beyond noise
  ([QUALITY](docs/QUALITY.md)).
- Ranked search finds words of every script, scripts without spaces as character pairs; names
  compare in NFC (new dependency `unicode-normalization`), and Traditional and simplified Han
  find each other (a Unihan 18.0.0 table).
- Constants are symbols of kind `constant` in many languages (TypeScript `static
  readonly` fields, JavaScript `static` fields in capitals, Java `static final` and the like):
  `symbol`, `outline`, `read_code` and `refs` find them, ranked search never returns them.
- A method is named by its owner's bare type (`Box.put` for `Box<T>::put`); a dotted Lua or Elixir
  name is split off its owner (`MyApp.Accounts.User` is `User`); `Account:deposit` in Lua is
  `Account.deposit`; an object literal's functions are methods under what holds it.
- New definitions in many languages, among them Rust `mod`, `type` and `macro_rules!`, C++
  methods declared in a class, JavaScript `module.exports.X`, TypeScript `declare function`, and
  C and C++ prototypes; a `.h` file in C++ or Objective-C is parsed with that grammar
  ([LANGUAGES](docs/LANGUAGES.md)).
- Calls inside Rust macro arguments and turbofish calls (`f::<T>(x)`) are call edges.
- Calls are recorded in more languages and forms (PHP `?->`, Lisp names whole), and a Scheme
  `(define (f x) ...)` head is not a call ([CLI](docs/CLI.md)).
- Chunk text is cut at 32 KiB, and many sibling definitions or deep nesting index in linear time
  (20,000 one-line JavaScript functions: 58 s to 0.8 s; 60,000 nested Lisp calls: 19 s to 0.3 s);
  HTML and XML are no longer parsed.
- Inside a git repository the ignore files decide for `build`, `dist`, `target` and `venv`;
  `\,` keeps a comma in `SCRUTATE_INDEX_EXCLUDE_DIRS`.
- The heuristic tier reads OCaml `let f x =` as a function; Kconfig and `.txt` files get no
  keyword symbols.
- The model signature is a hash of the model file, so a retrained model rebuilds the vectors.
- A read never waits for another process's writer: it answers from the last committed index. A
  `serve` call waits up to 20 seconds for a first build.
- More kinds of damage are rebuilt, a writer stopped before its commit no longer blocks later
  ones, and a read-only index is answered as it is ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- On Unix a change of status time alone (a `chmod`) is seen.
- A `.scrutate` that is a symbolic link, or holds one (checked three levels deep), is refused, so
  nothing outside the repository is written or deleted through a link. Outside the repository
  scrutate writes only the frecency file under `$XDG_STATE_HOME/scrutate/`, the agent
  configurations `setup` writes, and the Grep hook's counter ([SECURITY](docs/SECURITY.md)).
- `refs`, `callers` and `callees` infer a receiver's type from return types, fields, constructors
  and more, and a call matches a definition only in its own language family and by the name as
  written (`Ok(1)` is not a call of `ok`) ([CLI](docs/CLI.md)).
- Name lookup in `symbol`, `callers`, `callees`, `similar`, `refs` and `read_code` takes the name
  as typed first, then `Type::method` and generic owners, and orders namesakes; the C++
  `operator->` and `operator*` are found by name, and an explicit destructor or operator call
  (`a.~Shape()`, `a.operator==(b)`, `p->operator()(3)`) is recorded, so `callers ~Shape` and
  `callers operator==` find it ([CLI](docs/CLI.md)).
- A location (`path:start-end`) names exactly one definition in `symbol`, `callers` and
  `callees`.
- `callers`, `callees`, `symbol`, `dupes`, `tsquery`, `read_code symbol`, `duplicates` and
  `structural_search` say when they stopped at their limit and which argument raises it.
- `tsquery` prints paths as `grep PATH...` does and names a missing path in a warning, exit code
  2; `dupes` lists groups in one fixed order.
- Imports: Go through `go.mod`, an `#include "util.h"` beside the including file; config files
  are read only as regular files inside the repository of at most 1 MB.
- A name binding hides an outer one only in its own scope; `tsquery` decodes non-UTF-8 files,
  skips files over 1 MB and refuses an empty query.
- `dupes`: `exact` means the same body once whitespace is squeezed; `--threshold 1` prints the
  exact groups only.
- `grep --symbols` with a path argument finds the file by its path under the root.
- `deps` and `rdeps` resolve imports as `refs` does (Python `from . import`, Rust, Go,
  TypeScript, Java), and `rdeps` counts the unresolved imports that may name the file.
- An index built with older parsing rules rereads every file once (`content_rev` in `meta`).
- An update can be cancelled and rolls back, also when it panics.
- A one-line file with 8,000 nested functions peaks at 542 MB to index (462 MB with
  `SCRUTATE_SEMANTIC=none`), down from about 1.1 GB (0.75 GB).
- `similar` on a constant says constants are not compared, and `similar -k 0` returns nothing
  instead of panicking.
- A backslash in a Unix file name stays in `tsquery`, `read_code` and the language server.
- Names with control characters print as escapes in the index commands, and read back as an
  argument.
- The Python reference indexes the engine's file set and cuts the same chunks; the parity check
  fails on any difference.
- One module, `stores.rs`, keeps every store under `.scrutate/` current; `reindex` and the
  bindings' `index()` now also update the text index when there is one.
- `tsquery` keys its cache by the code index's content hash, so a warm run reads no unchanged
  file.
- Every front end runs the same operations through `Repo`.
- Breaking, the JSON of the index commands (schema 2): `symbol`, `callers`, `callees`, `refs`,
  `outline`, `deps`, `rdeps`, `similar`, `dupes`, `read`, `search` and the ranked `find` print
  `{"schema": 2, "result": ...}` with `stopped_at` when the answer stopped at its limit; they
  printed the answer alone. `refs --json` and the `grep_code` result lose their own
  `"schema": 1`; a `batch` record and the MCP structured result say `"schema": 2`. Engine A's
  JSON Lines, the error records, `tsquery --json`, `check`, `stats`, `languages` and `config`
  keep schema 1 ([CLI](docs/CLI.md)).
- `scrutate read PATH [--start N] [--end N]` and `read --symbol NAME`: the MCP `read_code` tool
  on the command line.
- The MCP `grep_code` tool takes `mode` (`regex`, `literal`, `identifier`, `fuzzy`,
  `fuzzy_regex`, `pcre2`, with `max_edits`), `case` (`sensitive`, `insensitive`, `smart`),
  `word`, `path` and `globs`; it was a case-sensitive regex.
- `grep --max-bytes N` (`SCRUTATE_GREP_MAX_BYTES`, `[grep] max_bytes`) bounds a grep's text to
  whole lines with the `--offset` that continues (on this repository `grep -n "fn " .` printed
  285 KB; with `--max-bytes 4000`, 4,000 bytes or fewer). A paged answer that filled its limit
  exactly no longer prints one byte more.
- `find` lists the lines of its Engine A search in path order.
- The Python bindings gain `Repo.references`, `Repo.call`, `undecided` on the callers list, and
  `GrepIter.stats()["notes"]`.

### Product
- The `tsquery` cache and the kept answers are read only as regular files within their size
  limit, never through a symbolic link ([SECURITY](docs/SECURITY.md)).
- `scrutate --root DIR grep ...` reads grep's own arguments and shows grep's help. CI builds every
  fuzz target before replaying the inputs that once crashed one.
- The structural search numbers against ast-grep were measured with scrutate's capture cache
  filled, so scrutate parsed no file; the documents and the chart now say so, and the benchmark
  harness runs `tsquery --no-cache` from now on.
- In a linked git work tree (`git worktree add`), the first lookup copies the main checkout's
  index and parses only the files that differ: 0.06 s of user CPU (0.2 s with system time) on
  this repository, where a build costs 4.3 s. Renames, copies and a branch switch and back no
  longer parse files the index has seen; nothing under the main checkout's `.scrutate/` is written
  (SQLite marks its read lock in `index.db-shm`, as for every reader).
- An index command uses the index a running `scrutate serve` holds open on the same root, through
  `.scrutate/session.sock` (Unix, owner only), with the bytes and exit code a fresh process
  prints; it never starts a server. `SCRUTATE_NO_SESSION=1` turns this off;
  `SCRUTATE_SESSION_DEBUG=1` says why a call did not use one ([CLI](docs/CLI.md)).
- The reference package lost its command line, MCP server and agent (`scrutate_ref/cli.py`,
  `mcp_server.py`, `agent.py`, `python -m scrutate_ref`, the `scrutate-ref` script and the `mcp`,
  `agent` and `all` extras): nothing used them. The binary is the product.
- Ranking quality has its own guard: `bench/quality.py` runs 1,183 committed queries on a frozen
  copy of this repository and fails below the floor in `bench/quality/floor.json`; a CI step and
  the release gates run it ([QUALITY](docs/QUALITY.md)).
- The bit-for-bit Python mirror of the engine ended: `python/scrutate_ref` no longer parses,
  indexes or ranks (`text.py`, `parse.py`, `index.py`, `lex.py`, `search.py`, `pretrain.py` are
  gone, with the tree-sitter grammars and `snowballstemmer`), and `bench/parity.py` and
  `bench/parity_repos.sh` are gone. The mirror covered 7 of the 73 grammars and doubled every
  change; the quality floor and `tests/ranking_pins.rs` watch ranking now. The package keeps
  `rank.py` and `semantic.py`; commit `8a4f4b4` is the last with `bench/eval_transformers.py` and
  `bench/ivf_recall.py`.
- `[index] exclude_dirs` is read from the `.scrutate.toml` at the index root (or
  `SCRUTATE_INDEX_EXCLUDE_DIRS`), so every front end indexes the same files; the file may be a
  link inside the root (at most 1,000,000 bytes), and a name with a backslash, which made
  `grep_code` fail, excludes nothing. A capped `--json` answer says so on standard
  error, `grep --index` from an ignored directory exits 2, and `dupes --min-tokens` at the
  largest value no longer wraps.
- Contributions are accepted under a contributor licence agreement (`CLA.md`), and the name and
  the logo have a trademark policy (`TRADEMARKS.md`).
- The licence is `MIT OR Apache-2.0` at the user's choice (it was Apache-2.0 alone): `LICENSE` is
  now `LICENSE-APACHE` beside a new `LICENSE-MIT`, and every package and
  `THIRD_PARTY_NOTICES.txt` say so; `vendor/ignore` keeps `Unlicense OR MIT`. The README has a
  "Sponsor and hire" section.
- Unified `find`, MCP server (19 tools), configuration files with environment-variable
  precedence, shell completions, progress reporting; errors as JSON records with `--json` or
  `SCRUTATE_DIAGNOSTICS=json`.
- On/off environment variables are off for `0`, `false`, `f`, `no`, `n`, `off` or empty; `NO_COLOR` turns
  colour off for any non-empty value.
- One definition of every index operation for the CLI, the MCP server, `batch` and the Python
  bindings (`ops.rs`); `batch` runs any operation by name.
- For AI agents: no MCP tool changes a source file unless `serve --allow-edits` lists
  `edit_symbol`, and every tool but `reindex` and `edit_symbol` is marked read-only
  (`readOnlyHint`); `grep_code` groups matches by
  enclosing symbol; `references` resolves each use; an answer is at most 24,000 bytes by default
  (`max_bytes`) with the `offset` that continues it, as `--max-bytes` (`SCRUTATE_MAX_BYTES`) and
  `--offset` on the CLI; a first index build blocks a tool call for at most 20 seconds
  ([CLI](docs/CLI.md)).
- A capped `grep_code` or `references` answer holds the first matches in path order.
- `read_code` by path keeps at most 1 MB of line text, refuses a file with a NUL byte in its first
  8 KB, and says `more lines follow` when a large file was not read to its end.
- Commands run in a subdirectory use the index and `.scrutate.toml` above it, up to the top of
  the repository, so a nested clone or submodule does not use the outer index; `search --also`
  prints another repository's results by the path given ([CLI](docs/CLI.md)).
- Editors opened on a folder below the repository top use the index at the top, through
  `scrutate config --json`; the language server built a second, partial index there.
- In the home directory or at a filesystem root with no index, a command that would build one
  stops with exit code 2 and names `--root` and `scrutate index`.
- `languages` lists the extensions a language shares with another (`extensions_by_content`).
- `symbol`, `callers` and `callees` take `--limit`; `check --json` counts the indexed files and
  the constants; `batch` reports a line that is not JSON and runs the rest.
- `reindex` takes `force`. `grep_code` searches the files the index holds.
- JSON-RPC answers as the specification says (-32700, -32600, -32601, -32602), and notifications
  are never answered.
- MCP revision 2026-07-28 beside the handshake revisions; `serve --structured` gives every tool
  an `outputSchema`; `notifications/cancelled` stops a call, and a `progressToken` gets progress
  during the first build ([CLI](docs/CLI.md)).
- `read_code` reads a whole definition with `symbol`, and the location at the start of every
  result (`path:start-end`) is accepted wherever a tool takes a symbol.
- The index recovers from a corrupt database in every front end that stays open, not only the
  CLI.
- `refs` resolves a method call on a variable through its declared type (`x: T`, `x = T()`,
  `new T()`, `T::new()`, `T {}`, `&T{}`), and finds a Go method by its receiver type.
- TypeScript and JavaScript imports through `paths` or `baseUrl` resolve for `refs`, `deps` and
  `rdeps`.
- Smart case sees a letter written by its code point (`\x41`).
- `watch` and `serve --watch` no longer stop on Linux and Windows when `.git/info/exclude` has
  patterns.
- `-U` with `--fuzzy` or `--fuzzy-regex` is an error.
- An MCP answer checks the files it cites with one `stat` each and reindexes a changed one alone;
  without `--refresh-interval` the server scans at most a second apart.
- At the end of its input the server answers the calls in flight first; an `initialize` without a
  known `protocolVersion` is answered with 2025-11-25.
- `read_code` honours an explicit `end` and says where the rest starts.
- A tool argument of the wrong type is an error naming it; `search_code` takes at most 10
  `alt_queries`.
- `watch` and `serve --watch` set the watch before the first update, use one inotify watch per
  directory on Linux (2 instead of 7,012 on a tree with an ignored `node_modules`), and update a
  burst of saved files without a walk on Linux and Windows ([ARCHITECTURE](docs/ARCHITECTURE.md)).
- `find` takes a question with a space and a trailing `?` as words, and goes to ranked search
  when a regex-looking query finds nothing.
- Text files check out with LF line ends on every platform (`.gitattributes`).
- Toolchain pinned to Rust 1.100.0, edition 2024 (`rust-version` 1.91).
- Python bindings (PyO3 0.30): three wheels per platform (`cp39-abi3`, `cp314t`,
  `cp315-abi3.abi3t`), the GIL released (in `stats()` too), notices in `dist-info/licenses`,
  Ctrl-C within about 100 ms, and results as a `scrutate.Results` list with `stopped_at`
  ([bindings](crates/scrutate-py/README.md)).
- Integrations: one MCP configuration per client, GitLab and Bitbucket templates, a pre-commit
  hook that works through `repo:`, the GitHub Action on Windows runners, Neovim's
  `lsp/scrutate.lua` (so `vim.lsp.enable('scrutate')` is the whole setup), and VS Code and Neovim
  that on Windows take a bare `scrutate` from an absolute `PATH` entry only, not from the working
  directory.
- `release.yml` refuses a tag whose version differs from the packages' (`tools/check_version.py`)
  and runs all of `ci.yml` on the tagged commit first; every action is pinned to a commit; the
  glibc archives need at most glibc 2.34.
- The GitHub Action puts scrutate's exit code in its `exit-code` output and fails only on an
  error (2).
- Benchmark suite (`bench/leaderboard.py`, `bench/indexed.py`) against grep, ripgrep, ack, ag,
  git grep, ugrep, ast-grep, Zoekt and livegrep, every answer checked, with a regression gate
  (`SCRUTATE_BASE_BIN`, `leaderboard_regression.py --within`) ([BENCHMARKS](docs/BENCHMARKS.md)).
  `benchmarks.yml` runs it on the small corpora and counts every benchmark's instructions under
  Valgrind (`bench/instructions.py`, past 1% fails; start-up without the dynamic loader's own,
  a row of their own), behind one `benchmark gate` check.
- Random checks in CI: `bench/fuzz_grep.py` and `bench/fuzz_incremental.py`, 20 seconds each.
- Packaging: Homebrew, Scoop and winget (schema 1.12.0), `.deb` and `.rpm`, each carrying
  `THIRD_PARTY_NOTICES.txt`; `docs/dashboard.html` charts the benchmark runs.
- Fuzz targets (`fuzz/`): the `.npy` reader (three defects fixed), the text index, the
  edit-distance matcher and every parser tier.
