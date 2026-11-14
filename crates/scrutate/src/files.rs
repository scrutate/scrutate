//! Which files belong to the repository, for each index. Engine A's default walk (which the text
//! index reuses) is `instant::build_walker`; the code index's set is `list_files`, walked by
//! `CodeWalk`; an Engine A search of the repository reads that walk's files
//! (`repository_files`); `Membership` asks that walk about one path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::index::Stat;
use crate::parse::is_indexable;

/// Where the indexes live under the repository root; no walk enters it.
pub const INDEX_DIR: &str = ".scrutate";

/// An error unless `root` is an existing directory.
pub fn root_dir(root: &Path) -> anyhow::Result<()> {
    if !root.is_dir() {
        anyhow::bail!("{}: {}", root.display(), if root.exists() { "not a directory" } else { "no such directory" });
    }
    Ok(())
}

/// `root/.scrutate`, once `root` is an existing directory and nothing the indexes write into is
/// a symbolic link (which would put the index, and its deletions, outside the repository).
pub fn index_dir(root: &Path) -> anyhow::Result<PathBuf> {
    root_dir(root)?;
    let dir = root.join(INDEX_DIR);
    let refuse = |p: &Path| anyhow::bail!("{} is a symbolic link; the index is only written inside the repository (remove it)", p.display());
    if std::fs::symlink_metadata(&dir).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return refuse(&dir);
    }
    // every name the index writes is three levels deep at most (seg/s000001/ids.npy); entry types
    // come from the directory listing, so this costs no stat per file
    let mut todo = vec![(dir.clone(), 0)];
    while let Some((d, depth)) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                return refuse(&e.path());
            }
            if ft.is_dir() && depth < 2 {
                todo.push((e.path(), depth + 1));
            }
        }
    }
    Ok(dir)
}

/// `index_dir`, made if missing, with its `.gitignore`. On Unix only its owner may enter it, since
/// it holds the text of files that may be private to the owner; an existing one is tightened the
/// same way (left as it is when this user cannot change it).
pub fn make_index_dir(root: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    let dir = index_dir(root)?;
    #[cfg(unix)]
    let made = std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(&dir);
    #[cfg(not(unix))]
    let made = std::fs::create_dir(&dir);
    match made {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e).with_context(|| format!("create {}", dir.display())),
        _ => {}
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && m.permissions().mode() & 0o777 != 0o700) {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    crate::index::write_gitignore(&dir).with_context(|| format!("create {}", dir.join(".gitignore").display()))?;
    Ok(dir)
}

/// The most files an index may hold, and the most bytes of the files it reads
/// (`SCRUTATE_INDEX_MAX_FILES`, `SCRUTATE_INDEX_MAX_BYTES`, from `[index]` of the configuration
/// file); no limit when unset or empty.
#[derive(Clone, Copy, Debug, Default)]
pub struct Quota {
    pub files: Option<u64>,
    pub bytes: Option<u64>,
}

impl Quota {
    pub fn from_env() -> anyhow::Result<Quota> {
        let get = |name: &str, parse: fn(&str) -> anyhow::Result<u64>| -> anyhow::Result<Option<u64>> {
            match std::env::var(name) {
                Ok(v) if !v.trim().is_empty() => parse(&v).map(Some).map_err(|e| anyhow::anyhow!("{name}: {e}")),
                _ => Ok(None),
            }
        };
        let count = |v: &str| v.trim().parse::<u64>().map_err(|_| anyhow::anyhow!("bad number {v:?}"));
        Ok(Quota { files: get("SCRUTATE_INDEX_MAX_FILES", count)?, bytes: get("SCRUTATE_INDEX_MAX_BYTES", crate::cli::parse_size)? })
    }

    pub fn is_set(&self) -> bool {
        self.files.is_some() || self.bytes.is_some()
    }

    /// An error naming the setting when `index` would hold more than the quota allows.
    pub fn check(&self, index: &str, files: u64, bytes: u64) -> anyhow::Result<()> {
        let over = |what: &str, n: u64, max: Option<u64>, key: &str, var: &str| match max {
            Some(max) if n > max => {
                Err(anyhow::anyhow!("the {index} would hold {n} {what}, more than index.{key} = {max} ({var}); nothing was written, the index is as it was"))
            }
            _ => Ok(()),
        };
        over("files", files, self.files, "max_files", "SCRUTATE_INDEX_MAX_FILES")?;
        over("bytes of files", bytes, self.bytes, "max_bytes", "SCRUTATE_INDEX_MAX_BYTES")
    }
}

