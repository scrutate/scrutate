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
tree included, until the request ended; a tree takes 90 to 176 bytes a node in memory, by its grammar
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
   recently used first under `LOADED_CAP` (256 MiB): their text, their bindings (64 bytes each),
   their definitions (160 bytes each and their names) and the parses they hold, a parse weighed as
   the store weighs it. The set drops files when one is loaded, when one
   is found in it, and when a file in it makes its tree, sparing the file in use. A file dropped
   from the set is loaded again when asked for again. What the resolver holds is bounded by
   `PARSED_CAP` plus `LOADED_CAP` of weight, plus the two memos below (16 MiB each) and the one
   file in use beyond the cap, in a fresh
   process and in a `serve`; a tree a caller still holds after its file left the set goes when
   the caller drops it.
   Two memos sit beside the set for the length of the request, each under 16 MiB: by path, each
   file's definitions, bindings and depth with the hash of the bytes they came from, used only
   while the file still has that hash (`FACTS_MEMO`), so a dropped file
   comes back without reading its two rows in the index again (its tree is still built only when
   asked for, as without the memo); and, by spelling and
   limit, the occurrences a nested `references` of a class name found that read as an
   instantiation (`NESTED_MEMO`), so `references` of `__init__` looks each class name up once
   however many classes share it. A file whose hash moved is learned again. Without them a
   request over more than the set holds read the same files again for every class of a name:
   `refs __init__` over six copies of the Python standard library parsed 3,618 files 85,828 times.
4. **Node ids stay safe.** The resolver keeps two caches keyed by tree node ids (`found`, `tys`).
   Once trees can go during a request, a later tree can reuse an id, so every `Parsed` that drops
   a tree counts it (`TREES_FREED`), and a resolver that sees the count move empties both caches
   before it reads them.
