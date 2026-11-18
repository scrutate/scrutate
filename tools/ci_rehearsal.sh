#!/usr/bin/env bash
# The Linux jobs of .github/workflows/ci.yml, command by command, in a clean container: the way to
# know they pass before a runner has ever seen them. It stops at the first failure.
#
#   docker run --rm -i -v "$PWD":/src:ro python:3.13-slim-bookworm bash /src/tools/ci_rehearsal.sh
#
# Not covered: the six-runner matrix (macOS, Windows), the VS Code test (it needs xvfb and a
# desktop's libraries; integrations/vscode/README.md) and anything that needs GitHub itself.
set -euo pipefail
step() { echo; echo "=== $* ==="; }
export DEBIAN_FRONTEND=noninteractive CARGO_TERM_COLOR=never
apt-get update -qq && apt-get install -y -qq curl build-essential git ripgrep sqlite3 musl-tools file xz-utils > /dev/null
curl -sSf --retry 5 --retry-all-errors https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal > /dev/null 2>&1
export PATH=$HOME/.cargo/bin:$PATH
mkdir -p /work && tar -C /src --exclude=./.claude --exclude=./target --exclude=./dist --exclude=./dist-linux --exclude=node_modules --exclude=./bench/results --exclude=.vscode-test -cf - . | tar -C /work -xf -
cd /work
arch=$(uname -m)
export RUSTFLAGS="-D warnings"

step "toolchain from rust-toolchain.toml"
rustup toolchain install --no-self-update 2>&1 | tail -1
rustc --version

step "rust job: fmt, clippy, test, build, smoke"
cargo fmt --all --check
cargo clippy --workspace --all-targets --exclude scrutate-py -- -D warnings 2>&1 | tail -1
cargo test --release -p scrutate --locked 2>&1 | grep -E "^test result|FAILED|panicked" | awk '/^test result/ {p+=$4; f+=$6; next} {print} END {print "tests passed:", p, "failed:", f}'
cargo build --release -p scrutate --locked 2>&1 | tail -1
./target/release/scrutate --version
./target/release/scrutate grep -F "fn main" crates/scrutate/src | head -2
./target/release/scrutate --root crates/scrutate index | tail -1
./target/release/scrutate --root crates/scrutate search "parse a file into chunks" -k 3 | head -3
./target/release/scrutate --root crates/scrutate check | tail -1

step "musl job"
rustup target add "$arch-unknown-linux-musl" 2>&1 | tail -1
cargo build --release -p scrutate --locked --target "$arch-unknown-linux-musl" 2>&1 | tail -1
m=target/$arch-unknown-linux-musl/release/scrutate
file "$m" | grep -qE "statically linked|static-pie linked"
if ldd "$m" 2>&1 | grep "=>" > /dev/null; then echo "the musl binary loads a shared library"; exit 1; fi
untimed() { sed -E 's/"elapsed_ms":[0-9.eE+-]+,?//'; }
./target/release/scrutate grep --sort --json -F "fn " crates | untimed > /tmp/glibc.jsonl
"$m" grep --sort --json -F "fn " crates | untimed > /tmp/musl.jsonl
cmp /tmp/glibc.jsonl /tmp/musl.jsonl && echo "identical: $(wc -l < /tmp/glibc.jsonl) lines"
python3 bench/musl_vs_glibc.py target/release/scrutate "$m" crates | tail -2

step "python job"
python3 --version
pip install -q --root-user-action=ignore -e "python[test]" maturin -c python/constraints.txt 2>&1 | tail -2
maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o target/wheels 2>&1 | tail -1
pip install -q --root-user-action=ignore --force-reinstall --no-deps target/wheels/scrutate-*.whl
python -c "import scrutate; print(scrutate.__version__)"
(cd python && SCRUTATE_BIN=/work/target/release/scrutate python -m pytest -q -rf tests 2>&1 | tail -6)
python bench/differential.py --cases 200 --seed 3 2>&1 | tail -2
python bench/fuzz_grep.py --bin target/release/scrutate --seconds 20 --seed 7 2>&1 | tail -2
python bench/fuzz_incremental.py --bin target/release/scrutate --seconds 20 --seed 7 2>&1 | tail -2
python bench/quality.py target/release/scrutate 2>&1 | tail -1

step "compat job"
cargo test --release -p scrutate --features extra-langs --locked 2>&1 | grep -E "^test result|FAILED|panicked" | awk '/^test result/ {p+=$4; f+=$6; next} {print} END {print "tests passed:", p, "failed:", f}'
msrv=$(sed -n 's/^rust-version = "\(.*\)"/\1/p' Cargo.toml)
rustup toolchain install "$msrv" --profile minimal --no-self-update 2>&1 | tail -1
RUSTFLAGS="" cargo "+$msrv" check -p scrutate --locked 2>&1 | tail -1

step "fuzz job"
export RUSTFLAGS="" NIGHTLY=nightly-2026-10-26
rustup toolchain install "$NIGHTLY" --profile minimal --no-self-update 2>&1 | tail -1
rustc "+$NIGHTLY" --version
cargo install cargo-fuzz --version 0.13.2 --locked 2>&1 | tail -1
for f in fuzz/regressions/*/*; do
  cargo "+$NIGHTLY" fuzz run -O "$(basename "$(dirname "$f")")" "$f" 2>&1 | tail -2
done

step "editors job: Neovim"
cd /tmp
a=x86_64 sum=474430d53e6264f6d6dd18db42d6dc9df3a1b56ca9e88a325bbf860e1a811d87
[ "$arch" = aarch64 ] && a=arm64 sum=8f1f64a0bdb97247034038c3823c6cbad5bdf9ecd5751b85494b71c3ee04c815
curl -sSfL --retry 5 --retry-all-errors -o nvim.tar.gz "https://github.com/neovim/neovim/releases/download/v0.12.6/nvim-linux-$a.tar.gz"
echo "$sum  nvim.tar.gz" | sha256sum -c -
tar xzf nvim.tar.gz
bash /work/integrations/fixture.sh repo
cd repo
"../nvim-linux-$a/bin/nvim" --headless -u NONE -l /work/integrations/neovim/test/run.lua /work/target/release/scrutate 2>&1 | tail -4
echo
echo "REHEARSAL PASSED"
