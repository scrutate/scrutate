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

/// The MCP tool's answer for `args`, its `result`, on a tree its own session indexes.
fn mcp(root: &std::path::Path, tool: &str, args: Value) -> Value {
    let session = scrutate::session::Session::open(root, scrutate::session::Every::given(Some(3600.0))).unwrap();
    session.build().unwrap();
    let c = scrutate::mcp::call_tool(&session, tool, &args, &scrutate::mcp::Settings::new(None), &Default::default());
    assert!(!c.is_error, "{tool} {args}: {}", c.text);
    c.structured["result"].clone()
}

/// 8,000 definitions that rank above the ones a filter keeps: the filter still finds every one.
fn zebras(name: &str, indexed: bool) -> Repo {
    let mut files = Vec::new();
    for i in 0..10 {
        let defs: String = (0..800).map(|j| format!("def zebra_{i}_{j}(zebra):\n    '''zebra zebra'''\n    return zebra\n\n\n")).collect();
        files.push((format!("noise/n{i}.py"), defs));
    }
    let words = |w: &str| (0..300).map(|i| if i == 150 { w.to_string() } else { format!("word{i}") }).collect::<Vec<_>>().join(" ");
    let mut keep = String::new();
    for i in 0..3 {
        keep.push_str(&format!("class Keeper{i}:\n    '''{}'''\n\n\n", words("zebra")));
        keep.push_str(&format!("def keeper_{i}():\n    '''{}'''\n    return {i}\n\n\n", words("zebra")));
    }
    files.push(("keep/k00.py".to_string(), keep));
    for i in 1..11 {
        files.push((format!("keep/k{i:02}.py"), format!("def other_{i}():\n    '''{}'''\n    return {i}\n", words("giraffe"))));
    }
    let files: Vec<(&str, String)> = files.iter().map(|(p, t)| (p.as_str(), t.clone())).collect();
    if indexed {
        Repo::new(name, &files)
    } else {
        Repo::unindexed(name, &files)
    }
}

