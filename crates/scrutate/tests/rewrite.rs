//! `scrutate tsquery --rewrite` on trees made for each test: the diff it prints, the files it
//! writes, a second run, overlapping matches, text that does not parse, a file edited between the
//! read and the write, and what the index answers afterwards.

#[path = "support/canon.rs"]
mod canon;
#[path = "support/tag.rs"]
mod tag;
use canon::canon;
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
        tag::claim(&root);
        let _ = fs::remove_dir_all(&root);
        // its own repository, so the work tree above lends it no ignore file
        fs::create_dir_all(root.join(".git")).unwrap();
        Tree { root: canon(root) }
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

/// A link named on the command line is left alone, whether it points to a file of the root or out
/// of it: `--write` changes only the files it names.
#[cfg(unix)]
#[test]
fn a_link_to_a_file_inside_the_root_is_not_a_regular_file_and_is_left_alone() {
    let t = Tree::new("link_inside");
    let outside = Tree::new("link_inside_outside");
    t.write("a.py", "print(x)\n");
    outside.write("o.py", "print(secret)\n");
    std::os::unix::fs::symlink(t.root.join("a.py"), t.root.join("l.py")).unwrap();
    std::os::unix::fs::symlink(outside.root.join("o.py"), t.root.join("o.py")).unwrap();
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, "l.py", "o.py", "--rewrite", "log$args", "--write"]);
    assert_eq!((code, out.as_str()), (2, ""), "{err}");
    assert!(err.contains("l.py: not a regular file, left alone"), "{err}");
    assert!(err.contains("o.py: outside the root, left alone"), "{err}");
    assert_eq!(t.read("a.py"), "print(x)\n");
    assert_eq!(outside.read("o.py"), "print(secret)\n");
}

/// A file reached through a link to a directory of the root is left alone, named as the link
/// names it, whether the command line names the link or a file below it.
#[cfg(unix)]
#[test]
fn a_file_through_a_directory_link_inside_the_root_is_left_alone() {
    let t = Tree::new("dirlink_inside");
    t.write("real/b.py", "print(x)\n");
    std::os::unix::fs::symlink(t.root.join("real"), t.root.join("dl")).unwrap();
    for named in ["dl/b.py", "dl"] {
        let (code, out, err) = t.run(&["tsquery", "python", PRINT, named, "--rewrite", "log$args", "--write"]);
        assert_eq!((code, out.as_str()), (2, ""), "{named}: {err}");
        assert!(err.contains("dl/b.py: a directory on its path is a link, left alone"), "{named}: {err}");
        assert_eq!(t.read("real/b.py"), "print(x)\n", "{named}");
    }
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, "real", "--rewrite", "log$args", "--write"]);
    assert_eq!((code, out.as_str()), (0, "real/b.py: 1 replaced\n"), "{err}");
}

/// A link that leads back to the root, or up to it from below, is a link on the path too: the file
/// it reaches is left alone, and the root's own copy is untouched.
#[cfg(unix)]
#[test]
fn a_file_through_a_link_back_to_the_root_is_left_alone() {
    let t = Tree::new("dirlink_self");
    t.write("n.py", "print(x)\n");
    t.write("a/keep.txt", "");
    std::os::unix::fs::symlink(".", t.root.join("self")).unwrap();
    std::os::unix::fs::symlink("..", t.root.join("a/up")).unwrap();
    for named in ["self/n.py", "a/up/n.py", "self"] {
        let (code, out, err) = t.run(&["tsquery", "python", PRINT, named, "--rewrite", "log$args", "--write"]);
        assert_eq!((code, out.as_str()), (2, ""), "{named}: {err}");
        assert!(err.contains("a directory on its path is a link, left alone"), "{named}: {err}");
        assert_eq!(t.read("n.py"), "print(x)\n", "{named}");
    }
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, "n.py", "--rewrite", "log$args", "--write"]);
    assert_eq!((code, out.as_str()), (0, "n.py: 1 replaced\n"), "{err}");
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

/// A file already named as the write's temporary file is never opened or removed: the write is
/// refused, names it, and leaves it and the file it would have written as they were.
#[test]
fn a_file_with_the_temporary_name_is_left_alone_and_the_write_refused() {
    let t = Tree::new("tmp_taken");
    t.write("a.py", "print(x)\n");
    let tmp = format!(".a.py.scrutate-{}.tmp", std::process::id());
    t.write(&tmp, "the user's own file\n");
    let plan = scrutate::rewrite::plan("python", PRINT, "log$args", std::slice::from_ref(&t.root), &t.root).unwrap();
    let left = plan.write();
    assert!(left[0].as_deref().is_some_and(|w| w.contains(&tmp) && w.ends_with("left alone")), "{left:?}");
    assert_eq!(t.read(&tmp), "the user's own file\n");
    assert_eq!(t.read("a.py"), "print(x)\n");
}

