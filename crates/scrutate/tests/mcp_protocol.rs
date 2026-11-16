//! The MCP server's JSON-RPC surface on the real binary: what gets an answer, what does not, and
//! what the `serve` flags change.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[path = "support/tag.rs"]
mod tag;

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str, files: usize) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_mcpp_{}_{}", name, std::process::id()));
        tag::claim(&root);
        let _ = fs::remove_dir_all(&root);
        // a repository: no index in a directory above takes it over
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("pkg")).unwrap();
        for i in 0..files {
            let body = format!("import os\n\n\ndef make_widget_{i}(size):\n    return os.path.join(\"w\", str(size))\n");
            fs::write(root.join(format!("pkg/m{i}.py")), body).unwrap();
        }
        Repo { root }
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn spawn(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Child {
    let mut c = Command::new(BIN);
    c.arg("--root").arg(&r.root).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.spawn().unwrap()
}

/// Send raw lines, close stdin, and return every message the server wrote, and its exit status.
fn exchange(r: &Repo, args: &[&str], lines: &[&[u8]]) -> (Vec<Value>, bool) {
    let mut child = spawn(r, args, &[]);
    let mut stdin = child.stdin.take().unwrap();
    for l in lines {
        stdin.write_all(l).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let msgs = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    (msgs, out.status.success())
}

/// A server driven one request at a time.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    fn start(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Session {
        let mut child = spawn(r, args, env);
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        Session { child, stdin, stdout }
    }
    /// `start`, with the server's debug lines: the session's and the watch's.
    fn start_said(r: &Repo, args: &[&str]) -> (Session, std::sync::mpsc::Receiver<String>) {
        let mut c = Command::new(BIN);
        c.arg("--root").arg(&r.root).args(args).env("SCRUTATE_SESSION_DEBUG", "1").env("SCRUTATE_WATCH_DEBUG", "1");
        let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let err = child.stderr.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        (Session { child, stdin, stdout }, rx)
    }
    fn ask(&mut self, msg: Value) -> Value {
        writeln!(self.stdin, "{msg}").unwrap();
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == msg["id"] {
                return v;
            }
        }
    }
    /// The text of a tool call's answer, after the index is built.
    fn text(&mut self, id: i64, name: &str, args: Value) -> String {
        loop {
            let v = self.ask(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}}));
            let text = v["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{v}")).to_string();
            if !text.contains("still building its index") {
                return text;
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn an_invalid_utf8_line_is_a_parse_error_and_the_server_keeps_serving() {
    let r = Repo::new("utf8", 2);
    let (msgs, ok) =
        exchange(&r, &["serve"], &[b"{\"jsonrpc\":\"2.0\",\"id\":301,\"method\":\"pi\xff\xfeng\"}", br#"{"jsonrpc":"2.0","id":302,"method":"ping"}"#]);
    assert!(ok);
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert_eq!(msgs[0]["error"]["code"], -32700);
    assert_eq!(msgs[0]["id"], Value::Null);
    assert_eq!(msgs[1]["id"], 302);
    assert_eq!(msgs[1]["result"], json!({}));
}

#[test]
fn a_client_that_disconnects_during_the_first_build_leaves_a_complete_index() {
    let r = Repo::new("eof", 600);
    let init = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    let (msgs, ok) = exchange(&r, &["serve"], &[init]);
    assert!(ok);
    assert_eq!(msgs[0]["id"], 1);
    let db = rusqlite::Connection::open_with_flags(r.root.join(".scrutate/index.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let files: i64 = db.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0)).unwrap();
    assert_eq!(files, 600);
}

// the planted id overflows, which panics, only where overflow checks are on
#[cfg(debug_assertions)]
#[test]
fn a_first_build_that_panics_is_reported_and_the_server_still_answers_and_exits() {
    let r = Repo::new("build_panic", 1);
    assert!(Command::new(BIN).arg("--root").arg(&r.root).args(["index", "--no-text"]).status().unwrap().success());
    let db = rusqlite::Connection::open(r.root.join(".scrutate/index.db")).unwrap();
    db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('next_id',?1)", [i64::MAX.to_string()]).unwrap();
    drop(db);
    fs::write(r.root.join("pkg/new.py"), "def fresh_fn():\n    return 1\n").unwrap();
    let mut c = Command::new(BIN);
    c.arg("--root").arg(&r.root).arg("serve").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let call = |id: i64, meta: Value| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "find_symbol", "arguments": {"name": "make_widget_0"}, "_meta": meta}});
    for m in [call(1, json!({})), call(2, json!({"progressToken": "t"}))] {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let t0 = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() && t0.elapsed().as_secs() < 60 {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let exited = child.try_wait().unwrap().is_some();
    if !exited {
        let _ = child.kill();
    }
    let out = child.wait_with_output().unwrap();
    assert!(exited, "serve did not exit when its input closed");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("indexing failed"), "{err}");
    let msgs: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    for id in [1, 2] {
        let m = msgs.iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no answer to {id}: {msgs:?}"));
        let text = m["result"]["content"][0]["text"].as_str().unwrap();
        assert!(m["result"]["isError"] == true && text.contains("index could not be built"), "{m}");
    }
}

#[test]
fn notifications_get_no_answer_and_batches_get_an_array() {
    let r = Repo::new("batch", 2);
    let lines: &[&[u8]] = &[
        br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"make_widget_1"}}}"#,
        br#"{"jsonrpc":"2.0","method":"ping"}"#,
        b"42",
        br#""x""#,
        b"null",
        b"[]",
        br#"[{"jsonrpc":"2.0","id":11,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"make_widget_1"}}},7]"#,
        br#"[{"jsonrpc":"2.0","method":"notifications/initialized"}]"#,
        br#"{"jsonrpc":"2.0","id":13,"method":"ping"}"#,
    ];
    let (msgs, ok) = exchange(&r, &["serve"], lines);
    assert!(ok);
    let invalid: Vec<&Value> = msgs.iter().filter(|m| m["error"]["code"] == -32600).collect();
    assert_eq!(invalid.len(), 4, "{msgs:?}");
    assert!(invalid.iter().all(|m| m["id"] == Value::Null));
    let batches: Vec<&Value> = msgs.iter().filter(|m| m.is_array()).collect();
    assert_eq!(batches.len(), 1, "{msgs:?}");
    let batch = batches[0].as_array().unwrap();
    assert_eq!(batch.len(), 3, "{batch:?}");
    let by_id = |id: Value| batch.iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no {id} in {batch:?}"));
    assert_eq!(by_id(json!(11))["result"], json!({}));
    assert!(by_id(json!(12))["result"]["content"][0]["text"].is_string());
    assert_eq!(by_id(Value::Null)["error"]["code"], -32600);
    assert!(msgs.iter().any(|m| m["id"] == 13));
    assert_eq!(msgs.len(), 4 + 1 + 1, "{msgs:?}");
}

