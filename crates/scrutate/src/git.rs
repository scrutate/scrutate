//! The `git` program, run in a repository: the one program Engine B starts. It reads what a
//! change touches (`impact`): the base commit, the diff with each side's changed lines, the
//! untracked files and the blobs of the base copies.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Result};

use crate::fmt::echo;

/// Where the changed side of a diff comes from.
pub(crate) enum New {
    Tree,
    Staged,
    Commit(String),
}

/// One file of the diff: its paths on each side (None where it does not exist), git's blob ids,
/// and the changed lines of each side as (first, last).
#[derive(Default, Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) old: Option<String>,
    pub(crate) new: Option<String>,
    pub(crate) old_blob: String,
    pub(crate) new_blob: String,
    pub(crate) submodule: bool,
    pub(crate) binary: bool,
    pub(crate) untracked: bool,
    pub(crate) old_lines: Vec<(i64, i64)>,
    pub(crate) new_lines: Vec<(i64, i64)>,
}

/// The `git` program run in the repository, every call through `command`. Nothing it runs
/// writes, or starts a program of the configuration's: no optional locks, no file system monitor,
/// no external diff or text conversion, only commands that read (a diff by `diff-index` or
/// `diff-tree`, which never refresh the index), no clean or process filter, and no fetch of an
/// object a partial clone lacks. A revision is checked before it is passed, so it never reaches
/// git as an option.
pub(crate) struct Git {
    dir: PathBuf,
    env: Env,
    /// `no_filters`, read once
    filters: std::sync::OnceLock<Vec<String>>,
}

/// The pathspec settings git refuses beside `--literal-pathspecs`, never passed on.
const PATHSPEC_ENV: [&str; 3] = ["GIT_GLOB_PATHSPECS", "GIT_NOGLOB_PATHSPECS", "GIT_ICASE_PATHSPECS"];

/// The git commands `Git` runs: each only reads. A porcelain `diff` or `status` refreshes the
/// index and writes it back.
const READS: [&str; 9] = ["--version", "rev-parse", "merge-base", "config", "ls-files", "cat-file", "diff-index", "diff-tree", "log"];

/// What git gets of the environment.
enum Env {
    /// this process's own, these of `PASSED_ENV` set over it
    Own(Vec<(String, String)>),
    /// these of `PASSED_ENV`, and no GIT_* variable of this process
    Given(Vec<(String, String)>),
}

/// The variables that say where a repository's files are: the only ones another process may hand to git.
pub(crate) const PASSED_ENV: [&str; 6] =
    ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR"];

/// How git 2.53 and 2.54 read a relative value of each of `PASSED_ENV`, probed from a subdirectory:
///
/// | variable | read against | in git's source |
/// |---|---|---|
/// | GIT_DIR | the directory git starts in | setup.c `update_relative_gitdir` moves it with git's chdir |
/// | GIT_WORK_TREE | the directory git starts in | setup.c makes it absolute during setup |
/// | GIT_OBJECT_DIRECTORY | checked there, used at the work tree's top | setup.c `is_git_directory` (access), odb.c after the chdir |
/// | GIT_COMMON_DIR | checked there, used at the work tree's top | setup.c `get_common_dir`, repository.c `repo_set_commondir` |
/// | GIT_INDEX_FILE | the work tree's top | repository.c `expand_base_dir` keeps it as given |
/// | GIT_ALTERNATE_OBJECT_DIRECTORIES | the work tree's top | odb.c resolves alternates when it reads them |
///
/// These four are made absolute against the user's directory, so they hold wherever git starts; a
/// relative one of the middle two works in git itself only from the top. The other two stay as
/// given: git reaches the same top from the index root as from the user's directory.
const FROM_START: [&str; 4] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_OBJECT_DIRECTORY", "GIT_COMMON_DIR"];

/// The `PASSED_ENV` variables `vars` holds, a relative value of those of `FROM_START` made absolute
/// against `cwd`, the directory the user set them in.
pub(crate) fn absolute_env(cwd: &Path, vars: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    // Windows reads names in any case
    let named = |n: &str| PASSED_ENV.into_iter().find(|p| if cfg!(windows) { p.eq_ignore_ascii_case(n) } else { *p == n });
    vars.into_iter()
        .filter_map(|(name, value)| {
            let name = named(&name)?;
            let relative = !value.is_empty() && Path::new(&value).is_relative();
            let value = if relative && FROM_START.contains(&name) { cwd.join(&value).to_string_lossy().into_owned() } else { value };
            Some((name.to_string(), value))
        })
        .collect()
}

/// `absolute_env` of this process's variables, against its working directory.
pub(crate) fn own_env() -> Vec<(String, String)> {
    let vars = std::env::vars_os().filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)));
    absolute_env(&std::env::current_dir().unwrap_or_default(), vars)
}

/// The git directory's own configuration file: git reads core.worktree there alone, never from an
/// included file or the global and system ones, which `config --get` also reads.
fn own_config(dir: &Path) -> String {
    dir.join("config").to_string_lossy().into_owned()
}

/// With GIT_DIR and no GIT_WORK_TREE: when that git directory's core.worktree (read against the
/// git directory, as git reads it) does not hold `root`, why git finds no work tree here, naming
/// both.
fn tree_elsewhere(root: &Path, set: &[(String, String)]) -> Option<String> {
    let probe = Git { dir: plain_dir(root), env: Env::Own(set.to_vec()), filters: Default::default() };
    let text = |out: &[u8]| String::from_utf8_lossy(out.trim_ascii_end()).into_owned();
    let (true, dir, _) = probe.output(&["rev-parse", "--absolute-git-dir"]).ok()? else { return None };
    let dir = PathBuf::from(text(&dir));
    let (true, tree, _) = probe.output(&["config", "--file", &own_config(&dir), "--get", "core.worktree"]).ok()? else { return None };
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let tree = canonical(&dir.join(text(&tree)));
    let plain = crate::paths::plain_path;
    (!canonical(root).starts_with(&tree)).then(|| {
        format!(
            "GIT_DIR names {}, a git directory whose work tree is {}, not {}: run there, or unset GIT_DIR",
            plain(&dir).display(),
            plain(&tree).display(),
            plain(root).display()
        )
    })
}

impl Git {
    /// git in this process's environment, a relative value of `FROM_START` read where it was set.
    pub(crate) fn new(root: &Path) -> Result<Git> {
        let set = own_env();
        let given = |name: &str| set.iter().any(|(n, v)| n == name && !v.is_empty());
        let dir_alone = given("GIT_DIR") && !given("GIT_WORK_TREE");
        let git = match Git::overriding(root, set.clone()) {
            Err(e) if dir_alone => return Err(tree_elsewhere(root, &set).map_or(e, |why| anyhow!(why))),
            r => r?,
        };
        if dir_alone {
            git.root_s_own_git_dir(root)?;
        }
        Ok(git)
    }

