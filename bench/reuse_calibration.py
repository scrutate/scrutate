#!/usr/bin/env python3
"""Calibrate a reuse finding: recall and false alarms of `similar` and `search` on 13 pinned repositories.

usage: reuse_calibration.py collect --tasks FILE --work DIR --scrutate BIN [--only KEY...]
       reuse_calibration.py report DIR [--max-false-alarms 1.0] [--min-recall 0.05]

A positive is a coding-agent session that wrote a definition where it should have called an
existing helper. A false alarm is a definition the repository itself added in its history: if
the finding fires on it, it fires on ordinary work. `collect` measures both, per repository;
`report` sweeps the thresholds offline.

collect clones each repository into DIR/repos/KEY/S at its pinned commit and DIR/repos/KEY/H (a
worktree that walks the history), indexes both and writes DIR/reuse-KEY.jsonl:
- false alarms: the functions and methods outside test files that `impact --base C~1` calls
  added, walking first-parent history back from the pinned commit until --fa-target definitions
  are found or --fa-max-commits are walked; each is queried at S where it stands.
- positives: each session that did not call the helper, its file put in S, indexed, queried and
  removed again.
Per definition the record holds its body size (lines, and word tokens), the raw `similar -k 10`
and `search -k 10 "<name> <first line>"` results with each candidate's size, and the best
candidate after the filters with its cosine. A definition too short for a vector has `similar`
null. The binary must build vectors, so never run it with SCRUTATE_SEMANTIC=none.

The tasks file is not in the repository, as it holds the agents' session files. Format: a JSON
list of
  {"task": NAME, "repo": "github.com/OWNER/NAME", "commit": SHA, "path": the file the task writes,
   "helper": a regex on a definition's name, for the helper the session should have called,
   "sessions": [{"id": ID, "calls_helper": bool, "source": the file the session wrote}]}
Each task's repository and commit must be one of REPOS.

report reads DIR/reuse-*.jsonl (or features-*.jsonl, the same records under an older name).
Recall counts the positives with at least one function or method, a positive being found when
one of its definitions has the helper as its best candidate and that candidate passes the rule.
False alarms are per 100 added definitions, those with no vector included (they never fire).
The filters before taking the best: function or method only, no test file, not the session's
own file (positives) or the adding commit's changed files (false alarms). It prints the T sweep
and then the best points of each variant against the bar: (1) a minimum body size on both sides,
(2) a Jaccard band on the best candidate, (3) helper directories under test paths allowed,
(4a) the top search result alone, (4b) the best candidate gated by the search top k, and their
combination. For the combination's point it prints the hits and false alarms per repository and,
as a sensitivity check, the same point with each repository's records dropped. Then it holds
each repository out in turn: the point is picked on the other 12 by the same rule and scored on
the one left out, and the 13 scores are pooled. A point picked from thousands on the same records
can fit them; the held-out score is what the bar is judged on.
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys
import time

REPOS = {
    "atuin": ("https://github.com/atuinsh/atuin.git", "854654ec1ec6d13e0eb2468a6e57b2e2044a2162"),
    "bitwarden": ("https://github.com/bitwarden/server.git", "8fb62cff78af0b5fa6c7bc3b02abe7c8e9d881e3"),
    "bookstack": ("https://github.com/BookStackApp/BookStack.git", "045c6a98cac21ea86906674bb91d01f375460db6"),
    "cal.diy": ("https://github.com/calcom/cal.diy.git", "176037d0afbe572f870a3c702985e7cd83fe6c0c"),
    "ctfd": ("https://github.com/CTFd/CTFd.git", "d21c9cd3bb154e0e285aba2a20e176777b5f7d5d"),
    "dolphinscheduler": ("https://github.com/apache/dolphinscheduler.git", "89817f16f69b30ae3a12631f4f96653e1d3c96ce"),
    "eslint": ("https://github.com/eslint/eslint.git", "dc1e7a8416937edefe04cf836ee202a6fc03bedd"),
    "forem": ("https://github.com/forem/forem.git", "b5237b126cf27bd3897086e7811e33f7872ec444"),
    "gitea": ("https://github.com/go-gitea/gitea.git", "facc0f2b15460a545151dfb324aa1ef66ccae695"),
    "kotlinconf-app": ("https://github.com/JetBrains/kotlinconf-app.git", "907ce238e1ffe2472991eeb2d2bfa386bdb4ea8f"),
    "mastodon": ("https://github.com/mastodon/mastodon.git", "3f5c32f43f57751f994919335cf7b065c53044d1"),
    "openfoodnetwork": ("https://github.com/openfoodfoundation/openfoodnetwork.git", "4e121e10f1acde5aa975a4b8a37c421f2080565d"),
    "whitehall": ("https://github.com/alphagov/whitehall.git", "44848361bcf7f8871c716ac6e5f8f6211416a1a7"),
}
K = 10
KINDS = {"function", "method"}
TEST_DIR = re.compile(r"(^|/)(test|tests|spec|__tests__)/")
TEST_NAME = re.compile(r"(_test\.[^/]+|^test_[^/]*\.[^/]+|\.spec\.[^/]+|\.test\.[^/]+)$")
TOK = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
HELPER_DIR = re.compile(r"(^|/)((tests?|spec|__tests__)/(_?utils?|_?helpers?|support|test_?helpers|test_?utils|testutils?|testing)"
                        r"|support|test_?helpers|test_?support|testutils?|testing)/")
HELPER_FILE = re.compile(r"(^|/)(tests?|spec|__tests__)/([^/]+/)*(test_?helpers?|spec_helper|rails_helper|helpers?|utils?|support|conftest)\.[^/]+$")
TS = [round(0.50 + i / 100, 2) for i in range(50)]


def is_test(path):
    return bool(TEST_DIR.search(path) or TEST_NAME.search(os.path.basename(path)))


def parts(qualname):
    return re.split(r"\.|::|#|->", qualname)


def is_helper(c, rx, paths):
    return c["path"] in paths and any(re.search(rx, p) for p in parts(c["qualname"]))


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, file=sys.stderr, flush=True)



def run(cmd, cwd=None, check=True):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if check and p.returncode != 0:
        raise RuntimeError(f"{cmd} failed ({p.returncode}): {p.stderr.strip()[-2000:]}")
    return p


class Scrutate:
    def __init__(self, binary):
        self.bin = binary

    def json(self, root, *args):
        """A --json command's result; None on exit 1 (no results)."""
        p = run([self.bin, "--root", root, *args, "--json"], check=False)
        if p.returncode == 1:
            return None
        if p.returncode != 0:
            raise RuntimeError(f"scrutate {args} failed ({p.returncode}): {p.stderr.strip()[-1000:]}")
        return json.loads(p.stdout)["result"]

    def index(self, root):
        run([self.bin, "--root", root, "index", "--no-text"])

    def outline(self, root, path):
        return [d for d in self.json(root, "outline", path) or [] if d["kind"] in KINDS]

    def similar(self, root, path, line):
        try:
            return self.json(root, "similar", f"{path}:{line}", "-k", str(K)) or []
        except RuntimeError as e:  # a definition with no vector
            log("similar failed", path, line, str(e)[:200])
            return None

    def search(self, root, q):
        try:
            return self.json(root, "search", "-k", str(K), q) or []
        except RuntimeError as e:
            log("search failed", q[:80], str(e)[:200])
            return None


