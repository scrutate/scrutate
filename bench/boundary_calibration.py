#!/usr/bin/env python3
"""Calibrate a boundary finding: how often a new import first links two sibling directories, and
whether such imports are taken out again more often than other new imports.

usage: boundary_calibration.py collect --work DIR --scrutate BIN [--only KEY...] [--commits N] [--from DIR]
       boundary_calibration.py report DIR [--max-rate 5.0] [--min-lift 2.0] [--window 200] [--examples 10]

The finding: a change adds a resolved import from file F to file T. The pair is the two
directories just below the common ancestor of F's and T's directories (src/a/b.py importing
lib/c.py gives (src, lib)); there is none when F or T sits in that ancestor itself, so files in one
directory give none. It fires when, before the change, no file under the first directory imports a
file under the second. Left out: F or T a test file (the rule of scrutate's text::is_test_path),
and a pair where either directory did not exist before the change.

collect walks the last N+window first-parent commits before each pinned commit, oldest first, in
DIR/repos/KEY. At the first it indexes the tree and resolves the imports of every indexed file with
the `dependencies` operation of `scrutate batch`. At each later commit it checks the commit out,
indexes it incrementally and resolves the code files the commit changed against its first parent
(renames as a delete and an add). An edge into a deleted file goes with the file. Per commit it
writes one record to DIR/boundary-KEY.jsonl: merge or not, whether it changes a non-test code file,
the new edges with what the rule says of each, and the edges that went away. Every scrutate call
runs with SCRUTATE_SEMANTIC=none: the index is the harness's own and needs no vectors. With --from,
a repository whose DIR/KEY/S holds a clone starts from a copy of its git directory, not the network.
A code file is one scrutate indexes in a language other than data and markup (NOT_CODE).

report counts firings per 100 non-merge code commits (merges apart) over the first N commits after
the base, and the lift: the share of the edges it names that are removed within the window, over
the same share for the other new edges the rule could have named (both files outside tests, a
pair, both directories old). It prints per repository, pooled, and held out (each repository
scored with the others pooled, and the pooled sum over those leave-one-out sets), then examples.
"""
import argparse
import glob
import json
import os
import random
import sqlite3
import subprocess
import sys
import time
from collections import Counter

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from reuse_calibration import REPOS as REUSE_REPOS, log, run  # noqa: E402

REPOS = dict(REUSE_REPOS) | {
    "scrutate": ("https://github.com/scrutate/scrutate.git", "425d0117cd4f06a9bbce416f07275be826a12113"),
    "linux": ("https://github.com/torvalds/linux.git", "c03c1a14507dfaf8b856c2608f9ac216021fc222"),
}
WINDOW = 200
TEST_PARTS = {"test", "tests", "testing", "__tests__", "spec", "specs", "e2e", "fixtures", "mocks"}
JS = {"js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts"}
CLASS = {"java", "kt", "cs", "swift", "php"}
NOT_CODE = {"json", "yaml", "toml", "ini", "xml", "html", "css", "markdown", "rst"}


def is_test(path):
    """text::is_test_path."""
    path = path.replace("\\", "/")
    parts = path.lower().split("/")
    base = parts[-1]
    stem, ext = base.rsplit(".", 1) if "." in base else (base, "")
    dirs = path.rsplit("/", 1)[0] if "/" in path else ""
    written = path.rsplit("/", 1)[-1]
    name = written.rsplit(".", 1)[0] if "." in written else written
    return (any(p in TEST_PARTS for p in parts[:-1])
            or any(d.endswith(".Tests") or d.endswith(".Test") for d in dirs.split("/"))
            or base.startswith("test_")
            or (ext in JS and (stem.endswith(".test") or stem.endswith(".spec")))
            or (ext != "" and (stem.endswith("_test") or stem.endswith("_spec")))
            or (ext in CLASS and (name.endswith("Test") or name.endswith("Tests")))
            or base in ("conftest.py", "tests.py"))


