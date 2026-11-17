//! `scrutate lsp` end to end: the real binary over stdio with Content-Length framing, one request of
//! each kind, UTF-16 columns on a non-ASCII line, and a save picked up by the background update.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/tag.rs"]
mod tag;

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    /// A repository: no index in a directory above takes it over.
    fn new(name: &str) -> Repo {
        let r = Repo::plain(name);
        fs::create_dir_all(r.root.join(".git")).unwrap();
        r
    }

    /// A directory outside any repository, for a test that lays out its own.
    fn plain(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_lsp_{}_{}", name, std::process::id()));
        tag::claim(&root);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Repo { root }
    }
    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') { format!("file://{s}") } else { format!("file:///{s}") }
}

struct Client {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    notifications: Vec<Value>,
}

impl Client {
    fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }
    fn read(&mut self) -> Value {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed its output");
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some(v) = l.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read();
            if m["id"] == id {
                return m;
            }
            self.notifications.push(m);
        }
    }
    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

fn utf16(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

/// (file name, line, character) of each location.
fn places(v: &Value) -> Vec<(String, u64, u64)> {
    v.as_array()
        .unwrap_or_else(|| panic!("not a location list: {v}"))
        .iter()
        .map(|l| {
            let name = l["uri"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
            (name, l["range"]["start"]["line"].as_u64().unwrap(), l["range"]["start"]["character"].as_u64().unwrap())
        })
        .collect()
}

#[test]
fn lsp_round_trip() {
    let r = Repo::new("rt");
    r.write("src/geo.py", "class Point:\n    def norm(self):\n        return 1\n\n\ndef make_point():\n    return Point()\n");
    r.write("src/other.py", "def norm(x):\n    return x\n");
    let use_line = "    label = \"héllo 🙂 ünïcode\"; p = make_point()";
    r.write("src/app.py", &format!("from src.geo import make_point, Point\n\n\ndef main():\n{use_line}\n    return Point.norm(p)\n"));

    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };

    let init = c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    let caps = &init["result"]["capabilities"];
    assert_eq!(caps["definitionProvider"], true, "{init}");
    assert_eq!(caps["referencesProvider"], true);
    assert_eq!(caps["workspaceSymbolProvider"], true);
    assert_eq!(caps["documentSymbolProvider"], true);
    assert!(caps.get("hoverProvider").is_none(), "only implemented capabilities are advertised");
    assert_eq!(init["result"]["serverInfo"]["name"], "scrutate");
    c.notify("initialized", json!({}));

    let app = uri(&r.root.join("src/app.py"));
    let app_text = fs::read_to_string(r.root.join("src/app.py")).unwrap();
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": app, "languageId": "python", "version": 1, "text": app_text}}));

    // cursor in the middle of `make_point` on the non-ASCII line: the column is in UTF-16 units
    let col = utf16(&use_line[..use_line.find("make_point").unwrap()]);
    assert_ne!(col as usize, use_line.find("make_point").unwrap(), "the line must have multi-unit characters");
    let def = c.request("textDocument/definition", json!({"textDocument": {"uri": app}, "position": {"line": 4, "character": col + 3}}));
    assert_eq!(places(&def["result"]), vec![("geo.py".to_string(), 5, 4)], "{def}");
    assert!(
        c.notifications.iter().any(|n| n["method"] == "window/logMessage" && n["params"]["message"].as_str().unwrap().contains("building")),
        "{:?}",
        c.notifications
    );

    // qualified name wins over the other `norm`
    let line5 = "    return Point.norm(p)";
    let def = c.request(
        "textDocument/definition",
        json!({"textDocument": {"uri": app}, "position": {"line": 5, "character": utf16(&line5[..line5.find("norm").unwrap()])}}),
    );
    assert_eq!(places(&def["result"]), vec![("geo.py".to_string(), 1, 8)], "{def}");

    let refs = c.request(
        "textDocument/references",
        json!({"textDocument": {"uri": app}, "position": {"line": 4, "character": col}, "context": {"includeDeclaration": true}}),
    );
    let got = places(&refs["result"]);
    for want in [("geo.py", 5, 4), ("app.py", 0, utf16("from src.geo import ")), ("app.py", 4, col)] {
        assert!(got.contains(&(want.0.to_string(), want.1, want.2)), "missing {want:?} in {got:?}");
    }
    assert_eq!(got.len(), 3, "{got:?}");
    let refs = c.request(
        "textDocument/references",
        json!({"textDocument": {"uri": app}, "position": {"line": 4, "character": col}, "context": {"includeDeclaration": false}}),
    );
    assert!(!places(&refs["result"]).contains(&("geo.py".to_string(), 5, 4)), "{refs}");

    // references of the method under the cursor, not of the unrelated `norm` in other.py
    let refs = c.request(
        "textDocument/references",
        json!({"textDocument": {"uri": app}, "position": {"line": 5, "character": utf16(&line5[..line5.find("norm").unwrap()])}, "context": {"includeDeclaration": true}}),
    );
    let got = places(&refs["result"]);
    assert_eq!(got, vec![("geo.py".to_string(), 1, 8), ("app.py".to_string(), 5, utf16(&line5[..line5.find("norm").unwrap()]))], "{refs}");

    let ws = c.request("workspace/symbol", json!({"query": "make_po"}));
    let syms = ws["result"].as_array().unwrap();
    assert_eq!(syms[0]["name"], "make_point", "{ws}");
    assert_eq!(syms[0]["kind"], 12);
    assert!(syms[0]["location"]["uri"].as_str().unwrap().ends_with("/src/geo.py"));

    let outline = c.request("textDocument/documentSymbol", json!({"textDocument": {"uri": uri(&r.root.join("src/geo.py"))}}));
    let top = outline["result"].as_array().unwrap_or_else(|| panic!("{outline}"));
    let names: Vec<&str> = top.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Point", "make_point"], "{outline}");
    assert_eq!(top[0]["kind"], 5);
    assert_eq!(top[0]["children"][0]["name"], "norm", "methods nest under their class: {outline}");
    assert_eq!(top[0]["children"][0]["selectionRange"]["start"], json!({"line": 1, "character": 8}));

    // a save is indexed in the background; requests keep answering meanwhile
    r.write("src/other.py", "def norm(x):\n    return x\n\n\ndef freshly_saved():\n    return 2\n");
    c.notify("textDocument/didSave", json!({"textDocument": {"uri": uri(&r.root.join("src/other.py"))}}));
    let t0 = Instant::now();
    loop {
        let ws = c.request("workspace/symbol", json!({"query": "freshly_saved"}));
        if ws["result"].as_array().is_some_and(|a| a.iter().any(|s| s["name"] == "freshly_saved")) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "save never reached the index: {ws}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let unknown = c.request("textDocument/hover", json!({}));
    assert_eq!(unknown["error"]["code"], -32601);
    let bye = c.request("shutdown", Value::Null);
    assert!(bye.get("error").is_none(), "{bye}");
    c.notify("exit", Value::Null);
    drop(c);
    assert_eq!(child.wait().unwrap().code(), Some(0), "exit after shutdown is a clean exit");
}

#[test]
fn workspace_symbols_rank_the_name_then_prefixes_then_camel_case_humps_then_the_rest() {
    let r = Repo::new("ranked");
    r.write(
        "src/svc.py",
        "class MyBkSvc:\n    pass\n\n\nclass BookingsService:\n    pass\n\n\ndef bk_svc_config():\n    return 1\n\n\nclass ReportService:\n    pass\n",
    );
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let ws = c.request("workspace/symbol", json!({"query": "BkSvc"}));
    let names: Vec<&str> = ws["result"].as_array().unwrap_or_else(|| panic!("{ws}")).iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["bk_svc_config", "BookingsService", "MyBkSvc"], "{ws}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn workspace_symbols_list_each_spelling_only_under_its_own_match() {
    let r = Repo::new("spellings");
    r.write("src/a.py", "def foobar():\n    return 1\n");
    r.write("src/b.py", "class FooBar:\n    pass\n");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let ws = c.request("workspace/symbol", json!({"query": "FB"}));
    let names: Vec<&str> = ws["result"].as_array().unwrap_or_else(|| panic!("{ws}")).iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["FooBar"], "{ws}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

/// A folder the editor names through a link follows the link to the tree it names now: a link
/// pointed at another tree, a link replaced by a directory, a link to a link pointed elsewhere,
/// and a link that dangles for a moment first. A second folder stays as it is, and a document
/// named by the old tree's own path is answered from nothing.
#[cfg(unix)]
#[test]
fn a_folder_through_a_link_follows_the_link_to_its_new_tree() {
    use std::os::unix::fs::symlink;
    for case in ["retargeted", "replaced_by_a_directory", "link_to_a_link", "dangling_first"] {
        let r = Repo::plain(&format!("follow_{case}"));
        let v1 = "def foo_ll():\n    return 1\n\n\ndef only_v1():\n    return 4\n";
        let v2 = "def foo_ll():\n    return 2\n\n\ndef only_v2():\n    return 3\n\n\ndef uses_v2():\n    return only_v2()\n";
        for (dir, body) in [("rel/v1", v1), ("rel/v2", v2), ("other", "def in_other():\n    return 5\n")] {
            r.write(&format!("{dir}/a.py"), body);
            fs::create_dir_all(r.root.join(dir).join(".git")).unwrap();
        }
        let cur = r.root.join("cur");
        if case == "link_to_a_link" {
            symlink("rel/v1", r.root.join("mid")).unwrap();
            symlink("mid", &cur).unwrap();
        } else {
            symlink("rel/v1", &cur).unwrap();
        }
        let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
        let folders = json!([{"uri": uri(&cur), "name": "cur"}, {"uri": uri(&r.root.join("other")), "name": "other"}]);
        c.request("initialize", json!({"processId": null, "workspaceFolders": folders, "capabilities": {}}));
        c.notify("initialized", json!({}));
        let names = |c: &mut Client, q: &str| -> Option<usize> {
            let ws = c.request("workspace/symbol", json!({"query": q}));
            ws["result"].as_array().map(|a| a.iter().filter(|s| s["name"] == q).count())
        };
        let t0 = Instant::now();
        while names(&mut c, "only_v1").is_none_or(|n| n == 0) || names(&mut c, "in_other").is_none_or(|n| n == 0) {
            assert!(t0.elapsed() < Duration::from_secs(60), "{case}: the first builds never answered");
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(names(&mut c, "only_v2"), Some(0), "{case}: v1 has no only_v2");
        let old_doc = canon(r.root.join("rel/v1")).join("a.py");
        match case {
            "retargeted" => {
                symlink("rel/v2", r.root.join("cur.tmp")).unwrap();
                fs::rename(r.root.join("cur.tmp"), &cur).unwrap();
            }
            "replaced_by_a_directory" => {
                fs::remove_file(&cur).unwrap();
                fs::rename(r.root.join("rel/v2"), &cur).unwrap();
            }
            "link_to_a_link" => {
                symlink("rel/v2", r.root.join("mid.tmp")).unwrap();
                fs::rename(r.root.join("mid.tmp"), r.root.join("mid")).unwrap();
            }
            _ => {
                fs::remove_file(&cur).unwrap();
                std::thread::sleep(Duration::from_millis(1000));
                symlink("rel/v2", &cur).unwrap();
            }
        }
        let t0 = Instant::now();
        while names(&mut c, "only_v2").is_none_or(|n| n == 0) {
            assert!(t0.elapsed() < Duration::from_secs(20), "{case}: only_v2 of the new tree never found");
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(names(&mut c, "only_v1"), Some(0), "{case}: only_v1 of the old tree still found");
        assert_eq!(names(&mut c, "in_other"), Some(1), "{case}: the other folder stopped answering");
        // a document through the folder answers from the new tree, definition and references
        let doc = cur.join("a.py");
        c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri(&doc), "languageId": "python", "version": 1, "text": v2}}));
        let at = json!({"textDocument": {"uri": uri(&doc)}, "position": {"line": 9, "character": 12}});
        let def = c.request("textDocument/definition", at.clone());
        assert!(def["result"].to_string().contains("a.py") && def["result"].to_string().contains("\"line\":4"), "{case}: {def}");
        let refs = c.request(
            "textDocument/references",
            json!({"textDocument": at["textDocument"], "position": at["position"], "context": {"includeDeclaration": true}}),
        );
        assert_eq!(refs["result"].as_array().map(Vec::len), Some(2), "{case}: {refs}");
        // the old tree by its own path belongs to no folder now
        let def = c.request("textDocument/definition", json!({"textDocument": {"uri": uri(&old_doc)}, "position": {"line": 4, "character": 5}}));
        assert!(def["result"].is_null() || def["result"].as_array().is_some_and(Vec::is_empty), "{case}: {def}");
        c.request("shutdown", Value::Null);
        c.notify("exit", Value::Null);
        drop(c);
        child.wait().unwrap();
    }
}

/// The file a location names, from the repository top, after checking that it exists.
fn real_file(top: &Path, loc: &Value) -> String {
    let p = scrutate::lsp::uri_to_path(loc["uri"].as_str().unwrap()).unwrap();
    assert!(p.is_file(), "{p:?} does not exist: {loc}");
    let (p, top) = (canon(&p), canon(top));
    p.strip_prefix(&top).unwrap().to_string_lossy().replace('\\', "/")
}

/// Answers name files under the folder as the editor sent it, also when that folder is reached
/// through a link: the editor has the file open under that name.
#[cfg(unix)]
#[test]
fn answers_name_files_under_the_folder_as_the_editor_sent_it() {
    let r = Repo::new("linked_folder");
    r.write("real/proj/.git/HEAD", "ref: refs/heads/main\n");
    r.write("real/proj/lib.py", "def handler():\n    return 1\n");
    r.write("real/proj/app.py", "from lib import handler\n\n\ndef run():\n    return handler()\n");
    std::os::unix::fs::symlink(r.root.join("real"), r.root.join("code")).unwrap();
    let folder = r.root.join("code/proj");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&folder).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    let given = uri(&folder);
    c.request("initialize", json!({"processId": null, "rootUri": given, "workspaceFolders": [{"uri": given, "name": "proj"}], "capabilities": {}}));
    c.notify("initialized", json!({}));
    let at = json!({"textDocument": {"uri": uri(&folder.join("app.py"))}, "position": {"line": 4, "character": 12}});
    let def = c.request("textDocument/definition", at.clone());
    let mut refs_at = at;
    refs_at["context"] = json!({"includeDeclaration": true});
    let refs = c.request("textDocument/references", refs_at);
    let ws = c.request("workspace/symbol", json!({"query": "handler"}));
    let mut uris: Vec<String> = Vec::new();
    for v in [&def["result"], &refs["result"]] {
        uris.extend(v.as_array().unwrap_or_else(|| panic!("{v}")).iter().map(|l| l["uri"].as_str().unwrap().to_string()));
    }
    uris.extend(ws["result"].as_array().unwrap_or_else(|| panic!("{ws}")).iter().map(|s| s["location"]["uri"].as_str().unwrap().to_string()));
    assert!(uris.len() >= 3, "{def} {refs} {ws}");
    for u in &uris {
        assert!(u.starts_with(&format!("{given}/")), "{u} is not under {given}");
    }
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    let _ = child.wait();
}

