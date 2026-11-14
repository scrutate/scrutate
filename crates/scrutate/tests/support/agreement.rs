//! Whether `refs`, `callers` and `callees` give one answer about the calls of an indexed tree.
//!
//! The resolver test files run it on every tree they build, so each fixture is covered without
//! a list. With `SCRUTATE_AGREEMENT_LOG` naming a file, each disagreement is appended to it as one
//! line; with `SCRUTATE_AGREEMENT=assert`, the first tree with one fails its test.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

/// Check the indexed tree `root` (built for `fixture`) as the environment asks.
pub fn check(bin: &str, root: &Path, fixture: &str) {
    let log = std::env::var_os("SCRUTATE_AGREEMENT_LOG");
    let assert = std::env::var("SCRUTATE_AGREEMENT").is_ok_and(|v| v == "assert");
    if log.is_none() && !assert {
        return;
    }
    let found = disagreements(bin, root);
    if let Some(log) = log {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            for d in &found {
                let _ = writeln!(f, "{fixture}\t{d}");
            }
        }
    }
    assert!(!assert || found.is_empty(), "{fixture}: refs, callers and callees disagree:\n{}", found.join("\n"));
}

struct Def {
    path: String,
    start: u64,
    end: u64,
    kind: String,
    name: String,
    qualname: String,
}

fn run(bin: &str, root: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(bin).args(args).current_dir(root).stdin(Stdio::null()).output().expect("run scrutate");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
}

fn json(bin: &str, root: &Path, args: &[&str]) -> Option<Value> {
    let (_, out) = run(bin, root, args);
    let v: Value = serde_json::from_str(&out).ok()?;
    Some(if v["schema"] == 2 { v["result"].clone() } else { v })
}

fn files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == ".scrutate" || n == ".git") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

/// The callers `callers SPEC` lists, as (path, start, end, decided).
fn callers(bin: &str, root: &Path, spec: &str) -> Vec<(String, u64, u64, bool)> {
    let (_, out) = run(bin, root, &["callers", spec, "--limit", "100000", "--max-bytes", "1000000"]);
    let mut rows: Vec<(String, u64, u64)> = Vec::new();
    for l in out.lines() {
        let Some((loc, _)) = l.split_once("  [") else { continue };
        let Some((path, span)) = loc.rsplit_once(':') else { continue };
        let Some((a, b)) = span.split_once('-') else { continue };
        if let (Ok(a), Ok(b)) = (a.parse(), b.parse()) {
            rows.push((path.to_string(), a, b));
        }
    }
    let undecided = if out.contains("could not tell whether any") {
        rows.len()
    } else {
        out.split_once(" of these call").and_then(|(a, _)| a.rsplit('(').next()?.parse().ok()).unwrap_or(0)
    };
    let decided = rows.len().saturating_sub(undecided);
    rows.into_iter().enumerate().map(|(i, (p, a, b))| (p, a, b, i < decided)).collect()
}

