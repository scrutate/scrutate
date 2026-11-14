<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="docs/assets/banner-contributing.svg" alt="Contributing. What must pass before a change is merged?" width="880">
</p>

# Contributing

[Documentation](docs/README.md) &nbsp;·&nbsp; Next: [Releasing](docs/RELEASING.md)

<!-- End of head. -->

## Licence of contributions

Scrutate is and stays open source, under MIT or Apache-2.0 at the user's choice (LICENSE-MIT,
LICENSE-APACHE). Contributions are accepted under the
[contributor licence agreement](CLA.md): you keep the copyright and give the project a licence
that also lets the maintainer offer other terms (a commercial licence, for example); what it
accepts stays available under the open source licence it had that day, and is offered to everyone
under the same two licences. You agree once, by ticking the line in the pull request form.

The name and the logo are covered by the [trademark policy](TRADEMARKS.md): a fork with changes
beyond packaging takes its own name.

## Layout

```
crates/scrutate/        the product: Engine A (instant.rs, source.rs, policy.rs), Engine B
                        (index.rs, stores.rs, textindex.rs, lex.rs, parse.rs, langs.rs, names.rs,
                        resolve/, search.rs, semantic.rs, rank.rs, models.rs, intel.rs, impact.rs,
                        review.rs),
                        operations and sessions (ops.rs, session.rs), the file finder
                        (finder.rs), the structural rewrite (rewrite.rs), CLI (cli.rs, config.rs),
                        MCP server (mcp.rs), language server (lsp.rs), agent set-up and
                        the Grep hook (setup.rs, hook.rs); tests/ (samples,
                        robustness, properties, ranking pins, CLI, MCP, LSP and more);
                        models/ (semantic.npz, ranker.npz, embedded into the binary)
crates/scrutate-py/     optional PyO3 bindings (not built by default)
python/scrutate_ref/    the models' training code (ranker export, vector model tables)
python/tests/           tests of the training code, the benchmark builders and the bindings
bench/                  benchmark harness, differential tests, ranking quality, training scripts,
                        the agent eval (agent_eval.py: paid Claude Code runs, never in CI)
tools/                  name research, SBOM, docs generators, reproducibility check
integrations/           VS Code, Neovim, JetBrains, GitHub, GitLab, Bitbucket, MCP, pre-commit
docs/                   architecture, security, benchmarks, traceability, ...
```

## Quality gates (all must pass before a change is merged)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --exclude scrutate-py -- -D warnings
cargo test --release -p scrutate                  # unit + samples + robustness + properties + the pinned ranking features
cargo test --release -p scrutate-ignore           # the vendored walker's own tests (vendor/ignore)
cargo audit
cargo deny check                                  # advisories, licences, bans and sources (deny.toml)
python3 tools/unsafe_count.py                     # each file's `unsafe` count matches docs/SECURITY.md
ruff check --select F .                           # pyflakes over every Python file (ruff from python/constraints.txt)
cd python && SCRUTATE_BIN=../target/release/scrutate python -m pytest -q   # training code, benchmark builders (+ bindings if installed)
python bench/quality.py target/release/scrutate   # ranking quality (MRR@10, hit@5) not below bench/quality/floor.json
python bench/differential.py --cases 200          # Engine A == ripgrep on random inputs
SCRUTATE_BIN=target/release/scrutate python bench/yardstick.py --smoke   # every scrutate form answers each workload right; installed rivals too
python3 tools/gen_assets.py --check && python3 tools/gen_doc_assets.py --check && python3 bench/make_readme_charts.py --check   # images, charts, document heads and links are what the scripts write
```

## Rules that keep the engines honest

- **Ranking moves on purpose.** `tests/ranking_pins.rs` pins every feature and score of a small
  tree bit for bit. A change to tokenization, BM25, vectors, features or the ranker that moves
  them records the pins again (`UPDATE_PINS=1 cargo test --test ranking_pins`) in the same commit
  and says why in its message; `bench/quality.py` must stay above its floor. Training reads its
  rows from the engine, so there is no second copy to change.
- **Index format changes bump `SCHEMA_VERSION`**, so old indexes are rebuilt. Never read a field
  the version check did not guarantee.
- **Adding a language**: one `LangSpec` in `langs.rs` (node kinds, name rules, call rules, import
  rules) plus `tests/samples/sample.<ext>` and its entry in `expected.json`
  (`UPDATE_EXPECTED=1 cargo test --test samples` records it; review the diff). Then regenerate
  docs/LANGUAGES.md with `tools/gen_languages_md.py`.
- **No new dependency without a reason** in the PR: size, maintenance, licence, and what it
  replaces. Run `cargo audit`, `cargo deny check` and `tools/sbom.py`.
- **No claims without evidence.** Numbers in docs come from a rerunnable script in `bench/` or
  `tools/`; status words (PASS/PARTIAL/FAIL/BLOCKED) follow docs/TRACEABILITY.md.
- **Never execute repository content.** Parsers parse; nothing in a repository (including
  `.scrutate.toml`) may name a program to run.

## Releasing

[docs/RELEASING.md](docs/RELEASING.md) is the checklist.

## Commit messages

One logical change per commit. The subject is one plain sentence saying what the program or code
does after the change ("Resolved references take the files to read from the text index ..."),
never an order. The body names the traceability row the change serves (`R-12.3`) and any
benchmark deltas.
