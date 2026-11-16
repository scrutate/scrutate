# 8. The resolver reads bindings from the index and holds a bounded set of files

Status: accepted, 2026-11-16.

## Context

`refs` and `dead` held memory without bound on a large C tree. Measured at 5131e165 (release, a
4-core 8 GB build box, the Linux v7.3, hugo v0.170.0 and ripgrep 15.2.0 corpora of
`bench/corpora.py`):

- `dead` on the whole Linux tree was killed by the kernel after 31 minutes at 5.5 GB, with no
  output.
- `refs mutex_lock` took 104 s and held 5.4 GB; `refs kmalloc_array` took 6 to 7 s.

Where the time went (a build with phase timers, and `perf`):

- `refs`: tying the occurrences to definitions took 92.6 of 93.6 s for `mutex_lock` (22,765
  occurrences); the walk took 3 ms and the text search 0.8 s. For `kmalloc_array` the tie took
  6.5 s for 374 occurrences, and in that time the resolver loaded 3,272 files. Loading, which is
  mostly parsing, was 6.6 s of it: tree-sitter 40% of the samples, the kernel 28%, the
  allocator 10%.
- `dead` on one Linux directory: the text pass over the whole tree took 21 s (13.3 s of it
  loading every definition and building the name maps), the resolver 2.7 s.

Two causes. For a C name with nothing in front, the resolver walks the files the use's file
includes, up to 64, and loaded each one by parsing it, though that walk reads only a file's
definitions (from the index) and its `#include` lines. And `Resolver` kept every loaded file,
tree included, until the request ended; a tree takes about 110 bytes a node in memory
([ADR 4](0004-parsed-files-kept-across-requests.md)), so a request that touches thousands of
files held gigabytes.

The text pass of `dead` and `refs` is a small part on Linux. An occurrence table in the index,
which this item first proposed, would not have moved these numbers.

## Decision

1. **Bindings from the index, by content.** When the code index parses a file of a language the
   resolver reads, it also stores what the resolver takes from that tree (`bindings_of`, the same
   function on the same tree, and whether the tree is too deep) in the `binds` column of the
   file's artifact. The artifact is keyed by the content hash and the parts of the path a parse
   reads, so every path with those bytes shares one row, also after a branch switch, a rebuild
   from stored artifacts or a lookup in the main checkout's store, and the row leaves when its
   artifact does. A row carries `resolve::BINDINGS_REVISION` and a key of the grammar as the
   build has it (its node kinds and fields, and the tree it makes of a sample text), so a row is
   read only under the rules and the grammar that made it, however the binary was built; a test
   pins what the rules take from a tree, so a change of them raises the revision.
2. **Trees on demand, in one store.** `Resolver::load` reads and hashes the file as before. When
   the index holds bindings for the path made from those exact bytes, the file's tree is not
   built until something asks for it (`FileInfo::tree`). A tree is built through ADR 4's store
   (`ParsedStore::put`), under its cap and its weight. No row, another hash, or a file the index
   does not hold: the file is parsed at once, as before. The same bytes give the same tree and the
   same bindings, so every answer is what a parse gives.
3. **A bounded set of loaded files per request.** `Resolver` holds the files it loaded least
   recently used first under `LOADED_CAP` (256 MiB): their text, their bindings and the parses
   they hold, weighed as the store weighs them. The set drops files when one is loaded, when one
   is found in it, and when a file in it makes its tree, sparing the file in use. A file dropped
   from the set is loaded again when asked for again. What the resolver holds is bounded by
   `PARSED_CAP` plus `LOADED_CAP` of weight, plus the one file in use beyond the cap, in a fresh
   process and in a `serve`; a tree a caller still holds after its file left the set goes when
   the caller drops it.
4. **Node ids stay safe.** The resolver keeps two caches keyed by tree node ids (`found`, `tys`).
   Once trees can go during a request, a later tree can reuse an id, so every `Parsed` that drops
   a tree counts it (`TREES_FREED`), and a resolver that sees the count move empties both caches
   before it reads them.

## Measured

Before is 7cabeb7a, after is this commit's code on that base; both built in release on
scrutate-build-3 (a c7a.8xlarge: 32 cores, 61 GB; load 6 to 20 from other jobs), with tree-sitter
0.27.2 on both sides, runs interleaved, each binary on its own copy of the corpora with an index
it built. The commit was then moved onto newer bases without its code changing; the latest adds
dependency bumps (rusqlite 0.43 among them) and other items' work, and was not measured again.
`refs` is given as run 1 and run 2, then peak memory; run 1 is each binary's first read of its
own copy, so its page cache is colder.

