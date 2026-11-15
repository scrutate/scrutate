//! The model set: a change of vector model makes the vectors anew without reading a file into
//! rows again, and leaves the index a fresh build with that model writes; a ranker is read only
//! by what ranks, and one that does not fit is an error.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/tag.rs"]
mod tag;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("scrutate_models_{}_{}", name, std::process::id()));
        tag::claim(&p);
        let _ = fs::remove_dir_all(&p);
        // a repository: no index in a directory above takes it over
        fs::create_dir_all(p.join(".git")).unwrap();
        fs::create_dir_all(&p).unwrap();
        Tmp(canon(&p))
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `scrutate --root <root> <args>` with `SCRUTATE_SEMANTIC` set to `model` unless it is None.
fn run(root: &Path, model: Option<&str>, args: &[&str]) -> (i32, String, String) {
    let mut c = Command::new(BIN);
    c.arg("--root").arg(root).args(args).stdin(Stdio::null()).env_remove("SCRUTATE_SEMANTIC").env_remove("SCRUTATE_RANKER");
    if let Some(m) = model {
        c.env("SCRUTATE_SEMANTIC", m);
    }
    let out = c.output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn write_tree(root: &Path) {
    for i in 0..6 {
        fs::write(
            root.join(format!("m{i}.py")),
            format!("class Shop{i}:\n    \"\"\"A shop that sells things.\"\"\"\n    def total(self, items):\n        return sum(x.price for x in items) + {i}\n\n    def parse_size(self, text):\n        # a size like 10M\n        return int(text[:-1]) * 1024\n\nLIMIT = {i}\n"),
        )
        .unwrap();
        fs::write(
            root.join(format!("r{i}.rs")),
            format!("/// Adds two numbers.\npub fn add{i}(a: i64, b: i64) -> i64 {{\n    a + b + {i}\n}}\n\nstruct Cart{i} {{ n: usize }}\n"),
        )
        .unwrap();
    }
}

/// Every row of the index and the bytes of every segment array, in manifest order; the counters
/// that only say how many writes happened are left out.
fn dump(root: &Path) -> String {
    let db = rusqlite::Connection::open(root.join(".scrutate/index.db")).unwrap();
    let mut out = String::new();
    let mut table = |sql: &str| {
        let mut q = db.prepare(sql).unwrap();
        let n = q.column_count();
        let rows = q
            .query_map([], |r| Ok((0..n).map(|i| format!("{:?}", r.get_ref(i).unwrap())).collect::<Vec<_>>().join("|")))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        out.push_str(&format!("{sql}\n{}\n", rows.join("\n")));
    };
    table("SELECT path, size, hash FROM files ORDER BY path");
    table("SELECT * FROM chunks ORDER BY id");
    table("SELECT * FROM refs ORDER BY chunk_id, callee");
    table("SELECT * FROM imports ORDER BY path, line, module");
    table("SELECT key, value FROM meta WHERE key NOT IN ('data_version', 'chunks_at', 'seg_counter', 'segments') ORDER BY key");
    let segments: Vec<String> =
        serde_json::from_str(&db.query_row("SELECT value FROM meta WHERE key='segments'", [], |r| r.get::<_, String>(0)).unwrap()).unwrap();
    for s in segments {
        let mut files: Vec<_> = fs::read_dir(root.join(".scrutate/seg").join(&s)).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        for f in files {
            out.push_str(&format!("{} {:?}\n", f.file_name().unwrap().to_string_lossy(), fs::read(&f).unwrap()));
        }
    }
    out
}

#[test]
fn a_change_of_vector_model_reads_no_file_and_writes_what_a_fresh_build_writes() {
    let (a, none, bundled) = (Tmp::new("switch"), Tmp::new("fresh_none"), Tmp::new("fresh_bundled"));
    for t in [&a, &none, &bundled] {
        write_tree(&t.0);
    }
    assert_eq!(run(&a.0, None, &["index", "--no-text"]).0, 0);
    assert_eq!(run(&none.0, Some("none"), &["index", "--no-text"]).0, 0);
    assert_eq!(run(&bundled.0, None, &["index", "--no-text"]).0, 0);
    let (code, out, _) = run(&a.0, Some("none"), &["index", "--no-text"]);
    assert_eq!(code, 0);
    assert!(out.contains(" 0 changed"), "{out}");
    assert_eq!(dump(&a.0), dump(&none.0));
    let (code, out, _) = run(&a.0, None, &["index", "--no-text"]);
    assert_eq!(code, 0);
    assert!(out.contains(" 0 changed"), "{out}");
    assert_eq!(dump(&a.0), dump(&bundled.0));
    // a search after the switch back reads the new vectors
    let (code, out, _) = run(&a.0, None, &["search", "parse a size like 10M", "-k", "3"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        run(&bundled.0, None, &["search", "parse a size like 10M", "-k", "3"]).1.replace(&bundled.0.display().to_string(), &a.0.display().to_string())
    );
}

#[test]
fn files_changed_before_a_change_of_vector_model_are_indexed_as_they_are_now() {
    for (from, to) in [(None, Some("none")), (Some("none"), None)] {
        let (a, b) = (Tmp::new("edit_switch"), Tmp::new("edit_same"));
        for t in [&a, &b] {
            write_tree(&t.0);
        }
        assert_eq!(run(&a.0, from, &["index", "--no-text"]).0, 0);
        assert_eq!(run(&b.0, to, &["index", "--no-text"]).0, 0);
        for t in [&a, &b] {
            fs::write(t.0.join("m2.py"), "def checkout(cart):\n    return cart.total()\n").unwrap();
            fs::remove_file(t.0.join("r4.rs")).unwrap();
            fs::write(t.0.join("new.py"), "def refund(order):\n    return -order.total\n").unwrap();
        }
        let (code, out, _) = run(&a.0, to, &["index", "--no-text"]);
        assert_eq!(code, 0);
        assert!(out.contains(" 2 changed"), "{out}");
        assert_eq!(run(&b.0, to, &["index", "--no-text"]).0, 0);
        assert_eq!(dump(&a.0), dump(&b.0), "from {from:?} to {to:?}");
    }
}

#[test]
fn a_ranker_is_read_only_to_rank_and_one_that_does_not_fit_is_an_error() {
    let t = Tmp::new("ranker");
    write_tree(&t.0);
    let junk = t.0.join("..").join(format!("scrutate_models_junk_ranker_{}.npz", std::process::id()));
    fs::write(&junk, b"not a ranker").unwrap();
    let with = |args: &[&str]| {
        let out = Command::new(BIN)
            .arg("--root")
            .arg(&t.0)
            .args(args)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("SCRUTATE_RANKER", &junk)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    assert_eq!(with(&["index", "--no-text"]).0, 0);
    let (code, out, _) = with(&["symbol", "total"]);
    assert_eq!(code, 0);
    assert!(out.contains("Shop0.total"), "{out}");
    let (code, _, err) = with(&["search", "total of the items"]);
    assert_ne!(code, 0);
    assert!(err.contains("ranker"), "{err}");
    let _ = fs::remove_file(&junk);
}
