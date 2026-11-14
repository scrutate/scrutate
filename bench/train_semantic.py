#!/usr/bin/env python3
"""Train the semantic model *contrastively* on the real task: docstring sentence -> undocumented function.

The shipped v3 model is a PLS-SVD of the (description words x body identifiers) co-occurrence. This
trainer keeps the exact same inference (two embedding tables, sublinear-tf x idf pooling, L2 norm,
see python/scrutate_ref/semantic.py) but learns the tables with an InfoNCE objective over three pairings:

    q  = query_terms(first docstring sentence)            (what a user types)
    A  = name x3 + enclosing class + signature words      (the chunk's description *without* docstring)
    B  = body identifiers                                 (the chunk's implementation)

    loss = nce(q, A) + nce(q, B) + lam * nce(A, B)

Negatives are the other functions of the batch; functions of one file stay together in a batch, so
most negatives are hard (same module, same vocabulary). Model selection uses the DEV packages only
(semantic-only MRR, scored exactly as the engine scores: q . (A + B)); TEST packages are never read.

usage:
  train_semantic.py extract CACHE.pkl [--pkgs LIST]   # token lists of the pretraining packages
  train_semantic.py train CACHE.pkl OUT.npz [--init crates/scrutate/models/semantic.npz] [--epochs 40]
                        [--batch 1024] [--lr 2e-3] [--tau 0.05] [--lam 0.5] [--k 256] [--seed 0]
                        [--drop 0.2] [--wd 0.0]        # word dropout, decoupled weight decay
  train_semantic.py eval MODEL.npz [--types doc,kw]   # semantic-only MRR on DEV, variant A
"""
from __future__ import annotations

import math
import os
import pickle
import re
import sys
import time
from collections import Counter

import numpy as np
import scipy.sparse as sp

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import engine_views, first_relevant_rank, load_queries, summarize  # noqa: E402
from groups import BENCH, DEV, PRETRAIN, SP  # noqa: E402

from scrutate_ref.semantic import SemanticModel, _sublog, view_counts  # noqa: E402

SENT_END = re.compile(r"(?<=[.!?])\s+(?=[A-Z`(\[])")


def clean_rst(s: str) -> str:
    s = re.sub(r":\w+:`~?([^`]+)`", r"\1", s)
    s = s.replace("``", "").replace("`", "").replace("*", "")
    return " ".join(s.split())


def first_sentence(ds: str) -> str:
    ds = ds.strip()
    cut = len(ds)
    para = re.search(r"\n\s*\n", ds)
    m = SENT_END.search(ds)
    if para:
        cut = min(cut, para.start())
    if m:
        cut = min(cut, m.start())
    return clean_rst(ds[:cut]).rstrip(".:;, ")


# ------------------------------------------------------------------------------ extraction
TEST_PARTS = {"test", "tests", "testing", "_tests", "conftest.py", "benchmarks", "asv_bench", "docs",
              "examples", "doc"}


def _is_excluded(path: str) -> bool:
    parts = path.lower().split(os.sep)
    return any(p in TEST_PARTS or p.startswith("test_") or p.endswith("_test.py") for p in parts)


def view_request(path: str, rel: str) -> dict:
    """The file by its path, or its text when the file has carriage returns: the model was trained
    on files read with universal newlines, which the engine's own read of the bytes keeps."""
    with open(path, "rb") as f:
        raw = f.read()
    if b"\r" not in raw:
        return {"file": path, "path": rel}
    try:
        with open(path, encoding="utf8") as f:
            return {"text": f.read(), "path": rel}
    except UnicodeDecodeError:
        return {"file": path, "path": rel}


def query_terms_of(texts: list[str]) -> dict[str, list[str]]:
    """The engine's query terms of each text."""
    texts = list(dict.fromkeys(texts))
    return {r["query"]: r["terms"] for r in engine_views([{"query": t} for t in texts])}


def _functions(rec: dict) -> list[tuple]:
    """The documented-or-not functions of one file: (name, qual, sig, doc tokens, doc text, body)."""
    out = []
    for c in rec.get("chunks", []):  # a file that does not read or parse has none
        v = c["views"]
        if c["kind"] not in ("function", "method") or not v["name"] or len(v["body"]) < 2:
            continue
        out.append((v["name"], v["qual"], v["sig"], v["doc"], c["doc"], v["body"]))
    return out


