# 2. Reads during a write

Status: accepted, 2026-11-15.

## Context

A session's reads and writes share one writer lock inside the process. A write (the first build,
a watch burst's update, a save) can itself wait up to 10 minutes for another process's SQLite
lock; the scan before another read takes the same writer lock in the process, and so waits for
those. Before this decision the front ends disagreed on what a read did
then:

- an MCP tool call, a language server request and a Python call skipped the scan and answered
  from the last committed index, and said nothing;
- a command line call a `serve` answers was refused whenever a write ran, the first build ran,
  another call's scan held it 50 ms, or every reader was busy 50 ms, and the command did the work
  itself;
- a read that had to open a new reader waited for the whole write, because readers were opened
  from the repository the writes hold.

So a reader was sometimes refused, sometimes blocked for the length of a write, and sometimes
answered with old data without a word.

## Decision

One contract for every front end:

- An update stays one transaction. A reader sees the index before or after it, never half of it
  (WAL, the segment manifest swapped in the same transaction).
- A read never waits for a lock another process holds, except while another process makes the
  first index and nothing is committed: then it tries again for up to 20 s
  (`session::BUILD_WAIT`).
- The scan before a read waits up to 250 ms (`stores::OWN_WRITE_WAIT`) for a write of this
  process. A write that covers the read's change set and ends within that time is waited for, and
  the read then applies what is left of its own change set.
- Past the wait, the read answers from the last committed index with a pending set
  (`session::Pending`): the files of its change set the answer does not hold yet, or every file
  when a walk could not run. The token does not move, so the next read applies them.
- The answer says so, in each front end's own form: a line at the end of the MCP text and
  `pending` in `structuredContent`, a `window/logMessage` warning in the language server, a
  Python `UserWarning`, a stderr line for a command a `serve` answers. Never silently stale.
- A command line call a `serve` answers is not refused for a running write. It is refused only
  while a first build runs with nothing committed (or when the session followed its root's link
  to another tree, or the call failed), and the command then does the work itself.
- Readers are opened from a repository no write holds (`Session::opener`).

The bound: a watch burst's update takes 3 to 4 ms on Linux and about 150 ms on a loaded Mac
(measured on scrutate-build-2 and on an M-series Mac with other builds running), so 250 ms covers
the common write and keeps a read's worst case near a quarter of a second. A write held up by
another process's lock is the case the bound exists for.

The other choice was to commit an update per batch of files, so a reader waits at most one batch.
That makes half an update visible, which breaks the before-or-after rule the tests and the
documentation promise.

## Consequences

- A read's worst case during this process's own long write is 250 ms plus its own work, with a
  note naming what it lacks.
- Callers that only need the answer use `Session::run` and `Session::read`; the front ends use
  `run_noted` (the MCP server, the Python bindings), `read_pending` (the language server) and
  `run_scanned_for` (a command a `serve` answers), which return the pending set.
- Every session holds one more SQLite connection, the one readers are opened from.
