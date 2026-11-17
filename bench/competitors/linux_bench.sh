#!/usr/bin/env bash
# Linux benchmark environment: every competitor at its latest stable release, installed the way
# its maintainers publish it (release binaries where they exist, source builds otherwise), then the
# Engine A leaderboard (warm and cold cache); linux_indexed.sh then runs the indexed comparison.
# On a dedicated x86_64 server (Hetzner AX) it runs natively, at the same time as bench/run_macos.sh;
# in the Colima VM on the Mac it runs after that chain, since both would share the Mac's cores.
#
# (for x86_64 on an arm64 host: --platform linux/amd64, its own volume, and
#  -e SCRUTATE_PREBUILT=/prebuilt/scrutate with the cross-built binary mounted at /prebuilt)
#   docker run --rm --privileged --platform linux/arm64 -v "$PWD":/src:ro \
#     -v "$HOME/.cache/scrutate-bench/corpora":/corpora:ro -v "$HOME/.cache/scrutate-bench/livegrep":/livegrep:ro \
#     -v scrutate-bench-linux:/bench -v "$PWD/bench/results":/results rust:1-slim-bookworm bash /src/bench/competitors/linux_bench.sh
#
# In a container the results can only say which container they ran in; to record the host as well
# (what makes a section read "bare metal"), write its facts on the host first and hand them in:
#   docker volume create scrutate-bench-linux
#   sudo python3 bench/host_facts.py "$(docker volume inspect -f '{{.Mountpoint}}' scrutate-bench-linux)" > host-facts.json
# and add to the docker run: -v "$PWD/host-facts.json":/host-facts.json:ro -e SCRUTATE_BENCH_HOST_FACTS=/host-facts.json
# (keys: kernel, virtualization, virtualization_source, cpu_governor, disk, cpu; see bench/host_facts.py).
#
# The corpora are copied into the container's own disk (/bench) so reads do not go through the
# host file-sharing layer; the cold-cache runs drop the guest page cache before every run.
set -euo pipefail
B=/bench
mkdir -p $B/bin $B/src
export PATH=$B/bin:$B/go/bin:$PATH
arch=$(uname -m)
step() { echo; echo "=== $* ($(date -u +%H:%M:%S))"; }

step "system packages"
apt-get update -qq
apt-get install -y -qq make g++ pkg-config file perl python3 python3-venv silversearcher-ag libpcre2-dev zlib1g-dev libbz2-dev liblzma-dev libzstd-dev liblz4-dev \
  libbrotli-dev unzip xz-utils curl git autoconf automake > /dev/null

fetch() { [ -f "$B/src/$2" ] || curl -fsSL -o "$B/src/$2" "$1"; }
# check NAME SHA256: the file must have that hash (a cached one too); a wrong one is not kept
check() { echo "$2  $B/src/$1" | sha256sum -c - || { rm -f "$B/src/$1"; return 1; }; }
fetch_sum() { fetch "$1" "$2" && check "$2" "$3"; }
if [ ! -x $B/bin/rg ]; then
  step "ripgrep 15.2.0 (release binary)"
  # its x86_64 Linux release is the musl build; aarch64 has a glibc one
  rgt=$arch-unknown-linux-gnu; [ "$arch" = x86_64 ] && rgt=x86_64-unknown-linux-musl
  case "$arch" in
    x86_64) sum=33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c ;;
    aarch64) sum=a740b91c82eaf9914cfedd353572f2791cbe0162c84101ee0951058f4dcbc90d ;;
  esac
  fetch_sum https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-$rgt.tar.gz rg.tgz "$sum"
  tar -C $B/src -xzf $B/src/rg.tgz && cp $B/src/ripgrep-15.2.0-$rgt/rg $B/bin/
fi
if [ ! -x $B/bin/hyperfine ]; then
  step "hyperfine 2.0.0 (release binary)"
  case "$arch" in
    x86_64) sum=ae2beda2ac99c098427e4f755244552daf20e514422889cca6d44421e2d93c87 ;;
    aarch64) sum=f8244d1e17f8da23ee6b3ae2cdc04a6a185a450f6b74e09bff12e498038fcbc9 ;;
  esac
  fetch_sum https://github.com/sharkdp/hyperfine/releases/download/v2.0.0/hyperfine-v2.0.0-$arch-unknown-linux-gnu.tar.gz hf.tgz "$sum"
  tar -C $B/src -xzf $B/src/hf.tgz && cp $B/src/hyperfine-v2.0.0-$arch-unknown-linux-gnu/hyperfine $B/bin/
