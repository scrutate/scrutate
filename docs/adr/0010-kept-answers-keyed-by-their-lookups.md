# 10. Kept answers keyed by their index lookups: measured, not taken

Status: not taken, 2026-11-16. The idea was not disproved. It was not shown to pay inside one
measured round, and the rule for architecture items is that such an item closes with its numbers.

## Context

`callers`, `callees` and `dead` keep what they resolved (`answers.rs`, `.scrutate/answers/`). A
kept file holds the index database's data version in its key, so any update of the index, an
edit to any file, drops every kept answer, and the next request resolves everything again.

After the import aliases of a request's targets were kept too, a repeat on an unchanged tree
costs 1 to 4 ms. What was left to win was the repeat after an edit to a file the request never
reads, which today is a fresh run. Measured with no new code on the scrutate tree (AWS build box
1, 16 cores, `serve`, each request first, kept, and after an edit to a file none of them reads):

| Request | First (ms) | Kept (ms) | After the edit (ms) |
|---|---|---|---|
| `callers redact` | 225 | 3.5 | 204 |
| `callers spec_for` | 218 | 1.9 | 209 |
| `callers repo_bytes` | 175 | 1.4 | 151 |
| `callers lang_of` | 154 | 1.2 | 151 |
| `callers content_hash` | 77 | 2.0 | 72 |
| `callers split_line` | 42 | 1.9 | 41 |
| `callees redact` | 164 | 163 | 166 |
| `dead_code crates/scrutate/src` | 1165 | 71 | 1047 |

The bar set before building: after an edit at least 3 times the kept time, and over 50 ms gained
per call. `callers` met it. `callees` did not: its kept answer is no faster than a fresh run,
because its time is not resolution. `dead` reads every file, so any edit changes a stat it read,
and no key on its lookups could keep it. Only `callers` was tried.

## What was tried

`callers` was keyed by what it asked the index, in place of the data version:

- Each of the 28 lookup methods of `Index` reached from `callers` (from `files` and `named` to
  `callers_of`, `imports_holding`, `symbols_in` and `symbol`) became a public wrapper over a
  private `NAME_raw` that, while a request's `Reading` is open, logs the operation, its arguments
  and a digest of its answer (the `Debug` of the value, in the order it was returned; maps by key).
  Recording at the source catches every path to the index that goes through an `Index` method:
  `Names`, the resolver, `Searcher::find_symbol` and the helpers that take an `&Index`. A new method
  added without the wrapper escapes it, which is what the guard in item 2 below is for.
- The kept file holds the log after the answers. A later run asks each logged lookup again, in log
  order, inside the same read snapshot as the answer it returns, and stops at the first that
  answers otherwise. The stat of every file the run read is checked as before.
- Past 20,000 lookups, or after a lookup that failed, the file falls back to the data-version key.

## Result

Both sides on AWS build box 1, 16 cores, load 7.2 to 8.2, three runs of each build alternated on
fresh copies of the scrutate tree, after an edit to `bench/corpora.py`, which no request reads:

| Request | Kept, before (ms) | Kept, after (ms) | After the edit, before (ms) | After the edit, after (ms) |
|---|---|---|---|---|
| `callers redact` | 3.5 | 7.9 to 8.5 | 206 to 210 | 206 to 213 |
| `callers spec_for` | 1.8 to 1.9 | 6.9 to 7.5 | 209 to 215 | 212 to 218 |
| `callers repo_bytes` | 1.3 to 1.4 | 4.2 to 4.6 | 154 to 155 | 154 to 159 |
| `callers decode_source` | 2.7 | 4.7 to 5.2 | 156 to 166 | 159 to 230 |
| `callers lang_of` | 1.1 to 1.2 | 4.3 to 4.9 | 152 to 155 | 153 to 154 |

The answers were byte for byte the same in every run. No gain after the edit, and every kept
repeat cost 3 to 5 ms more, the time to ask the log again.

## Why it missed

Every miss was at the first lookup of the log, `files()`: the snapshot of the indexed paths the
resolver takes when it starts. Its query has no `ORDER BY`. Indexing the edited file again writes
its row anew, which moves it in the answer, so the digest, taken in the order the rows came,
changed, although the resolver holds the list as a set and the order never mattered. Other
lookups on the path have no `ORDER BY` either: `named`, `named_any_case`, `texts_of_kinds` and the
vectors inside `callees_by_lowercase`. Some of their consumers read the raw order (the C and Rust
resolvers take the first match of `named`), so their digests must stay in order, and whether a
reindex of an unrelated file moves them was not measured.

