#!/usr/bin/env bash
# The checks of .github/workflows/ci.yml, security.yml and benchmarks.yml that a Linux box can run,
# for the commit checked out here. tools/gate_map.json names, for every `run:` step of those
# workflows, the step below that covers it or why only CI runs it; tools/gate_parity.py (the
# gate-parity step) fails when a workflow step is missing from the map, so the two cannot drift.
#   bash tools/ci_gate.sh [--quick|--bench]
#   (no flag)  every step below
#   --quick    the Rust and tree checks only (no test suites, no release-build steps)
#   --bench    the release build and the steps that use it only
# One PASS/FAIL line per step; exit 0 only if every step run passed. The box is set up by its
# ci_setup.sh: the toolchains, wasi-sdk in ~/wasi-sdk, and in SCRUTATE_GATE_TOOLS (default
# /srv/tools) and SCRUTATE_GATE_VENV (default /srv/venv) the pinned rg, cargo-deny, gitleaks,
# zizmor, ruff and numpy.
set -u
. "$HOME/.cargo/env"
tools=${SCRUTATE_GATE_TOOLS:-/srv/tools} venv=${SCRUTATE_GATE_VENV:-/srv/venv} results=${SCRUTATE_GATE_RESULTS:-/srv/gate-results}
# as ci.yml: warnings fail every build and test (the MSRV check below clears it, as CI does)
export RUSTFLAGS="-D warnings"
# the pinned ripgrep 15.2.0 (15.1.0 has the multi-root ignore bug 43e2f08ede fixed): differential
# and fuzz compare against it, and the tests that run rg fail rather than skip without it
export SCRUTATE_RG=$tools/rg PATH=$tools:$PATH SCRUTATE_TEST_NEEDS_RG=1
# temp files on the disk beside this lane's target, not the shared /tmp tmpfs (16 GB with a per-user quota)
target=${CARGO_TARGET_DIR:-$PWD/target}
export TMPDIR="$target/tmp"
rm -rf "$TMPDIR" && mkdir -p "$TMPDIR"
sdk=$HOME/wasi-sdk
mode=${1:-full}
log=$(mktemp)
# one result per (commit, gate version, mode): a commit that already passed this exact gate is not
# gated again by another session; GATE_FORCE=1 runs it anyway
key="$(git rev-parse HEAD).$(sha256sum "$0" | cut -c1-12).${mode#--}"
res=$results/$key.pass out=$(mktemp)
if [ -z "${GATE_FORCE:-}" ] && [ -f "$res" ]; then
  echo "[gate] $(git rev-parse --short=8 HEAD) already passed this gate ($mode) on $(hostname) at $(date -r "$res" '+%F %H:%M'); its result:"
  cat "$res"; exit 0
fi
say() { echo "$@"; echo "$@" >>"$out"; }
# room to work: a gate that runs out of disk fails tests that are not at fault, so check first
# a box close to full first trims idle lanes (cron does it every 30 min too)
[ -x /srv/trim_targets.sh ] && /srv/trim_targets.sh 60 >/dev/null 2>&1
free_gb() { df -Pk "$1" | awk 'NR==2 {print int($4 / 1048576)}'; }
# every box carries the same tools; a missing one fails here, not as a 0 s step later
for c in git python3 sqlite3 jq strace; do command -v "$c" >/dev/null || { echo "FAIL setup: $c is not installed on $(hostname)"; exit 2; }; done
# tests write to TMPDIR (on disk); /tmp only holds sockets and small files, so it needs less
for d in / "$TMPDIR"; do
  f=$(free_gb "$d")
  if [ "$f" -lt 8 ]; then echo "FAIL disk ($d has $f GB free; need 8): free space before trusting any test result"; exit 3; fi
