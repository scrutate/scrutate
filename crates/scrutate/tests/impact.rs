//! `scrutate impact` and the MCP `impact` tool on git repositories made for each test: a call
//! chain three deep with two test files, then a deletion, a rename with an edit, a staged-only
//! change, a base revision, a merge base, an untracked file and a directory outside git, and the
//! callers of a deleted definition.

#[path = "support/canon.rs"]
mod canon;
#[path = "support/tag.rs"]
mod tag;
use canon::canon;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

/// git's own configuration is the only one read: no global or system file.
const GIT_ENV: [(&str, &str); 2] = [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("impact_{name}_{}", std::process::id()));
        tag::claim(&root);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Tree { root: canon(root) }
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        let mut c = Command::new("git");
        c.args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]).args(args).current_dir(&self.root);
        let out = c.envs(GIT_ENV).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// (exit code, standard output, standard error)
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(BIN)
            .args(["--root", self.root.to_str().unwrap()])
            .args(args)
            .current_dir(&self.root)
            .envs(GIT_ENV)
            .env("SCRUTATE_SEMANTIC", "none")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn impact(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(&[&["impact"], args].concat());
        assert_eq!(code, 0, "impact {args:?}: {out}{err}");
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        let v: Value = serde_json::from_str(&self.impact(&[args, &["--json"]].concat())).unwrap();
        assert_eq!(v["schema"], 2);
        v["result"].clone()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// A repository whose `top` calls `middle` calls `deepest`; one test file calls `top`, the other
/// tests something else.
fn chain(name: &str) -> Tree {
    let t = Tree::new(name);
    t.git(&["init", "-q"]);
    t.write("lib/__init__.py", "");
    t.write("lib/core.py", "def deepest(x):\n    return x + 1\n\n\ndef spare(x):\n    return x\n");
    t.write("lib/mid.py", "from lib.core import deepest\n\n\ndef middle(x):\n    return deepest(x) * 2\n");
    t.write("lib/top.py", "from lib.mid import middle\n\n\ndef top(x):\n    return middle(x)\n");
    t.write("tests/test_top.py", "from lib.top import top\n\n\ndef test_top():\n    assert top(1) == 4\n");
    t.write("tests/test_other.py", "from lib.core import spare\n\n\ndef test_spare():\n    assert spare(1) == 1\n");
    t.commit("first");
    t
}

/// A function body long enough for `dupes` (30 body tokens), its words from `seed`.
fn long_fn(name: &str, seed: &str) -> String {
    format!(
        "def {name}(rows):\n    total = 0\n    for row in rows:\n        if row.amount > {seed}_limit and row.status == \"open\":\n            \
         total += compute_{seed}_weight(row.amount, row.discount, row.region)\n        elif row.status == \"closed\":\n            \
         total -= penalty_for_{seed}(row.region, row.amount, row.owner)\n        else:\n            total = adjust_{seed}(total, row.created, row.updated)\n    \
         return normalize_{seed}_total(total, rows, scale_factor, offset_value, rounding_mode)\n"
    )
}

fn ids(v: &Value, key: &str) -> Vec<String> {
    v[key].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap_or_else(|| x["path"].as_str().unwrap()).to_string()).collect()
}

