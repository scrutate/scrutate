# 12. A fresh process looks at the tree on every core

Status: accepted, 2026-11-16.

## Context

Every command that reads the code index starts by bringing it up to date: it looks at every
file of the tree (`files::look`, one walk and one `stat` per file) and compares each file's stat
with the one the index recorded. On the Linux tree at v7.3 (95,951 files in the look) that
look was most of a warm `symbol`, `callers` or `search`: the answer itself takes tens of
milliseconds, the look and the comparison most of a second. Three things made it slow:

- the walk ran on one thread, so each directory's `stat` calls waited for the one before;
- the update read the index's record of every file (`SELECT path, size, mtime, ctime, hash
  FROM files`) only after the walk ended, though neither needs the other;
- that read took each file's content hash, about 40 bytes per row, though a hash is only
  wanted for a file whose stat changed.

## Decision

- `files::look` walks with `ignore`'s parallel walker on `rayon::current_num_threads()`
  threads. Each thread keeps the files it found in a list of its own and adds it to the shared
  list when it ends; the shared list is sorted by path at the end, so the look's answer is the
  same as the one-thread walk's.
- `Index::update_with` runs the walk on a scoped thread and reads the recorded stats on its
  own at the same time. The walk shares nothing with the database connection.
- The recorded stats are read without the hash. A file whose stat differs from its record has
  its hash read on its own (`SELECT hash FROM files WHERE path=?1`), and only then.

## Consequences

Measured on the AWS build box 1 (16 cores) on the Linux tree at v7.3: the commit before this
change and this change's code, built in one lane, six rounds of each command, the two binaries
in turn (ABBA), each run a fresh process. "Cold" drops every file's cached pages with
`posix_fadvise(DONTNEED)` before the run, which leaves the directory caches warm. Medians, in
milliseconds:

| Command | Warm, before | Warm, after | Cold, before | Cold, after |
|---|---|---|---|---|
| `symbol kmalloc` | 843 | 314 | 17,889 | 15,914 |
| `callers kmalloc` | 926 | 329 | 24,766 | 25,513 |
| `search kmalloc` | 904 | 483 | 19,491 | 20,028 |

- A warm command on a large tree answers 0.4 to 0.6 s sooner, 2 to 3 times as fast.
- A cold command does not change in a way these runs can show. The medians moved by -2.0 s
  (-11%), +0.75 s (+3%) and +0.54 s (+3%), while each command's single runs spread over 12.1 to
  33.2 s (`symbol`), 21.8 to 53.4 s (`callers`) and 16.4 to 53.0 s (`search`): it waits on reading
  the files from disk, and the box's load (up to 37 during the runs) moved single runs by tens of
  seconds.
- On few cores it still gains. The same binaries pinned with `taskset` to the first 1, 2 and 4
  cores of that box (warm, ABBA, six rounds), medians in milliseconds:

  | Cores | `symbol`, before | after | `search`, before | after |
  |---|---|---|---|---|
  | 1 | 821 | 743 | 890 | 808 |
  | 2 | 824 | 478 | 882 | 560 |
  | 4 | 818 | 385 | 866 | 410 |

  On one core the walk has one thread, as before, and the 10% comes from the hashes no longer
  read and the table read beside the walk.
- A look now takes as many threads as the process's pool. A command already runs its searches
  on that pool, and the look ends before they start.
- A file whose stat changed costs one more indexed query for its hash; an update of a few
  changed files reads a few rows, not every row's hash.

## Rejected

- Keeping a look between commands (a file of stats, or a daemon). A command that keeps no look
  answers with a file written just before it, and `serve` already keeps one for its own calls
  (`files::KeptLook`).
