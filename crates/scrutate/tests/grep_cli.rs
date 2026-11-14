//! `scrutate grep` and the command line as a user meets them: environment switches, odd
//! arguments, per-file limits, context lines, globs over several roots and the paths printed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let root = std::env::temp_dir().join(format!("scrutate_grepcli_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Dir(fs::canonicalize(&root).unwrap())
    }
    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
    }
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        run(&self.0, args.iter().map(|a| a.as_ref()).collect::<Vec<&std::ffi::OsStr>>().as_slice(), &[])
    }
    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        run(&self.0, args.iter().map(|a| a.as_ref()).collect::<Vec<&std::ffi::OsStr>>().as_slice(), env)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(cwd: &Path, args: &[&std::ffi::OsStr], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(cwd).stdin(Stdio::null()).env_remove("NO_COLOR");
    for v in ["SCRUTATE_MCP_STRUCTURED", "SCRUTATE_GREP_HIDDEN", "SCRUTATE_GREP_NO_IGNORE", "SCRUTATE_GREP_SMART_CASE", "SCRUTATE_GREP_FOLLOW"] {
        c.env_remove(v);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("run scrutate");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn sorted(out: &str) -> Vec<String> {
    let mut v: Vec<String> = out.lines().map(String::from).collect();
    v.sort();
    v
}

#[test]
fn on_off_environment_variables_take_one_and_empty() {
    let d = Dir::new("env");
    d.write(".hidden/a.txt", b"needle\n");
    d.write("ignored/b.txt", b"needle\n");
    d.write(".ignore", b"ignored/\n");
    d.write("c.txt", b"Needle\n");
    for var in ["NO_COLOR", "SCRUTATE_GREP_HIDDEN", "SCRUTATE_GREP_NO_IGNORE", "SCRUTATE_GREP_SMART_CASE", "SCRUTATE_GREP_FOLLOW", "SCRUTATE_MCP_STRUCTURED"] {
        for v in ["1", "", "0", "true", "off"] {
            let (code, _, err) = d.run_env(&["grep", "needle"], &[(var, v)]);
            assert!(code == 0 || code == 1, "{var}={v:?}: exit {code}, {err}");
            let (code, _, err) = d.run_env(&["languages"], &[(var, v)]);
            assert_eq!(code, 0, "{var}={v:?}: {err}");
        }
    }
    let (code, _, err) = d.run_env(&["serve"], &[("SCRUTATE_MCP_STRUCTURED", "1"), ("NO_COLOR", "1")]);
    assert_eq!(code, 0, "serve: {err}");
    let has = |env: &[(&str, &str)], what: &str| d.run_env(&["grep", "-l", "needle"], env).1.contains(what);
    assert!(has(&[("SCRUTATE_GREP_HIDDEN", "1")], ".hidden/a.txt"));
    assert!(has(&[("SCRUTATE_GREP_HIDDEN", "yes")], ".hidden/a.txt"));
    assert!(!has(&[("SCRUTATE_GREP_HIDDEN", "0")], ".hidden/a.txt"));
    assert!(!has(&[("SCRUTATE_GREP_HIDDEN", "")], ".hidden/a.txt"));
    assert!(has(&[("SCRUTATE_GREP_NO_IGNORE", "on")], "ignored/b.txt"));
    assert!(!has(&[("SCRUTATE_GREP_NO_IGNORE", "false")], "ignored/b.txt"));
    assert!(has(&[("SCRUTATE_GREP_SMART_CASE", "1")], "c.txt"));
    assert!(!has(&[("SCRUTATE_GREP_SMART_CASE", "no")], "c.txt"));
}

#[cfg(unix)]
#[test]
fn arguments_that_are_not_utf8_are_errors_not_aborts() {
    use std::os::unix::ffi::OsStrExt;
    let d = Dir::new("utf8");
    d.write("a.txt", b"foo\n");
    let bad = std::ffi::OsStr::from_bytes(b"d1/\xff.txt");
    for args in [vec!["grep".as_ref(), "foo".as_ref(), bad], vec!["grep".as_ref(), bad], vec!["grep".as_ref(), "--json".as_ref(), "foo".as_ref(), bad]] {
        let (code, _, err) = run(&d.0, &args, &[]);
        assert!(code == 1 || code == 2, "exit {code}: {err}");
        assert!(!err.contains("panicked"), "{err}");
    }
}

#[test]
fn a_pattern_that_starts_with_a_hyphen_is_given_with_e() {
    let d = Dir::new("hyphen");
    d.write("a.txt", b"a->b\nx = -1\n--foo\n");
    for (args, line) in [
        (vec!["grep", "-e", "->", "a.txt"], "1:a->b\n"),
        (vec!["grep", "-F", "-e", "-1", "a.txt"], "2:x = -1\n"),
        (vec!["grep", "--regexp", "--foo", "a.txt"], "3:--foo\n"),
        (vec!["grep", "-e", "->", "-e", "-1", "a.txt"], "1:a->b\n2:x = -1\n"),
        (vec!["grep", "--", "->", "a.txt"], "1:a->b\n"),
    ] {
        let os: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_ref()).collect();
        let (code, out, err) = run(&d.0, &os, &[]);
        assert_eq!((code, out.as_str()), (0, line), "{args:?}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn with_e_a_first_path_that_is_not_utf8_is_a_path() {
    use std::os::unix::ffi::OsStrExt;
    let d = Dir::new("utf8path");
    d.write("a.txt", b"Foo bar\n");
    let bad = std::ffi::OsStr::from_bytes(b"\xff");
    // a missing path, as ripgrep reports it, and the search goes on
    let (code, out, err) = run(&d.0, &["grep".as_ref(), "-e".as_ref(), "Foo".as_ref(), bad, "a.txt".as_ref()], &[]);
    assert_eq!((code, out.as_str()), (2, "a.txt:1:Foo bar\n"), "{err}");
    assert!(err.contains("No such file or directory"), "{err}");
    // as a pattern it is still refused
    let usage = "error: invalid UTF-8 was detected in one or more arguments\n\nUsage: scrutate grep [OPTIONS] [PATTERN] [PATHS]...\n\nFor more information, try '--help'.\n";
    for args in [vec!["grep".as_ref(), bad, "a.txt".as_ref()], vec!["grep".as_ref(), "--json".as_ref(), bad]] {
        assert_eq!(run(&d.0, &args, &[]), (2, String::new(), usage.to_string()), "{args:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_link_named_on_the_command_line_is_searched_as_ripgrep_does() {
    let d = Dir::new("rootlinks");
    d.write("real/a.txt", b"foo\n");
    d.write("deep/sub/b.txt", b"foo\n");
    d.write("file.txt", b"foo\n");
    for (link, to) in [("link", "real"), ("dlink", "deep"), ("flink", "file.txt"), ("broken", "nowhere")] {
        std::os::unix::fs::symlink(to, d.0.join(link)).unwrap();
    }
    let summary = |out: &str| out.lines().last().unwrap_or("").split(",\"files_matched").next().unwrap_or("").split("\"errors\"").nth(1).map(String::from);
    // the walk of one thread (a root with no subdirectory) and the parallel one
    for small in ["256", "0"] {
        for (args, want_out, want_err) in [
            (&["foo", "link"][..], "link/a.txt:1:foo\n", ""),
            (&["-L", "foo", "link"], "link/a.txt:1:foo\n", ""),
            (&["foo", "dlink"], "dlink/sub/b.txt:1:foo\n", ""),
            (&["foo", "flink"], "1:foo\n", ""),
            (&["foo", "broken"], "", "No such file or directory (os error 2)\n"),
        ] {
            let code = if want_out.is_empty() { 2 } else { 0 };
            let env = [("SCRUTATE_SMALL_TREE", small)];
            let (got_code, out, err) = d.run_env(&[&["grep"][..], args].concat(), &env);
            assert_eq!((got_code, out.as_str()), (code, want_out), "{small} {args:?}");
            // the one-thread walk words a broken link as ripgrep does, "IO error for operation on broken: ..."
            assert!(
                if want_err.is_empty() { err.is_empty() } else { err.starts_with("scrutate: warning: broken: ") && err.ends_with(want_err) },
                "{small} {args:?}: {err}"
            );
            let (_, out, _) = d.run_env(&[&["grep", "--json"][..], args].concat(), &env);
            assert_eq!(summary(&out).as_deref(), Some(if code == 0 { ":0" } else { ":1" }), "{small} {args:?}: {out}");
        }
    }
}

fn three_foo_files(d: &Dir, n: usize) {
    for i in 0..n {
        d.write(&format!("d1/f{i}.txt"), b"foo 1\nx\nfoo 3\ny\nfoo 5\nz\n");
    }
}

#[test]
fn max_count_is_a_per_file_limit_of_the_search() {
    let d = Dir::new("maxcount");
    three_foo_files(&d, 8);
    let (_, out, _) = d.run(&["grep", "-m", "1", "--max-results", "5", "foo", "d1"]);
    assert_eq!(out.lines().count(), 5, "{out}");
    let (_, out, _) = d.run(&["grep", "-m", "1", "-A", "1", "foo", "d1/f0.txt"]);
    assert_eq!(out, "1:foo 1\n2-x\n");
    // as ripgrep: a match inside the trailing context still shows as a match
    let (_, out, _) = d.run(&["grep", "-m", "1", "-A", "3", "foo", "d1/f0.txt"]);
    assert_eq!(out, "1:foo 1\n2-x\n3:foo 3\n4-y\n");
    let (_, out, _) = d.run(&["grep", "-c", "-m", "2", "foo", "d1/f0.txt"]);
    assert_eq!(out, "2\n");
    let (_, out, _) = d.run(&["grep", "-v", "-m", "2", "foo", "d1/f0.txt"]);
    assert_eq!(out, "2:x\n4:y\n");
    let (_, out, _) = d.run(&["grep", "-U", "-m", "1", r"foo 1\nx", "d1/f0.txt"]);
    assert_eq!(out, "1:foo 1\n2:x\n");
    let (_, out, _) = d.run(&["grep", "--json", "-m", "1", "foo", "d1"]);
    let summary = out.lines().find(|l| l.contains("\"summary\"")).unwrap();
    assert!(summary.contains("\"matches\":8"), "{summary}");
    assert_eq!(out.lines().filter(|l| l.contains("\"type\":\"match\"")).count(), 8);
    // a file large enough to be searched in several ranges: still one limit for the whole file
    let mut big = Vec::new();
    for i in 0..20000 {
        big.extend_from_slice(format!("line {i} foo\n").as_bytes());
    }
    d.write("big.txt", &big);
    let split = [("SCRUTATE_SPLIT_FILE_BYTES", "4096"), ("SCRUTATE_THREADS", "4")];
    let (_, out, _) = d.run_env(&["grep", "-m", "3", "foo", "big.txt"], &split);
    assert_eq!(out.lines().count(), 3, "{out}");
    let (_, out, _) = d.run_env(&["grep", "-c", "-m", "3", "foo", "big.txt"], &split);
    assert_eq!(out, "3\n");
    let (_, out, _) = d.run_env(&["grep", "-c", "foo", "big.txt"], &split);
    assert_eq!(out, "20000\n");
}

#[test]
fn max_results_holds_for_counts_and_file_lists() {
    let d = Dir::new("maxresults");
    three_foo_files(&d, 4);
    let (_, out, _) = d.run(&["grep", "-c", "--max-results", "2", "foo", "d1"]);
    let total: u64 = out.lines().map(|l| l.rsplit(':').next().unwrap().parse::<u64>().unwrap()).sum();
    assert_eq!(total, 2, "{out}");
    let (_, out, _) = d.run(&["grep", "-l", "--max-results", "2", "foo", "d1"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    let (code, _, _) = d.run(&["grep", "-l", "--max-results", "2", "foo", "d1"]);
    assert_eq!(code, 0);
}

#[test]
fn max_results_statistics_count_only_the_matches_printed() {
    let d = Dir::new("maxstats");
    for i in 0..16 {
        d.write(&format!("f{i}.txt"), "foo\n".repeat(200).as_bytes());
    }
    for _ in 0..20 {
        for j in ["2", "4", "8"] {
            for max in ["1", "3"] {
                let (_, out, err) = d.run(&["grep", "--stats", "-j", j, "--max-results", max, "foo"]);
                assert_eq!(err.split(' ').next(), Some(out.lines().count().to_string().as_str()), "-j {j}: {out}{err}");
                assert_eq!(out.lines().count().to_string(), max, "-j {j}: {out}");
                let (_, out, _) = d.run(&["grep", "--json", "-j", j, "--max-results", max, "foo"]);
                let printed = out.lines().filter(|l| l.contains(r#""type":"match""#)).count();
                assert!(out.contains(&format!(r#""matches":{printed},"#)) && printed.to_string() == max, "-j {j}: {out}");
            }
        }
    }
}

#[test]
fn context_lines_of_crlf_files_lose_the_carriage_return_too() {
    let d = Dir::new("crlf");
    d.write("crlf.txt", b"ctx1\r\nfoo\r\nctx2\r\n");
    let (_, out, _) = d.run(&["grep", "-C1", "foo", "crlf.txt"]);
    assert_eq!(out, "1-ctx1\n2:foo\n3-ctx2\n");
    let (_, out, _) = d.run(&["grep", "--json", "-C1", "foo", "crlf.txt"]);
    assert!(!out.contains("\\r"), "{out}");
}

#[test]
fn globs_with_a_slash_apply_under_every_root() {
    let d = Dir::new("globs");
    d.write("dirA/src/x.rs", b"needle\n");
    d.write("dirB/src/x.rs", b"needle\n");
    d.write("dirB/other/x.rs", b"needle\n");
    let (_, out, _) = d.run(&["grep", "-l", "-g", "src/*.rs", "needle", "dirA", "dirB"]);
    assert_eq!(sorted(&out), ["dirA/src/x.rs", "dirB/src/x.rs"]);
    let (_, out, _) = d.run(&["grep", "-l", "-g", "!src/*.rs", "needle", "dirA", "dirB"]);
    assert_eq!(sorted(&out), ["dirB/other/x.rs"]);
    let (_, out, _) = d.run(&["grep", "-l", "--max-results", "1", "-g", "src/*.rs", "needle", "dirA", "dirB"]);
    assert_eq!(out.lines().count(), 1, "{out}");
}

#[test]
fn paths_are_printed_as_given() {
    let d = Dir::new("paths");
    d.write("dir/a.txt", b"needle\n");
    d.write("top.txt", b"needle\n");
    let abs = d.0.join("dir").to_string_lossy().into_owned();
    for (args, want) in [
        (vec![], vec!["dir/a.txt", "top.txt"]),
        (vec!["."], vec!["./dir/a.txt", "./top.txt"]),
        (vec!["dir"], vec!["dir/a.txt"]),
        (vec!["dir/"], vec!["dir/a.txt"]),
        (vec!["./top.txt"], vec!["./top.txt"]),
        (vec!["dir", "./top.txt"], vec!["./top.txt", "dir/a.txt"]),
    ] {
        let mut a = vec!["grep", "-l", "needle"];
        a.extend(&args);
        assert_eq!(sorted(&d.run(&a).1), want, "{args:?}");
    }
    let (_, out, _) = d.run(&["grep", "-l", "needle", &abs]);
    // Windows: `/` between the parts, and the plain form of the verbatim path `canonicalize` gave
    let shown = if cfg!(windows) { abs.trim_start_matches(r"\\?\").replace('\\', "/") } else { abs.clone() };
    assert_eq!(out, format!("{shown}/a.txt\n"));
    let (_, out, _) = d.run(&["grep", "needle", "dir"]);
    assert_eq!(out, "dir/a.txt:1:needle\n");
    let (_, out, _) = d.run(&["grep", "--json", "needle", "dir"]);
    assert!(out.contains("\"path\":\"dir/a.txt\""), "{out}");
    let (_, out, _) = d.run(&["grep", "-c", "needle", "dir"]);
    assert_eq!(out, "dir/a.txt:1\n");
}

#[test]
fn a_size_too_large_for_64_bits_is_refused() {
    let d = Dir::new("size");
    d.write("a.txt", b"foo\n");
    let (code, _, err) = d.run(&["grep", "--max-filesize", "17179869184G", "foo"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("bad size"), "{err}");
}

#[test]
fn a_case_folded_class_that_matches_nothing_finds_nothing() {
    let d = Dir::new("emptyclass");
    d.write("e.txt", b"xab\n");
    for pat in [r"ab[^\s\S]", r"(?-u)ab[^\x00-\xff]"] {
        let (code, out, err) = d.run(&["grep", "-i", pat, "e.txt"]);
        assert_eq!((code, out.as_str()), (1, ""), "{pat}: {err}");
    }
}

#[test]
fn after_and_before_win_over_context_in_either_order() {
    let d = Dir::new("ctxorder");
    d.write("a.txt", b"a\nb\nfoo\nc\nd\ne\n");
    for args in [["-C1", "-A3"], ["-A3", "-C1"]] {
        let (_, out, err) = d.run(&[&["grep"][..], &args, &["foo", "a.txt"]].concat());
        assert_eq!(out, "2-b\n3:foo\n4-c\n5-d\n6-e\n", "{args:?}: {err}");
    }
    let (_, out, _) = d.run(&["grep", "-B2", "-C0", "foo", "a.txt"]);
    assert_eq!(out, "1-a\n2-b\n3:foo\n");
}

/// `grep foo . > out.txt` in the tree it searches, with the file size capped at 10 MB so a search
/// that reads its own output stops (SIGXFSZ) instead of growing without bound.
#[cfg(unix)]
#[test]
fn a_walk_skips_the_file_standard_output_writes() {
    let d = Dir::new("ownoutput");
    let lines: String = (0..2000).map(|i| format!("foo {i}\n")).collect();
    for i in 0..50 {
        d.write(&format!("f{i:02}.txt"), lines.as_bytes());
    }
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("ulimit -f 20000; exec '{BIN}' grep foo . > out.txt"))
        .current_dir(&d.0)
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = fs::read_to_string(d.0.join("out.txt")).unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!text.lines().any(|l| l.starts_with("out.txt:") || l.starts_with("./out.txt:")), "its own output was searched");
    assert_eq!(text.lines().count(), 50 * 2000);
}

/// A write that fails for another reason than a closed pipe (here the file size limit, with
/// SIGXFSZ ignored so the write returns EFBIG) is an error, as in ripgrep.
#[cfg(unix)]
#[test]
fn a_failed_write_to_standard_output_is_an_error() {
    let d = Dir::new("writefails");
    let lines: String = (0..5000).map(|i| format!("foo {i}\n")).collect();
    d.write("a.txt", lines.as_bytes());
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("trap '' XFSZ; ulimit -f 20; exec '{BIN}' grep foo a.txt > out.txt"))
        .current_dir(&d.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{err}");
    assert!(err.contains("os error 27"), "{err}");
}

#[test]
fn smart_case_folds_only_a_pattern_with_lowercase_literals() {
    let d = Dir::new("smart");
    d.write("a.txt", b"ABC\nabc\n");
    for (pat, want) in [
        (r"\p{Ll}+", vec!["2:abc"]),
        (r"\w+", vec!["1:ABC", "2:abc"]),
        (r"[a-c]+", vec!["1:ABC", "2:abc"]),
        (r"\p{Ll}b", vec!["1:ABC", "2:abc"]),
        (r"[[:lower:]]+", vec!["2:abc"]),
        (r"Ab", vec![]),
    ] {
        assert_eq!(sorted(&d.run(&["grep", "-S", pat, "a.txt"]).1), want, "{pat}");
    }
}

#[test]
fn a_search_path_that_does_not_exist_is_an_error() {
    let d = Dir::new("missing");
    d.write("a.txt", b"foo\n");
    let (code, _, err) = d.run(&["grep", "foo", "no/such/dir"]);
    assert_eq!(code, 2, "{err}");
    let (code, out, _) = d.run(&["grep", "foo", "a.txt", "no/such/dir"]);
    assert_eq!((code, out.as_str()), (2, "a.txt:1:foo\n"));
    let (code, _, _) = d.run(&["grep", "--json", "foo", "no/such/dir"]);
    assert_eq!(code, 2);
    let (code, _, _) = d.run(&["grep", "bar", "a.txt"]);
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_in_the_walk_does_not_change_the_exit_code() {
    use std::os::unix::fs::PermissionsExt;
    let d = Dir::new("unreadable");
    d.write("dir/a.txt", b"foo\n");
    d.write("dir/b.txt", b"foo\n");
    fs::set_permissions(d.0.join("dir/b.txt"), fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(d.0.join("dir/b.txt")).is_ok() {
        return; // running as root
    }
    let (code, _, _) = d.run(&["grep", "foo", "dir"]);
    fs::set_permissions(d.0.join("dir/b.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(code, 0);
}

#[test]
fn find_takes_a_question_as_words() {
    let d = Dir::new("find");
    d.write("auth.py", b"def renew_token(session):\n    \"\"\"Renew the session token before it expires.\"\"\"\n    return session\n");
    let (code, plain, err) = d.run(&["find", "where is the token renewed"]);
    assert_eq!(code, 0, "{err}");
    let (code, asked, err) = d.run(&["find", "where is the token renewed?"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(asked, plain);
    // a regex-looking query that greps nothing falls back to the ranked search once there is an index
    let (code, out, err) = d.run(&["find", "renew token (session"]);
    assert!(code == 0 && out.contains("auth.py"), "exit {code}: {out} {err}");
}

#[test]
fn tsquery_shows_control_characters_in_paths_as_escapes() {
    let d = Dir::new("tsq");
    if fs::write(d.0.join("a\nb\x1b.py"), b"def f():\n    pass\n").is_err() {
        return; // the filesystem refuses such names
    }
    let (code, out, err) = d.run(&["tsquery", "python", "(function_definition name: (identifier) @n)", "."]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains(r"a\nb\x1b.py:1:"), "{out}");
}

#[test]
fn a_control_character_in_a_file_name_is_printed_raw_for_programs_and_escaped_by_symbols() {
    let d = Dir::new("ctlname");
    if fs::write(d.0.join("a\nb\x1b.py"), b"def holder():\n    needle = 1\n").is_err() {
        return; // the filesystem refuses such names
    }
    // piped output names the file as it is, as grep and ripgrep do, so a program can open it
    assert_eq!(d.run(&["grep", "-l", "needle"]).1, "a\nb\x1b.py\n");
    assert_eq!(d.run(&["grep", "needle"]).1, "a\nb\x1b.py:2:    needle = 1\n");
    assert!(d.run(&["grep", "--json", "-l", "needle"]).1.contains(r#""path":"a\nb\u001b.py""#));
    assert_eq!(d.run(&["index"]).0, 0);
    let (code, out, err) = d.run(&["grep", "--symbols", "needle"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "a\\nb\\x1b.py:1-2  [function] holder\n    2:     needle = 1\n");
}

#[test]
fn symbols_show_a_binary_file_note_after_the_lines_of_its_file() {
    let d = Dir::new("symbols_nul");
    let filler: String = (0..3000).map(|i| format!("x = {i}  # filler line to push the NUL past the first read\n")).collect();
    d.write("a.py", format!("def needle():\n    return 1\n\n{filler}\0\nneedle()\n").as_bytes());
    assert_eq!(d.run(&["index"]).0, 0);
    let (code, out, err) = d.run(&["grep", "--symbols", "needle"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("a.py:1-2  [function] needle\n    1: def needle():\n    note: WARNING: stopped searching binary file after match"), "{out}");
    let (_, out, _) = d.run(&["grep", "--symbols", "--json", "needle"]);
    assert!(out.contains(r#""note":"WARNING: stopped searching binary file"#), "{out}");
}

#[test]
fn symbols_are_found_for_matches_under_a_path_argument() {
    let d = Dir::new("symbols_sub");
    d.write("sub/c.py", b"def holder():\n    needle = 1\n    return needle\n");
    assert_eq!(d.run(&["index"]).0, 0);
    for path in ["sub", "sub/", "./sub", "."] {
        let (code, out, _) = d.run(&["grep", "--symbols", "needle", path]);
        assert_eq!(code, 0);
        assert!(out.contains("sub/c.py:1-3  [function] holder"), "{path}: {out}");
    }
    let abs = d.0.join("sub");
    let (_, out, _) = d.run(&["grep", "--symbols", "needle", abs.to_str().unwrap()]);
    assert!(out.contains("sub/c.py:1-3  [function] holder"), "{out}");
}

#[test]
fn a_glob_on_the_command_line_is_taken_whole() {
    let d = Dir::new("globcomma");
    for f in ["a.txt", "b.log", "c.rs"] {
        d.write(f, b"foo\n");
    }
    let (code, out, err) = d.run(&["grep", "-l", "-g", "*.{txt,log}", "foo"]);
    assert_eq!((code, sorted(&out)), (0, vec!["a.txt".to_string(), "b.log".to_string()]), "{err}");
    // as ripgrep: one glob that holds a comma, so no file matches
    let (code, out, _) = d.run(&["grep", "-l", "-g", "*.txt,*.log", "foo"]);
    assert_eq!((code, out.as_str()), (1, ""));
    let (_, out, _) = d.run(&["grep", "-l", "-g", "*.txt", "-g", "*.rs", "foo"]);
    assert_eq!(sorted(&out), ["a.txt", "c.rs"]);
    let (_, out, _) = d.run_env(&["grep", "-l", "foo"], &[("SCRUTATE_GREP_GLOBS", "*.txt,*.log")]);
    assert_eq!(sorted(&out), ["a.txt", "b.log"]);
    let (_, out, _) = d.run_env(&["grep", "-l", "foo"], &[("SCRUTATE_GREP_GLOBS", "*.{txt,log},!b.*")]);
    assert_eq!(sorted(&out), ["a.txt"]);
    let (_, out, _) = d.run_env(&["grep", "-l", "-g", "*.rs", "foo"], &[("SCRUTATE_GREP_GLOBS", "*.txt")]);
    assert_eq!(sorted(&out), ["c.rs"]);
    d.write(".scrutate.toml", b"[grep]\nglobs = [\"*.{txt,log}\"]\n");
    let (code, out, err) = d.run(&["grep", "-l", "foo"]);
    assert_eq!((code, sorted(&out)), (0, vec!["a.txt".to_string(), "b.log".to_string()]), "{err}");
}

#[test]
fn context_groups_are_separated_as_ripgrep_does() {
    let d = Dir::new("ctxsep");
    d.write("a.txt", b"foo\na\nb\nc\nd\nfoo\ne\n");
    d.write("b.txt", b"x\nfoo\ny\nfoo\nz\n");
    d.write("t.txt", b"foo\nb\nc\nfoo\ne\nf\ng\nfoo\n");
    let a = "a.txt:1:foo\na.txt-2-a\n--\na.txt:6:foo\na.txt-7-e\n";
    let b = "b.txt:2:foo\nb.txt-3-y\nb.txt:4:foo\nb.txt-5-z\n";
    for sort in [false, true] {
        let with = |args: &[&str]| {
            let mut v = vec!["grep", "-H", "-j", "1"];
            if sort {
                v.push("--sort");
            }
            v.extend(args);
            d.run(&v).1
        };
        assert_eq!(with(&["-A1", "foo", "a.txt"]), a, "sort {sort}");
        assert_eq!(with(&["-A1", "foo", "a.txt", "b.txt"]), format!("{a}--\n{b}"), "sort {sort}");
        assert_eq!(with(&["-A1", "foo", "b.txt"]), b, "sort {sort}");
        assert_eq!(with(&["-C1", "foo", "t.txt"]), "t.txt:1:foo\nt.txt-2-b\nt.txt-3-c\nt.txt:4:foo\nt.txt-5-e\n--\nt.txt-7-g\nt.txt:8:foo\n", "sort {sort}");
        assert_eq!(with(&["-A0", "foo", "a.txt"]), "a.txt:1:foo\na.txt:6:foo\n", "sort {sort}");
        assert_eq!(with(&["-U", "-A1", r"c\nd", "a.txt", "b.txt"]), "a.txt:4:c\na.txt:5:d\na.txt-6-foo\n", "sort {sort}");
    }
    // many threads: one separator between any two groups, wherever each file's lines land
    let (_, out, _) = d.run(&["grep", "-A1", "foo", "a.txt", "b.txt", "t.txt"]);
    assert_eq!(out.lines().filter(|l| *l == "--").count(), 5, "{out}");
    assert!(!out.starts_with("--") && !out.ends_with("--\n"), "{out}");
    for flags in [["--json", "-A1"], ["-c", "-A1"], ["-l", "-A1"]] {
        let (_, out, _) = d.run(&["grep", flags[0], flags[1], "foo", "a.txt", "b.txt"]);
        assert!(!out.lines().any(|l| l == "--"), "{flags:?}: {out}");
    }
    // a file large enough to be searched in ranges gives the same lines
    let big: String = (0..3000).map(|i| if i % 100 == 0 { "foo here\n".to_string() } else { format!("line {i}\n") }).collect();
    d.write("big.txt", big.as_bytes());
    let (_, whole, _) = d.run(&["grep", "-C1", "foo", "big.txt"]);
    let (_, split, _) = d.run_env(&["grep", "-C1", "foo", "big.txt"], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096"), ("SCRUTATE_THREADS", "4")]);
    assert_eq!(whole, split);
    assert_eq!(whole.lines().filter(|l| *l == "--").count(), 29, "{whole}");
}

#[test]
fn context_output_of_large_files_on_many_threads_is_not_interleaved() {
    let d = Dir::new("ctxmix");
    for f in ["a", "b", "c", "d"] {
        let text: String = (0..40_000).map(|i| format!("hit {f} {i}\nctx\nctx\nctx\n")).collect();
        d.write(&format!("{f}.txt"), text.as_bytes());
    }
    let (code, one, err) = d.run(&["grep", "-C1", "-j", "1", "hit"]);
    assert_eq!(code, 0, "{err}");
    let (_, many, _) = d.run(&["grep", "-C1", "-j", "4", "hit"]);
    // a line of one file right after a line of another, with no separator between them
    let mut switches = 0;
    let mut last: Option<&str> = None;
    for l in many.lines() {
        if l == "--" {
            last = None;
            continue;
        }
        let file = &l[..5];
        switches += usize::from(last.is_some_and(|f| f != file));
        last = Some(file);
    }
    assert_eq!(switches, 0);
    assert_eq!(sorted(&many), sorted(&one));
}

#[test]
fn file_names_are_printed_as_ripgrep_prints_them() {
    let d = Dir::new("withname");
    d.write("a.txt", b"foo\nbar\nfoo2\n");
    d.write("d/b.txt", b"foo x\n");
    let big: String = (0..3000).map(|i| if i % 100 == 0 { "foo here\n".to_string() } else { format!("line {i}\n") }).collect();
    d.write("big.txt", big.as_bytes());
    let long = format!("foo{}\n", "y".repeat(70_000));
    d.write("long.txt", long.as_bytes());
    let out = |args: &[&str]| {
        let mut v = vec!["grep"];
        v.extend(args);
        let (code, out, err) = d.run(&v);
        assert!(code <= 1, "{args:?}: {err}");
        out
    };
    // one file argument: no name; a directory, several paths or the default root: names
    for sort in [&[][..], &["--sort"][..], &["-j", "1"][..]] {
        let with = |args: &[&str]| out(&[sort, args].concat());
        assert_eq!(with(&["foo", "a.txt"]), "1:foo\n3:foo2\n", "{sort:?}");
        assert_eq!(with(&["foo", "./a.txt"]), "1:foo\n3:foo2\n", "{sort:?}");
        assert_eq!(with(&["-c", "foo", "a.txt"]), "2\n", "{sort:?}");
        assert_eq!(with(&["-l", "foo", "a.txt"]), "a.txt\n", "{sort:?}");
        assert_eq!(with(&["-C1", "bar", "a.txt"]), "1-foo\n2:bar\n3-foo2\n", "{sort:?}");
        assert_eq!(with(&["-U", r"bar\nfoo", "a.txt"]), "2:bar\n3:foo2\n", "{sort:?}");
        assert_eq!(with(&["foo", "d"]), "d/b.txt:1:foo x\n", "{sort:?}");
        assert_eq!(with(&["-c", "foo", "d"]), "d/b.txt:1\n", "{sort:?}");
        assert_eq!(sorted(&with(&["foo", "a.txt", "d"])), ["a.txt:1:foo", "a.txt:3:foo2", "d/b.txt:1:foo x"], "{sort:?}");
        assert_eq!(sorted(&with(&["-c", "foo", "a.txt", "d/b.txt"])), ["a.txt:2", "d/b.txt:1"], "{sort:?}");
        assert!(with(&["-c", "foo"]).contains("a.txt:2\n"), "{sort:?}");
        // -H and -I: the later one wins
        assert_eq!(with(&["-H", "foo", "a.txt"]), "a.txt:1:foo\na.txt:3:foo2\n", "{sort:?}");
        assert_eq!(with(&["--with-filename", "-c", "foo", "a.txt"]), "a.txt:2\n", "{sort:?}");
        assert_eq!(with(&["-I", "foo", "d"]), "1:foo x\n", "{sort:?}");
        assert_eq!(with(&["--no-filename", "-c", "foo", "d"]), "1\n", "{sort:?}");
        assert_eq!(with(&["-I", "-l", "foo", "d"]), "d/b.txt\n", "{sort:?}");
        assert_eq!(with(&["-H", "-I", "foo", "a.txt"]), "1:foo\n3:foo2\n", "{sort:?}");
        assert_eq!(with(&["-I", "-H", "foo", "a.txt"]), "a.txt:1:foo\na.txt:3:foo2\n", "{sort:?}");
        // a file searched in ranges and a line written from where it lies
        let lines = with(&["foo", "big.txt"]);
        assert_eq!(lines.lines().count(), 30, "{sort:?}");
        assert!(lines.starts_with("1:foo here\n101:foo here\n"), "{sort:?}: {lines}");
        assert_eq!(with(&["foo", "long.txt"]), format!("1:{long}"), "{sort:?}");
    }
    let (_, split, _) = d.run_env(&["grep", "foo", "big.txt"], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096"), ("SCRUTATE_THREADS", "4")]);
    assert_eq!(split, out(&["foo", "big.txt"]));
    let (_, split, _) = d.run_env(&["grep", "-c", "foo", "big.txt"], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096"), ("SCRUTATE_THREADS", "4")]);
    assert_eq!(split, "30\n");
    // JSON always carries the path
    for args in [&["--json", "foo", "a.txt"][..], &["--json", "-I", "foo", "a.txt"], &["--json", "-c", "foo", "a.txt"], &["--json", "-I", "-l", "foo", "d"]] {
        let o = out(args);
        assert!(
            o.lines().filter(|l| !l.contains("\"summary\"")).all(|l| l.contains("\"path\":\"a.txt\"") || l.contains("\"path\":\"d/b.txt\"")),
            "{args:?}: {o}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_file_name_is_printed_as_it_is() {
    let d = Dir::new("backslash");
    d.write("real\\nname.py", b"REAL_MARK = 1\n");
    d.write("dir\\sub/file.py", b"BSD_MARK = 1\n");
    let (code, out, err) = d.run(&["grep", "-l", "REAL_MARK|BSD_MARK"]);
    assert_eq!((code, sorted(&out)), (0, vec!["dir\\sub/file.py".to_string(), "real\\nname.py".to_string()]), "{err}");
    let (_, out, _) = d.run(&["grep", "-c", "MARK"]);
    assert_eq!(sorted(&out), ["dir\\sub/file.py:1", "real\\nname.py:1"]);
    let (_, out, _) = d.run(&["grep", "--json", "REAL_MARK"]);
    assert!(out.contains(r#""path":"real\\nname.py""#), "{out}");
    let (_, out, _) = d.run(&["grep", "BSD_MARK", "dir\\sub"]);
    assert_eq!(out, "dir\\sub/file.py:1:BSD_MARK = 1\n");
    let (_, out, _) = d.run(&["grep", "-H", "REAL_MARK", "real\\nname.py"]);
    assert_eq!(out, "real\\nname.py:1:REAL_MARK = 1\n");
}

/// procfs and sysfs files report a size of 0 and return a page or less per read.
#[cfg(target_os = "linux")]
#[test]
fn files_under_proc_and_sys_are_read_to_the_end() {
    let d = Dir::new("procfs");
    let mut checked = 0;
    for (file, pat) in
        [("/proc/cpuinfo", "processor"), ("/proc/kallsyms", " "), ("/proc/self/mountinfo", " "), ("/sys/kernel/mm/transparent_hugepage/enabled", "a")]
    {
        let Ok(text) = fs::read_to_string(file) else { continue };
        let want = text.lines().filter(|l| l.contains(pat)).count();
        let (dir, name) = file.rsplit_once('/').unwrap();
        // reached from a root above it that is not `/` by its path (a procfs mounted deeper)
        let (depth, rel) = (file.matches('/').count().to_string(), &file[1..]);
        let mut cases: Vec<Vec<&str>> =
            vec![vec!["grep", "-c", pat, file], vec!["grep", "-c", "--stats", pat, file], vec!["grep", "-c", "--max-depth", "1", "-g", name, pat, dir]];
        if !file.contains("/self/") {
            cases.push(vec!["grep", "-c", "--max-depth", &depth, "-g", rel, pat, "/etc/.."]);
        }
        for args in &cases {
            let (_, out, err) = d.run(args);
            let got: usize = out.trim().rsplit(':').next().unwrap_or("").parse().unwrap_or(usize::MAX);
            assert_eq!(got, want, "{args:?}: {out} {err}");
        }
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn the_later_case_flag_wins() {
    let d = Dir::new("caseflags");
    d.write("c.txt", b"Foo\nfoo\nFOO\n");
    let count = |args: &[&str], env: &[(&str, &str)]| {
        let mut v = vec!["grep", "-c"];
        v.extend(args);
        v.extend(["Foo", "c.txt"]);
        d.run_env(&v, env).1
    };
    assert_eq!(count(&["-i", "-S"], &[]), "1\n");
    assert_eq!(count(&["-S", "-i"], &[]), "3\n");
    assert_eq!(count(&["--ignore-case", "--smart-case"], &[]), "1\n");
    assert_eq!(count(&["-i"], &[("SCRUTATE_GREP_SMART_CASE", "1")]), "3\n");
    assert_eq!(count(&["-S", "-i"], &[("SCRUTATE_GREP_SMART_CASE", "1")]), "3\n");
    assert_eq!(count(&[], &[("SCRUTATE_GREP_SMART_CASE", "1")]), "1\n");
}

#[test]
fn a_multi_line_count_counts_matches_that_share_a_line() {
    let d = Dir::new("mlcount");
    d.write("m.txt", b"x a\nb a\nb\nq\n");
    d.write("aa.txt", b"aa\n");
    // ripgrep's counts: the two matches of `a\nb` share line 2; `a` cannot span lines
    for (args, want) in
        [(&["-U", r"a\nb", "m.txt"][..], "2\n"), (&["-U", r"a\nb|q", "m.txt"], "3\n"), (&["-U", "a$", "m.txt"], "2\n"), (&["-U", "a", "aa.txt"], "1\n")]
    {
        let mut v = vec!["grep", "-c"];
        v.extend(args);
        assert_eq!(d.run(&v).1, want, "{args:?}");
    }
}

#[test]
fn a_multi_line_count_does_not_count_the_empty_match_after_a_group_ends() {
    let d = Dir::new("mlempty");
    d.write("b.txt", b"a\n\nb\n");
    // ripgrep 15's counts: `^` and `$` match again where the group's line end is, past the group
    for (pat, want) in [("^$", "1\n"), ("^", "3\n"), ("$", "3\n")] {
        assert_eq!(d.run(&["grep", "-c", "-U", pat, "b.txt"]).1, want, "{pat}");
    }
}

#[test]
fn a_multi_line_count_matches_ripgrep_without_a_final_line_end_and_after_a_match() {
    let d = Dir::new("mlnofinal");
    // ripgrep 15.2.0's counts
    for (text, pat, want) in [
        (&b"a\n\nb"[..], "$", "2\n"),
        (b"a\n\nb", "b?$", "3\n"),
        (b"a\nb", "$", "1\n"),
        (b"a\nb", "b?$", "2\n"),
        (b"b", "$", "1\n"),
        (b"b", "b?$", "1\n"),
        (b"a\n\nb\n", "b?$", "3\n"),
        (b"a\n\nb\n", "$", "3\n"),
        (b"a\nb", "^", "2\n"),
    ] {
        d.write("t.txt", text);
        assert_eq!(d.run(&["grep", "-c", "-U", pat, "t.txt"]).1, want, "{pat} on {:?}", String::from_utf8_lossy(text));
    }
}

/// The spans `--json` gives, as ripgrep 15.2.0 gives them: no empty match right where a match
/// ended, none past the line's end, and an empty match stays empty.
#[test]
fn json_spans_leave_out_the_empty_matches_ripgrep_leaves_out() {
    let d = Dir::new("jsonspans");
    let spans = |text: &str, args: &[&str]| -> Vec<(u64, u64)> {
        d.write("t.txt", format!("{text}\n").as_bytes());
        let (_, out, err) = d.run(&[&["grep", "--json"][..], args, &["t.txt"]].concat());
        let line = out.lines().find(|l| l.contains("\"type\":\"match\"")).unwrap_or_else(|| panic!("{args:?}: {out}{err}"));
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        v["spans"].as_array().unwrap().iter().map(|s| (s["start"].as_u64().unwrap(), s["end"].as_u64().unwrap())).collect()
    };
    assert_eq!(spans("foo  ", &["-e", r"\s*$"]), [(3, 5)]);
    assert_eq!(spans("foo", &["-e", "^"]), [(0, 0)]);
    assert_eq!(spans("hello", &["-F", "-e", ""]), [(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5)]);
    assert_eq!(spans("ab", &["-e", "b*"]), [(0, 0), (1, 2)]);
    assert_eq!(spans("aXbX", &["-e", "X*"]), [(0, 0), (1, 2), (3, 4)]);
    // an empty match keeps its byte offset, inside `é` and `€` too, as ripgrep's does
    assert_eq!(spans("é€x", &["-P", "-e", "x*"]), [(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 6)]);
}

#[test]
fn a_bad_configuration_file_stops_only_the_commands_that_use_it() {
    let d = Dir::new("badcfg");
    d.write("a.txt", b"foo\n");
    d.write(".scrutate.toml", b"[grep]\nhiden = true\n");
    for args in [&["--version"][..], &["--help"], &["grep", "--help"], &["refs", "--help"], &["help", "grep"], &["completions", "bash"]] {
        let (code, out, err) = d.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(!out.is_empty(), "{args:?}");
    }
    let (code, out, err) = d.run(&["config"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("hiden"), "{out}");
    assert!(err.contains(".scrutate.toml") && err.contains("hiden"), "{err}");
    for args in [&["grep", "foo"][..], &["grep", "foo", "a.txt"], &["languages"]] {
        let (code, _, err) = d.run(args);
        assert_eq!(code, 2, "{args:?}");
        assert!(err.contains(".scrutate.toml") && err.contains("hiden"), "{args:?}: {err}");
    }
}

#[test]
fn the_thread_count_has_a_ceiling() {
    let d = Dir::new("threads");
    for i in 0..11 {
        d.write(&format!("sub/f{i}.txt"), b"foo\n");
    }
    let t = std::time::Instant::now();
    let (code, out, err) = d.run(&["grep", "-j", "100000", "-c", "foo", "."]);
    assert_eq!((code, out.lines().count()), (0, 11), "{err}");
    let (code, out, err) = d.run_env(&["grep", "-c", "foo", "."], &[("SCRUTATE_THREADS", "100000")]);
    assert_eq!((code, out.lines().count()), (0, 11), "{err}");
    d.write("big.txt", &b"foo\n".repeat(5000));
    let (code, out, err) = d.run_env(&["grep", "-j", "100000", "-c", "foo", "big.txt"], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096")]);
    assert_eq!((code, out.as_str()), (0, "5000\n"), "{err}");
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    assert!(d.run(&["grep", "--help"]).1.contains("256"));
}

/// A file of 300 KB: a match, then a NUL byte past the first read buffer, then two more matches.
fn late_nul_file() -> Vec<u8> {
    let mut v = b"foo first\n".to_vec();
    v.extend_from_slice(&[b'x'; 80]);
    v.push(b'\n');
    for _ in 0..3000 {
        v.extend_from_slice(&[b'y'; 99]);
        v.push(b'\n');
    }
    v.extend_from_slice(b"nul\0here foo\nfoo last\n");
    v
}

#[test]
fn named_files_are_searched_as_ripgrep_does() {
    let d = Dir::new("named");
    d.write(".ignore", b"ign.txt\n");
    d.write("ign.txt", b"foo\n");
    d.write("a.txt", b"foo\n");
    d.write("nul.txt", b"foo\nbar\0baz\nfoo2\n");
    d.write("big.bin", &late_nul_file());
    let note = |off: u64| format!("binary file matches (found \"\\0\" byte around offset {off})");
    // ripgrep 15's output and exit status for each
    for (args, code, want) in [
        (&["foo", "ign.txt"][..], 0, "1:foo\n".to_string()),
        (&["-c", "foo", "ign.txt", "nul.txt"], 0, "ign.txt:1\nnul.txt:2\n".into()),
        (&["foo", "nul.txt"], 0, format!("{}\n", note(7))),
        (&["-H", "foo", "nul.txt"], 0, format!("nul.txt: {}\n", note(7))),
        (&["--sort", "foo", "nul.txt", "a.txt"], 0, format!("nul.txt: {}\na.txt:1:foo\n", note(7))),
        (&["-c", "foo", "nul.txt"], 0, "2\n".into()),
        (&["-l", "foo", "nul.txt"], 0, "nul.txt\n".into()),
        (&["-v", "foo", "nul.txt"], 0, format!("{}\n", note(7))),
        (&["-v", "-c", "foo", "nul.txt"], 0, "2\n".into()),
        (&["-v", "-l", "foo", "nul.txt"], 0, "nul.txt\n".into()),
        (&["zzz", "nul.txt"], 1, String::new()),
        (&["foo", "big.bin"], 0, format!("1:foo first\n{}\n", note(300094))),
        (&["-c", "foo", "big.bin"], 0, "3\n".into()),
    ] {
        let mut v = vec!["grep"];
        v.extend(args);
        let (c, out, err) = d.run(&v);
        assert_eq!((c, out), (code, want), "{args:?}: {err}");
        // the same through the sorted (non-streaming) output
        if !args.contains(&"-c") && !args.contains(&"-l") && !args.contains(&"--sort") {
            v.insert(1, "--sort");
            assert_eq!(d.run(&v).0, code, "{v:?}");
        }
    }
    // a file named twice is searched, counted and listed twice, as ripgrep does
    assert_eq!(d.run(&["grep", "foo", "a.txt", "a.txt"]).1, "a.txt:1:foo\na.txt:1:foo\n");
    assert_eq!(d.run(&["grep", "-c", "foo", "a.txt", "a.txt"]).1, "a.txt:1\na.txt:1\n");
    assert_eq!(d.run(&["grep", "-l", "foo", "a.txt", "a.txt"]).1, "a.txt\na.txt\n");
}

#[test]
fn repeated_and_overlapping_paths_are_searched_once_per_path_as_ripgrep_does() {
    let d = Dir::new("overlap");
    d.write("sub/deep/a.txt", b"end one\nx\nend two\n");
    d.write("sub/b.txt", b"end three\n");
    d.write("sub/c.rs", b"fn end() {}\n");
    // ripgrep 15 (`rg -n`, `-T rust` for `-t '!rust'`), sorted
    let cases: &[(&[&str], &[&str])] = &[
        (&["-c", "end", "sub/deep", "sub"], &["sub/b.txt:1", "sub/c.rs:1", "sub/deep/a.txt:2", "sub/deep/a.txt:2"]),
        (&["-c", "end", "sub/b.txt", "sub/b.txt"], &["sub/b.txt:1", "sub/b.txt:1"]),
        (&["-c", "end", "sub/deep/a.txt", "sub/deep", "sub"], &["sub/b.txt:1", "sub/c.rs:1", "sub/deep/a.txt:2", "sub/deep/a.txt:2", "sub/deep/a.txt:2"]),
        (&["-l", "end", "sub/deep", "sub"], &["sub/b.txt", "sub/c.rs", "sub/deep/a.txt", "sub/deep/a.txt"]),
        (&["-l", "end", "sub/b.txt", "sub/b.txt"], &["sub/b.txt", "sub/b.txt"]),
        (
            &["end", "sub/deep", "sub"],
            &[
                "sub/b.txt:1:end three",
                "sub/c.rs:1:fn end() {}",
                "sub/deep/a.txt:1:end one",
                "sub/deep/a.txt:1:end one",
                "sub/deep/a.txt:3:end two",
                "sub/deep/a.txt:3:end two",
            ],
        ),
        (
            &["end", "sub/b.txt", "sub"],
            &["sub/b.txt:1:end three", "sub/b.txt:1:end three", "sub/c.rs:1:fn end() {}", "sub/deep/a.txt:1:end one", "sub/deep/a.txt:3:end two"],
        ),
        (
            &["-t", "!rust", "end", "sub/c.rs", "sub"],
            &["sub/b.txt:1:end three", "sub/c.rs:1:fn end() {}", "sub/deep/a.txt:1:end one", "sub/deep/a.txt:3:end two"],
        ),
        (&["-t", "!rust", "-c", "end", "sub/c.rs", "sub"], &["sub/b.txt:1", "sub/c.rs:1", "sub/deep/a.txt:2"]),
        (&["-g", "!*.rs", "-l", "end", "sub/c.rs", "sub"], &["sub/b.txt", "sub/c.rs", "sub/deep/a.txt"]),
        (&["-c", "end", "sub", "./sub"], &["./sub/b.txt:1", "./sub/c.rs:1", "./sub/deep/a.txt:2", "sub/b.txt:1", "sub/c.rs:1", "sub/deep/a.txt:2"]),
    ];
    // both walks (the one-thread walk stopping early and the parallel walk), and the sorted output
    let envs: &[&[(&str, &str)]] = &[&[], &[("SCRUTATE_SMALL_TREE", "1")], &[("SCRUTATE_SMALL_TREE", "0")]];
    for &(args, want) in cases {
        for extra in [&[][..], &["--sort"], &["-j1"], &["--sort", "--max-results", "100"]] {
            for env in envs {
                let v = [&["grep"][..], extra, args].concat();
                let (code, out, err) = d.run_env(&v, env);
                assert_eq!((code, sorted(&out)), (0, want.iter().map(|s| s.to_string()).collect::<Vec<_>>()), "{v:?} {env:?}: {err}");
            }
        }
    }
    let (_, out, _) = d.run(&["grep", "--json", "-c", "end", "sub/b.txt", "sub/b.txt"]);
    assert_eq!(out.lines().filter(|l| l.contains("\"type\":\"count\"") && l.contains("\"count\":1")).count(), 2, "{out}");
    assert_eq!(d.run(&["grep", "-c", "zzz", "sub", "sub"]).0, 1);
}

#[test]
fn rgignore_files_are_read_before_ignore_and_gitignore_files_as_ripgrep_does() {
    let d = Dir::new("rgignore");
    fs::create_dir_all(d.0.join(".git")).unwrap();
    d.write(".rgignore", b"*.log\n!keep.txt\n");
    d.write(".ignore", b"keep.txt\n*.tmp\n");
    d.write("sub/.gitignore", b"!b.log\n");
    d.write("sub/.rgignore", b"!c.log\n");
    for f in ["a.log", "keep.txt", "x.tmp", "plain.txt", "sub/b.log", "sub/c.log"] {
        d.write(f, b"foo\n");
    }
    // ripgrep 15
    assert_eq!(sorted(&d.run(&["grep", "-l", "foo"]).1), ["keep.txt", "plain.txt", "sub/c.log"]);
    assert_eq!(sorted(&d.run(&["grep", "-l", "foo", "sub"]).1), ["sub/c.log"]);
    assert_eq!(sorted(&run(&d.0.join("sub"), &["grep".as_ref(), "-l".as_ref(), "foo".as_ref()], &[]).1), ["c.log"]);
    assert_eq!(sorted(&d.run(&["grep", "-l", "--no-ignore", "foo"]).1), ["a.log", "keep.txt", "plain.txt", "sub/b.log", "sub/c.log", "x.tmp"]);
}

#[test]
fn a_file_with_a_nul_byte_found_by_the_walk_is_left_out_of_counts_and_lists() {
    let d = Dir::new("walknul");
    d.write("d/keep.log", b"\n\n\nx\na.b\nfoobar e\0binnd\n");
    d.write("d/ok.txt", b"zzz\n");
    d.write("d/big.bin", &late_nul_file());
    let run = |args: &[&str]| sorted(&d.run(&[&["grep"][..], args, &["d"]].concat()).1);
    // ripgrep 15: -v -l and -v -c agree, and a count stops at the NUL byte
    assert_eq!(run(&["-v", "-l", "baz"]), ["d/ok.txt"]);
    assert_eq!(run(&["-v", "-c", "baz"]), ["d/ok.txt:1"]);
    assert!(run(&["-c", "foo"]).is_empty());
    assert_eq!(run(&["-l", "foo"]), ["d/big.bin"]);
}

#[test]
fn a_reader_that_closes_early_ends_the_output_quietly() {
    use std::io::BufRead;
    let d = Dir::new("pipeclose");
    for i in 0..3000 {
        d.write(&format!("a_file_with_a_long_name_so_that_the_list_fills_the_pipe_{i:05}.txt"), b"needle\n");
    }
    d.write("many.txt", "needle on a line\n".repeat(20_000).as_bytes());
    for args in [
        &["grep", "-l", "needle"][..],
        &["grep", "-c", "needle"],
        &["grep", "--sort", "needle"],
        &["grep", "--sort", "--json", "needle"],
        &["grep", "--type-list"],
        &["languages"],
    ] {
        let mut c = Command::new(BIN);
        c.args(args).current_dir(&d.0).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let mut first = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut first).unwrap();
        let out = child.wait_with_output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!first.is_empty() && err.is_empty() && out.status.code() == Some(0), "{args:?}: {:?} {err}", out.status);
    }
}

#[test]
fn a_reader_that_stops_reading_and_then_goes_away_ends_a_json_search() {
    use std::io::Read;
    let d = Dir::new("pipestall");
    let lines = "abc e\n".repeat(300_000);
    d.write("one/a.txt", lines.as_bytes());
    for i in 0..4 {
        d.write(&format!("tree/sub/{i}.txt"), lines.as_bytes());
    }
    for (args, dir) in [(&["grep", "--json", "e"][..], "one"), (&["grep", "--json", "e"], "tree"), (&["grep", "--json", "-j1", "e"], "tree")] {
        let mut child =
            Command::new(BIN).args(args).current_dir(d.0.join(dir)).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut first = [0u8; 4096];
        stdout.read_exact(&mut first).unwrap();
        // the pipe and the queue of hits fill and the search threads wait
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(stdout);
        let t0 = std::time::Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if t0.elapsed() > std::time::Duration::from_secs(30) {
                let _ = child.kill();
                panic!("{args:?} in {dir}: still running 30 s after its reader went away");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(0), "{args:?} in {dir}");
    }
}

#[test]
fn a_walked_file_whose_search_stops_at_a_nul_byte_says_so_after_its_matches() {
    let d = Dir::new("walkstop");
    for f in ["d/a.bin", "d/b.bin", "d/c.bin", "d/e.bin"] {
        d.write(f, &late_nul_file());
    }
    d.write("d/ok.txt", b"foo\n");
    let warn = "WARNING: stopped searching binary file after match (found \"\\0\" byte around offset 300094)";
    // ripgrep 15 (`rg -n --sort path`)
    let mut want = String::new();
    for f in ["a", "b", "c", "e"] {
        want += &format!("d/{f}.bin:1:foo first\nd/{f}.bin: {warn}\n");
    }
    want += "d/ok.txt:1:foo\n";
    for extra in [&["--sort"][..], &["-j", "1"], &["-j", "4"]] {
        let (code, out, err) = d.run(&[&["grep"][..], extra, &["foo", "d"]].concat());
        let mut got: Vec<&str> = out.lines().collect();
        if !extra.contains(&"--sort") {
            // one file's lines stay together, the warning last
            got.sort_by_key(|l| l.split(':').next().unwrap().to_string());
        }
        assert_eq!((code, got.join("\n") + "\n"), (0, want.clone()), "{extra:?}: {err}");
        assert!(err.is_empty(), "{err}");
    }
    assert_eq!(d.run(&["grep", "-I", "foo", "d/a.bin", "d/ok.txt"]).1.lines().filter(|l| *l == warn).count(), 0, "a named file says binary file matches");
    assert_eq!(d.run(&["grep", "-I", "--sort", "foo", "d"]).1.lines().filter(|l| *l == warn).count(), 4);
    let (_, out, _) = d.run(&["grep", "--json", "--sort", "foo", "d"]);
    let rec = format!("{{\"path\":\"d/a.bin\",\"schema\":1,\"text\":{},\"type\":\"binary\"}}", serde_json::to_string(warn).unwrap());
    assert!(out.lines().any(|l| l == rec), "{out}");
    for quiet in [&["-q"][..], &["-c"], &["-l"]] {
        assert!(!d.run(&[&["grep"][..], quiet, &["foo", "d"]].concat()).1.contains("WARNING"));
    }
}

/// `scrutate ARGS` in `cwd` with `input` as its standard input: a pipe, or with `None` a regular
/// file (`file`).
fn run_stdin(cwd: &Path, args: &[&str], input: Option<&[u8]>, file: Option<&Path>) -> (i32, String, String) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(cwd).env_remove("NO_COLOR").stdout(Stdio::piped()).stderr(Stdio::piped());
    match file {
        Some(f) => c.stdin(fs::File::open(f).unwrap()),
        None => c.stdin(Stdio::piped()),
    };
    let mut child = c.spawn().unwrap();
    if let Some(bytes) = input {
        use std::io::Write;
        // a command that never reads standard input may exit before the write
        if let Err(e) = child.stdin.take().unwrap().write_all(bytes) {
            assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "{e}");
        }
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn standard_input_is_searched_when_it_is_a_pipe_or_a_file() {
    let d = Dir::new("stdin");
    d.write("a.txt", b"foo in a file\n");
    let pipe = |args: &[&str], input: &[u8]| run_stdin(&d.0, &[&["grep"][..], args].concat(), Some(input), None);
    // ripgrep 15 with -n
    assert_eq!(pipe(&["foo"], b"x\nfoo\n"), (0, "2:foo\n".into(), String::new()));
    assert_eq!(pipe(&["-H", "foo"], b"foo\n").1, "<stdin>:1:foo\n");
    assert_eq!(pipe(&["-c", "foo"], b"foo\nfoo\n").1, "2\n");
    assert_eq!(pipe(&["-c", "-H", "foo"], b"foo\n").1, "<stdin>:1\n");
    assert_eq!(pipe(&["-l", "foo"], b"foo\n").1, "<stdin>\n");
    assert_eq!(pipe(&["--sort", "foo"], b"foo\n").1, "1:foo\n");
    assert!(pipe(&["--json", "foo"], b"foo\n").1.contains("\"path\":\"<stdin>\""));
    assert_eq!(pipe(&["foo"], b"bar\n").0, 1);
    assert_eq!(pipe(&["foo"], b"foo\0\nfoo\n").1, "binary file matches (found \"\\0\" byte around offset 3)\n");
    assert_eq!(pipe(&["-c", "foo"], b"foo\0\nfoo\n").1, "2\n");
    // `-` names standard input among other paths
    assert_eq!(sorted(&pipe(&["foo", "-", "a.txt"], b"foo\n").1), ["<stdin>:1:foo", "a.txt:1:foo in a file"]);
    assert_eq!(pipe(&["foo", "a.txt"], b"foo\n").1, "1:foo in a file\n");
    // a regular file as standard input
    d.write("in.txt", b"foo from a redirect\n");
    assert_eq!(run_stdin(&d.0, &["grep", "foo"], None, Some(&d.0.join("in.txt"))).1, "1:foo from a redirect\n");
    // no standard input to read (`/dev/null`, the tests' default): the current directory
    assert_eq!(sorted(&d.run(&["grep", "-l", "foo"]).1), ["a.txt", "in.txt"]);
    // --index answers from the index whatever standard input is
    let (code, _, err) = pipe(&["--index", "foo"], b"foo\n");
    assert!(code == 2 && err.contains("text index"), "{code} {err}");
}

/// `scrutate ARGS` in `cwd`, killed and failed after 20 s: a read that blocks must not hang the tests.
#[cfg(unix)]
fn run_limited(cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t0 = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if t0.elapsed().as_secs() >= 20 {
            let _ = child.kill();
            panic!("scrutate {args:?} did not finish in 20 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[cfg(unix)]
#[test]
fn a_named_pipe_or_dev_stdin_given_as_a_path_is_searched_as_ripgrep_does() {
    let d = Dir::new("fifo");
    let fifo = |rel: &str| {
        let p = d.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        assert!(Command::new("mkfifo").arg(&p).status().unwrap().success());
        p
    };
    // a writer that waits for a reader; one never met leaves its thread blocked until the tests end
    let feed = |p: PathBuf, text: &'static [u8]| {
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = fs::OpenOptions::new().write(true).open(&p).and_then(|mut f| f.write_all(text));
        })
    };
    let p = fifo("p");
    for (args, want) in [
        (&["grep", "foo", "p"][..], "1:foo one\n2:foo two\n"),
        (&["grep", "-H", "foo", "p"], "p:1:foo one\np:2:foo two\n"),
        (&["grep", "-c", "foo", "p"], "2\n"),
        (&["grep", "-l", "foo", "p"], "p\n"),
        (&["grep", "-z", "foo", "p"], "1:foo one\n2:foo two\n"),
    ] {
        let w = feed(p.clone(), b"foo one\nfoo two\n");
        let (code, out, err) = run_limited(&d.0, args);
        assert_eq!((code, out.as_str()), (0, want), "{args:?}: {err}");
        w.join().unwrap();
    }
    let w = feed(p.clone(), b"foo one\n");
    let (code, out, err) = run_limited(&d.0, &["grep", "--json", "foo", "p"]);
    assert!(code == 0 && out.contains(r#""path":"p""#) && out.contains("foo one"), "{code} {out} {err}");
    w.join().unwrap();
    // a pipe named as /dev/stdin
    let mut child = Command::new(BIN).args(["grep", "foo", "/dev/stdin"]).current_dir(&d.0).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(b"foo piped\n").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!((out.status.code(), String::from_utf8_lossy(&out.stdout).as_ref()), (Some(0), "1:foo piped\n"));
    // a pipe the walk finds is skipped: reading it would wait for a writer
    fs::remove_file(&p).unwrap();
    fifo("dir/q");
    d.write("dir/a.txt", b"foo a\n");
    let (code, out, err) = run_limited(&d.0, &["grep", "foo", "dir"]);
    assert_eq!((code, out.as_str()), (0, "dir/a.txt:1:foo a\n"), "{err}");
}

#[test]
fn a_sorted_search_with_a_cap_keeps_the_first_matches_in_path_order() {
    let d = Dir::new("orderedcap");
    for i in 0..40 {
        let body: String = (0..30).map(|k| if k % 3 == 0 { format!("foo {i} {k}\n") } else { format!("x {k}\n") }).collect();
        d.write(&format!("d{}/s{}/f{i:02}.txt", i % 4, i % 3), body.as_bytes());
    }
    d.write("d9/big.txt", &(0..3000).map(|k| format!("foo big {k}\n")).collect::<String>().into_bytes());
    for env in [&[][..], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096")], &[("SCRUTATE_SMALL_TREE", "0"), ("SCRUTATE_THREADS", "8")]] {
        let full = d.run_env(&["grep", "--sort", "foo"], env).1;
        for (n, extra) in [(50usize, &[][..]), (7, &["-v"][..]), (380, &[]), (5000, &[])] {
            let mut args = vec!["grep", "--sort", "--max-results"];
            let n_s = n.to_string();
            args.push(&n_s);
            args.extend(extra);
            args.push("foo");
            let want: String = if extra.is_empty() {
                full.lines().take(n).map(|l| format!("{l}\n")).collect()
            } else {
                d.run_env(&["grep", "--sort", "-v", "foo"], env).1.lines().take(n).map(|l| format!("{l}\n")).collect()
            };
            for _ in 0..5 {
                let (code, out, err) = d.run_env(&args, env);
                assert_eq!(code, 0, "{err}");
                assert_eq!(out, want, "{args:?} {env:?}");
            }
        }
        // counts and file lists are cut at the same line
        let firsts: Vec<&str> = full.lines().take(50).collect();
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for l in &firsts {
            *counts.entry(l.split(':').next().unwrap()).or_default() += 1;
        }
        let want: String = counts.iter().map(|(p, c)| format!("{p}:{c}\n")).collect();
        assert_eq!(d.run_env(&["grep", "--sort", "-c", "--max-results", "50", "foo"], env).1, want, "{env:?}");
        let files: String = counts.keys().take(3).map(|p| format!("{p}\n")).collect();
        assert_eq!(d.run_env(&["grep", "--sort", "-l", "--max-results", "3", "foo"], env).1, files, "{env:?}");
    }
}

#[test]
fn flags_that_need_no_pattern_work_alone() {
    let d = Dir::new("nopattern");
    let (code, out, err) = d.run(&["grep", "--type-list"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.lines().any(|l| l.starts_with("rust: ") && l.contains("*.rs")), "{out}");
    for flag in ["--version", "-V"] {
        let (code, out, err) = d.run(&["grep", flag]);
        assert!(code == 0 && out.contains(env!("CARGO_PKG_VERSION")), "{flag}: {out}{err}");
    }
    for flag in ["--help", "-h"] {
        assert_eq!(d.run(&["grep", flag]).0, 0, "{flag}");
    }
    assert_eq!(d.run(&["grep"]).0, 2);
}

#[test]
fn line_number_flags_are_accepted() {
    let d = Dir::new("lineno");
    d.write("a.txt", b"x\nfoo\n");
    d.write("b.txt", b"foo\n");
    assert_eq!(d.run(&["grep", "-n", "foo", "a.txt"]).1, "2:foo\n");
    assert_eq!(d.run(&["grep", "--line-number", "foo", "a.txt"]).1, "2:foo\n");
    assert_eq!(d.run(&["grep", "-N", "foo", "a.txt"]).1, "foo\n");
    assert_eq!(d.run(&["grep", "--no-line-number", "-H", "foo", "a.txt"]).1, "a.txt:foo\n");
    assert_eq!(d.run(&["grep", "-N", "-C1", "foo", "a.txt"]).1, "x\nfoo\n");
    assert_eq!(d.run(&["grep", "-N", "--sort", "foo", "a.txt", "b.txt"]).1, "a.txt:foo\nb.txt:foo\n");
    // the later of the two wins, as in ripgrep
    assert_eq!(d.run(&["grep", "-N", "-n", "foo", "a.txt"]).1, "2:foo\n");
    assert_eq!(d.run(&["grep", "-n", "-N", "foo", "a.txt"]).1, "foo\n");
}

#[test]
fn a_sorted_search_orders_each_root_by_path_with_the_roots_in_command_line_order_as_ripgrep_does() {
    let d = Dir::new("rootorder");
    let body = |f: &str| -> &[(u64, &str)] {
        match f.rsplit('/').next().unwrap() {
            "a.txt" => &[(1, "end one"), (3, "end two")],
            "z.txt" => &[(1, "end z")],
            "deep.txt" => &[(1, "end d")],
            "b.txt" => &[(1, "end b")],
            "o.txt" => &[(1, "end o")],
            _ => &[(1, "end t")],
        }
    };
    for f in ["sub/deep/a.txt", "sub/deep-x/z.txt", "sub/deep.txt", "sub/b.txt", "other/o.txt", "top.txt"] {
        let text: String =
            (1..=body(f).last().unwrap().0).map(|n| body(f).iter().find(|l| l.0 == n).map_or("x\n".to_string(), |l| format!("{}\n", l.1))).collect();
        d.write(f, text.as_bytes());
    }
    // ripgrep 15, `rg --sort path -l end ROOTS`; its -c and -n follow the same file order
    let all = ["sub/b.txt", "sub/deep/a.txt", "sub/deep-x/z.txt", "sub/deep.txt"];
    let dot = ["./other/o.txt", "./sub/b.txt", "./sub/deep/a.txt", "./sub/deep-x/z.txt", "./sub/deep.txt", "./top.txt"];
    let cases: Vec<(Vec<&str>, Vec<&str>)> = vec![
        (vec!["sub", "other"], [&all[..], &["other/o.txt"]].concat()),
        (vec!["other", "sub"], [&["other/o.txt"][..], &all].concat()),
        (vec!["sub/b.txt", "sub/b.txt"], vec!["sub/b.txt", "sub/b.txt"]),
        (vec!["sub", "sub"], [all, all].concat()),
        (vec!["sub/deep", "sub"], [&["sub/deep/a.txt"][..], &all].concat()),
        (vec![".", "sub"], [&dot[..], &all].concat()),
        (vec!["top.txt", "sub", "other/o.txt"], [&["top.txt"][..], &all, &["other/o.txt"]].concat()),
        (vec![], vec!["other/o.txt", "sub/b.txt", "sub/deep/a.txt", "sub/deep-x/z.txt", "sub/deep.txt", "top.txt"]),
    ];
    for (roots, files) in &cases {
        let l: String = files.iter().map(|f| format!("{f}\n")).collect();
        let c: String = files.iter().map(|f| format!("{f}:{}\n", body(f).len())).collect();
        let n: Vec<String> = files.iter().flat_map(|f| body(f).iter().map(move |(k, t)| format!("{f}:{k}:{t}\n"))).collect();
        for extra in [&[][..], &["-j1"], &["-j8"]] {
            let run = |args: &[&str]| d.run(&[&["grep", "--sort"][..], extra, args, &["end"], roots].concat()).1;
            assert_eq!(run(&["-l"]), l, "{roots:?} {extra:?}");
            assert_eq!(run(&["-c"]), c, "{roots:?} {extra:?}");
            assert_eq!(run(&["-n"]), n.concat(), "{roots:?} {extra:?}");
            let json: Vec<String> = run(&["--json"])
                .lines()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .filter(|r| r["type"] == "match")
                .map(|r| format!("{}:{}:{}\n", r["path"].as_str().unwrap(), r["line"], r["text"].as_str().unwrap().trim_end()))
                .collect();
            assert_eq!(json, n, "{roots:?} {extra:?}");
            for cap in [1, 3, 5] {
                assert_eq!(run(&["-n", "--max-results", &cap.to_string()]), n.iter().take(cap).cloned().collect::<String>(), "{roots:?} {extra:?} {cap}");
            }
        }
    }
    // ripgrep 15: `--` before each search of a file, the same file too
    let a = "sub/deep/a.txt:1:end one\nsub/deep/a.txt-2-x\nsub/deep/a.txt:3:end two\n";
    assert_eq!(d.run(&["grep", "--sort", "-n", "-A1", "end", "sub/deep", "sub/deep/a.txt", "sub/b.txt"]).1, format!("{a}--\n{a}--\nsub/b.txt:1:end b\n"));
    // the text index gives the order of the walk
    assert_eq!(d.run(&["index", "--text"]).0, 0);
    let walk = d.run(&["grep", "--sort", "-n", "end"]).1;
    assert_eq!(d.run(&["grep", "--sort", "--index", "-n", "end"]).1, walk);
    assert_eq!(d.run(&["grep", "--sort", "--index", "--max-results", "3", "-n", "end"]).1, walk.lines().take(3).map(|l| format!("{l}\n")).collect::<String>());
}

#[test]
fn a_fuzzy_regex_counts_an_edit_as_one_character_and_folds_case_as_without_it() {
    let d = Dir::new("fuzzyreuni");
    d.write("a.txt", "ÉTÉ\nnaïve\nKelvin \u{212A}\nnaive\n".as_bytes());
    d.write("bad.txt", b"na\xffve\n\xff\xfe\xc3\n");
    let lines = |args: &[&str]| d.run(&[&["grep", "-N", "--no-filename"][..], args, &["a.txt"]].concat()).1;
    assert_eq!(lines(&["--fuzzy-regex=0", "-i", "été"]), "ÉTÉ\n");
    assert_eq!(lines(&["--fuzzy-regex=0", "-S", "été"]), "ÉTÉ\n");
    assert_eq!(lines(&["--fuzzy-regex=0", "-S", "ÉTé"]), "");
    assert_eq!(lines(&["--fuzzy-regex=0", "-i", "x?k"]), "Kelvin \u{212A}\n");
    assert_eq!(lines(&["--fuzzy-regex=0", "na.ve"]), "naïve\nnaive\n");
    assert_eq!(lines(&["--fuzzy-regex=0", "na[ïx]ve"]), "naïve\n");
    // one substitution of a character, not of the bytes of its encoding
    assert_eq!(lines(&["--fuzzy-regex=1", "-c", "(naive|zzzzz)"]), "2\n");
    assert_eq!(lines(&["--fuzzy-regex=1", "-c", "nave"]), "2\n");
    assert_eq!(lines(&["--fuzzy-regex=1", "-c", "ét(é|x)"]), "");
    assert_eq!(lines(&["--fuzzy-regex=1", "-c", "-i", "étx"]), "1\n");
    // a byte of invalid UTF-8 is one unit that no position matches
    let bad = |args: &[&str]| d.run(&[&["grep", "-c"][..], args, &["bad.txt"]].concat());
    assert_eq!(bad(&["--fuzzy-regex=1", "naive"]).1, "1\n");
    assert_eq!(bad(&["--fuzzy-regex=0", "na.ve"]).0, 1);
    assert_eq!(bad(&["--fuzzy-regex=2", "a[^a]."]).0, 0);
    // the text index has no condition for a fuzzy regex: it answers as the walk does
    assert_eq!(d.run(&["index", "--text"]).0, 0);
    for args in [&["--fuzzy-regex=1", "-i", "étx"][..], &["--fuzzy-regex=1", "naive"], &["--fuzzy-regex=0", "-S", "x?k"]] {
        let walk = d.run(&[&["grep", "--sort"][..], args].concat());
        let indexed = d.run(&[&["grep", "--sort", "--index"][..], args].concat());
        assert_eq!(walk.1, indexed.1, "{args:?}");
        assert!(!walk.1.is_empty(), "{args:?}");
    }
}

#[test]
fn a_case_insensitive_literal_matches_every_case_form_up_to_the_last_byte() {
    let d = Dir::new("foldfind");
    let filler = "x".repeat(70);
    d.write("a.txt", format!("{filler}\n\u{212A}önig\nkoenig\npm_re\u{17F}ume {filler}\nPM_RESUME").as_bytes());
    let (code, out, _) = d.run(&["grep", "-j1", "-n", "-i", "-e", "KÖNIG", "a.txt"]);
    assert_eq!((code, out.as_str()), (0, "2:\u{212A}önig\n"));
    let (code, out, _) = d.run(&["grep", "-j1", "-n", "-F", "-i", "-e", "pm_resume", "a.txt"]);
    assert_eq!((code, out), (0, format!("4:pm_re\u{17F}ume {filler}\n5:PM_RESUME\n")));
}

/// What each span of the JSON match records picks out of their `text`, by line.
#[test]
fn file_list_statistics_count_files_not_matches() {
    let d = Dir::new("liststats");
    for i in 0..8 {
        d.write(&format!("f{i}.txt"), b"foo\nfoo\n");
    }
    // -l stops each file at its first match, so the number of matches is not known
    let (code, out, err) = d.run(&["grep", "-l", "--stats", "foo"]);
    assert_eq!((code, out.lines().count()), (0, 8));
    assert!(err.starts_with("8 of 8 files match ("), "{err}");
    let (_, out, _) = d.run(&["grep", "--json", "-l", "foo"]);
    let summary: serde_json::Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
    assert_eq!((summary["type"].as_str(), summary["files_matched"].as_u64()), (Some("summary"), Some(8)), "{out}");
    assert!(summary["matches"].is_null(), "{out}");
    // the other modes count every match
    let (_, _, err) = d.run(&["grep", "-c", "--stats", "foo"]);
    assert!(err.starts_with("16 matches in 8 of 8 files ("), "{err}");
}

fn json_spans(out: &str) -> Vec<(u64, Vec<String>)> {
    out.lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|v| v["type"] == "match")
        .map(|v| {
            let text = v["text"].as_str().unwrap();
            let spans = v["spans"].as_array().unwrap();
            let picked = spans.iter().map(|s| text[s["start"].as_u64().unwrap() as usize..s["end"].as_u64().unwrap() as usize].to_string()).collect();
            assert_eq!(v["column"].as_u64(), spans.first().map(|s| s["start"].as_u64().unwrap() + 1), "{v}");
            (v["line"].as_u64().unwrap(), picked)
        })
        .collect()
}

#[test]
fn json_spans_lie_inside_the_text_they_describe() {
    let d = Dir::new("spans");
    d.write("inv.txt", b"x\x80\xffabc def abc\nq\xffz abc\xff\nab\xffcd\r\nend\n");
    d.write("crlf.txt", b"one abc\r\ntwo\r\n");
    let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (code, out, _) = d.run(&["grep", "--json", "-e", "abc", "-e", r"b(?-u:\xFF)c", "inv.txt"]);
    assert_eq!(code, 0);
    assert_eq!(json_spans(&out), [(1, s(&["abc", "abc"])), (2, s(&["abc"])), (3, s(&["b\u{FFFD}c"]))]);
    for (args, want) in [
        (&["grep", "--json", "abc\r", "crlf.txt"][..], "abc"),
        (&["grep", "--json", "-U", r"abc\r\n", "crlf.txt"], "abc"),
        (&["grep", "--json", "-U", r"abc\r\ntwo", "crlf.txt"], "abc\r\ntwo"),
    ] {
        let (code, out, _) = d.run(args);
        assert_eq!(code, 0, "{args:?}");
        assert_eq!(json_spans(&out), [(1, s(&[want]))], "{args:?}");
    }
}

/// `-t` matches after the ignore rules, as ripgrep's `--type` does: a gitignored file of the
/// type stays out, a hidden one comes in, and a file named on the command line is searched
/// whatever its type. Checked against `rg` where it is installed, and against its answers here.
#[test]
fn a_type_matches_after_the_ignore_rules_as_ripgrep_does() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("grepcli_types_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    // a repository of its own, so its ignore file applies for ripgrep too
    fs::create_dir_all(root.join(".git")).unwrap();
    let d = Dir(fs::canonicalize(&root).unwrap());
    d.write(".gitignore", b"gone.py\n");
    for f in ["gone.py", "kept.py", ".hidden.py", "notes.txt", "sub/also.py"] {
        d.write(f, b"needle\n");
    }
    let cases: &[(&[&str], &[&str])] = &[
        (&["-t", "py", "needle"], &[".hidden.py:1:needle", "kept.py:1:needle", "sub/also.py:1:needle"]),
        (&["-t", "py", "needle", "."], &["./.hidden.py:1:needle", "./kept.py:1:needle", "./sub/also.py:1:needle"]),
        (&["-t", "!py", "needle"], &["notes.txt:1:needle"]),
        (&["-t", "py", "-g", "*.txt", "needle"], &["notes.txt:1:needle"]),
        (&["-t", "py", "needle", "gone.py", "notes.txt", "sub"], &["gone.py:1:needle", "notes.txt:1:needle", "sub/also.py:1:needle"]),
        (&["--no-ignore", "-t", "py", "needle"], &[".hidden.py:1:needle", "gone.py:1:needle", "kept.py:1:needle", "sub/also.py:1:needle"]),
    ];
    let want = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let have_rg = Command::new("rg").arg("--version").stdout(Stdio::null()).status().is_ok_and(|s| s.success());
    for &(args, w) in cases {
        for env in [&[][..], &[("SCRUTATE_SMALL_TREE", "0")]] {
            let v = [&["grep", "-n"][..], args].concat();
            let (code, out, err) = d.run_env(&v, env);
            assert_eq!((code, sorted(&out)), (0, want(w)), "{v:?} {env:?}: {err}");
        }
        if have_rg {
            // ripgrep spells a negated type `-T NAME`
            let mut rg: Vec<&str> = vec!["--no-config", "-n"];
            for a in args {
                match *a {
                    "!py" => {
                        rg.pop();
                        rg.extend(["-T", "py"]);
                    }
                    a => rg.push(a),
                }
            }
            let out = Command::new("rg").args(&rg).current_dir(&d.0).stdin(Stdio::null()).output().unwrap();
            // rg writes Windows' `\`, which scrutate prints as `/`
            let text = String::from_utf8_lossy(&out.stdout).replace('\\', "/");
            assert_eq!(sorted(&text), want(w), "rg {rg:?}");
        }
    }
    let (code, out, _) = d.run_env(&["grep", "-n", "needle"], &[("SCRUTATE_GREP_TYPES", "py")]);
    assert_eq!((code, sorted(&out)), (0, want(&[".hidden.py:1:needle", "kept.py:1:needle", "sub/also.py:1:needle"])));
}

/// `-t` of a language takes the extensions it shares with another language and is told apart in
/// by content (`.h` for C++ and Objective-C, `.m` for Objective-C), as ripgrep 15.2.0's types do.
#[test]
fn a_type_takes_the_extensions_its_language_shares_by_content() {
    let d = Dir::new("shared_types");
    for f in ["a.c", "a.cpp", "a.h", "a.hpp", "a.m", "a.mm"] {
        d.write(f, b"needle\n");
    }
    for (t, want) in [("cpp", "a.cpp\na.h\na.hpp\n"), ("c", "a.c\na.h\n"), ("objc", "a.h\na.m\na.mm\n")] {
        let (code, out, err) = d.run(&["grep", "--sort", "-l", "-t", t, "needle"]);
        assert_eq!((code, out.as_str()), (0, want), "-t {t}: {err}");
    }
}

/// `grep --index` skips the file standard output writes to, as the walk does, also when that
/// file is in the text index (the output of an earlier search, indexed with the rest).
#[cfg(unix)]
#[test]
fn an_indexed_search_skips_the_file_standard_output_writes() {
    let d = Dir::new("ownoutput_index");
    fs::create_dir_all(d.0.join(".git")).unwrap();
    let lines: String = (0..200).map(|i| format!("foo {i}\n")).collect();
    for i in 0..50 {
        d.write(&format!("f{i:02}.txt"), lines.as_bytes());
    }
    d.write("out.txt", lines.as_bytes());
    let (code, _, err) = d.run(&["index", "--text"]);
    assert_eq!(code, 0, "{err}");
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("trap '' XFSZ; ulimit -f 20000; exec '{BIN}' grep --index foo > out.txt"))
        .current_dir(&d.0)
        .env_remove("NO_COLOR")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = fs::read_to_string(d.0.join("out.txt")).unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!text.lines().any(|l| l.starts_with("out.txt:")), "its own output was searched");
    assert_eq!(text.lines().count(), 50 * 200);
}

/// A grep option given twice takes its last value, as in ripgrep 15.2.0, where it was an error.
#[test]
fn an_option_given_twice_takes_its_last_value() {
    let d = Dir::new("twice");
    d.write("a.txt", b"x\nfoo 1\nFOO 2\nfoo 3\n");
    for (args, want) in [
        (&["-N", "-m1", "-m2", "foo", "a.txt"][..], "foo 1\nfoo 3\n"),
        (&["-i", "-i", "-c", "foo", "a.txt"], "3\n"),
        (&["-n", "-n", "-F", "-F", "-w", "-w", "FOO", "a.txt"], "3:FOO 2\n"),
        (&["-A1", "-A0", "-c", "-c", "FOO", "a.txt"], "1\n"),
    ] {
        let mut all = vec!["grep"];
        all.extend_from_slice(args);
        let (code, out, err) = d.run(&all);
        assert_eq!((code, out.as_str()), (0, want), "{args:?}: {err}");
    }
}

/// Ctrl-C ends `grep --index --update`'s wait for the text index's lock that another process
/// holds.
#[cfg(unix)]
#[test]
fn ctrl_c_ends_an_indexed_search_s_wait_for_the_text_index_lock() {
    let d = Dir::new("text_lock_grep");
    fs::create_dir_all(d.0.join(".git")).unwrap();
    d.write("a.txt", b"foo\n");
    assert_eq!(d.run(&["index", "--text"]).0, 0);
    d.write("a.txt", b"foo two\n");
    let lock = fs::File::options().write(true).open(d.0.join(".scrutate/text/lock")).unwrap();
    lock.lock().unwrap();
    let mut child = Command::new(BIN)
        .args(["grep", "--index", "--update", "foo"])
        .current_dir(&d.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(child.try_wait().unwrap().is_none(), "the search ended before the signal");
    assert!(Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap().success());
    let t0 = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        assert!(t0.elapsed().as_secs() < 10, "the search did not stop on Ctrl-C");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(child.wait().unwrap().code(), Some(130));
    drop(lock);
}

/// `-c -U` counts each match as ripgrep 15.2.0 does, with `$`, `\B` and `-w` read against the
/// text around the matched lines, not against their end.
#[test]
fn a_multiline_count_reads_anchors_against_the_whole_text() {
    let d = Dir::new("multiline_count");
    d.write("one.txt", b"b\nc");
    d.write("two.txt", b"a b\nc\n");
    for (args, want) in [
        (&["-U", "-c", "-e", r"b|\n$", "one.txt"][..], "1\n"),
        (&["-U", "-c", "-e", r"a|b\n\B", "two.txt"], "1\n"),
        (&["-U", "-w", "-c", "-e", r"a|b\n", "two.txt"], "1\n"),
        (&["-U", "-c", "-e", r"b\n|\n$", "one.txt"], "1\n"),
        (&["-U", "-c", "-e", r"b$|c", "one.txt"], "2\n"),
    ] {
        let mut all = vec!["grep"];
        all.extend_from_slice(args);
        let (code, out, err) = d.run(&all);
        assert_eq!((code, out.as_str()), (0, want), "{args:?}: {err}");
    }
}
