//! A limit applies after what narrows: an answer with a filter and a limit is the answer with no
//! limit, filtered, then cut. Repositories where one name has 2 to 60 definitions across files,
//! languages and kinds, a fixed seed, and for `find_symbol` and `references` each filter
//! (a glob, a language, a kind, two at once) with a random limit.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use scrutate::session::{Every, Session};
use serde_json::{Value, json};

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
    match rng.below(6) {
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
        4 => (json!({"types": ["py"]}), Box::new(|p: &str, _: &str| p.ends_with(".py"))),
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

/// The items of an answer in their order, for the tools whose items are a list or groups of lines.
fn items(v: &Value) -> Vec<String> {
    let r = &v["result"];
    let lines = |g: &Value| -> Vec<String> { g["lines"].as_array().into_iter().flatten().map(|l| format!("{}:{}", g["path"], l["line"])).collect() };
    match r["groups"].as_array() {
        Some(groups) => groups.iter().flat_map(lines).collect(),
        None => r.as_array().into_iter().flatten().map(|i| format!("{}:{}:{}", i["path"], i["line"], i["column"])).collect(),
    }
}

#[test]
fn a_limit_cuts_the_filtered_answer_never_before_the_filter() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_filters_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let mut rng = Rng(0x00d2_7d02);
    let settings = scrutate::mcp::Settings::new(None);
    let cancel = Arc::default();
    let (mut rows, mut broken) = (0, Vec::new());
    let mut covered = std::collections::BTreeSet::new();
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
            for k in filter.as_object().unwrap().keys() {
                covered.insert(format!("find_symbol {k}"));
                covered.insert(format!("references {k}"));
                covered.insert(format!("search_code {k}"));
            }
            covered.insert("find_symbol exact".to_string());
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
            // search is ranked: every hit passes, and at least k when that many pass unfiltered
            let k = 1 + rng.below(10);
            rows += 1;
            let hits = |v: &Value| -> Vec<(String, String)> {
                v["result"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|h| (h["path"].as_str().unwrap_or("").into(), h["kind"].as_str().unwrap_or("").into()))
                    .collect()
            };
            let passing = hits(&call("search_code", json!({"query": "nm", "k": 200}))).iter().filter(|(p, k)| keeps(p, k)).count();
            let got = hits(&call("search_code", with(json!({"query": "nm", "k": k}), &filter)));
            if got.iter().any(|(p, k)| !keeps(p, k)) || got.len() < k.min(passing) {
                broken.push(format!("r{r} (n={n}) search_code {filter} k {k}: {} hits where {passing} pass ({got:?})", got.len()));
            }
        }
        // references with def: the occurrences of that one definition, the first max_results of them
        let target = &all[rng.below(all.len())];
        let line = target.0.rsplit(':').next().unwrap_or("1").split('-').next().unwrap_or("1");
        let def = format!("{}:{line}", target.1);
        let max = 1 + rng.below(4);
        rows += 1;
        let full = occurrences(&call("references", json!({"name": "nm", "def": def, "max_results": 5000})));
        let cut = occurrences(&call("references", json!({"name": "nm", "def": def, "max_results": max})));
        if cut[..] != full[..full.len().min(max)] {
            broken.push(format!(
                "r{r} (n={n}) references def {def} max_results {max}: {} occurrences where the first {} of {} pass",
                cut.len(),
                full.len().min(max),
                full.len()
            ));
        }
        covered.insert("references def".to_string());
        // the other narrowing arguments: the answer with a limit is the first items of the one without
        let d = rng.below(3);
        let others = [
            ("references", json!({"name": "nm", "textual": true, "globs": [format!("d{d}/**")]}), "max_results", 5000, "references textual"),
            ("grep_code", json!({"pattern": "nm", "path": format!("d{d}")}), "max_results", 5000, "grep_code path"),
            ("grep_code", json!({"pattern": "nm", "globs": [format!("d{d}/**")]}), "max_results", 5000, "grep_code globs"),
            ("find_files", json!({"query": format!("f{}", rng.below(n))}), "limit", 500, "find_files query"),
            ("structural_search", json!({"lang": "python", "query": "(function_definition) @f"}), "limit", 5000, "structural_search lang"),
            ("duplicates", json!({"min_tokens": 5, "threshold": 0.5}), "limit", 500, "duplicates min_tokens threshold"),
        ];
        for (tool, args, key, big, pair) in others {
            let limit = 1 + rng.below(2 * n);
            rows += 1;
            let full = items(&call(tool, with(args.clone(), &json!({key: big}))));
            let cut = items(&call(tool, with(args.clone(), &json!({key: limit}))));
            if cut[..] != full[..full.len().min(limit)] {
                broken.push(format!(
                    "r{r} (n={n}) {tool} {args} {key} {limit}: {} items where the first {} of {} pass",
                    cut.len(),
                    full.len().min(limit),
                    full.len()
                ));
            }
            covered.insert(pair.to_string());
        }
    }
    eprintln!("limit filters: {rows} rows, {} failed; covered: {}", broken.len(), covered.into_iter().collect::<Vec<_>>().join(", "));
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