fi
if [ ! -x $B/bin/ast-grep ]; then
  step "ast-grep 0.51.3 (release binary)"
  case "$arch" in
    x86_64) sum=22803eb0c5bf83164fb7fd76d4721d355ecf0c8466ef47342c1ec7d80897ca47 ;;
    aarch64) sum=5b44fd68e78744934b359b07badce347d4197ed50b341d50f1f99d056ecedb08 ;;
  esac
  fetch_sum https://github.com/ast-grep/ast-grep/releases/download/0.51.3/app-$arch-unknown-linux-gnu.zip sg.zip "$sum"
  unzip -o -q $B/src/sg.zip -d $B/src/sg && cp $B/src/sg/ast-grep $B/bin/
fi
if [ ! -x $B/bin/ugrep ]; then
  step "ugrep 7.8.5 (source, ./build.sh as its README says)"
  fetch_sum https://github.com/Genivia/ugrep/archive/refs/tags/v7.8.5.tar.gz ugrep.tgz f080ab6cd9d96a5357570cb60a99e09ebe5eef11cf322c35373dbe815378a1f2
  tar -C $B/src -xzf $B/src/ugrep.tgz && (cd $B/src/ugrep-7.8.5 && ./build.sh > /dev/null 2>&1 && cp bin/ugrep $B/bin/)
fi
if [ ! -x $B/bin/grep ]; then
  step "GNU grep 3.12 (source)"
  fetch_sum https://ftp.gnu.org/gnu/grep/grep-3.12.tar.xz grep.txz 2649b27c0e90e632eadcd757be06c6e9a4f48d941de51e7c0f83ff76408a07b9
  tar -C $B/src -xJf $B/src/grep.txz && (cd $B/src/grep-3.12 && ./configure -q --prefix=$B > /dev/null && make -s -j"$(nproc)" > /dev/null && cp src/grep $B/bin/)
fi
if [ ! -x $B/bin/git ]; then
  # git grep is timed: Debian's git is older than the latest release, so it is built as GNU grep is
  step "git 2.56.1 (source, with PCRE2 for git grep -P)"
  fetch_sum https://www.kernel.org/pub/software/scm/git/git-2.56.1.tar.xz git.txz f0b0bf89264b9bd2e1c640363e7529643989da76281600473c50745cd34a607f
  tar -C $B/src -xJf $B/src/git.txz && (cd $B/src/git-2.56.1 && make -s -j"$(nproc)" prefix=$B USE_LIBPCRE2=1 NO_CURL=1 NO_EXPAT=1 NO_GETTEXT=1 \
    NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 NO_OPENSSL=1 install > /dev/null)
fi
if [ ! -x $B/bin/ack ]; then
  step "ack 3.10.0 (standalone file from beyondgrep.com)"
  # the hash is the one Homebrew's formula pins for this file, so the file is checked whatever the
  # state of the site's certificate (it expired on 2026-11-06 and curl then refuses the site)
  fetch https://beyondgrep.com/ack-v3.10.0 ack || curl -fsSLk -o $B/src/ack https://beyondgrep.com/ack-v3.10.0
  check ack 28a740de59bdb52000a281aa89dc9d44a24a1f144cdfe4bc24b7d0582b40e90e
  cp $B/src/ack $B/bin/ack && chmod +x $B/bin/ack
fi
if [ ! -x $B/go/bin/zoekt ]; then
  step "Go and Zoekt (go install, as its README says)"
  goarch=${arch/aarch64/arm64}
  case "$arch" in
    x86_64) sum=7226736c2339719cda28fbf3a54d71aa63bcefe376e98cef00034268dab24840 ;;
    aarch64) sum=3392582988a273a87ab5043ee1b2c9c17a2c3c00580bf9326c47a2e0aaaeac92 ;;
  esac
  fetch_sum "https://go.dev/dl/go1.27.4.linux-${goarch/x86_64/amd64}.tar.gz" go.tgz "$sum"
  tar -C $B -xzf $B/src/go.tgz
  GOPATH=$B/go GOFLAGS=-mod=mod $B/go/bin/go install github.com/sourcegraph/zoekt/cmd/zoekt-index@33ce1d04d373 github.com/sourcegraph/zoekt/cmd/zoekt@33ce1d04d373
  mv $B/go/bin/zoekt $B/go/bin/zoekt-index $B/bin/ 2>/dev/null || true
