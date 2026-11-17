<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-install.svg" alt="Install. How do I install, update and remove scrutate?" width="880">
</p>

# Installation, update, uninstallation

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Questions](FAQ.md) &nbsp;·&nbsp; Next: [Command line](CLI.md)

<!-- End of head. -->

One static-friendly binary and an optional Python package. Nothing runs in the background or
phones home.

## Supported platforms (0.1.0)

A platform is supported only once its binary and wheel were built, installed and tested on it.
The logs each row names are in `bench/results/platforms/`.

<p align="center">
<img src="assets/diagram-platforms.svg" alt="Platforms: all six supported. macOS arm64, Linux aarch64 and Linux x86_64 built and tested here; macOS x86_64, Windows x86_64 and Windows ARM64 built and tested, wheels included, on GitHub runners." width="880">
<br><sub>A summary of the table below: all six platforms supported, three tested here and three on GitHub's runners.</sub>
</p>

| Platform | Binary | Wheels | Status | Evidence |
|---|---|---|---|---|
| macOS arm64 (Apple Silicon; macOS 13 or newer, tested on macOS 27) | built and tested natively | `cp39-abi3` on CPython 3.10, 3.11, 3.12, 3.13; `cp314t` on free-threaded 3.14 | **supported** | docs/TEST_RESULTS.md; `wheels-macos-arm64.json` |
| Linux aarch64 (glibc and static musl) | built and tested in a clean `rust:1-slim-bookworm` container (no Python on the image); the static musl binary prints byte-identical output | `cp39-abi3` on CPython 3.10, 3.12, 3.13; `cp314t` on free-threaded 3.14 | **supported** | `linux-aarch64/verify.log`, `musl-identical.txt`, `wheels.json` |
| Linux x86_64 (glibc and static musl) | the same script in a `linux/amd64` container under emulation on the Apple Silicon host (behaviour, not x86 speed). For the release commit gcc crashed under emulation, so the binary and every test binary were cross-built on arm64 and all 118 Linux tests run in an amd64 container (the other four of the 122 are for macOS on Apple silicon) | same as aarch64 (full run on an earlier commit) | **supported** | `linux-x86_64/cross-verify.log` (release commit); `verify.log`, `musl-identical.txt`, `wheels.json` (earlier full run) |
| macOS x86_64 | GitHub's `macos-15-intel` runner: the Rust tests and a smoke test of the release binary pass (CI runs 43013926055 and 43408907893); cross-built here, where the test binaries compile and link (no Rosetta 2 on the build machine) | `cp39-abi3` on CPython 3.13; `cp314t` on free-threaded 3.14, built, installed and tested on the runner (CI run 43408907893) | **supported** | `macos-x86_64/verify.log`; CI runs 43013926055 and 43408907893 |
| Windows x86_64 | GitHub's `windows-latest` runner: the Rust tests and a smoke test of the release binary pass (CI runs 43013926055 and 43408907893; five tests of files removed while open or of file names Windows forbids run on Unix only); cross-built here with MinGW-w64 | `cp39-abi3` on CPython 3.13; `cp314t` on free-threaded 3.14, built, installed and tested on the runner (CI run 43408907893) | **supported** | `windows-x86_64-cross/build.log`; CI runs 43013926055 and 43408907893 |
| Windows ARM64 | GitHub's `windows-11-arm` runner: the Rust tests and a smoke test of the release binary pass (CI runs 43013926055 and 43408907893); not built here | `cp39-abi3` on CPython 3.13; `cp314t` on free-threaded 3.14, built, installed and tested on the runner (CI run 43408907893) | **supported** | CI runs 43013926055 and 43408907893 |

## From a release archive

No archive is published for 0.1.0: the repository is public on GitHub, and the release workflow
has not run. Once releases exist:

```sh
v=0.1.0; t=x86_64-unknown-linux-gnu
curl -sSfLO "https://github.com/scrutate/scrutate/releases/download/v$v/scrutate-$v-$t.tar.gz"
curl -sSfLO "https://github.com/scrutate/scrutate/releases/download/v$v/scrutate-$v-$t.sha256"
sha256sum -c "scrutate-$v-$t.sha256"        # checksum
gh attestation verify "scrutate-$v-$t.tar.gz" --repo scrutate/scrutate \
  --signer-workflow scrutate/scrutate/.github/workflows/release.yml \
  --source-ref "refs/tags/v$v" --deny-self-hosted-runners   # Sigstore provenance (optional)
tar xzf "scrutate-$v-$t.tar.gz"
install -m 0755 "scrutate-$v-$t/scrutate" ~/.local/bin/scrutate
scrutate completions bash > ~/.local/share/bash-completion/completions/scrutate   # or zsh/fish/powershell
```

