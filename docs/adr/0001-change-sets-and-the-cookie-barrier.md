# 1. Change sets and the cookie barrier

Status: accepted, 2026-11-14; amended 2026-11-15 for macOS (see the end).

## Context

Every indexed read in a session started with a walk of the tree and a `stat` of every file
(`Repo::refresh`), so a call cost O(files) before it did any work of its own. Seven places walked
the tree, each with its own copy of the membership rules. A running watcher already knew which
files changed, and the read path never asked it.

A read must still see every write that finished before it started: that is the freshness rule
the documentation promises. A watcher's events arrive late: the kernel queues them, a thread reads
them, FSEvents holds them for its latency, and a burst waits for quiet before it updates. Asking
the watcher what it has seen so far is not enough.

## Decision

One module answers "what changed since token T": `fresh::Freshness::changes_since(T)`, with
`files()` for the full membership. The answer is a change set (`stores::Changes`): the files
named, or every file.

- The walk adapter (`fresh::Walk`) has no watcher and always says every file. A session without a
  watch, and a fresh process, use it.
- The watch's adapter (`watch::Notes`) notes each event as it arrives, on a thread of its own, so
  no debounce and no running update holds a read up.
- The barrier: a read writes a cookie file under `.scrutate/cookies/` and waits for that file's
  event (Watchman's sync queries work the same way). Events arrive in order on inotify's and
  ReadDirectoryChangesW's queue, so once the cookie's event is in, so is every event of a change
  made before the read started. Not on macOS: see the amendment below.
- The events since T go through the rules a watch burst already uses (`Burst::note`,
  `Burst::files`), so there is no second copy of the membership rules.
- Whatever those rules cannot vouch for is every file: lost events, a watcher error, an ignore or
  config file, a directory that came or went, a directory without a watch, a rule file outside
  every watch that changed, a directory's first watch on Linux (new, or made again at its path),
  more than 10,000 events since the last read, a cookie that took over a second. `SCRUTATE_SESSION_DEBUG=1` names the reason.
- Every session the MCP server, the language server and the Python bindings open runs a watch,
  on a local disk only (`watch::local`: Linux `statfs` network types, macOS `MNT_LOCAL`, Windows
  `GetDriveTypeW`); inotify(7) reports no event for a change made over a network. A tree on a
  network disk, a watch that cannot start, and the wasi build leave the walk
  (`fresh::for_session` chooses).
- A safety walk off the read path finds the changes no event reports (a write through a memory
  map, a bind mount that does not pass events on): every 60 s, or 50 times its own cost when that
  is longer, it compares a look at the tree with its last one and feeds the difference in as
  events. Such a change shows within that bound.

The other choice was to read inotify and ReadDirectoryChangesW ourselves until they have nothing
queued. The `notify` crate owns those handles, so that meant replacing its backends on two
platforms, for the same guarantee the cookie gives.

## Consequences

- A read whose change set is empty costs the barrier and no lock. A read that names files costs
  one `stat` per named file (`UpdateStats::files_looked`).
- The barrier is one file create and delete per read, plus one event's trip through the OS. The
  macOS FSEvents path is the slowest of the three.
- The cookie directory gets its own watch on Linux; elsewhere the root's watch covers it.
- Every session of a root sees every cookie's event, so a cookie's name holds its process id,
  a number per session and 64 random bits (two containers sharing a bind-mounted root can both
  be pid 1), and a session takes only the events of the cookies it still waits for.
- A tree changing faster than reads come in falls back to a walk after 10,000 events, which is
  what every read cost before.
- The test `session::tests::a_file_written_just_before_a_call_is_in_its_answer_every_time` writes
  a file and reads it back 1,000 times through the watch, with no miss and one file looked at
  each time.

## Amendment, 2026-11-15: macOS reads look at the tree

FSEvents does not keep that order. A read missed a write made just before it under load
(`a_file_written_just_before_a_call_is_in_its_answer_every_time`, once in a full parallel test run;
reproduced with three copies, four busy CPUs and two file-event floods): the cookie's event came
back 338 us after it was written, no event for the written file came before it, and the change
set was empty. A C probe (`bench/event_order.c`) writes a file, then makes a cookie, flushes and
waits for the cookie's event, then 30 ms more. On macOS the cookie came first 2 times in 300 for
an in-place write (`open(O_TRUNC)`, write, close) at load 5, and 2 in 300, 0 in 400 and 1 in 2,000
under file-event floods at load 6 to 10; for a write by rename 0 in 300 at rest, and 1 in 300,
1 in 400 and 1 in 2,000 under the floods. Each time the file's event came later, with a later
event id than the cookie's (cookie 1531117780, file 1531117940; cookie 1537168847, file
1537168898). No wait for the cookie, flush or stream latency can close that: the write's event
is numbered after the cookie's. Watchman met the same reordering ("Cookie files are not
sufficient", commit d1795de4ec, 2021) and added a flush after the cookie is seen, which does not
help when the write's event is numbered later.

So on macOS (`watch::ORDER_PROVES` false, held per `Notes`) a read writes no cookie and its change
set is every file: the read looks at the tree, at once, and indexes what changed. Each read takes
a token of its own, so a walk counts for a read only when it began after the read did. The cookie
code stays for the platforms whose queue keeps the order: on Linux (inotify, one queue for every
watch of an instance, kernel 7.0, 4 vCPU) the same probe under its flood saw the cookie first 0
times in 5,000 in-place writes and 0 in 5,000 renames. Windows is not tested for it.

The cost, on an Apple M3 Pro (11 cores, macOS 27.2.1) at load 6 to 9, both builds alternating call
by call (`rivals.scrutate.Mcp`, the yardstick's resident row, `find_symbol`): on a 726-file copy of
this repository 3.9 ms median before, 8.2 ms after (a call right after an edit 26.0 ms before, 29.3
ms after); on 14,520 files (20 copies) 15.0 ms before, 130.8 ms after (after an edit 206 ms, 314
ms). The look grows with the tree. A command line call through `serve` already looked at every file
and is unchanged (`symbol` through it: 10.2 and 15.0 ms before, 9.8 and 12.4 ms after, two rounds
of 15 calls each). A barrier cheaper than the look that is still a proof is the open item
(ARCH-01m).