#[test]
fn a_message_without_a_method_is_a_response_or_an_invalid_request() {
    let r = Repo::new("nomethod", 1);
    let lines: &[&[u8]] = &[
        br#"{"jsonrpc":"2.0","id":81,"result":{}}"#,
        br#"{"jsonrpc":"2.0","id":82,"error":{"code":-1,"message":"no"}}"#,
        br#"{"jsonrpc":"2.0","id":83}"#,
        br#"{"jsonrpc":"2.0","id":84,"method":5}"#,
    ];
    let (msgs, ok) = exchange(&r, &["serve"], lines);
    assert!(ok);
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert_eq!(msgs[0]["id"], 83);
    assert_eq!(msgs[0]["error"]["code"], -32600);
    assert_eq!(msgs[1]["id"], 84);
    assert_eq!(msgs[1]["error"]["code"], -32600);
}

#[test]
fn an_unknown_protocol_version_is_answered_with_the_latest() {
    let r = Repo::new("version", 1);
    let init = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    let (msgs, _) = exchange(&r, &["serve"], &[init]);
    assert_eq!(msgs[0]["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn an_unknown_or_missing_tool_name_is_a_protocol_error() {
    let r = Repo::new("tool", 1);
    let lines: &[&[u8]] = &[
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}"#,
        br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"arguments":{}}}"#,
    ];
    let (msgs, _) = exchange(&r, &["serve"], lines);
    let by_id = |id: i64| msgs.iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no {id} in {msgs:?}"));
    assert_eq!(by_id(1)["error"]["code"], -32602);
    assert_eq!(by_id(1)["error"]["message"], "unknown tool: no_such_tool");
    assert_eq!(by_id(2)["error"]["code"], -32602);
    assert_eq!(by_id(2)["error"]["message"], "tools/call has no params.name");
}