    /// With GIT_DIR and no GIT_WORK_TREE git takes `root`, where it runs, as the work tree: the
    /// git directory must be `root`'s own, or name the tree in core.worktree, or the answers are
    /// another repository's history laid over this tree.
    fn root_s_own_git_dir(&self, root: &Path) -> Result<()> {
        let dir_of = |git: &Git| {
            let (ok, out, _) = git.output(&["rev-parse", "--absolute-git-dir"]).ok()?;
            let dir = PathBuf::from(String::from_utf8_lossy(out.trim_ascii_end()).into_owned());
            ok.then(|| std::fs::canonicalize(&dir).unwrap_or(dir))
        };
        let own = Git { dir: plain_dir(root), env: Env::Given(Vec::new()), filters: Default::default() };
        // none: `checked` already refused it, or git reports no repository
        let Some(named) = dir_of(self) else { return Ok(()) };
        if dir_of(&own).as_ref() == Some(&named) || self.output(&["config", "--file", &own_config(&named), "--get", "core.worktree"]).is_ok_and(|(ok, _, _)| ok)
        {
            return Ok(());
        }
        bail!(
            "GIT_DIR names {}, which is not the git directory of {}: set GIT_WORK_TREE to the tree it belongs to, or unset GIT_DIR",
            crate::paths::plain_path(&named).display(),
            crate::paths::plain_path(root).display()
        );
    }

    fn overriding(root: &Path, set: Vec<(String, String)>) -> Result<Git> {
        Git::checked(Git { dir: plain_dir(root), env: Env::Own(set), filters: Default::default() }, root)
    }

    /// As `new`, with git given these of `PASSED_ENV` and no GIT_* variable of this process; any
    /// other name is refused before git runs.
    #[cfg_attr(not(test), expect(dead_code, reason = "the session's caller of this comes with impact requests that carry git's variables"))]
    pub(crate) fn with_env(root: &Path, env: &[(String, String)]) -> Result<Git> {
        if let Some((name, _)) = env.iter().find(|(name, _)| !PASSED_ENV.contains(&name.as_str())) {
            bail!("{} cannot be handed to git: only {} can", echo(name), PASSED_ENV.join(", "));
        }
        Git::checked(Git { dir: plain_dir(root), env: Env::Given(env.to_vec()), filters: Default::default() }, root)
    }

    fn checked(git: Git, root: &Path) -> Result<Git> {
        let out = git.command(&["rev-parse", "--is-inside-work-tree"])?.output().map_err(not_run)?;
        if !out.status.success() || out.stdout.trim_ascii() != b"true" {
            let why = first_line(&out.stderr);
            if why.is_empty() || why.contains("not a git repository") {
                bail!("{} is not in a git repository: impact reads the changes with git", crate::paths::plain_path(root).display());
            }
            bail!("git: {why}");
        }
        Ok(git)
    }

    /// The one way git starts (`tests/git_spawns.rs` keeps it so): a command of `READS`, with
    /// every filter driver emptied, on the contract of `raw`.
    fn command(&self, args: &[&str]) -> Result<Command> {
        let verb = verb(args);
        if !READS.contains(&verb) {
            bail!("git {verb} is no command scrutate runs: it may write the repository");
        }
        let filters = match self.filters.get() {
            Some(f) => f,
            None => {
                let f = self.no_filters()?;
                self.filters.get_or_init(|| f)
            }
        };
        // a diff runs no external diff or textconv driver, whatever the call passed
        let at = verb_at(args);
        let shows_diffs = matches!(verb, "diff-index" | "diff-tree" | "log");
        let no_drivers: &[&str] = if shows_diffs { &["--no-ext-diff", "--no-textconv"] } else { &[] };
        let all: Vec<&str> = filters
            .iter()
            .map(String::as_str)
            .chain(args[..=at].iter().copied())
            .chain(no_drivers.iter().copied())
            .chain(args[at + 1..].iter().copied())
            .collect();
        Ok(self.raw(&all))
    }