/// `$/cancelRequest` ends the request it names: the request is answered RequestCancelled at once,
/// and the requests behind it are not kept waiting.
#[test]
fn a_cancelled_request_is_answered_at_once_and_the_next_is_not_kept_waiting() {
    let r = Repo::new("cancel");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    // a first build that takes a while: a request waits for it
    for i in 0..3000 {
        r.write(&format!("m{i}.py"), &format!("def f{i}(a):\n    return a + {i}\n"));
    }
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let at = json!({"textDocument": {"uri": uri(&r.root.join("m1.py"))}, "position": {"line": 0, "character": 5}});
    let t = std::time::Instant::now();
    c.send(json!({"jsonrpc": "2.0", "id": 900, "method": "textDocument/definition", "params": at}));
    c.notify("$/cancelRequest", json!({"id": 900}));
    let reply = loop {
        let m = c.read();
        if m["id"] == 900 {
            break m;
        }
    };
    assert_eq!(reply["error"]["code"], -32800, "{reply}");
    assert!(t.elapsed() < Duration::from_secs(5), "the cancel was not seen at once: {:?}", t.elapsed());
    // one cancelled while it waits its turn or runs is answered the same way
    c.send(json!({"jsonrpc": "2.0", "id": 901, "method": "workspace/symbol", "params": {"query": "f1"}}));
    c.notify("$/cancelRequest", json!({"id": 901}));
    let reply = loop {
        let m = c.read();
        if m["id"] == 901 {
            break m;
        }
    };
    assert_eq!(reply["error"]["code"], -32800, "{reply}");
    let t = std::time::Instant::now();
    let bye = c.request("shutdown", Value::Null);
    assert!(bye.get("error").is_none() && t.elapsed() < Duration::from_secs(5), "{bye} after {:?}", t.elapsed());
    c.notify("exit", Value::Null);
    drop(c);
    assert_eq!(child.wait().unwrap().code(), Some(0));
}