The attestation check passes only for a file that `release.yml` of `scrutate/scrutate` built
from the tag `v$v` on a GitHub-hosted runner, and it names the commit. With `--owner scrutate`
alone, a file built by any workflow of the organization would pass, a dry run's included. It says
where and how the file was built, not that its code is safe. The archives, `.deb`, `.rpm`, wheels,
the `.vsix` and `SHA256SUMS` on a release carry such an attestation (the per-archive `.sha256`
files and the SBOM file do not; `SHA256SUMS` lists the SBOM), and `--predicate-type
https://cyclonedx.org/bom` checks the SBOM attestation of an archive.

The `-linux-gnu` archives need glibc 2.34 or newer (Debian 12, Ubuntu 22.04, RHEL 9 and later);
the `-linux-musl` archives are static and run on any Linux.

Windows: unzip, put `scrutate.exe` on `PATH`, `scrutate completions powershell | Out-String | Invoke-Expression`.

Debian, Ubuntu, Fedora and RHEL (x86_64 and aarch64, glibc 2.34 or newer): the release has a
`.deb` and an `.rpm` per architecture, listed in `SHA256SUMS`. They come from no package
repository, so they do not update themselves: install the next release's file the same way.

```sh
sudo apt install ./scrutate_0.1.0-1_amd64.deb      # remove: sudo apt remove scrutate
sudo rpm -i --nosignature scrutate-0.1.0-1.x86_64.rpm   # remove: sudo rpm -e scrutate
```

The `.rpm` carries no GPG signature, which `dnf` refuses by default; the attestation and
`SHA256SUMS` are its integrity check.

### Unsigned binaries on macOS and Windows

