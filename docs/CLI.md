<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-cli.svg" alt="Command line. What does each command do, and what does it print?" width="880">
</p>

# Command-line reference

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Install](INSTALL.md) &nbsp;·&nbsp; Next: [MCP server](../integrations/mcp/README.md)

**Contents**: [Conventions](#conventions) &nbsp;·&nbsp; [Engine A: grep](#engine-a-grep-alias-g) &nbsp;·&nbsp; [Engine B](#engine-b) &nbsp;·&nbsp; [Setup and the Grep hook](#setup-and-the-grep-hook)

<!-- End of head. -->

`scrutate --help` and `scrutate <command> --help` are authoritative; this page explains the model.

<p align="center">
<img src="assets/diagram-cli-map.svg" alt="The commands by family: search without an index, search the index, code intelligence, maintenance, servers." width="880">
<br><sub>The commands by family. The sections below are the reference.</sub>
</p>

## Conventions

<p align="center">
<img src="assets/diagram-exit-codes.svg" alt="Exit codes: 0 results found, 1 no results, 2 error, 130 interrupted." width="880">
<br><sub>Exit codes, as the first convention below states them.</sub>
</p>

- **Exit codes**: `0` results found, `1` no results, `2` error (bad arguments, unreadable index,
  invalid pattern, a `--root` that is not an existing directory for a command that opens the index,
  a missing search path, a `grep`
  of the current directory that searched no file...),
  `130` interrupted (Ctrl-C, or stopped early for a reason other than `--max-results`).
  `grep --type-list` and `grep --version` need no pattern.
- **Diagnostics**: an error is one stderr line (`scrutate: error: ...`), and an invalid regex adds
  the regex parser's lines that point at the fault; with `--json`, or
  `SCRUTATE_DIAGNOSTICS=json` for any command, one JSON record
  `{"type": "error", "schema": 1, "message": "..."}` (`"internal_error"` for a bug); a path that
  cannot be read is a warning (`scrutate: warning: ...`, `"type": "warning"`) and still makes the
  exit code 2. An argument
  the argument parser refuses (an unknown flag, a value that is not a number, given on the command
  line or by its variable) gets that parser's own lines instead (`error: invalid value ...` and a
  hint), with `--json` and `SCRUTATE_DIAGNOSTICS=json` too; the exit code is 2.
- **Streams**: results on stdout, diagnostics and progress on stderr. Piped output is plain
  `path:line:text`; colour only on a terminal, off with `--no-color` or `NO_COLOR` set, empty
  too.
- **Determinism**: `--sort` orders Engine A output as `rg --sort path` does: paths in
  command-line order, under each its files by path compared part by part, then by line
  (`sub/deep/a.txt` before `sub/deep-x/b.txt`, `foo/mod.rs` before `foo.rs`); a file two paths
  reach comes once under each. The default is completion order, for speed. `-c`, `-l` and
  `--symbols` always sort (without `--sort`, `-c` and `-l` compare whole paths as strings, all
  paths together). Engine B output is deterministic: what ranks equal is ordered by path, then
  line, so a tree answers the same however its index was built.
- **Machine-readable output**: `--json` on every command that prints results (`stats` always
  prints JSON). Engine A emits JSON Lines with `"schema": 1` and `"type"` ∈ {`match`, `context`,
  `count`, `file`, `binary`, `summary`, `symbol_group`}. Engine B commands emit one document,
  `{"schema": 2, "result": ...}`, plus `"stopped_at": N` when the answer stopped at its limit `N`
  with more left, and `stopped_reason` when a bounded read stopped short (`stopped_at` is then
  the count kept); a `batch` operation's record and the MCP structured result carry the same
  envelope. A definition such an answer lists (`search`, ranked `find`, `symbol`, `callers`,
  `callees`' `resolved`, `path`'s `from`, `to` and each step's `definition`, a `batch` search's
  `hits`) is an object
  with `id`, `path`, `start`, `end`, `name`, `kind`, `score` and `why` (what the score is made of,
  by name), plus `match` for a near match. The
  schema number changes only on incompatible changes (in schema 1 the index commands printed the
  bare answer, and `refs` and the `grep_code` answer carried their own `schema` key).
  `tsquery --json`, `check`, `stats`, `languages` and `config` print their own report. Engine A's
  records: `match` and `context` have `path`, `line`, `column`, `text` and `spans` (`start`,
  `end`); `count` has `path` and `count`; `file` has `path`; `symbol_group` has `path`, `symbol`
  and `matches` (`line`, `text`); `summary` has `files_searched`, `files_matched`, `matches`,
  `bytes_searched`, `errors`, `elapsed_ms` and `cancelled`; every record has `type` and `schema`. In
  `match` and `context` records, `spans` and `column` count bytes of `text` (an invalid byte
  prints as U+FFFD, three bytes) and a span never runs past `text` into the line end. Plain
  output (piped, without colour) prints a line that is not UTF-8 as its own bytes, as ripgrep
  does, so a script that feeds lines back (`sed`, `patch`) gets the file's text. An empty
  match keeps its byte offset as ripgrep gives it, inside a character too, when the line is valid
  UTF-8; in a line with an invalid byte it sits at the start of its character. The Python
  bindings' `Match.spans` and `Match.column` count the same way.
