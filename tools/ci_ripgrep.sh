#!/usr/bin/env bash
# ripgrep 15.2.0, checked against its pinned SHA-256, on PATH for the rest of a GitHub Actions job,
# with SCRUTATE_TEST_NEEDS_RG set so a test that compares with rg fails rather than skips when it
# is missing. Every job that runs the Rust tests, or compares with rg, calls this one copy.
#   bash tools/ci_ripgrep.sh
set -euo pipefail
case "$RUNNER_OS-$RUNNER_ARCH" in
  Windows-X64) t=x86_64-pc-windows-msvc; ext=zip; sum=71b2fef860abe467217a538ff31de02f5258807c0129f771846f87bd029aafc5 ;;
  Windows-ARM64) t=aarch64-pc-windows-msvc; ext=zip; sum=e4abca10c3a64ebea742667dd7009449d49403db5460dd6873e389fa2945360f ;;
  Linux-X64) t=x86_64-unknown-linux-musl; ext=tar.gz; sum=33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c ;;
  Linux-ARM64) t=aarch64-unknown-linux-gnu; ext=tar.gz; sum=a740b91c82eaf9914cfedd353572f2791cbe0162c84101ee0951058f4dcbc90d ;;
  macOS-ARM64) t=aarch64-apple-darwin; ext=tar.gz; sum=3750b2e93f37e0c692657da574d7019a101c0084da05a790c83fd335bad973e4 ;;
  macOS-X64) t=x86_64-apple-darwin; ext=tar.gz; sum=af7825fcc69a2afc7a7aea55fc9af90e26421d8f20fe59df32e233c0b8a231c1 ;;
  *) echo "no pinned ripgrep for $RUNNER_OS-$RUNNER_ARCH"; exit 1 ;;
esac
cd "$RUNNER_TEMP"
# schannel fails the download when it cannot reach the revocation server; the SHA-256 below checks the archive
nr=; if [ "$RUNNER_OS" = Windows ]; then nr=--ssl-no-revoke; fi
curl -sSfL --retry 5 --retry-all-errors ${nr:+"$nr"} -o "rg.$ext" "https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-$t.$ext"
# macOS has shasum, not sha256sum
if command -v sha256sum > /dev/null; then echo "$sum  rg.$ext" | sha256sum -c -; else echo "$sum  rg.$ext" | shasum -a 256 -c -; fi
if [ "$ext" = zip ]; then unzip -q "rg.$ext"; else tar xzf "rg.$ext"; fi
echo "$RUNNER_TEMP/ripgrep-15.2.0-$t" >> "$GITHUB_PATH"
echo "SCRUTATE_TEST_NEEDS_RG=1" >> "$GITHUB_ENV"
