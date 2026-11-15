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
  places. The text, the JSON, the MCP tool and the GitLab report then show it with no edit of
  their own; `review`'s Markdown has a section per kind and needs one for it.
- The answer's JSON gains keys only: `findings` and `more_findings` were added under schema 2.

## Consequences

- An agent gets the findings from the MCP `impact` tool, with no tool added.
- Every `impact` call now pays for the duplicate search, which only `review` paid before. It
  reads every definition's duplicate keys once per call and a text only for a definition in a
  group or a shared bucket ([Architecture](../ARCHITECTURE.md#intelligence-intelrs)). It was held to
  at most 1.2 times the time of `impact --json` without the findings on the Linux tree, measured
  by hand for the change (its commit message has the run; no script in `bench/` reruns it).
- `review` keeps its Markdown and GitLab output byte for byte; its JSON gains the `findings` key
  and keeps `new_duplicates` and `untested`, read from the list.

## Rejected: a reuse kind

A `reuse` finding was tried and dropped: an added function or method outside a test file, named
with its nearest function or method by implementation vector outside the changed files and the
test files, when the two vectors' cosine reached a threshold T. It was held to at most 1 false
alarm per 100 added definitions with a recall of at least 5%.

It was calibrated on 13 repositories (atuin, bookstack, whitehall, openfoodnetwork, eslint,
kotlinconf-app, bitwarden, dolphinscheduler, gitea, mastodon, forem, cal.diy, ctfd): 281 coding
agent sessions that wrote a new function where the repository had a helper they should have
called, and 1,613 definitions the repositories added themselves in their history.

- No T from 0.50 to 0.99 met the bar: at 0.90, 12.8 false alarms per 100 and a recall of 2.8%;
  at 0.99, 5.2 per 100 and none. Most alarms at 0.99 and above are real copies (built bundles,
  generated migrations) or short bodies with the same vector and few shared words.
- The helper was the nearest candidate in 51 of the 281 sessions, never above a cosine of 0.96,
  and in three repositories never among the ten nearest.
- A minimum body size, a band of token Jaccard, helper directories under test paths, and the
  search model (alone, or as a gate on the nearest) each failed the bar. One combination passed
  at one point picked from about 8,000 on the same data: 0.68 per 100 and 15 of 281, 12 of them in
  one repository (dropping that repository's records leaves a recall of 1.4%).
- Held out, it failed: with each repository left out in turn, the point picked on the other 12 by
  the same rule and scored on the one left out, the 13 pooled to 2.1 false alarms per 100 and a
  recall of none of the 281. This is why the kind was rejected.
- The lookup was not the limit. Reading only the 48, 96 or 192 clusters nearest an added
  definition gave the same recall as every row from a T of 0.85 up. `similar` keeps its
  exhaustive scan: on the Linux tree the clusters nearest the target reach 99% of its ten
  nearest only at 768 probes, at about the exhaustive scan's cost.

The numbers were measured by hand on 2026-11-15 with scrutate 2b1a270 and
[`bench/reuse_calibration.py`](../../bench/reuse_calibration.py): `collect` on each repository,
then `report` for the sweep and the variants. Its tasks file is not committed: it holds the
agents' session files, from an evaluation outside this repository. A reuse kind is worth trying
again with a signal that finds the helper as the nearest candidate far more often than 51 times
in 281.

## Rejected: a boundary kind

A `boundary` finding was tried and dropped: an added import from a file F to a file T, neither a
test file, where the two directories just below the common ancestor of F's and T's directories
(`src/a/b.py` importing `lib/c.py` gives `src` and `lib`) both existed in the base and no file
under the first imported a file under the second. It was held to two bars, each met with every
repository left out in turn: at most 5 firings per 100 non-merge commits that change code, and a
lift of at least 2, the share of the imports it names that the repository took out again within
200 commits over the same share for the other added imports between linked directories.

It was calibrated on the same 13 repositories, scrutate and Linux: the 400 first-parent commits
after a base, each import followed for 200 more.

- The rate held: 3.9 per 100 pooled (121 of 3,081 commits), 3.2 to 4.4 held out.
- The lift failed: 134 of 904 named imports were taken out (14.8%), against 1,447 of 17,056
  others (8.5%), a lift of 1.75 and 1.54 to 1.84 held out. Per repository it ran from 0 (mastodon)
  and 0.54 (dolphinscheduler) to 7.2 (gitea).
- Counted per firing (one commit and one pair of directories), the lift held out was 1.47 to 1.81
  when any of its imports was taken out, and 1.73 to 2.43 when all of them were. Neither met the
  bar in every held-out set.
- Linux stopped after 305 of its 600 commits: its base alone resolved 745,252 imports and the walk
  ran over 3 hours. None of its firings could be followed for 200 commits. The pool without Linux
  already fails the lift (1.73), so the rest of its history cannot make the kind pass.

The index keeps no table of the links between directories, which a boundary kind would have read.

The numbers were measured on 2026-11-15 with scrutate built from 425d011 and
[`bench/boundary_calibration.py`](../../bench/boundary_calibration.py): `collect` on each
repository, then `report`. A boundary kind is worth trying again against a better sign of a bad
import than its removal within 200 commits.
