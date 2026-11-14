//! End-to-end tests of the real binary: exit codes, JSON schema, configuration precedence, the
//! unified `find`, cross-repository search, `check`, compaction, the MCP protocol, and the
//! edge cases the traceability matrix promises (UTF-16 BOM, `.git` files, no Python at runtime).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/conforms.rs"]
mod schema;
use schema::conforms;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_cli_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Repo { root }
    }
    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
    }
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        run_in(&self.root, args, &[])
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_in(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(cwd).stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("run scrutate");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Output lines with the summary's run time removed (it differs on every run).
fn untimed((code, out, err): (i32, String, String)) -> (i32, Vec<String>, String) {
    let out = out
        .lines()
        .map(|l| match l.find("\"elapsed_ms\":") {
            Some(i) => format!("{}{}", &l[..i], &l[i..][l[i..].find(',').unwrap_or(l.len() - i)..]),
            None => l.to_string(),
        })
        .collect();
    (code, out, err)
}

fn python_repo(name: &str) -> Repo {
    let r = Repo::new(name);
    for i in 0..12 {
        r.write(
            &format!("pkg/m{i}.py"),
            format!("\"\"\"Module {i}: widgets.\"\"\"\n\nimport os\nfrom pkg.m{} import make_widget_{}\n\n\ndef make_widget_{i}(size):\n    \"\"\"Build widget number {i} with the given size.\"\"\"\n    return os.path.join(\"w\", str(size))\n\n\ndef use_{i}():\n    return make_widget_{i}(1)\n", (i + 1) % 12, (i + 1) % 12).as_bytes(),
        );
    }
    r.write("README.md", b"# demo\n\nSome docs mention widgets.\n");
    r
}

#[test]
fn exit_codes_and_plain_output() {
    let r = python_repo("exit");
    let (code, out, _) = r.run(&["grep", "-F", "import os", "."]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 12);
    assert!(out.lines().all(|l| l.starts_with("./pkg/m") && l.contains(":3:import os")), "{out}");
    let (code, out, _) = r.run(&["grep", "-F", "nothing-like-this", "."]);
    assert_eq!((code, out.as_str()), (1, ""));
    let (code, _, err) = r.run(&["grep", "(unclosed", "."]);
    assert_eq!(code, 2);
    assert!(err.contains("error"), "{err}");
    let (code, _, _) = r.run(&["definitely-not-a-command"]);
    assert_eq!(code, 2);
}

#[test]
fn a_root_that_is_no_directory_is_an_error_for_files_and_tsquery_too() {
    let r = Repo::new("badroot");
    r.write("a.py", b"x = 1\n");
    for root in ["nope", "a.py"] {
        for args in [&["files", "x"][..], &["tsquery", "python", "(identifier) @i"]] {
            let (code, out, err) = r.run(&[&["--root", root][..], args].concat());
            assert_eq!(code, 2, "--root {root} {args:?}: {out}{err}");
            assert!(err.contains(if root == "nope" { "no such directory" } else { "not a directory" }), "{err}");
        }
    }
}

#[test]
fn json_lines_schema_and_modes() {
    let r = python_repo("json");
    let (code, out, _) = r.run(&["grep", "-F", "--json", "--sort", "import os", "."]);
    assert_eq!(code, 0);
    let recs: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).expect("json line")).collect();
    let matches: Vec<&serde_json::Value> = recs.iter().filter(|r| r["type"] == "match").collect();
    assert_eq!(matches.len(), 12);
    for m in &matches {
        assert_eq!(m["schema"], 1);
        assert_eq!(m["line"], 3);
        assert!(m["spans"].is_array() && m["column"] == 1, "{m}");
    }
    let summary = recs.iter().find(|r| r["type"] == "summary").expect("summary record");
    assert_eq!(summary["matches"], 12);
    assert_eq!(summary["files_matched"], 12);
    // sorted output is deterministic: two runs, identical match records (the summary carries timings)
    let (_, out2, _) = r.run(&["grep", "-F", "--json", "--sort", "import os", "."]);
    let strip = |s: &str| s.lines().filter(|l| !l.contains("\"summary\"")).collect::<Vec<_>>().join("\n");
    assert_eq!(strip(&out), strip(&out2));
    // counts and file lists
    let (_, out, _) = r.run(&["grep", "-F", "-c", "widget", "."]);
    assert!(out.lines().all(|l| l.rsplit_once(':').map(|(_, n)| n.parse::<u64>().is_ok()).unwrap_or(false)), "{out}");
    let (_, out, _) = r.run(&["grep", "-F", "-l", "widget", "."]);
    assert_eq!(out.lines().count(), 13);
    // identifier mode finds every casing of the whole identifier only
    let (_, out, _) = r.run(&["grep", "--ident", "makeWidget3", "."]);
    assert!(out.lines().all(|l| l.contains("make_widget_3")) && out.lines().count() >= 2, "{out}");
}

