<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-architecture.svg" alt="Architecture. How do the two engines work inside?" width="880">
</p>

# Architecture

[Documentation](README.md) &nbsp;·&nbsp; Next: [Languages](LANGUAGES.md)

**Contents**: [Engine A: universal instant search](#engine-a-universal-instant-search-cratesscrutatesrcinstantrs) &nbsp;·&nbsp; [Engine B: code intelligence](#engine-b-code-intelligence-indexrs-lexrs-parsers-langsrs-searchrs-intelrs) &nbsp;·&nbsp; [Unified layer](#unified-layer-opsrs-librs-clirs-mcprs-cratesscrutate-py) &nbsp;·&nbsp; [Models](#models-cratesscrutatemodels-modelsrs) &nbsp;·&nbsp; [Design rules for the next decade](#design-rules-for-the-next-decade)

<!-- End of head. -->

Scrutate is one binary with two independent engines and a thin unifying layer.

```
                 scrutate (CLI, MCP server, Python bindings)
                 ┌──────────────────────────────────────────┐
  query ───────► │ unified layer: `find` picks the engine,  │
                 │ `grep --symbols` and `refs` combine them  │
                 └───────┬───────────────────────┬──────────┘
                         │                       │
        ┌────────────────▼──────┐   ┌────────────▼────────────────────────┐
        │ ENGINE A  instant.rs  │   │ ENGINE B  index.rs lex.rs search.rs  │
        │ no index, no parsers  │   │ persistent, incremental index        │
        │ ignore-aware parallel │   │ tree-sitter symbols + call graph +   │
        │ walk, regex/literal/  │   │ imports; BM25 postings; semantic     │
        │ identifier/fuzzy,     │   │ vectors (IVF); LightGBM ranker;      │
        │ streaming, cancel     │   │ dupes/similar/outline/deps/tsquery   │
        └───────────────────────┘   └──────────────────────────────────────┘
```

## Engine A: universal instant search (`crates/scrutate/src/instant.rs`)

<p align="center">
<img src="assets/diagram-engine-a.svg" alt="Engine A: the command line, the walk, the first read of each file, four ways to search a file, and the output." width="880">
<br><sub>Engine A's path for one search. A file's first read picks how it is searched; the bullets below give each path's limits and measurements.</sub>
</p>

Built on the `grep-searcher`, `grep-regex`, `grep-matcher` and `ignore` crates that ripgrep is
built from (search, regex, matcher trait, ignore-aware walk, per their crates.io pages). Scrutate
adds:

- **Matching modes**: regex (Rust `regex`: linear time, no backtracking, size-limited), literal,
  **identifier** (one spelling matches every casing: `refreshToken` ≡ `refresh_token` ≡
  `REFRESH_TOKEN`; digits and acronyms are parts), **fuzzy/approximate** (Myers' bit-parallel
  edit distance in characters: one 128-bit word up to 128, 64-bit blocks with carries up to 4096,
  a pigeonhole prefilter so only candidate lines pay for the DP; `-i` folds case as a regex does),
  multiline, inverted, whole-word, smart case. A case-insensitive literal (`-i`, `-F -i`, `-S`)
  finds candidates without the regex engine: two needle bytes, each in either case, at a fixed
  distance inside a stretch whose characters keep their byte length under case folding (`k` and
  `s` end one: KELVIN SIGN and LONG S are longer); the regex confirms each candidate line. The
  scan is safe slice code over 64-byte blocks (NEON or SSE2, plus an AVX2 copy picked at run time
  on x86_64). One thread, a 740 MB file: 63 ms of user time for `pm_resume` (regex engine 101),
  48 ms for `KÖNIG` (regex 96).
- **Traversal**: the `ignore` parallel walker (work stealing, `.gitignore` / `.ignore` / global
  excludes, hidden files off, symlink loop detection, depth and size filters, type globs from the
  language registry), vendored (`vendor/ignore`) with eleven changes listed in its README and
  published as `scrutate-ignore`, which `crates/scrutate` imports as `ignore`. Four are for speed. The global git excludes path comes from `~/.gitconfig` through a
  small parser, not a regex per run (0.4 ms of a 6 ms search). An idle thread waits 10 µs, doubling
  up to the original 1 ms, before looking for work (598 files: 12.3 ms to 9.3 ms). Gitignore lines
  of three shapes (`**/` and a name with `*` or `?`, `**/` and a literal path, `/` and a name with
  `*` or `?`) match by a byte loop, tested at random against `GlobSet`, whose regex cost 100 to
  400 µs per file to build, more than a small repository's whole walk. Many such globs on many paths are
  slow (the Linux tree's root `.gitignore`: over 100 globs, 95,000 paths, 7.8% of the CPU), so a
  matcher builds one `GlobSet` once its tries cost about the build, which a small repository never
  reaches. A parallel walk can add threads as it runs (`grow_threads`), their visitors built at the
  start.
- **Thread count** ([CLI](CLI.md#engine-a-grep-alias-g) has the limits): on macOS the performance
  cores, as more threads opening files wait on each other in the kernel. Small-file read time tells
  page cache from disk. From disk a walk or file list adds a thread per 16 slow reads, as the disk
  serves reads in flight, one at a time, as a thread costs about a slow read, which a small tree
  never earns back. `-j` overrides all of it.

  | Measured | Result |
  |---|---|
  | the Linux tree on an M3 Pro | 977 ms on 11 threads, about 845 ms on 5 |
  | a small-file read, page cache against disk | macOS: about 25 µs against 160 µs or more; Linux: under 15 µs against 30 µs or more |
  | the Linux tree cold on 5 CPUs, Linux | 1.22 s on 5 threads, 0.37 s with a thread per 16 slow reads |
  | cold, macOS | 2.7 s on 5 threads, 1.3 s on 22; 48 gained 5% on one tree, lost 4% on another |
  | 231 files cold on Linux | 5.8 ms adding one at a time, 8.3 ms with a thread per slow read |
- **Searches at the same time (macOS)**: the kernel serves about four file-reading threads at full
  speed across all processes, so four searches on five threads each lose to four on one. A search
  that joins the count ([CLI](CLI.md#engine-a-grep-alias-g); also `grep_code` and references)
  adds 1 to a per-user System V semaphore (`running.rs`) with `SEM_UNDO`, so the kernel takes it
  back however the process dies. It reads the count after its first four files (searches started
  together undercount each other) and, past 1,000 files, every 64 files. Walk threads over the
  share pause between directories (`pause` in the vendored walker) until it rises; a list's extra
  threads end and restart as it rises. A paused thread sleeps a thirty-second of its paused time,
  at most 1 ms: the walk cannot end before it wakes, and a 1 ms sleep cost a 13 ms search over a
  millisecond. A lone search makes three system calls for this; one directory without
  subdirectories, or a list of 256 files or fewer, makes none. A process leaves the count while a
  pipe or terminal write may block and while stopped from its terminal: the SIGTSTP handler
  (installed only while counted and only if nothing else handles the signal) takes it out, stops it
  with the default action, and puts it back on continue. On Linux the cores are the limit and
  splitting never won (py-big on 5 CPUs, four at once: 53 ms on 5 threads each, 60 ms on 1), so
  nothing is counted.

  | At once, macOS | Before | Counted |
  |---|---|---|
  | py-big, two | 113 ms | 91 ms |
  | py-big, four | 244 ms (189 ms on one thread each) | 185 ms |
  | py-big, eight | 482 ms | 413 ms |
  | the ripgrep sources (250 files), four | 14.7 ms | 13.3 ms (`-j1` 13.1 ms) |
- **One scheduler, one set of thread rules** (`instant::schedule`, `policy.rs`): every walk, file
  list and standard input goes through one scheduler, which builds each thread's `Run`, starts the
  threads, and asks the rules how many to start, when to add one and the search's share. The rules
  read cores, performance cores, searches running, slow small reads, and a table measured per
  system (macOS, Linux; Windows and others take the macOS read time and cold threads, measured
  nowhere, and count nothing), once per search and per look at the count, never per file. A test
  supplies its own readings, so the share and the cold-disk rule are tested without the hardware.
- **One large file, every core**: a file of 64 MB or more is cut into ranges of whole lines (8 MB
  each). Pass one counts each range's newlines, 256 KiB at a time per thread (1 MiB held 14.3 MB on
  a 740 MB file, 6.1 MB now); a NUL byte sends the file back to the single pass, so binary handling
  is unchanged. Pass two searches a window of ranges per thread and writes output in file order, so
  memory stays bounded. Context lines, `--max-count`, `--max-results`, `-l`, multiline and a
  byte-order mark keep the single pass.
- **Files without line breaks**: a large file with no line break before its last byte is read into
  one buffer of its exact size, checked for NUL bytes, and searched as a slice. Threads fill it in
  1 MiB pieces in order; the first line break or NUL byte sends it to the stream, so lines too long
  for the probes (100 MB of 4 MB lines) hold 15 to 18 MB. The kernel's page copying spreads like
  opens (100 MB on an M3 Pro: 12.6 ms on one thread, 4.0 ms on the five performance cores, 6.7 ms
  on all eleven). A plain `-F` literal wanting plain output or a count reads such a file in
  256 KiB pieces instead, a buffer per thread on up to 8 threads, to find the literal and rule out
  NUL bytes and inner line breaks, then rereads each matching line to write it: on a 100 MB line,
  3 to 5 MB of peak memory where the buffer held 103 MB, and less CPU (a cached small buffer costs
  7 ms of system time per 100 MB, fresh memory 12 ms on one thread and more on several, which fault
  at once). One thread never splits a file, but one with no line break at its probe points still
  gets an exact buffer, where the stream reader grows to 3^k times 64 KiB (141 MB for one 100 MB
  line against 103 MB).
- **Memory of a walk**: a thread keeps its output buffer across files, a queued walk entry is
  96 bytes, down from 144 (its rare ignore error is boxed), and a one-thread walk queues entries
  in a plain vector (LLVM with `-j1`: 9.4 to 8.3 MB peak). What `-j1` still holds over GNU grep on
  a large tree, 1 to 3 MB, is mostly memory the allocator freed and kept (per-CPU magazines, as the
  walk thread moves between cores), the ignore matchers, and directories read whole. Output that
  is not plain (colour, `--json`, a caller's iterator) goes through a queue of at most 64 batches;
  a thread that fills it waits for the formatter, so held hits do not grow with matches.
- **Roots without directories**: when no root holds a directory to enter, a sequential walk with
  the same rules runs on the calling thread and three threads search what it finds, more from
  disk. No thread starts before a second file (four threads cost about 1 M instructions, a third
  of a one-file search past process start). Past 256 files the parallel walk takes over, skipping
  files already searched (three large files: 40.6 ms against 41.9). Any directory to enter goes
  straight to the parallel walk, as one thread reading every directory held searches up (the
  ripgrep repository, 60 directories: 5.6 ms in parallel against 6.0).
- **Start-up** (`lazy_bind.rs`, `cli.rs`): process start dominates a search of a few hundred
  files. dyld binds every imported symbol before `main`, about 3.3 µs each for libSystem symbols
  (a C program importing 121 more starts 0.4 ms later); the executable imported 210 and a search
  calls about 75. On macOS arm64 the other 133 are private stubs that look the function up with
  `dlsym` on first call and jump to it, keeping every argument register and the stack; `fork`
  binds them all first, so a child never looks one up. clap built every subcommand's definitions
  before parsing; `scrutate grep` builds only its own.

  On macOS each `open` or `getcwd` costs 150 to 200 thousand instructions. So the global git
  excludes file is read when a walk first meets a repository (git's configuration and that file
  cost 0.5 M in every walk outside one), the walk reuses the command line's current directory for
  `.`, a search without the index skips resolving the repository root, and a one-file tree starts
  no thread.

  Page reclaims were mostly the executable's own code. `scrutate grep` parses plain argument shapes
  by hand and hands the rest to clap (a property test holds both to one answer; a configuration
  file, or a variable clap reads for grep, goes to clap), `-F` with one literal is a substring
  search with no regex engine, 14 more system functions bind on first use, and on macOS the linker
  drops the unused libiconv and lays out side by side the functions a small search runs, from
  `crates/scrutate/startup.order`. `tools/order_file.py` writes it: it runs the release links under
  lldb with a breakpoint on every function of the link map (standard library ones included) and
  checks how many names a release link still has; a missing name is skipped. ThinLTO makes ld64 skip
  most of them (the `.llvm.N` ones it promoted, local ones several objects hold), so the release
  profile links one object (fat LTO, one codegen unit), and a release build takes 3 to 5 minutes
  from scratch, up from under 2. The order file is written for macOS on Apple silicon. The release
  builds for glibc Linux take the same order through rust-lld's `--symbol-ordering-file`, after a
  first link whose link map gives the names `tools/order_file.py --elf` maps it to (crate hashes in
  Rust names differ per target, and the binary is stripped); musl and Windows link as before. Measured and left alone: 37,000 pointer fix-ups (applied lazily by the kernel, no cost),
  the code-signature page size (no difference), the 85 MB binary size (0.05 to 0.2 ms).

  | A one-file directory search, macOS | Measured |
  |---|---|
  | start-up | 2.4 ms to 1.9 ms after the stubs and clap change (a Rust hello-world: 1.5 ms) |
  | instructions | 21.1 M after start-up, against GNU grep's 20.8 M in the C locale (hello-world 14.8 M); 19.2 M after the walk changes; 17 M with the order file, where it was 19.3 M |
  | page reclaims | 459, against 307 for GNU grep and 291 for hello-world; 328 with the order file (GNU grep: 308) |
  | `__TEXT` pages touched | 75, then 36 with the order file |
  | peak memory | 4.8 MB, then 2.6 MB (GNU grep: 2.05 MB) |
  | CPU time | GNU grep's, with the order file |
- **What bounds a search on macOS**: opening files, which scale to the performance cores and no
  further (processes alike), hence the default thread count. A search of the Linux tree takes
  0.66 s, so about 0.1 s is ours.

  | On APFS | Cost |
  |---|---|
  | `open`, a file opened recently | 8 to 11 µs |
  | `open`, once a large tree's kernel data has left the CPU caches | about 19 µs |
  | `close`; a whole-file read; `stat` | 0.5 µs; 1.3 µs; 1 to 2 µs |
  | the Linux tree, open and read only | 1.95 s on one thread, 0.56 s on five, 0.71 s on six, 0.95 s on eleven |
- **Reading a file**: the first read asks for 64 KB and one byte. A short read is the whole file,
  searched in the thread's buffer with no size call (else a `statx` per file: 96,000 on the Linux
  tree). A larger file, or a NUL byte or byte-order mark in the first read, goes to the stream
  reader, up to 1 MB with a searcher made for it, so the thread's own searcher keeps its buffer
  size (which decides where a NUL byte after the first fill stops output). Reading those whole
  made every thread keep a buffer as large as the largest it met: 12.6 MB peak resident on py-big
  with five threads against 8.2 MB streamed (ripgrep: 8.5 MB), for 0.2% to 1.5% fewer
  instructions on macOS and 1% to 3% less CPU on the Linux tree in a Linux VM. Linux pseudo
  filesystem files report size 0 and answer a first read short (4 KB of the 7.3 MB
  `/proc/kallsyms`); one `statfs` per root and per `proc` or `sys` directory finds them
  ([CLI](CLI.md#engine-a-grep-alias-g)). On Linux, where opens are cheap, this and the matcher change took the Linux-tree
  search from 144 ms to 120 ms in the test VM (ripgrep: 135 ms), with 316,000 system calls against
  ripgrep's 526,000.
- **One way in**: `instant::search` takes a `Query` (the `Pattern`, invert, report: lines, counts
  or files, context, limits, record shape), a `Files` (a walk's roots and rules, a file list under
  a root, or named texts in memory, plus threads) and the caller's cancel flag, which it reads and
  never sets. Results leave as `Record`s of one kind (match, context, count, file, note), or as
  plain lines the threads write (`Output::Plain`). A record is 104 bytes, the size of one result
  back when a flag and a sentinel line number told its kind.
- **One run, one translation**: every file search of a run shares a `Run` (matcher, query, file
  set, counters, stop flag, output target). The pattern is translated once (`Pattern::translate`:
  the form its mode means, and whether case folds); the matcher and the text index's trigram
  condition both come from it, so a file the index lets through meets the same pattern. Smart case
  counts only letters that stand for themselves (`\S` is not uppercase), as in ripgrep.
- **Byte sources** (`source.rs`): every rule of reading a source, written once: first read,
  byte-order mark, NUL byte, size thresholds, large-file split, long lines, searcher setup. A
  source gives its size if known, positioned reads if it has them, and one sequential read. A disk
  file and text in memory (with no system call) give all three; standard input and archive members
  only the read (a member also asks for the line limit; one archive's members share a searcher).
  The rules are generic over the source, so the file path still compiles for files alone. A test
  searches the same bytes as a file and as memory text over every threshold and option shape and
  gets the same records and output.
- **Line anchors**: the regex is always multi-line, so `^` and `$` mean line start and end and the
  searcher scans whole buffers, not a line at a time.
- **Line-start literals**: when every match starts a line with one of at most 64 literals
  (`^(a|aa)+$`, `^#include`), candidates are line starts followed by one, each decided by an
  anchored search. A match that would cross a line break goes to grep-regex's matcher, which
  cannot match one, so it finds nothing.
- **Suffix literals**: when a regex has no literal prefix but every match ends in one of at most
  64 literals of 3 bytes or more (`\w+_(suspend|resume)\(`), Aho-Corasick finds candidate lines
  and the regex runs only there (ripgrep, from the same crates, scans every byte).
- **Streaming**: matches are batched per file and sent over a channel to the caller's sink while
  other threads search. A stop flag (the caller's cancel flag or the search's own) ends the walk
  between files (Ctrl-C → exit 130, `--max-results`, broken pipe). Plain piped output skips the
  channel: each thread formats a file's lines into one shared sink (`PlainSink`), where output
  within a millisecond of other output waits in a 64 KB buffer, so many files leave in one write
  (a write per matching file: 88,000 writes for `return` in the Linux tree; on Linux 302 ms to
  190 ms). Output after a quiet spell leaves at once and threads release held output between
  files, so nothing waits longer than a millisecond or the longest file being searched.
- **Output**: path:line:text, `-c`, `-l`, context, colour, deterministic `--sort`, and JSONL
  (`--json`, schema version 1, record types in [CLI](CLI.md#conventions)). Flags: [CLI
  reference](CLI.md#engine-a-grep-alias-g).
- **No dependency on Engine B**: `grep` never opens the index; `grep --symbols` is the one opt-in
  bridge (it groups matches by enclosing function, using the index if one exists).

## Engine B: code intelligence (`index.rs`, `lex.rs`, `parse.rs`, `langs.rs`, `search.rs`, `intel.rs`)

<p align="center">
<img src="assets/diagram-engine-b-update.svg" alt="Engine B's update: walk, hash, parse and chunk; symbols and call edges go to SQLite, BM25 postings and vectors to a new segment, in one transaction." width="880">
<br><sub>One index update, from the walk to the commit. Parsing, Storage and its Lifecycle below describe each step.</sub>
</p>

### Parsing (`parse.rs`, `langs.rs`)
Three tiers, so every file yields something (docs/LANGUAGES.md): tree-sitter grammars driven by a
**data-driven rule registry** (per language: which node kinds are definitions, how the name is
found, what a call is, how imports look), a keyword heuristic tier, and overlapping line windows.
Each definition becomes a *chunk* with fields name / qualified name / signature / documentation /
body / path, plus call edges and imports. A new language is a registry entry and a sample file.

A definition's documentation is the comment block right above it, with no blank line between;
attributes, decorators, annotations and type specs (Rust `#[...]`, `@Get(...)`, `@override`,
Elixir `@spec`, Erlang `-spec`) may stand in between. It is the lines of those comment nodes,
whatever their marks (a GNU style C block, an Elm `{-| -}`, a Lua `--[[ ]]`), and no window
repeats them; a comment a blank line above is in a window. Elixir's `@doc` and `@moduledoc`, a
Julia docstring and Haskell haddock comments count, and in Python and Starlark the docstring
after that comment. A Haskell or Elm function starts at its type signature, whose comment is its
documentation. In Rust, PHP, JavaScript, TypeScript, Java, Kotlin, Scala, Groovy, Dart, Swift,
Elixir and Gleam a definition starts at the attribute lines right above it (`#[...]` or `@...`),
as a Python one starts at its decorators.

A `[[language]]` table adds a language without a build (`custom.rs`,
[CLI](CLI.md#custom-languages)), parsed as the keyword tier parses. The tables in effect are
process-wide, loaded by each command and each index update for the root. Where one process
serves several roots (the Python module's `Repo`s), each `Repo` call and index update runs under
its own root's tables (`custom::of_root`, then `custom::with` on the threads that parse), and
`grep(types=)` carries the tables of the index it starts in (`Files::customs`), so a second root
cannot swap them midway. A custom-language file's
artifact key holds its language's signature, and the index's meta keeps every extension's
signature (`languages`), so a changed table rereads only the files of the extensions it changed.

A chunk's text and body keep at most 200 lines (40 for a container such as a class) and 32 KiB,
cut at a character boundary, and a chunk ends on the last line holding its text. The definitions
of one file share a budget (`parse::TextBudget`): 1 MiB plus 8 bytes per byte of the file, text
and body together, spent in the file's chunk order. A definition filled once it is spent keeps
its first line, at most 256 bytes of it, then `...`, so a file of deeply nested definitions keeps
text in proportion to its size. Normal files stay under it: over 6,262 files of Rust, Python,
C, C++, JavaScript, Markdown and JSON (this repository, ripgrep, networkx, pandas, mypy, parts of
Linux and LLVM), every chunk of a file together kept at most 5.1 times its size and 1.8 MB. A
one-line JavaScript file of 8,000 nested functions (112 KB) indexes in 0.13 s with a 44 MB peak
(release build, macOS arm64; 3.1 to 3.5 s and 542 MB before) and keeps 0.34 MB of compressed
chunk text in the database (40 MB before). Windows do not nest, so they are not cut. Every line no
chunk keeps (past those caps, outside definitions, or only in a constant) is in overlapping
50-line windows named after the enclosing definition, or the file's stem, so ranked search reads
every line; a line of only brackets or a closing word (`end`, `fi`, `done`,
`esac`) is in a window only next to other lines. On the Linux kernel tree the windows add 8.9%
chunks and 11.9% to the code index (not the text index). A window is no symbol: `symbol`,
`refs --def`, `outline` and the language server never return one, and a window's location given
to `symbol`, `callers` or `refs --def` gets "these lines are outside every definition". `callers`
lists a window, or the module chunk, as the caller of a call outside every definition, and
`callees` and `similar` take a window's location.

The index cuts each chunk's text from the source by the parser's byte ranges as it stores it, so
a file of thousands of nested definitions never holds all their copies at once. A
`.h` file is C++, Objective-C or C by its text (docs/CLI.md lists the signs). A method's
qualified name uses its owner's bare type (`Stack.push` for `impl<T> Stack<T>`), with at most 32
segments. The tree walk keeps its own ancestor list and visits children with a cursor, so a
deeply nested or very wide file indexes in linear time. A call's name is read from both ends of
its target: the last separator inside it is found by binary search over that separator's offsets
in the file, gathered once per file, so a call chain costs time linear in its length. VHDL
gives `f(x)` and the element `a(i)` one shape, so after the walk a VHDL file's calls lose the
names the file declares as signals, variables, constants, files or ports (one more walk of the
tree). Constants
(`const`, `static`, enum members, `#define` values and the like) are chunks of kind `constant`
with no postings and no vectors: name lookups find them, ranked search never does; Java fields and
record components are kept the same way, as kind `field`. A JavaScript or
TypeScript object literal's methods are owned by the name holding the literal (`api.get`), so
`callers` treats them as methods, never free functions.

### Tokens (`text.rs`)
One tokenizer serves index and query in both engines. An ASCII word is split at `_` and case
changes, lowercased and stemmed (Snowball English); a compound identifier also gives its joined
form. Another word is normalised to NFC first: runs of a script written without spaces (Han, kana,
Thai, Lao, Tibetan, Myanmar, Khmer, the Tai scripts, Balinese and Javanese) give overlapping
character bigrams (a lone character itself), and any other script one lowercased token per word,
final sigma folded. A Tibetan run ends at the tsheg closing each syllable. Before the bigrams are
cut, in index and query, every Han character with a simplified form is replaced by it, and a
Japanese shinjitai by the key of its traditional form, so 関, 關 and 关 give one token; names and
stored text keep their own characters. The table `crates/scrutate/data/han_fold.txt`, which both
engines read, comes from `tools/gen_han_fold.py`: Unihan `kSimplifiedVariant`, then OpenCC's
`JPShinjitaiCharacters.txt` at a pinned release whose SHA-256 the script checks. The script leaves
out a shinjitai line whose values fold to several keys (連 for 連 or 聯) and a shinjitai that is a
common Chinese character with another reading (芸, 沪, 予, 欠, 缶, 萌, by the Unihan fields
`kTGHZ2013` and the Mandarin readings), and checks that folding twice gives what folding once
does. Through the simplified key, 発 and 髪 meet as 發 and 髮 do in Chinese. An all-ASCII text
gives the tokens it gave before Unicode words were added. Names compare through a key that ignores case and `_`, in
NFC with final sigma folded.

### Storage (`index.rs`, `lex.rs`)
- **SQLite** (bundled, WAL, `synchronous=NORMAL`): `files` (size, mtime, status-change time
  `ctime_ns`, SHA-1 content hash, and `art`, the key of the file's artifact: the path map), `chunks`
  (metadata + zlib-compressed text), `refs` (call edges), `imports`, `artifacts` (the artifact
  store, below), `meta` (schema version, model signature, segment manifest, data version, and
  `chunks_at`, the chunk count of the commit that wrote that data version). The tables are private
  to `index.rs` (the database handle is not public): other modules ask `Index` a question (whether a
  path has a row, the indexed files, the chunks of ids, a file's outline, the chunk or window at a
  line, the chunks of a name, a name's definitions in lookup order, a name's callers and a chunk's
  callees, call counts, a file's imports and every import, the counts `check` and `stats` print).
  Each question keeps the order its callers read, and its statement is cached on the connection. A
  test that damages the index does it through `Index::damage_for_tests`.
- **Segments** (`.scrutate/seg/sNNNNNN/`): immutable, memory-mapped `.npy` arrays, Lucene-style.
  Per field (name, qualified name, signature, docs, body, path): BM25 postings (`ptr`, `doc`,
  `tf`) over a shared 63-bit token-hash vocabulary; `ids`, `dl`; int8 semantic vectors (`vec`, two
  views per chunk: description and implementation) with one float32 scale per view (`vscale`:
  symmetric quantisation, value = code × max|x| / 127, rounded half to even, identical in both
  engines; half the size of the float16 vectors up to schema 12, measured in docs/QUALITY.md),
  stored in **IVF cluster order** for segments ≥ 20 000 rows (spherical k-means, K = clamp(2√n,
  32, 1024); a query probes 48 clusters, measured at ±0.000 MRR against the exhaustive scan).
  Constant chunks have no segment rows.
- **Artifacts** (`artifacts.rs`): what parsing and tokenizing make of one file, keyed by what that
  depends on, so the same bytes under another path, after a branch switch and back, or in the main
  checkout of a linked work tree are not parsed, tokenized or packed again. A parse reads the
  bytes and, of the path, only the file name (language, module chunk name, comment marks) and, for
  an `index` file, its directory's name (an anonymous default export); those, the artifact format
  and `CONTENT_REVISION` make the key (128-bit BLAKE2b). The rest stays with the path map, made by
  `artifacts::derive`, the one function that turns an artifact into a path's rows and postings,
  for a parsed file and a stored one alike:

  | In the key (the artifact holds what they decide) | With the path (`derive` adds it) |
  |---|---|
  | the bytes (SHA-1 of the file) | the path of every row |
  | the file name: language, module chunk name, comment marks, prose files | the test flag (`is_test_path`) |
  | the directory name of an `index` file | the path field of the postings, and its share of each chunk's length |
  | the parsing rules (`CONTENT_REVISION`) and the artifact format | the chunk ids and segment rows |
  | | the vectors, when they are made: per model, from the stored word counts |
  | | name resolution, which reads other files (asked at query time; `callers` and `callees` keep their answers by request, below) |

  An artifact holds the chunks (kind, names, lines, the name's subwords, calls), the imports, and
  per chunk and field the subwords and whole identifiers with their counts. The postings come from
  those, and so do any model's vectors (`Artifact::vectors`, through
  `SemanticModel::encode_counts`), at a change of model too. Chunk texts and vectors are not stored
  twice: while a path names an artifact they are its rows and segment rows, and an artifact
  found there takes them from those. When the last path
  naming it drops it (an edit, a delete, a rename, a branch switch), its texts move into the
  artifact (`kept`), and its vectors are made from the counts when it is used again. Artifacts no
  path names are kept up to a quarter of the database's size (at least 4 MiB), least recently
  dropped deleted first, by the update that dropped some, inside its transaction; a lookup never
  deletes, nor is a named artifact deleted. `index --force` parses every file. The store adds
  3.2 MB (7.5%) to the 42 MB `.scrutate/` of a fresh build of this repository (`--no-text`), and
  11.6 MB (9.5%) to Guava's 123 MB, whose 1,329 copied files (the GWT sources) are now parsed once.

  In a linked git work tree (`files::main_checkout` reads the `.git` file, then the `gitdir` and
  `commondir` files git keeps for the work tree, the link confirmed both ways), a new index starts
  as a copy of the main checkout's index of the same schema, parsing rules and vector model: the
  database read with `VACUUM INTO` in one snapshot and the segments it lists copied, each of which
  must open (a segment the main checkout removed meanwhile fails the seed, and the index is built
  as usual). Copied segments are never deleted by a seed that lost or failed: several first opens
  at once share them, and the one whose database lands first lists them; an unlisted copy goes
  with the segment collection. The first update reads and hashes every file and
  parses only those that differ. Content an update has not seen is looked up in this checkout's
  store, then the main checkout's, read-only in one snapshot per update. Nothing of the other
  checkout is written (review in docs/SECURITY.md). Two plain clones do not find each other.
- **Lifecycle**:
  - content hashing: a fast path when size, mtime and `ctime_ns` match, so a `chmod`
    alone costs a read and a hash; a row without `ctime_ns`, from an older index, is read once.
    On Windows a directory entry has no change time: the walk lists each directory once more by
    handle (`GetFileInformationByHandleEx` with `FileIdBothDirectoryInfo`) and takes every
    entry's `ChangeTime` from that listing, and a single file's stat reads it by name
    (`GetFileInformationByName`, Windows 11 24H2 and later) or else by a handle opened for its
    attributes only, as CPython's `os.stat` does (`index::Listed`, `index::change_time`);
    SHA-1 verification;
  - one `BEGIN IMMEDIATE` transaction per update; new segments written to a temp directory,
    each file and the directory flushed to disk, and renamed before the database names them, so
    a checkpoint never makes a manifest durable ahead of its segments (the name is taken inside that transaction, so a directory already under it was left
    by a writer stopped before its commit, and is removed); deletions as liveness bitmaps;
  - one read snapshot per operation (`ops::run`, each language server request): its reads see
    one commit, and a commit meanwhile shows in the next operation; after an update the log is
    cut to nothing once the readers on older commits finish, waited for about a second, else
    by the next update, and a cut log keeps at most 4 MB of its file;
  - background **compaction** (merge small segments / purge dead rows) that aborts if another
    writer moved the manifest; segment **GC** with an age guard inside the update's transaction;
  - **corruption detection**, all with automatic **rebuild**: `PRAGMA quick_check` on opening a
    database up to 64 MB (`QUICK_CHECK_MAX_BYTES` or `SCRUTATE_QUICK_CHECK_MAX_BYTES`), a larger
    one when an error shows damage and in `scrutate check`; structural validation of every array
    on segment open, deep validation in `scrutate check`; also a chunk text that does not inflate,
    a segment list that is not JSON, a segment without vectors under a model, an `index.db`
    deleted under an open index or that is a directory, a `seg` that is a file, an emptied
    `chunks` table, and a chunk count that differs from `chunks_at` when the statistics are read;
  - **cancellation**: an update checks a cancel flag between batches of 256 files and rolls back;
    a panic inside an update rolls back at once, and the next update clears a transaction left
    open;
  - a **content revision** (`content_rev` in `meta`): an index built with older parsing rules is
    reread on its next full update;
  - **schema and model versioning**: an older schema clears and re-indexes inside the database, so
    concurrent processes stay consistent; another vector model makes the vectors anew (Models);
  - **an order between versions**: an index records the schema version, content revision and model
    revision of the scrutate that built it, and a binary rebuilds or rereads only an older index,
    so an old `serve` and a new command line on one checkout do not rebuild it in turn (newer
    indexes: [CLI](CLI.md#the-index-directory); a newer schema version, model revision or text
    index format is refused until `scrutate index --force`);
  - multi-process safety: busy-timeout + retry on `SQLITE_BUSY`, never deleting a live database; a
    read never waits for another process's writer; a process whose database another process
    deleted and rebuilt (after damage or with `index --force`) notices by the file's device and
    inode on its next call and opens the new one (on Windows the open file cannot be deleted); two
    processes that find an index of an older schema clear it once;
  - a `.scrutate` that is a symbolic link, or holds one, is refused, and fixed-name files are
    written with remove and `create_new`, so the index never writes through a link;
  - every writer makes or opens `.scrutate` through `files::make_index_dir`, which on Unix gives
    it mode 0700 (also to an existing one of another mode), so the copies of private files it
    holds stay with their owner;
  - the quota (`files::Quota`, `index.max_files` and `index.max_bytes`) is checked once the files
    of an update are known and before anything is written: the code index inside its write
    transaction, which then rolls back, the text index before its first segment, so the manifest
    still names the last committed one.

### Retrieval and ranking (`search.rs`, `semantic.rs`, `rank.rs`)

<p align="center">
<img src="assets/diagram-ranked-query.svg" alt="A ranked query: BM25 by field, exact lookup and semantic neighbours give candidates; 48 features each go through the LightGBM ranker." width="880">
<br><sub>A ranked query: three sources of candidates, 48 features per candidate, one learned ranker.</sub>
</p>
A query takes candidates from three sources: field-weighted BM25 (name 10, qualified name 3,
signature 3, docs 5, body 1, path 2), exact name/qualified-name lookup, and semantic nearest
neighbours (the query encoded into the chunks' 256-d space by a contrastively trained code-word
model, inference only, bundled in the binary). 48 features per candidate (lexical scores and
ranks per field, z-scored semantic similarities of both views, literal hits, name coverage, the
name as typed (case included), kind, lengths, test-path flags...) go through a LightGBM
LambdaRank ensemble exported to plain arrays and evaluated in Rust. In `search.rs` the sources are
a list (`Source::ALL`, each with the rank feature it fills) and the features are tables filled by
name (`LEX_FIELDS`, `ROW_FEATURES`, `QUERY_FEATURES`), so a new source or feature is one entry.

All arithmetic is specified to the bit; `tests/ranking_pins.rs` pins every candidate, feature and
score of a small tree with the bundled models in `cargo test`. IEEE basic operations, no fused
multiply-add, no reordered sums. The platform's `ln` is taken only of counts and ratios of
counts. Where it is rounded to float32 at once (count features, idf), any `ln` within one unit in
the last place gives the same float32: checked for every count up to 200,000 and every idf of an
index of up to 1,000 chunks. In the vector weights it stays float64, and the fixture keeps those
counts at 7 or less, whose logarithms every libm rounds alike. So the pins should be exact on
every CI platform: macOS arm64 and Linux arm64 with glibc gave the same bits; x86_64, musl and
Windows rest on this argument until CI runs there. A Python reference engine once mirrored this
arithmetic bit for bit (`bench/parity.py`); it ended because it covered 7 of the 73 grammars and
every change to tokens, chunks, BM25 or features was written twice. Vector dot products are summed in float64 in one fixed order
(four running sums, added left to right) and rounded once, so no summation order can reorder
near-tied chunks. Clustering a large segment is deterministic too: one xorshift64* generator draws
the sample, the first centroids and the re-seeds of empty clusters in a fixed order, and every
float32 sum (row norms, cluster sums) runs in index order. The nearest centroid comes from a
float32 matrix product in any order, within k·2^-24 of the true dot product, so a row with more
than one centroid within 4·k·eps of the best is decided by exact float64 products summed in a
fixed order; the lower cluster number wins a tie.

### Text index (`textindex.rs`)

<p align="center">
<img src="assets/diagram-text-index.svg" alt="The text index: trigrams and mask bytes per file in segments; a pattern becomes a trigram condition, the postings give candidate files, Engine A searches them." width="880">
<br><sub>The text index names the files that can match; Engine A decides what matches.</sub>
</p>
The indexed counterpart of Engine A, for repeated grep-style queries on large trees (the job of
Zoekt and livegrep). It covers exactly the files Engine A's default walk visits (same walker, same
ignore rules) and stores each file's set of byte trigrams (ASCII case folded) as immutable
segments: sorted trigram keys, delta + LEB128 posting lists of document numbers, and per document
the path, size, mtime, flags and status change time. Each posting carries two mask bytes: a bit
for the low three bits of each byte that follows the trigram in that file, and a bit for each
position of the trigram modulo 8.

A query becomes a boolean trigram condition by walking the pattern's regex-syntax HIR (literals,
small classes and alternations expand to exact string sets; concatenations combine them; anything
else adds no constraint), evaluated over the postings as bitsets; Engine A's per-file searcher
searches the surviving files. A string of four bytes or more needs its trigrams at positions that
can line up, each followed by a byte that fits (`kernel_thread` on the Linux tree: 164 candidate
files instead of 1,781). The condition is necessary, never sufficient, so results equal Engine A's
on the indexed snapshot; a property test and a coverage-guided fuzz target check that any text a
pattern matches satisfies its condition, Unicode case folding included (the Kelvin sign matches
`k` under `-i`). Files with a NUL byte index only the bytes before it (line-mode search stops
there) and are always searched by multi-line queries; UTF-16 files and files unreadable at index
time are always searched.

Updates are incremental (size, mtime and status change time, so an edit that keeps size and mtime,
as `cp -p` and `rsync -t` make, is seen), write new segments and a new liveness file per
generation, flush them and their directories to disk, then swap `manifest.json` atomically. More than four segments merges all but the
largest by a streaming k-way merge, so the small segments a watch adds do not rewrite the whole
index; more than 30% dead documents merges all of them. A merge checks the segments it reads
first, and a damaged one rebuilds the index. A file over 64 MB is read in 1 MB pieces, never
mapped: a mapped file another process shortens ends the reader with SIGBUS. A manifest that is not
valid JSON (a crash can leave it empty) is rebuilt by the next update like a damaged segment, and a
generation's liveness file is written again when an update that stopped early left one. Writers
serialise on a lock file (polled with `try_lock`: SMB mounts answer a blocking lock with EAGAIN).
The index stores no file contents (409 MB for the 1.6 GB Linux v7.3 tree; docs/LEADERBOARD.md
compares sizes with Zoekt and livegrep, whose indexes hold the text), so a query reads the
candidate files from the source tree.

### Watching (`watch.rs`)
`scrutate watch` and `serve --watch` turn filesystem events into index updates: inotify and
ReadDirectoryChangesW through the `notify` crate (9.0.0-rc.8, pinned: the first release that
reports an overflowed Windows buffer as lost events), and on macOS FSEvents through `watch.rs`'s
own `fsevents` module. That module loads CoreServices with `dlopen` and takes every function it
calls from `dlsym` on that handle, so the executable imports nothing for it and only `watch`
loads the framework; `notify` is built there with its kqueue backend, which is never started,
for its event types alone. When the load or the stream fails, macOS falls back to the index's
own incremental check on a timer ([CLI](CLI.md#watch) has the platform details and the walk
triggers). The watch is in place before the first update. A burst of files is decided by listing
their directories with the indexes' own walks (`files::CodeWalk`, `textindex::walk_entries`), so
no ignore rule is written by hand. Each walk also records the rule files it read that no event in
the tree names; on Linux their directories are watched after the tree's own; on Windows they are
watched beside the recursive watch on the root; on macOS they are polled. Whatever got no watch
(Linux past `max_user_watches`, a rule directory, the rule files above a macOS root) goes to
`Poll`, which lists those directories or stats those files on a timer and hands each change to
the burst as the event a watch would have sent, so no directory without a watch makes a burst
walk the tree. A burst hands its change
set (the files it names, or every file) to `Repo::update_stores`. There is no new write path: the
watcher is one more writer behind the same SQLite transaction and text index lock file. The
update's own reads are not events, so updates do not trigger themselves. It is no daemon: it runs
only while the command (or server) runs.

### Keeping the stores current (`stores.rs`)
`.scrutate/` holds four stores: the code index, the text index, the `tsquery` cache and the
answers `callers`, `callees` and `dead` keep (`answers.rs`, under Intelligence). One module keeps
them current. A caller names what it does (`stores::Writer`, or a read through `Repo`) and what
changed (`stores::Changes`: every file, or the files a watch burst names), never a store; the
module decides which stores that touches, when it looks, and what happens on damage. A new store
is added there alone.

| Operation | Looks for changes | Keeps current |
|---|---|---|
| `index` | every file; waits for another writer, up to 10 minutes or a cancel | code index, text index (built when missing) |
| `index --no-text` | every file; waits | code index |
| `index --text` | every file; waits | text index (built when missing) |
| `grep --index --update` | every file; waits | text index (built when missing) |
| `grep --index` | does not look: answers from the last text index build | nothing |
| `watch`, `serve --watch` | the files a burst names, else every file; waits | code index, text index when one exists |
| `reindex` (MCP, `batch`), bindings `index()` | every file; waits | code index, text index when one exists |
| an index command, `batch` | every file before the first operation; never waits | code index |
| an MCP tool call, a bindings call, a language server request | every file, at most once per refresh interval of the session; never waits | code index |
| an index command a running `serve` answers | every file, in that session, before the operation; never waits (else the command does the work itself) | code index |
| any operation on `Repo` | the files its answer cites and its arguments name, one `stat` each | code index |
| `serve` and `lsp` start, an `lsp` save | every file; waits | code index |
| `tsquery` | its own walk, one `stat` per file against the code index's record | `tsquery` cache |
| `callers`, `callees` | as any index command, then one `stat` per file the kept run read | its answers |

A look at every file is one walk and one `stat` per file (`files::look`), whatever stores it
serves: it enters each directory the code index's walk or Engine A's default walk enters, and
gives each store the files its own walk would list. No look is kept between calls, so a file
written just before a call is in its answer, inside a server's refresh interval too.

`Writer::text` alone decides whether a text index is kept: `index` (unless `--no-text`) and
`grep --index --update` build it; every other writer keeps one that exists. `tsquery` keys a
file's cached captures by the content hash the code index records and the file name, so a file
whose stat the code index holds as current is not read on a hit.

Recovery has one rule: a store found damaged is built again from the sources, once, by the
process that found it, which says so on stderr, and the operation that met the damage runs again.
The code index's files are deleted first (`Repo::recover`, which every front end reaches: the CLI,
`batch` and the MCP server through `Repo::run` and `Repo::recovering`, the bindings through
`Repo::recovering`, the language server and the watcher through `Repo`). On Windows a file another
handle holds open cannot be deleted, so the database is emptied in place (`Index::clear_in_place`)
before the build, and a segment directory that stays keeps its name, which a new segment skips.
The text index is rebuilt in place, its manifest swapped last. `Index` and `textindex` only report
damage (`index::Damaged`, `textindex::Damaged`). `grep --index` without `--update` reports a
damaged text index, as it promises not to walk the tree. An index file that does not open at all
is sorted out by `Index::open` (`Triage`), before any caller holds it.

Locking: each store has its own writer lock (the code index a SQLite write transaction, the text
index `.scrutate/text/lock`; the `tsquery` cache and the kept answers none, their files written by
readers and replaced by rename) and commits in one step (a transaction, a manifest rename, a file
rename), so a reader sees a store's last commit, never half of one. A writer that keeps both
indexes takes the two locks in turn, code index first, never both at once, so two writers cannot
deadlock; between the two commits a reader can see the new code index beside the old text index,
and no answer reads both. The repositories a session opens (`Repo::another`) share one more
in-process lock, held by every write any of them makes, so a rebuild after damage happens once;
reads take no lock. The artifact store, a table of the code index, commits and recovers with it. A
linked work tree reads its main checkout's index read-only, each lookup in its own read
transaction: it takes only SQLite's read lock and holds back the other's checkpoint for one lookup
at most; the copy a new index starts from is one `VACUUM INTO`, one read.

### Sessions (`session.rs`)
A session holds one repository open for many calls. The MCP server, the language server, the
Python bindings and `watch` are its adapters: none keeps a `Repo` of its own, and each says only
what its protocol needs (newline framing and progress notifications in `mcp.rs`,
`Content-Length` framing and UTF-16 columns in `lsp.rs`).

| `Session` | What it does |
|---|---|
| `open(root, Every)` | opens the index; `Every` is the refresh interval (`serve --refresh-interval`, else one second, shorter when a scan is cheap) |
| `build()`, `building()`, `wait_built(cancel, limit, tick)`, `build_failed()`, `build_count()` | the first build: every file, waiting for another writer; calls wait for it; a panic in it is reported until an update goes through; the files it has read and has to read |
| `run(op, cancel)`, `run_counted(op, cancel, count)` | an operation on an index scanned at most once per interval, waiting up to 20 s for another process's first build; `run_counted` adds the files it reads to `count` |
| `read(cancel, refresh, f)` | a function of the searcher (the language server's lookups), damage rebuilt and `f` run again |
| `write(f)` | an update (a watch burst, a save, bindings `index()`) |
| `run_scanned(op)` | a command line call: every file first, then the operation; nothing when it would have to wait |
| `hold_writes()` | at exit: the first build is waited for and no write starts after it |

Every call gets its own cancel flag, and a panic stays inside it: the call gets the error
`Panicked`, its reader is closed, and the session goes on. Reads run side by side, each on its own
reader: a SQLite connection with its statement cache, the segment maps and statistics
(`LexStore`) and the searcher's caches, none of which threads can share. Readers are opened as
calls need them, from the open index without its triage (`Index::another`, about one SQLite
open), up to the limit [CLI](CLI.md#serve-mcp) gives, and kept; the model set is shared
(`Arc<ModelSet>`). Writes take the shared writer lock; a read beside a write sees the index as of
its last commit (WAL, one transaction per update, the segment manifest swapped in that
transaction). The scan before a read is a write, and a call that waited for another call's scan
does not scan again.

A command line call may use the session `serve` runs on its root ([CLI](CLI.md#serve-mcp): which
commands, the settings `SCRUTATE_SESSION_WAIT_MS`, `SCRUTATE_SESSION_DEBUG=1`,
`SCRUTATE_NO_SESSION=1`, the fallbacks). The session listens on a Unix socket at
`.scrutate/session.sock` (mode 0600, the peer's user checked before a byte is read) from the
server's start to its exit. An index command sends its operation (`ops::Op`, as JSON) with its
printing options (`--json`, the byte limit and offset, a search's `--why`); the session scans
every file, runs it and prints the answer with the command line's own function, and the command
line writes those bytes and exits with that code. The command line first checks that the socket
file is a socket this user owns, the peer is this user, and the greeting names the same executable
(device, inode, size and time), index schema and content revisions, models, `SCRUTATE_*` settings
and root; any difference, error, slow greeting or wait (a write running, every reader busy,
another process holding the write lock) makes it do the work itself. A socket path too long for a
socket address is reached from its directory; the command line goes back by a descriptor of the
directory it left, and stops with an error if even that fails. Windows has no session use. The
language server offers no socket, so `serve` is the one meeting point per root.

### Intelligence (`intel.rs`)
Definitions and references, callers/callees, outline, imports mapped to files by the resolver
`refs` uses, reverse dependencies, **similar code** (implementation-view vectors + token Jaccard),
**duplicates** (exact: the same body text once whitespace runs are squeezed; near: MinHash/LSH
over token 3-grams, nested chunks excluded), and **structural search** (`tsquery`: tree-sitter
query language over all files of a language, parallel, no index needed).

`refs` resolves names (`resolve/`). Engine A finds the whole-word occurrences; each file holding
one (Python, JavaScript/TypeScript, Rust, Go, Java, PHP, C, C++, C#) is parsed again with tree-sitter for the
identifier node, its qualifier, the enclosing scopes and the names its imports bind, while
definitions and their nesting come from the index's `chunks` rows. A file whose tree is more than
2,000 levels deep is not read and type walks stop at 32 nested steps, so hostile nesting cannot
overflow the stack or make a request take minutes; a name's binders and assignments are found once
per scope, so a function of thousands of uses resolves in linear time. The rules (scopes, method
resolution order, qualifiers and declared types, Java packages and overloads, import paths and
re-exports, followed up to 6 import hops deep; Rust `crate`/`self`/`super` paths among them) are in
[CLI](CLI.md#refs). Java overloads share one qualified name in the index.
Inherited methods are kept per class, and the Java walks
over base types read a type reached by many paths once per depth, so a diamond of interfaces costs
linear time. It is a query-time step: no schema change, and `refs --textual` keeps the unresolved
answer.

The resolver is one walker and one rule set per language. The walker (`resolve/mod.rs`) owns what
every language shares: the scopes the index's definitions make, the lookup memo, the depth guards
and the order of steps (the import at a name's own statement, the definition's own name, then the
local scopes, the receiver and its type, the imports, the package and the wildcard imports). A
rule set (`resolve/python.rs`, `js.rs`, `rust.rs`, `go.rs`, `java.rs`, `php.rs`, `c.rs`,
`csharp.rs`) implements the `Rules` trait: its grammars, what binds a name, which blocks a local
does not leave, how a receiver and a base class list are read, how a module path maps to files,
and, for Java, PHP and C#, a walk of its own: Java's locals, fields, enclosing classes and
overloads; PHP's namespaces, `use`, typed locals and properties, `$this`, `self::` and `parent::`;
C#'s namespaces, `using`, typed locals, members and partial classes. A PHP or C# class is found by
name in the index and kept when its defining file declares the namespace the reference names, so
no namespace list is built for `refs` (C# builds one for `deps`, from the index's namespace rows
and each file's head). The C and C++ rule set (one for both grammars) binds each `#include` as a
wildcard import, maps it to files by a path suffix through a file name table built once per
request, and maps a function declaration to the definition with a body, once per declaration. A
Java field is a definition of the index (kind `field`); a field and a method of one name stay
apart because the walker's member lookup skips fields and the Java rules ask for a field only
where a value is read. A step a rule set leaves out is the walker's own. The walker picks a file's
rule set with one `match` on its language, in a macro, so a language added to the `Lang` enum
without a rule set does not compile; it tests a language nowhere else. The node kind tables near
the top of the walker (functions, local binders, patterns) list the kinds of the grammars that use
the walker's scopes together, C, C++ and C# binders among them; PHP does not read them. A file's language comes from `langs::detect` alone: by its name, and by its text where an
extension is shared (`.h`, `.m`, `.pp`, `.cls`); the parser, the resolver and `names.rs` all ask
it.

A call on the result of a call from outside the repository asks the rule set for its type
(`Rules::outside_call`). Rust's (`resolve/rust.rs`) answers from a table of standard methods
written from their std 1.99.1 signatures (`std_method`), whose answer keeps the type arguments
it knows (`Option<Report>`, an iterator of `Report`), and else from two rules: a method of an
outside type that is no standard holder and holds no repository type, and a function of the std
modules whose functions return types of their own, give an outside type of no known name. A
caller-picked method or a closure argument gives none. A type built by `Type::new()` and its kin
carries one unknown type argument, so it never counts as holding no repository type. `type_of` keeps each type it finds by
syntax node, so a chain such as `a.b().c().d()` types each receiver once though resolving a method
and typing its call both ask for it. A plain name in the arguments of a macro that is not a
standard expression macro answers `macro argument` where the walk would tie it to a definition;
`Names` counts that answer as undecided.

One module asks those rules (`names.rs`, `Names`): `callers`, `callees`, resolved `refs`, `deps`,
`rdeps` and the language server's definition and references requests each open one `Names` per
request and ask what the occurrence at a place refers to; no other module builds a resolver.
Within a request each file is read and parsed once, however many of its call sites are asked
about. Of a `callers` run that keeps nothing (`callers size` on Guava, 1,970 sites), about half is
reading and parsing the call sites' files and half the walk at each site. A fifth used to be
asking the index, at each call on an outside type, whether the repository extends that type with
the method; the walker now asks once per type and name.

`path` (`Names::path`) walks breadth first from one definition, asking at each the question
`callees` asks (`Names::calls`), so every step is a callee `callees` lists; the call site is the
first line the resolver ties to the definition reached. It walks the calls the resolver ties
first, and only when they reach nothing walks again over every callee, each untied call marked;
the second walk reuses what the first asked. The repo map (`repomap.rs`) resolves no call site:
resolving every one took 188 s on Guava with one `Names`, where the map takes 1.1 s. Its file
graph comes from the index's names alone (`Names::file_edges`; the edges and PageRank, damping
0.85, 50 iterations, are in [CLI](CLI.md#engine-b)).

`callers` and `callees` keep what they learn of each call site (`answers.rs`,
`.scrutate/answers/`): one file per request (its kind and the name as given) holding, per call
site (path, lines, name), the definitions its uses resolve to and which stayed unresolved, and
what the run read: the index database (device, number and creation time of `index.db`) and its
data version, which every update that changes a row moves, the executable, the root, and the
`stat` of every file the resolver read from disk, absent ones included. A run uses the file only
when all of that is unchanged and the stored run went at least as far (one stopped at its limit
stands for that limit and lower ones). It then asks about the same call sites in the same order of
the same index and bytes, so its answer is the one a run keeping nothing makes. An answer is not
keyed by its own file's content alone because it reads more (the definitions of a name anywhere,
the file list, the Java package map, the repository's interfaces), so after any index update a
request resolves again. Readers write these files without the writer lock, like the `tsquery`
cache: whole, to a temporary name, then renamed into place, so two writers lose one write, never
mix two; a file that does not decode in full is no file, and above 64 MB in all the least recently
used go first. On Guava `callers size` costs 1.4 s of CPU on a first run (19.2 billion
instructions), 0.43 s (6.1 billion) from what was kept, where asking about the 400 likeliest sites
cost 0.6 s (8.3 billion).

`impact` (`impact.rs`) is an operation like the others, so it scans first and pages and prints as
they do. It reads the diff through `git.rs`, which runs `git` in the root, the one program Engine B
starts: `git diff --raw -z` for each file's status, modes and blob ids, `git diff -U0` for the changed
lines of each side, `git ls-files --others --exclude-standard` for untracked files, `git
rev-parse` and `git merge-base` for the base commit, and one `git cat-file --batch` for the base
copies (and, with `--staged`, the staged copies). Every call passes `--no-optional-locks`,
`core.fsmonitor=false`, `--no-ext-diff` and `--no-textconv`, so git writes nothing and starts no
helper of its own; a revision is checked before it is passed (none starts with `-`) and resolved to
a commit id, which every later call uses. Git sees this process's environment, a relative
`GIT_DIR`, `GIT_WORK_TREE`, `GIT_OBJECT_DIRECTORY` or `GIT_COMMON_DIR` made absolute against
its working directory, where git reads such a value (`git::own_env`; a table in `git.rs` says
where git reads each of the six); or, built with `Git::with_env`, a caller's own values of the six variables that say where a repository's files
are (`git::PASSED_ENV`: `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`,
`GIT_ALTERNATE_OBJECT_DIRECTORIES`, `GIT_COMMON_DIR`) in place of every `GIT_*` variable of this
process, its configuration and programs included; any other name is refused before git runs. On Windows
each path Git hands git, its directory and these values, goes in its plain form (`git::plain`),
since git does not read a verbatim one (`\\?\C:\x`, what `canonicalize` gives). Each hunk's new lines map onto the index's definitions
of the file as it is now, and its old lines onto the definitions a parse of the base copy gives
(`parse::parse_file`, the parse the index stores); a definition is matched across the sides by
kind and qualified name ([CLI](CLI.md#impact) has the statuses and windows). The callers are
followed breadth first through one `Names` for the whole request, each step the answer of
`Names::callers` for one location (as `callers --limit 500`), so the answer agrees with `callers`
and each file is parsed once. A deleted definition is not in the index, so its first step is
`Names::callers_of_deleted`: the call edges of its name (`Index::callers_of`) whose call the
resolver ties to no existing definition. The test files are those with the index's test flag
(`text::is_test_path`) among the changed definitions, the callers and the changed files.

`review` (`review.rs`) is a command line answer, not an operation: it serves CI, so no MCP tool or
session runs it. It asks `impact::between` for the diff of two commits (`git diff BASE HEAD`, each
side parsed from git's copies, as `--staged` reads the staged side) and keeps what that answer
read: each changed file's base copy and every call the walk met. The new duplicates take two runs
of `dupes`' search (`intel::dupes_in`) over the index's bodies: one as they are, one with the
changed files replaced by the definitions a parse of their base copies gives; a head group with no
member in a changed file is dropped before the second run, since the base had it unchanged. The
untested mark and the three renderings (Markdown, JSON, GitLab, methods of `Review`) are in
[CLI](CLI.md#review).

`files` (`finder.rs`, the MCP `find_files`) is an operation that reads no index: the command line
runs it without opening one, and the MCP server through `Repo` like any other. It walks the files
of `files::repository_files` (the set `grep_code` searches) and opens none. Each path is scored by
a matcher of its own, not the Myers matcher of `grep --fuzzy`, because ranking needs to know how a
path matched: a substring of the basename (or the path), the query's letters in order, or Sellers'
approximate substring search with the optimal string alignment distance (a swap of neighbours is
one edit; the edit limit and the order are in [CLI](CLI.md#files)). On this repository a warm
call takes about
8 ms of wall time (best of 20). The frecency file (`$XDG_STATE_HOME/scrutate/frecency.json`,
keyed by the canonical root, at most 1,000 files) holds per file a count and when it was last set,
decayed with a half-life of 7 days when read, written whole and renamed into place. The reads of
the `read_code` operation feed it (`finder::visited`), so `read`, the MCP tool, `batch` and the
Python bindings all count; an error there never fails the read.

`dead` (`dead.rs`, through `Names::dead`) reads the text first: one parallel pass over every
regular file the code index's walk reaches splits each into whole words and keeps, for every
function, method, class or type name of the index, the lines that hold it. The same pass reads
each definition's own lines for the rules that make it an entry point of its language (`main`,
tests, exports, public items of a library, registering attributes and decorators). A definition
that is no entry point and that no line outside it names is dead, with no resolver asked. For the
others `Names` asks the resolver about up to 16 lines each in files of the languages it reads,
through the per-line answers `callers` keeps, and stops at the first line it ties to the
definition; one without such a line is undecided, with counts of what the resolver made of the
lines. So a textual use the resolver cannot tie keeps a definition out of dead, never in it.

The caller count that ranking reads (`log_ncallers`) comes from the index (`Index::call_counts`):
every call edge of the name, namesakes included, the number the ranker was fitted on.

`tsquery` keeps each file's captures in `.scrutate/tsquery/` (`tscache.rs`): one file per
(version, language, query text, searched paths from the root) mapping a 128-bit BLAKE2b content
hash to the captures, without paths, so files whose content was seen before are read and hashed
but not parsed. A file cut off by `--limit` is never stored, so a cached list is always complete
and the global limit gives the same prefix. Files are written to a temporary name and renamed; an
unreadable one counts as empty. Eviction is by query file, least recently used first, above
128 MB in total.

`tsquery --rewrite` (`rewrite.rs`) shares the query's checks and walk with `tsquery`
(`intel::compile_query`, `intel::query_files`) but not the cache, since a write must rest on the
bytes it read. `rewrite::plan` reads and parses each file once, takes per match the `@match`
capture or the widest one, fills the template from the captures' original text, keeps the
outermost of overlapping matches, and checks that the text parses where it changes, before and
after. `Plan::diff` prints the hunks from the known edit positions (no line diff is computed), and
`Plan::write` writes each file whole to a temporary file beside it and renames it over the original
only while the original still holds the bytes read. No index is touched: the next lookup finds the
file's new stat and reads it. The MCP server has no tool for it.

## Unified layer (`ops.rs`, `lib.rs`, `cli.rs`, `mcp.rs`, `crates/scrutate-py`)

<p align="center">
<img src="assets/diagram-front-ends.svg" alt="The CLI, Python bindings, MCP server, language server and watcher go through one engine to one .scrutate directory." width="880">
<br><sub>Every front end goes through the same operations to one <code>.scrutate/</code> directory.</sub>
</p>
`find QUERY` chooses: regex-looking → Engine A; otherwise ranked search when an index exists or
the repository is small enough to index on the spot; otherwise identifier grep. `grep --index`
sends an Engine A query through the text index. Both engines stay independently usable (`grep`,
`index`/`search`) and independently tested.

Every question a front end can ask of the index is an **operation** (`ops::Op`): its arguments,
defaults and limits live there, and it returns a typed **answer** (`ops::Answer`) with one text
form and one JSON form. The CLI's index commands (`read` among them) and ranked search, the MCP
server's nineteen tools, `batch` and the Python bindings parse into operations, run them through
`Repo` (`Repo::run`, or `Repo::run_as_is` after a scan of their own), and render answers; none has
a default or output format of its own (they had drifted: three different limits for `callers`,
references textual in one and resolved in another). The JSON form leaves in one envelope
(`Answer::envelope`: `schema`, `result`, `stopped_at`), which `--json`, a `batch` record and the
MCP structured result print (the MCP result adds `complete`, and on a page `offset`,
`next_offset` and `total`). A tool's JSON arguments are parsed in one place (`Op::from_tool`), used by the MCP server,
`batch`, the CLI's `read` and the bindings' `Repo.call`. The language server asks the same
operations for references, document symbols and the definitions of a name, and answers in LSP's
shapes; resolving the name under the cursor (`Names::at`) and the workspace symbol search
(`Index::symbols_containing`) are its own. An Engine A search is an operation argument too:
`Op::Grep` carries the `instant::Pattern` (mode, case, whole words) and an `ops::Scope` (a path
under the root, globs), so `grep_code` reaches every matching mode.

With a text index, `grep_code` and `references` (textual, and the name search of resolved
references) make a look of their own (one walk and one `stat` per file, no index update), read the
trigrams for the files the text index holds as they are now (the size, mtime and status change
time that look found) and read every other file of the look directly: new, changed, hidden or left
out by the text index's walk (`stores::grep_files`). The walk runs instead when there is no text index,
when the pattern's trigrams narrow nothing, when a glob brings files in, when a name is not UTF-8,
or when the path is a file or a directory the look did not enter. No grep builds a text index. A
look costs about what a walk costs without the search (on 12,001 small files, 90 ms of CPU for the
tool against 405 ms for `grep` over the tree).

A plain `scrutate grep` is not an operation: its flags are read once into a `Query` and a `Files`
and go to Engine A directly, with no record built (see Start-up above); with `--max-bytes` its
lines are paged by the operations' budget rule (`ops::paged_counted`). An answer renders within a
byte budget ([CLI](CLI.md#answers-from-the-index-one-definition-for-every-front-end)), because an
agent's harness cuts a long tool result where it likes (Codex at 10,000 tokens, from the middle).
It renders in one of two forms (`ops::Form`, the tools' `response_format`): `Answer::parts` builds
each answer kind's items once with a concise line set beside each item, and the page is cut by the
detailed items in both forms, so an offset means the same items. The concise lines are pieces
(text, a path, a location) that the page writes with each directory once and a location in the
file before it as `:start-end`; the sizes are in [CLI](CLI.md#serve-mcp) (`tests/concise.rs`).

`Repo` (`stores.rs`) is an open repository: the searcher on its index and the stores kept current
around it (see "Keeping the stores current" above). After an operation it checks the files the
answer cites and the files its arguments name against the index, one `stat` each, and updates
and reruns once when one changed ([CLI](CLI.md#serve-mcp)). `read_code` by path streams the file
through a 64 KB buffer and stops after the last line it shows. The MCP server, the Python
bindings, the language server and `watch` reach one through their session (`session.rs`). The MCP
server builds the first index in the background, answering "still building" after a 20 second
wait. A cancelled call sets the flag the index update checks between batches, so a `reindex` or
the scan before a read rolls back there.

The MCP server (`mcp.rs`) is an adapter: the tool table with its client hints (`readOnlyHint`,
`openWorldHint`) and output schemas, a short description per tool for `serve --short-tools`, server
instructions, and the protocol loop, which serves two eras on one pipe: the `initialize` handshake
of the revisions up to 2025-11-25, and revision 2026-07-28, where each request names its version in
`_meta` and nothing is remembered between requests. Each tool call runs on its own thread, so the
loop keeps reading and a `notifications/cancelled` finds the call it names. A result is text cut
to the byte budget; with `serve --structured` it also carries the envelope in `structuredContent`.
When both forms do not fit, that is a page: `Answer::text_page` says how many items the text
page shows, and `Answer::page_json` cuts the JSON form to the same items in the same shape,
walking the text's items in order (`ops::Walk`), so one `offset` continues both. A call with a
progress token runs beside a thread that looks at the call's `index::FileCount` every 50 ms and
reports it at most every half second; the session gives each call its count (`run_counted`),
the index update and `structural_search` add to it, and the first build counts into the
session's own (`Session::build_count`).
That is a switch because clients differ in which form they show their model, and the text is the
one written for a model. Flags and protocol details: [CLI reference](CLI.md#serve-mcp). The
Python bindings expose both engines with the GIL released and streaming results.

`lsp.rs` is a language server for editors (`scrutate lsp`), hand-written like the MCP server: a
`Content-Length` framed JSON-RPC loop on stdio that turns definition, references, workspace symbol
and document symbol requests into the index lookups of `symbol`, `refs` and `outline`, and
converts between the index's lines and the protocol's UTF-16 columns. Definition and references
first ask `Names` about the name under the cursor, so both name the same definition. `initialize`
finds the root with the CLI's own rule (`cli::index_above`), and a save updates the index
through the session's writer; the rest is in [CLI](CLI.md#lsp).

`setup.rs` writes an agent's configuration for `scrutate setup`. It makes the whole plan first (a
file from its old text to its new one, the agent's own command, a note) and writes nothing when a
file of it does not parse. JSON and JSONC files are edited on their text by byte span: one member
or list item goes in or comes out, and the new text must parse as the old value with that one
change. Codex's `config.toml` gets one table appended or cut out, checked the same way, and the
instruction block sits between two marker lines. `hook.rs` is Claude Code's Grep hook: it maps a
Grep call onto an Engine A query and file set the way Grep calls ripgrep (a `type` through
`Files::types`, which `grep -t` uses too), words the records as Grep words ripgrep's lines, and
bounds the result as an MCP answer is bounded; an argument it cannot map, or any error, lets Grep
run.

### WebAssembly (`wasm32-wasip1`)
The crate builds for `wasm32-wasip1`, where `std::thread::spawn` fails, and so does every scope
that spawns. Each branch for it sits under `cfg(target_os = "wasi")` beside the native code, which
it leaves as it is. The thread rules (`policy.rs`) take one thread as if asked for, so no file is
split into ranges and no read shared out. Engine A's `schedule` walks with the sequential walker
(`WalkBuilder::build`) and searches each file on the calling thread as it is found. A bounded
channel on one thread waits forever once full, so on wasi a file's records wait in a vector and
go to the caller before the next file is opened. `vendor/ignore` runs a parallel walk's one worker
on the calling thread, so `files`, the text index's walk and every other caller of `build_walker`
work unchanged. rayon's global pool, which the index build's `par_iter` uses, falls back to the
calling thread by itself. What must run beside the caller is left out: `serve`, `lsp` and
`tsquery` fail with a message that says so, `watch` too (wasi has no file events, so `notify` is
not a dependency there), and the Grep hook lets Grep answer, having no thread to keep its
deadline. The session socket is Unix only. `memmap2` maps nothing on wasi, so `npy::map` reads a
segment file whole when a map fails with `Unsupported`; every other error is reported as before,
and native builds keep mapping. wasi has no file locks or process id: the text index updates
without its lock when locking is unsupported (one process sees the files), and the temporary
names of the answer cache, the frecency state and a seeded index leave the process id out.
PCRE2's JIT has no wasm backend, so that build leaves out the `pcre2` feature:
`cargo build --release -p scrutate --target wasm32-wasip1 --no-default-features`, with the C
compiler of a wasi-sdk for SQLite and the grammars. The `wasi` job of `ci.yml` runs that build,
and runs clippy on the native build without `pcre2`, on every push.

## Models (`crates/scrutate/models/`, `models.rs`)
`semantic.npz` (7.6 MB: code-word vocabulary tables and projections, the two word tables as int8
codes with one float32 scale per row, quantised as the index's vectors are) and `ranker.npz` (87 KB)
are embedded in the binary with `include_bytes!`; `SCRUTATE_SEMANTIC` / `SCRUTATE_RANKER` can
point at other files or `none` (a vector model file with the float16 tables of revision 1 is an
error that names the file). `models.rs` holds them as one model set, with the feature list the
ranker reads (`FEATURES`) and the model revision. Each part loads on first use, once per process,
and a server's threads share it: `symbol`, `outline` and the other lookups never unpack either
file. A ranker whose feature names are not `FEATURES` is an error the moment a search ranks with
it; commands that do not rank never read it.

The index records the vector model's signature, a blake2b hash of the model file. The bundled
model's is a constant in `models.rs`, checked against the file by a test; a model named at run
time is hashed on first need and the hash kept in `.scrutate/model.sig` beside the file's size,
modification time and status change time. Another vector model makes the vectors
anew in the next update, inside its write transaction: each segment is written again with its
postings and the new vectors, or none for `none`, and no database row is rewritten. The vectors
come from the word counts of each file's artifact (`Artifact::vectors`), which a build with `none`
stores as well, so no source file is read or parsed: on this repository 1.39 s CPU where a fresh
build takes 4.12 s, on Guava 2.90 s of 9.46 s, nearly all of it vector arithmetic a fresh build
does too. A file whose content changed, or that names no artifact, is indexed whole; one that
names an artifact the store does not hold, or one that does not decode or fit its rows, is
damage, rebuilt by the one recovery rule (`stores.rs`). The index is then the one a fresh build
with that model writes (`tests/models.rs` compares every row and segment array). A switch that
stops halfway leaves the index as it was, and until one commits a search does not compare its
query with another model's vectors.

Training (numpy + LightGBM) lives in `bench/train_semantic.py` and `bench/train_ranker.py` and
reads its inputs from the engine: `bench/gen_features.py` indexes each benchmark package with the
binary and keeps the rows `batch --features` prints, and `train_semantic.py` reads the chunks,
view tokens and query terms of the hidden `views` command, which the benchmark builders also ask
for a file's language, test flag and definitions, a name's subwords and the query stop list.
`python/scrutate_ref` holds only what the trainers share: the ranker's export and the vector
model's tables. Ranking is watched by the pinned features above and by the quality floor of
`bench/quality.py` (docs/QUALITY.md).

## Design rules for the next decade
- Formats are versioned (`schema_version`, `.npy` headers, JSONL `schema`) and migrations are a
  clear-and-rebuild of a cache, never a data-loss event: the source tree is the truth.
- Components are replaceable behind small interfaces: a grammar is a registry row; the ranker is
  an array file; the vector model is an `npz`; the store is a directory of immutable arrays.
- No network, no execution of repository code, no background daemons: the engines are libraries
  that a CLI, an MCP server and Python call directly. `impact` runs the `git` program (`git.rs`) found on
  `PATH`, which reads the repository; nothing in the repository names a program to run. The one
  file written outside `.scrutate/` is the frecency state of `files`, under `XDG_STATE_HOME`. A
  command line call may use a session a running `serve` offers on a local socket, and never
  starts one.
- Everything that can be measured is measured by a script in `bench/` that anyone can rerun.
