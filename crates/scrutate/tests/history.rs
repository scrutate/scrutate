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

/// A git that prints one commit for `log -L` and then hangs: the lookup stops at its bound, shows
/// what git had printed, and says how to make it fast.
#[cfg(unix)]
#[test]
fn a_history_lookup_stops_at_its_bound_and_says_how_to_make_it_fast() {
    use std::os::unix::fs::PermissionsExt;
    let t = shaped("slow");
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout).unwrap().trim().to_string();
    let bin = t.root.with_extension("bin");
    let _ = fs::remove_dir_all(&bin);
    fs::create_dir_all(&bin).unwrap();
    let fake = bin.join("git");
    fs::write(&fake, format!("#!/bin/sh\ncase \" $* \" in *\" log \"*) printf '\\001%040d\\000%s\\000%s\\n' 0 2026-01-01 'the newest'; exec sleep 60;; esac\nexec '{real}' \"$@\"\n")).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let started = std::time::Instant::now();
    let out = Command::new(BIN)
        .args(["--root", t.root.to_str().unwrap(), "read", "--symbol", "f", "--history", "--json"])
        .current_dir(&t.root)
        .envs(GIT_ENV)
        .env("SCRUTATE_SEMANTIC", "none")
        .env("PATH", path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let took = started.elapsed();
    let _ = fs::remove_dir_all(&bin);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(took < std::time::Duration::from_secs(20), "took {took:?}");
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let def = &v["result"]["definitions"][0];
    assert_eq!(subjects(def), ["the newest"], "{def}");
    let note = def["history_note"].as_str().unwrap_or_default();
    assert!(note.contains("stopped after 5 s") && note.contains("git commit-graph write --reachable"), "{def}");
}

/// A git on PATH that runs the real one, except as `case` says.
#[cfg(unix)]
fn fake_git(t: &Tree, case: &str) -> (std::path::PathBuf, String) {
    use std::os::unix::fs::PermissionsExt;
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout).unwrap().trim().to_string();
    let bin = t.root.with_extension("bin");
    let _ = fs::remove_dir_all(&bin);
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("git"), format!("#!/bin/sh\ncase \" $* \" in {case} esac\nexec '{real}' \"$@\"\n")).unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    (bin, path)
}

#[cfg(unix)]
#[test]
fn a_git_older_than_the_floor_gives_no_history_and_says_why() {
    let t = shaped("old_git");
    let (bin, path) = fake_git(&t, "*\" --version \"*) echo 'git version 2.20.0'; exit 0;;");
    let out = Command::new(BIN)
        .args(["--root", t.root.to_str().unwrap(), "read", "--symbol", "f", "--history", "--json"])
        .current_dir(&t.root)
        .envs(GIT_ENV)
        .env("SCRUTATE_SEMANTIC", "none")
        .env("PATH", path)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&bin);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stderr)));
    let note = v["result"]["history_note"].as_str().unwrap_or_default();
    assert!(note.contains("git 2.20 is older than 2.25"), "{v}");
    assert!(v["result"]["definitions"][0].get("history").is_none(), "{v}");
}

/// A clone of depth 1: git gives its one commit every line, which is no change of the definition.
#[cfg(unix)]
#[test]
fn a_shallow_clone_marks_where_its_history_is_cut() {
    let full = shaped("shallow_source");
    let t = Tree::new("shallow");
    let out = Command::new("git")
        .args(["clone", "-q", "--depth", "1", &format!("file://{}", full.root.display()), t.root.to_str().unwrap()])
        .envs(GIT_ENV)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v = t.json(&["read", "--symbol", "g", "--history"]);
    let def = &v["definitions"][0];
    let history = def["history"].as_array().unwrap_or_else(|| panic!("{v}"));
    assert_eq!(history.len(), 1, "{def}");
    assert!(history[0]["cut"].as_str().is_some_and(|c| c.contains("shallow clone")) && history[0].get("subject").is_none(), "{def}");
    assert!(def["history_note"].as_str().unwrap_or_default().starts_with("shallow clone: history before "), "{def}");
    let text = t.ok(&["read", "--symbol", "g", "--history"]);
    assert!(text.contains("(history cut here: shallow clone)") && !text.contains(" edit f again"), "{text}");
}

/// A definition with uncommitted edits is looked up in HEAD's copy without git rewriting
/// .git/index or running the clean filter the configuration names.
#[test]
fn a_history_lookup_rewrites_no_index_and_runs_no_clean_filter() {
    let t = shaped("no_filter");
    t.write(".gitattributes", "*.py filter=logged\n");
    t.git(&["config", "filter.logged.clean", "sh -c 'echo ran >> .git/filter.log; cat'"]);
    t.git(&["add", "-A"]);
    t.git(&["commit", "-q", "-m", "filtered"]);
    let log = t.root.join(".git/filter.log");
    let _ = fs::remove_file(&log);
    // a line added above g moves it, so the lookup reads the file's diff against HEAD
    t.write("new.py", "# moved\ndef f(x):\n    return x + 2\n\n\ndef g(y):\n    return y * 2\n");
    let index = fs::read(t.root.join(".git/index")).unwrap();
    let v = t.json(&["read", "--symbol", "g", "--history"]);
    assert!(v["definitions"][0]["history"].as_array().is_some_and(|h| !h.is_empty()), "{v}");
    assert!(fs::read(t.root.join(".git/index")).unwrap() == index, ".git/index was rewritten");
    assert!(!log.exists(), "the clean filter ran {} times", fs::read_to_string(&log).unwrap_or_default().lines().count());
}
