<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-quality.svg" alt="Ranking quality. How good are ranked answers on code it was not fitted on?" width="880">
</p>

# Ranking quality (Engine B)

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Benchmarks](BENCHMARKS.md) &nbsp;·&nbsp; Next: [Test results](TEST_RESULTS.md)

**Contents**: [Protocol](#protocol) &nbsp;·&nbsp; [Python, variant A: every docstring removed (MRR@10)](#python-variant-a-every-docstring-removed-mrr10) &nbsp;·&nbsp; [Python, variant B: first docstring sentence removed](#python-variant-b-first-docstring-sentence-removed) &nbsp;·&nbsp; [Other languages](#other-languages) &nbsp;·&nbsp; [What weighing type answers cost](#what-weighing-type-answers-cost) &nbsp;·&nbsp; [The sealed group](#the-sealed-group) &nbsp;·&nbsp; [What these numbers say and do not say](#what-these-numbers-say-and-do-not-say) &nbsp;·&nbsp; [int8 vectors (schema 13)](#int8-vectors-schema-13) &nbsp;·&nbsp; [Semantic model and ranker](#semantic-model-and-ranker) &nbsp;·&nbsp; [Agent eval](#agent-eval) &nbsp;·&nbsp; [Reproduce](#reproduce)

<!-- End of head. -->

How good the answers are, apart from how fast, measured by `bench/retrain_ranker.sh` (its `eval`
and `compare` stages) on packages never used to train the semantic model or fit the ranker. The
logs of its last run (`f3`, 2026-11-12: `eval.log`, `eval_python.log`, `compare.log`) are not
committed, so this page gives none of their figures: each section below says what it measures,
and its figures wait for a recorded run. The one committed measure is the floor of
`bench/quality.py` (`bench/quality/floor.json`).

Two sets are held out, of different weight:

- **TEST** (three Python packages, ten repositories in five other languages) was in view while
  this ranker was chosen: every fit on the way was read on it (four training weights for type
  answers, then a fit after each later engine change), and the rule that accepts a ranker changed
  twice after a fit failed it. The tables show the shipped ranker on code it was not fitted on,
  but the choices were made looking at them, so they are no untouched estimate.
- **SEALED** (three more Python packages, five more repositories) was never used to pretrain,
  tune, fit or test, and no ranker has run on it. It is evaluated once, with the shipped ranker,
  after the last engine change ([The sealed group](#the-sealed-group)).

Run `f3` used the binary before the semantic model's word tables went to int8 (`b7f71537`,
2026-11-14) and before ties were ordered by path and line (`2cfa63c0`, 2026-11-16). Its rows came from a Python reference engine matching the
binary's candidates, features and scores bit for bit, checked by `bench/parity.py` on networkx, the
engine's own sources and 21 repositories (the script went with the reference engine; commit
`57b5c9cc` is the last that has it). That mirror ended
([Architecture](ARCHITECTURE.md#retrieval-and-ranking-searchrs-semanticrs-rankrs) says why); before
it went, both ways gave the same benchmark files, rows and ranker, byte for byte. Now
`bench/gen_features.py` indexes each package with the binary and keeps what `batch --features`
prints, and the benchmark builders read files and words through the binary. Two guards watch
ranking.

`crates/scrutate/tests/ranking_pins.rs`, in `cargo test` on every CI platform, pins every
candidate, feature and score of six queries on a small tree with the bundled models, bit for bit
(docs/ARCHITECTURE.md says why the bits match on every platform, and where they were run). It sees
a feature move in its last digit, but not whether ranking got better or worse, and covers five
small files: not every language or scan path, not the clustered vector scan of large segments.

`bench/quality.py`, in CI and the release gates, sees a loss of more than 0.02 on its own corpus,
not a feature drifting in its fourth digit. Neither guard sees a small loss on code unlike this
repository's (another language, a large tree); `bench/retrain_ranker.sh`'s evaluation does. `bench/quality.py --make` writes the corpus and the queries
together, so only the engine moves the measure, never an edit to the repository's code.

| `bench/quality.py` | |
|---|---|
| corpus | a frozen copy of this repository's code (`crates/scrutate/src`, `vendor/ignore/src`, `bench/*.py`, `tools/*.py`), every doc comment and docstring removed as in variant A, in `bench/quality/corpus.tar.gz` |
| queries | the 1,183 committed queries of `bench/quality/queries.json` (the five types below, at most 150 per type and target kind, made from those doc comments), run through the binary |
| fails when | MRR@10 or hit@5 is under the floor in `bench/quality/floor.json`, the measured value less 0.02 |
| the floor file records | the measured values (MRR@10 0.5873, hit@5 0.6636 on 2026-11-13), the commit, the vector model's signature, the ranker's hash |

## Protocol

- A query is made from a real definition, its one correct answer.
- The answer is a function or method, or a documented type (class, struct, enum, trait,
  interface, type alias). Type queries are drawn after a package's function queries from their own
  random stream, so adding them moved no function query. An impl block or constructor does not
  answer a type query. The Python and other-language tables give function and type answers apart.
- The indexed copy lacks that definition's documentation (**variant A**: every docstring in the
  package removed, an undocumented code base; **variant B**: only each docstring's first sentence
  removed), so the answer text is not in the index.
- **MRR@10**: mean of 1/rank of the correct result (0 when not in the top 10). **hit@k**: share of
  queries with the correct result in the top k. Brackets: 95 % bootstrap intervals over queries.
- TEST: networkx 3.4.2, mcp 1.30.0, rich 15.0.0 (Python), and ten repositories in other languages
  (below). All systems share the **candidate pool, tokenizer, stemming and field weights**.

| Query type | Text |
|---|---|
| **doc** | the first documentation sentence written as a natural-language request |
| **kw** | three content words sampled from it |
| **name** | the name as words (`remove tool`) |
| **id** | the bare identifier (`remove_tool`) |
| **qid** | the qualified name (`Server.remove_tool`) |

| System | Ranks by |
|---|---|
| *BM25* | Scrutate's own lexical layer alone |
| *semantic only* | vector similarity alone |
| *linear blend* | the hand-set fallback when no ranker is present |
| *learned* | the shipped pipeline |

## Python, variant A: every docstring removed (MRR@10)

Each of the five query types, function and type answers apart, for BM25, semantic only, the
linear blend and the learned ranker: MRR@10 with its 95 % interval, the learned ranker's gain over
BM25 and its hit@1, hit@5 and hit@10. Its figures wait for a recorded run: they came from the logs of run `f3` of `bench/retrain_ranker.sh`, which are not committed.

## Python, variant B: first docstring sentence removed

The same rows for BM25 and the learned ranker with only each docstring's first sentence removed.
Its figures wait for a recorded run: they came from the logs of run `f3` of `bench/retrain_ranker.sh`, which are not committed.

## Other languages

Ten held-out repositories (`bench/groups.py` `CODE_TEST`): ripgrep and serde_json (Rust),
protobuf-go and client_golang (Go), jsoup and commons-io (Java), ws and lodash (JavaScript), RxJS
and PixiJS (TypeScript). Queries are built the same way from doc comments
(`bench/make_bench_code.py`). Variant A, MRR@10, BM25 → learned ranker.

Variant A, MRR@10 from BM25 to the learned ranker, per language and query type, function and type
answers apart. Its figures wait for a recorded run: they came from the logs of run `f3` of `bench/retrain_ranker.sh`, which are not committed.

## What weighing type answers cost

The previous ranker was fitted on function answers alone; this one weights function and type
answers alike (`TYPE_WEIGHT=1`). The comparison of the two on the same queries, per variant and
query type, a cross-validation on DEV (`bench/train_ranker.py cv`, `CV=1` in
`bench/retrain_ranker.sh`) of where function rows lose, and a sweep of type weights 0.1, 0.3, 0.5
and 1 all wait for a recorded run: none of their output is committed. What they were run to
decide: equal weighting finds type answers, which the previous ranker almost never ranked first,
at a cost to function answers on doc and kw queries.

## The sealed group

`bench/groups.py` `SEALED` and `CODE_SEALED`: click 8.5.1, httpx 0.28.1 and attrs 26.1.0 (Python),
rayon v1.12.0 (Rust), gin v1.12.0 (Go), commons-collections 4.6.0 (Java), video.js v8.24.2
(JavaScript) and TypeORM 1.1.2 (TypeScript). None was used to pretrain the semantic model, tune,
fit or test, and the semantic model's training refuses them by name. `bench/eval_sealed.sh`
evaluates the ranker in `crates/scrutate/models/ranker.npz` on the group, records each evaluation in
`sealed_evaluated.json` (ranker SHA-256, commit, date), and refuses a second ranker unless
`FORCE_SEALED=1` is set, which it records too.

Not evaluated at this commit. Its one evaluation, with the shipped ranker after the last engine
change, goes here.

## What these numbers say and do not say

- Whether the learned pipeline beats its own lexical layer on TEST, row by row, waits for a
  recorded run.
- Six languages. The semantic model was trained on Python only, the ranker fitted on Python and
  the five other languages above. Other languages get the lexical layer's quality plus what the
  ranker and the code-word vectors generalise.
- No comparison with other code-search products is published here: `bench/yardstick.py --quality
  N` runs these queries on ck, grepai and claude-context (its W8 workload), but no result of it is
  committed, and public code-search benchmarks (e.g. natural-language-to-code retrieval sets) use
  other protocols. Absolute MRR does not compare across protocols.
- A constant is found through the window holding its line, never as its own result, so no row
  measures constants.

## int8 vectors (schema 13)

Since schema 13 the index stores each semantic vector as int8 codes plus one float32 scale
(symmetric, per vector) in place of float16. When the change went in it was checked with an earlier
ranker (v7): the protocol before and after on one fresh benchmark (`bench/make_bench.py` on
networkx 3.4.2, mcp 1.30.0 and rich 15.0.0, then `bench/gen_features.py` and
`bench/train_ranker.py eval` with the shipped models, not retrained), vector recall against the
float16 exact top-100, index size, query latency and parity with the reference engine of the
time; a trial of zstd with a trained dictionary for chunk text was not shipped. No output of
these checks is committed, so their figures wait for a recorded run.

## Semantic model and ranker

- Semantic model (`crates/scrutate/models/semantic.npz`, 7.6 MB): a code-word model with two views
  per chunk (description: name + signature + docs; implementation: body), 256 dimensions, trained in
  numpy (`bench/train_semantic.py`) with an InfoNCE contrastive objective over (query, description),
  (query, implementation) and (description, implementation) pairs with 20 % word dropout. How much
  it improved on the previous PLS-SVD model waits for a recorded run.
- Its two word tables are stored as int8 codes with one float32 scale per row (symmetric, value =
  code × max|row| / 127, rounded half to even, as the index's vectors), in place of float16: the
  file went from 14.6 MB to 7.6 MB, so the crate fits crates.io's 10 MB limit. `bench/quality.py`
  measured MRR@10 0.5873 and hit@5 0.6636 (floor 0.5673 and 0.6436; by type, doc function 0.3941,
  kw type 0.0769, name function 0.8623) on 2026-11-13 (`bench/quality/floor.json`, commit
  `b236c4b`); the int8 run of `b7f71537` is not committed, so the change int8 made waits for a
  recorded run. The ranking pins moved in the six `sem_` features and nine scores, with no change
  of order.
- Training data: the 37 Python packages in `bench/groups.py` (27 to pretrain the semantic model,
  7 to tune and fit the ranker, 3 held out for the test). The model files keep word statistics,
  vocabulary tables and projections, not source text. Licences as declared on PyPI, and of the
  sealed group, read for evaluation only:

  | Licence | Training packages | Sealed group |
  |---|---|---|
  | BSD-2/3-Clause | pandas, scipy, scikit-learn, numpy, scikit-image, reportlab, pypdf, xlsxwriter, pygments, uvicorn, imageio, mammoth, protobuf, werkzeug, seaborn, joblib, networkx | click, httpx (BSD-3-Clause) |
  | MIT | spacy, mypy, fonttools, narwhals, thinc, srsly, anyio, markdown-it-py, pytest, pydantic, python-pptx, python-docx, openpyxl, mcp, rich | attrs, gin, TypeORM |
  | MIT-CMU | pillow | |
  | the Matplotlib licence | matplotlib | |
  | Apache-2.0 | playwright, cryptography (as one option) | commons-collections, video.js |
  | MIT or Apache-2.0 | | rayon |
  | MPL-2.0 | pikepdf | |

- Ranker (`crates/scrutate/models/ranker.npz`, 86 KB): LightGBM LambdaRank over 48 features,
  exported to arrays; the fit prints how closely numpy and LightGBM predictions agree (its log is
  not committed). Fitted on the 7 Python DEV packages and the 16 `CODE_DEV` repositories of
  `bench/groups.py` (tokio, clap, chrono, grpc-go, zap, cobra, commons-lang, gson, javapoet, guava,
  commander, axios, immutable-js, eslint, three.js, vue core; MIT, Apache-2.0, or both), function
  and type answers weighted alike (`TYPE_WEIGHT=1`). `bench/retrain_ranker.sh` installs a new ranker
  only when on TEST, for all languages together and for each one, no row of 50 queries or more falls
  below BM25 beyond its interval and no variant's mean over every query falls below the previous
  ranker's beyond its interval. Top features by gain, in the same uncommitted fit log: lexical
  ratio, rank and z-score, name coverage, A-view semantic z-score, query length, name ratio, the
  name spelled as typed.

## Agent eval

The measures above score ranked answers, not an agent at work. `bench/agent_eval.py` runs Claude
Code (`claude -p`) on 20 multi-step tasks over three pinned corpora (this repository at a fixed
commit, ripgrep 15.2.0, networkx 3.7), each task twice: with scrutate's MCP server and with the
built-in tools alone, same model and prompt, a fresh session in a fresh copy each time. A check
derived from the code scores each answer, and each run records tokens, cost, tool calls, tool
errors, turns and wall time; seven tasks are held out from any tuning. Its header says how to
run the smoke test and a full round, and what a run is estimated to cost.

No round has run yet, so there are no results: nothing here says whether the server helps an
agent or saves tokens.

## Reproduce

```sh
# the features come from target/release/scrutate, which retrain_ranker.sh builds first
# (SCRUTATE_BIN names another build);
# a virtual environment with numpy, lightgbm, scikit-learn
# the DEV packages, and the TEST and SEALED packages at the versions named in the scripts' headers
export SCRUTATE_WORK=/some/scratch SCRUTATE_VENV=/path/to/venv THREADS=4
bash bench/retrain_ranker.sh   # benchmark, features, fit on DEV, old and new ranker on TEST
bash bench/eval_sealed.sh      # the sealed group, once per ranker
```

`retrain_ranker.sh` writes `crates/scrutate/models/ranker.npz` only when the new ranker passes the rule above; the
binary embeds that file, so rebuild it afterwards. `eval_sealed.sh` never changes the ranker.
