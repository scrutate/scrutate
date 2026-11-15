<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="docs/assets/banner-glossary.svg" alt="Glossary. What does each word of this codebase mean?" width="880">
</p>

# Glossary

[Documentation](docs/README.md) &nbsp;·&nbsp; Previous: [Languages](docs/LANGUAGES.md)

<!-- End of head. -->

The words this codebase uses for its own concepts, one meaning each, used the same way in code,
docs and reviews. Each term links to the section of [Architecture](docs/ARCHITECTURE.md) or the
[CLI reference](docs/CLI.md) that has the detail.

**[Engine A](docs/ARCHITECTURE.md#engine-a-universal-instant-search-cratesscrutatesrcinstantrs)**:
instant search with no index (`instant.rs`, `scrutate grep`): it walks the files and matches each
one. Its one way in is `instant::search`, given a **query**, a **file set** and a cancel flag.

**Query**: what an Engine A search asks for (`instant::Query`): the **pattern** (`instant::Pattern`:
text, mode, case, whole words, multi-line), inversion, the report (lines, counts or files),
context lines and limits.

**File set**: which files an Engine A search reads, and the threads that read them
(`instant::Files`): walk roots with their rules, a list of files under a root, or named text in
memory. How a type meets the ignore rules is in the [CLI reference](docs/CLI.md#engine-a-grep-alias-g).

**Record**: one Engine A result of one kind (`instant::Record`): a match, a context line, a count,
a file, or a note on a file (its search stopped at a NUL byte).

**[Engine B](docs/ARCHITECTURE.md#engine-b-code-intelligence-indexrs-lexrs-parsers-langsrs-searchrs-intelrs)**:
the index and everything answered from it (`index.rs`, `search.rs`, `intel.rs`, `resolve/`):
ranked search, symbols, callers, callees, references, structural queries.

**[Text index](docs/ARCHITECTURE.md#text-index-textindexrs)**: Engine B's trigram index over file
contents (`textindex.rs`). It tells Engine A which files can match, so `grep --index` opens only
those.

**Chunk**: a piece of a file the index stores as one unit, usually a function, class or method.
**Symbol** names a chunk when the question is "what encloses this line"; `index::Symbol` is a
chunk without its text, which name resolution reads.

**[Index](docs/ARCHITECTURE.md#storage-indexrs-lexrs)**: Engine B's store of chunks, call edges
and imports (`index::Index`). Other modules ask it questions and never see its tables.

**[Custom language](docs/CLI.md#custom-languages)**: a language added without a build by a
`[[language]]` table (`custom.rs`). `custom::current` is the set in effect; its **stamp** (each
extension with its language's signature) tells an index update which files to read again.

**[Artifact](docs/ARCHITECTURE.md#storage-indexrs-lexrs)**: what parsing and tokenizing make of one
file, kept in the **artifact store** (`artifacts.rs`, a table of the index) under a key of its
content and the parts of its path the parser reads, so the same bytes elsewhere are not parsed
again. The **path map** (the `art` column of the `files` table) names each file's artifact;
`artifacts::derive` makes a path's rows and postings from one.

**Hit**: one result: a matching line in Engine A (`instant::Record::Match`), a chunk in Engine B
(`search::Hit`).

**Occurrence**: an Engine A hit for a name, before it is known what the name refers to.
**Reference** is an occurrence together with the definition it resolves to.

**[Names](docs/ARCHITECTURE.md#intelligence-intelrs)**: the one owner of what a name refers to
(`names::Names`). Callers, callees, references, dependencies, dependents and the language server's
definition ask it about the occurrence at a place; it answers with the definition and how it was
found, or why none was. One request opens one, so each file is read and
parsed once. Its rules live in `resolve/`.

**[Dead](docs/ARCHITECTURE.md#intelligence-intelrs)** and **undecided**: what `dead` (`dead.rs`,
`Names::dead`) calls a function, method, class or type that is no entry point of its language.
Dead: no line outside its own names it. Undecided: some line names it but the resolver ties none
to it, or its type extends another whose code may call it. An undecidable use never makes one dead.

**[Duplicate keys](docs/ARCHITECTURE.md#storage-indexrs-lexrs)**: what `dupes` compares of one
definition (`intel::DupKeys`): hashes of its body and of its words, its word count and its
MinHash band keys, made once per text at parse time and kept in the index (`dupkeys`).

**[Kept answers](docs/ARCHITECTURE.md#intelligence-intelrs)**: what `callers`, `callees` and
`dead` learned of each call site, stored per request (`answers.rs`, `.scrutate/answers/`) with
what the run read. A later run uses them only while all of that is unchanged.

**[Repo map](docs/CLI.md#engine-b)**: the files that define code, ranked by PageRank over the edges
between files (`repomap.rs`, `scrutate map`, the MCP tool `repo_map`). The edges come from the
index's names without resolving call sites (`Names::file_edges`).

**[Call path](docs/CLI.md#engine-b)**: the shortest chain of calls from one definition to another
(`Names::path`, `scrutate path`, the MCP tool `call_path`), found breadth first over the calls
`callees` resolves.

**[Walker](docs/ARCHITECTURE.md#intelligence-intelrs)**: the part of the resolver every language
shares (`resolve/mod.rs`): scopes, the lookup memo, the depth guards and the order of the steps
that decide what a name refers to.

**[Rule set](docs/ARCHITECTURE.md#intelligence-intelrs)**: what one language decides differently
in name resolution, one file each under `resolve/` behind the `Rules` trait. The walker asks the
rule set of a file's language wherever languages differ.

**[Impact](docs/CLI.md#impact)**: what a change touches (`impact.rs`, `scrutate impact`): the
definitions a git diff changes, their callers through `Names` with the distance of each, the
test files to run, and its findings.

**[Git](docs/ARCHITECTURE.md#intelligence-intelrs)**: the one module that runs the `git` program
(`git.rs`, `git::Git`): the base commit, the diff with the changed lines of each side, the
untracked files and the blobs of base copies. Impact reads a change through it, and `read
--history` the commits that changed a definition's lines.

**[Filter](docs/CLI.md#engine-b)**: what `search`, `symbol` and `refs` keep (`filter::Filter`):
files by glob (`-g`), by type (`-t`) and by the language they were parsed as (`--lang`), and
definitions by kind (`--kind`); the MCP tools take the same as `globs`, `types`, `langs` and
`kinds`.

**[Finding](docs/adr/0003-impact-findings.md)**: something a change does that a reviewer would want
named (`findings::Finding`): a **new duplicate** (a head `dupes` group no base group matches) or a
changed definition no test reaches. Impact makes the list; every front end and review render it.

**[Review](docs/CLI.md#review)**: the report of a pull request (`review.rs`, `scrutate review`):
the impact between a base and a head commit, findings included, rendered as the PR comment the
GitHub Action posts and edits in place (marker `<!-- scrutate-review -->`), as JSON, or as a
GitLab Code Quality report.

**[Rewrite](docs/CLI.md#tsquery---rewrite)**: a structural search that replaces what it finds
(`rewrite.rs`, `scrutate tsquery --rewrite`). A **plan** (`rewrite::Plan`) holds each file's bytes
as read and as rewritten, and prints as a diff or writes each file.

**[Frecency](docs/CLI.md#files)**: how often and how lately a file was read; `scrutate files`
(`finder.rs`, the MCP `find_files`) ranks equal matches by it. Kept in
`$XDG_STATE_HOME/scrutate/frecency.json`, outside the repository.

**Grouped grep**: an Engine A search whose matching lines are grouped by the symbol that encloses
them (`intel::grouped_grep`). The MCP `grep_code` tool, `grep --symbols` and both kinds of
references use it.

**[Front end](docs/ARCHITECTURE.md#unified-layer-opsrs-librs-clirs-mcprs-cratesscrutate-py)**: a
way in: the CLI (`batch` among its commands), the MCP server, the LSP server, the Python bindings
and the watcher. Each parses input into operations and renders their answers, except a plain
`scrutate grep`, which hands its `Query` and `Files` straight to Engine A, and `watch`, which only
writes the stores.

**[Operation](docs/ARCHITECTURE.md#unified-layer-opsrs-librs-clirs-mcprs-cratesscrutate-py)**: one
question a front end can ask of the index (`ops::Op`). It owns its arguments, their defaults and
limits, and whether to refresh the index first (`Op::reads_index`). Its grep carries an
`instant::Pattern` and a **scope** (`ops::Scope`). Front ends parse into operations
(`Op::from_tool` for a tool's JSON arguments) and hold no defaults of their own.

**Answer**: the typed result of an operation (`ops::Answer`), with one text form and one JSON form.
With `serve --structured` the MCP server also sends the JSON form as the **structured** result,
whole when it fits the budget and else cut to the items of the text page (`Answer::structured`).

**Envelope**: the one JSON shape an answer leaves in (`Answer::envelope`): `schema`
(`ops::ANSWER_SCHEMA`), `result` and `stopped_at`. The CLI's `--json` of an index command, a
`batch` record (with `op`) and the structured result (with `complete`) are this envelope.

**Location**: where a symbol is, `path:start-end` (or `path:line`). Every answer starts each result
with one, and every operation that takes a symbol accepts one in place of a name, meaning that
definition only (`intel::location`).

**Budget**: the size limit of an answer as text (`Answer::text_within`). A cut answer keeps whole
items and says how to continue. `grep --max-bytes` pages its lines by the same rule
(`ops::paged_counted`).

**Redaction**: a secret in an answer replaced with `[redacted:KIND]` before the budget pages it
(`secrets::Redactor`), on for what an agent reads (MCP, the hooks, `lsp`) and off for the CLI.

**Form**: how an answer's text is written (`ops::Form`, the MCP argument `response_format`):
**detailed**, or **concise**, every location and name without what a call can ask for again. Both
page the same items (`Answer::text_as`).

**[Repo](docs/ARCHITECTURE.md#unified-layer-opsrs-librs-clirs-mcprs-cratesscrutate-py)**: an open
repository (`scrutate::Repo`, in `stores.rs`): the searcher on its index plus the stores kept
current around it.

**[Session](docs/ARCHITECTURE.md#sessions-sessionrs)**: one repository held open for many calls
(`session::Session`), with parallel reads, each on its own **reader** (a `Repo` from
`Repo::another`), and writes one at a time. The MCP server, the language server, the Python
bindings and `watch` are its adapters. A command line call may use the session a running `serve`
offers on `.scrutate/session.sock`.

**[Change set](docs/ARCHITECTURE.md#sessions-sessionrs)**: the Repository files that changed
since a **token** (`stores::Changes`), as the freshness module (`fresh::Freshness`) answers it:
the files named, or every file when nothing can vouch for fewer. A session's reads ask it before
each call; the walk adapter always says every file, a watch's (`watch::Notes`) names the files
its events touched.

**[Stores](docs/ARCHITECTURE.md#keeping-the-stores-current-storesrs)**: the derived data under
`.scrutate/` and the one module that keeps them current (`stores.rs`). A caller names a **change
set** (`stores::Changes`, see Change set) and what it does (`stores::Writer`), never a store.

**[Model set](docs/ARCHITECTURE.md#models-cratesscrutatemodels-modelsrs)**: the vector model, the ranker and the
feature list the ranker reads (`models::ModelSet`), each loaded on first use.

**[Candidate source](docs/ARCHITECTURE.md#retrieval-and-ranking-searchrs-semanticrs-rankrs)**: one
way a ranked search finds candidates (`search::Source`): exact names, lexical (BM25) or semantic
(vectors). Each candidate gets the 48 **features** by name (`search::FEATURES`); `batch --features`
prints them as the **training rows** the ranker is fitted on.

**[Quality floor](docs/QUALITY.md)**: the lowest MRR@10 and hit@5 the ranking may reach on a frozen
copy of this repository's code before CI fails (`bench/quality.py`, `bench/quality/floor.json`).

**[Ranking pins](docs/ARCHITECTURE.md#retrieval-and-ranking-searchrs-semanticrs-rankrs)**: every
candidate, feature and score of a small tree with the bundled models, recorded bit for bit and
checked by `cargo test` (`tests/ranking_pins.rs`).

**Translation**: what a pattern asks every matcher of it to find (`Pattern::translate`). Engine A's
matcher and the text index's trigram condition are both built from it.

**Run**: one Engine A search in progress (`instant::Run`), shared by every file search of it.

**Thread rules**: how many threads an Engine A search runs (`instant::policy`). They decide over
readings of the machine (`Machine`), given by the operating system (`System`) or a test. One
scheduler (`instant::schedule`) asks them for every walk and file list.

**[Byte source](docs/ARCHITECTURE.md#engine-a-universal-instant-search-cratesscrutatesrcinstantrs)**:
what one Engine A file search reads (`source.rs`): a file on disk, standard input, an archive
member or text held in memory. The read rules are written once on top.

**Repository files**: which files belong to the repository for each index (`files.rs`), including
the files a repository search covers (`files::repository_files`) and **membership**, the test of
one changed path that the watcher asks. A **look** (`files::look`) is one walk of the tree and one
`stat` per file ([Architecture](docs/ARCHITECTURE.md#keeping-the-stores-current-storesrs)).

**[Setup](docs/CLI.md#setup-and-the-grep-hook)**: what `scrutate setup <agent>` writes
(`setup.rs`): one MCP server entry named `scrutate` in an agent's configuration and, on request,
the Grep hook and an instruction block, each added and removed as text.

**[Grep hook](docs/CLI.md#setup-and-the-grep-hook)**: Claude Code's `PreToolUse` hook for its Grep
tool (`hook.rs`, `scrutate hook grep`). It answers a Grep call with Engine A's matches, or lets
Grep run when it cannot answer exactly (it **fails open**). Its **counter** keeps the bytes Grep
would have returned against the bytes sent (`scrutate hook stats`).
