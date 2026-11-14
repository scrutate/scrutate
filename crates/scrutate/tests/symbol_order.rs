//! The order of `find_symbol` answers: the name as typed before its case-folded namesakes, types
//! before functions, test files last within each, a definition before its prototype.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::fs;
use std::path::PathBuf;

struct Repo {
    root: PathBuf,
    s: scrutate::search::Searcher,
}

impl Repo {
    fn new(name: &str, files: &[(String, String)]) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_symorder_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, body) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
        }
        let root = canon(&root);
        let s = scrutate::open_updated(&root, true).unwrap().0.into_searcher();
        Repo { root, s }
    }

    /// (kind, qualname, path) of each answer
    fn symbol(&self, name: &str, limit: usize) -> Vec<(String, String, String)> {
        self.s.find_symbol(name, limit).unwrap().into_iter().map(|h| (h.kind, h.qualname, h.path)).collect()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn file(rel: &str, body: &str) -> (String, String) {
    (rel.to_string(), body.to_string())
}

#[test]
fn the_name_as_typed_comes_before_thirty_case_folded_namesakes() {
    let mut files: Vec<(String, String)> = (0..30).map(|i| file(&format!("src/m{i:02}.rs"), "pub fn error() -> u8 { 1 }\n")).collect();
    files.push(file("src/zz.rs", "pub struct Error { pub code: u8 }\n"));
    let r = Repo::new("thirty", &files);
    let got = r.symbol("Error", 20);
    assert_eq!(got.len(), 20);
    assert_eq!(got[0], ("struct".into(), "Error".into(), "src/zz.rs".into()), "{got:?}");
    assert!(got[1..].iter().all(|(k, q, _)| k == "function" && q == "error"), "{got:?}");
    // the folded lookup still finds the other spelling, after the exact one
    let lower = r.symbol("error", 40);
    assert_eq!(lower.len(), 31);
    assert!(lower[..30].iter().all(|(_, q, _)| q == "error"), "{lower:?}");
    assert_eq!(lower[30].1, "Error");
}

#[test]
fn a_constant_comes_before_a_method_whose_name_differs_in_case() {
    let java = "public class Config {\n    public static final int MAX = 5;\n    public int max() { return MAX; }\n}\n";
    let r = Repo::new("max", &[file("Config.java", java)]);
    let kinds = |n: &str| r.symbol(n, 20).into_iter().map(|(k, q, _)| (k, q)).collect::<Vec<_>>();
    let want = vec![("constant".to_string(), "Config.MAX".to_string()), ("method".to_string(), "Config.max".to_string())];
    assert_eq!(kinds("MAX"), want);
    assert_eq!(kinds("Config.MAX"), want);
    assert_eq!(kinds("max"), want.into_iter().rev().collect::<Vec<_>>());
}

#[test]
fn a_struct_comes_before_a_function_of_the_same_name() {
    let r = Repo::new("stat", &[file("a.c", "int stat(const char *p) { return 0; }\n"), file("b.c", "struct stat { int size; };\n")]);
    let got = r.symbol("stat", 20);
    assert_eq!(got.iter().map(|(k, _, p)| (k.as_str(), p.as_str())).collect::<Vec<_>>(), vec![("struct", "b.c"), ("function", "a.c")]);
}

#[test]
fn test_files_come_last_within_the_exact_and_the_folded_answers() {
    let r = Repo::new(
        "tests",
        &[file("tests/t.rs", "pub struct Error;\npub fn error() {}\n"), file("src/a.rs", "pub fn error() {}\n"), file("src/b.rs", "pub enum Error { A }\n")],
    );
    let got: Vec<(String, String)> = r.symbol("Error", 20).into_iter().map(|(_, q, p)| (q, p)).collect();
    let want = [("Error", "src/b.rs"), ("Error", "tests/t.rs"), ("error", "src/a.rs"), ("error", "tests/t.rs")];
    assert_eq!(got, want.map(|(q, p)| (q.to_string(), p.to_string())));
}

#[test]
fn a_definition_comes_before_its_prototype() {
    let r = Repo::new("proto", &[file("a.h", "int add(int a, int b);\n"), file("b.c", "int add(int a, int b) { int total = a + b; return total; }\n")]);
    let got = r.symbol("add", 20);
    assert_eq!(got.iter().map(|(_, _, p)| p.as_str()).collect::<Vec<_>>(), vec!["b.c", "a.h"]);
}
