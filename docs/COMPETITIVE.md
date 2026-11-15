<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-competitive.svg" alt="Competitive position. What does the evidence say against other tools?" width="880">
</p>

# Competitive positioning: what the evidence supports

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Release readiness](RELEASE_READINESS.md)

<!-- End of head. -->

What Scrutate 0.1.0 does better than the tools it was measured against, where it ties, and where
it is behind. Performance statements point at docs/LEADERBOARD.md (generated from the raw results
in `bench/results/`), capability statements at a test. Claims about other tools cover only what
those runs observed and the tools' documented options. The machine is a developer laptop (Apple
M3 Pro) shared with other work, and section 14 of the mission keeps "#1" claims for dedicated idle
hardware, so the wins below are measured results, not publishable leadership claims.

## 1. Against grep-class tools (GNU grep, ripgrep, ugrep, git grep, ag, ack)

On the development Mac, Engine A is significantly fastest in all 37 warm-cache benchmarks (30 runs
per contender in three interleaved blocks; the 95% interval of the ratio of medians lies wholly
below 1) and in all 5 cold-cache ones. Its lead over the next tool:

| Corpus | Lead |
|---|---|
| Linux kernel tree | 15 to 30% on 12 of the 13 text searches (ugrep in most rows, ripgrep or git grep where ugrep's answer is wrong), 45% with 1.3 million matching lines |
| LLVM | 15 to 30% on five rows |
| 200,000 small files | 24 to 26% |
| a binary-heavy tree | 24% |
| a 211-file repository | 17 to 24% |
| one 740 MB file, searched on every core | 43 to 58% |
| page cache cleared before every run | 23 to 28% |

Other work on the laptop disturbed six of the 37 rows, so the harness measured each once more
([Leaderboard](LEADERBOARD.md) gives both measurements). Five had passed. The sixth,
`smallfiles/simple regex`, had an interval reaching 1.02 the first time and 0.757 the second. One
second measurement is worse: other applications had by then taken the memory the page cache needs
for the LLVM tree (4.9 GB in 16 KB pages), every tool read part of it from disk, and
`llvm/case-insensitive` is 8% ahead where it had been 16%.

In a 5-CPU Linux VM on the same Mac (one virtual CPU per performance core), Engine A is
significantly fastest on all 14 Linux-tree benchmarks: 7 to 16% ahead of ripgrep on 11, 40% ahead
with 1.3 million matching lines (its output leaves in 64 KB writes, ripgrep's in one write per
file) and 59% on the complex regex. On the structural query ast-grep takes 24 times as long when
scrutate reads its capture cache (a cache hit), but on a first run scrutate is the slower one (1.41
times ast-grep's time on a 4-vCPU Linux x86_64 server, section 2). With
the page cache dropped before every run it takes 0.28 to 0.30 of the next tool's time on all four
rows (git grep or ugrep, 1.2 to 1.4 s each), because it reads a cold tree on up to 64 threads
where the others use one per core: 18 to 24% above the time to merely read the files.

**On macOS, opening files is the expensive part, and more threads make it slower.** Reading every
file of the Linux tree and nothing else (the "read files" column, `bench/rawread.c`) is fastest on
five threads, one per performance core: 518 ms. Scrutate searches the same files in 581 ms, 12%
above that floor; the next tool takes 711 ms. That floor is handed the list of files; the
leaderboard's "walk and read" column (`bench/walkread.c`) is a second one, which lists the tree
and reads every file in it, the least a search that has to find its files does. It reads no
ignore rule, so it is a floor only where the tree holds no untracked or ignored file, and a row
shows it only when it read as many files as the row searches. On LLVM it is 10% above the floor, on 200,000 small
files within 1%. So Scrutate walks with one thread per performance core and adds threads when its
reads show the files coming from disk: up to twice the cores on macOS, up to 64 on Linux
([Architecture](ARCHITECTURE.md#engine-a-universal-instant-search-cratesscrutatesrcinstantrs)).
The other margins come from the matcher and the output path: one large file searched on every
core, regexes anchored at line starts examined only there, and search threads that format their
own output and write it in 64 KB pieces.

**Capabilities the others lack (each verified by tests):** identifier search across casings
(`--ident`), fuzzy search within N edits (ugrep has `-Z` with another edit model), results grouped
by enclosing function (`--symbols`), and an index-backed `grep --index` whose output equals the
walk's.

**Where they remain better**: on no row of this suite; every row is PASS on both platforms. The
narrowest leads: `linux/multiline` in the Linux VM (0.925 of ripgrep's
time), the second measurement of `llvm/case-insensitive` on the Mac (0.915) and
`patho/one-byte lines` (0.869 of ripgrep's: ten million one-byte lines, where printing sets the
time). On large trees on macOS the lead cannot grow much, for the reason above. Outside the suite,
ugrep has an interactive query UI and Boolean queries; Scrutate has neither. ast-grep rewrites
code, and so does `scrutate tsquery --rewrite` with a template of the captures, with fewer means:
no rule files, no interactive review of each change.

**Correctness differences found while making the comparison fair** (the leaderboard's
"Disqualified answers"): ag and ack have no Unicode case folding (`-i KÖNIG` finds nothing); ag,
ack and git grep's own `-w` use ASCII word boundaries (they report `ERR_PTR` glued to Chinese
text as a whole word); GNU grep needs a UTF-8 locale for Unicode word and case rules; tools other
than ripgrep and Scrutate skip the dotfiles a repository's `.gitignore` re-includes (`!.mailmap`
in the Linux tree).

## 2. Against ast-grep (structural search)

Against ast-grep 0.51.1 on the same question (every call to `kmalloc` in the Linux tree, every
`format!` in ripgrep), both answers checked against a tree-sitter reference. A cache hit, on the
Apple M3 Pro: 943 ms against 13.4 s on the Linux tree (0.070x), 6.4 ms against 39.0 ms on ripgrep (0.163x); in the Linux VM, 662 ms
against 16.3 s on the Linux tree. These runs read scrutate's capture cache (`.scrutate/tsquery/`),
filled by earlier runs, so scrutate parsed no file while ast-grep parsed every file: they compare
the cached tool with the uncached one. A first run, parse for parse (`tsquery --no-cache`), is the
other way round: on a
4-vCPU Linux x86_64 server (`bench/competitors/linux_bench.sh` at 3ff4eb5), scrutate took 56.9 s
against ast-grep's 40.4 s on the Linux tree, 1.41 times as long (95% interval 1.405 to 1.447).
For C, ast-grep needs
`--selector call_expression` and a statement as the pattern: alone, `kmalloc($$$A)` parses as a
type macro and matches nothing, which the first comparison mistook for ast-grep being wrong.

## 3. Against Zoekt and livegrep (indexed search)

On the Linux kernel tree (95,578 files, 1.6 GB) on the development Mac, against Zoekt at commit
`a5a933bc` built with `go install` as its README says (docs/LEADERBOARD.md, Engine B section; raw
data in `bench/results/macos-arm64/superseded/indexed-9aa0.json`). Both tools answered all 20
queries as an independent reference did.

| Measure | Scrutate | Zoekt |
|---|---|---|
| building the index | 5.3 s | 132 s (0.040x) |
| index on disk | 440 MB | 4,238 MB (0.10x) |
| peak memory while building | 1,078 MB | 1,631 MB |
| after editing 100 files | 0.16 s (the text index updates in place) | 129 s (`zoekt-index`, its directory indexer, rebuilds the whole index; the corpus is a plain directory, so its git indexer was left out) |
| query latency, 20 queries x 20 runs, each a fresh process (all PASS) | p50 20.4 ms, p95 57.2 ms, p99 98.8 ms | 54.4 ms, 161 ms, 785 ms |
| memory per query | 12 MB | 277 MB |
| throughput, 11 clients at once | 37 queries per second (1.20x, PASS) | 31 queries per second |

The p95 interval, 0.06 to 0.77, is wide because each tool's slowest query sits right at the 95th
percentile. The index is smaller and quicker to build because it stores trigram postings and no
file contents and reads candidate files from the source tree, which must be present. Zoekt stores
the contents and can answer without the tree. Scrutate offers only the first mode in 0.1.0
(R-14.12).

In the Linux VM, on the same tree and 20 queries with every answer checked, against Zoekt and
livegrep (revision `923d5ad7`, built with Bazel as its README says; its `codesearch` server holds
the index and a minimal gRPC client prints each answer), Scrutate is ahead on all 8 metrics
(`bench/results/linux-aarch64/superseded/indexed-261e.json`).

| Metric | Scrutate | Zoekt | livegrep |
|---|---|---|---|
| Building the index | 4.2 s | 117.4 s | 24.3 s |
| Index on disk | 428 MB | 4,238 MB | 7,781 MB |
| After editing 100 files | 0.12 s | 118.2 s (rebuild) | 25.0 s (rebuild) |
| Latency p50 / p95 / p99 | 11.2 / 40.6 / 54.1 ms | 47.6 / 191 / 887 ms | 73.4 / 281 / 322 ms |
| Throughput, 5 clients | 139 queries/s | 23 | 36 |
| Memory to answer a query | 16 MB | 351 MB | 2,791 MB (the server's resident index included) |

livegrep keeps its index in a long-running server's memory and answers there, which makes its
large queries fast; Scrutate and Zoekt start a process per query and map their index files.

Kept running as servers, both rivals answer faster than Scrutate's process per query. On a 4-CPU
Linux box (AMD EPYC-Milan, 16 GB) with the Linux tree and the 20 queries of `bench/yardstick.py`
W3, livegrep's server answered 16 (HTTP 500 on two common literals, a phrase and a multiple-literal
query) in 12.6 ms median. On those 16, zoekt-webserver took 13.2 ms and `scrutate --index` 17.9 ms;
on all 20, zoekt-webserver took 36 ms and `scrutate --index` 24.3 ms. Scrutate's own server, the MCP
`grep_code` tool, answered 4 of the 20 (two literals and two queries that match nothing) in 1,260 ms
median, where on the same 4 `scrutate --index` took 7.6 ms, zoekt-webserver 1.4 ms and livegrep
2.0 ms. The other 16 have more than the 5,000 lines `grep_code` returns, so they were not timed. In
that run each `grep_code` call walked the whole tree before it searched.

## 4. Against "nothing": the code-intelligence engine

No measured tool has a code-intelligence index, so this part has no competitor:

- **Ranked search by meaning** with a learned ranker: MRR@10 0.635 for natural-language queries
  with a function answer on an undocumented held-out Python code base, against 0.463 for BM25
  over the same candidates; 0.916 to 1.000 for name and identifier queries; held-out Rust, Go,
  Java, JavaScript and TypeScript repositories gain too (docs/QUALITY.md).
- **Symbols from 61 grammars** (74 with the optional grammars), each verified by a sample; call
  graph, imports and dependents, references grouped by symbol, outline, similar code, exact and
  near-duplicate detection, cross-repository search.
- **One interface for people and agents**: JSON everywhere (errors included), an MCP server with 19
  tools, Python bindings with the GIL released.

## 5. The case for adopting it, at the strength the evidence allows

Scrutate replaces a grep-class tool: it was the fastest measured on every benchmark of the suite
on both platforms (section 1), most where its own design sets the time (one large file, heavy
output, a cold disk). It adds identifier and fuzzy search, results grouped by
function, and an index that answers the same greps in a fraction of the time and stays current as
files change. Its ranking quality is measured on held-out code, with a quality floor in CI and
features and scores pinned bit for bit by a test. Its outputs are machine-readable and versioned; hostile inputs, corrupted indexes and
concurrent writers are tested; its dependencies are audited and listed in an SBOM.

What the evidence does not support, and this page does not claim:

- A published "#1": every number comes from one developer laptop shared with other work and a VM
  on it, and section 14 of the mission asks for dedicated idle hardware
  (docs/RELEASE_READINESS.md).
- Results on Windows or Intel Macs, tested on GitHub's runners only, with no benchmark there
  (docs/INSTALL.md).
- Linux results on real hardware: the cold-tree thread count and the read time that counts as a
  disk read were fitted in the VM (docs/ROADMAP.md).
- A lead of half the time everywhere: on macOS a search of a large tree takes 0 to 12% longer than
  merely reading its files, so the margin has little room left there.
- An answer without the source tree: the text index stores no file contents; Zoekt's does.
- Ranking quality beyond the six languages measured (docs/QUALITY.md).
