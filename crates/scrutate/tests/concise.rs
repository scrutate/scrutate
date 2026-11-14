//! `response_format: concise` on every MCP tool, run on a copy of this crate's own source: the
//! concise answer lists the locations the detailed one lists, in at most half its bytes when the
//! detailed one is over 1 KB.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use scrutate::session::{Every, Session};
use serde_json::{json, Value};

/// git's own configuration is the only one read: no global or system file.
const GIT_ENV: [(&str, &str); 2] = [("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")];

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy(&src, &dst);
        } else {
            fs::copy(&src, &dst).unwrap();
        }
    }
}

fn git(root: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    c.args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main", "-c", "core.autocrlf=false"]);
    let out = c.args(args).current_dir(root).envs(GIT_ENV).stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A git repository holding this crate's `src` and `tests` at their paths in the repository, with
/// one edited function in the working tree.
fn tree() -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("concise_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in ["src", "tests"] {
        copy(&here.join(dir), &root.join("crates/scrutate").join(dir));
    }
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "base"]);
    let fmt = root.join("crates/scrutate/src/fmt.rs");
    let text = fs::read_to_string(&fmt).unwrap().replace("let mut out = Vec::with_capacity(hits.len());", "let mut out = Vec::with_capacity(hits.len() + 0);");
    fs::write(&fmt, text).unwrap();
    canon(root)
}

/// Every `path:start-end` of a text. In a concise answer a line that ends with `/` names the
/// directory of the paths after it written without one (`./` is the root), and a location without
/// a path is in the file of the location before it. A quoted one is code, not a location.
fn locations(text: &str) -> BTreeSet<String> {
    let token = regex::Regex::new(r"(?:^|[\s(])([A-Za-z0-9_./-]*):(\d+)(-\d+|:\d+)?").unwrap();
    let (mut out, mut dir, mut last) = (BTreeSet::new(), String::new(), String::new());
    for line in text.lines() {
        if line.ends_with('/') && !line.contains(' ') {
            dir = if line == "./" { String::new() } else { line.trim_end_matches('/').to_string() };
            continue;
        }
        for m in token.captures_iter(line) {
            let mut path = m[1].to_string();
            if path.is_empty() {
                path = last.clone();
            } else if let Some(p) = path.strip_prefix("./") {
                path = p.to_string();
            } else if !path.contains('/') && !dir.is_empty() {
                path = format!("{dir}/{path}");
            }
            last = path.clone();
            if let Some(end) = m.get(3).map(|e| e.as_str()).filter(|e| e.starts_with('-')) {
                out.insert(format!("{path}:{}{end}", &m[2]));
            }
        }
    }
    out
}