def extract(cache: str, pkg_list: str | None) -> None:
    """pkg_list: file with one package directory name per line (the shipped model's 140 packages are
    listed in MODEL_CARD.md); without it, every local package that is not a DEV/TEST package."""
    from groups import SEALED, TEST

    if pkg_list:
        pkgs = [p for p in open(pkg_list).read().split() if os.path.isdir(os.path.join(SP, p))]
    else:
        pkgs = [p for p in os.listdir(SP) if os.path.isdir(os.path.join(SP, p)) and not p.endswith(".dist-info")]
    banned = set(DEV) | set(TEST) | set(SEALED) | {"pip", "setuptools", "pkg_resources", "_distutils_hack"}  # pip vendors rich
    assert not (set(pkgs) & banned), sorted(set(pkgs) & banned)
    dirs = [os.path.join(SP, p) for p in sorted(pkgs) if p not in banned and (not p.startswith("_") or p in PRETRAIN)]
    files = []
    for root in dirs:
        for dp, dns, fns in os.walk(root):
            dns[:] = [d for d in dns if d != "__pycache__"]
            for f in fns:
                if f.endswith(".py"):
                    p = os.path.join(dp, f)
                    if not _is_excluded(os.path.relpath(p, SP)):
                        files.append(p)
    t0 = time.time()
    # one list per file (kept together: hard negatives)
    found = [g for g in map(_functions, engine_views([view_request(p, os.path.relpath(p, SP)) for p in files])) if g]
    terms = query_terms_of([first_sentence(f[4]) for g in found for f in g if f[4]])
    groups = [[(a, b, c, d, terms[first_sentence(doc)] if doc else [], body) for a, b, c, d, doc, body in g] for g in found]
    n = sum(len(g) for g in groups)
    nd = sum(1 for g in groups for f in g if len(f[4]) >= 2)
    print(f"{len(dirs)} packages, {len(files)} files, {n:,} functions, {nd:,} with a usable docstring sentence ({time.time()-t0:.0f}s)")
    with open(cache, "wb") as f:
        pickle.dump({"groups": groups, "packages": [os.path.basename(d) for d in dirs]}, f)


# ------------------------------------------------------------------------------ data matrices
def build_vocab(groups, min_df=2, max_vocab=30000, max_df_frac=0.4):
    n = sum(len(g) for g in groups)
    dfa, dfb = Counter(), Counter()
    for g in groups:
        for name_s, qual_s, sig_s, doc_s, q, body_s in g:
            a, b = view_counts(name_s, qual_s, sig_s, doc_s, body_s)
            dfa.update(a.keys())
            dfb.update(b.keys())
    out = []
    for df in (dfa, dfb):
        keep = sorted(((t, c) for t, c in df.items() if min_df <= c <= max_df_frac * n), key=lambda x: -x[1])[:max_vocab]
        vocab = {t: i for i, (t, _) in enumerate(keep)}
        idf = np.array([math.log(1 + n / c) for _, c in keep], np.float32)
        out.append((vocab, idf))
    return out


def weight_matrix(counts_list, vocab, idf) -> sp.csr_matrix:
    """Rows = the exact pooling weights scrutate_ref.semantic._proj uses: sublog(tf) * idf, L2-normalised."""
    rows, cols, vals = [], [], []
    for r, counts in enumerate(counts_list):
        idx, w = [], []
        for t, c in counts.items():
            i = vocab.get(t)
            if i is not None:
                idx.append(i)
                w.append(_sublog(c) * idf[i])
        if idx:
            nrm = math.sqrt(sum(x * x for x in w))
            rows.extend([r] * len(idx))
            cols.extend(idx)
            vals.extend(x / nrm for x in w)
    return sp.csr_matrix((np.array(vals, np.float32), (rows, cols)), shape=(len(counts_list), len(vocab)))