done
f=$(free_gb /tmp); if [ "$f" -lt 1 ]; then echo "FAIL disk (/tmp has $f GB free; need 1)"; exit 3; fi
say "PASS disk (/: $(free_gb /) GB, /tmp: $(free_gb /tmp) GB free)"
fail=0
step() {
  local name=$1 t0=$SECONDS; shift
  if "$@" >"$log" 2>&1; then say "PASS $name ($((SECONDS - t0)) s)"; else say "FAIL $name ($((SECONDS - t0)) s)"; tail -40 "$log"; fail=1; fi
}
# this lane's venv with python[test] (pytest, PyYAML) and maturin, as the Python job installs them;
# a venv made before /srv/venv-epoch (a trim or a full disk may have cut it) is made again once
pyvenv() {
  if [ ! "$target/pyvenv/.ok" -nt /srv/venv-epoch ]; then rm -rf "$target/pyvenv"; /usr/bin/python3 -m venv "$target/pyvenv" || return 1; fi
  "$target/pyvenv/bin/pip" install -q -e "python[test]" maturin -c python/constraints.txt || return 1
  touch "$target/pyvenv/.ok"
}
# the Python job's tree checks (ci.yml "python"): committed images, charts and document heads are
# what their scripts write; the shipped licence texts are copies of the root ones; pyflakes
licences() {
  for f in LICENSE-APACHE LICENSE-MIT; do cmp "$f" "crates/scrutate/$f" && cmp "$f" "crates/scrutate-py/$f" && cmp "$f" "python/$f" || return 1; done
  python3 -c "import sys; v = open('integrations/vscode/LICENSE').read(); sys.exit(not all(open(f).read().strip() in v for f in ('LICENSE-MIT', 'LICENSE-APACHE')))"
}
# the bindings wheel built, installed and tested as the Python job does, in this lane's venv
bindings() {
  local b; b=$(realpath "$bin")
  pyvenv || return 1
  rm -rf "$target/wheels"
  "$target/pyvenv/bin/maturin" build --release --strip -m crates/scrutate-py/Cargo.toml -o "$target/wheels" || return 1
  "$target/pyvenv/bin/pip" install -q --force-reinstall --no-deps "$target"/wheels/scrutate-*.whl || return 1
  "$target/pyvenv/bin/python" -c "import scrutate; print(scrutate.__version__)" || return 1
  (cd python && SCRUTATE_BIN="$b" "$target/pyvenv/bin/python" -m pytest -q tests)
}
# tools/gate_parity.py, with PyYAML from python[test]
parity() { pyvenv && "$target/pyvenv/bin/python" tools/gate_parity.py; }
# ci.yml rust job "smoke test the binary"
smoke() {
  "$bin" --version && "$bin" grep -F "fn main" crates/scrutate/src >/dev/null && "$bin" --root crates/scrutate index &&
    "$bin" --root crates/scrutate search "parse a file into chunks" -k 3 && "$bin" --root crates/scrutate check
}
# ci.yml rust job "function order (Linux)": a link with a map, the order file made from it, a second
# link with that order, checked; the unordered link must fail the same check
function_order() {
  local order="$target/startup.order.elf"
  SCRUTATE_LINUX_ORDER=$order cargo build --release -p scrutate --locked || return 1
  cp "$order.map" "$target/unordered.map" || return 1
  python3 tools/order_file.py --elf "$order.map" "$order" || return 1
  SCRUTATE_LINUX_ORDER=$order cargo build --release -p scrutate --locked || return 1
  python3 tools/order_file.py --elf-check "$order.map" "$order" || return 1
  ! python3 tools/order_file.py --elf-check "$target/unordered.map" "$order" >/dev/null
}
# ci.yml pre-commit job: the hook from this commit, with pre-commit 4.7.0 in this lane's venv
pre_commit() {
  pyvenv && "$target/pyvenv/bin/pip" install -q pre-commit==4.7.0 || return 1
  PATH="$(dirname "$(realpath "$bin")"):$target/pyvenv/bin:$PATH" bash tools/test_precommit.sh "$PWD"
}
if [ "$mode" != "--bench" ]; then
  # every run step of the three workflows is in tools/gate_map.json, covered here or CI-only
  step gate-parity parity
  # security.yml's checks: the unsafe inventory in docs/SECURITY.md, cargo deny, zizmor
  [ -f tools/unsafe_count.py ] && step unsafe-count python3 tools/unsafe_count.py
  [ -f deny.toml ] && step cargo-deny "$tools/cargo-deny" --locked check
  step zizmor "$venv/bin/zizmor" --persona pedantic --offline .github
  step doc-assets bash -c 'python3 tools/gen_assets.py --check && python3 tools/gen_doc_assets.py --check && python3 bench/make_readme_charts.py --check'
  step licences licences
  step ruff "$venv/bin/ruff" check --select F .
  step fmt cargo fmt --all --check
  step clippy cargo clippy --workspace --all-targets --exclude scrutate-py --locked -- -D warnings
  step clippy-windows-gnu cargo clippy -p scrutate --all-targets --target x86_64-pc-windows-gnu --locked -- -D warnings
  # the bindings crate: CI builds it only inside the Python job (maturin), so a field added to an
  # Answer variant can break the wheel with every Rust step green
  step clippy-scrutate-py cargo clippy -p scrutate-py --all-targets --locked -- -D warnings
  step clippy-no-default-features cargo clippy -p scrutate --all-targets --no-default-features --locked -- -D warnings
  step wasm32-wasip1 env CC_wasm32_wasip1="$sdk/bin/clang" AR_wasm32_wasip1="$sdk/bin/llvm-ar" CFLAGS_wasm32_wasip1="--sysroot=$sdk/share/wasi-sysroot" \
    cargo build --release --target wasm32-wasip1 -p scrutate --no-default-features --locked
  # the repository holds no secret-shaped literal (GitHub push protection refuses such pushes);
  # gitleaks 8.30.1, the allow list is the repository's own .gitleaks.toml
  if [ -f .gitleaks.toml ]; then
    step secret-scan "$tools/gitleaks" dir . --no-banner --redact --config .gitleaks.toml
  else
    say "SKIP secret-scan (no .gitleaks.toml in this commit yet)"
  fi
  # a new item slipped between a doc comment and the item it documented: the whole range since the
  # release tip remote.py sends (refs/audit/release), so a shift in a middle commit that is still
  # there at the tip is caught; the tip alone otherwise
  rel=$(git rev-parse -q --verify refs/audit/release 2>/dev/null)
  if [ -n "$rel" ] && [ "$rel" != "$(git rev-parse HEAD)" ] && git merge-base --is-ancestor "$rel" HEAD; then step doc-shift python3 tools/check_doc_shift.py "$rel..HEAD"
  else step doc-shift python3 tools/check_doc_shift.py HEAD; fi
  msrv=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
  step "msrv-$msrv" env RUSTFLAGS= cargo "+$msrv" check -p scrutate --locked
