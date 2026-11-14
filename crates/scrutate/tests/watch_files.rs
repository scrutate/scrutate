//! `scrutate watch` against the real binary: what it sees while it starts, which directories it
//! watches, and what happens when the tree under it moves or goes away.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_watchfiles_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Repo { root: fs::canonicalize(&root).unwrap() }
    }
    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
    }
    fn run(&self, args: &[&str]) -> i32 {
        run_in(&self.root, args)
    }
    fn indexed(&self, word: &str) -> bool {
        indexed_in(&self.root, word)
    }
}

fn run_in(dir: &Path, args: &[&str]) -> i32 {
    Command::new(BIN).args(args).current_dir(dir).stdin(Stdio::null()).output().unwrap().status.code().unwrap_or(-1)
}

fn indexed_in(dir: &Path, word: &str) -> bool {
    run_in(dir, &["grep", "--index", "-F", word]) == 0
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `watch` in `root`; stderr lines arrive on the receiver.
fn start_watch(root: &Path) -> (KillOnDrop, Receiver<String>) {
    // no global git excludes file from the user's own configuration
    start_watch_with(root, Path::new("/nonexistent"))
}

/// `start_watch` with `git_config` as git's global configuration file.
fn start_watch_with(root: &Path, git_config: &Path) -> (KillOnDrop, Receiver<String>) {
    let mut child = Command::new(BIN)
        .args(["watch", "."])
        .current_dir(root)
        .env("SCRUTATE_WATCH_DEBUG", "1")
        .env("GIT_CONFIG_GLOBAL", git_config)
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    (KillOnDrop(child), rx)
}

fn wait_watching(rx: &Receiver<String>) {
    while !rx.recv_timeout(Duration::from_secs(60)).expect("watch starts").contains("watching") {}
}

fn wait_for(secs: u64, what: &str, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed().as_secs() < secs, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn files_written_while_the_first_update_runs_are_indexed() {
    let r = Repo::new("startup");
    r.write("a.py", b"def alpha():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    // the first update indexes these, which takes long enough to write during it
    for i in 0..3000 {
        r.write(&format!("src/d{}/m{i}.py", i % 30), format!("def existing_{i}():\n    return {i}\n").as_bytes());
    }
    let (_child, rx) = start_watch(&r.root);
    // write until the watcher says it is watching; nothing is written after that
    let mut n = 0;
    loop {
        r.write(&format!("late/l{n}.py"), format!("def written_late_{n}():\n    return {n}\n").as_bytes());
        n += 1;
        match rx.recv_timeout(Duration::from_millis(30)) {
            Ok(line) if line.contains("watching") => break,
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => assert!(n < 3000, "watch never started"),
            Err(e) => panic!("watch exited: {e}"),
        }
    }
    let last = n - 1;
    wait_for(20, "the files written during start-up", || r.indexed(&format!("written_late_{last}()")) && r.run(&["check", "--files"]) == 0);
}

#[test]
fn a_directory_that_replaces_one_moved_away_loses_its_ignore_rules() {
    let r = Repo::new("moved");
    r.write("a.py", b"def alpha():\n    return 1\n");
    r.write("sub2/.ignore", b"b.py\n");
    r.write("sub2/c.py", b"def gamma():\n    return 3\n");
    assert_eq!(r.run(&["index"]), 0);
    let elsewhere = r.root.with_extension("elsewhere");
    let _ = fs::remove_dir_all(&elsewhere);
    let (_child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    // the matcher of sub2/.ignore is cached once an event under sub2 is judged
    r.write("sub2/c.py", b"def gamma_two():\n    return 3\n");
    wait_for(20, "the first change", || r.indexed("gamma_two"));
    fs::rename(r.root.join("sub2"), &elsewhere).unwrap();
    fs::create_dir(r.root.join("sub2")).unwrap();
    // past the burst of the move, so only the write's own event can bring b.py in
    std::thread::sleep(Duration::from_millis(1500));
    r.write("sub2/b.py", b"def beta_after_move():\n    return 2\n");
    let found = Instant::now();
    while !r.indexed("beta_after_move") && found.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_dir_all(&elsewhere);
    assert!(r.indexed("beta_after_move"), "sub2/b.py is indexed once sub2/.ignore is gone");
}

#[test]
fn new_nested_directories_are_watched() {
    let r = Repo::new("nested");
    r.write("a.py", b"def alpha():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    let (_child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    r.write("x/y/z/n.py", b"def nested_first():\n    return 1\n");
    wait_for(20, "the new nested file", || r.indexed("nested_first"));
    // a later change inside the new directories still sends an event
    r.write("x/y/z/n.py", b"def nested_second():\n    return 2\n");
    wait_for(20, "a change inside the new directories", || r.indexed("nested_second") && !r.indexed("nested_first"));
}

// Windows does not let a directory that is being watched be removed
#[cfg(unix)]
#[test]
fn watch_stops_with_an_error_when_its_root_is_removed() {
    let r = Repo::new("rootgone");
    r.write("a.py", b"def alpha():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    let (mut child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    fs::remove_dir_all(&r.root).unwrap();
    let t0 = Instant::now();
    let status = loop {
        if let Some(s) = child.0.try_wait().unwrap() {
            break s;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "watch keeps running after its root was removed");
        std::thread::sleep(Duration::from_millis(100));
    };
    let err: Vec<String> = rx.try_iter().collect();
    assert_eq!(status.code(), Some(2), "{err:?}");
    assert!(err.iter().any(|l| l.contains("is gone")), "{err:?}");
}

/// Inotify watches the process holds, from /proc/PID/fdinfo.
#[cfg(target_os = "linux")]
fn inotify_watches(pid: u32) -> usize {
    let mut n = 0;
    for fd in fs::read_dir(format!("/proc/{pid}/fd")).unwrap().flatten() {
        if fs::read_link(fd.path()).map(|l| l.to_string_lossy() == "anon_inode:inotify").unwrap_or(false) {
            let info = fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", fd.file_name().to_string_lossy())).unwrap_or_default();
            n += info.lines().filter(|l| l.starts_with("inotify wd")).count();
        }
    }
    n
}

#[cfg(target_os = "linux")]
#[test]
fn only_directories_the_index_holds_are_watched() {
    let r = Repo::new("count");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write(".gitignore", b"node_modules/\ntarget/\n");
    r.write("src/a.py", b"def alpha():\n    return 1\n");
    for i in 0..200 {
        r.write(&format!("node_modules/pkg{i}/lib/index.js"), b"module.exports = 1;\n");
        fs::create_dir_all(r.root.join(format!("target/debug/build/c{i}/out"))).unwrap();
    }
    assert_eq!(r.run(&["index"]), 0);
    let (child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    // each directory above the root, for its ignore files
    let above = r.root.ancestors().count() - 1;
    assert_eq!(inotify_watches(child.0.id()), above + 2, "the root and src/");
    r.write("src/new/b.py", b"def beta():\n    return 2\n");
    wait_for(20, "the new file", || r.indexed("beta"));
    assert_eq!(inotify_watches(child.0.id()), above + 3, "the root, src/ and src/new/");
    fs::remove_dir_all(r.root.join("src/new")).unwrap();
    wait_for(20, "the removal", || !r.indexed("beta"));
    assert_eq!(inotify_watches(child.0.id()), above + 2, "a removed directory's watch is gone");
}

/// Stderr lines up to and including the first update line that reports `changed` and `removed`
/// files (for each index that has a change).
fn wait_update(rx: &Receiver<String>, changed: usize, removed: usize) -> Vec<String> {
    let want = format!("[scrutate] {changed} changed, {removed} removed");
    let t0 = Instant::now();
    let mut seen = Vec::new();
    loop {
        let left = Duration::from_secs(30).checked_sub(t0.elapsed()).unwrap_or_else(|| panic!("no line {want:?} in {seen:?}"));
        let line = rx.recv_timeout(left).unwrap_or_else(|_| panic!("no line {want:?} in {seen:?}"));
        let done = line.starts_with(&want);
        seen.push(line);
        if done {
            return seen;
        }
    }
}

/// The last burst line among `lines`.
fn last_burst(lines: &[String]) -> &str {
    lines.iter().rev().find(|l| l.contains("burst:")).map(String::as_str).unwrap_or_else(|| panic!("no burst line in {lines:?}"))
}

/// Assert that the burst before the update named `paths` (when given) and walked the tree or not.
fn burst_was(lines: &[String], paths: Option<usize>, full: bool) {
    let line = last_burst(lines);
    assert!(line.ends_with(&format!("full walk: {}", if full { "yes" } else { "no" })), "{lines:?}");
    // Windows' recursive watch also reports the directory a change happened in, and paths
    // inside a nested repository's `.git`: the count is checked where events name files only
    if let (Some(n), false) = (paths, cfg!(windows)) {
        assert!(line.contains(&format!("burst: {n} paths,")), "{lines:?}");
    }
}

/// A repository with an index and a watcher that has said it is watching.
fn watched(name: &str, files: &[(&str, &str)]) -> (Repo, KillOnDrop, Receiver<String>) {
    let r = Repo::new(name);
    for (rel, text) in files {
        r.write(rel, text.as_bytes());
    }
    assert_eq!(r.run(&["index"]), 0);
    let (child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    (r, child, rx)
}

#[test]
fn a_save_deep_in_a_wide_tree_updates_that_file_without_a_walk() {
    let r = Repo::new("wide");
    for i in 0..400 {
        r.write(&format!("m{}/p{i}/a.py", i % 20), format!("def f_{i}():\n    return {i}\n").as_bytes());
    }
    assert_eq!(r.run(&["index"]), 0);
    let (_child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    #[cfg(target_os = "linux")]
    let before = inotify_watches(_child.0.id());
    r.write("m3/p123/a.py", b"def saved_deep():\n    return 1\n");
    burst_was(&wait_update(&rx, 1, 0), Some(1), false);
    #[cfg(target_os = "linux")]
    assert_eq!(inotify_watches(_child.0.id()), before, "no directory is watched again");
    assert!(r.indexed("saved_deep"));
}

/// About 2 MB of change records: far past a Windows watch buffer (64 KiB), whose overflow drops
/// them all and must walk the tree. Nothing changes after the burst, so a lost file stays lost.
#[test]
fn a_burst_larger_than_the_watch_buffer_is_indexed_whole() {
    let (r, _child, rx) = watched("buffer", &[("src/a.py", "def alpha():\n    return 1\n")]);
    let pad = "p".repeat(80);
    for i in 0..5000 {
        r.write(&format!("src/{pad}_{i}.py"), format!("def burst_{i}():\n    return {i}\n").as_bytes());
    }
    wait_update_any(&rx);
    let t0 = Instant::now();
    while !(r.indexed("burst_4999()") && r.indexed("burst_0()") && r.run(&["check", "--files"]) == 0) {
        if t0.elapsed().as_secs() >= 120 {
            // what the watcher said and what the index lacks, so a failure on a runner can be read
            let said: Vec<String> = rx.try_iter().collect();
            let (first, last) = (r.indexed("burst_0()"), r.indexed("burst_4999()"));
            panic!("timed out waiting for every file of the burst (burst_0 {first}, burst_4999 {last}); the watcher said: {said:#?}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_file_created_next_to_another_is_indexed_without_a_walk() {
    let (r, _child, rx) = watched("created", &[("src/a.py", "def alpha():\n    return 1\n")]);
    r.write("src/b.py", b"def created_next():\n    return 2\n");
    burst_was(&wait_update(&rx, 1, 0), Some(1), false);
    assert!(r.indexed("created_next"));
}

#[test]
fn a_removed_file_leaves_the_index_without_a_walk() {
    let (r, _child, rx) = watched("removed", &[("src/a.py", "def alpha():\n    return 1\n"), ("src/b.py", "def removed_one():\n    return 2\n")]);
    fs::remove_file(r.root.join("src/b.py")).unwrap();
    burst_was(&wait_update(&rx, 0, 1), Some(1), false);
    assert!(!r.indexed("removed_one"));
}

#[test]
fn a_file_renamed_in_its_directory_is_moved_in_the_index_without_a_walk() {
    let (r, _child, rx) = watched("renamed", &[("src/a.py", "def alpha():\n    return 1\n"), ("src/b.py", "def renamed_one():\n    return 2\n")]);
    fs::rename(r.root.join("src/b.py"), r.root.join("src/c.py")).unwrap();
    burst_was(&wait_update(&rx, 1, 1), Some(2), false);
    assert_eq!(r.run(&["grep", "--index", "-l", "renamed_one"]), 0);
    assert_eq!(r.run(&["check", "--files"]), 0);
}

#[test]
fn an_editor_save_through_a_temporary_file_is_indexed_without_a_walk() {
    let (r, _child, rx) = watched("editor", &[("src/a.py", "def before_save():\n    return 1\n")]);
    r.write("src/.a.py.swp", b"def after_save():\n    return 2\n");
    fs::rename(r.root.join("src/.a.py.swp"), r.root.join("src/a.py")).unwrap();
    burst_was(&wait_update(&rx, 1, 0), Some(2), false);
    assert!(r.indexed("after_save") && !r.indexed("before_save"));
}

#[test]
fn a_save_with_a_new_directory_in_the_same_burst_walks_the_tree() {
    let (r, _child, rx) = watched("mixed", &[("src/a.py", "def alpha():\n    return 1\n")]);
    r.write("src/a.py", b"def alpha_saved():\n    return 1\n");
    r.write("src/new/b.py", b"def in_new_dir():\n    return 2\n");
    burst_was(&wait_update(&rx, 2, 0), None, true);
    assert!(r.indexed("alpha_saved") && r.indexed("in_new_dir"));
}

#[test]
fn a_save_of_an_ignored_file_starts_no_update() {
    let (r, _child, rx) = watched("ignored", &[(".ignore", "skip.py\n"), ("src/a.py", "def alpha():\n    return 1\n")]);
    r.write("src/skip.py", b"def ignored_one():\n    return 2\n");
    // longer than a burst's quiet time
    std::thread::sleep(Duration::from_millis(2500));
    let lines: Vec<String> = rx.try_iter().collect();
    assert!(lines.iter().all(|l| !l.contains("changed")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("burst:")), "{lines:?}");
    r.write("src/a.py", b"def alpha_two():\n    return 1\n");
    burst_was(&wait_update(&rx, 1, 0), Some(1), false);
}

/// Like `watched`, for a repository at `<name>/repo`; -> (`<name>`, the repository, ...).
fn watched_below(name: &str, files: &[(&str, &str)]) -> (Repo, Repo, KillOnDrop, Receiver<String>) {
    let above = Repo::new(name);
    let r = Repo { root: above.root.join("repo") };
    for (rel, text) in files {
        r.write(rel, text.as_bytes());
    }
    assert_eq!(r.run(&["index"]), 0);
    let (child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    (above, r, child, rx)
}

#[test]
fn a_file_an_ignore_file_above_the_root_starts_to_list_leaves_the_index() {
    let (above, r, _child, rx) =
        watched_below("aboveadd", &[("src/a.py", "def alpha():\n    return 1\n"), ("src/b.py", "def listed_above():\n    return 2\n")]);
    above.write(".ignore", b"b.py\n");
    burst_was(&wait_update(&rx, 0, 1), Some(0), true);
    assert!(!r.indexed("listed_above"));
}

#[test]
fn a_file_the_exclude_file_of_a_nested_repository_starts_to_list_leaves_the_index() {
    let (r, _child, rx) = watched(
        "nestedexclude",
        &[("src/a.py", "def alpha():\n    return 1\n"), ("vendor/lib/.git/info/exclude", ""), ("vendor/lib/x.py", "def excluded_later():\n    return 2\n")],
    );
    r.write("vendor/lib/.git/info/exclude", b"x.py\n");
    burst_was(&wait_update(&rx, 0, 1), Some(0), true);
    assert!(!r.indexed("excluded_later"));
}

#[test]
fn a_change_of_the_git_configuration_that_names_the_excludes_file_walks_the_tree() {
    let above = Repo::new("gitconfig");
    let r = Repo { root: above.root.join("repo") };
    r.write(".git/HEAD", b"ref: refs/heads/main\n");
    r.write("src/a.py", b"def alpha():\n    return 1\n");
    r.write("src/b.py", b"def listed_globally():\n    return 2\n");
    above.write("rules", b"b.py\n");
    let config = above.root.join("gitconfig");
    let names = |file: &str| format!("[core]\n\texcludesFile = {}\n", above.root.join(file).display());
    above.write("gitconfig", names("none").as_bytes());
    let run = |args: &[&str]| {
        let mut cmd = Command::new(BIN);
        cmd.args(args).current_dir(&r.root).env("GIT_CONFIG_GLOBAL", &config).env("XDG_CONFIG_HOME", "/nonexistent");
        cmd.stdin(Stdio::null()).output().unwrap().status.code()
    };
    assert_eq!(run(&["index"]), Some(0));
    assert_eq!(run(&["grep", "--index", "-F", "listed_globally"]), Some(0));
    let (_child, rx) = start_watch_with(&r.root, &config);
    wait_watching(&rx);
    above.write("gitconfig", names("rules").as_bytes());
    burst_was(&wait_update(&rx, 0, 1), Some(0), true);
    assert_eq!(run(&["grep", "--index", "-F", "listed_globally"]), Some(1));
}

#[test]
fn a_file_written_above_the_root_starts_no_update() {
    let (above, r, _child, rx) = watched_below("aboveother", &[("src/a.py", "def alpha():\n    return 1\n")]);
    above.write("notes.txt", b"x\n");
    // outside a git repository the walks read no .gitignore
    above.write(".gitignore", b"a.py\n");
    std::thread::sleep(Duration::from_millis(2500));
    let lines: Vec<String> = rx.try_iter().collect();
    assert!(lines.iter().all(|l| !l.contains("changed")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("burst:")), "{lines:?}");
    r.write("src/a.py", b"def alpha_two():\n    return 1\n");
    burst_was(&wait_update(&rx, 1, 0), Some(1), false);
}

/// Watch `dir` (indexed first), save each of `ignored` (path, function name) and then `marker`:
/// once the marker is indexed, no ignored function may be.
fn saves_stay_out(dir: &Path, ignored: &[(&str, &str)], marker: (&str, &str)) {
    assert_eq!(run_in(dir, &["index"]), 0);
    for (rel, _) in ignored {
        assert!(dir.join(rel).is_file(), "{rel} is in the tree before the watch starts");
    }
    let (_child, rx) = start_watch(dir);
    wait_watching(&rx);
    for (rel, name) in ignored.iter().chain([&marker]) {
        fs::write(dir.join(rel), format!("def {name}():\n    return 2\n")).unwrap();
    }
    wait_for(20, "the marker", || indexed_in(dir, marker.1));
    for (rel, name) in ignored {
        assert!(!indexed_in(dir, name), "{rel} is ignored by the walk, and saving it put it in the index");
    }
}

#[test]
fn a_save_of_a_file_an_ignore_file_above_the_repository_lists_stays_out() {
    let r = Repo::new("above");
    fs::create_dir_all(r.root.join("proj/.git")).unwrap();
    r.write(".ignore", b"secret.py\n");
    r.write("proj/secret.py", b"def secret_one():\n    return 1\n");
    r.write("proj/a.py", b"def alpha():\n    return 1\n");
    saves_stay_out(&r.root.join("proj"), &[("secret.py", "secret_two")], ("a.py", "alpha_two"));
}

#[test]
fn a_root_outside_git_keeps_the_rules_of_a_repository_inside_it() {
    let r = Repo::new("several");
    fs::create_dir_all(r.root.join("repo1/.git")).unwrap();
    r.write("repo1/.gitignore", b"gen.py\ncov/\n");
    r.write("repo1/gen.py", b"def gen_one():\n    return 1\n");
    r.write("repo1/cov/x.py", b"def cov_one():\n    return 1\n");
    r.write("repo1/a.py", b"def alpha():\n    return 1\n");
    saves_stay_out(&r.root, &[("repo1/gen.py", "gen_two"), ("repo1/cov/x.py", "cov_two")], ("repo1/a.py", "alpha_two"));
}

#[test]
fn a_linked_worktree_keeps_the_exclude_file_of_its_main_repository() {
    let r = Repo::new("worktree");
    let common = r.root.join("main/.git");
    r.write("main/.git/info/exclude", b"private.py\n");
    r.write("main/.git/HEAD", b"ref: refs/heads/main\n");
    r.write("main/.git/worktrees/wt/commondir", b"../..\n");
    r.write("main/.git/worktrees/wt/HEAD", b"ref: refs/heads/wt\n");
    r.write("main/.git/worktrees/wt/gitdir", format!("{}\n", r.root.join("wt/.git").display()).as_bytes());
    r.write("wt/.git", format!("gitdir: {}\n", common.join("worktrees/wt").display()).as_bytes());
    r.write("wt/private.py", b"def private_one():\n    return 1\n");
    r.write("wt/a.py", b"def alpha():\n    return 1\n");
    let wt = r.root.join("wt");
    saves_stay_out(&wt, &[("private.py", "private_two")], ("a.py", "alpha_two"));
    assert!(indexed_in(&wt, "alpha_two"));
    let (_child, rx) = start_watch(&wt);
    wait_watching(&rx);
    // a rule added to that exclude file applies to the next save
    fs::write(common.join("info/exclude"), "private.py\nmore.py\n").unwrap();
    fs::write(wt.join("more.py"), "def more_one():\n    return 1\n").unwrap();
    fs::write(wt.join("b.py"), "def beta():\n    return 1\n").unwrap();
    wait_for(20, "b.py", || indexed_in(&wt, "beta"));
    assert!(!indexed_in(&wt, "more_one"), "more.py is excluded by the main repository's info/exclude");
}

#[test]
fn a_nested_repository_does_not_take_the_ignore_rules_of_the_outer_one() {
    let r = Repo::new("nestedrepo");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    fs::create_dir_all(r.root.join("vendor/lib/.git")).unwrap();
    r.write(".gitignore", b"*.gen.py\nout/\n");
    r.write("src/a.py", b"def alpha():\n    return 1\n");
    r.write("vendor/lib/x.gen.py", b"def nested_gen():\n    return 1\n");
    r.write("vendor/lib/out/o.py", b"def nested_out():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    assert!(r.indexed("nested_gen") && r.indexed("nested_out"), "the walk lists both");
    let (_child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    r.write("vendor/lib/x.gen.py", b"def nested_gen_two():\n    return 1\n");
    wait_for(20, "the edit of vendor/lib/x.gen.py", || r.indexed("nested_gen_two"));
    r.write("vendor/lib/out/o2.py", b"def nested_out_two():\n    return 1\n");
    wait_for(20, "a new file in vendor/lib/out", || r.indexed("nested_out_two"));
}

/// A directory without read permission cannot be watched (inotify needs read access), as one past
/// the watch limit cannot; root has it whatever the mode says, so the test does nothing there.
#[cfg(target_os = "linux")]
#[test]
fn a_directory_that_could_not_be_watched_is_polled() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let r = Repo::new("unwatched");
    r.write("a.py", b"def alpha():\n    return 1\n");
    r.write("locked/b.py", b"def locked_one():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    let locked = r.root.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let restore = Restore(locked.clone());
    if fs::read_dir(&locked).is_ok() {
        return;
    }
    let (_child, rx) = start_watch(&r.root);
    let mut lines = Vec::new();
    while !lines.last().is_some_and(|l: &String| l.contains("watching")) {
        lines.push(rx.recv_timeout(Duration::from_secs(60)).expect("watch starts"));
    }
    assert!(lines.iter().any(|l| l.contains("could not watch 1 of")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("checked on a timer")), "{lines:?}");
    drop(restore);
    // no event comes from locked/: the poll finds this file, with no other change and no walk
    r.write("locked/c.py", b"def locked_new():\n    return 1\n");
    burst_was(&wait_update_any(&rx), None, false);
    wait_for(20, "locked/c.py", || r.indexed("locked_new"));
}

/// A directory above the root that cannot be watched costs its rules' changes the wait for the next
/// walk, not a walk on every save. Root can read it whatever the mode says: nothing to test there.
#[cfg(target_os = "linux")]
#[test]
fn a_directory_above_the_root_that_could_not_be_watched_does_not_walk_every_burst() {
    use std::os::unix::fs::PermissionsExt;
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let above = Repo::new("aboveunwatched");
    let r = Repo { root: above.root.join("repo") };
    r.write("src/a.py", b"def alpha():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    // search permission only: the walks still open its .ignore by name, inotify cannot watch it
    fs::set_permissions(&above.root, fs::Permissions::from_mode(0o311)).unwrap();
    let _restore = Restore(above.root.clone());
    if fs::read_dir(&above.root).is_ok() {
        return;
    }
    let (_child, rx) = start_watch(&r.root);
    let mut lines = Vec::new();
    while !lines.last().is_some_and(|l: &String| l.contains("watching")) {
        lines.push(rx.recv_timeout(Duration::from_secs(60)).expect("watch starts"));
    }
    assert!(lines.iter().any(|l| l.contains("that only hold ignore rules")), "{lines:?}");
    assert!(lines.iter().all(|l| !l.contains("directories (")), "every directory in the tree has a watch: {lines:?}");
    r.write("src/a.py", b"def alpha_two():\n    return 1\n");
    burst_was(&wait_update(&rx, 1, 0), Some(1), false);
}

/// Stderr lines up to the first update line after a burst line.
fn wait_update_any(rx: &Receiver<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    loop {
        let line = rx.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| panic!("no update in {seen:?}"));
        let done = line.contains(" changed, ") && seen.iter().any(|l| l.contains("burst:"));
        seen.push(line);
        if done {
            return seen;
        }
    }
}

/// The names of the definitions the code index holds, read from its database without an update.
#[cfg(unix)]
fn chunk_names(root: &Path) -> String {
    let db = rusqlite::Connection::open_with_flags(root.join(".scrutate/index.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.query_row("SELECT group_concat(qualname, ' ') FROM chunks", [], |r| r.get::<_, Option<String>>(0)).unwrap().unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn ctrl_c_ends_a_wait_for_another_writer() {
    let (r, mut child, _rx) = watched("sigint_wait", &[("a.py", "def alpha():\n    return 1\n")]);
    let db = rusqlite::Connection::open(r.root.join(".scrutate/index.db")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    r.write("a.py", b"def alpha_new():\n    return 1\n");
    // the update of that burst is waiting for the lock by now
    std::thread::sleep(Duration::from_secs(2));
    assert!(Command::new("kill").args(["-INT", &child.0.id().to_string()]).status().unwrap().success());
    let t0 = Instant::now();
    wait_for(5, "watch to stop on Ctrl-C", || child.0.try_wait().unwrap().is_some());
    assert_eq!(child.0.wait().unwrap().code(), Some(130), "{:?}", t0.elapsed());
    db.execute_batch("ROLLBACK").unwrap();
}

#[cfg(unix)]
#[test]
fn a_failed_update_is_tried_again_with_no_further_change() {
    use std::os::unix::fs::PermissionsExt;
    let (r, _child, rx) = watched("retry", &[("a.py", "def alpha():\n    return 1\n")]);
    let seg = r.root.join(".scrutate/seg");
    fs::set_permissions(&seg, fs::Permissions::from_mode(0o555)).unwrap();
    r.write("a.py", b"def alpha_new():\n    return 1\n");
    wait_for(30, "the update to fail", || rx.try_iter().any(|l| l.contains("update failed")));
    fs::set_permissions(&seg, fs::Permissions::from_mode(0o755)).unwrap();
    // no file changes again: the retry alone indexes the burst
    wait_for(30, "the retry", || chunk_names(&r.root).contains("alpha_new"));
}

/// Ctrl-C during watch's first update stops it at its next batch of files, and a second Ctrl-C
/// ends the process at once, as for any program.
#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_long_first_update_of_watch() {
    let r = Repo::new("sigint_update");
    let body: String = (0..40).map(|j| format!("def f{j}(x):\n    return x + {j}\n\n")).collect();
    for i in 0..20000 {
        r.write(&format!("d{}/m{i}.py", i % 200), body.as_bytes());
    }
    let (mut child, rx) = start_watch(&r.root);
    // the first update is reading the files by now
    std::thread::sleep(Duration::from_millis(1500));
    assert!(child.0.try_wait().unwrap().is_none(), "watch ended before the signal");
    assert!(!rx.try_iter().any(|l| l.contains("watching")), "the first update ended before the signal: a larger tree is needed");
    assert!(Command::new("kill").args(["-INT", &child.0.id().to_string()]).status().unwrap().success());
    let t0 = Instant::now();
    wait_for(10, "watch to stop on Ctrl-C", || child.0.try_wait().unwrap().is_some());
    assert_eq!(child.0.wait().unwrap().code(), Some(130), "{:?}", t0.elapsed());
}

/// A rename that changes only the case of a name (on macOS's default disk the old name still
/// opens the file) leaves the text index with the new name alone.
#[test]
fn a_rename_of_the_case_alone_leaves_one_name_in_the_text_index() {
    let (r, _child, rx) = watched("case_rename", &[("src/Foo.txt", "kappa\n"), ("mod.py", "def f():\n    return 1\n")]);
    fs::rename(r.root.join("src/Foo.txt"), r.root.join("src/foo.txt")).unwrap();
    let listed = || {
        let out = Command::new(BIN).args(["grep", "--index", "-l", "kappa"]).current_dir(&r.root).stdin(Stdio::null()).output().unwrap();
        let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect();
        v.sort();
        v
    };
    wait_for(30, "the rename in the text index", || listed().iter().any(|p| p.ends_with("foo.txt")));
    // a later burst may still be on its way: give it the time a burst takes
    std::thread::sleep(Duration::from_secs(3));
    let _ = rx.try_iter().count();
    assert_eq!(listed(), ["src/foo.txt"]);
}

/// Ctrl-C ends watch's wait for the text index's lock that another process holds, as it ends the
/// wait for the code index's.
#[cfg(unix)]
#[test]
fn ctrl_c_ends_a_wait_for_the_text_index_lock() {
    let (r, mut child, _rx) = watched("text_lock", &[("a.txt", "one\n"), ("mod.py", "def f():\n    return 1\n")]);
    let lock = fs::File::options().write(true).open(r.root.join(".scrutate/text/lock")).unwrap();
    lock.lock().unwrap();
    r.write("a.txt", b"two\n");
    // the burst reaches the text index and waits for its lock
    std::thread::sleep(Duration::from_secs(4));
    assert!(child.0.try_wait().unwrap().is_none(), "watch ended before the signal");
    assert!(Command::new("kill").args(["-INT", &child.0.id().to_string()]).status().unwrap().success());
    wait_for(10, "watch to stop on Ctrl-C while the text index is locked", || child.0.try_wait().unwrap().is_some());
    assert_eq!(child.0.wait().unwrap().code(), Some(130));
    drop(lock);
}