#[cfg(unix)]
#[test]
fn a_repository_s_own_config_file_cannot_make_grep_follow_links() {
    let r = python_repo("config_follow");
    let outside = r.root.parent().unwrap().join(format!("{}_outside", r.root.file_name().unwrap().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&outside);
    std::fs::create_dir_all(outside.join("keys")).unwrap();
    std::fs::write(outside.join("keys/creds.env"), "SECRET_TOKEN=hunter2\n").unwrap();
    std::os::unix::fs::symlink(&outside, r.root.join("vendor_link")).unwrap();
    r.write(".scrutate.toml", b"[grep]\nfollow = true\n");
    let (code, out, err) = r.run(&["grep", "SECRET_TOKEN", "."]);
    assert_eq!((code, out.as_str()), (1, ""), "{err}");
    assert!(err.contains("follow") && err.contains("ignored"), "{err}");
    // the user's own word still follows: the flag, the environment, a file named by SCRUTATE_CONFIG
    let (_, out, _) = r.run(&["grep", "-L", "SECRET_TOKEN", "."]);
    assert!(out.contains("creds.env"), "{out}");
    let (_, out, _) = run_in(&r.root, &["grep", "SECRET_TOKEN", "."], &[("SCRUTATE_GREP_FOLLOW", "true")]);
    assert!(out.contains("creds.env"), "{out}");
    let own = outside.join("own.toml");
    std::fs::write(&own, "[grep]\nfollow = true\n").unwrap();
    let (_, out, err) = run_in(&r.root, &["grep", "SECRET_TOKEN", "."], &[("SCRUTATE_CONFIG", own.to_str().unwrap())]);
    assert!(out.contains("creds.env") && !err.contains("ignored"), "{out} {err}");
    // `follow = false` in the repository's file is its right
    r.write(".scrutate.toml", b"[grep]\nfollow = false\n");
    let (_, _, err) = r.run(&["grep", "SECRET_TOKEN", "."]);
    assert!(!err.contains("ignored"), "{err}");
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn config_file_sets_defaults_but_command_line_wins() {
    let r = python_repo("config");
    r.write(".hidden/h.py", b"def hidden_fn():\n    pass\n");
    r.write(".scrutate.toml", b"[grep]\nhidden = true\nglobs = [\"!pkg/**\"]\n");
    let (code, out, err) = r.run(&["grep", "-F", "-l", "def ", "."]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(".hidden/h.py") && !out.contains("pkg/"), "config applied: {out}");
    // the command line overrides the file (an explicit glob re-including pkg)
    let (_, out, _) = r.run(&["grep", "-F", "-l", "-g", "pkg/**", "def ", "."]);
    assert!(out.contains("pkg/m1.py"), "{out}");
    // an environment variable beats the file, the command line beats both
    let (_, out, _) = run_in(&r.root, &["grep", "-F", "-l", "def ", "."], &[("SCRUTATE_GREP_HIDDEN", "false")]);
    assert!(!out.contains(".hidden/"), "env beats file: {out}");
    let (_, out, _) = run_in(&r.root, &["grep", "-F", "-l", "--hidden", "def ", "."], &[("SCRUTATE_GREP_HIDDEN", "false")]);
    assert!(out.contains(".hidden/"), "flag beats env: {out}");
    // unknown keys are rejected, loudly
    r.write(".scrutate.toml", b"[grep]\nhiden = true\n");
    let (code, _, err) = r.run(&["grep", "-F", "x", "."]);
    assert_eq!(code, 2);
    assert!(err.contains("hiden"), "{err}");
    r.write(".scrutate.toml", b"[grep]\nhidden = true\n");
    let (code, out, _) = r.run(&["config"]);
    assert_eq!(code, 0);
    assert!(out.contains(".scrutate.toml") && out.contains("SCRUTATE_GREP_HIDDEN=true"), "{out}");
}

/// The JSON form of an index command's answer, out of the envelope every answer leaves in.
fn answer(out: &str) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert_eq!(v["schema"], 2, "{out}");
    v["result"].take()
}

#[test]
fn index_search_symbol_refs_outline_deps() {
    let r = python_repo("intel");
    let (code, out, _) = r.run(&["index"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("13 files"), "{out}");
    let (code, out, _) = r.run(&["search", "build a widget with a size", "-k", "3", "--json"]);
    assert_eq!(code, 0);
    let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    assert!(!hits.is_empty() && hits[0]["name"].as_str().unwrap().starts_with("make_widget_"), "{out}");
    let (code, out, _) = r.run(&["symbol", "make_widget_4"]);
    assert_eq!(code, 0);
    assert!(out.contains("pkg/m4.py"), "{out}");
    let (code, out, _) = r.run(&["callers", "make_widget_4", "--json"]);
    assert_eq!(code, 0);
    let callers: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    let names: Vec<&str> = callers.iter().map(|h| h["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"use_4"), "{names:?}");
    let (code, out, _) = r.run(&["refs", "make_widget_4", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_value(answer(&out)).unwrap();
    assert_eq!(v["definitions"].as_array().unwrap().len(), 1);
    assert!(v["definitions"][0]["references"].as_array().unwrap().len() >= 2, "{out}");
    let (code, out, _) = r.run(&["refs", "make_widget_4", "--textual", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_value(answer(&out)).unwrap();
    assert!(v["references"].as_array().unwrap().len() >= 2, "{out}");
    let (code, out, _) = r.run(&["outline", "pkg/m4.py"]);
    assert_eq!(code, 0);
    assert!(out.contains("make_widget_4") && out.contains("use_4"), "{out}");
    let (code, out, _) = r.run(&["deps", "pkg/m4.py"]);
    assert_eq!(code, 0);
    assert!(out.contains("os") && out.contains("pkg.m5") && out.contains("-> pkg/m5.py"), "resolution of a repository module: {out}");
    let (code, out, _) = r.run(&["rdeps", "pkg/m5.py"]);
    assert_eq!(code, 0);
    assert!(out.contains("pkg/m4.py"), "{out}");
    let (code, out, _) = r.run(&["dupes", "--min-tokens", "3", "--threshold", "0.6", "--json"]);
    assert_eq!(code, 0, "{out}");
    let groups: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    let widgets = |g: &serde_json::Value| {
        g["members"].as_array().is_some_and(|m| m.len() == 12 && m.iter().all(|x| x[1].as_str().is_some_and(|n| n.starts_with("make_widget_"))))
    };
    assert!(groups.iter().any(widgets), "the twelve make_widget functions are one group: {out}");
    // a minimum past the largest signed integer is no wrapped, negative one
    let near = "    total = compute_alpha(x)\n    shifted = adjust_beta(total)\n    kept = filter_entries(shifted)\n    more = store_results(kept)\n    again = parse_payload(more)\n    final = send_request(again)\n";
    r.write("near/a.py", format!("def f_a(x):\n{near}    return finalize_gamma(final, alpha_one)\n").as_bytes());
    r.write("near/b.py", format!("def f_b(x):\n{near}    return finalize_gamma(final, omega_two)\n").as_bytes());
    let (_, out, _) = r.run(&["dupes", "--min-tokens", "3", "--json"]);
    assert!(out.contains("near/a.py"), "{out}");
    for most in [u64::MAX.to_string(), (1u64 << 63).to_string()] {
        let (code, out, _) = r.run(&["dupes", "--min-tokens", &most, "--json"]);
        assert_eq!((code, answer(&out)), (1, serde_json::json!([])), "{most}");
    }
    std::fs::remove_dir_all(r.root.join("near")).unwrap();
    let (code, out, _) = r.run(&["tsquery", "python", "(function_definition name: (identifier) @n)", "."]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 24, "{out}");
    let (code, out, _) = r.run(&["stats"]);
    assert_eq!(code, 0);
    let st: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["files"], 13);
    assert_eq!(st["imports"], 24);
}

#[test]
fn find_routes_by_query_shape() {
    let r = python_repo("find");
    // regex-looking -> Engine A output (path:line:text)
    let (code, out, _) = r.run(&["find", r"def use_\d+"]);
    assert_eq!(code, 0);
    assert!(out.lines().next().unwrap().contains(":def use_"), "{out}");
    // words on a small repository -> ranked results (symbol lines), indexing on the spot
    let (code, out, _) = r.run(&["find", "build widget size"]);
    assert_eq!(code, 0);
    assert!(out.contains("[function]"), "{out}");
}

#[test]
fn search_finds_a_python_function_by_the_comment_above_it() {
    let r = Repo::new("comment");
    r.write("lib/tokens.py", b"def open_session():\n    return 1\n\n\n# Renews the zqvortex lease.\ndef renew(session):\n    return session.renew()\n");
    let (code, out, err) = r.run(&["search", "zqvortex", "-k", "3", "--json"]);
    assert_eq!(code, 0, "{err}");
    let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    assert!(hits.first().is_some_and(|h| h["path"] == "lib/tokens.py" && h["name"] == "renew"), "{out}");
}

#[test]
fn search_ranks_vendored_code_below_project_code_and_marks_it() {
    let r = Repo::new("vendored");
    r.write("lib/tokens.py", b"# Renews the zqvortex lease.\ndef renew(session):\n    return session.renew()\n\n\ndef parse_lease(text):\n    return text\n");
    r.write(
        "vendor/leases/lease.py",
        b"# Renews the zqvortex lease.\ndef zqvortex_lease(session):\n    return session\n\n\ndef parse_lease(text):\n    return text\n\n\ndef burst_frames(n):\n    return n\n",
    );
    let search = |q: &str| {
        let (code, out, err) = r.run(&["search", q, "-k", "5", "--json"]);
        assert_eq!(code, 0, "{err}");
        let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
        hits
    };
    let hits = search("zqvortex lease");
    // every project hit first, though the vendored zqvortex_lease matches the query better
    let paths: Vec<&str> = hits.iter().map(|h| h["path"].as_str().unwrap()).collect();
    let first_vendored = paths.iter().position(|p| p.starts_with("vendor/")).unwrap_or_else(|| panic!("vendored hits kept: {paths:?}"));
    assert!(first_vendored > 0 && paths[first_vendored..].iter().all(|p| p.starts_with("vendor/")), "{paths:?}");
    let v = hits.iter().find(|h| h["name"] == "zqvortex_lease").unwrap_or_else(|| panic!("vendored hit kept: {hits:?}"));
    assert_eq!(v["vendored"], true, "{v}");
    assert!(hits[0].get("vendored").is_none(), "{}", hits[0]);
    // a name both define: the project's first; a name only vendored code defines: still first
    assert_eq!(search("parse_lease").first().map(|h| h["path"].clone()), Some("lib/tokens.py".into()));
    let burst = search("burst_frames");
    assert!(burst.first().is_some_and(|h| h["name"] == "burst_frames" && h["vendored"] == true), "{burst:?}");
}

#[test]
fn every_json_item_with_a_vendored_path_is_marked() {
    let r = Repo::new("vendored_all");
    r.write("lib/app.py", b"from vendor.leases.lease import parse_lease\n\n\ndef main():\n    return parse_lease(1)\n");
    r.write("vendor/leases/lease.py", b"def parse_lease(text):\n    return text\n\n\ndef unused_vendored(x):\n    return x\n");
    r.run(&["index"]);
    let items = |args: &[&str]| {
        let (code, out, err) = r.run(args);
        assert!(code == 0 || code == 1, "{args:?}: {err}");
        let mut all = Vec::new();
        let mut stack = vec![answer(&out)];
        while let Some(v) = stack.pop() {
            match v {
                serde_json::Value::Object(m) => {
                    if let Some(p) = m.get("path").and_then(|p| p.as_str()) {
                        all.push((p.to_string(), m.get("vendored") == Some(&serde_json::Value::Bool(true))));
                    }
                    stack.extend(m.into_iter().map(|(_, v)| v));
                }
                serde_json::Value::Array(a) => stack.extend(a),
                _ => {}
            }
        }
        assert!(!all.is_empty(), "{args:?}: no item with a path");
        all
    };
    for args in [&["refs", "parse_lease", "--json"][..], &["dead", "--json"], &["callers", "parse_lease", "--json"], &["symbol", "parse_lease", "--json"]] {
        for (path, marked) in items(args) {
            assert_eq!(marked, path.starts_with("vendor/"), "{args:?}: {path}");
        }
    }
}

#[test]
fn search_finds_a_rust_struct_by_the_doc_comment_above_its_derive() {
    let r = Repo::new("derive_doc");
    r.write("src/lib.rs", b"pub fn open() {}\n\n/// Holds the zqvortex lease.\n#[derive(Debug, Clone)]\npub struct Lease {\n    id: u32,\n}\n");
    let (code, out, err) = r.run(&["search", "zqvortex", "-k", "3", "--json"]);
    assert_eq!(code, 0, "{err}");
    let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    assert!(hits.first().is_some_and(|h| h["path"] == "src/lib.rs" && h["name"] == "Lease"), "{out}");
}

#[test]
fn search_finds_a_documented_function_once_when_its_comment_lines_carry_no_mark() {
    let r = Repo::new("block_doc");
    r.write("a.vim", b"function! Open()\nendfunction\n\n\" Renews the zqvortex lease.\nfunction! Renew()\n  return 1\nendfunction\n");
    r.write("a.c", b"int open(void) { return 1; }\n\n/* Totals the zqvortex ledger.\n   Second line.  */\nint total(void) { return 2; }\n");
    let (code, out, err) = r.run(&["search", "zqvortex", "-k", "5", "--json"]);
    assert_eq!(code, 0, "{err}");
    let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    let mut got: Vec<_> = hits.iter().map(|h| (h["path"].as_str().unwrap_or(""), h["name"].as_str().unwrap_or(""))).collect();
    got.sort();
    assert_eq!(got, [("a.c", "total"), ("a.vim", "Renew")], "{out}");
}

#[test]
fn search_finds_the_definition_below_a_comment_in_a_language_without_a_grammar() {
    let r = Repo::new("heur_doc");
    r.write("a.ml", b"let first x = x + 1\n\n(** Computes the zqvortex total. *)\nlet second y = first y\n");
    r.write("a.f90", b"subroutine first()\nend subroutine first\n\n! Computes the zqvortex sum.\nsubroutine second()\n  call first()\nend subroutine second\n");
    let (code, out, err) = r.run(&["search", "zqvortex", "-k", "5", "--json"]);
    assert_eq!(code, 0, "{err}");
    let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    let mut got: Vec<_> = hits.iter().map(|h| (h["path"].as_str().unwrap_or(""), h["name"].as_str().unwrap_or(""))).collect();
    got.sort();
    assert_eq!(got, [("a.f90", "second"), ("a.ml", "second")], "{out}");
}

fn gdscript_repo(name: &str) -> Repo {
    let r = Repo::new(name);
    r.write(
        "game/player.gd",
        b"class_name Player\nextends CharacterBody2D\n## The hero the user steers.\n\nsignal health_changed(value)\nsignal died\n\nenum Mood { CALM, ANGRY }\n\nconst MAX_HEALTH = 10\n\nvar health := MAX_HEALTH\n\n\n## Takes damage and tells the listeners.\nfunc take_damage(amount):\n\thealth = Scoring.clamp_health(health - amount)\n\thealth_changed.emit(health)\n\n\nclass Backpack:\n\tvar slots = []\n\n\t## Puts a thing into the first free slot.\n\tfunc stash(thing):\n\t\tslots.append(thing)\n",
    );
    r.write(
        "game/scoring.gd",
        b"class_name Scoring\nextends RefCounted\n\n\n## Bounds the number inside the quartz lantern range.\nstatic func clamp_health(value):\n\treturn clampi(value, 0, Player.MAX_HEALTH)\n",
    );
    r.write(
        "game/level.gd",
        b"extends Node\n\n@onready var player = $Player\n\n\nfunc _ready():\n\tplayer.take_damage(3)\n\tprint(Scoring.clamp_health(20))\n",
    );
    r
}

#[test]
fn gdscript_definitions_calls_and_docs_answer_through_the_index_commands() {
    let r = gdscript_repo("gdscript");
    let (code, out, err) = r.run(&["outline", "game/player.gd", "--json"]);
    assert_eq!(code, 0, "{err}");
    let outline = |out: &str| -> Vec<(String, String)> {
        answer(out).as_array().unwrap().iter().map(|h| (h["kind"].as_str().unwrap().to_string(), h["qualname"].as_str().unwrap().to_string())).collect()
    };
    // the file that declares `class_name Player` is the class, and its top-level definitions are its members
    let want = [
        ("class", "Player"),
        ("signal", "Player.health_changed"),
        ("signal", "Player.died"),
        ("enum", "Player.Mood"),
        ("constant", "Player.MAX_HEALTH"),
        ("key", "Player.health"),
        ("method", "Player.take_damage"),
        ("class", "Player.Backpack"),
        ("key", "Player.Backpack.slots"),
        ("method", "Player.Backpack.stash"),
    ];
    assert_eq!(outline(&out), want.map(|(k, q)| (k.to_string(), q.to_string())), "{out}");
    let (_, out, _) = r.run(&["outline", "game/level.gd", "--json"]);
    assert_eq!(outline(&out), [("module", "level"), ("key", "player"), ("function", "_ready")].map(|(k, q)| (k.to_string(), q.to_string())), "{out}");
    for (name, path, kind) in [
        ("Player.take_damage", "game/player.gd", "method"),
        ("take_damage", "game/player.gd", "method"),
        ("Player.Backpack.stash", "game/player.gd", "method"),
        ("died", "game/player.gd", "signal"),
        ("Scoring.clamp_health", "game/scoring.gd", "method"),
        ("_ready", "game/level.gd", "function"),
    ] {
        let (code, out, _) = r.run(&["symbol", name, "--json"]);
        let hits = answer(&out);
        assert!(code == 0 && hits[0]["path"] == path && hits[0]["kind"] == kind, "{name}: {out}");
    }
    let (code, out, _) = r.run(&["callers", "Player.take_damage", "--json"]);
    let callers: Vec<String> =
        answer(&out).as_array().unwrap().iter().map(|h| format!("{}:{}", h["path"].as_str().unwrap(), h["name"].as_str().unwrap())).collect();
    assert_eq!((code, callers), (0, vec!["game/level.gd:_ready".to_string()]), "{out}");
    let (code, out, _) = r.run(&["callers", "clamp_health", "--json"]);
    assert_eq!(code, 0, "{out}");
    let mut callers: Vec<String> =
        answer(&out).as_array().unwrap().iter().map(|h| format!("{}:{}", h["path"].as_str().unwrap(), h["name"].as_str().unwrap())).collect();
    callers.sort();
    assert_eq!(callers, ["game/level.gd:_ready", "game/player.gd:Player.take_damage"], "{out}");
    // the words of a doc comment alone find its member, each its own hit, and the comment below the class header finds the class
    for (query, path, name) in [
        ("quartz lantern", "game/scoring.gd", "Scoring.clamp_health"),
        ("damage listeners", "game/player.gd", "Player.take_damage"),
        ("hero the user steers", "game/player.gd", "Player"),
    ] {
        let (code, out, _) = r.run(&["search", query, "-k", "3", "--json"]);
        let hits = answer(&out);
        assert!(code == 0 && hits[0]["path"] == path && hits[0]["name"] == name, "{query}: {out}");
    }
    let (code, out, _) = r.run(&["refs", "clamp_health", "--json"]);
    let unresolved = answer(&out)["unresolved"].clone();
    assert!(code == 0 && unresolved.as_array().unwrap().iter().all(|u| u["reason"] == "not resolved in this language"), "{out}");
    let (code, out, _) = r.run(&["grep", "--type", "gdscript", "-l", "clamp_health", "."]);
    assert_eq!((code, sorted_lines(&out)), (0, vec!["./game/level.gd".to_string(), "./game/player.gd".into(), "./game/scoring.gd".into()]), "{out}");
    let (code, out, _) = r.run(&["grep", "--type", "py", "-l", "clamp_health", "."]);
    assert_eq!((code, out.as_str()), (1, ""));
    let (code, out, _) = r.run(&["languages"]);
    assert!(code == 0 && out.lines().any(|l| l.split_whitespace().collect::<Vec<_>>() == ["gdscript", ".gd"]), "{out}");
    let (_, out, _) = r.run(&["languages", "--json"]);
    let langs: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    let gd = langs.iter().find(|l| l["id"] == "gdscript").expect("gdscript in languages --json");
    assert_eq!(gd["definition_kinds"], serde_json::json!(["class", "constant", "enum", "function", "key", "method", "signal"]), "{gd}");
    assert_eq!((gd["call_edges"].as_bool(), gd["tier"].as_str()), (Some(true), Some("grammar")), "{gd}");
}

#[test]
fn search_finds_a_line_past_what_a_long_function_keeps_as_one_hit_naming_it() {
    let r = Repo::new("long_fn");
    let mut src = String::from("def very_long(x):\n");
    for i in 1..=600 {
        src += &if i == 450 { "    x = x + 1  # the zorblax step\n".to_string() } else { format!("    x = x + {i}  # the plain step\n") };
    }
    src += "    return x\n";
    r.write("long.py", src.as_bytes());
    let (code, out, err) = r.run(&["search", "zorblax step", "-k", "5", "--json"]);
    assert_eq!(code, 0, "{err}");
    let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    assert!(hits.first().is_some_and(|h| h["kind"] == "window" && h["name"] == "very_long"), "{out}");
    let (start, end) = (hits[0]["start"].as_i64().unwrap(), hits[0]["end"].as_i64().unwrap());
    assert!(start <= 451 && 451 <= end && end - start < 50, "the lines of the part that matched: {out}");
    assert_eq!(hits.iter().filter(|h| h["name"] == "very_long").count(), 1, "one hit per definition: {out}");
}

#[test]
fn search_finds_the_attribute_above_a_rust_handler() {
    let r = Repo::new("attr");
    r.write("src/lib.rs", b"pub fn open() {}\n\n#[get(\"/users/zqshow\")]\npub fn show() -> u32 {\n    1\n}\n");
    let (code, out, err) = r.run(&["search", "zqshow", "-k", "3", "--json"]);
    assert_eq!(code, 0, "{err}");
    let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    assert!(hits.first().is_some_and(|h| h["name"] == "show" && h["start"] == 3), "{out}");
}

#[test]
fn search_also_merges_repositories() {
    let a = python_repo("also_a");
    let b = Repo::new("also_b");
    b.write("lib/tokens.py", b"def refresh_expired_token(session):\n    \"\"\"Renew an expired access token.\"\"\"\n    return session.renew()\n");
    let (code, out, _) = run_in(&a.root, &["search", "renew an expired access token", "-k", "3", "--also", b.root.to_str().unwrap(), "--json"], &[]);
    assert_eq!(code, 0, "{out}");
    let hits: Vec<serde_json::Value> = serde_json::from_value(answer(&out)).unwrap();
    let canon = |p: &Path| fs::canonicalize(p).unwrap().to_string_lossy().into_owned();
    assert!(hits.iter().any(|h| h["path"] == "lib/tokens.py" && h["root"] == canon(&b.root)), "the other repository's root in a field: {out}");
    assert!(hits.iter().any(|h| h["path"].as_str().unwrap().starts_with("pkg/") && h["root"] == canon(&a.root)), "{out}");
}

#[test]
fn symbol_callers_and_callees_take_a_limit() {
    let r = python_repo("limits");
    assert_eq!(r.run(&["index"]).0, 0);
    let (code, out, _) = r.run(&["symbol", "--limit", "1", "make_widget_4"]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().filter(|l| l.contains("[function]")).count(), 1, "{out}");
    for cmd in ["symbol", "callers", "callees"] {
        let (code, out, _) = r.run(&[cmd, "--json", "--limit", "1", "make_widget_4"]);
        assert_eq!(code, 0, "{cmd}");
        let v: serde_json::Value = serde_json::from_value(answer(&out)).unwrap();
        let items = v.as_array().or_else(|| v["resolved"].as_array()).map_or(0, |a| a.len());
        assert!(items <= 1, "{cmd}: {out}");
    }
    let (code, out, _) = r.run(&["callers", "--limit", "1", "make_widget_4"]);
    assert_eq!(code, 0);
    assert!(out.contains("use_4"), "{out}");
}

#[test]
fn search_also_prints_other_repositories_by_the_path_given() {
    let outer = Repo::new("also_paths");
    let body = |doc: &str| format!("def renew_token(session):\n    \"\"\"{doc}\"\"\"\n    return session\n");
    outer.write("one/src/auth.py", body("Renew an expired session token.").as_bytes());
    outer.write("x/one/src/auth.py", body("Renew a token, in another repository named one.").as_bytes());
    outer.write(
        "two/src/db.py",
        b"def renew_token(conn):\n    \"\"\"Renew the token of a connection.\"\"\"\n    return conn\n\n\ndef only_in_two():\n    return 2\n",
    );
    for d in ["one", "x/one", "two"] {
        assert_eq!(run_in(&outer.root.join(d), &["index"], &[]).0, 0);
    }
    let one = outer.root.join("one");
    let (code, out, err) =
        run_in(&one, &["search", "renew token", "-k", "10", "--also", "../x/one", "--also", "../two", "--also", ".", "--also", "../one/"], &[]);
    assert_eq!(code, 0, "{err}");
    let heads: Vec<&str> = out.lines().filter(|l| !l.starts_with(' ') && !l.is_empty()).map(|l| l.split("  ").next().unwrap()).collect();
    let mut sorted = heads.clone();
    sorted.sort();
    assert_eq!(sorted, ["../two/src/db.py:1-3", "../two/src/db.py:6-7", "../x/one/src/auth.py:1-3", "src/auth.py:1-3"], "each repository once: {out}");
    // one hint, naming a printed location of another repository that can be passed back as it is
    let hints: Vec<&str> = err.lines().filter(|l| l.starts_with("[scrutate] ")).collect();
    assert_eq!(hints.len(), 1, "{err}");
    assert!(!hints[0].contains("--root"), "{err}");
    let example = hints[0].rsplit_once("scrutate symbol ").map(|(_, l)| l.trim_end_matches(')')).unwrap_or_default();
    assert!(example.starts_with("../") && heads.contains(&example), "{err}");
    let (code, out, err) = run_in(&one, &["symbol", example], &[]);
    assert!(code == 0 && out.contains("renew_token"), "{example}: {out}{err}");
    // a location of another repository is also asked about with --root
    let (code, out, _) = run_in(&one, &["--root", "../two", "symbol", "src/db.py:6"], &[]);
    assert!(code == 0 && out.contains("only_in_two"), "{out}");
    // or as printed: the index above the file answers, with paths as it stores them
    for (args, want) in [
        (&["symbol", "../two/src/db.py:6-7"][..], "src/db.py:6-7  [function] only_in_two"),
        (&["outline", "../two/src/db.py"], "only_in_two"),
        (&["refs", "only_in_two", "--def", "../two/src/db.py"], "src/db.py:6-7"),
        (&["symbol", "../x/one/src/auth.py:1-3"], "src/auth.py:1-3"),
    ] {
        let (code, out, err) = run_in(&one, args, &[]);
        assert!(code == 0 && out.contains(want), "{args:?}: {code} {out}{err}");
        assert_eq!(err.lines().filter(|l| l.starts_with("[scrutate] using the index in ")).count(), 1, "{args:?}: {err}");
    }
    // a file no index holds keeps the error and says why
    outer.write("loose/a.py", b"def loose_fn():\n    return 1\n");
    let (code, _, err) = run_in(&one, &["symbol", "../loose/a.py:1"], &[]);
    assert!(code == 2 && err.contains("no symbol at ../loose/a.py:1 (../loose/a.py is outside the repository)"), "{err}");
    let (code, _, err) = run_in(&one, &["outline", "../loose/a.py"], &[]);
    assert!(code == 1 && err.contains("no symbols in ../loose/a.py") && err.contains("is outside the repository"), "{err}");
    // without --also, no hint
    let (_, _, err) = run_in(&one, &["search", "renew token"], &[]);
    assert!(!err.contains("[scrutate]"), "{err}");
}

#[test]
fn check_reports_consistency_and_staleness() {
    let r = python_repo("check");
    r.run(&["index"]);
    let (code, out, _) = r.run(&["check", "--files", "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["database_ok"], true);
    assert_eq!(v["chunks_db"], v["chunks_alive"]);
    assert_eq!(v["files_changed"], 0);
    // constants have no segment row: counted apart
    r.write("pkg/consts.py", b"LIMIT = 10\nNAME = 'x'\n");
    r.run(&["index"]);
    let (_, json, _) = r.run(&["check", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(v["constants"].as_u64().unwrap() >= 2 && v["chunks_db"] == v["chunks_alive"], "{json}");
    let (code, out, _) = r.run(&["check"]);
    let line = format!(
        "chunks: {} ranked in the database, {} alive in segments, {} constants and fields (no postings)",
        v["chunks_db"], v["chunks_alive"], v["constants"]
    );
    assert!(code == 0 && out.lines().any(|l| l == line), "{line}\n{out}");
    fs::remove_file(r.root.join("pkg/consts.py")).unwrap();
    r.run(&["index"]);
    r.write("pkg/m0.py", b"def changed():\n    pass\n");
    let (code, out, _) = r.run(&["check", "--files", "--json"]);
    assert_eq!(code, 1, "stale files are reported: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["files_changed"], 1);
    r.run(&["index"]);
    let (code, _, _) = r.run(&["check", "--files"]);
    assert_eq!(code, 0);
}

#[test]
fn compaction_merges_segments_and_keeps_counts() {
    let r = python_repo("compact");
    r.run(&["index"]);
    // six incremental updates each add a segment; compaction must keep the store small and exact
    for round in 0..6 {
        r.write(&format!("pkg/extra{round}.py"), format!("def extra_{round}():\n    return {round}\n").as_bytes());
        let (code, _, _) = r.run(&["index"]);
        assert_eq!(code, 0);
    }
    for i in 0..6 {
        fs::remove_file(r.root.join(format!("pkg/m{i}.py"))).unwrap();
    }
    let (code, _, _) = r.run(&["index"]);
    assert_eq!(code, 0);
    let (code, out, _) = r.run(&["check", "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["chunks_db"], v["chunks_alive"]);
    assert!(v["segments"].as_u64().unwrap() <= 3, "segments were merged: {out}");
    let (code, out, _) = r.run(&["symbol", "make_widget_2"]);
    assert!(code == 1 && !out.contains("pkg/m2.py"), "deleted file's symbols are gone: {out}");
    let (code, out, _) = r.run(&["symbol", "extra_5"]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn utf16_bom_is_searched_by_engine_a_and_indexed_by_engine_b() {
    let r = Repo::new("utf16");
    let mut u16le = vec![0xff, 0xfe];
    for u in "def utf16_marker():\n    pass\n".encode_utf16() {
        u16le.extend_from_slice(&u.to_le_bytes());
    }
    r.write("w.py", &u16le);
    r.write("ok.py", b"def plain():\n    pass\n");
    let (code, out, _) = r.run(&["grep", "-F", "utf16_marker", "."]);
    assert_eq!(code, 0, "Engine A transcodes UTF-16 with a BOM: {out}");
    assert!(out.starts_with("./w.py:1:"), "{out}");
    r.run(&["index"]);
    let (_, out, _) = r.run(&["stats"]);
    let st: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["files_skipped"], 0, "Engine B transcodes UTF-16 too: {out}");
    let (code, out, _) = r.run(&["symbol", "utf16_marker", "--json"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"w.py\"") && out.contains("\"start\": 1"), "{out}");
}

#[cfg(unix)]
#[test]
fn the_index_directory_is_private_to_its_owner_under_umask_022() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let under_022 = |r: &Repo, args: &str| {
        let out =
            Command::new("sh").arg("-c").arg(format!("umask 022; exec \"$0\" {args}")).arg(BIN).current_dir(&r.root).stdin(Stdio::null()).output().unwrap();
        assert!(matches!(out.status.code(), Some(0 | 1)), "{args}: {}", String::from_utf8_lossy(&out.stderr));
    };
    let r = Repo::new("private_index");
    r.write("secrets.py", b"def token():\n    return 'hunter2'\n");
    fs::set_permissions(r.root.join("secrets.py"), fs::Permissions::from_mode(0o600)).unwrap();
    for args in ["index --text", "index --no-text", "callers token", "tsquery python '(function_definition) @f' ."] {
        let _ = fs::remove_dir_all(r.root.join(".scrutate"));
        under_022(&r, args);
        assert_eq!(mode(&r.root.join(".scrutate")), 0o700, "{args}");
    }
    under_022(&r, "index");
    assert_eq!(mode(&r.root.join(".scrutate")), 0o700);
    assert_eq!(mode(&r.root.join(".scrutate/text")) & 0o077, 0o055, "inside, the umask applies; the directory above keeps others out");
    // an index an earlier version left open to everyone is closed on the next write
    fs::set_permissions(r.root.join(".scrutate"), fs::Permissions::from_mode(0o755)).unwrap();
    under_022(&r, "index");
    assert_eq!(mode(&r.root.join(".scrutate")), 0o700);
}

#[test]
fn the_index_quota_comes_from_the_configuration_file_and_the_environment_beats_it() {
    let r = Repo::new("index_quota");
    r.write("a.py", b"def a():\n    return 1\n");
    r.write("b.py", b"def b():\n    return 2\n");
    r.write(".scrutate.toml", b"[index]\nmax_files = 2\n");
    let (code, _, err) = r.run(&["index", "--no-text"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("the code index would hold 3 files, more than index.max_files = 2 (SCRUTATE_INDEX_MAX_FILES)"), "{err}");
    let (code, _, err) = run_in(&r.root, &["index"], &[("SCRUTATE_INDEX_MAX_FILES", "3")]);
    assert_eq!(code, 0, "{err}");
    r.write(".scrutate.toml", b"[index]\nmax_bytes = \"10\"\n");
    let (code, _, err) = r.run(&["index", "--text"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("the text index would hold") && err.contains("index.max_bytes = 10"), "{err}");
}

#[test]
fn git_file_of_worktrees_and_submodules_is_not_indexed() {
    let r = Repo::new("gitfile");
    r.write(".git", b"gitdir: /somewhere/else/.git/worktrees/wt\n");
    r.write("vendor/sub/.git", b"gitdir: ../../.git/modules/sub\n");
    r.write("vendor/sub/lib.py", b"def sub_symbol():\n    pass\n");
    r.write("a.py", b"def top():\n    pass\n");
    r.run(&["index"]);
    let (_, out, _) = r.run(&["grep", "-F", "gitdir", "."]);
    assert!(out.is_empty(), "Engine A treats .git files as VCS metadata: {out}");
    let (_, out, _) = r.run(&["stats"]);
    let st: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["files"], 2, "{out}");
    let (code, _, _) = r.run(&["symbol", "sub_symbol"]);
    assert_eq!(code, 0);
}

#[test]
fn runs_without_python_on_path() {
    let r = python_repo("nopython");
    let mut c = Command::new(BIN);
    c.args(["grep", "-F", "-c", "import", "."]).current_dir(&r.root).env("PATH", "/nonexistent").env_remove("PYTHONPATH").stdin(Stdio::null());
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut c = Command::new(BIN);
    c.args(["index"]).current_dir(&r.root).env("PATH", "/nonexistent").stdin(Stdio::null());
    assert!(c.output().unwrap().status.success());
}

#[test]
fn mcp_protocol_round_trip() {
    let r = python_repo("mcp");
    let mut child = Command::new(BIN)
        .args(["--root", r.root.to_str().unwrap(), "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let msgs = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"make_widget_7"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read_code","arguments":{"path":"../../etc/passwd"}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"bogus/method"}"#,
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"grep_code","arguments":{"pattern":"def make_widget_[37]\\("}}}"#,
        r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"grep_code","arguments":{"pattern":"(unclosed"}}}"#,
        r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"grep_code","arguments":{"pattern":"import os","max_results":3}}}"#,
        r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"grep_code","arguments":{"pattern":"import os","offset":10}}}"#,
        "this is not json",
    ];
    for m in msgs {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let by_id = |id: i64| lines.iter().find(|v| v["id"] == id).unwrap_or_else(|| panic!("no response {id}"));
    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(by_id(1)["result"]["serverInfo"]["name"], "scrutate");
    assert!(by_id(1)["result"]["instructions"].as_str().unwrap().starts_with("scrutate searches the code of this repository"));
    let tools = by_id(2)["result"]["tools"].as_array().unwrap();
    assert!(tools.len() >= 14, "{}", tools.len());
    assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
    // every tool tells the client it only reads and stays on this machine, except reindex
    for t in tools {
        assert_eq!(t["annotations"]["readOnlyHint"], t["name"] != "reindex", "{}", t["name"]);
        assert_eq!(t["annotations"]["openWorldHint"], false);
    }
    let text = by_id(3)["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pkg/m7.py"), "{text}");
    assert_eq!(by_id(3)["result"]["isError"], false);
    let text = by_id(4)["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("error"), "path escape refused: {text}");
    assert_eq!(by_id(4)["result"]["isError"], true);
    assert_eq!(by_id(5)["error"]["code"], -32602);
    assert_eq!(by_id(6)["error"]["code"], -32601);
    // grep_code is Engine A: matching lines under their enclosing symbol, an error for a bad pattern,
    // and a note when it stopped at the limit
    let text = by_id(7)["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("2 matching lines in 2 symbols"), "{text}");
    assert!(text.contains("pkg/m3.py") && text.contains("7: def make_widget_7(size):"), "{text}");
    assert_eq!(by_id(8)["result"]["isError"], true);
    assert!(by_id(8)["result"]["content"][0]["text"].as_str().unwrap().contains("invalid pattern"));
    let text = by_id(9)["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("3 matching lines") && text.contains("stopped at 3 matching lines"), "{text}");
    // an answer continues from an offset: the last 2 of 12 groups
    let text = by_id(10)["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("12 matching lines in 12 symbols"), "{text}");
    assert_eq!(text.lines().filter(|l| l.contains(": import os")).count(), 2, "{text}");
    assert!(tools.iter().all(|t| t["name"] == "reindex" || t["inputSchema"]["properties"]["max_bytes"]["default"] == 24_000));
    // text is the whole answer unless the server was started with --structured
    assert!(tools.iter().all(|t| t.get("outputSchema").is_none()));
    assert!(by_id(3)["result"].get("structuredContent").is_none());
    assert!(lines.iter().any(|v| v["error"]["code"] == -32700), "parse error for non-JSON input");
}

/// The revision without a handshake, structured results that fit their schemas, whole definitions
/// by name or by a location from another answer, and the same call under both eras.
#[test]
fn mcp_stateless_revision_and_structured_results() {
    let r = python_repo("mcp2");
    let mut child = Command::new(BIN)
        .args(["--root", r.root.to_str().unwrap(), "serve", "--structured"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let meta = r#""_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}"#;
    let call = |id: i64, name: &str, args: &str| {
        format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{name}","arguments":{args},{meta}}}}}"#)
    };
    let msgs = [
        format!(r#"{{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{{{meta}}}}}"#),
        r#"{"jsonrpc":"2.0","id":2,"method":"server/discover","params":{}}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01"}}}"#.to_string(),
        format!(r#"{{"jsonrpc":"2.0","id":4,"method":"tools/list","params":{{{meta}}}}}"#),
        call(10, "search_code", r#"{"query":"build a widget","k":3}"#),
        call(11, "find_symbol", r#"{"name":"make_widget_7"}"#),
        call(12, "callers", r#"{"name":"make_widget_7"}"#),
        call(13, "callees", r#"{"name":"make_widget_7"}"#),
        call(14, "grep_code", r#"{"pattern":"import os","max_results":3}"#),
        call(15, "read_code", r#"{"path":"pkg/m7.py","start":1,"end":3}"#),
        call(16, "read_code", r#"{"symbol":"make_widget_7"}"#),
        call(17, "read_code", r#"{"symbol":"pkg/m7.py:7-9"}"#),
        call(18, "outline", r#"{"path":"pkg/m7.py"}"#),
        call(19, "dependencies", r#"{"path":"pkg/m7.py"}"#),
        call(20, "dependents", r#"{"target":"pkg.m7"}"#),
        call(21, "references", r#"{"name":"make_widget_7"}"#),
        call(22, "references", r#"{"name":"make_widget_7","textual":true}"#),
        call(23, "similar_code", r#"{"target":"make_widget_7","k":3}"#),
        call(24, "duplicates", r#"{}"#),
        call(25, "structural_search", r#"{"lang":"python","query":"(function_definition name: (identifier) @f)","limit":5}"#),
        call(26, "reindex", r#"{}"#),
        call(27, "grep_code", r#"{"pattern":"widget","max_bytes":1000}"#),
        call(28, "read_code", r#"{}"#),
        call(29, "callers", r#"{"name":"pkg/m7.py:7-9"}"#),
        call(30, "read_code", r#"{"symbol":"no_such_symbol_anywhere"}"#),
        r#"{"jsonrpc":"2.0","id":40,"method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"make_widget_7"}}}"#.to_string(),
    ];
    for m in &msgs {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let by_id = |id: i64| lines.iter().find(|v| v["id"] == id).unwrap_or_else(|| panic!("no response {id}"));

    let found = &by_id(1)["result"];
    assert_eq!(found["resultType"], "complete", "{found}");
    assert_eq!(found["supportedVersions"], serde_json::json!(["2026-07-28"]));
    assert!(found["capabilities"]["tools"].is_object());
    assert_eq!(found["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "scrutate");
    assert_eq!(found["cacheScope"], "public");
    assert!(found["ttlMs"].as_u64().unwrap() > 0);
    assert!(found["instructions"].as_str().unwrap().len() < 4096);
    assert_eq!(by_id(2)["error"]["code"], -32602, "a request of this revision must name its version");
    assert_eq!(by_id(3)["error"]["code"], -32022);
    assert_eq!(by_id(3)["error"]["data"], serde_json::json!({"supported": ["2026-07-28"], "requested": "1900-01-01"}));
    let list = &by_id(4)["result"];
    assert_eq!((&list["resultType"], &list["cacheScope"]), (&serde_json::json!("complete"), &serde_json::json!("public")));
    assert!(list["ttlMs"].as_u64().unwrap() > 0);
    let tools = list["tools"].as_array().unwrap();

    // every structured result fits the schema its tool declares, and carries the whole answer
    for id in 10..=26 {
        let (m, res) = (&msgs[(id - 10 + 4) as usize], &by_id(id)["result"]);
        assert_eq!(res["resultType"], "complete", "{m}");
        assert_eq!(res["isError"], false, "{m}: {res}");
        let name = serde_json::from_str::<serde_json::Value>(m).unwrap()["params"]["name"].as_str().unwrap().to_string();
        let schema = &tools.iter().find(|t| t["name"] == name.as_str()).unwrap()["outputSchema"];
        let structured = &res["structuredContent"];
        conforms(schema, structured).unwrap_or_else(|e| panic!("{name}: {e}\nschema {schema}"));
        assert_eq!(structured["complete"], true, "{name}: {structured}");
        assert!(!structured["result"].is_null(), "{name}");
    }
    let sc = |id: i64| &by_id(id)["result"]["structuredContent"]["result"];
    let text = |id: i64| by_id(id)["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(sc(11)[0]["id"], "pkg/m7.py:7-9", "{}", sc(11));
    // a whole definition by name, and by the location another answer printed
    assert!(text(16).starts_with("pkg/m7.py:7-9  [function] make_widget_7\n    7  def make_widget_7(size):"), "{}", text(16));
    assert_eq!(text(16), text(17));
    assert_eq!(sc(16)["definitions"][0]["id"], "pkg/m7.py:7-9");
    assert!(sc(16)["definitions"][0]["text"].as_str().unwrap().contains("def make_widget_7"));
    assert_eq!(sc(15)["text"].as_str().unwrap().lines().count(), 3);
    assert_eq!(text(29), text(12), "a location names the symbol at it");
    // an answer too large for its budget pages in both forms: the structured page holds the
    // groups the text page shows, and says where the next page starts
    let big = &by_id(27)["result"];
    let page = &big["structuredContent"];
    let grep = &tools.iter().find(|t| t["name"] == "grep_code").unwrap()["outputSchema"];
    conforms(grep, page).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((&page["complete"], &page["offset"]), (&serde_json::json!(false), &serde_json::json!(0)), "{page}");
    let shown = big["content"][0]["text"].as_str().unwrap();
    let next = page["next_offset"].as_u64().unwrap();
    assert!(shown.contains(&format!("not shown: call again with offset={next}")), "{big}");
    assert_eq!(page["result"]["groups"].as_array().unwrap().len() as u64, next);
    assert!(page["total"].as_u64().unwrap() > next && page.get("text").is_none(), "{page}");
    for id in [28, 30] {
        assert_eq!(by_id(id)["result"]["isError"], true);
        assert_eq!(by_id(id)["result"]["structuredContent"]["result"], serde_json::Value::Null);
    }
    assert!(text(28).contains("missing path or symbol"), "{}", text(28));
    assert!(text(30).contains("no definition matches"), "{}", text(30));
    // the same call without the per-request version: the older shape, structured result included
    let old = &by_id(40)["result"];
    assert!(old.get("resultType").is_none(), "{old}");
    assert_eq!(old["structuredContent"]["result"], *sc(11));
}

#[test]
fn corruption_found_during_use_triggers_a_rebuild() {
    // large databases skip the open-time page walk; damage inside a page must still be caught by
    // the first operation that hits it and lead to a rebuild (simulated by disabling the walk)
    let r = python_repo("latecorrupt");
    r.run(&["index"]);
    let db = r.root.join(".scrutate/index.db");
    let mut bytes = fs::read(&db).unwrap();
    assert!(bytes.len() > 8192 * 4);
    // garbage over the fourth page (the header page stays intact)
    for b in bytes[4096 * 3..4096 * 4].iter_mut() {
        *b = 0xA5;
    }
    fs::write(&db, &bytes).unwrap();
    let _ = fs::remove_file(r.root.join(".scrutate/index.db-wal"));
    let _ = fs::remove_file(r.root.join(".scrutate/index.db-shm"));
    let (code, out, err) = run_in(&r.root, &["search", "build a widget", "-k", "2"], &[("SCRUTATE_QUICK_CHECK_MAX_BYTES", "0")]);
    assert_eq!(code, 0, "stdout: {out}\nstderr: {err}");
    assert!(out.contains("make_widget_"), "{out}");
    let (code, out, _) = run_in(&r.root, &["check", "--json"], &[("SCRUTATE_QUICK_CHECK_MAX_BYTES", "0")]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn search_finds_a_definition_named_by_a_stop_word() {
    let r = Repo::new("stopname");
    r.write("a.py", b"def where(x):\n    return x\n\n\ndef find(x):\n    return x\n\n\ndef get(x):\n    return x\n\n\ndef id(x):\n    return x\n");
    r.run(&["index"]);
    for (query, line) in [("find", "a.py:5-6"), ("where", "a.py:1-2"), ("get", "a.py:9-10"), ("id", "a.py:13-14"), ("where is it", "a.py:1-2")] {
        let (code, out, err) = r.run(&["search", query, "-k", "1"]);
        assert_eq!(code, 0, "{query}: {out}{err}");
        assert!(out.starts_with(line), "{query}: {out}");
    }
}

#[test]
fn a_posting_row_outside_its_segment_triggers_a_rebuild() {
    let r = python_repo("badposting");
    r.run(&["index"]);
    let seg = fs::read_dir(r.root.join(".scrutate/seg")).unwrap().next().unwrap().unwrap().path();
    for c in 0..6 {
        let p = seg.join(format!("doc{c}.npy"));
        let mut bytes = fs::read(&p).unwrap();
        // every row number becomes -1; the header and so the length stay as they were
        let data = 10 + u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        bytes[data..].fill(0xFF);
        fs::write(&p, &bytes).unwrap();
    }
    let (code, out, err) = r.run(&["search", "build a widget", "-k", "2"]);
    assert_eq!(code, 0, "stdout: {out}\nstderr: {err}");
    assert!(out.contains("make_widget_") && err.contains("rebuilding"), "stdout: {out}\nstderr: {err}");
    let (code, out, _) = r.run(&["check", "--json"]);
    assert_eq!(code, 0, "{out}");
}

/// `grep --index` must print exactly what the walk prints, for every kind of query, before and
/// after incremental updates (edits, additions, deletions, renames) and after segment merges.
#[test]
fn indexed_grep_equals_walk() {
    let r = Repo::new("textindex");
    r.write(".gitignore", b"ignored/\n*.log\n");
    r.write(".rgignore", b"!debug.log\nsrc/rg_only.rs\n");
    r.write("src/rg_only.rs", b"fn make_widget_0() {}\n");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    for i in 0..40 {
        r.write(
            &format!("src/m{i}.rs"),
            format!("// module {i}\nfn make_widget_{i}(size: usize) -> Widget {{\n    Widget::new(size * {i})\n}}\n\nstruct RefreshToken{i};\nconst KÖNIG_{i}: &str = \"König\";\n").as_bytes(),
        );
    }
    r.write("src/crlf.txt", b"alpha beta\r\nkernel_thread here\r\n");
    r.write("src/bin.dat", b"needle before\0needle after\n");
    r.write("src/late_nul.txt", &[b"needle line\n".repeat(8000), b"\0tail needle\n".to_vec()].concat());
    r.write("src/utf16.txt", &[0xFF, 0xFE, b'n', 0, b'e', 0, b'e', 0, b'd', 0, b'l', 0, b'e', 0, b'\n', 0]);
    r.write("src/multi.c", b"struct file_operations fops = {\n\t.owner = THIS_MODULE,\n};\n");
    r.write("ignored/x.rs", b"fn make_widget_0() {}\n");
    r.write("debug.log", b"make_widget_0\n");
    r.write(".hidden/h.rs", b"fn make_widget_0() {}\n");
    let queries: Vec<Vec<&str>> = vec![
        vec!["-F", "make_widget_1"],
        vec!["make_widget_\\d+"],
        vec!["-i", "-F", "REFRESHTOKEN3"],
        vec!["-w", "-F", "Widget"],
        vec!["--ident", "refreshToken"],
        vec!["--fuzzy=1", "make_widgte"],
        vec!["-i", "könig"],
        vec!["-e", "alpha", "-e", "kernel_thread"],
        vec!["-F", "needle"],
        vec!["-U", "fops = \\{\\n\\s+\\.owner"],
        vec!["-v", "-c", "-F", "Widget"],
        vec!["-l", "-F", "size"],
        vec!["-C", "1", "-F", "struct RefreshToken7"],
        vec!["-g", "*.txt", "-F", "needle"],
        vec!["-t", "rust", "-F", "module"],
        vec!["x{0}"],
        vec!["(ab|cd)?e"],
    ];
    let check = |stage: &str| {
        for q in &queries {
            let mut walk = vec!["grep", "--sort"];
            walk.extend(q.iter());
            let mut idx = walk.clone();
            idx.insert(1, "--index");
            let (c1, o1, e1) = r.run(&walk);
            let (c2, o2, e2) = r.run(&idx);
            assert_eq!((c1, &o1), (c2, &o2), "{stage}: {q:?}\nwalk stderr: {e1}\nindex stderr: {e2}");
        }
    };
    let (code, out, err) = r.run(&["index", "--text"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("text index: 46 files"), "{out}");
    check("fresh");
    // edit, add, delete, rename
    r.write("src/m3.rs", b"fn make_widget_1000() {}\n");
    r.write("src/new.rs", b"fn make_widget_42() {}\nstruct RefreshToken99;\n");
    fs::remove_file(r.root.join("src/m5.rs")).unwrap();
    fs::rename(r.root.join("src/m6.rs"), r.root.join("src/renamed.rs")).unwrap();
    let (_, out, _) = r.run(&["index", "--text"]);
    assert!(out.contains("3 changed, 2 removed"), "{out}");
    check("incremental");
    // many small updates: segments merge, dead documents are purged
    for i in 0..8 {
        r.write(&format!("src/extra{i}.rs"), format!("fn extra_{i}() {{}}\n").as_bytes());
        r.run(&["index", "--text"]);
    }
    let (_, out, _) = r.run(&["index", "--text"]);
    let segs: usize = out.split(" segment(s)").next().and_then(|s| s.rsplit(", ").next()).and_then(|s| s.parse().ok()).unwrap();
    assert!(segs <= 4, "{out}");
    check("merged");
    // a damaged segment is reported with the repair command; rebuilding repairs it
    let seg = fs::read_dir(r.root.join(".scrutate/text")).unwrap().flatten().find(|e| e.path().is_dir()).unwrap().path();
    fs::write(seg.join("ptr.npy"), b"garbage").unwrap();
    let (code, _, err) = r.run(&["grep", "--index", "-F", "make_widget_1"]);
    assert_eq!(code, 2);
    assert!(err.contains("index --text --force"), "{err}");
    let (code, _, _) = r.run(&["index", "--text", "--force"]);
    assert_eq!(code, 0);
    check("rebuilt");
    // flags that change the file set are refused rather than silently ignored
    let (code, _, err) = r.run(&["grep", "--index", "--hidden", "x"]);
    assert_eq!(code, 2, "{err}");
}

/// A repository with `.git` and its index at the top, for commands run from `src/deep`.
fn nested_repo(name: &str) -> Repo {
    let r = Repo::new(name);
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("top.py", b"def top_fn():\n    return 1\n");
    r.write("src/s.py", b"from top import top_fn\n\n\ndef src_fn():\n    return top_fn()\n");
    r.write("src/deep/d.py", b"from src.s import src_fn\n\n\ndef deep_fn():\n    return src_fn()\n");
    assert_eq!(r.run(&["index"]).0, 0);
    r
}

#[test]
fn the_index_of_a_repository_is_at_its_top_whichever_directory_a_command_runs_in() {
    // no index anywhere: the first command, run in a subdirectory, builds it at the top
    let r = Repo::new("first_build");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("top.py", b"def top_fn():\n    return 1\n");
    r.write("src/deep/d.py", b"def deep_fn():\n    return 2\n");
    let deep = r.root.join("src/deep");
    let (code, out, err) = run_in(&deep, &["symbol", "top_fn"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("top.py:1"), "{out}");
    assert!(r.root.join(".scrutate").is_dir() && !deep.join(".scrutate").exists(), "the index is at the top");
    assert_eq!(err.lines().filter(|l| l.starts_with("[scrutate] using the index in ")).count(), 1, "{err}");
    // an index an earlier run left below the top does not hide the top's
    let (code, _, err) = run_in(&deep, &["--root", ".", "index"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(deep.join(".scrutate").is_dir());
    let (code, out, err) = run_in(&deep, &["symbol", "top_fn"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("top.py:1"), "{out}");
    // with no index at the top, the one in the subdirectory is the repository's only one: it is used
    fs::remove_dir_all(r.root.join(".scrutate")).unwrap();
    let (code, out, _) = run_in(&deep, &["symbol", "deep_fn"], &[]);
    assert_eq!(code, 0);
    assert!(out.starts_with("d.py:1"), "{out}");
    assert!(!r.root.join(".scrutate").exists(), "no second index is built at the top");
}

#[test]
fn config_json_names_the_root_an_index_command_uses() {
    let r = Repo::new("config_root");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("src/deep/d.py", b"def deep_fn():\n    return 2\n");
    let deep = r.root.join("src/deep");
    let root_of = |args: &[&str]| {
        let (code, out, err) = run_in(&deep, args, &[]);
        assert_eq!(code, 0, "{args:?}: {err}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        assert!(v["configuration_file"].is_null(), "{out}");
        fs::canonicalize(v["root"].as_str().unwrap()).unwrap()
    };
    assert_eq!(root_of(&["config", "--json"]), fs::canonicalize(&r.root).unwrap(), "the top, before any index");
    assert!(!r.root.join(".scrutate").exists(), "config builds no index");
    assert_eq!(root_of(&["--root", ".", "config", "--json"]), fs::canonicalize(&deep).unwrap(), "--root as given");
}

#[test]
fn a_repository_inside_another_does_not_use_the_outer_index() {
    let r = nested_repo("inner_repo");
    // a submodule or a git worktree: `.git` is a file there, a directory in a clone
    for (dir, git_is_file) in [("vendor/sub", true), ("vendor/clone", false)] {
        let inner = r.root.join(dir);
        fs::create_dir_all(inner.join("pkg")).unwrap();
        if git_is_file {
            fs::write(inner.join(".git"), "gitdir: ../../.git/modules/sub\n").unwrap();
        } else {
            fs::create_dir_all(inner.join(".git")).unwrap();
        }
        fs::write(inner.join("pkg/own.py"), "def inner_fn():\n    return 1\n").unwrap();
        for cwd in [inner.clone(), inner.join("pkg")] {
            let (code, out, err) = run_in(&cwd, &["symbol", "inner_fn"], &[]);
            assert_eq!(code, 0, "{dir}: {err}");
            assert!(out.contains("own.py:1"), "{dir}: {out}");
            // from `pkg` the index of the inner repository is the one above
            let outer = err.lines().any(|l| l.starts_with("[scrutate] using the index in ") && !l.contains(dir.rsplit('/').next().unwrap()));
            assert!(!outer, "{dir}: {err}");
            assert_eq!(run_in(&cwd, &["symbol", "top_fn"], &[]).0, 1, "{dir}: the outer repository is not searched");
        }
    }
}

#[test]
fn index_commands_in_a_subdirectory_answer_from_the_index_above() {
    let r = nested_repo("subdir");
    let deep = r.root.join("src/deep");
    let run = |args: &[&str]| run_in(&deep, args, &[]);
    let (code, out, err) = run(&["symbol", "top_fn"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("top.py:1"), "paths stay relative to the root: {out}");
    // the path itself is printed as the system spells it (a short 8.3 name on a Windows runner)
    assert_eq!(err.lines().filter(|l| l.starts_with("[scrutate] using the index in ")).count(), 1, "one line names the root: {err}");
    assert!(!err.contains("indexed"), "{err}");
    // a path is read from here, or as an answer printed it (from the root)
    for (args, want) in [
        (&["outline", "d.py"][..], "deep_fn"),
        (&["outline", "./d.py"], "deep_fn"),
        (&["outline", "src/deep/d.py"], "deep_fn"),
        (&["outline", "../s.py"], "src_fn"),
        (&["outline", "../../top.py"], "top_fn"),
        (&["outline", "top.py"], "top_fn"),
        (&["deps", "d.py"], "src.s"),
        (&["rdeps", "../s.py"], "src/deep/d.py"),
        (&["refs", "src_fn", "--def", "../s.py"], "src/deep/d.py:5"),
        (&["refs", "src_fn", "--def", "../s.py:4"], "src/deep/d.py:5"),
        (&["callers", "../s.py:4"], "deep_fn"),
        (&["callees", "d.py:4"], "src_fn"),
        (&["symbol", "d.py:4"], "deep_fn"),
        (&["similar", "d.py:4"], "src/s.py"),
        (&["search", "deep"], "src/deep/d.py"),
        (&["find", "deep"], "src/deep/d.py"),
        (&["stats"], "\"files\":3"),
        (&["check"], "OK"),
    ] {
        let (code, out, err) = run(args);
        assert!(code == 0 && out.contains(want), "{args:?}: {code} {out}{err}");
        assert_eq!(err.lines().filter(|l| l.starts_with("[scrutate] using the index in ")).count(), 1, "{args:?}: {err}");
    }
    // tsquery reads paths from here and keeps its cache in the index above
    let q = "(function_definition name: (identifier) @n)";
    let (_, out, _) = run(&["tsquery", "python", q]);
    assert!(out.contains("deep_fn") && !out.contains("top_fn"), "{out}");
    let (_, out, _) = run(&["tsquery", "python", q, "../../top.py"]);
    assert!(out.contains("top_fn"), "{out}");
    let (_, out, _) = run(&["tsquery", "python", q, "top.py"]);
    assert!(out.contains("top_fn"), "{out}");
    // grep --index answers as the walk does from here
    let walk = run(&["grep", "--sort", "-F", "_fn"]);
    let (code, out, err) = run(&["grep", "--sort", "--index", "-F", "_fn"]);
    assert_eq!((code, &out), (walk.0, &walk.1), "{err}");
    assert!(out.starts_with("d.py:"), "{out}");
    assert_eq!(walk.2, "", "a plain grep does not use the index");
    // index updates the index above
    r.write("src/deep/e.py", b"def extra_fn():\n    pass\n");
    let (code, out, _) = run(&["index"]);
    assert!(code == 0 && out.starts_with("4 files"), "{out}");
    assert_eq!(r.run(&["symbol", "extra_fn"]).0, 0);
    assert!(!deep.join(".scrutate").exists(), "no index in the subdirectory");
    // --root is taken as given
    let (code, _, _) = run(&["--root", ".", "symbol", "deep_fn"]);
    assert_eq!(code, 0);
    assert!(deep.join(".scrutate").exists());
    // with no index here or above, the current directory is indexed, as before
    let bare = Repo::new("subdir_bare");
    bare.write("a/b.py", b"def bare_fn():\n    pass\n");
    let (code, _, err) = run_in(&bare.root.join("a"), &["symbol", "bare_fn"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(bare.root.join("a/.scrutate").exists() && !bare.root.join(".scrutate").exists());
}

fn sorted_lines(out: &str) -> Vec<String> {
    let mut v: Vec<String> = out.lines().map(String::from).collect();
    v.sort();
    v
}

#[test]
fn the_configuration_file_is_the_nearest_one_up_to_the_repository_top() {
    let outer = Repo::new("subcfg");
    outer.write(".scrutate.toml", b"[grep]\nglobs = [\"!*.md\"]\n");
    outer.write("repo/.git/HEAD", b"ref: refs/heads/main\n");
    outer.write("repo/.scrutate.toml", b"[grep]\nglobs = [\"!*.py\", \"!src/deep/gen/**\", \"!src/*/skip.txt\"]\n");
    for f in [
        "repo/top.py",
        "repo/src/deep/d.py",
        "repo/src/deep/note.txt",
        "repo/src/deep/gen/g.txt",
        "repo/src/deep/skip.txt",
        "repo/src/other.txt",
        "repo/src/deep/doc.md",
    ] {
        outer.write(f, b"x_fn\n");
    }
    let repo = outer.root.join("repo");
    let deep = repo.join("src/deep");
    let files = |cwd: &Path, args: &[&str], env: &[(&str, &str)]| {
        let (_, out, err) = run_in(cwd, &[&["grep", "-l"][..], args, &["x_fn"]].concat(), env);
        assert_eq!(err, "", "a plain grep names no root");
        sorted_lines(&out)
    };
    // globs apply relative to the file's directory, from the top and from below
    assert_eq!(files(&repo, &[], &[]), ["src/deep/doc.md", "src/deep/note.txt", "src/other.txt"]);
    assert_eq!(files(&deep, &[], &[]), ["doc.md", "note.txt"]);
    let (_, out, _) = run_in(&deep, &["config"], &[]);
    assert!(out.contains(&format!("{}", repo.join(".scrutate.toml").display())), "{out}");
    // a nearer file wins
    outer.write("repo/src/.scrutate.toml", b"[grep]\nglobs = [\"!*.txt\"]\n");
    assert_eq!(files(&deep, &[], &[]), ["d.py", "doc.md"]);
    fs::remove_file(repo.join("src/.scrutate.toml")).unwrap();
    // the file above the repository top is not read
    fs::remove_file(repo.join(".scrutate.toml")).unwrap();
    assert!(files(&deep, &[], &[]).contains(&"doc.md".to_string()));
    // SCRUTATE_CONFIG and --root choose the file as before
    let cfg = outer.root.join("other.toml");
    fs::write(&cfg, b"[grep]\nglobs = [\"!*.txt\"]\n").unwrap();
    assert_eq!(files(&deep, &[], &[("SCRUTATE_CONFIG", cfg.to_str().unwrap())]), ["d.py", "doc.md"]);
    outer.write("repo/src/deep/.scrutate.toml", b"[grep]\nglobs = [\"!d.py\"]\n");
    let (_, out, _) = run_in(&repo.join("src"), &["grep", "--root", "deep", "-l", "x_fn", "deep"], &[]);
    assert_eq!(sorted_lines(&out), ["deep/doc.md", "deep/gen/g.txt", "deep/note.txt", "deep/skip.txt"]);
}

#[test]
fn the_globs_of_the_configuration_file_match_from_its_directory_whatever_path_is_searched() {
    let r = Repo::new("cfgglobs");
    r.write(".git/HEAD", b"ref: refs/heads/main\n");
    r.write(".scrutate.toml", b"[grep]\nglobs = [\"!gen/**\", \"!*.min.js\", \"!src/deep/**\"]\n");
    for f in ["top.txt", "docs/f.md", "gen/a.txt", "src/e.txt", "src/d.min.js", "src/deep/c.txt", "src/gen/b.txt"] {
        r.write(f, b"needle\n");
    }
    let src = r.root.join("src");
    let files = |cwd: &Path, args: &[&str]| {
        let (code, out, err) = run_in(cwd, &[&["grep", "-l", "needle"][..], args].concat(), &[]);
        assert!(code < 2, "{args:?}: {err}");
        sorted_lines(&out)
    };
    assert_eq!(files(&r.root, &["."]), ["./docs/f.md", "./src/e.txt", "./src/gen/b.txt", "./top.txt"]);
    assert_eq!(files(&src, &["."]), ["./e.txt", "./gen/b.txt"]);
    assert_eq!(files(&src, &[".."]), ["../docs/f.md", "../src/e.txt", "../src/gen/b.txt", "../top.txt"]);
    // printed as given, never in the canonical form (a Windows temp directory may be an 8.3 name)
    let abs = r.root.to_str().unwrap();
    let slashed = |p: &str| p.replace('\\', "/");
    let want: Vec<String> = ["docs/f.md", "src/e.txt", "src/gen/b.txt", "top.txt"].iter().map(|f| slashed(&format!("{abs}/{f}"))).collect();
    let got: Vec<String> = files(&src, &[abs]).iter().map(|p| slashed(p)).collect();
    assert_eq!(got, want);
    assert_eq!(files(&src, &["deep", "../gen", "../docs"]), ["../docs/f.md"]);
    // a file named on the command line is searched whatever the globs, as with -g and in ripgrep
    assert_eq!(files(&src, &["deep/c.txt", "d.min.js", "../gen/a.txt"]), ["../gen/a.txt", "d.min.js", "deep/c.txt"]);
    assert_eq!(files(&src, &["-g", "!*.txt", "e.txt"]), ["e.txt"]);
    let (_, out, _) = run_in(&src, &["config"], &[]);
    let dir = r.root.file_name().unwrap().to_str().unwrap();
    assert!(out.lines().any(|l| l.starts_with("grep globs, matched from ") && l.ends_with(&format!("{dir}: !gen/**, !*.min.js, !src/deep/**"))), "{out}");
    assert!(!out.contains("SCRUTATE_GREP_GLOBS"), "{out}");
    // -g and SCRUTATE_GREP_GLOBS replace the file's globs and match from each searched path
    assert_eq!(
        files(&src, &["-g", "!e.txt", ".."]),
        ["../docs/f.md", "../gen/a.txt", "../src/d.min.js", "../src/deep/c.txt", "../src/gen/b.txt", "../top.txt"]
    );
    let (_, out, _) = run_in(&src, &["grep", "-l", "needle", "."], &[("SCRUTATE_GREP_GLOBS", "!gen/**")]);
    assert_eq!(sorted_lines(&out), ["./d.min.js", "./deep/c.txt", "./e.txt"]);
    // the index answers with the files the walk finds
    let (code, _, err) = r.run(&["index", "--text"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(files(&r.root, &["--index"]), ["docs/f.md", "src/e.txt", "src/gen/b.txt", "top.txt"]);
    assert_eq!(files(&src, &["--index"]), ["e.txt", "gen/b.txt"]);
    // alternatives and classes in a directory part
    r.write(".scrutate.toml", b"[grep]\nglobs = [\"!{src,lib}/deep/**\", \"![sx]rc/gen/**\"]\n");
    assert_eq!(files(&src, &["."]), ["./d.min.js", "./e.txt"]);
    assert_eq!(files(&src, &["--index"]), ["d.min.js", "e.txt"]);
    assert_eq!(files(&r.root, &["src"]), ["src/d.min.js", "src/e.txt"]);
}

#[test]
fn grep_index_refuses_a_directory_the_index_never_enters() {
    let r = Repo::new("indexunder");
    r.write(".git/HEAD", b"ref: refs/heads/main\n");
    r.write(".gitignore", b"gen/\nout/*\n!out/keep/\n");
    r.write("src/.gitignore", b"local/\n");
    for f in ["gen/a.txt", "gen/inner/b.txt", ".hid/c.txt", "src/local/d.txt", "out/keep/e.txt", "out/drop/f.txt", "src/g.txt", "top.txt"] {
        r.write(f, format!("needle {f}\n").as_bytes());
    }
    assert_eq!(r.run(&["index", "--text"]).0, 0);
    for dir in ["gen", "gen/inner", ".hid", "src/local", "out/drop"] {
        let cwd = r.root.join(dir);
        assert_eq!(run_in(&cwd, &["grep", "needle"], &[]).0, 0, "{dir}: plain grep walks it");
        for args in [&["grep", "--index", "needle"][..], &["grep", "--index", "--update", "needle"], &["grep", "--index", "--symbols", "needle"]] {
            let (code, out, err) = run_in(&cwd, args, &[]);
            assert_eq!((code, out.as_str()), (2, ""), "{dir} {args:?}: {err}");
            assert!(err.contains("the current directory is not in the index (hidden or ignored); search without --index"), "{dir}: {err}");
        }
    }
    for dir in ["src", "out/keep", "out"] {
        let cwd = r.root.join(dir);
        let walk = run_in(&cwd, &["grep", "--sort", "needle"], &[]);
        let (code, out, err) = run_in(&cwd, &["grep", "--sort", "--index", "needle"], &[]);
        assert_eq!((code, &out), (walk.0, &walk.1), "{dir}: {err}");
        assert_eq!(code, 0, "{dir}");
    }
}

#[test]
fn a_path_outside_the_directory_of_the_configuration_file_is_not_touched_by_its_globs() {
    let r = Repo::new("cfgoutside");
    r.write(".git/HEAD", b"ref: refs/heads/main\n");
    r.write("sub/.scrutate.toml", b"[grep]\nglobs = [\"!gen/**\"]\n");
    for f in ["gen/a.txt", "other/gen/b.txt", "sub/gen/c.txt", "sub/d.txt"] {
        r.write(f, b"needle\n");
    }
    let (_, out, err) = run_in(&r.root.join("sub"), &["grep", "-l", "needle", "..", "../other"], &[]);
    assert_eq!(sorted_lines(&out), ["../gen/a.txt", "../other/gen/b.txt", "../other/gen/b.txt", "../sub/d.txt"], "{err}");
}

#[test]
fn exclude_dirs_follows_the_index_wherever_a_command_starts() {
    let r = Repo::new("excludedirs");
    r.write(".git/HEAD", b"ref: refs/heads/main\n");
    // names that are no directory name, or a pattern, exclude nothing else
    r.write(".scrutate.toml", b"[index]\nexclude_dirs = [\"gen\", \"*\", \"..\", \"../src\", \"/src\", \"a/src\", \"\"]\n");
    r.write("gen/g.py", b"def g_fn():\n    return 1\n");
    r.write("src/s.py", b"def s_fn():\n    return 2\n");
    r.write("sub/.scrutate.toml", b"[index]\nexclude_dirs = [\"src\"]\n");
    r.write("sub/t.py", b"def t_fn():\n    return 3\n");
    let sub = r.root.join("sub");
    let user = r.root.join("user.toml");
    fs::write(&user, b"[index]\nexclude_dirs = [\"src\"]\n").unwrap();
    let user = user.to_string_lossy().into_owned();
    let found = |cwd: &Path, name: &str, env: &[(&str, &str)]| run_in(cwd, &["symbol", name], env).0 == 0;
    let stats = |cwd: &Path| run_in(cwd, &["stats"], &[]).1.lines().find(|l| l.contains("\"files\"")).map(String::from);
    assert_eq!(run_in(&r.root, &["index"], &[]).0, 0);
    let top_stats = stats(&r.root);
    for (cwd, env) in [(&r.root, &[][..]), (&sub, &[]), (&r.root, &[("SCRUTATE_CONFIG", user.as_str())]), (&sub, &[]), (&r.root, &[])] {
        assert!(!found(cwd, "g_fn", env), "{cwd:?} {env:?}: gen/ is excluded by the root's file");
        assert!(found(cwd, "s_fn", env) && found(cwd, "t_fn", env), "{cwd:?} {env:?}: another file's list does not apply");
        assert_eq!(stats(cwd), top_stats, "{cwd:?} {env:?}");
    }
    let (_, _, err) = run_in(&sub, &["symbol", "s_fn"], &[]);
    assert!(err.contains("sub") && err.contains(".scrutate.toml is ignored: the index reads it from"), "{err}");
    let (_, _, err) = run_in(&r.root, &["symbol", "s_fn"], &[("SCRUTATE_CONFIG", &user)]);
    assert!(err.contains("user.toml is ignored"), "{err}");
    let (_, _, err) = run_in(&r.root, &["symbol", "s_fn"], &[]);
    assert!(!err.contains("ignored"), "{err}");
    // the user's environment replaces the file's list; without it the list comes back
    assert!(found(&sub, "g_fn", &[("SCRUTATE_INDEX_EXCLUDE_DIRS", "")]));
    assert!(!found(&sub, "g_fn", &[]));
    assert!(!found(&r.root, "s_fn", &[("SCRUTATE_INDEX_EXCLUDE_DIRS", "src")]));
    assert!(found(&r.root, "s_fn", &[]));
    // the MCP server and batch read the same list, from any directory
    r.write("q.jsonl", b"{\"op\": \"grep_code\", \"pattern\": \"_fn\"}\n");
    let q = r.root.join("q.jsonl").to_string_lossy().into_owned();
    for cwd in [&r.root, &sub] {
        let (code, out, err) = run_in(cwd, &["batch", &q], &[]);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("src/s.py") && out.contains("sub/t.py") && !out.contains("gen/g.py"), "{out}");
        let mut child = Command::new(BIN).arg("serve").current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        for m in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"g_fn"}}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"s_fn"}}}"#,
        ] {
            writeln!(stdin, "{m}").unwrap();
        }
        drop(stdin);
        let text = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned();
        assert!(!text.contains("gen/g.py") && text.contains("src/s.py:1"), "{cwd:?}: {text}");
    }
    // `config` lists the names the index uses, not those it drops
    let (_, out, _) = run_in(&sub, &["config"], &[]);
    let line = out.lines().find(|l| l.starts_with("index exclude_dirs, from ")).unwrap_or_default();
    assert!(line.ends_with(".scrutate.toml: gen, *"), "{out}");
}

#[test]
fn an_exclude_dirs_name_with_a_backslash_excludes_nothing_in_batch() {
    let r = Repo::new("excludebackslash");
    r.write(".git/HEAD", b"ref: refs/heads/main\n");
    r.write(".scrutate.toml", b"[index]\nexclude_dirs = [\"a\\\\b\", \"x\\\\\"]\n");
    r.write("ab/b.py", b"def ab_fn():\n    return 1\n");
    r.write("src/s.py", b"def s_fn():\n    return 2\n");
    if cfg!(unix) {
        r.write("a\\b/c.py", b"def back_fn():\n    return 3\n");
    }
    r.write("q.jsonl", b"{\"op\": \"grep_code\", \"pattern\": \"_fn\"}\n{\"op\": \"find_symbol\", \"name\": \"back_fn\"}\n");
    // in the variable, `\,` is a comma inside a name and any other backslash is kept, so these are `a\b` and `x\`
    for env in [&[][..], &[("SCRUTATE_INDEX_EXCLUDE_DIRS", "a\\b, x\\")]] {
        let (code, out, err) = run_in(&r.root, &["batch", "q.jsonl"], env);
        assert_eq!(code, 0, "{env:?}: {err}");
        assert!(!out.contains("error") && out.contains("ab/b.py") && out.contains("src/s.py"), "{env:?}: {out}");
        if cfg!(unix) {
            assert!(out.lines().all(|l| l.contains("a\\\\b/c.py")), "{env:?}: {out}");
        }
    }
}

#[cfg(unix)]
#[test]
fn a_root_config_that_links_to_a_file_inside_the_root_is_read_up_to_its_size_cap() {
    let r = Repo::new("excludelink");
    r.write(".git/HEAD", b"ref: refs/heads/main\n");
    r.write("gen/g.py", b"def g_fn():\n    return 1\n");
    r.write("src/s.py", b"def s_fn():\n    return 2\n");
    let outside = Repo::new("excludelink_outside");
    outside.write("real.toml", b"[index]\nexclude_dirs = [\"src\"]\n");
    let toml = r.root.join(".scrutate.toml");
    let excluded = |name: &str| run_in(&r.root, &["symbol", name], &[]).0 != 0;
    // a link inside the root is followed; one out of it is not
    r.write("conf/real.toml", b"[index]\nexclude_dirs = [\"gen\"]\n");
    std::os::unix::fs::symlink("conf/real.toml", &toml).unwrap();
    assert!(excluded("g_fn") && !excluded("s_fn"));
    let (_, out, _) = run_in(&r.root, &["config"], &[]);
    assert!(out.contains("index exclude_dirs, from ") && out.contains(": gen"), "{out}");
    fs::remove_file(&toml).unwrap();
    std::os::unix::fs::symlink(outside.root.join("real.toml"), &toml).unwrap();
    assert!(!excluded("g_fn") && !excluded("s_fn"));
    let (_, out, _) = run_in(&r.root, &["config"], &[]);
    assert!(!out.contains("index exclude_dirs"), "{out}");
    // the cap is that of every file the index reads: 1,000,000 bytes
    fs::remove_file(&toml).unwrap();
    for (size, read) in [(1_000_000, true), (1_000_001, false)] {
        let mut text = b"[index]\nexclude_dirs = [\"gen\"]\n".to_vec();
        text.resize(size, b'\n');
        r.write(".scrutate.toml", &text);
        assert_eq!(excluded("g_fn"), read, "{size} bytes");
    }
}

#[test]
fn serve_and_watch_in_a_subdirectory_use_the_index_above() {
    use std::io::BufRead;
    let r = nested_repo("subserve");
    let deep = r.root.join("src/deep");
    let mut child = Command::new(BIN).arg("serve").current_dir(&deep).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for m in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"top_fn"}}}"#,
    ] {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("top.py:1"), "{text}");
    let mut child = Command::new(BIN).arg("watch").current_dir(&deep).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let _guard = KillOnDrop(child);
    let top = fs::canonicalize(&r.root).unwrap();
    let line = std::io::BufReader::new(stderr).lines().map_while(Result::ok).find(|l| l.contains("watching")).unwrap();
    assert!(line.ends_with(&format!("watching {}", top.display())), "{line}");
    assert!(!deep.join(".scrutate").exists());
}

#[test]
fn no_first_index_of_the_home_directory_without_asking() {
    let h = Repo::new("home");
    h.write("Documents/notes.py", b"def stray_fn():\n    return 1\n");
    let home = h.root.to_string_lossy().into_owned();
    let env = [(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home.as_str())];
    let refused = |args: &[&str]| {
        let (code, out, err) = run_in(&h.root, args, &env);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
        assert_eq!(err.lines().count(), 1, "{args:?}: {err}");
        assert!(err.contains("--root") && err.contains("scrutate index"), "{args:?}: {err}");
        assert!(!h.root.join(".scrutate").exists(), "{args:?} built an index");
    };
    for args in [
        &["symbol", "stray_fn"][..],
        &["search", "stray"],
        &["stats"],
        &["grep", "--index", "stray_fn"],
        &["grep", "--index", "--update", "stray_fn"],
        &["serve"],
        &["watch"],
    ] {
        refused(args);
    }
    // a plain grep needs no index
    let (code, out, _) = run_in(&h.root, &["grep", "-F", "stray_fn"], &env);
    assert!(code == 0 && out.contains("notes.py"), "{out}");
    assert!(!h.root.join(".scrutate").exists());
    // --root, SCRUTATE_ROOT and `index` say where to build
    let (code, out, err) = run_in(&h.root, &["--root", ".", "symbol", "stray_fn"], &env);
    assert!(code == 0 && out.contains("notes.py:1"), "{out}{err}");
    fs::remove_dir_all(h.root.join(".scrutate")).unwrap();
    let (code, _, err) = run_in(&h.root, &["symbol", "stray_fn"], &[env[0], ("SCRUTATE_ROOT", ".")]);
    assert_eq!(code, 0, "{err}");
    fs::remove_dir_all(h.root.join(".scrutate")).unwrap();
    let (code, _, err) = run_in(&h.root, &["index"], &env);
    assert_eq!(code, 0, "{err}");
    // an index built there on purpose is used
    let (code, out, err) = run_in(&h.root, &["symbol", "stray_fn"], &env);
    assert!(code == 0 && out.contains("notes.py:1"), "{out}{err}");
}

#[cfg(unix)]
#[test]
fn a_home_directory_kept_in_git_is_not_indexed_from_below_when_home_is_a_link() {
    let h = Repo::new("homelink");
    h.write("realhome/.git/HEAD", b"ref: refs/heads/main\n");
    h.write("realhome/notes/n.py", b"def note_fn():\n    return 1\n");
    std::os::unix::fs::symlink(h.root.join("realhome"), h.root.join("homelink")).unwrap();
    let notes = h.root.join("realhome/notes");
    for home in ["realhome", "homelink"] {
        let home = h.root.join(home).to_string_lossy().into_owned();
        let (code, out, err) = run_in(&notes, &["stats"], &[("HOME", home.as_str())]);
        assert_eq!(code, 0, "{home}: {out}{err}");
        assert!(!err.contains("using the index in"), "{home}: {err}");
        assert!(!h.root.join("realhome/.scrutate").exists(), "HOME={home} indexed the home directory");
        let _ = fs::remove_dir_all(notes.join(".scrutate"));
    }
}

#[test]
fn rgignore_files_keep_files_out_of_the_code_index() {
    let r = Repo::new("rgignore");
    r.write(".ignore", b"*.py\n");
    r.write(".rgignore", b"gen/\n!keep.py\n");
    r.write("keep.py", b"def kept_fn():\n    return 1\n");
    r.write("gen/x.py", b"def generated_fn():\n    return 2\n");
    r.write("other.py", b"def other_fn():\n    return 3\n");
    assert_eq!(r.run(&["index"]).0, 0);
    assert_eq!(r.run(&["symbol", "kept_fn"]).0, 0, ".rgignore speaks before .ignore");
    assert_eq!(r.run(&["symbol", "generated_fn"]).0, 1);
    assert_eq!(r.run(&["symbol", "other_fn"]).0, 1);
    assert_eq!(r.run(&["grep", "--index", "-F", "generated_fn"]).0, 1);
    assert_eq!(r.run(&["grep", "--index", "-F", "kept_fn"]).0, 0);
}

/// Each line of a multi-line match is printed with its own number, as grep-class tools do.
#[test]
fn multiline_matches_print_every_line() {
    let r = Repo::new("multiline");
    r.write("a.c", b"int x;\nstruct file_operations fops = {\n\t.owner = THIS_MODULE,\n};\n");
    let (code, out, _) = r.run(&["grep", "-U", "fops = \\{\\n\\s+\\.owner"]);
    assert_eq!(code, 0);
    assert_eq!(out, "a.c:2:struct file_operations fops = {\na.c:3:\t.owner = THIS_MODULE,\n");
}

/// Ctrl-C stops a search and exits 130.
#[cfg(unix)]
#[test]
fn interrupt_exits_130() {
    let r = Repo::new("interrupt");
    let line = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron\n".repeat(20_000);
    for i in 0..60 {
        r.write(&format!("f{i}.txt"), line.as_bytes());
    }
    // a signal that lands before the handler is installed kills the process: on a loaded machine
    // start-up can take long, so wait longer and try again; a missing handler fails every attempt
    let mut last = None;
    for wait_ms in [200u64, 600, 1500] {
        let mut child = Command::new(BIN)
            .args(["grep", "-j", "1", "--fuzzy=3", "omicron_pi_rho_sigma"])
            .current_dir(&r.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(wait_ms));
        if child.try_wait().unwrap().is_some() {
            eprintln!("search finished before the signal; nothing to interrupt on this machine");
            return;
        }
        // the system call, not a `kill` binary: minimal images have none
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        assert_eq!(unsafe { kill(child.id() as i32, 2) }, 0, "SIGINT");
        let st = child.wait().unwrap();
        if st.code() == Some(130) {
            return;
        }
        last = Some(st);
    }
    panic!("Ctrl-C did not stop the search with exit code 130: {last:?}");
}

/// With --json (or SCRUTATE_DIAGNOSTICS=json) an error is one JSON record on stderr.
#[test]
fn errors_are_json_when_asked() {
    let r = Repo::new("jsonerr");
    r.write("a.txt", b"x\n");
    let (code, _, err) = r.run(&["grep", "--json", "(unclosed"]);
    assert_eq!(code, 2);
    let v: serde_json::Value = serde_json::from_str(err.trim()).unwrap_or_else(|e| panic!("{e}: {err}"));
    assert_eq!(v["type"], "error");
    assert_eq!(v["schema"], 1);
    assert!(v["message"].as_str().unwrap().contains("unclosed"), "{v}");
    let (code, _, err) = run_in(&r.root, &["outline", "missing.py"], &[("SCRUTATE_DIAGNOSTICS", "json")]);
    if code == 2 {
        let v: serde_json::Value = serde_json::from_str(err.trim().lines().last().unwrap()).unwrap();
        assert_eq!(v["type"], "error");
    }
    let (code, _, err) = r.run(&["grep", "(unclosed"]);
    assert_eq!(code, 2);
    assert!(err.starts_with("scrutate: error: "), "{err}");
}

#[test]
fn a_file_searched_in_ranges_equals_one_pass() {
    let r = Repo::new("split");
    let mut big = String::new();
    for i in 0..4000 {
        match i % 7 {
            0 => big.push_str(&format!("alpha {i} König\n")),
            3 => big.push_str(&format!("{}\r\n", "x".repeat(i % 300))),
            5 => big.push('\n'),
            _ => big.push_str(&format!("beta {i} konig gamma\n")),
        }
    }
    big.push_str(&"long ".repeat(400)); // a line longer than a range, and no final newline
    big.push_str("alpha tail");
    r.write("big.txt", big.as_bytes());
    let mut late_nul = big.clone().into_bytes();
    late_nul.splice(late_nul.len() - 500..late_nul.len() - 500, [0u8]);
    r.write("late_nul.txt", &late_nul);
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(big.as_bytes());
    r.write("bom.txt", &bom);
    r.write("one_line.txt", format!("{}alpha beta{}", "y".repeat(5000), "z".repeat(5000)).as_bytes());
    r.write("one_line_nul.txt", format!("{}alpha beta{}\0tail alpha", "y".repeat(5000), "z".repeat(5000)).as_bytes());
    // a line over a megabyte goes into one buffer in parts, each read by its own thread
    let words: String = (0..200_000).map(|i| format!("w{i} ")).collect();
    r.write("long_line.txt", format!("{words}alpha beta KÖNIG").as_bytes());
    let mut nul = format!("{words}alpha tail").into_bytes();
    nul[1_200_000] = 0;
    r.write("long_line_nul.txt", &nul);
    for args in [
        &["grep", "alpha"][..],
        &["grep", "-i", "KÖNIG"],
        &["grep", "--json", "beta 2"],
        &["grep", "-c", "konig"],
        &["grep", "-v", "a"],
        &["grep", "--json", "-v", "e"],
        &["grep", "x{20}"],
        &["grep", "nothing here"],
        &["grep", "-j", "3", "gamma"],
    ] {
        for file in ["big.txt", "late_nul.txt", "bom.txt", "one_line.txt", "one_line_nul.txt", "long_line.txt", "long_line_nul.txt"] {
            let mut a = args.to_vec();
            a.push(file);
            let one = untimed(run_in(&r.root, &a, &[("SCRUTATE_SPLIT_FILE_BYTES", "999999999999")]));
            let split = untimed(run_in(&r.root, &a, &[("SCRUTATE_SPLIT_FILE_BYTES", "1000")]));
            assert_eq!(one, split, "{a:?}");
            assert!(one.0 == 0 || one.0 == 1, "{a:?}: {one:?}");
            if one.0 == 0 && !a.contains(&"-c") && !a.contains(&"--json") {
                // one file given: lines without its name, as ripgrep prints them, and its note on a NUL byte
                let line = |l: &String| l.split(':').next().is_some_and(|n| n.parse::<u64>().is_ok()) || l.starts_with("binary file matches (found ");
                assert!(one.1.iter().all(line), "{a:?}: {:?}", &one.1[..1]);
            }
        }
    }
}

#[test]
fn small_tree_path_equals_parallel_walk() {
    let r = python_repo("smalltree");
    r.write(".gitignore", b"ignored/\n*.log\n");
    r.write("ignored/x.py", b"make_widget_1 hidden by gitignore\n");
    r.write("a.log", b"make_widget_1 in a log\n");
    r.write(".hidden/h.py", b"make_widget_1 in a hidden dir\n");
    r.write("bin.dat", b"make_widget_1\0binary\n");
    for args in [
        &["grep", "--sort", "make_widget"][..],
        &["grep", "--sort", "--json", "-i", "WIDGET"],
        &["grep", "--sort", "--hidden", "--no-ignore", "make_widget_1"],
        &["grep", "--sort", "-c", "import"],
        &["grep", "--sort", "-l", "widget"],
        &["grep", "--sort", "-g", "*.md", "widgets"],
    ] {
        let small = untimed(run_in(&r.root, args, &[("SCRUTATE_SMALL_TREE", "100000")]));
        let parallel = untimed(run_in(&r.root, args, &[("SCRUTATE_SMALL_TREE", "0")]));
        // a budget smaller than the tree: some files searched by the first walk, the rest by the second
        let handed_over = untimed(run_in(&r.root, args, &[("SCRUTATE_SMALL_TREE", "3")]));
        assert_eq!(small, parallel, "{args:?}");
        assert_eq!(handed_over, parallel, "{args:?}");
        assert_eq!(small.0, 0, "{args:?}: {small:?}");
    }
}

#[test]
fn an_index_answer_fits_max_bytes_and_continues_from_an_offset() {
    let r = python_repo("budget");
    assert_eq!(r.run(&["index", "--no-text"]).0, 0);
    let (code, all, _) = r.run(&["outline", "pkg/m3.py"]);
    assert_eq!(code, 0);
    let lines: Vec<&str> = all.lines().collect();
    assert!(lines.len() >= 2, "{all}");
    // room for the first line only
    let (code, cut, _) = r.run(&["outline", "pkg/m3.py", "--max-bytes", "40"]);
    assert_eq!(code, 0);
    assert_eq!(cut.lines().next(), Some(lines[0]));
    assert!(cut.contains(&format!("{} more symbols not shown: call again with offset=1", lines.len() - 1)), "{cut}");
    let (_, rest, _) = r.run(&["outline", "pkg/m3.py", "--offset", "1"]);
    assert_eq!(rest.lines().collect::<Vec<_>>(), lines[1..]);
}

#[test]
fn batch_runs_any_operation() {
    let r = python_repo("batchops");
    assert_eq!(r.run(&["index", "--no-text"]).0, 0);
    r.write("q.jsonl", b"{\"op\": \"find_symbol\", \"name\": \"make_widget_3\"}\n{\"op\": \"outline\", \"path\": \"pkg/m3.py\"}\n{\"op\": \"callers\"}\n{\"query\": \"make_widget_3\", \"k\": 2}\n");
    let (code, out, _) = r.run(&["batch", "q.jsonl"]);
    assert_eq!(code, 0);
    let recs: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(recs.len(), 4, "{out}");
    assert_eq!(recs[0]["op"], "find_symbol");
    assert_eq!(recs[0]["result"][0]["path"], "pkg/m3.py");
    assert!(recs[1]["result"].as_array().unwrap().iter().any(|s| s["name"] == "make_widget_3"), "{}", recs[1]);
    assert_eq!(recs[2]["error"], "missing required argument name");
    assert!(recs[3]["hits"].as_array().is_some_and(|h| h.len() <= 2), "{}", recs[3]);
}

#[test]
fn json_answers_that_stop_at_their_limit_say_so() {
    let r = Repo::new("jsonstop");
    let body = "def f():\n    return compute_total(1, 2, 3) + other_value(4, 5, 6)\n\n\ndef m():\n    return alpha_beta(7) * gamma_delta(8) - epsilon(9)\n";
    r.write("a.py", body.as_bytes());
    r.write("b.py", body.as_bytes());
    r.write("c.py", b"def h():\n    pass\n\n\ndef g():\n    f()\n    h()\n\n\ndef k():\n    f()\n");
    assert_eq!(r.run(&["index", "--no-text"]).0, 0);
    let q = "(function_definition name: (identifier) @n)";
    for (args, note) in [
        (&["symbol", "f", "--limit", "1"][..], "(stopped at 1 results: raise limit to see more)"),
        (&["callers", "f", "--limit", "1"], "(stopped at 1 results: raise limit to see more)"),
        (&["callees", "g", "--limit", "1"], "(stopped at 1 callees: raise limit to see more)"),
        (&["dupes", "--min-tokens", "2", "--limit", "1"], "(stopped at 1 groups: raise limit to see more)"),
        (&["tsquery", "python", q, "--limit", "1"], "(stopped at 1 matches: raise limit to see more)"),
        (&["refs", "f", "--max-results", "1"], "(the search stopped at max_results=1 occurrences"),
    ] {
        let (code, out, err) = r.run(&[args, &["--json"]].concat());
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(!out.contains("stopped at"), "{args:?}: the JSON keeps its shape: {out}");
        assert!(err.contains(note), "{args:?}: {err}");
        let roomy: Vec<&str> = args.iter().map(|a| if *a == "1" { "50" } else { a }).chain(["--json"]).collect();
        let (code, _, err) = r.run(&roomy);
        assert!(code == 0 && !err.contains("stopped"), "{roomy:?}: {err}");
    }
    r.write("q.jsonl", b"{\"op\": \"find_symbol\", \"name\": \"f\", \"limit\": 1}\n{\"op\": \"find_symbol\", \"name\": \"f\", \"limit\": 5}\n");
    let (code, out, _) = r.run(&["batch", "q.jsonl"]);
    assert_eq!(code, 0);
    let recs: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(recs[0]["result"].as_array().map(Vec::len), Some(1), "{out}");
    assert_eq!(recs[0]["stopped_at"], 1, "{out}");
    assert_eq!(recs[1]["result"].as_array().map(Vec::len), Some(2), "{out}");
    assert!(recs[1].get("stopped_at").is_none(), "{out}");
}

#[test]
fn an_index_command_and_a_batch_record_print_one_envelope() {
    let r = Repo::new("envelope");
    r.write("a.py", b"def f():\n    pass\n\n\ndef g():\n    return f()\n");
    r.write("b.py", b"def f():\n    pass\n");
    r.write("q.jsonl", b"{\"op\": \"find_symbol\", \"name\": \"f\", \"limit\": 1}\n{\"op\": \"outline\", \"path\": \"a.py\"}\n");
    let (_, out, _) = r.run(&["batch", "q.jsonl"]);
    let recs: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    for (args, rec) in [(&["symbol", "f", "--limit", "1", "--json"][..], &recs[0]), (&["outline", "a.py", "--json"], &recs[1])] {
        let (_, out, _) = r.run(args);
        let mut cli: serde_json::Value = serde_json::from_str(&out).unwrap();
        let mut rec = rec.clone();
        assert!(rec.as_object_mut().unwrap().remove("op").is_some_and(|op| op.is_string()), "{rec}");
        assert_eq!(cli["schema"], 2, "{out}");
        assert_eq!(cli.take(), rec, "{args:?}");
    }
    assert_eq!(recs[0]["stopped_at"], 1, "{out}");
}

#[test]
fn read_reads_lines_or_a_definition_as_read_code_does() {
    let r = Repo::new("read");
    r.write("src/a.py", b"import os\n\n\ndef f():\n    return 1\n\n\ndef g():\n    return f()\n");
    let (code, out, _) = r.run(&["read", "src/a.py", "--start", "4", "--end", "5"]);
    assert_eq!((code, out.as_str()), (0, "    4  def f():\n    5      return 1\n(lines 4-5 of 9 in src/a.py: read_code path=src/a.py start=6 for more)\n"));
    let (code, out, _) = run_in(&r.root.join("src"), &["read", "a.py", "--start", "8"], &[("SCRUTATE_ROOT", r.root.to_str().unwrap())]);
    assert_eq!((code, out.as_str()), (0, "    8  def g():\n    9      return f()\n"), "a path from the current directory");
    let (code, out, _) = r.run(&["read", "--symbol", "g"]);
    assert_eq!((code, out.as_str()), (0, "src/a.py:8-9  [function] g\n    8  def g():\n    9      return f()\n"));
    let (code, out, _) = r.run(&["read", "--symbol", "g", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!((code, &v["result"]["definitions"][0]["qualname"]), (0, &serde_json::json!("g")), "{out}");
    let (code, _, err) = r.run(&["read", "src/a.py", "--start", "5", "--end", "2"]);
    assert_eq!(code, 2);
    assert!(err.contains("end (2) is before start (5)"), "{err}");
    assert_eq!(r.run(&["read"]).0, 2, "a path or a symbol");
    let (code, _, err) = r.run(&["read", "../elsewhere.py"]);
    assert!(code == 2 && err.contains("outside"), "{err}");
    assert_eq!(r.run(&["read", "--help"]).0, 0);
}

#[test]
fn a_paged_answer_and_its_closing_newline_fit_the_limit() {
    let r = Repo::new("page_fit");
    let mut src = String::from("def target():\n    return 1\n\n\n");
    for i in 0..60 {
        src.push_str(&format!("def caller_{i}():\n    return target()\n\n\n"));
    }
    r.write("a.py", src.as_bytes());
    for i in 0..40 {
        r.write(&format!("d{}/f{i:02}.txt", i % 4), format!("needle {i} one\nneedle {i} two\n").as_bytes());
    }
    r.run(&["index"]);
    // an answer that fills its limit exactly used to print one byte more: the newline after it
    for limit in 300..520 {
        let l = limit.to_string();
        for cmd in [vec!["grep", "-n", "needle", "."], vec!["callers", "target"], vec!["refs", "target"], vec!["search", "caller", "-k", "50"]] {
            let mut args = cmd.clone();
            args.extend(["--max-bytes", l.as_str()]);
            let out = r.run(&args).1;
            assert!(out.len() <= limit, "{cmd:?} --max-bytes {limit} printed {} bytes", out.len());
        }
    }
}

#[test]
fn a_shell_caller_can_bound_a_grep() {
    let r = Repo::new("grep_budget");
    for i in 0..40 {
        r.write(&format!("d{}/f{i:02}.txt", i % 4), format!("needle {i} one\nneedle {i} two\nother\n").as_bytes());
    }
    let whole = r.run(&["grep", "-n", "needle", "."]);
    assert_eq!((whole.0, whole.1.lines().count()), (0, 80), "whole by default, piped");
    let (code, cut, _) = r.run(&["grep", "-n", "needle", ".", "--max-bytes", "400"]);
    assert_eq!(code, 0);
    assert!(cut.len() <= 400, "{} bytes: {cut}", cut.len());
    let shown = cut.lines().count() - 1;
    let last = cut.lines().last().unwrap();
    assert_eq!(last, format!("({} more lines not shown: call again with offset={shown}, or narrow the request)", 80 - shown));
    // the lines come in path order, so the next page starts where this one stopped
    let mut sorted: Vec<&str> = whole.1.lines().collect();
    sorted.sort_by_key(|l| (l.split(':').next().unwrap().to_string(), l.split(':').nth(1).unwrap().parse::<u32>().unwrap()));
    assert_eq!(cut.lines().take(shown).collect::<Vec<_>>(), sorted[..shown]);
    let (_, next, _) = r.run(&["grep", "-n", "needle", ".", "--max-bytes", "400", "--offset", &shown.to_string()]);
    assert_eq!(next.lines().next(), Some(sorted[shown]));
    let env = run_in(&r.root, &["grep", "-n", "needle", "."], &[("SCRUTATE_GREP_MAX_BYTES", "400")]);
    assert_eq!(env.1, cut, "the variable");
    r.write(".scrutate.toml", b"[grep]\nmax_bytes = 400\n");
    assert_eq!(r.run(&["grep", "-n", "needle", "."]).1, cut, "the configuration file");
    fs::remove_file(r.root.join(".scrutate.toml")).unwrap();
    // the budget of index answers is not grep's, and --json stays whole
    assert_eq!(run_in(&r.root, &["grep", "-n", "needle", "."], &[("SCRUTATE_MAX_BYTES", "400")]).1.lines().count(), 80);
    // the same with an option every command takes before the subcommand
    let root = r.root.to_string_lossy().into_owned();
    assert_eq!(run_in(&r.root, &["--root", &root, "grep", "-n", "needle", "."], &[("SCRUTATE_MAX_BYTES", "400")]).1.lines().count(), 80);
    assert_eq!(run_in(&r.root, &["--root", &root, "grep", "-n", "needle", "."], &[("SCRUTATE_GREP_MAX_BYTES", "400")]).1, cut);
    assert_eq!(r.run(&["--max-bytes", "400", "grep", "-n", "needle", "."]).1, cut);
    for args in [&["grep", "--help"][..], &["--root", ".", "grep", "--help"][..], &["--max-bytes", "10", "grep", "--help"][..]] {
        let help = r.run(args).1;
        assert!(help.contains("SCRUTATE_GREP_MAX_BYTES") && !help.contains("SCRUTATE_MAX_BYTES="), "{args:?}: {help}");
    }
    assert_eq!(r.run(&["grep", "--json", "needle", ".", "--max-bytes", "400"]).1.lines().filter(|l| l.contains("\"match\"")).count(), 80);
    // a budget that holds everything changes nothing but the order
    let (_, all, _) = r.run(&["grep", "-n", "needle", ".", "--max-bytes", "100000"]);
    assert_eq!(all.lines().collect::<Vec<_>>(), sorted);
    assert!(r.run(&["grep", "-c", "needle", ".", "--max-bytes", "100"]).1.lines().last().unwrap().contains("offset="));
    assert_eq!(r.run(&["grep", "nothing_here", ".", "--max-bytes", "100"]), (1, String::new(), String::new()));
}

#[test]
fn find_lists_grep_results_in_path_order() {
    let r = Repo::new("find_sorted");
    for i in 0..60 {
        r.write(&format!("d{}/f{i:02}.rs", i % 6), format!("fn helper_{i}(x: u8) {{}}\n").as_bytes());
    }
    let (code, first, _) = r.run(&["find", "fn helper_[0-9]+\\("]);
    assert_eq!(code, 0);
    assert_eq!(first.lines().filter(|l| l.contains("fn helper_")).count(), 60);
    let mut sorted: Vec<&str> = first.lines().collect();
    sorted.sort();
    assert_eq!(first.lines().collect::<Vec<_>>(), sorted);
}

#[test]
fn the_largest_limit_is_no_limit() {
    let r = Repo::new("maxlimit");
    r.write("a.py", b"def f():\n    pass\n");
    r.write("b.py", b"def f():\n    pass\n");
    r.write("c.py", b"def h():\n    pass\n\n\ndef g():\n    f()\n    h()\n\n\ndef k():\n    f()\n");
    let body = b"def total():\n    return compute_total(1, 2, 3) + other_value(4, 5, 6)\n";
    r.write("d.py", body);
    r.write("e.py", body);
    assert_eq!(r.run(&["index", "--no-text"]).0, 0);
    let max = u64::MAX.to_string();
    let q = "(function_definition name: (identifier) @n)";
    for (args, want) in [
        (&["symbol", "f"][..], 2),
        (&["callers", "f"], 2),
        (&["callees", "g"], 1),
        (&["tsquery", "python", q], 5),
        (&["dupes", "--min-tokens", "0", "--threshold", "1"], 1),
    ] {
        let (code, out, err) = r.run(&[args, &["--limit", &max, "--json"]].concat());
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        let n = if args[0] == "tsquery" {
            out.lines().count()
        } else {
            let v: serde_json::Value = serde_json::from_value(answer(&out)).unwrap();
            v.as_array().or_else(|| v["resolved"].as_array()).map_or(0, Vec::len)
        };
        assert!(n >= want, "{args:?}: {out}");
        assert!(!err.contains("stopped"), "{args:?}: {err}");
    }
    let (code, out, err) = r.run(&["similar", "d.py:1", "-k", &max]);
    assert!(code == 0 && out.contains("e.py:1-2"), "{out}{err}");
}

#[test]
fn a_count_stops_at_max_count() {
    let r = Repo::new("countmax");
    r.write("f.txt", b"a\na\na\na\nb\n");
    r.write("g.txt", b"a\nb\n");
    let (code, out, _) = r.run(&["grep", "--sort", "-c", "-m", "2", "a"]);
    assert_eq!((code, out.as_str()), (0, "f.txt:2\ng.txt:1\n"));
    let (_, out, _) = r.run(&["grep", "--sort", "-c", "a"]);
    assert_eq!(out, "f.txt:4\ng.txt:1\n");
    let (_, out, _) = r.run(&["grep", "--sort", "-m", "2", "a"]);
    assert_eq!(out, "f.txt:1:a\nf.txt:2:a\ng.txt:1:a\n");
}

#[test]
fn whole_file_reads_equal_the_stream_reader() {
    // files around the sizes read in one piece (64 KB and 1 MB): several threads read them whole,
    // one thread (-j 1) streams them, and both must print the same
    let r = Repo::new("wholeread");
    let line = "alpha beta gamma KÖNIG delta\n".as_bytes();
    let text = |n: usize| line.iter().copied().cycle().take(n).collect::<Vec<u8>>();
    let with = |mut t: Vec<u8>, at: usize, byte: u8| {
        t[at] = byte;
        t
    };
    let bom = |t: Vec<u8>| [&[0xEF, 0xBB, 0xBF][..], &t].concat();
    r.write("empty.txt", b"");
    r.write("small.txt", &text(1000));
    r.write("small_nul.txt", &with(text(5000), 100, 0));
    r.write("small_bom.txt", &bom(text(100)));
    r.write("edge_64k.txt", &text(64 << 10));
    r.write("edge_64k_1.txt", &text((64 << 10) + 1));
    r.write("mid.txt", &text(300_000));
    r.write("mid_crlf.txt", &"alpha beta\r\ngamma\r\n".as_bytes().iter().copied().cycle().take(200_000).collect::<Vec<u8>>());
    r.write("mid_nul.txt", &with(text(200_000), 150_000, 0));
    r.write("mid_bom.txt", &bom(text(100_000)));
    r.write("edge_1m.txt", &text(1 << 20));
    r.write("edge_1m_1.txt", &text((1 << 20) + 1));
    for args in [
        &["grep", "--sort", "alpha"][..],
        &["grep", "--sort", "-c", "beta"],
        &["grep", "--sort", "-i", "könig"],
        &["grep", "--sort", "-v", "alpha"],
        &["grep", "--sort", "--json", "gamma"],
        &["grep", "--sort", "-F", "a d"],
        &["grep", "--sort", "nothing here"],
    ] {
        let whole = untimed(r.run(args));
        let mut one = args.to_vec();
        one.extend_from_slice(&["-j", "1"]);
        let stream = untimed(r.run(&one));
        assert_eq!(whole.0, stream.0, "{args:?}");
        assert_eq!(whole.1.len(), stream.1.len(), "{args:?}");
        for (a, b) in whole.1.iter().zip(&stream.1) {
            assert_eq!(a, b, "{args:?}");
        }
    }
    // and the whole-file path is the one that ran: every text file up to 1 MB has its matches
    let (_, out, _) = r.run(&["grep", "--sort", "-c", "beta"]);
    for file in ["small.txt", "edge_64k.txt", "edge_64k_1.txt", "mid.txt", "mid_crlf.txt", "mid_bom.txt", "edge_1m.txt", "edge_1m_1.txt"] {
        assert!(out.lines().any(|l| l.starts_with(&format!("{file}:"))), "{file} missing in\n{out}");
    }
}

#[test]
#[cfg(feature = "pcre2")]
fn pcre2_look_around_backreferences_and_the_match_limit() {
    let r = Repo::new("pcre2");
    r.write("t.txt", b"foobar\nbazbar\nabba\nabc\n");
    r.write("trap.txt", format!("{}!\n", "a".repeat(40)).repeat(50).as_bytes());
    let (code, out, _) = r.run(&["grep", "-P", "(?<=foo)bar", "t.txt"]);
    assert_eq!((code, out.as_str()), (0, "1:foobar\n"));
    let (code, out, _) = r.run(&["grep", "-P", r"(\w)\1", "t.txt"]);
    assert_eq!((code, out.as_str()), (0, "1:foobar\n3:abba\n"));
    let (code, _, _) = r.run(&["grep", "-P", "-F", "a(b", "t.txt"]);
    assert_eq!(code, 1, "-F makes the pattern literal");
    let (code, out, _) = r.run(&["grep", "-P", "-w", "-e", "ab", "-e", "abc", "t.txt"]);
    assert_eq!((code, out.as_str()), (0, "4:abc\n"));
    // exponential backtracking stops at PCRE2's match limit and says so
    let t0 = std::time::Instant::now();
    let (code, out, err) = r.run(&["grep", "-P", "^(a|aa)+$", "trap.txt"]);
    assert!(t0.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("trap.txt") && err.contains("match limit"), "{err}");
    let (code, _, err) = r.run(&["grep", "-P", "(?<=", "t.txt"]);
    assert_eq!(code, 2);
    assert!(err.contains("PCRE2"), "{err}");
}

#[test]
fn search_zip_reads_archive_members_and_compressed_files() {
    use std::io::Write as _;
    let r = Repo::new("searchzip");
    let members: [(&str, &[u8]); 3] = [("src/a.txt", b"alpha needle\n"), ("src/b.txt", b"no\nneedle two\n"), ("bin.dat", b"needle\0binary\n")];
    let zip_path = r.root.join("a.zip");
    let mut z = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
    for (name, data) in members {
        z.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap();
    let tar_bytes = {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in members {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, data).unwrap();
        }
        b.into_inner().unwrap()
    };
    r.write("a.tar", &tar_bytes);
    let gz = |data: &[u8]| {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    };
    r.write("a.tar.gz", &gz(&tar_bytes));
    r.write("c.txt.gz", &gz(b"needle in gz\n"));
    let (code, out, _) = r.run(&["grep", "--sort", "-z", "needle"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "a.tar{src/a.txt}:1:alpha needle\na.tar{src/b.txt}:2:needle two\na.tar.gz{src/a.txt}:1:alpha needle\na.tar.gz{src/b.txt}:2:needle two\n\
         a.zip{src/a.txt}:1:alpha needle\na.zip{src/b.txt}:2:needle two\nc.txt.gz:1:needle in gz\n"
    );
    let (code, _, _) = r.run(&["grep", "needle"]);
    assert_eq!(code, 1, "without -z archives are binary files and skipped");
    let (code, out, _) = r.run(&["grep", "-z", "-l", "two"]);
    let mut listed: Vec<&str> = out.lines().collect();
    listed.sort();
    assert_eq!((code, listed), (0, vec!["a.tar.gz{src/b.txt}", "a.tar{src/b.txt}", "a.zip{src/b.txt}"]));
    let (code, _, err) = r.run(&["grep", "-z", "--index", "needle"]);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn search_zip_of_one_archive_names_its_members_as_a_directory_does() {
    use std::io::Write as _;
    let r = Repo::new("zipalone");
    let zip_of = |name: &str, members: &[(&str, &str)]| {
        let mut z = zip::ZipWriter::new(fs::File::create(r.root.join(name)).unwrap());
        for (m, data) in members {
            z.start_file(*m, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    };
    zip_of("whole.zip", &[("a.txt", "needle a\nneedle a\nneedle a\n"), ("b.txt", "needle b\nneedle b\n")]);
    zip_of("one.zip", &[("a.txt", "needle a\n")]);
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b"needle gz\n").unwrap();
    r.write("c.txt.gz", &e.finish().unwrap());
    for (args, want) in [
        (&["-c", "needle", "whole.zip"][..], "whole.zip{a.txt}:3\nwhole.zip{b.txt}:2\n"),
        (
            &["--sort", "needle", "whole.zip"],
            "whole.zip{a.txt}:1:needle a\nwhole.zip{a.txt}:2:needle a\nwhole.zip{a.txt}:3:needle a\nwhole.zip{b.txt}:1:needle b\nwhole.zip{b.txt}:2:needle b\n",
        ),
        (&["-I", "-c", "needle", "whole.zip"], "3\n2\n"),
        (&["needle", "one.zip"], "one.zip{a.txt}:1:needle a\n"),
        (&["needle", "c.txt.gz"], "1:needle gz\n"),
    ] {
        let (code, out, err) = r.run(&[&["grep", "-z"][..], args].concat());
        assert_eq!((code, out.as_str()), (0, want), "{args:?}: {err}");
    }
    assert_eq!(r.run(&["grep", "-c", "needle", "c.txt.gz"]).1, "", "without -z it is one binary file");
}

#[test]
fn search_zip_shows_control_characters_of_member_names_as_escapes() {
    use std::io::Write as _;
    let r = Repo::new("zipnames");
    let mut z = zip::ZipWriter::new(fs::File::create(r.root.join("n.zip")).unwrap());
    for (name, data) in [("evil}\nfake.txt:1:needle forged\nx{y.txt", "needle two\n"), ("esc\x1b[2J\\.txt", "needle three\n")] {
        z.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(data.as_bytes()).unwrap();
    }
    z.finish().unwrap();
    let (code, out, err) = r.run(&["grep", "--sort", "-z", "needle", "."]);
    assert_eq!(
        (code, out.as_str()),
        (0, "./n.zip{esc\\x1b[2J\\\\.txt}:1:needle three\n./n.zip{evil}\\nfake.txt:1:needle forged\\nx{y.txt}:1:needle two\n"),
        "{err}"
    );
    let (_, out, _) = r.run(&["grep", "-z", "-l", "three", "."]);
    assert_eq!(out, "./n.zip{esc\\x1b[2J\\\\.txt}\n");
    // JSON escapes the name itself
    let (_, out, _) = r.run(&["grep", "-z", "--json", "three", "."]);
    assert!(out.contains(r#""path":"./n.zip{esc\u001b[2J\\.txt}""#), "{out}");
}

#[test]
fn search_zip_stops_a_member_whose_line_is_too_long_and_goes_on() {
    use std::io::Write as _;
    let r = Repo::new("ziplongline");
    // a few MiB past the 64 MiB a line may take: it expands from about 70 KB
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut h = tar::Header::new_gnu();
    let long = vec![b'a'; 68 << 20];
    let mut t = tar::Builder::new(Vec::new());
    h.set_size(long.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    t.append_data(&mut h, "long.txt", &long[..]).unwrap();
    let mut h = tar::Header::new_gnu();
    h.set_size(9);
    h.set_mode(0o644);
    h.set_cksum();
    t.append_data(&mut h, "b.txt", &b"needle b\n"[..]).unwrap();
    e.write_all(&t.into_inner().unwrap()).unwrap();
    r.write("a.tar.gz", &e.finish().unwrap());
    r.write("c.txt", b"needle c\n");
    let (code, out, err) = r.run(&["grep", "--sort", "-z", "needle"]);
    assert_eq!((code, out.as_str()), (0, "a.tar.gz{b.txt}:1:needle b\nc.txt:1:needle c\n"), "{err}");
    assert!(err.contains("a.tar.gz{long.txt}: a line longer than 64 MiB"), "{err}");
}

#[test]
fn search_zip_skips_a_tar_member_whose_long_name_is_too_long() {
    let r = Repo::new("ziplongname");
    let file = |b: &mut tar::Builder<Vec<u8>>, name: &str, data: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, name, data).unwrap();
    };
    let pax = |b: &mut tar::Builder<Vec<u8>>, records: &[(&str, &[u8])]| {
        let mut data = Vec::new();
        for (k, v) in records {
            let rest = k.len() + v.len() + 3;
            let mut n = rest + 1;
            while n != rest + n.to_string().len() {
                n = rest + n.to_string().len();
            }
            data.extend_from_slice(format!("{n} {k}=").as_bytes());
            data.extend_from_slice(v);
            data.push(b'\n');
        }
        let mut h = tar::Header::new_ustar();
        h.set_entry_type(tar::EntryType::XHeader);
        h.set_path("PaxHeader").unwrap();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append(&h, &data[..]).unwrap();
    };
    let gnu_name = format!("d/{}.txt", "g".repeat(200));
    let pax_name = format!("p/{}.txt", "p".repeat(300));
    let mut b = tar::Builder::new(Vec::new());
    file(&mut b, &gnu_name, b"needle gnu\n");
    pax(&mut b, &[("path", pax_name.as_bytes())]);
    file(&mut b, "short", b"needle pax\n");
    // an extended attribute far larger than any name, as macOS tar writes for a resource fork
    pax(&mut b, &[("LIBARCHIVE.xattr.big", &vec![b'x'; 300 << 10][..]), ("path", b"xattr.txt")]);
    file(&mut b, "short2", b"needle xattr\n");
    file(&mut b, &format!("{}.txt", "a".repeat(1 << 20)), b"needle huge gnu\n");
    pax(&mut b, &[("path", &vec![b'b'; 1 << 20][..])]);
    file(&mut b, "short3", b"needle huge pax\n");
    file(&mut b, "after.txt", b"needle after\n");
    r.write("a.tar", &b.into_inner().unwrap());
    let (code, out, err) = r.run(&["grep", "--sort", "-z", "needle"]);
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            format!(
                "a.tar{{after.txt}}:1:needle after\na.tar{{{gnu_name}}}:1:needle gnu\na.tar{{{pax_name}}}:1:needle pax\na.tar{{xattr.txt}}:1:needle xattr\n"
            )
            .as_str()
        ),
        "{err}"
    );
    assert_eq!(err.matches("a.tar: a member name longer than 64 KiB; skipped").count(), 2, "{err}");
}

#[test]
fn search_zip_reads_the_members_after_an_old_gnu_sparse_member() {
    let r = Repo::new("zipsparse");
    // an old-GNU sparse member of `n` 512-byte segments: four in its header, 21 in each extension block
    let sparse = |n: u64, data: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_path("sparse.bin").unwrap();
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::GNUSparse);
        h.set_size(n * 512);
        let g = h.as_gnu_mut().unwrap();
        let segs: Vec<u64> = (0..n).map(|i| i * 1024).collect();
        for (s, &o) in g.sparse.iter_mut().zip(&segs) {
            s.set_offset(o);
            s.set_length(512);
        }
        g.set_is_extended(n > 4);
        g.set_real_size(n * 1024);
        h.set_cksum();
        let mut out = h.as_bytes().to_vec();
        let rest: Vec<u64> = segs.into_iter().skip(4).collect();
        for (i, chunk) in rest.chunks(21).enumerate() {
            let mut ext = tar::GnuExtSparseHeader::new();
            for (s, &o) in ext.sparse.iter_mut().zip(chunk) {
                s.set_offset(o);
                s.set_length(512);
            }
            ext.set_is_extended((i + 1) * 21 < rest.len());
            out.extend_from_slice(ext.as_bytes());
        }
        let mut d = data.to_vec();
        d.resize(n as usize * 512, b'x');
        out.extend_from_slice(&d);
        out
    };
    let file = |name: &str, data: &[u8]| {
        let mut h = tar::Header::new_gnu();
        h.set_path(name).unwrap();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        let mut out = h.as_bytes().to_vec();
        out.extend_from_slice(data);
        out.resize(512 + data.len().div_ceil(512) * 512, 0);
        out
    };
    // member data shaped like a header and its body, where a reader that skips no extension block looks
    let mut forged = vec![b'x'; 24 * 512];
    forged.extend(file("fake.txt", b"needle forged\n"));
    for (name, n, data) in [("four.tar", 4, &[][..]), ("forged.tar", 26, &forged[..]), ("five.tar", 5, &[][..])] {
        let mut t = sparse(n, data);
        t.extend(file("b.txt", b"needle here\n"));
        t.extend([0; 1024]);
        r.write(name, &t);
        let (code, out, err) = r.run(&["grep", "-z", "needle", name]);
        assert_eq!((code, out.as_str(), err.as_str()), (0, format!("{name}{{b.txt}}:1:needle here\n").as_str(), ""), "{name}");
    }
}

/// Poll `ok` every 100 ms until it holds or `secs` pass.
fn wait_for(secs: u64, what: &str, mut ok: impl FnMut() -> bool) {
    let t0 = std::time::Instant::now();
    while !ok() {
        assert!(t0.elapsed().as_secs() < secs, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn watch_keeps_the_code_and_text_indexes_fresh() {
    use std::io::BufRead;
    let r = Repo::new("watch");
    r.write("a.py", b"def alpha_one():\n    return 1\n");
    r.write("c.py", b"def gamma_old():\n    return 3\n");
    r.write(".ignore", b"skipped/\n");
    assert_eq!(r.run(&["index"]).0, 0);
    let mut child =
        Command::new(BIN).args(["watch", "."]).current_dir(&r.root).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let _guard = KillOnDrop(child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    while !rx.recv_timeout(std::time::Duration::from_secs(30)).expect("watch starts").contains("watching") {}

    r.write("a.py", b"def alpha_two():\n    return 22\n");
    r.write("sub/b.py", b"def beta_new():\n    return 2\n");
    fs::remove_file(r.root.join("c.py")).unwrap();
    r.write("skipped/s.py", b"def ignored_by_rules():\n    pass\n");
    // `check` and `grep --index` read the indexes as they are: only the watcher can have updated them
    let t0 = std::time::Instant::now();
    while !(r.run(&["check", "--files"]).0 == 0
        && r.run(&["grep", "--index", "-F", "beta_new"]).0 == 0
        && r.run(&["grep", "--index", "-F", "alpha_two"]).0 == 0
        && r.run(&["grep", "--index", "-F", "gamma_old"]).0 == 1)
    {
        if t0.elapsed().as_secs() >= 30 {
            let said: Vec<String> = rx.try_iter().collect();
            panic!("timed out waiting for the watcher to update both indexes; it said: {said:#?}\ncheck: {:?}", r.run(&["check", "--files"]));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let (code, out, _) = r.run(&["stats"]);
    assert_eq!(code, 0);
    let st: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["files"], 3, "a.py, sub/b.py, .ignore: {out}");
    assert_eq!(r.run(&["symbol", "beta_new"]).0, 0);
    assert_eq!(r.run(&["symbol", "alpha_two"]).0, 0);
    assert_eq!(r.run(&["symbol", "gamma_old"]).0, 1);
    assert_eq!(r.run(&["grep", "--index", "-F", "ignored_by_rules"]).0, 1);
}

#[test]
fn serve_watch_updates_the_index_while_serving() {
    use std::io::BufRead;
    let r = python_repo("servewatch");
    assert_eq!(r.run(&["index"]).0, 0);
    let mut child = Command::new(BIN)
        .args(["--root", r.root.to_str().unwrap(), "serve", "--watch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let stderr = std::io::BufReader::new(child.stderr.take().unwrap());
    let _guard = KillOnDrop(child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    while !rx.recv_timeout(std::time::Duration::from_secs(30)).expect("watch starts").contains("watching") {}
    r.write("pkg/m3.py", b"def served_and_watched():\n    return 3\n");
    wait_for(30, "the watcher thread to update the index", || r.run(&["check", "--files"]).0 == 0);
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"find_symbol","arguments":{{"name":"served_and_watched"}}}}}}"#)
        .unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(line.contains("pkg/m3.py"), "{line}");
}

#[test]
fn tsquery_cache_output_equals_a_fresh_parse() {
    let r = Repo::new("tscache");
    r.write("py/a.py", b"def one():\n    return two()\n\n\ndef two():\n    return 2\n");
    r.write("py/copy.py", b"def one():\n    return two()\n\n\ndef two():\n    return 2\n");
    r.write("rs/lib.rs", b"fn main() {\n    helper(1);\n}\n\nfn helper(x: i32) -> i32 {\n    x + 1\n}\n");
    r.write("js/app.js", b"function start() {\n  return stop(1);\n}\nfunction stop(n) {\n  return n;\n}\n");
    r.write("c/k.c", b"#include <stdlib.h>\nvoid *f(void) { return kmalloc(4, 0); }\nvoid *g(void) { return kmalloc(8, 0); }\n");
    let queries: &[(&str, &str)] = &[
        ("python", "(function_definition name: (identifier) @n)"),
        ("python", "(call function: (identifier) @f) @call"),
        ("rust", "(function_item name: (identifier) @n)"),
        ("javascript", "(call_expression function: (identifier) @f)"),
        ("c", "(call_expression function: (identifier) @f (#eq? @f \"kmalloc\")) @call"),
    ];
    let compare = |stage: &str| {
        for (lang, q) in queries {
            for extra in [&[][..], &["--json"], &["--limit", "1"], &["--limit", "3", "--json"]] {
                let mut args = vec!["tsquery", lang, q];
                args.extend_from_slice(extra);
                let fresh = r.run(&[&args[..], &["--no-cache"]].concat());
                let first = r.run(&args);
                let again = r.run(&args);
                assert_eq!(fresh.0, 0, "{stage} {args:?}: {fresh:?}");
                assert_eq!(fresh, first, "{stage} {args:?}");
                assert_eq!(fresh, again, "{stage} {args:?}");
            }
        }
    };
    compare("first runs");
    assert!(r.root.join(".scrutate/tsquery").read_dir().unwrap().count() >= queries.len());
    r.write("py/a.py", b"def renamed():\n    return other()\n");
    r.write("rs/lib.rs", b"fn main() {}\n");
    r.write("c/k.c", b"void *f(void) { return kmalloc(1, 0); }\n");
    compare("after edits");
    let (_, out, _) = r.run(&["tsquery", "python", "(function_definition name: (identifier) @n)"]);
    assert!(out.contains("py/a.py:1:5  @n (identifier)  renamed") && !out.contains("py/a.py:5"), "{out}");
    let sub = ["tsquery", "python", "(function_definition name: (identifier) @n)", "py"];
    assert_eq!(r.run(&sub), r.run(&[&sub[..], &["--no-cache"]].concat()), "a subtree has its own cache");
    assert_eq!(r.run(&sub), r.run(&[&sub[..], &["--no-cache"]].concat()));
}

/// The code index's record of a file whose stat it holds as current names the cached captures,
/// and the file is not read: shown with a record made to look current for other bytes.
#[test]
fn tsquery_takes_the_content_hash_the_code_index_records_for_a_current_file() {
    let r = Repo::new("tscache_index");
    r.write("a.py", b"def one():\n    return 1\n");
    assert_eq!(r.run(&["index", "--no-text"]).0, 0);
    let q = "(function_definition name: (identifier) @n)";
    assert_eq!(r.run(&["tsquery", "python", q]).1, "a.py:1:5  @n (identifier)  one\n");
    r.write("a.py", b"def two():\n    return 2\n");
    let md = fs::metadata(r.root.join("a.py")).unwrap();
    let ix = scrutate::index::Index::open(&fs::canonicalize(&r.root).unwrap(), scrutate::models::ModelSet::none()).unwrap();
    let s = scrutate::index::Stat::of(&r.root.join("a.py"), &md);
    ix.damage_for_tests(&format!("UPDATE files SET size={}, mtime_ns={}, ctime_ns={} WHERE path='a.py'", s.size, s.mtime, s.ctime)).unwrap();
    drop(ix);
    assert_eq!(r.run(&["tsquery", "python", q]).1, "a.py:1:5  @n (identifier)  one\n", "the file was read");
    assert_eq!(r.run(&["tsquery", "python", q, "--no-cache"]).1, "a.py:1:5  @n (identifier)  two\n");
}

/// A copy of a tree at another place, its `.scrutate/` with it, answers from the cache the first
/// tree made: the cache's names hold no absolute path.
#[test]
fn a_tsquery_cache_made_in_one_tree_answers_in_a_copy_at_another_path() {
    let a = Repo::new("tscache_copy_a");
    a.write("a.py", b"def one():\n    return 1\n");
    a.write("sub/b.py", b"def two():\n    return 2\n");
    assert_eq!(a.run(&["index", "--no-text"]).0, 0);
    let q = "(function_definition name: (identifier) @n)";
    let whole = a.run(&["tsquery", "python", q]).1;
    assert_eq!(a.run(&["tsquery", "python", q, "sub"]).1, "sub/b.py:1:5  @n (identifier)  two\n");
    let b = Repo::new("tscache_copy_b");
    let cp = std::process::Command::new("cp").arg("-R").arg(a.root.join(".")).arg(&b.root).status().unwrap();
    assert!(cp.success());
    let caches = |r: &Repo| {
        let mut v: Vec<(String, Vec<u8>)> = fs::read_dir(r.root.join(".scrutate/tsquery"))
            .unwrap()
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).unwrap()))
            .collect();
        v.sort();
        v
    };
    let made = caches(&a);
    assert_eq!(made.len(), 2);
    // the copy's a.py holds other bytes under the stat its index records: only a cache hit answers `one`
    b.write("a.py", b"def won():\n    return 1\n");
    let md = fs::metadata(b.root.join("a.py")).unwrap();
    let ix = scrutate::index::Index::open(&fs::canonicalize(&b.root).unwrap(), scrutate::models::ModelSet::none()).unwrap();
    let s = scrutate::index::Stat::of(&b.root.join("a.py"), &md);
    ix.damage_for_tests(&format!("UPDATE files SET size={}, mtime_ns={}, ctime_ns={} WHERE path='a.py'", s.size, s.mtime, s.ctime)).unwrap();
    drop(ix);
    assert_eq!(b.run(&["tsquery", "python", q]).1, whole);
    assert_eq!(b.run(&["tsquery", "python", q, "sub"]).1, "sub/b.py:1:5  @n (identifier)  two\n");
    assert_eq!(caches(&b), made, "the copy found the first tree's cache files and did not write new ones");
}

#[test]
fn tsquery_on_a_single_file() {
    let r = python_repo("tsqfile");
    let (code, out, _) = r.run(&["tsquery", "python", "(function_definition name: (identifier) @n)", "pkg/m1.py", "--no-cache"]);
    assert_eq!(code, 0);
    assert!(out.lines().count() >= 2 && out.lines().all(|l| l.starts_with("pkg/m1.py:")), "{out}");
}

#[test]
fn tsquery_names_a_missing_path_reads_paths_under_root_and_says_where_limit_stopped() {
    let r = python_repo("tsqpaths");
    let q = "(function_definition name: (identifier) @n)";
    // as grep: the missing path is named, the others are still searched, exit 2
    let (code, out, err) = r.run(&["tsquery", "python", q, "pkg/m1.py", "nope.py", "--no-cache"]);
    assert_eq!(code, 2, "{err}");
    assert!(out.lines().count() == 2 && err.contains("nope.py"), "{out}{err}");
    let (code, _, err) = r.run(&["tsquery", "python", q, "nope.py", "--no-cache"]);
    assert!(code == 2 && err.contains("nope.py"), "{err}");
    // with --root from another directory, a path not found here is read under the root
    let elsewhere = r.root.join("pkg");
    let (code, out, err) = run_in(&elsewhere, &["--root", r.root.to_str().unwrap(), "tsquery", "python", q, "pkg/m1.py", "--no-cache"], &[]);
    assert!(code == 0 && out.lines().count() == 2 && out.contains("make_widget_1"), "{code} {out}{err}");
    let (code, out, _) = r.run(&["tsquery", "python", q, "pkg", "--limit", "3", "--no-cache"]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 4, "{out}");
    assert_eq!(out.lines().last(), Some("(stopped at 3 matches: raise limit to see more)"));
    let (_, out, _) = r.run(&["tsquery", "python", q, "pkg", "--limit", "24", "--no-cache"]);
    assert!(!out.contains("stopped at"), "{out}");
}

#[test]
fn tsquery_prints_paths_as_grep_does_so_they_can_be_passed_back() {
    let r = Repo::new("tsqgiven");
    r.write("src/a.py", b"def f():\n    pass\n");
    assert_eq!(r.run(&["index"]).0, 0);
    let q = "(function_definition name: (identifier) @n)";
    let first = |cwd: &Path, args: &[&str]| {
        let (code, out, err) = run_in(cwd, &[&["tsquery", "python", q][..], args].concat(), &[]);
        assert_eq!(code, 0, "{args:?}: {err}");
        out.lines().next().unwrap_or("").to_string()
    };
    // the cache is filled by the first spelling and answers the others
    for (arg, want) in
        [(&["src"][..], "src/a.py"), (&["./src"], "./src/a.py"), (&["src/"], "src/a.py"), (&["src/a.py"], "src/a.py"), (&[], "src/a.py"), (&["."], "src/a.py")]
    {
        for extra in [&[][..], &["--no-cache"]] {
            let line = first(&r.root, &[arg, extra].concat());
            assert!(line.starts_with(&format!("{want}:1:5  @n")), "{arg:?} {extra:?}: {line}");
            let json = first(&r.root, &[arg, extra, &["--json"]].concat());
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(v["path"], want, "{arg:?} {extra:?}: {json}");
        }
    }
    // from a subdirectory and with --root, a printed path is one outline reads as it is
    let src = r.root.join("src");
    let root = r.root.to_str().unwrap();
    for (cwd, args) in [(&src, &["a.py"][..]), (&src, &[]), (&src, &["../src"]), (&src, &["src/a.py"])] {
        let line = first(cwd, args);
        let path = line.split(':').next().unwrap();
        assert!(path.ends_with("a.py"), "{args:?}: {line}");
        let (code, out, err) = run_in(cwd, &["outline", path], &[]);
        assert!(code == 0 && out.contains("f"), "{args:?} {path}: {out}{err}");
        let (code, out, err) = run_in(cwd, &["--root", root, "outline", path], &[]);
        assert!(code == 0 && out.contains("f"), "--root, {args:?} {path}: {out}{err}");
    }
    assert!(first(&src, &["a.py"]).starts_with("a.py:1:5"));
    assert!(first(&src, &[]).starts_with("a.py:1:5"));
}

#[test]
fn rdeps_counts_only_unresolved_imports_that_could_name_the_target() {
    let r = Repo::new("rdeps_js");
    r.write("web/a.js", b"export function a() { return 1 }\n");
    r.write("web/b.js", b"import { a } from './a'\nimport x from '../outside'\nimport 'side-effect'\nexport function b() { return a() }\n");
    let (code, out, err) = r.run(&["rdeps", "web/a.js"]);
    assert_eq!((code, out.as_str()), (0, "web/b.js:1  ./a\n"), "{err}");
    // a relative import whose path is another directory's `a`, and a bare package named `a`
    r.write("other/c.js", b"import './a'\nimport 'a'\n");
    let (_, out, _) = r.run(&["rdeps", "web/a.js"]);
    assert!(!out.contains("could not be resolved"), "{out}");
    // the target's directory, which has no index file: it may mean the target
    r.write("web/sub/d.js", b"import '..'\n");
    let (_, out, _) = r.run(&["rdeps", "web/a.js"]);
    assert!(out.contains("(1 more import could not be resolved and may name it)"), "{out}");
}

#[test]
fn fuzzy_regex_finds_near_misses_of_a_regex() {
    let r = Repo::new("fuzzyre");
    r.write("a.txt", b"def get_value(x):\ndef set_valu(y):\ndef fetch_all():\n");
    let (code, out, _) = r.run(&["grep", "--fuzzy-regex=1", "(get|set)_value", "a.txt"]);
    assert_eq!((code, out.as_str()), (0, "1:def get_value(x):\n2:def set_valu(y):\n"));
    let (code, out, _) = r.run(&["grep", "--fuzzy-regex", "-e", "fetch_al+x", "-e", "nothing", "a.txt"]);
    assert_eq!((code, out.as_str()), (0, "3:def fetch_all():\n"));
    let (code, _, err) = r.run(&["grep", "--fuzzy-regex=1", "^def", "a.txt"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, _) = r.run(&["grep", "--fuzzy-regex=1", "-F", "x", "a.txt"]);
    assert_eq!(code, 2);
}

#[test]
fn grep_index_update_answers_for_the_tree_as_it_is_now() {
    let r = python_repo("idxupdate");
    let (code, _, _) = r.run(&["grep", "--index", "--update", "-F", "make_widget_1("]);
    assert_eq!(code, 0, "--update builds a missing index");
    r.write("pkg/new.py", b"def fresh_name():\n    return 1\n");
    let (code, _, _) = r.run(&["grep", "--index", "fresh_name"]);
    assert_eq!(code, 1, "without --update the index is the old snapshot");
    let (code, out, _) = r.run(&["grep", "--index", "--update", "fresh_name"]);
    assert_eq!((code, out.trim()), (0, "pkg/new.py:1:def fresh_name():"));
    let (_, walked, _) = r.run(&["grep", "--sort", "widget"]);
    let (_, indexed, _) = r.run(&["grep", "--sort", "--index", "--update", "widget"]);
    assert_eq!(walked, indexed);
}

#[test]
fn max_results_is_never_exceeded() {
    // many matching files searched by several threads at once: each thread that finds a match after
    // the limit was reached elsewhere must drop it
    let r = Repo::new("maxres");
    for i in 0..400 {
        r.write(&format!("d{}/f{i}.txt", i % 20), "needle one\nneedle two\nneedle three\n".as_bytes());
    }
    for round in 0..30 {
        for extra in [&[][..], &["--json"][..]] {
            let small = if round % 2 == 0 { "0" } else { "256" };
            let mut args = vec!["grep", "--max-results", "5", "-j", "8", "needle"];
            args.extend_from_slice(extra);
            let (code, out, err) = run_in(&r.root, &args, &[("SCRUTATE_SMALL_TREE", small)]);
            assert_eq!(code, 0, "{err}");
            let n = out.lines().filter(|l| l.contains("needle")).count();
            assert!((1..=5).contains(&n), "round {round} {extra:?}: {n} matches\n{out}");
        }
    }
}

/// The text of one MCP tool call, through `serve` as an agent makes it.
fn mcp_text(root: &Path, tool: &str, args: serde_json::Value) -> String {
    let mut child =
        Command::new(BIN).args(["--root", root.to_str().unwrap(), "serve"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let hello = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}});
    let call = serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool, "arguments": args}});
    writeln!(stdin, "{hello}\n{call}").unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let reply = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()).find(|v| v["id"] == 2).unwrap();
    reply["result"]["content"][0]["text"].as_str().unwrap().to_string()
}

/// A module five others import and call, and a leaf nobody uses.
fn hub_repo(name: &str) -> Repo {
    let r = Repo::new(name);
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("hub.py", b"def helper(x):\n    return x + 1\n\n\ndef other_helper(x):\n    return helper(x) * 2\n");
    for i in 1..=5 {
        r.write(&format!("user{i}.py"), format!("import hub\n\n\ndef run_{i}():\n    return hub.helper({i})\n").as_bytes());
    }
    r.write("leaf.py", b"def lonely():\n    return 0\n");
    r
}

#[test]
fn map_ranks_the_hub_first_and_fits_every_budget() {
    let r = hub_repo("map");
    let (code, out, err) = r.run(&["map"]);
    assert_eq!(code, 0, "{err}");
    let files: Vec<&str> = out.lines().filter(|l| !l.starts_with(' ')).collect();
    assert!(files[0].starts_with("hub.py  (rank 1, score "), "{out}");
    assert!(files.last().unwrap().starts_with("leaf.py"), "the leaf comes last: {out}");
    // the hub's most called definition first
    let hub: Vec<&str> = out.lines().skip(1).take_while(|l| l.starts_with(' ')).collect();
    assert!(hub[0].contains(" helper  (6 calls)") && hub[1].contains("other_helper"), "{out}");
    assert_eq!(r.run(&["map"]).1, out, "two runs give the same bytes");
    assert!(out.len() > 300, "{out}");
    for budget in [180, 300, out.len()] {
        let (code, cut, _) = r.run(&["--max-bytes", &budget.to_string(), "map"]);
        assert_eq!(code, 0);
        assert!(cut.len() <= budget, "{budget}: {} bytes\n{cut}", cut.len());
        assert!(cut == out || cut.starts_with("hub.py") && cut.trim_end().ends_with("or narrow the request)"), "{budget}: {cut}");
        assert_eq!(r.run(&["--max-bytes", &budget.to_string(), "map"]).1, cut);
    }
    let (code, json, _) = r.run(&["map", "--json"]);
    assert_eq!(code, 0);
    let v = answer(&json);
    assert_eq!((v[0]["path"].as_str(), v[0]["rank"].as_u64()), (Some("hub.py"), Some(1)), "{json}");
    assert_eq!((v[0]["definitions"][0]["qualname"].as_str(), v[0]["definitions"][0]["calls"].as_u64()), (Some("helper"), Some(6)), "{json}");
    assert_eq!(mcp_text(&r.root, "repo_map", serde_json::json!({})), out.trim_end(), "the MCP tool gives the command's text");
    // a path keeps the files under it
    r.write("sub/inner.py", b"import hub\n\n\ndef inner():\n    return hub.helper(0)\n");
    let (code, out, _) = r.run(&["map", "sub"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("sub/inner.py  (rank 1, score ") && out.lines().all(|l| l.starts_with(' ') || l.starts_with("sub/")), "{out}");
    assert_eq!(r.run(&["map", "nothing_here"]).0, 2);
}

/// `top -> middle -> deep`, each in its own file.
fn chain_repo(name: &str) -> Repo {
    let r = Repo::new(name);
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("a.py", b"from b import middle\n\n\ndef top():\n    x = 1\n    return middle(x)\n");
    r.write("b.py", b"from c import deep\n\n\ndef middle(x):\n    return deep(x)\n");
    r.write("c.py", b"def deep(x):\n    return x\n");
    r
}

#[test]
fn path_follows_the_call_chain_and_says_when_there_is_none() {
    let r = chain_repo("path");
    let (code, out, err) = r.run(&["path", "top", "deep"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["a.py:4-6  [function] top", "  a.py:6  calls b.py:4-5  [function] middle", "  b.py:5  calls c.py:1-2  [function] deep"],
        "{out}"
    );
    // each step is one that callees gives
    assert!(r.run(&["callees", "top"]).1.contains("b.py:4-5"));
    assert!(r.run(&["callees", "b.py:4"]).1.contains("c.py:1-2"));
    let (code, json, _) = r.run(&["path", "top", "deep", "--json"]);
    assert_eq!(code, 0);
    let v = answer(&json);
    assert_eq!(v["steps"].as_array().unwrap().len(), 2, "{json}");
    assert_eq!((v["steps"][0]["path"].as_str(), v["steps"][0]["line"].as_i64()), (Some("a.py"), Some(6)), "{json}");
    assert_eq!(v["steps"][1]["definition"]["path"], "c.py", "{json}");
    assert_eq!(mcp_text(&r.root, "call_path", serde_json::json!({"from": "top", "to": "deep"})), out.trim_end());
    let (code, out, _) = r.run(&["path", "deep", "top"]);
    assert_eq!(code, 1);
    assert_eq!(out, "(no call path from c.py:1-2  [function] deep to a.py:4-6  [function] top)\n");
    let (code, out, _) = r.run(&["path", "top", "deep", "--depth", "1"]);
    assert_eq!(code, 1);
    assert!(out.contains("within 1 call: raise depth"), "the depth limit says so: {out}");
    // a name with two definitions lists them, and a location picks one
    r.write("d.py", b"def deep(x):\n    return -x\n");
    let (code, out, err) = r.run(&["path", "top", "deep"]);
    assert_eq!(code, 2, "{out}");
    assert!(err.contains("c.py:1-2") && err.contains("d.py:1-2") && err.contains("path:line"), "{err}");
    let (code, out, _) = r.run(&["path", "top", "c.py:1"]);
    assert_eq!(code, 0);
    assert!(out.ends_with("  b.py:5  calls c.py:1-2  [function] deep\n"), "{out}");
    assert_eq!(r.run(&["path", "top", "d.py:1"]).0, 1);
}

#[test]
fn path_takes_a_function_named_like_its_file_over_the_files_module() {
    let r = Repo::new("path_module_name");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("lib/core.py", b"def deep(x):\n    return x + 1\n");
    r.write("lib/mid.py", b"from lib.core import deep\n\n\ndef middle(x):\n    return deep(x)\n");
    r.write("lib/top.py", b"from lib.mid import middle\n\n\ndef top(x):\n    return middle(x)\n");
    let (code, out, err) = r.run(&["path", "top", "deep"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["lib/top.py:4-5  [function] top", "  lib/top.py:5  calls lib/mid.py:4-5  [function] middle", "  lib/mid.py:5  calls lib/core.py:1-2  [function] deep"],
        "{out}"
    );
}

#[test]
fn path_takes_a_call_the_resolver_cannot_tie_and_says_so() {
    // C is a language the resolver does not read: callees lists `deep` by name
    let r = Repo::new("path_c");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("a.c", b"#include \"b.h\"\n\nint top(int x) {\n    return deep(x);\n}\n");
    r.write("b.c", b"int deep(int x) {\n    return x;\n}\n");
    let (code, out, err) = r.run(&["path", "top", "deep"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.lines().nth(1), Some("  a.c:4  calls b.c:1-3  [function] deep  (by name: the resolver did not tie this call)"), "{out}");
    assert_eq!(answer(&r.run(&["path", "top", "deep", "--json"]).1)["steps"][0]["tied"], false);
}

/// A compressed file named on the command line is searched as a named file is: text with a NUL
/// is a binary match, as ripgrep 15.2.0 reports it; found by a walk, it is skipped, as ripgrep
/// skips it.
#[test]
fn a_named_compressed_file_whose_text_has_a_nul_is_a_binary_match() {
    use std::io::Write as _;
    let r = Repo::new("named_gz_binary");
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b"needle\0x\nneedle\n").unwrap();
    r.write("bin.gz", &e.finish().unwrap());
    let (code, out, _) = r.run(&["grep", "-z", "needle", "bin.gz"]);
    assert_eq!((code, out.as_str()), (0, "binary file matches (found \"\\0\" byte around offset 6)\n"));
    let (code, out, _) = r.run(&["grep", "-z", "-c", "needle", "bin.gz"]);
    assert_eq!((code, out.as_str()), (0, "2\n"));
    let (code, out, _) = r.run(&["grep", "-z", "needle"]);
    assert_eq!((code, out.as_str()), (1, ""));
}

/// A path that leaves the repository and comes back into it (`../../repo/pkg/a.py` from
/// `repo/pkg`) names the repository's file.
#[test]
fn a_path_that_leaves_the_repository_and_comes_back_names_its_file() {
    let r = Repo::new("out_and_back");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("pkg/a.py", b"def used():\n    return 1\n\ndef dead_one():\n    return used()\n");
    assert_eq!(r.run(&["index"]).0, 0);
    let name = r.root.file_name().unwrap().to_string_lossy().into_owned();
    let (code, out, err) = run_in(&r.root.join("pkg"), &["outline", &format!("../../{name}/pkg/a.py")], &[]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("function  used") && out.contains("function  dead_one"), "{out}");
}

/// `..` from a directory below the root names the root, for `dead` and `map` as for the other
/// commands that take a path.
#[test]
fn a_path_naming_the_root_from_below_it_is_the_whole_repository() {
    let r = Repo::new("root_from_below");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write("pkg/a.py", b"def used():\n    return 1\n\ndef dead_one():\n    return used()\n");
    r.write("b.py", b"from pkg.a import used\n\nused()\n");
    assert_eq!(r.run(&["index"]).0, 0);
    for cmd in ["dead", "map"] {
        let (code, whole, err) = r.run(&[cmd]);
        assert_eq!(code, 0, "{cmd}: {err}");
        let (code, out, err) = run_in(&r.root.join("pkg"), &[cmd, ".."], &[]);
        assert_eq!((code, out), (0, whole), "{cmd} ..: {err}");
    }
}