fi
if [ "$mode" = "full" ]; then
  # the suites build with ThinLTO and 16 codegen units in their own target dir: fat LTO with one
  # unit relinks every test binary whole and was most of a gate's time; CI runs them with the real
  # release profile on every platform, and the release-build step below keeps it for the binary
  fast="env CARGO_TARGET_DIR=$target/fasttest CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16"
  step test $fast cargo test --release -p scrutate -p scrutate-ignore --locked --no-fail-fast
  step test-extra-langs $fast cargo test --release -p scrutate --features extra-langs --locked --no-fail-fast
  # ci.yml fuzz job's targets on stable: fuzz/ is a workspace of its own, which no clippy above
  # compiles; the nightly sanitizer build and the crash inputs stay CI's
  step fuzz-check env CARGO_TARGET_DIR="$target/fuzz" cargo check --manifest-path fuzz/Cargo.toml --bins
fi
if [ "$mode" != "--quick" ]; then
  # the Python job's bench steps (stdlib only), against this commit's release binary
  bin="$target/release/scrutate"
  py=$venv/bin/python3  # numpy for bench/evalcore.py
  step release-build cargo build --release -p scrutate --locked
  # the grammar parsers keep their jump tables (GCC 15 drops them under tree-sitter's O0 pragma)
  [ -f tools/jump_tables.py ] && step jump-tables python3 tools/jump_tables.py "$target/release"
  step smoke smoke
  step python-bindings bindings
  step quality-floor "$py" bench/quality.py "$bin"
  step yardstick-smoke env SCRUTATE_BIN="$bin" "$py" bench/yardstick.py --smoke
  step differential env SCRUTATE_BIN="$bin" "$py" bench/differential.py --cases 200 --seed 3
  step fuzz-grep "$py" bench/fuzz_grep.py --bin "$bin" --seconds 20 --seed 7
  step fuzz-incremental "$py" bench/fuzz_incremental.py --bin "$bin" --seconds 20 --seed 7
  step pre-commit pre_commit
  # last: it relinks the release binary twice, with the function order the second time
  step function-order function_order
fi
[ "$fail" = 0 ] && mkdir -p "$results" && cp "$out" "$res.tmp" && mv -f "$res.tmp" "$res"
rm -f "$log" "$out"
rm -rf "$TMPDIR"
exit $fail
