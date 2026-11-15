# 6. The session on Windows

Status: accepted, 2026-11-15.

## Context

A running `scrutate serve` offers its open index to command line calls of the same user on the
same root (CLI.md, `serve`). On Unix the meeting point is `.scrutate/session.sock`, an `AF_UNIX`
socket, mode 0600, checked both ways by the peer's uid (`getpeereid`, `SO_PEERCRED`). On Windows
`session::ask` and `session::listen` are stubs: every command line call does the whole work
itself, so Windows never gets the resident path, and Windows is where starting a process and
opening the index cost the most.

The brief named two transports: `AF_UNIX` (Windows 10 1803 and later) with a named-pipe fallback.
What each gives on Windows:

| | `AF_UNIX` on Windows | named pipe |
|---|---|---|
| available | Windows 10 1803+, Server 2019+; Rust's std has no `UnixStream` there, so raw Winsock or a crate | every Windows; `windows-sys` (already a dependency) |
| owner only | an ACL on the socket file, set after `bind` (a window where the file has the directory's inherited ACL) | a security descriptor given to `CreateNamedPipeW` itself: no window |
| who is at the other end | `SIO_AF_UNIX_GETPEERPID` (a process id, newer builds only), then its token | the client: its connection's own token (`ImpersonateNamedPipeClient`); the server: `GetNamedPipeServerProcessId`, then its token |
| a server that died | its socket file stays behind and must be cleaned up | the name goes away with the last handle; nothing is left |
| remote callers | none | rejected with `PIPE_REJECT_REMOTE_CLIENTS` |
| testable under Wine | partly | yes |

## Decision

One transport on Windows: a named pipe. `AF_UNIX` is not used, as primary or fallback. It adds a
second code path that needs Windows 10 1803+, carries a stale-file problem the pipe does not have,
and gives a weaker peer check; the pipe does everything the session needs on every Windows. One
path is also one path to test.

- **Name.** `\\.\pipe\scrutate-session-<hash>`, the hash a BLAKE2b-128 of the user's SID and the
  canonical root, in hex. The SID in the hash keeps two users on one root apart; the name is not
  a secret and is not relied on for safety. Nothing is written to `.scrutate` on Windows.
- **Owner only.** The pipe is created with a protected DACL of one entry, full access for the
  current user's SID and a mandatory label at the server's integrity level (SDDL
  `D:P(A;;GA;;;<sid>)S:(ML;;NW;;;S-1-16-<level>)`), so no other account can open it and no lower
  process write to it, and with `PIPE_REJECT_REMOTE_CLIENTS`.
- **Both ends checked, as on Unix.** The client writes one byte first; the server reads it,
  impersonates the client at the identification level the client allows, reads that token (the
  connection's own, where a process id could name another process by the time it is looked up)
  and reverts. A client of another user, below the server's integrity level, restricted or an
  AppContainer one gets nothing, not even the greeting, and so does a client that opened the
  pipe for reading alone (a Low process may: the pipe's label forbids it only to write), which
  cannot send the byte. A connection's request, all of it, comes within 2 s, and each write to
  the other end waits at most 2 s. The client does the same with the server's process id before it
  reads the greeting, so a pipe of the same name made first by another account (pipe squatting)
  is passed over. The client also reads the pipe's label and drops a pipe below its own level
  (the label stays with the pipe, where a process id can name another process by then), and it reads the server's integrity level and drops a server below its
  own, or one whose token is restricted or an AppContainer one: a sandboxed process of the same
  user that took the name first would otherwise see every request and forge every answer (a test
  shows it on Windows: without the check the client takes the forged answer of a Low copy of
  itself that made the name first). What
  stays trusted: any process of the same user at the client's level or above, an elevated one
  included, as any such process could read the repository and run scrutate itself. The client opens the pipe with `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`,
  so a server can learn who the caller is but never act as the caller.
- **First server wins.** The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`. A
  second `serve` on the root, or anything that holds the name already, makes that create fail, and
  that server offers nothing; its command line calls go to the first, as on Unix.
- **Stale cleanup.** None is needed: the name exists only while a server holds an instance, and
  the system closes a killed server's handles. The Unix stale-socket rules stay as they are.
- **The protocol and the identity check are the same code on both systems.** One greeting line
  (the identity), one request line, one reply line; the same waits (`HELLO` 20 ms or
  `SCRUTATE_SESSION_WAIT_MS`, `REQUEST` 2 s, `ANSWER` 60 s), done on Windows with overlapped I/O
  and a timed wait, the command line's write of its request bounded by `REQUEST` too (a pipe
  holds 64 KB, and a server stopped or stuck after its greeting takes no more), 64 KB at a time,
  so the bound is on progress, as a socket's send timeout is; the same limit of 16 calls served at once. The build in the identity is the
  executable's path, size and times on Windows, where Unix uses device and inode as well.
- **The switches stay.** `SCRUTATE_NO_SESSION=1` and `SCRUTATE_SESSION_DEBUG=1` work as on Unix.

## Consequences

- Windows command line calls get the resident path while a `serve` runs; the same bytes and exit
  codes as a fresh process, which `tests/session.rs` now checks on Windows too. The socket-file
  tests stay Unix only, and so does the moved-root test (Windows does not rename a directory a
  running server works in). On a CI runner `a_pipe_is_for_its_owner_alone` reads the pipe's DACL
  (one entry, this user's SID alone, protected), then adds a local account, has it open the
  session's pipe and sees the open refused; the two-servers test shows a second
  server offers nothing.
- New `unsafe` in `session.rs` for the pipe, token and SID calls, listed in SECURITY.md.
- On Unix a peer of another account is still checked by code review only, as before.
- Measured on GitHub's Windows runners (run 43725229123, `bench/session_calls.py` on a copy of
  this repository, 30 interleaved rounds, every call answered by the session): median time of one
  command line call through a running `serve` against the same call in a fresh process
  (`SCRUTATE_NO_SESSION=1`, which is every call before this change):

  | command | windows-latest | windows-11-arm |
  |---|---|---|
  | `symbol Session` | 9.1 ms vs 30.5 ms | 14.4 ms vs 48.2 ms |
  | `callers open` | 32.6 ms vs 56.4 ms | 38.0 ms vs 74.9 ms |
  | `outline crates/scrutate/src/session.rs` | 10.9 ms vs 32.1 ms | 15.7 ms vs 49.3 ms |
  | `refs Identity` | 368.5 ms vs 400.8 ms | 325.3 ms vs 365.7 ms |

  `refs` gains least: it reads the files it searches either way.
