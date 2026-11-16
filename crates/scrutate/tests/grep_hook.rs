//! `scrutate hook grep` fed the JSON Claude Code sends for its Grep tool: the lines it answers
//! with are ripgrep's for the same arguments (run here with the arguments Grep passes it), a call
//! it cannot answer exactly gets no answer, and `hook stats` counts the bytes it measured.

#[path = "support/canon.rs"]
mod canon;
#[path = "support/rg.rs"]
mod rg;
#[path = "support/tag.rs"]
mod tag;
use rg::have_rg;

use canon::canon;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
const HEADER: &str = "Grep result:";
/// The separator Grep prints between the parts of a path.
const SEP: &str = if cfg!(windows) { "\\" } else { "/" };

/// `p` as Claude Code has it: canonical, on Windows without the verbatim prefix.
fn real(p: PathBuf) -> PathBuf {
    canon(p)
}

/// A fixture tree and a scratch home under the build's own temporary directory.
struct Tree {
    base: PathBuf,
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("hook_{name}_{}", std::process::id()));
        tag::claim(&base);
        let _ = fs::remove_dir_all(&base);
        let root = base.join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(base.join("home")).unwrap();
        let t = Tree { root: real(root), base: real(base) };
        t.write(".git/config", "needle in git\n");
        t.write(".gitignore", "ignored/\n*.log\n");
        t.write("ignored/i.txt", "needle ignored\n");
        t.write("x.log", "needle log\n");
        t.write("src/a.rs", "fn needle() {}\n// Needle upper\nlet x = needles;\nneeeedle\n");
        t.write("src/b.py", "needle = 1\n");
        t.write("src/c.pyi", "needle: int\n");
        t.write("deep/x/y.txt", "one\nneedle two\nthree\n");
        t.write(".hidden/h.txt", "needle hidden\n");
        t.write("long.txt", &format!("needle {}\nshort needle\n", "x".repeat(600)));
        t.write("crlf.txt", "needle\r\nother\r\n");
        t.write("multi.txt", "start here\nmiddle\nend there\n");
        t.write("ctx.txt", &(1..=20).map(|i| if [5, 7, 15].contains(&i) { format!("needle {i}\n") } else { format!("line {i}\n") }).collect::<String>());
        t.write("uni.txt", "naïve needle ü\n");
        t
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn input(&self, tool_input: Value) -> Value {
        json!({"session_id": "test-session", "hook_event_name": "PreToolUse", "tool_name": "Grep", "cwd": self.root.to_str().unwrap(), "tool_input": tool_input, "tool_use_id": "t1", "permission_mode": "default"})
    }

    /// The hook's stdout for `input`, and its exit code.
    fn hook(&self, input: &Value, env: &[(&str, &str)]) -> (i32, String) {
        let home = self.base.join("home");
        let tmp = canon(env!("CARGO_TARGET_TMPDIR"));
        assert!(canon(&self.base).starts_with(&tmp), "the hook runs only with a scratch home");
        let mut c = Command::new(BIN);
        c.args(["hook", "grep"]).current_dir(&self.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        c.env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("LOCALAPPDATA", home.join("state"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("CLAUDE_PROJECT_DIR", &self.root)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env("SCRUTATE_MAX_BYTES", "1000000");
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8(out.stdout).unwrap())
    }

    /// The reason text the hook answers with, or `None` when it let Grep run.
    fn reason(&self, tool_input: Value) -> Option<String> {
        let (code, out) = self.hook(&self.input(tool_input), &[]);
        assert_eq!(code, 0);
        if out.is_empty() {
            return None;
        }
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        let r = v["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().to_string();
        Some(r.strip_prefix(HEADER).and_then(|r| r.strip_prefix('\n')).expect("the header").to_string())
    }

    fn stats(&self) -> Value {
        let home = self.base.join("home");
        let out = Command::new(BIN)
            .args(["hook", "stats", "--json"])
            .env("HOME", &home)
            .env("XDG_STATE_HOME", home.join("state"))
            .env("LOCALAPPDATA", home.join("state"))
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn a_type_keeps_ignored_files_out_where_a_glob_brings_them_in() {
    use scrutate::instant::{Files, Output, Pattern, Query, Record, search};
    let t = Tree::new("types");
    t.write(".gitignore", "gone.py\n");
    t.write("gone.py", "needle\n");
    let files = |f: Files| {
        let q = Query { pattern: Pattern { text: "needle".into(), ..Pattern::default() }, first_in_path_order: true, ..Query::default() };
        let mut paths = Vec::new();
        search(
            &q,
            &f,
            Default::default(),
            Output::Records(&mut |r| {
                if let Record::Match(l) = r {
                    paths.push(l.path.rsplit('/').next().unwrap().to_string());
                }
            }),
        )
        .unwrap();
        paths
    };
    let walk = || Files::walk(vec![t.root.clone()]);
    assert_eq!(files(Files { types: vec!["py".into()], ..walk() }), ["b.py", "c.pyi"]);
    assert_eq!(files(Files { globs: vec!["*.py".into()], ..walk() }), ["gone.py", "b.py"]);
    assert!(search(&Query::default(), &Files { types: vec!["nope".into()], ..walk() }, Default::default(), Output::Records(&mut |_| {})).is_err());
}

/// What Grep makes of ripgrep's output for `input`: ripgrep run with the arguments Grep gives it,
/// then Grep's own handling of the lines (long lines, context breaks, paths from the session's
/// directory). `rg --sort path` keeps the order fixed; Grep's own runs in any order.
fn grep_lines(t: &Tree, input: &Value) -> Vec<String> {
    let i = &input["tool_input"];
    let mode = i["output_mode"].as_str().unwrap_or("files_with_matches");
    let mut a: Vec<String> = vec!["--sort".into(), "path".into(), "--hidden".into()];
    for d in [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"] {
        a.extend(["--glob".into(), format!("!{d}")]);
    }
    a.extend(["--max-columns".into(), "500".into()]);
    if i["multiline"] == true {
        a.extend(["-U".into(), "--multiline-dotall".into()]);
    }
    if i["-i"] == true {
        a.push("-i".into());
    }
    match mode {
        "files_with_matches" => a.push("-l".into()),
        "count" => a.extend(["-c".into(), "-H".into()]),
        _ => {}
    }
    a.push(if mode == "content" { "--json".into() } else { "--null".into() });
    let num = |k: &str| i[k].as_u64();
    if mode == "content" {
        a.push("-n".into());
        if let Some(c) = num("context").or(num("-C")) {
            a.extend(["-C".into(), c.to_string()]);
        } else {
            if let Some(b) = num("-B") {
                a.extend(["-B".into(), b.to_string()]);
            }
            if let Some(n) = num("-A") {
                a.extend(["-A".into(), n.to_string()]);
            }
        }
    }
    a.extend(["-e".into(), i["pattern"].as_str().unwrap().into()]);
    if let Some(ty) = i["type"].as_str() {
        a.extend(["--type".into(), ty.into()]);
    }
    if let Some(g) = i["glob"].as_str() {
        for g in g.split_whitespace() {
            a.extend(["--glob".into(), g.into()]);
        }
    }
    // Grep resolves the path by name before it gives it to ripgrep
    let target = i["path"].as_str().map_or(t.root.clone(), |p| real(t.root.join(p)));
    a.push(target.to_str().unwrap().into());
    let out = Command::new("rg").args(&a).current_dir(&t.root).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let rel = |p: &str| p.strip_prefix(&format!("{}{SEP}", t.root.display())).unwrap_or(p).to_string();
    match mode {
        "files_with_matches" => text.split('\0').filter(|s| !s.trim().is_empty()).map(|s| rel(s.trim_start_matches('\n'))).collect(),
        "count" => text.lines().map(|l| l.split_once('\0').map(|(p, n)| format!("{}:{n}", rel(p))).unwrap()).filter(|l| !l.ends_with(":0")).collect(),
        _ => {
            let breaks = ["context", "-C", "-B", "-A"].iter().any(|k| num(k).is_some_and(|n| n > 0));
            let dir = target.is_dir();
            let numbers = i["-n"] != false;
            let mut lines = Vec::new();
            let mut last: Option<(String, u64)> = None;
            for l in text.lines() {
                let v: Value = serde_json::from_str(l).unwrap();
                let kind = v["type"].as_str().unwrap();
                if kind != "match" && kind != "context" {
                    continue;
                }
                let d = &v["data"];
                let path = d["path"]["text"].as_str().unwrap().to_string();
                let first = d["line_number"].as_u64().unwrap();
                let body = d["lines"]["text"].as_str().unwrap();
                let body = body.strip_suffix('\n').unwrap_or(body);
                let body = body.strip_suffix('\r').unwrap_or(body);
                if breaks && last.as_ref().is_some_and(|(p, n)| *p != path || first > n + 1) {
                    lines.push("--".to_string());
                }
                let sep = if kind == "match" { ':' } else { '-' };
                let parts: Vec<&str> = body.split('\n').collect();
                for (k, part) in parts.iter().enumerate() {
                    let part = part.trim_end_matches('\r');
                    let part = if part.encode_utf16().count() > 500 {
                        if kind == "match" { "[Omitted long matching line]" } else { "[Omitted long context line]" }
                    } else {
                        part
                    };
                    let mut s = if dir { format!("{}{sep}", rel(&path)) } else { String::new() };
                    if numbers {
                        s.push_str(&format!("{}{sep}", first + k as u64));
                    }
                    s.push_str(part);
                    lines.push(s);
                }
                last = Some((path, first + parts.len() as u64 - 1));
            }
            lines
        }
    }
}

/// The hook's lines: the reason without Grep's own first and last lines.
fn hook_lines(reason: &str, mode: &str) -> Vec<String> {
    let mut lines: Vec<String> = reason.lines().map(String::from).collect();
    match mode {
        "files_with_matches" => {
            assert!(lines.remove(0).starts_with("Found "));
            lines.sort();
        }
        "count" => {
            assert!(lines.pop().unwrap().starts_with("Found "));
        }
        _ => {}
    }
    lines
}

#[test]
fn the_hook_answers_with_ripgreps_lines_for_greps_arguments() {
    if !have_rg() {
        return;
    }
    let t = Tree::new("same");
    let asks = [
        json!({"pattern": "needle", "output_mode": "content"}),
        json!({"pattern": "needle", "output_mode": "content", "-i": true}),
        json!({"pattern": "needle", "output_mode": "content", "-n": false}),
        json!({"pattern": "needle", "output_mode": "content", "-C": 1}),
        json!({"pattern": "needle", "output_mode": "content", "-A": 1, "-B": 2}),
        json!({"pattern": "needle", "output_mode": "content", "context": 0, "-A": 3}),
        json!({"pattern": "needle", "output_mode": "content", "glob": "*.rs"}),
        json!({"pattern": "needle", "output_mode": "content", "glob": "*.{rs,py}"}),
        json!({"pattern": "needle", "output_mode": "content", "glob": "!*.txt"}),
        json!({"pattern": "needle", "output_mode": "content", "glob": "**/*.txt"}),
        json!({"pattern": "needle", "output_mode": "content", "glob": "*.log"}),
        json!({"pattern": "needle", "output_mode": "content", "type": "py"}),
        json!({"pattern": "needle", "output_mode": "content", "type": "rust"}),
        json!({"pattern": "needle", "output_mode": "content", "path": "src"}),
        json!({"pattern": "needle", "output_mode": "content", "path": "src/a.rs"}),
        json!({"pattern": "needle", "output_mode": "content", "path": "./deep/../src"}),
        json!({"pattern": "start.*end", "output_mode": "content", "multiline": true}),
        json!({"pattern": "start.*end", "output_mode": "content", "multiline": true, "-C": 1}),
        json!({"pattern": "here.middle", "output_mode": "count", "multiline": true}),
        json!({"pattern": r"needle\s+\d", "output_mode": "content", "multiline": true}),
        json!({"pattern": "ne+dle\\b", "output_mode": "content"}),
        json!({"pattern": "-?needle", "output_mode": "content"}),
        json!({"pattern": "naïve", "output_mode": "content"}),
        json!({"pattern": "needle"}),
        json!({"pattern": "needle", "output_mode": "files_with_matches", "glob": "*.txt"}),
        json!({"pattern": "needle", "output_mode": "files_with_matches", "path": "src/a.rs"}),
        json!({"pattern": "needle", "output_mode": "count"}),
        json!({"pattern": "needle", "output_mode": "count", "-i": true, "type": "rust"}),
    ];
    for ask in asks {
        let mut ask = ask;
        ask["head_limit"] = json!(0);
        let input = t.input(ask.clone());
        let mode = ask["output_mode"].as_str().unwrap_or("files_with_matches");
        let mut want = grep_lines(&t, &input);
        if mode == "files_with_matches" {
            want.sort();
        }
        assert!(!want.is_empty(), "the fixture matches {ask}");
        let reason = t.reason(ask.clone()).unwrap_or_else(|| panic!("no answer for {ask}"));
        assert_eq!(hook_lines(&reason, mode), want, "for {ask}");
    }
    // an answer the bound does not cut costs the header and its line break more than Grep's
    let s = t.stats();
    let (grep, sent) = (s["total"]["grep_bytes"].as_u64().unwrap(), s["total"]["sent_bytes"].as_u64().unwrap());
    eprintln!("{} calls: Grep {grep} bytes, sent {sent}", s["total"]["calls"]);
    assert_eq!(sent, grep + 28 * (HEADER.len() as u64 + 1));
}

#[test]
fn calls_it_cannot_answer_exactly_let_grep_run() {
    let t = Tree::new("open");
    t.write("bin/nul.txt", "needle first\n\0binary after\n");
    let unanswered = [
        json!({"pattern": "needle", "output_mode": "content", "-o": true}),
        json!({"pattern": "needle", "glob": "src/*.rs"}),
        json!({"pattern": "needle", "type": "js"}),
        json!({"pattern": "needle", "path": "~/x"}),
        json!({"pattern": "needle", "path": "/"}),
        json!({"pattern": "needle", "path": "missing"}),
        json!({"pattern": "needle", "unknown": 1}),
        json!({"pattern": "needle", "head_limit": "5"}),
        json!({"pattern": "needle", "path": "bin", "output_mode": "content"}),
        json!({"pattern": "(unclosed", "output_mode": "content"}),
        json!({"pattern": "nothing-matches-this", "output_mode": "content"}),
        json!({"pattern": ""}),
    ];
    for ask in unanswered {
        assert_eq!(t.reason(ask.clone()), None, "for {ask}");
    }
    // a separator on Windows, where Grep's ripgrep matches the glob from its working directory
    if cfg!(windows) {
        assert_eq!(t.reason(json!({"pattern": "needle", "glob": "src\\*.rs"})), None);
    }
    let (code, out) =
        t.hook(&json!({"hook_event_name": "PreToolUse", "tool_name": "Read", "cwd": t.root.to_str().unwrap(), "tool_input": {"file_path": "x"}}), &[]);
    assert_eq!((code, out.as_str()), (0, ""));
    let (code, out) = t.hook(&json!("not a call"), &[]);
    assert_eq!((code, out.as_str()), (0, ""));
    // a read rule of the project's settings is Claude Code's to apply
    t.write(".claude/settings.json", r#"{"permissions": {"deny": ["Read(./.env)"]}}"#);
    assert_eq!(t.reason(json!({"pattern": "needle"})), None);
    t.write(".claude/settings.json", r#"{"permissions": {"allow": ["Bash(ls)"]}}"#);
    assert!(t.reason(json!({"pattern": "needle"})).is_some());
    assert_eq!(t.stats()["total"]["calls"], 1, "only answered calls count");
}

#[test]
fn greps_paging_and_the_byte_limit_bound_the_answer() {
    let t = Tree::new("page");
    for i in 0..300 {
        t.write(&format!("many/f{i:03}.txt"), "needle\n");
    }
    let reason = t.reason(json!({"pattern": "needle", "output_mode": "content", "path": "many"})).unwrap();
    let lines: Vec<&str> = reason.lines().collect();
    assert_eq!(lines.len(), 251, "Grep's 250 lines, then its paging line");
    assert_eq!(lines[0], format!("many{SEP}f000.txt:1:needle"));
    assert_eq!(lines[250], "[Showing results with pagination = limit: 250]");
    let second = t.reason(json!({"pattern": "needle", "output_mode": "content", "path": "many", "offset": 250})).unwrap();
    assert_eq!(second.lines().next(), Some(format!("many{SEP}f250.txt:1:needle").as_str()));
    assert!(second.ends_with("[Showing results with pagination = offset: 250]"), "{second}");

    let (code, out) =
        t.hook(&t.input(json!({"pattern": "needle", "output_mode": "content", "path": "many", "head_limit": 0})), &[("SCRUTATE_MAX_BYTES", "1000")]);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(&out).unwrap();
    let r = v["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
    assert!(r.len() <= 1000, "{} bytes", r.len());
    let last = r.lines().last().unwrap();
    assert!(last.starts_with('(') && last.contains("more lines not shown: call Grep again with offset="), "{last}");
    let shown = r.lines().count() - 2;
    assert!(last.contains(&format!("offset={shown},")), "{last}");
}

#[test]
fn the_counter_moves_by_the_measured_bytes() {
    let t = Tree::new("count");
    let reason = t.reason(json!({"pattern": "needle", "output_mode": "content"})).unwrap();
    let s = t.stats();
    assert_eq!(s["total"]["calls"], 1);
    // an answer that was not cut is Grep's result after the header
    assert_eq!(s["total"]["grep_bytes"].as_u64().unwrap() as usize, reason.len());
    assert_eq!(s["total"]["sent_bytes"].as_u64().unwrap() as usize, HEADER.len() + 1 + reason.len());
    assert_eq!(s["sessions"][0]["session"], "test-session");
    for i in 0..400 {
        t.write(&format!("many/f{i:03}.txt"), &format!("needle {}\n", "y".repeat(100)));
    }
    let (_, out) = t.hook(&t.input(json!({"pattern": "needle", "output_mode": "content", "path": "many", "head_limit": 0})), &[("SCRUTATE_MAX_BYTES", "2000")]);
    assert!(!out.is_empty());
    let s2 = t.stats();
    let grep = s2["sessions"][0]["grep_bytes"].as_u64().unwrap() - s["total"]["grep_bytes"].as_u64().unwrap();
    let sent = s2["sessions"][0]["sent_bytes"].as_u64().unwrap() - s["total"]["sent_bytes"].as_u64().unwrap();
    assert_eq!(grep, (0..400).map(|i| format!("many/f{i:03}.txt:1:needle {}", "y".repeat(100)).len() as u64 + 1).sum::<u64>() - 1);
    assert!(sent <= 2000 && sent > 1000, "{sent}");
    assert!(s2["total"]["saved_bytes"].as_i64().unwrap() > 0);
    let state = t.base.join("home").join("state");
    let text =
        Command::new(BIN).args(["hook", "stats"]).env("HOME", t.base.join("home")).env("XDG_STATE_HOME", &state).env("LOCALAPPDATA", &state).output().unwrap();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.starts_with("session test-session: 2 calls"), "{text}");
    assert!(text.contains("all sessions: 2 calls"), "{text}");
}

#[test]
fn the_ripgrep_types_the_hook_takes_have_ripgreps_files() {
    if !have_rg() {
        return;
    }
    let out = Command::new("rg").arg("--type-list").output().unwrap();
    let list = String::from_utf8(out.stdout).unwrap();
    let rg = |name: &str| -> Option<Vec<String>> {
        list.lines().find_map(|l| l.strip_prefix(&format!("{name}: "))).map(|g| {
            let mut v: Vec<String> = g.split(", ").map(String::from).collect();
            v.sort();
            v
        })
    };
    for name in scrutate::hook::SAME_TYPES {
        let mut ours = scrutate::instant::type_globs(name).unwrap();
        ours.sort();
        assert_eq!(Some(ours), rg(name), "type {name}");
    }
}
