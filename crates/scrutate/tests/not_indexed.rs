//! A file the code index leaves out for its size is said wherever an answer from that index
//! depends on it: the index's own reports, a lookup that found nothing or names the file, the
//! listings of the whole repository, references, impact and review; on the command line and
//! through the MCP tools. A file at the bound is indexed.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
const NOTE: &str = "1 file over 1 MB is not in the index: big.py";
const NAMED: &str = "big.py is not in the index (over 1 MB): read it by lines, or search it with grep";

/// A repository of small.py (which calls big_marker), big.py one byte over the bound with
/// big_marker first, and at.py exactly at the bound; indexed, and committed.
fn repo(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("not_indexed_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("small.py"), "def small_marker():\n    return 1\n\n\ndef other():\n    return big_marker()\n").unwrap();
    let fill = |head: &str, size: usize| {
        let mut s = head.to_string();
        while s.len() + 9 <= size {
            s.push_str("x = 1  #\n");
        }
        s.push_str(&"#".repeat(size - s.len() - 1));
        s.push('\n');
        assert_eq!(s.len(), size);
        s
    };
    fs::write(root.join("big.py"), fill("def big_marker():\n    return small_marker()\n\n\n", 1_000_001)).unwrap();
    fs::write(root.join("at.py"), fill("def at_marker():\n    return 2\n\n\n", 1_000_000)).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-qm", "base"]);
    let root = canon(&root);
    assert_eq!(run(&root, &["index"]).0, 0);
    root
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main", "-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", if cfg!(windows) { "NUL" } else { "/dev/null" })
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// (exit code, stdout, stderr)
fn run(root: &Path, args: &[&str]) -> (i32, String, String) {
    let home = root.with_extension("home");
    let out = Command::new(BIN)
        .args(args)
        .current_dir(root)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("LOCALAPPDATA", home.join("state"))
        .env("SCRUTATE_SEMANTIC", "none")
        .env("SCRUTATE_NO_SESSION", "1")
        .env("SCRUTATE_CACHE_DIR", root.with_extension("cache"))
        .env_remove("SCRUTATE_REDACT")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn field(v: &Value) -> Value {
    v["not_indexed"].clone()
}

#[test]
fn the_index_reports_say_a_file_over_the_bound_and_index_one_at_it() {
    let root = repo("reports");
    let (_, out, _) = run(&root, &["index"]);
    assert!(out.contains(&format!("note: {NOTE}")), "{out}");
    let (_, out, _) = run(&root, &["check", "--files"]);
    assert!(out.contains(&format!("note: {NOTE}")), "{out}");
    let check: Value = serde_json::from_str(&run(&root, &["check", "--json"]).1).unwrap();
    assert_eq!(field(&check), json!([{"reason": "over 1 MB", "count": 1, "paths": ["big.py"]}]), "{check}");
    let stats: Value = serde_json::from_str(&run(&root, &["stats"]).1).unwrap();
    assert_eq!(field(&stats), json!([{"reason": "over 1 MB", "count": 1, "paths": ["big.py"]}]), "{stats}");
    // the file at the bound is in the index
    let (code, out, _) = run(&root, &["symbol", "at_marker"]);
    assert!(code == 0 && out.contains("at.py"), "{code} {out}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn every_lookup_says_what_the_index_leaves_out_when_it_found_nothing_or_names_it() {
    let root = repo("lookups");
    let says = |args: &[&str], want: &str| {
        let (code, out, err) = run(&root, args);
        assert!(out.contains(want) || err.contains(want), "{args:?}: {code}\n{out}\n{err}");
    };
    // found nothing: the note follows the empty answer
    says(&["symbol", "big_marker"], NOTE);
    says(&["callers", "big_marker"], NOTE);
    says(&["callees", "big_marker"], NOTE);
    says(&["read", "--symbol", "big_marker"], NOTE);
    says(&["path", "big_marker", "small_marker"], NOTE);
    // names the file: the answer says why it is not in the index and what reads it
    says(&["outline", "big.py"], NAMED);
    says(&["read", "big.py", "--symbol", "big_marker"], NAMED);
    says(&["search", "big_marker", "-g", "big.py"], NAMED);
    // a found answer that names no left-out file stays as it is
    let (_, out, _) = run(&root, &["symbol", "small_marker"]);
    assert!(!out.contains("not in the index"), "{out}");
    // the listings of the whole repository always say it
    for args in [&["dupes"][..], &["dead"], &["map"]] {
        says(args, NOTE);
    }
    // references: an occurrence in the left-out file is labelled so, never "not an identifier"
    let (_, out, _) = run(&root, &["refs", "big_marker"]);
    assert!(out.contains("big.py:1:5") && out.contains("in a file that is not indexed (over 1 MB)") && !out.contains("not an identifier"), "{out}");
    // --json carries the field
    let v: Value = serde_json::from_str(&run(&root, &["symbol", "big_marker", "--json"]).1).unwrap();
    assert_eq!(field(&v), json!([{"reason": "over 1 MB", "count": 1, "paths": ["big.py"]}]), "{v}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn impact_and_review_name_a_changed_file_that_is_not_indexed() {
    let root = repo("impact");
    let big = fs::read_to_string(root.join("big.py")).unwrap();
    fs::write(root.join("big.py"), big.replacen("return small_marker()", "return small_marker() + 1", 1)).unwrap();
    let (_, out, _) = run(&root, &["impact"]);
    assert!(out.contains("big.py  [file]  (edited; not indexed: over 1 MB)"), "{out}");
    let v: Value = serde_json::from_str(&run(&root, &["impact", "--json"]).1).unwrap();
    assert!(v["result"]["changed"].as_array().unwrap().iter().any(|c| c["path"] == "big.py" && c["not_indexed"] == "over 1 MB"), "{v}");
    git(&root, &["commit", "-qam", "edit the big file"]);
    let (_, out, _) = run(&root, &["review", "--base", "HEAD~1"]);
    assert!(out.contains("1 changed file not indexed (over 1 MB)") && out.contains("not indexed: over 1 MB"), "{out}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_mcp_tools_say_it_in_their_text_and_their_envelope() {
    let root = repo("mcp");
    let session = scrutate::session::Session::open(&root, scrutate::session::Every::given(Some(0.0))).unwrap();
    session.set_cache_dir(root.with_extension("cache"));
    let settings = scrutate::mcp::Settings::new(None);
    for (tool, args, want) in [
        ("find_symbol", json!({"name": "big_marker"}), NOTE),
        ("callers", json!({"name": "big_marker"}), NOTE),
        ("callees", json!({"name": "big_marker"}), NOTE),
        ("read_code", json!({"symbol": "big_marker"}), NOTE),
        ("call_path", json!({"from": "big_marker", "to": "small_marker"}), NOTE),
        ("outline", json!({"path": "big.py"}), NAMED),
        ("search_code", json!({"query": "big_marker", "globs": ["big.py"]}), NAMED),
        ("references", json!({"name": "big_marker"}), "in a file that is not indexed (over 1 MB)"),
        ("duplicates", json!({}), NOTE),
        ("dead_code", json!({}), NOTE),
        ("repo_map", json!({}), NOTE),
    ] {
        let c = scrutate::mcp::call_tool(&session, tool, &args, &settings, &Default::default());
        assert!(c.text.contains(want), "{tool} {args}: {}", c.text);
    }
    let c = scrutate::mcp::call_tool(&session, "find_symbol", &json!({"name": "big_marker"}), &settings, &Default::default());
    assert_eq!(field(&c.structured), json!([{"reason": "over 1 MB", "count": 1, "paths": ["big.py"]}]), "{}", c.structured);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn batch_and_a_second_repository_s_answer_carry_it() {
    let root = repo("batch");
    fs::write(root.with_extension("ops.jsonl"), "{\"op\": \"find_symbol\", \"name\": \"big_marker\"}\n{\"query\": \"zz_no_such_thing_qq\"}\n").unwrap();
    let (_, out, err) = run(&root, &["batch", root.with_extension("ops.jsonl").to_str().unwrap()]);
    let lines: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{out}\n{err}");
    for l in &lines {
        assert_eq!(field(l), json!([{"reason": "over 1 MB", "count": 1, "paths": ["big.py"]}]), "{l}");
    }
    // --also: the other repository's answer says what its own index leaves out
    let other = repo("also_other");
    let base = root.with_extension("also");
    fs::create_dir_all(base.join(".git")).unwrap();
    let base = canon(&base);
    fs::write(base.join("a.py"), "def nothing_here():\n    return 0\n").unwrap();
    run(&base, &["index"]);
    let also = other.to_str().unwrap();
    let (_, out, _) = run(&base, &["search", "zz_no_such_thing_qq", "--also", also]);
    assert!(out.contains(&format!("in {also}: {NOTE}")), "{out}");
    let v: Value = serde_json::from_str(&run(&base, &["search", "zz_no_such_thing_qq", "--also", also, "--json"]).1).unwrap();
    assert!(
        field(&v)
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["paths"] == json!(["big.py"]) && r["root"].as_str().is_some_and(|p| p.contains("not_indexed_also_other"))),
        "{v}"
    );
    for d in [root, other, base] {
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn a_dependency_s_answer_says_what_its_own_index_leaves_out() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("not_indexed_deps_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("node_modules/leftpad")).unwrap();
    fs::write(root.join("a.js"), "function main() {\n  return 1;\n}\n").unwrap();
    fs::write(root.join("package-lock.json"), r#"{"lockfileVersion": 3, "packages": {"": {}, "node_modules/leftpad": {"version": "1.0.0"}}}"#).unwrap();
    fs::write(root.join("node_modules/leftpad/package.json"), r#"{"name": "leftpad", "version": "1.0.0"}"#).unwrap();
    fs::write(root.join("node_modules/leftpad/index.js"), "function pad(s) {\n  return s;\n}\n").unwrap();
    fs::write(root.join("node_modules/leftpad/big.js"), format!("function huge() {{\n  return 1;\n}}\n{}", "var x = 1;\n".repeat(100_000))).unwrap();
    let root = canon(&root);
    run(&root, &["index"]);
    let session = scrutate::session::Session::open(&root, scrutate::session::Every::given(Some(0.0))).unwrap();
    session.set_cache_dir(root.with_extension("cache"));
    let c = scrutate::mcp::call_tool(
        &session,
        "find_symbol",
        &json!({"name": "leftpad.huge", "scope": "deps"}),
        &scrutate::mcp::Settings::new(None),
        &Default::default(),
    );
    assert!(c.text.contains("1 file over 1 MB is not in the index") && c.text.contains("big.js"), "{}", c.text);
    assert!(field(&c.structured).as_array().is_some_and(|a| a[0]["reason"] == "over 1 MB"), "{}", c.structured);
    let _ = fs::remove_dir_all(&root);
}

/// The reason is decided by the file's content first: a file that is not text (by the index's
/// own test, a NUL in its first 4 KiB) is "not text" at any size and is never in a lookup's note;
/// "over 1 MB" is only a text file. This repository's own binaries, a large random one and a
/// text file with a NUL near its start are each named under "not text" in index, check and
/// stats, and the lookups name only the large source file.
#[test]
fn a_binary_file_is_not_text_at_any_size_and_only_large_source_is_over_the_bound() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("not_indexed_binary_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    // this repository's own five, as they are in it
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut own = vec![here.join("models/semantic.npz"), here.join("models/ranker.npz")];
    own.extend(fs::read_dir(here.join("../../fuzz/regressions/npy")).unwrap().flatten().map(|e| e.path()));
    assert_eq!(own.len(), 5, "{own:?}");
    fs::create_dir_all(root.join("own")).unwrap();
    for p in &own {
        fs::copy(p, root.join("own").join(p.file_name().unwrap())).unwrap();
    }
    // 3 MB of random bytes, a 1.2 MB source file, a 2 MB text file with a NUL in its first 4 KiB
    let mut seed = 7u64;
    let random: Vec<u8> = (0..3_000_000)
        .map(|_| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 56) as u8
        })
        .collect();
    fs::write(root.join("weights.npz"), &random).unwrap();
    fs::write(root.join("large.py"), format!("def large_marker():\n    return 1\n\n\n{}", "x = 1  #\n".repeat(133_000))).unwrap();
    let mut nul = format!("note = 1\n{}", "y = 2  #\n".repeat(222_300)).into_bytes();
    nul[100] = 0;
    fs::write(root.join("nul.txt"), nul).unwrap();
    fs::write(root.join("small.py"), "def small_marker():\n    return 1\n").unwrap();
    git(&root, &["init", "-q"]);
    let root = canon(&root);
    run(&root, &["index"]);
    let over = "1 file over 1 MB is not in the index: large.py";
    let binaries = ["nul.txt", "own/crash-", "own/ranker.npz", "own/semantic.npz", "weights.npz"];
    // lookups and listings speak of the source file alone
    for args in [&["symbol", "zz_no_such_name"][..], &["callers", "zz_no_such_name"], &["dupes"], &["dead"], &["map"]] {
        let (_, out, err) = run(&root, args);
        let all = format!("{out}{err}");
        assert!(all.contains(over) && !all.contains("not text") && !binaries.iter().any(|b| all.contains(b)), "{args:?}:\n{all}");
    }
    let v: Value = serde_json::from_str(&run(&root, &["symbol", "zz_no_such_name", "--json"]).1).unwrap();
    assert_eq!(field(&v), json!([{"reason": "over 1 MB", "count": 1, "paths": ["large.py"]}]), "{v}");
    // a named file gets its reason, and what reads it only when it is text
    let (_, out, err) = run(&root, &["outline", "own/semantic.npz"]);
    let all = format!("{out}{err}");
    assert!(
        all.contains("own/semantic.npz is not in the index (not text or could not be parsed)")
            && !all.contains("semantic.npz is not in the index (not text or could not be parsed): read"),
        "{all}"
    );
    let (_, out, err) = run(&root, &["outline", "large.py"]);
    assert!(format!("{out}{err}").contains("large.py is not in the index (over 1 MB): read it by lines, or search it with grep"), "{out}{err}");
    // index, check and stats name every one of the seven under its reason
    let (_, out, _) = run(&root, &["check"]);
    assert!(out.contains(&format!("note: {over}")), "{out}");
    let line = out.lines().find(|l| l.contains("(not text or could not be parsed)")).unwrap_or_else(|| panic!("{out}"));
    assert!(line.starts_with("note: 7 files are not in the index: ") && binaries.iter().all(|b| line.contains(b)), "{line}");
    let stats: Value = serde_json::from_str(&run(&root, &["stats"]).1).unwrap();
    let reasons = field(&stats);
    assert_eq!(reasons[0], json!({"reason": "over 1 MB", "count": 1, "paths": ["large.py"]}), "{stats}");
    assert!(reasons[1]["reason"] == "not text or could not be parsed" && reasons[1]["count"] == 7, "{stats}");
    let _ = fs::remove_dir_all(&root);
}
