#!/bin/sh
# Builds the .deb and .rpm into dist/. Run from the repository root, in rust:1-slim-bookworm:
#   docker run --rm -v "$PWD":/src -w /src rust:1-slim-bookworm sh packaging/linux/build-packages.sh
# With TARGET set it packages the release binary already at $CARGO_TARGET_DIR (or target)/$TARGET/
# release/scrutate and installs nothing with apt (release.yml, on the -linux-gnu runners).
set -eux
cargo install --locked -j 3 cargo-deb@3.8.0 cargo-generate-rpm@0.21.1
if [ -n "${TARGET:-}" ]; then
  out=${CARGO_TARGET_DIR:-target}/$TARGET
  set -- --target "$TARGET"
else
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends dpkg-dev python3 > /dev/null
  # a separate target directory keeps the Linux build apart from the host's
  export CARGO_TARGET_DIR=/src/target/linux
  cargo build --release --locked -j 3 -p scrutate
  out=$CARGO_TARGET_DIR
fi
python3 tools/third_party_notices.py --out crates/scrutate/THIRD_PARTY_NOTICES.txt
cargo deb -p scrutate --no-build "$@"
cargo generate-rpm -p crates/scrutate "$@"
mkdir -p dist
cp "$out"/debian/*.deb "$out"/generate-rpm/*.rpm dist/
ls -l dist
