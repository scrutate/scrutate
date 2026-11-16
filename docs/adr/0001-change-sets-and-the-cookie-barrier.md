# 1. Change sets and the cookie barrier

Status: accepted, 2026-11-14; amended 2026-11-15 and 2026-11-16 for macOS (see the end).

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
  one `stat` per named file (`UpdateStats::files_looked`). On macOS see the amendments below: a
  read there costs a kqueue drain and the same `stat` per named file while the kqueue is in
  place, and a look at the tree while it is not.
- The barrier is one file create and delete per read, plus one event's trip through the OS.
  Replaced on macOS by the amendments below, which write no cookie there.
- The cookie directory gets its own watch on Linux; elsewhere the root's watch covers it.
- Every session of a root sees every cookie's event, so a cookie's name holds its process id,
  a number per session and 64 random bits (two containers sharing a bind-mounted root can both
  be pid 1), and a session takes only the events of the cookies it still waits for.
- A tree changing faster than reads come in falls back to a walk after 10,000 events, which is
  what every read cost before.
- The test `session::tests::a_file_written_just_before_a_call_is_in_its_answer_every_time` writes
  a file and reads it back 1,000 times through the watch, with no miss and one file looked at
  each time. On macOS it runs through the kqueue (second amendment below), with one file looked
  at each time too.

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

So on macOS (`watch::ORDER_PROVES` false, held per `Notes`) a read writes no cookie and, until the
kqueue of the next amendment is in place, its change set is every file: the read looks at the tree, at once, and indexes what changed. Each read takes
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
of 15 calls each). A barrier cheaper than the look that is still a proof was the open item
(ARCH-01m); the amendment below closes it.

## Amendment, 2026-11-16: a kqueue proves macOS reads

The look costs a read O(files) on macOS (115 ms at 14,520 files above). The proof that costs less
is the kernel's own order. XNU posts a vnode's kqueue note inside the syscall that changes it, before
the syscall returns: `VNOP_WRITE` calls `post_event_if_success(vp, _err, NOTE_WRITE)` after the file
system's write, `VNOP_CREATE` and `VNOP_REMOVE` post `NOTE_WRITE` on the directory, `VNOP_RENAME`
posts on both directories, `NOTE_DELETE` on a replaced target and `NOTE_RENAME` on the moved vnode,
`VNOP_SETATTR` posts `NOTE_ATTRIB` (bsd/vfs/kpi_vfs.c), and `lock_vnode_and_post` runs `KNOTE` on
the vnode's list under the vnode lock (bsd/vfs/vfs_subr.c), which activates the note on its kqueue.
A read that starts after a write returned takes the kqueue's lock in `kevent` and finds that note:
the order is the kernel's, not a daemon's.

So on macOS `watch::Notes` keeps a kqueue (`kq.rs`) with an `EVFILT_VNODE` watch, each opened
`O_EVTONLY`, on every directory the walk enters inside the root and on each file it lists there or
that sets rules (`kq_keeps`: the walk's ignore rules, not its size or language limits); an ignored
file takes no descriptor, and a directory the walk has not entered yet keeps all its files. A read drains it with a zero timeout,
no cookie and no wait, and notes what it names (a file written or replaced, a file made or removed
in a watched directory) as events; the change set follows by the rules above. FSEvents' events then
feed only the watch's bursts.

- Watch, then look. The watches are set on the watch's thread whenever the watches are set (the
  same directories by path and inode keep the kqueue they have), and on a thread of their own when
  a read's drain finds a directory that came, went or moved, or a rule file that changed, which
  builds again even over the same directories, since which files the walk lists changed with it. A directory is watched before it is
  listed, and listed until two listings agree; a file is watched before it is named. Reads look at
  the tree until the first kqueue is in place, and the first read after that looks too, whatever
  the refresh interval, so a file changed before its watch existed is in that look, which starts
  after every watch does.
- No gap on a rebuild. A new kqueue is built beside the one in place, which keeps watching until
  the swap and is drained into the notes at it: a change made while both watched is in one queue
  or the other, so a file the watch knew is never missed. When the budget cannot hold both, the one
  in place is dropped first and reads look until the new one is in. Only the new directories want a walk
  then, which a refresh interval the user gave may hold back, as on Linux. A directory that came or
  went is noted as such for the burst rules, with the other changes of the same drain. A directory
  made in a parent before the build watched that parent is in the parent's listing with no watch
  of its own, and no later note names it, so each build lists the tree's directories again once
  its watches are set: when they changed, reads keep looking and another build follows, at most 4
  in a row (`KQ_BUILDS`) while they keep changing. A
  directory no walk enters (an ignored one) changes no listing, so it asks for no build.