#[test]
fn an_edit_in_the_deepest_function_names_it_its_callers_by_distance_and_the_one_test_that_reaches_it() {
    let t = chain("chain");
    t.write("lib/core.py", "def deepest(x):\n    return x + 2\n\n\ndef spare(x):\n    return x\n");
    let text = t.impact(&[]);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("working tree against HEAD ("), "{text}");
    assert_eq!(
        lines[1..].to_vec(),
        [
            "changed: 1",
            "lib/core.py:1-2  [function] deepest  (edited)",
            "callers: 3 (depth 3)",
            " 1  lib/mid.py:4-5  [function] middle",
            " 2  lib/top.py:4-5  [function] top",
            " 3  tests/test_top.py:4-5  [function] test_top",
            "tests: 1",
            "tests/test_top.py  (distance 3: tests/test_top.py:4-5  [function] test_top)",
        ],
        "{text}"
    );
    // the same on every run, and the JSON holds the same items
    assert_eq!(t.impact(&[]), text);
    let v = t.json(&[]);
    assert_eq!(ids(&v, "changed"), ["lib/core.py:1-2"]);
    assert_eq!(v["changed"][0]["status"], "edited");
    assert_eq!(ids(&v, "callers"), ["lib/mid.py:4-5", "lib/top.py:4-5", "tests/test_top.py:4-5"]);
    assert_eq!(v["callers"].as_array().unwrap().iter().map(|c| c["distance"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(v["callers"][1]["via"], "lib/mid.py:4-5");
    assert_eq!(v["tests"].as_array().unwrap().iter().map(|x| x["path"].as_str().unwrap()).collect::<Vec<_>>(), ["tests/test_top.py"]);
    // each step is what `callers` answers
    let (_, callers, _) = t.run(&["callers", "lib/core.py:1-2"]);
    assert_eq!(callers.lines().next(), Some("lib/mid.py:4-5  [function] middle"));
    // a shorter walk stops sooner and finds no test
    let short = t.impact(&["--depth", "2"]);
    assert!(short.contains("callers: 2 (depth 2)") && short.contains("tests: 0"), "{short}");
    // paged by whole lines, with the line that says how to go on
    let (_, paged, _) = t.run(&["--max-bytes", "150", "impact"]);
    assert!(paged.len() <= 150 && paged.contains("more lines not shown: call again with offset="), "{paged}");
    let next: usize = paged.rsplit("offset=").next().unwrap().split(',').next().unwrap().parse().unwrap();
    let (_, rest, _) = t.run(&["--offset", &next.to_string(), "impact"]);
    assert!(rest.contains(&lines[next + 1].to_string()), "{rest}");

    // the MCP tool gives the same text, and its structured result the same JSON
    let mut child = Command::new(BIN)
        .args(["--root", t.root.to_str().unwrap(), "serve", "--structured"])
        .envs(GIT_ENV)
        .env("SCRUTATE_SEMANTIC", "none")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for m in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"impact","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"impact","arguments":{"base":"--output=x"}}}"#,
    ] {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let replies: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let reply = |id: i64| replies.iter().find(|v| v["id"] == id).unwrap()["result"].clone();
    let tool = reply(2)["tools"].as_array().unwrap().iter().find(|x| x["name"] == "impact").cloned().unwrap();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(reply(3)["content"][0]["text"].as_str().unwrap(), text.trim_end());
    assert_eq!(reply(3)["structuredContent"]["result"], v);
    assert_eq!(reply(4)["isError"], true);
    assert!(reply(4)["content"][0]["text"].as_str().unwrap().contains("not a revision: --output=x"));
    assert!(!t.root.join("x").exists());
}

#[test]
fn a_deleted_definition_is_named_from_the_base_and_a_new_one_as_added() {
    let t = chain("deleted");
    t.write("lib/core.py", "def deepest(x):\n    return x + 1\n\n\ndef fresh(x):\n    return deepest(x)\n");
    let v = t.json(&[]);
    let changed: Vec<(String, String)> =
        v["changed"].as_array().unwrap().iter().map(|c| (c["id"].as_str().unwrap().to_string(), c["status"].as_str().unwrap().to_string())).collect();
    // `spare` was at lines 5-6 of the base: it is named from there, beside the new definition
    assert_eq!(changed, [("lib/core.py:5-6".to_string(), "added".to_string()), ("lib/core.py:5-6".to_string(), "deleted".to_string())], "{v}");
    let text = t.impact(&[]);
    assert!(text.contains("lib/core.py:5-6  [function] spare  (deleted, lines of the base)"), "{text}");
    assert!(text.contains("lib/core.py:5-6  [function] fresh  (added)"), "{text}");
    // nothing calls the new definition; the test of the deleted one still calls it, so it is run
    assert!(text.contains(" 1  tests/test_other.py:4-5  [function] test_spare  (calls a deleted definition)"), "{text}");
    assert!(text.contains("tests: 1\ntests/test_other.py  (distance 1"), "{text}");
}

#[test]
fn the_callers_of_a_deleted_definition_are_followed_to_their_callers_and_tests() {
    let t = chain("deletedcalled");
    t.write("lib/core.py", "def spare(x):\n    return x\n");
    let text = t.impact(&[]);
    let lines: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(
        lines,
        [
            "changed: 2",
            "lib/core.py  [file]  (edited)",
            "lib/core.py:1-2  [function] deepest  (deleted, lines of the base)",
            "callers: 3 (depth 3)",
            " 1  lib/mid.py:4-5  [function] middle  (calls a deleted definition)",
            " 2  lib/top.py:4-5  [function] top",
            " 3  tests/test_top.py:4-5  [function] test_top",
            "tests: 1",
            "tests/test_top.py  (distance 3: tests/test_top.py:4-5  [function] test_top)",
        ],
        "{text}"
    );
    let v = t.json(&[]);
    let callers = v["callers"].as_array().unwrap();
    assert_eq!((callers[0]["calls_deleted"].clone(), callers[0]["via"].clone()), (Value::Bool(true), Value::from("lib/core.py:1-2")));
    assert!(callers[1..].iter().all(|c| c["calls_deleted"] == false), "{v}");
}

/// A file moved to a path the index's rules leave out (a directory `.ignore` names, or one
/// `.gitignore` names, the file added with `-f`) is gone as far as the index goes: its
/// definitions come out deleted, with the callers and tests a `git rm` of the file gives, in the
/// working tree and staged.
#[test]
fn a_file_renamed_out_of_the_index_reads_as_deleted() {
    let summary = |v: &Value| (v["changed"].clone(), v["callers"].clone(), v["tests"].clone());
    let removed = chain("renamed_out_rm");
    removed.git(&["rm", "-q", "lib/core.py"]);
    assert_eq!(removed.run(&["index"]).0, 0);
    for form in [&[][..], &["--staged"][..]] {
        let want = summary(&removed.json(form));
        let changed: Vec<&str> = want.0.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert!(changed.contains(&"lib/core.py:1-2") && want.1.as_array().unwrap().iter().any(|c| c["id"] == "lib/mid.py:4-5"), "{form:?}: {want:?}");
        for rules in [".ignore", ".gitignore"] {
            let to = "old/core.py";
            let t = chain(&format!("renamed_out{}_{}", rules.replace('.', "_"), form.len()));
            t.write(rules, "old/\n");
            t.commit("leave old/ out");
            fs::create_dir_all(t.root.join("old")).unwrap();
            fs::rename(t.root.join("lib/core.py"), t.root.join(to)).unwrap();
            t.git(&["add", "-A"]);
            t.git(&["add", "-f", to]);
            let how = rules;
            assert_eq!(t.run(&["index"]).0, 0);
            let v = t.json(form);
            assert_eq!(summary(&v), want, "{how} {form:?}: {v}");
            assert!(v["notes"].as_array().unwrap().iter().any(|n| n["path"] == to), "{how} {form:?}: {v}");
        }
    }
    // a tracked file the rules leave out, edited where it is: not moved, so nothing reads as deleted
    let t = chain("renamed_out_in_place");
    t.write(".gitignore", "lib/core.py\n");
    t.commit("ignore core, still tracked");
    t.write("lib/core.py", "def deepest(x):\n    return x + 2\n\n\ndef spare(x):\n    return x\n");
    assert_eq!(t.run(&["index"]).0, 0);
    let v = t.json(&[]);
    assert!(v["changed"].as_array().unwrap().iter().all(|c| c["status"] != "deleted"), "{v}");
    assert!(v["notes"].as_array().unwrap().iter().any(|n| n["path"] == "lib/core.py"), "{v}");
}

/// A module renamed within the index: a definition whose importer still names the old path no
/// longer reaches it, and comes out as `git rm` gives it, the rename note right above it; one
/// whose importers still resolve (updated in the same change, a Rust `mod` path) is not listed.
#[test]
fn a_renamed_module_lists_the_definitions_its_importers_lost() {
    let module = |name: &str| {
        let t = Tree::new(name);
        t.git(&["init", "-q"]);
        t.write("lib/__init__.py", "");
        t.write("lib/util.py", "def helper():\n    return 1\n");
        t.write("lib/app.py", "from lib.util import helper\n\n\ndef run():\n    return helper()\n");
        t.write("tests/test_app.py", "from lib.app import run\n\n\ndef test_run():\n    assert run() == 1\n");
        t.commit("first");
        t.git(&["mv", "lib/util.py", "lib/helpers.py"]);
        t
    };
    let deleted = |v: &Value| -> Vec<String> {
        v["changed"].as_array().unwrap().iter().filter(|c| c["status"] == "deleted").map(|c| c["id"].as_str().unwrap().to_string()).collect()
    };
    // a pure rename
    let t = module("renamed_pure");
    let v = t.json(&["--staged"]);
    assert_eq!(deleted(&v), ["lib/util.py:1-2"], "{v}");
    assert_eq!(ids(&v, "callers"), ["lib/app.py:4-5", "tests/test_app.py:4-5"], "{v}");
    assert_eq!(v["callers"][0]["calls_deleted"], true, "{v}");
    assert_eq!(ids(&v, "tests"), ["tests/test_app.py"], "{v}");
    let renamed = |n: &Value| n["path"] == "lib/helpers.py" && n["note"] == "renamed from lib/util.py" && n["renamed_from"] == "lib/util.py";
    assert!(v["notes"].as_array().unwrap().iter().any(renamed), "{v}");
    let text = t.impact(&["--staged"]);
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| *l == "lib/util.py:1-2  [function] helper  (deleted, lines of the base)").unwrap_or_else(|| panic!("{text}"));
    assert_eq!(lines[at - 1], "lib/helpers.py  renamed from lib/util.py", "{text}");
    // the importer updated in the same change
    let t = module("renamed_importer_too");
    t.write("lib/app.py", "from lib.helpers import helper\n\n\ndef run():\n    return helper()\n");
    t.git(&["add", "-A"]);
    let v = t.json(&["--staged"]);
    assert!(deleted(&v).is_empty(), "{v}");
    // renamed and edited: git's similarity under 100 percent
    let t = module("renamed_edited");
    t.write("lib/helpers.py", "def helper():\n    return 1\n\n\ndef other():\n    return 2\n");
    t.git(&["add", "-A"]);
    let v = t.json(&["--staged"]);
    assert_eq!(deleted(&v), ["lib/util.py:1-2"], "{v}");
    assert!(ids(&v, "callers").contains(&"lib/app.py:4-5".to_string()), "{v}");
    // to a name the index still reads (lib/util.py.bak, the Auditor's first case): the same
    let t = module("renamed_bak");
    t.git(&["mv", "lib/helpers.py", "lib/util.py.bak"]);
    let v = t.json(&["--staged"]);
    assert_eq!(deleted(&v), ["lib/util.py:1-2"], "{v}");
    // a package moved whole: 400 files of 10 definitions, the first 1000 checked, the rest counted
    let t = Tree::new("renamed_package");
    t.git(&["init", "-q"]);
    t.write("old/__init__.py", "");
    for f in 1..=400 {
        let defs: String = (1..=10).map(|i| format!("def g{f}_{i}():\n    return {i}\n\n\n")).collect();
        t.write(&format!("old/m{f}.py"), &defs);
        t.write(&format!("app/u{f}.py"), &format!("from old.m{f} import g{f}_1\n\n\ndef use{f}():\n    return g{f}_1()\n"));
    }
    t.commit("first");
    t.git(&["mv", "old", "new"]);
    let v = t.json(&["--staged"]);
    assert_eq!(deleted(&v).len(), 100, "{v}");
    let past: Vec<&Value> = v["notes"].as_array().unwrap().iter().filter(|n| n.get("unchecked").is_some()).collect();
    assert_eq!(past.len(), 1, "{v}");
    assert_eq!(past[0]["unchecked"], 3000);
    let line = "3000 of the renamed files' 4000 definitions, past the first 1000, were not checked for callers that still name the old path";
    assert_eq!(past[0]["note"], line);
    assert!(t.impact(&["--staged"]).lines().any(|l| l.ends_with(line)));
    // a Rust module moved to its own directory: `mod util;` still finds it
    let t = Tree::new("renamed_rust_mod");
    t.git(&["init", "-q"]);
    t.write("Cargo.toml", "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    t.write("src/lib.rs", "mod util;\n\npub fn run() -> i32 {\n    util::helper()\n}\n");
    t.write("src/util.rs", "pub fn helper() -> i32 {\n    1\n}\n");
    t.commit("first");
    fs::create_dir_all(t.root.join("src/util")).unwrap();
    t.git(&["mv", "src/util.rs", "src/util/mod.rs"]);
    let v = t.json(&["--staged"]);
    assert!(deleted(&v).is_empty(), "{v}");
}

/// impact's time grows with the change, not with its square: four times the edited definitions
/// (each a caller of the next) cost at most twice four times as long.
#[test]
fn impact_takes_time_in_proportion_to_the_change() {
    let took = |files: usize| -> std::time::Duration {
        let t = Tree::new(&format!("linear_{files}"));
        t.git(&["init", "-q"]);
        let module = |f: usize, pad: &str| -> String { (1..=200).map(|i| format!("def f{f}_{i}():\n    return {pad}f{f}_{}()\n\n", i % 200 + 1)).collect() };
        for f in 1..=files {
            t.write(&format!("m{f}.py"), &module(f, ""));
        }
        t.commit("base");
        for f in 1..=files {
            t.write(&format!("m{f}.py"), &module(f, " "));
        }
        t.commit("head");
        assert_eq!(t.run(&["index"]).0, 0);
        let t0 = std::time::Instant::now();
        let v = t.json(&["--base", "HEAD~1"]);
        let took = t0.elapsed();
        assert!(v["changed"].as_array().unwrap().len() >= files * 200, "{files}");
        took
    };
    let (small, large) = (took(5), took(20));
    assert!(large < small * 8, "1,000 edited definitions took {small:?}, 4,000 took {large:?}");
}

#[test]
fn a_call_that_resolves_to_a_namesake_is_no_caller_of_the_deleted_definition() {
    let t = chain("deletednamesake");
    t.write("lib/other.py", "def deepest(x):\n    return x\n");
    t.write("lib/user.py", "from lib.other import deepest\n\n\ndef user(x):\n    return deepest(x)\n");
    t.commit("a namesake");
    t.write("lib/core.py", "def spare(x):\n    return x\n");
    let v = t.json(&[]);
    let gone: Vec<&str> = v["callers"].as_array().unwrap().iter().filter(|c| c["calls_deleted"] == true).map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(gone, ["lib/mid.py:4-5"], "{v}");
    assert!(!ids(&v, "callers").contains(&"lib/user.py:4-5".to_string()), "{v}");
}

#[test]
fn a_deleted_method_is_followed_where_the_receiver_types_it() {
    let t = Tree::new("deletedmethod");
    t.git(&["init", "-q"]);
    t.write(
        "shapes.py",
        "class Box:\n    def area(self):\n        return 1\n\n    def side(self):\n        return 2\n\n\nclass Disc:\n    def area(self):\n        return 3\n",
    );
    t.write("use.py", "from shapes import Box, Disc\n\n\ndef boxed():\n    return Box().area()\n\n\ndef round_one():\n    return Disc().area()\n");
    t.commit("first");
    t.write("shapes.py", "class Box:\n    def side(self):\n        return 2\n\n\nclass Disc:\n    def area(self):\n        return 3\n");
    let v = t.json(&[]);
    let gone: Vec<&str> = v["callers"].as_array().unwrap().iter().filter(|c| c["calls_deleted"] == true).map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(gone, ["use.py:4-5"], "{v}");
}

#[test]
fn a_renamed_file_with_an_edit_names_the_definition_at_its_new_path() {
    let t = chain("renamed");
    t.git(&["mv", "lib/top.py", "lib/upper.py"]);
    t.write("lib/upper.py", "from lib.mid import middle\n\n\ndef top(x):\n    return middle(x) + 0\n");
    t.write("tests/test_top.py", "from lib.upper import top\n\n\ndef test_top():\n    assert top(1) == 4\n");
    let text = t.impact(&[]);
    assert!(text.contains("lib/upper.py:4-5  [function] top  (edited)"), "{text}");
    assert!(text.contains("lib/upper.py  renamed from lib/top.py"), "{text}");
    // the test file changed its import, so it is a test to run
    assert!(text.contains("tests/test_top.py  (distance 0"), "{text}");
}

#[test]
fn staged_changes_alone_and_a_base_revision() {
    let t = chain("staged");
    t.write("lib/mid.py", "from lib.core import deepest\n\n\ndef middle(x):\n    return deepest(x) * 3\n");
    t.git(&["add", "lib/mid.py"]);
    t.write("lib/top.py", "from lib.mid import middle\n\n\ndef top(x):\n    return middle(x) - 1\n");
    let staged = t.json(&["--staged"]);
    assert_eq!(ids(&staged, "changed"), ["lib/mid.py:4-5"], "{staged}");
    assert!(t.impact(&["--staged"]).starts_with("staged changes against HEAD ("));
    let all = t.json(&[]);
    assert_eq!(ids(&all, "changed"), ["lib/mid.py:4-5", "lib/top.py:4-5"]);

    // committed: nothing against HEAD, the change against the commit before it
    t.commit("second");
    let (code, out, _) = t.run(&["impact"]);
    assert_eq!(code, 1);
    assert!(out.starts_with("no changes: working tree against HEAD ("), "{out}");
    let v = t.json(&["--base", "HEAD~1"]);
    assert_eq!(ids(&v, "changed"), ["lib/mid.py:4-5", "lib/top.py:4-5"]);
    assert_eq!(v["against"], "HEAD~1");
}

#[test]
fn three_dots_compare_with_the_merge_base() {
    let t = chain("mergebase");
    t.git(&["checkout", "-q", "-b", "feature"]);
    t.write("lib/core.py", "def deepest(x):\n    return x + 5\n\n\ndef spare(x):\n    return x\n");
    t.commit("feature");
    t.git(&["checkout", "-q", "main"]);
    t.write("tests/test_other.py", "from lib.core import spare\n\n\ndef test_spare():\n    assert spare(2) == 2\n");
    t.commit("main moves on");
    t.git(&["checkout", "-q", "feature"]);
    assert_eq!(ids(&t.json(&["--base", "main..."]), "changed"), ["lib/core.py:1-2"]);
    assert_eq!(ids(&t.json(&["--base", "main"]), "changed"), ["lib/core.py:1-2", "tests/test_other.py:4-5"]);
    let (code, _, err) = t.run(&["impact", "--base", "nope..."]);
    assert_eq!(code, 2);
    assert!(err.contains("not a revision of this repository: nope"), "{err}");
}

#[test]
fn an_untracked_file_counts_and_a_binary_file_is_a_note() {
    let t = chain("untracked");
    t.write("lib/extra.py", "from lib.core import deepest\n\n\ndef extra(x):\n    return deepest(x)\n");
    fs::write(t.root.join("logo.bin"), [0u8, 1, 2, 0, 255]).unwrap();
    let v = t.json(&[]);
    let changed: Vec<String> =
        v["changed"].as_array().unwrap().iter().map(|c| format!("{} {}", c["id"].as_str().unwrap(), c["status"].as_str().unwrap())).collect();
    assert!(changed.contains(&"lib/extra.py:4-5 added".to_string()), "{v}");
    assert!(v["notes"].as_array().unwrap().iter().any(|n| n["path"] == "logo.bin"), "{v}");

    // a tracked binary file that changed, and a submodule, are notes too
    t.commit("binary");
    fs::write(t.root.join("logo.bin"), [0u8, 9, 9, 0, 255]).unwrap();
    let head = t.git(&["rev-parse", "HEAD"]);
    t.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{},vendor/sub", head.trim())]);
    let text = t.impact(&[]);
    assert!(text.contains("logo.bin  a binary file"), "{text}");
    assert!(t.impact(&["--staged"]).contains("vendor/sub  a submodule: its commits are not read"), "{text}");
}

#[test]
fn outside_git_and_with_a_bad_revision_it_says_so_and_exits_2() {
    let t = Tree::new("nogit");
    t.write("a.py", "def f():\n    return 1\n");
    let out = Command::new(BIN)
        .args(["--root", t.root.to_str().unwrap(), "impact"])
        .envs(GIT_ENV)
        .env("GIT_CEILING_DIRECTORIES", t.root.parent().unwrap())
        .env("SCRUTATE_SEMANTIC", "none")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("is not in a git repository"), "{err}");
    assert!(out.stdout.is_empty());

    let t = chain("badrev");
    for (rev, says) in [("nope", "not a revision of this repository: nope"), ("--output=x", "not a revision: --output=x"), ("-p", "not a revision: -p")] {
        let (code, out, err) = t.run(&["impact", &format!("--base={rev}")]);
        assert_eq!(code, 2, "{rev}: {out}");
        assert!(err.contains(says), "{rev}: {err}");
    }
    assert!(!t.root.join("x").exists());

    // two histories with no commit in common
    t.git(&["checkout", "-q", "--orphan", "alone"]);
    t.commit("alone");
    let (code, _, err) = t.run(&["impact", "--base", "main..."]);
    assert_eq!(code, 2);
    assert!(err.contains("main and HEAD have no merge base"), "{err}");

    // without git on the path
    let empty = t.root.join("no-programs");
    fs::create_dir_all(&empty).unwrap();
    let out = Command::new(BIN).args(["--root", t.root.to_str().unwrap(), "impact"]).env("PATH", &empty).stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("git is not installed or not on PATH"));
}