# ------------------------------------------------------------------------------ model pieces
def encode(W: sp.csr_matrix, E: np.ndarray):
    U = W @ E
    nrm = np.sqrt((U * U).sum(1, keepdims=True)) + 1e-8
    return U / nrm, nrm


def back(W: sp.csr_matrix, dZ: np.ndarray, Z: np.ndarray, nrm: np.ndarray) -> np.ndarray:
    dU = (dZ - Z * (dZ * Z).sum(1, keepdims=True)) / nrm
    return (W.T @ dU).astype(np.float32)


def nce(Zx: np.ndarray, Zy: np.ndarray, tau: float, valid: np.ndarray | None = None):
    """Symmetric InfoNCE between matched rows; rows where `valid` is False (e.g. no docstring) are
    dropped from the loss but still serve as negatives. -> loss, dZx, dZy"""
    L = (Zx @ Zy.T) / tau
    n = len(L)
    eye = np.eye(n, dtype=np.float32)
    Pr = np.exp(L - L.max(1, keepdims=True))
    Pr /= Pr.sum(1, keepdims=True)
    Pc = np.exp(L - L.max(0, keepdims=True))
    Pc /= Pc.sum(0, keepdims=True)
    v = np.ones(n, np.float32) if valid is None else valid.astype(np.float32)
    m = max(float(v.sum()), 1.0)
    loss = float((-np.log(np.diag(Pr) + 1e-12) * v).sum() + (-np.log(np.diag(Pc) + 1e-12) * v).sum()) / (2 * m)
    G = ((Pr - eye) * v[:, None] + (Pc - eye) * v[None, :]) / (2 * m)
    return loss, (G @ Zy) / tau, (G.T @ Zx) / tau


class Adam:
    def __init__(self, shape, lr):
        self.m = np.zeros(shape, np.float32)
        self.v = np.zeros(shape, np.float32)
        self.t = 0
        self.lr = lr

    def step(self, E, g, lr=None):
        self.t += 1
        self.m = 0.9 * self.m + 0.1 * g
        self.v = 0.999 * self.v + 0.001 * g * g
        mh = self.m / (1 - 0.9 ** self.t)
        vh = self.v / (1 - 0.999 ** self.t)
        E -= (lr or self.lr) * mh / (np.sqrt(vh) + 1e-8)


# ------------------------------------------------------------------------------ DEV evaluation
_DEV_CACHE: dict = {}


def dev_data(variant="A"):
    """Chunks of the DEV packages (masked copies) as (name, qual, sig, doc, body) token lists + queries."""
    if variant in _DEV_CACHE:
        return _DEV_CACHE[variant]
    data = []
    for pkg in DEV:
        root = os.path.join(BENCH, "src", variant, pkg)
        files = [os.path.join(dp, fn) for dp, _, fns in os.walk(root) for fn in fns if fn.endswith(".py")]
        chunks = []
        for rec in engine_views([view_request(p, os.path.relpath(p, root)) for p in files]):
            for c in rec.get("chunks", []):
                if c["kind"] != "window":
                    v = c["views"]
                    chunks.append((rec["path"], c["qualname"], c["name"], v["name"], v["qual"], v["sig"], v["doc"], v["body"]))
        # its chunks carry no kind, so the type answers are left out
        data.append((pkg, chunks, [q for q in load_queries(pkg) if q.get("target_kind") != "type"]))
    _DEV_CACHE[variant] = data
    _DEV_CACHE["terms"] = {**_DEV_CACHE.get("terms", {}), **query_terms_of([q["query"] for _, _, qs in data for q in qs])}
    return data


class H:
    __slots__ = ("path", "qualname", "name")

    def __init__(self, p, q, n):
        self.path, self.qualname, self.name = p, q, n