def load_tasks(path):
    by_url = {url: key for key, (url, _) in REPOS.items()}
    out = {}
    for t in json.load(open(path)):
        key = by_url.get(f"https://{t['repo']}.git")
        if key is None or t["commit"] != REPOS[key][1]:
            sys.exit(f"task {t['task']}: {t['repo']} at {t['commit']} is not a pinned repository")
        out.setdefault(key, []).append(t)
    return out


def checkout(key, work, sq):
    """S at the pinned commit and the H worktree, both indexed."""
    url, commit = REPOS[key]
    s, h = f"{work}/repos/{key}/S", f"{work}/repos/{key}/H"
    if not os.path.isdir(f"{s}/.git"):
        os.makedirs(os.path.dirname(s), exist_ok=True)
        # the history walk checks out hundreds of commits, so no shallow clone; blobs come on demand
        if run(["git", "clone", "-q", "--filter=blob:none", "--no-checkout", url, s], check=False).returncode:
            run(["git", "clone", "-q", "--no-checkout", url, s])
    if run(["git", "cat-file", "-e", commit + "^{commit}"], cwd=s, check=False).returncode:
        run(["git", "fetch", "-q", "origin", commit], cwd=s)
    run(["git", "checkout", "-q", "--force", "--detach", commit], cwd=s)
    if not os.path.isdir(h):
        run(["git", "worktree", "add", "-q", "--detach", h, commit], cwd=s)
    for d in (s, h):
        head = run(["git", "rev-parse", "HEAD"], cwd=d).stdout.strip()
        if head != commit:
            sys.exit(f"{key}: {d} holds {head}, not the pinned {commit}")
        sq.index(d)
    return s, h


