//! `hook bash` rewrites a shell search, and the rewritten command prints what the original does:
//! the same lines (in each file, the same order) and the same exit code. The commands run in `sh`.
#![cfg(unix)]

#[path = "support/rg.rs"]
mod rg;
use rg::have_rg;

#[path = "support/tag.rs"]
mod tag;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_shell_hook_{}_{}", name, std::process::id()));
        tag::claim(&root);
        let _ = std::fs::remove_dir_all(&root);
        // a repository: no index in a directory above takes it over
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        Repo { root }
    }
    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// (stdout, stderr, code) of `cmd` run by sh in `dir`, with no scrutate setting from outside.
fn sh(dir: &Path, cmd: &str) -> (String, String, i32) {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd).current_dir(dir).stdin(Stdio::null());
    for (k, _) in std::env::vars_os() {
        if k.as_encoded_bytes().starts_with(b"SCRUTATE_") || k == "RIPGREP_CONFIG_PATH" {
            c.env_remove(k);
        }
    }
    c.env("SCRUTATE_REDACT", "off");
    let o = c.output().unwrap();
    (String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned(), o.status.code().unwrap_or(-1))
}

/// The command `hook bash` makes of `cmd`, or None when it leaves `cmd` alone.
fn rewrite(dir: &Path, cmd: &str) -> Option<String> {
    let input = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "session_id": "test", "cwd": dir, "tool_input": {"command": cmd}});
    let mut child = Command::new(BIN).args(["hook", "bash"]).current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    Some(v["hookSpecificOutput"]["updatedInput"]["command"].as_str().unwrap().to_string())
}

/// The lines of a search's output by file (the part before the first `:`), in order: files may
/// come in any order, as ripgrep's threads finish.
fn by_file(out: &str) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in out.lines() {
        let file = l.split([':', '-']).next().unwrap_or("").to_string();
        m.entry(file).or_default().push(l.to_string());
    }
    m
}

#[test]
fn a_rewritten_search_prints_what_ripgrep_prints() {
    if !have_rg() {
        return;
    }
    let r = Repo::new("parity");
    r.write("src/a.rs", "fn foo() {}\nlet a.b = 1;\nFOO\nfoo_bar\n");
    r.write("src/b.py", "def foo():\n    return 'foo'\n\nfoo = 2\n");
    r.write("docs/c.md", "the foo is here\nnothing\nfoo again\n");
    r.write(".hidden/d.txt", "foo hidden\n");
    r.write(".gitignore", "ignored/\n");
    r.write("ignored/e.txt", "foo ignored\n");
    let cases = [
        "rg foo",
        "rg -n foo",
        "rg -i FOO src",
        "rg -l foo",
        "rg -c foo",
        "rg -w foo",
        "rg -F 'a.b'",
        "rg -n 'fo+' src docs",
        "rg -g '*.rs' foo",
        "rg -t py foo",
        "rg --hidden foo",
        "rg -A1 foo docs",
        "rg -o 'fo+_?\\w*' src",
        "rg nothere",
        "rg foo missing_dir",
        "rg '('",
        "grep -rn foo .",
        "grep -rl foo src docs",
        "grep -rni 'FOO' --include='*.py' .",
        "grep -rE 'fo+|bar' src",
        "grep -r nothere .",
        "grep -r foo missing_dir",
        "find . -name '*.rs'",
        "find src -type f",
        "find . -name 'nothing*'",
    ];
    for cmd in cases {
        let new = rewrite(&r.root, cmd).unwrap_or_else(|| panic!("{cmd:?} was not rewritten"));
        let (want, want_err, want_code) = sh(&r.root, cmd);
        let (got, got_err, got_code) = sh(&r.root, &new);
        assert_eq!(got_code, want_code, "{cmd:?} -> {new}\nrg: {want}{want_err}\nhook: {got}{got_err}");
        assert_eq!(by_file(&got), by_file(&want), "{cmd:?} -> {new}\nrg stderr: {want_err}\nhook stderr: {got_err}");
        assert_eq!(got_err.is_empty(), want_err.is_empty(), "{cmd:?}: rg stderr {want_err:?}, hook stderr {got_err:?}");
    }
    // a flag the hook does not know is left to rg, which reports it in its own words
    assert_eq!(rewrite(&r.root, "rg --no-such-flag foo"), None);
    // the rest of the line runs as written, after each operator, with a glob the shell expands
    // and stderr redirected
    for cmd in [
        "rg foo | sort",
        "grep -rn foo src | cut -d: -f1 | sort -u",
        "rg -n foo |& sort",
        "rg nothere || echo none",
        "rg foo src && echo found",
        "rg foo docs; echo after",
        "grep foo src/*.rs",
        "grep -l foo src/* docs/*",
        "find . -name '*.rs' | sort",
        "rg foo missing_dir 2>/dev/null; echo code $?",
        "rg foo missing_dir 2>&1 | sort",
        "rg foo src >/dev/null && echo quiet",
        "rg foo src &>/dev/null; echo code $?",
        "rg -c foo | sort | head -n 2",
    ] {
        let new = rewrite(&r.root, cmd).unwrap_or_else(|| panic!("{cmd:?} was not rewritten"));
        assert!(new.ends_with(cmd), "{cmd:?} -> {new}: the line is kept byte for byte");
        let (want, want_err, want_code) = sh(&r.root, cmd);
        let (got, got_err, got_code) = sh(&r.root, &new);
        assert_eq!((got_code, by_file(&got)), (want_code, by_file(&want)), "{cmd:?} -> {new}\nhook err: {got_err}\nrg err: {want_err}");
    }
    // `| head`: as many lines, each one ripgrep prints
    for (cmd, n) in [("rg foo | head -n 2", 2), ("rg -n foo | head -3", 3), ("rg nothere | head", 0)] {
        let new = rewrite(&r.root, cmd).unwrap();
        let (all, _, _) = sh(&r.root, cmd.split(" | ").next().unwrap());
        let (got, _, code) = sh(&r.root, &new);
        assert_eq!(code, 0, "{cmd:?}");
        assert_eq!(got.lines().count(), n.min(all.lines().count()), "{cmd:?}: {got}");
        assert!(got.lines().all(|l| all.lines().any(|a| a == l)), "{cmd:?}: {got}\nnot all in\n{all}");
    }
}

