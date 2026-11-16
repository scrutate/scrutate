//! A limit applies after what narrows: an answer with a filter and a limit is the answer with no
//! limit, filtered, then cut. Repositories where one name has 2 to 60 definitions across files,
//! languages and kinds, a fixed seed, and for `find_symbol` and `references` each filter
//! (a glob, a language, a kind, two at once) with a random limit.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use scrutate::session::{Every, Session};
use serde_json::{json, Value};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// (extension, language name, a function of `nm`, a type of `nm`, a caller of it), for file `i`.
fn file(lang: usize, i: usize, function: bool) -> (&'static str, String) {
    match lang {
        0 => (
            "py",
            if function {
                format!("def nm():\n    return {i}\n\n\ndef use_{i}():\n    return nm()\n")
            } else {
                format!("class nm:\n    v = {i}\n\n\ndef use_{i}():\n    return nm()\n")
            },
        ),
        1 => (
            "js",
            if function {
                format!("function nm() {{ return {i} }}\n\nfunction use_{i}() {{ return nm() }}\n")
            } else {
                format!("class nm {{ v() {{ return {i} }} }}\n\nfunction use_{i}() {{ return new nm() }}\n")
            },
        ),
        2 => (
            "rs",
            if function {
                format!("fn nm() -> u64 {{ {i} }}\n\nfn use_{i}() -> u64 {{ nm() }}\n")
            } else {
                format!("struct nm(u64);\n\nfn use_{i}() -> nm {{ nm({i}) }}\n")
            },
        ),
        _ => (
            "go",
            if function {
                format!("package p\n\nfunc nm() int {{ return {i} }}\n\nfunc use_{i}() int {{ return nm() }}\n")
            } else {
                format!("package p\n\ntype nm int\n\nfunc use_{i}() nm {{ return nm({i}) }}\n")
            },
        ),
    }
}

const LANGS: [&str; 4] = ["python", "javascript", "rust", "go"];

/// The same rule as a filter, over a definition's path and kind.
type Keeps = Box<dyn Fn(&str, &str) -> bool>;

/// A filter the tools take, and its rule.
fn filters(rng: &mut Rng) -> (Value, Keeps) {
    match rng.below(5) {
        0 => {
            let d = rng.below(3);
            (json!({"globs": [format!("d{d}/**")]}), Box::new(move |p: &str, _: &str| p.starts_with(&format!("d{d}/"))))
        }
        1 => {
            let l = rng.below(4);
            let ext = ["py", "js", "rs", "go"][l];
            (json!({"langs": [LANGS[l]]}), Box::new(move |p: &str, _: &str| p.ends_with(&format!(".{ext}"))))
        }
        2 => (json!({"kinds": ["function"]}), Box::new(|_: &str, k: &str| k == "function")),
        3 => (json!({"kinds": ["class", "struct", "type"]}), Box::new(|_: &str, k: &str| matches!(k, "class" | "struct" | "type"))),
        _ => {
            let d = rng.below(3);
            (
                json!({"globs": [format!("d{d}/**")], "kinds": ["function"]}),
                Box::new(move |p: &str, k: &str| p.starts_with(&format!("d{d}/")) && k == "function"),
            )
        }
    }
}

fn with(mut a: Value, b: &Value) -> Value {
    a.as_object_mut().unwrap().extend(b.as_object().unwrap().clone());
    a
}

/// The definitions a find_symbol answer lists: (id, path, kind).
fn listed(v: &Value) -> Vec<(String, String, String)> {
    v["result"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| (h["id"].as_str().unwrap_or("").into(), h["path"].as_str().unwrap_or("").into(), h["kind"].as_str().unwrap_or("").into()))
        .collect()
}

/// The occurrences a references answer lists, in the order the walk meets them: (path, line, column).
fn occurrences(v: &Value) -> Vec<(String, u64, u64)> {
    let r = &v["result"];
    let at = |o: &Value| (o["path"].as_str().unwrap_or("").to_string(), o["line"].as_u64().unwrap_or(0), o["column"].as_u64().unwrap_or(0));
    let mut all: Vec<(String, u64, u64)> =
        r["definitions"].as_array().into_iter().flatten().flat_map(|d| d["references"].as_array().into_iter().flatten().map(at)).collect();
    all.extend(r["unresolved"].as_array().into_iter().flatten().map(at));
    all.sort();
    all
}

