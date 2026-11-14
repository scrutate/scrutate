#!/usr/bin/env bash
# Build the release artifacts for the current platform without CI: binary tarball, wheels, SBOM and
# checksums. The release workflow does the same for every platform and adds Sigstore attestations.
set -euo pipefail
cd "$(dirname "$0")/.."
# .cargo/config.toml sets it for cargo; maturin reads it from the environment for the wheel tag
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.0}
v=$(grep -m1 '^version = ' crates/scrutate/Cargo.toml | cut -d'"' -f2)
target=$(rustc -vV | sed -n 's/^host: //p')
rm -rf dist && mkdir -p dist
cargo build --release -p scrutate --locked
# the binary must need only the operating system's own libraries
if command -v otool > /dev/null; then
  if otool -L target/release/scrutate | tail -n +2 | grep -v -E '^[[:space:]]+(/usr/lib/|/System/)'; then
    echo "target/release/scrutate links a library outside the system"; exit 1
  fi
elif command -v ldd > /dev/null; then
  if ldd target/release/scrutate | grep -v -E 'linux-vdso|ld-linux|lib(c|m|dl|pthread|rt|gcc_s)\.so'; then
    echo "target/release/scrutate links a library outside the system"; exit 1
  fi
fi
name="scrutate-$v-$target"
rm -rf "dist/$name" && mkdir -p "dist/$name"
cp target/release/scrutate LICENSE-APACHE LICENSE-MIT README.md CHANGELOG.md "dist/$name/"
python3 tools/third_party_notices.py --out "dist/$name/THIRD_PARTY_NOTICES.txt"
# tracked files only: a note or a .DS_Store left in the working tree must not ship
if git rev-parse --is-inside-work-tree > /dev/null 2>&1; then
  git archive HEAD docs integrations | tar -x -C "dist/$name"
else
  cp -r docs integrations "dist/$name/"
fi
tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "dist/$name"
# the installers (the GitHub action, the GitLab and Bitbucket templates) check the archive against this
(cd dist && sha256sum "$name.tar.gz" > "$name.sha256")
if command -v maturin >/dev/null; then
  for f in LICENSE-APACHE LICENSE-MIT; do
    cmp -s "$f" "crates/scrutate-py/$f" || { echo "crates/scrutate-py/$f (the wheel's copy) differs from $f"; exit 1; }
  done
  python3 tools/third_party_notices.py --package scrutate-py --out crates/scrutate-py/THIRD_PARTY_NOTICES.txt
  # one wheel per kind: cp39-abi3 (standard CPython up to 3.14), cp314t, abi3t (3.15 and newer)
  if command -v uv > /dev/null; then
    pys=()
    for pv in 3.12 3.14t 3.15; do
      py=$(uv python find "$pv") || { echo "no Python $pv for its wheel: uv python install $pv"; exit 1; }
      pys+=("$py")
    done
  else pys=(python3); fi
  for py in "${pys[@]}"; do maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o dist -i "$py"; done
fi
python3 tools/sbom.py --out "dist/scrutate-$v.cdx.json"
(cd dist && sha256sum -- *.tar.gz *.whl *.cdx.json > SHA256SUMS)
echo "artifacts:"; ls -la dist
