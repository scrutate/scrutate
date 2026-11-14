#!/usr/bin/env python3
"""Train / evaluate the learned ranker on saved candidate features.

usage:
  train_ranker.py cv      TAG                  3-fold package-held-out CV on DEV features
  train_ranker.py fit     TAG OUT.npz          fit on all DEV features, save ensemble
  train_ranker.py eval    TAG [OUT.npz]        evaluate scorers on TEST features (+ ranker if given)
  train_ranker.py compare TAG OLD.npz NEW.npz [OUT.json] [--gate]
                                               two rankers on TEST, all languages and each one;
                                               --gate exits 1 when the new ranker is below BM25 on
                                               a row of 50 queries or more, or below the old ranker
                                               over all queries of a variant, by more than the
                                               confidence interval
  train_ranker.py sealed  TAG RANKER.npz       the ranker on the SEALED group, all languages and each
                                               one (bench/eval_sealed.sh runs it once per ranker)

Each query type is reported twice: the queries whose answer is a function or method ("doc"), and
those whose answer is a type ("doc:type"). Older feature files have function answers only.
DEV, TEST and SEALED include the other-language packages of groups.CODE_DEV / CODE_TEST /
CODE_SEALED unless PYTHON_ONLY=1. RANKER_PARAMS='{"n_estimators": 400}' overrides PARAMS (for
tuning with cv); TYPE_WEIGHT=0.5 overrides the weight of a type query's rows in training (default 1: every query
counts the same; 0 leaves the type queries out).

The feature names, the features and the engine's linear blend of each candidate come from the
feature files (bench/gen_features.py); the ranker sees the PRUNE candidates best by that blend, as
in the engine. DROP zeroes features for the ranker and leaves the blend as the engine computed it.
"""
from __future__ import annotations

import glob
import gzip
import json
import os
import pickle
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import bootstrap_ci, fmt_row, mrr_values, paired_diff_ci, summarize  # noqa: E402
from groups import CODE_DEV, CODE_SEALED, CODE_TEST, DEV, SEALED, TEST, WORK, language  # noqa: E402

from scrutate_ref.rank import TreeEnsemble  # noqa: E402

PRUNE = 200  # the engine's search::PRUNE
FEATURES: list[str] = []  # the engine's feature names, read with the first feature file
FI: dict[str, int] = {}

DROP = [p for p in os.environ.get("DROP", "").split(",") if p]  # ablation: zero these feature groups

PARAMS = dict(objective="lambdarank", metric="ndcg", n_estimators=250, learning_rate=0.06,
              num_leaves=31, min_child_samples=40, subsample=0.8, subsample_freq=1,
              colsample_bytree=0.8, reg_lambda=1.0, lambdarank_truncation_level=20,
              verbose=-1, n_jobs=2)
if os.environ.get("THREADS"):
    PARAMS["n_jobs"] = int(os.environ["THREADS"])
PARAMS.update(json.loads(os.environ.get("RANKER_PARAMS", "{}")))
if not os.environ.get("PYTHON_ONLY"):
    DEV, TEST, SEALED = DEV + CODE_DEV, TEST + CODE_TEST, SEALED + CODE_SEALED
# 1: every query counts the same. Package-held-out MRR on DEV over all queries by this weight:
# 0.55 (0), 0.62 (0.1), 0.64 (0.3), 0.65 (1); less lifts the function doc and kw rows by up to
# 0.03 and costs the type rows up to 0.4.
TYPE_WEIGHT = float(os.environ.get("TYPE_WEIGHT", 1.0))
GATE_MIN_ROW = 50
TYPES = ("doc", "kw", "name", "id", "qid")
ROWS = TYPES + tuple(t + ":type" for t in TYPES)  # function answers, then type answers


def row_of(rec) -> str:
    return rec["type"] + (":type" if rec["q"].get("target_kind") == "type" else "")