/// A file over 1 MB is not searched and is always said. Named on the command line it gets
/// `PATH: over 1 MB, left alone` and exit 2 in every form (plain, `--json`, the rewrite's dry run
/// and `--write`), and is left unchanged; met in a walk it is one note with its count and name,
/// the exit code still by the matches, and a `skipped` record in `--json` and in the MCP tool's
/// envelope.
#[test]
fn a_file_over_one_megabyte_is_said_whether_named_or_met_in_a_walk() {
    let t = Tree::new("over_bound");
    let big = "print(x)\n".repeat(120_000);
    assert_eq!(big.len(), 1_080_000);
    t.write("big.py", &big);
    t.write("small.py", "print(y)\n");
    let forms = |path: &str| -> Vec<Vec<String>> {
        let q = ["tsquery", "python", PRINT, path].map(String::from).to_vec();
        let with = |extra: &[&str]| q.iter().cloned().chain(extra.iter().map(|s| s.to_string())).collect::<Vec<_>>();
        vec![with(&[]), with(&["--json"]), with(&["--rewrite", "log$args"]), with(&["--rewrite", "log$args", "--write"])]
    };
    for args in forms("big.py") {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, out, err) = t.run(&args);
        assert!(code == 2 && err.contains("big.py: over 1 MB, left alone"), "{args:?}: {code}\n{out}\n{err}");
        assert_eq!(t.read("big.py"), big, "{args:?}");
    }
    for args in forms(".") {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, out, err) = t.run(&args);
        assert!(code == 0 && err.contains("1 file over 1 MB was not searched: big.py") && !err.contains("left alone"), "{args:?}: {code}\n{out}\n{err}");
        if args.contains(&"--json") {
            let last: serde_json::Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
            assert!(last["type"] == "skipped" && last["paths"] == serde_json::json!(["big.py"]), "{out}");
        }
        assert_eq!(t.read("big.py"), big, "{args:?}");
    }
    assert_eq!(t.read("small.py"), "log(y)\n", "the walk's --write changed the file within the bound");
    // the MCP tool says the same, in its text and its envelope
    let session = scrutate::session::Session::open(&t.root, scrutate::session::Every::given(Some(0.0))).unwrap();
    session.set_cache_dir(t.root.with_extension("cache"));
    let c = scrutate::mcp::call_tool(
        &session,
        "structural_search",
        &serde_json::json!({"lang": "python", "query": PRINT}),
        &scrutate::mcp::Settings::new(None),
        &Default::default(),
    );
    assert!(c.text.contains("1 file over 1 MB was not searched: big.py"), "{}", c.text);
    assert_eq!(c.structured["skipped"]["paths"], serde_json::json!(["big.py"]), "{}", c.structured);
}

/// The bound is the file's size: one of exactly 1,000,000 bytes is read and rewritten, one byte
/// more and the file named is left alone with its line and exit 2.
#[test]
fn the_size_bound_reads_one_million_bytes_and_not_one_more() {
    let t = Tree::new("bound_edge");
    let at = format!("{}\n", "print(x)\n".repeat(111_111));
    assert_eq!(at.len(), 1_000_000);
    t.write("at.py", &at);
    t.write("past.py", &format!("{at}\n"));
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, "at.py", "--rewrite", "log$args", "--write"]);
    assert!(code == 0 && out.contains("at.py: 111111 replaced"), "{code}\n{out}\n{err}");
    let (code, out, err) = t.run(&["tsquery", "python", PRINT, "past.py", "--rewrite", "log$args", "--write"]);
    assert!(code == 2 && err.contains("past.py: over 1 MB, left alone") && !out.contains("replaced"), "{code}\n{out}\n{err}");
    assert_eq!(t.read("past.py"), format!("{at}\n"));
}

/// A second process that appends to a file while `--write` runs never loses an appended line: a
/// file changed before the check is left alone, and one changed between the check and the rename
/// is put back. Unix only: Windows checks nothing after the rename.
#[cfg(unix)]
#[test]
fn a_write_never_lands_on_top_of_another_writers_change() {
    let t = Tree::new("second_writer");
    let text = "print(x)\n".repeat(30_000);
    for round in 0..10u64 {
        t.write("f.py", &text);
        let path = t.root.join("f.py");
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(round * 40));
            let mut done = Vec::new();
            for i in 0..50 {
                use std::io::Write as _;
                let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
                writeln!(f, "# a{round}_{i}").unwrap();
                done.push(format!("# a{round}_{i}"));
            }
            done
        });
        let (code, _, err) = t.run(&["tsquery", "python", PRINT, "f.py", "--rewrite", "log$args", "--write"]);
        let appended = writer.join().unwrap();
        let now = t.read("f.py");
        let lost: Vec<&String> = appended.iter().filter(|l| !now.lines().any(|n| n == l.as_str())).collect();
        assert!(lost.is_empty(), "round {round}: exit {code}, {err}: lost {lost:?}");
    }
}