/// A tree of `functions` functions named `token` (what the exact source finds, 60 more each time
/// the candidates widen), 1,000 classes without the word (a sixth of the chunks or more: too many
/// for the kind to narrow the candidates at once) and `holders` classes that hold it, indexed, and
/// its root.
fn search_bound_tree(name: &str, functions: usize, holders: usize) -> std::path::PathBuf {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..functions.div_ceil(10) {
        let body: String = (0..10).map(|k| format!("def token():\n    return {i} + {k}\n\n\n")).collect();
        fs::write(root.join(format!("f{i}.py")), body).unwrap();
    }
    for i in 0..100 {
        let body: String = (0..10).map(|k| format!("class Plain{i}x{k}:\n    v = {k}\n\n\n")).collect();
        fs::write(root.join(format!("c{i}.py")), body).unwrap();
    }
    let body: String = (0..holders).map(|k| format!("class Holder{k}:\n    v = \"token\"\n\n\n")).collect();
    fs::write(root.join("holders.py"), body).unwrap();
    let out = std::process::Command::new(BIN)
        .arg("index")
        .current_dir(&root)
        .env("SCRUTATE_SEMANTIC", "none")
        .env("SCRUTATE_NO_SESSION", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    root
}

/// `search token --kind class -k 10` on `root`, with no semantic model: (CLI JSON, CLI text, MCP
/// `search_code` text, its structured content from `serve --structured`).
fn filtered_search(root: &Path) -> (Value, String, String, Value) {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let run = |json: bool| {
        let mut args = vec!["search", "token", "--kind", "class", "-k", "10"];
        if json {
            args.push("--json");
        }
        let out = std::process::Command::new(BIN)
            .args(&args)
            .current_dir(root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    use std::io::{BufRead, Write};
    let mut serve = std::process::Command::new(BIN)
        .args(["--root", &root.to_string_lossy(), "serve", "--structured"])
        .env("SCRUTATE_SEMANTIC", "none")
        .env("SCRUTATE_NO_SESSION", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut input = serve.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(serve.stdout.take().unwrap());
    let initialize = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}});
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "search_code", "arguments": {"query": "token", "kinds": ["class"], "k": 10}}});
    writeln!(input, "{initialize}\n{call}").unwrap();
    let answer = (0..)
        .map_while(|_| {
            let mut line = String::new();
            (output.read_line(&mut line).unwrap() > 0).then(|| serde_json::from_str::<Value>(&line).unwrap())
        })
        .find(|v| v["id"] == json!(2))
        .unwrap();
    let _ = serve.kill();
    let _ = serve.wait();
    let result = &answer["result"];
    let text = result["content"][0]["text"].as_str().unwrap_or("").to_string();
    (serde_json::from_str(&run(true)).unwrap(), run(false), text, result["structuredContent"].clone())
}

