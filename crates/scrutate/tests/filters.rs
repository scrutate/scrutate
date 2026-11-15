//! Scope filters on search, symbol and refs, and their MCP tools: -g, -t, --lang and --kind.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str, files: &[(&str, String)]) -> Repo {
        let r = Repo::unindexed(name, files);
        r.ok(&["index"]);
        r
    }

    /// The tree alone: an MCP session builds its own index, with the model it runs with.
    fn unindexed(name: &str, files: &[(&str, String)]) -> Repo {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scrutate_filters_{name}_{}_{n}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, text) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        Repo { root: fs::canonicalize(&root).unwrap() }
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(BIN).args(args).current_dir(&self.root).env("SCRUTATE_SEMANTIC", "none").stdin(Stdio::null()).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    /// The `result` of a `--json` answer.
    fn json(&self, args: &[&str]) -> Value {
        let out = self.ok(&[args, &["--json"]].concat());
        serde_json::from_str::<Value>(&out).unwrap_or_else(|e| panic!("{e}: {out}"))["result"].clone()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Every `path` an answer names, at any depth.
fn paths(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(p)) = m.get("path") {
                    out.push(p.clone());
                }
                stack.extend(m.values());
            }
            Value::Array(a) => stack.extend(a),
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A shape library in three languages and two directories, each with callers.
fn shapes() -> Repo {
    Repo::new("shapes", &shape_files())
}

fn shape_files() -> Vec<(&'static str, String)> {
    let py = |dir: &str| {
        format!(
            "class Shape:\n    \"\"\"A shape with an area.\"\"\"\n\n    def area(self):\n        return shape_area(self)\n\n\ndef shape_area(shape):\n    \"\"\"The area of a shape in {dir}.\"\"\"\n    return 0\n"
        )
    };
    let files = [
        ("src/geo/shape.py", py("src")),
        ("src/geo/use.py", "from src.geo.shape import shape_area\n\n\ndef total(shapes):\n    return sum(shape_area(s) for s in shapes)\n".to_string()),
        ("lib/shape.py", py("lib")),
        ("lib/use.py", "from lib.shape import shape_area\n\n\ndef first(shapes):\n    return shape_area(shapes[0])\n".to_string()),
        ("src/geo/shape.rs", "/// A shape with an area.\npub struct Shape {\n    pub w: f64,\n}\n\n/// The area of a shape in rust.\npub fn shape_area(s: &Shape) -> f64 {\n    s.w * s.w\n}\n\npub fn twice(s: &Shape) -> f64 {\n    shape_area(s) * 2.0\n}\n".to_string()),
        ("include/shape.h", "#include <vector>\nnamespace geo {\n/// A shape with an area.\nclass Shape {\n public:\n  double area() const;\n};\n}\n".to_string()),
        ("include/cshape.h", "#include <stdio.h>\n/* A shape with an area. */\nstruct Shape { double w; };\ndouble shape_area(struct Shape s);\n".to_string()),
    ];
    files.to_vec()
}

#[test]
fn a_glob_keeps_search_symbol_and_refs_to_the_files_it_names() {
    let r = shapes();
    let all = paths(&r.json(&["search", "area of a shape", "-k", "20"]));
    assert!(all.iter().any(|p| p.starts_with("lib/")) && all.iter().any(|p| p.starts_with("src/")), "{all:?}");
    let src = paths(&r.json(&["search", "area of a shape", "-k", "20", "-g", "src/**"]));
    assert!(!src.is_empty() && src.iter().all(|p| p.starts_with("src/")), "{src:?}");
    let not_lib = paths(&r.json(&["symbol", "shape_area", "-g", "!lib/**"]));
    assert!(not_lib.contains(&"src/geo/shape.py".to_string()) && not_lib.iter().all(|p| !p.starts_with("lib/")), "{not_lib:?}");
    let refs = paths(&r.json(&["refs", "shape_area", "-g", "lib/**"]));
    assert!(refs.contains(&"lib/use.py".to_string()) && refs.iter().all(|p| p.starts_with("lib/")), "{refs:?}");
}

#[test]
fn a_type_keeps_them_to_files_of_that_type_and_a_negated_one_leaves_them_out() {
    let r = shapes();
    let rust = paths(&r.json(&["search", "area of a shape", "-k", "20", "-t", "rust"]));
    assert_eq!(rust, ["src/geo/shape.rs"]);
    let py = paths(&r.json(&["symbol", "Shape", "-t", "py"]));
    assert_eq!(py, ["lib/shape.py", "src/geo/shape.py"]);
    let not_py = paths(&r.json(&["refs", "shape_area", "-t", "!py"]));
    assert!(not_py.contains(&"src/geo/shape.rs".to_string()) && not_py.iter().all(|p| !p.ends_with(".py")), "{not_py:?}");
}

#[test]
fn a_language_is_the_one_each_file_was_parsed_as() {
    let r = shapes();
    let cpp = paths(&r.json(&["symbol", "Shape", "--lang", "cpp"]));
    assert_eq!(cpp, ["include/shape.h"], "the C header is not C++");
    let c = paths(&r.json(&["search", "shape with an area", "-k", "20", "--lang", "c"]));
    assert_eq!(c, ["include/cshape.h"]);
    let rust_refs = paths(&r.json(&["refs", "shape_area", "--lang", "rust"]));
    assert_eq!(rust_refs, ["src/geo/shape.rs"]);
}

