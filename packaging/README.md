# Packaging

Package definitions for Scrutate 0.1.0. None is published: no release is tagged yet, so the
release URLs below do not exist. The test logs are in `bench/results/platforms/packages/`; the
crates.io figures below have none.

| Path | What it is |
|---|---|
| `homebrew/scrutate.rb` | Homebrew formula. Builds from the source tarball of tag `v0.1.0` with the Homebrew `rust`, installs bash, zsh and fish completions. |
| `homebrew/tap/` | The files of the tap repository `scrutate/homebrew-tap` as `brew tap-new --github-packages scrutate/tap` writes them with Homebrew 7.0.13 (its `tap-new-*.yml` templates; tap-new picks the autobump hour at random): `tests.yml` (`brew test-bot` on pushes and pull requests, bottles on pull requests), `publish.yml` (`brew pr-pull` by dispatch, bottles to GitHub Packages, attestations), `autobump.yml` (a daily `brew bump`), `dependabot.yml`, and a README with `brew trust --tap`. |
| `scoop/scrutate.json` | Scoop manifest for the Windows x86_64 and ARM64 release zips, with `checkver` and `autoupdate` (hashes from `SHA256SUMS`). |
| `scoop/bucket/` | The files of the bucket repository `scrutate/scoop-bucket`, from `ScoopInstaller/BucketTemplate` (commit `dd0ce8a`, 2026-08-24): Excavator (`excavator.yml`, every 4 hours, updates the manifest from `checkver` and `autoupdate`), the bucket tests (`ci.yml`), the `bin/` scripts with `auto-pr.ps1` pointed at `scrutate/scoop-bucket:main`, and a README. Left out: the issue and pull request handlers and the template's CRLF `.gitattributes`. |
| `winget/manifests/c/crisnahine/Scrutate/0.1.0/` | winget manifests (version, installer, default locale; schema 1.12.0). Portable install from the same zips. |
| `linux/build-packages.sh` | Builds the `.deb` (cargo-deb 3.8.0) and `.rpm` (cargo-generate-rpm 0.21.1) in a `rust:1-slim-bookworm` container, or with `TARGET` set from a binary already built for that target, as `release.yml` runs it. The package metadata is in `crates/scrutate/Cargo.toml` under `[package.metadata.deb]` and `[package.metadata.generate-rpm]`. |
| `linux/test-packages.sh` | Installs a package in a clean container, runs `scrutate --version` and a `scrutate grep`, removes it. |
| `validate_manifests.py` | Parses the Scoop and winget files, checks them against the published schemas and fails on an all-zero sha256 in any manifest. Until a release exists the hashes are placeholders, so use `--draft`: `uvx --with jsonschema --with pyyaml python packaging/validate_manifests.py --draft`. |

## How each was tested

All on 2026-11-09, Apple Silicon host, Docker through colima.

**Homebrew.** Installed from a `git archive --prefix=scrutate-0.1.0/` tarball of the source,
through a throwaway tap with the `url` swapped for `file://` and the real `sha256`.