class Files:
    def __init__(self, root):
        self.root, self.cache = root, {}

    def lines(self, path):
        if path not in self.cache:
            try:
                self.cache[path] = open(os.path.join(self.root, path), errors="replace").read().split("\n")
            except OSError:
                self.cache[path] = None
        return self.cache[path]

    def size(self, path, start, end):
        ls = self.lines(path)
        if ls is None or start is None or end is None:
            return None, None
        return end - start + 1, len(TOK.findall("\n".join(ls[start - 1:end])))

    def first_line(self, path, start):
        ls = self.lines(path)
        return ls[start - 1].strip() if ls and 0 < start <= len(ls) else ""


def best(cands, drop, path, qualname):
    for c in cands:
        if c["kind"] in KINDS and c["path"] not in drop and not is_test(c["path"]) and (c["path"], c["qualname"]) != (path, qualname):
            return c
    return None


def measure(sq, files, root, path, d, drop):
    """One definition: size, raw similar and search, and the best candidate after the filters."""
    n, t = files.size(path, d["start"], d["end"])
    q = f"{d['name']} {files.first_line(path, d['start'])}".strip()
    sim = sq.similar(root, path, d["start"])
    if sim is not None:
        sim = [{k: c[k] for k in ("path", "qualname", "kind", "start", "end", "cosine", "jaccard")}
               | dict(zip(("lines", "tokens"), files.size(c["path"], c["start"], c["end"]))) for c in sim]
    res = sq.search(root, q)
    if res is not None:
        res = [{"path": r["path"], "qualname": r["name"], "kind": r["kind"], "start": r["start"], "end": r["end"], "score": r["score"]}
               for r in res]
    b = best(sim or [], drop, path, d["qualname"])
    return {"qualname": d["qualname"], "name": d["name"], "line": d["start"], "end": d["end"], "lines": n, "tokens": t,
            "query": q, "similar": sim, "search": res, "no_vector": sim is None,
            "best": b and {k: b[k] for k in ("qualname", "path", "kind", "start")},
            "cosine": b["cosine"] if b else None, "jaccard": b["jaccard"] if b else None}


def added_defs(key, s, h, sq, target, max_commits):
    """(commit, path, outline entry at S) for definitions first-parent history added."""
    commit = REPOS[key][1]
    at_s, found, seen, walked = {}, [], set(), 0
    for c in run(["git", "rev-list", "--first-parent", f"--max-count={max_commits}", commit], cwd=s).stdout.split():
        if len(found) >= target:
            break
        walked += 1
        if run(["git", "rev-parse", "-q", "--verify", c + "~1"], cwd=s, check=False).returncode:
            break
        changed = [p for p in run(["git", "diff", "--name-only", "--no-renames", c + "~1", c], cwd=s).stdout.split("\n") if p]
        if not any(not is_test(p) and os.path.isfile(os.path.join(s, p)) for p in changed):
            continue
        run(["git", "checkout", "-q", "--force", "--detach", c], cwd=h)
        sq.index(h)
        for d in (sq.json(h, "impact", "--base", c + "~1", "--depth", "0") or {}).get("changed", []):
            if d.get("status") != "added" or d.get("kind") not in KINDS or is_test(d["path"]) or (d["path"], d["qualname"]) in seen:
                continue
            if d["path"] not in at_s:
                at_s[d["path"]] = {x["qualname"]: x for x in sq.outline(s, d["path"])} if os.path.isfile(os.path.join(s, d["path"])) else {}
            sd = at_s[d["path"]].get(d["qualname"])
            if sd:
                seen.add((d["path"], d["qualname"]))
                found.append((c, d["path"], sd))
    run(["git", "checkout", "-q", "--force", "--detach", commit], cwd=h)
    log(key, f"walked {walked} commits, {len(found)} added definitions")
    return found


