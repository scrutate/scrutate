#!/usr/bin/env bash
# Verify one Linux platform from a clean container: build and cargo test on an image with no Python
# at all, the static musl binary (statically linked, byte-identical output, timed against glibc),
# and the abi3, cp314t and abi3t wheels installed and tested on several interpreters.
#
#   docker run --rm --platform linux/arm64 -v "$PWD":/src:ro -v "$PWD/bench/results/platforms":/out \
#     rust:1-slim-bookworm bash /src/tools/linux_verify.sh
#
# Results go to /out/linux-<arch>/ (log, wheels, wheel-matrix JSON, musl checks and timing).
set -euo pipefail
arch=$(uname -m)
out=/out/linux-$arch
mkdir -p "$out"
exec > >(tee "$out/verify.log") 2>&1
step() { echo; echo "=== $* ($(date -u +%H:%M:%S))"; }

step "environment"
uname -a
rustc --version
cargo --version
nproc

step "copy source (the mount is read-only; host build output excluded)"
rm -rf /work && mkdir /work
tar -C /src --exclude=./.claude --exclude=./target --exclude=./dist --exclude='.scrutate' --exclude='__pycache__' -cf - . | tar -C /work -xf -
cd /work

step "cargo build + test on an image without Python"
if command -v python3 || command -v python; then echo "unexpected: the base image has Python"; exit 1; fi
cargo build --release -p scrutate --locked
cargo test --release -p scrutate --locked 2>&1 | grep -E '^test result|FAILED|panicked'

step "system packages"
apt-get update -qq
apt-get install -y -qq musl-tools file python3 ripgrep curl git > /dev/null
case "$arch" in
  x86_64) uvsum=5acfd9177cfecf0bf87578f059728215ccc884388988f55f7a65041b57387d91 ;;
  aarch64) uvsum=74f2f5c6024d397481db228db8fc25eed420db192243f88438c118e7e013e585 ;;
esac
curl -fsSL -o /tmp/uv.tgz "https://github.com/astral-sh/uv/releases/download/0.13.8/uv-$arch-unknown-linux-gnu.tar.gz"
echo "$uvsum  /tmp/uv.tgz" | sha256sum -c -
tar -C /usr/local/bin --strip-components=1 -xzf /tmp/uv.tgz
export PATH="$HOME/.local/bin:$PATH"

step "musl static binary"
musl="$arch-unknown-linux-musl"
rustup target add "$musl"
cargo build --release -p scrutate --locked --target "$musl"
m="target/$musl/release/scrutate"
file "$m" | tee "$out/musl-file.txt"
file "$m" | grep -qE "statically linked|static-pie linked"
if ldd "$m" 2>&1 | grep -q "=>"; then echo "musl binary has dynamic dependencies"; exit 1; fi
untimed() { sed -E 's/"elapsed_ms":[0-9.eE+-]+,?//'; } # the summary's run time differs on every run
for q in '-F fn ' '\w+::\w+\(' '--ident parseFile'; do
  # shellcheck disable=SC2086
  { target/release/scrutate grep --sort --json $q crates || [ $? -eq 1 ]; } | untimed > /tmp/glibc.jsonl
  # shellcheck disable=SC2086
  { "$m" grep --sort --json $q crates || [ $? -eq 1 ]; } | untimed > /tmp/musl.jsonl
  cmp /tmp/glibc.jsonl /tmp/musl.jsonl
  echo "identical output for [$q]: $(wc -l < /tmp/glibc.jsonl) lines, $(wc -c < /tmp/glibc.jsonl) bytes"
done | tee "$out/musl-identical.txt"
python3 bench/musl_vs_glibc.py target/release/scrutate "$m" crates --runs 41 | tee "$out/musl-timing.txt"

step "wheels (abi3, cp314t, abi3t)"
uv python install 3.10 3.12 3.13 3.14t 3.15 3.15t
uv tool install maturin
rm -rf target/wheels
maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o target/wheels -i "$(uv python find 3.12)"
maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o target/wheels -i "$(uv python find 3.14t)"
maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o target/wheels -i "$(uv python find 3.15)"
ls target/wheels
uv run --no-project --python 3.12 python tools/wheel_matrix.py --wheels target/wheels --cli "$PWD/target/release/scrutate" \
  --json "$out/wheels.json" 3.10 3.12 3.13 3.14t 3.15 3.15t | tee "$out/wheels.md"

step "done"