    /// git with `args`: no optional locks, no file system monitor, no fetch, and the environment
    /// `env` gives. Only `command`, and `no_filters` under it, call this.
    fn raw(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        // no file system monitor, and no signature check, which runs gpg.program
        c.current_dir(&self.dir).args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "log.showSignature=false"]).args(args).stdin(Stdio::null());
        match &self.env {
            Env::Own(set) => {
                c.envs(set.iter().map(|(name, value)| (name, plain_value(name, value))));
            }
            Env::Given(env) => {
                // git reads its configuration, programs and paths from GIT_* names: none of this process's reach it
                for (name, _) in std::env::vars_os() {
                    if name.to_string_lossy().to_ascii_uppercase().starts_with("GIT_") {
                        c.env_remove(name);
                    }
                }
                for name in PASSED_ENV {
                    c.env_remove(name);
                }
                c.envs(env.iter().map(|(name, value)| (name, plain_value(name, value))));
            }
        }
        // a partial clone would fetch an object it lacks: network, a write to .git/objects, and
        // the remote's upload program from the configuration (git 2.44 and later read this)
        c.env("GIT_NO_LAZY_FETCH", "1");
        // git refuses `--literal-pathspecs` beside a global glob or case-folding pathspec setting
        for name in PATHSPEC_ENV {
            c.env_remove(name);
        }
        c
    }

    /// (exited with 0, standard output, standard error)
    fn output(&self, args: &[&str]) -> Result<(bool, Vec<u8>, String)> {
        let out = self.command(args)?.output().map_err(not_run)?;
        Ok((out.status.success(), out.stdout, reason(out.status, &out.stderr)))
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        match self.output(args)? {
            (true, out, _) => Ok(out),
            (false, _, why) => Err(failed(args, &why)),
        }
    }

    /// The commit `rev` names; `A...B` is the merge base of the two (an empty side is `HEAD`).
    pub(crate) fn base(&self, rev: &str) -> Result<String> {
        let Some((a, b)) = rev.split_once("...") else { return self.commit(rev) };
        let (a, b) = (if a.is_empty() { "HEAD" } else { a }, if b.is_empty() { "HEAD" } else { b });
        let (x, y) = (self.commit(a)?, self.commit(b)?);
        match self.output(&["merge-base", &x, &y])? {
            (true, out, _) if !out.trim_ascii().is_empty() => Ok(String::from_utf8_lossy(out.trim_ascii()).into_owned()),
            _ => bail!("{} and {} have no merge base", echo(a), echo(b)),
        }
    }

    /// The commit a head names: one commit, so a range (two dots or three) is refused as one.
    pub(crate) fn head(&self, rev: &str) -> Result<String> {
        if rev.contains("..") {
            bail!("{} is a range, not a revision: name one commit", echo(rev));
        }
        self.commit(rev)
    }

    pub(crate) fn commit(&self, rev: &str) -> Result<String> {
        // a revision is one argument and never an option, so `--output=x` is refused here
        if rev.is_empty() || rev.starts_with('-') {
            bail!("not a revision: {}", echo(rev));
        }
        match self.output(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])? {
            (true, out, _) => Ok(String::from_utf8_lossy(out.trim_ascii()).into_owned()),
            _ if rev == "HEAD" => bail!("HEAD names no commit yet: commit once, or pass a base revision"),
            // a ref name holds no `..`, so this is a range: two dots for the three of a merge base
            _ if rev.contains("..") => {
                bail!("{} is a range, not a revision: pass a base revision, or A...B (three dots) for the merge base of A and B", echo(rev))
            }
            _ => bail!("not a revision of this repository: {}", echo(rev)),
        }
    }

    /// The files that differ from `base`, with their changed lines, and the untracked files.
    pub(crate) fn diff(&self, base: &str, new: &New) -> Result<Vec<Entry>> {
        // plumbing, which never refreshes the index and so never writes it back
        let mut common: Vec<String> = Vec::new();
        common.push(if matches!(new, New::Commit(_)) { "diff-tree" } else { "diff-index" }.to_string());
        // git 2.53 and 2.54 crash on `--cached --relative` below a conflict outside the directory:
        // the staged diff of a root below the top names the root as its pathspec instead, and the
        // paths git gives from the top lose the root's own part here
        let prefix = if matches!(new, New::Staged) {
            String::from_utf8_lossy(self.run(&["rev-parse", "--show-prefix"])?.trim_ascii()).into_owned()
        } else {
            String::new()
        };
        let relative: &[&str] = if prefix.is_empty() { &["--relative"] } else { &[] };
        common.extend(["-r", "--no-color", "--no-ext-diff", "--no-textconv"].map(String::from));
        common.extend(relative.iter().map(|s| s.to_string()));
        common.extend(["-M", "--src-prefix=a/", "--dst-prefix=b/"].map(String::from));
        let with = |more: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = common.iter().cloned().chain(more.iter().map(|s| s.to_string())).collect();
            if matches!(new, New::Staged) {
                v.push("--cached".into());
            }
            v.push(base.to_string());
            if let New::Commit(head) = new {
                v.push(head.clone());
            }
            v.push("--".into());
            if !prefix.is_empty() {
                v.push(".".into());
            }
            v
        };
        // the working tree's diff reports a file stopped at a conflict as edited, not as unmerged
        if matches!(new, New::Tree) {
            let unmerged = self.run(&["ls-files", "-u", "-z"])?;
            if let Some(path) = unmerged.split(|&b| b == 0).find_map(|e| e.splitn(2, |&b| b == b'\t').nth(1)) {
                bail!("{} is unmerged: finish the merge before asking what it changes", echo(&String::from_utf8_lossy(path)));
            }
        }
        let raw = self.run(&with(&["--raw", "-z", "--no-abbrev"]).iter().map(String::as_str).collect::<Vec<_>>())?;
        let patch = self.run(&with(&["-p", "-U0", "--inter-hunk-context=0"]).iter().map(String::as_str).collect::<Vec<_>>())?;
        let mut entries = raw_entries(&raw)?;
        let sections = sections(&patch);
        // a file whose stat alone changed is in the raw list and has no section: nothing changed
        entries.retain_mut(|e| {
            let key = e.new.as_deref().or(e.old.as_deref()).unwrap_or_default();
            match sections.get(key) {
                Some(s) => {
                    e.binary = s.binary;
                    e.old_lines = s.old.clone();
                    e.new_lines = s.new.clone();
                    true
                }
                None => e.submodule || e.old != e.new,
            }
        });
        if !prefix.is_empty() {
            let strip = |p: &mut Option<String>| {
                if let Some(rest) = p.as_deref().and_then(|p| p.strip_prefix(prefix.as_str())) {
                    *p = Some(rest.to_string());
                }
            };
            for e in &mut entries {
                strip(&mut e.old);
                strip(&mut e.new);
            }
        }
        if matches!(new, New::Tree) {
            for p in self.run(&["ls-files", "--others", "--exclude-standard", "-z"])?.split(|&b| b == 0).filter(|p| !p.is_empty()) {
                entries.push(Entry { new: Some(String::from_utf8_lossy(p).into_owned()), untracked: true, ..Entry::default() });
            }
        }
        Ok(entries)
    }

    /// The commits a shallow clone's history stops at; none in a full clone.
    pub(crate) fn shallow_boundaries(&self) -> Result<BTreeSet<String>> {
        if self.run(&["rev-parse", "--is-shallow-repository"])?.trim_ascii() != b"true" {
            return Ok(BTreeSet::new());
        }
        let at = String::from_utf8_lossy(&self.run(&["rev-parse", "--git-path", "shallow"])?).trim().to_string();
        let file = std::fs::read_to_string(self.dir.join(at)).unwrap_or_default();
        Ok(file.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
    }

    /// The version of git, as (major, minor).
    pub(crate) fn version(&self) -> Result<(u32, u32)> {
        let out = String::from_utf8_lossy(&self.run(&["--version"])?).into_owned();
        let v = out.split_whitespace().nth(2).unwrap_or_default();
        let mut parts = v.split('.').map(|p| p.parse::<u32>().ok());
        match (parts.next().flatten(), parts.next().flatten()) {
            (Some(a), Some(b)) => Ok((a, b)),
            _ => bail!("git said its version is {}", out.trim()),
        }
    }

    /// The commits, newest first and at most `n`, that changed lines `start` to `end` of `path` (from
    /// the index root) as HEAD holds them; `git log -L` follows those lines through renames. It walks
    /// the file's whole history, so git is stopped at `limit`: then the answer is the commits it had
    /// printed, the newest, and `true`.
    pub(crate) fn line_log(&self, path: &str, start: i64, end: i64, n: usize, limit: std::time::Duration) -> Result<(Vec<Commit>, bool)> {
        // the path is all that follows the range's colon, so a colon or a dash in it reads as itself
        let range = format!("{start},{end}:{}", crate::paths::plain(path));
        let n = n.to_string();
        let args = [
            "--literal-pathspecs",
            "log",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "-L",
            &range,
            "-s",
            "-n",
            &n,
            "--date=short",
            "--format=%x01%H%x00%ad%x00%s",
        ];
        let (out, status, why) = self.bounded(&args, limit)?;
        let stopped = status.is_none();
        if status.is_some_and(|s| !s.success()) {
            // log -L names a blob it lacks without the lazy-fetch warning diff gives
            if why.starts_with("unable to read ") && self.partial_clone() {
                return Err(LacksObjects("log".into()).into());
            }
            return Err(failed(&args, &why));
        }
        // a stopped git may have printed part of a line
        let whole = if stopped { out.iter().rposition(|&b| b == b'\n').map_or(&out[..0], |i| &out[..i]) } else { &out[..] };
        let commits = whole
            .split(|&b| b == b'\n')
            .filter_map(|line| line.strip_prefix(b"\x01"))
            .filter_map(|line| {
                let mut parts = line.splitn(3, |&b| b == 0).map(|p| String::from_utf8_lossy(p).into_owned());
                Some(Commit { id: parts.next()?, date: parts.next()?, subject: parts.next().unwrap_or_default(), boundary: false })
            })
            .collect();
        Ok((commits, stopped))
    }

    /// Whether this repository is a partial clone, which may lack objects by design.
    fn partial_clone(&self) -> bool {
        let any_set = |args: &[&str]| {
            self.output(args)
                .is_ok_and(|(ok, out, _)| ok && String::from_utf8_lossy(&out).lines().any(|l| l.split_once(' ').is_some_and(|(_, v)| v != "false")))
        };
        // older git names the promisor remote in extensions.partialClone, newer only marks the
        // remote; that mark is a boolean as git reads one (`no`, a bare key)
        any_set(&["config", "--get-regexp", r"^extensions\.partialclone$"]) || any_set(&["config", "--type=bool", "--get-regexp", r"^remote\..*\.promisor$"])
    }

    /// git with `args`, stopped once it has run for `limit`: its output so far, its exit status
    /// (none when stopped) and the first line of its errors.
    fn bounded(&self, args: &[&str], limit: std::time::Duration) -> Result<(Vec<u8>, Option<std::process::ExitStatus>, String)> {
        /// Kills and reaps git on every way out, an early return and a panic too: on Unix its whole
        /// process group, so a wrapper's forked child goes with it.
        struct Reap(std::process::Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                #[cfg(unix)]
                let _ = rustix::process::kill_process_group(rustix::process::Pid::from_child(&self.0), rustix::process::Signal::KILL);
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut cmd = self.command(args)?;
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = Reap(cmd.spawn().map_err(not_run)?);
        let stdout = child.0.stdout.take().ok_or_else(|| anyhow!("git: no output"))?;
        let stderr = child.0.stderr.take().ok_or_else(|| anyhow!("git: no error output"))?;
        // read as it comes, so what git printed is kept when a child it left holds the pipe open
        let drain = |mut from: Box<dyn Read + Send>| {
            let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let into = got.clone();
            let h = std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n @ 1..) = from.read(&mut buf) {
                    into.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&buf[..n]);
                }
            });
            (h, got)
        };
        let (reader, out) = drain(Box::new(stdout));
        let (errors, err) = drain(Box::new(stderr));
        let deadline = std::time::Instant::now() + limit;
        let status = loop {
            if let Some(s) = child.0.try_wait()? {
                break Some(s);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        drop(child);
        // the group is gone; a child outside it may still hold a pipe, so the wait is short
        let until = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while !(reader.is_finished() && errors.is_finished()) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let take = |b: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>| std::mem::take(&mut *b.lock().unwrap_or_else(|e| e.into_inner()));
        let why = take(&err);
        let why = match status {
            Some(s) => reason(s, &why),
            None => first_line(&why),
        };
        Ok((take(&out), status, why))
    }

    /// Where lines `start` to `end` of the working tree's `path` are in HEAD's copy of the file.
    pub(crate) fn at_head(&self, path: &str, start: i64, end: i64) -> Result<AtHead> {
        let path = crate::paths::plain(path);
        if !self.output(&["cat-file", "-e", &format!("HEAD:./{path}")])?.0 {
            return Ok(AtHead::Absent);
        }
        let patch =
            self.run(&["--literal-pathspecs", "diff-index", "-p", "--no-color", "--no-ext-diff", "--no-textconv", "--relative", "-U0", "HEAD", "--", &path])?;
        let mut shift = 0;
        for (old_count, new_start, new_count) in hunks(&patch) {
            // with no new lines, the hunk takes lines out between new_start and the line after it
            let (touches, before) = if new_count > 0 {
                (new_start <= end && new_start + new_count > start, new_start + new_count <= start)
            } else {
                (new_start >= start && new_start < end, new_start < start)
            };
            if touches {
                return Ok(AtHead::Changed);
            }
            if before {
                shift += new_count - old_count;
            }
        }
        Ok(AtHead::Lines(start - shift, end - shift))
    }

    /// `-c` options that empty every filter driver the configuration names, so reading a file of
    /// the working tree runs no clean or process filter (git-lfs's among them). A file such a
    /// filter would turn back into its committed text is compared as it is on disk.
    fn no_filters(&self) -> Result<Vec<String>> {
        let args = ["config", "-z", "--get-regexp", r"^filter\."];
        let got = self.raw(&args).output().map_err(not_run)?;
        // 1 is no such key; anything else left the filters unknown, and none could be emptied
        if !got.status.success() && got.status.code() != Some(1) {
            return Err(failed(&args, &reason(got.status, &got.stderr)));
        }
        let out = got.stdout;
        let mut names = BTreeSet::new();
        for entry in out.split(|&b| b == 0) {
            let key = String::from_utf8_lossy(entry.split(|&b| b == b'\n').next().unwrap_or_default()).into_owned();
            if let Some((name, _)) = key.strip_prefix("filter.").and_then(|k| k.rsplit_once('.')) {
                names.insert(name.to_string());
            }
        }
        let mut args = Vec::new();
        for name in names {
            // `-c` ends the key at its first `=`: such a driver could not be emptied
            if name.contains('=') {
                bail!("git: the filter driver {} has a name with `=`, which a diff cannot turn off", echo(&name));
            }
            for set in ["clean=", "smudge=", "process=", "required=false"] {
                args.extend(["-c".to_string(), format!("filter.{name}.{set}")]);
            }
        }
        Ok(args)
    }

    /// The contents of these blobs, by id, read by one `git cat-file --batch`.
    pub(crate) fn blobs(&self, ids: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
        let ids: Vec<&String> = ids.iter().filter(|id| !id.is_empty() && id.bytes().any(|b| b != b'0')).collect();
        let mut out = BTreeMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let mut child = self.command(&["cat-file", "--batch"])?.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(not_run)?;
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("git cat-file: no input"))?;
        let asked: String = ids.iter().map(|id| format!("{id}\n")).collect();
        // written beside the read, so a full pipe on either side does not stop both
        let writer = std::thread::spawn(move || stdin.write_all(asked.as_bytes()));
        let mut bytes = Vec::new();
        child.stdout.take().ok_or_else(|| anyhow!("git cat-file: no output"))?.read_to_end(&mut bytes)?;
        let _ = writer.join();
        let mut errors = Vec::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_end(&mut errors);
        }
        // a git that died part way gives no copy of the rest: an empty file would read as a change
        let status = child.wait()?;
        if !status.success() {
            return Err(failed(&["cat-file", "--batch"], &reason(status, &errors)));
        }
        let mut rest = &bytes[..];
        for id in ids {
            let Some(nl) = rest.iter().position(|&b| b == b'\n') else { break };
            let head = String::from_utf8_lossy(&rest[..nl]).into_owned();
            rest = &rest[nl + 1..];
            let mut parts = head.split(' ');
            let (_, kind, size) = (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok()));
            // git says so on its output, exits 0 and writes no error: an absent copy is no empty file
            if matches!(kind, Some("missing" | "ambiguous")) {
                return Err(failed(&["cat-file", "--batch"], &head));
            }
            if let (Some("blob"), Some(n)) = (kind, size) {
                let n = n.min(rest.len());
                out.insert(id.clone(), rest[..n].to_vec());
                rest = rest.get(n + 1..).unwrap_or_default();
            }
        }
        Ok(out)
    }
}