#[test]
fn a_limit_cuts_the_filtered_answer_never_before_the_filter() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_filters_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let mut rng = Rng(0x00d2_7d02);
    let settings = scrutate::mcp::Settings::new(None);
    let cancel = Arc::default();
    let (mut rows, mut broken) = (0, Vec::new());
    for r in 0..24 {
        let root = base.join(format!("r{r}"));
        let n = 2 + rng.below(59);
        for i in 0..n {
            let (ext, text) = file(rng.below(4), i, rng.below(2) == 0);
            let p = root.join(format!("d{}/f{i}.{ext}", i % 3));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        fs::create_dir_all(root.join(".git")).unwrap();
        let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
        let call = |tool: &str, args: Value| scrutate::mcp::call_tool(&session, tool, &args, &settings, &cancel).structured;
        let all = listed(&call("find_symbol", json!({"name": "nm", "limit": 500, "exact": true})));
        for _ in 0..4 {
            let (filter, keeps) = filters(&mut rng);
            let limit = 1 + rng.below(n.min(25));
            rows += 1;
            let want: Vec<String> = all.iter().filter(|(_, p, k)| keeps(p, k)).take(limit).map(|d| d.0.clone()).collect();
            let got: Vec<String> =
                listed(&call("find_symbol", with(json!({"name": "nm", "limit": limit, "exact": true}), &filter))).into_iter().map(|d| d.0).collect();
            if got != want {
                broken.push(format!("r{r} (n={n}) find_symbol {filter} limit {limit}: {} where {} pass ({got:?} vs {want:?})", got.len(), want.len()));
            }
            let max = 1 + rng.below(2 * n);
            rows += 1;
            let full = occurrences(&call("references", with(json!({"name": "nm", "max_results": 5000}), &filter)));
            let cut = occurrences(&call("references", with(json!({"name": "nm", "max_results": max}), &filter)));
            let want = &full[..full.len().min(max)];
            if cut != want {
                broken.push(format!(
                    "r{r} (n={n}) references {filter} max_results {max}: {} occurrences where the first {} of {} pass ({:?} vs {:?})",
                    cut.len(),
                    want.len(),
                    full.len(),
                    cut.first(),
                    want.first()
                ));
            }
        }
    }
    eprintln!("limit filters: {rows} rows, {} failed", broken.len());
    assert!(broken.is_empty(), "{}", broken.join("\n"));
    let _ = fs::remove_dir_all(&base);
}

