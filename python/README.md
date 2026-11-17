# scrutate-ref

The training code for the models of [Scrutate](../README.md), in Python. It is not the product:
the product is the Rust binary in `crates/scrutate`.

`bench/train_ranker.py` and `bench/train_semantic.py` fit the ranker and the semantic vectors
that the Rust engine ships (`crates/scrutate/models/`). This package holds the code they import,
one module each: the export of a LightGBM ensemble to the arrays the engine reads (`rank.py`, for
`train_ranker.py`) and the vector model's tables and file format (`semantic.py`, for
`train_semantic.py`). The rows they train on come from the engine itself
(`bench/gen_features.py`, the hidden `views` command).

This package used to be also a second engine that parsed, indexed and ranked six languages
bit for bit like the Rust one, and `bench/parity.py` compared the two. That mirror ended: the
engine is the one producer of training rows, and ranking is watched by the quality floor of
`bench/quality.py` and the pinned features of `crates/scrutate/tests/ranking_pins.rs`
(docs/QUALITY.md).

Install it for development and run its tests:

```sh
pip install -e "python[test]" -c python/constraints.txt
SCRUTATE_BIN=target/release/scrutate python -m pytest python/tests -q --ignore=python/tests/test_bindings.py
```

The Python bindings of the product are a separate package, `crates/scrutate-py`.