def false_alarms(key, s, h, sq, args, out):
    files, shown = Files(s), {}
    for c, path, d in added_defs(key, s, h, sq, args.fa_target, args.fa_max_commits):
        if c not in shown:
            shown[c] = [x for x in run(["git", "show", "--name-only", "--format=", "--diff-merges=first-parent", c], cwd=s).stdout.split("\n") if x]
        rec = {"set": "fa", "repo": key, "commit": c, "path": path, "changed": shown[c]}
        rec |= measure(sq, files, s, path, d, set(shown[c]))
        out.write(json.dumps(rec | {"is_helper": False}) + "\n")


def helper_paths(s, sq, rx):
    """Where `scrutate symbol` puts the helper, for each name in the regex."""
    paths = set()
    for n in re.sub(r"[\\^$()]", "", rx).split("|"):
        for d in sq.json(s, "symbol", n) or []:
            if any(re.search(rx, p) for p in parts(d["name"])):
                paths.add(d["path"])
    return paths


def session(key, s, sq, t, sess, hp):
    path, rx = t["path"], t["helper"]
    rec = {"set": "pos", "repo": key, "task": t["task"], "session": sess["id"], "path": path, "helper": rx,
           "helper_paths": sorted(hp), "defs": []}
    full = os.path.join(s, path)
    if sess["source"].strip():
        if os.path.exists(full):
            sys.exit(f"{path} already exists at the pinned commit")
        os.makedirs(os.path.dirname(full), exist_ok=True)
        open(full, "w").write(sess["source"])
        sq.index(s)
        files = Files(s)
        for d in sq.outline(s, path):
            x = measure(sq, files, s, path, d, {path})
            hc = [c for c in x["similar"] or [] if c["kind"] in KINDS and is_helper(c, rx, hp)]
            x["is_helper"] = bool(x["best"] and is_helper(x["best"], rx, hp))
            x["helper_cosine"] = max((c["cosine"] for c in hc), default=None)
            rec["defs"].append(x)
        os.remove(full)
        run(["git", "checkout", "-q", "--", "."], cwd=s)
        run(["git", "clean", "-fdq"], cwd=s)  # no -x: the ignored .scrutate stays
    hit = [d for d in rec["defs"] if d["is_helper"]]
    pick = max(hit or [d for d in rec["defs"] if d["cosine"] is not None] or [None], key=lambda d: d["cosine"] if d else 0)
    rec.update({"qualname": pick and pick["qualname"], "best": pick and pick["best"], "cosine": pick and pick["cosine"],
                "jaccard": pick and pick["jaccard"], "is_helper": bool(hit), "no_defs": not rec["defs"]})
    return rec


def positives(key, s, sq, tasks, out):
    for t in tasks:
        hp = helper_paths(s, sq, t["helper"])
        log(t["task"], "helper at", sorted(hp))
        for sess in t["sessions"]:
            if not sess["calls_helper"]:
                out.write(json.dumps(session(key, s, sq, t, sess, hp)) + "\n")
                out.flush()
    sq.index(s)


def collect(args):
    tasks = load_tasks(args.tasks)
    sq = Scrutate(os.path.abspath(args.scrutate))
    for key in args.only or sorted(tasks):
        if key not in tasks:
            sys.exit(f"no task for {key}")
        t0 = time.time()
        s, h = checkout(key, args.work, sq)
        with open(f"{args.work}/reuse-{key}.jsonl", "w") as out:
            false_alarms(key, s, h, sq, args, out)
            positives(key, s, sq, tasks[key], out)
        log(key, f"done in {time.time() - t0:.0f} s")



def helper_dir(p):
    return bool(HELPER_DIR.search(p) or HELPER_FILE.search(p))