def pair(f, t):
    """The two directories just below the common ancestor of f's and t's directories, or None."""
    a, b = f.split("/")[:-1], t.split("/")[:-1]
    i = 0
    while i < len(a) and i < len(b) and a[i] == b[i]:
        i += 1
    if i == len(a) or i == len(b):
        return None
    return "/".join(a[:i + 1]), "/".join(b[:i + 1])


def dirs_of(path):
    p = path.split("/")[:-1]
    return ["/".join(p[:i]) for i in range(1, len(p) + 1)]


class Scrutate:
    def __init__(self, binary, tmp):
        self.bin, self.tmp = binary, tmp
        self.env = dict(os.environ, SCRUTATE_SEMANTIC="none")
        langs = json.loads(self.call(".", "languages", "--json").stdout)
        langs = [x for x in langs if x["id"] not in NOT_CODE]
        self.exts = {e.lower().lstrip(".") for x in langs for e in list(x["extensions"]) + list(x.get("extensions_by_content") or [])}
        self.names = {n for x in langs for n in x.get("filenames") or []}

    def call(self, root, *args):
        p = subprocess.run([self.bin, "--root", root, *args], capture_output=True, text=True, env=self.env)
        if p.returncode != 0:
            raise RuntimeError(f"scrutate {args[:2]} failed ({p.returncode}): {p.stderr.strip()[-1000:]}")
        return p

    def index(self, root):
        self.call(root, "index", "--no-text")

    def indexed(self, root):
        # shortcut: reads the index's own file table, update this if the schema changes
        db = sqlite3.connect(f"file:{root}/.scrutate/index.db?mode=ro", uri=True)
        try:
            return {r[0] for r in db.execute("SELECT path FROM files")}
        finally:
            db.close()

    def is_code(self, path):
        base = path.rsplit("/", 1)[-1]
        return base in self.names or ("." in base and base.rsplit(".", 1)[1].lower() in self.exts)

    def deps(self, root, paths):
        """{path: set of resolved repository files} for each path, from one batch."""
        paths = sorted(paths)
        if not paths:
            return {}
        f = os.path.join(self.tmp, "deps.jsonl")
        with open(f, "w") as out:
            for p in paths:
                out.write(json.dumps({"op": "dependencies", "path": p}) + "\n")
        lines = self.call(root, "batch", f).stdout.splitlines()
        if len(lines) != len(paths):
            raise RuntimeError(f"batch answered {len(lines)} lines for {len(paths)} paths")
        res = {}
        for p, line in zip(paths, lines):
            rec = json.loads(line)
            if "error" in rec:
                res[p] = set()
                continue
            res[p] = {d["resolved"] for d in rec["result"] if d.get("resolved") and d["resolved"] != p}
        return res


def clone(key, work, src):
    url, commit = REPOS[key]
    r = f"{work}/repos/{key}"
    if not os.path.isdir(f"{r}/.git"):
        os.makedirs(os.path.dirname(r), exist_ok=True)
        if src and os.path.isdir(f"{src}/{key}/S/.git"):
            # a partial clone cannot be cloned from, so its git directory is copied
            os.makedirs(r)
            run(["cp", "-a", f"{src}/{key}/S/.git", f"{r}/.git"])
            run(["rm", "-rf", f"{r}/.git/worktrees"])
        else:
            run(["git", "clone", "-q", "--filter=blob:none", "--no-checkout", url, r])
    if run(["git", "cat-file", "-e", commit + "^{commit}"], cwd=r, check=False).returncode:
        run(["git", "fetch", "-q", url, commit], cwd=r)
    return r


