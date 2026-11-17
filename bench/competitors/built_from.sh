#!/usr/bin/env bash
# built_from.sh write|check SRC DIR: `write` records the commit SRC is at in DIR/BUILT_FROM when
# scrutate is built from it; `check` fails when SRC, the harness a run is about to use, is at
# another commit (or DIR recorded none), so no result pairs one commit's harness with another's binary.
# Both refuse a SRC with changes no commit holds (the build copies the working tree, not HEAD):
# an edited or staged file, or an untracked one outside bench/results/ and host-facts.json, which
# a run writes there itself.
set -euo pipefail
mode=$1 src=$2 dir=$3
have=$(git -C "$src" rev-parse HEAD)
dirty=$(git -C "$src" status --porcelain --untracked-files=all -- . ':!bench/results' ':!host-facts.json')
if [ -n "$dirty" ] && { [ "$mode" = write ] || [ "$mode" = check ]; }; then
  echo "built_from.sh: $src has changes no commit holds; commit or remove them before a run:" >&2
  echo "$dirty" | head -20 >&2
  exit 1
fi
case "$mode" in
  write) echo "$have" > "$dir/BUILT_FROM" ;;
  check)
    want=$(cat "$dir/BUILT_FROM" 2> /dev/null || echo "no recorded commit")
    if [ "$want" != "$have" ]; then
      echo "built_from.sh: the harness at $src is at $have, but $dir was built from $want: run linux_bench.sh again" >&2
      exit 1
    fi ;;
  *) echo "usage: built_from.sh write|check SRC DIR" >&2; exit 2 ;;
esac
