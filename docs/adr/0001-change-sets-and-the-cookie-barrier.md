# 1. Change sets and the cookie barrier

Status: accepted, 2026-11-14.

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
  event (Watchman's sync queries work the same way). Events arrive in order on each platform's
  queue, so once the cookie's event is in, so is every event of a change made before the read
  started. On macOS the read also calls `FSEventStreamFlushSync` and the stream's latency is
  5 ms, to cut the wait. One code path on every platform; the cookie is the proof.
- The events since T go through the rules a watch burst already uses (`Burst::note`,
  `Burst::files`), so there is no second copy of the membership rules.
- Whatever those rules cannot vouch for is every file: lost events, a watcher error, an ignore or
  config file, a directory that came or went, a directory without a watch, a rule file outside
  every watch that changed, watches set again on Linux, more than 10,000 events since the last
  read, a cookie that took over a second. `SCRUTATE_SESSION_DEBUG=1` names the reason.
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
- Every session of a root sees every cookie's event, so a cookie's name holds its process id and
  a number per session, and a session takes only the events of the cookies it still waits for.
- A tree changing faster than reads come in falls back to a walk after 10,000 events, which is
  what every read cost before.
- The test `session::tests::a_file_written_just_before_a_call_is_in_its_answer_every_time` writes
  a file and reads it back 1,000 times through the watch, with no miss and one file looked at
  each time.
