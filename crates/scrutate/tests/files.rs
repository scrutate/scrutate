//! `scrutate files` and the MCP `find_files` tool: a typo, a dropped letter, a swap and a path
//! fragment find the file, an exact basename ranks first, reads through `read` and `read_code`
//! raise a file above its namesake, and the state file lives under `XDG_STATE_HOME`.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Tree {
    root: PathBuf,
    state: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let top = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("files_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&top);
        let root = top.join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(top.join("state")).unwrap();
        let top = canon(top);
        let t = Tree { root: top.join("repo"), state: top.join("state") };
        for p in
            ["src/search.rs", "lib/search.rs", "src/source.rs", "src/session.rs", "src/searcher_tools.rs", "deep/a/b/ops.rs", "x/topsy.rs", "docs/notes.md"]
        {
            t.write(p, "fn x() {}\n");
        }
        t
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn command(&self) -> Command {
        let mut c = Command::new(BIN);
        c.args(["--root", self.root.to_str().unwrap()])
            .current_dir(&self.root)
            .env("XDG_STATE_HOME", &self.state)
            .env("HOME", &self.state)
            .env("LOCALAPPDATA", &self.state)
            .env("SCRUTATE_SEMANTIC", "none")
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = self.command().args(args).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    /// The replies of `serve --structured` to `messages`, after the greeting.
    fn serve(&self, messages: &[String]) -> Vec<Value> {
        let mut child = self.command().args(["serve", "--structured"]).stdout(Stdio::piped()).stdin(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","capabilities":{{}},"clientInfo":{{"name":"t","version":"0"}}}}}}"#).unwrap();
        for m in messages {
            writeln!(stdin, "{m}").unwrap();
        }
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    fn files(&self, query: &str) -> Vec<String> {
        let (code, out, err) = self.run(&["files", query]);
        assert_eq!(code, 0, "files {query}: {err}");
        out.lines().map(str::to_string).collect()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.root.parent().unwrap());
    }
}

#[test]
fn a_typo_a_dropped_letter_a_swap_and_a_path_fragment_find_the_file() {
    let t = Tree::new("typos");
    for q in ["srch.rs", "serch.rs", "saerch.rs", "seerch.rs", "src/sear", "SRC/Search"] {
        let found = t.files(q);
        assert!(found.iter().take(3).any(|p| p == "src/search.rs"), "{q}: {found:?}");
    }
    // namesakes tie on everything but the path
    assert_eq!(t.files("search.rs")[..2], ["lib/search.rs", "src/search.rs"]);
    // an exact basename (with or without its extension) beats a shorter path that only contains it
    assert_eq!(t.files("ops")[0], "deep/a/b/ops.rs");
    assert_eq!(t.files("search")[..3], ["lib/search.rs", "src/search.rs", "src/searcher_tools.rs"]);
    // the same answer every time
    assert_eq!(t.run(&["files", "s"]), t.run(&["files", "s"]));
    let (code, out, _) = t.run(&["files", "zqzqzq"]);
    assert_eq!((code, out.as_str()), (1, ""));
    let (code, _, err) = t.run(&["files", " "]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("files needs a query"), "{err}");
    assert!(!t.root.join(".scrutate").exists(), "no index is made");
}

#[test]
fn reads_through_read_and_read_code_rank_a_file_above_its_namesake() {
    let t = Tree::new("frecency");
    assert_eq!(t.files("search.rs")[0], "lib/search.rs");
    for _ in 0..3 {
        assert_eq!(t.run(&["read", "src/search.rs"]).0, 0);
    }
    assert_eq!(t.files("search.rs")[..2], ["src/search.rs", "lib/search.rs"]);
    let file = t.state.join("scrutate/frecency.json");
    let v: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let key = t.root.to_string_lossy().into_owned();
    assert!(v["roots"][&key]["src/search.rs"]["score"].as_f64().unwrap() > 2.9, "{v}");

    // a query that is exactly a path is the caller's choice of that file
    for _ in 0..5 {
        assert_eq!(t.files("lib/search.rs")[0], "lib/search.rs");
    }
    assert_eq!(t.files("search.rs")[0], "lib/search.rs");

    // read_code over MCP counts too (its calls run at once, so find_files goes to a second server)
    let reads: Vec<String> = (0..10)
        .map(|i| format!(r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"read_code","arguments":{{"path":"src/search.rs"}}}}}}"#, 10 + i))
        .collect();
    let replies = t.serve(&reads);
    assert!((10..20).all(|id| replies.iter().any(|v| v["id"] == id && v["result"]["isError"] != true)), "{replies:?}");
    let replies = t.serve(&[
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_files","arguments":{"query":"search.rs","limit":2}}}"#.to_string(),
    ]);
    let reply = |id: i64| replies.iter().find(|v| v["id"] == id).unwrap()["result"].clone();
    let tool = reply(2)["tools"].as_array().unwrap().iter().find(|x| x["name"] == "find_files").cloned().unwrap();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    let text = reply(3)["content"][0]["text"].as_str().unwrap().to_string();
    assert!(text.starts_with("src/search.rs\nlib/search.rs"), "{text}");
    let (_, cli, _) = t.run(&["files", "search.rs", "--limit", "2"]);
    assert_eq!(text, cli.trim_end());
    let result = &reply(3)["structuredContent"]["result"];
    assert_eq!(result[0]["path"], "src/search.rs");
    assert_eq!(result[0]["match"], "exact");
    assert!(result[0]["frecency"].as_f64().unwrap() > 12.9, "{result}");

    // a state file that does not parse is ignored and written again
    fs::write(&file, "not json").unwrap();
    assert_eq!(t.files("search.rs")[0], "lib/search.rs");
    assert_eq!(t.run(&["read", "src/search.rs"]).0, 0);
    let v: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(v["roots"][&key]["src/search.rs"]["score"].as_f64(), Some(1.0), "{v}");
}

#[test]
fn json_and_limit_say_where_the_answer_stopped() {
    let t = Tree::new("json");
    let (code, out, err) = t.run(&["files", "s", "--limit", "2", "--json"]);
    assert_eq!(code, 0, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["stopped_at"], 2, "{v}");
    let hits = v["result"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0], serde_json::json!({"path": "x/topsy.rs", "match": "basename", "edits": 0, "frecency": 0.0}));
    let (_, text, err) = t.run(&["files", "s", "--limit", "2"]);
    assert_eq!(text.lines().collect::<Vec<_>>(), ["x/topsy.rs", "docs/notes.md", "(stopped at 2 files: raise limit to see more)"], "{err}");
}

#[test]
fn this_repository_finds_its_own_files_from_misspelt_names() {
    let top = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let state = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("files_self_{}", std::process::id()));
    fs::create_dir_all(&state).unwrap();
    for (q, want) in
        [("srch.rs", "crates/scrutate/src/search.rs"), ("fuzzyr", "crates/scrutate/src/fuzzyre.rs"), ("src/fnder", "crates/scrutate/src/finder.rs")]
    {
        let out = Command::new(BIN)
            .args(["--root", top.to_str().unwrap(), "files", q, "--limit", "3"])
            .env("XDG_STATE_HOME", &state)
            .env("HOME", &state)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.lines().any(|l| l == want), "{q}: {text}");
    }
    fs::remove_dir_all(&state).unwrap();
}
