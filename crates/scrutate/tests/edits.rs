//! `serve --allow-edits` and its `edit_symbol` tool: one whole definition replaced, a dry run
//! unless asked, written only with the hash of the file as it is, and only when the new file
//! parses and keeps its definitions.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Server {
    root: PathBuf,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: i64,
}

impl Server {
    fn start(name: &str, files: &[(&str, &str)], args: &[&str]) -> Server {
        let root = std::env::temp_dir().join(format!("scrutate_edits_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".git")).unwrap();
        for (rel, text) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        let mut child = Command::new(BIN)
            .arg("--root")
            .arg(&root)
            .arg("serve")
            .args(args)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        let mut s = Server { root, child, stdin, stdout, id: 0 };
        s.ask("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}));
        s
    }

    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})).unwrap();
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == self.id {
                return v;
            }
        }
    }

    /// The answer's text and whether it is an error, once the index is built.
    fn call(&mut self, name: &str, args: Value) -> (String, bool) {
        loop {
            let v = self.ask("tools/call", json!({"name": name, "arguments": args}));
            if let Some(e) = v.get("error") {
                return (e["message"].as_str().unwrap_or("").to_string(), true);
            }
            let text = v["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{v}")).to_string();
            if !text.contains("still building its index") {
                return (text, v["result"]["isError"] == true);
            }
        }
    }

    fn file(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn hash_in(text: &str) -> String {
        text.lines().find_map(|l| l.strip_prefix("hash: ")).unwrap_or_else(|| panic!("no hash in {text}")).trim().to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

const PY: &str =
    "import os\n\n\ndef first():\n    return 1\n\n\ndef second():\n    return first() + 1\n\n\nclass Box:\n    def size(self):\n        return 3\n";

#[test]
fn without_the_flag_the_tool_is_neither_listed_nor_run() {
    let mut s = Server::start("off", &[("a.py", PY)], &[]);
    let tools = s.ask("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(!names.contains(&"edit_symbol"), "{names:?}");
    let (text, err) = s.call("edit_symbol", json!({"symbol": "first", "new_text": "def first():\n    return 9\n", "apply": true}));
    assert!(err && text.contains("unknown tool"), "{text}");
    assert_eq!(s.file("a.py"), PY);
    // no environment variable turns it on
    let out = Command::new(BIN).args(["serve", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("--allow-edits") && !help.contains("SCRUTATE_ALLOW_EDITS"), "{help}");
}

#[test]
fn with_the_flag_it_is_listed_as_the_one_tool_that_writes() {
    let mut s = Server::start("listed", &[("a.py", PY)], &["--allow-edits"]);
    let tools = s.ask("tools/list", json!({}));
    let all = tools["result"]["tools"].as_array().unwrap();
    let edit = all.iter().find(|t| t["name"] == "edit_symbol").expect("edit_symbol is listed");
    assert_eq!(edit["annotations"]["readOnlyHint"], false);
    assert_eq!(edit["annotations"]["destructiveHint"], true);
    for k in ["symbol", "new_text", "apply", "hash", "path"] {
        assert!(edit["inputSchema"]["properties"].get(k).is_some(), "{k}: {edit}");
    }
    assert!(all.iter().filter(|t| t["name"] != "edit_symbol" && t["name"] != "reindex").all(|t| t["annotations"]["readOnlyHint"] == true));
}

#[test]
fn a_dry_run_shows_the_diff_and_the_hash_and_writes_nothing() {
    let mut s = Server::start("dry", &[("a.py", PY)], &["--allow-edits"]);
    let (text, err) = s.call("edit_symbol", json!({"symbol": "first", "new_text": "def first():\n    return 2\n"}));
    assert!(!err, "{text}");
    assert!(text.contains("-    return 1") && text.contains("+    return 2") && text.contains("--- a.py"), "{text}");
    assert!(text.contains("dry run"), "{text}");
    Server::hash_in(&text);
    assert_eq!(s.file("a.py"), PY);
}

#[test]
fn apply_writes_exactly_with_the_current_hash_and_refuses_a_stale_one() {
    let mut s = Server::start("apply", &[("a.py", PY)], &["--allow-edits"]);
    let new = "def first():\n    return 2\n";
    let (dry, _) = s.call("edit_symbol", json!({"symbol": "first", "new_text": new}));
    let hash = Server::hash_in(&dry);
    // apply without the hash is refused
    let (text, err) = s.call("edit_symbol", json!({"symbol": "first", "new_text": new, "apply": true}));
    assert!(err && text.contains("hash"), "{text}");
    let (text, err) = s.call("edit_symbol", json!({"symbol": "first", "new_text": new, "apply": true, "hash": hash}));
    assert!(!err, "{text}\nthe dry run said:\n{dry}");
    assert_eq!(s.file("a.py"), PY.replace("    return 1\n", "    return 2\n"));
    // the same call again: the file moved, so its hash did
    let (text, err) = s.call("edit_symbol", json!({"symbol": "first", "new_text": new, "apply": true, "hash": hash}));
    assert!(err && text.contains("changed"), "{text}");
    let now = Server::hash_in(&text);
    assert_ne!(now, hash);
    // a write by someone else after the dry run: refused, and the file keeps that write
    let (dry, _) = s.call("edit_symbol", json!({"symbol": "second", "new_text": "def second():\n    return 0\n"}));
    let hash = Server::hash_in(&dry);
    let theirs = s.file("a.py").replace("import os\n", "import os\nimport sys\n");
    fs::write(s.root.join("a.py"), &theirs).unwrap();
    let (text, err) = s.call("edit_symbol", json!({"symbol": "second", "new_text": "def second():\n    return 0\n", "apply": true, "hash": hash}));
    assert!(err && text.contains("changed"), "{text}");
    assert_eq!(s.file("a.py"), theirs);
}

#[test]
fn an_edit_that_breaks_the_parse_or_the_definitions_is_refused() {
    let mut s = Server::start("checks", &[("a.py", PY), ("lib.rs", "pub fn one() -> u8 {\n    1\n}\n\npub fn two() -> u8 {\n    2\n}\n")], &["--allow-edits"]);
    let refused = |s: &mut Server, symbol: &str, new: &str, why: &str| {
        let (text, err) = s.call("edit_symbol", json!({"symbol": symbol, "new_text": new}));
        assert!(err && text.contains(why), "{symbol} {new:?}: {text}");
    };
    refused(&mut s, "first", "def first(:\n    return 1\n", "parse");
    refused(&mut s, "one", "pub fn one() -> u8 {\n    1\n", "parse");
    // the body twice: the duplicate an edit by range makes
    refused(&mut s, "first", "def first():\n    return 2\n\n\ndef first():\n    return 1\n", "first");
    // another name, or another definition beside it
    refused(&mut s, "first", "def other():\n    return 1\n", "first");
    refused(&mut s, "two", "pub fn two() -> u8 {\n    2\n}\n\npub fn three() -> u8 {\n    3\n}\n", "three");
    assert_eq!(s.file("a.py"), PY);
    // a method, by its qualified name, keeps its class around it
    let (dry, err) = s.call("edit_symbol", json!({"symbol": "Box.size", "new_text": "    def size(self):\n        return 4\n"}));
    assert!(!err, "{dry}");
    let (text, err) = s.call(
        "edit_symbol",
        json!({"symbol": "Box.size", "new_text": "    def size(self):\n        return 4\n", "apply": true, "hash": Server::hash_in(&dry)}),
    );
    assert!(!err, "{text}");
    assert!(s.file("a.py").ends_with("class Box:\n    def size(self):\n        return 4\n"));
}

#[test]
fn a_name_with_two_definitions_is_refused_until_a_path_or_location_picks_one() {
    let mut s = Server::start("two", &[("a.py", PY), ("b.py", "def first():\n    return 10\n")], &["--allow-edits"]);
    let (text, err) = s.call("edit_symbol", json!({"symbol": "first", "new_text": "def first():\n    return 2\n"}));
    assert!(err && text.contains("a.py:4-5") && text.contains("b.py:1-2"), "{text}");
    let (text, err) = s.call("edit_symbol", json!({"symbol": "first", "path": "b.py", "new_text": "def first():\n    return 2\n"}));
    assert!(!err && text.contains("--- b.py"), "{text}");
    let (text, err) = s.call("edit_symbol", json!({"symbol": "a.py:4-5", "new_text": "def first():\n    return 2\n"}));
    assert!(!err && text.contains("--- a.py"), "{text}");
}

#[test]
fn crlf_lines_stay_crlf_and_the_rest_of_the_file_is_byte_for_byte_the_same() {
    let crlf = PY.replace('\n', "\r\n");
    let mut s = Server::start("crlf", &[("a.py", &crlf)], &["--allow-edits"]);
    let new = "def second():\n    return 5\n";
    let (dry, _) = s.call("edit_symbol", json!({"symbol": "second", "new_text": new}));
    let (text, err) = s.call("edit_symbol", json!({"symbol": "second", "new_text": new, "apply": true, "hash": Server::hash_in(&dry)}));
    assert!(!err, "{text}");
    assert_eq!(s.file("a.py"), crlf.replace("    return first() + 1\r\n", "    return 5\r\n"));
}

#[cfg(unix)]
#[test]
fn a_file_reached_through_a_link_is_never_written() {
    let mut s = Server::start("link", &[("real/a.py", PY)], &["--allow-edits"]);
    std::os::unix::fs::symlink(s.root.join("real/a.py"), s.root.join("linked.py")).unwrap();
    let (text, _) = s.call("edit_symbol", json!({"symbol": "linked.py:4-5", "new_text": "def first():\n    return 2\n", "apply": true, "hash": "x"}));
    assert!(!text.contains("+    return 2") || text.starts_with("error"), "{text}");
    assert_eq!(s.file("real/a.py"), PY);
}

/// One file per language with four definitions, each returning its number.
fn defs_file(lang: &str, values: &[i32]) -> (String, Vec<String>) {
    let def = |i: usize, v: i32| match lang {
        "py" => format!("def f{i}():\n    return {v}\n"),
        "rs" => format!("fn f{i}() -> i32 {{\n    {v}\n}}\n"),
        "ts" => format!("function f{i}(): number {{\n  return {v};\n}}\n"),
        "go" => format!("func F{i}() int {{\n\treturn {v}\n}}\n"),
        _ => format!("    int f{i}() {{\n        return {v};\n    }}\n"),
    };
    let defs: Vec<String> = values.iter().enumerate().map(|(i, v)| def(i, *v)).collect();
    let body = defs.join("\n");
    let text = match lang {
        "go" => format!("package p\n\n{body}"),
        "java" => format!("class C {{\n{body}}}\n"),
        _ => body,
    };
    (text, defs)
}

#[test]
fn a_hundred_edits_over_five_languages_land_byte_for_byte_and_every_stale_one_is_refused() {
    let langs = ["py", "rs", "ts", "go", "java"];
    let files: Vec<(String, String)> = langs.iter().map(|l| (format!("f.{l}"), defs_file(l, &[0, 1, 2, 3]).0)).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let mut s = Server::start("hundred", &refs, &["--allow-edits"]);
    let (mut exact, mut stale_refused, mut broken_landed) = (0, 0, 0);
    for lang in langs {
        let path = format!("f.{lang}");
        let mut values = vec![0, 1, 2, 3];
        let symbol = |i: usize| match lang {
            "go" => format!("F{i}"),
            "java" => format!("C.f{i}"),
            _ => format!("f{i}"),
        };
        for k in 0..20 {
            let i = k % 4;
            let old = defs_file(lang, &values).1[i].clone();
            let mut next = values.clone();
            next[i] = 100 + k as i32;
            let new = defs_file(lang, &next).1[i].clone();
            let args = json!({"symbol": symbol(i), "path": path, "new_text": new});
            let (dry, err) = s.call("edit_symbol", args.clone());
            assert!(!err, "{lang} {k}: {dry}");
            let hash = Server::hash_in(&dry);
            let mut apply = args.clone();
            apply["apply"] = json!(true);
            apply["hash"] = json!(hash);
            if k % 5 == 4 {
                // another writer comes between the read and the write
                let mut theirs = s.file(&path);
                theirs.push_str(if lang == "py" { "# theirs\n" } else { "// theirs\n" });
                fs::write(s.root.join(&path), &theirs).unwrap();
                let (text, err) = s.call("edit_symbol", apply);
                stale_refused += usize::from(err && text.contains("changed") && Server::hash_in(&text) != hash && s.file(&path) == theirs);
                continue;
            }
            let expected = s.file(&path).replacen(&old, &new, 1);
            let (text, err) = s.call("edit_symbol", apply);
            assert!(!err, "{lang} {k}: {text}");
            exact += usize::from(s.file(&path) == expected);
            values = next;
        }
        // an edit that breaks the parse never lands
        let before = s.file(&path);
        let broken = defs_file(lang, &values).1[0].replacen('(', "((", 1);
        let (dry, _) = s.call("edit_symbol", json!({"symbol": symbol(0), "path": path, "new_text": broken}));
        let hash = dry.lines().find_map(|l| l.strip_prefix("hash: ")).unwrap_or("none").to_string();
        let _ = s.call("edit_symbol", json!({"symbol": symbol(0), "path": path, "new_text": broken, "apply": true, "hash": hash}));
        broken_landed += usize::from(s.file(&path) != before);
    }
    assert_eq!((exact, stale_refused, broken_landed), (80, 20, 0));
}

/// A nested repository's git directory under another spelling (APFS and NTFS take `.GIT` for
/// `.git`) is neither indexed nor edited.
#[test]
fn a_nested_git_directory_under_another_spelling_is_neither_indexed_nor_edited() {
    let hook = "def inside_git():\n    return 1\n";
    let mut s = Server::start("dotgit", &[("vendor/lib/.GIT/hooks/helper.py", hook), ("a.py", PY)], &["--allow-edits"]);
    let (text, err) = s.call("find_symbol", json!({"name": "inside_git", "exact": true}));
    assert!(!err && !text.contains(".GIT"), "{text}");
    let (text, err) = s.call(
        "edit_symbol",
        json!({"symbol": "vendor/lib/.GIT/hooks/helper.py:1-2", "new_text": "def inside_git():\n    return 2\n", "apply": true, "hash": "x"}),
    );
    assert!(err, "{text}");
    assert_eq!(s.file("vendor/lib/.GIT/hooks/helper.py"), hook);
}

/// A repository file that also has a hard link inside `.scrutate` is refused by every check of the
/// index directory, an edit's dry run too, since its identity cannot tell which name came first;
/// each refusal names the index file it is and says to remove the hard link.
/// A path inside, and a link that leads inside, keep the plain "is inside" line. Where the file
/// system makes no hard link (FAT, exFAT) it says so and checks nothing.
#[test]
fn a_file_with_a_hard_link_in_the_index_directory_is_refused_as_a_hard_link_not_as_inside() {
    const LINKED: &str = "remove the hard link, by its path";
    let mut s = Server::start("hardlink", &[("a.py", PY)], &["--allow-edits"]);
    let (text, err) = s.call("find_symbol", json!({"name": "first", "exact": true}));
    assert!(!err, "{text}");
    if let Err(e) = fs::hard_link(s.root.join("a.py"), s.root.join(".scrutate/copy.py")) {
        eprintln!("skipped: this file system makes no hard link ({e})");
        return;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(".scrutate/copy.py", s.root.join("via.py")).unwrap();
    let calls = [
        ("edit_symbol", json!({"symbol": "first", "path": "a.py", "new_text": "def first():\n    return 9\n"})),
        ("read_code", json!({"path": "a.py"})),
        ("read_code", json!({"symbol": "first", "path": "a.py"})),
        ("grep_code", json!({"pattern": "first", "path": "a.py"})),
    ];
    for (tool, args) in calls {
        let (text, err) = s.call(tool, args.clone());
        assert!(err && text.contains("a.py is the same file as") && text.contains(LINKED) && !text.contains("a.py is inside"), "{tool} {args}: {text}");
    }
    let inside: &[(&str, &str)] = if cfg!(unix) {
        &[(".scrutate/copy.py", "read_code"), ("via.py", "read_code"), ("via.py", "grep_code")]
    } else {
        &[(".scrutate/copy.py", "read_code")]
    };
    for &(path, tool) in inside {
        let args = if tool == "read_code" { json!({"path": path}) } else { json!({"pattern": "first", "path": path}) };
        let (text, err) = s.call(tool, args);
        assert!(err && text.contains(&format!("{path} is inside scrutate's index directory")) && !text.contains(LINKED), "{tool} {path}: {text}");
    }
    assert_eq!(s.file("a.py"), PY);
    let home = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edits_hardlink_home_{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let hook = |args: &[&str], input: Option<Value>| {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(&s.root)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LOCALAPPDATA", home.join("state"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("CLAUDE_PROJECT_DIR", &s.root)
            .env_remove("CLAUDE_CONFIG_DIR");
        c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.map(|v| v.to_string()).unwrap_or_default().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let (_, err) = hook(&["hook", "run", "--", "grep", "-n", "first", "a.py"], None);
    assert!(err.contains("a.py is the same file as") && err.contains(LINKED), "hook run: {err}");
    let (_, err) = hook(&["hook", "run", "--", "grep", "-n", "first", ".scrutate/copy.py"], None);
    assert!(err.contains(".scrutate/copy.py is inside") && !err.contains(LINKED), "hook run inside: {err}");
    let grep = |path: &str| json!({"session_id": "t", "hook_event_name": "PreToolUse", "tool_name": "Grep", "cwd": s.root.to_str().unwrap(), "tool_input": {"pattern": "first", "path": path}, "tool_use_id": "t1", "permission_mode": "default"});
    let (out, _) = hook(&["hook", "grep"], Some(grep("a.py")));
    assert!(out.contains("a.py is the same file as") && out.contains(LINKED), "Grep hook: {out}");
    #[cfg(unix)]
    {
        let (out, _) = hook(&["hook", "grep"], Some(grep("via.py")));
        assert!(out.contains("via.py is inside") && !out.contains(LINKED), "Grep hook through a link: {out}");
    }
}

/// An edit replaces whole lines, so a definition that shares its first or last line with other
/// code (another definition, a statement before or after it) is refused, a dry run too, and the
/// file is left as it was.
#[test]
fn a_definition_that_shares_a_line_with_other_code_is_refused() {
    let cases = [
        ("a.js", "function f() { return 1 } function g() { return 2 }\n\nfunction h() { return g() }\n", "f", "function f() { return 5 }\n"),
        ("a.rs", "pub struct A(u8); pub struct B;\n\nfn main() {}\n", "A", "pub struct A(u16);\n"),
        ("b.js", "init(); function f() {\n  return 1\n}\n\nfunction h() { return f() }\n", "f", "function f() {\n  return 5\n}\n"),
        ("c.js", "function f() {\n  return 1\n}; register(f); window.ready = true;\n\nfunction h() { return f() }\n", "f", "function f() {\n  return 5\n}\n"),
    ];
    for (file, text, name, new_text) in cases {
        let mut s = Server::start(&format!("shared_{}", file.replace('.', "_")), &[(file, text)], &["--allow-edits"]);
        let (dry, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text}));
        assert!(err && dry.contains("shares a line with other code") && dry.contains("nothing written"), "{file} dry run: {dry}");
        let (out, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text, "apply": true, "hash": "0000000000000000"}));
        assert!(err, "{file} apply: {out}");
        assert_eq!(s.file(file), text, "{file} was written");
    }
}

/// A definition whose own first line starts with a word of it (`export`, `async`, `static`, a Go
/// receiver), or with an attribute or decorator on the line above, has its lines to itself and is
/// edited, the prefix as `new_text` gives it.
#[test]
fn a_definition_with_a_prefix_on_its_line_is_edited() {
    let cases = [
        ("a.ts", "export function f() {\n  return 1\n}\n", "f", "export function f() {\n  return 2\n}\n"),
        ("b.js", "export default function f() {\n  return 1\n}\n", "f", "export default function f() {\n  return 2\n}\n"),
        ("c.js", "async function f() {\n  return 1\n}\n", "f", "async function f() {\n  return 2\n}\n"),
        ("d.c", "static int f(void) {\n  return 1;\n}\n", "f", "static int f(void) {\n  return 2;\n}\n"),
        ("e.go", "package p\n\ntype T struct{}\n\nfunc (r T) M() int {\n\treturn 1\n}\n", "M", "func (r T) M() int {\n\treturn 2\n}\n"),
        ("f.py", "@decorator\ndef f():\n    return 1\n", "f", "@decorator\ndef f():\n    return 2\n"),
        ("g.rs", "#[inline]\npub fn f() -> u8 {\n    1\n}\n", "f", "#[inline]\npub fn f() -> u8 {\n    2\n}\n"),
    ];
    let mut failed = Vec::new();
    for (file, text, name, new_text) in cases {
        let mut s = Server::start(&format!("prefix_{}", file.replace('.', "_")), &[(file, text)], &["--allow-edits"]);
        let (dry, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text}));
        if err {
            failed.push(format!("{file} dry run: {dry}"));
            continue;
        }
        let (out, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text, "apply": true, "hash": Server::hash_in(&dry)}));
        let got = s.file(file);
        if err || !got.ends_with(new_text) || got.len() != text.len() {
            failed.push(format!("{file} apply ({err}): {out}\n{got:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// An edit of a file's first definition keeps the file's UTF-8 byte-order mark, which lies on the
/// definition's first line but is no part of it; an edit of a later one leaves it as it was, and a
/// `new_text` that starts with one gets none of its own.
#[test]
fn an_edit_keeps_the_byte_order_mark_and_never_doubles_it() {
    const BOM: &str = "\u{feff}";
    let py = "def first():\n    return 1\n\n\ndef second():\n    return 2\n";
    let ts = "function first() {\n  return 1;\n}\n\nfunction second() {\n  return 2;\n}\n";
    let cs = "class First {\n  int F() { return 1; }\n}\n\nclass Second {\n  int G() { return 2; }\n}\n";
    let cases = [
        ("a.py", py, "first", "def first():\n    return 9\n".to_string(), py.replace("return 1", "return 9")),
        ("a.ts", ts, "first", "function first() {\n  return 9;\n}\n".to_string(), ts.replace("return 1", "return 9")),
        ("a.cs", cs, "First", "class First {\n  int F() { return 9; }\n}\n".to_string(), cs.replace("return 1", "return 9")),
        ("b.py", py, "second", "def second():\n    return 9\n".to_string(), py.replace("return 2", "return 9")),
        ("c.py", py, "first", format!("{BOM}def first():\n    return 9\n"), py.replace("return 1", "return 9")),
    ];
    let mut failed = Vec::new();
    for (file, text, name, new_text, want) in cases {
        let mut s = Server::start(&format!("bom_{}", file.replace('.', "_")), &[(file, &format!("{BOM}{text}"))], &["--allow-edits"]);
        let (dry, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text}));
        if err {
            failed.push(format!("{file} dry run: {dry}"));
            continue;
        }
        let (out, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text, "apply": true, "hash": Server::hash_in(&dry)}));
        let got = s.file(file);
        if err || got != format!("{BOM}{want}") {
            failed.push(format!("{file} apply ({err}): {out}\n{got:?}"));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// A file already named as the edit's temporary file is never opened or removed: the edit is
/// refused, names it, and leaves it and the edited file as they were.
#[test]
fn a_file_with_the_temporary_name_is_left_alone_and_the_edit_refused() {
    let mut s = Server::start("tmp_taken", &[("a.py", PY)], &["--allow-edits"]);
    let tmp = format!(".a.py.scrutate-{}.tmp", s.child.id());
    fs::write(s.root.join(&tmp), "the user's own file\n").unwrap();
    let new_text = "def first():\n    return 9\n";
    let (dry, err) = s.call("edit_symbol", json!({"symbol": "first", "path": "a.py", "new_text": new_text}));
    assert!(!err, "{dry}");
    let (out, err) = s.call("edit_symbol", json!({"symbol": "first", "path": "a.py", "new_text": new_text, "apply": true, "hash": Server::hash_in(&dry)}));
    assert!(err && out.contains(&tmp), "{out}");
    assert_eq!(s.file(&tmp), "the user's own file\n");
    assert_eq!(s.file("a.py"), PY);
}

/// The new text takes the line ending of the lines it replaces: one CRLF line elsewhere in an LF
/// file makes the edited definition no CRLF, and a CRLF file's edit is all CRLF.
#[test]
fn the_edit_takes_the_line_ending_of_the_lines_it_replaces() {
    let lf = "def first():\n    return 1\n\n\ndef second():\r\n    return 2\n";
    let crlf = "def first():\r\n    return 1\r\n\r\n\r\ndef second():\r\n    return 2\r\n";
    // a CRLF file whose last line, the edited definition, has no line ending
    let last = "def a():\r\n    return 1\r\n\r\n\r\ndef first(): return 2";
    let cases = [
        ("lf.py", lf, lf.replace("return 1", "return 9")),
        ("crlf.py", crlf, crlf.replace("return 1", "return 9")),
        ("last.py", last, "def a():\r\n    return 1\r\n\r\n\r\ndef first():\r\n    return 9\r\n".to_string()),
    ];
    for (file, text, want) in cases {
        let mut s = Server::start(&format!("eol_{}", file.replace('.', "_")), &[(file, text)], &["--allow-edits"]);
        let new_text = "def first():\n    return 9\n";
        let (dry, err) = s.call("edit_symbol", json!({"symbol": "first", "path": file, "new_text": new_text}));
        assert!(!err, "{file}: {dry}");
        let (out, err) = s.call("edit_symbol", json!({"symbol": "first", "path": file, "new_text": new_text, "apply": true, "hash": Server::hash_in(&dry)}));
        assert!(!err, "{file}: {out}");
        assert_eq!(s.file(file), want, "{file}");
    }
}

/// A comment on the definition's first or last line is the definition's, as `read_code` shows
/// its lines: the edit goes through with the comment replaced, where other code there is refused.
#[test]
fn a_comment_on_the_definitions_first_or_last_line_is_edited_with_it() {
    // (file, text, name, the definition's old lines, its new lines)
    let cases = [
        ("a.go", "package p\n\ntype ID int // the user id\n", "ID", "type ID int // the user id\n", "type ID int64 // the user id\n"),
        (
            "b.rs",
            "fn f() -> u8 {\n    1\n} // end f\n\nfn g() -> u8 {\n    f()\n}\n",
            "f",
            "fn f() -> u8 {\n    1\n} // end f\n",
            "fn f() -> u8 {\n    2\n} // end f\n",
        ),
        ("c.js", "const f = () => 1; // one\n\nfunction g() {\n  return f()\n}\n", "f", "const f = () => 1; // one\n", "const f = () => 2; // two\n"),
        (
            "d.go",
            "package p\n\nfunc f() int { return 1 } // doc\n\nfunc g() int { return f() }\n",
            "f",
            "func f() int { return 1 } // doc\n",
            "func f() int { return 2 } // doc\n",
        ),
        (
            "e.js",
            "/* first */ function f() {\n  return 1\n}\n\nfunction g() {\n  return f()\n}\n",
            "f",
            "/* first */ function f() {\n  return 1\n}\n",
            "/* first */ function f() {\n  return 2\n}\n",
        ),
    ];
    let mut failed = Vec::new();
    for (file, text, name, old_def, new_text) in cases {
        let mut s = Server::start(&format!("comment_{}", file.replace('.', "_")), &[(file, text)], &["--allow-edits"]);
        let (dry, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text}));
        if err {
            failed.push(format!("{file} dry run: {dry}"));
            continue;
        }
        let (out, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text, "apply": true, "hash": Server::hash_in(&dry)}));
        let got = s.file(file);
        if err || got != text.replacen(old_def, new_text, 1) {
            failed.push(format!("{file} apply ({err}): {out}\n{got:?}"));
        }
    }
    // a statement after the definition is other code, with a comment after it or not
    let mut s = Server::start("comment_and_code", &[("f.js", "function f() {\n  return 1\n}; init() // go\n")], &["--allow-edits"]);
    let (dry, err) = s.call("edit_symbol", json!({"symbol": "f", "path": "f.js", "new_text": "function f() {\n  return 2\n}\n"}));
    if !(err && dry.contains("other code that is not a comment")) {
        failed.push(format!("f.js: {dry}"));
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// A name many files define is found in the file a path names, whatever the others: the path
/// narrows the lookup before the answer's limit of definitions applies. 25 files each define
/// `a0`; past the first 21 a lookup by name answered "no definition matches" for the rest.
#[test]
fn a_path_finds_its_definition_of_a_name_many_files_define() {
    const N: usize = 25;
    let files: Vec<(String, String)> = (1..=N).map(|i| (format!("f{i}.py"), format!("def a0():\n    return {i}\n"))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let mut s = Server::start("many_a0", &refs, &["--allow-edits"]);
    let mut missed = Vec::new();
    for (i, (path, _)) in files.iter().enumerate() {
        let (text, err) = s.call("read_code", json!({"symbol": "a0", "path": path}));
        if err || !text.contains(&format!("return {}", i + 1)) {
            missed.push(format!("read_code {path}: {text}"));
        }
        let (text, err) = s.call("edit_symbol", json!({"symbol": "a0", "path": path, "new_text": "def a0():\n    return 0\n"}));
        if err {
            missed.push(format!("edit_symbol {path}: {text}"));
        }
        let out = Command::new(BIN)
            .args(["read", path, "--symbol", "a0"])
            .current_dir(&s.root)
            .env("SCRUTATE_SEMANTIC", "none")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        if !out.status.success() || !String::from_utf8_lossy(&out.stdout).contains(&format!("return {}", i + 1)) {
            missed.push(format!("read --symbol {path}: {}", String::from_utf8_lossy(&out.stderr)));
        }
    }
    assert!(missed.is_empty(), "{} of {} lookups missed:\n{}", missed.len(), 3 * N, missed.join("\n"));
}

/// A constant on a file's first line is edited: the file's module chunk, which starts there too, is
/// the file's lines and no second definition in the new text.
#[test]
fn a_constant_on_the_first_line_is_edited() {
    let cases = [
        ("m.js", "const a = 1;\nconst b = 2;\n", "a", "const a = 5;\n"),
        ("m.py", "A = 1\nB = 2\n", "A", "A = 5\n"),
        ("m.rs", "pub const A: u8 = 1;\npub const B: u8 = 2;\n", "A", "pub const A: u8 = 5;\n"),
    ];
    for (file, text, name, new_text) in cases {
        let mut s = Server::start(&format!("first_{}", file.replace('.', "_")), &[(file, text)], &["--allow-edits"]);
        let (dry, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text}));
        assert!(!err, "{file}: {dry}");
        let (out, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text, "apply": true, "hash": Server::hash_in(&dry)}));
        assert!(!err, "{file}: {out}");
        assert_eq!(s.file(file), text.replacen(text.lines().next().unwrap(), new_text.trim_end(), 1), "{file}");
    }
    // a name the file's module chunk carries is no definition: the answer says so, not "call again"
    let text = "int a = 1;\n\nint f(void) { return a; }\n";
    let mut s = Server::start("first_global_c", &[("a.c", text)], &["--allow-edits"]);
    let (out, err) = s.call("edit_symbol", json!({"symbol": "a", "path": "a.c", "new_text": "int a = 2;\n"}));
    assert!(err && out.contains("a names a module (a.c:1-2), not a definition"), "{out}");
    assert_eq!(s.file("a.c"), text);
}