/// Every disagreement between `refs`, `callers` and `callees` on the indexed tree `root`.
pub fn disagreements(bin: &str, root: &Path) -> Vec<String> {
    let mut defs: Vec<Def> = Vec::new();
    for f in files(root) {
        let Some(v) = json(bin, root, &["outline", &f, "--json"]) else { continue };
        for d in v.as_array().into_iter().flatten() {
            let s = |k: &str| d[k].as_str().unwrap_or("").to_string();
            let n = |k: &str| d[k].as_u64().unwrap_or(0);
            defs.push(Def { path: f.clone(), start: n("start"), end: n("end"), kind: s("kind"), name: s("name"), qualname: s("qualname") });
        }
    }
    // the innermost definition around a line, as `callers` names the code a call sits in
    let around = |path: &str, line: u64| {
        defs.iter().filter(|d| d.path == path && d.start <= line && line <= d.end && d.kind != "module").min_by_key(|d| d.end - d.start)
    };
    let mut refs_of: HashMap<String, Value> = HashMap::new();
    let mut callees_of: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut out = Vec::new();
    for x in defs.iter().filter(|d| matches!(d.kind.as_str(), "function" | "method")) {
        let bare = x.name.rsplit('.').next().unwrap_or(&x.name).to_string();
        // `path:line` names the outermost definition starting there: on a shared line, the qualified name
        let shared = defs.iter().filter(|d| d.path == x.path && d.start == x.start && d.kind != "module").count() > 1;
        let spec = if shared { x.qualname.clone() } else { format!("{}:{}", x.path, x.start) };
        let refs = refs_of
            .entry(bare.clone())
            .or_insert_with(|| json(bin, root, &["refs", &bare, "--json", "--max-results", "1000000", "--max-bytes", "1000000"]).unwrap_or(Value::Null));
        let group = refs["definitions"].as_array().into_iter().flatten().find(|g| g["path"] == x.path.as_str() && g["start"].as_u64() == Some(x.start));
        // the uses refs ties to X, past its own name (a call through an import is tied "import" too)
        let tied: Vec<(String, u64, String, u64)> = group
            .and_then(|g| g["references"].as_array())
            .into_iter()
            .flatten()
            .filter(|r| r["via"].as_str() != Some("definition"))
            .map(|r| {
                (
                    r["path"].as_str().unwrap_or("").to_string(),
                    r["line"].as_u64().unwrap_or(0),
                    r["text"].as_str().unwrap_or("").to_string(),
                    r["column"].as_u64().unwrap_or(1),
                )
            })
            .collect();
        let inside = |p: &str, a: u64, b: u64| tied.iter().any(|(rp, l, _, _)| rp == p && a <= *l && *l <= b);
        let listed = callers(bin, root, &spec);
        for (p, a, b, decided) in &listed {
            if *decided && !inside(p, *a, *b) {
                out.push(format!("decided-caller-not-in-refs\t{} ({spec})\tcallers: {p}:{a}-{b} decided; refs ties no use of it there", x.qualname));
            }
            if !*decided && inside(p, *a, *b) {
                out.push(format!("undecided-caller-tied-by-refs\t{} ({spec})\tcallers: {p}:{a}-{b} undecided; refs ties a use there", x.qualname));
            }
            if *decided {
                let key = format!("{p}:{a}");
                let resolved = callees_of.entry(key.clone()).or_insert_with(|| {
                    let v = json(bin, root, &["callees", &key, "--json", "--limit", "100000", "--max-bytes", "1000000"]).unwrap_or(Value::Null);
                    v["resolved"].as_array().into_iter().flatten().filter_map(|h| h["id"].as_str().map(str::to_string)).collect()
                });
                // a constructor's call is a call of its class, which callees names instead
                let owner = x.qualname.rsplit_once('.').map(|(o, _)| o);
                let class = defs.iter().find(|d| Some(d.qualname.as_str()) == owner && d.path == x.path).map(|d| format!("{}:{}-{}", d.path, d.start, d.end));
                if !resolved.contains(&format!("{}:{}-{}", x.path, x.start, x.end)) && !class.is_some_and(|c| resolved.contains(&c)) {
                    out.push(format!("callee-missing\t{} ({spec})\tcallers lists {key} decided; callees {key} does not list {spec}", x.qualname));
                }
            }
        }
        // a call refs ties to X sits in code callers should list
        for (p, l, text, col) in &tied {
            let rest = text.get((*col as usize).saturating_sub(1)..).unwrap_or("");
            let call = rest.strip_prefix(bare.as_str()).is_some_and(|r| r.trim_start().starts_with('('));
            let Some(f) = around(p, *l).filter(|_| call) else { continue };
            if !listed.iter().any(|(cp, a, b, _)| cp == &f.path && *a == f.start && *b == f.end) {
                out.push(format!(
                    "call-in-refs-not-in-callers\t{} ({spec})\trefs ties the call at {p}:{l} in {}; callers does not list it",
                    x.qualname, f.qualname
                ));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