- A listing keeps each name with its kind (directory, file, or a link, socket or pipe): a name
  that keeps its place and changes kind, as a link that `git checkout` turns into a file or a
  directory, is a new file or a new directory to the next drain, and its old watch is dropped so
  what is there now is watched.
- The budget: one for the process, shared by every kqueue in it (two sessions in one Python
  process, a rebuild beside the kqueue in place): at most half of `RLIMIT_NOFILE` (raised to the
  process ceiling, `kern.maxfilesperproc`, as Go's runtime does at start) less 1,024 kept for the
  index and the rest of the process. A tree with more files and directories, a directory that
  changes so fast that 8 listings never agree (`Off::Churn`), or any other refused watch (`EMFILE`,
  `ENFILE` or another error) drops every watch: reads take the look, never a part of a tree that
  claims proof. A file or directory the user cannot read (`EACCES`, `EPERM`) gets no watch and
  leaves the rest in place: each drain tries up to 32 of them again, in turn, and names one once it
  opens (a directory with its files); the walk cannot read them either.
- Descriptors, where this design can fail quietly: one open descriptor per file and directory of
  the walk, for as long as the session runs (16,639 for the 14,520-file tree below; a 100,000-file
  tree needs about 110,000). Raising the soft limit to `kern.maxfilesperproc` (61,440 on the Mac
  measured here; lower on some) gives at most half of it to the kqueue, less the reserve, so a
  tree past about 29,700 files and directories here never gets the kqueue: every read looks at
  the tree, as with R4-FR-01, and `SCRUTATE_SESSION_DEBUG=1` says why on each read. A watch
  refused later (a new file past the budget, or the system out of descriptors) drops the whole
  kqueue the same way; no read ever rests on a part of the tree.
- A probe: before it is in place, a file in `.scrutate/cookies/` is watched, written, and its note
  must be in the drain at once, or the volume reports nothing through kqueue (some network, FUSE or
  external volumes) and reads take the look. A tree with a directory on another device takes the
  look too.
- Not seen, as before: a write through a memory map makes no `VNOP_WRITE`, and a file reached through
  a link to outside the tree has no watch. The safety walk stays for those.

Tested: `kq::tests` (a write is in the next drain 1,000 times; a file replaced by a temporary file's
rename 1,000 times is named each time, with the same descriptors under its tree before and after;
a new file is watched before it is named; a directory renamed away and back asks for new watches;
a name that changes kind is a change; 1,000 files made and removed leave no entry behind, their
slots reused; two kqueues of one process draw on one budget; a file whose access comes back is
named within a few drains however many are shut; a tree past the budget keeps no descriptor; the
temporary directory's probe), `watch::tests::two_kqueue_builds_at_once_each_prove_the_volume`, and
the session tests `a_file_written_just_before_a_call_is_in_its_answer_every_time_through_the_kqueue`
(1,000 writes with no FSEvents at all, one file looked at each time),
`a_tree_past_the_kqueue_budget_is_looked_at_by_every_read`,
`a_directory_made_while_the_kqueue_is_built_is_not_left_unwatched`,
`a_new_directory_and_one_made_again_get_the_kqueue`, `a_new_directory_costs_one_look_and_then_none`,
`kind_changes_answer_as_the_tree_is` (fixed seeds over every ordered pair of absent, file,
directory, a link to a file and a link to a directory),
`a_file_the_user_cannot_read_leaves_the_kqueue_in_place`,
`a_directory_the_user_cannot_read_leaves_the_kqueue_in_place`,
`ignored_files_take_no_kqueue_watch_until_a_rule_takes_them_back` and
`the_watch_s_build_past_the_budget_beside_the_kqueue_in_place_brings_it_back`; the injected-order
test stays. Not
tested here: a volume whose probe fails (no such volume on the machines this ran on; the code takes
the look on that path).

Numbers, Apple M3 Pro, load 2 to 4. The C probe (`bench/kqueue_order.c`) on 16,799 files and
directories under a file-event flood: 20,000 in-place writes and 5,000 replaces by rename, each
followed at once by a zero-timeout drain, missed 0; a drain took 12.7 us and 14.1 us on average.
Through the yardstick's resident MCP adapter (`find_symbol`, the look build and the kqueue build
called in turn): on a 726-file copy of this repository 6.9 ms median before, 0.84 ms after; on
14,520 files 108.3 ms before, 5.8 ms after. A call right after an edit is bound by the index update
of the edited file (about 185 ms on 14,520 files with either build). Setting the watches: 16,622
in 442 ms, off the read path. Memory: a server on 14,520 files holds 16,639 descriptors and about
19.5 MB more resident memory with the kqueue (about 1.2 KB per watch, the paths kept for each; the
kernel's own per-watch memory is not in that count).