#[test]
fn a_concise_answer_lists_the_same_locations_in_at_most_half_the_bytes() {
    let root = tree();
    let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
    session.build().unwrap();
    let settings = scrutate::mcp::Settings::new(None);
    // the location of the definition whose line starts so
    let at = |file: &str, start: &str| {
        let path = format!("crates/scrutate/src/{file}");
        let text = fs::read_to_string(root.join(&path)).unwrap();
        format!("{path}:{}", text.lines().position(|l| l.starts_with(start)).unwrap() + 1)
    };
    let calls: Vec<(&str, Value)> = vec![
        ("search_code", json!({"query": "page an answer within a byte budget"})),
        ("search_code", json!({"query": "resolve a call to its definition", "k": 30})),
        ("grep_code", json!({"pattern": "fn text"})),
        ("grep_code", json!({"pattern": "max_bytes", "mode": "identifier"})),
        ("find_symbol", json!({"name": "run"})),
        ("callers", json!({"name": "shown"})),
        ("callees", json!({"name": at("ops.rs", "pub fn run(")})),
        ("callees", json!({"name": at("cli.rs", "pub fn run(")})),
        ("call_path", json!({"from": at("mcp.rs", "pub fn call_tool("), "to": "read_range"})),
        ("repo_map", json!({})),
        ("read_code", json!({"path": "crates/scrutate/src/ops.rs"})),
        ("read_code", json!({"symbol": "Parts.render"})),
        ("read_code", json!({"symbol": "Answer"})),
        ("outline", json!({"path": "crates/scrutate/src/ops.rs"})),
        ("dependencies", json!({"path": "crates/scrutate/src/cli.rs"})),
        ("dependents", json!({"target": "crates/scrutate/src/fmt.rs"})),
        ("references", json!({"name": "hit_blocks"})),
        ("references", json!({"name": "Answer", "textual": true})),
        ("similar_code", json!({"target": at("ops.rs", "pub fn stop_line("), "k": 50})),
        ("duplicates", json!({"threshold": 0.6, "limit": 200, "min_tokens": 15})),
        ("structural_search", json!({"lang": "rust", "query": "(function_item name: (identifier) @f)"})),
        ("impact", json!({})),
        ("find_files", json!({"query": "rs", "limit": 200})),
        ("dead_code", json!({})),
        ("reindex", json!({})),
    ];
    let mut tools = BTreeSet::new();
    let mut failed = Vec::new();
    eprintln!("{:<18} {:>8} {:>8} {:>6} {:>6}", "tool", "detailed", "concise", "ratio", "places");
    for (tool, args) in &calls {
        let call = |form: &str| {
            let mut args = args.clone();
            args["response_format"] = json!(form);
            let c = scrutate::mcp::call_tool(&session, tool, &args, &settings, &Default::default());
            assert!(!c.is_error, "{tool} {args}: {}", c.text);
            c.text
        };
        let (detailed, concise) = (call("detailed"), call("concise"));
        let (d, c) = (locations(&detailed), locations(&concise));
        eprintln!("{tool:<18} {:>8} {:>8} {:>6.2} {:>6}", detailed.len(), concise.len(), concise.len() as f64 / detailed.len() as f64, d.len());
        if d != c {
            failed.push(format!("{tool} {args}: locations differ: {:?} / {:?}", d.difference(&c).collect::<Vec<_>>(), c.difference(&d).collect::<Vec<_>>()));
        }
        if detailed.len() > 1024 && concise.len() * 2 > detailed.len() {
            failed.push(format!("{tool} {args}: {} bytes concise for {} detailed", concise.len(), detailed.len()));
        }
        tools.insert(*tool);
    }
    assert_eq!(tools.len(), 19, "every tool is called");
    assert!(failed.is_empty(), "{failed:#?}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn concise_paths_name_their_directory_once_and_a_bad_form_is_refused() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("concise_small_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("pkg/sub")).unwrap();
    // a root of its own, not the work tree above it
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("pkg/a.py"), "def make_point():\n    return 1\n\n\ndef make_line():\n    return make_point()\n").unwrap();
    fs::write(root.join("pkg/sub/b.py"), "from pkg.a import make_point\n\n\ndef make_box():\n    return make_point()\n").unwrap();
    fs::write(root.join("top.py"), "from pkg.a import make_point\n\n\ndef make_top():\n    return make_point()\n").unwrap();
    let root = canon(&root);
    let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
    session.build().unwrap();
    let settings = scrutate::mcp::Settings::new(None);
    let call = |tool: &str, args: Value| scrutate::mcp::call_tool(&session, tool, &args, &settings, &Default::default());
    let c = call("callers", json!({"name": "make_point", "response_format": "concise"}));
    assert_eq!(c.text, "pkg/\na.py:5-6 make_line\npkg/sub/\nb.py:4-5 make_box\n./\ntop.py:4-5 make_top", "{}", c.text);
    let d = call("callers", json!({"name": "make_point"}));
    assert_eq!(locations(&c.text), locations(&d.text));
    assert_eq!(c.structured, d.structured, "the JSON is the detailed one in both forms");
    // a location in the file of the one before it leaves out the path
    let c = call("outline", json!({"path": "pkg/a.py", "response_format": "concise"}));
    assert_eq!(c.text, "1 make_point\n5 make_line");
    let c = call("find_symbol", json!({"name": "make_line", "response_format": "concise"}));
    assert_eq!(c.text, "pkg/\na.py:5-6 make_line");
    for bad in [json!("short"), json!(1)] {
        let c = call("find_symbol", json!({"name": "make_line", "response_format": bad}));
        assert!(c.is_error && c.text == "error: response_format must be \"detailed\" or \"concise\"", "{}", c.text);
    }
    // the same items on each page, so an offset means the same in both forms
    for offset in [0, 1, 2] {
        let page = |form: &str| call("callers", json!({"name": "make_point", "response_format": form, "max_bytes": 1000, "offset": offset})).text;
        assert_eq!(locations(&page("detailed")), locations(&page("concise")), "offset {offset}");
    }
    let _ = fs::remove_dir_all(&root);
}