| | before | after |
|---|---|---|
| index Linux, fresh build | 488.5 s | 315.5 s |
| index Linux, `index --force` | 1,174.8 s | 686.8 s |
| index Linux, `.scrutate` size | 6,539,044 KB | 6,558,096 KB (+0.29%) |
| `refs kmalloc_array` | 20.9 s, 6.4 s; 722 MB | 4.5 s, 4.6 s; 384 MB |
| `refs mutex_lock` | 100.8 s, 123.2 s; 6.2 GB | 111.5 s, 111.8 s; 671 MB |
| `refs spin_lock_irqsave` | 53.0 s, 65.6 s; 4.0 GB | 59.2 s, 57.7 s; 563 MB |
| `refs list_add` | 21.3 s, 23.4 s; 2.6 GB | 18.8 s, 18.6 s; 470 MB |
| `refs container_of` | 71.1 s, 89.6 s; 8.0 GB | 77.0 s, 77.2 s; 763 MB |
| `dead fs/ext4`, cold and kept | 11.3 s, 5.2 s | 10.5 s, 5.2 s |
| `dead drivers/net/ethernet/intel/e1000`, kept | 11.7 s | 13.5 s |
| hugo `dead`, cold and kept | 4.24 s, 0.32 s | 4.28 s, 0.33 s |
| `dead`, the whole Linux tree | 4,179 s; 23.8 GB | 6,442 s; 4.35 GB |

The before build always ran first, so its two index rows carry an ordering effect; the after
build was never slower. `dead` of a Linux directory peaks at 0.5 to 0.7 GB both ways; its cold
e1000 run took 6.6 to 55.6 s in the three binaries of this table, depending on what the page
cache held. Every answer is the same byte for byte: `refs` of six names on ripgrep, six on hugo
and five on Linux, `dead` on ripgrep, hugo and the two Linux directories, two runs each, and
`dead` on the whole Linux tree (118,303,225 bytes).

`dead` on the whole Linux tree is where the bound costs time. Before, it held 23.8 GB at its
peak; bounded, it holds about 4.3 GB, most of it what `dead` itself keeps (every definition and
the lines that name it), and takes 54 percent longer, since its candidates' lines spread over
the tree and a bounded set reads their files again. Asking in rounds, every open candidate's
next line grouped by file, would read each file once per round; that is a separate step.

The same run under a 7 GB memory limit (scrutate-build-2, a systemd scope with MemoryMax=7G and
MemorySwapMax=0, each binary on its own copy of the tree with an index it built):

- before (7cabeb7a): killed after 2,042 s. Exit 137; `/usr/bin/time` 2042.07 s, 7,337,776 KB;
  memory.peak 7,516,192,768 bytes; memory.events max 3692, oom 1, oom_kill 1; no output.
- this chain's tip (6dc49f28, the code of this commit and the next), run twice on the same box:
  exit 0 after 5,224 s, max RSS 4,152,380 KB, memory.events max 0, oom 0, oom_kill 0,
  memory.peak 7,036,764,160 bytes (Reviewer 4's run); and exit 0 after 5,167 s, max RSS
  4,137,192 KB, memory.events max 811, oom 0, oom_kill 0, memory.peak 7,516,192,768 bytes
  (the Architect's run). memory.peak counts page cache: in the second run the scope reached its
  limit 811 times and gave back cache each time, so its peak equals the limit. The process's own
  memory peaked at 4.1 GB both times; the page-cache pressure varies by run. Both outputs are
  118,303,225 bytes, the same as the runs without a limit.

The cap: with `LOADED_CAP` at 64 MiB, 256 MiB and 1 GiB, `refs mutex_lock` took 140.0, 116.9 and
104.0 s at 446, 670 and 1,227 MB, and `refs container_of` 99.3, 76.3 and 71.7 s. Below 256 MiB
the set reads files again and drops trees, and every tree dropped empties the node-id caches.
On another box type (m8a, 32 cores) the 64 MiB build was 30 to 55 percent slower than the
before build on the three largest names; the first `binds` layout, after `body`, cost about 2 ms
a load. Neither is in the table above.

`refs kmalloc_array` parses 280 files where it parsed 3,272. On a name with tens of thousands of
occurrences most of the time is the walk itself: looking a name up through each use's includes,
again for every occurrence.

## Other choices

- **An occurrence table in the index** (every name with the lines that hold it). It replaces the
  0.8 s of text search in `refs mutex_lock` and part of the text pass of `dead`, not the tie, and
  costs index space for every word of every file.
- **Includes from the `imports` table.** The index records C includes, but `normalize_module`
  drops the `<>` that tells a system include from a quoted one, and its walk differs from
  `c_bindings`, so answers could change.
- **Bindings keyed by path.** After a branch switch or a rebuild from stored artifacts most files
  take their rows from an artifact and would have none.
- **Bindings inside the artifact's body.** The body is decoded whole (chunks, words), which costs
  more than the parse it saves; a column is read alone.
- **A second store of trees per request.** It would hold trees the process store already
  bounds, and the two bounds would add up for the same trees.

## Consequences

- `SCHEMA_VERSION` 18, with `binds` the column after `key`: a column after the large `body`
  blob would be reached only through body's overflow pages, about 2 ms a load. An index of an
  older schema is rebuilt, every table dropped and made again;
  `index::tests::the_schema_sql_and_its_version_change_together` ties the SQL to the version.
- The index build walks each tree of a resolver language once more: +5.3% on Linux, +0.25% size.
- A file dropped from a request's set is read again when asked for again, and parsed again if
  the store dropped its tree too.