def evaluate(model: SemanticModel, types=("doc", "kw"), variant="A") -> dict:
    """Semantic-only retrieval scored like the engine: query . (A + B) over all chunks of a package."""
    ranks = {t: [] for t in types}
    for pkg, chunks, qs in dev_data(variant):
        M = np.zeros((len(chunks), model.k), np.float32)
        hits = []
        for i, (rel, qn, nm, name_s, qual_s, sig_s, doc_s, body_s) in enumerate(chunks):
            za = model.encode_chunk(name_s, qual_s, sig_s, doc_s, [])
            zb = model.encode_chunk([], [], [], [], body_s)
            if za is not None:
                M[i] += za
            if zb is not None:
                M[i] += zb
            hits.append(H(rel, qn, nm))
        for q in qs:
            if q["type"] not in types:
                continue
            z = model.encode_query(_DEV_CACHE["terms"][q["query"]])
            if z is None:
                ranks[q["type"]].append(None)
                continue
            sc = M @ z
            top = np.argpartition(-sc, 9)[:10]
            top = top[np.argsort(-sc[top])]
            ranks[q["type"]].append(first_relevant_rank(q, [hits[i] for i in top]))
    return {t: summarize(r) for t, r in ranks.items()}


# ------------------------------------------------------------------------------ training
def _dropout(W: sp.csr_matrix, drop: float, rng) -> sp.csr_matrix:
    """Randomly drop a share of the words of every bag (the encoder renormalises, so no rescaling)."""
    if drop <= 0:
        return W
    W = W.copy()
    W.data = W.data * (rng.random(len(W.data)) >= drop)
    return W


def train(cache: str, out: str, init: str | None, epochs: int, batch: int, lr: float, tau: float,
          lam: float, k: int, seed: int, drop: float = 0.0, wd: float = 0.0) -> None:
    with open(cache, "rb") as f:
        groups = pickle.load(f)["groups"]
    rng = np.random.default_rng(seed)
    (va, ia), (vb, ib) = build_vocab(groups)
    funcs = [f for g in groups for f in g]
    file_of = np.repeat(np.arange(len(groups)), [len(g) for g in groups])
    n = len(funcs)
    print(f"{n:,} functions in {len(groups):,} files; vocab A {len(va):,} B {len(vb):,}", flush=True)
    t0 = time.time()
    WQ = weight_matrix([Counter(f[4]) for f in funcs], va, ia)
    WA = weight_matrix([view_counts(f[0], f[1], f[2], [], [])[0] for f in funcs], va, ia)
    WB = weight_matrix([view_counts([], [], [], [], f[5])[1] for f in funcs], vb, ib)
    has_q = np.asarray(WQ.getnnz(axis=1) >= 2)
    has_a = np.asarray(WA.getnnz(axis=1) > 0)
    has_b = np.asarray(WB.getnnz(axis=1) > 0)
    print(f"matrices built ({time.time()-t0:.0f}s): {int(has_q.sum()):,} usable queries", flush=True)

    EA = (rng.standard_normal((len(va), k)) * 0.05).astype(np.float32)
    EB = (rng.standard_normal((len(vb), k)) * 0.05).astype(np.float32)
    if init:
        m0 = SemanticModel.load(init)
        assert m0.k == k, "init model has another dimension"
        ca = cb = 0
        for t, i in va.items():
            j = m0.va.get(t)
            if j is not None:
                EA[i] = m0.U[j]
                ca += 1
        for t, i in vb.items():
            j = m0.vb.get(t)
            if j is not None:
                EB[i] = m0.V[j]
                cb += 1
        # the SVD tables have a scale of their own; bring both to unit rms so the lr means the same thing
        EA *= 1.0 / (np.sqrt((EA * EA).mean()) + 1e-8) * 0.05
        EB *= 1.0 / (np.sqrt((EB * EB).mean()) + 1e-8) * 0.05
        print(f"warm start from {init}: {ca:,}/{len(va):,} A words, {cb:,}/{len(vb):,} B words", flush=True)
    optA, optB = Adam(EA.shape, lr), Adam(EB.shape, lr)

    def make_model(EA_, EB_):
        cfg = {"k": k, "min_df": 2, "max_vocab": 30000, "n_pairs": n, "gamma": 0.0, "wa": 0.5,
               "objective": "infonce(q,A)+infonce(q,B)+%.2f*infonce(A,B)" % lam, "tau": tau,
               "epochs": epochs, "batch": batch, "lr": lr, "drop": drop, "wd": wd,
               "init": os.path.basename(init) if init else None}
        return SemanticModel(sorted(va, key=va.get), sorted(vb, key=vb.get), ia, ib, EA_.copy(), EB_.copy(),
                             np.ones(k, np.float32), cfg)

    best, best_ep = -1.0, -1
    steps_total = epochs * math.ceil(n / batch)
    step = 0
    base = evaluate(make_model(EA, EB))
    print(f"epoch  0  dev doc MRR {base['doc']['MRR']:.3f}  kw MRR {base['kw']['MRR']:.3f}  (start)", flush=True)
    for ep in range(1, epochs + 1):
        t0 = time.time()
        order = np.concatenate([np.flatnonzero(file_of == g) for g in rng.permutation(len(groups))])
        losses = []
        for s0 in range(0, n, batch):
            idx = order[s0:s0 + batch]
            if len(idx) < 64:
                continue
            frac = step / steps_total
            cur_lr = lr * (0.5 * (1 + math.cos(math.pi * frac)))  # cosine decay
            step += 1
            wq, wa, wb = _dropout(WQ[idx], drop, rng), _dropout(WA[idx], drop, rng), _dropout(WB[idx], drop, rng)
            Zq, nq = encode(wq, EA)
            Za, na = encode(wa, EA)
            Zb, nb = encode(wb, EB)
            vqa, vqb, vab = has_q[idx] & has_a[idx], has_q[idx] & has_b[idx], has_a[idx] & has_b[idx]
            l1, dq1, da1 = nce(Zq, Za, tau, vqa)
            l2, dq2, db2 = nce(Zq, Zb, tau, vqb)
            l3, da3, db3 = nce(Za, Zb, tau, vab)
            losses.append(l1 + l2 + lam * l3)
            gA = back(wq, dq1 + dq2, Zq, nq) + back(wa, da1 + lam * da3, Za, na)
            gB = back(wb, db2 + lam * db3, Zb, nb)
            if wd > 0:  # decoupled weight decay
                EA *= 1 - cur_lr * wd
                EB *= 1 - cur_lr * wd
            optA.step(EA, gA, cur_lr)
            optB.step(EB, gB, cur_lr)
        msg = f"epoch {ep:>2}  loss {np.mean(losses):.3f}  ({time.time()-t0:.0f}s)"
        if ep % 2 == 0 or ep == epochs:
            r = evaluate(make_model(EA, EB))
            msg += f"  dev doc MRR {r['doc']['MRR']:.3f}  kw MRR {r['kw']['MRR']:.3f}"
            if r["doc"]["MRR"] > best:
                best, best_ep = r["doc"]["MRR"], ep
                make_model(EA, EB).save(out)
                msg += "  *saved*"
        print(msg, flush=True)
    print(f"best dev doc MRR {best:.3f} at epoch {best_ep}; model written to {out}")