/// No class holds the query and 5,000 functions do: the widened candidates stop at their bound
/// with none of the kind, and every form of the answer says so.
#[test]
fn a_filtered_search_that_finds_none_by_its_bound_says_it_stopped_there() {
    let root = search_bound_tree("search_bound_none", 5000, 0);
    let (json, text, mcp, structured) = filtered_search(&root);
    let _ = fs::remove_dir_all(&root);
    let stop = scrutate::search::FILTER_STOP;
    assert!(json["result"].as_array().is_none_or(Vec::is_empty) && json["stopped_reason"] == json!(stop), "{json}");
    assert!(text.contains(stop), "{text}");
    assert!(mcp.contains(stop) && structured["stopped_reason"] == json!(stop), "{mcp}\n{structured}");
}

/// Three classes hold the query and 200 functions do: the candidates run out before the bound, so
/// the answer holds the three and says nothing of a bound.
#[test]
fn a_filtered_search_that_runs_out_before_its_bound_says_nothing_of_it() {
    let root = search_bound_tree("search_bound_short", 200, 3);
    let (json, text, mcp, structured) = filtered_search(&root);
    let _ = fs::remove_dir_all(&root);
    let stop = scrutate::search::FILTER_STOP;
    assert!(json["result"].as_array().map_or(0, Vec::len) == 3 && json["stopped_reason"].is_null(), "{json}");
    assert!(!text.contains(stop) && text.contains("Holder"), "{text}");
    assert!(!mcp.contains(stop) && structured["stopped_reason"].is_null(), "{mcp}\n{structured}");
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
    assert!(text.contains("10000 occurrences were read") && !text.contains("raise max_results"), "{text}");
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

/// `references --def` counts max_results over the occurrences of the definition it names, never
/// over every occurrence of the name: 400 files define and use a function `Thing`, then a class
/// `Thing` used 7 times; its answer holds min(M, all of its occurrences) at every M.
#[test]
fn references_def_counts_the_occurrences_of_that_definition() {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_refs_def_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("zz")).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..400 {
        fs::write(root.join(format!("f{i}.py")), format!("def Thing():\n    return {i}\n\n\ndef use_{i}():\n    Thing()\n    return Thing()\n")).unwrap();
    }
    let uses: String = (0..7).map(|_| "    Thing()\n").collect();
    fs::write(root.join("zz/last.py"), format!("class Thing:\n    pass\n\n\ndef use_last():\n{uses}")).unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(BIN)
            .args(args)
            .current_dir(&root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        serde_json::from_slice::<Value>(&out.stdout).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&out.stdout)))
    };
    let count =
        |v: &Value| -> usize { v["result"]["definitions"].as_array().into_iter().flatten().map(|d| d["references"].as_array().map_or(0, Vec::len)).sum() };
    std::process::Command::new(BIN).arg("index").current_dir(&root).env("SCRUTATE_SEMANTIC", "none").status().unwrap();
    let full = count(&run(&["refs", "Thing", "--def", "zz/last.py", "--max-results", "5000", "--json"]));
    assert!(full >= 7, "{full}");
    for m in [1usize, 3, 7, 8, 50, 200] {
        let ms = m.to_string();
        let got = count(&run(&["refs", "Thing", "--def", "zz/last.py", "--max-results", &ms, "--json"]));
        assert_eq!(got, full.min(m), "--max-results {m}");
        let got = count(&run(&["refs", "Thing", "--def", "zz/last.py", "-g", "**/*.py", "--max-results", &ms, "--json"]));
        assert_eq!(got, full.min(m), "-g, --max-results {m}");
    }
    // the answer counts the occurrences tied to other definitions and to none
    let text = String::from_utf8_lossy(
        &std::process::Command::new(BIN)
            .args(["refs", "Thing", "--def", "zz/last.py"])
            .current_dir(&root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(text.contains("other occurrences resolve to other definitions, 0 are unresolved"), "{text}");
    // more than 10,000 other uses come first: the read stops there with none of the class's kept, and
    // says it stopped, in text and JSON, never as if there were none
    let calls: String = (0..10_050).map(|_| "    Thing()\n").collect();
    fs::write(root.join("a_many.py"), format!("def Thing():\n    return 0\n\n\ndef many():\n{calls}")).unwrap();
    std::process::Command::new(BIN).arg("index").current_dir(&root).env("SCRUTATE_SEMANTIC", "none").status().unwrap();
    let text = String::from_utf8_lossy(
        &std::process::Command::new(BIN)
            .args(["refs", "Thing", "--def", "zz/last.py", "--max-results", "5"])
            .current_dir(&root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_NO_SESSION", "1")
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(text.contains("10000 occurrences were read"), "{text}");
    let v = run(&["refs", "Thing", "--def", "zz/last.py", "--max-results", "5", "--json"]);
    assert!(
        v["result"]["truncated"] == json!(true) && v["result"]["stopped_reason"].as_str().is_some_and(|r| r.contains("narrow the name or the files")),
        "{v}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// One kept occurrence past max_results, the last one read, still marks the answer cut: a method
/// a0 with its definition line and 7 uses, after 40 uses of a function a0, at --max-results 7, by
/// kind, by --def, and by --def with a glob.
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
    for args in [&["--kind", "method"][..], &["--def", "m.py:2"], &["--def", "m.py:2", "-g", "**/*.py"]] {
        let all: Vec<&str> = ["refs", "a0"].into_iter().chain(args.iter().copied()).chain(["--max-results", "5000", "--json"]).collect();
        let full = run(&all);
        assert!(count(&full) == 8 && full["result"]["truncated"] == json!(false), "{args:?} whole: {full}");
        let cut: Vec<&str> = ["refs", "a0"].into_iter().chain(args.iter().copied()).chain(["--max-results", "7", "--json"]).collect();
        let v = run(&cut);
        assert!(count(&v) == 7 && v["result"]["truncated"] == json!(true), "{args:?} at 7: {v}");
    }
    let _ = fs::remove_dir_all(&root);
}

/// A narrowed read (`--def`, a kind) whose only uses are unresolved shows them, and never reads as
/// unused: `x.go()` on an untyped `x` may be either class's `go`, in text, JSON and MCP.
#[test]
fn a_narrowed_answer_shows_the_unresolved_uses() {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_refs_unresolved_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("m.py"), "class A:\n    def go(self):\n        return 1\n\n\nclass B:\n    def go(self):\n        return 2\n").unwrap();
    fs::write(root.join("u.py"), "def use(x):\n    return x.go()\n").unwrap();
    std::process::Command::new(BIN).arg("index").current_dir(&root).env("SCRUTATE_SEMANTIC", "none").status().unwrap();
    let run = |args: &[&str]| -> String {
        let out =
            std::process::Command::new(BIN).args(args).current_dir(&root).env("SCRUTATE_SEMANTIC", "none").env("SCRUTATE_NO_SESSION", "1").output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    for args in [&["--def", "m.py:2"][..], &["--kind", "method"]] {
        let all: Vec<&str> = ["refs", "go"].into_iter().chain(args.iter().copied()).collect();
        let text = run(&all);
        assert!(text.contains("unresolved (1):") && text.contains("u.py:2:"), "{args:?}: {text}");
        if args[0] == "--def" {
            assert!(text.contains("(1 other occurrences resolve to other definitions, 1 are unresolved)"), "{text}");
        }
        let json: Vec<&str> = all.iter().copied().chain(["--json"]).collect();
        let v: Value = serde_json::from_str(&run(&json)).unwrap();
        let u = &v["result"]["unresolved"];
        assert!(u.as_array().is_some_and(|a| a.len() == 1 && a[0]["path"] == "u.py") && v["result"]["unresolved_count"] == 1, "{args:?}: {v}");
    }
    let settings = scrutate::mcp::Settings::new(None);
    let cancel = Arc::default();
    let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
    for args in [json!({"name": "go", "def": "m.py:2"}), json!({"name": "go", "kinds": ["method"]})] {
        let r = scrutate::mcp::call_tool(&session, "references", &args, &settings, &cancel);
        assert!(r.text.contains("u.py:2:"), "{args}: {}", r.text);
        assert_eq!(r.structured["result"]["unresolved"].as_array().map(Vec::len), Some(1), "{args}: {}", r.structured);
    }
    let _ = fs::remove_dir_all(&root);
}

/// A narrowed answer at max_results one below, at and one above a definition's own count is the
/// first of its whole answer, cut exactly when more exist: uses through an alias of another
/// definition (`tb()`) never take a slot (Reviewer 4's shape), and a constructor's uses are its
/// class's instantiations under --def too.
#[test]
fn an_alias_or_a_constructor_of_another_definition_never_takes_a_slot() {
    const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("limit_refs_alias_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let files = [
        ("a.py", "def thing():\n    return 1\n".to_string()),
        ("b.py", "class K:\n    pass\n\n\ndef thing():\n    return 2\n".to_string()),
        ("ua.py", format!("from a import thing as ta\n\n\ndef use():\n{}", "    ta()\n".repeat(3))),
        ("ub.py", format!("from b import thing as tb\n\n\ndef use():\n{}", "    tb()\n".repeat(4))),
        ("da.py", "from a import thing\n\n\ndef use():\n    thing()\n".to_string()),
        ("P.java", "class P {\n    P() {\n    }\n}\n".to_string()),
        ("Main.java", "class Main {\n    void f() {\n        new P();\n        new P();\n    }\n}\n".to_string()),
    ];
    for (p, t) in &files {
        fs::write(root.join(p), t).unwrap();
    }
    std::process::Command::new(BIN).arg("index").current_dir(&root).env("SCRUTATE_SEMANTIC", "none").status().unwrap();
    let run = |args: Vec<String>| -> Value {
        let out =
            std::process::Command::new(BIN).args(&args).current_dir(&root).env("SCRUTATE_SEMANTIC", "none").env("SCRUTATE_NO_SESSION", "1").output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {args:?}: {}", String::from_utf8_lossy(&out.stdout)))
    };
    let rows = |v: &Value| -> Vec<String> {
        v["result"]["definitions"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|d| d["references"].as_array().into_iter().flatten())
            .map(|o| format!("{}:{}:{}", o["path"].as_str().unwrap_or(""), o["line"], o["column"]))
            .collect()
    };
    let mut failed = Vec::new();
    for (name, args) in [
        ("thing", vec!["--def", "a.py"]),
        ("thing", vec!["--def", "b.py"]),
        ("thing", vec!["--kind", "function"]),
        ("thing", vec![]),
        ("P", vec!["--def", "P.java:2"]),
    ] {
        let with = |m: usize| -> Vec<String> {
            ["refs", name]
                .iter()
                .map(|s| s.to_string())
                .chain(args.iter().map(|s| s.to_string()))
                .chain(["--max-results".into(), m.to_string(), "--json".into()])
                .collect()
        };
        let whole = rows(&run(with(5000)));
        // the constructor's own line and the two `new P()`
        if name == "P" && whole.len() != 3 {
            failed.push(format!("the constructor's uses: {whole:?}"));
        }
        for m in [whole.len().saturating_sub(1).max(1), whole.len(), whole.len() + 1] {
            let v = run(with(m));
            let (got, cut) = (rows(&v), v["result"]["truncated"] == json!(true));
            if got[..] != whole[..whole.len().min(m)] || cut != (whole.len() > m) {
                failed.push(format!("{name} {args:?} at {m}: {got:?} truncated {cut}, whole {whole:?}"));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    let _ = fs::remove_dir_all(&root);
}
