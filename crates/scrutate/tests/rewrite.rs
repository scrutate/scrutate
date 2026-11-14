//! `scrutate tsquery --rewrite` on trees made for each test: the diff it prints, the files it
//! writes, a second run, overlapping matches, text that does not parse, a file edited between the
//! read and the write, and what the index answers afterwards.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

const PRINT: &str = r#"(call function: (identifier) @f (#eq? @f "print") arguments: (argument_list) @args) @match"#;

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("rewrite_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // its own repository, so the work tree above lends it no ignore file
        fs::create_dir_all(root.join(".git")).unwrap();
        Tree { root: fs::canonicalize(root).unwrap() }
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    /// (exit code, standard output, standard error)
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let state = self.root.with_extension("state");
        let out = Command::new(BIN)
            .args(["--root", self.root.to_str().unwrap()])
            .args(args)
            .current_dir(&self.root)
            .env("SCRUTATE_SEMANTIC", "none")
            .env("XDG_STATE_HOME", &state)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    /// The names in the tree, temporary files included.
    fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.root).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir_all(self.root.with_extension("state"));
    }
}

#[test]
fn print_becomes_log_as_a_diff_then_written_then_nothing_is_left_to_change() {
    let t = Tree::new("print");
    let before = "import os\n\n\ndef main(x, y):\n    print(x)\n    value = 1\n    print(\"a\", y)\n    return value\n";
    t.write("a.py", before);
    t.write("b.py", "def other():\n    return 2\n");
    let rw = ["tsquery", "python", PRINT, "--rewrite", "log$args"];
    let names = t.names();

    let (code, out, err) = t.run(&rw);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "--- a.py\n+++ a.py\n@@ -2,7 +2,7 @@\n \n \n def main(x, y):\n-    print(x)\n+    log(x)\n     value = 1\n-    print(\"a\", y)\n+    log(\"a\", y)\n     return value\n"
    );
    assert_eq!(t.read("a.py"), before, "the diff mode writes nothing");
    assert_eq!(t.names(), names);

    let (code, out, err) = t.run(&[&rw[..], &["--write"]].concat());
    assert_eq!((code, out.as_str(), err.as_str()), (0, "a.py: 2 replaced\n", ""));
    assert_eq!(t.read("a.py"), "import os\n\n\ndef main(x, y):\n    log(x)\n    value = 1\n    log(\"a\", y)\n    return value\n");
    assert_eq!(t.read("b.py"), "def other():\n    return 2\n");
    assert_eq!(t.names(), names, "no temporary file is left");

    for args in [&rw[..], &[&rw[..], &["--write"]].concat()] {
        assert_eq!(t.run(args), (1, String::new(), String::new()), "a second run finds nothing to change");
    }
}

