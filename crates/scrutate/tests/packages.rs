//! `symbol --deps`, `read --symbol --deps` and the tools' `scope: "deps"`: a dependency's source
//! at the version the lockfile pins, from where it is installed, never another version.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/canon.rs"]
mod canon;
use canon::canon;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Tree {
    /// the repository
    root: PathBuf,
    /// the stores and the cache, beside it
    home: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let top = std::env::temp_dir().join(format!("scrutate_packages_{name}_{}_{n}", std::process::id()));
        let _ = fs::remove_dir_all(&top);
        fs::create_dir_all(top.join("repo/.git")).unwrap();
        fs::create_dir_all(top.join("home")).unwrap();
        let top = canon(top);
        Tree { root: top.join("repo"), home: top.join("home") }
    }

    fn write(&self, rel: &str, text: &str) {
        write(&self.root.join(rel), text);
    }

    fn store(&self, rel: &str, text: &str) {
        write(&self.home.join(rel), text);
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        self.run_in(&self.root, args)
    }

    fn run_in(&self, root: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(BIN)
            .args(["--root", root.to_str().unwrap()])
            .args(args)
            .current_dir(root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_CACHE_DIR", self.home.join("cache"))
            .env("CARGO_HOME", self.home.join("cargo"))
            .env("GOMODCACHE", self.home.join("gomod"))
            .env_remove("SCRUTATE_REDACT")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn batch(&self, lines: &[serde_json::Value]) -> Vec<serde_json::Value> {
        let f = self.home.join("batch.jsonl");
        fs::write(&f, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let (_, out, err) = self.run(&["batch", f.to_str().unwrap()]);
        let v: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(v.len(), lines.len(), "{out}{err}");
        v
    }

    /// A crate in the cargo registry and the Cargo.lock that pins it.
    fn cargo(&self, name: &str, version: &str, lib: &str) {
        self.store(&format!("cargo/registry/src/index.crates.io-1949cf8c6b5b557f/{name}-{version}/src/lib.rs"), lib);
        self.store(
            &format!("cargo/registry/src/index.crates.io-1949cf8c6b5b557f/{name}-{version}/Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        );
    }

    fn cargo_lock(&self, entries: &[(&str, &str)]) {
        let mut t = String::from("version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n");
        for (name, version) in entries {
            let (name, version) = (toml_text(name), toml_text(version));
            t.push_str(&format!(
                "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"00\"\n"
            ));
        }
        self.write("Cargo.lock", &t);
        self.write("Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n");
        self.write("src/main.rs", "fn main() {}\n");
    }

    fn npm_lock(&self, packages: &str) {
        self.write("package-lock.json", &format!("{{\"name\": \"app\", \"lockfileVersion\": 3, \"packages\": {{\"\": {{\"name\": \"app\"}}, {packages}}}}}"));
        self.write("index.js", "const express = require('express');\n");
    }

    fn npm(&self, name: &str, version: &str, file: &str, text: &str) {
        self.write(&format!("node_modules/{name}/package.json"), &format!("{{\"name\": \"{name}\", \"version\": \"{version}\"}}"));
        self.write(&format!("node_modules/{name}/{file}"), text);
    }

    fn uv_lock(&self, entries: &[(&str, &str)]) {
        let mut t =
            String::from("version = 1\nrequires-python = \">=3.9\"\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\nsource = { virtual = \".\" }\n");
        for (name, version) in entries {
            let (name, version) = (toml_text(name), toml_text(version));
            t.push_str(&format!("\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\n"));
        }
        self.write("uv.lock", &t);
        self.write("main.py", "import requests\n");
    }

    /// A distribution in `.venv`: its files and the RECORD that lists them.
    fn wheel(&self, dist: &str, version: &str, files: &[(&str, &str)]) {
        let sp = ".venv/lib/python3.12/site-packages";
        let info = format!("{}-{version}.dist-info", dist.replace('-', "_"));
        let mut record = String::new();
        for (f, text) in files {
            self.write(&format!("{sp}/{f}"), text);
            record.push_str(&format!("{f},sha256=x,1\n"));
        }
        record.push_str(&format!("{info}/METADATA,,\n{info}/RECORD,,\n../../../bin/{dist},,\n"));
        self.write(&format!("{sp}/{info}/METADATA"), &format!("Metadata-Version: 2.1\nName: {dist}\nVersion: {version}\n"));
        self.write(&format!("{sp}/{info}/RECORD"), &record);
    }

    fn go_mod(&self, body: &str) {
        self.write("go.mod", &format!("module example.com/app\n\ngo 1.22\n\n{body}\n"));
        self.write("main.go", "package main\n\nfunc main() {}\n");
    }
}

/// `s` inside a TOML basic string.
fn toml_text(s: &str) -> String {
    s.replace('\\', "\\\\")
}

fn write(p: &Path, text: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

#[test]
fn a_crate_is_read_at_the_version_cargo_lock_pins_never_another() {
    let t = Tree::new("cargo");
    t.cargo_lock(&[("serde", "1.0.200")]);
    t.cargo("serde", "1.0.200", "/// the one at 1.0.200\npub trait Deserialize {\n    fn new_api();\n}\n");
    t.cargo("serde", "0.9.0", "/// the one at 0.9.0\npub trait Deserialize {\n    fn old_api();\n}\n");
    let (code, out, err) = t.run(&["symbol", "--deps", "serde::Deserialize"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("serde 1.0.200 (Cargo.lock)"), "{out}");
    assert!(out.contains("serde-1.0.200/src/lib.rs:2-4"), "{out}");
    assert!(!out.contains("0.9.0"), "{out}");
    let (code, out, err) = t.run(&["read", "--deps", "--symbol", "serde::Deserialize"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("fn new_api();") && !out.contains("old_api"), "{out}");
    // the repository's own index never holds it
    let (code, out, _) = t.run(&["symbol", "--exact", "Deserialize"]);
    assert_eq!(code, 1, "{out}");
}

#[test]
fn a_crate_the_registry_lacks_at_the_locked_version_is_an_error_that_says_how_to_fetch_it() {
    let t = Tree::new("cargo_missing");
    t.cargo_lock(&[("serde", "1.0.200")]);
    t.cargo("serde", "0.9.0", "pub trait Deserialize {}\n");
    let (code, out, err) = t.run(&["symbol", "--deps", "serde::Deserialize"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("serde 1.0.200") && err.contains("cargo fetch"), "{err}");
    assert!(!out.contains("Deserialize"), "{out}");
}

#[test]
fn an_npm_package_is_read_from_node_modules_at_the_locked_version() {
    let t = Tree::new("npm");
    t.npm_lock(r#""node_modules/express": {"version": "4.21.2", "resolved": "https://registry.npmjs.org/express/-/express-4.21.2.tgz"}"#);
    t.npm("express", "4.21.2", "lib/router/index.js", "// routes\nfunction Router(options) {\n  return options;\n}\nmodule.exports = Router;\n");
    let (code, out, err) = t.run(&["read", "--deps", "--symbol", "express.Router"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("express 4.21.2 (package-lock.json)"), "{out}");
    assert!(out.contains("node_modules/express/lib/router/index.js"), "{out}");
    assert!(out.contains("return options;"), "{out}");
}

#[test]
fn an_installed_package_at_another_version_than_the_lock_is_refused_naming_both() {
    let t = Tree::new("npm_mismatch");
    t.npm_lock(r#""node_modules/express": {"version": "4.21.2"}"#);
    t.npm("express", "4.17.0", "index.js", "function Router() {}\n");
    let (code, out, err) = t.run(&["symbol", "--deps", "express.Router"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("4.21.2") && err.contains("4.17.0") && err.contains("npm ci"), "{err}");
    assert!(!out.contains("index.js"), "{out}");
    let t = Tree::new("uv_mismatch");
    t.uv_lock(&[("requests", "2.32.3")]);
    t.wheel("requests", "2.31.0", &[("requests/api.py", "def get(url):\n    return url\n")]);
    let (code, _, err) = t.run(&["symbol", "--deps", "requests.get"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("2.32.3") && err.contains("2.31.0") && err.contains("uv sync"), "{err}");
}

#[test]
fn a_python_package_is_read_from_the_venv_by_its_import_name() {
    let t = Tree::new("uv");
    t.uv_lock(&[("requests", "2.32.3"), ("PyYAML", "6.0.2")]);
    t.wheel("requests", "2.32.3", &[("requests/__init__.py", ""), ("requests/api.py", "def get(url, params=None):\n    return request(\"get\", url)\n")]);
    // the import name is not the distribution's
    t.wheel("PyYAML", "6.0.2", &[("yaml/__init__.py", "def safe_load(stream):\n    return stream\n")]);
    let (code, out, err) = t.run(&["symbol", "--deps", "requests.get"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("requests 2.32.3 (uv.lock)"), "{out}");
    assert!(out.contains(".venv/lib/python3.12/site-packages/requests/api.py:1-2"), "{out}");
    let (code, out, err) = t.run(&["symbol", "--deps", "yaml.safe_load"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("PyYAML 6.0.2 (uv.lock)") && out.contains("site-packages/yaml/__init__.py:1-2"), "{out}");
    // poetry.lock pins the same way
    let t = Tree::new("poetry");
    t.write(
        "poetry.lock",
        "[[package]]\nname = \"requests\"\nversion = \"2.32.3\"\ndescription = \"\"\noptional = false\n\n[metadata]\nlock-version = \"2.0\"\n",
    );
    t.wheel("requests", "2.32.3", &[("requests/api.py", "def get(url):\n    return url\n")]);
    let (code, out, err) = t.run(&["symbol", "--deps", "requests.get"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("requests 2.32.3 (poetry.lock)"), "{out}");
}

#[test]
fn a_go_module_is_read_from_the_module_cache_at_the_version_go_mod_requires() {
    let t = Tree::new("go");
    t.go_mod("require (\n\tgithub.com/gin-gonic/gin v1.10.0\n\tgithub.com/BurntSushi/toml v1.4.0 // indirect\n)");
    t.store("gomod/github.com/gin-gonic/gin@v1.10.0/gin.go", "package gin\n\n// Default returns an Engine.\nfunc Default() *Engine {\n\treturn New()\n}\n");
    t.store("gomod/github.com/gin-gonic/gin@v1.9.1/gin.go", "package gin\n\nfunc Default() *Engine {\n\treturn nil\n}\n");
    // the module cache writes an upper-case letter as `!` and its lower case
    t.store("gomod/github.com/!burnt!sushi/toml@v1.4.0/decode.go", "package toml\n\nfunc Unmarshal(data []byte, v any) error {\n\treturn nil\n}\n");
    let (code, out, err) = t.run(&["read", "--deps", "--symbol", "gin.Default"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("github.com/gin-gonic/gin v1.10.0 (go.mod)"), "{out}");
    assert!(out.contains("gin@v1.10.0/gin.go") && out.contains("return New()") && !out.contains("v1.9.1"), "{out}");
    let (code, out, err) = t.run(&["symbol", "--deps", "toml.Unmarshal"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("!burnt!sushi/toml@v1.4.0/decode.go:3-5"), "{out}");
}

#[test]
fn the_tools_take_scope_deps_and_answer_the_same() {
    let t = Tree::new("tools");
    t.cargo_lock(&[("serde", "1.0.200")]);
    t.cargo("serde", "1.0.200", "pub trait Deserialize {\n    fn new_api();\n}\n");
    let v = t.batch(&[
        serde_json::json!({"op": "find_symbol", "name": "serde::Deserialize", "scope": "deps"}),
        serde_json::json!({"op": "read_code", "symbol": "serde::Deserialize", "scope": "deps"}),
        serde_json::json!({"op": "find_symbol", "name": "Deserialize", "scope": "repo"}),
        serde_json::json!({"op": "find_symbol", "name": "serde::Deserialize", "scope": "everywhere"}),
        serde_json::json!({"op": "read_code", "path": "src/main.rs", "scope": "deps"}),
    ]);
    let r = &v[0]["result"];
    assert_eq!(r["package"]["name"], "serde", "{}", v[0]);
    assert_eq!(r["package"]["version"], "1.0.200", "{}", v[0]);
    assert!(r["answer"][0]["path"].as_str().unwrap().ends_with("serde-1.0.200/src/lib.rs"), "{}", v[0]);
    assert!(v[1]["result"]["answer"]["definitions"][0]["text"].as_str().unwrap().contains("fn new_api();"), "{}", v[1]);
    assert_eq!(v[2]["result"], serde_json::json!([]), "{}", v[2]);
    assert!(v[3]["error"].as_str().unwrap().contains("scope"), "{}", v[3]);
    assert!(v[4]["error"].as_str().unwrap().contains("symbol"), "{}", v[4]);
}

#[test]
fn a_second_repository_that_pins_the_same_version_reuses_the_cached_copy() {
    let t = Tree::new("reuse");
    t.cargo_lock(&[("serde", "1.0.200")]);
    t.cargo("serde", "1.0.200", "pub trait Deserialize {}\n");
    assert_eq!(t.run(&["symbol", "--deps", "serde::Deserialize"]).0, 0);
    let entries = |t: &Tree| fs::read_dir(t.home.join("cache/deps/cargo")).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect::<Vec<_>>();
    assert_eq!(entries(&t), ["serde@1.0.200"]);
    let done = t.home.join("cache/deps/cargo/serde@1.0.200/done");
    let first = fs::metadata(&done).unwrap().modified().unwrap();
    let other = t.home.parent().unwrap().join("other");
    fs::create_dir_all(other.join(".git")).unwrap();
    fs::copy(t.root.join("Cargo.lock"), other.join("Cargo.lock")).unwrap();
    let (code, out, err) = t.run_in(&other, &["symbol", "--deps", "serde::Deserialize"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(entries(&t), ["serde@1.0.200"]);
    assert_eq!(fs::metadata(&done).unwrap().modified().unwrap(), first, "the copy was made again");
    // the copy is indexed once: a file added to it later is never indexed
    let indexed = t.home.join("cache/deps/cargo/serde@1.0.200/indexed");
    assert!(indexed.is_file());
    write(&t.home.join("cache/deps/cargo/serde@1.0.200/src/src/late.rs"), "pub fn late() {}\n");
    let (code, out, _) = t.run(&["symbol", "--deps", "--exact", "serde::late"]);
    assert_eq!(code, 1, "{out}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for d in ["cache/deps", "cache/deps/cargo", "cache/deps/cargo/serde@1.0.200"] {
            assert_eq!(fs::metadata(t.home.join(d)).unwrap().permissions().mode() & 0o777, 0o700, "{d}");
        }
    }
}

#[test]
fn a_name_or_version_that_leaves_its_store_is_refused_by_name() {
    let bad = ["../../x", "..", "a/../../b", "/etc", "a\\..\\b"];
    for b in bad {
        let t = Tree::new("cargo_bad");
        t.store("cargo/registry/src/index.crates.io-1949cf8c6b5b557f/x/src/lib.rs", "pub fn leak() {}\n");
        t.cargo_lock(&[(b, "1.0.0"), ("ok", b)]);
        for name in [format!("{b}::leak"), "ok::leak".to_string()] {
            let (code, out, err) = t.run(&["symbol", "--deps", &name]);
            assert_ne!(code, 0, "{name}: {out}");
            assert!(!out.contains("leak"), "{name}: {out}");
            if name.starts_with("ok") {
                assert!(err.contains("refused"), "{name}: {err}");
            }
        }
        let t = Tree::new("npm_bad");
        t.write("secret/index.js", "function leak() {}\n");
        t.npm_lock(&format!(
            r#""node_modules/{}": {{"version": "1.0.0"}}, "node_modules/ok": {{"version": "{}"}}"#,
            b.replace('\\', "\\\\"),
            b.replace('\\', "\\\\")
        ));
        t.npm("ok", b, "index.js", "function leak() {}\n");
        let (code, out, err) = t.run(&["symbol", "--deps", "ok.leak"]);
        assert_ne!(code, 0, "{b}: {out}");
        assert!(err.contains("refused"), "{b}: {err}");
        let t = Tree::new("uv_bad");
        t.uv_lock(&[(b, "1.0.0"), ("ok", b)]);
        t.wheel("ok", "1.0.0", &[("ok/__init__.py", "def leak():\n    pass\n")]);
        let (code, out, err) = t.run(&["symbol", "--deps", "ok.leak"]);
        assert_ne!(code, 0, "{b}: {out}");
        assert!(err.contains("refused"), "{b}: {err}");
        let t = Tree::new("go_bad");
        t.go_mod(&format!("require (\n\texample.com/{b} v1.0.0\n\texample.com/ok {b}\n)"));
        t.store("gomod/example.com/ok@v1.0.0/ok.go", "package ok\n\nfunc Leak() {}\n");
        let (code, out, err) = t.run(&["symbol", "--deps", "ok.Leak"]);
        assert_ne!(code, 0, "{b}: {out}");
        assert!(err.contains("refused"), "{b}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn a_link_out_of_the_package_is_never_followed() {
    let t = Tree::new("links");
    t.npm_lock(r#""node_modules/evil": {"version": "1.0.0"}"#);
    t.npm("evil", "1.0.0", "index.js", "function fine() {}\n");
    t.store("dot-ssh/id_rsa.js", "function stolenKey() { return 'outside-the-package'; }\n");
    std::os::unix::fs::symlink(t.home.join("dot-ssh/id_rsa.js"), t.root.join("node_modules/evil/key.js")).unwrap();
    std::os::unix::fs::symlink(t.home.join("dot-ssh"), t.root.join("node_modules/evil/ssh")).unwrap();
    // a link that stays inside the package is read as its target
    std::os::unix::fs::symlink("index.js", t.root.join("node_modules/evil/alias.js")).unwrap();
    let (code, out, err) = t.run(&["symbol", "--deps", "evil.fine"]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = t.run(&["symbol", "--deps", "evil.stolenKey"]);
    assert_ne!(code, 0, "{out}{err}");
    assert!(!out.contains("outside-the-package") && !out.contains("stolenKey"), "{out}");
    let copy = t.home.join("cache/deps/npm/evil@1.0.0/src");
    assert!(copy.join("index.js").exists() && copy.join("alias.js").is_file());
    assert!(!copy.join("key.js").exists() && !copy.join("ssh").exists());
    // a package directory that is itself a link out of node_modules is refused
    let t = Tree::new("links_dir");
    t.npm_lock(r#""node_modules/evil": {"version": "1.0.0"}"#);
    t.store("pkg/evil/package.json", "{\"name\": \"evil\", \"version\": \"1.0.0\"}");
    t.store("pkg/evil/index.js", "function fine() {}\n");
    fs::create_dir_all(t.root.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(t.home.join("pkg/evil"), t.root.join("node_modules/evil")).unwrap();
    let (code, out, err) = t.run(&["symbol", "--deps", "evil.fine"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("refused"), "{err}");
}

#[test]
fn a_path_dependency_inside_the_repository_answers_from_its_index_and_one_outside_is_refused() {
    let t = Tree::new("local_npm");
    t.npm_lock(r#""node_modules/shared": {"resolved": "packages/shared", "link": true}, "packages/shared": {"version": "1.0.0"}"#);
    t.write("packages/shared/index.js", "function shared() {\n  return 1;\n}\n");
    let (code, out, err) = t.run(&["symbol", "--deps", "shared.shared"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("packages/shared/index.js:1-3"), "{out}");
    let t = Tree::new("far_go");
    t.go_mod("require example.com/far v1.0.0\n\nreplace example.com/far => ../far");
    let (code, _, err) = t.run(&["symbol", "--deps", "far.Thing"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("example.com/far") && err.contains("outside the repository"), "{err}");
    let t = Tree::new("near_go");
    t.go_mod("require example.com/near v1.0.0\n\nreplace example.com/near => ./near");
    t.write("near/near.go", "package near\n\nfunc Thing() {}\n");
    let (code, out, err) = t.run(&["symbol", "--deps", "near.Thing"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("near/near.go:3"), "{out}");
}

#[test]
fn a_name_outside_every_lockfile_says_which_lockfiles_were_read() {
    let t = Tree::new("unknown");
    t.cargo_lock(&[("serde", "1.0.200")]);
    let (code, _, err) = t.run(&["symbol", "--deps", "tokio::spawn"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("tokio") && err.contains("Cargo.lock"), "{err}");
    let (code, _, err) = t.run(&["symbol", "--deps", "serde"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("serde::NAME") || err.contains("PACKAGE.NAME"), "{err}");
}