#[test]
fn max_bytes_given_to_serve_is_the_default_answer_size() {
    let r = Repo::new("maxbytes", 200);
    let grep = || json!({"pattern": "import os", "max_results": 500});
    let mut wide = Session::start(&r, &["serve"], &[]);
    assert!(wide.text(1, "grep_code", grep()).len() > 5_000);
    for (args, env) in [(&["--max-bytes", "1000", "serve"][..], &[][..]), (&["serve"][..], &[("SCRUTATE_MAX_BYTES", "1000")][..])] {
        let mut s = Session::start(&r, args, env);
        let len = s.text(1, "grep_code", grep()).len();
        assert!(len < 1_200, "{len} bytes under --max-bytes 1000");
        let mut args = grep();
        args["max_bytes"] = json!(5_000);
        assert!(s.text(2, "grep_code", args).len() > 4_000, "a call's own max_bytes still wins");
        let tools = s.ask(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}));
        let find = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "find_symbol").unwrap().clone();
        assert_eq!(find["inputSchema"]["properties"]["max_bytes"]["default"], 1000);
    }
}

/// After a new directory: Windows waits for the watch's burst over it, whose walk lets a call name
/// the directory's files (a call before it holds them back); elsewhere 300 ms. The lines read.
fn after_the_burst(said: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
    if !cfg!(windows) {
        std::thread::sleep(std::time::Duration::from_millis(300));
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    while !lines.iter().any(|l| l.contains("full walk: yes")) {
        lines.push(said.recv_timeout(std::time::Duration::from_secs(30)).expect("the watch's burst over the new directory"));
    }
    lines
}

/// A call's debug lines say it walked every file and no interval held that walk back.
fn walked(lines: &[String]) -> bool {
    lines.iter().any(|l| l.contains("session: every file")) && !lines.iter().any(|l| l.contains("the refresh interval holds that walk back"))
}

#[test]
fn an_explicit_refresh_interval_is_a_floor() {
    let r = Repo::new("refresh", 2);
    let find = |n: &str| json!({"name": n});
    // indexed before the server starts, so its first call answers before the watches are set
    let indexed = Command::new(BIN).arg("--root").arg(&r.root).arg("index").env("SCRUTATE_NO_SESSION", "1").output().unwrap();
    assert!(indexed.status.success(), "{}", String::from_utf8_lossy(&indexed.stderr));
    let (mut floor, said) = Session::start_said(&r, &["serve", "--refresh-interval", "3600"]);
    let mut id = 1;
    assert!(floor.text(id, "find_symbol", find("make_widget_1")).contains("pkg/m1.py"));
    // start-up done: the watch (on macOS the kqueue) is in place and a read applied the walk that
    // follows it, so a plain call looks at no file
    let t0 = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        while said.try_recv().is_ok() {}
        id += 1;
        assert!(floor.text(id, "find_symbol", find("make_widget_1")).contains("pkg/m1.py"));
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !std::iter::from_fn(|| said.try_recv().ok()).any(|l| l.contains("every file")) {
            break;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(30), "start-up never ended");
    }
    while said.try_recv().is_ok() {}
    fs::create_dir_all(r.root.join("pkg/new_dir")).unwrap();
    fs::write(r.root.join("pkg/new_dir/later.py"), "def arrived_later():\n    return 1\n").unwrap();
    let mut lines = after_the_burst(&said);
    id += 1;
    let seen = floor.text(id, "find_symbol", find("arrived_later")).contains("pkg/new_dir/later.py");
    std::thread::sleep(std::time::Duration::from_millis(100));
    lines.extend(std::iter::from_fn(|| said.try_recv().ok()));
    // Linux: the new directory has no inotify watch, so its file has no event, and the walk that
    // lists it waits for the floor. macOS: the kqueue sees the directory change and asks for that
    // walk too. Windows: the subtree watch has an event for the file, and the watch's walk over the
    // new directory lets the call name it: one stat, no walk.
    assert_eq!(seen, cfg!(windows), "seen on Windows alone; the call said {lines:?}");
    assert!(!walked(&lines), "a walk of every file within the interval: {lines:?}");
    assert!(lines.iter().any(|l| l.contains("the refresh interval holds that walk back")), "{lines:?}");
    // an edit of a file the watch knows, and a new file in a directory it knows: no interval
    // holds them back
    let m1 = r.root.join("pkg/m1.py");
    fs::write(&m1, format!("{}\n\ndef edited_in_place():\n    return 1\n", fs::read_to_string(&m1).unwrap())).unwrap();
    assert!(floor.text(id + 1, "find_symbol", find("edited_in_place")).contains("pkg/m1.py"), "an edit within the interval");
    fs::write(r.root.join("pkg/late.py"), "def arrived_late():\n    return 1\n").unwrap();
    assert!(floor.text(id + 2, "find_symbol", find("arrived_late")).contains("pkg/late.py"), "a named file within the interval");
}