def allowed(c, drop, hd):
    if c["kind"] not in KINDS or c["path"] in drop:
        return False
    return not is_test(c["path"]) or (hd and helper_dir(c["path"]))


def size(x, unit):
    v = x.get(unit)
    return 10 ** 9 if v is None else v  # unknown size never blocks


def units(r):
    """([(added definition, dropped paths)], helper regex, helper paths) for a record."""
    if r["set"] == "fa":
        return [(r, set(r["changed"]) | {r["path"]})], None, None
    return [(x, {r["path"]}) for x in r["defs"]], r["helper"], set(r["helper_paths"])


def search_rank(r, b, x, drop, hd):
    """Rank (from 1) of the best candidate among the filtered search results, or None."""
    i = 0
    for s in x["search"] or []:
        if not allowed(s, drop, hd) or (s["path"] == r["path"] and s["start"] == x["line"]):
            continue
        i += 1
        if s["path"] == b["path"] and (s["start"] == b["start"] or s["qualname"] == parts(b["qualname"])[-1]):
            return i
    return None


class Sweep:
    def __init__(self, recs, fa_max, rec_min):
        self.fa = [r for r in recs if r["set"] == "fa"]
        self.pos = [r for r in recs if r["set"] == "pos" and r["defs"]]
        self.fa_max, self.rec_min, self.cache = fa_max, rec_min, {}

    def prep(self, filt):
        """Per record, per added definition: (best, is helper, search rank of the best)."""
        if filt not in self.cache:
            unit, mn, hd = filt
            out = []
            for r in self.fa + self.pos:
                defs, rx, hp = units(r)
                row = []
                for x, drop in defs:
                    if x["similar"] is None or (mn and size(x, unit) < mn):
                        continue
                    b = next((c for c in x["similar"] if allowed(c, drop, hd) and not (mn and size(c, unit) < mn)), None)
                    if b is not None:
                        row.append((b, r["set"] == "fa" or is_helper(b, rx, hp), search_rank(r, b, x, drop, hd)))
                out.append(row)
            self.cache[filt] = out
        return self.cache[filt]

    def score(self, flags):
        nfa = len(self.fa)
        fa_f, pos_f = flags[:nfa], flags[nfa:]
        return {"fa_per_100": round(100 * sum(fa_f) / nfa, 2), "recall": round(sum(pos_f) / len(self.pos), 4),
                "fa": sum(fa_f), "hits": sum(pos_f)}

    def flags(self, filt, g):
        return [any(h and g(b, k) for b, h, k in row) for row in self.prep(filt)]

    def points(self, filt, params):
        """params: (label, gate(best, search rank)) pairs; every point scored."""
        return [{**lab, **self.score(self.flags(filt, g))} for lab, g in params]

    def by_repo(self, filt, g):
        """Per repository at one point: [false alarms, added definitions, hits, positives]."""
        out = {}
        for r, f in zip(self.fa + self.pos, self.flags(filt, g)):
            c = out.setdefault(r["repo"], [0, 0, 0, 0])
            i = 0 if r["set"] == "fa" else 2
            c[i] += f
            c[i + 1] += 1
        return out

    def best(self, pts):
        ok = [p for p in pts if p["fa_per_100"] <= self.fa_max]
        ok2 = [p for p in pts if p["recall"] >= self.rec_min]
        return (max(ok, key=lambda p: (p["recall"], -p["fa_per_100"])) if ok else None,
                min(ok2, key=lambda p: (p["fa_per_100"], -p["recall"])) if ok2 else None)

    def search_alone(self, s_min, hd):
        flags = []
        for r in self.fa + self.pos:
            defs, rx, hp = units(r)
            fire = False
            for x, drop in defs:
                top = next((s for s in x["search"] or [] if allowed(s, drop, hd) and not (s["path"] == r["path"] and s["start"] == x["line"])), None)
                if top and top["score"] >= s_min and (r["set"] == "fa" or is_helper(top, rx, hp)):
                    fire = True
            flags.append(fire)
        return self.score(flags)


def totals(rep):
    fa, nfa, hits, npos = (sum(v[i] for v in rep.values()) for i in range(4))
    return {"fa_per_100": round(100 * fa / nfa, 2), "recall": round(hits / npos, 4), "fa": fa, "hits": hits}


