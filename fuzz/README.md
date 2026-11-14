# Fuzz targets

Coverage-guided fuzzing with libFuzzer through `cargo fuzz`, which needs nightly. This directory
is its own workspace, so the product never builds with nightly.

| Target | What it checks |
|---|---|
| `npy` | arbitrary bytes as an `.npy` array: the reader rejects or reads them, never panics or over-allocates |
| `trigram_query` | soundness of the text index: if a regex, literal, identifier or fuzzy pattern matches a line (a fuzzy one through the matcher grep builds), that line's trigrams satisfy the pattern's trigram condition |
| `fuzzy_matcher` | the edit-distance matcher: no panic, every match inside the haystack and on one line |
| `parse_file` | every parser tier under 60 file extensions: no panic, chunk line ranges inside the file |

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run -O trigram_query -- -max_total_time=600
```

CI builds every target and replays `regressions/` on each push. Once a week
`.github/workflows/fuzz-weekly.yml` fuzzes each target for 45 minutes, starting from the corpus the
last run uploaded (an artifact named `corpus-<target>`); a crash fails the run and is uploaded as
`crash-<target>`. Add a crash input to `regressions/` with its fix.

`regressions/` keeps every input that once crashed a target. Replay one with
`cargo +nightly fuzz run -O <target> regressions/<target>/<file>`; `tools/gen_test_results.py`
replays them all. Each also has a unit test in the crate, so `cargo test` covers them on stable.
