# 9. Verified symbol edits over MCP, off by default

Status: accepted, 2026-11-15.

## Context

Every MCP tool reads: the server writes nothing but its index, and its tools say so to the
client (`readOnlyHint`). Agents edit code with their own tools, by line ranges or by text they
search for. Two failures recur. An edit applies to a file that changed after the agent read it,
and overwrites the change (Serena #1013, a stale buffer). An edit that replaces a body by its
range leaves the old body in place beside the new one, or cuts a neighbour in half (Serena #576:
`replace_symbol_body` duplicates code, 41 comments). The index knows each definition's exact
range and can parse the result before it is written, so an edit by definition can be checked in
ways a text edit cannot.

The tool writes source files, which no other MCP tool does, so the user decided it is opt-in.

## Decision

`serve --allow-edits` lists one more tool, `edit_symbol`. Without the flag the tool is not listed,
and a call is refused like a call of an unknown tool. The flag is read from the command line only:
no environment variable or configuration file turns it on, so nothing in a repository can. Only
an MCP call reaches the edit: the operation is never serialized, so the session socket that
command line calls use cannot carry one to a running server (one without the flag included), and
`batch` refuses the tool by name (the Python bindings have no call that edits).

`edit_symbol` replaces one whole definition with `new_text`:

- `symbol` names it, as `read_code` takes a symbol (a name, `Class.method`, a location), with
  `path` to keep the ones in one file. It must name exactly one definition, or the call says which
  ones it names and changes nothing.
- A dry run is the default. It returns the unified diff the edit makes and `hash`, 16 hex digits
  of the file's content hash as it was read (short enough that no secret detector takes it for a
  key, so an answer shows it as it is).
- `apply: true` writes the edit, and only with `hash` given and equal to the hash of the file as
  it is now. A file that changed since the hash was taken is refused, with its current hash and
  nothing written, so an agent always looks at the current text before it writes.
- Before anything is written the new file is parsed with the file's language and checked: no
  parse error or missing node inside the replaced range that the old text did not have; one
  definition of the same name and kind where the old one began; every other definition of the
  file still there by name and kind, in order. An edit that fails a check is refused with the
  check that failed.
- The write goes through the structural rewrite's writer (`rewrite.rs`): the file is opened by
  its directory with no link on the way, its bytes are checked to still be the ones read, and a
  new file in the same directory is renamed over it, keeping its permissions. A file outside the
  root, a link, a file under `.git/` or `.scrutate/` at any depth (by name under every spelling
  git's own rules take for its directory, which the index skips too, and by file identity with
  the root's own, so a link to either counts), and a file the index does not hold are refused. A
  dry run writes nothing in the repository, not even a temporary file. The file's line endings are kept: `new_text` written with `\n` takes the file's `\r\n`.
- The answer names the definition's new lines and the file's new hash. The index sees the
  edit at the next call, as it sees any write to the tree.

`edit_symbol` is the one tool without `readOnlyHint`. Its hints are `destructiveHint: true` (it
replaces text) and `idempotentHint: true` (the same call again is refused: the hash moved).

## Consequences

- The MCP server's read-only stance holds unless the user starts it with `--allow-edits`. The
  security model gains one writer, under the same rules as `tsquery --rewrite --write`
  ([Security](../SECURITY.md#verified-symbol-edits)).
- An edit costs two calls: a dry run for the diff and the hash, then the apply.
- A definition's comment block and decorators belong to its range as the index has it, so
  `new_text` replaces them too; the dry run's diff shows it.
- Renaming a definition is out of scope: the same name must be where it was. A rename touches
  its callers, which is another tool.

## Rejected

- A per-definition hash, of the definition's text alone: an edit elsewhere in the file can still
  make the edit wrong (an import it uses removed, a neighbour that moved into its range), and the
  file is what is written.
- Writing by line range, as the agents' own tools do: the ranges move as soon as a file changes,
  which is the failure this tool is for.
- Turning it on from the environment or `.scrutate.toml`: a repository could then turn on writes
  for every agent that opens it.