## What a second try needs first

1. Digests that ignore order for answers their consumers use as sets (`files`, `texts_of_kinds`),
   or an `ORDER BY` on those queries; for each lookup without one whose consumer reads the order,
   proof that an edit to another file cannot move its rows. The digests carry row ids too: show
   that an update and a compaction of the index leave the ids of other files' rows as they were,
   or digest rows by path, lines and name instead of id; else fixing the order only moves the miss
   to the first lookup that names a row by id. Only then measure again, on one box, both sides,
   against the table above, with the cost of a miss beside the cost of a hit: on a miss a run pays
   the re-asks up to the first that differs and then a whole run, so measure an edit that does
   touch a lookup, and report how often `dead`, or a large `callers`, runs past `MAX_LOOKUPS`.
2. The guard, as the reviewer set it:
   - a test that lists every function of every `impl Index` in the crate, at any visibility,
     against a list of recorded lookups and a list of writes and upkeep, so a new method fails
     until it is classified; and fails on any field of `Index` that hands out the connection or a
     statement;
   - no recorded method returns a handle that reads later (an iterator, a statement, a cursor);
   - the `callers` path stays on one thread: a check that names.rs, resolve/, search.rs and the
     targets code use no `par_iter`, `rayon::` or `thread::spawn`, since the log is thread-local
     (failing closed on a lookup from a thread with no log would misfire on `serve`'s concurrent
     calls, which each have their own);
   - every type a recorded method returns derives `Debug` over all its fields, or the digest uses
     an encoding of its own;
   - the re-asks, the comparison and the returned answer read one snapshot, as `ops::run` gives.
3. Every disk probe on the path goes through `answers::noted`, absent files included. On the
   `callers` path all of them do (the resolver's file loads, `detect_at` for `.m`, `.h`, `.pp` and
   `.cls`, `go.mod`, `package.json`, `tsconfig.json` and `jsconfig.json`, the `Cargo.toml` beside a
   `lib.rs`); the resolver's other "is there a file" checks test the `files()` snapshot, not the
   disk. A test that fails on a new `std::fs`, `exists`, `is_file` or `metadata` call there keeps it
   so.
4. A freshness property test that proves the guards work: random edits to related and unrelated
   files, every kept answer equal to a run with nothing kept. Its cases include a new importer
   under an alias, a new `__init__.py` or other package marker, a `.h` whose text turns it from C
   to C++, a rename, a definition moved inside an unrelated file (row ids may move: a miss is
   allowed, a wrong answer never), a delete, and a change of a custom language's configuration.
   One mutation per guard (one `looked` call left out, one `noted` probe left out) must make it
   fail.
5. A file written again must keep what its old answers rest on. In the version tried, a run that
   reused a kept file and then added one new alias set wrote the old alias sets again under a log
   of its own lookups only, so a later edit to a lookup behind an old set would not have dropped
   it. The fix, kept on the branch, carries the stored file's lookups and file stats into the new
   one; a test writes a file again with nothing asked, edits an importer of the old set, and the
   next open must not find it. No cache that outlives one request may sit on the path unless a hit
   logs the lookups behind it (`Searcher::callers_cache` feeds only the ranking, not `callers`).

## Observed on the way

`dead` reads every file of the tree inside a parallel walk (`dead.rs`, `every_file`, `read_regular`,
the `Cargo.toml` `[lib]` check and `detect_at` there), and the record of what a request read is
kept per thread, so none of those reads is noted. With the data version in the key that is safe,
since an edit to an indexed file moves it. No stale `dead` answer was shown: it keeps only the
resolver's answer per call site and counts the mentions in files the resolver does not read anew
on every run. Any key of `dead` without the data version would have to record those reads first.

## Consequences

- Kept answers keep the data version in their key; an edit anywhere still drops them.
- The branch `audit/dev8-arch06` holds the version tried, the carry-over fix and a first step of
  item 1 above (set lookups digested sorted), unshipped.