def walk(key, args, sq):
    t0 = time.time()
    r = clone(key, args.work, args.src)
    commits = run(["git", "rev-list", "--first-parent", f"--max-count={args.commits + args.window + 1}", REPOS[key][1]], cwd=r).stdout.split()[::-1]
    git = lambda *a: run(["git", *a], cwd=r).stdout
    git("checkout", "-q", "--force", "--detach", commits[0])
    sq.index(r)
    indexed = sq.indexed(r)
    code = {p for p in indexed if sq.is_code(p)}
    edges = {f: ts for f, ts in sq.deps(r, code).items() if ts}
    files = set(git("ls-tree", "-r", "--name-only", "-z", "HEAD").split("\0")) - {""}
    dirs = Counter(d for p in files for d in dirs_of(p))
    links = Counter(pr for f, ts in edges.items() for t in ts if (pr := pair(f, t)))
    out = open(f"{args.work}/boundary-{key}.jsonl", "w")
    out.write(json.dumps({"base": commits[0], "repo": key, "files": len(files), "code": len(code), "edges": sum(map(len, edges.values())),
                          "commits": len(commits) - 1, "n": args.commits, "window": args.window, "seconds": round(time.time() - t0)}) + "\n")
    log(key, f"base {commits[0][:10]}: {len(code)} code files, {sum(map(len, edges.values()))} edges, {time.time() - t0:.0f} s")
    for i, c in enumerate(commits[1:], 1):
        info = git("show", "-s", "--format=%P%x00%s", c).split("\0")
        merge = len(info[0].split()) > 1
        fields = git("diff", "--name-status", "--no-renames", "-z", c + "^1", c).split("\0")[:-1]
        changes = list(zip(fields[0::2], fields[1::2]))
        git("checkout", "-q", "--force", "--detach", c)
        sq.index(r)
        deleted = {p for s, p in changes if s == "D"}
        present = {p for s, p in changes if s != "D"}
        now = sq.indexed(r) if present else set()
        touched = {p for p in present if p in now and sq.is_code(p)}
        changes_code = any(not is_test(p) and sq.is_code(p) and (p in now or p in code) for _, p in changes)
        res = sq.deps(r, touched)
        new, gone = [], []
        for f in sorted(touched | (deleted & set(edges))):
            old, cur = edges.get(f, set()), res.get(f, set()) - deleted
            gone += [[f, t] for t in sorted(old - cur)]
            for t in sorted(cur - old):
                pr = pair(f, t)
                if is_test(f) or is_test(t):
                    why = "test"
                elif pr is None:
                    why = "nopair"
                elif links[pr]:
                    why = "linked"
                elif not dirs[pr[0]] or not dirs[pr[1]]:
                    why = "newdir"
                else:
                    why = "fire"
                new.append([f, t, why, pr])
        if deleted:
            for f in list(edges):
                hit = edges[f] & deleted
                if hit and f not in deleted and f not in touched:
                    gone += [[f, t] for t in sorted(hit)]
        for f, t in gone:
            edges[f].discard(t)
            if pr := pair(f, t):
                links[pr] -= 1
        for f, t, _, pr in new:
            edges.setdefault(f, set()).add(t)
            if pr:
                links[pr] += 1
        for s, p in changes:
            if s == "D" and p in files:
                files.discard(p)
                dirs.subtract(dirs_of(p))
            elif s != "D" and p not in files:
                files.add(p)
                dirs.update(dirs_of(p))
        code = (code - deleted) | touched
        out.write(json.dumps({"i": i, "commit": c, "subject": info[1].strip(), "merge": merge, "code": changes_code,
                              "changed": len(changes), "new": new, "gone": gone}) + "\n")
        out.flush()
        if i % 50 == 0:
            log(key, f"{i}/{len(commits) - 1} commits, {time.time() - t0:.0f} s")
    out.write(json.dumps({"done": True, "seconds": round(time.time() - t0)}) + "\n")
    out.close()
    log(key, f"done in {time.time() - t0:.0f} s")


def collect(args):
    os.makedirs(args.work, exist_ok=True)
    for key in args.only or sorted(REPOS):
        tmp = f"{args.work}/tmp-{key}"
        os.makedirs(tmp, exist_ok=True)
        walk(key, args, Scrutate(os.path.abspath(args.scrutate), tmp))