/// A folder that cannot answer (deleted, or its first build still running) leaves the other
/// folders' answers: `workspace/symbol` answers from the ones that can.
#[test]
fn a_folder_that_cannot_answer_leaves_the_others_answers() {
    let r = Repo::new("multi_root_gone");
    fs::create_dir_all(r.root.join("x/.git")).unwrap();
    r.write("x/lib.py", "def handler():\n    return 'x'\n");
    let gone = r.root.join("gone");
    let folder = |p: &Path| json!({"uri": uri(p), "name": p.file_name().unwrap().to_string_lossy()});
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request(
        "initialize",
        json!({"processId": null, "rootUri": uri(&r.root.join("x")), "workspaceFolders": [folder(&r.root.join("x")), folder(&gone)], "capabilities": {}}),
    );
    c.notify("initialized", json!({}));
    let symbols = |c: &mut Client, when: &str| {
        let ws = c.request("workspace/symbol", json!({"query": "handler"}));
        let got: Vec<String> = ws["result"].as_array().unwrap_or_else(|| panic!("{when}: {ws}")).iter().map(|s| real_file(&r.root, &s["location"])).collect();
        assert_eq!(got, ["x/lib.py"], "{when}: {ws}");
    };
    symbols(&mut c, "with a deleted folder");
    c.notify("workspace/didChangeWorkspaceFolders", json!({"event": {"added": [folder(&r.root.join("also_gone"))], "removed": []}}));
    symbols(&mut c, "with a deleted folder added later");
    // a folder whose first build cannot end while the request is read: the test holds its
    // index's write lock, which the build waits for
    let big = r.root.join("big");
    fs::create_dir_all(big.join(".git")).unwrap();
    r.write("big/m.py", "def f(a):\n    return a\n");
    assert!(Command::new(BIN).arg("index").current_dir(&big).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().success());
    let lock = rusqlite::Connection::open(big.join(".scrutate/index.db")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    c.notify("workspace/didChangeWorkspaceFolders", json!({"event": {"added": [folder(&big)], "removed": []}}));
    let t = std::time::Instant::now();
    symbols(&mut c, "with a folder in its first build");
    // set aside, not waited for: a request that waits for the build waits out all of BUILD_WAIT
    let left_out = "big is left out of this answer: the index is still being built";
    assert!(
        c.notifications.iter().any(|n| n["method"] == "window/logMessage" && n["params"]["message"].as_str().is_some_and(|m| m.ends_with(left_out))),
        "{:?}",
        c.notifications
    );
    assert!(t.elapsed() < scrutate::session::BUILD_WAIT, "the build was waited for: {:?}", t.elapsed());
    lock.execute_batch("ROLLBACK").unwrap();
    // a folder with no index at all, asked at once: its build may or may not end first, so it is
    // left out or answers, but is never waited for
    let fresh = r.root.join("fresh");
    fs::create_dir_all(fresh.join(".git")).unwrap();
    for i in 0..3000 {
        r.write(&format!("fresh/m{i}.py"), &format!("def g{i}(a):\n    return a + {i}\n"));
    }
    c.notify("workspace/didChangeWorkspaceFolders", json!({"event": {"added": [folder(&fresh)], "removed": []}}));
    let t = std::time::Instant::now();
    symbols(&mut c, "with a folder that has no index yet");
    assert!(t.elapsed() < scrutate::session::BUILD_WAIT, "the build was waited for: {:?}", t.elapsed());
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    let _ = child.wait();
}

/// A workspace of several repositories: each folder is answered from its own index, a folder
/// added later is answered, a removed one is not, and a file in no folder gets no answer.
#[test]
fn each_workspace_folder_is_answered_from_its_own_repository() {
    let r = Repo::new("multi_root");
    for name in ["a", "b", "c"] {
        fs::create_dir_all(r.root.join(name).join(".git")).unwrap();
        r.write(&format!("{name}/lib.py"), &format!("def handler():\n    return '{name}'\n"));
        r.write(&format!("{name}/app.py"), "from lib import handler\n\n\ndef run():\n    return handler()\n");
    }
    let folder = |name: &str| json!({"uri": uri(&r.root.join(name)), "name": name});
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    let init = c.request(
        "initialize",
        json!({"processId": null, "rootUri": uri(&r.root.join("a")), "workspaceFolders": [folder("a"), folder("b")], "capabilities": {}}),
    );
    c.notify("initialized", json!({}));
    let at = |name: &str| json!({"textDocument": {"uri": uri(&r.root.join(name).join("app.py"))}, "position": {"line": 4, "character": 12}, "context": {"includeDeclaration": true}});
    // the files a request names, from the workspace's directory
    let files = |v: &Value| -> Vec<String> {
        let mut f: Vec<String> = v["result"].as_array().unwrap_or_else(|| panic!("{v}")).iter().map(|l| real_file(&r.root, l)).collect();
        f.sort();
        f.dedup();
        f
    };
    for name in ["a", "b"] {
        assert_eq!(files(&c.request("textDocument/definition", at(name))), [format!("{name}/lib.py")], "definition in {name}");
        assert_eq!(files(&c.request("textDocument/references", at(name))), [format!("{name}/app.py"), format!("{name}/lib.py")], "references in {name}");
    }
    assert_eq!(init["result"]["capabilities"]["workspace"]["workspaceFolders"], json!({"supported": true, "changeNotifications": true}), "{init}");
    // c is in no folder yet
    assert!(c.request("textDocument/definition", at("c"))["result"].is_null());
    c.notify("workspace/didChangeWorkspaceFolders", json!({"event": {"added": [folder("c")], "removed": [folder("b")]}}));
    assert_eq!(files(&c.request("textDocument/definition", at("c"))), ["c/lib.py"], "a folder added later is answered");
    assert!(c.request("textDocument/definition", at("b"))["result"].is_null(), "a removed folder is not answered");
    let ws = c.request("workspace/symbol", json!({"query": "handler"}));
    let mut names: Vec<String> = ws["result"].as_array().unwrap_or_else(|| panic!("{ws}")).iter().map(|s| real_file(&r.root, &s["location"])).collect();
    names.sort();
    assert_eq!(names, ["a/lib.py", "c/lib.py"], "{ws}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    let _ = child.wait();
}

#[test]
fn an_editor_folder_below_the_top_is_answered_from_the_index_at_the_top() {
    let r = Repo::new("subfolder");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("lib/util.py", "def lib_fn():\n    return 1\n");
    let main = "from lib.util import lib_fn\n\n\ndef app_fn():\n    return lib_fn()\n";
    r.write("pkg/app/main.py", main);
    let other = Repo::new("subfolder_other");
    other.write("x.py", "def other_fn():\n    return 2\n");
    let sub = r.root.join("pkg/app");

    let mut child = Command::new(BIN).arg("lsp").current_dir(&sub).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    // each folder is answered from its own repository's index
    let folders = json!([{"uri": uri(&sub), "name": "app"}, {"uri": uri(&other.root), "name": "other"}]);
    c.request("initialize", json!({"processId": null, "rootUri": uri(&sub), "workspaceFolders": folders, "capabilities": {}}));
    c.notify("initialized", json!({}));

    // the editor's text, two lines down from the disk's: the open document is found by its path from the top
    let file = uri(&sub.join("main.py"));
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": file, "languageId": "python", "version": 1, "text": format!("\n\n{main}")}}));
    let at = json!({"textDocument": {"uri": file}, "position": {"line": 6, "character": 13}});
    let def = c.request("textDocument/definition", at.clone());
    let got: Vec<String> = def["result"].as_array().unwrap_or_else(|| panic!("{def}")).iter().map(|l| real_file(&r.root, l)).collect();
    assert_eq!(got, ["lib/util.py"], "{def}");

    let mut refs_at = at.clone();
    refs_at["context"] = json!({"includeDeclaration": true});
    let refs = c.request("textDocument/references", refs_at);
    let mut got: Vec<String> = refs["result"].as_array().unwrap_or_else(|| panic!("{refs}")).iter().map(|l| real_file(&r.root, l)).collect();
    got.dedup();
    assert_eq!(got, ["lib/util.py", "pkg/app/main.py"], "{refs}");

    for (q, want) in [("lib_fn", "lib/util.py"), ("app_fn", "pkg/app/main.py")] {
        let ws = c.request("workspace/symbol", json!({"query": q}));
        let syms = ws["result"].as_array().unwrap_or_else(|| panic!("{ws}"));
        assert_eq!(syms.len(), 1, "{ws}");
        assert_eq!(real_file(&r.root, &syms[0]["location"]), want, "{ws}");
    }
    // the second folder, another repository, is answered from its own index once its first
    // build is done (until then the other folders answer without it)
    let t0 = std::time::Instant::now();
    let ws = loop {
        let ws = c.request("workspace/symbol", json!({"query": "other_fn"}));
        if ws["result"].as_array().is_some_and(|a| !a.is_empty()) || t0.elapsed() > Duration::from_secs(60) {
            break ws;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let syms = ws["result"].as_array().unwrap_or_else(|| panic!("{ws}"));
    assert_eq!(syms.iter().map(|s| real_file(&other.root, &s["location"])).collect::<Vec<_>>(), ["x.py"], "{ws}");

    let outline = c.request("textDocument/documentSymbol", json!({"textDocument": {"uri": file}}));
    let names: Vec<&str> = outline["result"].as_array().unwrap_or_else(|| panic!("{outline}")).iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"app_fn"), "{outline}");

    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
    let mut log = String::new();
    BufReader::new(stderr).read_to_string(&mut log).unwrap();
    assert_eq!(log.matches("using the index in").count(), 1, "the root in use, said once: {log}");
    assert!(!sub.join(".scrutate").exists(), "no index in the editor's folder");
    assert!(r.root.join(".scrutate").is_dir(), "the index is at the top");
    assert!(other.root.join(".scrutate").is_dir(), "the second folder has an index of its own");
}

#[test]
fn no_first_index_of_the_home_directory_when_the_editor_names_no_folder() {
    let h = Repo::new("home");
    h.write("Documents/notes.py", "def stray_fn():\n    return 1\n");
    let mut child = Command::new(BIN)
        .arg("lsp")
        .current_dir(&h.root)
        .env(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, &h.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "capabilities": {}}));
    c.notify("initialized", json!({}));
    let ws = c.request("workspace/symbol", json!({"query": "stray_fn"}));
    assert!(ws["error"]["message"].as_str().is_some_and(|m| m.contains("not available")), "{ws}");
    assert!(
        c.notifications.iter().any(|n| n["params"]["message"].as_str().is_some_and(|m| m.contains("--root") && m.contains("scrutate index"))),
        "{:?}",
        c.notifications
    );
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
    assert!(!h.root.join(".scrutate").exists(), "no index built in the home directory");
}