#[test]
fn far_apart_changes_are_hunks_of_their_own_and_line_ends_stay_as_they_were() {
    let t = Tree::new("hunks");
    let mut lines: Vec<String> = (0..12).map(|i| format!("x{i} = {i}")).collect();
    lines[0] = "print(a)".into();
    lines[11] = "print(b)".into();
    t.write("a.py", &format!("{}\r\n{}", lines[..2].join("\r\n"), lines[2..].join("\n")));
    let (code, out, _) = t.run(&["tsquery", "python", PRINT, "--rewrite", "log$args"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "--- a.py\n+++ a.py\n@@ -1,4 +1,4 @@\n-print(a)\r\n+log(a)\r\n x1 = 1\r\n x2 = 2\n x3 = 3\n\
         @@ -9,4 +9,4 @@\n x8 = 8\n x9 = 9\n x10 = 10\n-print(b)\n\\ No newline at end of file\n+log(b)\n\\ No newline at end of file\n"
    );
    assert_eq!(t.run(&["tsquery", "python", PRINT, "--rewrite", "log$args", "--write"]).0, 0);
    assert_eq!(t.read("a.py"), format!("log(a)\r\nx1 = 1\r\n{}", lines[2..].join("\n").replace("print(b)", "log(b)")));
}

#[test]
fn a_javascript_call_is_rewritten_with_a_braced_capture_before_a_letter() {
    let t = Tree::new("js");
    t.write("app.js", "const r = fs.read(path, 'utf8');\nfs.read(other);\nconsole.log(r);\n");
    let q = r#"(call_expression function: (member_expression object: (identifier) @o (#eq? @o "fs") property: (property_identifier) @p) arguments: (arguments) @a) @match"#;
    let (code, out, err) = t.run(&["tsquery", "javascript", q, "--rewrite", "$o.${p}Async$a", "--write"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "app.js: 2 replaced\n", ""));
    assert_eq!(t.read("app.js"), "const r = fs.readAsync(path, 'utf8');\nfs.readAsync(other);\nconsole.log(r);\n");
    // `$$` is a dollar
    let (code, _, err) = t.run(&["tsquery", "javascript", "((identifier) @i (#eq? @i \"r\"))", "--rewrite", "$$r", "--write"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(t.read("app.js"), "const $r = fs.readAsync(path, 'utf8');\nfs.readAsync(other);\nconsole.log($r);\n");
}

#[test]
fn without_match_the_widest_capture_is_replaced() {
    let t = Tree::new("widest");
    t.write("a.py", "print(x)\n");
    let q = r#"(call function: (identifier) @f (#eq? @f "print") arguments: (argument_list) @args) @call"#;
    assert_eq!(t.run(&["tsquery", "python", q, "--rewrite", "log$args", "--write"]).0, 0);
    assert_eq!(t.read("a.py"), "log(x)\n");
}

#[test]
fn of_two_overlapping_matches_the_outer_one_is_rewritten_and_the_inner_one_reported() {
    let t = Tree::new("overlap");
    t.write("a.py", "print(print(x))\n");
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, "--rewrite", "log$args", "--write"]);
    assert_eq!((code, out.as_str()), (0, "a.py: 1 replaced\n"));
    assert_eq!(err, "scrutate: warning: a.py:1:7: inside the match at 1:1, not rewritten\n");
    // the inner capture's text is the original's
    assert_eq!(t.read("a.py"), "log(print(x))\n");
}

#[test]
fn a_file_that_does_not_parse_where_it_would_change_is_left_alone() {
    let t = Tree::new("broken");
    t.write("bad.py", "print(1 +)\n");
    t.write("good.py", "print(x)\n");
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, "--rewrite", "log$args", "--write"]);
    assert_eq!((code, out.as_str()), (2, "good.py: 1 replaced\n"));
    assert_eq!(err, "scrutate: warning: bad.py: a match holds text that does not parse, left alone\n");
    assert_eq!(t.read("bad.py"), "print(1 +)\n");
    // a template whose output does not parse changes nothing
    t.write("good.py", "print(x)\n");
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, "--rewrite", "log($args", "--write"]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.contains("good.py: the rewritten text does not parse, left alone"), "{err}");
    assert_eq!(t.read("good.py"), "print(x)\n");
}

#[test]
fn a_file_edited_between_the_read_and_the_write_is_left_alone_and_reported() {
    let t = Tree::new("race");
    t.write("a.py", "print(x)\n");
    t.write("b.py", "print(y)\n");
    let plan = scrutate::rewrite::plan("python", PRINT, "log$args", std::slice::from_ref(&t.root), &t.root).unwrap();
    assert_eq!(plan.files.len(), 2);
    t.write("a.py", "print(z)\n");
    let left = plan.write();
    assert_eq!(left.len(), 2);
    assert!(left[0].as_deref().is_some_and(|w| w.ends_with("a.py: changed since it was read, left alone")), "{left:?}");
    assert_eq!(left[1], None);
    assert_eq!(t.read("a.py"), "print(z)\n");
    assert_eq!(t.read("b.py"), "log(y)\n");
    assert!(!t.names().iter().any(|n| n.ends_with(".tmp")), "{:?}", t.names());
}

#[test]
fn the_index_answers_with_the_new_text_after_a_write() {
    let t = Tree::new("index");
    t.write("a.py", "def old_name():\n    return 1\n\n\ndef caller():\n    return old_name()\n");
    assert_eq!(t.run(&["index"]).0, 0);
    assert!(t.run(&["symbol", "old_name"]).1.contains("a.py"));
    let q = r#"((identifier) @match (#eq? @match "old_name"))"#;
    let (code, out, err) = t.run(&["tsquery", "python", q, "--rewrite", "new_name", "--write"]);
    assert_eq!((code, out.as_str(), err.as_str()), (0, "a.py: 2 replaced\n", ""));
    let (code, out, _) = t.run(&["symbol", "new_name"]);
    assert_eq!(code, 0);
    assert!(out.contains("a.py:1"), "{out}");
    assert_eq!(t.run(&["symbol", "old_name"]).0, 1);
    for grep in [&["grep", "-F", "new_name", "."][..], &["grep", "--index", "--update", "-F", "new_name"]] {
        let (code, out, err) = t.run(grep);
        assert_eq!(code, 0, "{grep:?} {err}");
        assert_eq!(out.lines().count(), 2, "{grep:?} {out}");
    }
    assert_eq!(t.run(&["grep", "--index", "--update", "-F", "old_name"]).0, 1);
}