def folds() -> list[list[str]]:
    """The Python folds; each other language's packages dealt round-robin over them."""
    out = [["pydantic", "werkzeug"], ["pptx", "seaborn", "joblib"], ["docx", "openpyxl"]]
    by_lang: dict[str, list[str]] = {}
    for p in DEV:
        if language(p) != "python":
            by_lang.setdefault(language(p).replace("typescript", "javascript"), []).append(p)
    for pkgs in by_lang.values():
        for i, p in enumerate(pkgs):
            out[i % 3].append(p)
    return out


def load(tag: str, pkgs: list[str]):
    out = {}
    for pkg in pkgs:
        recs = []
        for f in sorted(glob.glob(os.path.join(WORK, "feat", tag, f"*_{pkg}.pkl"))):
            with open(f, "rb") as fh:
                gz = fh.read(2) == b"\x1f\x8b"
            with (gzip.open if gz else open)(f, "rb") as fh:
                d = pickle.load(fh)
            if not FEATURES:
                FEATURES.extend(d["features"])
                FI.update((n, i) for i, n in enumerate(FEATURES))
            assert d["features"] == FEATURES, f"{f} has other features than the files before it; regenerate features"
            assert all("lin" in r for r in d["recs"]), f"{f} has no linear blend; regenerate features"
            drop = [i for i, n in enumerate(FEATURES) if any(n.startswith(p) for p in DROP)]
            for r in d["recs"]:
                r["variant"] = os.path.basename(f).split("_")[0]
                r["lang"] = language(pkg)
                if drop:
                    r["X"] = r["X"].copy()
                    r["X"][:, drop] = 0.0
            recs += d["recs"]
        out[pkg] = recs
    return out


def prune_indices(lin: np.ndarray, n: int = PRUNE) -> np.ndarray:
    if len(lin) <= n:
        return np.arange(len(lin))
    return np.argsort(-lin, kind="stable")[:n]  # ties: lower index first, as in the engine


def pruned(rec):
    X, y = rec["X"], rec["y"]
    keep = prune_indices(rec["lin"])
    return X[keep], y[keep], keep


def build(recs):
    """-> (X, y, group sizes, row weights); a type query's rows weigh TYPE_WEIGHT."""
    Xs, ys, groups, ws = [], [], [], []
    for r in recs:
        w = TYPE_WEIGHT if r["q"].get("target_kind") == "type" else 1.0
        if r["y"].sum() == 0 or w == 0:
            continue
        X, y, _ = pruned(r)
        if y.sum() == 0:
            continue
        Xs.append(X); ys.append(y); groups.append(len(y)); ws.append(np.full(len(y), w))
    return np.vstack(Xs), np.concatenate(ys), np.array(groups), np.concatenate(ws)


def fit(recs, params=None):
    import lightgbm as lgb
    X, y, g, w = build(recs)
    m = lgb.LGBMRanker(**(params or PARAMS))
    m.fit(X, y, group=g, sample_weight=None if (w == 1).all() else w)
    return m


def rank_of_first(scores, y, limit=10):
    order = np.argsort(-scores)[:limit]
    pos = np.nonzero(y[order])[0]
    return int(pos[0]) + 1 if len(pos) else None


SCORERS = {
    "bm25 (lexical)": lambda r: r["X"][:, FI["lex_log"]],
    "semantic only": lambda r: r["X"][:, FI["sem_mix"]],
    "linear blend": lambda r: r["lin"],
}


def evaluate(recs, ranker=None, types=ROWS):
    res = {}
    scorers = dict(SCORERS)
    if ranker is not None:
        def ltr(r):
            X = r["X"]
            keep = prune_indices(r["lin"])
            out = np.full(len(X), -1e9, np.float32)
            out[keep] = ranker.predict(X[keep])
            return out
        scorers["LEARNED ranker"] = ltr
    for t in types:
        rs = [r for r in recs if row_of(r) == t]
        if not rs:
            continue
        for name, fn in scorers.items():
            res.setdefault(t, {})[name] = [rank_of_first(fn(r), r["y"]) for r in rs]
    return res


