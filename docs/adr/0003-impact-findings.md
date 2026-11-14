# 3. Impact owns the findings

Status: accepted, 2026-11-15.

## Context

`review` was the only place that named what a change does that a reviewer would want to know:
the duplicates it adds and the changed definitions no test reaches. It found them itself, from
fields of `impact`'s answer it reached into, and each of its three renderings (Markdown, JSON,
GitLab) listed them its own way. An agent asks through the MCP `impact` tool and never saw them,
and every new kind of finding would have cost an edit to each rendering.

## Decision

`impact` makes one list of findings (`findings::find`, kept in `Impact::findings`). Every front
end (the command line, the MCP tool, `batch`, the Python bindings) and `review` render that list;
none of them looks for findings of its own.

A finding (`findings::Finding`) has:

- a place: the path, lines and qualified name of the definition the change touched;
- a kind, a stable name: `duplicate`, `untested-change`;
- a severity, a level of GitLab's Code Quality scale: `info`, `minor`, `major`, `critical`,
  `blocker`;
- a sentence, the same in every rendering;
- related places, the other definitions it names;
- an identity of paths and names, never line numbers, so a finding keeps it while its lines
  move.

The list holds the duplicates first, largest group first and at most 50 groups, then the
untested definitions in the order of the changed ones. A later kind comes after these.

The rules a renderer and a later kind keep:

- Kinds are only added. A kind is never renamed, and its identity rule never changes: GitLab
  matches an issue across pushes by its fingerprint, the hash of the check and the identity.
- One severity scale. A renderer for another format maps it; SARIF takes `info` as `note`,
  `minor` and `major` as `warning`, `critical` and `blocker` as `error`.
- A new kind is one `What` variant with its kind, severity, sentence, identity and related
  places; every rendering then shows it with no edit of its own.
- The answer's JSON gains keys only: `findings` and `more_findings` were added under schema 2.

## Consequences

- An agent gets the findings from the MCP `impact` tool, with no tool added.
- Every `impact` call now pays for the duplicate search, which only `review` paid before. It
  reads every definition's duplicate keys once per call and a text only for a definition in a
  group or a shared bucket ([Architecture](../ARCHITECTURE.md#intelligence-intelrs)). On the Linux
  tree, with a copy of one function and an edit of another, `impact --json` took 3.60 s with the
  findings and 3.00 s without them (warm, best of 5, in turn), within the 1.2 times this was held
  to.
- `review` keeps its Markdown and GitLab output byte for byte; its JSON gains the `findings` key
  and keeps `new_duplicates` and `untested`, read from the list.
