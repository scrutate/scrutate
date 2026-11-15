# 11. One read per changed file for both indexes: not taken

Status: rejected after measurement, 2026-11-16.

## Context

An update that keeps both stores (`index`, `watch`, `reindex`) brings the code index up to date
first, then the text index, each under its own lock, and each reads every changed file itself: the
code index to hash and parse it, the text index to take its trigrams. `grep --index --update` keeps
the text index alone, so it reads each file once already.

An update of every file takes one look at the tree (`files::look`), and the text index records the
stat that look took. A watch burst names its files, and the text index takes their stat itself,
after the code index's update.

## What was tried

The code index kept the bytes it read, by path, with the stat it took before each read, at most
64 MiB in all; the text index took a file's bytes from there when the stat it records was the
same, and read the file itself otherwise. No schema or format change.

Measured on a copy of the Linux tree (95,952 files) on a 16-core box under other jobs (load 11 to
31), before and after, the same edits on both:

| | before | after |
|---|---|---|
| opens of a changed file, 1 and 100 changed | 2 | 1 |
| opens of a changed file, 10,000 changed | 2 | 1 for 5,669, 2 for 4,331 (the cap) |
| `openat` of tree files, 10,000 changed | 28,395 | 22,726 |
| `read` calls, 10,000 changed | 41,671 | 30,333 |
| user CPU, 10,000 changed, median of 5 runs taking turns | 96.6 s | 100.4 s |
| system CPU, same | 11.05 s | 10.98 s |
| peak RSS, same | 2,013,940 KiB | 2,075,816 KiB (+60 MiB) |

Both indexes came out equal: the code index's tables hashed the same, and the text index gave the
same answers as a walk. There was no time to win: the second read came from the page cache, so
the saving was syscalls only.

## Decision

Not taken. Each index keeps reading the files it updates.

- What the shared read makes consistent repairs itself already. A write between the two reads
  changes the file's size, mtime or ctime, so the stat recorded by whichever index read before the
  write no longer matches the disk, and the next update reads the file again for that index.
- The one case the next update does not repair would get worse. A write that leaves size, mtime
  and ctime unchanged (inside one timestamp tick) between the code index's read and the text
  index's would leave the text index with the code index's older bytes, where today it reads the
  newer ones.
- The cost is 64 MiB more memory at the peak of an update and a seam between the two indexes, for
  no measured time.

## Consequences

The two reads stay. Revisit only if a measurement shows the second read costs time, such as a
tree larger than the page cache.
