#!/usr/bin/env bash
# The indexed comparison on Linux (Scrutate's text index, Zoekt, livegrep), after linux_bench.sh has
# built the tools and copied the corpora into the scrutate-bench-linux volume. It runs in its own
# ubuntu:24.04 container because livegrep's build (build_livegrep.sh, Ubuntu 24.04) needs glibc
# 2.38; Scrutate's and Zoekt's binaries run there unchanged. livegrep's query client lgq comes from
# bench/competitors/livegrep_query/build.sh, into the same directory as codesearch (the command is
# in its header); indexed.py stops before indexing when any binary is missing.
#
#   docker run --rm --platform linux/arm64 -v "$PWD":/src:ro -v "$HOME/.cache/scrutate-bench/livegrep":/livegrep:ro \
#     -v scrutate-bench-linux:/bench -v "$PWD/bench/results":/results ubuntu:24.04 bash /src/bench/competitors/linux_indexed.sh
set -euo pipefail
B=/bench
apt-get update -qq && apt-get install -y -qq python3 git time > /dev/null  # GNU time: per-query peak memory
export PATH=$B/bin:$B/go/bin:$PATH SCRUTATE_BIN=$B/scrutate/target/release/scrutate ZOEKT_BIN_DIR=$B/bin
git config --global --add safe.directory '*'
out=/results/linux-$(uname -m)
mkdir -p "$out"
cd /src  # the harness as committed; the binary is the one linux_bench.sh built
python3 bench/indexed.py --corpus $B/corpora/linux-nodot --work $B/indexed-work --out "$out/indexed.json" --tools "${INDEXED_TOOLS:-scrutate,zoekt,livegrep}" \
  --livegrep-bin /livegrep --max-load "$(nproc)"
rm -rf $B/indexed-work