/// A kind narrows the name lookup itself: a method `a0` indexed after 101,000 functions `a0` is
/// found by `symbol a0 --kind method`, where the read stopped at its first 100,000 definitions and
/// answered "(no results)". Heavy (about a minute): run with `--ignored`.
#[test]
#[ignore = "heavy: indexes 101,000 definitions of one name"]
fn a_kind_finds_a_definition_past_100000_of_the_name() {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_kind_bound_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..1010 {
        let body: String = (0..100).map(|k| format!("def a0():\n    return {}\n\n\n", i * 100 + k)).collect();
        fs::write(root.join(format!("a/f{i}.py")), body).unwrap();
    }
    let run = |args: &[&str]| {
        let out = std::process::Command::new(BIN)
            .args(args)
            .current_dir(&root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    run(&["index"]);
    fs::create_dir_all(root.join("zz")).unwrap();
    fs::write(root.join("zz/last.py"), "class C:\n    def a0(self):\n        return 1\n").unwrap();
    run(&["index"]);
    let text = run(&["symbol", "a0", "--kind", "method"]);
    assert!(text.contains("zz/last.py"), "{text}");
    let json = run(&["symbol", "a0", "--kind", "method", "--json"]);
    assert!(json.contains("zz/last.py"), "{json}");
    let _ = fs::remove_dir_all(&root);
}

/// A filtered search answers k when k pass, or says it stopped: a kind that holds about a sixth of
/// the chunks, too many to narrow the pools at once, and ten classes whose text holds the query
/// only in their bodies, behind 5,000 functions named by it, are found.
#[test]
fn a_filtered_search_short_at_its_bound_says_so() {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_search_bound_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..500 {
        let body: String = (0..10).map(|k| format!("def token_{i}_{k}():\n    return \"token {i} {k}\"\n\n\n")).collect();
        fs::write(root.join(format!("f{i}.py")), body).unwrap();
    }
    for i in 0..100 {
        let body: String = (0..10).map(|k| format!("class Plain{i}x{k}:\n    v = {k}\n\n\n")).collect();
        fs::write(root.join(format!("c{i}.py")), body).unwrap();
    }
    let body: String = (0..10).map(|k| format!("class Holder{k}:\n    v = \"token\"\n\n\n")).collect();
    fs::write(root.join("holders.py"), body).unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(BIN)
            .args(args)
            .current_dir(&root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    run(&["index"]);
    let v: Value = serde_json::from_str(&run(&["search", "token", "--kind", "class", "-k", "10", "--json"])).unwrap();
    let found = v["result"].as_array().map_or(0, Vec::len);
    assert!(found == 10 || !v["stopped_at"].is_null(), "{found} results and no stopped_at: {v}");
    let text = run(&["search", "token", "--kind", "class", "-k", "10"]);
    assert!(found == 10 || text.contains("stopped at"), "{text}");
    eprintln!("filtered search at its bound: {found} of 10");
    let _ = fs::remove_dir_all(&root);
}

/// A name written as a path finds its definition by the stored qualified names however many share
/// its last part: `zz::Holder::push` finds `Holder.push` indexed after 10,100 other methods `push`,
/// where the lookup read only the first 10,000 of them and answered "(no results)".
#[test]
fn a_path_name_finds_its_definition_past_10000_of_its_last_part() {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_qualnames_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..101 {
        let body: String = (0..100).map(|k| format!("pub struct A{i}x{k};\nimpl A{i}x{k} {{\n    pub fn push(&self) {{}}\n}}\n\n")).collect();
        fs::write(root.join(format!("src/a{i}.rs")), body).unwrap();
    }
    let run = |args: &[&str]| {
        let out = std::process::Command::new(BIN)
            .args(args)
            .current_dir(&root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    run(&["index"]);
    fs::write(root.join("src/zz.rs"), "pub struct Holder;\nimpl Holder {\n    pub fn push(&self) {}\n}\n").unwrap();
    run(&["index"]);
    let text = run(&["symbol", "zz::Holder::push"]);
    assert!(text.contains("src/zz.rs"), "{text}");
    let _ = fs::remove_dir_all(&root);
}

/// references with kinds answers at once when no definition of the name has one, resolves at most
/// 10,000 occurrences (or max_results when larger) and says how many when it stops there.
#[test]
fn references_with_kinds_stop_at_their_bound_and_say_why() {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_refs_kind_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let calls: String = (0..10_050).map(|_| "    a0()\n").collect();
    fs::write(root.join("big.py"), format!("def a0():\n    return 1\n\n\ndef many():\n{calls}")).unwrap();
    fs::write(
        root.join("zz.py"),
        "class C:\n    def a0(self):\n        return 1\n\n    def use(self):\n        self.a0()\n        self.a0()\n        self.a0()\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(BIN)
            .args(args)
            .current_dir(&root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    run(&["index"]);
    // no definition of a0 is a class: empty at once, and it says so
    let none: Value = serde_json::from_str(&run(&["refs", "a0", "--kind", "class", "--json"])).unwrap();
    assert!(none["result"]["stopped_reason"].as_str().is_some_and(|r| r.contains("no definition named a0 is of those kinds")), "{none}");
    // the method's uses come after 10,050 of the function's: the read stops at its bound and says so
    let text = run(&["refs", "a0", "--kind", "method", "--max-results", "5"]);
    assert!(text.contains("10000 occurrences were resolved") && !text.contains("raise max_results"), "{text}");
    // that answer holds none, and says it stopped rather than that there are none
    let stopped: Value = serde_json::from_str(&run(&["refs", "a0", "--kind", "method", "--max-results", "5", "--json"])).unwrap();
    assert!(
        stopped["result"]["truncated"] == json!(true) && stopped["result"]["stopped_reason"].as_str().is_some_and(|r| r.contains("10000 occurrences")),
        "{stopped}"
    );
    // a max_results above the bound is read whole, and finds them
    let all: Value = serde_json::from_str(&run(&["refs", "a0", "--kind", "method", "--max-results", "20000", "--json"])).unwrap();
    let found: usize = all["result"]["definitions"].as_array().into_iter().flatten().map(|d| d["references"].as_array().map_or(0, Vec::len)).sum();
    assert!(found >= 3 && all["result"]["stopped_reason"].is_null(), "{found}: {all}");
    let _ = fs::remove_dir_all(&root);
}

/// One kept occurrence past max_results, the last one read, still marks the answer cut: a method
/// a0 with its definition line and 7 uses, after 40 uses of a function a0, at --max-results 7, by
/// kind.
#[test]
fn the_occurrence_past_max_results_marks_the_answer_cut_when_it_is_the_last() {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_refs_last_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let calls: String = (0..40).map(|_| "    a0()\n").collect();
    fs::write(root.join("big.py"), format!("def a0():\n    return 1\n\n\ndef many():\n{calls}")).unwrap();
    let uses: String = (0..7).map(|_| "        self.a0()\n").collect();
    fs::write(root.join("m.py"), format!("class C:\n    def a0(self):\n        return 1\n\n    def use(self):\n{uses}")).unwrap();
    std::process::Command::new(BIN).arg("index").current_dir(&root).env("SCRUTATE_SEMANTIC", "none").status().unwrap();
    let run = |args: &[&str]| -> Value {
        let out =
            std::process::Command::new(BIN).args(args).current_dir(&root).env("SCRUTATE_SEMANTIC", "none").env("SCRUTATE_NO_SESSION", "1").output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let count =
        |v: &Value| -> usize { v["result"]["definitions"].as_array().into_iter().flatten().map(|d| d["references"].as_array().map_or(0, Vec::len)).sum() };
    let args = ["--kind", "method"];
    let all: Vec<&str> = ["refs", "a0"].into_iter().chain(args.iter().copied()).chain(["--max-results", "5000", "--json"]).collect();
    let full = run(&all);
    assert!(count(&full) == 8 && full["result"]["truncated"] == json!(false), "{args:?} whole: {full}");
    let cut: Vec<&str> = ["refs", "a0"].into_iter().chain(args.iter().copied()).chain(["--max-results", "7", "--json"]).collect();
    let v = run(&cut);
    assert!(count(&v) == 7 && v["result"]["truncated"] == json!(true), "{args:?} at 7: {v}");
    let _ = fs::remove_dir_all(&root);
}