fi
ln -sf $B/bin/zoekt $B/bin/zoekt-index $B/go/bin/ 2>/dev/null || true
if ! command -v uv > /dev/null; then
  step "uv 0.13.9 (release binary; the structural benchmark's reference runs tree-sitter's Python bindings under it)"
  case "$arch" in
    x86_64) sum=a7fbaff2b9a271d85c1bdb540941090fd1ce011797b4abeb48e4dcd39c2471d5 ;;
    aarch64) sum=0797542018e541a9288855df555c8077eed06f7a47d206362030480e8ecc9f3d ;;
  esac
  fetch_sum https://github.com/astral-sh/uv/releases/download/0.13.9/uv-$arch-unknown-linux-gnu.tar.gz uv.tgz "$sum"
  tar -C $B/src -xzf $B/src/uv.tgz && cp $B/src/uv-$arch-unknown-linux-gnu/uv $B/src/uv-$arch-unknown-linux-gnu/uvx $B/bin/
fi

step "scrutate (release build)"
git config --global --add safe.directory '*'
rm -rf $B/scrutate && mkdir -p $B/scrutate
tar -C /src --exclude=./.claude --exclude=./target --exclude=./dist --exclude=.scrutate --exclude=./fuzz -cf - . | tar -C $B/scrutate -xf -
bash /src/bench/competitors/built_from.sh write /src $B/scrutate
if [ -n "${SCRUTATE_PREBUILT:-}" ]; then
  # x86_64 on an arm64 host: gcc crashes on the grammar sources under emulation, so the binary is
  # cross-built first (tools/linux_x86_cross_verify.sh) and handed in
  mkdir -p $B/scrutate/target/release && cp "$SCRUTATE_PREBUILT" $B/scrutate/target/release/scrutate
else
  # linked twice with the function order, as release.yml links the glibc binaries
  (cd $B/scrutate && export SCRUTATE_LINUX_ORDER=$B/scrutate/target/startup.order.elf \
    && cargo build --release -p scrutate --locked -q \
    && python3 tools/order_file.py --elf "$SCRUTATE_LINUX_ORDER.map" "$SCRUTATE_LINUX_ORDER" \
    && cargo build --release -p scrutate --locked -q \
    && python3 tools/order_file.py --elf-check "$SCRUTATE_LINUX_ORDER.map" "$SCRUTATE_LINUX_ORDER" > /dev/null)
fi
export SCRUTATE_BIN=$B/scrutate/target/release/scrutate ZOEKT_BIN_DIR=$B/bin

step "corpora onto the container disk"
mkdir -p $B/corpora
for c in linux linux-nodot ripgrep; do
  [ -d $B/corpora/$c ] || cp -a /corpora/$c $B/corpora/
done
cp /corpora/manifest.json $B/corpora/
if [ -n "${SETUP_ONLY:-}" ]; then step "set up; no benchmark asked for"; exit 0; fi

bash /src/bench/competitors/built_from.sh check /src $B/scrutate
# a corpus copied by an earlier run is kept, so check it is still the tree the manifest pins
python3 /src/bench/corpora.py --root $B/corpora --only linux ripgrep --verify
out=/results/linux-$arch
mkdir -p $out
cd $B/scrutate
warm=(--corpora $B/corpora --out $out/leaderboard-warm.json --only linux/ --max-load "$(nproc)" --no-stop-early)
cold=(--corpora $B/corpora --out $out/leaderboard-cold.json --cache cold --cold-prepare 'sync; echo 3 > /proc/sys/vm/drop_caches'
  --only 'linux/literal' 'linux/case-insensitive' 'linux/complex regex' 'linux/no match' --runs 30 --max-load "$(nproc)" --no-stop-early)
step "Engine A leaderboard, warm cache (linux corpus)"
python3 bench/leaderboard.py "${warm[@]}"
step "Engine A leaderboard, cold cache (linux corpus subset)"
python3 bench/leaderboard.py "${cold[@]}"
step "benchmarks disturbed by other work on the machine, measured again once"
python3 bench/leaderboard.py "${warm[@]}" --redo-unstable
python3 bench/leaderboard.py "${cold[@]}" --redo-unstable
# the indexed comparison runs next, in an ubuntu:24.04 container: bench/competitors/linux_indexed.sh
step "done"