/// The impact tool of one repository parses with that repository's custom languages, whichever
/// repository of the process loaded its own last (two Python `Repo`s side by side).
#[test]
fn the_impact_tool_parses_with_its_own_repository_s_languages() {
    let (a, b) = (Tree::new("own_langs_a"), Tree::new("own_langs_b"));
    for (t, word) in [(&a, "fn"), (&b, "def")] {
        t.write(
            ".scrutate.toml",
            &format!(
                "[[language]]\nname = \"twolang\"\nextensions = [\"twl\"]\ndefinitions = [\n  {{ kind = \"function\", pattern = '^{word}\\s+([a-z]+)' }},\n]\n"
            ),
        );
    }
    a.write("x.twl", "fn alpha\n  1\nend\n");
    a.git(&["init", "-q"]);
    a.commit("one");
    a.write("x.twl", "fn alpha\n  2\nend\n");
    // checked when opened, then not again for an hour: the call parses with whatever tables are in effect
    let every = scrutate::session::Every::given(Some(3600.0));
    let sa = scrutate::session::Session::open(&a.root, every).unwrap();
    let call = |tool: &str, args: serde_json::Value| scrutate::mcp::call_tool(&sa, tool, &args, &scrutate::mcp::Settings::new(None), &Default::default());
    // the first build has ended once a call answers
    assert!(call("outline", serde_json::json!({"path": "x.twl"})).text.contains("alpha"));
    scrutate::custom::load(&b.root).unwrap();
    let c = call("impact", serde_json::json!({}));
    // read with b's table the old copy of x.twl holds no definition, and alpha would be "added"
    assert!(!c.is_error && c.text.contains("alpha  (edited)"), "{}", c.text);
}

