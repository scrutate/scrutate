#!/usr/bin/env python3
"""The acceptance run of `read --deps`: 40 hand-labelled calls into four real packages, each read
at the version its fixture repository's lockfile pins.

usage: deps_acceptance.py setup --work DIR
       deps_acceptance.py run --work DIR --scrutate BIN

setup makes four fixture repositories under DIR and installs their dependencies with the
ecosystems' own tools, from the network: Python with requests in `.venv` (uv), express in
`node_modules` (npm), serde in the cargo registry (cargo) and gin in the Go module cache (go). The
cargo and Go stores are DIR's own (CARGO_HOME, GOMODCACHE), and each holds a second version of the
package as well, so an answer from the wrong version can be told. A second Rust repository pins the
same serde.

run asks `read --deps --symbol CALL --json` for each call. A call passes when the answer holds the
definition its label names (a file of the package, and the line a pattern finds in that file: the
definition's range holds it, since a range starts at its attributes and doc comment) and every path
in it is under the locked version's directory; it counts strictly too (the range starts at that
line). It prints each failure, the counts (the bar: 36 of 40 and none from another version), the
median of 5 of: the first call into serde (the copy and its index made), the same call from the
second repository (the cached copy read), a read of the cached copy as a repository of its own
(the bar: the second repository's call costs no more than 1.5 times it) and a `read --symbol` of
the repository's own code (the start-up every call pays), and whether each repository's own index
file is byte for byte the same before and after.
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import time

# (package directory key, call, file under the package, pattern of the definition's first line)
CALLS = [
    ("py", "requests.get", "requests/api.py", r"^def get\("),
    ("py", "requests.post", "requests/api.py", r"^def post\("),
    ("py", "requests.request", "requests/api.py", r"^def request\("),
    ("py", "requests.Session", "requests/sessions.py", r"^class Session\("),
    ("py", "requests.Session.send", "requests/sessions.py", r"^    def send\("),
    ("py", "requests.Response.json", "requests/models.py", r"^    def json\("),
    ("py", "requests.Response.raise_for_status", "requests/models.py", r"^    def raise_for_status\("),
    ("py", "requests.PreparedRequest", "requests/models.py", r"^class PreparedRequest\("),
    ("py", "requests.HTTPError", "requests/exceptions.py", r"^class HTTPError\("),
    ("py", "requests.adapters.HTTPAdapter", "requests/adapters.py", r"^class HTTPAdapter\("),
    ("js", "express.application.listen", "lib/application.js", r"^app\.listen = function listen\("),
    ("js", "express.application.use", "lib/application.js", r"^app\.use = function use\("),
    ("js", "express.application.render", "lib/application.js", r"^app\.render = function render\("),
    ("js", "express.response.json", "lib/response.js", r"^res\.json = function json\("),
    ("js", "express.response.send", "lib/response.js", r"^res\.send = function send\("),
    ("js", "express.response.status", "lib/response.js", r"^res\.status = function status\("),
    ("js", "express.response.redirect", "lib/response.js", r"^res\.redirect = function redirect\("),
    ("js", "express.response.sendFile", "lib/response.js", r"^res\.sendFile = function sendFile\("),
    ("js", "express.request.accepts", "lib/request.js", r"^req\.accepts = function\("),
    ("js", "express.createApplication", "lib/express.js", r"^function createApplication\("),
    ("rs", "serde::Deserialize", "src/de/mod.rs", r"^pub trait Deserialize<'de>"),
    ("rs", "serde::Serialize", "src/ser/mod.rs", r"^pub trait Serialize \{"),
    ("rs", "serde::Serializer", "src/ser/mod.rs", r"^pub trait Serializer"),
    ("rs", "serde::Deserializer", "src/de/mod.rs", r"^pub trait Deserializer<'de>"),
    ("rs", "serde::de::Visitor", "src/de/mod.rs", r"^pub trait Visitor<'de>"),
    ("rs", "serde::de::DeserializeOwned", "src/de/mod.rs", r"^pub trait DeserializeOwned"),
    ("rs", "serde::ser::SerializeStruct", "src/ser/mod.rs", r"^pub trait SerializeStruct \{"),
    ("rs", "serde::de::IgnoredAny", "src/de/ignored_any.rs", r"^pub struct IgnoredAny"),
    ("rs", "serde::de::value::StrDeserializer", "src/de/value.rs", r"^pub struct StrDeserializer"),
    ("rs", "serde::de::Unexpected", "src/de/mod.rs", r"^pub enum Unexpected"),
    ("go", "gin.Default", "gin.go", r"^func Default\("),
    ("go", "gin.New", "gin.go", r"^func New\("),
    ("go", "gin.Engine", "gin.go", r"^type Engine struct"),
    ("go", "gin.Engine.Run", "gin.go", r"^func \(engine \*Engine\) Run\("),
    ("go", "gin.Context.JSON", "context.go", r"^func \(c \*Context\) JSON\("),
    ("go", "gin.Context.Param", "context.go", r"^func \(c \*Context\) Param\("),
    ("go", "gin.Recovery", "recovery.go", r"^func Recovery\("),
    ("go", "gin.Logger", "logger.go", r"^func Logger\("),
    ("go", "gin.RouterGroup.GET", "routergroup.go", r"^func \(group \*RouterGroup\) GET\("),
    ("go", "gin.SetMode", "mode.go", r"^func SetMode\("),
]
VERSIONS = {"py": "2.32.3", "js": "4.21.2", "rs": "1.0.219", "go": "v1.10.0"}
OTHER = {"rs": "1.0.200", "go": "v1.9.1"}


def sh(cmd, cwd, env=None):
    print("+", " ".join(cmd), file=sys.stderr)
    subprocess.run(cmd, cwd=cwd, env=env, check=True)


def env_of(work):
    return dict(os.environ, CARGO_HOME=f"{work}/cargo", GOMODCACHE=f"{work}/gomod", GOPATH=f"{work}/gopath",
                GOFLAGS="-modcacherw", SCRUTATE_CACHE_DIR=f"{work}/cache", SCRUTATE_SEMANTIC="none",
                UV_CACHE_DIR=f"{work}/uvcache", npm_config_cache=f"{work}/npmcache")


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def setup(args):
    w = os.path.abspath(args.work)
    env = env_of(w)
    py = f"{w}/repos/py"
    write(f"{py}/pyproject.toml", f'[project]\nname = "app"\nversion = "0.1.0"\nrequires-python = ">=3.9"\ndependencies = ["requests=={VERSIONS["py"]}"]\n')
    write(f"{py}/main.py", "import requests\n\nrequests.get('https://example.com')\n")
    sh(["uv", "sync"], py, env)
    js = f"{w}/repos/js"
    write(f"{js}/package.json", json.dumps({"name": "app", "version": "0.1.0", "dependencies": {"express": VERSIONS["js"]}}))
    write(f"{js}/index.js", "const express = require('express');\nexpress().listen(3000);\n")
    sh(["npm", "install", "--no-audit", "--no-fund", "--ignore-scripts"], js, env)
    for name, version in (("rs", VERSIONS["rs"]), ("rs-old", OTHER["rs"]), ("rs2", VERSIONS["rs"])):
        r = f"{w}/repos/{name}"
        write(f"{r}/Cargo.toml", f'[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nserde = "={version}"\n')
        write(f"{r}/src/main.rs", "fn main() {}\n")
        sh(["cargo", "generate-lockfile"], r, env)
        sh(["cargo", "fetch"], r, env)
    for name, version in (("go", VERSIONS["go"]), ("go-old", OTHER["go"])):
        g = f"{w}/repos/{name}"
        write(f"{g}/go.mod", "module example.com/app\n\ngo 1.22\n")
        write(f"{g}/main.go", 'package main\n\nimport "github.com/gin-gonic/gin"\n\nfunc main() { gin.Default().Run() }\n')
        sh(["go", "get", f"github.com/gin-gonic/gin@{version}"], g, env)
        sh(["go", "mod", "download", "all"], g, env)
    sh(["git", "init", "-q"], w)


def package_dir(w, key):
    if key == "py":
        sp = [d for d in os.listdir(f"{w}/repos/py/.venv/lib") if d.startswith("python")][0]
        return f"{w}/repos/py/.venv/lib/{sp}/site-packages"
    if key == "js":
        return f"{w}/repos/js/node_modules/express"
    if key == "rs":
        src = f"{w}/cargo/registry/src"
        return f"{src}/{os.listdir(src)[0]}/serde-{VERSIONS['rs']}"
    return f"{w}/gomod/github.com/gin-gonic/gin@{VERSIONS['go']}"


def ask(binary, root, call, env):
    t = time.time()
    p = subprocess.run([binary, "--root", root, "read", "--deps", "--symbol", call, "--json"], capture_output=True, text=True, env=env)
    return p, time.time() - t


def index_hash(root):
    f = f"{root}/.scrutate/index.db"
    return hashlib.sha256(open(f, "rb").read()).hexdigest() if os.path.exists(f) else None


def run(args):
    w = os.path.abspath(args.work)
    env = env_of(w)
    binary = os.path.abspath(args.scrutate)
    if os.path.isdir(f"{w}/cache"):
        subprocess.run(["rm", "-rf", f"{w}/cache"], check=True)
    for key in ("py", "js", "rs", "go"):
        subprocess.run([binary, "--root", f"{w}/repos/{key}", "index"], env=env, capture_output=True, check=True)
    before = {k: index_hash(f"{w}/repos/{k}") for k in ("py", "js", "rs", "go")}
    passed, strict, wrong_version, firsts = 0, 0, 0, {}
    for key, call, rel, pattern in CALLS:
        pkg = os.path.realpath(package_dir(w, key))
        path = os.path.join(pkg, rel)
        lines = open(path).read().splitlines()
        want = [i + 1 for i, line in enumerate(lines) if re.search(pattern, line)]
        p, took = ask(binary, f"{w}/repos/{key}", call, env)
        firsts.setdefault(key, (call, took))
        got = []
        if p.returncode == 0:
            res = json.loads(p.stdout)["result"]
            for d in res["answer"].get("definitions", []):
                full = d["path"] if os.path.isabs(d["path"]) else os.path.join(f"{w}/repos/{key}", d["path"])
                got.append((os.path.realpath(full), d["start"], d["end"]))
        outside = [g for g in got if not g[0].startswith(pkg + os.sep)]
        wrong_version += bool(outside)
        here = [g for g in got if g[0] == os.path.realpath(path)]
        ok = len(want) == 1 and any(g[1] <= want[0] <= g[2] for g in here) and not outside
        passed += ok
        strict += ok and any(g[1] == want[0] for g in here)
        if not ok:
            print(f"FAIL {call}: want {rel}:{want}, got {[(os.path.relpath(g[0], pkg), g[1], g[2]) for g in got]} "
                  f"(exit {p.returncode}: {p.stderr.strip()[:300]})")
    print(f"\n{passed} of {len(CALLS)} calls land on the locked version's definition (bar: 36), {strict} with the range starting at the labelled line; "
          f"{wrong_version} answers name another version's file (bar: 0)")
    call = firsts["rs"][0]
    median = lambda xs: sorted(xs)[len(xs) // 2]
    firsts_rs, agains, bases = [], [], []
    for _ in range(5):
        subprocess.run(["rm", "-rf", f"{w}/cache/deps/cargo"], check=True)
        firsts_rs.append(ask(binary, f"{w}/repos/rs", call, env)[1])
        agains.append(ask(binary, f"{w}/repos/rs2", call, env)[1])
        t = time.time()
        subprocess.run([binary, "--root", f"{w}/repos/rs2", "read", "--symbol", "main", "--json"], capture_output=True, env=env)
        bases.append(time.time() - t)
    copies = []
    src = next(iter(os.scandir(f"{w}/cache/deps/cargo"))).path + "/src"
    for _ in range(5):
        t = time.time()
        subprocess.run([binary, "--root", src, "read", "--symbol", call.rsplit("::", 1)[-1], "--json"], capture_output=True, env=env)
        copies.append(time.time() - t)
    first, again, base = median(firsts_rs), median(agains), median(bases)
    copy = median(copies)
    print(f"the cached copy read as a repository of its own: {copy:.3f} s")
    print(f"serde, median of 5: first call {first:.3f} s (copy and index), from a second repository {again:.3f} s "
          f"({again / copy:.2f} times the read of the copy; bar: at most 1.5), "
          f"a read of the repository's own code {base:.3f} s")
    for k, h in before.items():
        print(f"{k}: repository index {'unchanged' if index_hash(f'{w}/repos/{k}') == h else 'CHANGED'}")
    for k in ("py", "js", "rs", "go"):
        print(f"{k} package: {firsts[k][0]} first call {firsts[k][1]:.3f} s")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("setup")
    s.add_argument("--work", required=True)
    r = sub.add_parser("run")
    r.add_argument("--work", required=True)
    r.add_argument("--scrutate", required=True)
    args = ap.parse_args()
    setup(args) if args.cmd == "setup" else run(args)


if __name__ == "__main__":
    main()