/// The entries of `git diff --raw -z`: `:mode mode blob blob status`, then one path, or two for a rename.
fn raw_entries(raw: &[u8]) -> Result<Vec<Entry>> {
    let mut fields = raw.split(|&b| b == 0).map(|f| String::from_utf8_lossy(f).into_owned());
    let mut out = Vec::new();
    while let Some(head) = fields.next() {
        let Some(head) = head.strip_prefix(':') else { continue };
        let parts: Vec<&str> = head.split(' ').collect();
        let [old_mode, new_mode, old_blob, new_blob, status] = parts[..] else { bail!("git diff --raw: cannot read {}", echo(head)) };
        let path = fields.next().unwrap_or_default();
        let mut e = Entry { old_blob: old_blob.into(), new_blob: new_blob.into(), submodule: old_mode == "160000" || new_mode == "160000", ..Entry::default() };
        match status.as_bytes().first() {
            Some(b'A') => e.new = Some(path),
            Some(b'D') => e.old = Some(path),
            Some(b'R' | b'C') => {
                e.new = fields.next();
                e.old = Some(path);
            }
            Some(b'U') => bail!("{} is unmerged: finish the merge before asking what it changes", echo(&path)),
            _ => {
                e.old = Some(path.clone());
                e.new = Some(path);
            }
        }
        out.push(e);
    }
    Ok(out)
}

