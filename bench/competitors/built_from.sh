#!/usr/bin/env bash
# built_from.sh write|check SRC DIR: `write` records the commit SRC is at in DIR/BUILT_FROM when
# scrutate is built from it; `check` fails when SRC, the harness a run is about to use, is at
# another commit (or DIR recorded none), so no result pairs one commit's harness with another's binary.
set -euo pipefail
mode=$1 src=$2 dir=$3
have=$(git -C "$src" rev-parse HEAD)
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
