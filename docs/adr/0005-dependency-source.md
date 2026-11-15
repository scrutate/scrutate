# 5. A dependency's source at its locked version

Status: accepted, 2026-11-15.

## Context

An agent that cannot see a library guesses its API. The index skips `node_modules`, `.venv` and
the like by name, so `symbol` and `read` never reach a dependency, and indexing those directories
would put every installed package into every search. The agent needs the source of the version
the repository runs, which only the lockfile says, and it must not get another version that also
sits on the disk (the cargo registry and the Go module cache keep every version ever fetched).

This is the first answer read from outside the repository root, from names a lockfile gives. A
lockfile is a file anyone with a commit can write, so its names are untrusted input that become
paths.

## Decision

`symbol --deps PACKAGE.NAME` and `read --deps --symbol PACKAGE.NAME`, and the MCP `find_symbol`
and `read_code` with `scope: "deps"`, answer from a dependency (`packages.rs`):

- The package comes from the lockfiles at the root: `Cargo.lock`, `package-lock.json`, `uv.lock`,
  `poetry.lock` and `go.mod`. The longest start of the name that a locked package is imported by
  is the package.
- Its files come from where its locked version is installed, and only there: the cargo registry,
  `node_modules`, `.venv`'s `site-packages` (the files the distribution's `RECORD` lists), the Go
  module cache. An installed package at another version is an error that names both versions and
  the command that installs the locked one.
- A path dependency answers from the repository's own index, kept to its directory, when the
  directory is inside the root, and is refused when it is outside.
- The package is copied once into the user cache, `deps/ECO/NAME@VERSION/src`, and indexed there
  once, with no vectors. The copy is made in a temporary directory and renamed into place, so another
  process never reads half of one; a second repository at the same version reads the same copy.
  The repository's index is never touched.
- Answers name the installed files, not the copy, so the agent can open them with its own tools.

The read boundary, which every rule below keeps:

1. A name and a version from a lockfile must fit its ecosystem's grammar (letters, digits and
   `-._~+`, no `.` or `..` part, an npm scope, a Go module path of such parts, a Go version that
   starts with `v`). The directory built from them is canonicalized and must stay under its store
   (`$CARGO_HOME/registry/src`, `node_modules`, `site-packages`, `$GOMODCACHE`). Anything else is
   refused, by name, before a file is read.
2. The copy never follows a link out of the package: a linked file is copied only when it
   resolves inside the package directory, a linked directory is skipped, and a `RECORD` entry is
   read only when its directory resolves inside `site-packages`.
3. A copy holds at most 20,000 files and 256 MiB. A larger package is an error, and nothing of it
   is kept, so no half copy is ever indexed.
4. The cache directories are created with mode 0700.
5. Answers are redacted as the repository's are, on every front end.

## Consequences

- An agent reads the API it calls at the version it runs, offline, with no hosted service.
- The read boundary grows by exactly the packages the lockfiles name, in those four stores, at
  the locked versions ([Security](../SECURITY.md#dependency-source)).
- A copy is keyed by name and version alone. A package edited in place after its copy was made
  (a debug print in `node_modules`) is answered as it was copied, until the cache entry is
  removed. Two repositories whose installs of the same version differ get the first one's copy.
- The first call for a package pays for the copy and its index. A copy never changes once made,
  so it is indexed once and later calls read that index as it is, with no look at its files. A
  second repository at the same version neither copies nor indexes: its call costs no more than
  1.5 times a read of the cached copy as a repository of its own (`bench/deps_acceptance.py`,
  0.020 s against 0.017 s for serde 1.0.219 on the 32-core build box). A large package (TypeScript's compiler, an SDK of thousands of files) can be
  over the cap and is then not answered at all.

## Rejected

- Indexing `node_modules` and `.venv` into the repository's index: every search and every
  `refs` would wade through every installed package, and the cargo and Go stores are outside the
  root anyway.
- An index in place in the store (`.scrutate/` inside `~/.cargo/registry/src/...`): it writes into
  a directory the package manager owns and may check.
- Reading the newest version on disk: the cargo registry and the Go module cache keep old versions
  side by side, so the newest is often not the one the repository builds with.
- Not yet: pnpm and Yarn lockfiles, `external:` callees and `search` reaching into dependencies.