def cos_gate(t):
    return lambda b, k: b["cosine"] >= t


def gate(t, lo, hi, k=None):
    hi_ = 1.01 if hi == 1.0 else hi
    return lambda b, r: b["cosine"] >= t and lo <= b["jaccard"] < hi_ and (k is None or (r is not None and r <= k))


def show(name, pair):
    for what, p in zip(("most recall at the false-alarm bar", "fewest false alarms at the recall bar"), pair):
        print(f"  {name} {what}: " + ("none" if not p else " ".join(f"{a}={b}" for a, b in p.items())))


def sensitivity(rep):
    """A point's hits and false alarms per repository, and with each repository's records dropped."""
    print("  per repository at that point: hits of its positives, false alarms of its added definitions")
    for k, (fa, nfa, hits, npos) in sorted(rep.items()):
        print(f"    {k:<17} {hits:>3} of {npos:<3} {fa:>3} of {nfa}")
    total = [sum(v[i] for v in rep.values()) for i in range(4)]
    print("  sensitivity: the same point with one repository's records dropped")
    for k, v in sorted(rep.items()):
        fa, nfa, hits, npos = (t - x for t, x in zip(total, v))
        print(f"    without {k:<17} recall {hits}/{npos} = {hits / npos:.3f}, {100 * fa / nfa:.2f} false alarms per 100")


def held_out(sw, per):
    """Leave one repository out: pick the point on the other 12 by the same rule, score it on the
    one left out, and pool the 13 scores. A repository where no point passes on the other 12 gets
    no finding at all."""
    repos = sorted(per[0][1])
    pooled = [0, 0, 0, 0]
    print("  held out: the point picked on the other 12, scored on the one left out")
    for out in repos:
        def rest(rep):
            return [sum(v[i] for k, v in rep.items() if k != out) for i in range(4)]
        ok = [(lab, rep, r) for lab, rep in per for r in [rest(rep)] if 100 * r[0] / r[1] <= sw.fa_max]
        if ok:
            lab, rep, _ = max(ok, key=lambda c: (c[2][2] / c[2][3], -c[2][0] / c[2][1]))
            fa, nfa, hits, npos = rep[out]
            label = " ".join(f"{a}={b}" for a, b in lab.items())
        else:
            (fa, nfa, hits, npos), label = (0, per[0][1][out][1], 0, per[0][1][out][3]), "none passes"
        pooled = [a + b for a, b in zip(pooled, (fa, nfa, hits, npos))]
        print(f"    {out:<17} {hits:>3} of {npos:<3} {fa:>3} of {nfa:<4} ({label})")
    fa, nfa, hits, npos = pooled
    print(f"  held out, pooled: {100 * fa / nfa:.2f} false alarms per 100, recall {hits}/{npos} = {hits / npos:.3f}")


def load_records(d):
    files = sorted(glob.glob(f"{d}/reuse-*.jsonl")) or sorted(glob.glob(f"{d}/features-*.jsonl"))
    if not files:
        sys.exit(f"no reuse-*.jsonl or features-*.jsonl in {d}")
    return [json.loads(line) for f in files for line in open(f)]


