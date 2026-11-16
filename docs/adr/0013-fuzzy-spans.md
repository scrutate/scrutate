# 13. A fuzzy line's spans follow one definition

Status: accepted, 2026-11-16.

## Context

`grep --fuzzy` and `--fuzzy-regex` decide which lines match by the first point where the line
comes within N edits of the pattern. The spans a matched line shows (colour, the `--json` spans
and column, the count of matches) were first taken from that same search: the first end, the
start of the longest text ending there, and the search went on from that end. That gave spans
more than N edits from the pattern (`--fuzzy=2 b2日` on `a123 b` lit ` b`, three edits), a
spelling slip lit only part of the word (`--fuzzy=1 abcd` on `abcd` lit `abc`), and later
fixes that carried the end on found a new case each time: overlapping occurrences swallowed
into one span (`aa` in `aaaa`), then a greedy quantifier cut at its first end (`[0-9]+` on
`1234` as four spans). Each fix was a rule about how far to carry, and none said what a span is.

## Decision

A span is defined, and the matcher implements the definition.

**The definition.** An occurrence is a part of the line within N edits of the pattern (for a
regex, of some text the regex matches whole). An edit is one character inserted, deleted or
substituted, in the matcher's units: a character, or one byte of invalid UTF-8. At either end of
an occurrence the character is matched as it stands, or is a word character (`\w`) substituted
for a pattern character; a space or punctuation mark is never a substitute at an end (there the
pattern's character counts as deleted, and the occurrence is one shorter), and an inserted
character is never at an end. Inside, any edit. Spans are chosen: the fewest edits first; then
the leftmost start; then the longest. A chosen span removes every occurrence that overlaps it,
and the choice repeats until none is left. They are reported left to right.

What follows from it:

- every span's own text is within N edits;
- an exact occurrence is always its own span, never swallowed by a looser one;
- with no edits, a literal's spans are `rg -F`'s, and a regex's are the leftmost-longest
  matches; they differ from ripgrep's leftmost-first only where it stops short: alternation, an
  optional part, a lazy quantifier (`a|ab` on `ab`: ours `ab`, ripgrep's `a`);
- a line matches exactly when it has a span: dropping an edited end character never raises an
  occurrence's cost, and the empty string is never within N edits (such a pattern is refused).

Cases that are stated behaviour, not defects: a fragment left beside a better span is an
occurrence of its own (`--fuzzy=1 ab` on `aab`: `a`, then `ab`); a single character within N
edits is a span (`--fuzzy=2 abc` on `xxaxx`: `a`).

**The build.** Which lines match is unchanged: `find_at` still stops at the first end, so `-c`,
`-l`, `-q` and the line sets are what they were. A matched line's spans come from `spans`
(`FuzzyMatcher::spans`, `FuzzyRegex::spans`), called from `instant::each_match_in` for every
consumer of spans, only on lines that print with them. It runs N + 1 passes; pass c works in the
gaps the earlier passes left, where nothing costs less than c (trimming the edited ends off a
cheaper occurrence would leave a cheaper allowed one, which an earlier pass took). In each gap:

1. A backward bit-parallel search with the reversed pattern (Myers' words for a literal, the
   reversed Glushkov automaton for a regex) lists every position where an occurrence within c
   starts and ends inside the gap.
2. Walking that list left to right, the first position from which a forward run anchored there
   finds an occurrence under the end rule is the span's start (every allowed occurrence is also
   one the search lists, so this is the leftmost), and the run's last allowed end is its end.
3. The walk goes on from that end.

The forward run of a literal keeps 2c + 1 pattern positions around the diagonal, so its work per
character goes with c, not the pattern's length; a regex's keeps one state set per number of
edits, with each position's count of trailing deletions to an end worked out once. A line of
100,000 matches is read a constant number of times per pass.

## Consequences

- Spans are checked against a brute-force reference written from the definition alone
  (`tests/fuzzy_spans.rs`): literals at N 0, 1 and 2, regexes at N 0 and 1, on generated short
  lines with overlapping occurrences, quantifiers and multi-byte text, compared for exact
  equality with the `--json` spans.
- A debug build asserts, where spans meet detection, that a line the matcher matched has a span.
- Spans cost N + 1 backward passes over a matched line on top of detection; lines that only
  count or list do not pay it.

## Alternatives not taken

- **The earliest end, then a carry.** The search's first end with a start found back from it,
  carried on by a rule. Every rule tried left a class of wrong spans: past N edits, overlapping
  occurrences joined, greedy runs cut. The span was a side effect of where detection stopped.
- **Leftmost-first, as ripgrep's regex engine.** It needs the priority of each branch and each
  quantifier, which a Glushkov automaton with edit counts does not have; with edits there is no
  ripgrep answer to match. Leftmost-longest needs no priorities and is what the definition gives.
