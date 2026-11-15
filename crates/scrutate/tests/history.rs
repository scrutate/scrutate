//! `read --symbol --history` and read_code's `history`: the commits that changed a definition.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
const GIT_ENV: [(&str, &str); 2] = [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scrutate_history_{name}_{}_{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Tree { root: fs::canonicalize(root).unwrap() }
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(&self.root)
            .envs(GIT_ENV)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn commit(&self, rel: &str, text: &str, message: &str) {
        self.write(rel, text);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

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

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(&[args, &["--json"]].concat());
        serde_json::from_str::<Value>(&out).unwrap()["result"].clone()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// f and g, each edited in commits of its own, the file renamed between.
fn shaped(name: &str) -> Tree {
    let t = Tree::new(name);
    t.git(&["init", "-q"]);
    t.commit("old.py", "def f(x):\n    return x\n\n\ndef g(y):\n    return y\n", "add f and g");
    t.commit("old.py", "def f(x):\n    return x + 1\n\n\ndef g(y):\n    return y\n", "edit f");
    t.git(&["mv", "old.py", "new.py"]);
    t.git(&["commit", "-q", "-m", "rename"]);
    t.commit("new.py", "def f(x):\n    return x + 1\n\n\ndef g(y):\n    return y * 2\n", "edit g");
    t.commit("new.py", "def f(x):\n    return x + 2\n\n\ndef g(y):\n    return y * 2\n", "edit f again");
    t
}

fn subjects(def: &Value) -> Vec<String> {
    def["history"].as_array().unwrap_or_else(|| panic!("{def}")).iter().map(|c| c["subject"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_definition_lists_the_commits_that_changed_its_lines_newest_first() {
    let t = shaped("lines");
    let v = t.json(&["read", "--symbol", "f", "--history"]);
    assert_eq!(subjects(&v["definitions"][0]), ["edit f again", "edit f", "add f and g"]);
    let c = &v["definitions"][0]["history"][0];
    assert!(c["commit"].as_str().unwrap().len() == 40 && c["date"].as_str().unwrap().len() == 10, "{c}");
    let v = t.json(&["read", "--symbol", "g", "--history", "1"]);
    assert_eq!(subjects(&v["definitions"][0]), ["edit g"]);
    let text = t.ok(&["read", "--symbol", "f", "--history"]);
    let at = text.find("history:").unwrap_or_else(|| panic!("{text}"));
    let lines: Vec<&str> = text[at..].lines().skip(1).take(3).collect();
    assert!(lines[0].ends_with(" edit f again") && lines[2].ends_with(" add f and g"), "{text}");
    // without the flag the answer is what it was
    assert!(!t.ok(&["read", "--symbol", "f"]).contains("history"));
    assert!(t.json(&["read", "--symbol", "f"])["definitions"][0].get("history").is_none());
}

#[test]
fn an_uncommitted_edit_moves_the_lines_or_marks_the_definition() {
    let t = shaped("edits");
    t.write("new.py", "# one\n# two\n# three\ndef f(x):\n    return x + 2\n\n\ndef g(y):\n    return y * 2\n");
    let v = t.json(&["read", "--symbol", "f", "--history"]);
    assert_eq!(subjects(&v["definitions"][0]), ["edit f again", "edit f", "add f and g"], "the lines above f moved it");
    t.write("new.py", "def f(x):\n    return x + 3\n\n\ndef g(y):\n    return y * 2\n");
    let v = t.json(&["read", "--symbol", "f", "--history"]);
    assert_eq!(v["definitions"][0]["history"], json!([]));
    assert_eq!(v["definitions"][0]["history_note"], "changed since HEAD", "{v}");
    assert!(t.ok(&["read", "--symbol", "f", "--history"]).contains("(history: changed since HEAD)"));
    t.write("added.py", "def fresh():\n    return 1\n");
    let v = t.json(&["read", "--symbol", "fresh", "--history"]);
    assert_eq!(v["definitions"][0]["history_note"], "not in HEAD", "{v}");
}

#[test]
fn outside_a_repository_the_answer_says_why_it_has_no_history() {
    let t = Tree::new("nogit");
    t.write("m.py", "def f(x):\n    return x\n");
    let v = t.json(&["read", "--symbol", "f", "--history"]);
    assert!(v["history_note"].as_str().unwrap().contains("not in a git repository"), "{v}");
    assert!(t.ok(&["read", "--symbol", "f", "--history"]).contains("(no history: "));
}

#[test]
fn history_is_given_to_the_first_ten_definitions_shown() {
    let t = Tree::new("cap");
    t.git(&["init", "-q"]);
    for i in 0..12 {
        t.write(&format!("m{i:02}.py"), "def run():\n    return 1\n");
    }
    t.git(&["add", "-A"]);
    t.git(&["commit", "-q", "-m", "twelve runs"]);
    let v = t.json(&["read", "--symbol", "run", "--history"]);
    let defs = v["definitions"].as_array().unwrap();
    assert_eq!(defs.iter().filter(|d| d.get("history").is_some()).count(), 10, "{v}");
    assert_eq!(v["history_more"], defs.len() - 10);
    assert!(t.ok(&["read", "--symbol", "run", "--history"]).contains("(history for the first 10 definitions)"));
}

#[test]
fn read_code_takes_history_and_gives_the_same_list() {
    let t = shaped("mcp");
    // the session builds its own index, with the model it runs with
    let session = scrutate::session::Session::open(Path::new(&t.root), scrutate::session::Every::given(Some(3600.0))).unwrap();
    session.build().unwrap();
    let call = |args: Value| {
        let c = scrutate::mcp::call_tool(&session, "read_code", &args, &scrutate::mcp::Settings::new(None), &Default::default());
        assert!(!c.is_error, "{}", c.text);
        (c.text, c.structured)
    };
    let (text, v) = call(json!({"symbol": "f", "history": 5}));
    assert_eq!(subjects(&v["result"]["definitions"][0]), ["edit f again", "edit f", "add f and g"], "{text}");
    assert!(text.contains("edit f again"), "{text}");
    let (_, v) = call(json!({"symbol": "f"}));
    assert!(v["result"]["definitions"][0].get("history").is_none());
}