/// A commit that changed some lines.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Commit {
    pub id: String,
    /// the author date, `YYYY-MM-DD`
    pub date: String,
    pub subject: String,
    /// a shallow clone's boundary: git gives it every line older history changed
    pub boundary: bool,
}

/// Where some lines of the working tree are in HEAD's copy of their file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtHead {
    /// at these lines, unchanged
    Lines(i64, i64),
    /// some of them changed since HEAD
    Changed,
    /// HEAD has no such file
    Absent,
}

#[derive(Default)]
struct Section {
    binary: bool,
    old: Vec<(i64, i64)>,
    new: Vec<(i64, i64)>,
}

/// The (old count, new start, new count) of each hunk of a one-file `-U0` patch.
fn hunks(patch: &[u8]) -> Vec<(i64, i64, i64)> {
    let side = |t: &str| -> Option<(i64, i64)> {
        let t = t.trim_start_matches(['-', '+']);
        match t.split_once(',') {
            Some((a, n)) => Some((a.parse().ok()?, n.parse().ok()?)),
            None => Some((t.parse().ok()?, 1)),
        }
    };
    patch
        .split(|&b| b == b'\n')
        .filter_map(|line| line.strip_prefix(b"@@ -"))
        .filter_map(|h| {
            let h = String::from_utf8_lossy(h);
            let mut sides = h.split(' ');
            let (_, old_count) = side(sides.next()?)?;
            let (new_start, new_count) = side(sides.next()?)?;
            Some((old_count, new_start, new_count))
        })
        .collect()
}

/// The files of a `-U0` patch, by the path they have after the change (before it, when deleted):
/// their changed lines on each side and whether git found them binary.
fn sections(patch: &[u8]) -> BTreeMap<String, Section> {
    let mut out = BTreeMap::new();
    let mut cur: Option<(String, Section)> = None;
    let mut done = |cur: &mut Option<(String, Section)>| {
        if let Some((p, s)) = cur.take() {
            out.insert(p, s);
        }
    };
    for line in patch.split(|&b| b == b'\n') {
        if let Some(head) = line.strip_prefix(b"diff --git ") {
            done(&mut cur);
            cur = Some((header_path(head).unwrap_or_default(), Section::default()));
            continue;
        }
        let Some((path, s)) = cur.as_mut() else { continue };
        if let Some(p) = line.strip_prefix(b"rename to ") {
            *path = unquoted(p);
        } else if line.starts_with(b"Binary files ") || line == b"GIT binary patch" {
            s.binary = true;
        } else if let Some(h) = line.strip_prefix(b"@@ -") {
            let h = String::from_utf8_lossy(h);
            let mut sides = h.split(' ');
            let range = |t: Option<&str>| -> Option<(i64, i64)> {
                let t = t?.trim_start_matches(['-', '+']);
                let (first, n) = match t.split_once(',') {
                    Some((a, n)) => (a.parse().ok()?, n.parse::<i64>().ok()?),
                    None => (t.parse().ok()?, 1),
                };
                (n > 0).then_some((first, first + n - 1))
            };
            if let Some(r) = range(sides.next()) {
                s.old.push(r);
            }
            if let Some(r) = range(sides.next()) {
                s.new.push(r);
            }
        }
    }
    done(&mut cur);
    out
}

/// The path of a `diff --git a/P b/P` line: both halves name the same file unless it was renamed,
/// and then the `rename to` line names it.
fn header_path(head: &[u8]) -> Option<String> {
    if head.first() == Some(&b'"') {
        let (a, rest) = quoted(head)?;
        let b = rest.strip_prefix(b" ").and_then(|r| quoted(r).map(|q| q.0)).unwrap_or(a);
        return Some(String::from_utf8_lossy(b.strip_prefix(b"b/").unwrap_or(&b[..])).into_owned());
    }
    let n = head.len().checked_sub(5)? / 2;
    let (a, b) = (head.get(..n + 2)?, head.get(n + 3..)?);
    match (a.strip_prefix(b"a/"), b.strip_prefix(b"b/")) {
        (Some(x), Some(y)) if x == y => Some(String::from_utf8_lossy(y).into_owned()),
        _ => Some(String::from_utf8_lossy(b.strip_prefix(b"b/").unwrap_or(b)).into_owned()),
    }
}

/// A path git wrote as it is, or in C quotes when it holds special bytes.
fn unquoted(p: &[u8]) -> String {
    match quoted(p) {
        Some((q, _)) => String::from_utf8_lossy(&q).into_owned(),
        None => String::from_utf8_lossy(p).into_owned(),
    }
}

