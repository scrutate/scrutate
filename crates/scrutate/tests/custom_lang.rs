//! A `[[language]]` table in a repository's `.scrutate.toml` or in the user's configuration file
//! adds a language: its files get symbols, outlines, ranked hits and a `--type`, `languages`
//! lists it, a change of the table reparses its files, and a bad table is one error with its line.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

const TABLE: &str = r#"[[language]]
name = "mylang"
extensions = ["myl"]
line_comment = "--"
block_comment = ["{-", "-}"]
definitions = [
  { kind = "function", pattern = '^\s*fn\s+([A-Za-z_][A-Za-z0-9_]*)' },
  { kind = "class", pattern = '^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)' },
]
"#;

const SOURCE: &str = "-- Shapes and their areas.\n\n-- The area of a circle of radius r.\nfn circle_area(r)\n  pi * r * r\nend\n\ntype Rectangle\n  width\n  height\nend\n\n{- Doubles a number. -}\nfn double(x)\n  x * 2\nend\n";

/// A repository with its own `.git`, and a scratch home, under the build's temporary directory.
struct Tree {
    base: PathBuf,
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("custom_lang_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(base.join("home/.config")).unwrap();
        let t = Tree { root: fs::canonicalize(&root).unwrap(), base: fs::canonicalize(&base).unwrap() };
        t.write("shapes.myl", SOURCE);
        t.write("other.py", "def circle_area_py(r):\n    return 3 * r * r\n");
        t
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let home = self.base.join("home");
        let mut c = Command::new(BIN);
        c.args(args).current_dir(&self.root).stdin(Stdio::null()).env_remove("SCRUTATE_CONFIG").env_remove("SCRUTATE_ROOT");
        c.env("HOME", &home).env("XDG_CONFIG_HOME", home.join(".config")).env("XDG_STATE_HOME", home.join("state")).env("XDG_DATA_HOME", home.join("data"));
        c.env("SCRUTATE_SEMANTIC", "none");
        let out = c.output().expect("run scrutate");
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}"))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[test]
fn a_language_from_the_repository_s_table_is_indexed_searched_and_listed() {
    let t = Tree::new("indexed");
    t.write(".scrutate.toml", TABLE);
    let (code, out, err) = t.run(&["symbol", "circle_area"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("shapes.myl:4-6") && out.contains("function"), "{out}");
    let (_, out, _) = t.run(&["symbol", "Rectangle"]);
    assert!(out.contains("shapes.myl:8-11") && out.contains("class"), "{out}");
    let (code, out, err) = t.run(&["outline", "shapes.myl"]);
    assert_eq!(code, 0, "{out}{err}");
    for want in ["circle_area", "Rectangle", "double"] {
        assert!(out.contains(want), "outline lists {want}: {out}");
    }
    // the comment above a definition is its documentation: a ranked search finds it by those words
    let (code, out, err) = t.run(&["search", "doubles a number"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.lines().next().is_some_and(|l| l.contains("shapes.myl:14")), "{out}");
    let (_, out, _) = t.run(&["search", "area of a circle"]);
    assert!(out.lines().next().is_some_and(|l| l.contains("shapes.myl:4")), "{out}");

    let (code, out, err) = t.run(&["grep", "--type", "mylang", "-l", "circle_area", "."]);
    assert_eq!((code, out.trim()), (0, "./shapes.myl"), "{err}");
    let (_, out, _) = t.run(&["grep", "--type-list", "."]);
    assert!(out.lines().any(|l| l.starts_with("mylang") && l.contains("*.myl")), "{out}");

    let (code, out, _) = t.run(&["languages"]);
    assert_eq!(code, 0);
    let line = out.lines().find(|l| l.starts_with("mylang")).unwrap_or_else(|| panic!("{out}"));
    assert!(line.contains(".myl") && line.contains("custom: .scrutate.toml"), "{line}");
    let v = t.json(&["languages", "--json"]);
    let me = v.as_array().unwrap().iter().find(|l| l["id"] == "mylang").expect("mylang in languages --json");
    assert_eq!(me["tier"], "custom");
    assert_eq!(me["source"], ".scrutate.toml");
    assert_eq!(me["extensions"], serde_json::json!([".myl"]));
    assert_eq!(me["definition_kinds"], serde_json::json!(["class", "function"]));
    // other languages are parsed as before
    let (_, out, _) = t.run(&["symbol", "circle_area_py"]);
    assert!(out.contains("other.py:1-2"), "{out}");
}

#[test]
fn a_change_of_the_table_reparses_the_files_of_its_language() {
    let t = Tree::new("change");
    t.write(".scrutate.toml", TABLE);
    let (_, out, _) = t.run(&["symbol", "double"]);
    assert!(out.contains("shapes.myl:14-16"), "{out}");
    t.write(".scrutate.toml", &TABLE.replace(r"^\s*fn\s+", r"^\s*fn\s+d"));
    let (_, out, _) = t.run(&["symbol", "ouble"]);
    assert!(out.contains("shapes.myl:14-16"), "the new pattern names it `ouble`: {out}");
    let (_, out, _) = t.run(&["symbol", "circle_area"]);
    assert!(!out.contains("shapes.myl"), "the new pattern does not match circle_area: {out}");
    // without the table the file is read by the keyword heuristics again
    fs::remove_file(t.root.join(".scrutate.toml")).unwrap();
    let (_, out, _) = t.run(&["symbol", "double"]);
    assert!(out.contains("shapes.myl:14"), "{out}");
    let (_, out, _) = t.run(&["languages"]);
    assert!(!out.contains("mylang"), "{out}");
}

#[test]
fn the_user_s_configuration_file_adds_a_language_too() {
    let t = Tree::new("user");
    let dir = t.base.join("home/.config/scrutate");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), TABLE).unwrap();
    let (_, out, _) = t.run(&["symbol", "Rectangle"]);
    assert!(out.contains("shapes.myl:8-11"), "{out}");
    let (_, out, _) = t.run(&["languages"]);
    let line = out.lines().find(|l| l.starts_with("mylang")).unwrap_or_else(|| panic!("{out}"));
    assert!(line.contains("custom: ") && line.contains("config.toml"), "{line}");
    // the same name in the repository's file as well is one error naming that file
    t.write(".scrutate.toml", TABLE);
    let (code, _, err) = t.run(&["languages"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("config.toml:2:") && err.contains("mylang"), "{err}");
}

/// The one error a bad table gives: exit 2, one line, the file and the line named.
fn one_error(t: &Tree, toml: &str, line: usize, words: &str) {
    t.write(".scrutate.toml", toml);
    for args in [&["symbol", "circle_area"][..], &["languages"], &["grep", "--type", "mylang", "x", "."]] {
        let (code, out, err) = t.run(args);
        assert_eq!(code, 2, "{args:?} {toml}: {out}{err}");
        let lines: Vec<&str> = err.lines().collect();
        assert_eq!(lines.len(), 1, "one line: {err}");
        assert!(lines[0].contains(&format!(".scrutate.toml:{line}:")), "names the line {line}: {err}");
        assert!(lines[0].contains(words), "says {words:?}: {err}");
    }
}

/// The patterns of every table share one size budget, so a repository's table of many large
/// patterns is refused at once where it took gigabytes (each had the whole budget alone); many
/// small ones still load.
#[test]
fn the_definition_patterns_of_all_tables_share_one_size_budget() {
    let t = Tree::new("budget");
    let defs = |pattern: &str, n: usize| (0..n).map(|_| format!("  {{ kind = \"function\", pattern = '{pattern}' }},\n")).collect::<String>();
    t.write(".scrutate.toml", &format!("[[language]]\nname = \"big\"\nextensions = [\"big\"]\ndefinitions = [\n{}]\n", defs(r"(\w{1,300})", 10)));
    let t0 = std::time::Instant::now();
    let (code, out, err) = t.run(&["languages"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("does not compile") && err.contains("size"), "{err}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(10), "{:?}", t0.elapsed());
    t.write(".scrutate.toml", &format!("[[language]]\nname = \"small\"\nextensions = [\"sml\"]\ndefinitions = [\n{}]\n", defs(r"^\s*fn\s+(\w+)", 50)));
    let (code, out, err) = t.run(&["languages"]);
    assert_eq!(code, 0, "{out}{err}");
}

#[test]
fn a_bad_table_is_one_error_that_names_the_file_and_the_line() {
    let t = Tree::new("errors");
    one_error(&t, &TABLE.replace("name = \"mylang\"", "name = \"mylang"), 2, "");
    one_error(&t, &TABLE.replace(r"([A-Za-z_][A-Za-z0-9_]*)' },", r"([A-Za-z_' },"), 7, "does not compile");
    one_error(&t, &TABLE.replace(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)", r"^\s*type\s+[A-Za-z_]+"), 8, "group");
    one_error(&t, &TABLE.replace("line_comment", "line_coment"), 4, "line_coment");
    one_error(&t, &TABLE.replace("\"mylang\"", "\"python\""), 2, "built-in");
    one_error(&t, &TABLE.replace("[\"myl\"]", "[\"myl\", \"rs\"]"), 3, "rust");
    // a built-in language that claims a whole file name (CMakeLists.txt) claims its extension too
    one_error(&t, &TABLE.replace("[\"myl\"]", "[\"myl\", \"txt\"]"), 3, "cmake");
    one_error(&t, &TABLE.replace("kind = \"class\"", "kind = \"widget\""), 8, "widget");
    one_error(&t, &TABLE.replace("name = \"mylang\"", "name = \"my lang\""), 2, "letters");
}

#[test]
fn a_repository_s_table_cannot_name_a_file_or_reach_outside_the_repository() {
    let t = Tree::new("outside");
    let outside = t.base.join("outside.toml");
    fs::write(&outside, TABLE).unwrap();
    // a key that would read another file is no key of the table
    let include = format!("{}include = {:?}\n", TABLE, outside.display().to_string());
    one_error(&t, &include, 10, "include");
    let path_key = TABLE.replace("name = \"mylang\"", &format!("name = \"mylang\"\npath = {:?}", outside.display().to_string()));
    one_error(&t, &path_key, 3, "path");
    // a link out of the repository is not read as the repository's table
    fs::remove_file(t.root.join(".scrutate.toml")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, t.root.join(".scrutate.toml")).unwrap();
        let (code, out, err) = t.run(&["languages"]);
        assert_eq!(code, 0, "{err}");
        assert!(!out.contains("mylang"), "{out}");
    }
}