- macOS (`homebrew-macos.log`): not possible here. `brew install --build-from-source` stops before
  building: Xcode 26.6 is installed and Homebrew requires Xcode 27 on macOS 27. Nothing of
  scrutate was installed (`brew test` installed 11 gems into Homebrew's own vendor bundle); the
  downloaded bottles were deleted and the tap removed.
- Linux (`homebrew-linux.log`): `homebrew/brew` arm64 container after `brew update`, Homebrew
  7.0.13. `brew install --build-from-source` built it with `cargo install` in 2 min 53 s,
  `scrutate --version` printed `scrutate 0.1.0`, the bash, zsh and fish completions were installed,
  `brew test` passed (version and a `scrutate grep`), `brew uninstall` removed it.

The formula logic is the same on both; the macOS run still needs a machine with a current Xcode
(or with Xcode removed and only the Command Line Tools).

**Scoop and winget** (`manifests-validate.log`): all four files parse and passed the published
schema (Scoop `schema.json`, winget 1.9.0, the version the manifests had then). The manifests and
`validate_manifests.py` now name winget 1.12.0; no run against 1.12.0 is logged. Not installed:
the release zips do not exist yet and no Windows machine was available.

**Debian and RPM.** `linux/build-packages.sh` in `rust:1-slim-bookworm` (log `build.log`) built
`scrutate_0.1.0-1_arm64.deb` (18.8 MB, `Depends: libc6 (>= 2.34)`) and
`scrutate-0.1.0-1.aarch64.rpm` (20.2 MB) into `dist/`. `linux/test-packages.sh` then passed in a
clean `debian:bookworm-slim` (`test-deb-debian-bookworm.log`) and a clean `fedora` 45
(`test-rpm-fedora.log`): install, `scrutate --version`, `scrutate grep` with the exact expected line
the script then had (`hello.py:1:def greet():`), uninstall, binary gone. The script now expects
`/tmp/t/hello.py:1:def greet():`; it has not been run since that change. The RPM is unsigned and Fedora 45 refuses unsigned packages by default, so
the test uses `rpm -i --nosignature`. x86_64 packages were not built.

**crates.io**: `cargo publish --dry-run -p scrutate-ignore -p scrutate` packages and verifies both
crates, the second against the first through Cargo's temporary local registry
(`bench/results/platforms/packages/cargo-package.log`). `scrutate-ignore` (`vendor/ignore`, a
modified copy of `ignore`) is 22 files, 83 KiB compressed; `scrutate` is 79 files (counted from
its `include` list) and 7.8 MiB compressed (8,181,927 bytes), of which the int8 semantic model is
most (crates.io refuses over 10 MB).
The `scrutate` package holds `src/`, `data/`, `models/`, `build.rs`, `startup.order`, the README and
the two licence files; its tests and their sample trees stay in the repository, so Cargo says it
ignores each test target. Nothing was published.

## At release time

1. Tag `v0.1.0` and let `.github/workflows/release.yml` publish the archives and `SHA256SUMS`.
2. Homebrew: create the public repository `scrutate/homebrew-tap` and commit the files of
   `homebrew/tap/` to its `main`, with `homebrew/scrutate.rb` as `Formula/scrutate.rb` and its
   `sha256` set to `shasum -a 256` of
   `https://github.com/scrutate/scrutate/archive/refs/tags/v0.1.0.tar.gz`, after
   `brew audit --strict --online scrutate/tap/scrutate`. Users run `brew trust --tap
   scrutate/tap` first. `autobump.yml` opens a pull request for each new tag; a pull request
   opened with `GITHUB_TOKEN` starts no workflow, so its tests run only when re-run by hand or
   when the tap gets a GitHub App token. homebrew-core takes no repository younger than 30 days,
   nor, when its own author submits it, one below 90 forks, 90 watchers or 225 stars.
3. Scoop: create the public repository `scrutate/scoop-bucket` (from `ScoopInstaller/BucketTemplate`,
   or empty), commit the files of `scoop/bucket/` and `scoop/scrutate.json` as
   `bucket/scrutate.json` with both `hash` values from `SHA256SUMS`, and in Settings, Actions,
   General allow all actions and keep "Read repository contents and packages permissions"
   (Excavator's workflow asks for `contents: write` itself). Later versions update through
   Excavator and `autoupdate`. ScoopInstaller/Extras wants an issue before a new manifest; Main
   needs 500 stars and 150 forks.
4. winget: set both `InstallerSha256` values from `SHA256SUMS`, run
   `winget validate --manifest packaging/winget/manifests/c/crisnahine/Scrutate/0.1.0` on Windows, then
   open a PR to microsoft/winget-pkgs (`wingetcreate submit` does both).
5. Debian and RPM: `release.yml` runs `linux/build-packages.sh` with `TARGET` set in the two
   `-linux-gnu` binary jobs (x86_64 and aarch64, Ubuntu 22.04 runners), on the binary those jobs
   built and tested, and attaches the four packages to the release, listed in `SHA256SUMS` and
   attested. The RPM is unsigned, so INSTALL.md installs it with `rpm -i --nosignature`; signing
   it (`rpmsign --addsign` in the release job) needs a GPG key held as an environment secret.
6. crates.io: `cargo publish -p scrutate-ignore -p scrutate` with the owner's crates.io token, after
   the tag ([Releasing](../docs/RELEASING.md#7-what-a-published-release-adds)).

The zero hashes in the Homebrew, Scoop and winget files are placeholders that fail any install
until replaced. Then `validate_manifests.py` without `--draft` must pass.