- **Paths**: `scrutate grep` prints a path as the walk reached it, as grep and ripgrep do:
  `dir/a.txt` for `dir/`, `./dir/a.txt` for `.`, absolute for absolute, relative to `.` with no
  path (text, `--json`, `-c` and `-l` alike; with `--symbols`, the path under `--root` the index
  names). Each line and count carries its path unless the only path given is one file; `-H`
  (`--with-filename`) and `-I` (`--no-filename`) choose, the later winning; `-l` and `--json`
  always carry it. `find`, the index commands, the MCP tools and the LSP server print paths from
  the repository root. On Windows `\` prints as `/` and a verbatim drive path (`\\?\C:\x`) as
  `C:/x`; on Unix a backslash is part of a file name. A non-UTF-8 Unix file name prints as its
  own bytes in piped output, `-l` and `-c` (so `scrutate grep -l x | xargs cat` opens it); in
  `--json` its `path` shows U+FFFD for the bad bytes and the record adds `path_bytes`, the name's
  bytes as an array of numbers. Windows names that are not valid UTF-16 print with U+FFFD. The
  index commands and MCP tools escape a control character in a name (`\n`, `\x1b`) and a character
  that reorders text or breaks a line (`\u{202e}`), double a backslash (`\\`), and every command
  that takes a path or location reads that form back.
  `scrutate grep` does the same in colour on a terminal and in `--symbols` text; piped, it prints
  the name as is, as grep and ripgrep do.
- **Configuration**: the nearest `.scrutate.toml` from the current directory up to the
  repository top (the directory holding `.git` or `.scrutate`), else
  `$XDG_CONFIG_HOME/scrutate/config.toml`; `$SCRUTATE_CONFIG` names a file, and with `--root` the
  file in that directory is used. `[index] exclude_dirs` is read only from the `.scrutate.toml`
  at the index root, so every command, the MCP server, the language server and the Python
  bindings leave out the same directories wherever they start; elsewhere (a subdirectory's
  `.scrutate.toml`, `$SCRUTATE_CONFIG`, your `config.toml`) it is ignored with a warning.
  `SCRUTATE_INDEX_EXCLUDE_DIRS`, when set, replaces it (empty: those directories are indexed).
  `[index] max_files` (`SCRUTATE_INDEX_MAX_FILES`) and `max_bytes` (`SCRUTATE_INDEX_MAX_BYTES`, a
  number of bytes or with a `K`, `M` or `G` suffix, `"4G"`) cap each index: the files it would
  hold after an update, and their total size (files over 1 MB, which the code index never reads,
  add nothing to its bytes; the text index reads every file). An update that would pass either
  stops before it writes anything, with exit 2 and one line naming the index, the count, the
  setting and its variable; the last committed index stays as it was and answers as before, and
  Engine A (`grep` without `--index`) is not limited. There is no limit by default. A
  `.scrutate.toml`'s globs match each path from the file's directory, whatever is searched and
  wherever the command runs; they never filter a path outside that directory or one named on
  the command line, and `-g` or `SCRUTATE_GREP_GLOBS` replaces them. `follow = true` in a
  repository's own `.scrutate.toml` is ignored with a warning: a search follows links only on
  your word (`-L`, `SCRUTATE_GREP_FOLLOW`, the file `$SCRUTATE_CONFIG` names, or your own
  `config.toml`). `scrutate config` names the file and shows what is in effect, with the
  directory the globs match from. An unknown field or invalid TOML stops every command that reads
  the file (exit 2, one line naming the file, the line and the fault); `--help`, `--version`,
  `config` and `completions` still run. `[[language]]` tables add languages
  ([below](#custom-languages)). Precedence: command line > environment variable > configuration
  file > built-in default. Every command-line option that has a variable names it in `--help` (e.g.
  `--hidden` ↔ `SCRUTATE_GREP_HIDDEN`). An on/off variable is off when `0`, `false`, `f`, `no`, `n`,
  `off` (in any case) or empty, on otherwise (`1`, `true`, `yes`, `on`), except `SCRUTATE_REDACT`
  and `SCRUTATE_HOOK_BASH`, whose rows under [Environment variables](#environment-variables) give
  their values. `--root` (`SCRUTATE_ROOT`) must be an
  existing directory for a command that opens the index (`grep --index` and `--symbols` among
  them), else it exits 2 and creates nothing; `grep` with paths does not use it. An empty value
  (`--root=`, `SCRUTATE_ROOT=`) is refused by every command, exit 2.
- **Running in a subdirectory**: without `--root` (or `SCRUTATE_ROOT`), a command that uses the
  index runs on the top of the current repository (the first directory upward holding `.git`, as a file or a
  directory) and says so on stderr (`[scrutate] using the index in <path>`). The first index is
  built there from any subdirectory, and the top's index wins over one an earlier run left in a
  subdirectory; with none at the top, an index in a directory between is used. A clone,
  submodule or git worktree inside an indexed checkout is its own repository with its own index
  (a linked worktree's starts as a copy of its main checkout's, see
  [The index directory](#the-index-directory)). Outside a repository the nearest directory above
  holding `.scrutate` is used, else the current directory; the climb stops short of the system's
  temporary directory (and `/tmp` and `/var/tmp` on Unix), the home directory (also one kept in
  git) and a filesystem root, whose index serves only that directory itself: a command in a directory below one runs as with no index above, and says in
  one line that it is not using it. A home directory kept in git is never
  a top. In the home directory or at a filesystem root with no index there, a command that would
  build a first index exits 2 and says to pass `--root` or run `scrutate index`; either of those or
  `SCRUTATE_ROOT` builds there as asked, and an index already there is used. A path argument is
  read from the current directory when it exists there, else as given (from the repository root,
  as answers print it). A file or location of another indexed repository, as `search --also`
  prints it (`../two/src/db.py:5-6`), is answered from that repository's index by `symbol`,
  `callers`, `callees`, `refs --def`, `outline`, `deps`, `rdeps` and `similar`; a file under no
  index `is outside the repository`. `grep --index` from a subdirectory answers for that
  subdirectory, and from a directory the index never enters (hidden or ignored) says so and exits
  2. `tsquery` walks from the current directory and keeps its cache in the root's index; `serve`
  and `watch` use the index above. The language server applies the same rule to each of the
  editor's workspace folders (else `rootUri`).
- **Shell completion**: `scrutate completions bash|zsh|fish|powershell|elvish`.
- **Cancellation**: Ctrl-C stops a search between files and exits 130, and a second Ctrl-C
  ends the process at once; a closed pipe
  (`| head`) stops it silently, with `-c`, `-l`, `--sort`, `--type-list` and `languages` too, and
  so does every other command (`impact`, `review` in any format, `refs`, `dead`, `--json`
  answers): no message, exit 0; any other failed write of the output (a full disk, a file size
  limit) is an error, exit code 2.

## Engine A: `grep` (alias `g`)

```
scrutate grep [OPTIONS] PATTERN [PATHS...]
```
With no path, `grep` searches standard input when it is a pipe or a file, else the current
directory; `-` names standard input among other paths. A command-line path that is not a
directory (a named pipe, `<(...)`, `/dev/stdin`) is searched as ripgrep does; a named pipe met
while walking is skipped. Standard input's lines carry the label `<stdin>` where a path would
stand (`-H`, `--json`); `--index` ignores standard input.

`PATTERN` is a regex unless one of these is given:

| Option | Pattern |
|---|---|
| `-F` | literal |
| `--ident` | identifier in any casing |
| `--fuzzy[=N]` | literal within N edits (default 1), at most 4096 characters; an edit is one character inserted, deleted or substituted, a byte of invalid UTF-8 counts as one, and `-i` and `-S` fold case as for a regex. Which lines match is decided first; a matched line's spans are the parts within N edits chosen the fewest edits first, then the leftmost, then the longest, none overlapping, and at each end of a span the character is matched or is a word character substituted, never a space, a punctuation mark or an inserted character ([ADR 0013](adr/0013-fuzzy-spans.md)): `--fuzzy=1 kmalloc` lights `kmallod` whole and `kmallo` of `kmallo(`, and with no edits the spans are `-F`'s. The column, `--json` spans, colour and the count of matches follow them |
| `--fuzzy-regex[=N]` | regex within N edits (default 1), an edit counted as with `--fuzzy`; a byte of invalid UTF-8 is one unit no position matches; classes (`\w`, `\d`, `\s` too) are Unicode, and `-i` and `-S` fold case as for a regex (`-i 'été'` finds `ÉTÉ`); no anchors or word boundaries, at most 64 character positions (a bounded repeat counts each copy), and a pattern within N edits of the empty string is refused. Spans are chosen as with `--fuzzy`, the parts within N edits of a string the regex matches; with no edits they are the leftmost-longest matches, which differ from ripgrep's leftmost-first only where it stops short: alternation, a repeat's optional copies, a lazy quantifier (`a|ab` on `ab`: ours `ab`, ripgrep's `a`) |
| `-P` (`--pcre2`) | PCRE2 regex, with look-around and backreferences |

The default regex engine runs in linear time and has no look-around or backreferences. PCRE2
backtracks, so a pattern can take exponential time; its match limit then stops the file's
search with a warning naming it. With `-P`, `--index` cannot narrow the files and searches all
of them. PCRE2 is the `pcre2` cargo feature, on by default. A build without it
(`--no-default-features`, as the `wasm32-wasip1` build is, since PCRE2's JIT has no wasm backend)
refuses `-P` and the `pcre2` mode (`grep_code`) with
`-P/--pcre2 (mode pcre2) is not in this build: it was built without the pcre2 feature`.

`-z` also searches zip and tar archives (members shown as `archive.zip{dir/file}`) and
compressed files: gzip in process; bzip2, xz, zstd, lz4, brotli and `.Z` through the system's
tools, as ripgrep does. Limits:

- an archive or compressed file stops with a warning at 8 GiB expanded or past a million members;
- a member or compressed file stops with a warning at a line longer than 64 MiB (not with `-U`,
  which holds the whole member);
- a tar member whose GNU long name or PAX path is over 64 KiB is skipped with a warning; other
  PAX records are read past and not kept;
- an old-GNU sparse member (`tar --sparse`) is skipped and the members after it searched.

An archive named alone is searched as a directory of its members, so each line names its member,
even the only one; a compressed file named alone prints no name, and its text with a NUL is a
binary match as a named file's is (a member, or a compressed file found by a walk, is skipped). Control characters and the
characters that reorder text or break a line in a member name show as escapes (`\n`, `\x1b`,
`\u{202e}`, a backslash as two), so a name cannot forge or disguise an output line;
`--json` keeps the name as it is. Archives inside archives are not opened, and `--index` does
not cover archive members.

`-e PATTERN` (repeatable) gives several patterns; a line matching any matches, and the first
positional argument is then a path. The argument after `-e` is the pattern whatever it starts
with (`-e '->'`, `-F -e -1`), as in grep and ripgrep. A walk skips the file standard output
writes to, as ripgrep does, so `grep foo . > out.txt` does not search its own output; so does
`grep --index`, also when that file is in the text index.

Options mirror grep/ripgrep where the meaning is the same: `-i`, `-S` (smart case), `-w`, `-v`,
`-l`, `-c`, `-A/-B/-C`, `-m`, `--max-results`, `--hidden`, `--no-ignore`, `-L` (follow links),
`--max-filesize`, `--max-depth`, `-j THREADS`, `--sort`, `--json`. As in ripgrep an option given
twice takes its last value (`-m1 -m2` stops at two), and one that adds (`-e`, `-g`, `-t`) adds
each. Also:

| Option | Notes |
|---|---|
| `-U` | multiline: each line of a multi-line match is printed with its own number; `-c` counts each match, with `$`, `\B` and `-w` read against the text after the matched lines, as ripgrep counts |
| `-g GLOB` | `!` excludes |
| `-t TYPE` / `--type-list` | `-t !TYPE` excludes (ripgrep's `-T`). As in ripgrep a type matches the file name after the ignore rules: a gitignored file of the type stays out, a hidden one comes in, a glob that matches a file decides before the type, and a file named on the command line is searched whatever its type. A language's type also takes the extensions it shares with another language, every file of them, whatever its content: `.h` for `cpp` and `objc` as for `c`, `.m` for `objc` and `matlab`, `.pp` for `pascal` and `puppet`, as ripgrep's `cpp` takes `.h` |
| `-l` / `-c` | the files with matches, or each file's count of matching lines; the later wins (`-c -l` lists files, `-l -c` counts), as in ripgrep |
| `-n` / `-N` | line numbers or none; numbers by default, to a terminal and to a pipe or file alike (see below); the later wins, and `--json` keeps them |
| `--max-bytes BYTES`, `--offset N` | a bounded answer in path order, see "Answers from the index" below |
| `--stats` | with `-l` each file is searched only to its first match, so it prints `N of M files match`; the JSON summary of `-l` gives `"matches": null`, with `--stats` or without |
| `--symbols` | groups lines by the enclosing function using the index, which it builds or brings up to date first (`[scrutate] indexed N changed files` on stderr); a file whose search a NUL byte stopped after a match ends its last group with a `note:` line, `"note"` in the JSON `symbol_group` |

Each short flag has the long name `grep --help` lists: `-e` `--regexp`, `-F` `--fixed-strings`,
`-P` `--pcre2`, `-z` `--search-zip`, `-i` `--ignore-case`, `-S` `--smart-case`, `-w` `--word`, `-U`
`--multiline`, `-v` `--invert-match`, `-l` `--files-with-matches`, `-c` `--count`, `-H`
`--with-filename`, `-I` `--no-filename`, `-n` `--line-number`, `-N` `--no-line-number`, `-A`
`--after`, `-B` `--before`, `-C` `--context`, `-m` `--max-count`, `-g` `--glob`, `-t` `--type`, `-L`
`--follow`, `-j` `--threads`. ripgrep's `--after-context`, `--before-context` and `--word-regexp`
are taken too, for `--after`, `--before` and `--word`.

Threads: `-j`, `SCRUTATE_THREADS` and the configuration file's `threads` take at most 256. The
default is the cores, at most 12 (on macOS the performance cores), and more when the files come
from the disk, a cold cache: up to 64 on Linux, elsewhere twice the cores, at most 24. On macOS,
one user's concurrent searches split the default threads and take them back as the others end:
each gets the default divided by the number running, at least 1 (py-big, four at once: 244 ms
each before, 185 ms now; one alone is unchanged). A search that walks a directory with
subdirectories, or more than 256 files, or searches a list of more than 256 files (`--sort`,
`--index`), joins that count when it starts (a flat directory of fewer files runs on at most 3
threads uncounted), so four
searches of a small repository started together run on one thread each (13.3 ms each, GNU grep
14.5 ms; before, 20 threads on 5 cores and 14.7 ms). Not counted: a search given a thread count
(`-j`, `SCRUTATE_THREADS`, `threads`), which keeps it, and, until it goes on, one waiting on a
reader that does not read or stopped with Ctrl-Z. One stopped with SIGSTOP, which no program can
see coming, stays counted until it continues or ends. The count is a System V semaphore of the
user, listed by `ipcs -s` until the machine restarts; a killed search leaves nothing in it, and
where the system gives none a search runs as it would alone. `SCRUTATE_GREP_DEBUG=1` prints the
count and the threads taken on stderr each time they change; `SCRUTATE_COUNT_KEY=N` (a
number) keeps a count of its own, apart from the one the user's other searches join, as the
tests of the count do.

Files that cannot be read, or not to the end, are named in stderr warnings (the first ten), and
the exit code is then 2, as ripgrep gives it, after any matches: a search path that does not
exist, a file or directory without read permission, named or found by the walk, a dangling link
or a loop that `-L` follows, an archive member not searched to its end, and PCRE2's match limit.
Matches in the other paths still print; exit 1 means only that nothing matched. A `grep` given
no path that searched no file, its `-g`, `-t`, `--max-depth` or ignore files having left out
every one, says so on stderr and exits 2, as ripgrep does; with a path given it exits 1. On Linux, files on `/proc`, `/sys` or
another kernel pseudo filesystem (debugfs, tracefs, securityfs, configfs, cgroup) give their size
as 0 and can answer the first read short, so they are read until the kernel has no more. The walk
finds them by one `statfs` per root and per directory named `proc` or `sys` below one (with
`-L`, a link of another name to such a directory is not checked).

The ignore files are `.gitignore`, `.ignore`, `.rgignore`, the global git excludes and
`.git/info/exclude`, read as ripgrep reads them (`.rgignore` before `.ignore` before
`.gitignore`); `--no-ignore` turns them off. As in ripgrep, `scrutate grep` searches
`node_modules` and the other directories the code index skips by name, unless an ignore file
excludes them.

Where tools differ in the details, scrutate does what ripgrep does:

| Case | Behaviour |
|---|---|
| `-m N`, `--max-results N` | `-m N` stops a file at its Nth matching line; the trailing context after it prints, and a match inside that context shows as a match. `--max-results N` stops after N matching lines in total, with `-c` and `-l` too; with `--sort` they are the first N in path order, the same every run. The trailing context of the Nth prints too, as `-m N` prints its own: a match inside it shows as a match and does not count toward N |
| NUL byte | A file named on the command line is searched as ripgrep searches it. Where ripgrep maps it into memory (Linux, Windows) it is one slice: a NUL in its first 64 KiB is seen before any line prints, past that the first matched or context line holding a NUL ends the line output, and a NUL ends no line (`-v -c` counts `bar\0baz` as one line). On macOS, where ripgrep does not map a file, it is read, and a NUL ends a line. A process that stays up (`serve`, `lsp`, a session) maps no file: it reads a named file whole up to 64 MiB, the same slice, and reads a larger one as on macOS. Either way counts and lists include the file, and line output ends with `binary file matches (found "\0" byte around offset N)` (with `--json`, a record `{"type": "binary", "path", "text"}`). A file the walk finds with a NUL byte is searched as ripgrep reads it, up to the read that meets the NUL; ripgrep's first read of a file is its first 3 bytes, taken alone by its BOM check (encoding_rs_io's `BomPeeker`), so a match on a line that ends within them prints and one further on does not (`x\r\n...foo x...\0` prints line 1 only). Such a file is left out of `-c`, and `-l` lists it only when a match came before that read; with `-v`, the lines before that read print, while `-v -l` and `-v -c` leave the file out, as ripgrep does. Where such a file stops at a NUL past its first 64 KiB does not depend on the files searched before it: ripgrep's reads grow with a line longer than its 64 KiB buffer, and every later file on that thread then stops further on, so its answer changes with the walk order and the thread a file lands on (`rg -j1 PAT long/ binary/` against `rg -j1 PAT binary/`). scrutate gives every file the answer ripgrep gives it searched alone. When such a file's search stops at a NUL byte after a match, a last line says so: `path: WARNING: stopped searching binary file after match (found "\0" byte around offset N)` (a `binary` record with `--json`) |
| `-c`, `-l` | one record per path given: a file two paths reach is counted or listed twice |
| CRLF line | printed as it is, its `\r` before the `\n` (`1:foo\r`), streamed, sorted (`--sort`), cut (`--max-bytes`) or in colour alike; the text of a `--json` record or an MCP answer has no line end |
| `-i`, `-S` | smart case folds only a pattern that has a literal and no uppercase letter, so `-S '\p{Ll}'` stays case-sensitive and matches lowercase letters only. The later of `-i` and `-S` wins; `SCRUTATE_GREP_SMART_CASE` applies only when neither is given |
| `-A`, `-B`, `-C` | `-A` and `-B` win over `-C` in either order, as in ripgrep (`-C1 -A3` shows one line before and three after); a line `--` separates groups that are not adjacent, and different files (not in `--json`). With context on several threads a file's lines leave a group at a time, so another file's lines never stand between them; a file searched alone still streams |
| `-g` | one glob per value, commas included (`-g '*.{txt,log}'`); `SCRUTATE_GREP_GLOBS` is split at commas outside braces |

Six differences:

- A `-g` glob with a slash (`-g 'sub/*.rs'`) applies under every searched path, where ripgrep
  matches it from the current directory.
- A `.scrutate.toml`'s globs match from that file's directory (see Configuration above).
- Line numbers print by default to a pipe or a file too, where ripgrep prints them only to a
  terminal (`1:foo` against `foo`): an agent reads that output, and the number lets it open the
  file at the line. `-N` turns them off; the Bash hook's `rg` rewrite adds it, so a rewritten
  `rg` prints what `rg` prints.
- Where a walked file stops at a NUL past its first 64 KiB is ripgrep's answer for that file
  searched alone, whatever was searched before it (see NUL byte above); ripgrep's own answer
  changes with the walk order and its threads.
- A UTF-16 file of odd length: its last byte, which has no pair, prints as a whole U+FFFD in every
  search. ripgrep 15.2.0 sometimes prints that U+FFFD cut short, to `EF` or `EF BF`, which is not
  UTF-8, under `-v -U` with a pattern that can match a line break. It does so reading a named file
  with `--no-mmap` (`rg --no-mmap -v -U 'x\ny'` on the bytes `FF FE` and ` bad \r\n`), and on some
  runs or on every run for a file found by a walk; it printed the whole U+FFFD for a mapped file in
  every run measured.
- On Unix, a file the walk finds that is one of scrutate's index files under another name (a hard
  link, either way round) is left out, where ripgrep searches it, so no answer carries text from
  the index. The walk says so once on stderr, `scrutate check` lists each such file, and a path
  that names one is still searched.

`--index` answers from Engine B's text index (`scrutate index`): its trigram postings name the
files that can contain a match, and Engine A's matcher searches only those, so the output is what
the walk prints for the indexed snapshot of `--root`. Run `scrutate index` (or `index --text`)
after changing files; the update is incremental. Options that change which files a walk visits
(`--hidden`, `--no-ignore`, `-L`, `--max-depth`, `--max-filesize`, explicit paths) are refused
with `--index`; `-g` and `-t` filter the indexed files, so a hidden file a type or glob brings
into the walk is not searched (the index holds no hidden file). `--index --update` first brings
the text index up to date (building it if there is none; only new and changed files are read),
so the answer is the walk's for the tree as it is now: the call for an editor that searches often
while files change.

## Engine B

| Command | Purpose |
|---|---|
| `index [--force] [--text \| --no-text]` | Build or incrementally update `.scrutate/`: the code-intelligence index and the text index for `grep --index` (`--text`: only the text index; `--no-text`: skip it). Progress on a terminal. |
| `search QUERY [-k N] [-a ALT]... [--also ROOT]... [--why] [FILTERS] [--json]` (alias `s`) | Ranked search by meaning, name or identifier; `--alt` fuses alternative phrasings; `--also` searches other repositories too: their results print by the path given (`../two/src/db.py:5-6`), this repository's as without it, and a root given twice is searched once; with `--json` each result keeps its path in its repository and gains `root`. A definition matches by its name, signature, body and documentation: the comment block right above it plus any docstring, also across attributes, decorators, annotations or type specs (Rust `#[derive(...)]`, `@Get(...)`, `@override`, Elixir `@spec`, Erlang `-spec`), and the language's own form where it has one (Elixir `@doc`, a Julia docstring, Haskell haddock, PowerShell's help block); not a comment a blank line away, a shebang, or a comment on the enclosing definition's header line. A query of one word, or of stop words alone, is also looked up as a name (`search find`). A name typed with its case ranks the definition spelled that way first (`search BurstFiles`: the struct before `burst_files`; typed in a style neither uses, `burstFiles`, both come first), and a struct and its `impl` block are separate results. Every line of an indexed file can be found: a line past what a definition's chunk keeps, or code outside any definition, is found as a window named after its definition or file (`long.py:885-934  [window] very_long`), and a definition and its windows give one result, the best-matching part, with its lines. Vendored code, which a repository holds but did not write, ranks lower than the project's own by a fixed margin, so a clearly better vendored match can still come first: `vendor/`, `third_party/` and `third-party/`, `node_modules/`, `bower_components/`, `jspm_packages/`, `Pods/`, `Carthage/`, `Godeps/_workspace/` and minified `.min.js` and `.min.css` files (a subset of GitHub linguist's `vendor.yml`; its broad patterns such as `dist/`, `deps/` and `cache/` are left out). A directory the repository's root manifest builds as its own package is the project's code, under these names too: a Cargo `[workspace] members` entry (globs too) or a `path` dependency of `Cargo.toml`, a `replace ... => ./dir` of `go.mod`, a `workspaces` entry of `package.json`, a `packages` entry of `pnpm-workspace.yaml` (each a regular file of at most 1 MB at the root, read again when one of them changes, so a member added while `serve` or `lsp` runs counts); an entry that starts with `!` takes its directories out of that manifest's list again, so `'!vendor/x'` leaves `vendor/x` vendored unless another manifest (a Cargo workspace member there) owns it. In `package.json`, as npm reads it, a `!` entry that a later entry matches is dropped (`["vendor/*", "!vendor/x", "vendor/x"]` keeps `vendor/x`); pnpm keeps every `!` entry wherever it stands. It is never left out: a name only vendored code defines, searched for as itself, keeps its place. In `--json`, and in the MCP tools' structured results, every item of any answer whose `path` is vendored code has `"vendored": true` (a `dupes` member, which is an array, has no such mark, in `review`'s `new_duplicates` too); in text, every line whose location names such a path carries the mark too: `[function, vendored]` after a location followed by its kind, `(vendored)` at the end of any other line (the concise form as well; `review`'s Markdown and GitLab reports carry no mark). A mark never shares a line with a file's text: a file's lines (`read`, `read_code`) and the code an answer quotes (a hit's snippet, a definition's body, a grep line after its number) are never marked, so they read as the file has them, and a line that quotes code after its location (`path:12:5: code`, as `refs` writes an occurrence) keeps that shape, its vendored file named instead in one `vendored: PATH, ...` line at the end of the answer. |
| `find QUERY [-k N]` | Unified: regex-looking → `grep`; otherwise ranked search (indexing a small repository on the spot); otherwise identifier grep. A regex-looking query that finds nothing, or is not a valid regex, goes to ranked search when an index exists. A question with a space and a trailing `?` (`how is the token refreshed?`) is taken as words. It takes no filters: it can answer by `grep`, whose lines have no definition kind. |
| `symbol NAME [--limit N] [--exact] [FILTERS]` | Where it is defined (`Class.method` works). The name is first looked up as typed: a whole stored name (`list->vec`, `Foo::Bar`, `#main`), a qualified name, or the end of one. Only when that finds nothing are `Type::method`, `Type#method` and `p->method` read as `Type.method`; then a stored owner is compared without its generic arguments and path (`Stack<T>.push`, `crate::config::Config::new`, `App\Models\User::find`); last, for a name written as a path, a top-level definition of its last part. A leading dot is dropped (`.helper` as `helper`). Names match ignoring case and `_`, in this order: the spelling as typed before namesakes in another case, then definitions outside test files, then types (`class`, `struct`, `enum`, `trait`, `interface`, `type`, `impl`) before functions and methods before the rest, and a definition with a body before one without. `callers`, `callees`, `similar`, `refs` and `read_code symbol` take names the same way. A method is named by its owner's bare type: `Stack.push` for `impl<T> Stack<T>` in Rust and for `void Stack<T>::push()` in C++. A dotted Lua or Elixir name is split off its owner: `function M.helper` is `helper` with the qualified name `M.helper`, `defmodule MyApp.Accounts.User` is `User`. A method of a JavaScript or TypeScript object literal, or a property whose value is a function, is named under what holds the literal: `api.get`, `store.sub.m`, `tool.build` for `module.exports.tool`, also through `as`, `satisfies` and parentheses; a literal nothing names keeps the bare name. A C or C++ function declared without a body (at file scope, in a namespace, in `extern "C"`, inside an include guard) is a `function` symbol, so a prototype and its definition are two symbols, the definition first; a method declared in its class and defined outside it is likewise two, the declaration (`method`) and the definition (`function`). An Objective-C method declared in an `@interface` is a `method` symbol beside its definition, and a function pointer typedef (`typedef int (*cb)(int)`) is the type `cb`. At most 20 definitions, or `--limit`. When no definition has the name, the near matches are listed, each saying how it matched (`(prefix match)`; `match` in JSON, MCP `find_symbol` and Python `Hit.match`): names that start with the query (`BookingsServ`), then names whose first words the query's parts start, one part a word, in order, a run of digits a word of its own (humps: `BkSvc`, `bk_svc` or `BS` find `BookingsService`, `Svc` does not; `make_widget_2` does not find `make_widget_7`), then names that hold the query (`ingsServ`), this last only for a query of three characters or more once case and `_` are dropped (`s_e` counts two); prefix and contains matches shorter names first, humps matches fewest words first and then shorter, case and `_` ignored. `--exact` (MCP `exact`) lists only definitions of the name. `--deps` (MCP `scope: "deps"`) finds a definition of a dependency, at the version its lockfile pins ([below](#--deps-a-dependency-at-its-locked-version)). |
| `callers NAME` / `callees NAME` (`--limit N`) | Call graph edges resolved to definitions (callee names outside the repository listed after `external:`; a function whose callees are all external lists them with no `(no results)` above). `callers` lists first the callers the resolver ties to the definition, then those it could not decide, and says how many are undecided. A call in a language the resolver reads that it could not place (`tx.foo.update()` on an untyped `tx`) is undecided, tied by its name only, also when no other definition has the name, so `callers update` and `callers ProviderProfile.update` agree; in a language the resolver does not read, such a call is decided when the name has no other definition; callers outside tests come first, then by path and line, and when the name has another definition in the repository, those that name its owner type or sit in its package come first. So an index brought up to date answers as a fresh index of the same tree does. A call written outside every definition (a script's main block, a shell script's last `main "$@"`, top-level JavaScript) belongs to the chunk that holds its line: the module chunk before the first definition, else the first window that holds it. `callers` lists that chunk (`job.py:5-6  [window] job`), the resolver decides the call as any other, and `callees FILE:LINE` for a line outside every definition answers for that chunk. A call matches a definition by the name as the call writes it (`Ok(1)` is not a call of `ok`), in any case where the language ignores it: a Fortran (a build with `--features extra-langs`), Pascal or Ada call, and a PHP call of a function or method (`FormatPrice(3)` calls `formatPrice`), and only in its own language family: JavaScript and TypeScript; C, C++, CUDA and Objective-C; Java, Kotlin, Scala and Groovy; Erlang, Elixir and Gleam; C# and F#; every other language alone (a file of no known language matches any). CUDA, F#, LLVM IR, OCaml, TLA+, Verilog and VHDL, named in this row, are parsed only by a build with `--features extra-langs`: without it their files get only keyword-found definitions and no call edges ([Languages](LANGUAGES.md)). An unknown qualifier (`Nope.refresh`) gives no results, and a leading dot is dropped (`callers .helper` as `callers helper`). A call the resolver ties to something else (a function of an imported module such as `subprocess.run`, a local variable, a field) is not a call of a namesake in the repository; `callees` lists such a name after `external:`. A call through an import alias is a call of what the import names: with `from util import helper as h`, `h()` is a caller of `helper`, `callees` of its function lists `helper`, and `refs helper` lists the call too (Python, JavaScript, TypeScript, Rust and PHP, where the import names the item or the module of its file). In a language the resolver does not read (every one outside Python, JavaScript, TypeScript, Rust, Go, Java, PHP, C, C++ and C#), a call without a receiver runs the definition in its own file when there is one, a free function of that file or a method of the caller's own type: `callees` lists that one alone, and `callers` of a namesake in another file leaves the call out (`helper()` in `e.rb` calls the `helper` of `e.rb`). For any other call the resolver cannot place, `callees` lists up to three definitions of the name, the caller's file first, then its directory. A file whose extension two languages share (`.m`, `.h`, `.pp`, `.cls`) is put in its family by its text. A Nix function, which is a key, is listed as a callee, as is a macro of the repository (`#define SQUARE(x)`, `macro_rules! square`, a Lisp `defmacro`). A method call `x.m()` is never a call of a free function `m` in Rust, JavaScript, TypeScript, Go, Java, C, C++ and C#, a receiver whose type comes from outside the repository makes the call external, and a definition's own name is not a call of it. Calls inside the arguments of a Rust macro (`println!("{}", f(x))`, `vec![k(1)]`) count. A method call records the method, not the receiver: Lua `obj:meth()`, Objective-C `[obj meth]`, R `obj$meth()`, PowerShell `$obj.Meth()`, PHP `$o?->m()`; `new Point()` in C++, Scala, Groovy, Dart, D and Solidity records `Point`. A Lisp name is recorded whole (`list->vec`, `set-car!`), a JavaScript private method with its `#`. A call is recorded under its definition's name: a Ruby or Elixir name keeps its trailing `?` or `!` (`valid?`, `save!`), a Julia name its `!` (`update!`), an R name its dots (`is.ready`), a Bash name its `::` (`ns::helper`), a Haskell, OCaml, F# or Nix name its primes (`step'`); a PHP call through a namespace is recorded by its last part (`\App\Util\helper()` calls `helper`); a Rust raw identifier without its `r#` (`r#type()` calls `type`), and `symbol` and `callers` take either spelling. Dart, Groovy, Elixir, LLVM IR (`call @g`), TLA+ and Verilog (`g(x)` in an expression) record calls, and so does a Pascal statement that is a name alone (`DoWork;`, `Obj.Tidy;`); an Elixir `def`, `use`, `alias` or `if` and the head of a definition are not calls. Verilog task calls (`h(1);` and, in a block or a task body, `h;`) and module instances, VHDL calls in a subprogram (a subprogram with a body is a function of its package body or architecture) and the entity or component an instance names (`u1: entity work.adder port map (...)` records `adder`), Jsonnet calls, Make `$(call g,...)` and assembly `call`, `bl` and `jal` targets record calls; an assembly label holds its instructions up to the next label or section, so a call under a local label (`.L2:`) counts for that label and for the global label above it, whose code the local label is part of. `a(i)` of a signal, variable, constant, file or port that the same VHDL file declares is no call (the grammar gives it the shape of `f(x)`), compared without case. Not recorded: a Verilog call on a member (`obj.m(1);`), which the grammar parses as an error; a VHDL indexed name declared in another file reads as a call. A process that answers many requests (`serve`, `lsp`, `batch`) keeps each file's parse (its text, tree and imports, found again by the hash of its bytes) for the next request, up to 256 MiB of memory (a tree counted at an estimate of 90 to 176 bytes a node by its grammar, the most measured for that grammar, and 176 for one not measured: [ADR 4](adr/0004-parsed-files-kept-across-requests.md) has the table), the least recently used going first: `callers` of another name in the same files parses none of them again. Within one request the resolver holds the files it read under 256 MiB more (their text, imports and trees), the least recently used going first and read again when asked for, so `refs mutex_lock` on the Linux tree holds about 670 MB where it held 6.2 GB. Beside that set it keeps, for the rest of the request, what it learned of each file it read (definitions and imports, checked against the hash of the file's bytes) and the instantiations a class's name was found at, up to 16 MiB each: a file dropped from the set is read and hashed again but its definitions and imports are not looked up again, and six classes of one name are looked up once. On Linux with glibc, `serve`, `lsp` and `batch` give the memory a request freed back to the system after its answer, at most once a second for short requests ([ADR 8](adr/0008-resolver-reads-bindings-from-the-index.md)). A file whose bytes the index parsed brings its imports from the index, and its tree is built only when the resolver needs it: a C name looked up through a file's includes parses none of the headers between, and `refs kmalloc_array` on Linux parses 280 files where it parsed 3,272. `callers` asks the resolver about every call site and keeps what it learned (`.scrutate/answers/`), so the same `callers` or `callees` again on an unchanged index and unchanged files resolves nothing (`callers size` on Guava, 1,970 call sites: 1.4 s of CPU the first time, 0.43 s from what was kept, where asking about the 400 likeliest cost 0.6 s). A call of a class is a call of its constructor: `callers Point.__init__` lists `Point(1)`, and likewise a JavaScript or TypeScript `constructor` (`new Widget()`), a Java, C# or C++ constructor, PHP `__construct`, Swift `init`, and Ruby `initialize` through `Invoice.new` (`callees` lists the class `Invoice` for it). PHP `new self()` and `new static()` are calls of the class they sit in and `new parent()` of its base, in any case (`callers Kid` and `callers Base` list `Kid.run`). A JavaScript or TypeScript `super()` in a subclass constructor is a call of the base class (`callers Base` lists `Kid.constructor`, `refs Base` the `super()`), also in a class expression (`const Kid = class extends Base {..}`). A Java call its arguments do not tie to one overload is a caller of each overload. A call through a base class, an interface, a trait or an abstract method that the target overrides or implements, at any depth, is an undecided caller of the target (`b.size()` with `b: Base` for `Big.size`); a call that resolves to a sibling's override is not. A variable declared as the base type and built by a constructor in the same function (`Base b = new Big()`, `const b: Base = new Big()`, `b: Base = Big()`, Go `var s Sizer = Big{}`, Rust `Box::new(Big)`) has the constructor's type until assigned again, so its call is a decided caller of the override. `callees` of a function that calls through the base type lists the base method only. A `super` call (Java, TypeScript and JavaScript `super.m()`, Python `super().m()` and `super(C, self).m()`) is a decided call of the nearest base method that defines it, by Python's method resolution order (`super(C, self).m()` takes the first class after `C` in the order of the class the call sits in, and stays undecided when `C` is not in that order), never a call of the override it sits in; with a base from outside the repository it is external. Go `x.Embedded.M()` resolves through the embedded field. |
| `path FROM TO [--depth N]` | The shortest chain of calls from FROM to TO, each a name, `Class.method` or a location (`path:line`) as `callers` takes them. The search goes breadth first over the calls `callees` gives for each definition, at most `--depth` calls deep (default 10): first over the calls the resolver ties to a definition, and only when those reach no chain, over every call `callees` lists, where a step the resolver did not tie ends `(by name: the resolver did not tie this call)` (`"tied": false` in JSON). The first line is FROM's definition, then one line per call: the call site and the definition it reaches (`  a.py:6  calls b.py:4-5  [function] middle`). Of several shortest chains, the one through the definitions first by path and line wins, and of one definition's calls the first by line. The call site is the first line where the resolver ties the call to that definition, else the call's first line. No chain: `(no call path from A to B)` and exit 1; when the depth stopped the search with definitions left to follow, the line ends `within N calls: raise depth to search further`. A name with several definitions is an error (exit 2) that lists them (up to 20) with their locations: pass one as `path:line`. `--json` gives `from`, `to`, `found`, `depth`, `stopped` and `steps`, each step with `path`, `line`, `definition` and `tied`. |
| `map [PATH]` | The repo map: the files that define code, ranked by PageRank (damping 0.85, 50 iterations) over the edges between files that the index's names give. A call of a name goes to every other file of the caller's language family that defines it, its weight split among them, unless the caller's own file defines it; an import goes to the file its module path maps onto, as `deps` maps a module for a language the resolver does not read. No call site is resolved and no file parsed: resolving every call site, as `callees` does, took 188 s of one process on Guava (3,364 files) where the map takes 1.1 s of CPU, and 2.0 s on this repository's `crates/scrutate/src` where it takes 0.02 s. Of two files with the same score, the one with more edges in and out comes first, then by path, so one index gives the same bytes on every run. Each file is listed as `path  (rank N, score S)` with up to 5 definitions, one line each (`  1-2  [function] helper  (6 calls)`): most called first (the name's calls split among the files that define it, rounded), then outer before inner, longer before shorter, and by line; overloads of one name are listed once. `PATH` keeps the files under one directory, and a `PATH` that is not there is an error; the scores are the whole repository's, and rank counts from 1 within `PATH`. `--json` gives a list of files, each with `path`, `rank`, `score` and `definitions` (`id`, `path`, `start`, `end`, `kind`, `qualname`, `calls`). |
| `refs NAME [--def PATH[:LINE]] [--textual] [--max-results N] [FILTERS]` | Every whole-word occurrence, resolved to the definition it refers to or listed as unresolved with a reason (see below); in any case, in a file of Fortran (by its extension where the build has no Fortran grammar), Pascal, Ada or PHP, for a name that a definition in one of those languages has (a PHP function, method, class, interface, trait or enum). It takes the names `symbol` takes: a qualified name (`Stack::push`) keeps the definitions `symbol` finds for it and counts `--max-results` over their uses, where a bare `push` lists every definition of the name. `--max-results N` stops after N uses, every use on a line counted, and a stopped answer says so. A use on a line longer than 400 bytes shows the 300 bytes around it, with `...` where the line is cut (`"cut": true` in JSON; `column` stays the byte column in the whole line). |
| `read [PATH] [--symbol NAME] [--start N] [--end N] [--history [N]]` | Lines of a file, or with `--symbol` whole definitions (a name, `Class.method` or a location; `PATH` then keeps those in that file): the MCP `read_code` tool, with its arguments, limits and answers (see `serve` below). A path is read from the current directory, as the other commands read one. Reading a file by its path does not update the index. With `--symbol`, `--history` lists under each definition the last N commits (5 when no N is given, at most 50) that changed its lines, newest first: an id, the author date and the subject, no author name. They come from `git log -L` on HEAD's copy of the file, which follows the lines through renames; when the file has uncommitted edits, the definition's lines are found in HEAD's copy through them, and a definition whose own lines changed since HEAD says so (`changed since HEAD`), as one HEAD lacks does (`not in HEAD`). Each definition costs a run of git (they run at once), so the first 10 shown get a history and a last line says so. `git log -L` walks the file's whole past, whatever N is, so a lookup stops after 5 s with the commits git had printed by then (the newest) and a note: `git commit-graph write --reachable` makes it fast, and scrutate never writes that graph itself. A scrutate killed during a lookup leaves that git to finish its read-only walk. In a shallow clone git gives every older line to the commit the history stops at, which may or may not have changed them, so that commit keeps its subject and is marked `(history cut here: shallow clone: this commit or an older one changed these lines)` (`cut` in the JSON), and the note says it is the oldest commit here. In a partial clone that lacks the blobs git needs, git fetches none of them (git 2.44 and later) and the note says so, as `impact` does: `this partial clone lacks objects that git log needs: fetch them first`. Outside a git repository, without git, or with a git older than 2.25 (the oldest tested: 2.25, 2.30, 2.34, 2.39 and 2.53 follow renames and print no patch), the answer is the same with one line saying why it has no history. A git that fails is named in the note with its reason or how it ended (`git could not follow these lines: git log: ended by signal 11 (a crash) and gave no reason`). `--deps` reads `--symbol PACKAGE.NAME` from a dependency ([`--deps`](#--deps-a-dependency-at-its-locked-version)). `--json` gives `text`, `start`, `end` and `file_lines` for a file's lines, and with `--symbol` `definitions`, each with `id`, `path`, `start`, `end`, `kind`, `qualname` and `text`. |
| `outline FILE` | Symbols of a file in source order. `--json` gives each with `kind`, `name`, `qualname`, `start` and `end`. |
| `deps FILE` / `rdeps TARGET` | Imports of a file (resolved to repository files when possible) / files importing a module or file. Imports resolve as in `refs`: Rust `crate::`, `self::` and `super::` paths and `mod` declarations (`deps` lists a `mod` line too, and a `#[path]` one names the file it gives; `rdeps` of that file lists it), Go imports through `go.mod`, also from a module in a subdirectory, Python `from . import b`, TypeScript `./lib/c.js` to `c.ts` and `./types` to `types.d.ts`. `deps` gives one line per file a statement names. An `#include "util.h"` resolves to the file next to the including one; `rdeps` matches an import by its base name only when it resolves to no file. When nothing resolves to the target but some imports could not be resolved, `rdeps` says so: `no import resolves to X, but N imports could not be resolved and may name it`. It counts a relative import whose path is the target without its extension, the target or its directory, and, in a file that is not JavaScript or TypeScript, a bare import with a part spelled as the target's module name (`b` for `a/b.py` or `a/b/__init__.py`). A Java import resolves through the files' package declarations, in any source root (`src/main/java` included): a class import to the class's file, `a.b.*` to the package's files, a static import to its class's file. A PHP `use` resolves to the file that defines the class or function it names, in the namespace it names. An import is recorded as its statement writes it. JavaScript and TypeScript: the module of `import`, of `export ... from` and of `import x = require()`, and the argument of a `require` or `import()` call when it is a string or a template string without `${}`; an exported declaration is no import. PHP: `require` and `include` with either quote, and `use`, plain, `function` or grouped (`use Foo\{A, B}` gives `Foo\A` and `Foo\B`). R: `library`, `require`, `requireNamespace` and `source`. Julia: each module of a `using` or `import` list, the module before a colon, and `include`. Ruby and Lua: `require` and its kin without a receiver, with a literal argument. Kotlin and C#: the imported name, not its alias. Objective-C `@import` and QML imports are recorded too. `--json`: `deps` gives each import's `line`, `module` and `resolved` (the file, or `null`); `rdeps` each importer's `path`, `line` and `module`. |
| `similar TARGET [-k N]` | Code similar to a symbol or `path:line` (vector cosine + token Jaccard). A constant has no vector: `similar` on one is an error that says so. `--json` gives each with `path`, `start`, `end`, `kind`, `qualname`, `cosine` and `jaccard`. |
| `dupes [--min-tokens N] [--threshold J] [--limit N]` | Exact and near-duplicate functions. A function is compared only when its body (the lines below the signature line) has at least `--min-tokens` tokens (default 30): the pieces of its identifiers split at `_` and case changes, of two letters or more, with numbers and common English and language keywords left out, so `v1 = compute_1(x, 1)` counts one. `exact` is the same body text once runs of whitespace are squeezed; a pair with the same words written differently is `near` with similarity 1.00. Any other `near` pair has at least `--threshold` (default 0.75) of the two bodies' three-token runs in common (Jaccard). `--threshold 1` prints the exact groups only. Near pairs are found by sampling, through MinHash buckets: at the default threshold practically every near pair is found; at `--threshold 0.5` about two in three of the pairs at that similarity are, and the answer says so. A bucket counts the copies of one body once, so a near copy of code copied many times is found and shown against its first copy (`(one of 64 exact copies)`, `copies` in `--json`). A bucket of more than 64 bodies is not compared pair by pair: the answer says how many functions fell in one, and that a near pair among them that shares no other bucket is not in this answer. Groups come largest first, then most alike, each with its members by path and line, so any index of the same tree gives the same answer; at most 50 groups, or `--limit`, cut in that order. `--json` gives each group's `kind` (`exact` or `near`), `similarity`, `tokens` and `members`, each member an array `[path, qualname, start, end]`, and for a near pair one of whose members stands for an exact group, `copies`: how many copies each member stands for, in member order. |
| `impact [--base REV] [--staged] [--depth N]` | What a change touches: the definitions the git diff changes, the callers that reach them up to `N` steps away (default 3) with their distances, and the test files to run (see below). |
| `review --base REV [--head REV] [--format markdown\|json\|gitlab] [--depth N] [--link URL]` | A pull request report: `impact` between two commits and the duplicates the head adds, as a PR comment, JSON or a GitLab Code Quality report (see below). |
| `files QUERY [--limit N]` | Paths of the repository ranked for a short query that may be misspelt (`srch.rs` finds `search.rs`), files read lately first among equals; no index needed (see below). |
| `dead [PATH]` | Definitions nothing in the repository uses: functions, methods, classes, structs, traits, interfaces, types and enums (not fields, constants, variables, modules, `impl` blocks or windows). One is dead when nothing outside its own text, and outside the first line of another declaration or body of the same qualified name (Pascal's `procedure Tidy;` in the class and `procedure TFoo.Tidy`), names it: no line outside its own lines, and on its first or last line nothing outside the span its grammar gives it, so a definition another one calls from the same line (`static int sq(int v) { return v * v; } int area(int v) { return sq(v); }`, a one-line class) is not dead; in a language with no grammar a definition has no span, and one whose name stands again on its first or last line is undecided (`its name stands 2 times on its own first or last line, where no grammar span tells its own from a use`). It is named as a whole word of the file's language (`foo'` in Haskell, `$init` in JavaScript, `push!` in Julia and `foo-bar` or `foo?` in a Lisp are other names; `App\Models\User` names `User`, `s"v$version"` in Scala or Groovy names `version` and Haskell's quote `'helper` names `helper`), by its own name (`Tidy` for `TFoo.Tidy`; Ruby's writer `label=` only where it is called, `.label =` or `:label=`, not where `@label` is read) (in any case for a Fortran (by its extension where the build has no Fortran grammar), Pascal or Ada name and a PHP function, method, class, interface, trait or enum, as those languages ignore case), in any file the code index's walk reaches (every regular file the ignore rules let through, also one the index leaves out by extension or size; a binary file is not read). When other lines hold its name, the resolver (`Names`) is asked about up to 16 of them in files it reads, its own file first: a line it ties to the definition is a use, and the definition is in neither group. Otherwise it is undecided, and the reason counts the lines that name it: those the resolver ties to something else (another definition, a local, a comment or a string, so `getattr(obj, "name")` keeps `name` undecided), those it cannot place, those in files it does not read, and those not asked. While a file over 64 MB is there, nothing is called dead: a definition no other line names is undecided. Entry points are in neither group: definitions in a test file, `main`, a definition with an attribute, decorator or annotation other than those that change nothing about who calls it (`#[inline]`, `#[allow]`, `#[derive]`, `#[cfg]` other than `cfg(test)`, `@property`, `@staticmethod`, `@Deprecated`, `@Nullable` and their kin), every method of a type or `impl` that has such an attribute (`#[pymethods]`), a method written `override`, and a constructor (`constructor`, `initialize`, a method named as its class). A definition's modifiers and attributes are its own: where another definition shares its line (a one-line class, two functions on a line) they are read from its own text and what is written just before it, back to a neighbour's end or its holder's `{`, so a neighbour's `public`, `private`, `internal`, `export`, `pub`, `extern`, `override` or attribute decides nothing for it; with no grammar span there, it is undecided (`another definition shares its line, and no grammar span says which of the line's modifiers are its own`). Per language: Python names that start with `__` (called by name by Python or a host such as `__lldb_init_module`) or with `test`, and a name quoted in a file with `__all__`; Rust `extern` functions, the methods of a trait `impl`, `write_fmt` (which `write!` calls), and `pub` items and the methods of a `pub` trait in a library crate (a file under the `src/` of the nearest `Cargo.toml` that has `src/lib.rs` or a `[lib]` table, `src/main.rs` and `src/bin/` aside); Go `init`, exported names of a package other than `main` and exported methods (an interface may call them); JavaScript and TypeScript an `export`ed definition, the methods of an exported class, a name on a line of its file that exports (`module.exports = { f }`; on a line definitions share, outside their text), every definition of a `.d.ts` file, and an interface, type or class of a TypeScript file that has `declare module` or `declare global` (it may merge with one from outside); Java and C# `public`, `protected` and `internal` members, interface members and Java's serialization hooks (`readObject`, `writeObject`, `readObjectNoData`, `readResolve`, `writeReplace`); Kotlin, Scala and Groovy definitions and PHP methods unless `private` or `internal`, PHP magic methods; Ruby operator methods (`<=>`, `==`, `[]`) and the protocol methods Ruby and its core library call without naming them (`method_missing`, `respond_to_missing?`, `coerce`, `to_s`, `to_str`, `to_ary`, `to_proc`, `inspect`, `hash`, `eql?`, `each`, `initialize_copy`, `inherited`, `included`, `extended`, `method_added`, `marshal_dump` and their kin); Swift `public` and `open`; Dart names without a leading `_`; GDScript names with one (the engine's virtual functions); Make targets. A JavaScript or TypeScript method of an object literal, and a definition in a `.vue`, `.svelte` or `.astro` file, is undecided when no other line names it: code may call it by its key, or the component's template may use it. A method of a type that extends or implements another, and such a class or struct itself, is undecided when no other line names it: the base type's code may call it. A name built at run time (`getattr(obj, "handle_" + kind)`) or by a macro that pastes identifiers is not seen. `PATH` (a directory or file) keeps the definitions under it, uses are still looked for in the whole repository, and a path that is not there is an error. The dead come first, then the undecided, each by path and line, and a last line counts both: `app.py:8-9  [function] unused_private`, `app.py:21-22  [function] by_name  (undecided: named on 1 other line: 1 tied to something else (another definition, a local, a comment or a string))`, `(1 dead, 1 undecided)`; nothing found prints `(no dead code found)` and exits 1. `--json` gives `dead` and `undecided`, lists of `id`, `path`, `start`, `end`, `kind`, `name`, `qualname`, and `reason` for an undecided one. What the resolver said of each line is kept (`.scrutate/answers/`) as for `callers`. `tools/check_dead.py BINARY [ROOT]` checks with ripgrep that no other line of the tree names a definition it calls dead. |
| `tsquery LANG QUERY [PATHS] [--limit N] [--no-cache]` | Tree-sitter structural query over every file of a language up to 1 MB (no index; per-file results are cached, see below). A larger file is not searched and is always said: one named on the command line gets `PATH: over 1 MB, left alone` on stderr and exit 2, since no match in it is known (also with `--rewrite`, the dry run and `--write`); those a walk met get one note with their count and first names (`3 files over 1 MB were not searched: a.py, b.py, c.py`), the exit code still by the matches, and with `--json` a last record `{"type": "skipped", "why": "over 1 MB", "paths": [...]}`; every other `--json` record is one capture, with `path`, `line`, `column`, `kind`, `capture` and `text`. The MCP tool `structural_search` gives the same note, and `skipped` in its envelope. Errors: an empty query, a query without a `@capture`, an unknown predicate, and a predicate written outside every pattern's parentheses (tree-sitter would apply it to nothing, so the pattern would match every node: write `((identifier) @i (#eq? @i "x"))`). A path that does not exist from the current directory is read under `--root`; one in neither place is named in a warning, the other paths are still searched, and the exit code is 2. A path found under `--root` prints joined to it (`../src/a.py`). Paths print as `grep PATH...` prints them, in plain output and in `--json`: the path as given, then the path below it (`src/a.py` for `src`, `./src/a.py` for `./src`, nothing in front for `.` or no path), so a printed path can be passed to the other commands. `--rewrite TEMPLATE` replaces each match with a template of its captures and prints a diff; `--write` changes the files (see below). |
| `check [--files] [--json]` | Verify database integrity, segment consistency (vectors present and as wide as the model's, and the inverted-file lists of every segment of 20,000 chunks or more that has vectors, included), that every code definition has its duplicate keys and no duplicate key, call or import is left from a chunk or file that is gone, and (with `--files`) that indexed files are unchanged. Exit 1 on problems. The line `chunks: N ranked in the database, M alive in segments, K constants and fields (no postings)` counts the chunks that have segment rows (constants and fields have none), then the constants and fields; `--json` gives that count as `constants`. `--json` counts the indexed files also without `--files` (`files_changed` and `files_missing` are then 0: not checked). An entry of `.scrutate/seg` no segment list names (a segment directory, or a stray file) gets a `note:` line with its size and age, and `unlisted` in the JSON; it is no problem and leaves the exit code as it is: it may belong to a merge in progress or to a writer that stopped, and an update removes it once it is older than 10 minutes. A file every search leaves out because it is one of scrutate's index files under another name (on Unix, see [Security](SECURITY.md)) gets a `note:` line naming it, the index file it is and that removing the hard link, by its path, lets it be searched, and `left_out` in the JSON (`path`, `index_file`); it leaves the exit code as it is. The whole `--json` report: `database_ok` and `segments_ok` (booleans), `segments`, `chunks_db`, `chunks_alive`, `constants`, `files_indexed`, `files_changed`, `files_missing`, `problems` (a list of the problems' text), `unlisted` (each with `name`, `bytes` and `age_s`), `left_out`, and `not_indexed` when files are left out. |
| `stats` | Index statistics as one JSON object: `files`, `files_skipped` (the files it leaves out), `chunks`, `symbols`, `call_edges`, `imports`, `segments`, `semantic` (whether the semantic model is on), `text_index` (its `files`, `segments` and `bytes`), and `not_indexed` when files are left out (see below). |
| `languages [--json]` | The verified capability registry. The JSON gives each language's `id`, its `extensions`, the extensions it shares with another language and takes by the file's text (`extensions_by_content`), its file names (`filenames`), whether it has `definitions`, `call_edges`, `imports` and `docstrings` (booleans), every kind of definition the index gives it (`definition_kinds`: `method` for a function in a class and `constant` included), and `"tier": "grammar"`. A custom language follows the built-in ones: its line ends `custom: .scrutate.toml` (or `custom:` and the path of your configuration file), and its JSON has `"tier": "custom"` and `"source"`. |
| `watch [PATH]` | Keep the index (and the text index, if there is one) up to date from filesystem events (see below). |
| `serve [--refresh-interval S] [--max-bytes N] [--watch] [--structured] [--short-tools]` | MCP server on stdio (see below); `--watch` also brings the indexes up to date after each burst of file changes; `--structured` also sends each answer as JSON; `--short-tools` sends the short tool list. |
| `lsp` | Language Server Protocol server on stdio (see below). |
| `config [--json]` | Show the configuration file in effect. `--json` prints `{"root": ..., "configuration_file": ...}`: the root an index command run here uses, and the configuration file (`null` when none). It builds no index; the editor plugins read it to find the root. |
| `batch FILE [--features] [--timing]` | Run requests from a JSON-lines file with one index open. A line `{"query": ..., "k": ...}` is a ranked search, printed as `{"query", "hits"}` (`ms`, its time in milliseconds, added with `--timing`); a line `{"op": TOOL, ...}` runs any operation, named and given arguments as the MCP tool is (`{"op": "callers", "name": "parse"}`), and prints the answer's envelope with the operation's name (`{"op", "schema", "result"}`, and `stopped_at` when it stopped at its limit) or `{"op", "error"}`; a `response_format` other than `detailed` or `concise` is such an error, and either form prints the same envelope. A `max_results` over 5,000 for `grep_code` or `references` is lowered to 5,000 here too, with no note. A `grep_code` answer that stopped carries `stopped_at` inside its `result`. A line that is not JSON prints `{"error": "line 3, column 2: not JSON (...)"}` and the other lines still run. `--features` adds to a search every candidate (`candidates`, each with `id`, `path`, `name`, `qualname`, `kind`, `score` and its features `x`), the feature names once per line (`feature_names`, the order of `x`), the ids each source found (`exact_ids`, `lex_ids`, `sem_ids`) and the query's words and literal names (`terms`, `lits`): the rows the ranker is trained on. |

**Filters** (`FILTERS` above): `search`, `symbol` and `refs` keep what these admit, and so do the
MCP tools `search_code`, `find_symbol` and `references`, each taking the same as an array of
strings (`globs`, `types`, `langs`, `kinds`). Each flag repeats, and all but `-g` take a
comma-separated list. The values of one flag are any of them; every flag given must hold.

- `-g GLOB`: the files the glob matches, from the root, as `grep -g`; `!` leaves them out.
- `-t TYPE`: the files of a type, as `grep -t` (see `grep --type-list`); `!` leaves them out.
  Where `grep` lets a glob that matches a file decide alone, here `-g` and `-t` must both hold.
- `--lang LANG`: the files parsed as that language (see `scrutate languages`): a `.h` file is C
  or C++ by its text.
- `--kind KIND`: the definitions of that kind (`function`, `method`, `class`, `struct`, `trait`,
  `module`, `constant`, `field`, `macro` and the others a language gives; a stretch outside every
  definition is a `window`). With `refs` it keeps the definitions of that kind and the
  occurrences that resolve to them, so `refs --textual` takes none.

An unknown type, language or kind is an error that says where the names are listed. `search`
returns `-k` results whenever that many pass: a filter that keeps an eighth of the files or the
definitions or fewer draws the lexical and semantic candidates from those alone, and a broader
one filters a first search's candidates, then draws them from what it keeps when too few pass,
widening the candidates up to 64 times their usual number; an answer still short of `-k` there
says so. A
kind no definition has, or a filter that keeps no file, answers at once. `symbol`'s filters narrow inside the name lookup, so `--limit` counts only what they keep; when
no definition has the name, the near matches are ranked before the filters see them, so they are
read again until `--limit` pass, up to 100,000, and an answer stopped there says so. `refs`
walks only the files the globs and types (or, with no type, the languages' file names) admit, and
`--max-results` counts only the occurrences in files the filters keep: a file whose language its
text decides (a `.h` header, C or C++) is read and judged before its occurrences count, up to the
same 10,000 as `--kind`. Its file filters pick each occurrence by its own file, and a
definition outside them heads the occurrences they keep, marked `(outside the filter)`
(`outside_filter` in the JSON). `--kind` picks the definitions, and `--def` names one whatever
the file filters keep; with `--def` or a qualified name, `--max-results` counts the occurrences
that resolve to those definitions, under the same bound as `--kind`. With `--kind`, `--max-results` counts only the occurrences of the kinds it
keeps: a kind is known once an occurrence is resolved, so the search resolves occurrences until
that many are kept, at most 10,000 (or `--max-results` when larger), dropping the others as it
goes, and an answer cut there says how many it read. The bound is a count: on the Linux tree a
read that runs to it takes about 3.5 to 21 seconds by the name, from `refs size --kind
function` (3.53 s) to `refs kfree --def mm/slub.c:6781 --max-results 20` (21.4 s and 492 MB, its
17 uses past the bound). Narrow the files with `-g`,
`-t` or `--lang`, which narrow the search before anything is resolved. When no definition of the name has
one of the kinds, the answer is empty at once and says so.

The code index holds every file the ignore rules let through, hidden files included, except:

- binary files, files over 1 MB, `.git/` and `.scrutate/` at any depth and under any spelling a
  file system takes for them, by git's own rules for `.git` (case ignored, the code points HFS+
  ignores, trailing dots and spaces and an NTFS stream, the short names `GIT~1` and `GI7EBA~1`),
  so a nested `vendor/lib/.GIT/` is skipped on every platform;
- the directories named in `SCRUTATE_INDEX_EXCLUDE_DIRS`, or when it is not set in
  `[index] exclude_dirs` of the root's `.scrutate.toml` (names only, at most 1000; a name with `/` or
  `\`, `.` and `..` exclude nothing; in the variable `\,` is a comma inside a name; the file may
  be a link to a file inside the root);
- cache and tool directories by name (`node_modules`, `__pycache__`, `.venv`, `.mypy_cache`,
  `.pytest_cache`, `.next`, `.tox`, `.idea`, `.vscode`); `build`, `dist`, `target` and `venv`
  only outside a git repository, since inside one the ignore files decide (a `target/` that
  `.gitignore` does not list is indexed); a file of one of these names, such as a `build`
  script, is indexed;
- data, generated and binary files by extension (`.lock`, `.sum`, `.csv`, `.log`, `.min.js`,
  `.map`, archives, media) and lock files by name (`package-lock.json`, `npm-shrinkwrap.json`,
  `pnpm-lock.yaml`, `packages.lock.json`, `Package.resolved`, `.terraform.lock.hcl`,
  `*.lockfile`, `bun.lockb`), which `scrutate grep` still searches.

The ignore files are those `grep` reads, `.rgignore` included. A `.h` file is Objective-C when
it holds `@interface`, `@protocol`, `@implementation` or `@class`; C++ when a line starts with
`namespace`, `class`, `template <`, `using namespace`, an access label (`public:`),
`extern "C++"` or a standard C++ include (`#include <vector>`); else C.

Constants are symbols of kind `constant`:

| Language | Constants |
|---|---|
| Rust | `const` and `static` at file level, in a `mod` block or in an `impl` or trait body. |
| Go | package-level `const` and `var`. |
| JavaScript and TypeScript | a top-level `const` that is not a function (exported or not); in a class body, a TypeScript `static readonly` field and a JavaScript `static` field named in capitals (`static MAX = 1`). A `let` or `var` is not indexed. |
| TypeScript | enum members. |
| Java | `static final` fields, enum constants and the fields of an interface. |
| Python | an assignment to a name in capitals at module level or in a class body, and each such name of a tuple or list target (`A, B = 1, 2`, `[A, B] = 1, 2`). |
| C, C++, Objective-C and GLSL | `#define NAME value`, enumerators, and a `const` or `constexpr` declaration with a value at file level, in a namespace or in `extern "C"`, also inside an include guard (`const int SIZE = 4;`, `NSString *const kName = @"x";`, not `extern const int SIZE;`). |
| C++ | a `static const` or `static constexpr` member with a value in a class body (`Config.kSize`), and a member defined with its value outside the class (`const int Config::kLater = 5;` is `Config.kLater`). |
| GDScript | `const` at file level or in an inner class body (`Backpack.LIMIT`), owned by the file's `class_name` when it declares one (`Player.LIMIT`, `Player.Backpack.LIMIT`). |

Each name of a Go or Java declaration with several (`var X, Y int`, `const A, B = 1, 2`) is a
constant whose text is the whole declaration. Java's other fields and its record components are symbols
of kind `field`, kept as constants are. `symbol`, `outline`, `read_code symbol` and `refs`
find constants and fields; ranked search (`search`, `find`, the MCP `search_code`) never returns one.

Ranked search reads words of every script. Scripts written without spaces (Han, kana, Thai, Lao,
Tibetan, Myanmar, Khmer, the Tai scripts, Balinese and Javanese) become overlapping character
pairs, so `天気` finds `東京の天気`; other scripts give one lowercased token per word. Han
characters fold to their simplified form for matching, in index and query, so `资料库` finds
`資料庫` and the reverse, and Japanese shinjitai fold with them: `関係` finds `關係` and `关系`.
Names, `symbol`, `grep` and the text shown in results keep the characters as written. Only ASCII
words are split at case changes and stemmed.
Names compare in Unicode NFC, so a name typed with a combining accent finds the precomposed
spelling. A definition name longer than 200 bytes is still a symbol when it holds no whitespace.

### Files the index leaves out

The code index reads a source file of up to 1 MB (1,000,000 bytes) that is text and parses. Its
walk first leaves out data and generated files by name (`.csv`, `.tsv`, `.log`, minified `.min.js`
and `.min.css`, source maps, lockfiles and the like), whatever their size; of the rest it leaves
out a text file over 1 MB ("over 1 MB": a 3 MB `.json` or `.txt` is one), and a file that is not
text at any size (a NUL byte in its first 4 KiB, as `.npz` weights have) or could not be parsed
("not text or could not be parsed"), and records why. `grep` and the text index have no such
bound, and `read` reads any text file by its lines. Where an answer from the code index depends on
a left-out file, it says so, as a note line and, in `--json` and the MCP tools' envelope, a
`not_indexed` field (`reason`, `count`, the first twenty `paths`): `index`, `check` and `stats`
name both reasons (`1 file over 1 MB is not in the index: big.py`, past twenty `and N more`);
the rest speak of "over 1 MB" alone, which is what a smaller copy would fix: a lookup (`symbol`, `search`, `find`, `callers`,
`callees`, `refs`, `read --symbol`, `outline`, `path`) after an empty answer, and whenever a path,
glob or file it is given names a left-out file (`big.py is not in the index (over 1 MB): read it
by lines, or search it with grep`, and for a file that is not text the reason alone); `dupes`, `dead` and `map` always, as they claim the whole
repository. `refs` labels an occurrence in such a file `in a file that is not indexed (over 1 MB)`
and keeps it among the unresolved; `impact` lists a changed one as `big.py  [file]
(edited; not indexed: over 1 MB)`, and `review` as ``- `big.py` (edited; not indexed: over 1 MB)``. A repository with no left-out file gets every answer as before.

### `refs`
```
scrutate refs NAME [--def PATH[:LINE]] [--textual] [--max-results N] [--json]
```
Finds every whole-word occurrence of `NAME` (as `grep -w`) and decides which definition each
refers to. A file is read as the index reads it: one with no NUL byte in its first 4 KiB is read
to its end, where `grep` stops at the NUL, and one with a NUL there is binary and not read. One inside a longer name of its file's language is no occurrence and is not listed:
`foo` in Haskell's `foo'`, `init` in JavaScript's `$init`, `valid` in Ruby's `valid?`, `foo` in Emacs Lisp, Scheme, Racket or Common Lisp `foo-bar` (a Lisp name runs to the next delimiter; a Common Lisp `pkg:foo` is `foo`). For Python, JavaScript, TypeScript, Rust and Go the rules are, in order:

1. The name of a definition, or the name in an import statement, is that definition (or what
   the import resolves to). In a Go declaration of several names (`var X, Y int`, `const A, B =
   1, 2`) only the first is tied so; a later one is unresolved, `not found`.
2. A qualified occurrence (`obj.N`, `a::b::N`, `pkg.N`) is looked up in what the qualifier names:
   - a module bound by an import (`import x`, `import * as x`, `use crate::a`, a Go import), or a
     module path (`crate::`, `self::`, `super::`, a workspace crate, a Rust child module);
   - a class or type (`Class.N`, `Type::N`), then its Python and JavaScript base classes in
     method resolution order, up to 8 levels; a Python base from outside the repository keeps its
     place in the order, and a method it may define stays undecided. `self.N`, `this.N`, `cls.N`
     and `Self::N` look in the enclosing class or impl;
   - for a local variable or parameter (`x.N`), the type its declaration states: an annotation
     (`x: T`, `x T`, `x: &T`) or what it is built from (`T(...)`, `new T(...)`, `T::new(...)`,
     `T { .. }`, `&T{}`). That is the nearest in-scope declaration before the use, in the
     function around the use or the functions around that. A closure sees its enclosing
     function's variables unless it binds the name itself; a Rust `fn` item does not, and a
     Python method does not see its class body; a name a Python class body binds (an assignment,
     also inside an `if` or as a tuple or `for` target, a `def`) is a local of the body once bound,
     before an import of that name, though `h = h` reads the outer `h` on its right side and a
     comprehension does not see the class's names. Not counted: a declaration in a block that has
     ended, a closure or lambda, an `if let` or `match` arm, or the declaration's own value. A
     Python function is one scope; a JavaScript `var` belongs to its whole function.

   An element of a container whose element type is written (Go `xs []T` and `m map[K]T`, Python
   `list[T]` and `dict[K, T]`, Rust `[T]`, `[T; N]`, `Vec<T>` and `HashMap<K, T>`, TypeScript
   `T[]`, `Array<T>` and `Record<K, T>`) has that type, and so has a place of a tuple by its
   number (Python `tuple[A, B]`, where `tuple[A, ...]` is all `A`; TypeScript `[A, B]`), and a Go call of a variable of a function type
   (`f func() T`) has its result type. A variable set from a call takes the function's declared return type (through `?` and holders
   such as `Box`, `Rc` and `Arc` in Rust, `await` in TypeScript); `self.x.N` takes field `x`'s
   declared type, and a module-level or static variable its declared or built type. A Go method
   is found by its receiver type in any file of the package, or through an embedded field
   (`v.Hello()` with `type Wrap struct { *Base }` is `Base.Hello`); an embedded `base.Base` is
   the `Base` of the package imported as `base`, never a local type of that name. A Rust method a type does not define
   itself is the default method of a trait the type implements (`impl Greet for S {}`, also
   `impl Greet for inner::S` outside `S`'s module); a supertrait's default methods are not
   followed. A method call on a call's result
   (`mk().norm()`) goes through the callee's declared return type. `this` inside a method of a
   JavaScript or TypeScript object literal is the literal (`this.put()` in `api.get` is
   `api.put`, also under a quoted key, `"get": function () {}`); an arrow function takes `this` from the code around it. A call on a value of an
   outside type is external, unless a repository trait or interface declares a method of that
   name: then it stays undecided, since a blanket `impl` or a merged interface can give the
   outside type that method. In Rust a call on the result of a method of an outside type is typed
   by a table of standard methods (their signatures in std 1.99.1: `Option`, `Result`, `Vec` and
   slices, `HashMap`, `BTreeMap`, `HashSet`, `BTreeSet`, `String` and `str`, iterators), so
   `v.first().unwrap().ok()` with `v: Vec<Report>` is `Report.ok`. A method the table does not
   list, of an outside type that is not a standard holder (`Option`, `Vec`, `Box`, `Mutex` and
   their kin) and whose whole type is known to hold no repository type (a type written with all
   its arguments, or what the table or such a method returns), returns an outside type:
   `file.metadata().ok()` is external. So is what a function of `std::env`, `std::fs`, `std::io`,
   `std::path` or `std::process` returns (`std::fs::metadata(p).ok()`). A method whose type the
   caller picks (`into`, `parse`, `collect`, `get`, `as_ref` and their kin) or that takes a
   closure gives no type, so a call on its result stays undecided. A value built by `new`,
   `default`, `with_capacity` or `from` has type arguments the resolver does not know
   (`let m = HashMap::new()`), so what a method returns from it, and a call on that, stay
   undecided: `m.get(&k).unwrap().ok()` may call a repository `ok`. A variable whose type none of
   these gives is unresolved.
3. A plain occurrence is, in order:
   - a definition nested in an enclosing function (or directly in an enclosing class body). A
     parameter or local of that name hides everything else within its own scope (a lambda's
     parameter inside the lambda, a block-scoped name inside its block). A name bound by a
     pattern hides only in its own branch (Rust `if let`, `while let` and a `match` arm), and a Python
     `case` capture in its whole function, before the `match` too; a `catch` binding and a Go `:=` in an `if`, `for`, `switch` or type switch only in
     that statement; a Python parameter only in the function body (defaults and annotations are
     outside it); a `let` or `for` not in its own value; an import inside a function only in that
     function; the name of a JavaScript function, generator or class expression (`var h = function
     load() {}`, `var C = class Box {}`) only inside it. A closure sees the locals of the function it is written in; a Rust `fn` item
     inside a function does not;
   - a top-level definition of the file;
   - a name an import binds (`from x import N`, `import {N} from './x'`, `use a::N`, `require`, also
     `const N = require('./x').N`, `const {N} = await import('./x')` and TypeScript
     `import m = require('./x')`);
   - in Go, a top-level definition of another file of the same package directory with the same
     package clause (`package a_test` is another package), where a `_test.go` file is seen only
     from a test file; an import sees the package's files without its `_test.go` files, except the
     package's own external test (`package a_test` beside it), whose import also sees the
     `_test.go` files of `package a` (the `export_test.go` pattern);
   - wildcard imports (`from x import *`, `export * from`, `use a::*`, Go dot imports).

Inside a Rust macro's arguments, which are tokens to the grammar, a name after `.` is a field,
or a method when `(` follows; `name:` inside `{}` is a field name; a closure parameter or a
`let`, or the pattern of a `for`, is a local. A name followed by `(`, `::` or `!` resolves as outside a
macro, a name after `::` as the same path outside a macro, in any macro, when the path starts
with a plain segment (`util::work` in `println!("{}", util::work())` or `mymac!(util::work)` is
`work` of `util`; `::a::work` and `Vec::<u8>::work` are `macro argument`), and so does any name in a standard macro that takes expressions (`assert`, `assert_eq`,
`assert_ne`, `dbg`, the `debug_assert` forms, `eprint`, `eprintln`, `format`, `format_args`,
`panic`, `print`, `println`, `todo`, `unimplemented`, `unreachable`, `vec`, `write`,
`writeln`): the `x` in `println!("{}", x)` is the local or definition `x`. Any other plain name
the rules would tie to a definition is unresolved as `macro argument`, since the macro may use
it as anything: the `text` in `mymac!(text, 3)` or `stringify!(text)`; `callers` and `dead` count
it as undecided. A local still hides a definition there. An occurrence matches the name in either Unicode spelling, composed or
decomposed.

Java has rules of its own:

- A file's package comes from its `package` declaration, so every source root works; a file
  without one is in the default package of its own directory, and a class it names that only
  another directory defines is `external`, as from the classpath, and so are calls on it
  (`new Util().inst()`), unless a `*` import may bring it. Imports
  are single-type, on-demand (`.*`) and `static`.
- Locals are parameters, lambda parameters, the variables of a method, a constructor, a static or
  instance initializer block, and those of `for`, `catch`, an `instanceof` pattern and a
  try-with-resources.
- A plain name is also looked up in the enclosing class and what it extends, through nested,
  inner and anonymous classes, enums with methods, records and default methods.
- A receiver goes through its declared type, a chain through declared return types; one typed by
  an interface resolves to the interface's method. `Outer.this` is the enclosing class `Outer`. A class in front of a call (`Util.foo()`,
  also `p.Util.foo()` with `p` a repository package) is a use of that class; a local, parameter
  or field there is that variable.
- The number of arguments picks one overload; when it does not, the call, like a method
  reference (`Sq::grow`), stands for every overload of the name.
- Undecided: an untyped lambda parameter, an element of a generic type
  (`list.get(0).area()`), a value of a type parameter, and a class whose base type is not in
  the repository.
- A field, an enum constant and a record component are definitions: a plain name, `this.f`,
  `obj.f` through the declared type, `C.F`, an inherited field and a static import resolve to
  them, and a record's implicit accessor (`p.x()` for `record P(int x)`) to its component. A
  field and a method of one name stay apart: a call is the method, a read the field. A field of
  an anonymous class hides a local of that name of the method around the class, inside it.

PHP has rules of its own too:

- A class or function name is made full through the `use` that binds its first part (plain,
  aliased, grouped, `use function`), else through the `namespace` it is written in (a
  `namespace X;` line or a braced block); a name with a leading `\` is full as it is. As in PHP,
  an unqualified function call takes only a `use function`, and a class name or the first part
  of a qualified name only a plain `use`: `use App\Models\Config;` leaves `config()` the
  global function.
- A full name `A\B\C` is the class, interface, trait or enum named `C` whose file declares the
  namespace `A\B` where it defines it; an unqualified function call that finds none there takes
  the global function, as PHP does.
- `$x->m()` goes through the class of `$x`: `$this` is the class around; a variable is the value
  of its latest assignment before the use (`new C()`, what a call returns), else its parameter's
  declared type; `$this->p` is the type property `p` declares, also through a constructor
  parameter that declares it. A call takes its method or function's declared return type
  (`self` and `static` are the class it is in).
- `self::m()` and `static::m()` look in the class around, `parent::m()` in the class it extends,
  `C::m()` in `C`. A method is looked for in the class, then its traits, then the class it
  extends and the interfaces it implements; when a base from outside the repository may declare
  it, the call is external. In a trait, `$this->m()` for an `m` the trait lacks is `receiver`: the
  class that uses the trait decides.
- A variable a closure takes with `use ($u)` or `use (&$u)` has the type it has around the closure, as one an
  arrow `fn` reads does. A static property (`User::$current`) has the type it declares.
- Undecided: a variable bound by `foreach` or `catch`, a value of an untyped parameter, a union
  type, a dynamic name (`$o->$m()`).
- Properties and class constants are `field or key`, a bare constant is `constant`, a part of a
  namespace is `namespace`. Class, interface, trait, enum, function, method, namespace and `use`
  alias names, and `self`, `static` and `parent`, compare in any case, as PHP compares them (`s::SLUG()` after `use App\Util\Str as
  S` is `Str::slug`); variables, properties and constants compare as written.

C and C++ have rules of their own:

- An `#include` binds every name at the top of the file it names, and the names that file
  includes in turn, up to 64 files. A plain name is a local, then a member of the class around
  (also of a method defined outside its class, `void Shape::area() {..}`) or of its bases, then a
  name of the file or of a namespace around the use, then one of the included files. An
  enumerator of an unscoped enum is in the scope of its enum.
- `a::b::f` is the definition of that qualified name, else one inside a namespace of that name.
- A function or method declaration (`int add(int, int);` in a header) stands for the definition
  of its name with a body: the only one, else the one at the declaring file's path with another
  extension (`inc/util.h` and `inc/util.c`, not `lib/util.c`); with several and none of that name the use is undecided.
- Undecided: a call on a value (`s.area()`, `p->next()`), whose type is not followed, and a
  member of a class whose base is not in the repository. A field read on a value is `field or
  key`. Include paths of the build (`-I`, `compile_commands.json`) are not read.

C# has rules of its own too:

- A type is looked for in the classes around the use, then in the namespace the file declares
  (block or file-scoped) and the namespaces around it, then the `using` namespaces and aliases
  (`using M = A.B;`). A dotted name is a type of a namespace (`A.B.C`) or a nested type.
- A plain name is a local or parameter (lambda ones too), then a member of a class around, its
  other parts if it is `partial`, and its bases, then a type, then a member a `using static`
  brings in.
- `x.M()` goes through the type of `x`: a local's declared type or what it is built from
  (`var c = new Circle()`), a parameter's, a field's or a property's type; a call takes the
  method's declared return type. `this` is the class around, `base` its bases.
- Undecided: `x?.M()` and a member of a class whose base could not be found. A call on a value of
  an untyped lambda parameter is `external`. Fields and enum members are `field or key`, a part of a namespace is
  `namespace`.

Import paths resolve to repository files:

| Language | Resolves |
|---|---|
| Python | relative imports, packages (`__init__.py`), and an absolute import whose first part is the checkout directory's name (`networkx.classes` in a checkout named `networkx`). When no module from the root has the name and the importing file's directory is not a package (no `__init__.py`), the module beside the importing file, as Python finds it when it runs the script (`from helpers import make_items` in `tests/` finds `tests/helpers.py`). Such a sibling wins over a standard library module of the same name, while `import logging` in a package that has its own `logging.py` stays the standard library. |
| JavaScript | relative paths with the usual extensions and `index` files (a `.js` import finds the `.ts` source), and the project's own folder names in `tsconfig.json` or `jsconfig.json` (`paths`, `baseUrl`, also through a relative `extends`, a string or a list whose later entries win; `${configDir}` at the start of a `paths` place or of `baseUrl` is the folder of the config nearest the importing file; `paths` are relative to the final `baseUrl`, else to the folder of the config that sets them). Then a workspace package by its `package.json` name: through `exports` (the conditions `types`, `import`, `default`, `require` and `node` in that order, then any other by name), `module`, `main` or an `index` file. A target in a build directory the repository does not hold stands for the same path under `src/` (`./dist/index.js` for `src/index.ts`); a subpath goes through its `exports` key or pattern, else a path in the package; of two packages with one name, the one nearest the importing file wins. A declaration file (`.d.ts`) stands in only when no implementation has the name (`./m` with `m.js` and `m.d.ts` side by side is `m.js`). |
| Rust | module files (`a.rs`, `a/mod.rs`) from the crate root, the file a `#[path = "..."]` attribute gives a `mod` item (from the declaring file's directory, or inside an inline `mod` block from that block's module directory; a path to no file of the repository gives none), workspace crates by the name their `Cargo.toml` gives the library (`[lib] name`, else `[package] name`), and an inline `mod a { }` below the deepest module file of the path (`use other::work` with `mod other { pub fn work() }` in the same file); in a qualified name inside inline modules, `self::` and `super::` count from those modules first. A file directly in a package's `src/bin/`, `tests/`, `examples/` or `benches/`, and `build.rs`, is a crate root of its own (`mod common;` in `tests/it.rs` is `tests/common/mod.rs`); a file beside it is its module only when it declares it; `path` entries of `Cargo.toml` are not read. |
| Go | import paths through the module path in `go.mod` (by the directory they end with when there is no `go.mod`). |
| PHP | a `use` to the file that defines the class or function it names; a `require` or `include` path is mapped onto the files as in a language the resolver does not read. |
| C and C++ | a quoted `#include` to the file beside the including file, else (and a `<...>` include always) to the files whose path ends with the included path, of several those that share the most directories with the including file. |
| C# | a `using` to the files that declare the namespace (a block or a file-scoped `namespace`), a `using static` or an alias of a type to the file of the type. |

`tsconfig.json`, `jsconfig.json`, `package.json`, `Cargo.toml` and `go.mod` are read only as
regular files inside the repository, of at most 1 MB. Re-exports (`from .x import N` in
`__init__.py`, `export {N} from`, `pub use`) are followed, also by `callers`, `callees` and
`impact` for a name imported under an alias from the re-exporting package (`from pkg import
helper as h`). In JavaScript a `var` belongs to its
function, `let` and `const` to their block. Imports of modules outside the repository give
`external`; a package name no repository `package.json` declares is outside. Unresolved:

- occurrences in comments and strings;
- in a language the resolver does not read, or a file whose syntax tree is more than 2,000
  levels deep, a use whose name has more than one definition in the language family and none in
  its own file (`by name, with other definitions of the name`), or with no definition in the language
  family, or that is part of a longer name the index holds (`not resolved in this language`,
  `nested too deeply to read` in a deep file;
  `get-user` where a definition has that name; Lua's `width-margin` is a use of `width`); a use with one such
  definition, or one in its own file, is tied to it by name and marked `(by name)` (`by name:
  nested too deeply to read` for a deep file; `via` in JSON), as `callers` and `callees` read it;
- a file without a grammar (`no grammar`): the index holds none of its calls;
- anything the rules do not reach (method calls on a value whose type they cannot infer, enum
  variants, local variables), and a plain name in the arguments of a Rust macro (`macro argument`).

A constructor's references are its class's instantiations as `callers` reads them (`new Box(1)`,
`Point(1)`, Ruby's `Invoice.new`, `super(..)` in a JavaScript or TypeScript subclass constructor,
`via` `class`), and a PHP class's include `new self()`, `new static()` and, for the base class,
`new parent()`, in any case. An alias in a language that ignores case (PHP's `use function
Lib\phpfn as pf`) is found in any case too (`PF()`).

The output lists each definition with its references, then the unresolved occurrences with the
reason. `--def PATH[:LINE]` keeps one definition of `NAME`: the one in `PATH`, or the one
containing `LINE` when the file has several; no such definition is an error. A qualified name
(`Stack::push`, `Stack.push`) keeps the definitions `symbol` finds for it, as `--def` keeps one,
and one that names none keeps no occurrence: the answer has only its counts and unresolved uses,
and with no unresolved use it is `(no results)` (the counts in `--json` alone) with exit code 1. `--textual` skips
resolution: definitions, callers and every occurrence grouped by enclosing symbol (it never
misses anything; it reads definitions, callers and enclosing symbols from the index, which it
builds first as any lookup does). Both search the files the MCP `grep_code` tool
searches: hidden files included, the directories the index skips left out. With `--json`, the
document has `name`, `definitions` (each with its `path`, `start`, `end`, `kind` and `qualname`,
`"outside_filter": true` for one the filters leave out, and a `references` list: `path`, `line`,
`column` in bytes, `text`, `via`, and `"cut": true` when the text is a part of a long line) and an
`unresolved` list (`path`, `line`, `column`, `text`, `cut`, `reason`). `--textual --json` is shaped
otherwise: `name`, `definitions` (`id`, `path`, `start`, `end`, `kind`, `qualname`), `callers`
(definitions as under Conventions), `references` (one group per enclosing symbol: `symbol`, the definition
or `null`, `path`, `lines` of `line`, `column` (always 0 here) and `text`, and `note` for a file a NUL
byte stopped) and `truncated`. `--def`, a qualified name and `--kind` still
resolve every occurrence they read: the unresolved ones are listed, at most `--max-results` of
them, with `unresolved_count` counting every one the read saw (an unresolved use may be a use of
the definition asked for), and `--def` or a qualified name ends with `(N other occurrences resolve
to other definitions, M are unresolved)`, `other_definitions` in JSON, over the occurrences read when the answer stopped
early. The JSON also has `truncated` (whether `--max-results` cut it), and `stopped_reason` when
the read stopped at its bound or no definition of the name has the kinds. A file with a NUL byte
is read as `grep` reads one the walk finds, up to the read that meets that byte (see "NUL byte"
above): a use after that read is not listed, which in a small file can be every use, and a file
whose search the byte stopped after a match is named in a final `note:` line (`notes` in JSON,
each with `path` and `note`). Resolution reads the index's definitions and
parses the files holding occurrences again, so it needs no index change. The MCP `references`
tool gives the same answer (`textual` and `def` are arguments there, and the two together are
refused, as on the command line).

### `impact`
```
scrutate impact [--base REV] [--staged] [--depth N] [--json]
```
What a change touches, from git's diff. By default: the working tree against `HEAD`, tracked
files staged or not, plus every untracked file that is not ignored, as a new file. `--base REV`
compares the working tree with another commit; `--base main...` (three dots) with the merge base
of `main` and `HEAD`, as git reads it (`A...B` is the merge base of `A` and `B`). Two dots
(`main..feat`) is a range, not a revision: the error says so and points at the three. `--staged`
takes the staged changes alone, against `HEAD` or `--base`, and no untracked file. The answer has
three sections, a fourth when the change has findings and a fifth, `notes`, when a file gets a note
(below), each a line with its count and then one line per item:

- `changed`: the innermost definition around each changed line, as
  `path:start-end  [kind] name  (status)`. `added`: the base lacks it (by kind and qualified name
  in the file; a whole new file is all `added`); `edited`: the base has it; `deleted`: only the
  base has it, named with its base lines. A file renamed with edits (git's `-M`) names its
  definitions at the new path, with a note naming the old one. A changed line outside every
  definition names the window holding it, as `callers` lists a call written there, else the file
  (`path  [file]`: a blank line, a deleted line); a staged or committed line's window comes from a
  parse of git's copy. A staged change is read from the
  staged copy and named where the index holds that definition now.
- `callers`: the callers of a changed definition, then theirs, breadth first, each once at its
  smallest distance, shown in front (`1` calls a changed definition). Each step is the answer of
  `scrutate callers LOCATION --limit 500` for one definition, undecided callers included (marked
  `(undecided)`). A deleted definition has no location to ask about, so its distance `1` callers
  are the definitions whose code still calls its name where the resolver ties that call to no
  existing definition (not a namesake elsewhere, a local or an outside import; a deleted method
  is matched by name, with the receiver's type where the resolver decides it). As `callers` does,
  the name is also asked under the local names imports give it (`helper as h`) and, where the
  language ignores case, in its other cases (PHP `dothing()` for `DoThing`). Each is marked
  `(calls a deleted definition)`, the first thing the diff breaks, and the walk goes on from it.
  Windows and modules are not followed. `--depth N` (at most 10, `0` for none, default 3) stops
  the walk; past three steps a much used function's callers grow toward most of the repository
  and say little about what to test. The walk stops at 2,000 callers and says so; the callers it
  found keep the same order, and it names no `untested-change`, since a test past where it stopped
  may reach the change.
- `tests`: each test file (the index's test flag: a `test` or `tests` directory, `test_*.py`,
  `*_test.go` and the like) that defines a changed symbol or holds a caller, with its distance
  and the symbol reached, and each test file the diff changed, once, at its smallest distance. A
  test file the change deletes is no test to run, though what it defined is a change.
- `findings`: what a reviewer would want named, as `path:start-end  [kind, severity] sentence`.
  First each new duplicate group (kind `duplicate`, severity `minor`), at its member the change
  touched: the groups `dupes` finds with its defaults (`--min-tokens 30`, `--threshold 0.75`)
  that the base lacked, largest first, at most 50, with a line after them when more were new. Only a
  group with a member in a changed file can be new. The base's groups come from the same search
  over the index's definitions, each changed file replaced by a parse of its base copy. A group is
  old when one base group matches each of its members, each base member used once: the same
  definition (path and qualified name), or the same body text up to whitespace. So a pair the base
  had stays out when one of them is edited or moved, and a third copy is listed. Then each changed
  code definition (a function, method, class, struct, trait, interface, type, enum or impl, added
  or edited, outside a test file) that no test file reaches through the calls the walk found
  (kind `untested-change`, severity `info`). These are the kinds that have call edges; a
  constant, macro, key, field, signal, block, module, window or Markdown section has none for the
  walk to follow, so none of them is named. Severities are GitLab's Code Quality levels.

Items sort by distance, path and line, the same every run. A file with no definitions to name
gets a line under `notes`: a binary file, a submodule (its commits are not read), a file the
index's rules leave out; a text file over 1 MB is listed under `changed` instead, as
`path  [file]  (edited; not indexed: over 1 MB)`. A file moved to a path the index's rules leave out (a directory `.ignore` or
`.gitignore` names) reads as deleted, its definitions and callers as `git rm` of it gives. A file
renamed within the index gets `renamed from OLD`, and a definition of it
whose callers still name the old path (`from lib.util import helper` after `lib/util.py` became
`lib/helpers.py`) no longer reaches them: it is listed as deleted at the old path, the rename
note right above it, and its callers as calling a deleted definition, as for `git rm`; one whose
importers still resolve is not listed. The first 1000 definitions of renamed files are
checked so (each costs a lookup of its name's call sites); past that a note says how many were
not (`unchecked` in JSON). `--json` gives the envelope, its `result` with `against`, `base` (the commit
compared with), `staged`, `depth`, `changed` (each with `id`, `path`, `start` and `end`, `null`
for a whole file, `kind`, `qualname`, `status`, and `not_indexed` (`over 1 MB`) for a text file
over 1 MB), `callers` (each with
`distance`, `via`, the location of what it calls, `undecided` and `calls_deleted`), `tests` (`path`, `distance`, `via`), `notes` (each with `path`, `note` and, for a
rename, `renamed_from`, the old path, and on the note of definitions past the first 1000,
`unchecked`, how many), `findings` (each with `kind`, `severity`, `path`, `start`,
`end`, `qualname`, `text`, the sentence, and `related`, the other definitions it names, each with
`path`, `qualname`, `start` and `end`), `more_findings` and `stopped`. `--max-bytes` pages the text by whole lines,
as the other answers. No change is exit code 1. Errors (exit 2): a directory outside a git
repository, git missing, a revision git does not know, two revisions with no merge base, a file
under the root with unmerged changes (one outside it is not read), a git that fails (named with
git's reason or how it ended: `git diff-index: ended by signal 11 (a crash) and gave no reason`),
a base copy git does not have (`git cat-file: OID missing`). A revision that starts with `-` is refused before git runs, so it can
never be read as an option. The command line does not send `impact` to a running `serve`: git
runs in the command's own environment. Git runs at the root, and a relative `GIT_DIR`,
`GIT_WORK_TREE`, `GIT_OBJECT_DIRECTORY` or `GIT_COMMON_DIR` is read from the directory the command
runs in, as git reads it; `GIT_INDEX_FILE` and `GIT_ALTERNATE_OBJECT_DIRECTORIES` are read at the
work tree's top, as git reads them. With `GIT_DIR` and no `GIT_WORK_TREE`, git takes the directory
it starts in, the root, as the work tree's top (unless `core.worktree` names one), so from a
subdirectory the answers are the root's, where git started there would take that subdirectory as
the top. A `GIT_DIR` that is not the root's own git directory and has no `core.worktree` stops
the command with exit 2 (`read --history` gives that as its note): its history laid over this
tree would be another repository's; set `GIT_WORK_TREE` with it. One whose `core.worktree`
names a tree that does not hold the root (a submodule's, from its superproject) stops it too,
naming that tree. `core.worktree` counts only from the git directory's own `config`, where git
reads it: one in an included file or the global configuration names no tree. The MCP tool `impact` takes `base`, `staged` and `depth`
and gives the same answer, findings included.

### `review`
```
scrutate review --base REV [--head REV] [--format markdown|json|gitlab] [--depth N] [--link URL]
```
A pull request report for CI: `impact` between `--base` (a revision, or `main...` for the merge
base) and `--head` (`HEAD` by default), from git's diff of the two, with no untracked file. Each
side is read from git's copies, and a definition is named where the index (the working tree's)
holds it, so check the head out first. Callers, tests, findings and `--depth` work as in
`impact`: `review` renders that answer and looks for nothing of its own. A changed definition
with an `untested-change` finding is marked: no test reaches it.

`--format markdown` (the default) is a PR comment body: first the marker `<!-- scrutate-review -->`
(by which the GitHub Action finds its comment again), then a line naming the two commits, a line of
counts, and a section for each part with items: `Changed definitions`,
`Callers, up to N steps away` (each with its distance), `Tests to run`, `New duplicates` and
`Notes`. Locations are paths from the repository root; with `--link URL` each links to
`URL/PATH#Lstart-Lend` (the Action passes `https://github.com/OWNER/REPO/blob/COMMIT`). The body
is cut at whole lines before 65,536 bytes (GitHub's comment limit is 65,536 characters), with a last line that
says so and names the command that prints the whole report, the same comparison with `--format json`
(with `--head` when the head is not `HEAD` and `--depth` when it is not the default), each name
in a code span; a line saying the walk stopped is kept inside the limit. `--format json` is the envelope: `impact`'s `result`, `findings` included, with `head`,
`head_commit`, `new_duplicates` (each duplicate finding's group as `dupes --json` gives it) and
`untested` (the marked definitions' locations). `--format gitlab` is a GitLab Code Quality
report: a JSON list of issues with `description`, `check_name`, `fingerprint`, `severity` and
`location.path`/`location.lines.begin`, one per finding: `check_name` is `scrutate-` and its kind
(`scrutate-duplicate`, `scrutate-untested-change`), `description` its sentence, at its place. A
fingerprint is the SHA-1 of the check and the paths and qualified names involved, not the lines,
so GitLab matches an issue across pushes.

Nothing to report (no changed definition, note or test, no new duplicate) is exit code 1, with
the report still printed (in Markdown a line that says so, in GitLab an empty list). The errors
are `impact`'s (exit 2), and `--head` is checked as `--base` is: a revision that starts with `-`
is refused before git runs, and a head given as a range (`A...B` or `A..B`) is an error that
says to name one commit. `review` runs `git` as `impact` does and nothing else, writes only
the index, as every index command does, and posts nothing; the GitHub Action posts its output
([GitHub Actions](../integrations/github/README.md)). There is no MCP tool; the MCP `impact` tool
gives the same findings.

### `files`
```
scrutate files QUERY [--limit N] [--json]
```
The paths of the files a repository search covers (the files `grep_code` sees), ranked for
`QUERY`, one per line, at most 20 or `--limit N` (1 to 1000), with a closing line when more
matched. No file is opened (the walk reads only the ignore files) and no index is needed. Case
is ignored, and `\` in the query is `/`. A path matches when the query is a part of it, when the
query's letters appear in it in order (`srch.rs` finds `search.rs`), or when the query is within
a few edits of a part of it (a letter wrong, added or left out, or two neighbours swapped:
`saerch.rs`): one edit per four query characters, at most three. The order:

1. a basename equal to the query, with or without its extension (`search` and `search.rs` both
   name `src/search.rs` exactly);
2. then a match in the basename, then in the rest of the path (a query with `/` matches the
   whole path only);
3. then a part before letters in order before fewer edits;
4. then frecency, higher first;
5. then the shorter path, then the path.

The same files and frecency give the same answer every run. Frecency is a file's number of
reads, each counting half as much after 7 days. `read` and the MCP `read_code` count one read of
each file they read; `files` counts one for a query that is exactly a path it lists, so a caller
that shows the list and opens the chosen path says which it chose by asking for that path. The
counts live in `frecency.json` under `$XDG_STATE_HOME/scrutate/` (else
`~/.local/state/scrutate/`; on Windows `%LOCALAPPDATA%\scrutate\`), keyed by repository root: at
most 1,000 files over every repository, lowest counts dropped first, written whole to a new file
renamed over the old. A file that does not parse is ignored and replaced at the next write; one
that cannot be written leaves the read as it was. Calls of one process wait for each other; two
processes writing at the same moment can lose one count. `--json` gives the envelope with each
`path`, its `match` (`exact`, `basename` or `path`), `edits` and `frecency`, and `stopped_at`
when the limit cut it. No match is exit code 1 with nothing on standard output (with `--json`, the
envelope with an empty `result`); an empty query
is an error (exit 2). The command line does not send `files` to a running `serve`. The MCP tool
`find_files` takes `query` and `limit` and gives the same answer.

### `--deps`: a dependency at its locked version

`symbol --deps PACKAGE.NAME` and `read --deps --symbol PACKAGE.NAME` (the MCP `find_symbol` and
`read_code` with `scope: "deps"`; `repo`, the default, is the repository) answer from a
dependency's source at the version the repository's lockfile pins, read where that version is
installed. `PACKAGE` is the package as the code imports it, `.` or `::` before the definition:
`requests.get`, `yaml.safe_load` (PyYAML), `serde::Deserialize`, `express.Router`,
`@scope/pkg.fn`, `gin.Default` or `github.com/gin-gonic/gin.Default` (a Go module by its path or
its last part, a module go.mod replaces with another by the path go.mod requires and the code
imports, and by the replacement's own, read from the replacement, a major version suffix `/v2` skipped; the last part also without a `.v3` suffix or a
`go-` prefix, as the code names the package: `yaml.Unmarshal` for gopkg.in/yaml.v3, `sqlite3.Open`
for github.com/mattn/go-sqlite3; when that name is two modules', a direct requirement comes before
an `// indirect` one, which no package of the repository imports, and of two direct ones the one
the repository's own Go files import (`yaml` is sigs.k8s.io/yaml beside an indirect
gopkg.in/yaml.v3); when its files import both or neither, the answer is an error that names both,
and the full import path still answers, `gopkg.in/yaml.v3.Unmarshal`). The longest start of the name that is a
locked package is the package. A Go package in a module's subdirectory is named by its import path,
`go.uber.org/mock/gomock.NewController` (the module whose path, after a `replace` the path go.mod
requires, is the longest the import path starts with, and the directory after it, which must hold
Go files), or by its name alone, `gomock.NewController`, when one direct requirement has a
directory of that name whose package clause is that name; of two, the one the repository's own Go
files import, else an error that names every import path (the first three, then how many more). The import path names the directory
and the package clause the name the code writes, so a package whose clause is not its directory's
name is found by its import path only. A dependency's `internal` package answers by its import
path (the dependency's own code imports it), never by its name, which the repository cannot
import; `testdata`, `vendor` and another module's directories are no package. The answer keeps to
that directory and the ones under it. The rest is looked up as `symbol` and `read` look up a name, then
by its last part (`de::Deserialize` finds `Deserialize`). In a Go module or a Python package from the
store (not one a path names), the definitions in the package's own directory come first (a Go package is the directory at its
import path: `yaml.Marshal` lists sigs.k8s.io/yaml's root `yaml.go` before its vendored
`goyaml.v2` and `goyaml.v3`); in each part a definition whose qualified name is the name as asked comes first, so
`assert.Equal` is testify's function and `assert.Assertions.Equal` its method, as in Python
`requests.get` before a method `Session.get`; then a name in its typed case before one that
matches only without case. The answer's first line names the
package, its version, the lockfile and the directory (`serde 1.0.200 (Cargo.lock):
/home/me/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.200`), and every path in it
is the installed file: from the root when it is inside the repository (`node_modules/...`,
`.venv/...`), else whole, written with `/` on Windows too (`C:/Users/me/.cargo/...`). `--json` wraps the answer as `{"package": {name, version, lockfile,
path}, "answer": ...}`.

| Lockfile at the root | Installed in | When the version is not there |
|---|---|---|
| `Cargo.lock` | `$CARGO_HOME/registry/src/*/NAME-VERSION` (default `~/.cargo`) | `cargo fetch` |
| `package-lock.json` (v1 to v3; the top level of `node_modules` only) | `node_modules/NAME`, whose `package.json` must give the version | `npm ci` |
| `uv.lock`, `poetry.lock` | `.venv`'s `site-packages`, the `NAME-VERSION.dist-info` whose `RECORD` lists the files; the import names are the first parts of those files, and under a namespace package (a directory with no `__init__.py`, as `google` is for google-auth and protobuf, and `google.cloud` for google-cloud-core and google-cloud-storage; or one whose `__init__.py` more than one distribution's `RECORD` lists, the pkgutil style of `zope` and `backports`) each part down to the first package of its own (`google.protobuf`, `google.cloud.storage`, `zope.interface`, `backports.tarfile`) | `uv sync`; `poetry install --sync` once poetry keeps its environment in `.venv` (it keeps it under its cache directory, which --deps does not read, unless the project has a `.venv` or sets `poetry config virtualenvs.in-project true --local`) |
| `go.mod` (`require`, and `replace` with another module) | `$GOMODCACHE/MODULE@VERSION`, an upper-case letter as `!` and its lower case (default `$GOPATH/pkg/mod`, then `~/go/pkg/mod`) | `go mod download` |

The version is never another one: when the installed package has another version than the lock
(`node_modules`, `.venv`), the answer is an error that names both and the command that installs
the locked one (exit 2). When a lockfile holds one package at two versions (Cargo.lock often does;
a uv fork does by its markers), or Cargo.lock one version from two sources (the registry and a
git copy), the one read is the one the repository uses: for Cargo the version and source
the repository's own packages (those Cargo.lock gives no `source`) depend on, for Python the one
`.venv` has. When that does not decide it, the answer is an error that names both versions
(exit 2), never the first one in the lockfile. A path dependency (a Cargo `path`, an npm `link`, a uv `editable`,
`directory` or `path` source, a poetry `directory`, a Go `replace` with a directory) answers from
the repository's own index, looked up inside that directory before `--limit` counts (and a read
past its 20 definitions says to name one by its location, as `--deps` takes no path), when it is
inside the repository, and is an error when it is outside; one that names a drive, a share or a root (a `\`, a `:`, a leading `/`)
is refused by its text, with nothing looked at. A git or URL source is an error. A package is read once: its files are
copied to `deps/ECO/NAME@VERSION/src` in the user cache (`SCRUTATE_CACHE_DIR`, else
`%LOCALAPPDATA%\scrutate` on Windows, `$XDG_CACHE_HOME/scrutate`, `~/Library/Caches/scrutate` on
macOS, `~/.cache/scrutate` elsewhere; on Unix its `deps` directories are mode 0700, and a cache
root that belongs to another user or that others may write to is refused; a temporary directory
a crash left is removed at the next new copy once it is a day old), into a temporary directory
renamed into place, and indexed there once, without vectors: later calls read that index as it
is. A copy from the cargo registry or the Go module cache is shared: another repository that pins
the same version reads it, neither copying nor indexing it again, and its call costs about what a
read of the copy as a repository of its own costs. A copy from `node_modules` or `.venv` is the
repository's own (`deps/ECO/NAME@VERSION+ROOT`, a hash of its root): another repository makes its
own. A copy holds at most 20,000 files and 256 MiB; a larger package is an
error and nothing of it is kept. The copy never follows a link out of the package: a linked file
is copied only when it resolves inside it, a linked directory is skipped, and so are nested
`node_modules`, `.git` and `.scrutate`. A name or version in a lockfile that is not one of its
ecosystem's (a `/`, a `..`, a character outside letters, digits and `-._~+`), a directory that
resolves outside the cargo registry or the Go module cache, and a link anywhere from the root down
to a package in `node_modules` (`node_modules` itself included) or to `.venv`'s `site-packages`,
are refused by name. Of a Python distribution only the files its `RECORD` lists are copied, none
that is a link or lies under one. The
repository's index never holds a dependency's files. Answers are redacted as the repository's are
([serve](#serve-mcp)). Not read yet: pnpm and Yarn lockfiles, `callees`' `external:` names and
`search`.

### Answers from the index: one definition for every front end

`symbol`, `callers`, `callees`, `path`, `map`, `refs`, `read`, `outline`, `deps`, `rdeps`,
`similar`, `dupes`, `impact`, `dead` and the ranked search of `search` and `find` are operations
(`ops.rs`). The CLI, the MCP server, `batch`, the Python bindings and the language server run the
same code, with the same check of the files an answer cites and the same recovery from a damaged
index. All but the language server also share the defaults (search `k` 8, callers 30, callees 40,
call path depth 10, references 200, duplicates 50, structural search 1000) and the text or JSON;
the language server reads up to 5000 references and 50 definitions and answers in the protocol's
form. An empty answer has exit
code 1; `outline`, `deps`, `rdeps`, `similar` and `dupes` then print nothing on standard output
and one sentence on standard error. When `symbol`, `callers`, `callees`, `dupes`, `tsquery`,
`read_code symbol`, `duplicates` or `structural_search` stops at its limit with more left, a last
line says so and, but for `read_code symbol` (its 20 is fixed: the line says to pass a path or a
location), names the argument that raises it
(`(stopped at 30 results: raise limit to see more)`): the MCP argument `limit`, and `--limit` for
`symbol`, `callers`, `callees`, `dupes` and `tsquery`. A filtered `symbol` stopped at its bound of
near matches ends with `(100,000 near matches were read before the filter: narrow the name)`
instead, and its JSON has `stopped_reason`. With `serve --structured` the JSON carries
`stopped_at`. With `--json` the envelope carries `stopped_at` and that line goes to standard
error (`tsquery --json` prints records and no envelope, so it has only that line); a `batch`
record that stopped carries `stopped_at` too. A `dupes` or `duplicates` answer
whose functions fell in a MinHash bucket of more than 64 bodies ends with one more line: how many
functions, that a near pair among them that shares no other bucket is not in this answer, and
the first five. The envelope carries it as `crowded` (`functions`, `first`). Under the threshold
at which sampling finds 99% of near pairs, the last line says what share of the pairs at the
threshold it finds (`(near pairs are found by sampling: about 64% of pairs at 0.50 are found)`),
and the envelope carries `sampling` (`threshold`, `found`).

`--max-bytes BYTES` (or `SCRUTATE_MAX_BYTES`) limits the size of such an answer, and of `search`
and `tsquery` answers (`--json` ignores both). A cut answer keeps whole items and ends with a
line like `(12 more results not shown: call again with offset=8, or narrow the request)`;
`--offset 8` continues it. The whole answer, that line included, fits the budget; only a budget
smaller than one item plus that line is passed: the first item is shown whole when it fits the
budget alone, else it is cut to fit (to nothing at a small enough budget) with a line that says
so. A `tsquery` answer stopped at its `--limit` says so in its last line, on the last page only. An agent running the CLI through a shell can set
the variable once, so its harness never cuts an answer in the middle.

`grep` keeps its own budget, so a pipeline (`| wc -l`, `| xargs`) sees every line unless asked:
`grep --max-bytes BYTES` (or `SCRUTATE_GREP_MAX_BYTES`, or `max_bytes` under `[grep]` in the
configuration file; `SCRUTATE_MAX_BYTES` does not apply) prints whole lines in path order, as
`--sort` does, and closes with
`(N more lines not shown: call again with offset=K, or narrow the request)` when it cut the answer;
`grep --offset K` continues it. The whole answer, that line included, fits the budget (one
smaller than a line plus that line is passed, as above), and the exit code is the whole answer's. `-c`, `-l`, `--symbols` and context lines are cut the same way,
by lines; `--json` is never cut. Without a budget the answer is whole, and a plain piped `grep`
takes the path it took before.
### `serve` (MCP)

Nineteen tools over stdio, each an operation above (`search_code`, `grep_code`, `find_symbol`,
`callers`, `callees`, `call_path`, `repo_map`, `read_code`, `outline`, `dependencies`,
`dependents`, `references`, `similar_code`, `duplicates`, `structural_search`, `impact`,
`find_files`, `dead_code`, `reindex`). `repo_map` is `map` (argument `path`), `call_path` is
`path` (arguments `from`, `to` and `depth`, at most 100), both with the same text, and
`dead_code` is `dead`, with its directory or file in `path`. A message is one line of at most
256 MiB, as for `lsp`; a longer one is read and dropped, and answered with a parse error.

Answers are secret-safe: a secret in the text a tool returns is replaced with
`[redacted:KIND]` (`aws-access-key-id`, `github-token`, `private-key`, `env-secret`,
`generic-secret` and the other kinds [Security](SECURITY.md#secrets-in-answers) lists), its key
name, URL scheme or PEM armour left in place, and the page ends with `(N secrets redacted:
SCRUTATE_REDACT=off shows them)`; the structured form carries `"redacted": {KIND: count}`.
Both forms are redacted, `detailed` and `concise`, before the budget pages them, so a page edge
never cuts a secret in two, and a structural search capture as its whole line reads it. A path
that holds a secret is redacted too. The Grep hook and `lsp` answer the same way. The CLI and the
Python bindings print what the files hold; `SCRUTATE_REDACT=on` redacts every CLI answer that
prints file text, in text and `--json` (as values, the closing line on stderr), and a path that
holds a secret in any answer, `files` included; counts stay as they are: `grep` with or without `--index` line by line with colour off (a
`--json` record whose text was redacted loses its `spans`, and its `column` counts bytes of the
redacted text, at the marker when the match is inside a secret), `search` with `--also` too, `read`,
`symbol`, `refs`, `callers`, `callees`, `similar`, `dupes`, `impact`, `review` in its three
formats, `tsquery` and the diff of `tsquery --rewrite` (which then no longer applies to a redacted
line), `batch` and `views`. `SCRUTATE_REDACT=off` turns redaction off everywhere. No tool argument turns it off, so an agent cannot.

Before a call the server brings the index up to date with the files that changed since the last
call. A watch on the tree (FSEvents on macOS, one inotify watch per directory on Linux,
ReadDirectoryChangesW on Windows) names them, `--watch` or not: the call writes a cookie file under
`.scrutate/cookies/` and waits for its event, so every file written before the call started is
named, then it looks at the named files alone (one `stat` each) and indexes those that changed.
On macOS a cookie proves nothing (FSEvents can give a write a later event than a cookie made
after the write returned), so the server watches every file and directory the walk lists (its ignore rules leave the rest out, so an ignored build output takes no descriptor) through a
kqueue instead, whose notes the system posts inside the call that makes the change; a call reads
them at once, with no wait, and looks at the files they name. Until those watches are in place
(at the start, after a directory came or went), for a tree with more files and directories
than half the descriptors a process may open less 1,024 (`sysctl kern.maxfilesperproc`; on a Mac
where it is 61,440, about 29,700; one budget for the process, so two sessions in it share it),
on a volume whose probe write gives no note, or for a tree that
spans devices, every call looks at every file instead
([ADR 0001](adr/0001-change-sets-and-the-cookie-barrier.md)). A file or directory the user cannot
read gets no watch and leaves the others in place: each call tries up to 32 of them again, in
turn (one `open` each), and names one once it can be read (a directory with its files).
When the watch cannot vouch for fewer files (it is not in place yet, it lost events or reported an
error, an ignore rule or a directory changed, the cookie's event took more than a second, or on
Linux a directory got its first watch, new or made again at its path) the call looks at every
file instead, at once. The one
exception is a `--refresh-interval` you give: it holds that walk back as below, so a new directory
or a changed ignore rule can wait up to that interval before calls see it, on macOS too (on
Windows the watch names the files made in a new directory, so calls see those at once, one
`stat` each). The one fallback: on macOS while no kqueue is in place (as above), a call looks at
every file whatever the interval. Even then a call looks at the files the watch named (an edit,
a new file in a directory the watch knows), and the one walk that follows the watches coming up
at start (on macOS, the first kqueue) is never held back. What waits for the next scan under
`--refresh-interval` is a change only a scan can find: a new directory (on Windows only what is
made in it later), an ignore-rule change,
and anything changed while the watch lost events. Until the watches are set again after such a
change, `grep_code` and textual `references` take a look of their own at each call, so a new
directory's files reach them at once, and the code index (`find_symbol` and the rest) at the next
scan. The other case is a
change that makes no event at all (a write through a memory map, a bind mount that does not pass
events on): a walk in the background, off the calls' path, finds those within 60 s, or within 50
times what one walk of the tree takes when that is longer. `SCRUTATE_SESSION_DEBUG=1` says why a
call looked at every file. A tree on a network disk (NFS, SMB, 9p, a FUSE file system such as
sshfs, a virtual machine's shared folder) gets no watch, since a change made on another machine
makes no event here, and neither does a watch that cannot start (the inotify watch limit): every
call there looks at every file, as follows. Without
`--refresh-interval`, scans are at most a second apart, closer when a scan is cheap; with
`--refresh-interval S` (or `SCRUTATE_REFRESH_INTERVAL`) no scan comes sooner than S seconds after
the last. The interval runs from the last scan before a call; an update `--watch` makes does not
restart it. Under `--refresh-interval`, a scan that could not run because another process held the index does not
start the interval. Inside that window an answer still
checks, one `stat` each, the files it cites and those its arguments name: when one changed or is
gone, or a named file is new and a scan would index it, those files alone are indexed again and
the answer computed again, once. A change in a file the answer does not cite or name waits for
the next scan. At the end of its input (EOF) the server answers the calls in flight, lets a
running index build finish, and exits. So a client killed during the first build leaves the
server running until that build is committed; a server started on
the same root meanwhile is not held up by it, and answers from that build when it is done, where
stopping it would leave the next server to build from the start.

While it runs, the server offers its open index to command line calls of the same user on the
same root: `symbol`, `callers`, `callees`, `path`, `map`, `refs`, `outline`, `deps`, `rdeps`,
`read`, `similar`, `dupes`, `dead`, `search` (of this repository alone: with `--also` another
root it does its own work) and the ranked part of `find`. Not `impact`, since git runs in the
command's own environment, nor `files`, which needs no index. The files that changed are looked
at first, as for a tool call (every file when the watch cannot vouch for fewer), and the output is the same bytes and exit code a fresh process gives. The meeting point is a socket file on Unix,
`.scrutate/session.sock` (owner only; no network port), removed when the server exits or its root's
path comes to resolve to another directory (a link pointed elsewhere), and a named pipe on Windows, `\\.\pipe\scrutate-session-<hash of the user
and root>` (owner only, local callers only), which goes away with the server, or when it stops
listening because its root moved to another tree, and leaves no file. A
command never starts a server. When anything about the server differs from how the command would
do the work (another build of scrutate, another `SCRUTATE_*` setting that can change an answer,
which is every one but `SCRUTATE_ROOT`, `SCRUTATE_NO_SESSION`, `SCRUTATE_SESSION_DEBUG`,
`SCRUTATE_SESSION_WAIT_MS`, `SCRUTATE_MAX_BYTES` and `SCRUTATE_REFRESH_INTERVAL`, another root, a
stale socket), the command does the work itself and says nothing;
`SCRUTATE_SESSION_DEBUG=1` says why on stderr, and `SCRUTATE_NO_SESSION=1` turns the use off. A
command waits 20 ms for the server's greeting (a server stopped in a terminal costs no more);
`SCRUTATE_SESSION_WAIT_MS` sets another wait for a machine too busy to greet in time. A server
that takes no request for 2 s once it greeted (stopped or stuck, with a request longer than the
socket or, on Windows, the pipe holds) is given up as well, and so is one whose answer takes over
60 s. A server takes at most 16 connections at once (a further caller does its own work) and a
request of at most 1 MiB. Of two
servers on one root, the first offers the socket.

- Every tool but `reindex` and `edit_symbol` is marked read-only (`readOnlyHint`), and all as
  staying on this machine (`openWorldHint: false`), so clients that ask before a write do not
  ask for these. `edit_symbol` is listed only by `serve --allow-edits`
  ([Edits](#serve---allow-edits-and-edit_symbol)), marked `destructiveHint: true`.
- The server returns `instructions`: which tool to start with, that answers are capped, and how
  a concise answer writes its paths.
- Every read-only tool takes `max_bytes` and `offset`, as `--max-bytes` and `--offset` above;
  the whole answer, closing line included, fits `max_bytes`. The default is 24,000 bytes (about
  6,000 tokens); `serve --max-bytes N` (or `SCRUTATE_MAX_BYTES`) sets another, and `tools/list`
  shows it. A call's `max_bytes` and the server's default are both held between 1,000 and
  1,000,000, and a call's own value wins. `serve` refuses `--offset`: each tool call takes its
  own.
- Every tool but `edit_symbol` takes `response_format`: `detailed` (the default, the text described here) or
  `concise`, which keeps every location and name and drops what a call can ask for again: code,
  snippets, kinds, scores, the containers of a name (`Answer.text` is `text`) and the notes that
  explain an item. Closing lines (how to continue, where an answer stopped) and errors stay. A
  concise page holds the same items as the detailed page of the same `max_bytes` and `offset`,
  so an offset means the same in both; when its text would pass `max_bytes`, the detailed page
  is sent in its place. Paths are written once per directory: a line ending with
  `/` names a directory (`./` the root), and a path without `/` below it is in that directory (a
  path in another directory on the same line is written whole, a file at the root `./name`); a
  location written `:start-end` (or `:line`, `:line:column`) is in the file of the location
  before it in the same section: the first location after a section's head (`changed:`,
  `findings:`, `references (...)` and the like) names its file below a directory line of its
  own. With `serve --structured` the JSON is the detailed one in both forms. Any other
  value is an error (`response_format must be "detailed" or "concise"`). Per tool, concise gives:

  | Tool | Concise answer |
  |---|---|
  | `search_code`, `find_symbol`, `callers`, `callees`, `similar_code` | one line per result, location and name (`callees` counts the external names in place of listing them) |
  | `grep_code` | the first line, then per symbol its location, name and the numbers of its matching lines without their text (a file with no enclosing symbol: its path and the numbers) |
  | `read_code` | by path: the lines read that begin a definition the index knows, `start-end  first line`, and the closing line (a page with none says so); by symbol: each definition's location and name, its first line, and the definitions inside it, `start-end  first line` |
  | `outline` | each symbol's first line number and name, one space of indent per level |
  | `dependencies`, `dependents` | each file the imports resolve to and each module that resolves to none, once; each importing file once |
  | `references` | per definition its location, name and count, then each use as `path:line:column`, and the unresolved uses the same way, without line text or reasons (`textual`: the definitions and callers as locations, the matching lines as `grep_code` gives them) |
  | `duplicates` | one line per group, the similarity and the members' locations, with one name when they share it |
  | `structural_search` | `path:line:column @capture text`, without the node kind |
  | `repo_map` | each file and its definitions' `start-end name`, without rank, score, kind or calls |
  | `call_path` | `path:line -> location name` per step, `(by name)` for a call the resolver did not tie |
  | `impact` | the same sections, a changed definition as `location name (status)`, a caller as `distance location name`, `?` after the name of an undecided one; a test as `location name (test, distance)` when the definition that reaches the change is in it, else `path (distance via location name)` |
  | `dead_code` | `location name`, `?` after an undecided one |
  | `find_files` | the paths in rank order, with a directory line each time the directory changes |
  | `reindex` | the same line |

  Where the detailed answer is over 1 KB, the concise one is at most half of it where it carries code or
  snippets, and at most two thirds where it is already only places and names (similar code, an
  outline of long names) (`tests/concise.rs`). The command line has no concise form.
- Arguments: a wrong type, or a negative number where a count is taken, is a tool error naming the
  argument (`k must be a non-negative integer`; `read_code`'s `start` takes any integer, one below 1
  reading from line 1, and an `end` before `start` is an error, and `duplicates`' `threshold` any number), as is a required
  argument left out (`missing required
  argument name`; `read_code` without either says `missing path or symbol` and what each takes),
  and JSON `null` is the same as leaving an argument out.
  An error quotes at most 200 characters of an argument. Every path argument may be written `./x`
  or `pkg/../x`. In text answers, control characters in a path show as escapes (`\n`, `\x1b`),
  and so do the characters that reorder text or break a line (bidi marks, embeddings,
  overrides and isolates, U+2028 and U+2029, as `\u{202e}`); the JSON form keeps them as they are. A path or location written that way is read back, so
  what an answer prints can be passed to the next call.

Per tool:

| Tool | Arguments and answer |
|---|---|
| `grep_code` | An Engine A search of the files the index holds. Hidden files such as `.github/` are searched (with a text index, its trigrams say which of the files it holds as they are now can match, and the other files are read; the answer is the same. Which files are there and as they are now comes from a look at the tree; a server whose watch runs keeps that look up to date with the files each change set names, one `stat` each, so a call walks nothing, and it looks again when the watch cannot vouch for every file, as on macOS while no kqueue is in place); ignored files, `.git/` and the directories the index skips by name (`node_modules` and the like) are not. (`scrutate grep` skips hidden files unless `--hidden`, and reads those directories unless an ignore file excludes them.) The pattern is a regex unless `mode` names another (`literal`, `identifier`, `fuzzy` and `fuzzy_regex` within `max_edits` edits, default 1, or `pcre2`), as `grep -F`, `--ident`, `--fuzzy`, `--fuzzy-regex` and `-P` do; `case` is `sensitive` (the default), `insensitive` or `smart`, and `word` keeps whole words. `path` keeps one file or directory of the repository (answers still name files from the root; one that leaves the repository is refused, and one that leaves it and comes back by its parts, `../repo/src`, is read from the root as `src`, as every tool's path is) and `globs` keeps the files they match, as `grep -g` (`!` excludes); `globs` brings back no directory the index skips, but a `path` that names one (`node_modules`, `.git`) searches it. Matching lines are grouped by enclosing symbol; `max_results` (default 100) stops it, and the answer says so. A capped answer holds the first matches in path order, so the same call gives the same lines every time and a second page neither repeats nor skips one; `references` does the same, over the same files. A file whose search a NUL byte stopped after a match ends its last group with a `note:` line (`"note"` in JSON). An invalid pattern is an error. `max_results` is at most 5,000: a larger number is lowered to it, and the answer ends with a line that says so (`capped` in the structured answer). |
| `search_code` | Takes at most 10 `alt_queries`; a cancelled search stops between them. `search_code`, `find_symbol` and `references` take the filters `globs`, `types`, `langs` and `kinds` (see Filters above), except `find_symbol` with `scope: "deps"` ([`--deps`](#--deps-a-dependency-at-its-locked-version)). A cancelled `grep_code` or `references` stops between files. |
| `read_code` | Reads a line range of a file (`path`, `start`, `end`), or with `symbol` a whole definition: a name, `Class.method` or a location. A name with several definitions returns each (up to 20), and `path` keeps those in that file, narrowing before that limit, so a file's definition is found however many other files define the name. A definition longer than 400 lines shows its first 400 and closes with `(the definition goes on to line N: read_code path=P start=S)`. With `end`, every line up to `end` is read; without it, 400 lines. The answer closes with `(lines A-B of N in P: read_code path=P start=B+1 for more)` while lines are left. An empty file answers `P is empty`, a `start` past the end `P has N lines: start=S is past the end`; an `end` before `start` and a path that is not a regular file are errors. A file is read through a 64 KB buffer, with at most 1 MB of line text (a longer line is cut and says so): a file of at most 1 MB is read to its end to count its lines, a larger one only as far as the last line asked for. A file with a NUL byte in its first 8 KB is refused; a definition read with `symbol` goes through the same reader, so it fails as its file's read would. A path that leaves the repository by its own parts answers `path outside repo: give a path relative to the repository root, or an absolute one under it` (`grep_code` with such a `path` too) with no file looked at, and a link that leaves it answers as a missing file does. An absolute path is one under the root as `config --json` prints it or as the server was given it; any other spelling of the same directory (through a link) counts as outside. A file over 1 MB not read to its end closes with `of P, more lines follow` in place of the line count, and the JSON `file_lines` is `null`. With `symbol`, `history` (an integer, default 0, at most 50) is `read --history`: each definition's JSON gains `history`, a list of `{commit, date, subject}` (with `cut` on a shallow clone's oldest commit), and `history_note` where it has none or stopped short; the answer gains `history_note` when no definition has one and `history_more`, the definitions past the first 10. With `scope: "deps"`, `symbol` is `PACKAGE.NAME`, a definition of a dependency ([`--deps`](#--deps-a-dependency-at-its-locked-version)); `path`, `start`, `end` and `history` do not apply. |
| `references` | Ends with a closing line when `max_results` cut the search (at most 5,000: a larger number is lowered to it, and the answer ends with a line that says so, `capped` in the structured answer), answers `(no results)` when nothing is found, and refuses an empty name. |
| `structural_search` | Refuses an empty query, a query without a `@capture` and an unknown predicate, as `tsquery` does. |
| `impact` | Takes `base` (a revision, `main...` for a merge base), `staged` (boolean) and `depth` (default 3, at most 10), and answers as `scrutate impact` does (see `impact` above). |
| `find_files` | Takes `query` and `limit` (default 20, at most 1000) and answers as `scrutate files` does (see `files` above). A `read_code` call counts one read of each file it reads. |
| `reindex` | Takes `force` (boolean): every file is read again, also one whose size, modification time and change time did not change, and a damaged index is rebuilt. It also updates the text index when there is one, so `grep --index` sees what it indexed; it never makes one. |

- A `path` argument is a path from the root or an absolute path under it, in any spelling that
  names the same file (`./a.py`, `pkg/../a.py`, `<root>/src`, and the root as the server was
  given it, through a link or macOS's `/tmp`); an escape an answer printed for a
  file (`a\tb.py`) names that file, and a name that holds a backslash itself is read as written
  when no such file exists. `repo_map`, `dead_code` and `grep_code` refuse a path that is not
  there.
- Every answer begins each result with its location (`path:start-end`). A location can be passed
  wherever a tool takes a symbol, to mean exactly that definition among namesakes:
  `read_code symbol`, `find_symbol`, `callers`, `callees`, `call_path from` and `to`,
  `similar_code target`, `references def`. For `callers` of a location, the resolver decides when
  another definition of the same name in that language could be meant; a call site it cannot
  decide is kept, and a closing line counts those. `callees` of a location lists the calls inside
  that definition.
- `serve --short-tools` (`SCRUTATE_MCP_SHORT_TOOLS=1`) sends a short `tools/list`: a few words
  per tool, no argument descriptions, and for a read-only tool only `readOnlyHint` and
  `openWorldHint` (the other two hints matter only for a tool that writes). The three shared
  arguments (`response_format` for every tool but `edit_symbol`, and `max_bytes` and `offset` for
  every tool but `reindex` and `edit_symbol`) are
  left out of each schema and named once, with types and defaults, in the server's
  `instructions`. Every other argument name, type, enum, default and `required` is the full
  list's, and a call is checked as without the option. The full list is under 24,000 bytes and the
  short one under 7,800 (the `tools/list` result as compact JSON), and the instructions that name the
  shared arguments add under 260 bytes: budgets `tests/mcp_protocol.rs` holds.
- `serve --structured` (`SCRUTATE_MCP_STRUCTURED=1`) adds an `outputSchema` to every tool and
  `structuredContent` to every result, beside its text: the envelope the matching CLI command
  prints with `--json`, plus `complete` (`{"schema": 2, "complete": true, "result": ...}`). When text and JSON together pass one and a half times `max_bytes`, or `offset` is
  above 0, the result is a page: `complete` is `false`, `result` has the shape of the whole
  answer with only the items the text page shows (the same items in `detailed` and `concise`),
  `offset` is the first of them, `next_offset` the `offset` of the next page (`null` on the
  last) and `total` the number of items. `pending` is there when the server's own update ran past
  the read's wait: the changed files the answer does not hold yet
  ([The index directory](#the-index-directory)); `capped` (`{"max_results": 5000}`) when
  `max_results` asked for more than `grep_code` or `references` takes. An item is what the text pages by: a result, a
  `grep_code` group, a line of `read_code`, `references` or `impact`; a definition or group whose
  lines a page boundary cuts is on both pages, each with its own lines, and `callees` names its
  external callees on the last page. A failed call has a `null` result and the error in `text`.
  It is off by default because clients differ in what they show their model: Claude Code
  2.1.322 shows the JSON in place of the text, and the text is the form with the snippets
  (measured with that client on 2026-11-11). Turn it on for a client whose scripts read tool
  results, such as Codex in Code Mode.
- Each call runs on its own thread, up to 32 in flight, so `notifications/cancelled` reaches it;
  a call past that runs on the thread that reads the messages, so nothing more is read, a cancel
  of it included, until it ends. Calls run side
  by side, each on its own reader (up to the number of cores, at least 2 and at most 8; one more
  waits for a reader). On cancel, a call waiting for the index or a reader stops, an index update
  it runs (`reindex`, or the scan before a read) stops and rolls back at its next batch, a
  `structural_search` stops reading files, and the answer of one already computing is dropped.
  Nothing more is sent for a cancelled request.
- While a repository's first index is being built, by this server or another process, a call
  waits up to 20 seconds and then answers that the index is still being built. A call carrying a
  `progressToken` waits for the index instead and gets its answer.
- A call carrying a `progressToken` gets `notifications/progress` while the first index is built
  and while it runs, at most one every half second and only when the count moved: `progress` is
  the files read so far and `total` the files to read, both counted by an index update (the
  first build, `reindex`, the scan before a read) and by `structural_search`; `message` says
  which. `progress` rises with every report, also from the build to the call. No report comes
  after the answer, and none after the call has seen its `notifications/cancelled`. The text index that `reindex` also
  updates is not counted, and a call that reads no files sends none.
- Protocol: a client that opens with `initialize` is served revisions 2024-11-05, 2025-03-26,
  2025-06-18 and 2025-11-25. A request naming its version in `_meta`
  (`io.modelcontextprotocol/protocolVersion`, a string) is served by revision 2026-07-28, which has
  no handshake: `server/discover` describes the server, every result carries `resultType`, lists
  carry `ttlMs` and `cacheScope`, and another string version in `_meta` gets error -32022 and the
  supported one. A version that is not a string is served as the handshake era, and so is
  `initialize` whatever its `_meta` says. Both eras work on the same pipe. An `initialize` with a missing or unknown
  `protocolVersion` is answered with 2025-11-25.
- JSON-RPC: a batch (an array of messages) gets one array. A blank line is skipped. A line that is
  not valid JSON, or not UTF-8, gets error -32700, and the server goes on reading. A message that is not an object, an
  empty array and a request without `method` get -32600; an unknown method -32601; an unknown
  tool or a `tools/call` without `name` -32602. A message without `id` (a notification) is never
  answered; only `notifications/cancelled` does anything. A client's response (a `result` or
  `error` without `method`) is ignored.

#### `serve --allow-edits` and `edit_symbol`

`serve --allow-edits` lists one more tool, `edit_symbol`, the one tool that writes source files.
Without the flag it is not listed, and a call of it is an unknown tool. Only the flag turns it
on: no environment variable or configuration file does, so a repository cannot.

`edit_symbol` replaces one whole definition with `new_text`. `symbol` names it as `read_code`
takes one (a name, `Class.method`, a location), and `path` keeps the ones in one file; a name
with more than one definition is an error that lists each one's kind and location and gives one
location to pass (`A names 2 definitions in a.rs: struct at 1-1, impl at 3-7: pass one location
(a.rs:1) to pick the one to edit`; without `path`, pass `path` or one location). A module (a
file's top level or a C++ namespace, `[module] N` in `read_code`) and a window are not edit
targets: a name that matches only one is refused with its kind and lines (`N names a module
(b.cpp:1-3), not a definition`); name a definition inside it. When the name also matches a
definition, the module or window is no candidate (a file's module chunk carries the file's name,
so `func G` in `g.go` is one candidate). A name matches whatever its case, as `read_code` takes
it, but when some candidates' own names are the name in the case typed, only those are (`F` the
class `F`, `f` its method `F.f`); in a case none has, all stay. A call is a dry run unless
it passes `apply: true`: the answer gives the unified diff (the `tsquery --rewrite` form) and
`hash:`, 16 hex digits of the file's content hash. A file marked read-only (on Unix, no write
bit for its owner; on Windows, the read-only attribute) is refused, a dry run too: `PATH is
read-only (...): edit_symbol does not write a file marked read-only; nothing written`.
`apply: true` writes only with that `hash`, and only while the file still has it: a file changed
since is refused with its current hash and nothing written. Before it writes, the new file is parsed with its language and the edit is
refused, with the reason, when it adds a parse error or a missing node (the answer names the
line of the first one the old file did not have, in the new text when there is one there); when
the new text does not define the same name and kind where the old definition began (an edit
never renames); when
it defines another definition beside it (the body twice, a second function); or when a
definition outside it is gone, moved or changed: one around it (the class of a method) keeps its
name and kind, every other one its text too. Code that is no definition is kept the same way: a
new text that adds code outside the definition (a statement after it) is refused with `the new
text adds code outside the definition`, and one that changes code before or after it (a comment
or string left open that swallows what follows, a statement continued) with `the new text
changes code after it` or `changes code before it`, and one that leaves an attribute or a doc
comment after the definition, where it would mark the next item (Rust's `#[cfg(any())]` or
`///`), with `the new text leaves an attribute or a doc comment after the definition`; nothing is
written. A definition declared together with another
(`const a = 1, b = 2`, `var a, b = 1, 2`) is refused, the declaration holding both. The edit replaces the definition's lines whole, so one
that shares its first or last line with other code that is not a comment (another definition, a
statement before or after it) is refused, a dry run too: `NAME shares a line with other code that
is not a comment (PATH:LINE): edit_symbol replaces whole lines; nothing written`. A comment before
the definition on its first line or after it on its last (`type ID int // the user id`,
`function f() {...} // end`) belongs to the definition, as `read_code` shows its lines, and is
replaced with it; the dry run's diff shows it. So is a lone `;` right after the definition on its
last line (`struct P {...};` in C and C++, a template's too, `interface I {...};` in
TypeScript); a declarator there (`struct P {...} p;`) is other code. A comment that runs on past
those lines is other code. The definitions nested in it may change. A definition's
range is the index's: its attributes are in it, so `new_text` replaces them too, and the comments
above it are not. `edit_symbol` keeps the comment above a definition unless `new_text` brings its
own doc comment: a `new_text` that opens with the language's doc-comment form (Rust, Swift, C and
C++ `///` or `/** */`; JavaScript, TypeScript, Java, PHP, Kotlin, Scala, Groovy and Dart `/** */`;
C# `///`; Go any `//` block, so a banner directly above a Go function goes with it) replaces the
doc comment right above the definition when every line of it is in that form, no blank line
between, and it neither opens the file nor is an inner (`//!`, `/*!`) or `#!` line; the dry run's
diff shows it taken away. Otherwise (Python, whose doc is the docstring, a licence header, a
banner) the new comment lands right above the definition and the old lines stay. A definition that
had none moves down by those lines. Taking a comment away is an edit of the file's lines. The new text takes the line ending of the definition's first line (`\r\n` stays `\r\n`,
and one `\r\n` line elsewhere in a `\n` file changes nothing), and the rest of the
file is written byte for byte as it was. The file is written as `tsquery --rewrite --write`
writes one: opened through its directory with no link on the way, its bytes checked to still be
the ones read, then a new file in the same directory renamed over it, its permissions kept. A
file under `.git/` or `.scrutate/`, a link, a file that is not UTF-8 text, and a language with
no grammar are refused. The answer to an applied edit names the definition's new lines and the
file's new hash; the next call sees the edit in the index, as it sees any write. The JSON form:
`path`, `start`, `end`, `new_start`, `new_end`, `applied`, `hash`, `diff`.

### `lsp`

```
scrutate lsp
```
A language server for editors (JSON-RPC 2.0 over stdin and stdout, `Content-Length` framing).
Each workspace folder (`workspaceFolders`, else `rootUri`) is answered from the index of the
repository holding it, found as for a command run in that folder, so the editor and a shell there
share one index, and two folders of one repository share it too; `--root` when the editor sends
no folder. A request about a document goes to the folder that holds it; `workspace/symbol` asks
every folder. The first folder's index is opened at `initialize`, the others when a request first
needs them. The server takes `workspace/didChangeWorkspaceFolders`: an added folder is answered,
and a removed one's index is closed unless another folder uses it. A request is answered from
the repository that holds the folder, so a file outside every folder's repository gets no answer.
`workspace/symbol` leaves out a folder that cannot answer (its index cannot be opened, or its
first build is still running, which is not waited for when there are other folders), says so
once in the log, and fails only when no folder answered: then with -32801 when a folder is still
building, which asks the editor to ask again. Locations name files under the folder as the editor sent it, also when it is reached
through a link, so the editor finds them among its open files; a repository the editor names in
two ways (two folders, one through a link, or a file named through one) answers a request in the
spelling of its document, and `workspace/symbol` in the first folder's. With no folder and no `--root`, started in the home directory or at
a filesystem root with no index, the server builds none and says so in the log. It answers from
the Engine B index:

| Request | Answer |
|---|---|
| `textDocument/definition` | the definition the resolver ties the name under the cursor to, the same one `textDocument/references` names (`item.total()` after `item = Order()` is `Order.total`); when the resolver cannot place it, definitions of the name, as `symbol` finds them: first the run of characters around it (`get-user`, `list->vec`, `Foo::Bar`), then the plain word; with a qualifier (`Point.norm`, `a::b`), only the definitions whose qualified name matches, when there are any; definitions in the same file first. The name is taken as the file's language writes it, with or without a receiver before it: `valid?` and `save!` in Ruby and Elixir, `update!` in Julia, `step'` in Haskell, `is.ready` in R, `~Shape` and `operator==` in C++ (a wider name only when the index has it, so `x-y` in Python is still `x` minus `y` and `~mask` is still the bitwise operator). In Rust the name of `r#type` is `type`, with the cursor on either part. A Haskell or Elm function that starts with its type signature is answered at the equation below it |
| `textDocument/references` | for the same name, the occurrences `refs` resolves to the definition under the cursor, and that definition (left out when `includeDeclaration` is false); when the cursor's occurrence is unresolved, every whole-word occurrence, as `refs --textual` |
| `workspace/symbol` | symbols the query matches as `symbol`'s near matches do: the name itself, then prefix, humps and contains matches; an empty query lists every name, shorter first; each folder's after the one before, at most 200 in all; a file under a folder opened inside another (a nested repository) is listed once, from the deepest folder that answered, as a document request goes to it |
| `textDocument/documentSymbol` | the `outline` of the file, nested (methods under their class); a definition that starts on the first or last line of another and is not named inside it is its sibling, its range starting at its name (the index keeps lines, not columns) |

It advertises only these four, plus full text sync, workspace folders with change
notifications and `positionEncoding: utf-16`. Open documents are read from the editor's
text, so an unsaved line still finds the word under the cursor; locations in other files come
from disk. `didSave` updates the index incrementally in a background thread, so requests keep
being answered; before a request the index is scanned for changes, at most a second apart and closer when a scan
is cheap, as under `serve`. With no
index, `initialize` builds one (as `scrutate index` does, without the text index) and reports it
with `window/logMessage`; a request that arrives meanwhile waits for it up to 20 seconds, then
is answered with error -32801 (ContentModified), "the index is still being built", which asks the
editor to ask again. Requests read within a second of that answer were queued behind it and are
answered so at once, so a `shutdown` behind any number of them is read within one wait. Messages
are read on a thread of their own, so `$/cancelRequest` reaches the request it names while it
runs: its wait for the build and its search stop, and it is answered with error -32800
(RequestCancelled); one cancelled while it waits its turn is answered so without running, and a
cancel for a request already answered is dropped. What the code index leaves out (see [Files
the index leaves out](#files-the-index-leaves-out)) goes to the log as a `window/logMessage`
warning, never a popup: once with the first answer from an index that leaves files out and again
whenever that set changes, and once per left-out file the first time a request names it (a
document's symbols, or a definition or references from a position in it); an answer itself is
the protocol's, empty where it found nothing. Columns are UTF-16 code
units, as the protocol requires. The exit code is 0 after `shutdown` (then `exit` or the end of input), 1 otherwise.
Log messages go to the editor (`window/logMessage`); the index in use, when it is found above the
editor's folder, and a failed index update after a save go to stderr, and a refusal to open the index to both.

### `watch`
```
scrutate watch [PATH]
```
Sets up the watch on `PATH` (default `--root`), then updates the index once, so a change made
during that first update is seen, prints `[scrutate] watching` and the resolved path on stderr, and waits for filesystem
events. While another process writes the index, an update waits for it (up to 10 minutes), the
first one too; Ctrl-C stops `watch` at once, also during that wait for the code index's or the
text index's lock, with exit code 130, and
an update that is running stops at its next batch of files. Started with a pipe for its standard
input (an agent, an editor or a script that starts it), `watch` stops the same way, with exit
code 0, when that pipe ends, so it ends with whoever started it, killed or not; from a terminal,
or with no input (`/dev/null`, a service manager's default), it runs until it is stopped.

Per platform:

| Platform | How `watch` sees changes |
|---|---|
| Linux | each directory the indexes can hold gets its own inotify watch, an ignored tree none (2 watches instead of 7,012 on a tree with an ignored `node_modules` and `target`). The set is walked again after a burst that creates, removes or moves a directory, so new, moved and replaced directories are watched. A directory moved, removed or made while the watches are being set, after the walk listed the tree and before its watch, is caught by walking the tree again, at most 3 times; after that every directory is checked on the timer below, and stderr says so. The root is watched first, then shallow directories before deep ones, by path; when some get no watch (past `fs.inotify.max_user_watches`, or a directory that cannot be read) stderr says how many, and at the watch limit names that sysctl. Those directories are checked on a timer instead: every second or, when a check takes long, after a pause ten times as long as the last one (at most 30 s), their entries are listed and compared by size, modification time, inode and status-change time, and what changed joins a burst as an event would. `.git/info` gets a watch too, for its `exclude` file, also the main repository's when the root is a linked worktree. |
| Windows | one recursive ReadDirectoryChangesW watch on `PATH`, with a 64 KiB buffer. A burst too large for the buffer loses its change records; the `notify` crate reports that as lost events, so the tree is walked. |
| macOS | one FSEvents stream on `PATH` with file-level events, on its own dispatch queue. CoreServices is loaded with `dlopen` when `watch` starts, so no other `scrutate` command links or loads it. A stream on a directory above the root would report every change under it, so the rule files outside the root are checked on the timer the Linux row describes. When FSEvents cannot be loaded or started, stderr says so and `watch` runs the index's own incremental check instead, every second or, on a large tree, after a pause ten times as long as the last check took (at most 30 s). |

Events under `.scrutate/` and `.git/` are dropped. A file of a burst is kept when an index's walk
lists it in its directory, so every rule the walks follow decides: ignore files above the
repository top, a nested repository's own rules, a linked worktree's `info/exclude`, the skipped
build and vendor directories and `SCRUTATE_INDEX_EXCLUDE_DIRS` for the code index, hidden paths
for the text index. A burst is collected until 200 ms pass without an event (at most 2 s). A
burst naming only files (written, created, removed or renamed in directories
already watched) updates those files alone, with no tree walk and no new watches, so a saved file
costs the same in a tree of 20,000 directories as in a small one. The usual incremental update of
the whole tree, reading the ignore files again, runs for a burst that:

- creates, removes or moves a directory;
- renames a file to another case of its name on a disk that ignores case (macOS's default),
  where the old name still opens the file;
- changes an ignore file (`.gitignore`, `.ignore`, `.rgignore`, `.git/info/exclude`) or
  `.scrutate.toml` in a directory the walks enter (one in an ignored directory, as the
  `.gitignore` `python3 -m venv` writes in `.venv/`, changes nothing), also a rule file outside
  the tree: an `.ignore` or `.rgignore` in any
  directory above the root, a `.gitignore` above it up to the repository top,
  `.git/info/exclude` of each repository nested in the tree, git's global excludes file and the
  git configuration files that name it;
- reports lost events (an inotify queue overflow, an overflowed Windows buffer, an FSEvents
  stream that says it dropped events or must rescan);
- changes an indexed file once the last walk is 30 s old, which is how a change the watcher did
  not see is found;
- is the retry of a burst whose update failed (stderr says why): it comes with the next burst or
  after a pause of 1 s that doubles up to 30 s, whichever is first, so that burst's files are
  indexed with no further change.

On Linux each directory above the root gets its own watch for those rule files, and another file
written there costs a filter and nothing more. When such a directory cannot be watched the
watcher says so once and checks its rule files on the timer instead; a rule directory made after
the last walk applies with the next tree walk. Either way the code index is updated, and the text
index when `.scrutate/text/` exists (`watch` never creates one; `scrutate index` does).

`SCRUTATE_WATCH_DEBUG=1` prints one stderr line per burst (the number of paths, and whether the
tree was walked). Each update that changed something prints one stderr line. A failed first
update stops the watch; a later failure is reported and the watch goes on. Ctrl-C stops it (exit 130):
a code index update in progress stops at its next batch of files, or block of rows while a large
segment's vectors are clustered, and leaves the index as it was; a text index update in progress
ends first. When `PATH` is removed or moved away and nothing takes its place within 2 s, `watch`
stops with an error (exit 2); when `PATH` itself comes back within the 2 s (renamed away and back), `watch` goes on watching it, and when a link to it is put where it was, `watch` says `root replaced; watching the new tree` and watches the tree through the link. When another directory takes its place (moved or copied there, as `mv repo.new repo`
does, or removed and made again), stderr says `root replaced; watching the new tree`, and `watch`
opens that tree's index (building it when it has none) and watches it. A `PATH` that goes through
a link (a deploy's `current`) is followed the same way when the link is pointed at another
directory, also through a moment when it names nothing; on Windows a junction is resolved the same
way, which no test covers there; `serve --watch` and the
watch of the MCP server, the language server and the Python bindings do the same, each opened
by the path as given (the MCP server's `--root`, the editor's folder). When their root's path comes to
resolve to another directory (a link pointed elsewhere), `serve` removes its session socket in the old tree (on Windows it stops listening
on the old tree's pipe and frees its name once the calls in flight end) and answers no caller of that tree, so a command line
call made there does its own work and a `serve` started there can offer its own session. Other processes can read and update the same index meanwhile; the indexes' own
locks order the writers, and stopping the process mid-update leaves the previous state in place.

`serve --watch` runs the same watcher in a background thread while the server answers; when the
watcher stops, stderr says why and the server goes on answering. Watcher and tool calls share
one session: the watcher's updates and the scans before calls take one writer lock, so they run
one at a time, and a call that reads meanwhile sees the index as of its last commit.

### `tsquery` cache
Each file's captures are kept in `--root`/.scrutate/tsquery/, one file per (scrutate version,
language, query text, searched paths from `--root`), keyed by a hash of the file's content and
name. No absolute path is part of a name, so a copy of the tree with its `.scrutate/` uses the
same cache. The hash is the one the code index records, so with an index in `--root` a file
whose size, modification time and change time the index holds as current is not read when its
captures are cached. A file whose content was seen before is not parsed again; the output is
byte-identical to `--no-cache`, which parses everything and leaves the cache alone. A run that
visits every file keeps only the entries it used, so edited and deleted files do not accumulate.
The directory is bounded at 128 MB: beyond it the least recently used query files are deleted,
and a query whose results alone exceed it is not cached. The cache is safe to delete at any time.

A file that holds none of the strings every match of the query needs is not parsed either: when
each pattern compares a capture that is in every match with strings (`(#eq? @f "kmalloc")`,
`(#any-of? @k "get" "set")`), a file whose text has none of them answers with no captures, as
parsing it would. A pattern with no such predicate (a capture compared with a capture, `#match?`,
a `not-` or `any-` form, an optional capture) leaves every file parsed.

### `tsquery --rewrite`
`scrutate tsquery LANG QUERY [PATHS] --rewrite TEMPLATE [--write]` replaces each match of the
query with TEMPLATE. The replaced text is the capture named `@match`; a match without one
replaces the capture that starts first, the longest of those, then the first written. In the template `$NAME` is the text of
capture `@NAME`, `${NAME}` the same before a letter or for a name with characters other than
letters, digits and `_`, and `$$` a dollar. A capture with several nodes gives the text from its
first node to its last; one the match does not hold gives nothing. A template naming a capture
the query lacks, or with a `$` without a name, is an error.

```
scrutate tsquery python '(call function: (identifier) @f (#eq? @f "print") arguments: (argument_list) @args) @match' --rewrite 'log$args'
```

Without `--write` it changes nothing and prints a unified diff of every file it would change
(three lines of context, `--- PATH` and `+++ PATH` as `tsquery` prints the path, so `patch -p0`
applies it). With `--write` it changes them and prints `PATH: N replaced` per file written. Exit
code 0: something to change; 1: nothing; 2: a path cannot be read or a file was left alone.
`--json`, `--limit` and `--offset` are refused with it, `--max-bytes` does not cut the diff, and
it never reads or fills the `tsquery` cache: each file is read and parsed afresh.

Of two overlapping matches only the outer one is rewritten (of two over the same text, the first
pattern's), and the inner one is named in a warning
(`a.py:1:7: inside the match at 1:1, not rewritten`). The text the outer one keeps of its
captures is the file's text as read, so a second run may rewrite what the first skipped. A match
whose template gives back the same text is no change. Once the template's text no longer matches
the query, a second run finds nothing to change and exits 1.

Each file is written whole: its bytes are read once, the new bytes go to a temporary file next to
it (`.NAME.scrutate-PID.tmp`) with the file's permissions, and that file is renamed over the
original only if the original's bytes on disk are still exactly those read; a file changed
meanwhile is left alone and named in a warning. A file is also left alone, and named, when its
text where a match would change does not parse (a tree-sitter error or missing node there), when
the rewritten text does not parse there, when it is not UTF-8, when it is a link or a directory on its path
from `--root` is one, when it lies outside `--root`, and when a directory on its path has become a link (on Windows a link or a
junction) since it was read; the root itself is held as it was when the plan started, so a root swapped for a
link afterwards is never written through. Only files under the given paths are read, as `tsquery` reads them (ignore
files obeyed, hidden files and links below a given path left out).

The index learns of a written file as of any edit: the next `symbol`, `search` or other lookup
sees its new size and time and reads it again, and Engine A's `grep` reads the file as it is.
`grep --index` answers from the text index as it was built, so `grep --index --update` sees the
new text. No MCP tool rewrites: the server only reads, except `reindex`.

### Custom languages

A `[[language]]` table adds a language without a new build. It is read, wherever the command
starts, from both the `.scrutate.toml` at the index root and your own configuration file
(`$SCRUTATE_CONFIG`, else `$XDG_CONFIG_HOME/scrutate/config.toml` or
`~/.config/scrutate/config.toml`):

```toml
[[language]]
name = "mylang"
extensions = ["myl"]
line_comment = "--"
block_comment = ["{-", "-}"]
definitions = [
  { kind = "function", pattern = '^\s*fn\s+([A-Za-z_][A-Za-z0-9_]*)' },
  { kind = "class", pattern = '^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)' },
]
```

- `name`: the language's id (letters, digits, `_` and `-`). Not a built-in language, a `--type`
  alias (`py`, `js`, `sh` ...), or a name another table already took.
- `extensions`: one or more, without the dot, matched without regard to case by the index
  (`grep --type` takes a file whose extension is written as listed, as for a built-in type). Refused: an
  extension a built-in language owns, as one of its extensions or the end of a file name it
  claims (`txt` for `CMakeLists.txt`, `xml` for `pom.xml`), or that built-in languages share and
  tell apart by what the file holds (`m`, `h`, `pp`, `cls`), so that the parser, the resolver and
  the language server agree on what a file is; one the index never reads (`csv`, `log`, `lock`
  ...); and one another custom language owns.
- `line_comment` (optional): the mark that starts a comment line.
- `block_comment` (optional): the opener and the closer of a block comment.
- `definitions`: one table per pattern, `kind` one of `class`, `constant`, `enum`, `function`,
  `impl`, `interface`, `macro`, `method`, `module`, `struct`, `trait`, `type`, and `pattern` a
  regular expression (Rust `regex` syntax) matched against one line at a time. All the patterns
  of every table share the size limits of one `grep` pattern (20 MB compiled, 10 MB of lazy DFA),
  split evenly, so a table of many large patterns is an error at the pattern that is over. The first group is the name, so a pattern needs one. The first pattern that matches a
  line wins; comment lines, and every line inside a block comment, never define.

Files with those extensions are parsed as the heuristic tier parses a file
([LANGUAGES](LANGUAGES.md)): a definition runs to the line before the next definition at the
same or a shallower indentation, or before the first later line of code shallower than it or
item at its indentation, whichever comes first, without its trailing blank and comment lines; the text kept for it is cut at
200 lines (40 for a `class`, `struct`, `enum`, `impl`, `interface`, `trait`, `type` or `module`),
and the comment right above it is its documentation, for the ranked search. Lines outside the
definitions are windows. `symbol`, `outline`, `search` and `find` answer from them,
`grep --type mylang` (and `--type-list`) knows the extensions,
and `languages` lists the language. There are no call edges or imports, so `callers` and `deps`
find nothing in these files.

The index keeps a signature of each custom language. When a table changes (a pattern, an
extension, a comment mark, the table removed), the next update reads again the files of the
extensions whose language changed, and no others. A bad table stops every command that reads
it, and every index update, with one line naming the file and the line, and exit 2: TOML that
does not parse, a key the table does not have (no key names a file, so a table cannot make
scrutate read one), a bad name or extension, a kind not in the list, a pattern that does not
compile or has no group. The repository's file is read as other repository files are: a regular
file inside the root, at most 1 MB, through a link only when the resolved file is inside the
root. That holds for all of it, the configuration as well as the languages: a root's
`.scrutate.toml` outside those bounds stops the command with one line naming it, as a file that
does not parse does.

### Environment variables
`SCRUTATE_ROOT`, `SCRUTATE_THREADS`, `SCRUTATE_GREP_{HIDDEN,NO_IGNORE,FOLLOW,SMART_CASE,MAX_FILESIZE,MAX_DEPTH,MAX_BYTES,GLOBS,TYPES}`,
`SCRUTATE_INDEX_EXCLUDE_DIRS`, `SCRUTATE_INDEX_MAX_FILES`, `SCRUTATE_INDEX_MAX_BYTES`, `SCRUTATE_REFRESH_INTERVAL`, `SCRUTATE_MAX_BYTES`,
`SCRUTATE_MCP_STRUCTURED`, `SCRUTATE_MCP_SHORT_TOOLS`, `SCRUTATE_CONFIG`, `SCRUTATE_WATCH_DEBUG`, `SCRUTATE_GREP_DEBUG`, `SCRUTATE_COUNT_KEY`,
`SCRUTATE_NO_SESSION`, `SCRUTATE_SESSION_DEBUG`, `SCRUTATE_SESSION_WAIT_MS` (see `serve` above), and:

| Variable | Meaning |
|---|---|
| `SCRUTATE_REDACT` | `off` returns secrets as the files hold them in the MCP tools, the Grep hook, `hook run` and `lsp`, which redact them by default ([serve](#serve-mcp)); `on` redacts every CLI answer that prints file text too ([serve](#serve-mcp)); an empty value keeps the default, and any other value (`0`, `OFF`) keeps it and says so on stderr |
| `SCRUTATE_HOOK_BASH` | `off` (or `0`, `false`, `no`, in any case): `hook bash` rewrites nothing, so every shell command runs as written ([setup](#setup-and-the-grep-hook)); any other value, empty, `f` and `n` too, leaves it on |
| `SCRUTATE_SEMANTIC` | path or `none`; the file is an `.npz` with int8 word tables and one scale per row, as `bench/train_semantic.py` writes it; another model makes the stored vectors anew on the next update, from the word counts the index keeps and without reading a source file; a model whose tables do not agree in size is refused with an error |
| `SCRUTATE_RANKER` | path or `none`; a ranker whose features differ from this version's, or whose trees do not fit their arrays, makes `search` fail |
| `SCRUTATE_PROFILE` | print indexing phase timings |
| `SCRUTATE_SMALL_TREE` | for testing: how many files a `grep` walk searches on its one walking thread before the parallel walk takes the rest (default 256 for paths that hold only files, 0 when there is a directory to enter) |
| `SCRUTATE_SPLIT_FILE_BYTES` | for testing: the size from which one file is searched in parallel pieces (default 64 MiB) |
| `SCRUTATE_QUICK_CHECK_MAX_BYTES` | the largest database, in bytes, that `PRAGMA quick_check` reads on open (default 64 MiB); a larger one is checked by `scrutate check` |
| `SCRUTATE_DIAGNOSTICS` | `json`: errors as JSON records |
| `SCRUTATE_CACHE_DIR` | where `--deps` keeps its copies of dependencies, in place of the user cache ([`--deps`](#--deps-a-dependency-at-its-locked-version)); `CARGO_HOME`, `GOMODCACHE` and `GOPATH` name the stores it reads |
| `NO_COLOR` | any value, empty too |
| `XDG_STATE_HOME` | where `files` keeps its frecency and the Grep hook its counter, on every system when it is an absolute path, see `files` above |

On/off variables but `SCRUTATE_REDACT` and `SCRUTATE_HOOK_BASH` follow the rule under
Conventions.

### The index directory
`.scrutate/index.db` (SQLite, WAL), `.scrutate/checked` (the last database state its integrity
checks passed, so an open of the same state skips them), `.scrutate/seg/*/` (memory-mapped arrays),
`.scrutate/text/` (the text index: a manifest and immutable trigram segments, which also hold the
hash of every line of a private key's body),
`.scrutate/tsquery/` (the `tsquery` cache), `.scrutate/answers/` (what `callers` and `callees`
resolved, one file per request), `.scrutate/session.sock` while `serve` runs (Unix), and a
`.gitignore` so it is never committed. On Unix only its owner can enter it (mode 0700, set by
every command that writes it, also on an index an earlier version made), since it holds the text
of files that may be private (docs/SECURITY.md). It is a cache: deleting it is always safe;
`scrutate index --force` rebuilds it, parsing every file; a damaged index rebuilds itself. The
index keeps what parsing made of each file's content (the artifact store), so a file renamed or
moved under the same name, or brought back by a branch switch, is not parsed again. Beside each
artifact it keeps the imports the resolver takes from the file's tree, so the resolver does not
parse a file it reads only for its imports and definitions. An index an earlier version made under
another schema is rebuilt from scratch, every table made again, by the next command that updates
it. In a linked
git work tree (`git worktree add`), a new index starts as a copy of the main checkout's, and
content new to the work tree is looked up in the main checkout's store; the main checkout's
database is only read, though SQLite creates its `index.db-wal` and `index.db-shm` there when
they are missing (docs/SECURITY.md).

If a newer scrutate built `.scrutate/`, this version reads the index without updating it (one
stderr line says so), or refuses it with exit 2 when it cannot read it;
`scrutate index --force` (`--text --force` for the text index) rebuilds it with this version. A
running `serve` or `lsp` answers each request with that refusal as an error and stays up. A
command that reads the index never waits for another process's writer: it answers from the last
committed index and says so in one stderr line. In `serve`, `lsp` and the Python module a read
waits up to 250 ms for the process's own update (a `serve --watch` update, perhaps waiting for
another process's lock); a burst's update takes 3 to 4 ms on Linux and about 150 ms on a loaded
Mac. Past that it answers from the last committed index and says which changed files the answer
does not hold yet: a line at the end of the MCP text and `pending` (`{"files": [...], "all":
false}`, or `"all": true` for a walk that could not run) in `structuredContent`, a
`window/logMessage` warning from `lsp`, a `UserWarning` in Python, a stderr line from a command
`serve` answers. The next read applies those files. The one exception is another process's first build, with nothing
committed to answer from: the command fails with "the index is being built by another process"
(exit 2), and a `serve` tool call waits up to 20 seconds, then answers that the index is still
being built. `scrutate index` (and `--force`) waits for the writer, at most 10 minutes: then it
fails with "another process has held the index write lock for 10 minutes" ("another process has
held the text index lock for 10 minutes" when the text index's writer is the one it waits for).
Ctrl-C ends the wait, as it does `watch`'s and that of `grep --index --update` (exit 130). A read-only index directory, or a read-only repository
with a complete index, is answered from as it is; one that holds no files (a rebuild cut short
after it cleared the index) and cannot be filled is an error (exit 2), never "(no results)". A `.scrutate` that is a symbolic link, or holds
one, is refused, so nothing is written or deleted outside the repository through it.
## Setup and the Grep hook

```
scrutate setup <claude|codex|opencode|cursor|gemini> [--project] [--uninstall] [--dry-run] [--hook] [--instructions]
scrutate hook grep
scrutate hook bash
scrutate hook run [--session ID] [--piped] -- <rg|grep|find> ARGS...
scrutate hook stats [--json]
```

`setup` adds one MCP server entry named `scrutate` to an agent's configuration. It runs this
binary by absolute path (an agent started from the dock has no shell `PATH`) with `serve`, so the
server serves the repository the agent starts it in. With `--project` the entry goes into the
agent's file at the top of the current repository (the directory that holds `.git`) and runs
`--root <that directory> serve`.

| Agent | User file | `--project` | Entry | `--instructions` file (user; `--project`) | `--hook` |
|---|---|---|---|---|---|
| `claude` | `~/.claude.json` (`$CLAUDE_CONFIG_DIR/.claude.json` when set), top-level `mcpServers`; through `claude mcp add-json scrutate JSON --scope user` when `claude` is on `PATH` | `.mcp.json` | `{"type": "stdio", "command": ..., "args": [...]}` | `~/.claude/CLAUDE.md` (`$CLAUDE_CONFIG_DIR/CLAUDE.md` when set); `CLAUDE.md` | the Grep hook and the Bash hook, below |
| `codex` | `$CODEX_HOME/config.toml` (default `~/.codex`) | none: an error | `[mcp_servers.scrutate]` with `command` and `args` | `$CODEX_HOME/AGENTS.md` | the Bash hook in `$CODEX_HOME/hooks.json`, below |
| `opencode` | `opencode.jsonc` or `opencode.json` in `$XDG_CONFIG_HOME/opencode` (default `~/.config/opencode`): the first that exists, else a new `opencode.json` | the same names at the top | `mcp.scrutate` = `{"type": "local", "command": [...], "enabled": true}`; in a file whose `mcp` already holds `servers`, `mcp.servers.scrutate` without `enabled` | `~/.config/opencode/AGENTS.md` (`$XDG_CONFIG_HOME/opencode/AGENTS.md` when it is set to an absolute path); `AGENTS.md` | a note: opencode's hooks are JavaScript plugins (`tool.execute.before`), not commands |
| `cursor` | `~/.cursor/mcp.json` | `.cursor/mcp.json` | `mcpServers.scrutate` = `{"command": ..., "args": [...]}` | none (Cursor keeps a user's rules in its settings); `AGENTS.md` | a note: cursor-agent has a `preToolUse` hook, but its search tool takes other arguments and answers in another form |
| `gemini` | `~/.gemini/settings.json` | `.gemini/settings.json` | `mcpServers.scrutate` = `{"command": ..., "args": [...]}` | `~/.gemini/GEMINI.md`; `GEMINI.md` | a note: Gemini CLI has a `BeforeTool` hook, but its search tool takes other arguments and answers in another form |

Every other key, server, comment and blank line stays byte for byte: JSON and JSONC files are
edited as text, one member in or out, never rewritten from a parse; `config.toml` gets one table
appended. A file that does not parse, or where a key on the way to the entry is not an object, is
named in an error (exit 2), and the run writes nothing. The new text goes to a temporary file in
the same directory, made with the file's permissions before a byte is written, and renamed over
the file; a symbolic link is followed
and stays. A file that changed between read and write is not written, and the error says to run
setup again. A missing file is created, with its directory, holding only the entry. A second run
changes nothing; a `scrutate` entry that runs something else is replaced.

`--uninstall` takes out everything `setup` adds for that agent and scope, leaving each file byte
for byte as before: the `scrutate` entry, the marked block, and the hooks (any `PreToolUse`
group with matcher `Grep` whose one command is a `scrutate` binary's `hook grep`, or with matcher
`Bash` and `hook bash`, wherever that binary is now). Text added after the marked block stays on its own line: the line break `setup`
put before the block goes with it when the block ends the file or the text before it already
ends with a line break, and stays otherwise. `--hook` and
`--instructions` are accepted with it and change nothing. It reads
the instruction file in the table with or without `--instructions`: when that file has a
`<!-- scrutate:begin -->` line with no `<!-- scrutate:end -->` line, even one `setup` never wrote,
the uninstall stops with exit code 2 and changes no file, the MCP entry and hook included, and
the error names the file. Remove that line and run it again. A file
`setup` created is deleted when nothing else is left in it. Claude Code's user file goes through
`claude mcp remove scrutate --scope user` when `claude` is on `PATH`; Claude Code writes that
whole file itself (its running sessions do so all the time) and keeps an empty `mcpServers` it
made. An entry that runs something else is replaced there by `remove`, then `add-json`; when the
add fails, the old entry is added back and the error says so. `--dry-run` writes and runs nothing: it prints each file it would change, with the lines it
would add (`+`) or take out (`-`), and the commands it would run.

Claude Code asks once before it starts a server of a project's `.mcp.json`. Gemini CLI starts a
stdio server only in a folder it trusts (`gemini trust`, or `~/.gemini/trustedFolders.json`);
`setup gemini` says so and leaves trust to you.

`--hook` (Claude Code and Codex; the table says why the others get a note and no hook) adds these
`PreToolUse` groups to `~/.claude/settings.json` (`$CLAUDE_CONFIG_DIR/settings.json`), or to
`.claude/settings.json` with `--project`:
`{"matcher": "Grep", "hooks": [{"type": "command", "command": "<binary> hook grep", "timeout": 10}]}`
and the same with `"matcher": "Bash"` and `hook bash`. For Codex it adds the Bash group to
`$CODEX_HOME/hooks.json`, which has the same shape; Codex runs a new hook only after the user
trusts it (`/hooks` in Codex), and `setup` says so. Claude Code checks its permission rules
against the command the hook returns, so a search the user allowed (`Bash(rg:*)`) asks again as
`<binary> hook run ...`; `setup` prints the rule that allows it, `Bash('<binary>' hook run:*)`.
On Windows Claude Code runs a hook command in Git Bash, else PowerShell, so the command is the
binary's path with `/` (`C:/Users/me/.cargo/bin/scrutate.exe hook grep`), which both run as
written; a path with a space or another character either shell reads goes as
`"command": "<binary>", "args": ["hook", "grep"]`, which Claude Code starts with no shell. The
hook runs only where Claude Code's Grep tool is enabled: never under a setup that turns Grep off
or a `--tools` list without it (`claude -p --tools Grep` runs it).

`--instructions` appends a short block telling the agent to use scrutate's tools, between
`<!-- scrutate:begin -->` and `<!-- scrutate:end -->`, after a line break (a blank line when the
file ends in one; a new or empty file holds the block alone), to the instruction
file in the table (the `--project` file at the top). A marker counts only as a line of its own
(ended by LF or CRLF), so a sentence that quotes one is the user's text; a begin line with no end line makes setup stop
and change nothing. Without the option `setup` reads and writes no instruction file;
`--uninstall` reads it (above).

`hook grep` is the hook program. It reads on standard input the JSON Claude Code sends before a
Grep call (`hook_event_name`, `tool_name`, `tool_input`, `cwd`, `session_id`) and answers with
Engine A's matches for the arguments Grep passes ripgrep: `--hidden`, `--glob '!.git'` and the
other version control directories, `--max-columns 500`, `-U --multiline-dotall` for `multiline`,
`-i`, `-A`, `-B`, `-C` or `context`, `type`, and `glob` split at whitespace and, in a word with no `{...}`, at commas outside
braces. The answer is worded as Grep words it:

| Case | Wording |
|---|---|
| lines | `path:N:text` for a match, `path-N-text` for a context line, `--` between groups when context was asked, no number for `-n: false`, no path when the path searched is a file |
| paths | from the session's directory; on Windows with `\`, the session's directory matched without case (as Node's `path.relative` does), and the file's real path with its case on disk |
| long lines | `[Omitted long matching line]` for a matching line, `[Omitted long context line]` for a context line, longer than 500 UTF-16 code units |
| `files_with_matches` | `Found N files` and the paths, newest first as Grep sorts them |
| `count` | the counts and `Found N total occurrences across M files.` |

Matches come in path order. Grep's paging applies (`head_limit`, 250 lines when not given and
none for 0, and `offset`, shown as Grep shows them: `[Showing results with pagination = ...]` after
the lines, `Found N files limit: L, offset: O` for files, `with pagination = ...` after a count's
`Found` line), then the MCP
tools' bound: at most 24,000 bytes (`SCRUTATE_MAX_BYTES`, held between 1,000 and 1,000,000),
whole lines, ending with `(N more lines not shown: call Grep again with offset=K, or narrow the request)`
(`files` in place of `lines` in the `files_with_matches` and `count` modes) when it cut. It prints
`{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "Grep result:\n..."}}`,
and Claude Code gives the model the reason in place of Grep's result. The header line
`Grep result:` is the whole cost of an uncut answer with nothing redacted: 13 bytes more than Grep's
own result; a redaction adds its closing line. In
Claude Code 2.1.323's own code, a hook reason from the user's settings files reaches the model
whole; another hook's is cut at 2,000 characters.

It fails open (prints nothing and exits 0, so Grep runs as if there were no hook) on:

- another tool or event, an argument it does not know, or `-o` with `output_mode: "content"`;
- a `glob` with a `/` other than a leading `**/` (ripgrep matches it from its own working
  directory), or with a `\` on Windows (a separator there);
- a `type` whose files differ between ripgrep and scrutate's registry (it takes `asm`, `cmake`,
  `cs`, `csharp`, `dart`, `elisp`, `elm`, `erlang`, `fish`, `gleam`, `go`, `julia`, `kotlin`,
  `lua`, `nix`, `py`, `python`, `racket`, `rst`, `rust`, `solidity`, `swift` and `yaml`);
- a path that starts with `~`, goes through a symbolic link, lies outside the project
  (`$CLAUDE_PROJECT_DIR`, else `cwd`), or on Windows is not on a drive letter (a `\\server\share`
  path, which Claude Code searches another way);
- a `deny` or `ask` rule for `Read` or `Grep` in a Claude Code settings file: the user's, the
  project's, the local or the managed one (on Windows
  `C:\Program Files\ClaudeCode\managed-settings.json`, with every `*.json` in the
  `managed-settings.d` directory beside it);
- managed policy it does not read: a configuration profile's `com.anthropic.claudecode.plist`
  under `/Library/Managed Preferences` on macOS, the registry key `SOFTWARE\Policies\ClaudeCode`
  under HKLM or HKCU on Windows (where one exists, Grep runs and applies it);
- an invalid pattern, no match, a non-UTF-8 file name, a binary file that matched, a file it
  could not read, or a search longer than 3 seconds.

A `Read` or `Grep` rule given only on Claude Code's command line (`--disallowedTools`,
`--settings`) or by an SDK host is in no file the hook reads, so the hook answers a Grep that rule
would have filtered: put such rules in a settings file, or leave the hook out
([SECURITY](SECURITY.md#known-gaps-and-residual-risks)).

It never exits with another code (exit code 2 would block the call) and reads no
`.scrutate.toml` or other scrutate configuration file.

`hook stats` prints the counter. Each answered call appends one line to
`$XDG_STATE_HOME/scrutate/hook-grep.jsonl` (default `~/.local/state/scrutate/`; on Windows
`%LOCALAPPDATA%\scrutate\`): `{"session", "grep_bytes", "sent_bytes", "time"}`, with `grep_bytes`
the size of the result Grep would have returned (every match Engine A found, worded and paged as
Grep does) and `sent_bytes` the size of the reason sent. The text form gives one line per
session, in the order the sessions began,
`session ID: N calls, Grep would have returned A bytes, scrutate sent B (C saved)`, then the same
for `all sessions`; `--json` prints
`{"counter": PATH, "sessions": [{"session", "calls", "grep_bytes", "sent_bytes", "saved_bytes", "cut_calls"}], "total": {...}}`.
The exit code is 1 when no call was counted. A small answer costs the header line more than Grep
would have sent (a negative saving); the saving comes from long answers the byte bound cuts.
The searches `hook run` answered are counted too (`"via": "bash"`, `grep_bytes` the bytes of the
program's output it read: the whole output, but for a search the byte bound cut only what was read
before the program was ended. Such a row has `"cut": true` and is counted apart, `cut_calls` in
the JSON and the line `of which cut at the byte bound: N`, and its saving is at least the figure
shown, never below 0) and also shown apart: the line `of which shell searches (hook run): ...` and
`"shell"` in the JSON.

`hook bash` is the hook of the shell tool: Claude Code's Bash and Codex's shell. It reads the
`PreToolUse` call on standard input. When the first command of the line is a read-only search by
`rg`, `grep` or `find`, it puts `<binary> hook run --session ID [--piped] -- ` in front of that
command and keeps the whole line after it byte for byte: the shell still expands its globs, `~`
and quotes as written, and runs whatever follows. The first command ends at the first unquoted
`|`, `|&`, `||`, `&&`, `;`, `&` or line end. It is left alone when it holds, outside quotes, `$`,
a backquote (these two also inside double quotes), `(`, `)`, `{`, `}`, `<`, a comment, an assignment before the program, a path to the program, or a
redirection other than `2>/dev/null`, `2>&1`, `>/dev/null` and `&>/dev/null` (and on Windows
`NUL` in place of `/dev/null`; elsewhere `NUL` is a file): any other target, of standard output or standard error, is a file the line
would write. Those four stay and apply to `hook run`, which passes standard error through.
Read-only means the words as written pass the same check `hook run` applies when it runs (below).
For Claude Code it prints
`{"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": {...}}}`, the whole tool
input with only `command` changed, and no permission decision: Claude Code checks each command of
the line against its own rules, so `hook run`'s rule never approves what follows it. For Codex (a
call with a `turn_id`) it prints `"permissionDecision": "allow"` and
`"updatedInput": {"command": ...}`, the form Codex takes; as that allows the whole line, it
rewrites for Codex only a search with nothing after it but one `| head` or `| tail` (plain words,
`-n N`, `-N`, `--lines=N`). It prints nothing when `RIPGREP_CONFIG_PATH` is set (`rg` would read
that file) or `SCRUTATE_HOOK_BASH=off`, and always exits 0. On Windows it leaves `find` alone:
the first `find` on `PATH` there may be System32's, another program.

`hook run` is the boundary: an agent can type it, and a user may allow every command that starts
with it. It runs only a bare `rg`, `grep` or `find` (not `./rg`, `/tmp/x/rg`, `env`, `sh` or
anything else; on Windows not `find`, as the first one on `PATH` may be System32's) whose arguments, as the shell expanded them, are all on its allow-list:
the `rg` and `grep` flags that only choose what is read and how it is printed (`-n`, `-i`, `-l`,
`-c`, `-w`, `-F`, `-e`, `-g`, `-t`, `-A`/`-B`/`-C`, `--include`, `--hidden` and their kin), read as
the program reads them, so a flag's value is skipped whatever it looks like (`-g --` gives `-g`
the value `--`), and in `find` only its read-only expressions (`-name`, `-type`, `-size`,
`-mtime`, `-maxdepth`, `-print`, `-printf` and their kin; never `-exec`, `-execdir`, `-ok`,
`-okdir`, `-delete`, `-fprint`, `-fprint0`, `-fprintf` or `-fls`). A flag the list lacks, a rare
one or a future release's, means no rewrite. A word naming `rg --pre`, `--pre-glob`, `-z`
(`--search-zip`, whose decompressors are programs), `--hostname-bin`, or `scrutate grep`'s own
`--index` and `--update` (they write the text index) is refused wherever it sits, as a value or
after `--` too. Anything else exits 2 and runs nothing. A program is looked up
in the directories of its `PATH` given as absolute paths and outside the current directory and its
git work tree, never the current directory, a relative entry or a repository's own `bin`, and
scrutate's own directory only when `PATH` names it (Windows would search it otherwise), and runs with `RIPGREP_CONFIG_PATH`, `LD_PRELOAD`, `LD_LIBRARY_PATH`,
`LD_AUDIT` and every `DYLD_*` variable removed. An `rg` whose arguments `scrutate grep` takes (they
parse, and every path they name is there) runs this binary's own `grep` instead, with every
`SCRUTATE_*` variable removed and `SCRUTATE_CONFIG` set to the null device, so the output is the
one `rg` gives. Scrutate's own index directory is never shown (its files hold the repository's text, see
[Security](SECURITY.md)), decided before the search runs, by file identity, never by reading its output: the files of
the `.scrutate` directories of the current directory, of those above it and of every one inside
the searched paths are known by device and inode (on Windows by the file handle's volume and
index). Before it runs a `grep` or `rg`, `hook run`
walks what the search could read (each path it names, or the current directory for `rg` and a
recursive `grep`): hidden and ignored entries taken, a link it names followed, a link met inside
followed only where the search follows it (`grep -R`, `rg -L`), each directory entered once, and a
directory named `.scrutate` not entered, as the search is told not to. If a named path lies inside
an index directory, or the walk reaches an index file or directory by identity (a link into one,
a hard link under another name, a nested repository's), the search exits 2 with a line naming
the path and why (for a hard link, the index file it is and that removing the hard link, by its
path, lets the search run), and nothing runs. A walk past 1,000,000 entries or 5 seconds stops, and the
search exits 2 with a line asking for narrower paths. A `grep` also gets `--exclude-dir=.scrutate` first and an `rg` `--glob=!.scrutate` and
`--iglob=!.scrutate` after its own flags (rg weighs every `--iglob` after every `--glob`), so the
search skips the directory the walk skipped. Colour is always off: the search's own `--color`,
`--colour` and `--colors` are dropped, and `--color=never` is added when `rg` or `grep` itself runs
(an `rg` that `scrutate grep` answers writes to a pipe, which gets no colour), since escape codes
around a match would cut a secret in two. An `rg` with `--replace` (`-r`) or `--json` exits 2 and runs
nothing: both rewrite each line before scrutate can find a secret in it (`scrutate grep --json`
gives JSON with secrets redacted). An `rg` with `--stats` exits 2 and runs nothing too: rg prints
its statistics on standard output, and `scrutate grep`, which answers an `rg`, prints its own
summary on stderr (`scrutate grep --stats`). A search for the
text `.scrutate`, the pattern and not a path, runs. A line
with `\r` or another control character but tab and line feed is left alone.

It answers the search as the program would. `rg` goes to `scrutate grep -N` with the same
arguments (line numbers only when asked, as `rg` writing to a pipe), with no scrutate
configuration file or `SCRUTATE_*` setting in play, when `scrutate grep` takes those arguments
and every path they name is there; otherwise `rg` itself runs, so its own message and code come
back. `rg --sort` takes a value where `scrutate grep --sort` is a flag: `--sort path` becomes
`--sort`, `--sort none` is dropped (the last one wins), and any other order (`--sortr`,
`--sort modified`) runs `rg`.
`grep` and `find` run as given: GNU and BSD grep word binary files, their own regular expressions
and `-c` in ways a translation could not match exactly. Secrets are redacted unless
`SCRUTATE_REDACT=off` (see below), with the closing line that counts them, also when a pipe takes
the output (`--piped`): a `cut` after it sees `[redacted:KIND]`. The output streams through line by line
(a line longer than 1 MiB in pieces, each redacted and cut where no secret crosses the cut), so
memory stays bounded. With no pipe after
it, it stops at the MCP tools' bound (24,000 bytes, `SCRUTATE_MAX_BYTES`) with whole lines, ends
the program and adds
`[scrutate] output cut here: narrow the search, or pass a path`, exiting 0 as the search had
matches; the closing line counts the redactions in the lines sent; a piped search is never cut, so `wc`, `sort` or `cut` after it compute
what they would have. Standard error and the exit code are otherwise the program's. It has no
timer of its own: the agent's command timeout governs, as it would without the hook, and when
that ends `hook run`, by any signal, the program ends with it (on Unix a watcher the program's
process forks before it starts, on Windows a job closed with `hook run`).
