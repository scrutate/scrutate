<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-research.svg" alt="Research. Would a small transformer model rank code better?" width="880">
</p>

# Research: a small transformer embedding model for Engine B

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Roadmap](ROADMAP.md) &nbsp;·&nbsp; Next: [Naming](NAMING.md)

<!-- End of head. -->

## Question

Would a small transformer text-embedding model, run on CPU with no network at runtime, rank code
better than the current code-word semantic model (`crates/scrutate/models/semantic.npz`), and at what cost?

## Setup

- Data and protocol as in [QUALITY.md](QUALITY.md). `bench/make_bench.py` (seed 1234) rebuilt the
  corpora and queries from the latest PyPI releases of the packages in `bench/groups.py`
  (networkx 3.7, mcp 2.3.0, rich 15.0.0 for TEST). Query counts matched QUALITY.md as it was
  before ranker v8 (`d882a7b0`, 2026-11-10) except `kw` (1 585 instead of 1 604, from newer
  package versions); QUALITY.md's tables now come from a later run with other counts.
- Models compared:

  | Model | Params | Dim | Licence (model card) | Weights on disk |
  |---|---:|---:|---|---:|
  | current code-word model | n/a | 256 | this repository | 14.6 MB (float16; 7.6 MB as int8 since) |
  | `sentence-transformers/all-MiniLM-L6-v2` | 22.7 M | 384 | Apache-2.0 | 91 MB (fp32) |
  | `BAAI/bge-small-en-v1.5` | 33 M | 384 | MIT | 133 MB (fp32) |
  | `ibm-granite/granite-embedding-small-english-r2` (model card reports code retrieval scores, CoIR) | 47 M | 384 | Apache-2.0 | 95 MB (bf16), 190 MB as fp32 |

- `bench/eval_transformers.py` plugs a transformer into the same two vector slots per chunk as
  the current model (view A: qualified name, signature and docstring; view B: body) and embeds the
  raw query text (bge gets its documented query prefix). Inputs are cut to 256 tokens and vectors
  normalised. Nothing else changes: the candidate pool (BM25 + exact name + 100 semantic
  neighbours), the 47 features of the time (48 now) and `bench/train_ranker.py`.
- For every model, the current one included, the ranker was retrained with `train_ranker.py fit`
  on that model's DEV features and evaluated once on TEST. On the current model it is within
  0.005 MRR of the shipped `crates/scrutate/models/ranker.npz` on every query type: a sound baseline.
- *semantic only* ranks the candidate pool by vector similarity alone, as in QUALITY.md.
- Intervals: 95 % paired bootstrap over queries (1 000 resamples, seed 0), from `evalcore.py`.
- Bulk embedding for the quality runs used the Apple GPU (MPS) to keep CPU load low, with granite
  at its stored bf16 precision; the timings below are CPU only.

## Results, variant A (every docstring removed)

MRR@10. Learned = ranker retrained for that model.

| Query | n | current sem. only | MiniLM sem. only | bge sem. only | granite sem. only | current learned | MiniLM learned | bge learned | granite learned |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| doc | 1 695 | 0.421 | 0.460 | **0.529** | 0.511 | 0.631 | 0.676 | **0.700** | 0.689 |
| kw | 1 585 | 0.265 | 0.269 | **0.296** | 0.287 | 0.426 | 0.438 | 0.445 | **0.448** |
| name | 1 274 | 0.657 | 0.646 | **0.740** | 0.711 | 0.924 | 0.960 | **0.980** | 0.967 |
| id | 900 | 0.696 | 0.662 | **0.756** | 0.738 | 0.985 | 0.989 | 0.989 | 0.988 |
| qid | 754 | 0.622 | 0.657 | **0.754** | 0.723 | 0.980 | 0.984 | 0.982 | **0.986** |

Semantic only, hit@1 / hit@5 / hit@10, variant A:

| Query | current | MiniLM | bge | granite |
|---|---|---|---|---|
| doc | 0.309 / 0.568 / 0.657 | 0.337 / 0.631 / 0.732 | 0.405 / 0.696 / 0.775 | 0.388 / 0.684 / 0.780 |
| kw | 0.180 / 0.380 / 0.464 | 0.178 / 0.389 / 0.482 | 0.204 / 0.418 / 0.514 | 0.192 / 0.411 / 0.520 |
| name | 0.553 / 0.790 / 0.881 | 0.522 / 0.822 / 0.892 | 0.625 / 0.901 / 0.954 | 0.595 / 0.860 / 0.936 |
| id | 0.594 / 0.831 / 0.906 | 0.541 / 0.819 / 0.902 | 0.648 / 0.900 / 0.952 | 0.632 / 0.881 / 0.938 |
| qid | 0.493 / 0.808 / 0.879 | 0.529 / 0.820 / 0.901 | 0.646 / 0.893 / 0.936 | 0.599 / 0.878 / 0.943 |

Learned ranker, MRR difference against the current model [95 % CI]:

| Query | MiniLM A | bge A | granite A | bge B | granite B |
|---|---|---|---|---|---|
| doc | +0.045 [+0.033, +0.057] | +0.070 [+0.055, +0.082] | +0.059 [+0.046, +0.072] | +0.068 [+0.054, +0.082] | +0.056 [+0.042, +0.069] |
| kw | +0.013 [+0.001, +0.023] | +0.019 [+0.008, +0.030] | +0.022 [+0.010, +0.034] | +0.020 [+0.008, +0.031] | +0.022 [+0.011, +0.033] |
| name | +0.035 [+0.025, +0.045] | +0.055 [+0.044, +0.067] | +0.042 [+0.032, +0.053] | +0.050 [+0.039, +0.062] | +0.034 [+0.024, +0.045] |
| id | +0.004 [-0.001, +0.010] | +0.004 [-0.001, +0.011] | +0.003 [-0.003, +0.010] | +0.007 [+0.001, +0.013] | +0.005 [-0.001, +0.013] |
| qid | +0.003 [-0.004, +0.009] | +0.002 [-0.005, +0.008] | +0.006 [-0.002, +0.013] | -0.004 [-0.012, +0.003] | +0.002 [-0.005, +0.008] |

Variant B (first docstring sentence removed), learned ranker MRR for doc: current 0.674, MiniLM
0.712, bge 0.742, granite 0.730. The `compare` command below prints the full output per type and
variant.

## Costs

Machine: Apple M3 Pro (11 cores), 18 GB RAM, macOS, 4 threads. Sample: 1 000 random function and
class chunks from the TEST packages, two views each (2 000 texts), sorted by length, batch 64.

| Model | Embed 1 000 chunks (PyTorch, CPU) | Embed 1 000 chunks (candle + Accelerate, CPU) | One query | Peak RSS (PyTorch) |
|---|---:|---:|---:|---:|
| current (Python reference) | 0.19 s | n/a | 0.07 ms | 143 MB |
| MiniLM | 8.8 s | 32 s | 5.2 ms | 1.0 GB |
| bge-small | 16.6 s | 68 s | 9.2 ms | 0.9 GB |
| granite-small (fp32) | 20.3 s | not measured | 11.4 ms | 1.4 GB |

- Indexing compute grows 46 to 107 times over the Python reference of the current model (the Rust
  engine is faster still): a 100 000-chunk repository on 4 threads takes about 15 min (MiniLM) or
  28 min (bge) with PyTorch, more with candle, against seconds today.
- Every query pays one more forward pass: 5 to 11 ms on CPU.
- The binary is 82 MB today (release, macOS arm64). A transformer adds:
  - runtime: candle-core + candle-nn + candle-transformers + tokenizers, about 4.3 MB stripped
    (a probe binary with the same profile, 4.6 MB, against 0.4 MB for an empty one); or ONNX
    Runtime, whose macOS arm64 library is 33 MB (version 1.31.1);
  - weights: 91 MB (MiniLM) or 133 MB (bge) as fp32, about half as fp16, plus 0.5 to 0.7 MB of
    tokenizer file: +95 to +140 MB with candle, more than doubling the binary.
- The candle probe was 3.6 to 4 times slower than PyTorch here and peaked at 2.2 GB RSS with
  batch 64; smaller batches would cut memory at some cost in speed.
- At the time the Python reference and the Rust engine agreed to the bit (`bench/parity.py`), a
  mirror since ended. The engine's features are still pinned bit for bit on every platform
  (`crates/scrutate/tests/ranking_pins.rs`); transformer inference through another backend would
  not give the same bits everywhere, so those pins would need a tolerance.

## Recommendation

Do not ship a transformer in the binary now.

The quality gain is real and consistent: with bge-small, learned MRR rises by +0.070 for doc,
+0.055 for name and +0.019 for kw queries on undocumented code, intervals clear of zero; id and
qid queries do not move. The price: 46 to 107 times more indexing compute, about 1 GB of memory
while indexing, 5 to 11 ms per query, a binary more than twice its size, and the loss of
bit-level agreement (then between the reference and the engine, now across platforms).

What would change the answer:

1. Distillation, the cheapest next step: train the code-word model (`bench/train_semantic.py`)
   with bge-small as a teacher, at today's runtime cost. Closing half of the +0.070 doc gap would
   beat shipping the transformer.
2. An opt-in model file: the current model by default and a separately installed transformer for
   users who index once and query often, the way `SCRUTATE_SEMANTIC` already points at another
   file.
3. A measured int8 or fp16 model that keeps the gain at under 40 MB and under 5 s per 1 000
   chunks (not measured here).

## Not measured

- JavaScript or other languages: only the Python TEST set was run.
- ONNX Runtime speed and int8 quantisation: only the library size was measured.
- granite in candle: it uses ModernBERT, which the probe did not load.
- Larger code-specific models (CodeRankEmbed, jina code): skipped, since they need remote model
  code or are well above 100 M parameters.

## Reproduce

`bench/eval_transformers.py` ran on the Python reference engine and was removed with it. Commit
`8a4f4b4` is the last that has it; check it out to run the commands below.

```sh
# in a fresh virtual environment with numpy, scipy, scikit-learn, lightgbm, the scrutate_ref
# dependencies, the benchmark packages, torch and sentence-transformers
export SCRUTATE_WORK=/some/scratch HF_HOME=/some/scratch/hf THREADS=4
python bench/make_bench.py
python bench/eval_transformers.py features current cur TEST
python bench/eval_transformers.py features current cur DEV
python bench/train_ranker.py fit cur ranker_cur.npz
python bench/eval_transformers.py features BAAI/bge-small-en-v1.5 bge TEST
python bench/eval_transformers.py features BAAI/bge-small-en-v1.5 bge DEV
python bench/train_ranker.py fit bge ranker_bge.npz
python bench/eval_transformers.py compare bge ranker_bge.npz cur ranker_cur.npz
python bench/eval_transformers.py speed BAAI/bge-small-en-v1.5 1000
```