def main() -> None:
    a = sys.argv[1:]
    opts = {}
    for flag in ("--init", "--epochs", "--batch", "--lr", "--tau", "--lam", "--k", "--seed", "--types", "--pkgs",
                 "--drop", "--wd"):
        if flag in a:
            i = a.index(flag)
            opts[flag] = a[i + 1]
            del a[i:i + 2]
    cmd = a[0]
    if cmd == "extract":
        extract(a[1], opts.get("--pkgs"))
    elif cmd == "train":
        train(a[1], a[2], opts.get("--init"), int(opts.get("--epochs", 40)), int(opts.get("--batch", 1024)),
              float(opts.get("--lr", 2e-3)), float(opts.get("--tau", 0.05)), float(opts.get("--lam", 0.5)),
              int(opts.get("--k", 256)), int(opts.get("--seed", 0)), float(opts.get("--drop", 0.0)),
              float(opts.get("--wd", 0.0)))
    elif cmd == "eval":
        m = SemanticModel.load(a[1])
        r = evaluate(m, tuple(opts.get("--types", "doc,kw").split(",")))
        for t, v in r.items():
            print(f"{t:<4} MRR {v['MRR']:.3f}  hit@1 {v['hit@1']:.3f}  hit@5 {v['hit@5']:.3f}  hit@10 {v['hit@10']:.3f}  n={v['n']}")
    else:
        print(__doc__)


if __name__ == "__main__":
    main()