5. **The process gives freed memory back.** The bounds above are on counted weight. On Linux with
   glibc a process holds more: each thread allocates from an arena of its own, and the pages a
   thread frees stay with that arena until something asks for them. `serve` answers each call
   on a thread of its own (up to 32 in flight), so over a session of large requests its RSS climbed to 1.7 to 2.2 GB
   while what it held stayed under the bounds. After an answer is out, `serve`, `lsp` and `batch`
   call `memory::give_back_memory`, which calls `malloc_trim(0)` when the request ran a second or
   more or a second has passed since the last call. A long request pays the trim each time (6 to
   77 ms, about 25), a burst of short requests once a second, and what stays unreturned is what
   short requests freed within one second. A first form trimmed only after a request that used the
   resolver; a session of other calls then never trimmed, and 160 `grep_code`, `search_code` and
   `find_files` calls through one `serve` climbed from 101 to 196 MB, where trimming after each
   held them at 62 to 83 MB (Reviewer 6's runs). On macOS, Windows and musl it does nothing:
   musl's allocator, measured below, gives pages back by itself. The watch loop holds no resolver
   store and does not call it.

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
occurrences most of the time was the walk itself: looking a name up through each use's includes,
again for every occurrence (`refs mutex_lock`: memcmp 15% of the samples, malloc 13%,
`lookup_top` 4.4%, `c_include` 4.2%). So the resolver keeps, for one request, what an include
names, the files a lookup from a file reads in order (the same for every name), and each file's
definitions by name. Same box and runs as above, with those kept:

| | before | after, with them |
|---|---|---|
| `refs kmalloc_array` | 20.9 s, 6.4 s; 722 MB | 31.0 s, 5.9 s; 405 MB |
| `refs mutex_lock` | 100.8 s, 123.2 s; 6.2 GB | 129.6 s, 76.1 s; 652 MB |
| `refs spin_lock_irqsave` | 53.0 s, 65.6 s; 4.0 GB | 55.6 s, 41.4 s; 580 MB |
| `refs list_add` | 21.3 s, 23.4 s; 2.6 GB | 21.8 s, 17.1 s; 492 MB |
| `refs container_of` | 71.1 s, 89.6 s; 8.0 GB | 96.9 s, 71.1 s; 673 MB |
| `dead`, the whole Linux tree | 4,179 s; 23.8 GB | 4,642 s; 4.34 GB |

Run 1 here was each binary's first read of its own cold copy; run 2 is the fair comparison. With
them, `dead` on the whole Linux tree takes 11 percent longer than the build before, where it
took 54 percent longer without them. The answers are again the same byte for byte.

The two memos beside the set (decision 3) were measured on scrutate-build (c7a.4xlarge, 16 cores)
with `refs --json --max-results 2000 __init__` over six copies of the Python 3.14 standard library
and a virtualenv (4,205 files, 84 MB), each binary on its own copy with an index it built, runs
interleaved: 48629c0a, before the set, took 136.8, 125.0, 123.0 and 148.5 s at 1.93 GB; the set
without the memos 634.5 and 622.1 s at 0.56 GB, loading 4,052 files 395,072 times and parsing
3,618 of them 85,828 times; with the memos 105.7, 100.5, 109.9 and 98.8 s at 0.55 to 0.56 GB. Without the
facts memo the same request took 111.2, 108.6 and 108.8 s against 110.6, 100.0 and 100.4 s with
it, 8 percent more CPU (Reviewer 6's runs, same box). All
outputs are the same byte for byte. `dead` on the whole Linux tree, the set without and with the
memos run at once on scrutate-build-3: 4,439 s at 4.44 GB and 4,520 s at 4.37 GB, both the same
output as before.

Giving memory back (decision 5) was chosen from one round on scrutate-build (16-vCPU AMD EPYC
9R14): one `serve` session asks `references` and `callers` of 26 names over the Python tree
above, two sessions each, interleaved, with the same 52 answers in all:

| allocator | first request | RSS after the last request | session peak | session |
|---|---|---|---|---|
| glibc as it is | 98.9 and 103.3 s, 521 MB | 1,683 and 1,681 MB | 1.69 GB | 200 and 175 s |
| `malloc_trim(0)` after each answer | 100.0 and 103.3 s, 524 MB | 513 and 508 MB | 0.76 GB | 179 and 180 s |
| `M_ARENA_MAX` 2 at start | 106.2 and 101.9 s, 518 MB | 1,274 and 1,265 MB | 1.29 GB | 178 and 172 s |

The musl build that CI ships (24da6b5e, before the memos) against a glibc build of the same
commit, one session each: musl ended at 452 MB (peak 539 MB) and glibc at 1,256 MB, with a first
request of 669.6 s against 633.9 s. With the rule as built (Rust 1.100.0, the same box,
alternated with a build that never trims): the 26-name session twice, first request 99.8 and
99.6 s at 503 MB, after the last request 747 and 516 MB, peak 0.81 GB, 36 and 27 trims of 48 to
51 ms, the same answers; in the first session the last call ran within a second of the trim
before it, so what it freed stayed until a next call. 160 `grep_code`, `search_code` and
`find_files` calls through one `serve` ended at 104 MB, against 190 MB never trimming and 98 MB
trimming after every call. 300 short `references` and `callers` calls on a 200-file tree took
3.70 and 3.55 s with 4 trims (under 0.4 ms each), against 4.13 and 3.71 s; 300 `grep_code` calls
on a 20,000-file tree took 2.34 to 2.82 s (2.64 on average over six runs) with 3 trims (3 to 6
ms in all), against 2.51 to 3.03 s (2.70).

The evidence for a whole session is this table, not a test. A session small enough for a test
never drops trees past the caps, which is what leaves freed pages in many arenas: 40 calls over a
200-file tree whose first call loads every file went from 176 to 177 MB with no trim at all. So
`tests/memory_given_back.rs` shows the call gives freed pages back (262 MB stayed without it, none
with it), and `memory::tests` shows when the rule calls it.

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
- **Fewer arenas (`M_ARENA_MAX` 2).** It left the session at 1.27 GB and cost up to 7 percent on
  the first request.
- **Giving memory back after every answer.** A trim costs about 25 ms however small the request,
  so a burst of short editor requests would pay it each time.

## Not done

- Another allocator for the whole process (one that returns pages itself, as musl's does): a new
  dependency, measured nowhere yet.

## Consequences

- `SCHEMA_VERSION` 18, with `binds` the column after `key`: a column after the large `body`
  blob would be reached only through body's overflow pages, about 2 ms a load. An index of an
  older schema is rebuilt, every table dropped and made again;
  `index::tests::the_schema_sql_and_its_version_change_together` ties the SQL to the version.
- The index build walks each tree of a resolver language once more: +5.3% on Linux, +0.25% size.
- A file dropped from a request's set is read again when asked for again, and parsed again if
  the store dropped its tree too.