#[test]
fn hook_run_takes_no_program_from_the_repository_and_no_preloaded_library() {
    let r = Repo::new("planted");
    r.write("a.txt", "foo\n");
    std::fs::create_dir_all(r.root.join(".git")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let plant = |dir: &Path, body: &str| {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join("grep");
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    // a repository's bin on PATH, as a .envrc puts it there, by absolute path
    plant(&r.root.join("bin"), "#!/bin/sh\ntouch planted.txt\n");
    let path = format!("{}:{}", r.root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new(BIN).args(["hook", "run", "--", "grep", "-r", "foo", "."]).env("PATH", &path).current_dir(&r.root).output().unwrap();
    assert!(!r.root.join("planted.txt").exists(), "the repository's grep ran");
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    // a program outside the repository runs, with no preloaded library in its environment
    let outside = Repo::new("planted_outside");
    plant(&outside.root, "#!/bin/sh\nenv > \"$(dirname \"$0\")/env.txt\"\n");
    let path = format!("{}:{}", outside.root.display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new(BIN)
        .args(["hook", "run", "--", "grep", "-r", "foo", "."])
        .env("PATH", &path)
        .env("LD_PRELOAD", "/nowhere/x.so")
        .env("LD_LIBRARY_PATH", "/nowhere")
        .env("LD_AUDIT", "/nowhere/a.so")
        .env("DYLD_LIBRARY_PATH", "/nowhere")
        .current_dir(&r.root)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let env = std::fs::read_to_string(outside.root.join("env.txt")).unwrap();
    for k in ["LD_PRELOAD=", "LD_LIBRARY_PATH=", "LD_AUDIT=", "DYLD_LIBRARY_PATH="] {
        assert!(!env.lines().any(|l| l.starts_with(k)), "{k} reached the program");
    }
}

#[test]
fn the_program_dies_when_hook_run_is_killed() {
    let r = Repo::new("orphan");
    let bin = Repo::new("orphan_bin");
    use std::os::unix::fs::PermissionsExt;
    // a grep that writes nothing and runs on, as `grep foo /dev/zero` does
    let fake = bin.root.join("grep");
    std::fs::write(&fake, format!("#!/bin/sh\necho $$ > {}/pid\nexec sleep 30\n", bin.root.display())).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.root.display(), std::env::var("PATH").unwrap_or_default());
    let mut run = Command::new(BIN).args(["hook", "run", "--", "grep", "foo", "/dev/zero"]).env("PATH", path).current_dir(&r.root).spawn().unwrap();
    let pid_file = bin.root.join("pid");
    let start = std::time::Instant::now();
    while !pid_file.exists() && start.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_string();
    let alive = || Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    assert!(alive(), "the program started");
    // SIGKILL: no handler of hook run's own runs
    run.kill().unwrap();
    run.wait().unwrap();
    let start = std::time::Instant::now();
    while alive() && start.elapsed() < std::time::Duration::from_secs(1) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!alive(), "the program outlived hook run");
}

#[test]
fn a_program_ends_its_answer_when_hook_run_holds_descriptors_past_1024() {
    let r = Repo::new("many_fds");
    r.write("a.txt", "foo\n");
    // hook run starts with descriptors 3 to 1100 open, so its own pipes take numbers past them
    let script =
        format!("ulimit -n 2048 2>/dev/null || exit 0; for i in $(seq 3 1100); do eval \"exec $i</dev/null\"; done; exec {BIN} hook run -- grep foo a.txt");
    let mut child = Command::new("bash").arg("-c").arg(&script).current_dir(&r.root).stdout(Stdio::piped()).spawn().unwrap();
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(20) {
            let _ = child.kill();
            panic!("hook run waited on a descriptor the watcher kept");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty() || String::from_utf8_lossy(&out.stdout).contains("foo"));
}

#[test]
fn a_long_answer_stops_at_the_budget_and_a_piped_one_streams_whole() {
    if !have_rg() {
        return;
    }
    let r = Repo::new("long");
    let lines: String = (0..200_000).map(|i| format!("foo line {i}\n")).collect();
    r.write("big.txt", &lines);
    let run = |piped: bool| {
        let mut args = vec!["hook", "run"];
        if piped {
            args.push("--piped");
        }
        args.extend(["--", "grep", "foo", "big.txt"]);
        Command::new(BIN).args(&args).current_dir(&r.root).env("SCRUTATE_REDACT", "off").output().unwrap()
    };
    let cut = run(false);
    assert_eq!(cut.status.code(), Some(0));
    let text = String::from_utf8_lossy(&cut.stdout);
    assert!(cut.stdout.len() <= 24_100 && text.ends_with("narrow the search, or pass a path\n"), "{} bytes", cut.stdout.len());
    let whole = run(true);
    assert_eq!(whole.status.code(), Some(0));
    assert_eq!(whole.stdout.len(), lines.len(), "a piped answer is the program's whole output");
}

#[test]
fn a_glob_that_expands_to_a_flag_that_runs_a_program_is_refused_when_it_runs() {
    let r = Repo::new("glob_flag");
    r.write("a.txt", "foo\n");
    // a file whose name is a flag of rg's that runs a program
    r.write("--pre=./prog", "");
    r.write("prog", "#!/bin/sh\ntouch ran.txt\n");
    let new = rewrite(&r.root, "rg foo *").expect("the words as written are a plain search");
    let (out, err, code) = sh(&r.root, &new);
    assert_eq!(code, 2, "{out}{err}");
    assert!(out.is_empty() && err.contains("hook run"), "{err}");
    assert!(!r.root.join("ran.txt").exists(), "the preprocessor ran");
}

#[test]
fn a_command_the_hook_does_not_model_runs_as_written() {
    let r = Repo::new("untouched");
    for cmd in [
        "ls",
        "rg foo > out.txt",
        "rg foo >> out.txt",
        "rg foo >| out.txt",
        "rg foo &> out.txt",
        "rg foo 1> out.txt",
        "rg foo 2> err.txt",
        "rg foo 2>> err.txt",
        "rg foo >| out.txt",
        "rg foo 2>~/.bashrc",
        "rg foo < in.txt",
        "rg $X",
        "cd src && rg foo",
        "(rg foo)",
        "X=1 rg foo",
    ] {
        assert_eq!(rewrite(&r.root, cmd), None, "{cmd:?}");
    }
    // a command that could write or run a program is never rewritten, and hook run refuses it
    for cmd in ["find . -name x -delete", "find . -exec rm '{}' +", "rg --pre ./prog x", "rg -z x"] {
        assert_eq!(rewrite(&r.root, cmd), None, "{cmd:?}");
    }
    assert!(r.root.exists(), "nothing was deleted");
}

/// A `--` that is a flag's value (`-g --`, `-e --`) ends nothing for rg, so a flag after it that
/// runs a program is live: hook run refuses it, and the hook rewrites or allows none of them.
#[test]
fn a_dashdash_given_as_a_flag_s_value_lets_no_program_run() {
    let r = Repo::new("dashdash_value");
    r.write("f.txt", "hello\n");
    let prog = r.root.join("p");
    std::fs::write(&prog, "#!/bin/sh\ntouch ran.txt\ncat \"$1\"\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
    let lines = [
        "rg -g -- --pre=./p hello f.txt",
        "rg -e -- --pre=./p hello f.txt",
        "rg --regexp -- --pre=./p hello f.txt",
        "rg -e -- -z hello f.txt",
        "rg --glob -- --pre-glob=x --pre=./p hello f.txt",
        "rg -iz hello f.txt",
    ];
    for line in lines {
        let argv: Vec<&str> = line.split(' ').collect();
        let out = Command::new(BIN).args(["hook", "run", "--"]).args(&argv).current_dir(&r.root).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{line}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(!r.root.join("ran.txt").exists(), "{line}: the program ran");
        assert_eq!(rewrite(&r.root, line), None, "{line}: rewritten for Claude");
        let codex = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "turn_id": "t", "cwd": &r.root, "tool_input": {"command": line}});
        let mut child = Command::new(BIN).args(["hook", "bash"]).current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        use std::io::Write;
        child.stdin.take().unwrap().write_all(codex.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(!String::from_utf8_lossy(&out.stdout).contains("allow"), "{line}: allowed for Codex");
    }
}

#[test]
fn hook_run_by_hand_runs_only_a_read_only_rg_grep_or_find() {
    let r = Repo::new("by_hand");
    r.write("keep.txt", "foo\n");
    std::fs::create_dir_all(r.root.join("bin")).unwrap();
    let refused: &[&[&str]] = &[
        &["/tmp/x/rg", "foo"],
        &["./rg", "foo"],
        &["bin/rg", "foo"],
        &["rm", "-rf", "keep.txt"],
        &["sh", "-c", "rm keep.txt"],
        &["env", "rm", "keep.txt"],
        &["X=1", "rg", "foo"],
        &["find", ".", "-delete"],
        &["find", ".", "-exec", "rm", "{}", "+"],
        &["find", ".", "-fprint", "out.txt"],
        &["rg", "--pre", "./prog", "foo"],
        &["rg", "--pre=./prog", "foo"],
        &["rg", "-z", "foo"],
        &["rg", "--hostname-bin", "./h", "foo"],
        &["rg", "-g", "--", "--pre=./prog", "foo", "keep.txt"],
        &["rg", "-e", "--", "--pre=./prog", "foo", "keep.txt"],
        &["rg", "--regexp", "--", "--pre=./prog", "foo", "keep.txt"],
        &["rg", "-e", "--", "-z", "foo"],
        &["rg", "--glob", "--", "--pre-glob=x", "foo"],
        &["rg", "-iz", "foo"],
        &["rg", "--exec-later", "x", "foo"],
        &[],
    ];
    for argv in refused {
        let out = Command::new(BIN).args(["hook", "run", "--"]).args(*argv).current_dir(&r.root).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{argv:?}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(out.stdout.is_empty(), "{argv:?}");
    }
    assert!(r.root.join("keep.txt").is_file(), "nothing was run");
    assert!(!r.root.join("out.txt").exists(), "nothing was written");
    // the program comes from an absolute directory of PATH, never the current directory
    {
        let fake = r.root.join("grep");
        std::fs::write(&fake, "#!/bin/sh\ntouch ran.txt\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(".:bin:{}", std::env::var("PATH").unwrap_or_default());
        let out = Command::new(BIN).args(["hook", "run", "--", "grep", "-r", "foo", "."]).env("PATH", path).current_dir(&r.root).output().unwrap();
        assert!(!r.root.join("ran.txt").exists(), "a grep of the current directory ran: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    }
    // turned off
    let input = serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": "rg foo"}});
    let mut child = Command::new(BIN).args(["hook", "bash"]).env("SCRUTATE_HOOK_BASH", "off").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.stdout.is_empty() && out.status.success());
}

#[test]
fn a_sort_value_is_read_as_ripgrep_reads_it() {
    if !have_rg() {
        eprintln!("skipped: no rg on PATH");
        return;
    }
    // `path` is --sort's value: read as the pattern, `config` and `src` would turn into paths
    let r = Repo::new("sort_value");
    r.write("src/a.txt", "load config here\nthe path to it\n");
    r.write("config/x.toml", "path = 1\n");
    r.write("TODO", "TODO: path\n");
    r.write("config/y.toml", "--sort here\n");
    // files in several directories, so an order the walk gives differs from path order
    for (i, d) in ["a", "b/c", "d", "e/f/g", "src2", "z", "m/n", "k"].iter().enumerate() {
        for j in 0..3 {
            r.write(&format!("{d}/f{j}.txt"), &format!("load config {i} {j}\nthe path {i}\n"));
        }
    }
    for cmd in [
        "rg -e --sort path config",
        "rg --sort path config",
        "rg --sort path src",
        "rg --sort path TODO .",
        "rg --sort=path config",
        "rg --sortr path config",
        "rg --sort none config",
        "rg -n --sort modified src",
        "rg --sort path -n path",
    ] {
        let (want, _, want_code) = sh(&r.root, cmd);
        // every one of these is read-only: one the hook leaves alone would test nothing
        let new = rewrite(&r.root, cmd).unwrap_or_else(|| panic!("not rewritten: {cmd}"));
        let (got, _, got_code) = sh(&r.root, &new);
        // in order, but where rg's own order is its parallel walk's
        let order = |s: &str| {
            let mut v: Vec<String> = s.lines().map(str::to_string).collect();
            if cmd.contains("--sort none") {
                v.sort();
            }
            v
        };
        assert_eq!((order(&got), got_code), (order(&want), want_code), "{cmd}");
    }
}

#[test]
fn the_corpus_of_real_search_shapes_is_rewritten_with_no_mismatch() {
    if !have_rg() {
        return;
    }
    let r = Repo::new("corpus");
    r.write("src/a.rs", "fn foo() {}\nlet a.b = 1;\nFOO\nfoo_bar\n// TODO x\nreturn 1\n");
    r.write("src/b.py", "def foo():\n    return 'foo'\n\nfoo = 2\nFoo = 3\n");
    r.write("docs/c.md", "the foo is here\nnothing\nfoo again\nTODO later\n");
    r.write("config/x.toml", "path = 1\nfoo = 2\n");
    let corpus = include_str!("shell_hook_corpus.txt");
    let cmds: Vec<&str> = corpus.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).collect();
    assert_eq!(cmds.len(), 210);
    // a word that names a flag that runs a program: such a line is never rewritten
    let runs = |c: &str| {
        c.split(' ').any(|w| {
            w.starts_with("--pre") || w == "--search-zip" || w.starts_with("--hostname-bin") || w.starts_with('-') && !w.starts_with("--") && w.contains('z')
        })
    };
    let mut rewritten = 0;
    let mut mismatches = Vec::new();
    for cmd in &cmds {
        let Some(new) = rewrite(&r.root, cmd) else { continue };
        assert!(!runs(cmd), "a line with a flag that runs a program was rewritten: {cmd}");
        rewritten += 1;
        let (want, _, want_code) = sh(&r.root, cmd);
        let (got, _, got_code) = sh(&r.root, &new);
        let sorted = |s: &str| {
            let mut v: Vec<&str> = s.lines().collect();
            v.sort();
            v.into_iter().map(str::to_string).collect::<Vec<_>>()
        };
        // a line kept by head, tail or sed -n depends on the order files come in
        let same = if cmd.contains("head") || cmd.contains("tail") || cmd.contains("sed -n") {
            got.lines().count() == want.lines().count()
        } else {
            sorted(&got) == sorted(&want)
        };
        if got_code != want_code || !same {
            mismatches.push(format!("{cmd}\n  sh: {want:?} {want_code}\n  hook: {got:?} {got_code}"));
        }
    }
    assert!(mismatches.is_empty(), "{} mismatches:\n{}", mismatches.len(), mismatches.join("\n"));
    assert!(rewritten * 10 >= cmds.len() * 7, "{rewritten} of {} rewritten", cmds.len());
}