def load(d, window):
    """Per repository: the counts the bar needs, and the firings."""
    per, fired = {}, []
    for path in sorted(glob.glob(f"{d}/boundary-*.jsonl")):
        recs = [json.loads(ln) for ln in open(path)]
        head, steps = recs[0], [x for x in recs[1:] if "i" in x]
        if not recs[-1].get("done"):
            log(f"{path}: not finished, {len(steps)} commits")
        n = head["n"]
        gone_at = {}
        for x in steps:
            for f, t in x["gone"]:
                gone_at.setdefault((f, t), []).append(x["i"])
        last = steps[-1]["i"] if steps else 0
        c = Counter()
        for x in steps:
            if x["i"] > n:
                break
            kind = "merge" if x["merge"] else "plain"
            c[kind] += 1
            c[kind + "_code"] += x["code"]
            fires = [e for e in x["new"] if e[2] == "fire"]
            c[kind + "_fired"] += bool(fires) and x["code"]
            c["newdir_edges"] += sum(e[2] == "newdir" for e in x["new"])
            c["newdir_only_commits"] += x["code"] and not fires and any(e[2] == "newdir" for e in x["new"])
            for f, t, why, pr in x["new"]:
                if why not in ("fire", "linked"):
                    c[why + "_edges"] += 1
                    continue
                if x["i"] + window > last:
                    c["unseen"] += 1
                    continue
                removed = any(x["i"] < j <= x["i"] + window for j in gone_at.get((f, t), []))
                side = "f" if why == "fire" else "o"
                c[side] += 1
                c[side + "_removed"] += removed
                if why == "fire":
                    fired.append({"repo": head["repo"], "pair": pr, "f": f, "t": t, "subject": x["subject"], "merge": x["merge"], "removed": removed})
        per[head["repo"]] = c
    if not per:
        sys.exit(f"no boundary-*.jsonl in {d}")
    return per, fired


def per_firing(d, window):
    """Per repository: the firing and other units (commit, pair) the lift can follow, and how many lost any or all edges."""
    per = {}
    for path in sorted(glob.glob(f"{d}/boundary-*.jsonl")):
        recs = [json.loads(ln) for ln in open(path)]
        head, steps = recs[0], [x for x in recs[1:] if "i" in x]
        last = steps[-1]["i"] if steps else 0
        gone_at = {}
        for x in steps:
            for f, t in x["gone"]:
                gone_at.setdefault((f, t), []).append(x["i"])
        c = Counter()
        for x in steps:
            if x["i"] > head["n"] or x["i"] + window > last:
                continue
            units = {}
            for f, t, why, pr in x["new"]:
                if why in ("fire", "linked"):
                    removed = any(x["i"] < j <= x["i"] + window for j in gone_at.get((f, t), []))
                    units.setdefault(("f" if why == "fire" else "o", tuple(pr)), []).append(removed)
            for (side, _), rs in units.items():
                c[side] += 1
                c[side + "_any"] += any(rs)
                c[side + "_all"] += all(rs)
        per[head["repo"]] = c
    return per


def unit_lift(c, k):
    return (c["f_" + k] / c["f"]) / (c["o_" + k] / c["o"]) if c["f"] and c["o"] and c["o_" + k] else float("nan")


def score(c):
    rate = 100 * c["plain_fired"] / c["plain_code"] if c["plain_code"] else float("nan")
    mrate = 100 * c["merge_fired"] / c["merge_code"] if c["merge_code"] else float("nan")
    fs = c["f_removed"] / c["f"] if c["f"] else float("nan")
    os_ = c["o_removed"] / c["o"] if c["o"] else float("nan")
    lift = fs / os_ if c["f"] and c["o"] and os_ else float("nan")
    return rate, mrate, fs, os_, lift


def row(name, c, args):
    rate, mrate, fs, os_, lift = score(c)
    ok = rate <= args.max_rate and lift >= args.min_lift
    print(f"{name:<18} {c['plain_code']:>5} {c['plain_fired']:>4} {rate:>6.2f}  {c['merge_code']:>4} {c['merge_fired']:>4} {mrate:>6.2f}"
          f"  {c['f_removed']:>4}/{c['f']:<5} {fs:>6.3f}  {c['o_removed']:>5}/{c['o']:<6} {os_:>6.3f}  {lift:>5.2f}  {c['newdir_edges']:>5}  {'pass' if ok else 'fail'}")