#[test]
fn an_index_a_newer_scrutate_took_over_is_an_error_for_every_request() {
    let r = Repo::new("newer");
    r.write("app.py", "def user():\n    return 1\n\ndef get():\n    return user()\n");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let pos = json!({"textDocument": {"uri": uri(&r.root.join("app.py"))}, "position": {"line": 4, "character": 12}});
    let def = c.request("textDocument/definition", pos.clone());
    assert_eq!(places(&def["result"]).len(), 1, "{def}");
    // what a newer scrutate leaves behind: a schema this server cannot read
    let db = rusqlite::Connection::open(r.root.join(".scrutate/index.db")).unwrap();
    db.execute("UPDATE meta SET value = CAST(value AS INTEGER) + 1 WHERE key = 'schema_version'", []).unwrap();
    let schema: String = db.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| row.get(0)).unwrap();
    for method in ["textDocument/definition", "textDocument/references"] {
        let mut p = pos.clone();
        p["context"] = json!({"includeDeclaration": true});
        let answer = c.request(method, p);
        let message = answer["error"]["message"].as_str().unwrap_or_default().to_string();
        assert!(message.contains("newer scrutate"), "{method}: {answer}");
    }
    // a save must not rebuild it either
    r.write("app.py", "def user():\n    return 2\n\ndef get():\n    return user()\n");
    c.notify("textDocument/didSave", json!({"textDocument": {"uri": uri(&r.root.join("app.py"))}}));
    let again = c.request("workspace/symbol", json!({"query": "user"}));
    assert!(again["error"]["message"].as_str().unwrap_or_default().contains("newer scrutate"), "{again}");
    let now: String = db.query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| row.get(0)).unwrap();
    assert_eq!(now, schema);
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn definition_takes_the_name_with_dashes_arrows_and_colons_around_the_cursor() {
    let r = Repo::new("names");
    r.write("core.clj", "(ns app.core)\n\n(defn get-user [id]\n  {:id id})\n\n(defn run []\n  (get-user 1))\n");
    r.write("util.el", "(defun my-trim (s)\n  (string-trim s))\n\n(defun use-it (s)\n  (my-trim s))\n");
    r.write("conv.scm", "(define (list->vec l)\n  (list->vector l))\n\n(define (use l)\n  (list->vec l))\n");
    r.write("mod.rb", "class Foo::Bar\n  def call\n    1\n  end\nend\n\ndef use\n  Foo::Bar.new.call\nend\n");
    r.write("app.py", "def user():\n    return 1\n\ndef get():\n    return 2\n");

    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    for (file, line, ch, want) in [
        ("core.clj", 6, 4, ("core.clj", 2)),
        ("core.clj", 6, 8, ("core.clj", 2)),
        ("util.el", 4, 7, ("util.el", 0)),
        ("conv.scm", 4, 5, ("conv.scm", 0)),
        ("mod.rb", 7, 8, ("mod.rb", 0)),
        ("mod.rb", 7, 16, ("mod.rb", 1)),
    ] {
        let def = c.request("textDocument/definition", json!({"textDocument": {"uri": uri(&r.root.join(file))}, "position": {"line": line, "character": ch}}));
        let got: Vec<(String, u64)> = places(&def["result"]).into_iter().map(|(f, l, _)| (f, l)).collect();
        assert_eq!(got, [(want.0.to_string(), want.1)], "{file}:{line}:{ch}: {def}");
    }
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn definition_and_references_name_the_same_method_of_a_variable_s_class() {
    let r = Repo::new("same_def");
    let shop = "class Order:\n    def total(self):\n        return 1\n\n\nclass Item:\n    def total(self):\n        return 2\n\n\ndef main():\n    item = Order()\n    return item.total()\n";
    r.write("shop.py", shop);
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let pos = json!({"textDocument": {"uri": uri(&r.root.join("shop.py"))}, "position": {"line": 12, "character": 16}});
    let def = c.request("textDocument/definition", pos.clone());
    assert_eq!(places(&def["result"]), [("shop.py".to_string(), 1, 8)], "{def}");
    let mut refs = pos.clone();
    refs["context"] = json!({"includeDeclaration": true});
    let refs = c.request("textDocument/references", refs);
    assert_eq!(places(&refs["result"]).first(), Some(&("shop.py".to_string(), 1, 8)), "{refs}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_unix_file_name_stays_in_the_uri() {
    let r = Repo::new("backslash");
    r.write("a\\b.py", "def slash_fn():\n    return 1\n\n\ndef use():\n    return slash_fn()\n");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let file = format!("{}/a%5Cb.py", uri(&r.root));
    let def = c.request("textDocument/definition", json!({"textDocument": {"uri": file}, "position": {"line": 5, "character": 12}}));
    let uris: Vec<&str> = def["result"].as_array().unwrap_or_else(|| panic!("{def}")).iter().map(|l| l["uri"].as_str().unwrap()).collect();
    assert_eq!(uris, [file.as_str()], "{def}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn references_are_placed_right_after_a_byte_order_mark_or_an_invalid_byte() {
    let r = Repo::new("columns");
    r.write("helper.py", "def go():\n    return 1\n");
    fs::write(r.root.join("bom.py"), b"\xef\xbb\xbffrom helper import go\ngo()\n").unwrap();
    fs::write(r.root.join("latin.py"), b"from helper import go\nx = \"caf\xe9 au lait\"; go()\n").unwrap();

    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let all = [
        ("bom.py".to_string(), 0, 19),
        ("bom.py".to_string(), 1, 0),
        ("helper.py".to_string(), 0, 4),
        ("latin.py".to_string(), 0, 19),
        ("latin.py".to_string(), 1, 20),
    ];
    let ask = |c: &mut Client, file: &str, line: u64, character: u64| {
        let pos = json!({"textDocument": {"uri": uri(&r.root.join(file))}, "position": {"line": line, "character": character}, "context": {"includeDeclaration": true}});
        let found = c.request("textDocument/references", pos);
        let mut got = places(&found["result"]);
        got.sort();
        got
    };
    // an editor counts characters without the mark: `go` on line 1 of the file that has one
    assert_eq!(ask(&mut c, "bom.py", 0, 20), all);
    assert_eq!(ask(&mut c, "latin.py", 1, 21), all);
    // the same once the editor has sent the text, which has no mark
    c.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri(&r.root.join("bom.py")), "languageId": "python", "version": 1, "text": "from helper import go\ngo()\n"}}),
    );
    assert_eq!(ask(&mut c, "bom.py", 0, 20), all);
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn a_document_that_is_not_under_the_root_is_never_read_from_disk() {
    let r = Repo::plain("confined");
    r.write("repo/a.py", "def secret_word():\n    return 1\n");
    r.write("outside/secret.py", "secret_word\n");
    let root = r.root.join("repo");

    let mut child = Command::new(BIN).arg("lsp").current_dir(&root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    // a name that is no URI, a URI outside the root, and one that leaves it through a missing folder
    let outside = uri(&r.root.join("outside/secret.py"));
    let through = format!("{}/missing/../../outside/secret.py", uri(&root));
    for doc in ["../outside/secret.py", outside.as_str(), through.as_str()] {
        let pos = json!({"textDocument": {"uri": doc}, "position": {"line": 0, "character": 2}});
        let def = c.request("textDocument/definition", pos.clone());
        assert!(def["result"].is_null() || def["result"].as_array().is_some_and(|a| a.is_empty()), "definition for {doc}: {def}");
        let mut p = pos;
        p["context"] = json!({"includeDeclaration": true});
        let refs = c.request("textDocument/references", p);
        assert!(refs["result"].is_null() || refs["result"].as_array().is_some_and(|a| a.is_empty()), "references for {doc}: {refs}");
    }
    // a link inside the root that leads outside is not followed: a file behind it answers as a
    // file that is not there
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside", root.join("link")).unwrap();
        let ask = |c: &mut Client, name: &str| {
            let doc = format!("{}/link/{name}", uri(&root));
            c.request("textDocument/definition", json!({"textDocument": {"uri": doc}, "position": {"line": 0, "character": 2}}))["result"].clone()
        };
        let there = ask(&mut c, "secret.py");
        assert!(there.is_null() || there.as_array().is_some_and(|a| a.is_empty()), "{there}");
        assert_eq!(there, ask(&mut c, "nothere.py"));
    }
    // a document in no folder gets no answer, also once the editor has sent its text
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": outside, "languageId": "python", "version": 1, "text": "secret_word\n"}}));
    let def = c.request("textDocument/definition", json!({"textDocument": {"uri": outside}, "position": {"line": 0, "character": 2}}));
    assert!(def["result"].is_null(), "{def}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn a_destructor_an_operator_and_a_raw_identifier_are_one_name() {
    let r = Repo::new("cppnames");
    r.write(
        "shape.cpp",
        "struct Shape {\n    Shape();\n    ~Shape();\n    bool operator==(const Shape& o) const;\n    int area() const;\n};\n\nShape::Shape() {}\n\nShape::~Shape() {}\n\nbool Shape::operator==(const Shape& o) const { return true; }\n\nint Shape::area() const { return 1; }\n\nint use(Shape& a, Shape& b) {\n    a.~Shape();\n    if (a.operator==(b)) { return a.area(); }\n    return a == b;\n}\n\nint bits() { return 1; }\n\nint flip() { return ~bits(); }\n",
    );
    r.write("raw.rs", "pub fn r#type() -> u32 {\n    1\n}\n\npub fn caller() -> u32 {\n    r#type() + 1\n}\n");

    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    // (file, line, text the cursor sits one character into, definition lines)
    let cases: &[(&str, u64, &str, &[u64])] = &[
        // the C/C++ rules take a declaration to the one definition with a body
        ("shape.cpp", 16, "Shape", &[9]),
        ("shape.cpp", 17, "operator", &[11]),
        // `~` before a name the index has no destructor for is the bitwise operator
        ("shape.cpp", 23, "bits", &[21]),
        ("raw.rs", 5, "r#type", &[0]),
    ];
    for &(file, line, needle, want) in cases {
        let text = fs::read_to_string(r.root.join(file)).unwrap();
        let src = text.lines().nth(line as usize).unwrap();
        let ch = utf16(&src[..src.find(needle).unwrap()]) + 1;
        let def = c.request("textDocument/definition", json!({"textDocument": {"uri": uri(&r.root.join(file))}, "position": {"line": line, "character": ch}}));
        let mut got: Vec<u64> = places(&def["result"]).into_iter().map(|(_, l, _)| l).collect();
        got.sort();
        assert_eq!(got, want, "definition {file}:{line} {needle:?}: {def}");
    }
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn a_name_ending_in_a_question_mark_bang_or_prime_is_one_word_after_a_receiver() {
    let r = Repo::new("suffix");
    r.write(
        "a.rb",
        "class Rec\n  def valid\n    @valid\n  end\n\n  def valid?\n    true\n  end\n\n  def save\n    1\n  end\n\n  def save!\n    2\n  end\n\n  def run(o)\n    valid?\n    o.valid?()\n    self.save!\n    return unless valid?\n    o.save!(1) if o.valid?\n  end\nend\n",
    );
    r.write("a.ex", "defmodule M do\n  def ok(x), do: x\n  def ok?(x), do: x\n  def run(x), do: ok?(x)\n  def go(x), do: M.ok?(x)\nend\n");
    r.write(
        "a.jl",
        "function update(x)\n    return x\nend\n\nfunction update!(x)\n    return x\nend\n\nfunction run(x)\n    update!(x)\n    return Main.update!(x)\nend\n",
    );
    r.write(
        "a.hs",
        "module A where\n\nstep :: Int -> Int\nstep x = x\n\nstep' :: Int -> Int\nstep' x = x + 1\n\nrun :: Int -> Int\nrun y = step' y - step y\n",
    );
    r.write("a.R", "ready <- function(x) {\n  TRUE\n}\n\nis.ready <- function(x) {\n  TRUE\n}\n\nrun <- function(x) {\n  is.ready(x)\n}\n");
    r.write("b.py", "def x():\n    return 1\n\n\ndef use(v):\n    return x-v\n");

    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    // (file, line, text the cursor sits one character into, definition line, reference lines)
    let cases: &[(&str, u64, &str, u64, &[u64])] = &[
        ("a.rb", 19, "valid?", 5, &[5, 18, 19, 21, 22]),
        ("a.rb", 20, "save!", 13, &[13, 20, 22]),
        ("a.rb", 22, "save!", 13, &[13, 20, 22]),
        ("a.ex", 4, "ok?", 2, &[2, 3, 4]),
        ("a.jl", 10, "update!", 4, &[4, 9, 10]),
        ("a.rb", 1, "valid", 1, &[1, 2]),
        // a Haskell function starts at its type signature; its definition is the equation below
        ("a.hs", 9, "step'", 6, &[5, 6, 9]),
        ("a.hs", 9, "step y", 3, &[2, 3, 9]),
        ("a.R", 9, "is.ready", 4, &[4, 9]),
        ("a.R", 9, "ready", 4, &[4, 9]),
        ("b.py", 5, "x-v", 0, &[0, 5]),
    ];
    for &(file, line, needle, want, refs) in cases {
        let text = fs::read_to_string(r.root.join(file)).unwrap();
        let src = text.lines().nth(line as usize).unwrap();
        let ch = utf16(&src[..src.find(needle).unwrap()]) + 1;
        let pos = json!({"textDocument": {"uri": uri(&r.root.join(file))}, "position": {"line": line, "character": ch}});
        let def = c.request("textDocument/definition", pos.clone());
        let got: Vec<(String, u64)> = places(&def["result"]).into_iter().map(|(f, l, _)| (f, l)).collect();
        assert_eq!(got, [(file.to_string(), want)], "definition {file}:{line} {needle:?}: {def}");
        let mut p = pos;
        p["context"] = json!({"includeDeclaration": true});
        let found = c.request("textDocument/references", p);
        let mut lines: Vec<u64> = places(&found["result"]).into_iter().map(|(_, l, _)| l).collect();
        lines.sort();
        assert_eq!(lines, refs, "references {file}:{line} {needle:?}: {found}");
    }
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn the_index_s_exclude_dirs_comes_from_its_root_not_the_server_s_directory() {
    let r = Repo::new("excludedirs");
    r.write(".git/HEAD", "ref: refs/heads/main\n");
    r.write(".scrutate.toml", "[index]\nexclude_dirs = [\"gen\"]\n");
    r.write("gen/g.py", "def g_fn():\n    return 1\n");
    r.write("src/s.py", "def s_fn():\n    return 2\n");
    r.write("elsewhere/.scrutate.toml", "[index]\nexclude_dirs = [\"src\"]\n");
    let mut child =
        Command::new(BIN).arg("lsp").current_dir(r.root.join("elsewhere")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    for (q, n) in [("g_fn", 0), ("s_fn", 1)] {
        let ws = c.request("workspace/symbol", json!({"query": q}));
        assert_eq!(ws["result"].as_array().map(Vec::len), Some(n), "{q}: {ws}");
    }
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

/// What an editor sees has no byte order mark: definition and document symbols count columns
/// without it, from the disk and from the editor's text alike; two definitions on one line are
/// siblings, each range starting where it starts.
#[test]
fn a_byte_order_mark_and_two_definitions_on_one_line_are_placed_as_the_editor_sees_them() {
    let r = Repo::new("bom_symbols");
    fs::write(r.root.join("u4.py"), b"\xef\xbb\xbfdef foo(): return foo2()\ndef foo2(): return 1\n").unwrap();
    r.write("u8.js", "function b() { return 1; } function c() { return 2; }\n");
    r.write("u9.js", "function b() {\n  return 1;\n} function c() { return 2; }\n");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let doc = |f: &str| json!({"uri": uri(&r.root.join(f))});
    for open in [false, true] {
        if open {
            c.notify("textDocument/didOpen", json!({"textDocument": {"uri": uri(&r.root.join("u4.py")), "languageId": "python", "version": 1, "text": "def foo(): return foo2()\ndef foo2(): return 1\n"}}));
        }
        let found = c.request("textDocument/definition", json!({"textDocument": doc("u4.py"), "position": {"line": 0, "character": 18}}));
        assert_eq!(places(&found["result"]), [("u4.py".to_string(), 1, 4)], "open {open}: {found}");
        let syms = c.request("textDocument/documentSymbol", json!({"textDocument": doc("u4.py")}));
        let foo = &syms["result"][0];
        assert_eq!(
            (foo["name"].as_str(), &foo["selectionRange"]["start"]["character"], &foo["selectionRange"]["end"]["character"]),
            (Some("foo"), &json!(4), &json!(7)),
            "open {open}: {syms}"
        );
    }
    let syms = c.request("textDocument/documentSymbol", json!({"textDocument": doc("u8.js")}));
    let roots: Vec<(&str, u64)> =
        syms["result"].as_array().unwrap().iter().map(|s| (s["name"].as_str().unwrap(), s["range"]["start"]["character"].as_u64().unwrap())).collect();
    // the index keeps lines, not columns: c's range starts at its name, after b
    assert_eq!(roots, [("b", 0), ("c", 36)], "{syms}");
    // one that starts on the line another ends on is its sibling too
    let syms = c.request("textDocument/documentSymbol", json!({"textDocument": doc("u9.js")}));
    let roots: Vec<(&str, u64, u64)> = syms["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s["range"]["start"]["line"].as_u64().unwrap(), s["range"]["start"]["character"].as_u64().unwrap()))
        .collect();
    assert_eq!(roots, [("b", 0, 0), ("c", 2, 11)], "{syms}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn a_reference_under_an_alias_spans_the_alias_as_written() {
    let r = Repo::new("alias_range");
    r.write("util.py", "def helper():\n    return 1\n");
    r.write("app.py", "from util import helper as h\n\n\ndef use():\n    return h()\n");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let at = json!({"textDocument": {"uri": uri(&r.root.join("util.py"))}, "position": {"line": 0, "character": 5}, "context": {"includeDeclaration": false}});
    let refs = c.request("textDocument/references", at);
    let spans: Vec<(u64, u64, u64)> = refs["result"]
        .as_array()
        .unwrap_or_else(|| panic!("{refs}"))
        .iter()
        .filter(|l| l["uri"].as_str().unwrap().ends_with("/app.py"))
        .map(|l| {
            (
                l["range"]["start"]["line"].as_u64().unwrap(),
                l["range"]["start"]["character"].as_u64().unwrap(),
                l["range"]["end"]["character"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(spans, [(0, 17, 23), (4, 11, 12)], "{refs}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn an_alias_that_starts_with_the_name_spans_the_whole_alias() {
    let r = Repo::new("alias_prefix");
    r.write("util.py", "def helper():\n    return 1\n");
    r.write("app.py", "from util import helper as helper2\n\n\ndef use():\n    return helper2()\n");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let at = json!({"textDocument": {"uri": uri(&r.root.join("util.py"))}, "position": {"line": 0, "character": 5}, "context": {"includeDeclaration": false}});
    let refs = c.request("textDocument/references", at);
    let spans: Vec<(u64, u64, u64)> = refs["result"]
        .as_array()
        .unwrap_or_else(|| panic!("{refs}"))
        .iter()
        .filter(|l| l["uri"].as_str().unwrap().ends_with("/app.py"))
        .map(|l| {
            (
                l["range"]["start"]["line"].as_u64().unwrap(),
                l["range"]["start"]["character"].as_u64().unwrap(),
                l["range"]["end"]["character"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(spans, [(0, 17, 23), (4, 11, 18)], "{refs}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn a_haskell_signature_and_its_clauses_are_one_declaration() {
    let r = Repo::new("hs_clauses");
    r.write("M.hs", "module M where\n\ndouble :: Int -> Int\ndouble 0 = 0\ndouble x = x * 2\n\nquad :: Int -> Int\nquad x = double (double x)\n");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    // from the signature and from a use: the two calls alone without the declaration, every
    // line of it with
    for (line, ch) in [(2, 1), (7, 10)] {
        for (with, want) in [(false, vec![7, 7]), (true, vec![2, 3, 4, 7, 7])] {
            let pos = json!({"textDocument": {"uri": uri(&r.root.join("M.hs"))}, "position": {"line": line, "character": ch}, "context": {"includeDeclaration": with}});
            let found = c.request("textDocument/references", pos);
            let mut lines: Vec<u64> = places(&found["result"]).into_iter().map(|(_, l, _)| l).collect();
            lines.sort();
            assert_eq!(lines, want, "from {line}:{ch}, includeDeclaration {with}: {found}");
        }
    }
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[test]
fn every_reader_of_names_takes_a_name_with_a_virama_whole() {
    // हिन्दी holds a virama (U+094D): a mark, not a letter, but part of the name (UAX #31)
    let r = Repo::new("virama");
    r.write("app.py", "def हिन्दी():\n    return 1\n\n\ndef use():\n    return हिन्दी()\n");
    let run = |args: &[&str]| -> Value {
        let out = Command::new(BIN).args(args).current_dir(&r.root).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    };
    assert!(Command::new(BIN).arg("index").current_dir(&r.root).output().unwrap().status.success());
    let callers: Vec<Value> = run(&["callers", "--json", "--", "हिन्दी"])["result"].as_array().unwrap().iter().map(|h| h["name"].clone()).collect();
    assert_eq!(callers, [json!("use")], "callers");
    let refs = run(&["refs", "--json", "--", "हिन्दी"]);
    let lines: Vec<&Value> = refs["result"]["definitions"][0]["references"].as_array().unwrap().iter().map(|x| &x["line"]).collect();
    assert_eq!(lines, [&json!(1), &json!(6)], "refs");
    let dead = run(&["dead", "--json"]);
    assert!(dead["result"]["dead"].as_array().unwrap().iter().all(|d| d["name"] != "हिन्दी"), "dead: {dead}");

    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    // the cursor on द, after the virama, in `return हिन्दी()`
    let at = json!({"textDocument": {"uri": uri(&r.root.join("app.py"))}, "position": {"line": 5, "character": utf16("    return हिन्") + 1}});
    let def = c.request("textDocument/definition", at.clone());
    let got: Vec<(String, u64)> = places(&def["result"]).into_iter().map(|(f, l, _)| (f, l)).collect();
    assert_eq!(got, [("app.py".to_string(), 0)], "definition: {def}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

/// Three modules in each of Python, TypeScript, Go and Rust, each importing the next: a function,
/// a class or type with a method, and a function that uses the next module's three. -> (relative
/// path, text) of each file, and the names of the definitions.
fn agreement_tree() -> (Vec<(String, String)>, Vec<String>) {
    const N: usize = 3;
    let mut files = vec![
        ("py/__init__.py".to_string(), String::new()),
        ("go.mod".to_string(), "module example.com/gen\n\ngo 1.22\n".to_string()),
        ("Cargo.toml".to_string(), "[package]\nname = \"gen\"\nversion = \"0.1.0\"\nedition = \"2021\"\n".to_string()),
        ("src/lib.rs".to_string(), (0..N).map(|i| format!("pub mod m{i};\n")).collect()),
    ];
    let mut names = Vec::new();
    for i in 0..N {
        // the last module imports nothing and uses its own definitions
        let (j, imports) = if i + 1 < N { (i + 1, true) } else { (i, false) };
        let py_import = if imports { format!("from py.m{j} import pyf{j}, PyC{j}\n\n\n") } else { String::new() };
        files.push((
            format!("py/m{i}.py"),
            format!(
                "{py_import}def pyf{i}(x):\n    return pyf{j}(x) + 1\n\n\nclass PyC{i}:\n    def pym{i}(self):\n        return pyf{i}(2)\n\n\ndef pyuse{i}():\n    c = PyC{j}()\n    return c.pym{j}() + pyf{j}(3)\n"
            ),
        ));
        let ts_import = if imports { format!("import {{ tsf{j}, TsC{j} }} from './m{j}';\n\n") } else { String::new() };
        files.push((
            format!("ts/m{i}.ts"),
            format!(
                "{ts_import}export function tsf{i}(x: number): number {{\n    return tsf{j}(x) + 1;\n}}\n\nexport class TsC{i} {{\n    tsm{i}(): number {{\n        return tsf{i}(2);\n    }}\n}}\n\nexport function tsuse{i}(): number {{\n    const c = new TsC{j}();\n    return c.tsm{j}() + tsf{j}(3);\n}}\n"
            ),
        ));
        let (go_import, q) = if imports { (format!("import \"example.com/gen/gen/p{j}\"\n\n"), format!("p{j}.")) } else { (String::new(), String::new()) };
        files.push((
            format!("gen/p{i}/p.go"),
            format!(
                "package p{i}\n\n{go_import}func GoF{i}(x int) int {{\n\treturn {q}GoF{j}(x) + 1\n}}\n\ntype GoT{i} struct{{}}\n\nfunc (t GoT{i}) GoM{i}() int {{\n\treturn GoF{i}(2)\n}}\n\nfunc GoUse{i}() int {{\n\tvar t {q}GoT{j}\n\treturn t.GoM{j}() + {q}GoF{j}(3)\n}}\n"
            ),
        ));
        let rs_import = if imports { format!("use crate::m{j}::{{rsf{j}, RsT{j}}};\n\n") } else { String::new() };
        files.push((
            format!("src/m{i}.rs"),
            format!(
                "{rs_import}pub fn rsf{i}(x: i32) -> i32 {{\n    rsf{j}(x) + 1\n}}\n\npub struct RsT{i};\n\nimpl RsT{i} {{\n    pub fn rsm{i}(&self) -> i32 {{\n        rsf{i}(2)\n    }}\n}}\n\npub fn rsuse{i}() -> i32 {{\n    let t = RsT{j};\n    t.rsm{j}() + rsf{j}(3)\n}}\n"
            ),
        ));
        for n in ["pyf", "PyC", "pym", "pyuse", "tsf", "TsC", "tsm", "tsuse", "GoF", "GoT", "GoM", "GoUse", "rsf", "RsT", "rsm", "rsuse"] {
            names.push(format!("{n}{i}"));
        }
    }
    (files, names)
}

/// The language server agrees with the command line on every definition of a generated tree in
/// four languages: `definition` from each use lands where `refs --def` puts the definition's name,
/// `references` from the definition is what `refs --def` resolves to it (occurrences `refs` leaves
/// unresolved have no place in LSP and are counted, not compared), `workspace/symbol` of the name
/// is `symbol`'s definitions, and `documentSymbol` of each file is its `outline`.
#[test]
fn the_language_server_agrees_with_the_command_line() {
    let r = Repo::new("agree");
    let root = canon(&r.root);
    let (files, names) = agreement_tree();
    for (rel, text) in &files {
        r.write(rel, text);
    }
    let cli = |args: &[&str]| -> Value {
        let out = Command::new(BIN)
            .args(args)
            .arg("--json")
            .current_dir(&root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stderr)))
    };
    let indexed = Command::new(BIN)
        .arg("index")
        .current_dir(&root)
        .env("SCRUTATE_SEMANTIC", "none")
        .env("SCRUTATE_NO_SESSION", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(indexed.status.success(), "{}", String::from_utf8_lossy(&indexed.stderr));
    let mut child = Command::new(BIN)
        .arg("lsp")
        .current_dir(&root)
        .env("SCRUTATE_SEMANTIC", "none")
        .env("SCRUTATE_NO_SESSION", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let prefix = format!("{}/", uri(&root));
    // (relative path, 0-based line, 0-based character) of each location in an LSP answer
    let at = |v: &Value| -> Vec<(String, u64, u64)> {
        let mut out: Vec<_> = v
            .as_array()
            .unwrap_or_else(|| panic!("not a list: {v}"))
            .iter()
            .map(|l| {
                let (u, range) = if l.get("location").is_some() { (&l["location"]["uri"], &l["location"]["range"]) } else { (&l["uri"], &l["range"]) };
                let rel = u.as_str().unwrap().strip_prefix(&prefix).unwrap_or_else(|| panic!("outside the root: {u}")).to_string();
                (rel, range["start"]["line"].as_u64().unwrap(), range["start"]["character"].as_u64().unwrap())
            })
            .collect();
        out.sort();
        out
    };
    let mut differ: Vec<String> = Vec::new();
    let (mut defs, mut uses, mut unresolved, mut siteless) = (0, 0, Vec::new(), Vec::new());
    for name in &names {
        let symbol = cli(&["symbol", name]);
        // a method's name is qualified by its type (`PyC0.pym0`)
        let found: Vec<(String, i64)> = symbol["result"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["name"].as_str().is_some_and(|n| n.rsplit('.').next() == Some(name.as_str())))
            .map(|d| (d["path"].as_str().unwrap().to_string(), d["start"].as_i64().unwrap()))
            .collect();
        if found.is_empty() {
            differ.push(format!("symbol {name}: no definition: {symbol}"));
            continue;
        }
        for (path, start) in &found {
            let refs = cli(&["refs", name, "--def", &format!("{path}:{start}")]);
            let Some(def) = refs["result"]["definitions"].as_array().and_then(|d| d.first()) else {
                differ.push(format!("refs {name} --def {path}:{start}: no definition: {refs}"));
                continue;
            };
            for u in refs["result"]["unresolved"].as_array().into_iter().flatten() {
                unresolved.push(format!(
                    "{name} {}:{} ({})",
                    u["path"].as_str().unwrap_or(""),
                    u["line"],
                    u["why"].as_str().or(u["reason"].as_str()).unwrap_or("")
                ));
            }
            let occurrences: Vec<(String, u64, u64, String)> = def["references"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| {
                    (
                        o["path"].as_str().unwrap().to_string(),
                        o["line"].as_u64().unwrap() - 1,
                        o["column"].as_u64().unwrap() - 1,
                        o["via"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect();
            // a Rust `impl` block is listed by `symbol`, but uses resolve to the type, so `refs
            // --def` gives it no occurrence, not even its own name: nothing to compare
            let Some(site) = occurrences.iter().find(|o| o.3 == "definition").map(|o| (o.0.clone(), o.1, o.2)) else {
                siteless.push(format!("{path}:{start} {}", def["kind"].as_str().unwrap_or("")));
                continue;
            };
            defs += 1;
            let mut want: Vec<(String, u64, u64)> = occurrences.iter().map(|o| (o.0.clone(), o.1, o.2)).collect();
            want.sort();
            let position = |p: &(String, u64, u64)| json!({"textDocument": {"uri": uri(&root.join(&p.0))}, "position": {"line": p.1, "character": p.2}});
            let mut params = position(&site);
            params["context"] = json!({"includeDeclaration": true});
            let got = at(&c.request("textDocument/references", params)["result"]);
            if got != want {
                differ.push(format!("references of {name} at {site:?}: lsp {got:?}, refs --def {want:?}"));
            }
            for o in occurrences.iter().filter(|o| o.3 != "definition") {
                uses += 1;
                let p = (o.0.clone(), o.1, o.2);
                let got = at(&c.request("textDocument/definition", position(&p))["result"]);
                if got != vec![site.clone()] {
                    differ.push(format!("definition of {name} from {p:?}: lsp {got:?}, refs --def {site:?}"));
                }
            }
        }
        // a definition's name is on its first line in this tree: `symbol`'s start is the line
        // workspace/symbol's location gives
        let mut want: Vec<(String, u64)> = found.iter().map(|(p, l)| (p.clone(), *l as u64 - 1)).collect();
        want.sort();
        let ws = c.request("workspace/symbol", json!({"query": name}))["result"].clone();
        let exact: Value = ws.as_array().into_iter().flatten().filter(|s| s["name"] == json!(name)).cloned().collect();
        let got: Vec<(String, u64)> = at(&exact).into_iter().map(|(p, l, _)| (p, l)).collect();
        if got != want {
            differ.push(format!("workspace/symbol {name}: lsp {got:?}, symbol {want:?}"));
        }
    }
    let mut outlined = 0;
    for (rel, _) in files.iter().filter(|(rel, _)| !rel.ends_with(".mod") && !rel.ends_with(".toml") && !rel.ends_with("__init__.py")) {
        let outline = cli(&["outline", rel]);
        let want: Vec<(String, i64)> =
            outline["result"].as_array().unwrap().iter().map(|d| (d["name"].as_str().unwrap().to_string(), d["start"].as_i64().unwrap())).collect();
        let doc = c.request("textDocument/documentSymbol", json!({"textDocument": {"uri": uri(&root.join(rel))}}))["result"].clone();
        let mut got = Vec::new();
        let mut stack: Vec<Value> = doc.as_array().cloned().unwrap_or_default().into_iter().rev().collect();
        while let Some(s) = stack.pop() {
            got.push((s["name"].as_str().unwrap().to_string(), s["range"]["start"]["line"].as_i64().unwrap() + 1));
            stack.extend(s["children"].as_array().cloned().unwrap_or_default().into_iter().rev());
        }
        outlined += want.len();
        if got != want {
            differ.push(format!("documentSymbol of {rel}: lsp {got:?}, outline {want:?}"));
        }
    }
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
    println!(
        "compared {defs} definitions of {} names, {uses} uses, {outlined} outline entries; {} definitions with no occurrence of their own: {siteless:?}; unresolved, which LSP has no place for: {unresolved:?}",
        names.len(),
        siteless.len()
    );
    assert!(differ.is_empty(), "{} differences:\n{}", differ.len(), differ.join("\n"));
    // the tree is fixed, so are the counts: a change that skips the comparisons fails here
    assert_eq!((defs, uses, outlined), (48, 75, 63), "definitions, uses and outline entries compared");
    assert_eq!(siteless, ["src/m0.rs:9 impl", "src/m1.rs:9 impl", "src/m2.rs:7 impl"], "definitions with no occurrence of their own");
}

/// The `window/logMessage` warnings a session has logged so far that say what the index leaves out.
fn left_out_logs(c: &Client) -> Vec<String> {
    c.notifications
        .iter()
        .filter(|m| m["method"] == "window/logMessage" && m["params"]["type"] == 2)
        .filter_map(|m| m["params"]["message"].as_str().map(String::from))
        .filter(|m| m.contains("not in the index"))
        .collect()
}

/// A language server whose index leaves out a file over 1 MB says so in the log, never as a
/// popup: once with its first answer, and once for that file the first time a request names it;
/// the answers themselves stay the protocol's.
#[test]
fn a_file_the_index_leaves_out_is_logged_once_for_the_session_and_once_when_named() {
    let r = Repo::new("left_out");
    r.write("small.py", "def small_marker():\n    return 1\n");
    r.write("big.py", &format!("def big_marker():\n    return 1\n\n\n{}", "x = 1  #\n".repeat(111_200)));
    let status =
        Command::new(BIN).arg("index").current_dir(&r.root).env("SCRUTATE_SEMANTIC", "none").stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(status.success());
    let mut child = Command::new(BIN)
        .arg("lsp")
        .current_dir(&r.root)
        .env("SCRUTATE_SEMANTIC", "none")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let found = c.request("workspace/symbol", json!({"query": "small_marker"}));
    assert!(found["result"].as_array().is_some_and(|a| !a.is_empty()), "{found}");
    assert_eq!(left_out_logs(&c), ["scrutate: 1 file over 1 MB is not in the index: big.py"], "{:?}", c.notifications);
    let big = json!({"textDocument": {"uri": uri(&r.root.join("big.py"))}});
    let symbols = c.request("textDocument/documentSymbol", big.clone());
    assert_eq!(symbols["result"], json!([]), "{symbols}");
    let named = "scrutate: big.py is not in the index (over 1 MB): read it by lines, or search it with grep";
    assert_eq!(left_out_logs(&c).last().map(String::as_str), Some(named), "{:?}", c.notifications);
    assert_eq!(left_out_logs(&c).len(), 2);
    // the same file again, and a name defined only in it: empty answers, no second message
    c.request("textDocument/documentSymbol", big);
    let none = c.request("workspace/symbol", json!({"query": "big_marker"}));
    assert_eq!(none["result"], json!([]), "{none}");
    assert_eq!(left_out_logs(&c).len(), 2, "{:?}", c.notifications);
    // no popup
    assert!(!c.notifications.iter().any(|m| m["method"] == "window/showMessage"), "{:?}", c.notifications);
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = child.wait();
}

/// Over a tree whose index leaves nothing out, no request logs anything about it.
#[test]
fn a_session_over_a_tree_with_nothing_left_out_logs_nothing_new() {
    let r = Repo::new("nothing_left_out");
    r.write("a.py", "def alpha():\n    return beta()\n\n\ndef beta():\n    return 1\n");
    let status =
        Command::new(BIN).arg("index").current_dir(&r.root).env("SCRUTATE_SEMANTIC", "none").stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert!(status.success());
    let mut child = Command::new(BIN)
        .arg("lsp")
        .current_dir(&r.root)
        .env("SCRUTATE_SEMANTIC", "none")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let doc = json!({"textDocument": {"uri": uri(&r.root.join("a.py"))}, "position": {"line": 1, "character": 12}});
    c.request("workspace/symbol", json!({"query": "alpha"}));
    c.request("textDocument/documentSymbol", json!({"textDocument": doc["textDocument"]}));
    c.request("textDocument/definition", doc.clone());
    c.request("textDocument/references", json!({"textDocument": doc["textDocument"], "position": doc["position"], "context": {"includeDeclaration": true}}));
    c.request("workspace/symbol", json!({"query": "zz_no_such_name"}));
    let logs: Vec<&Value> = c.notifications.iter().filter(|m| m["method"] == "window/logMessage").collect();
    assert!(logs.is_empty(), "{logs:?}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    let _ = child.wait();
}

/// One redaction switch for every agent surface: by default, and with a value that is neither
/// `off` nor `on` (`OFF`, `0`: the default stays, and stderr says so), the language server redacts
/// a secret in a symbol's name and in the warning that names a file the index leaves out;
/// `SCRUTATE_REDACT=off` gives both as they are.
#[test]
fn the_language_server_follows_the_redaction_switch_in_answers_and_logs() {
    // built here: no tracked file holds a secret-shaped literal
    let key = format!("AKIA{}", "Q7JMWB5VRT3NZ2XK");
    for (value, raw) in [(None, false), (Some("OFF"), false), (Some("0"), false), (Some("off"), true)] {
        let r = Repo::new(&format!("switch_{}", value.unwrap_or("unset").to_lowercase()));
        r.write("src/keys.py", &format!("def {key}():\n    return 1\n"));
        // `-`, not `_`: the key's rule wants a word boundary before it, as gitleaks' does
        r.write(&format!("big-{key}.py"), &"x = 1\n".repeat(200_000));
        let mut cmd = Command::new(BIN);
        cmd.arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        match value {
            Some(v) => cmd.env("SCRUTATE_REDACT", v),
            None => cmd.env_remove("SCRUTATE_REDACT"),
        };
        let mut child = cmd.spawn().unwrap();
        let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
        c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
        c.notify("initialized", json!({}));
        let t0 = Instant::now();
        let ws = loop {
            let ws = c.request("workspace/symbol", json!({"query": "AKIA"}));
            if ws["result"].as_array().is_some_and(|a| !a.is_empty()) {
                break ws;
            }
            assert!(t0.elapsed() < Duration::from_secs(60), "{value:?}: no symbol: {ws}");
            std::thread::sleep(Duration::from_millis(100));
        };
        c.request("shutdown", Value::Null);
        c.notify("exit", Value::Null);
        let logs =
            c.notifications.iter().filter(|n| n["method"] == "window/logMessage").map(|n| n["params"]["message"].to_string()).collect::<Vec<_>>().join("\n");
        drop(c);
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(logs.contains("big-"), "{value:?}: the left-out file is named: {logs}");
        assert_eq!(ws.to_string().contains(&key), raw, "{value:?}: {ws}");
        assert_eq!(logs.contains(&key), raw, "{value:?}: {logs}");
        if !raw {
            assert!(ws.to_string().contains("[redacted:aws-access-key-id]"), "{value:?}: {ws}");
            assert!(logs.contains("big-[redacted:aws-access-key-id].py"), "{value:?}: {logs}");
        }
        assert_eq!(err.contains("is neither off nor on"), matches!(value, Some("OFF") | Some("0")), "{value:?}: {err}");
    }
}