/// A C++ namespace is a module, as `read_code` names it: `edit_symbol` refuses it as one, not as a
/// name nothing matches, and the file stays as it was.
#[test]
fn a_namespace_is_refused_as_a_module() {
    let text = "namespace N {\nint f() { return 1; }\n}\n";
    let mut s = Server::start("namespace_module", &[("b.cpp", text)], &["--allow-edits"]);
    let (read, _) = s.call("read_code", json!({"symbol": "N"}));
    assert!(read.contains("[module] N"), "{read}");
    let (out, err) = s.call("edit_symbol", json!({"symbol": "N", "path": "b.cpp", "new_text": "namespace N {\nint f() { return 2; }\n}\n"}));
    assert!(err && out.contains("N names a module (b.cpp:1-3), not a definition") && out.contains("name one inside it"), "{out}");
    assert_eq!(s.file("b.cpp"), text);
}

/// A type that ends in `};` is edited: the `;` the grammar leaves outside the definition (beside
/// it, beside the template that wraps it, or as an empty statement) is the definition's. A
/// declarator after the brace (`} p;`) is other code, and refused.
#[test]
fn a_c_or_cpp_type_ending_in_a_semicolon_is_edited() {
    let cases = [
        ("a.cpp", "struct P {\n    int m() { return 1; }\n};\n\nint g() { return 2; }\n", "P", "struct P {\n    int m() { return 5; }\n};\n"),
        ("b.cpp", "class P {\n    int m() { return 1; }\n};\n\nint g() { return 2; }\n", "P", "class P {\n    int m() { return 5; }\n};\n"),
        ("c.cpp", "struct P { int x; };\n\nint g() { return 2; }\n", "P", "struct P { long x; };\n"),
        ("d.c", "struct P {\n    int x;\n};\n\nint g(void) { return 2; }\n", "P", "struct P {\n    long x;\n};\n"),
        ("e.cpp", "enum E {\n    A,\n    B\n};\n\nint g() { return 2; }\n", "E", "enum E {\n    A,\n    C\n};\n"),
        ("g.cpp", "template <typename T>\nstruct P {\n    T x;\n};\n\nint g() { return 2; }\n", "P", "template <typename T>\nstruct P {\n    T y;\n};\n"),
        ("h.cpp", "template <class T>\nclass B {\n    T x;\n};\n\nint g() { return 2; }\n", "B", "template <class T>\nclass B {\n    T y;\n};\n"),
        ("i.ts", "interface I {\n  x: number;\n};\n\nfunction g() { return 2 }\n", "I", "interface I {\n  y: number;\n};\n"),
        ("j.ts", "class K {\n  x = 1;\n};\n\nfunction g() { return 2 }\n", "K", "class K {\n  x = 5;\n};\n"),
        ("k.ts", "enum E {\n  A,\n};\n\nfunction g() { return 2 }\n", "E", "enum E {\n  B,\n};\n"),
    ];
    let mut failed = Vec::new();
    for (file, text, name, new_text) in cases {
        let mut s = Server::start(&format!("semi_{}", file.replace('.', "_")), &[(file, text)], &["--allow-edits"]);
        let (dry, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text}));
        if err {
            failed.push(format!("{file} dry run: {dry}"));
            continue;
        }
        let (out, err) = s.call("edit_symbol", json!({"symbol": name, "path": file, "new_text": new_text, "apply": true, "hash": Server::hash_in(&dry)}));
        let old_def = &text[..text.find("\n\n").unwrap() + 1];
        let want = text.replacen(old_def, new_text, 1);
        if err || s.file(file) != want {
            failed.push(format!("{file} apply ({err}): {out}\n{:?}", s.file(file)));
        }
    }
    let text = "struct P {\n    int x;\n} p;\n\nint g(void) { return p.x; }\n";
    let mut s = Server::start("semi_declarator", &[("f.c", text)], &["--allow-edits"]);
    let (dry, err) = s.call("edit_symbol", json!({"symbol": "P", "path": "f.c", "new_text": "struct P {\n    long x;\n};\n"}));
    if !(err && dry.contains("shares a line with other code")) || s.file("f.c") != text {
        failed.push(format!("f.c `}} p;`: {dry}"));
    }
    assert!(failed.is_empty(), "{}", failed.join("\n\n"));
}

/// A new text that does not parse is refused naming the error it adds, not one the file had: an
/// old error on line 3, an edit on line 20 that adds one.
#[test]
fn a_parse_error_is_named_where_the_edit_made_it() {
    let filler: String = (4..20).map(|i| format!("function g{i}() {{ return {i} }}\n")).collect();
    let text = format!("function a() {{ return 1 }}\n\nlet q = ;\n{filler}function f() {{ return 2 }}\n");
    let mut s = Server::start("parse_error_line", &[("m.js", text.as_str())], &["--allow-edits"]);
    let (out, err) = s.call("edit_symbol", json!({"symbol": "f", "path": "m.js", "new_text": "function f() { return 2 +; }\n"}));
    assert!(err && out.contains("does not parse: an error at line 20 of the edited file"), "{out}");
    assert_eq!(s.file("m.js"), text);
}
