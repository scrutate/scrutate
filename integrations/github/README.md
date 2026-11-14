<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="../../docs/assets/banner-github.svg" alt="GitHub Actions. How do I run scrutate in a GitHub workflow?" width="880">
</p>

# GitHub Actions

[Documentation](../../docs/README.md) &nbsp;·&nbsp; Previous: [JetBrains IDEs](../jetbrains/README.md) &nbsp;·&nbsp; Next: [pre-commit hook](../pre-commit/README.md)

<!-- End of head. -->

The action in `.github/actions/scrutate` downloads the release binary for the runner (Linux,
macOS or Windows; x86_64 or arm64), checks the release's checksum, puts it on `PATH` and runs one
command:

```yaml
jobs:
  duplicates:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: scrutate/scrutate/.github/actions/scrutate@v0.1.0
        with:
          version: v0.1.0
          args: index
      - id: dupes
        uses: scrutate/scrutate/.github/actions/scrutate@v0.1.0
        with:
          version: v0.1.0
          args: dupes --min-tokens 80 --threshold 1
      - name: no exact duplicate functions
        if: steps.dupes.outputs.exit-code == '0'
        run: |
          echo "exact duplicates are listed above"
          exit 1
```

Exit codes are 0 for results, 1 for none and 2 for an error. The step fails only on an error; on
0 and 1 it passes and puts the code in the `exit-code` output, so the job decides what a result
means. Above, duplicates (`0`) fail the job and none (`1`) pass it. For `search` you would gate
the other way round, on `exit-code == '1'`.

`args` is split on spaces; quote an argument with spaces or an apostrophe
(`search "don't parse twice" -k 5`). A backslash is kept as written (`grep -c \bTODO\b .`,
`--root C:\repo`); one before a blank, a quote or another backslash still escapes it, as in
xargs (`foo\ bar`, `don\'t`). Later steps can call `scrutate` directly, since the first step
put it on `PATH`. Without `args` the action runs no command and `exit-code` is empty.

## A review comment on each pull request

With `comment: true`, on a pull request the action posts
[`scrutate review`](../../docs/CLI.md#review) as one comment: the changed definitions, their
callers with distances, the test files to run and the duplicates the pull request adds. Each
later push edits that comment in place, so a pull request keeps one comment. The job needs
`pull-requests: write`, and the checkout needs the whole history so the base commit is there:

```yaml
on: pull_request
permissions: {}
jobs:
  review:
    runs-on: ubuntu-latest
    permissions:
      pull-requests: write # post and edit the review comment
    steps:
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0
          persist-credentials: false
      - uses: scrutate/scrutate/.github/actions/scrutate@v0.1.0
        with:
          version: v0.1.0
          comment: true
```

| Input | Default | What it does |
|---|---|---|
| `version` | (required) | the release tag to install, such as `v0.1.0` |
| `args` | none | the command to run, as above |
| `comment` | `false` | `true` posts or edits the review comment on a pull request |
| `github-token` | `github.token` | the token the comment is posted with |

The comment step runs `.github/actions/scrutate/comment.sh`. It compares the checked-out commit
with the pull request's base commit, lists the pull request's comments with `gh api`, and edits
the first one that starts with the marker `<!-- scrutate-review -->` and was written by a bot (or,
with a person's token in `github-token`, by that person), or else posts a new one. It never fails
the job: outside a pull request, without the base commit, when `review` fails or when GitHub
refuses the call (a pull request from a fork gets a read-only token), it prints a warning and the
step passes. Each value it uses (pull request number, base commit, repository name) comes through
the step's environment, is checked against its shape (a number, a commit id, `owner/name`) and is
used only in quotes, so no input reaches a shell. `actionlint` and `zizmor --persona pedantic`
report nothing on the action. On the workflow above, pedantic `zizmor` asks only for what any
workflow needs: actions pinned by commit (as this repository pins its own), a job `name` and a
`concurrency` group.

The action lives in a subdirectory, so it is not on the Marketplace.

The `action` job in `.github/workflows/ci.yml` runs the action of each commit on all six runners
(Linux, macOS and Windows; x86_64 and arm64). Before a release exists, `tools/fake_release.sh` packs
the CI binary as `release.yml` packs a release and makes `curl` serve it from the release URLs, so
the checksum check, the unpack, `PATH` and the `exit-code` output all run for real. The review
comment is tested against a stand-in `gh` in `crates/scrutate/tests/review.rs`.