The binaries carry no platform code signature: the macOS ones are not notarized by Apple (the
arm64 one has only the linker's ad-hoc signature) and the Windows ones have no Authenticode
signature. The checksum and the attestation above are the integrity check.

- macOS: Gatekeeper checks a file that carries the `com.apple.quarantine` attribute, which a
  browser download gets and a `curl` download does not. On a blocked file run
  `xattr -d com.apple.quarantine scrutate`, or open System Settings, Privacy & Security and
  choose "Open Anyway". Homebrew sets the attribute on cask downloads only; `scrutate` is a
  formula, poured from the tap's bottle or built from source, so a Homebrew install meets no
  Gatekeeper prompt.
- Windows: SmartScreen shows "Windows protected your PC" for a downloaded unsigned file, and the user
  must choose "Run anyway" (an enterprise policy can forbid it). With Smart App Control on
  (Windows 11), an unsigned executable with no reputation does not run at all, downloaded or not.

## From source (any platform with stable Rust 1.91 or newer)

```sh
git clone https://github.com/scrutate/scrutate && cd scrutate
cargo build --release -p scrutate --locked          # a few minutes; binary at target/release/scrutate
cargo test --release -p scrutate                     # the Rust test suite
```
`rust-toolchain.toml` pins the compiler (1.100.0) for reproducible builds; rustup installs it on
first use. `--features extra-langs` adds the optional heavy grammars (Fortran, OCaml, F#, Verilog,
VHDL, CUDA, TLA+, D, AutoHotkey, Odin, QML, LLVM IR; about 80 MB more). `.gitattributes` checks
text files out with LF line ends everywhere, so a Windows build's compiled-in data
(`crates/scrutate/data`) matches every other build.

With GCC 15 or newer, parsing is about 1.8 times slower unless the grammars are compiled with
`-fjump-tables`: their generated `parser.c` asks for `-O0`, and GCC 15 drops jump tables at
`-O0` (tree-sitter #5971). tree-sitter #5984 makes the generator write
`#pragma GCC optimize ("O0", "jump-tables")`, but of the grammars scrutate uses only tree-sitter-md
ships it so far, and the rest that set `-O0` still ship the old line with no newer crate: 22 in
the default build, 31 with `--features extra-langs`. A build in this repository adds the flag on
x86_64 and aarch64 Linux by setting `CFLAGS_x86_64_unknown_linux_gnu` and
`CFLAGS_aarch64_unknown_linux_gnu` in `.cargo/config.toml`. The `cc` crate joins `CFLAGS`,
`TARGET_CFLAGS` and the target's variables, so your own `CFLAGS` keeps the flag; only your own
`CFLAGS_x86_64_unknown_linux_gnu` (or the aarch64 one) takes its place, so add `-fjump-tables` to
that one if you set it. A build outside the repository (`cargo install`) does not read that file:
give it `CFLAGS=-fjump-tables`. The
x86_64 number is measured (Linux's C sources, GCC 15.2: 22 s against 12 s); aarch64 is from
tree-sitter #5971's report, not measured here. `python3 tools/jump_tables.py target/release` says
whether the built C grammar has its jump tables.

## Python package

```sh
pip install maturin
maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o dist
pip install dist/scrutate-*.whl
python -c "import scrutate; print(scrutate.__version__)"
```
Three wheels per OS and architecture cover every supported interpreter, and `pip` picks one:
`cp39-abi3` for standard CPython 3.9 to 3.14, `cp314t` for free-threaded 3.14 (which has no
stable ABI), and `cp315-abi3.abi3t` for 3.15 and newer, standard and free-threaded. The build
above makes the one for the interpreter that runs it; `-i python3.14t` names another. API, errors
and Ctrl-C: [Python bindings](../crates/scrutate-py/README.md).

## Connect your coding agent

`scrutate setup <claude|codex|opencode|cursor|gemini>` writes that agent's MCP server entry,
running this binary by its absolute path; `--project` writes the repository's file instead,
`--hook` adds Claude Code's Grep hook, and `--uninstall` takes back all setup added. Every option
and each agent's file: [Command line](CLI.md#setup-and-the-grep-hook).

## Updating

Replace the binary (same steps). Indexes are caches. A version with another index schema rebuilds
the index on first use (`index schema changed; rebuilding the index` is normal); one with another
bundled model keeps the index and makes its vectors anew on the next update; one that parses or
tokenizes differently reads every file once on its next update
(`index built with other parsing rules; every file is read again on the next update`).

## Uninstalling

```sh
scrutate setup claude --uninstall                                          # each agent you set up
rm ~/.local/bin/scrutate ~/.local/share/bash-completion/completions/scrutate
find ~/code -maxdepth 3 -name .scrutate -type d -prune -exec rm -rf {} +   # the caches, wherever you indexed
rm -rf ~/.config/scrutate                                                  # optional configuration
rm -rf ~/.local/state/scrutate                                             # read counts and the Grep hook's counter
rm -rf ~/.cache/scrutate                                                   # dependency sources symbol --deps copied
pip uninstall scrutate                                                     # the Python package, if installed
```
Scrutate writes nothing else: no services, no registry keys. Outside the repositories you indexed
it writes only your configuration, the agent files `scrutate setup` edits, its state
directory (`$XDG_STATE_HOME/scrutate/`, else `~/.local/state/scrutate/`, on Windows
`%LOCALAPPDATA%\scrutate\`, where `frecency.json` still goes under `$XDG_STATE_HOME` when that is
set) with `frecency.json` and `hook-grep.jsonl`, and its cache of dependency sources for
`symbol --deps`, `read --deps` and `scope: "deps"` (`$SCRUTATE_CACHE_DIR`, else
`$XDG_CACHE_HOME/scrutate/`, on Windows `%LOCALAPPDATA%\scrutate\`, on macOS
`~/Library/Caches/scrutate/`, else `~/.cache/scrutate/`).

## Package managers

None is published for 0.1.0. The package definitions are in `packaging/`: a Homebrew formula, a
Scoop manifest, winget manifests, and `.deb`/`.rpm` builds from `crates/scrutate/Cargo.toml`;
`packaging/README.md` says how each was tested and what is left for release time.

Once the tap `scrutate/homebrew-tap` and the bucket `scrutate/scoop-bucket` exist, these will
work. Homebrew 7 loads a tap only after it is trusted, so the first line is needed once:

```sh
brew trust --tap scrutate/tap
brew install scrutate/tap/scrutate     # update: brew upgrade scrutate
brew uninstall scrutate && brew untap scrutate/tap
```

```pwsh
scoop bucket add scrutate https://github.com/scrutate/scoop-bucket
scoop install scrutate/scrutate        # update: scoop update scrutate
scoop uninstall scrutate
```

The release workflow also uploads the wheels to PyPI and the extension to the VS Code
Marketplace and Open VSX once each is set up ([Releasing](RELEASING.md#7-what-a-published-release-adds)).
Then `pip install scrutate` (update `pip install -U scrutate`) and
`code --install-extension scrutate.scrutate` work; the extension runs the `scrutate` binary, which
is installed separately. Once the release is on crates.io, `cargo install scrutate --locked`
builds it from source with Rust 1.91 or newer and a C compiler (SQLite and the grammars are C); it
brings the walker it needs as the crate `scrutate-ignore`. From a checkout,
`cargo install --path crates/scrutate --locked` works.