def report(args):
    sw = Sweep(load_records(args.dir), args.max_false_alarms, args.min_recall)
    nv = sum(1 for r in sw.fa if r["similar"] is None)
    print(f"false-alarm definitions {len(sw.fa)} (no vector {nv}), positives {len(sw.pos)}")
    base = sw.points(("lines", 0, False), [({"T": t}, cos_gate(t)) for t in TS])
    print("T     recall        fa/100")
    for p in base[::5] + base[-1:]:
        print(f"{p['T']:.2f}  {p['hits'] / len(sw.pos):.3f} ({p['hits']:>3})  {p['fa_per_100']:.2f}")
    hb = sum(any(x["similar"] and (b := next((c for c in x["similar"] if allowed(c, {r["path"]}, False)), None))
                 and is_helper(b, r["helper"], set(r["helper_paths"])) for x in r["defs"]) for r in sw.pos)
    print(f"the helper is the best candidate in {hb} positives")
    show("T alone", sw.best(base))

    print("\n(1) minimum size on both sides")
    v1 = {}
    for unit, mins in (("lines", [2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 25, 30]), ("tokens", [5, 10, 15, 20, 30, 40, 50, 60, 80, 100, 150])):
        for mn in mins:
            v1[(unit, mn)] = sw.best(sw.points((unit, mn, False), [({"unit": unit, "min": mn, "T": t}, cos_gate(t)) for t in TS]))
            show(f"{unit}>={mn}", v1[(unit, mn)])

    print("\n(2) Jaccard band [lo, hi)")
    bands = [(lo / 20, hi / 20) for lo in range(0, 20) for hi in range(lo + 1, 21)]
    v2pts = sw.points(("lines", 0, False), [({"lo": lo, "hi": hi, "T": t}, gate(t, lo, hi)) for lo, hi in bands for t in TS])
    show("band", sw.best(v2pts))

    print("\n(3) helper directories under test paths")
    show("helper dirs", sw.best(sw.points(("lines", 0, True), [({"T": t}, cos_gate(t)) for t in TS])))

    print("\n(4a) search alone")
    for hd in (False, True):
        pts = [{"S": s, **sw.search_alone(s, hd)} for s in [round(i / 100, 2) for i in range(0, 101)] + [1.05, 1.1, 1.2, 1.5, 2.0]]
        show("helper dirs" if hd else "plain", sw.best(pts))
        print(f"    most recall at any S: {max(p['recall'] for p in pts)}")

    print("\n(4b) best candidate in the search top k")
    for k in (1, 3, 5, 10):
        show(f"k={k}", sw.best(sw.points(("lines", 0, False), [({"k": k, "T": t}, gate(t, 0.0, 1.0, k)) for t in TS])))

    print("\ncombined: the top 3 size filters and Jaccard bands, helper directories on or off, with or without (4b)")
    size_top = sorted(v1, key=lambda f: (v1[f][0] is None, -(v1[f][0] or {}).get("recall", 0), (v1[f][1] or {}).get("fa_per_100", 999)))[:3]
    seen, band_top = set(), []
    ranked = sorted(v2pts, key=lambda p: (p["fa_per_100"] > sw.fa_max, -p["recall"], p["fa_per_100"]))
    for p in ranked + sorted(v2pts, key=lambda p: (p["recall"] < sw.rec_min, p["fa_per_100"])):
        if (p["lo"], p["hi"]) not in seen:
            seen.add((p["lo"], p["hi"]))
            band_top.append((p["lo"], p["hi"]))
        if len(band_top) == 3:
            break
    per = []
    for unit, mn in [("lines", 0)] + size_top:
        for hd in (False, True):
            for lo, hi in [(0.0, 1.0)] + band_top:
                for k in (1, 3, 5, 10, None):
                    for t in TS:
                        lab = {"unit": unit, "min": mn, "helper_dirs": hd, "lo": lo, "hi": hi, "k": k, "T": t}
                        per.append((lab, sw.by_repo((unit, mn, hd), gate(t, lo, hi, k))))
    combo = [{**lab, **totals(rep)} for lab, rep in per]
    print(f"  {len(combo)} points")
    top = sw.best(combo)
    show("combined", top)
    if top[0]:
        sensitivity(next(rep for lab, rep in per if lab == {k: top[0][k] for k in lab}))
    held_out(sw, per)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("collect", help="clone, walk the history, query each definition (the build box)")
    c.add_argument("--tasks", required=True)
    c.add_argument("--work", required=True)
    c.add_argument("--scrutate", required=True)
    c.add_argument("--only", nargs="+", choices=sorted(REPOS))
    c.add_argument("--fa-target", type=int, default=100, help="stop the history walk at this many added definitions")
    c.add_argument("--fa-max-commits", type=int, default=600)
    r = sub.add_parser("report", help="sweep the thresholds and variants over the records (anywhere)")
    r.add_argument("dir")
    r.add_argument("--max-false-alarms", type=float, default=1.0, help="per 100 added definitions")
    r.add_argument("--min-recall", type=float, default=0.05)
    args = ap.parse_args()
    collect(args) if args.cmd == "collect" else report(args)


if __name__ == "__main__":
    main()
