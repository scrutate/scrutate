#!/usr/bin/env bash
# The yardstick's rivals (bench/yardstick.py), each pinned by version and checksum, installed under
# PREFIX without root: PREFIX/bin goes first on PATH. yardstick.Dockerfile runs this; so can a
# Linux x86_64 user account (needs curl, xz, zstd, a C/C++ toolchain, git and python3).
#   bench/competitors/install_rivals.sh /opt/rivals          # then: export PATH=/opt/rivals/bin:$PATH
# PREFIX/versions.json records what was installed; yardstick.py puts it in its output.
# livegrep is built by build_livegrep.sh (bazel); Milvus runs from claude-context.compose.yml.
set -euo pipefail
PREFIX=$(mkdir -p "$1" && cd "$1" && pwd)
mkdir -p "$PREFIX/bin" "$PREFIX/src"
cd "$PREFIX/src"
fetch() { # url sha256 file
  [ -f "$3" ] || curl -sSfL --retry 5 -o "$3" "$1"
  echo "$2  $3" | sha256sum -c - > /dev/null
}
unzip_to() { # zip dir: python's zipfile, as unzip is not everywhere (it keeps no file modes)
  python3 -c 'import sys, zipfile; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])' "$1" "$2"
}

fetch https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz \
  33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c rg.tar.gz
tar xzf rg.tar.gz && cp ripgrep-15.2.0-x86_64-unknown-linux-musl/rg "$PREFIX/bin/"

fetch https://github.com/Genivia/ugrep/archive/refs/tags/v7.8.5.tar.gz f080ab6cd9d96a5357570cb60a99e09ebe5eef11cf322c35373dbe815378a1f2 ugrep.tar.gz
tar xzf ugrep.tar.gz && (cd ugrep-7.8.5 && ./configure --prefix="$PREFIX" --disable-auto-color > /dev/null && make -j"$(nproc)" > /dev/null && make install > /dev/null)

fetch https://github.com/universal-ctags/ctags-nightly-build/releases/download/2026.10.08%2B82494361bb3e7d4f7cf1d2bdc0d0e9001c926e45/uctags-2026.10.08-linux-x86_64.release.tar.xz \
  e149d4f774cca9d6b657453f22fce97b94a5b691afaaa50f56f8564794839f87 uctags.tar.xz
tar xJf uctags.tar.xz && cp uctags-2026.10.08-linux-x86_64.release/bin/ctags uctags-2026.10.08-linux-x86_64.release/bin/readtags "$PREFIX/bin/"

fetch https://github.com/rust-lang/rust-analyzer/releases/download/2026-11-09/rust-analyzer-x86_64-unknown-linux-gnu.gz \
  8c039b25cb338b3970c731646677a4c9d4e9dd7a92c9ab9ae459c71467fbf4a9 ra.gz
gunzip -c ra.gz > "$PREFIX/bin/rust-analyzer" && chmod +x "$PREFIX/bin/rust-analyzer"

fetch https://github.com/ast-grep/ast-grep/releases/download/0.51.2/app-x86_64-unknown-linux-gnu.zip \
  40604c21c38d4c7581c56bdddeef277c56bf3c0113620aa96e78ae87e82366df ast-grep.zip
unzip_to ast-grep.zip ast-grep.d && cp ast-grep.d/ast-grep "$PREFIX/bin/" && chmod +x "$PREFIX/bin/ast-grep"

fetch https://github.com/clangd/clangd/releases/download/23.1.0/clangd-linux-23.1.0.zip \
  e53b1a96196095faedb7642cf64964f7fb9ad4a0c1f00dd2c172a3d9dcbafdfd clangd.zip