#[test]
fn a_bad_template_is_an_error_and_the_options_are_checked() {
    let t = Tree::new("errors");
    t.write("a.py", "print(x)\n");
    for (template, says) in [("log($nope)", "the query has no @nope"), ("cost$", "write $$ for a dollar"), ("${args", "without its }")] {
        let (code, _, err) = t.run(&["tsquery", "python", PRINT, "--rewrite", template]);
        assert_eq!(code, 2);
        assert!(err.contains(says), "{template}: {err}");
    }
    assert_eq!(t.run(&["tsquery", "python", PRINT, "--write"]).0, 2, "--write needs --rewrite");
    assert_eq!(t.run(&["tsquery", "python", PRINT, "--rewrite", "x", "--json"]).0, 2);
    assert_eq!(t.run(&["tsquery", "python", PRINT, "--rewrite", "x", "--offset", "1"]).0, 2);
    // a size limit, as SCRUTATE_MAX_BYTES may set for every command, does not cut the diff
    let (code, out, _) = t.run(&["--max-bytes", "10", "tsquery", "python", PRINT, "--rewrite", "log$args"]);
    assert_eq!((code, out.as_str()), (0, "--- a.py\n+++ a.py\n@@ -1,1 +1,1 @@\n-print(x)\n+log(x)\n"));
    assert_eq!(t.read("a.py"), "print(x)\n");
}

#[cfg(unix)]
#[test]
fn a_link_out_of_the_root_is_never_written_and_permissions_are_kept() {
    use std::os::unix::fs::PermissionsExt;
    let t = Tree::new("link");
    let outside = Tree::new("link_outside");
    outside.write("o.py", "print(secret)\n");
    std::os::unix::fs::symlink(outside.root.join("o.py"), t.root.join("o.py")).unwrap();
    t.write("a.py", "print(x)\n");
    fs::set_permissions(t.root.join("a.py"), fs::Permissions::from_mode(0o640)).unwrap();
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, ".", "o.py", "--rewrite", "log$args", "--write"]);
    assert_eq!((code, out.as_str()), (2, "a.py: 1 replaced\n"), "{err}");
    assert!(err.contains("o.py: outside the root, left alone"), "{err}");
    assert_eq!(outside.read("o.py"), "print(secret)\n");
    assert_eq!(fs::metadata(t.root.join("a.py")).unwrap().permissions().mode() & 0o777, 0o640);
}

/// A directory swapped for a link to somewhere outside the root between the read and the write
/// leaves the file alone: the write goes through the directories as they were opened, never
/// through a link, and leaves nothing outside.
#[cfg(unix)]
#[test]
fn a_directory_swapped_for_a_link_outside_the_root_after_the_read_is_left_alone() {
    let t = Tree::new("swap_dir");
    t.write("d/a.py", "print(x)\n");
    let outside = Tree::new("swap_dir_outside");
    outside.write("d/a.py", "print(x)\n");
    let plan = scrutate::rewrite::plan("python", PRINT, "log$args", std::slice::from_ref(&t.root), &t.root).unwrap();
    assert_eq!(plan.files.len(), 1);
    fs::rename(t.root.join("d"), t.root.join("d.old")).unwrap();
    std::os::unix::fs::symlink(outside.root.join("d"), t.root.join("d")).unwrap();
    let left = plan.write();
    assert!(left[0].as_deref().is_some_and(|w| w.contains("a.py: ") && w.ends_with("left alone")), "{left:?}");
    assert_eq!(outside.read("d/a.py"), "print(x)\n");
    let left_there: Vec<String> = fs::read_dir(outside.root.join("d")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left_there, ["a.py"]);
}