/// The main checkout of the linked git work tree whose top is `root`, read from git's own files
/// (no program is run): the `.git` file names `<common dir>/worktrees/<name>`, whose `gitdir`
/// file names this `.git` file back and whose `commondir` names the common directory, which is
/// the `.git` directory of the main checkout. None for anything else: a main checkout, a bare
/// repository, a link that is not confirmed both ways.
pub fn main_checkout(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    let gitdir = PathBuf::from(small_file(&dot)?.strip_prefix("gitdir:")?.trim());
    let gitdir = std::fs::canonicalize(root.join(gitdir)).ok()?;
    let back = PathBuf::from(small_file(&gitdir.join("gitdir"))?);
    if std::fs::canonicalize(gitdir.join(back)).ok()? != std::fs::canonicalize(&dot).ok()? {
        return None;
    }
    let common = std::fs::canonicalize(gitdir.join(small_file(&gitdir.join("commondir"))?)).ok()?;
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" || worktrees.parent()? != common || common.file_name()? != ".git" {
        return None;
    }
    let main = common.parent()?.to_path_buf();
    let is_dir = std::fs::symlink_metadata(main.join(".git")).ok()?.is_dir();
    (is_dir && main != std::fs::canonicalize(root).ok()?).then_some(main)
}

/// The bytes of `path` when it is a regular file of at most `max` bytes, not reached through a
/// symbolic link: a cache file planted in a repository's `.scrutate/` cannot make a read endless.
pub fn read_regular(path: &Path, max: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    if !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let f = std::fs::File::open(path).ok()?;
    let md = f.metadata().ok()?;
    if !md.is_file() || md.len() > max {
        return None;
    }
    let mut out = Vec::with_capacity(md.len() as usize);
    f.take(max + 1).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= max).then_some(out)
}

/// The first line of a regular file of at most 4 KiB, trimmed.
fn small_file(path: &Path) -> Option<String> {
    let md = std::fs::symlink_metadata(path).ok()?;
    if !md.is_file() || md.len() > 4096 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.lines().next()?.trim().to_string())
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    INDEX_DIR,
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".mypy_cache",
    ".pytest_cache",
    "target",
    ".next",
    ".tox",
    ".idea",
    ".vscode",
];

/// Build and environment directories some projects commit; inside a git repository the ignore
/// files decide for them.
const SKIP_UNLESS_GIT: &[&str] = &["build", "dist", "target", "venv"];

fn in_git(root: &Path) -> bool {
    root.ancestors().any(|a| a.join(".git").exists())
}

/// Most names `exclude_dirs` of a repository's `.scrutate.toml` adds; the rest are not read.
const MAX_CONFIG_EXCLUDES: usize = 1000;

/// Directory names the code index of `root` never enters: `SKIP_DIRS` (less `SKIP_UNLESS_GIT`
/// inside a git repository) plus those in `SCRUTATE_INDEX_EXCLUDE_DIRS`, or when it is not set,
/// in `[index] exclude_dirs` of `<root>/.scrutate.toml`. A name with a path separator or a
/// backslash, `.` and `..` name no directory and are dropped.
fn skip_dirs(root: &Path) -> Vec<String> {
    let git = in_git(root);
    let mut skip: Vec<String> = SKIP_DIRS.iter().filter(|d| !(git && SKIP_UNLESS_GIT.contains(d))).map(|s| s.to_string()).collect();
    match std::env::var("SCRUTATE_INDEX_EXCLUDE_DIRS") {
        Ok(v) => skip.extend(split_names(&v).into_iter().filter(|n| is_dir_name(n))),
        Err(_) => skip.extend(config_excludes(root)),
    }
    skip
}

/// A name `exclude_dirs` can use: a backslash is a separator on Windows and an escape in the
/// `!name` glob of `grep_code`.
fn is_dir_name(n: &str) -> bool {
    n != "." && n != ".." && !n.chars().any(|c| std::path::is_separator(c) || c == '\\')
}

