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
and 110 bytes for each node of its tree (`NODE_BYTES`). That is what a kept file takes beyond its
text, measured on Linux by `tests/parsed_memory.rs` (a store filled past a 96 MiB cap, RSS read
before and after, three runs each): 88 bytes a node for C, 104 to 106 for this repository's Rust,
whose imports add more, and 107 to 109 for both together, where the freed trees of evicted files
stay with the allocator. A tree takes 18 to 46 times its text, so counting it as a multiple of
the text, as the first version did at eight times, let a full store hold two to six times its
cap. Past 256 MiB (`PARSED_CAP`) the least recently used files go first, and a file that alone
weighs more is not kept (its older parse is dropped). The cap is stated in [CLI](../CLI.md).

## Consequences

- A second request about any name parses no file the first one parsed, until a file changes or
  is evicted to keep the store under its cap
  (`resolve::tests::a_repeat_request_parses_no_file_it_parsed_before`).
- A process holds up to the cap more than before, within a quarter
  (`tests/parsed_memory.rs` asserts it on Linux); a one-shot command (`scrutate callers` from the
  shell) pays only the hash of each file it reads.
- The other parse sites (index update, `tsquery`, `dead`) keep their own reads. Moving them onto
  this store is a later step, and its rule stays: a parsed file is found by its content.