/// A C-quoted string at the start of `s` (`"a\tb\303\251"`), decoded, and what follows it.
fn quoted(s: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut i = 1;
    if s.first() != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    while i < s.len() {
        match s[i] {
            b'"' => return Some((out, &s[i + 1..])),
            b'\\' => {
                let c = *s.get(i + 1)?;
                i += 2;
                match c {
                    b'0'..=b'7' => {
                        let digits = s.get(i - 1..i + 2)?;
                        out.push(u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok()?);
                        i += 2;
                    }
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b't' => out.push(b'\t'),
                    b'n' => out.push(b'\n'),
                    b'v' => out.push(11),
                    b'f' => out.push(12),
                    b'r' => out.push(b'\r'),
                    c => out.push(c),
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    None
}

fn not_run(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("git is not installed or not on PATH: impact reads the changes with git")
    } else {
        anyhow!("cannot run git: {e}")
    }
}

/// git needed an object this partial clone lacks, and was told not to fetch it.
#[derive(Debug)]
pub(crate) struct LacksObjects(String);

impl std::fmt::Display for LacksObjects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "this partial clone lacks objects that git {} needs: fetch them first (`git fetch`, or `git backfill`)", self.0)
    }
}

impl std::error::Error for LacksObjects {}

/// The error of a git that exited with `why`, the first line of its errors.
fn failed(args: &[&str], why: &str) -> anyhow::Error {
    if why.contains("lazy fetching disabled") {
        LacksObjects(verb(args).to_string()).into()
    } else {
        anyhow!("git {}: {why}", verb(args))
    }
}

/// The git command `args` run, past the options in front of it (`-c` and its setting,
/// `--literal-pathspecs`); `--version` is one.
fn verb<'a>(args: &[&'a str]) -> &'a str {
    args.get(verb_at(args)).copied().unwrap_or_default()
}

/// Where in `args` the git command is.
fn verb_at(args: &[&str]) -> usize {
    let mut i = 0;
    loop {
        match args.get(i).copied() {
            Some("-c") => i += 2,
            Some(a) if a.starts_with("--") && a != "--version" => i += 1,
            _ => return i,
        }
    }
}

/// Why git failed: the first line of its errors, or, when it wrote none, how it ended (a signal,
/// a crash, or its exit status), so no error ends in an empty reason.
fn reason(status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let line = first_line(stderr);
    if status.success() || !line.is_empty() {
        return line;
    }
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
        return format!("ended by signal {signal}{} and gave no reason", if signal == 11 { " (a crash)" } else { "" });
    }
    match status.code() {
        Some(code) => format!("exited with status {code} and gave no reason"),
        None => "ended and gave no reason".to_string(),
    }
}

fn first_line(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    line.strip_prefix("fatal: ").or_else(|| line.strip_prefix("error: ")).unwrap_or(line).to_string()
}

/// A value handed to git with each path in it plain; `GIT_ALTERNATE_OBJECT_DIRECTORIES` is a list.
fn plain_value(name: &str, value: &str) -> String {
    if cfg!(windows) && name == "GIT_ALTERNATE_OBJECT_DIRECTORIES" {
        value.split(';').map(crate::paths::plain).collect::<Vec<_>>().join(";")
    } else {
        crate::paths::plain(value).into_owned()
    }
}