def report(args):
    per, fired = load(args.dir, args.window)
    print(f"bar: rate <= {args.max_rate} per 100 non-merge code commits, lift >= {args.min_lift} (removal within {args.window} commits)")
    hdr = (f"{'repository':<18} {'code':>5} {'fire':>4} {'/100':>6}  {'mrg':>4} {'fire':>4} {'/100':>6}"
           f"  {'named removed':>10} {'share':>6}  {'other removed':>12} {'share':>6}  {'lift':>5}  {'newdir':>6}  bar")
    print("\nper repository (code: non-merge code commits; mrg: merge code commits; newdir: edges the new-directory rule left out)")
    print(hdr)
    for k, c in sorted(per.items()):
        row(k, c, args)
    pooled = sum(per.values(), Counter())
    print("\npooled")
    print(hdr)
    row("all", pooled, args)
    row("all but linux", sum((c for k, c in per.items() if k != "linux"), Counter()), args)
    print("\nheld out: each row is the pool with that repository left out")
    print(hdr)
    worst = []
    for k in sorted(per):
        c = pooled - per[k]
        row(f"without {k}"[:18], c, args)
        worst.append(score(c))
    print(f"held out range: rate {min(w[0] for w in worst):.2f} to {max(w[0] for w in worst):.2f}, "
          f"lift {min(w[4] for w in worst):.2f} to {max(w[4] for w in worst):.2f}")
    print("\nlift per firing (one commit and one pair; removed when any, or all, of its edges are removed)")
    units = per_firing(args.dir, args.window)
    pool_u = sum(units.values(), Counter())
    for k in ("any", "all"):
        ho = [unit_lift(pool_u - units[r], k) for r in units]
        print(f"  {k}: pooled {unit_lift(pool_u, k):.2f} ({pool_u['f_' + k]}/{pool_u['f']} named, {pool_u['o_' + k]}/{pool_u['o']} other), "
              f"held out {min(ho):.2f} to {max(ho):.2f}")
    print(f"\nnew-directory exclusion: {pooled['newdir_edges']} edges, and {pooled['newdir_only_commits']} code commits "
          f"(within the first N) that would have fired on it alone")
    print(f"other new edges: {pooled['test_edges']} test, {pooled['nopair_edges']} no pair; {pooled['unseen']} edges too late to follow")
    print(f"\nexamples ({args.examples}, at most 4 per repository, seeded)")
    rnd = random.Random(7)
    rnd.shuffle(fired)
    shown, seen = Counter(), 0
    for e in fired:
        if shown[e["repo"]] >= 4 or seen >= args.examples:
            continue
        shown[e["repo"]] += 1
        seen += 1
        print(f"  {e['repo']}: ({e['pair'][0]}, {e['pair'][1]}) {e['f']} -> {e['t']}"
              f"{' [merge]' if e['merge'] else ''}{' [removed]' if e['removed'] else ''}\n      {e['subject']}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("collect", help="clone, walk the history, resolve imports (the build box)")
    c.add_argument("--work", required=True)
    c.add_argument("--scrutate", required=True)
    c.add_argument("--only", nargs="+", choices=sorted(REPOS))
    c.add_argument("--commits", type=int, default=400, help="commits evaluated; the walk takes 200 more")
    c.add_argument("--window", type=int, default=WINDOW, help="commits walked after the last one evaluated")
    c.add_argument("--from", dest="src", help="a directory of existing clones, KEY/S each")
    r = sub.add_parser("report", help="the rate, the lift and the bar (anywhere)")
    r.add_argument("dir")
    r.add_argument("--max-rate", type=float, default=5.0)
    r.add_argument("--min-lift", type=float, default=2.0)
    r.add_argument("--window", type=int, default=WINDOW)
    r.add_argument("--examples", type=int, default=10)
    args = ap.parse_args()
    collect(args) if args.cmd == "collect" else report(args)


if __name__ == "__main__":
    main()