/// The root itself swapped for a link to somewhere else after the plan leaves that place alone:
/// the plan holds the root it opened and writes through it.
#[cfg(unix)]
#[test]
fn the_root_swapped_for_a_link_after_the_plan_is_never_written_through() {
    let t = Tree::new("swap_root");
    t.write("a.py", "print(x)\n");
    let outside = Tree::new("swap_root_outside");
    outside.write("a.py", "print(x)\n");
    let plan = scrutate::rewrite::plan("python", PRINT, "log$args", std::slice::from_ref(&t.root), &t.root).unwrap();
    assert_eq!(plan.files.len(), 1);
    let moved = t.root.with_extension("moved");
    let _ = fs::remove_dir_all(&moved);
    fs::rename(&t.root, &moved).unwrap();
    std::os::unix::fs::symlink(&outside.root, &t.root).unwrap();
    let left = plan.write();
    assert_eq!(outside.read("a.py"), "print(x)\n", "{left:?}");
    let there: Vec<String> = fs::read_dir(&outside.root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(!there.iter().any(|n| n.ends_with(".tmp")), "{there:?}");
    // the write went to the directory the plan opened, wherever it is now
    assert_eq!(fs::read_to_string(moved.join("a.py")).unwrap(), "log(x)\n", "{left:?}");
    fs::remove_file(&t.root).unwrap();
    let _ = fs::remove_dir_all(&moved);
}

/// `mklink /J LINK TARGET`: a junction, which needs no privilege on Windows.
#[cfg(windows)]
fn junction(link: &Path, target: &Path) {
    let plain = |p: &Path| p.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    let out = Command::new("cmd").args(["/C", "mklink", "/J"]).arg(plain(link)).arg(plain(target)).output().unwrap();
    assert!(out.status.success(), "mklink /J: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

/// On Windows, a directory swapped for a junction to somewhere outside the root between the read
/// and the write leaves the file alone, and nothing is written outside.
#[cfg(windows)]
#[test]
fn a_directory_swapped_for_a_junction_outside_the_root_after_the_read_is_left_alone() {
    let t = Tree::new("swap_dir_junction");
    t.write("d/a.py", "print(x)\n");
    let outside = Tree::new("swap_dir_junction_outside");
    outside.write("d/a.py", "print(x)\n");
    let plan = scrutate::rewrite::plan("python", PRINT, "log$args", std::slice::from_ref(&t.root), &t.root).unwrap();
    assert_eq!(plan.files.len(), 1);
    fs::rename(t.root.join("d"), t.root.join("d.old")).unwrap();
    junction(&t.root.join("d"), &outside.root.join("d"));
    let left = plan.write();
    assert!(left[0].as_deref().is_some_and(|w| w.contains("a.py: ") && w.ends_with("left alone")), "{left:?}");
    assert_eq!(outside.read("d/a.py"), "print(x)\n");
    let there: Vec<String> = fs::read_dir(outside.root.join("d")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(there, ["a.py"]);
    fs::remove_dir(t.root.join("d")).unwrap();
}

/// On Windows, the root itself swapped for a junction after the plan is never written through:
/// the write goes to the directory the plan opened, wherever it is now.
#[cfg(windows)]
#[test]
fn the_root_swapped_for_a_junction_after_the_plan_is_never_written_through() {
    let t = Tree::new("swap_root_junction");
    t.write("a.py", "print(x)\n");
    let outside = Tree::new("swap_root_junction_outside");
    outside.write("a.py", "print(x)\n");
    let plan = scrutate::rewrite::plan("python", PRINT, "log$args", std::slice::from_ref(&t.root), &t.root).unwrap();
    assert_eq!(plan.files.len(), 1);
    let moved = t.root.with_extension("moved");
    let _ = fs::remove_dir_all(&moved);
    fs::rename(&t.root, &moved).unwrap();
    junction(&t.root, &outside.root);
    let left = plan.write();
    assert_eq!(outside.read("a.py"), "print(x)\n", "{left:?}");
    let there: Vec<String> = fs::read_dir(&outside.root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(!there.iter().any(|n| n.ends_with(".tmp")), "{there:?}");
    assert_eq!(fs::read_to_string(moved.join("a.py")).unwrap(), "log(x)\n", "{left:?}");
    fs::remove_dir(&t.root).unwrap();
    let _ = fs::remove_dir_all(&moved);
}

/// On Windows, a junction named as a file of the root (its target outside, holding the same file)
/// is never read or written through: a junction is not a regular file.
#[cfg(windows)]
#[test]
fn a_junction_out_of_the_root_is_never_written() {
    let t = Tree::new("junction_link");
    let outside = Tree::new("junction_link_outside");
    outside.write("o/x.py", "print(secret)\n");
    junction(&t.root.join("o"), &outside.root.join("o"));
    t.write("a.py", "print(x)\n");
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, ".", "--rewrite", "log$args", "--write"]);
    assert_eq!((code, out.as_str()), (0, "a.py: 1 replaced\n"), "{err}");
    assert_eq!(outside.read("o/x.py"), "print(secret)\n");
    fs::remove_dir(t.root.join("o")).unwrap();
}