/// `[index] exclude_dirs` of the root's own `.scrutate.toml` (`root_config_text`): the setting
/// belongs to the index, so every process that updates it reads the same list, wherever it
/// starts. Names that are no directory name are dropped.
pub(crate) fn config_excludes(root: &Path) -> Vec<String> {
    let Some(text) = root_config_text(root) else { return Vec::new() };
    let Ok(table) = text.parse::<toml::Table>() else { return Vec::new() };
    let names = table.get("index").and_then(|i| i.get("exclude_dirs")).and_then(|v| v.as_array());
    names
        .map(|a| {
            let names = a.iter().filter_map(|v| v.as_str()).map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
            names.take(MAX_CONFIG_EXCLUDES).filter(|n| is_dir_name(n)).collect()
        })
        .unwrap_or_default()
}

/// The text of the root's own `.scrutate.toml`, read as other repository files are: after links
/// are resolved, a regular file inside the root of at most 1,000,000 bytes.
pub(crate) fn root_config_text(root: &Path) -> Option<String> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let real = std::fs::canonicalize(root.join(".scrutate.toml")).ok()?;
    crate::resolve::repo_text(&root, real.strip_prefix(&root).ok()?.to_str()?)
}

/// A comma-separated list of names; `\,` is a comma inside a name.
pub fn split_names(v: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&',') => out.last_mut().unwrap().push(chars.next().unwrap()),
            ',' => out.push(String::new()),
            _ => out.last_mut().unwrap().push(c),
        }
    }
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// The directory names the code index of `root` never enters.
pub fn skipped_dirs(root: &Path) -> Vec<String> {
    skip_dirs(root)
}

/// The files an Engine A search of the repository covers, walked by the code index's rules: the
/// ignore files, hidden files included, none of the skipped directories.
pub fn repository_files(root: &Path) -> crate::instant::Files {
    // a name, never a pattern: `*` in a repository's exclude_dirs must not hide every file
    let globs = skip_dirs(root).into_iter().map(|d| {
        d.chars().fold("!".to_string(), |mut g, c| {
            if "*?[]{}".contains(c) {
                g.extend(['[', c, ']']);
            } else {
                g.push(c);
            }
            g
        })
    });
    crate::instant::Files { hidden: true, globs: globs.collect(), ..crate::instant::Files::walk(vec![root.to_path_buf()]) }
}

/// Relative paths ('/' separated, sorted) of the files worth indexing: the same rules as ripgrep's
/// file listing (.gitignore, hidden files) plus the usual build/vendor directories.
pub fn list_files(root: &Path) -> Vec<String> {
    let walk = CodeWalk::new(root);
    let mut out: Vec<String> = walk.builder(root).build().flatten().filter_map(|e| walk.keeps(&e)).collect();
    out.sort();
    out
}

/// A file one look at the tree found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// from the root, '/' separated
    pub path: String,
    /// the file's `stat`, taken by the look when a store needs it (None when none does, or it failed)
    pub stat: Option<Stat>,
    /// a search of the repository covers it (`repository_files`)
    pub repo: bool,
    /// the code index holds it (`list_files`)
    pub code: bool,
    /// the text index's walk (Engine A's default walk) lists it
    pub text: bool,
    /// the name is not UTF-8, so `path` names it only in part
    pub lossy: bool,
}

