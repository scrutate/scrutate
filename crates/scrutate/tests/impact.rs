//! `scrutate impact` and the MCP `impact` tool on git repositories made for each test: a call
//! chain three deep with two test files, then a deletion, a rename with an edit, a staged-only
//! change, a base revision, a merge base, an untracked file and a directory outside git, and the
//! callers of a deleted definition.

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
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Tree { root: fs::canonicalize(root).unwrap() }
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