def print_eval(res, title):
    print(f"\n=== {title}")
    for t, d in res.items():
        print(f"-- {t}")
        for name, ranks in d.items():
            print("  " + fmt_row(name, ranks))
        if "LEARNED ranker" in d:
            m, lo, hi = paired_diff_ci(d["LEARNED ranker"], d["bm25 (lexical)"])
            print(f"  learned - bm25 MRR diff: {m:+.3f} [{lo:+.3f},{hi:+.3f}]")


def main() -> None:
    cmd, tag = sys.argv[1], sys.argv[2]
    if cmd == "cv":
        data = load(tag, DEV)
        allres: dict = {}
        print("params:", PARAMS, "type weight:", TYPE_WEIGHT)
        langs = sorted({r["lang"] for rs in data.values() for r in rs})
        for held in folds():
            train = [r for p, rs in data.items() if p not in held for r in rs]
            m = fit(train)
            ens = TreeEnsemble.from_lightgbm(m.booster_, FEATURES)
            for lang in ["all"] + langs:
                res = evaluate([r for p in held for r in data[p] if lang in ("all", r["lang"])], ens)
                for t, d in res.items():
                    for name, ranks in d.items():
                        allres.setdefault(lang, {}).setdefault(t, {}).setdefault(name, []).extend(ranks)
            print(f"held-out {held}: done", flush=True)
        for lang, res in allres.items():
            print_eval(res, f"3-fold package-held-out CV on DEV, {lang}")
            for label, sel in (("function", lambda t: ":" not in t), ("type", lambda t: ":" in t)):
                v = [v for t, d in res.items() if sel(t) for v in mrr_values(d["LEARNED ranker"])]
                if v:
                    print(f"  learned ranker MRR@10 over all {lang} {label} queries: {np.mean(v):.4f} (n={len(v)})")
    elif cmd == "fit":
        data = load(tag, DEV)
        recs = [r for rs in data.values() for r in rs]
        m = fit(recs)
        ens = TreeEnsemble.from_lightgbm(m.booster_, FEATURES)
        # equivalence check: exported numpy inference == lightgbm
        X = np.vstack([r["X"] for r in recs[:50]])
        d = np.abs(ens.predict(X) - m.booster_.predict(X)).max()
        print(f"export check: max |numpy - lightgbm| = {d:.2e}")
        ens.save(sys.argv[3])
        imp = sorted(zip(m.booster_.feature_importance("gain"), FEATURES), reverse=True)[:12]
        print("top features by gain:", ", ".join(f"{n}" for _, n in imp))
        print("saved", sys.argv[3], f"{os.path.getsize(sys.argv[3])/1e3:.0f} KB")
    elif cmd == "eval":
        data = load(tag, TEST)
        ens = TreeEnsemble.load(sys.argv[3]) if len(sys.argv) > 3 else None
        recs = [r for rs in data.values() for r in rs]
        for variant in ("A", "B"):
            print_eval(evaluate([r for r in recs if r["variant"] == variant], ens),
                       f"TEST (held-out packages), variant {variant}")
    elif cmd == "compare":
        args = [a for a in sys.argv[3:] if a != "--gate"]
        data = load(tag, TEST)
        old, new = TreeEnsemble.load(args[0]), TreeEnsemble.load(args[1])
        recs = [r for rs in data.values() for r in rs]
        out: dict = {}
        worse, below, overall = [], [], []
        for lang in ["all"] + sorted({r["lang"] for r in recs}):
            for variant in ("A", "B"):
                sub = [r for r in recs if lang in ("all", r["lang"]) and r["variant"] == variant]
                a, b = evaluate(sub, old), evaluate(sub, new)
                print(f"\n=== TEST {lang}, variant {variant}: old ranker -> new ranker")
                every_old, every_new = [], []
                for t in ROWS:
                    if t not in a:
                        continue
                    ro, rn = a[t]["LEARNED ranker"], b[t]["LEARNED ranker"]
                    so, sn = summarize(ro), summarize(rn)
                    (lo1, hi1), (lo2, hi2) = bootstrap_ci(mrr_values(ro)), bootstrap_ci(mrr_values(rn))
                    m, lo, hi = paired_diff_ci(rn, ro)
                    bm = summarize(a[t]["bm25 (lexical)"])["MRR"]
                    print(f"  {t:<8} n={so['n']:<5} bm25 {bm:.3f}  old {so['MRR']:.3f} [{lo1:.3f},{hi1:.3f}]  "
                          f"new {sn['MRR']:.3f} [{lo2:.3f},{hi2:.3f}]  new-old {m:+.3f} [{lo:+.3f},{hi:+.3f}]  "
                          f"hit@1/5/10 old {so['hit@1']:.3f}/{so['hit@5']:.3f}/{so['hit@10']:.3f} "
                          f"new {sn['hit@1']:.3f}/{sn['hit@5']:.3f}/{sn['hit@10']:.3f}")
                    out.setdefault(lang, {}).setdefault(variant, {})[t] = {
                        "n": so["n"], "bm25": bm, "old": {**so, "ci": [lo1, hi1]},
                        "new": {**sn, "ci": [lo2, hi2]}, "new_minus_old": [m, lo, hi]}
                    if ":" not in t and hi < 0:
                        worse.append(f"{lang} {variant} {t}: {m:+.3f} [{lo:+.3f},{hi:+.3f}]")
                    bm_m, bm_lo, bm_hi = paired_diff_ci(rn, b[t]["bm25 (lexical)"])
                    # seven queries cannot carry an interval: a small row is printed, not judged
                    if bm_hi < 0 and so["n"] >= GATE_MIN_ROW:
                        below.append(f"{lang} {variant} {t}: {bm_m:+.3f} [{bm_lo:+.3f},{bm_hi:+.3f}]")
                    every_old += ro
                    every_new += rn
                m, lo, hi = paired_diff_ci(every_new, every_old)
                so, sn = summarize(every_old), summarize(every_new)
                print(f"  every query n={so['n']:<5} old {so['MRR']:.3f}  new {sn['MRR']:.3f}  new-old {m:+.3f} [{lo:+.3f},{hi:+.3f}]")
                out.setdefault(lang, {}).setdefault(variant, {})["every query"] = {
                    "n": so["n"], "old": so, "new": sn, "new_minus_old": [m, lo, hi]}
                if hi < 0:
                    overall.append(f"{lang} {variant}: {m:+.3f} [{lo:+.3f},{hi:+.3f}]")
        if len(args) > 2:
            with open(args[2], "w") as f:
                json.dump(out, f, indent=1)
        print("\nfunction rows worse beyond the confidence interval:", "; ".join(worse) or "none")
        print(f"rows of {GATE_MIN_ROW} queries or more below BM25 beyond the confidence interval:", "; ".join(below) or "none")
        print("worse over all queries beyond the confidence interval:", "; ".join(overall) or "none")
        if (below or overall) and "--gate" in sys.argv:
            sys.exit(1)
    elif cmd == "sealed":
        data = load(tag, SEALED)
        missing = [p for p, rs in data.items() if not rs]
        if missing:
            sys.exit(f"no {tag} features for {', '.join(missing)}")
        ens = TreeEnsemble.load(sys.argv[3])
        recs = [r for rs in data.values() for r in rs]
        for lang in ["all"] + sorted({r["lang"] for r in recs}):
            for variant in ("A", "B"):
                res = evaluate([r for r in recs if lang in ("all", r["lang"]) and r["variant"] == variant], ens)
                print_eval(res, f"SEALED (held out, evaluated once) {lang}, variant {variant}")
                for label, sel in (("function", lambda t: ":" not in t), ("type", lambda t: ":" in t)):
                    v = [v for t, d in res.items() if sel(t) for v in mrr_values(d["LEARNED ranker"])]
                    if v:
                        print(f"  learned ranker MRR@10 over all {lang} {label} queries: {np.mean(v):.4f} (n={len(v)})")

if __name__ == "__main__":
    main()
