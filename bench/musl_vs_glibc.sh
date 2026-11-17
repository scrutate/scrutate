#!/usr/bin/env bash
# The static musl binary against the glibc one on x86_64 Linux, on idle hardware, by the
# leaderboard's own method: both built here from this commit (a tree with uncommitted changes is
# refused), their output compared byte for byte on every corpus, then bench/leaderboard.py runs every
# Engine A row with the musl binary as `scrutate` and the glibc one as `scrutate base`, the two in
# the same interleaved blocks, each answer checked against the reference. The per-row ratio
# musl / glibc and its 95% interval come from bench/leaderboard_regression.py.
#   bash bench/musl_vs_glibc.sh --corpora DIR [--out DIR] [--only ID-PREFIX...]
# (corpora as bench/corpora.py makes them; --only keeps the leaderboard rows whose id starts so)
# Needs the rust-toolchain.toml toolchain with the x86_64-unknown-linux-musl target, musl-tools,
# hyperfine and the leaderboard's other tools (bench/competitors/linux_bench.sh installs them).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
corpora='' out=$root/bench/results/platforms/linux-x86_64 only=()
while [ $# -gt 0 ]; do
  case "$1" in
    --corpora) corpora=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --only) shift; while [ $# -gt 0 ] && [ "${1#--}" = "$1" ]; do only+=("$1"); shift; done ;;
    *) echo "usage: musl_vs_glibc.sh --corpora DIR [--out DIR] [--only ID-PREFIX...]" >&2; exit 2 ;;
  esac
done
[ -d "$corpora" ] || { echo "musl_vs_glibc.sh: --corpora DIR is required" >&2; exit 2; }
[ "$(uname -sm)" = "Linux x86_64" ] || { echo "musl_vs_glibc.sh: runs on x86_64 Linux only" >&2; exit 2; }
mkdir -p "$out"
bash "$root/bench/competitors/built_from.sh" write "$root" "$out"

cd "$root"
cargo build --release -p scrutate --locked
cargo build --release -p scrutate --locked --target x86_64-unknown-linux-musl
target=${CARGO_TARGET_DIR:-$root/target}
case "$target" in /*) ;; *) target=$root/$target ;; esac
glibc=$target/release/scrutate musl=$target/x86_64-unknown-linux-musl/release/scrutate
file "$musl" | grep -qE "statically linked|static-pie linked" || { echo "the musl binary is not static" >&2; exit 1; }
if ldd "$musl" 2>&1 | grep -q "=>"; then echo "the musl binary loads a shared library" >&2; exit 1; fi

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
# the hash of one answer and its exit status, so a pattern that matches nothing (exit 1) is
# compared too; an error (exit 2) stops the run
answer() {
  local dir=$1 bin=$2 s
  shift 2
  (cd "$dir" && "$bin" grep --sort "$@" .) > "$tmp" && s=0 || s=$?
  if [ "$s" -gt 1 ]; then
    echo "$bin grep --sort $* failed in $(basename "$dir") (exit $s)" >&2
    return 1
  fi
  { cat "$tmp"; echo "exit $s"; } | sha256sum | cut -d' ' -f1
}

# the same bytes from both on every corpus, for each kind of pattern the leaderboard times
same=$out/musl-vs-glibc-identical.txt
{
  echo "glibc $("$glibc" --version), musl $("$musl" --version), commit $(cat "$out/BUILT_FROM")"
  for dir in "$corpora"/*/; do
    for q in "-F static" "-i error" "-w main" "[A-Z][a-z]+Error" "-c return"; do
      # shellcheck disable=SC2086 # each q is several words
      # one assignment each: a line of two takes the status of its last substitution alone
      a=$(answer "$dir" "$glibc" $q)
      # shellcheck disable=SC2086
      b=$(answer "$dir" "$musl" $q)
      [ "$a" = "$b" ] || { echo "DIFFERS: $(basename "$dir") grep $q"; exit 1; }
      echo "identical: $(basename "$dir") grep --sort $q ($a)"
    done
  done
} | tee "$same"

json=$out/musl-vs-glibc.json
SCRUTATE_BIN=$musl SCRUTATE_BASE_BIN=$glibc python3 bench/leaderboard.py --corpora "$corpora" --out "$json" --no-stop-early --force \
  ${only[@]+--only "${only[@]}"}
# a row where the musl answer is judged otherwise than the glibc one (wrong where glibc's is right)
python3 - "$json" <<'EOF'
import json, sys
rows = json.load(open(sys.argv[1]))["benchmarks"]
bad = [f"{bid}: musl {r['tools'].get('scrutate', {}).get('status')}, glibc {r['tools'].get('scrutate base', {}).get('status')}"
       for bid, r in sorted(rows.items()) if r["tools"].get("scrutate", {}).get("status") != r["tools"].get("scrutate base", {}).get("status")]
if bad:
    sys.exit("the musl answer is not judged as the glibc one:\n  " + "\n  ".join(bad))
EOF
# musl / glibc per row; a ratio above 1 is musl slower. With the answers equal, exit 1 says only that.
set +e
python3 bench/leaderboard_regression.py --within "$json" --tolerance 0 | tee "$out/musl-vs-glibc.txt"
status=${PIPESTATUS[0]}
set -e
[ "$status" -ne 2 ] || { echo "nothing compared" >&2; exit 1; }
echo "written: $same, $json, $out/musl-vs-glibc.txt"
