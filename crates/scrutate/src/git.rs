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
#[derive(Default, Debug)]
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

/// The `git` program run in the repository. Nothing it runs writes: no optional locks, no file
/// system monitor, no external diff or text conversion. A revision is checked before it is
/// passed, so it never reaches git as an option.
pub(crate) struct Git {
    dir: PathBuf,
    env: Env,
}

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

impl Git {
    /// git in this process's environment, a relative value of `FROM_START` read where it was set.
    pub(crate) fn new(root: &Path) -> Result<Git> {
        Git::overriding(root, own_env())
    }

    fn overriding(root: &Path, set: Vec<(String, String)>) -> Result<Git> {
        Git::checked(Git { dir: plain_dir(root), env: Env::Own(set) }, root)
    }

    /// As `new`, with git given these of `PASSED_ENV` and no GIT_* variable of this process; any
    /// other name is refused before git runs.
    #[cfg_attr(not(test), expect(dead_code, reason = "the session's caller of this comes with impact requests that carry git's variables"))]
    pub(crate) fn with_env(root: &Path, env: &[(String, String)]) -> Result<Git> {
        if let Some((name, _)) = env.iter().find(|(name, _)| !PASSED_ENV.contains(&name.as_str())) {
            bail!("{} cannot be handed to git: only {} can", echo(name), PASSED_ENV.join(", "));
        }
        Git::checked(Git { dir: plain_dir(root), env: Env::Given(env.to_vec()) }, root)
    }

