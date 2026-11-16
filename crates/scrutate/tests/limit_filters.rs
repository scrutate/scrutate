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
