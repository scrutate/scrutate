# 7. tsquery answers from structural facts the index stores: not taken

Status: rejected after measurement, 2026-11-16.

## Context

`tsquery` parses every file it is asked about. The capture cache (`.scrutate/tsquery/`) is keyed
by the query's text, so a new query parses everything again, and the literal gate only skips
files that hold none of a query's strings. Parsing is the cost: one thread over Linux's 64,436 C
and header files takes 190 s. The code index parses the same files at index time and keeps
nothing of the tree.

The budget for keeping something: the default index grows by at most 10% in size and in index
time on the Linux tree, and a query of the subset below gets clearly faster.

## What was tried

The index kept each parsed file's tree as structural facts, and `tsquery` matched a query of a
subset against them instead of parsing.

- The records: per node in preorder its kind id, field id, start (from its parent's start),
  length and child count, each a varint. Only named nodes, plus anonymous ones with children or
  a field: tree-sitter's query cursor never matches the rest for a pattern of named kinds.
- The store: table `facts(path, hash, lang, grammar, data)` in `index.db`, written when the index
  parses a file and deleted with the file's rows (schema 18). A row was read only when its
  content hash was the index's current one for the file, its language the query's, and its
  grammar id (a hash of the grammar's kind and field names and the record layout) the running
  binary's.
- The subset: one pattern of named kinds, fields and captures, with `#eq?` and `#any-of?` on
  captures. Anything else was parsed, and so was a file where a step's field is on two children
  of one node (tree-sitter decides those from the grammar's productions, which the records do not
  hold).
- Equality: over every sample file, 3,839 random queries of the subset built from the file's own
  nodes, children and fields matched as tree-sitter does, 17 files left to it for a shared field;
  over an indexed copy of the samples, 512 queries answered through `tsquery` the same from facts
  as from parsing, 480 files from facts.

Two variants, against the same build without facts: the records stored as they are, and the
records deflated at level 1 (`flate2`, `Compression::fast`).

## Numbers

`index --force --no-text` with `SCRUTATE_SEMANTIC=none` on a copy of the Linux tree (95,902
files), scrutate-build-2 (m7a.4xlarge, 16 cores) under other jobs (load 9 to 26), the three
builds taking turns, three runs each, medians. Then right after each index, `tsquery c` with a
query text the capture cache had not seen, so every file was answered from facts or parsed.

| | no facts | facts | facts, deflated |
|---|---|---|---|
| `index.db` | 4,141,187,072 B | 5,005,570,048 B (+20.9%) | 4,591,624,192 B used (+10.9%) |
| facts rows and bytes | | 82,180, 791,641,332 B | 82,180, 288,054,389 B |
| index, user CPU | 565.95 s | 587.64 s (+3.8%) | 752.14 s (+32.9%) |
| index, wall (load-bound) | 345.61 s | 420.41 s | 1,150.40 s |
| `kmalloc` call: wall, user CPU | 3.44 s, 24.96 s | 1.50 s, 3.63 s | 1.99 s, 4.10 s |
| `goto` label: wall, user CPU | 14.31 s, 128.29 s | 3.37 s, 8.65 s | 3.58 s, 11.42 s |

The deflated index's file was 5,006,069,760 B: `index --force` kept the pages the run before it
had freed, so its row is the pages in use (1,222,185 of 4,096 B less 101,183 free). The three
builds gave the same matches (2,706 and 207,736 lines, the same sorted output). Wall
time moved with the load from other jobs, user CPU far less, so index time is judged by user CPU.

## Decision

Not taken. Neither variant is inside the budget.

- Facts as they are: index time is inside it (+3.8% user CPU), the index is not (+20.9%).
- Facts deflated: the index is just over it (+10.9%), and index time is well over it (+32.9%
  user CPU).
- The gain is real: with the facts stored as they are, a structural query the capture cache has
  not seen goes from 14.31 s to 3.37 s wall on the Linux tree (`goto`) and from 3.44 s to 1.50 s
  (`kmalloc`), for 864 MB more index. A repeated query gains nothing, since the capture cache
  answers it already.

Not built, so not measured, by the rule that an item outside its budget after one measured round
closes with its numbers:

- zstd with a dictionary shared across files: smaller than deflate, at a compression cost of the
  same kind as the one that put deflate over the time budget.
- Facts built on the first structural query and kept: an index that is never asked pays
  nothing, and the first query pays a parse of every file it reaches (the "no facts" column) plus
  the writing of the records.
- A narrower fact set, such as the kinds a given grammar's common queries name.

## Consequences

`tsquery` keeps parsing what the capture cache does not hold, and the schema and the index stay
as they are. Revisit with a variant that leaves the default index untouched (built on the first
structural query) or one that stores less than a tenth of the index.
