# The twelve known limits of 0.1.0, checked against their sources (November 2026)

Research date: 2026-11-11. Every claim about another tool links to the source that owns it: its code, its documentation or the specification. Project references are `path:line` at the working tree of 2026-11-11 (branch `release/0.1.0`). The limits are the ones in `docs/ROADMAP.md` ("Known limits that stay in 0.1.0") and `docs/RELEASE_READINESS.md` ("Remaining defects and incomplete requirements").

Status on 2026-11-17: the project side of this note is the tree of its research date, and docs/ROADMAP.md now lists the limits that remain. Since then the code fixes limit 1 (calls in Rust macro arguments are read from the token trees; a plain name in a non-standard macro's arguments stays undecided), limit 2 (every `serve` session reads freshness from file system events), limit 3 (files on Linux kernel pseudo file systems are read to the end), limit 4 (the stat key holds the change time), limit 6 (chunk text is capped in bytes as well as lines), limit 7 (JavaScript `var` binders count for the whole function), limit 8 (`grep` follows ripgrep's rule for file names and takes `-H` and `-I`), limit 9 (an emptied `chunks` table is caught against the recorded counts) and limit 10 (C++ members declared in a class body are definitions), and limit 5 in part (a call on a call's result resolves through the callee's declared return type and, in Rust, a table of standard methods). Windows and Intel macOS run CI on GitHub's runners (limit 11, in part); limit 12 stands. `python/scrutate_ref` keeps only the training code, so the Python reference files named below are gone.

Run locally for this note, and nothing else: `rg` 15.2.0 and the scrutate release binary on two small files; GNU grep 3.8 and a procfs read test in a `debian:bookworm-slim` container (Colima, kernel 6.8.0); tree-sitter-rust 0.24.2 and tree-sitter-javascript 0.25.0 through the Python `tree_sitter` 0.26.1 package; tree-sitter-cpp 0.23.4 (the version in `Cargo.lock`), compiled from the cargo registry's `parser.c` and `scanner.c` into a scratch library; `man git-config` (git 2.54.0) and `man make` (GNU Make 3.81); a check for Rosetta. No benchmark, no cargo build, nothing installed, no repository file changed except this note.