#[test]
fn a_filter_finds_its_results_below_thousands_that_rank_higher() {
    let r = zebras("zebras", true);
    let hits = |args: &[&str]| -> Vec<(String, String)> {
        let v = r.json(&[&["search", "zebra", "-k", "10"], args].concat());
        v.as_array().unwrap().iter().map(|h| (h["path"].as_str().unwrap().to_string(), h["kind"].as_str().unwrap().to_string())).collect()
    };
    let classes = hits(&["--kind", "class"]);
    assert_eq!(classes.len(), 3, "{classes:?}");
    assert!(classes.iter().all(|(p, k)| p == "keep/k00.py" && k == "class"), "{classes:?}");
    assert_eq!(hits(&["-t", "py", "--kind", "class"]), classes);
    let kept = hits(&["-g", "keep/**"]);
    assert_eq!(kept.len(), 6, "{kept:?}");
    // a glob that keeps more than half the files
    let broad = hits(&["-g", "!noise/**"]);
    assert_eq!(broad.len(), 6, "{broad:?}");
    // a glob and a kind that each keep most of what they judge
    let fns = hits(&["-g", "keep/**", "--kind", "function"]);
    assert_eq!(fns.len(), 3, "{fns:?}");
    assert!(fns.iter().all(|(p, k)| p == "keep/k00.py" && k == "function"), "{fns:?}");
    // a kind no definition has answers at once, with nothing: exit code 1, as an empty answer
    let (code, out, _) = r.run(&["search", "zebra", "-k", "10", "--kind", "trait", "--json"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["result"], json!([]));
    let u = zebras("zebras_mcp", false);
    let v = mcp(&u.root, "search_code", json!({"query": "zebra", "k": 10, "kinds": ["class"]}));
    assert_eq!(v.as_array().unwrap().len(), 3, "{v}");
}

#[test]
fn a_custom_language_is_a_language_filter_too() {
    let files = [
        (".scrutate.toml", "[[language]]\nname = \"mylang\"\nextensions = [\"myl\"]\nline_comment = \"--\"\ndefinitions = [\n  { kind = \"function\", pattern = '^\\s*fn\\s+([A-Za-z_][A-Za-z0-9_]*)' },\n]\n".to_string()),
        ("a.myl", "fn alpha\n  -- the first letter\n".to_string()),
        ("b.py", "def alpha():\n    return 1\n".to_string()),
    ];
    let r = Repo::new("custom", &files);
    assert_eq!(paths(&r.json(&["symbol", "alpha", "--lang", "mylang"])), ["a.myl"]);
    assert_eq!(paths(&r.json(&["symbol", "alpha", "--lang", "python"])), ["b.py"]);
    let u = Repo::unindexed("custom_mcp", &files);
    assert_eq!(paths(&mcp(&u.root, "find_symbol", json!({"name": "alpha", "langs": ["mylang"]}))), ["a.myl"]);
}

fn configs(indexed: bool, name: &str) -> Repo {
    let files = [
        ("py/pkg/geo.py", "def parse_config(text):\n    return text.split()\n".to_string()),
        ("py/pkg/klass.py", "class parse_config:\n    pass\n\n\ndef make():\n    return parse_config()\n".to_string()),
        ("tests/test_geo.py", "from py.pkg.geo import parse_config\n\n\ndef test_parse():\n    assert parse_config('a b') == ['a', 'b']\n".to_string()),
        ("src/app.py", "from py.pkg.geo import parse_config\n\n\ndef main(t):\n    return parse_config(t)\n".to_string()),
        ("js/geo.js", "function parse_config(t) {\n  return t;\n}\nmodule.exports = { parse_config };\n".to_string()),
        ("js/use.js", "const { parse_config } = require('./geo');\nfunction run(t) {\n  return parse_config(t);\n}\n".to_string()),
    ];
    if indexed {
        Repo::new(name, &files)
    } else {
        Repo::unindexed(name, &files)
    }
}

/// (path, outside_filter) of each definition, and the paths of the occurrences under them.
fn heads(v: &Value) -> (Vec<(String, bool)>, Vec<String>) {
    let defs = v["definitions"].as_array().unwrap_or_else(|| panic!("{v}"));
    let mut d: Vec<(String, bool)> = defs.iter().map(|d| (d["path"].as_str().unwrap().to_string(), d["outside_filter"].as_bool().unwrap_or(false))).collect();
    d.sort();
    let mut o: Vec<String> =
        defs.iter().flat_map(|d| d["references"].as_array().into_iter().flatten()).map(|r| r["path"].as_str().unwrap().to_string()).collect();
    o.sort();
    o.dedup();
    (d, o)
}

#[test]
fn refs_keep_the_occurrences_in_the_files_a_filter_names_under_their_definitions() {
    let r = configs(true, "refs_files");
    // a definition outside the glob heads its uses inside it
    let (defs, occ) = heads(&r.json(&["refs", "parse_config", "-g", "tests/**"]));
    assert_eq!(occ, ["tests/test_geo.py"], "{defs:?}");
    assert_eq!(defs, [("py/pkg/geo.py".to_string(), true)]);
    // --def names a definition whatever the file filters keep
    let (defs, occ) = heads(&r.json(&["refs", "parse_config", "-g", "tests/**", "--def", "py/pkg/geo.py"]));
    assert_eq!((defs.len(), occ), (1, vec!["tests/test_geo.py".to_string()]));
    let (_, occ) = heads(&r.json(&["refs", "parse_config", "-t", "js"]));
    assert_eq!(occ, ["js/geo.js", "js/use.js"]);
    let (defs, occ) = heads(&r.json(&["refs", "parse_config", "--lang", "python", "-g", "src/**"]));
    assert_eq!(occ, ["src/app.py"], "{defs:?}");
    // a kind keeps the definitions of that kind and their uses
    let (defs, occ) = heads(&r.json(&["refs", "parse_config", "--kind", "class"]));
    assert_eq!(defs, [("py/pkg/klass.py".to_string(), false)]);
    assert_eq!(occ, ["py/pkg/klass.py"]);
    assert!(r.ok(&["refs", "parse_config", "-g", "tests/**"]).contains("(outside the filter)"));
    let u = configs(false, "refs_mcp");
    let (defs, occ) = heads(&mcp(&u.root, "references", json!({"name": "parse_config", "globs": ["tests/**"]})));
    assert_eq!((defs, occ), (vec![("py/pkg/geo.py".to_string(), true)], vec!["tests/test_geo.py".to_string()]));
}
