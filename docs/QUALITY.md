<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-quality.svg" alt="Ranking quality. How good are ranked answers on code it was not fitted on?" width="880">
</p>

# Ranking quality (Engine B)

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Benchmarks](BENCHMARKS.md) &nbsp;·&nbsp; Next: [Test results](TEST_RESULTS.md)

**Contents**: [Protocol](#protocol) &nbsp;·&nbsp; [Python, variant A: every docstring removed (MRR@10)](#python-variant-a-every-docstring-removed-mrr10) &nbsp;·&nbsp; [Python, variant B: first docstring sentence removed](#python-variant-b-first-docstring-sentence-removed) &nbsp;·&nbsp; [Other languages](#other-languages) &nbsp;·&nbsp; [What weighing type answers cost](#what-weighing-type-answers-cost) &nbsp;·&nbsp; [The sealed group](#the-sealed-group) &nbsp;·&nbsp; [What these numbers say and do not say](#what-these-numbers-say-and-do-not-say) &nbsp;·&nbsp; [int8 vectors (schema 13)](#int8-vectors-schema-13) &nbsp;·&nbsp; [Semantic model and ranker](#semantic-model-and-ranker) &nbsp;·&nbsp; [Agent eval](#agent-eval) &nbsp;·&nbsp; [Reproduce](#reproduce)

<!-- End of head. -->

How good the answers are, apart from how fast. The numbers come from `bench/retrain_ranker.sh`
(its `eval` and `compare` stages, run `f3`, 2026-11-12) on packages never used to train the
semantic model or fit the ranker. The logs of that run (`eval.log`, `eval_python.log`,
`compare.log`) are not committed, and no other file in the repository holds the tables' figures;
only the floor of `bench/quality.py` has a committed source (`bench/quality/floor.json`).

Two sets are held out, of different weight:

- **TEST** (three Python packages, ten repositories in five other languages) was in view while
  this ranker was chosen: every fit on the way was read on it (four training weights for type
  answers, then a fit after each later engine change), and the rule that accepts a ranker changed
  twice after a fit failed it. The tables show the shipped ranker on code it was not fitted on,
  but the choices were made looking at them, so they are no untouched estimate.
- **SEALED** (three more Python packages, five more repositories) was never used to pretrain,
  tune, fit or test, and no ranker has run on it. It is evaluated once, with the shipped ranker,
  after the last engine change ([The sealed group](#the-sealed-group)).

The numbers come from the binary of run `f3`, before the semantic model's word tables went to int8
(`b7f71537`, 2026-11-14) and before ties were ordered by path and line (`2cfa63c0`, 2026-11-16); the
tables have not been rerun since. Their rows came from a Python reference engine matching the
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
repository's (another language, a large tree); the tables below, rerun with
`bench/retrain_ranker.sh`, do. `bench/quality.py --make` writes the corpus and the queries
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

Function answers:

| Query type | n | BM25 | semantic only | linear blend | **learned ranker** | learned − BM25 [95 % CI] | hit@1 / hit@5 / hit@10 (learned) |
|---|---:|---:|---:|---:|---:|---:|---|
| doc | 1 494 | 0.463 | 0.435 | 0.508 | **0.635** [0.614, 0.653] | +0.172 [+0.156, +0.188] | 0.513 / 0.798 / 0.863 |
| kw | 1 392 | 0.322 | 0.285 | 0.344 | **0.412** [0.390, 0.435] | +0.090 [+0.075, +0.103] | 0.295 / 0.572 / 0.663 |
| name | 1 129 | 0.714 | 0.629 | 0.726 | **0.916** [0.902, 0.928] | +0.201 [+0.180, +0.223] | 0.873 / 0.967 / 0.979 |
| id | 893 | 0.927 | 0.704 | 0.985 | **1.000** [1.000, 1.000] | +0.073 [+0.058, +0.087] | 1.000 / 1.000 / 1.000 |
| qid | 753 | 0.885 | 0.616 | 0.894 | **0.996** [0.991, 0.999] | +0.110 [+0.091, +0.128] | 0.992 / 0.999 / 1.000 |

Type answers:

| Query type | n | BM25 | semantic only | linear blend | **learned ranker** | learned − BM25 [95 % CI] | hit@1 / hit@5 / hit@10 (learned) |
|---|---:|---:|---:|---:|---:|---:|---|
| doc | 457 | 0.280 | 0.274 | 0.366 | **0.558** [0.522, 0.594] | +0.278 [+0.244, +0.310] | 0.411 / 0.751 / 0.827 |
| kw | 437 | 0.214 | 0.230 | 0.284 | **0.407** [0.369, 0.443] | +0.192 [+0.161, +0.227] | 0.288 / 0.572 / 0.670 |
| name | 374 | 0.730 | 0.695 | 0.746 | **0.947** [0.929, 0.964] | +0.217 [+0.185, +0.255] | 0.909 / 0.995 / 0.995 |
| id | 437 | 0.831 | 0.665 | 0.955 | **1.000** [1.000, 1.000] | +0.169 [+0.138, +0.199] | 1.000 / 1.000 / 1.000 |
| qid | 10 | 0.684 | 0.428 | 0.850 | **1.000** [1.000, 1.000] | +0.316 [+0.100, +0.526] | 1.000 / 1.000 / 1.000 |

## Python, variant B: first docstring sentence removed

Function answers:

| Query type | n | BM25 | **learned ranker** | learned − BM25 [95 % CI] |
|---|---:|---:|---:|---:|
| doc | 1 494 | 0.522 | **0.666** [0.647, 0.685] | +0.144 [+0.127, +0.160] |
| kw | 1 392 | 0.371 | **0.451** [0.429, 0.474] | +0.081 [+0.067, +0.096] |
| name | 1 129 | 0.693 | **0.917** [0.904, 0.930] | +0.223 [+0.203, +0.244] |
| id | 893 | 0.911 | **1.000** [1.000, 1.000] | +0.089 [+0.073, +0.105] |
| qid | 753 | 0.867 | **0.996** [0.991, 0.999] | +0.128 [+0.108, +0.147] |

Type answers:

| Query type | n | BM25 | **learned ranker** | learned − BM25 [95 % CI] |
|---|---:|---:|---:|---:|
| doc | 457 | 0.285 | **0.550** [0.514, 0.584] | +0.265 [+0.232, +0.297] |
| kw | 437 | 0.243 | **0.434** [0.394, 0.471] | +0.190 [+0.161, +0.222] |
| name | 374 | 0.701 | **0.933** [0.912, 0.951] | +0.232 [+0.196, +0.268] |
| id | 437 | 0.813 | **1.000** [1.000, 1.000] | +0.187 [+0.155, +0.219] |
| qid | 10 | 0.662 | **1.000** [1.000, 1.000] | +0.338 [+0.117, +0.543] |

## Other languages

Ten held-out repositories (`bench/groups.py` `CODE_TEST`): ripgrep and serde_json (Rust),
protobuf-go and client_golang (Go), jsoup and commons-io (Java), ws and lodash (JavaScript), RxJS
and PixiJS (TypeScript). Queries are built the same way from doc comments
(`bench/make_bench_code.py`). Variant A, MRR@10, BM25 → learned ranker.

Function answers:

| Language | doc | kw | name | id | qid |
|---|---|---|---|---|---|
| Rust | 0.354 → **0.459** | 0.215 → **0.246** | 0.704 → **0.813** | 0.929 → **0.996** | 0.755 → **0.989** |
| Go | 0.315 → **0.419** | 0.162 → **0.211** | 0.625 → **0.800** | 0.926 → **1.000** | 0.877 → **0.965** |
| Java | 0.458 → **0.488** | 0.278 → **0.279** | 0.731 → **0.813** | 0.961 → **0.982** | 0.841 → **0.916** |
| JavaScript | 0.416 → **0.608** | 0.267 → **0.367** | 0.632 → **0.871** | 0.864 → **1.000** | 0.903 → **1.000** |
| TypeScript | 0.469 → **0.570** | 0.269 → **0.297** | 0.801 → **0.903** | 0.920 → **1.000** | 0.848 → **0.989** |

Type answers (a cell with fewer than 20 queries gives the count and no number):

| Language | doc | kw | name | id | qid |
|---|---|---|---|---|---|
| Rust | 0.190 → **0.244** | 0.136 → **0.168** | 0.688 → **0.812** | 0.725 → **0.937** | (5 queries) |
| Go | 0.224 → **0.339** | 0.145 → **0.228** | 0.587 → **0.762** | 0.736 → **0.994** | (none) |
| Java | 0.216 → **0.457** | 0.130 → **0.315** | 0.298 → **0.914** | 0.466 → **0.979** | 0.441 → **0.975** |
| JavaScript | (7 queries) | (5 queries) | (5 queries) | (7 queries) | (none) |
| TypeScript | 0.316 → **0.600** | 0.204 → **0.380** | 0.688 → **0.976** | 0.820 → **1.000** | (none) |

Java keyword queries with a function answer are the one row where the learned ranker does not
clear BM25 (0.279 against 0.278). Per-cell numbers, hit@k and intervals for every language and
both variants: `compare.log`.

## What weighing type answers cost

The previous ranker, fitted on function answers alone, ranked a type first for almost no
documentation query (doc with a type answer: 0.017). This one weights function and type answers
alike. All languages, previous ranker against this one, same queries:

| Variant | row | n | before | now | now − before [95 % CI] |
|---|---|---:|---:|---:|---:|
| A | doc | 7 024 | 0.564 | 0.534 | -0.031 [-0.034, -0.027] |
| A | kw | 5 333 | 0.347 | 0.312 | -0.035 [-0.040, -0.032] |
| A | name | 4 277 | 0.869 | 0.857 | -0.012 [-0.016, -0.008] |
| A | id | 3 245 | 0.990 | 0.996 | +0.006 [+0.004, +0.008] |
| A | qid | 2 516 | 0.967 | 0.971 | +0.004 [+0.001, +0.007] |
| A | doc:type | 2 145 | 0.017 | 0.478 | +0.462 [+0.445, +0.478] |
| A | kw:type | 2 032 | 0.008 | 0.326 | +0.318 [+0.301, +0.336] |
| A | name:type | 1 678 | 0.404 | 0.909 | +0.504 [+0.487, +0.521] |
| A | id:type | 1 613 | 0.637 | 0.987 | +0.350 [+0.333, +0.366] |
| A | qid:type | 114 | 0.263 | 0.969 | +0.707 [+0.668, +0.744] |
| A | every query | 29 977 | 0.566 | 0.656 | +0.090 [+0.087, +0.093] |
| B | doc | 7 024 | 0.623 | 0.591 | -0.032 [-0.036, -0.029] |
| B | kw | 5 333 | 0.405 | 0.370 | -0.035 [-0.039, -0.031] |
| B | name | 4 277 | 0.871 | 0.861 | -0.010 [-0.014, -0.005] |
| B | id | 3 245 | 0.991 | 0.996 | +0.005 [+0.004, +0.007] |
| B | qid | 2 516 | 0.967 | 0.972 | +0.006 [+0.003, +0.009] |
| B | doc:type | 2 145 | 0.023 | 0.508 | +0.485 [+0.469, +0.502] |
| B | kw:type | 2 032 | 0.012 | 0.366 | +0.354 [+0.336, +0.372] |
| B | name:type | 1 678 | 0.485 | 0.915 | +0.431 [+0.414, +0.448] |
| B | id:type | 1 613 | 0.678 | 0.991 | +0.314 [+0.297, +0.330] |
| B | qid:type | 114 | 0.276 | 0.982 | +0.707 [+0.667, +0.747] |
| B | every query | 29 977 | 0.598 | 0.686 | +0.088 [+0.085, +0.091] |

Function answers lost 0.031 on doc and 0.035 on kw (variant A); type answers went from almost
never found to found about as well as functions. The mean over every query rose from 0.566 to
0.656.

The cross-validation and type-weight figures below come from `bench/train_ranker.py cv` on DEV
(`CV=1` in `bench/retrain_ranker.sh`) and from `bench/retrain_ranker.sh` runs with
`TYPE_WEIGHT` 0.1, 0.3, 0.5 and 1; their output is not committed either.

Where function rows lose: in cross-validation on DEV, of 33 308 function doc and kw queries, the
function-only weighting ranks the answer first and equal weighting does not in 1 771, the reverse
in 667. Above the answer then stands another type (44 %), another function (25 %), the class or
impl holding the answer (22 %) or a sibling method (8 %). 88 % of those types share a word with
the query in their name: the cost is a type whose name matches the query, which nothing in the
candidate set tells apart from a wanted function. Features for a method's holder and its members
lifted no function row by 0.005 in cross-validation, so the engine lacks them.

Type weights on TEST (variant A, all languages) and in DEV cross-validation:

| Type weight | 0.1 | 0.3 | 0.5 | 1.0 |
|---|---:|---:|---:|---:|
| TEST, doc with a function answer | 0.564 | 0.558 | 0.547 | 0.533 |
| TEST, doc with a type answer | 0.240 | 0.360 | 0.417 | 0.478 |
| TEST, mean over every query | 0.632 | 0.649 | 0.653 | 0.655 |
| DEV cross-validation, mean over every query | 0.616 | 0.636 | | 0.649 |

At 0.1 the learned ranker is under BM25 on doc and kw queries with a type answer. Equal weight
has the highest mean on both, and counts every query the same.

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

- On TEST the learned pipeline beats its own lexical layer on every row of 50 queries or more but
  Java keyword queries, a tie. The gain is largest where lexical search is weakest:
  natural-language and keyword queries on undocumented code, and type answers.
- Six languages. The semantic model was trained on Python only, the ranker fitted on Python and
  the five other languages above. Other languages get the lexical layer's quality plus what the
  ranker and the code-word vectors generalise.
- - No comparison with other code-search products is published here: `bench/yardstick.py --quality
  N` runs these queries on ck, grepai and claude-context (its W8 workload), but no result of it is
  committed, and public code-search benchmarks (e.g. natural-language-to-code retrieval sets) use
  other protocols. Absolute MRR does not compare across protocols.
- A constant is found through the window holding its line, never as its own result, so no row
  measures constants.

## int8 vectors (schema 13)

Since schema 13 the index stores each semantic vector as int8 codes plus one float32 scale
(symmetric, per vector) in place of float16; the tables above use int8. The check below, made
with an earlier ranker (v7) when the change went in, reruns the protocol before and after on one
fresh benchmark (`bench/make_bench.py` on networkx 3.4.2, mcp 1.30.0 and rich 15.0.0, then
`bench/gen_features.py` and `bench/train_ranker.py eval` with the shipped models, not retrained).
Package versions differ from the v7 run, so n and absolute values differ from the tables above;
only the difference matters. No output of these schema 13 checks (the tables, recall, sizes,
latency and parity counts of this section, and the zstd trial) is committed.

Learned ranker MRR@10, float16 → int8:

| Query type | n | variant A | variant B |
|---|---:|---:|---:|
| doc | 1 494 | 0.6521 → 0.6514 (−0.0007) | 0.6797 → 0.6808 (+0.0010) |
| kw | 1 392 | 0.4526 → 0.4518 (−0.0008) | 0.4891 → 0.4898 (+0.0008) |
| name | 1 129 | 0.9227 → 0.9223 (−0.0004) | 0.9233 → 0.9230 (−0.0003) |
| id | 893 | 0.9922 → 0.9922 (0) | 0.9924 → 0.9924 (0) |
| qid | 753 | 0.9861 → 0.9854 (−0.0007) | 0.9810 → 0.9810 (0) |

BM25, semantic only and linear blend move by at most 0.001 as well.

Vector recall on a 121 760-chunk index (built by the reference engine of the time) of a copy of
10 site-packages (numpy, pandas, scipy and others; the tree of the size table below), 398 doc and
kw queries:

- exact scan, int8 top-100 against float16 top-100: recall 0.9945 (top-10: 0.9915);
- default scan (48 of 698 clusters) against the float16 exact top-100: 0.8910 int8, 0.8901
  float16 (`bench/ivf_recall.py` at nprobe 48: 0.8922 and 0.8901; the script ran on the reference
  engine and went with it; commit `8a4f4b4` is the last that has it);
- the whole pipeline's final top-10: same first result for 376 of 398 queries, 96.1 % of the
  top-10 shared.

Index size, fresh index built by the Rust engine (`du`, MiB):

| Repository | chunks | float16 | int8 | vectors (`vec` + `vscale`) |
|---|---:|---:|---:|---:|
| networkx package | 8 054 | 18.8 | 14.9 (−21 %) | 7.9 → 4.0 |
| 10 site-packages | 161 208 | 343.0 | 265.5 (−23 %) | 157.4 → 79.9 |

Query latency (`scrutate search` on the large index, warm, 4 queries, 3 alternating rounds of
20 runs, median): 62.4 ms float16, 61.5 ms int8.

When the change was made, parity with the reference engine (`bench/parity.py`, release build) gave
bit-identical vectors, features and scores on the networkx package (300 queries) and on a
119 358-chunk index using the clustered scan (the `.py` files of the same 10 packages, 200 queries).
There both engines sum the vector-scan dot products in float64 in one fixed order and round once
to float32; with float32 sums two near-tied rows could swap places between engines. Parity also
held end to end, each engine on its own index, on segments of 20 000 rows or more: both build the
same clusters bit for bit (go_protobuf, 41 377 chunks: 40/40), comparing centroids, cluster
pointers, cluster order and stored vectors directly.

Not shipped: compressing chunk text with a zstd dictionary trained on the repository's chunks. On
a 20 000-chunk sample of the large index, per-chunk zstd with a 110 KB trained dictionary is 19 %
(level 3) to 30 % (level 19) smaller than the current zlib level 6. The text column is 41 MiB, so
the index would shrink 8 to 13 MiB (3 to 5 %), about 0.5 to 0.8 MiB on networkx. For a few
percent it would add a C library to the engine, a third-party package to the reference engine of
the time, a dictionary training pass before chunks can be compressed (1.6 s of CPU for 30 000
samples) and a dictionary to version in the index.

## Semantic model and ranker

- - Semantic model (`crates/scrutate/models/semantic.npz`, 7.6 MB): a code-word model with two views
  per chunk (description: name + signature + docs; implementation: body), 256 dimensions, trained in
  numpy (`bench/train_semantic.py`) with an InfoNCE contrastive objective over (query, description),
  (query, implementation) and (description, implementation) pairs with 20 % word dropout. On DEV its
  semantic-only doc MRR rose from 0.247 (the previous PLS-SVD model) to 0.427, in a run whose output
  is not committed.
- - Its two word tables are stored as int8 codes with one float32 scale per row (symmetric, value =
  code × max|row| / 127, rounded half to even, as the index's vectors), in place of float16: the
  file went from 14.6 MB to 7.6 MB, so the crate fits crates.io's 10 MB limit. `bench/quality.py` on
  a release build, float16 → int8 (the float16 values and the floors are those of
  `bench/quality/floor.json`; the int8 run, at `b7f71537`, is not committed): MRR@10 0.5873 →
  0.5874, hit@5 0.6636 → 0.6653 (floor 0.5673 and 0.6436); by type, doc function 0.3941 → 0.3955, kw
  type 0.0769 → 0.0764, name function 0.8623 → 0.8618, the rest within 0.0004. The ranking pins
  moved in the six `sem_` features and nine scores, with no change of order.
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

- - Ranker (`crates/scrutate/models/ranker.npz`, 86 KB): LightGBM LambdaRank over 48 features,
  exported to arrays; numpy and LightGBM predictions agree to 3.5e-8 (printed by the fit; its log is
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
# and the DEV, TEST and SEALED packages at the versions named in the scripts' headers
export SCRUTATE_WORK=/some/scratch SCRUTATE_VENV=/path/to/venv THREADS=4
bash bench/retrain_ranker.sh   # benchmark, features, fit on DEV, old and new ranker on TEST
bash bench/eval_sealed.sh      # the sealed group, once per ranker
```

`retrain_ranker.sh` writes `crates/scrutate/models/ranker.npz` only when the new ranker passes the rule above; the
binary embeds that file, so rebuild it afterwards. `eval_sealed.sh` never changes the ranker.