/// New directories made straight after start-up, on an index built before the server (its
/// first call answers before the watches are set): start-up's one walk, then the floor holds.
#[test]
fn new_directories_right_after_start_up_cost_one_walk_under_a_refresh_interval() {
    let r = Repo::new("refresh_burst", 0);
    for i in 0..40 {
        let d = r.root.join(format!("d{i}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(format!("m{i}.py")), format!("def base_{i}():\n    return {i}\n")).unwrap();
    }
    let indexed = Command::new(BIN).arg("--root").arg(&r.root).arg("index").env("SCRUTATE_NO_SESSION", "1").output().unwrap();
    assert!(indexed.status.success(), "{}", String::from_utf8_lossy(&indexed.stderr));
    let (mut floor, said) = Session::start_said(&r, &["serve", "--refresh-interval", "3600"]);
    assert!(floor.text(1, "find_symbol", json!({"name": "base_1"})).contains("d1/m1.py"));
    // no read and no burst until the directories come, so start-up's walk is still owed then
    // (waiting for the watch by a burst of its own made the old rule pass)
    std::thread::sleep(std::time::Duration::from_secs(2));
    let (mut seen, mut walks, mut early, mut told) = (0, 0, 0, Vec::new());
    for k in 0..5 {
        while said.try_recv().is_ok() {}
        let d = r.root.join(format!("d0/new_{k}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("later.py"), format!("def later_{k}():\n    return {k}\n")).unwrap();
        let mut lines = after_the_burst(&said);
        seen += usize::from(floor.text(2 + k, "find_symbol", json!({"name": format!("later_{k}")})).contains(&format!("d0/new_{k}/later.py")));
        std::thread::sleep(std::time::Duration::from_millis(100));
        lines.extend(std::iter::from_fn(|| said.try_recv().ok()));
        // a call before the watch (on macOS the kqueue) is in place walks; the next pays
        // start-up's walk
        early += usize::from(lines.iter().any(|l| l.contains("the watch is not in place") || l.contains("FSEvents may number")));
        walks += usize::from(walked(&lines));
        told.push(format!("new_{k}: {lines:?}"));
    }
    // Windows: each call names the new directory's file, as in
    // an_explicit_refresh_interval_is_a_floor. Linux and macOS: start-up's one walk sees one, the
    // rest wait.
    if cfg!(windows) {
        assert_eq!(seen, 5, "the calls said {told:#?}");
    } else {
        assert!(seen <= early + 1, "{seen} of 5 new directories seen under the floor, {early} before the watch; the calls said {told:#?}");
    }
    assert!(walks <= early + 1, "{walks} walks of every file for 5 new directories, {early} before the watch: one is start-up's; the calls said {told:#?}");
}

/// Under `--refresh-interval` an edit the watch saw is seen at the next call when a directory was
/// made before that call: the walk the new directory asks for is held back, the edit is not.
#[test]
fn an_edit_followed_by_a_new_directory_is_seen_under_a_refresh_interval() {
    let r = Repo::new("refresh_edit_mkdir", 0);
    for i in 0..40 {
        let d = r.root.join(format!("d{i}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(format!("m{i}.py")), format!("def base_{i}():\n    return {i}\n")).unwrap();
    }
    let indexed = Command::new(BIN).arg("--root").arg(&r.root).arg("index").env("SCRUTATE_NO_SESSION", "1").output().unwrap();
    assert!(indexed.status.success(), "{}", String::from_utf8_lossy(&indexed.stderr));
    let (mut floor, said) = Session::start_said(&r, &["serve", "--refresh-interval", "3600"]);
    let find = |n: &str| json!({"name": n});
    let mut id = 1;
    assert!(floor.text(id, "find_symbol", find("base_1")).contains("d1/m1.py"));
    // start-up done, as in an_explicit_refresh_interval_is_a_floor
    let t0 = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        while said.try_recv().is_ok() {}
        id += 1;
        assert!(floor.text(id, "find_symbol", find("base_1")).contains("d1/m1.py"));
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !std::iter::from_fn(|| said.try_recv().ok()).any(|l| l.contains("every file")) {
            break;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(30), "start-up never ended");
    }
    for k in 0..3 {
        while said.try_recv().is_ok() {}
        let m = r.root.join(format!("d{k}/m{k}.py"));
        fs::write(&m, format!("{}\n\ndef edited_{k}():\n    return {k}\n", fs::read_to_string(&m).unwrap())).unwrap();
        let d = r.root.join(format!("new_{k}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("n.py"), format!("def made_{k}():\n    return {k}\n")).unwrap();
        // the watch's burst over the edit and the new directory sets its watches again first
        let mut lines: Vec<String> = Vec::new();
        while !lines.iter().any(|l| l.contains("full walk: yes")) {
            lines.push(said.recv_timeout(std::time::Duration::from_secs(30)).expect("the watch's burst over the new directory"));
        }
        id += 1;
        let seen = floor.text(id, "find_symbol", find(&format!("edited_{k}"))).contains(&format!("d{k}/m{k}.py"));
        std::thread::sleep(std::time::Duration::from_millis(100));
        lines.extend(std::iter::from_fn(|| said.try_recv().ok()));
        assert!(seen, "edit {k} within the interval, after a new directory; the call said {lines:?}");
        assert!(!walked(&lines), "a walk of every file within the interval: {lines:?}");
    }
}

#[test]
fn a_walk_the_watch_asks_for_comes_at_once_by_default() {
    let r = Repo::new("refresh_default", 2);
    let find = |n: &str| json!({"name": n});
    let mut default = Session::start(&r, &["serve"], &[]);
    assert!(default.text(1, "find_symbol", find("make_widget_1")).contains("pkg/m1.py"));
    fs::create_dir_all(r.root.join("pkg/new_dir")).unwrap();
    fs::write(r.root.join("pkg/new_dir/later.py"), "def arrived_later():\n    return 1\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(default.text(2, "find_symbol", find("arrived_later")).contains("pkg/new_dir/later.py"), "a walk the watch asks for comes at once by default");
}

/// Under `--refresh-interval` an edit of a file the watch knows and a new file in a directory it
/// knows are seen at once, on a tree of many directories: the interval holds back a walk of every
/// file, never the files the watch named, nor the one look that follows the watches coming up.
#[test]
fn an_edit_and_a_new_file_are_seen_at_once_under_a_refresh_interval_on_a_tree_of_many_directories() {
    let r = Repo::new("floor_many", 0);
    for i in 0..40 {
        let d = r.root.join(format!("d{i}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(format!("m{i}.py")), format!("def base_{i}():\n    return {i}\n")).unwrap();
    }
    // indexed before the server starts, so its first call answers before the watches are set
    let indexed = Command::new(BIN).arg("--root").arg(&r.root).arg("index").env("SCRUTATE_NO_SESSION", "1").output().unwrap();
    assert!(indexed.status.success(), "{}", String::from_utf8_lossy(&indexed.stderr));
    let mut floor = Session::start(&r, &["serve", "--refresh-interval", "3600"], &[]);
    assert!(floor.text(1, "find_symbol", json!({"name": "base_1"})).contains("d1/m1.py"));
    // the watches in place by now, the walk that follows them due
    std::thread::sleep(std::time::Duration::from_secs(1));
    let mut id = 1;
    let mut seen = |floor: &mut Session, name: &str, file: &str| {
        id += 1;
        floor.text(id, "find_symbol", json!({"name": name})).contains(file)
    };
    let (mut edits, mut new_files) = (0, 0);
    for k in 0..10 {
        let f = r.root.join(format!("d{k}/m{k}.py"));
        let body = format!("{}\n\ndef edited_{k}():\n    return {k}\n", fs::read_to_string(&f).unwrap());
        fs::write(&f, body).unwrap();
        edits += usize::from(seen(&mut floor, &format!("edited_{k}"), &format!("d{k}/m{k}.py")));
        fs::write(r.root.join(format!("d{k}/late_{k}.py")), format!("def late_{k}():\n    return {k}\n")).unwrap();
        new_files += usize::from(seen(&mut floor, &format!("late_{k}"), &format!("d{k}/late_{k}.py")));
    }
    assert_eq!((edits, new_files), (10, 10), "(edits, new files in a watched directory) seen at once, of 10 each");
}

#[test]
fn a_new_file_an_argument_names_is_indexed_within_the_refresh_interval() {
    let r = Repo::new("named_new", 2);
    fs::write(r.root.join(".ignore"), "skipped.py\n").unwrap();
    let mut floor = Session::start(&r, &["serve", "--refresh-interval", "3600"], &[]);
    assert!(floor.text(1, "find_symbol", json!({"name": "make_widget_1"})).contains("pkg/m1.py"));
    fs::write(r.root.join("pkg/late.py"), "def arrived_late():\n    return 1\n").unwrap();
    fs::write(r.root.join("skipped.py"), "def ignored_fn():\n    return 1\n").unwrap();
    let outline = floor.text(2, "outline", json!({"path": "pkg/late.py"}));
    assert!(outline.contains("arrived_late"), "{outline}");
    assert!(floor.text(3, "find_symbol", json!({"name": "arrived_late"})).contains("pkg/late.py"), "the file stays indexed");
    // a file a scan would not index is not indexed for naming it either
    let outline = floor.text(4, "outline", json!({"path": "skipped.py"}));
    assert!(outline.contains("not indexed"), "{outline}");
}

#[test]
fn serve_refuses_an_offset_it_would_not_use() {
    let r = Repo::new("offset", 3);
    let out = spawn(&r, &["--offset", "2", "serve"], &[]).wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// `schema` without the `description` of anything in it.
fn undescribed(mut schema: Value) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(m) => {
                if m.get("description").is_some_and(Value::is_string) {
                    m.remove("description");
                }
                m.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut schema);
    schema
}

#[test]
fn the_short_tool_list_keeps_every_tool_argument_type_default_and_check() {
    let r = Repo::new("short", 1);
    let list =
        |args: &[&str], env: &[(&str, &str)]| Session::start(&r, args, env).ask(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))["result"].clone();
    let (full, short) = (list(&["serve"], &[]), list(&["serve", "--short-tools"], &[]));
    assert_eq!(short, list(&["serve"], &[("SCRUTATE_MCP_SHORT_TOOLS", "1")]));
    assert_eq!(full, list(&["serve"], &[("SCRUTATE_MCP_SHORT_TOOLS", "0")]));
    let (full_tools, short_tools) = (full["tools"].as_array().unwrap(), short["tools"].as_array().unwrap());
    assert_eq!(full_tools.len(), 19);
    for (f, s) in full_tools.iter().zip(short_tools) {
        assert_eq!(f["name"], s["name"]);
        // the arguments every tool shares are named once, in the instructions
        let mut own = undescribed(f["inputSchema"].clone());
        for shared in ["response_format", "max_bytes", "offset"] {
            own["properties"].as_object_mut().unwrap().remove(shared);
        }
        assert_eq!(own, s["inputSchema"], "{}", f["name"]);
        assert_eq!(f["annotations"]["readOnlyHint"], s["annotations"]["readOnlyHint"]);
        assert_eq!(f["annotations"]["openWorldHint"], s["annotations"]["openWorldHint"]);
        assert_eq!(f["inputSchema"]["properties"]["response_format"]["default"], "detailed");
        assert!(s["description"].as_str().unwrap().len() < f["description"].as_str().unwrap().len(), "{}", f["name"]);
    }
    let bytes = |v: &Value| v.to_string().len();
    assert!(bytes(&short) * 2 <= bytes(&full), "{} bytes short, {} detailed", bytes(&short), bytes(&full));
    let hello = |args: &[&str]| {
        Session::start(&r, args, &[]).ask(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}))["result"]["instructions"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let told = hello(&["serve", "--short-tools"]);
    assert!(told.contains("response_format") && told.contains("max_bytes") && told.contains("offset"), "{told}");
    assert!(!hello(&["serve"]).contains("max_bytes (an integer"));
    // the same calls fail with the same errors
    let calls = [
        ("find_symbol", json!({"name": 7})),
        ("search_code", json!({})),
        ("grep_code", json!({"pattern": "x", "mode": "glob"})),
        ("read_code", json!({})),
        ("callers", json!({"name": "x", "limit": -1})),
        ("find_symbol", json!({"name": "x", "response_format": "short"})),
        ("no_such_tool", json!({})),
    ];
    let mut answers = Vec::new();
    for args in [&["serve"][..], &["serve", "--short-tools"][..]] {
        let mut s = Session::start(&r, args, &[]);
        let ask = |(i, (name, a)): (usize, &(&str, Value))| {
            s.ask(json!({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": name, "arguments": a}}))
        };
        answers.push(calls.iter().enumerate().map(ask).collect::<Vec<_>>());
    }
    assert_eq!(answers[0], answers[1]);
    for a in &answers[0][..6] {
        assert_eq!(a["result"]["isError"], true, "{a}");
    }
    assert_eq!(answers[0][5]["result"]["content"][0]["text"], "error: response_format must be \"detailed\" or \"concise\"");
    assert_eq!(answers[0][6]["error"]["code"], -32602);
}

impl Session {
    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
    }
    fn next(&mut self) -> Value {
        let mut line = String::new();
        assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the server closed stdout");
        serde_json::from_str(&line).unwrap()
    }
}

fn with_token(id: i64, name: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args, "_meta": {"progressToken": format!("t{id}")}}})
}

/// The progress of a call: each report counts more files than the one before, against a total,
/// and none comes after the answer.
#[test]
fn a_long_call_with_a_progress_token_reports_files_read_until_its_answer() {
    let r = Repo::new("progress", 4000);
    let mut s = Session::start(&r, &["serve"], &[]);
    s.text(1, "find_symbol", json!({"name": "make_widget_7"}));
    for (id, name, args) in [
        (2, "reindex", json!({"force": true})),
        (3, "structural_search", json!({"lang": "python", "query": "(function_definition name: (identifier) @f)", "limit": 100000, "max_bytes": 1000})),
    ] {
        s.send(with_token(id, name, args));
        let mut before = 0;
        let answer = loop {
            let m = s.next();
            if m["id"] == id {
                break m;
            }
            assert_eq!((&m["method"], &m["params"]["progressToken"]), (&json!("notifications/progress"), &json!(format!("t{id}"))), "{m}");
            let (done, total) = (m["params"]["progress"].as_u64().unwrap(), m["params"]["total"].as_u64().unwrap());
            assert!(done > before && done <= total && total >= 4000, "{m}");
            before = done;
        };
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        if name == "reindex" {
            assert!(before > 0, "a forced reindex of 4,000 files reports at least once");
        }
        // nothing more for that call: the next message answers the next request
        s.send(json!({"jsonrpc": "2.0", "id": 90 + id, "method": "ping"}));
        assert_eq!(s.next()["id"], 90 + id);
    }
}

#[test]
fn a_call_cancelled_after_its_first_progress_report_stops_and_sends_nothing_more() {
    let r = Repo::new("progress_cancel", 4000);
    let mut s = Session::start(&r, &["serve"], &[]);
    s.text(1, "find_symbol", json!({"name": "make_widget_7"}));
    s.send(with_token(2, "reindex", json!({"force": true})));
    let first = s.next();
    assert_eq!(first["method"], "notifications/progress", "{first}");
    let (done, total) = (first["params"]["progress"].as_u64().unwrap(), first["params"]["total"].as_u64().unwrap());
    assert!(done < total, "cancelled mid-way: {first}");
    s.send(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 2}}));
    // a request after the notice is answered; the cancelled one never is
    s.send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "find_symbol", "arguments": {"name": "make_widget_3999"}}}));
    s.send(json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}));
    let mut answered = Vec::new();
    while answered.len() < 2 {
        let m = s.next();
        assert_ne!(m["id"], 2, "a cancelled call is not answered: {m}");
        match m["id"].as_i64() {
            Some(3) => assert!(m["result"]["content"][0]["text"].as_str().unwrap().starts_with("pkg/m3999.py:4-5"), "{m}"),
            Some(4) => {}
            _ => {
                assert_eq!(m["method"], "notifications/progress", "{m}");
                continue;
            }
        }
        answered.push(m);
    }
}