/// The root as a directory to start git in, plain.
fn plain_dir(root: &Path) -> PathBuf {
    let s = root.to_string_lossy();
    let p = crate::paths::plain(&s);
    if p != s {
        PathBuf::from(p.as_ref())
    } else {
        root.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A git that fails and writes nothing gets a reason that says how it ended.
    #[cfg(unix)]
    #[test]
    fn a_git_that_fails_and_says_nothing_gets_a_reason() {
        use std::os::unix::process::ExitStatusExt;
        let signalled = std::process::ExitStatus::from_raw(11);
        assert_eq!(reason(signalled, b""), "ended by signal 11 (a crash) and gave no reason");
        assert_eq!(reason(std::process::ExitStatus::from_raw(9), b"\n"), "ended by signal 9 and gave no reason");
        assert_eq!(reason(std::process::ExitStatus::from_raw(3 << 8), b""), "exited with status 3 and gave no reason");
        assert_eq!(reason(std::process::ExitStatus::from_raw(128 << 8), b"fatal: bad object x\n"), "bad object x");
        assert_eq!(reason(std::process::ExitStatus::from_raw(0), b""), "");
    }

    #[test]
    fn a_patch_gives_each_file_its_changed_lines_by_the_path_it_has_now() {
        let patch = b"diff --git a/a.py b/a.py\nindex 1..2 100644\n--- a/a.py\n+++ b/a.py\n@@ -3 +3,2 @@ def f\n-x\n+y\n+z\n@@ -9,2 +10,0 @@\n-p\n-q\n\
diff --git a/old.py b/new.py\nsimilarity index 90%\nrename from old.py\nrename to new.py\n@@ -0,0 +1 @@\n+diff --git a/x b/x\n\
diff --git \"a/t\\tab.py\" \"b/t\\tab.py\"\n@@ -1 +1 @@\n-a\n+b\n\
diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        let s = sections(patch);
        assert_eq!(s.keys().map(String::as_str).collect::<Vec<_>>(), ["a.py", "img.png", "new.py", "t\tab.py"]);
        assert_eq!((s["a.py"].old.clone(), s["a.py"].new.clone()), (vec![(3, 3), (9, 10)], vec![(3, 4)]));
        assert_eq!(s["new.py"].new, vec![(1, 1)]);
        assert!(s["img.png"].binary && !s["a.py"].binary);
        assert_eq!(s["t\tab.py"].new, vec![(1, 1)]);
    }

    /// git run in `root` with its own configuration only.
    fn git_in(root: &Path, args: &[&str]) {
        let mut c = Command::new("git");
        c.args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(root);
        for name in SIX {
            c.env_remove(name);
        }
        let out = c.envs([("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")]).stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    fn commit_file(root: &Path, rel: &str, text: &str, message: &str) {
        std::fs::write(root.join(rel), text).unwrap();
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-q", "-m", message]);
    }

    #[test]
    fn a_line_log_lists_the_commits_that_changed_those_lines_newest_first_through_a_rename() {
        let root = scratch("line_log");
        repo(&root);
        commit_file(&root, "old.py", "def f(x):\n    return x\n\n\ndef g(y):\n    return y\n", "add f and g");
        commit_file(&root, "old.py", "def f(x):\n    return x + 1\n\n\ndef g(y):\n    return y\n", "edit f");
        git_in(&root, &["mv", "old.py", "new.py"]);
        git_in(&root, &["commit", "-q", "-m", "rename"]);
        commit_file(&root, "new.py", "def f(x):\n    return x + 1\n\n\ndef g(y):\n    return y * 2\n", "edit g");
        commit_file(&root, "new.py", "def f(x):\n    return x + 2\n\n\ndef g(y):\n    return y * 2\n", "edit f again");
        let git = Git::new(&root).unwrap();
        let long = std::time::Duration::from_secs(60);
        let subjects = |start, end, n| git.line_log("new.py", start, end, n, long).unwrap().0.into_iter().map(|c| c.subject).collect::<Vec<_>>();
        assert_eq!(subjects(1, 2, 10), ["edit f again", "edit f", "add f and g"]);
        assert_eq!(subjects(5, 6, 10), ["edit g", "add f and g"]);
        assert_eq!(subjects(1, 2, 1), ["edit f again"]);
        let c = &git.line_log("new.py", 1, 2, 1, long).unwrap().0[0];
        assert!(c.id.len() == 40 && c.id.bytes().all(|b| b.is_ascii_hexdigit()), "{}", c.id);
        assert!(c.date.len() == 10 && c.date.as_bytes()[4] == b'-', "{}", c.date);
        // a path git could read as a range, an option or pathspec magic; Windows names hold no colon
        let odd: &[&str] = if cfg!(windows) { &["-x.py"] } else { &["a:b.py", "-x.py", ":(top)m.py"] };
        for &odd in odd {
            commit_file(&root, odd, "def h():\n    return 1\n", &format!("add {odd}"));
            assert_eq!(git.line_log(odd, 1, 2, 10, long).unwrap().0.into_iter().map(|c| c.subject).collect::<Vec<_>>(), [format!("add {odd}")]);
        }
    }

    #[test]
    fn head_lines_read_their_path_literally() {
        let root = scratch("glob_path");
        repo(&root);
        commit_file(&root, "glob1.py", "def a():\n    return 1\n", "add glob1");
        commit_file(&root, "glob[1].py", "def b():\n    return 2\n", "add glob[1]");
        let git = Git::new(&root).unwrap();
        // as a pattern, glob[1].py would name glob1.py, and its edit would mark the other file
        std::fs::write(root.join("glob1.py"), "def a():\n    return 3\n").unwrap();
        assert_eq!(git.at_head("glob[1].py", 1, 2).unwrap(), AtHead::Lines(1, 2));
        assert_eq!(git.at_head("glob1.py", 1, 2).unwrap(), AtHead::Changed);
    }

    #[test]
    fn head_lines_follow_the_working_tree_through_uncommitted_edits() {
        let root = scratch("at_head");
        repo(&root);
        let text = "import os\n\n\ndef f(x):\n    return x\n\n\ndef g(y):\n    return y\n";
        commit_file(&root, "m.py", text, "add");
        let git = Git::new(&root).unwrap();
        assert_eq!(git.at_head("m.py", 4, 5).unwrap(), AtHead::Lines(4, 5));
        // three lines above f, and one taken out below it
        std::fs::write(root.join("m.py"), format!("# a\n# b\n# c\n{}", text.replace("    return y\n", ""))).unwrap();
        assert_eq!(git.at_head("m.py", 7, 8).unwrap(), AtHead::Lines(4, 5));
        std::fs::write(root.join("m.py"), text.replace("return x", "return x * 3")).unwrap();
        assert_eq!(git.at_head("m.py", 4, 5).unwrap(), AtHead::Changed);
        assert_eq!(git.at_head("m.py", 8, 9).unwrap(), AtHead::Lines(8, 9), "an edit above leaves g's lines");
        std::fs::write(root.join("new.py"), "def n():\n    pass\n").unwrap();
        assert_eq!(git.at_head("new.py", 1, 2).unwrap(), AtHead::Absent);
    }

    #[test]
    fn a_c_quoted_path_is_decoded() {
        assert_eq!(
            quoted(b"\"a\\303\\251\\\"b\" rest").map(|(q, r)| (String::from_utf8(q).unwrap(), r.to_vec())),
            Some(("a\u{e9}\"b".into(), b" rest".to_vec()))
        );
    }

    const SIX: [&str; 6] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR"];

    /// A new directory for a test, as a plain path, which git reads on every platform: every path a
    /// test hands git or puts in a variable comes from here.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scrutate_git_{name}_{}", std::process::id()));
        crate::claim_test_tree(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        plain_dir(&std::fs::canonicalize(dir).unwrap())
    }

    /// An object git does not have is an error naming it, where `cat-file --batch` says `missing`
    /// on its output, exits 0 and writes no error.
    #[test]
    fn a_base_copy_git_does_not_have_is_an_error_naming_it() {
        let root = scratch("missing_blob");
        repo(&root);
        let git = Git::new(&root).unwrap();
        let absent = "1234567890abcdef1234567890abcdef12345678".to_string();
        let e = git.blobs(&BTreeSet::from([absent.clone()])).unwrap_err().to_string();
        assert_eq!(e, format!("git cat-file: {absent} missing"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A repository with one empty commit, made with git's own configuration only; its HEAD commit.
    fn repo(root: &Path) -> String {
        let git = |args: &[&str]| {
            let mut c = Command::new("git");
            c.args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(root);
            for name in SIX {
                c.env_remove(name);
            }
            let out = c.envs([("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")]).stdin(Stdio::null()).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&["rev-parse", "HEAD"])
    }

    fn env(pairs: &[(&str, &Path)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string_lossy().into_owned())).collect()
    }

    #[test]
    fn a_git_dir_handed_in_is_the_repository_git_reads() {
        let (store, work) = (scratch("store"), scratch("work"));
        let head = repo(&store);
        let git = Git::with_env(&work, &env(&[("GIT_DIR", &store.join(".git")), ("GIT_WORK_TREE", &work)])).unwrap();
        assert_eq!(git.commit("HEAD").unwrap(), head);
        // canonical paths, verbatim on Windows, as a caller may hand them: Git makes them plain
        let (store, work) = (std::fs::canonicalize(&store).unwrap(), std::fs::canonicalize(&work).unwrap());
        let git = Git::with_env(&work, &env(&[("GIT_DIR", &store.join(".git")), ("GIT_WORK_TREE", &work)])).unwrap();
        assert_eq!(git.commit("HEAD").unwrap(), head);
    }

    #[test]
    fn git_gets_the_six_handed_in_and_none_of_this_process_s_own() {
        let root = scratch("envs");
        repo(&root);
        let dir = root.join(".git");
        let git = Git::with_env(&root, &env(&[("GIT_DIR", &dir)])).unwrap();
        let got: BTreeMap<String, Option<String>> = git
            .command(&["rev-parse"])
            .unwrap()
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        let own = std::env::vars().map(|(n, _)| n).filter(|n| n.to_ascii_uppercase().starts_with("GIT_"));
        let mut want: BTreeMap<String, Option<String>> = SIX.iter().map(|n| n.to_string()).chain(own).map(|n| (n, None)).collect();
        want.insert("GIT_DIR".into(), Some(dir.to_string_lossy().into_owned()));
        want.insert("GIT_NO_LAZY_FETCH".into(), Some("1".into()));
        want.extend(PATHSPEC_ENV.map(|n| (n.to_string(), None)));
        assert_eq!(got, want);
        let own: BTreeMap<String, Option<String>> = Git::new(&root)
            .unwrap()
            .command(&["rev-parse"])
            .unwrap()
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        let mut want: BTreeMap<String, Option<String>> = own_env().into_iter().map(|(n, v)| (n, Some(v))).collect();
        want.insert("GIT_NO_LAZY_FETCH".into(), Some("1".into()));
        want.extend(PATHSPEC_ENV.map(|n| (n.to_string(), None)));
        assert_eq!(own, want);
    }

    #[test]
    fn git_runs_only_commands_that_read() {
        let root = scratch("reads");
        repo(&root);
        let git = Git::new(&root).unwrap();
        for args in [&["diff", "HEAD"][..], &["status"], &["-c", "a.b=c", "diff"], &["--literal-pathspecs", "add", "x"], &["commit"]] {
            let e = git.command(args).err().unwrap_or_else(|| panic!("{args:?} ran")).to_string();
            assert!(e.contains("is no command scrutate runs"), "{e}");
        }
        for args in [&["--version"][..], &["--literal-pathspecs", "log", "-1"], &["diff-index", "HEAD"], &["config", "--list"]] {
            assert!(git.command(args).is_ok(), "{args:?}");
        }
    }

    #[test]
    fn none_of_this_process_s_git_variables_reach_git_through_with_env() {
        const NAME: &str = "git::tests::none_of_this_process_s_git_variables_reach_git_through_with_env";
        // a variable set in this process reaches every test in it, so the check runs alone in a process of its own
        let Some(root) = std::env::var_os("SCRUTATE_TEST_GIT_ROOT").map(PathBuf::from) else {
            let root = scratch("own_env");
            let head = repo(&root);
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--test-threads=1"])
                .env("SCRUTATE_TEST_GIT_ROOT", &root)
                .env("SCRUTATE_TEST_GIT_HEAD", &head)
                // git refuses to start on this, and writes a trace of every run to that file
                .env("GIT_CONFIG_PARAMETERS", "'unterminated")
                .env("GIT_TRACE", scratch("own_env_trace").join("trace"))
                .output()
                .unwrap();
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success() && said.contains("1 passed"), "{said}{}", String::from_utf8_lossy(&out.stderr));
            return;
        };
        let trace = PathBuf::from(std::env::var_os("GIT_TRACE").unwrap());
        // with this process's variables git stops, after it began its trace
        assert!(Git::new(&root).is_err());
        assert!(trace.exists());
        std::fs::remove_file(&trace).unwrap();
        let git = Git::with_env(&root, &[]).unwrap();
        assert_eq!(git.commit("HEAD").unwrap(), std::env::var("SCRUTATE_TEST_GIT_HEAD").unwrap());
        assert!(!trace.exists());
    }

    #[test]
    fn a_relative_value_is_made_absolute_where_git_reads_it_from_the_start_directory() {
        let (cwd, abs) = if cfg!(windows) { ("C:\\a\\b", "C:\\abs\\tree") } else { ("/a/b", "/abs/tree") };
        let joined = |rel: &str| if cfg!(windows) { format!("C:\\a\\b\\{rel}") } else { format!("/a/b/{rel}") };
        let vars = [
            ("GIT_DIR", "../x.git"),
            ("GIT_WORK_TREE", abs),
            ("GIT_OBJECT_DIRECTORY", "objs"),
            ("GIT_COMMON_DIR", ""),
            ("GIT_INDEX_FILE", "idx"),
            ("GIT_ALTERNATE_OBJECT_DIRECTORIES", "o1"),
            ("PATH", "bin"),
            ("GIT_EDITOR", "vi"),
        ];
        let got = absolute_env(Path::new(cwd), vars.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let want: Vec<(String, String)> = [
            ("GIT_DIR", joined("../x.git")),
            ("GIT_WORK_TREE", abs.to_string()),
            ("GIT_OBJECT_DIRECTORY", joined("objs")),
            // empty is unset to git
            ("GIT_COMMON_DIR", String::new()),
            // git reads these two at the top of the work tree, from wherever it starts
            ("GIT_INDEX_FILE", "idx".to_string()),
            ("GIT_ALTERNATE_OBJECT_DIRECTORIES", "o1".to_string()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn from_an_index_root_below_the_top_git_reads_what_it_would_from_the_user_s_directory() {
        let top = scratch("below_top");
        repo(&top);
        std::fs::create_dir_all(top.join("root/deep")).unwrap();
        std::fs::write(top.join("root/b"), "b\n").unwrap();
        // an index of its own at the top holds root/b staged, as `GIT_INDEX_FILE=idx git add` there leaves it
        let out = Command::new("git")
            .args(["read-tree", "HEAD"])
            .current_dir(&top)
            .env("GIT_INDEX_FILE", "idx")
            .envs([("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")])
            .status()
            .unwrap();
        assert!(out.success());
        assert!(Command::new("git").args(["add", "root/b"]).current_dir(&top).env("GIT_INDEX_FILE", "idx").status().unwrap().success());
        // the user is in root/deep, the index root is root, and both name the same work tree
        let user = [("GIT_DIR", "../../.git"), ("GIT_WORK_TREE", "../.."), ("GIT_INDEX_FILE", "idx")].map(|(k, v)| (k.to_string(), v.to_string()));
        // git's own answer, started where the user is
        let own = Command::new("git")
            .args(["diff", "--cached", "--name-only", "HEAD"])
            .current_dir(top.join("root/deep"))
            .envs(user.clone())
            .envs([("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&own.stdout), "root/b\n");
        let git = Git::overriding(&top.join("root"), absolute_env(&top.join("root/deep"), user)).unwrap();
        let staged: Vec<(Option<String>, Option<String>)> =
            git.diff(&git.commit("HEAD").unwrap(), &New::Staged).unwrap().into_iter().map(|e| (e.old, e.new)).collect();
        assert_eq!(staged, [(None, Some("b".to_string()))]);
    }

    #[test]
    fn a_verbatim_windows_path_reaches_git_in_its_plain_form() {
        for (given, on_windows) in [(r"\\?\C:\x\y", r"C:\x\y"), (r"\\?\UNC\host\share\x", r"\\host\share\x"), (r"C:\x", r"C:\x"), ("/a/b", "/a/b")] {
            assert_eq!(crate::paths::plain(given), if cfg!(windows) { on_windows } else { given });
        }
        let list = r"\\?\C:\a;\\?\UNC\h\s\b;c";
        assert_eq!(plain_value("GIT_ALTERNATE_OBJECT_DIRECTORIES", list), if cfg!(windows) { r"C:\a;\\h\s\b;c" } else { list });
        assert_eq!(plain_value("GIT_DIR", r"\\?\C:\r\.git"), if cfg!(windows) { r"C:\r\.git" } else { r"\\?\C:\r\.git" });
    }

    #[test]
    fn any_other_variable_is_refused_before_git_runs() {
        // no such directory: had git run, it could not have started there
        let nowhere = std::env::temp_dir().join(format!("scrutate_git_nowhere_{}", std::process::id()));
        for name in ["GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT", "GIT_SSH_COMMAND", "PATH", "git_dir"] {
            let err = Git::with_env(&nowhere, &[(name.to_string(), "x".to_string())]).err().unwrap_or_else(|| panic!("{name} was taken")).to_string();
            assert!(err.starts_with(&format!("{name} cannot be handed to git")), "{name}: {err}");
        }
    }
}