#[test]
fn a_kind_keeps_the_definitions_of_that_kind() {
    let r = shapes();
    let classes = r.json(&["symbol", "Shape", "--kind", "class"]);
    let kinds: Vec<&str> = classes.as_array().unwrap().iter().map(|h| h["kind"].as_str().unwrap()).collect();
    assert!(!kinds.is_empty() && kinds.iter().all(|k| *k == "class"), "{classes}");
    let structs = paths(&r.json(&["symbol", "Shape", "--kind", "struct"]));
    assert!(structs.contains(&"src/geo/shape.rs".to_string()) && !structs.iter().any(|p| p.ends_with(".py")), "{structs:?}");
    let fns = r.json(&["search", "area of a shape", "-k", "20", "--kind", "function", "-t", "py"]);
    let hits = fns.as_array().unwrap();
    assert!(!hits.is_empty() && hits.iter().all(|h| h["kind"] == "function" && h["path"].as_str().unwrap().ends_with(".py")), "{fns}");
    let refs = r.json(&["refs", "area", "--kind", "method"]);
    let defs = refs["definitions"].as_array().unwrap();
    assert!(!defs.is_empty() && defs.iter().all(|d| d["kind"] == "method"), "{refs}");
    let (code, _, err) = r.run(&["refs", "shape_area", "--textual", "--kind", "function"]);
    assert_eq!(code, 2);
    assert!(err.contains("textual references have no definition kind"), "{err}");
    let (code, _, err) = r.run(&["search", "shape", "--kind", "fuction"]);
    assert_eq!(code, 2);
    assert!(err.contains("unknown kind \"fuction\""), "{err}");
}

/// A filter keeps k results when k pass it, though more than the search's candidate pools hold
/// rank above them.
#[test]
fn a_filter_keeps_k_results_when_k_pass_it() {
    let mut files = Vec::new();
    for i in 0..150 {
        files.push((
            format!("noise/n{i}.py"),
            format!("def ledger_balance_{i}(ledger):\n    \"\"\"Balance the ledger: ledger balance ledger balance.\"\"\"\n    return ledger\n"),
        ));
    }
    for i in 0..6 {
        files.push((format!("keep/k{i}.py"), format!("def keeper_{i}(book):\n    \"\"\"Balance the ledger once.\"\"\"\n    return book\n")));
    }
    for i in 0..6 {
        files.push((format!("books/b{i}.py"), format!("class Book{i}:\n    \"\"\"Balance the ledger once.\"\"\"\n\n    pages = {i}\n")));
    }
    let files: Vec<(&str, String)> = files.iter().map(|(p, t)| (p.as_str(), t.clone())).collect();
    let r = Repo::new("starve", &files);
    let hits = r.json(&["search", "ledger balance", "-k", "5", "-g", "keep/**"]);
    let got: Vec<&str> = hits.as_array().unwrap().iter().map(|h| h["path"].as_str().unwrap()).collect();
    assert_eq!(got.len(), 5, "{hits}");
    assert!(got.iter().all(|p| p.starts_with("keep/")), "{got:?}");
    // a kind is no file the pools can be drawn from: the search looks again with wider pools
    let classes = r.json(&["search", "ledger balance", "-k", "5", "--kind", "class"]);
    let kinds: Vec<&str> = classes.as_array().unwrap().iter().map(|h| h["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["class"; 5], "{classes}");
}

#[test]
fn the_mcp_tools_take_the_same_filters() {
    let r = Repo::unindexed("shapes_mcp", &shape_files());
    let session = scrutate::session::Session::open(&r.root, scrutate::session::Every::given(Some(3600.0))).unwrap();
    session.build().unwrap();
    let call = |tool: &str, args: Value| {
        let c = scrutate::mcp::call_tool(&session, tool, &args, &scrutate::mcp::Settings::new(None), &Default::default());
        (c.is_error, c.text, c.structured)
    };
    let (failed, text, v) = call("search_code", json!({"query": "area of a shape", "k": 20, "globs": ["src/**"], "kinds": ["function"]}));
    assert!(!failed, "{text}");
    let p = paths(&v["result"]);
    assert!(!p.is_empty() && p.iter().all(|p| p.starts_with("src/")), "{text}");
    let (failed, text, v) = call("find_symbol", json!({"name": "Shape", "langs": ["cpp"]}));
    assert!(!failed, "{text}");
    assert_eq!(paths(&v["result"]), ["include/shape.h"], "{text}");
    let (failed, text, v) = call("references", json!({"name": "shape_area", "types": ["rust"]}));
    assert!(!failed, "{text}");
    assert_eq!(paths(&v["result"]), ["src/geo/shape.rs"], "{text}");
    let (failed, text, _) = call("find_symbol", json!({"name": "Shape", "kinds": ["clas"]}));
    assert!(failed && text.contains("unknown kind \"clas\""), "{text}");
}
