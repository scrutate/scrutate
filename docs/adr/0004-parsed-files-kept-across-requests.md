# 4. Parsed files are kept across requests

Status: accepted, 2026-11-15.

## Context

The resolver reads and parses each file a request needs once, then drops what it parsed when the
request ends. A process that answers many requests (`serve`, `lsp`, `batch`) parsed the same files
again for every one: `callers` of five names in this repository, asked twice in one `serve`
session, parsed the same files twice. The answer files in `.scrutate/answers/` help only when the
same request comes again; a request about another name in the same files gained nothing.

## Decision

The resolver keeps what parsing a file gives (`resolve::Parsed`: the decoded text, the bytes when
decoding replaced invalid UTF-8, the tree, whether it was too deep, the imports) in one store for
the process (`resolve::PARSED`), keyed by root, path and the content hash of the file's bytes. A
request still reads each file and hashes it: a file whose bytes changed gets another hash, so it
is parsed again, and the store never answers for bytes it did not parse. What the index says of
a file (its definitions) is read for each request, as before, since an update changes it without
changing the file.

The store is bounded by weight, not by count: a file weighs its text, the bytes kept beside it,
and each node of its tree at its grammar's figure (`node_bytes`). The weight is an estimate of
what a kept tree takes, not a count of it. Each figure is the heavier of two measurements, rounded
up: a real corpus, and generated code whose methods are arithmetic over calls, fields and
indexing, the heaviest shape found (in C#, doc comments take 98 bytes a node and such code 168 to
176).
Both are bytes a node beyond the text, measured on Linux with
`tests/parsed_memory_by_grammar.rs` (a store with no cap filled with the files, RSS read before
and after; one run each, and a second of the C#, Rust, Python, Java, TypeScript, Go and C
corpora, which agreed within a byte):

| grammar | figure | real corpus | real | expression-heavy |
|---|---|---|---|---|
| C# | 176 | Newtonsoft.Json 52fa3ae, 951 files | 144.0 | 167.5; 172.6 and 175.5 |
| PHP | 149 | Slim 8d872ae, 125 files | 126.4 | 148.9 |
| Rust | 131 | ripgrep 15.2.0, 100 files | 126.9 | 130.3 |
| Python | 116 | the Python 3.14.4 standard library, 627 files | 115.8 | 111.0 |
| Java | 115 | gson 6c06cbc, 268 files | 86.6 | 114.5 |
| JavaScript | 113 | express 43fca52, 142 files | 102.4 | 112.5 |
| TypeScript | 113 | vuejs/core cbd9595, 486 files | 111.4 | 112.8 |
| TSX | 113 | none | | 112.7 |
| Go | 104 | hugo v0.170.0, 914 files | 91.1 | 103.9 |
| C | 97 | Linux v7.3 `fs/`, 1,479 files | 96.4 | 89.0 |
| C++ | 90 | leveldb 2946558, 76 files | 83.5 | 89.5 |

C# was first set at 168 from the probe. The red test's store of the same kind of code, filled past a
64 MiB cap so the trees it drops stay with the allocator, then measured 172.6 bytes a node (the
store before this figure) and 175.5 (at 168), so C# moved to 176. At these figures every measured
store took 0.58 to 1.00 times its counted weight (the red test's store 1.00, doc-comment C# 0.58);
the quarter the tests allow is for shapes not measured. A grammar not measured counts at the
heaviest figure, 176, which over-counts a light grammar by up to about twice: its store holds fewer
parses when a request outweighs the cap, the price of keeping the memory bound where nothing was
measured. Go counted at 168, the default before C# moved, where it measures 104, `refs --json
--max-results 2000 Error` over eight copies of hugo v0.170.0 (7,312 files) took 2.67 and 2.58 s at
274 MB, against 2.61 and 2.64 s at 345 MB, with the same answer. Python counted at 116 where it was
110, `refs __init__` over six copies of its standard library and a virtualenv (4,205 files) took
100.7 and 100.6 s at 541 MB against 98.6 and 98.9 s at 563 MB, the same answer (box 1, a 16-vCPU AMD
EPYC 9R14, runs interleaved). Measure a grammar again when its version changes, with `PROBE_DIR=<a
tree> PROBE_EXT=<extension> cargo test --release -p scrutate --test parsed_memory_by_grammar --
--ignored --nocapture`. A tree takes 18 to 46 times its text, so counting it as a multiple of the
text, as the first version did at eight times, let a full store hold two to six times its cap. Past
256 MiB (`PARSED_CAP`) the least recently used files go first, and a file that alone weighs more is
not kept (its older parse is dropped). The cap is stated in [CLI](../CLI.md).

## Consequences

- A second request about any name parses no file the first one parsed, until a file changes or
  is evicted to keep the store under its cap
  (`resolve::tests::a_repeat_request_parses_no_file_it_parsed_before`).
- A process holds up to the cap more than before, within a quarter
  (`tests/parsed_memory.rs` and, for the heaviest C#, `tests/parsed_memory_by_grammar.rs`
  assert it on Linux); a one-shot command (`scrutate callers` from the
  shell) pays only the hash of each file it reads.
- The other parse sites (index update, `tsquery`, `dead`) keep their own reads. Moving them onto
  this store is a later step, and its rule stays: a parsed file is found by its content.

## Not done

- The grammars the resolver does not read keep no tree in the store, so none of them is measured;
  a grammar the resolver comes to read is measured before its figure leaves 176.
- Counting a tree's bytes exactly, through tree-sitter's allocator hook, in place of the estimate.
