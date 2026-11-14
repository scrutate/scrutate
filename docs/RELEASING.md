<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-releasing.svg" alt="Releasing. Which steps does a release take, in order?" width="880">
</p>

# Releasing

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Contributing](../CONTRIBUTING.md) &nbsp;·&nbsp; Next: [Delivery index](DELIVERY.md)

**Contents**: [1. Version and date](#1-version-and-date) &nbsp;·&nbsp; [2. Benchmarks on the release binary](#2-benchmarks-on-the-release-binary) &nbsp;·&nbsp; [3. Generated documents](#3-generated-documents) &nbsp;·&nbsp; [4. Documents that carry the numbers by hand](#4-documents-that-carry-the-numbers-by-hand) &nbsp;·&nbsp; [5. Quality gates](#5-quality-gates) &nbsp;·&nbsp; [6. Platform checks and artifacts](#6-platform-checks-and-artifacts) &nbsp;·&nbsp; [7. What a published release adds](#7-what-a-published-release-adds)

<!-- End of head. -->

The steps of a release, in order, from what the 0.1.0 release touched (`git log --stat` on its
last commits) and what the scripts refuse. Tick a step once its output is read, not once its
command starts. No release is published yet (docs/ROADMAP.md), so the list ends at local
artifacts.

A change to `crates/scrutate` or `vendor/ignore` after step 2 goes back to step 2: benchmarks,
test results and artifacts all name one binary by its SHA-256.

## 1. Version and date

| Where | Holds | `python3 tools/check_version.py 0.1.0` checks it |
|---|---|---|
| `crates/scrutate/Cargo.toml`, `crates/scrutate-py/Cargo.toml` | version | yes |
| `vendor/ignore/Cargo.toml` and the `ignore` line of `crates/scrutate/Cargo.toml` | the version of `scrutate-ignore`, kept apart from scrutate's: raise it when `vendor/ignore` changed since it was last published, since crates.io never takes a version twice | no |
| `Cargo.lock` (both packages) | version | no |
| `integrations/vscode/package.json` and its lock file | version | yes |
| `integrations/gitlab/.gitlab-ci.yml`, `integrations/bitbucket/bitbucket-pipelines.yml` | version | yes |
| `packaging/homebrew/scrutate.rb` | version | yes (the URL) |
| `packaging/scoop/scrutate.json` | version | yes |
| the winget manifests and their directory name | version | yes |
| the examples in the GitHub Action and pre-commit READMEs | version | yes |
| the `CHANGELOG.md` heading, the title of `docs/DELIVERY.md`, the scope line of `docs/SECURITY.md` | date | no |

The check fails when a file differs from the version. The release workflow runs it first on the
tag (less its `v`) and refuses a tag that does not match.

The oldest compiler that builds is `rust-version` in the workspace `Cargo.toml`: 1.91, which the
locked `notify` and `notify-types` need. A `ci.yml` job runs `cargo check` with it, so a dependency update
that raises it fails there. README.md and docs/INSTALL.md name it too.

| Raise by one, after a release | When |
|---|---|
| `CONTENT_REVISION` in `crates/scrutate/src/index.rs` (in the index's `meta` as `content_rev`) | `langs.rs`, `parse.rs` or `text.rs` changes: an index of the older parsing rules reads every file again |
| `SCHEMA_VERSION` | the database layout changes |
| `MODEL_REVISION` | the embedded semantic model changes |
| the text index `FORMAT` | its files change |

0.1.0 ships with content revision 8: builds with earlier rules were in use before the release,
and their indexes must not keep the chunks, symbols and tokens those rules made (revision 3 joins
Japanese shinjitai to the Han fold, and adds Java fields as symbols; revision 4 makes a Pascal
statement that is a name alone a call; revision 5 reads a Free Pascal `.pp` unit as Pascal;
revision 6 reads a C++ header with a Doxygen `@class` as C++; revision 7 names the definition
after `let rec`, `let inline` and the like in the keyword tier; revision 8 takes only OCaml's and
F#'s own words after `let` for modifiers, so `let data x y` binds `data`). `tools/gen_doc_assets.py --check` fails when this number
differs from `CONTENT_REVISION`. The four only go up: a binary leaves an index of a higher number
alone, so two versions on one checkout never rebuild it in turn
([Architecture](ARCHITECTURE.md#storage-indexrs-lexrs)).

## 2. Benchmarks on the release binary

First move the previous results away: the harness resumes from an existing file and refuses one
whose rows another binary measured (`--force` with no `--only` drops those rows before saving the
first new one; `bench/compare_tools.py --only` refuses such a file too). Each goes to
`bench/results/<platform>/superseded/`, named with the first four hex digits of its binary, with a
row in that directory's `README.md` and an entry in `HISTORY` in `bench/make_dashboard.py`.

macOS, about 3.5 hours plus the wait for the binary to settle (at most an hour):

```sh
cargo build --release -p scrutate --locked && bash bench/run_macos.sh
```

- Before the build, on Apple silicon: `python3 tools/order_file.py --check` reports the share of
  names in `crates/scrutate/startup.order` the release links still have. Under 80 %, or after a
  toolchain or `Cargo.lock` change, write it again (`python3 tools/order_file.py --dirs
  ~/.cache/scrutate-bench/corpora/startup ~/.cache/scrutate-bench/corpora/ripgrep`, about 15
  minutes: two release links, each run under lldb from Xcode's command line tools) and commit it.
  On some machines a process started under lldb hangs; the tool gives up on a search after a
  minute and says so: run it again. A stale file only costs pages at start, never the build.
- On glibc Linux the same file orders the functions, with nothing to commit. Rust names carry a
  hash of how their crate was built, and it differs per target, so `release.yml` sets
  `SCRUTATE_LINUX_ORDER=OUT` and builds once: build.rs links with `-fuse-ld=lld` and `-B` the
  toolchain's `gcc-ld` directory (rustc's own way to reach its `rust-lld`, the default on x86_64
  since Rust 1.90) and writes the link map to `OUT.map`, since the binary is stripped.
  `python3 tools/order_file.py --elf OUT.map OUT` maps the file to that link's names (both sides
  demangled with `c++filt`, hashes dropped). The second build finds `OUT` and adds
  `--symbol-ordering-file` and `--no-warn-symbol-ordering`; lld passes over a name the link
  lacks. `--elf-check OUT.map OUT` fails the job unless the first 20 listed functions lie in list
  order. A local Linux release build is unordered unless it does the same; `tools/repro_docker.sh`
  does, so its hash is the ordered binary's. The musl builds link with GNU ld and stay unordered.
  `bench/competitors/linux_bench.sh` builds the same way, so the Linux benchmarks measure the
  ordered binary (the cross-built x86_64 one it is handed with `SCRUTATE_PREBUILT` is unordered).
- A just-built executable starts about 0.2 ms slower than the same file an hour later. The chain
  waits for that first: `bench/wait_aged.py` times `scrutate --version` every 2 minutes against a
  fresh copy of the binary (a new file, so slow) and goes on once the binary starts faster than
  the copy in two rounds in a row with a steady median, or after an hour. The chain log records
  each round.
- `bench/leaderboard.py` stops a row after its second block, at 20 runs (`--no-stop-early` turns it off), when its
  wall-time verdict and its four CPU and memory verdicts (default and `-j1`) are all PASS, FAIL
  or n/a at alpha 0.001 and no tool's block medians differ by more than 25%. Every other row
  takes all 30 runs and is judged at alpha 0.049. This is the Haybittle-Peto boundary (Haybittle
  1971, Peto et al. 1976), with the last look lowered so the alphas sum to 0.05 by Bonferroni
  whatever the load did between blocks: a 99.9% or 95.1% interval that clears 1 meets a 95%
  claim. Replayed over every recorded 30-run, 3-block file, it stopped 266 of 383 rows at 20 runs
  and changed no verdict; a look after one block flipped 5, since one block has no interleaving.
  R-14.10 in docs/TRACEABILITY.md defines a #1 over at least 20 runs per tool, 30 when a verdict
  does not hold at the first look, with the run count recorded per row. `SCRUTATE_BASE_BIN` turns
  it off, because the regression gate's block ratio needs every block of both binaries.
- Cold runs evict only the corpus files' pages on macOS (`bench/evict.c`, `msync` with
  `MS_INVALIDATE`, no root); Linux drops the whole page cache with `drop_caches`.
- No `cargo test` or `cargo build` until the chain ends: both can replace the binary.
- Do not edit `bench/run_macos.sh` while it runs: bash reads it as it goes.
- Close the browser and the editor. With 18 GB the page cache cannot hold the LLVM tree beside
  them, and a warm run stops being warm (the harness records it as `resident`).

Linux aarch64, about 1.5 hours, in a Colima VM with one virtual CPU per performance core
(`colima start --cpu 5 --memory 12`; with 8, three run on efficiency cores and every tool's
medians move by 10 to 30%). Run `bench/competitors/linux_bench.sh`, then
`bench/competitors/linux_indexed.sh`; the commands are in their headers. Colima shares only the
home directory with containers. The first script checks each tool it downloads against a SHA-256
in the script: a new tool version needs its new hash there, from the tool's own release page.
The VM shares the Mac, so it runs after the macOS chain, never beside it. Linux x86_64 runs on a
dedicated server (Hetzner AX) with the same two scripts, natively (`--platform linux/amd64`,
without `SCRUTATE_PREBUILT`), and can run at the same time as the macOS chain.

The yardstick (`bench/yardstick.py`) runs on Linux x86_64 against every rival at once, each
answer checked before it is timed. Build the image with every rival pinned
(`docker build -f bench/competitors/yardstick.Dockerfile -t scrutate-yardstick .`), or install
them under a user account with `bench/competitors/install_rivals.sh DIR`. claude-context and
grepai need Milvus and Ollama: `bench/competitors/claude-context.compose.yml` starts both, and its
header has the model pull. Then run `yardstick.py --corpus DIR --queries FILE --out FILE` for
W1 to W6, W9 and W10, and `yardstick.py --quality 200 --out FILE` for W8; `--only W4,W5` narrows
a run. The output records each rival's version and the machine. clangd needs a
`compile_commands.json` in the corpus, and gopls a Go corpus (`corpora.py --only hugo`).

Then read the results:

- Every row's status, and the rows marked "measured again".
- The regression gate measures the change and its base in one run. With
  `SCRUTATE_BASE_BIN=<the previous release binary>` set, `bench/leaderboard.py` times that binary
  as `scrutate base` in the same interleaved blocks as `scrutate` (never ranked or published), and
  `python3 bench/leaderboard_regression.py --within FILE.json` compares the two in that file. The
  ratio is the median over blocks of each block's median(new) / median(base), its interval from
  resampling inside each block. A row regresses when that 95% interval lies above 1.10. A slow
  minute slows both binaries, so two runs minutes apart are no gate. `--redo-unstable` refuses to
  continue a run whose `scrutate` or base binary is not the one it measured.
  `leaderboard_regression.py OLD.json NEW.json` still compares two recorded runs of one machine.
  `/usr/bin/time -l` gives instructions retired, which a busy machine does not move.
- `.github/workflows/benchmarks.yml` runs both gates on GitHub's runners, on every pull request,
  on Mondays and by hand. It builds the change and its base once (the pull request's merge base;
  by hand, the `base` input; on Mondays the last `v*` tag, else `HEAD~1`). The wall-time job runs
  the leaderboard as above on the corpora a runner measures in time (`ripgrep/`, `rgbuilt/`,
  `patho/`, `binheavy/`, `smallfiles/`, `bigfile/`, `startup/`), without competitors, with
  `--max-load "$(nproc)" --max-wait 30`: both binaries share every block, so load slows both
  alike. The instruction job runs every benchmark of the suite, `linux/` and `llvm/` included,
  once per binary under Valgrind's cachegrind (`bench/instructions.py`): text searches with
  `-j1`, tsquery with `RAYON_NUM_THREADS=1`, since Valgrind runs one thread at a time. A row
  regresses past 1% more instructions. The counts are "highly reproducible" (Valgrind's manual),
  where wall time on shared runners varies by 2.66% run to run, so 1% catches what the wall-time
  gate's 10% cannot. A pull request that changes nothing under `crates/`, `vendor/`, `bench/`,
  the Cargo files, `.cargo/`, `rust-toolchain.toml` or the workflow passes without measuring.
  The check to require on `main` is `benchmark gate`, which reads every job's result.
- To prove the gate, run the workflow by hand twice on `main`: `mode: same-binary` measures HEAD
  against a copy of itself and must read `same` on every row; `mode: planted` measures HEAD built
  at `opt-level` 1 against HEAD at 3 and passes only when both gates fail on it.

## 3. Generated documents

```sh
M=bench/results/macos-arm64 L=bench/results/linux-aarch64
python3 bench/make_leaderboard_md.py --engine-a $M/leaderboard-warm.json --engine-a $M/leaderboard-cold.json \
  --engine-a $L/leaderboard-warm.json --engine-a $L/leaderboard-cold.json --indexed $M/indexed.json --indexed $L/indexed.json
python3 bench/make_benchmarks_md.py $M/compare.json
python3 bench/make_dashboard.py
python3 bench/make_readme_charts.py      # the README's charts, from the same results (binary hash and date in the image)
python3 tools/gen_dependencies_md.py     # when Cargo.lock changed
python3 tools/gen_languages_md.py        # when a language changed
C=/some/scratch/demo && mkdir -p $C && git archive HEAD:crates/scrutate src | tar -x -C $C
python3 tools/gen_assets.py capture --bin target/release/scrutate --repo $C                     # the social preview's output (it indexes $C)
python3 tools/gen_assets.py capture-hero --bin target/release/scrutate --repo crates/scrutate   # the hero's searches and match spans
python3 tools/gen_assets.py              # the README's images, from the two captures, the palette and rust-version;
                                         # then render social-preview.png again (the command is in the script's head)
python3 tools/gen_doc_assets.py          # banners, diagrams and the hub; then --check: links, anchors, heads
python3 tools/scrub_home.py              # last: logs and results write ~ for the home directory
```

## 4. Documents that carry the numbers by hand

| Document | What to update |
|---|---|
| `README.md` | Status |
| `docs/FAQ.md` | speed, install, status |
| `docs/COMPETITIVE.md`, `docs/ROADMAP.md`, `CHANGELOG.md` | the numbers |
| `docs/TRACEABILITY.md` | R-4.6, R-14.6, R-14.16, then `python3 tools/traceability_tally.py` |
| `docs/RELEASE_READINESS.md` | its table lists every row that is not PASS |
| `docs/SECURITY.md` | the `unsafe` inventory and the crate count |
| `docs/DELIVERY.md` | binary hashes and wheel names, after step 6 |

## 5. Quality gates

In a virtual environment with the training code's and the tests' dependencies (versions in
`python/constraints.txt`) and the three wheels: `cp39-abi3`, `cp314t` and `cp315-abi3.abi3t`.

```sh
V=/some/scratch/venv W=/some/scratch/wheels
uv venv --python 3.13 $V && uv pip install --python $V/bin/python -e "python[test]" -c python/constraints.txt
export MACOSX_DEPLOYMENT_TARGET=13.0
python3 tools/third_party_notices.py --package scrutate-py --out crates/scrutate-py/THIRD_PARTY_NOTICES.txt
for py in 3.13 3.14t 3.15; do
  maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o $W -i "$(uv python find $py)"
done
uv pip install --python $V/bin/python --reinstall $W/*cp39-abi3*.whl
$V/bin/python tools/gen_test_results.py --differential-cases 2000 --reproducible --wheels $W
```

`cargo test` runs `tests/ranking_pins.rs`, which pins the features and scores of a small tree bit
for bit, and `bench/quality.py` checks ranking quality against `bench/quality/floor.json`
(docs/QUALITY.md). The parity runs against the Python reference engine (networkx, 21
repositories) ended with that engine; no gate needs networkx, `rich`, `mcp` or the benchmark
clones now. A new ranker or vector model that moves the measure records a new floor in the same
commit with `bench/quality.py target/release/scrutate --record`; `bench/quality.py --make` takes a
newer copy of the code as the corpus and writes its queries, then `--record`.

The wheel gate installs on 3.10 to 3.13, 3.14t, 3.15 and 3.15t. A final free-threaded 3.15 needs
uv 0.13 or newer; with an older uv, install one elsewhere
(`UV_PYTHON_INSTALL_DIR=/some/scratch uvx uv@latest python install 3.15t --no-bin`) and name every
interpreter: `--pythons 3.10 3.11 3.12 3.13 3.14t 3.15 /some/scratch/.../bin/python3.15t`.

It writes `docs/TEST_RESULTS.md`. Every gate must say PASS, the fuzz gates too (every target
builds and every input that once crashed one runs; they build in their own workspace, which takes
`scrutate-ignore` from `vendor/ignore` through `crates/scrutate`'s path dependency). The "reproducible build" hash must be the one docs/LEADERBOARD.md
names for macOS.

## 6. Platform checks and artifacts

```sh
actionlint && uvx zizmor --offline --persona pedantic .github   # the workflows, when one changed
docker run --rm -i --platform linux/arm64 -v "$PWD":/src:ro python:3.13-slim-bookworm \
  bash /src/tools/ci_rehearsal.sh | tee bench/results/platforms/linux-aarch64/ci-rehearsal.log
tools/repro_docker.sh                                   # one hash from two build images
docker run --rm --platform linux/arm64 -v "$PWD":/src:ro -v "$PWD/bench/results/platforms":/out \
  rust:1-slim-bookworm bash /src/tools/linux_verify.sh  # tests with no Python, musl, wheels
tools/linux_x86_cross_verify.sh ~/.cache/scrutate-bench/x86work
tools/windows_cross_check.sh
tools/macos_x86_verify.sh
tools/release_local.sh                                  # dist/ on macOS
uvx --with jsonschema --with pyyaml python packaging/validate_manifests.py --draft   # Scoop, winget
```

`--draft` lets the all-zero sha256 placeholders through; drop it once the manifests carry the
release checksums. The `.deb` and `.rpm` (`packaging/linux/build-packages.sh`) carry
`LICENSE-APACHE`, `LICENSE-MIT` and `THIRD_PARTY_NOTICES.txt` (in `/usr/share/doc/scrutate/` and
`/usr/share/licenses/scrutate/` respectively).

`dist-linux/` is `tools/release_local.sh` run in a clean `rust:1-slim-bookworm` container, with
`maturin[patchelf]` in a virtual environment, on a copy of the release commit at `/build`
(`git archive HEAD | tar -x -C <dir>`, never a copy of the working tree: the container has no
git, so the script packs `docs/` and `integrations/` as it finds them).

On each platform three hashes must agree: the binary in the archive, the one docs/LEADERBOARD.md
says was measured, and the reproducible-build hash (`tools/repro_docker.sh` for Linux,
docs/TEST_RESULTS.md for macOS). On glibc Linux all three are of the binary linked twice with the
function order (section 2), which changes its bytes. The archives contain `docs/`, so build them
last.

## 7. What a published release adds

A tag `v*` starts `.github/workflows/release.yml` (every platform, signing, attestations). It
checks the version, then runs all of `ci.yml` and `security.yml` on the tagged commit
(`workflow_call`); the release job waits for both runs and for the binaries and wheels. The package definitions in `packaging/`
need the release checksums (packaging/README.md). The two `-linux-gnu` binaries build on Ubuntu
22.04 runners, and a step fails when one asks for a glibc newer than 2.34. When GitHub retires
that image, build them in a container of that glibc or older, never on a newer runner (Ubuntu
24.04 asks for 2.39).

The release workflow has not run. A manual run (`gh workflow run release.yml`) builds, tests and
packages the binaries and the three wheel kinds on every platform, installs each wheel, and
publishes and signs nothing. It runs 8 binary jobs and 18 wheel jobs (six per wheel kind); 4
binary and 12 wheel jobs are on macOS and Windows runners, whose minutes count ten and two times
on a private repository.

A dry run rehearses the tag path before the first tag:

```sh
gh workflow run release.yml --ref main -f dry_run=true
gh run watch "$(gh run list --workflow release.yml -L 1 --json databaseId -q '.[0].databaseId')"
```

It takes the version from `crates/scrutate/Cargo.toml` and runs every job, `ci.yml` and
`security.yml` too. The sign-and-publish job runs in the `rehearsal` environment: it attests every
file (real, public attestations that name `main` and a `workflow_dispatch`, so a verifier pinned to
`refs/tags/v0.1.0` rejects them), makes a draft release `rehearsal-<run id>` with every file,
checks that the draft has them all and that `gh attestation verify` passes on `SHA256SUMS` for this
workflow and ref, and then deletes the draft. A draft makes no tag; the job deletes the tag if one
exists. The files stay as the run's artifacts for 7 days.

The repository is on GitHub, public, `main` only. Run 43013926055 of `ci.yml` (on `9384c18`) is
the first green one; all 12 jobs of a push passed:

| Kind | Jobs |
|---|---|
| Rust tests | macOS arm64, Intel macOS, Linux x86_64, Linux arm64, Windows x64, Windows arm64 |
| builds | the musl build; extra-langs on the oldest supported Rust |
| others | fuzz regressions; the Python reference, bindings, parity and differential job (now training code, bindings, differential and ranking quality); advisories (now `security.yml`'s `deny`); editor integrations |

The dependency review runs on pull requests only. The workflows pass `actionlint` and `zizmor`,
and the Linux jobs of `ci.yml` pass in a container (`tools/ci_rehearsal.sh`).

Before the first tag, set in the repository settings:

| Setting | Detail |
|---|---|
| ruleset on `main` | pull request, the `ci` checks required, no force push |
| tag ruleset | on `v*` |
| environment `release` | a required reviewer and the deployment rule "tags `v*`"; the publishing job waits for it |
| environment `rehearsal` | no reviewer, the `main` branch only; a dry run's sign-and-publish job runs in it (GitHub makes it on first use when it is missing, with no rule) |
| immutable releases, push protection, CodeQL default setup | |

The release job makes a draft, attaches every file, then publishes, the order immutable releases
need. Its notes are the version's section of `CHANGELOG.md`. The files are the archives, the
`.deb` and `.rpm` of the two `-linux-gnu` jobs (`packaging/linux/build-packages.sh`), the wheels, the VS Code extension (`scrutate-<version>.vsix`, packaged with `@vscode/vsce` 4.1.1),
the SBOM and `SHA256SUMS`.

Four publishing jobs follow the release job. Each does nothing until its setup exists, so a
release without them still succeeds:

| Job | Runs when | Setup (owner) |
|---|---|---|
| `testpypi` | the repository variable `PYPI_PUBLISH` is `true`; on a tag and on a dry run | a TestPyPI account with 2FA and a pending publisher: owner `scrutate`, repository `scrutate`, workflow `release.yml`, environment `testpypi`, project `scrutate` |
| `pypi` | `PYPI_PUBLISH` is `true`, on a tag only, after `testpypi` | the same on PyPI with environment `pypi` |
| `marketplace` | the variable `VSCE_PUBLISH` is `true`, on a tag | a Microsoft account, the Marketplace publisher `scrutate`, and on it a Trusted Publishing policy: owner `scrutate`, repository `scrutate`, workflow `release.yml`, environment `marketplace`. `vsce publish --oidc` exchanges the job's OIDC token; Azure DevOps global PATs retire on 2026-12-01 |
| `openvsx` | on a tag; it skips its steps unless the `openvsx` environment has the secret `OVSX_PAT` | an Eclipse account with the Publisher Agreement signed, an Open VSX access token, and the namespace (`npx ovsx create-namespace scrutate -p TOKEN`) |

crates.io is published by hand, after the release job, with the owner's account (an API token from
crates.io, scope `publish-new` the first time and `publish-update` after, in `cargo login`):

```sh
git checkout v0.1.0
cargo publish --dry-run -p scrutate-ignore -p scrutate   # both verify, scrutate against scrutate-ignore
cargo publish -p scrutate-ignore -p scrutate
```

Cargo uploads `scrutate-ignore` first, waits until crates.io serves it, then uploads `scrutate`.
The two uploads are not one step: if the run stops after `scrutate-ignore` is up, run
`cargo publish -p scrutate` alone. A version that is up cannot be uploaded again (`cargo yank`
only hides it), so a failed `scrutate` upload is fixed and retried under the same version, and a
second `scrutate-ignore` upload needs a new version.

The PyPI uploads use trusted publishing (`pypa/gh-action-pypi-publish` v1.14.3, which also makes
PEP 740 attestations); TestPyPI skips a file it already has, since a dry run sends the version
first. No PyPI document says whether PyPI takes the `cp315-abi3.abi3t` wheels. Its upload code
(`warehouse/forklift/legacy.py` on 2026-11-12) parses a wheel name with `packaging` 26.3, which
reads `abi3.abi3t` as two tags, and checks only the platform tag; a dry run's TestPyPI upload
settles it.

R-6.1, R-9.10 and R-12.2 in docs/TRACEABILITY.md are PASS from run 43013926055. R-6.12 and R-13.9
stay PARTIAL until `release.yml` and `benchmarks.yml` have run on a runner: flip them from the
recorded runs, not before.

`.github/FUNDING.yml` lists GitHub Sponsors and Buy Me a Coffee, both under `crisnahine`, and
shows the Sponsor button. The "Sponsor and hire" section of README.md links the same two pages; a
changed funding link goes in both.

After the release, take `open-pull-requests-limit: 0` out of `.github/dependabot.yml`. Version
updates pause until then because each Dependabot pull request runs `ci.yml` on six runners, and
on a private repository macOS minutes count ten times. A `rust-stemmers` bump can change stems:
merge it only when `tests/ranking_pins.rs` and `bench/quality.py` still pass, or with the pins
recorded again on purpose.