    fn checked(git: Git, root: &Path) -> Result<Git> {
        let out = git.command(&["rev-parse", "--is-inside-work-tree"]).output().map_err(not_run)?;
        if !out.status.success() || out.stdout.trim_ascii() != b"true" {
            let why = first_line(&out.stderr);
            if why.is_empty() || why.contains("not a git repository") {
                bail!("{} is not in a git repository: impact reads the changes with git", root.display());
            }
            bail!("git: {why}");
        }
        Ok(git)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.dir).args(["--no-optional-locks", "-c", "core.fsmonitor=false"]).args(args).stdin(Stdio::null());
        match &self.env {
            Env::Own(set) => {
                c.envs(set.iter().map(|(name, value)| (name, value)));
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
                c.envs(env.iter().map(|(name, value)| (name, value)));
            }
        }
        c
    }

    /// (exited with 0, standard output, standard error)
    fn output(&self, args: &[&str]) -> Result<(bool, Vec<u8>, String)> {
        let out = self.command(args).output().map_err(not_run)?;
        Ok((out.status.success(), out.stdout, first_line(&out.stderr)))
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        match self.output(args)? {
            (true, out, _) => Ok(out),
            (false, _, why) => bail!("git {}: {why}", args[0]),
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

    pub(crate) fn commit(&self, rev: &str) -> Result<String> {
        // a revision is one argument and never an option, so `--output=x` is refused here
        if rev.is_empty() || rev.starts_with('-') {
            bail!("not a revision: {}", echo(rev));
        }
        match self.output(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])? {
            (true, out, _) => Ok(String::from_utf8_lossy(out.trim_ascii()).into_owned()),
            _ if rev == "HEAD" => bail!("HEAD names no commit yet: commit once, or pass a base revision"),
            _ => bail!("not a revision of this repository: {}", echo(rev)),
        }
    }

    /// The files that differ from `base`, with their changed lines, and the untracked files.
    pub(crate) fn diff(&self, base: &str, new: &New) -> Result<Vec<Entry>> {
        let common = ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--relative", "-M", "--src-prefix=a/", "--dst-prefix=b/"];
        let with = |more: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = common.iter().chain(more).map(|s| s.to_string()).collect();
            if matches!(new, New::Staged) {
                v.push("--cached".into());
            }
            v.push(base.to_string());
            if let New::Commit(head) = new {
                v.push(head.clone());
            }
            v.push("--".into());
            v
        };
        let raw = self.run(&with(&["--raw", "-z", "--no-abbrev"]).iter().map(String::as_str).collect::<Vec<_>>())?;
        let patch = self.run(&with(&["-U0", "--inter-hunk-context=0"]).iter().map(String::as_str).collect::<Vec<_>>())?;
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
        if matches!(new, New::Tree) {
            for p in self.run(&["ls-files", "--others", "--exclude-standard", "-z"])?.split(|&b| b == 0).filter(|p| !p.is_empty()) {
                entries.push(Entry { new: Some(String::from_utf8_lossy(p).into_owned()), untracked: true, ..Entry::default() });
            }
        }
        Ok(entries)
    }

    /// The contents of these blobs, by id, read by one `git cat-file --batch`.
    pub(crate) fn blobs(&self, ids: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
        let ids: Vec<&String> = ids.iter().filter(|id| !id.is_empty() && id.bytes().any(|b| b != b'0')).collect();
        let mut out = BTreeMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let mut child = self.command(&["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(not_run)?;
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("git cat-file: no input"))?;
        let asked: String = ids.iter().map(|id| format!("{id}\n")).collect();
        // written beside the read, so a full pipe on either side does not stop both
        let writer = std::thread::spawn(move || stdin.write_all(asked.as_bytes()));
        let mut bytes = Vec::new();
        child.stdout.take().ok_or_else(|| anyhow!("git cat-file: no output"))?.read_to_end(&mut bytes)?;
        let _ = writer.join();
        child.wait()?;
        let mut rest = &bytes[..];
        for id in ids {
            let Some(nl) = rest.iter().position(|&b| b == b'\n') else { break };
            let head = String::from_utf8_lossy(&rest[..nl]).into_owned();
            rest = &rest[nl + 1..];
            let mut parts = head.split(' ');
            let (_, kind, size) = (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok()));
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

#[derive(Default)]
struct Section {
    binary: bool,
    old: Vec<(i64, i64)>,
    new: Vec<(i64, i64)>,
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

fn first_line(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    line.strip_prefix("fatal: ").or_else(|| line.strip_prefix("error: ")).unwrap_or(line).to_string()
}

/// The root as a directory to start git in: on Windows a verbatim drive path (`\\?\C:\x`, what
/// `canonicalize` gives) in its plain form.
fn plain_dir(root: &Path) -> PathBuf {
    let s = root.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if cfg!(windows) && rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn a_c_quoted_path_is_decoded() {
        assert_eq!(
            quoted(b"\"a\\303\\251\\\"b\" rest").map(|(q, r)| (String::from_utf8(q).unwrap(), r.to_vec())),
            Some(("a\u{e9}\"b".into(), b" rest".to_vec()))
        );
    }

    const SIX: [&str; 6] = ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR"];

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scrutate_git_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
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
    }

    #[test]
    fn git_gets_the_six_handed_in_and_none_of_this_process_s_own() {
        let root = scratch("envs");
        repo(&root);
        let dir = root.join(".git");
        let git = Git::with_env(&root, &env(&[("GIT_DIR", &dir)])).unwrap();
        let got: BTreeMap<String, Option<String>> =
            git.command(&["status"]).get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
        let own = std::env::vars().map(|(n, _)| n).filter(|n| n.to_ascii_uppercase().starts_with("GIT_"));
        let mut want: BTreeMap<String, Option<String>> = SIX.iter().map(|n| n.to_string()).chain(own).map(|n| (n, None)).collect();
        want.insert("GIT_DIR".into(), Some(dir.to_string_lossy().into_owned()));
        assert_eq!(got, want);
        let own: BTreeMap<String, Option<String>> = Git::new(&root)
            .unwrap()
            .command(&["status"])
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        assert_eq!(own, own_env().into_iter().map(|(n, v)| (n, Some(v))).collect());
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
    fn any_other_variable_is_refused_before_git_runs() {
        // no such directory: had git run, it could not have started there
        let nowhere = std::env::temp_dir().join(format!("scrutate_git_nowhere_{}", std::process::id()));
        for name in ["GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT", "GIT_SSH_COMMAND", "PATH", "git_dir"] {
            let err = Git::with_env(&nowhere, &[(name.to_string(), "x".to_string())]).err().unwrap_or_else(|| panic!("{name} was taken")).to_string();
            assert!(err.starts_with(&format!("{name} cannot be handed to git")), "{name}: {err}");
        }
    }
}