unzip_to clangd.zip "$PREFIX" && chmod +x "$PREFIX"/clangd_23.1.0/bin/* && ln -sf "$PREFIX/clangd_23.1.0/bin/clangd" "$PREFIX/bin/clangd"

fetch https://nodejs.org/dist/v26.13.1/node-v26.13.1-linux-x64.tar.xz 5c063e46bab0fbc1c17cc445d7ff129cbdcd0471be6ed83b6ab864f8143bc864 node.tar.xz
tar xJf node.tar.xz -C "$PREFIX" && for b in node npm npx; do ln -sf "$PREFIX/node-v26.13.1-linux-x64/bin/$b" "$PREFIX/bin/$b"; done
export PATH="$PREFIX/bin:$PATH"
npm install -g --prefix "$PREFIX/npm" --silent pyright@1.1.414 && ln -sf "$PREFIX/npm/bin/pyright" "$PREFIX/npm/bin/pyright-langserver" "$PREFIX/bin/"
npm install --prefix "$PREFIX/claude-context" --silent @zilliz/claude-context-core@0.1.16

fetch https://go.dev/dl/go1.27.4.linux-amd64.tar.gz 7226736c2339719cda28fbf3a54d71aa63bcefe376e98cef00034268dab24840 go.tar.gz
tar xzf go.tar.gz -C "$PREFIX"
# module downloads are checked against Go's checksum database
GOBIN="$PREFIX/bin" GOPATH="$PREFIX/gopath" CGO_ENABLED=0 "$PREFIX/go/bin/go" install golang.org/x/tools/gopls@v0.24.0
GOBIN="$PREFIX/bin" GOPATH="$PREFIX/gopath" CGO_ENABLED=0 "$PREFIX/go/bin/go" install \
  github.com/sourcegraph/zoekt/cmd/zoekt@a5a933bc043e github.com/sourcegraph/zoekt/cmd/zoekt-index@a5a933bc043e \
  github.com/sourcegraph/zoekt/cmd/zoekt-webserver@a5a933bc043e
ln -sf "$PREFIX/go/bin/go" "$PREFIX/bin/go"

fetch https://github.com/BeaconBay/ck/releases/download/0.8.1/ck-0.8.1-x86_64-unknown-linux-gnu.tar.gz \
  042db74e0ed10cd5995024bc16ea9928de6aaef632ef6463b3b8812a8d1ba1ac ck.tar.gz
mkdir -p ck.d && tar xzf ck.tar.gz -C ck.d && cp "$(find ck.d -type f -name ck -perm -u+x | head -1)" "$PREFIX/bin/ck"

fetch https://github.com/yoanbernabeu/grepai/releases/download/v1.2.0/grepai_1.2.0_linux_amd64.tar.gz \
  ea21d0c9324ee41b087b80b5ef528a11996941c712efdeeb9a6dddc8a4691dc6 grepai.tar.gz
tar xzf grepai.tar.gz -C "$PREFIX/bin" grepai

fetch https://github.com/ollama/ollama/releases/download/v0.40.16/ollama-linux-amd64.tar.zst \
  87bfafb2acbc89b2e6a1be0a9043de6aa75fb450696f86bc426e65fc448f788d ollama.tar.zst
mkdir -p "$PREFIX/ollama" && tar --zstd -xf ollama.tar.zst -C "$PREFIX/ollama" && ln -sf "$PREFIX/ollama/bin/ollama" "$PREFIX/bin/ollama"

cat > "$PREFIX/versions.json" << 'EOF'
{"ripgrep": "15.2.0", "ugrep": "7.8.5", "universal-ctags": "2026.10.08+82494361bb3e", "rust-analyzer": "2026-11-09", "ast-grep": "0.51.2",
 "clangd": "23.1.0", "node": "26.13.1", "pyright": "1.1.414", "claude-context-core": "0.1.16", "go": "1.27.4", "gopls": "0.24.0",
 "zoekt": "a5a933bc043e", "ck": "0.8.1", "grepai": "1.2.0", "ollama": "0.40.16", "embedding model": "nomic-embed-text"}
EOF
echo "rivals installed under $PREFIX (PATH=$PREFIX/bin:\$PATH; CLAUDE_CONTEXT_DIR=$PREFIX/claude-context)"