Verdict words: **SOLVABLE** (with how and the cost), **BY DESIGN** (the reference tool makes the same choice), **EXTERNAL** (needs hardware, a licence or the owner's decision).

## What this means for scrutate

Ordered by how much a user would be hit.

1. **Limit 3, short reads of /proc and /sys: SOLVABLE, and it loses data today.** In the container, the first `read` of 65,536 bytes from `/proc/kallsyms` returned 4,072 bytes of a 7,307,759-byte file, and `stat` gave its size as 0. `split::first` (`crates/scrutate/src/instant.rs:1839`) takes any first read shorter than 64 KB as the whole file, so scrutate searches the first 4 KB and reports no error. The kernel's `seq_read_iter` stops at its own buffer, PAGE_SIZE to start with ([fs/seq_file.c](https://github.com/torvalds/linux/blob/master/fs/seq_file.c)). GNU grep and ripgrep both read until `read` returns 0. Fix: on Linux, keep the one-read fast path only on real filesystems: one `fstatfs` per directory and a check of `f_type` against `PROC_SUPER_MAGIC` 0x9fa0, `SYSFS_MAGIC` 0x62656572 and the other pseudo filesystems in [magic.h](https://github.com/torvalds/linux/blob/master/include/uapi/linux/magic.h). Read to 0 there. The cost is one call per directory, not per file.
2. **Limit 2, MCP answers inside the refresh window: SOLVABLE with code that is already there.** `serve --watch` exists (`crates/scrutate/src/cli.rs:733-760`, `watch.rs`), but it is off by default and on macOS it is a 1 to 30 s timer (`watch.rs:73-97`). Two steps fix it. First, turn `--watch` on by default where `notify` has a native backend (inotify, ReadDirectoryChangesW). Second, before answering, `stat` the files the answer cites. For `read_code symbol=`, compare the stored chunk text with the lines read from disk and re-index that one file on a mismatch. Checking cited files is sound for what the answer shows. It cannot find a new definition in a file the answer does not cite, so it does not replace the watcher. LSP has the same split: the client pushes `didChange`, and the server may watch the rest of the disk itself ([LSP 3.17](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/)).
3. **Limit 6, 1.2 GB for one line of nested functions: SOLVABLE in a few lines.** `head_bytes` (`crates/scrutate/src/parse.rs:282`) caps chunk text by newlines only. On one line nothing is cut, so every nested chunk holds the rest of the line, and memory grows with the square of the nesting. Add a byte cap next to the line cap. LlamaIndex's `CodeSplitter` uses `max_chars` 1,500 and splits a leaf that is still too big ([code.py](https://github.com/run-llama/llama_index/blob/main/llama-index-core/llama_index/core/node_parser/text/code.py)). The Python reference needs the same change.
4. **Limit 9, an emptied `chunks` table: SOLVABLE at no cost.** SQLite cannot see it. A `DELETE` leaves a well-formed database, and `integrity_check` tests structure only ([pragma](https://www.sqlite.org/pragma.html)). The index already has the numbers. `crates/scrutate/src/index.rs:771-773` counts the segment rows and the live chunks. When the segments hold rows, `chunks` holds none and `files` lists indexed files, raise `Damaged` and let the existing rebuild run.
5. **Limit 4, `chmod 000` with the same size and mtime: SOLVABLE, as git does it.** POSIX says `chmod` updates the change time ([chmod](https://pubs.opengroup.org/onlinepubs/9799919799/functions/chmod.html)). Git compares `st_ctime` with mtime, size, inode, uid, gid and mode bits ([racy-git](https://git-scm.com/docs/racy-git)). Add `ctime_ns` to the stat key (`crates/scrutate/src/index.rs:41`, `:880`). Rust's `metadata` already returns it, so it costs no extra system call. A false positive (a backup tool, `chown`, `setfacl`, a rename on most filesystems) costs one read and one SHA-1, then `Processed::Same` (`index.rs:348`), with no reparse. The schema version goes up, so each index is rebuilt once.
6. **Limit 10, C++ members declared in a class body: SOLVABLE.** In tree-sitter-cpp 0.23.4, a constructor or destructor declared in the body is a `declaration` with a `function_declarator`. A member template is a `template_declaration` around a `declaration`, and `operator bool() const;` is a `declaration` around an `operator_cast`. `CPP_DEFS` (`crates/scrutate/src/langs.rs:232-243`) has no `declaration` rule, and `cpp_member_decl` (`:1814`) matches only `field_declaration`. Add a `declaration` rule limited to a `field_declaration_list` parent.
7. **Limit 7, JavaScript `var` in a block: SOLVABLE.** The specification scopes `var` to the function, not the block (ECMAScript 2027, 14.3.2). `shadowed` (`crates/scrutate/src/resolve.rs:1367`) skips nested `statement_block`s for every binder. tree-sitter-javascript gives `var` its own node kind (`variable_declaration`, against `lexical_declaration` for `let` and `const`), so the fix is to keep searching nested blocks for `variable_declaration` binders.
8. **Limit 1, calls inside Rust macro arguments: SOLVABLE, and the grammar itself shows how.** tree-sitter-rust's own `queries/injections.scm` re-parses every `macro_invocation` token tree as Rust. Doing the same and running the normal call rules on the result finds `f` in `println!("{}", f(x))` and `k` in `vec![k(1)]`. It also finds `g` in `json!({"a": g(y)})`, with an `ERROR` node around it. rust-analyzer gets these calls by expanding the macro. ctags records no calls at all.
9. **Limit 8, file name for a single file: SOLVABLE, a choice to make.** ripgrep prints the name when `vimgrep` is set or when the search is not exactly one path that is not a directory ([hiargs.rs](https://github.com/BurntSushi/ripgrep/blob/master/crates/core/flags/hiargs.rs)). GNU grep 3.8 gives the same output on the same commands. `scrutate grep` prints `a.txt:1:hello` and refuses `-H` and `-I`. Copy ripgrep's rule and its flags. GNU's `-h` is taken by `--help`, so only ripgrep's `-I` fits.
10. **Limit 5, `x.ok()` on an unknown type: SOLVABLE for the common case, BY DESIGN for the rest.** Every precise indexer has a type checker behind it: scip-python is a fork of Pyright, scip-typescript runs the TypeScript compiler, the Rust SCIP index comes from rust-analyzer, and Kythe instruments compilers. Name-only tools (tree-sitter tags, Sourcegraph's search-based navigation, GitHub's search-based navigation) match by name, as scrutate's fallback does. The case the ROADMAP names can be fixed cheaply. `Result` has 38 method names on Rust 1.99.1, `ok` among them. When the receiver's type is unknown and the method is one of these, call the occurrence "external" instead of a namesake. Next by cost: declared return types and field types.
11. **Limit 11, Windows and Intel macOS never run: EXTERNAL, but most of it is free.** Rosetta 2 is not installed on this Mac (`arch -x86_64 /usr/bin/true` fails with "Bad CPU type in executable"). `softwareupdate --install-rosetta` lets every `x86_64-apple-darwin` test run here, and Apple supports it fully only up to macOS 27, which is what this Mac runs ([Apple 102527](https://support.apple.com/en-us/102527)). For Windows the strongest local test is Windows 11 Arm in VMware Fusion (free) or UTM, which runs the x64 `.exe` through Prism. It needs a Windows 11 Pro licence. Wine is a weaker smoke test, and GitHub runners on a public repository cost nothing.
12. **Limit 12, no x86_64 Linux or dedicated hardware: EXTERNAL, about €36 for all three platforms.** A 6-hour session costs €0.57 on a Hetzner AX41 (Ryzen 5 3600, hourly, no setup fee), €5.28 on a Scaleway Mac mini M4 (24-hour minimum) and €28.47 for Windows Server on the same AX41 (the licence is billed per calendar month). GitHub's hosted runners are VMs, and no runner size promises dedicated hardware.

## 1. Calls inside Rust macro arguments

Code: `crates/scrutate/src/langs.rs:388` gives Rust two call rules, `call_expression` and `macro_invocation`. The macro's name is recorded and its token tree is never read.

What the parser gives (tree-sitter-rust 0.24.2, parsed here): `println!("{}", f(x))` is a `macro_invocation` holding one `token_tree` with `string_literal`, `identifier "f"`, and a nested `token_tree` holding `identifier "x"`. There is no `call_expression` and no `field_expression`. A method call inside the arguments, `s.h(z)`, is `identifier "s"`, an anonymous `.`, `identifier "h"` and a `token_tree`.

| Source claim | Verdict for scrutate |
| --- | --- |
| tree-sitter-rust's `queries/injections.scm` injects `rust` into the `token_tree` of every `macro_invocation` and `macro_rule`, with `include-children` (crate source in the cargo registry; [repo](https://github.com/tree-sitter/tree-sitter-rust/blob/master/queries/injections.scm)) | The grammar's authors re-parse macro arguments as Rust. Tried here: wrapping the token text in `fn _m() { ...; }` and parsing again gives a `call_expression` for `f`, `k` (`vec![k(1); n]`), `Some` and `Ok`. For `g` in `json!({"a": g(y), "b": s.h(z)})` the result comes with `ERROR` nodes. |
| tree-sitter-rust's own `tags.scm` lists `call_expression` and `macro_invocation` as `@reference.call`, and nothing inside token trees | The stock tags query has the same gap as scrutate. |
| rust-analyzer: "Name resolution, macro expansion and type inference all happen here" (`hir-expand`, `hir-def`), and `mbe` handles "the actual parsing and expansion of declarative macro" ([architecture](https://rust-analyzer.github.io/book/contributing/architecture.html)). `descend_into_macros_exact` "Descends the token into expansions" ([semantics.rs](https://github.com/rust-lang/rust-analyzer/blob/master/crates/hir/src/semantics.rs)) | The precise answer comes from expanding the macro, and that needs the macro's definition and a proc-macro server. Its SCIP export is listed as the Rust SCIP indexer ([scip](https://github.com/sourcegraph/scip)). |
| universal-ctags Rust parser: kinds are definitions only (module, struct, trait, impl, fn, enum, type, static, macro, field, variant, method, const), and a `macro` field marks "macro invocation containing the definition" ([rust.c](https://github.com/universal-ctags/ctags/blob/master/parsers/rust.c)) | ctags looks into macro invocations for definitions only and records no calls. |
| GitHub stack graphs ship rules for Java, JavaScript, Python and TypeScript, and the repository is archived ("no longer supported or updated by GitHub") ([repo](https://github.com/github/stack-graphs), GitHub API: `archived: true`) | No stack-graph rules exist for Rust. |

What a plain token-tree heuristic (an `identifier` followed by a parenthesised `token_tree`) gets wrong, from the parses above:

- a method call needs a look back for `.`, or it is taken for a free function;
- patterns such as `matches!(x, Foo(_))` and `macro_rules!` bodies (`$f($x)`) look like calls;
- DSL macros (`html!`, `sql!`, `quote!`) are not Rust, and their "calls" are text;
- tuple struct and enum constructors (`Some(v)`) look like calls, but `call_expression` already includes them, so that adds no new false positive.

**Verdict: SOLVABLE.** In `parse.rs` call extraction, re-parse each `macro_invocation` token tree as Rust (as the injection query does) for macros whose arguments are expressions. Start with the std formatting, assertion and `vec!` family, plus any macro not defined by `macro_rules!` in the file. Run the existing call rules on the result. The cost is one extra parse of the argument text, which is usually under 100 bytes. Calls found this way should be marked as from a macro, so `callers` can rank them after the parsed ones.

## 2. MCP answers inside the refresh interval

Code: `crates/scrutate/src/mcp.rs:124-133` skips the scan when the last one was less than `refresh_interval` ago (default 1.0 s, `mcp.rs:346`). `Repo::refresh` (`crates/scrutate/src/lib.rs:107`) runs the stat-based update. `serve --watch` starts `watch::run` (`cli.rs:740-760`): `notify` events on Linux and Windows, and on macOS a timer whose pause is 10 times the last scan's cost, between 1 and 30 s (`watch.rs:73-97`).

| Source claim | Verdict for scrutate |
| --- | --- |
| inotify: "monitoring of directories is not recursive"; a new subdirectory may already hold files "by the time you create a watch"; "the event queue can overflow. In this case, events are lost"; network filesystems and /proc, /sys are not reported ([inotify(7)](https://man7.org/linux/man-pages/man7/inotify.7.html)) | A watcher needs a full rescan on overflow. `watch.rs` already treats an error event as "rescan" (`wants_event`). |
| FSEvents `kFSEventStreamEventFlagMustScanSubDirs`: "Your application must rescan not just the directory given in the event, but all its children, recursively", when "events were coalesced hierarchically" ([Apple](https://developer.apple.com/documentation/coreservices/kfseventstreameventflagmustscansubdirs)) | FSEvents is also lossy by design. scrutate does not link it on macOS (`Cargo.toml:139-141`). |
| watchman cookies: "Every file operation that happens before the query is started will be observed"; on macOS, relying on FSEvents for this "is unsupported on any current Apple platform", with `settle_period` as the workaround ([cookies](https://meta.github.io/watchman/docs/cookies)) | Even watchman promises a fresh answer only on Linux. |
| LSP: after `didOpen` "the server must not try to read the document's content"; `didChange` comes from the client; the client watches files and "Servers are allowed to run their own file system watching mechanism" ([LSP 3.17](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/)) | Language servers stay fresh because the editor pushes every edit. An MCP client pushes nothing. |
| Zoekt indexes git repositories; "The index server can be configured to periodically fetch and reindex repositories" ([README](https://github.com/sourcegraph/zoekt)) | Zoekt accepts a window much longer than 1 s. |
| ripgrep reads the files on every search (no index) | A grep-based agent is always fresh and pays a full read each time. `grep_code` already works this way. |
| Git compares stat data and re-checks content when the mtime is not older than the index file ("racy git") ([racy-git](https://git-scm.com/docs/racy-git)) | A stat check is only as good as the timestamp's resolution. APFS and ext4 keep nanoseconds. FAT keeps 2 s. |

Is validating only the cited files sound? Yes for what the answer shows: if every file an answer cites is stat-checked first and re-indexed when it changed, no line in the answer is stale. A deleted file shows up as a failed `stat`. It is not complete: a definition or caller added to a file the answer does not cite stays invisible until the next scan or watcher event. For `read_code symbol=` there is an exact check that costs nothing extra: the file is read anyway to return its lines, so compare them with the chunk's stored text and re-index that file on a mismatch.

**Verdict: SOLVABLE.** (1) Make `--watch` the default for `serve` on Linux and Windows. It already exists and rescans on overflow. (2) Stat the cited files before every answer: a few microseconds per path, and answers cite tens of paths. (3) Use the exact check for `read_code symbol=`. (4) On macOS, keep the timer or add an opt-in FSEvents backend, and document that one window remains there.

## 3. Short reads of /proc and /sys

Code: `crates/scrutate/src/instant.rs:1640-1668` calls `split::first` for files whose size is unknown or at most 64 KB. `split::first` (`instant.rs:1839-1859`) makes one `read_at` of 64 KB + 1 and returns `First::Whole(n)` for any `n` up to 64 KB. The comment at `:1641-1643` explains why: no `statx` per file (96,000 on the Linux tree).

Measured here (container on kernel 6.8.0): `/proc/self/maps`, `/proc/cpuinfo` and `/proc/self/status` report size 0, a sysfs attribute reports 4096 and holds 23 bytes, and `/proc/kallsyms` reports 0 and holds 7,307,759 bytes. One `sysread` of 65,536 bytes from it returned 4,072.

| Source claim | Verdict for scrutate |
| --- | --- |
| POSIX `read`: fewer bytes than asked "if the number of bytes left in the file is less than nbyte", if interrupted by a signal, or for "a pipe or FIFO or special file" ([read](https://pubs.opengroup.org/onlinepubs/9799919799/functions/read.html)) | The assumption holds for regular files on real filesystems. procfs files claim to be regular files and do not keep to it. |
| `seq_read_iter` allocates `m->buf = seq_buf_alloc(m->size = PAGE_SIZE)` and stops filling when the next record would not fit or `seq_has_overflowed(m)`. The rest comes on the next `read` ([fs/seq_file.c](https://github.com/torvalds/linux/blob/master/fs/seq_file.c)) | A short read from a seq_file means nothing about the end of the file. This is the 4,072 above. |
| sysfs: "The buffer will always be PAGE_SIZE bytes in length", "sysfs will call the method exactly once for each read", and show() "should fill the entire buffer" ([sysfs](https://docs.kernel.org/filesystems/sysfs.html)) | A sysfs text attribute fits one read. Binary attributes and debugfs do not follow this rule. |
| GNU grep `fillbuf` uses `st_size` only to size the buffer (`usable_st_size` is `S_ISREG` or shared memory). The loop in `grep()` ends when a read returns no new bytes ([grep.c](https://cgit.git.savannah.gnu.org/cgit/grep.git/plain/src/grep.c)) | GNU grep pays one extra `read` per file, always. |
| ripgrep `LineBuffer::fill`: `if readlen == 0 {` is the only end of input ([line_buffer.rs](https://github.com/BurntSushi/ripgrep/blob/master/crates/searcher/src/line_buffer.rs)). Its mmap path does not check the file's length ([mmap.rs](https://github.com/BurntSushi/ripgrep/blob/master/crates/searcher/src/searcher/mmap.rs)) | ripgrep also reads to 0. Whether its mmap path, used for a few explicit files on Linux, sees a size-0 procfs file as empty was not checked. |
| Pseudo filesystem magic numbers: `PROC_SUPER_MAGIC 0x9fa0`, `SYSFS_MAGIC 0x62656572`, `DEBUGFS_MAGIC 0x64626720`, `TRACEFS_MAGIC 0x74726163`, `CGROUP2_SUPER_MAGIC 0x63677270`, `SECURITYFS_MAGIC 0x73636673`, `BPF_FS_MAGIC 0xcafe4a11`, `CONFIGFS_MAGIC 0x62656570` ([magic.h](https://github.com/torvalds/linux/blob/master/include/uapi/linux/magic.h)) | These can be checked with one `fstatfs` on the directory. |

**Verdict: SOLVABLE.** There are three ways, cheapest first:

1. On Linux, call `fstatfs` once per directory the walker opens, and for the magic numbers above skip the fast path and read until 0. A tree has far fewer directories than files.
2. Read until 0 only when a first read is short and the path is under `/proc`, `/sys` or `/sys/kernel/debug`. A string check costs nothing, but a bind mount defeats it.
3. Do what grep and ripgrep do: one more `read_at` after every short first read. That is one system call per small file, about 96,000 on the Linux tree, on the hot path the benchmarks measure.

Option 1 is correct for bind mounts and keeps the fast path everywhere else. The same check belongs in `split::rest` (`instant.rs:1817`), which trusts the size it was given.

## 4. A file made unreadable without a size or mtime change

Code: `files(path, size, mtime_ns, hash)` (`crates/scrutate/src/index.rs:41`). The update skips a file when `size` and `mtime_ns` match (`index.rs:878-882`) and never opens it, so a file that `chmod 000` made unreadable keeps its chunks. `process` already drops a file it cannot read (`index.rs:340-342`). The Python reference has the same key (`python/scrutate_ref/index.py:49`, `:176-191`).

| Source claim | Verdict for scrutate |
| --- | --- |
| POSIX `chmod`: "Upon successful completion, chmod() shall mark for update the last file status change timestamp of the file" (Issue 8, IEEE Std 1003.1-2024) ([chmod](https://pubs.opengroup.org/onlinepubs/9799919799/functions/chmod.html)) | ctime catches `chmod`. It also catches `chown` and ACL changes, which a mode-bit check would miss. |
| Git compares "the file type ... and executable bits ... from `st_mode` member, `st_mtime` and `st_ctime` timestamps, `st_uid`, `st_gid`, `st_ino`, and `st_size`". Nanoseconds only with `USE_NSEC`, because on Linux "in-core timestamps can have finer granularity than on-disk timestamps" ([racy-git](https://git-scm.com/docs/racy-git)) | The reference tool for this problem includes ctime. |
| `core.trustctime`: "If false, the ctime differences ... are ignored; useful when the inode change time is regularly modified by something outside Git (file system crawlers and some backup systems). ... True by default." `core.checkStat=minimal` keeps only whole-second mtime, ctime and size (`man git-config`, git 2.54.0) | Git's own docs name the false positives: crawlers and backup tools. Git keeps ctime on by default anyway. |
| Cargo: `-Z checksum-freshness` "will replace the use of file mtimes in cargo's fingerprints with a file checksum value", and it is still unstable ([unstable](https://doc.rust-lang.org/cargo/reference/unstable.html)) | Cargo uses mtime only. It needs to rebuild on content changes, not to notice lost permissions. |
| GNU make: "uses the makefile data base and the last-modification times of the files to decide which of the files need to be updated" (`man make`, GNU Make 3.81) | mtime only, for the same reason as cargo. |
| Zoekt indexes git commits through `zoekt-git-index`, or a directory through `zoekt-index` ([README](https://github.com/sourcegraph/zoekt)) | No stat key to compare with. |

The cost of adding ctime. On Linux and macOS, `std::fs::metadata` already fills ctime (`MetadataExt::ctime`, `ctime_nsec`), so the key is free to read. A false positive leads to one read and a SHA-1 of a file of at most 1 MB (`MAX_FILE_BYTES`). Then `Processed::Same` updates the row without a reparse (`index.rs:921-923`). ctime changes on `chmod`, `chown`, `setfacl`, `setxattr` and link-count changes, and on a rename on most Linux filesystems. A whole-tree `chown` or a backup tool that sets xattrs costs one re-hash of the tree, once. Mode bits alone are cheaper still but miss an ownership change and ACLs.

**Verdict: SOLVABLE.** Add `ctime_ns` to `files` and to the comparison at `index.rs:880` (Unix), and bump the schema version, which rebuilds each index once. Do the same in the Python reference (`st.st_ctime_ns`). On Windows, `std` has no stable change time (see Not verified), so keep size and mtime there.

## 5. A method call on a value of unknown type

Code: `resolve_qualified` (`crates/scrutate/src/resolve.rs:734-792`) returns `Err("receiver")` when it cannot type `x` in `x.N`. It already uses the declared type and the constructor through `variable_type` (`:799`). `callers` (`crates/scrutate/src/ops.rs:478-500`) keeps such call sites as "undecided" and lists them after the decided ones.

| Source claim | Verdict for scrutate |
| --- | --- |
| tree-sitter tags: `@definition.*` and `@reference.call` captures, "identifying the entities that can be named in a program" ([code navigation](https://tree-sitter.github.io/tree-sitter/4-code-navigation.html)). The tags crate reads `@local.scope` and `@local.definition` and drops a reference marked `(#is-not? local)` when a local definition of that name is in scope. It never links a reference to a definition ([tags.rs](https://github.com/tree-sitter/tree-sitter/blob/master/crates/tags/src/tags.rs)) | Tags give names, and locals only remove local variables. That is less than scrutate already does. |
| GitHub code navigation "searches all definitions and references across a repository to find entities with a given name". Rust is in its language list ([GitHub docs](https://docs.github.com/en/repositories/working-with-files/using-files/navigating-code-on-github)) | GitHub matches Rust methods by name, as scrutate's fallback does. |
| Sourcegraph search-based navigation: "Find references: Performs a case-sensitive word-boundary cross-repository plain text search" and "filters results by file extension and by imports" ([docs](https://sourcegraph.com/docs/code-search/code-navigation/search_based_code_navigation)) | Same family, with less filtering than scrutate's same-language, same-package and owner-name ranking. |
| Stack graphs: rules for Java, JavaScript, Python and TypeScript only, and the repository is archived. The introduction post leaves dataflow and generics as open questions ([post](https://github.blog/open-source/introducing-stack-graphs/)) | No published syntax-only system resolves `x.ok()` without types. |
| scip-python is a "Sourcegraph fork of pyright"; scip-typescript indexes from a `tsconfig.json`; the Rust SCIP indexer is rust-analyzer ([scip-python](https://github.com/sourcegraph/scip-python), [scip-typescript](https://github.com/sourcegraph/scip-typescript), [scip](https://github.com/sourcegraph/scip)). Kythe starts from "A compiler that can be instrumented" ([Kythe overview](https://kythe.io/docs/kythe-overview.html)) | Precise method resolution comes from a type checker in every case. |
| `Result` on Rust 1.99.1 has 38 method names: `and, and_then, as_deref, as_deref_mut, as_mut, as_ref, cloned, copied, err, expect, expect_err, flatten, inspect, inspect_err, into_err, into_ok, is_err, is_err_and, is_ok, is_ok_and, iter, iter_mut, map, map_err, map_or, map_or_default, map_or_else, ok, or, or_else, transpose, unwrap, unwrap_err, unwrap_err_unchecked, unwrap_or, unwrap_or_default, unwrap_or_else, unwrap_unchecked` ([std](https://doc.rust-lang.org/std/result/enum.Result.html)) | A fixed list is enough for the case the ROADMAP names. |

No source gives a precision figure for syntax-only method resolution. None was found, and none is claimed here.

Inference steps by cost, all in `resolve.rs`:

1. **Well-known std names (cheapest).** With a Rust receiver of unknown type and a method name in the `Result`, `Option` or `Iterator` list, return "external" instead of "receiver". Keep a repository definition only when the file names its owner type. This is a static list and one lookup.
2. **Declared return types.** For `let x = f(..)`, resolve `f` (bare resolution already works) and read its `-> T`. Use `T` when it names a repository type, and treat `Result<..>` and `Option<..>` as std. This costs one more chunk lookup per typed variable.
3. **Field types.** For `self.field.m()`, read the field's declared type from the struct or class chunk. Python and TypeScript annotate fields, Go and Rust always declare them.
4. **Constructors.** Already done (`T::new`, `T { .. }`, `new T()`, `T()`).

Steps 1 and 2 cover `x.ok()` after `let x = something_returning_result()`.

**Verdict: SOLVABLE** for the case in the ROADMAP (steps 1 and 2, a day's work). **BY DESIGN** for the rest: a call on an untyped value with a non-std name is matched by name, as GitHub's and Sourcegraph's search-based navigation do, and scrutate already ranks those after the decided ones.

## 6. Chunk text of a one-line file with deep nesting

Code: `head_bytes(src, a, b, n)` (`crates/scrutate/src/parse.rs:282-288`) cuts after `n` newlines (`MAX_CHUNK_LINES` or `MAX_CONTAINER_LINES`, `:844`). With no newline, every chunk keeps the text from its start to its end. For nesting depth k in a line of L bytes, the chunks hold about k times L/2 bytes in all. `MAX_FILE_BYTES` is 1,000,000 (`index.rs:33`). The token bags are already capped (`BODY_CAP`, `index.rs:257`). Only `text` is not.

| Source claim | Verdict for scrutate |
| --- | --- |
| LlamaIndex `CodeSplitter`: `DEFAULT_CHUNK_LINES = 40`, `DEFAULT_MAX_CHARS = 1500`. Chunks are byte slices `text_bytes[child.start_byte : child.end_byte]`, and a leaf over the limit goes through `_split_oversized_leaf` ([code.py](https://github.com/run-llama/llama_index/blob/main/llama-index-core/llama_index/core/node_parser/text/code.py)) | The common tree-sitter RAG chunker caps by characters and by node bytes, not by lines. |
| Zoekt: `SizeMax` defaults to `2 << 20` (flag `-file_limit`), and `TrigramMax` ("maximum number of distinct trigrams per document") to 20000 ([builder.go](https://github.com/sourcegraph/zoekt/blob/main/index/builder.go)). Matches are offsets into the stored file (`ChunkMatches`, `NumContextLines`, [api.go](https://github.com/sourcegraph/zoekt/blob/main/api.go)) | Zoekt stores each file once and cuts snippets at query time, so nesting costs it nothing. |

**Verdict: SOLVABLE.** Give `head_bytes` a byte cap as well as the line cap, for example 16 KB for a function and the existing line cap for containers, with the existing `"\n..."` marker. Same in `python/scrutate_ref/parse.py`. The memory then grows with the number of chunks times the cap, not with depth times line length. The longer change, storing chunk offsets and slicing the file at answer time as Zoekt does, is not needed for this limit.

## 7. JavaScript `var` in a nested block

Code: `shadowed` (`crates/scrutate/src/resolve.rs:1358-1385`). For JavaScript it skips `statement_block` children that do not hold the use (`block_scoped`, `:1368-1371`). The `shortcut:` comment at `:1367` names the limit. tree-sitter-javascript 0.25.0, parsed here, gives `var a` a `variable_declaration` node and `let b` a `lexical_declaration` node, and a function in a block is a `function_declaration` in a `statement_block`.

| Source claim (ECMAScript 2027, the draft at [tc39.es/ecma262](https://tc39.es/ecma262/)) | Verdict for scrutate |
| --- | --- |
| 14.3.2 Variable Statement, Note: "A var statement declares variables that are scoped to the running execution context's VariableEnvironment. Var variables are created when their containing Environment Record is instantiated and are initialized to undefined when created." ([14.3.2](https://tc39.es/ecma262/multipage/ecmascript-language-statements-and-declarations.html#sec-variable-statement)) | A `var` in any nested block binds the name for the whole function, before and after the block. |
| 14.2.3 BlockDeclarationInstantiation, Note: "When a Block or CaseBlock is evaluated a new Declarative Environment Record is created and bindings for each block scoped variable, constant, function, or class declared in the block are instantiated" ([14.2.3](https://tc39.es/ecma262/multipage/ecmascript-language-statements-and-declarations.html#sec-blockdeclarationinstantiation)) | `let`, `const`, `class` and a function declared in a block are block-scoped. scrutate is right for these. |
| 10.2.11 FunctionDeclarationInstantiation: "If strict is true", the lexical environment is the variable environment. "Else, Normative Optional: If the host is a web browser or otherwise supports Block-Level Function Declarations Web Legacy Compatibility Semantics", each function declared in a block also gets a var binding when that "would not produce any Early Errors" ([10.2.11](https://tc39.es/ecma262/multipage/ordinary-and-exotic-objects-behaviours.html#sec-functiondeclarationinstantiation), [B.3.2](https://tc39.es/ecma262/multipage/additional-ecmascript-features-for-web-browsers.html#sec-block-level-function-declarations-web-legacy-compatibility-semantics)) | In sloppy scripts a block function is also visible in the function after the block runs. Modules and classes are strict, so it does not apply there. |

**Verdict: SOLVABLE.** In `shadowed`, for `Lang::Js`, keep walking into nested `statement_block`s, while still stopping at nested functions, and accept a binder there only when its declaration is a `variable_declaration` (or a `for (var ...)` head). This is a few lines plus two tests (`var` before and after the block). Annex B block functions can stay block-scoped: that is the strict-mode rule, and modules are strict.

## 8. File name for a single file argument

Checked here with ripgrep 15.2.0, GNU grep 3.8 (container) and `target/release/scrutate`:

| Command | rg | GNU grep | scrutate |
| --- | --- | --- | --- |
| `PAT a.txt` | `hello` | `hello` | `a.txt:1:hello` |
| `PAT d` (directory; `grep -r`) | `d/b.txt:hello` | `d/b.txt:hello` | name shown |
| `PAT a.txt d/b.txt` | names | names | names |
| `-H PAT a.txt` | `a.txt:hello` | `-H` exists | `error: unexpected argument '-H'` |
| `-I PAT d` | `hello` | (`-h`) | `error: unexpected argument '-I'` |

| Source claim | Verdict for scrutate |
| --- | --- |
| ripgrep `HiArgs::from_low_args`: `low.with_filename.unwrap_or_else(\|\| low.vimgrep \|\| !paths.is_one_file)`, where `is_one_file` is one explicit path that is `-` or not a directory, or the implicit stdin ([hiargs.rs](https://github.com/BurntSushi/ripgrep/blob/master/crates/core/flags/hiargs.rs)) | That is the rule to copy. |
| `rg --help`: `-H` "This is the default when more than one file is searched", `-I` "This is the default when ripgrep is explicitly instructed to search one file or stdin", and each overrides the other | Flags to add. |
| ripgrep shows line numbers by default only when `state.is_terminal_stdout && !paths.is_only_stdin()`, or with `--column` or `--vimgrep` (same file) | A second difference: `scrutate grep` into a pipe prints `:1:` and rg does not. |
| GNU grep 3.8 `--help`: `-H, --with-filename print file name with output lines`; `-h, --no-filename suppress the file name prefix on output`. `grep -r PAT a.txt` prints no name | GNU and rg agree on all four commands above. |

**Verdict: SOLVABLE.** Use ripgrep's rule for the default, add `-H/--with-filename` and `-I/--no-filename` (scrutate's `-h` is help, as in rg), and decide whether line numbers follow rg's terminal rule too. The MCP `grep_code` tool should keep printing paths: a model cannot see which file it asked about. The work is a few lines in the option parsing plus the tests that expect `a.txt:1:`.

## 9. An emptied `chunks` table

Code: on open, `Triage` (`crates/scrutate/src/index.rs:640-675`) runs `SELECT count(*) FROM sqlite_master`, `PRAGMA quick_check(1)` for databases up to 64 MB (`QUICK_CHECK_MAX_BYTES`, `:31`), and checks the meta keys. When the lexical store is reopened, `index.rs:770-781` already compares `n_rows` (the segment rows) with `n_live` (`SELECT COUNT(*) FROM chunks`). When they differ it treats the missing ids as deleted. An empty table therefore looks like "every chunk deleted", and searches return nothing.

| Source claim | Verdict for scrutate |
| --- | --- |
| `integrity_check` checks entries "out of sequence", "Misformatted records", "Missing pages", "Missing or surplus index entries", constraint errors and the freelist, in "O(NlogN) time"; `quick_check` "does not verify UNIQUE constraints and does not verify that index content matches table content" and "runs in O(N) time" ([pragma](https://www.sqlite.org/pragma.html)) | A table emptied with `DELETE` is well formed, and both checks pass it. SQLite cannot report it. |
| `data_version` values "will be different if changes were committed to the database by any other connection in the interim", and "It is only meaningful to compare ... the same database connection" (same page) | This catches a change made while scrutate holds the connection (an MCP server), not one made between two runs. |
| `user_version` and `application_id`: "SQLite makes no use of the user-version itself" (same page) | An application counter has to be written by the application. |

Options and their cost:

- **Free:** at `index.rs:772`, if `n_live == 0 && n_rows > 0`, and `files` holds a row whose hash is not `SKIP`, return `Damaged("chunks table emptied")` so `Repo::rebuild` runs (`lib.rs:149`). Both counts are already computed.
- **O(log N) on open:** `SELECT EXISTS(SELECT 1 FROM files WHERE hash != 'SKIP' AND size > 0)` against `SELECT EXISTS(SELECT 1 FROM chunks)`. Two lookups of one row each.
- **Catches partial deletion too:** write `chunk_count` into `meta` in the same transaction as each update, and compare it with `COUNT(*)` on open. The count is O(N). The code runs it on every lexical reopen anyway, so it costs nothing new there.
- **While serving:** read `PRAGMA data_version` before each MCP answer. A change by another process means reopen.

**Verdict: SOLVABLE.** The free check plus the meta count close it. The meta count needs a schema bump. This has nothing to do with SQLite's checks, which are about structure.

## 10. C++ members declared in a class body

Parsed here with tree-sitter-cpp 0.23.4 (the version in `Cargo.lock`):

| Code | Node in the class body (`field_declaration_list`) |
| --- | --- |
| `A(int x);` | `declaration` > `function_declarator` > `identifier "A"` |
| `~A();` | `declaration` > `function_declarator` > `destructor_name` |
| `explicit A(const A&) = default;` | `function_definition` with `default_method_clause` |
| `A(){ }` | `function_definition` |
| `template <typename T> void f(T t);` | `template_declaration` > `declaration` > `function_declarator` > `identifier "f"` |
| `operator bool() const;` | `declaration` > `operator_cast` > `abstract_function_declarator` |
| out of class `A::A(int x) {}` | `function_definition` > `function_declarator` > `qualified_identifier` |
| out of class `A::operator bool() const {}` | `function_definition` > `qualified_identifier` > `operator_cast` (no `function_declarator`) |

Code: `CPP_DEFS` (`crates/scrutate/src/langs.rs:232-243`) has `function_definition` (named by digging `declarator` for `ID_KINDS`) and `field_declaration` through `cpp_member_decl` (`:1814-1818`). That function accepts only a `function_declarator` whose name is a `field_identifier`, `identifier`, `destructor_name` or `operator_name`. A plain method prototype `void f();` is a `field_declaration`, and a constructor, destructor or conversion operator prototype is a `declaration`, so the last three never reach it. `template_declaration` is already in `wrappers` and `outer` (`:449-450`).

**Verdict: SOLVABLE.** Add `d("declaration", "function", &[NameRule::Custom(cpp_member_decl)])` and widen `cpp_member_decl`: take a `declaration` only when its parent is a `field_declaration_list`, so free prototypes stay out. Accept `operator_cast` as the declarator and name it `operator bool`, using the text up to the parameter list. Use the same name for the out-of-class `function_definition > qualified_identifier > operator_cast` form if `ID_KINDS` misses it (see Not verified). Add one sample per row of the table to the C++ test file.

## 11. Running the Windows and Intel macOS builds

Checked here: `arch -x86_64 /usr/bin/true` fails with "Bad CPU type in executable" and `oahd` is not running, so Rosetta 2 for macOS is not installed. `/Library/Apple/usr/libexec/oah/` holds only `RosettaLinux/`.

| Route | Source claim | Proves | Does not prove |
| --- | --- | --- | --- |
| Rosetta 2, `softwareupdate --install-rosetta` | "Rosetta is available for any Mac with Apple silicon using macOS 27 or earlier"; from macOS 28 only "certain older, unmaintained games" ([Apple 102527](https://support.apple.com/en-us/102527)) | The `x86_64-apple-darwin` binary loads, links the system libraries and passes its tests. This Mac runs macOS 27.2, the last release with full support. | A real Intel CPU's features and timing, older macOS releases. |
| Windows 11 Arm in a VM | Microsoft names Parallels Desktop as the authorized solution on M1 to M3; "A unique license is required for each instance" ([Microsoft](https://support.microsoft.com/en-us/windows/experience/platform-variants/options-for-using-windows-11-with-mac-computers-with-apple-m1-m2-and-m3-chips)). The ARM64 ISO is a free download ([download](https://www.microsoft.com/en-us/software-download/windows11arm64)). VMware Fusion Pro is free for all use since 13.5.2 ([VMware](https://blogs.vmware.com/cloud-foundation/2024/11/11/vmware-fusion-and-workstation-are-now-free-for-all-users/)). UTM: "You must have a valid license from Microsoft" ([UTM](https://docs.getutm.app/guides/windows/)). Prism emulates x86 and x64 user-mode code ([Microsoft Learn](https://learn.microsoft.com/en-us/windows/arm/apps-on-arm-x86-emulation)) | Real Windows: loader, CRT, paths, file locking, console. Run both the `x86_64-pc-windows-gnu` build (Prism) and an `aarch64-pc-windows-gnullvm` build (native). | Native x64 hardware. The 90-day Enterprise evaluation is x64 only ([eval center](https://www.microsoft.com/en-us/evalcenter/download-windows-11-enterprise)), so it does not help on this Mac. |
| Wine on the arm64 Linux VM | Wine ships no x86 emulator. One is plugged in through `HKLM\Software\Microsoft\Wow64\x86`, and FEX implements it (Wine 9.6 notes, [wine-announce](https://list.winehq.org/hyperkitty/list/wine-announce@list.winehq.org/latest?page=9&view=threads)). Hangover packages Wine with FEX for x86_64 programs on Debian and Ubuntu ([Hangover](https://github.com/AndreRH/hangover)). Wine 11.0 is the stable release (2026-01-13) | The PE links and runs against Wine's Win32 layer. An `aarch64-pc-windows-gnullvm` build runs with no emulator. | Windows. Wine is a reimplementation, and FEX or box64 is the CPU. |
| amd64 Wine in Colima `--vz-rosetta` | Colima: `colima start --vm-type=vz --vz-rosetta` ([README](https://github.com/abiosoft/colima)). Rosetta for Linux has a reported `MAP_FIXED` bug ([colima #1452](https://github.com/abiosoft/colima/issues/1452)). No first-party statement on Wine under Rosetta for Linux was found | The same as Wine, with Rosetta as the CPU. | The same as Wine, and it may not start at all (the `MAP_FIXED` risk is an inference). |
| Rust targets | `x86_64-pc-windows-gnu` is Tier 1 with host tools; `x86_64-pc-windows-gnullvm` and `aarch64-pc-windows-gnullvm` are Tier 2 with host tools ([platform support](https://doc.rust-lang.org/nightly/rustc/platform-support.html)) | No MSVC needed for any route above. | |
| GitHub-hosted runners | `windows-latest`, `windows-2025`, `windows-11-arm`, `macos-15-intel`, `macos-26-intel` ([runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)). Free on public repositories. Private: $0.010 a minute on Windows, $0.062 on macOS ([pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing)) | Real Windows x64 and a real Intel Mac. | Nothing more to add. This is the only route on native hardware. |

**Verdict: EXTERNAL**, three decisions for the owner, cheapest first:

1. Install Rosetta 2 (free, one command, changes the system). That runs every Intel macOS test here.
2. Install VMware Fusion or UTM with Windows 11 Arm. The software is free, the Windows 11 Pro licence is not. That runs the Windows tests on real Windows under Prism.
3. Publish the repository so `ci.yml` runs on `windows-latest` and `macos-15-intel` at no cost.

Wine in the arm64 VM is free and fast, but it can only be a smoke test.

## 12. Benchmarks on x86_64 Linux and on dedicated idle hardware

Prices are excl. VAT, as each provider's page or price feed showed them on 2026-11-11.

| Option | Source claim | Fits the "dedicated, idle" rule? |
| --- | --- | --- |
| Hetzner dedicated AX41 | Ryzen 5 3600, 64 GB, "€ 0.0945 per hour", "€ 0.00 one-time setup fee", "No minimum contract term", cancellation "immediately" ([AX41](https://www.hetzner.com/dedicated-rootserver/ax41/)). AX102 (Ryzen 9 7950X3D) €0.415 an hour plus €129 setup ([AX102](https://www.hetzner.com/dedicated-rootserver/ax102/)) | Yes, a whole machine. |
| Scaleway Elastic Metal | EM-B130E-NVMe (EPYC 4245P, Zen 5) €0.247 an hour. The one-month commitment fee applies to monthly billing only, and these are a "100%-dedicated machine without a hypervisor" (public catalog API; [FAQ](https://github.com/scaleway/docs-content/blob/main/pages/elastic-metal/faq.mdx)) | Yes. |
| Latitude.sh | m4.metal.small (AMD 4244P, 6 cores) $0.41 an hour ([pricing](https://www.latitude.sh/pricing)) | Yes. |
| Vultr bare metal | EPYC 4245P $0.41 an hour (public API `plans-metal`) | Yes. |
| OVHcloud | Dedicated servers are monthly with an installation fee. Hourly "Metal Instances" from $0.545 an hour ([metal instances](https://www.ovhcloud.com/en/public-cloud/metal-instances/)) | Yes. |
| AWS `.metal` (us-east-1, Linux) | c7g.metal $2.32, c7i.metal-24xl $4.284, c6a.metal $7.344 an hour ([on-demand](https://aws.amazon.com/ec2/pricing/on-demand/)) | Yes. Billed per second. |
| Equinix Metal | Sales stopped in November 2024, shut down 2026-06-30 ([sunset](https://deploy.equinix.com/blog/sunsetting-equinix-metal)) | Gone. No public successor was found. |
| Hetzner Cloud CCX | Dedicated vCPU, CCX13 €0.0593 to CCX63 €1.1626 an hour ([cloud](https://www.hetzner.com/cloud/general-purpose/)) | No. These are VMs on a shared host. |
| GCP sole-tenant, Azure Dedicated Host | VMs "physically separated" ([GCP](https://docs.cloud.google.com/compute/docs/nodes/sole-tenant-nodes)); "No other customer's VMs will be placed on your hosts" ([Azure](https://learn.microsoft.com/en-us/azure/virtual-machines/dedicated-hosts)) | Close, still a hypervisor. More expensive than bare metal. |
| AWS EC2 Mac | Dedicated Hosts "with a minimum allocation period of 24 hours" ([EC2 Mac](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/ec2-mac-instances.html)). mac2 (M1) $0.65, mac-m4 $1.23 an hour | Yes, 24 hours minimum. |
| Scaleway Apple silicon | Mac mini M4 - S €0.22 an hour. "The minimum lease period for Apple silicon is 24 hours due to licensing restrictions" ([FAQ](https://github.com/scaleway/docs-content/blob/main/pages/apple-silicon/faq.mdx)) | Yes. |
| MacStadium | Monthly or annual only, from $109 a month ([pricing](https://www.macstadium.com/pricing)) | Yes, but no hourly option. |
| Windows on bare metal | Hetzner Windows Server 2025 Standard €27.90 a month for 6 or 8 cores, "We will also invoice you for a full calendar month" ([add-on](https://www.hetzner.com/add-on-product/show/windows-server-2025-standard-edition/)). AWS m5zn.metal with Windows $6.1721 an hour | Yes. Windows Server, not Windows 11. |
| GitHub-hosted runners | Each job runs on "a new virtual machine (VM)", and Linux runs on "a multi-cloud compute pool" ([runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)). Larger runners "run on virtual machines (VMs)", 2 to 96 vCPU, and need a Team or Enterprise Cloud plan ([larger runners](https://docs.github.com/en/actions/reference/runners/larger-runners)) | No. No size promises dedicated hardware or a fixed CPU model. |

Cheapest plan for one 6-hour session:

| Target | Plan | Cost |
| --- | --- | --- |
| (a) x86_64 Linux bare metal | Hetzner AX41, hourly, cancel after the run | 6 × €0.0945 = **€0.57** |
| (a) with a current CPU | Scaleway EM-B130E-NVMe (Zen 5) | 6 × €0.247 = **€1.48** |
| (b) Apple Silicon Mac | Scaleway Mac mini M4 - S, 24-hour minimum | 24 × €0.22 = **€5.28** |
| (b) on AWS | mac-m4.metal, 24-hour minimum, plus EBS | 24 × $1.23 = **$29.52** |
| (c) Windows | Hetzner AX41 plus Windows Server 2025 Standard (monthly licence) | €0.57 + €27.90 = **€28.47** |
| (c) billed per second | AWS m5zn.metal, Windows licence included, plus EBS | 6 × $6.1721 = **$37.03** |

**Verdict: EXTERNAL.** It needs a provider account, a card and about €36 (a, b and c on Hetzner and Scaleway). The harness and corpora are ready (`docs/RELEASE_READINESS.md`, R-14.11). For "#1" claims, record the CPU model, kernel, governor and SMT state with each run, because the AX41 and the EPYC 4245P are different machines.

## Not verified

- Whether the ripgrep mmap path (used for a few explicit files on Linux) reads a size-0 procfs file as empty. No source said so, and rg was not run on Linux.
- What scrutate itself prints for `/proc/kallsyms` on Linux. The short read was measured with `perl sysread`, and the conclusion comes from reading `split::first`, not from running the Linux binary.
- Whether every indexed non-empty file yields at least one chunk. The "free" check in section 9 assumes it.
- Whether `ID_KINDS` already names the out-of-class `A::operator bool() const {}` definition, whose `function_definition` has no `function_declarator`.
- Whether Rust's `std` on Windows exposes a stable file change time. `MetadataExt` is known to have creation, access and write times. Nothing was checked for a stable `change_time`.
- rust-analyzer is not installed here, so its SCIP output for calls inside `println!` arguments was not run. That it finds them is inferred from its design (macro expansion, `descend_into_macros`).
- Wine under Rosetta for Linux in Colima: no source found either way. The `MAP_FIXED` risk is an inference from a Colima issue.
- Microsoft's licence page names Parallels on M1 to M3 only. Whether running Windows 11 Arm in VMware Fusion or UTM on an M4 or later is covered was not stated by Microsoft. The vendors say a valid licence is needed.
- Parallels' current price and trial length: the buy page loads prices in the browser.
- Stock of the Scaleway EM-B130E in fr-par-2, and whether Hetzner's Windows add-on can be ordered on an AX41. Scaleway and Vultr prices come from their public pricing APIs, because their web pages refused automated fetches.
- The GNU grep manual and GNU make manual pages returned HTTP 429. The grep and make rules come from `grep --help` (GNU grep 3.8 in a container) and `man make` (GNU Make 3.81 here).
- No source gives a measured precision for syntax-only resolution of method calls.
