# Superseded runs

Earlier runs, kept as raw data. Until a newer run is recorded in the directory above, the
published documents (docs/LEADERBOARD.md, the README's charts) are generated from the `*-261e`
files. The dashboard charts every run here as history.

| File | scrutate binary (SHA-256) | Virtual CPUs |
|---|---|---|
| `leaderboard-warm-6602.json` (and `-cold`, `indexed-`, `bench-`) | `6602bbe58900...` | 8 |
| `leaderboard-warm-558e.json` (and `-cold`, `indexed-`, `bench-`) | `558e6e6b109f...` | 5 |
| `leaderboard-warm-e49f.json` (and `-cold`, `indexed-`, `bench-`) | `e49f3fbf80a2...` | 5 |
| `leaderboard-warm-261e.json` (and `-cold`, `indexed-`, `bench-`) | `261ed1a2a461...` | 5 |

`*-6602` is the run of the first 0.1.0 build, in a virtual machine with 8 CPUs on a host with 5
performance cores: three virtual CPUs ran on efficiency cores at any time, and a tool's three
block medians differed by 3% to 158% in the warm file (40 tool rows, median 14.5%) and by 3% to
264% in the cold one (13 tool rows, median 23.7%). Its binary predates the speed changes named
under the runs below.

`*-558e` is the run of the build before a tree read from the disk got more threads than cores on
Linux. Warm cache: 14 of 14 PASS. Indexed: every metric PASS. Cold cache: `linux/literal` and
`linux/complex regex` PASS, `linux/case-insensitive` and `linux/no match` FAIL by 0.2% and 0.7%
against git grep, both within 3% of the time five threads take to read the files (git grep took
1.44 times that time on `linux/complex regex`; ripgrep, ugrep, ag and GNU grep 1.07 to 4.4 times
it). Its read-files ceiling used five threads that shared a directory, which is slower than the
disk allows.

`*-e49f` is a complete run (14 of 14 warm, 4 of 4 cold and every indexed metric PASS) of the build
before output was held for a millisecond and a large one-line file was read on several threads.
Its thinnest row was `linux/very many matches`, 0.950 of ripgrep's time with an interval up to
0.995.

`*-261e` is a complete run (14 of 14 warm, 4 of 4 cold and every indexed metric PASS) of the build
before the MCP server served revision 2026-07-28 and `refs` followed the types of variables and
TypeScript path aliases.