#[test]
fn a_relative_git_dir_set_in_a_subdirectory_is_read_from_there() {
    let t = chain("relative_git_dir");
    t.write("lib/core.py", "def deepest(x):\n    return x + 2\n\n\ndef spare(x):\n    return x\n");
    let sub = t.root.join("lib");
    let out = Command::new(BIN)
        .args(["--root", t.root.to_str().unwrap(), "impact"])
        .current_dir(&sub)
        .envs(GIT_ENV)
        .env("SCRUTATE_SEMANTIC", "none")
        .env("GIT_DIR", "../.git")
        .env("GIT_WORK_TREE", "..")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let (text, err) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{text}{err}");
    assert!(text.contains("lib/core.py:1-2  [function] deepest  (edited)"), "{text}");
}

#[test]
fn with_git_dir_alone_a_subdirectory_answers_as_the_root_and_another_repository_s_git_dir_is_refused() {
    let t = chain("git_dir_alone");
    // one committed change for review and history, one uncommitted for impact
    t.write("lib/core.py", "def deepest(x):\n    return x + 2\n\n\ndef spare(x):\n    return x\n");
    t.commit("second");
    t.write("lib/core.py", "def deepest(x):\n    return x + 3\n\n\ndef spare(x):\n    return x\n");
    let run = |dir: &Path, args: &[&str], vars: &[(&str, &str)]| {
        let out = Command::new(BIN)
            .args(["--root", t.root.to_str().unwrap()])
            .args(args)
            .current_dir(dir)
            .envs(GIT_ENV)
            .env("SCRUTATE_SEMANTIC", "none")
            .envs(vars.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let sub = t.root.join("lib");
    let calls: [&[&str]; 3] = [&["impact"], &["review", "--base", "HEAD~1"], &["read", "--symbol", "deepest", "--history"]];
    let answers = |dir: &Path, vars: &[(&str, &str)]| calls.map(|args| run(dir, args, vars)).map(|(code, out, _)| (code, out));
    let from_root = answers(&t.root, &[]);
    assert!(from_root.iter().all(|(code, _)| *code == Some(0)), "{from_root:?}");
    assert!(from_root.iter().take(2).all(|(_, out)| out.contains("deepest")), "{from_root:?}");
    assert!(from_root[2].1.contains("(history: changed since HEAD)"), "{}", from_root[2].1);
    // git runs at the root, so it takes the root as the work tree, as from the root itself
    assert_eq!(answers(&sub, &[("GIT_DIR", "../.git")]), from_root);
    assert_eq!(answers(&sub, &[("GIT_DIR", "../.git"), ("GIT_WORK_TREE", "..")]), from_root);
    // another repository's history is never laid over this tree
    let other = Tree::new("git_dir_other");
    other.git(&["init", "-q"]);
    other.write("lib/core.py", "def deepest(x):\n    return x\n");
    other.commit("other");
    let other_dir = other.root.join(".git");
    for dir in [&t.root, &sub] {
        let (code, text, err) = run(dir, &["impact"], &[("GIT_DIR", other_dir.to_str().unwrap())]);
        assert_eq!(code, Some(2), "{text}{err}");
        assert!(err.contains("is not the git directory of") && err.contains("GIT_WORK_TREE"), "{err}");
    }
    // a bare repository has no work tree at all
    let bare = t.root.with_file_name(format!("impact_git_dir_bare_{}", std::process::id()));
    let _ = fs::remove_dir_all(&bare);
    t.git(&["clone", "-q", "--bare", ".", bare.to_str().unwrap()]);
    let (code, text, err) = run(&sub, &["impact"], &[("GIT_DIR", bare.to_str().unwrap())]);
    let _ = fs::remove_dir_all(&bare);
    assert_eq!(code, Some(2), "{text}{err}");
    assert!(err.contains("not in a git repository"), "{err}");
    // a configured core.worktree names the tree for git dirs of every kind
    t.git(&["config", "core.worktree", t.root.to_str().unwrap()]);
    assert_eq!(answers(&sub, &[("GIT_DIR", "../.git")]), from_root);
    // a submodule's git directory names its own tree in core.worktree: the root is not in it
    let modules = t.root.join(".git/modules/sub");
    fs::create_dir_all(modules.parent().unwrap()).unwrap();
    t.git(&["init", "-q", "--separate-git-dir", modules.to_str().unwrap(), t.root.join("sub").to_str().unwrap()]);
    t.git(&["--git-dir", modules.to_str().unwrap(), "config", "core.worktree", "../../../sub"]);
    let (code, text, err) = run(&t.root, &["impact"], &[("GIT_DIR", modules.to_str().unwrap())]);
    assert_eq!(code, Some(2), "{text}{err}");
    assert!(err.contains("a git directory whose work tree is") && err.contains("sub,"), "{err}");
}

#[test]
fn a_new_copy_of_a_function_and_a_change_no_test_reaches_are_its_findings() {
    let t = chain("findings");
    t.write("lib/text.py", &long_fn("unique", "beta"));
    t.commit("unique");
    t.write("lib/fresh.py", &long_fn("copy_of_unique", "beta"));
    let v = t.json(&[]);
    let found: Vec<(&str, &str, &str, i64, i64, &str)> = v["findings"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .map(|f| {
            let s = |k: &str| f[k].as_str().unwrap_or_else(|| panic!("{f}"));
            (s("kind"), s("severity"), s("path"), f["start"].as_i64().unwrap(), f["end"].as_i64().unwrap(), s("qualname"))
        })
        .collect();
    assert_eq!(
        found,
        [("duplicate", "minor", "lib/fresh.py", 1, 10, "copy_of_unique"), ("untested-change", "info", "lib/fresh.py", 1, 10, "copy_of_unique")],
        "{v}"
    );
    assert_eq!(v["findings"][0]["text"], "copy_of_unique is an exact duplicate of unique at lib/text.py:1-10");
    assert_eq!(v["findings"][0]["related"], serde_json::json!([{"path": "lib/text.py", "qualname": "unique", "start": 1, "end": 10}]));
    assert_eq!(v["findings"][1]["text"], "copy_of_unique changed (added) and no test file reaches it within 3 steps of callers");
    assert_eq!(v["findings"][1]["related"], serde_json::json!([]));
}

#[test]
fn the_impact_tool_lists_the_findings_in_both_forms_and_says_it_does() {
    let t = chain("findings_tool");
    t.write("lib/text.py", &long_fn("unique", "beta"));
    t.commit("unique");
    t.write("lib/fresh.py", &long_fn("copy_of_unique", "beta"));
    let session = scrutate::session::Session::open(&t.root, scrutate::session::Every::given(Some(3600.0))).unwrap();
    let call = |form: &str| {
        let args = serde_json::json!({"response_format": form});
        let c = scrutate::mcp::call_tool(&session, "impact", &args, &scrutate::mcp::Settings::new(None), &Default::default());
        assert!(!c.is_error, "{}", c.text);
        c.text
    };
    let detailed = call("detailed");
    assert!(
        detailed.contains(
            "\nfindings: 2\nlib/fresh.py:1-10  [duplicate, minor] copy_of_unique is an exact duplicate of unique at lib/text.py:1-10\n\
             lib/fresh.py:1-10  [untested-change, info] copy_of_unique changed (added) and no test file reaches it within 3 steps of callers"
        ),
        "{detailed}"
    );
    let concise = call("concise");
    // the first line of a section names its directory and file, though the line above it is in
    // that file
    assert!(concise.contains("\nfindings: 2\nlib/\nfresh.py:1-10 copy_of_unique (duplicate) of text.py:1-10\n"), "{concise}");
    assert!(concise.ends_with("\nfresh.py:1-10 copy_of_unique (untested-change)"), "{concise}");
    let tools = scrutate::mcp::tools(false, 24_000, false);
    let impact = tools.as_array().unwrap().iter().find(|t| t["name"] == "impact").unwrap();
    assert!(impact["description"].as_str().unwrap().contains("findings"), "{impact}");
}

#[test]
fn an_edited_readme_section_is_a_changed_definition_and_no_untested_finding() {
    let t = chain("readme");
    t.write("README.md", "# Tool\n\n## Usage\n\nRun it.\n");
    t.commit("readme");
    t.write("README.md", "# Tool\n\n## Usage\n\nRun it twice.\n");
    let v = t.json(&[]);
    let kinds: Vec<&str> = v["changed"].as_array().unwrap().iter().map(|c| c["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"section"), "{v}");
    assert_eq!(v["findings"], serde_json::json!([]), "{v}");
}

/// The line that more duplicate groups were new than the list holds comes right after the
/// duplicates it counts, in both forms, and a page of the structured answer holds the findings
/// its text shows.
#[test]
fn the_line_that_more_duplicates_were_new_follows_the_duplicates_it_counts() {
    let t = chain("more");
    let fruit = ["mango", "pear", "plum", "kiwi", "lime", "guava", "papaya", "melon"];
    let seeds: Vec<String> = fruit.iter().flat_map(|a| fruit.iter().map(move |b| format!("{a}{b}"))).take(51).collect();
    for s in &seeds {
        t.write(&format!("lib/{s}.py"), &long_fn(&format!("{s}_one"), s));
    }
    t.commit("originals");
    for s in &seeds {
        t.write(&format!("lib/{s}_copy.py"), &long_fn(&format!("{s}_two"), s));
    }
    let text = t.impact(&[]);
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("(more than 50 duplicate groups")).unwrap_or_else(|| panic!("{text}"));
    assert!(lines[at - 1].contains("  [duplicate, minor] ") && lines[at + 1].contains("  [untested-change, info] "), "{text}");
    let session = scrutate::session::Session::open(&t.root, scrutate::session::Every::given(Some(3600.0))).unwrap();
    let call = |args: serde_json::Value| {
        let c = scrutate::mcp::call_tool(&session, "impact", &args, &scrutate::mcp::Settings::new(None), &Default::default());
        assert!(!c.is_error, "{}", c.text);
        c
    };
    let concise = call(serde_json::json!({"response_format": "concise"})).text;
    let lines: Vec<&str> = concise.lines().collect();
    let at = lines.iter().position(|l| l.starts_with("(more than 50 duplicate groups")).unwrap_or_else(|| panic!("{concise}"));
    assert!(lines[at - 1].ends_with(".py:1-10") && lines[at + 1].ends_with(" (untested-change)"), "{concise}");
    let (mut offset, mut pages, mut seen) = (0u64, 0, 0);
    loop {
        let c = call(serde_json::json!({"response_format": "detailed", "max_bytes": 1000, "offset": offset}));
        let shown = c.text.lines().filter(|l| l.contains("  [duplicate, minor] ") || l.contains("  [untested-change, info] ")).count();
        let held = c.structured["result"]["findings"].as_array().map_or(0, Vec::len);
        assert_eq!(shown, held, "page at {offset}: {}\n{}", c.text, c.structured);
        seen += held;
        pages += 1;
        match c.structured["next_offset"].as_u64() {
            Some(next) if pages < 200 => offset = next,
            _ => break,
        }
    }
    // 50 duplicate findings, then the untested copies
    assert_eq!(seen, 50 + seeds.len(), "over {pages} pages");
}

#[test]
fn a_test_file_the_change_deletes_is_no_test_to_run() {
    let t = chain("rm_test");
    t.git(&["rm", "-q", "tests/test_other.py"]);
    for args in [&[][..], &["--staged"][..]] {
        let v = t.json(args);
        assert!(!ids(&v, "tests").contains(&"tests/test_other.py".to_string()), "{args:?}: {v}");
        // what it defined is still a change
        assert!(ids(&v, "changed").iter().any(|c| c.starts_with("tests/test_other.py")), "{args:?}: {v}");
    }
}

/// Four changed functions with 500 callers each fill the walk's 2,000; the fifth, which a test
/// calls, is never reached.
#[test]
fn a_walk_cut_at_its_limit_sorts_its_callers_and_claims_no_untested_change() {
    let t = Tree::new("cut_walk");
    t.git(&["init", "-q"]);
    for (i, dir) in ["z", "y", "x", "w", "v"].iter().enumerate() {
        t.write(&format!("lib/f{i}.py"), &format!("def f{i}(x):\n    return x + {i}\n"));
        let callers = if i < 4 { 500 } else { 0 };
        for file in 0..callers / 100 {
            let defs: String = (0..100).map(|j| format!("def c{i}_{file}_{j}(x):\n    return f{i}(x)\n\n\n")).collect();
            t.write(&format!("{dir}{i}/m{file}.py"), &format!("from lib.f{i} import f{i}\n\n\n{defs}"));
        }
    }
    t.write("tests/test_f4.py", "from lib.f4 import f4\n\n\ndef test_f4():\n    assert f4(1) == 5\n");
    t.commit("first");
    t.run(&["index"]);
    for i in 0..5 {
        t.write(&format!("lib/f{i}.py"), &format!("def f{i}(x):\n    return x + {i} + 1\n"));
    }
    let v = t.json(&["--depth", "1"]);
    assert_eq!(v["stopped"], true, "{}", v["callers"].as_array().map_or(0, |c| c.len()));
    let callers: Vec<(i64, String, i64)> = v["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["distance"].as_i64().unwrap(), c["path"].as_str().unwrap().to_string(), c["start"].as_i64().unwrap()))
        .collect();
    let mut sorted = callers.clone();
    sorted.sort();
    assert!(callers == sorted, "first callers {:?}", &callers[..3]);
    // a test past the cut may reach a change: no change is said to be one no test reaches
    let untested: Vec<&Value> = v["findings"].as_array().unwrap().iter().filter(|f| f["kind"] == "untested-change").collect();
    assert!(untested.is_empty(), "{untested:?}");
}

/// Reading a change writes nothing of the repository's and starts no program its configuration
/// names: a touched file is no reason to rewrite .git/index, and a clean filter never runs.
#[test]
fn impact_rewrites_no_index_and_runs_no_clean_filter() {
    let t = chain("no_filter");
    t.write(".gitattributes", "*.py filter=logged\n");
    t.git(&["config", "filter.logged.clean", "sh -c 'echo ran >> .git/filter.log; cat'"]);
    t.git(&["config", "filter.logged.required", "true"]);
    t.commit("filtered");
    let log = t.root.join(".git/filter.log");
    let _ = fs::remove_file(&log);
    // a stat change alone, and an edit
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    fs::File::options().write(true).open(t.root.join("lib/top.py")).unwrap().set_modified(later).unwrap();
    t.write("lib/mid.py", "from lib.core import deepest\n\n\ndef middle(x):\n    return deepest(x) * 3\n");
    let unchanged = |args: &[&str]| {
        let index = fs::read(t.root.join(".git/index")).unwrap();
        let v = t.json(args);
        assert_eq!(ids(&v, "changed"), ["lib/mid.py:4-5"], "{args:?}: {v}");
        assert!(fs::read(t.root.join(".git/index")).unwrap() == index, "{args:?}: .git/index was rewritten");
        assert!(!log.exists(), "{args:?}: the clean filter ran {} times", fs::read_to_string(&log).unwrap_or_default().lines().count());
    };
    unchanged(&[]);
    // staging runs the filter itself, so its log starts again after it
    t.git(&["add", "lib/mid.py"]);
    let _ = fs::remove_file(&log);
    unchanged(&["--staged"]);
}

/// git 2.44 and later can be told not to fetch an object it lacks: (major, minor) of the git on
/// PATH.
#[cfg(unix)]
fn git_version() -> (u32, u32) {
    let out = Command::new("git").arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut v = text.split_whitespace().nth(2).unwrap_or("0.0").split('.').map(|n| n.parse().unwrap_or(0));
    (v.next().unwrap_or(0), v.next().unwrap_or(0))
}

/// A blobless clone lacks the base's old blobs. Reading the change must not fetch them, which
/// writes .git/objects and runs the remote's upload program from the repository's configuration;
/// the answer says what is missing.
#[cfg(unix)]
#[test]
fn a_partial_clone_fetches_nothing_and_says_what_it_lacks() {
    if git_version() < (2, 44) {
        eprintln!("skipped: git {:?} has no GIT_NO_LAZY_FETCH", git_version());
        return;
    }
    let origin = chain("lazy_origin");
    origin.write("lib/core.py", "def deepest(x):\n    return x + 5\n\n\ndef spare(x):\n    return x\n");
    origin.commit("second");
    origin.git(&["config", "uploadpack.allowFilter", "true"]);
    let t = Tree::new("lazy_clone");
    t.git(&["clone", "-q", "--filter=blob:none", &format!("file://{}", origin.root.display()), "."]);
    let marker = t.root.join(".git/fetched");
    t.git(&["config", "remote.origin.uploadpack", &format!("touch '{}'; git-upload-pack", marker.display())]);
    t.write("lib/core.py", "def deepest(x):\n    return x + 6\n\n\ndef spare(x):\n    return x\n");
    let (code, out, err) = t.run(&["impact", "--base", "HEAD~1"]);
    assert!(!marker.exists(), "impact fetched from the remote: {out}{err}");
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("this partial clone lacks"), "{err}");
}

#[test]
fn a_deleted_definition_called_through_an_alias_or_in_another_case_keeps_its_caller() {
    // Python and JavaScript import it under another name; PHP names a function in any case
    // (tree, files, the file as edited, the caller)
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], (&'a str, &'a str), &'a str);
    let cases: [Case; 3] = [
        (
            "deleted_alias_py",
            &[
                ("lib/__init__.py", ""),
                ("lib/util.py", "def helper():\n    return 1\n\n\ndef keep():\n    return 2\n"),
                ("lib/app.py", "from lib.util import helper as h\n\n\ndef run():\n    return h()\n"),
            ],
            ("lib/util.py", "def keep():\n    return 2\n"),
            "lib/app.py:4-5",
        ),
        (
            "deleted_alias_js",
            &[
                ("util.js", "export function helper() {\n  return 1;\n}\n\nexport function keep() {\n  return 2;\n}\n"),
                ("app.js", "import { helper as h } from \"./util.js\";\n\nexport function run() {\n  return h();\n}\n"),
            ],
            ("util.js", "export function keep() {\n  return 2;\n}\n"),
            "app.js:3-5",
        ),
        (
            "deleted_case_php",
            &[
                ("a.php", "<?php\nfunction DoThing() {\n  return 1;\n}\n\nfunction keep() {\n  return 2;\n}\n"),
                ("b.php", "<?php\nfunction user() {\n  return dothing();\n}\n"),
            ],
            ("a.php", "<?php\nfunction keep() {\n  return 2;\n}\n"),
            "b.php:2-4",
        ),
    ];
    for (name, files, (path, after), caller) in cases {
        let t = Tree::new(name);
        t.git(&["init", "-q"]);
        for (rel, text) in files {
            t.write(rel, text);
        }
        t.commit("first");
        t.write(path, after);
        let v = t.json(&[]);
        let gone: Vec<&str> = v["callers"].as_array().unwrap().iter().filter(|c| c["calls_deleted"] == true).map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(gone, [caller], "{name}: {v}");
    }
}
