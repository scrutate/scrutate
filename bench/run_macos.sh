#!/usr/bin/env bash
# The macOS runs behind docs/LEADERBOARD.md and docs/BENCHMARKS.md, in order: the Engine A
# leaderboard with a warm and a cold cache, the tool comparison, the indexed comparison. About
# 3.5 hours on an M3 Pro after the wait below when the tool comparison took 6 runs; it takes 30 now,
# so allow more; leave the machine idle, and do not run `cargo test` while it runs (it replaces
# target/release/scrutate). It refuses a tree with changes no commit holds and builds the binary
# from it, so every result is of the commit chain.log names. macOS starts a just-built executable
# about 0.2 ms slower than the same file later (identical copies, the same instructions executed),
# which is 5% of a small-tree search, and every competitor's binary is old: bench/wait_aged.py
# first waits until the binary starts faster than a fresh copy of itself, at most an hour.
#   bash bench/run_macos.sh
# A published verdict takes every run (--no-stop-early: at least 30 per tool); the stop rule is for
# the CI regression runs. A Linux x86_64 run on a dedicated server (bench/competitors/linux_bench.sh)
# can go at the same time; the Linux aarch64 virtual machine runs after this chain, since it shares
# this Mac.
set -uo pipefail
cd "$(dirname "$0")/.."
mkdir -p target/release
bash bench/competitors/built_from.sh write "$PWD" target/release || exit 1
cargo build --release -p scrutate --locked || exit 1
corpora=${CORPORA:-$HOME/.cache/scrutate-bench/corpora}
out=bench/results/macos-$(uname -m)
mkdir -p "$out"
[ -x target/evict ] || cc -O2 -o target/evict bench/evict.c
log() { echo "=== $* $(date '+%Y-%m-%d %H:%M:%S')" | tee -a "$out/chain.log"; }
bin=$(shasum -a 256 target/release/scrutate | cut -c1-16)
: > "$out/chain.log"
log "start $bin, built from $(cat target/release/BUILT_FROM) $(( ($(date +%s) - $(stat -f %m target/release/scrutate)) / 60 )) minutes ago"
python3 bench/wait_aged.py target/release/scrutate >> "$out/chain.log" 2>&1
log "binary aged"
warm=(--corpora "$corpora" --out "$out/leaderboard-warm.json" --max-load 40 --contender 1.15 --no-stop-early)
cold=(--corpora "$corpora" --out "$out/leaderboard-cold.json" --cache cold --cold-prepare "$PWD/target/evict {list}" --runs 30 --max-load 40 --contender 1.15
  --no-stop-early --only linux/literal linux/case-insensitive llvm/literal ripgrep/literal smallfiles/literal)
python3 bench/leaderboard.py "${warm[@]}" --force > "$out/leaderboard-warm.log" 2>&1
log "warm exit $?"
python3 bench/leaderboard.py "${cold[@]}" --force > "$out/leaderboard-cold.log" 2>&1
log "cold exit $?"
# benchmarks disturbed by other work on the machine, measured again once
python3 bench/leaderboard.py "${warm[@]}" --redo-unstable >> "$out/leaderboard-warm.log" 2>&1
log "warm again exit $?"
python3 bench/leaderboard.py "${cold[@]}" --redo-unstable >> "$out/leaderboard-cold.log" 2>&1
log "cold again exit $?"
python3 bench/compare_tools.py --corpus "small=$corpora/py-small" --corpus "big=$corpora/py-big" --corpus "massive=$corpora/py-massive" \
  --runs 30 --out "$out/compare.json" > "$out/compare.log" 2>&1
log "compare_tools exit $?"
python3 bench/indexed.py --corpus "$corpora/linux-nodot" --out "$out/indexed.json" --tools scrutate,zoekt --max-load 40 > "$out/indexed.log" 2>&1
log "indexed exit $?"
log "end $bin"