/// The files of the tree at `root`, from one walk and one `stat` per file a store needs, sorted by
/// path: the files a search of the repository covers and the code index's set, and with `text`
/// the text index's set too. The walk enters every directory one of those walks enters.
pub fn look(root: &Path, text: bool) -> anyhow::Result<Vec<Found>> {
    let walk = CodeWalk::new(root);
    let rules = match text {
        true => Some(Arc::new(std::sync::Mutex::new(crate::instant::default_rules(root)?))),
        false => None,
    };
    let left_out = |rules: &std::sync::Mutex<ignore::IncrementalIgnore>, rel: &Path, is_dir: bool| {
        maybe_hidden(rel) && rules.lock().unwrap_or_else(|p| p.into_inner()).matched(rel, is_dir).is_ignore()
    };
    let (skip, base, held) = (walk.skip.clone(), root.to_path_buf(), rules.clone());
    let mut wb = CodeWalk::rules(root);
    // a directory the code index skips is still entered when the text index's walk enters it
    wb.filter_entry(move |e| {
        let skipped = e.file_type().is_some_and(|t| t.is_dir()) && skip.iter().any(|s| *s == *e.file_name().to_string_lossy());
        !skipped || held.as_ref().is_some_and(|r| !e.path().strip_prefix(&base).is_ok_and(|rel| left_out(r, rel, true)))
    });
    let (mut out, mut listed) = (Vec::new(), crate::index::Listed::default());
    for e in wb.build().flatten() {
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = e.path().strip_prefix(root) else { continue };
        let path = slashed(rel);
        let repo = !path.split('/').any(|n| walk.skip.iter().any(|s| s == n));
        let code = repo && e.file_name() != ".git" && is_indexable(&path);
        let text = rules.as_ref().is_some_and(|r| !left_out(r, rel, false));
        if !(repo || text) {
            continue;
        }
        let stat = if code || text { e.metadata().ok().map(|m| listed.stat(e.path(), &m)) } else { None };
        out.push(Found { lossy: rel.to_str().is_none(), path, stat, repo, code, text });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Whether the default walk can leave out a path the code index's walk lists: a name on the way
/// starts with a dot (on Windows, any name may carry the hidden attribute).
fn maybe_hidden(rel: &Path) -> bool {
    cfg!(windows) || rel.components().any(|c| c.as_os_str().as_encoded_bytes().starts_with(b"."))
}

/// The code index's walk of a root. Started at a directory the root's walk enters, it lists that
/// directory as the root's walk does: the ignore files of every directory above it apply.
pub struct CodeWalk {
    root: PathBuf,
    skip: Arc<Vec<String>>,
}

impl CodeWalk {
    pub fn new(root: &Path) -> CodeWalk {
        CodeWalk { root: root.to_path_buf(), skip: Arc::new(skip_dirs(root)) }
    }

    pub fn builder(&self, start: &Path) -> ignore::WalkBuilder {
        let skip = self.skip.clone();
        let mut wb = Self::rules(start);
        wb.filter_entry(move |e| !(e.file_type().is_some_and(|t| t.is_dir()) && skip.iter().any(|s| *s == *e.file_name().to_string_lossy())));
        wb
    }

    /// The walk's ignore rules, without the skipped directories.
    fn rules(start: &Path) -> ignore::WalkBuilder {
        // hidden files are indexed (.github/workflows/*.yml, .eslintrc.js, .env.example are code or
        // config people search for); SKIP_DIRS prunes VCS internals and caches
        let mut wb = ignore::WalkBuilder::new(start);
        wb.hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .ignore(true)
            .parents(true)
            .add_custom_ignore_filename(crate::instant::RGIGNORE)
            .follow_links(false);
        wb
    }

    /// The path from the root of a file the walk yielded, when the code index holds that file.
    pub fn keeps(&self, e: &ignore::DirEntry) -> Option<String> {
        // a `.git` *file* (worktree or submodule pointer) is VCS metadata, not source
        if !e.file_type().is_some_and(|t| t.is_file()) || e.file_name() == ".git" {
            return None;
        }
        let rel = slashed(e.path().strip_prefix(&self.root).ok()?);
        self.indexable(&rel).then_some(rel)
    }

    fn indexable(&self, rel: &str) -> bool {
        is_indexable(rel) && !rel.split('/').any(|n| self.skip.iter().any(|s| s == n))
    }
}

pub(crate) fn slashed(rel: &Path) -> String {
    rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// Whether the code index's walk lists one path: each directory from the root down to it is listed
/// with the walk's own rules. A path that does not exist is judged by its directory.
pub struct Membership {
    walk: CodeWalk,
}

impl Membership {
    pub fn new(root: &Path) -> Membership {
        Membership { walk: CodeWalk::new(root) }
    }

    pub fn may_contain(&mut self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.walk.root) else { return false };
        let names: Vec<_> = rel.components().map(|c| c.as_os_str().to_os_string()).collect();
        let mut dir = self.walk.root.clone();
        for (i, name) in names.iter().enumerate() {
            let next = dir.join(name);
            let last = i + 1 == names.len();
            if last && std::fs::symlink_metadata(&next).is_err() {
                return self.walk.indexable(&slashed(rel));
            }
            let Some(e) = self.walk.builder(&dir).max_depth(Some(1)).build().flatten().find(|e| e.depth() == 1 && e.path() == next) else {
                return false;
            };
            if last {
                return e.file_type().is_some_and(|t| t.is_dir()) || self.walk.keeps(&e).is_some();
            }
            dir = next;
        }
        false
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_cache_file_is_read_only_as_a_regular_file_within_its_limit() {
        let d = std::env::temp_dir().join(format!("scrutate_read_regular_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("small"), b"12345").unwrap();
        assert_eq!(read_regular(&d.join("small"), 5).as_deref(), Some(&b"12345"[..]));
        assert_eq!(read_regular(&d.join("small"), 4), None, "over the limit");
        assert_eq!(read_regular(&d, 100), None, "a directory");
        assert_eq!(read_regular(&d.join("missing"), 100), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("small"), d.join("link")).unwrap();
            assert_eq!(read_regular(&d.join("link"), 100), None, "a symbolic link");
            std::os::unix::fs::symlink("/dev/zero", d.join("zero")).unwrap();
            assert_eq!(read_regular(&d.join("zero"), 100), None, "a device behind a link");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
    use super::*;

    /// A tree under the temporary directory; a name ending in `/` is a directory.
    fn tree(name: &str, entries: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_files_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, text) in entries {
            let p = root.join(rel);
            if rel.ends_with('/') {
                std::fs::create_dir_all(&p).unwrap();
            } else {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, text).unwrap();
            }
        }
        std::fs::canonicalize(&root).unwrap()
    }

    fn every_file(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                every_file(&e.path(), out);
            } else {
                out.push(e.path());
            }
        }
    }

    /// For the index rooted at `root`: `Membership` answers every file of the tree as `list_files`
    /// lists it, and a walk started at each directory the root's walk enters lists that directory's
    /// files as the root's walk does. -> what `list_files` holds.
    fn agrees(root: &Path) -> Vec<String> {
        let walked = list_files(root);
        let mut m = Membership::new(root);
        let mut files = Vec::new();
        every_file(root, &mut files);
        for f in &files {
            let rel = slashed(f.strip_prefix(root).unwrap());
            assert_eq!(m.may_contain(f), walked.contains(&rel), "membership of {rel} ({walked:?})");
        }
        let walk = CodeWalk::new(root);
        for d in walk.builder(root).build().flatten().filter(|e| e.file_type().is_some_and(|t| t.is_dir())) {
            let mut here: Vec<String> = walk.builder(d.path()).max_depth(Some(1)).build().flatten().filter_map(|e| walk.keeps(&e)).collect();
            here.sort();
            let from_root: Vec<String> = walked.iter().filter(|p| root.join(p).parent() == Some(d.path())).cloned().collect();
            assert_eq!(here, from_root, "the walk started at {}", d.path().display());
        }
        looks_agree(root);
        walked
    }

    /// One look gives each store, path for path and stat for stat, what its own walk gives: the
    /// code index `list_files`, a search of the repository the walk of `repository_files`, the
    /// text index Engine A's default walk; a look without the text index the same first two.
    fn looks_agree(root: &Path) -> usize {
        let both = look(root, true).unwrap();
        let code: Vec<String> = both.iter().filter(|f| f.code).map(|f| f.path.clone()).collect();
        assert_eq!(code, list_files(root), "the code index's files of {}", root.display());
        let stat = |p: &str| std::fs::symlink_metadata(root.join(p)).ok().map(|m| Stat::of(&root.join(p), &m));
        assert!(both.iter().filter(|f| f.code || f.text).all(|f| f.stat == stat(&f.path)), "a stat of each file");
        let text: Vec<(String, u64, i64, i64)> =
            both.iter().filter(|f| f.text).map(|f| f.stat.map_or((f.path.clone(), 0, 0, 0), |s| (f.path.clone(), s.size as u64, s.mtime, s.ctime))).collect();
        assert_eq!(text, crate::textindex::walk(root).unwrap(), "the text index's files of {}", root.display());
        let found = std::sync::Mutex::new(Vec::new());
        crate::instant::build_walker(&[root.to_path_buf()], &repository_files(root)).unwrap().run(|| {
            Box::new(|e| {
                if let Some(e) = e.ok().filter(|e| e.file_type().is_some_and(|t| t.is_file())) {
                    found.lock().unwrap().push(slashed(e.path().strip_prefix(root).unwrap()));
                }
                ignore::WalkState::Continue
            })
        });
        let mut repo = found.into_inner().unwrap();
        repo.sort();
        assert_eq!(both.iter().filter(|f| f.repo).map(|f| f.path.clone()).collect::<Vec<_>>(), repo, "a search's files of {}", root.display());
        let alone = look(root, false).unwrap();
        assert_eq!(alone.iter().filter(|f| f.code).map(|f| &f.path).collect::<Vec<_>>(), code.iter().collect::<Vec<_>>());
        assert_eq!(alone.iter().filter(|f| f.repo).map(|f| f.path.clone()).collect::<Vec<_>>(), repo);
        both.len()
    }

    #[test]
    fn one_look_lists_for_each_store_what_its_own_walk_lists() {
        let root = tree(
            "look",
            &[
                (".git/HEAD", "ref: refs/heads/main\n"),
                (".gitignore", "gen/\n*.log\n!.keep/\n!.env.example\n"),
                (".github/workflows/ci.yml", "x\n"),
                (".keep/a.py", "x\n"),
                (".env.example", "x\n"),
                (".env", "x\n"),
                ("src/a.py", "x\n"),
                ("src/.hidden.py", "x\n"),
                ("src/logo.png", "x\n"),
                ("src/run.log", "x\n"),
                ("gen/out.py", "x\n"),
                ("node_modules/pkg/index.js", "x\n"),
                ("node_modules/.bin/tool", "x\n"),
                ("node_modules/pkg/.cache/c.js", "x\n"),
                ("build/out.py", "x\n"),
                ("sub/target/t.rs", "x\n"),
                ("sub/__pycache__/m.pyc", "x\n"),
                (".scrutate/index.db", "x\n"),
                ("deep/.scrutate", "x\n"),
                ("deep/.scrutate.toml", "x\n"),
                ("deep/node_modules", "x\n"),
                ("nested/.git/HEAD", "ref: refs/heads/main\n"),
                ("nested/.gitignore", "local.py\n"),
                ("nested/local.py", "x\n"),
                ("nested/kept.py", "x\n"),
                ("wt/.git", "gitdir: elsewhere\n"),
                ("wt/a.py", "x\n"),
                ("empty/", ""),
            ],
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("src/a.py"), root.join("src/link.py")).unwrap();
            std::os::unix::fs::symlink(root.join("src"), root.join("linked_dir")).unwrap();
        }
        assert!(looks_agree(&root) > 10);
        let both = look(&root, true).unwrap();
        let has = |p: &str, f: fn(&Found) -> bool| both.iter().any(|x| x.path == p && f(x));
        assert!(has("node_modules/pkg/index.js", |f| f.text && !f.repo), "the text index's walk enters node_modules");
        assert!(has(".github/workflows/ci.yml", |f| f.repo && f.code && !f.text), "the code index takes hidden files");
        assert!(has(".keep/a.py", |f| f.text && f.code), "an ignore file can bring a hidden directory in");
        assert!(has("src/logo.png", |f| f.repo && !f.code && f.stat.is_some()));
        assert!(!both.iter().any(|f| f.path.starts_with(".git/") || f.path.starts_with(".scrutate/") || f.path.starts_with("gen/")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The trees named in `SCRUTATE_LOOK_ROOTS` (separated by `:`), each checked as the fixtures are.
    #[test]
    #[ignore]
    fn one_look_lists_for_each_store_what_its_own_walk_lists_on_real_trees() {
        for r in std::env::var("SCRUTATE_LOOK_ROOTS").unwrap_or_default().split(':').filter(|r| !r.is_empty()) {
            let root = std::fs::canonicalize(r).unwrap();
            eprintln!("{}: {} files agree", root.display(), looks_agree(&root));
        }
    }

    #[test]
    fn an_ignore_file_speaks_before_a_nearer_gitignore_as_in_the_walk() {
        let root = tree(
            "order",
            &[
                (".git/", ""),
                (".ignore", "*.log\nrg.rs\n"),
                (".gitignore", "*.tmp\n"),
                ("sub/.gitignore", "!keep.log\n!keep.tmp\n"),
                (".rgignore", "!rg.rs\n!rg.tmp\n"),
                ("sub/keep.log", "x\n"),
                ("sub/keep.tmp", "x\n"),
                ("sub/a.rs", "x\n"),
                ("sub/other.tmp", "x\n"),
                ("sub/rg.rs", "x\n"),
                ("sub/rg.tmp", "x\n"),
            ],
        );
        let walked = agrees(&root);
        for (name, kept) in
            [("sub/keep.log", false), ("sub/keep.tmp", true), ("sub/a.rs", true), ("sub/other.tmp", false), ("sub/rg.rs", true), ("sub/rg.tmp", true)]
        {
            assert_eq!(walked.iter().any(|p| p == name), kept, "the walk on {name}: {walked:?}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_rules_of_directories_above_the_repository_and_of_nested_repositories_apply_as_in_the_walk() {
        let outer = tree(
            "layouts",
            &[
                // an .ignore above the repository top
                (".ignore", "secret.py\n"),
                ("proj/.git/", ""),
                ("proj/secret.py", "x\n"),
                ("proj/a.py", "x\n"),
                // a root outside git holding a repository
                ("ws/repo1/.git/", ""),
                ("ws/repo1/.gitignore", "gen.py\ncov/\n"),
                ("ws/repo1/gen.py", "x\n"),
                ("ws/repo1/cov/x.py", "x\n"),
                ("ws/repo1/a.py", "x\n"),
                // a repository inside one whose ignore patterns would match its files
                ("outer/.git/", ""),
                ("outer/.gitignore", "*.gen.py\nout/\n"),
                ("outer/x.gen.py", "x\n"),
                ("outer/vendor/lib/.git/", ""),
                ("outer/vendor/lib/x.gen.py", "x\n"),
                ("outer/vendor/lib/out/o.py", "x\n"),
                // git's exclude file
                ("ex/.git/info/exclude", "*.tmp\n/gen/\n"),
                ("ex/src/a.tmp", "x\n"),
                ("ex/src/a.rs", "x\n"),
                ("ex/gen/out.rs", "x\n"),
                ("ex/src/gen/out.rs", "x\n"),
            ],
        );
        assert_eq!(agrees(&outer.join("proj")), ["a.py"]);
        assert_eq!(agrees(&outer.join("ws")), ["repo1/.gitignore", "repo1/a.py"]);
        assert_eq!(agrees(&outer.join("outer")), [".gitignore", "vendor/lib/out/o.py", "vendor/lib/x.gen.py"]);
        assert_eq!(agrees(&outer.join("ex")), ["src/a.rs", "src/gen/out.rs"]);
        std::fs::remove_dir_all(&outer).unwrap();
    }

    #[test]
    fn a_linked_worktree_reads_the_exclude_file_of_its_main_repository() {
        let root = tree(
            "worktree",
            &[
                ("main/.git/info/exclude", "private.py\n"),
                ("main/.git/HEAD", "ref: refs/heads/main\n"),
                ("main/.git/worktrees/wt/commondir", "../..\n"),
                ("main/.git/worktrees/wt/HEAD", "ref: refs/heads/wt\n"),
                ("wt/private.py", "x\n"),
                ("wt/a.py", "x\n"),
            ],
        );
        std::fs::write(root.join("wt/.git"), format!("gitdir: {}\n", root.join("main/.git/worktrees/wt").display())).unwrap();
        assert_eq!(agrees(&root.join("wt")), ["a.py"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn membership_of_paths_that_are_not_files_of_the_tree() {
        let root = tree("paths", &[(".ignore", "gen/\n"), ("src/a.rs", "x\n"), ("gen/b.rs", "x\n"), (".scrutate/x.py", "x\n"), ("node_modules/x/i.js", "x\n")]);
        let mut m = Membership::new(&root);
        assert!(m.may_contain(&root.join("src/new.rs")), "a missing file is judged by its directory");
        assert!(m.may_contain(&root.join("src")));
        assert!(!m.may_contain(&root.join("gen/new.rs")));
        assert!(!m.may_contain(&root.join("src/new.png")));
        assert!(!m.may_contain(&root.join(".scrutate/x.py")));
        assert!(!m.may_contain(&root.join("node_modules/x/i.js")));
        assert!(!m.may_contain(Path::new("/elsewhere/a.rs")));
        assert!(!m.may_contain(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
