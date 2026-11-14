//! `scrutate watch` and `serve --watch`: keep the index fresh from filesystem events. Bursts of
//! events are debounced, and each burst runs an incremental update (which takes the indexes' write
//! locks, so a crash mid-update is harmless). The indexes' own walks decide what a burst holds: a
//! burst that names files lists their directories with those walks and updates the files listed.
//! One that touches a directory or a rule file, loses events, or comes once the last walk is old
//! walks the tree and updates everything. Directories and rule files that have no watch are
//! checked on a timer, and what changed in them joins the next burst as events would.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind};
use notify::{Event, EventKind};
#[cfg(not(target_os = "macos"))]
use notify::{RecursiveMode, Watcher};

use crate::files::{CodeWalk, INDEX_DIR};
use crate::stores::{Changes, Named};

/// A burst of events ends after this much quiet.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
/// A burst that never goes quiet is still flushed this often.
const MAX_WAIT: Duration = Duration::from_secs(2);
/// A burst walks the tree once the last walk is this old: no event reports a directory made while
/// its parent's watch was being set, or events the OS or notify dropped without saying so.
const WALK_EVERY: Duration = Duration::from_secs(30);
/// The first pause before a failed update is tried again, and the longest.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MOST: Duration = Duration::from_secs(30);

fn has_text_index(root: &Path) -> bool {
    crate::textindex::exists(root)
}

/// Under the root, and not the root itself, the index directory or `.git`.
fn inside(root: &Path, p: &Path) -> bool {
    p.strip_prefix(root).ok().and_then(|r| r.components().next()).is_some_and(|c| c.as_os_str() != INDEX_DIR && c.as_os_str() != ".git")
}

/// git's `info/exclude` for the repository holding `root`. A linked worktree's `.git` is a file
/// naming its git directory, whose `commondir` names the main repository's, as the walk reads it.
fn git_exclude(root: &Path) -> Option<PathBuf> {
    let top = root.ancestors().find(|a| a.join(".git").exists())?;
    let git = top.join(".git");
    let dir = if git.is_dir() {
        git
    } else {
        let text = std::fs::read_to_string(&git).ok()?;
        let gitdir = top.join(text.trim().strip_prefix("gitdir:")?.trim());
        match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(c) => gitdir.join(c.trim()),
            Err(_) => gitdir,
        }
    };
    Some(std::fs::canonicalize(dir).ok()?.join("info").join("exclude"))
}

/// The rule files the walks read that are not ignore files inside the tree: those of each
/// directory above the root (`.gitignore` only up to the repository top), git's exclude file of
/// the repository and of each one in `dirs`, and git's global excludes file.
fn outer_rules<'a>(root: &Path, dirs: impl Iterator<Item = &'a PathBuf>) -> HashSet<PathBuf> {
    let top = root.ancestors().find(|a| a.join(".git").exists());
    let mut rules = HashSet::new();
    for a in root.ancestors().skip(1) {
        rules.extend([a.join(".ignore"), a.join(crate::instant::RGIGNORE)]);
        if top.is_some_and(|t| a.starts_with(t)) {
            rules.insert(a.join(".gitignore"));
        }
    }
    rules.extend(git_exclude(root));
    rules.extend(dirs.filter(|d| *d != root && d.join(".git").exists()).filter_map(|d| git_exclude(d)));
    // the walks apply the global file inside a repository only
    if top.is_some() {
        // as an event names it: its directory resolved, and no entry when that does not exist
        let named = |p: PathBuf| Some(std::fs::canonicalize(p.parent()?).ok()?.join(p.file_name()?));
        rules.extend(ignore::gitignore::gitconfig_excludes_path().and_then(named));
        // and the configuration files that say where it is
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = var(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
        let config = match var("GIT_CONFIG_GLOBAL") {
            Some(file) => vec![file],
            None => {
                let xdg = var("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config")));
                home.iter().map(|h| h.join(".gitconfig")).chain(xdg.map(|x| x.join("git").join("config"))).collect()
            }
        };
        rules.extend(config.into_iter().filter_map(named));
    }
    rules
}

/// What the last walk of the tree found.
struct Walked {
    /// the directories each index's walk enters; none for the text index when there is none
    code: HashSet<PathBuf>,
    text: HashSet<PathBuf>,
    text_index: bool,
    /// `outer_rules`
    rules: HashSet<PathBuf>,
    at: Instant,
    /// directories the walks enter that got no watch
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    unwatched: usize,
    /// directories that hold only rule files and got no watch
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    unwatched_rules: usize,
    /// what `Poll` looks at: directories with no watch, and rule files whose directory has none
    poll_dirs: Vec<PathBuf>,
    poll_files: Vec<PathBuf>,
}

impl Walked {
    fn new(root: &Path, code: &CodeWalk) -> Walked {
        let text_index = has_text_index(root);
        let code: HashSet<PathBuf> =
            code.builder(root).build().flatten().filter(|e| e.file_type().is_some_and(|t| t.is_dir())).map(|e| e.into_path()).collect();
        let text: HashSet<PathBuf> = if text_index { crate::textindex::walk_entries(root, None).0.into_iter().collect() } else { HashSet::new() };
        let rules = outer_rules(root, code.union(&text));
        Walked { code, text, text_index, rules, at: Instant::now(), unwatched: 0, unwatched_rules: 0, poll_dirs: Vec::new(), poll_files: Vec::new() }
    }

    fn holds(&self, dir: &Path) -> bool {
        self.code.contains(dir) || self.text.contains(dir)
    }

    /// The directories of `rules` that no walk enters.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn rule_dirs(&self) -> HashSet<PathBuf> {
        self.rules.iter().filter_map(|r| r.parent()).filter(|d| !self.holds(d) && d.is_dir()).map(Path::to_path_buf).collect()
    }

    /// Every directory either walk enters, and those of `rule_dirs`.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn watched(&self) -> HashSet<PathBuf> {
        let mut all: HashSet<PathBuf> = self.code.union(&self.text).cloned().collect();
        all.extend(self.rule_dirs());
        all
    }

    /// Whether a burst walks the tree whatever it names.
    fn due(&self, root: &Path) -> bool {
        self.at.elapsed() >= WALK_EVERY || self.text_index != has_text_index(root)
    }

    /// Whether `path` holds rules for what is indexed: an ignore file in the tree, one of `rules`,
    /// or a `.scrutate.toml`.
    fn sets_rules(&self, root: &Path, path: &Path) -> bool {
        self.rules.contains(path)
            || inside(root, path)
                && path.file_name().is_some_and(|n| [".gitignore", ".ignore", crate::instant::RGIGNORE, ".scrutate.toml"].iter().any(|r| n == *r))
    }
}

/// One directory as each index's walk lists it.
#[derive(Default)]
struct Listing {
    dirs: HashSet<PathBuf>,
    code: HashSet<PathBuf>,
    text: HashSet<PathBuf>,
}

impl Listing {
    fn of(dir: &Path, code: &CodeWalk, walked: &Walked) -> Listing {
        let mut l = Listing::default();
        if walked.code.contains(dir) {
            for e in code.builder(dir).max_depth(Some(1)).build().flatten().filter(|e| e.depth() == 1) {
                if e.file_type().is_some_and(|t| t.is_dir()) {
                    l.dirs.insert(e.into_path());
                } else if code.keeps(&e).is_some() {
                    l.code.insert(e.into_path());
                }
            }
        }
        if walked.text.contains(dir) {
            let (dirs, files) = crate::textindex::walk_entries(dir, Some(1));
            l.dirs.extend(dirs.into_iter().filter(|d| d != dir));
            l.text.extend(files);
        }
        l
    }

    /// Whether a file of this directory is named `p` in another case.
    fn holds_in_another_case(&self, p: &Path) -> bool {
        let lower = |q: &Path| q.file_name().map(|n| n.to_string_lossy().to_lowercase());
        let want = lower(p);
        self.code.iter().chain(&self.text).any(|q| q != p && lower(q) == want)
    }
}

/// What the events of one burst ask for.
#[derive(Default)]
struct Burst {
    /// a rule file or lost events: walk the tree and update everything
    full: bool,
    /// changed paths under the root
    paths: BTreeSet<PathBuf>,
    /// those of them created, removed or renamed
    moved: HashSet<PathBuf>,
}

impl Burst {
    /// Take one event in; -> whether it names anything that may matter.
    fn note(&mut self, root: &Path, walked: &Walked, ev: &notify::Result<Event>) -> bool {
        let e = match ev {
            // the watch limit or a lost queue: the walk and the full update recover
            Err(e) => {
                eprintln!("[scrutate] file watcher: {e}");
                self.full = true;
                return true;
            }
            // a lost queue or buffer (IN_Q_OVERFLOW, an empty ReadDirectoryChangesW read,
            // FSEvents' MustScanSubDirs): events were lost
            Ok(e) if e.need_rescan() => {
                self.full = true;
                return true;
            }
            // reads (the update's own included) are not changes
            Ok(Event { kind: EventKind::Access(_), .. }) => return false,
            Ok(e) => e,
        };
        let moved = !matches!(e.kind, EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any));
        let mut wanted = false;
        for p in &e.paths {
            if walked.sets_rules(root, p) {
                self.full = true;
                wanted = true;
            } else if inside(root, p) {
                if moved {
                    self.moved.insert(p.clone());
                }
                self.paths.insert(p.clone());
                wanted = true;
            }
        }
        wanted
    }

    /// The burst's files for each index, as the walks list their directories; None when a
    /// directory the walks enter came, went or was replaced, so the tree must be walked.
    fn files(&self, root: &Path, code: &CodeWalk, walked: &Walked) -> Option<Named> {
        let mut listed: HashMap<&Path, Listing> = HashMap::new();
        let mut out = Named::default();
        for p in &self.paths {
            let Some(parent) = p.parent() else { continue };
            let md = std::fs::symlink_metadata(p).ok();
            let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
            if walked.holds(p) {
                // a directory's own write time (Windows reports one per entry added or removed):
                // the entry has an event of its own
                if is_dir && !self.moved.contains(p) {
                    continue;
                }
                return None;
            }
            if !walked.holds(parent) {
                continue;
            }
            let l = listed.entry(parent).or_insert_with(|| Listing::of(parent, code, walked));
            if is_dir {
                if l.dirs.contains(p) {
                    return None;
                }
                continue;
            }
            // on a disk that ignores case the old name of a rename of its case alone still opens
            // the file: the listing holds it under another case, and only a walk tells the two
            // names apart
            if md.as_ref().is_some_and(|m| m.is_file()) && !l.code.contains(p) && !l.text.contains(p) && l.holds_in_another_case(p) {
                return None;
            }
            let Ok(rel) = p.strip_prefix(root) else { continue };
            let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            let gone = !md.is_some_and(|m| m.is_file());
            if gone || l.code.contains(p) {
                out.code.push(rel.clone());
            }
            if gone || l.text.contains(p) {
                out.text.push(rel);
            }
        }
        Some(out)
    }
}

/// An entry as `Poll` last saw it. A directory's own times change with each entry, which is seen
/// on its own, so only its presence counts.
#[derive(PartialEq)]
struct Seen {
    dir: bool,
    len: u64,
    modified: Option<SystemTime>,
    /// inode and status-change time: a rename over the file, or a change of its mode
    #[cfg(unix)]
    change: (u64, i64, i64),
}

impl Seen {
    fn of(md: &std::fs::Metadata) -> Seen {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        if md.is_dir() {
            return Seen {
                dir: true,
                len: 0,
                modified: None,
                #[cfg(unix)]
                change: (0, 0, 0),
            };
        }
        Seen {
            dir: false,
            len: md.len(),
            modified: md.modified().ok(),
            #[cfg(unix)]
            change: (md.ino(), md.ctime(), md.ctime_nsec()),
        }
    }
}

/// The directories and rule files that have no watch, looked at on a timer: each change since the
/// last look comes back as the event a watch would have sent. The pause grows with what a look
/// costs, as macOS's timer does.
struct Poll {
    dirs: Vec<PathBuf>,
    files: Vec<PathBuf>,
    seen: HashMap<PathBuf, Seen>,
    at: Instant,
    pause: Duration,
}

impl Poll {
    /// The first look, taken now: changes from here on are reported.
    fn new(walked: &Walked) -> Poll {
        let mut p = Poll { dirs: walked.poll_dirs.clone(), files: walked.poll_files.clone(), seen: HashMap::new(), at: Instant::now(), pause: POLL_MIN };
        p.seen = p.look();
        p
    }

    fn look(&self) -> HashMap<PathBuf, Seen> {
        let mut seen = HashMap::new();
        for d in &self.dirs {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                if let Ok(md) = e.metadata() {
                    seen.insert(e.path(), Seen::of(&md));
                }
            }
        }
        for f in &self.files {
            if let Ok(md) = std::fs::symlink_metadata(f) {
                seen.insert(f.clone(), Seen::of(&md));
            }
        }
        seen
    }

    /// The changes since the last look, once the pause is over; none before.
    fn check(&mut self) -> Vec<notify::Result<Event>> {
        if self.dirs.is_empty() && self.files.is_empty() || self.at.elapsed() < self.pause {
            return Vec::new();
        }
        self.check_now()
    }

    fn check_now(&mut self) -> Vec<notify::Result<Event>> {
        let t0 = Instant::now();
        let now = self.look();
        let event = |kind, p: &PathBuf| Ok(Event::new(kind).add_path(p.clone()));
        let mut out = Vec::new();
        for (p, s) in &now {
            match self.seen.get(p) {
                None => out.push(event(EventKind::Create(CreateKind::Any), p)),
                Some(old) if old.dir != s.dir => out.push(event(EventKind::Create(CreateKind::Any), p)),
                Some(old) if old != s => out.push(event(EventKind::Modify(ModifyKind::Data(DataChange::Any)), p)),
                Some(_) => {}
            }
        }
        out.extend(self.seen.keys().filter(|p| !now.contains_key(*p)).map(|p| event(EventKind::Remove(RemoveKind::Any), p)));
        self.seen = now;
        self.pause = (t0.elapsed() * 10).clamp(POLL_MIN, POLL_MAX);
        self.at = Instant::now();
        out
    }
}

/// The least and most time between two looks of `Poll` and two checks of macOS's timer.
const POLL_MIN: Duration = Duration::from_secs(1);
const POLL_MAX: Duration = Duration::from_secs(30);

fn root_present(root: &Path) -> Result<()> {
    if !root.is_dir() {
        anyhow::bail!("{} is gone: the watched directory was removed or moved", root.display());
    }
    Ok(())
}

/// The order watches are set in: the root first (the watch ends without it), then shallow
/// directories before deep ones and by path, so at the watch limit a tree keeps the same watches.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn watch_order(root: &Path, dirs: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = dirs.iter().filter(|d| *d != root).cloned().collect();
    v.sort_by_cached_key(|d| (d.components().count(), d.clone()));
    v.insert(0, root.to_path_buf());
    v
}

/// The line that says how many directories of the tree got no inotify watch.
#[cfg(target_os = "linux")]
fn unwatched_message(unwatched: usize, of: usize, e: &notify::Error) -> String {
    let limit = if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) {
        "; the limit is the sysctl fs.inotify.max_user_watches, which `sudo sysctl fs.inotify.max_user_watches=N` raises"
    } else {
        ""
    };
    format!("[scrutate] could not watch {unwatched} of {of} directories ({e}); they are checked on a timer instead, every 1 to 30 s{limit}")
}

/// Linux: walk the tree and set one non-recursive inotify watch per directory the walks enter, so
/// ignored trees cost no watches; new directories get a watch and ones that left lose theirs. Those
/// left without one (past the watch limit) go to `Poll`. Only an error on `root` itself is returned.
#[cfg(target_os = "linux")]
fn sync_watches(watcher: &mut impl Watcher, root: &Path, code: &CodeWalk, old: Option<&Walked>) -> Result<Walked> {
    let mut now = Walked::new(root, code);
    let dirs = now.watched();
    if let Some(old) = old {
        // removals first: a directory moved inside the tree has one inotify watch under both names
        for gone in old.watched().difference(&dirs) {
            let _ = watcher.unwatch(gone);
        }
    }
    // notify drops the watches of moved and deleted directories itself, and a lost queue or the
    // limit may have dropped others, so every directory is watched again (inotify merges repeats)
    // directories that only hold rule files come last, so at the watch limit they lose first
    let rule_dirs = now.rule_dirs();
    let tree: HashSet<PathBuf> = dirs.difference(&rule_dirs).cloned().collect();
    let mut order = watch_order(root, &tree);
    let mut rules_order: Vec<PathBuf> = rule_dirs.into_iter().collect();
    rules_order.sort();
    let tree_dirs = order.len();
    order.extend(rules_order);
    watcher.watch(root, RecursiveMode::NonRecursive).with_context(|| format!("watch {}", root.display()))?;
    let (mut first, mut first_rules) = (None, None);
    for (i, d) in order.iter().enumerate().skip(1) {
        if let Err(e) = watcher.watch(d, RecursiveMode::NonRecursive) {
            if i < tree_dirs {
                now.unwatched += 1;
                now.poll_dirs.push(d.clone());
                first.get_or_insert(e);
            } else {
                now.unwatched_rules += 1;
                let files = now.rules.iter().filter(|r| r.parent() == Some(d.as_path())).cloned().collect::<Vec<_>>();
                now.poll_files.extend(files);
                first_rules.get_or_insert(e);
            }
        }
    }
    if let Some(e) = first.filter(|_| old.is_none_or(|o| o.unwatched != now.unwatched)) {
        eprintln!("{}", unwatched_message(now.unwatched, tree_dirs, &e));
    }
    if let Some(e) = first_rules.filter(|_| old.is_none_or(|o| o.unwatched_rules != now.unwatched_rules)) {
        eprintln!(
            "[scrutate] could not watch {} of {} directories that only hold ignore rules ({e}); those rules are checked on a timer instead",
            now.unwatched_rules,
            order.len() - tree_dirs
        );
    }
    Ok(now)
}

/// Windows: one recursive watch on the root (`ReadDirectoryChangesW`), plus one non-recursive watch
/// per directory outside the root that holds rule files the walks read. The walk only records the
/// directories.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
fn sync_watches(watcher: &mut impl Watcher, root: &Path, code: &CodeWalk, old: Option<&Walked>) -> Result<Walked> {
    let now = Walked::new(root, code);
    if old.is_none() {
        watcher.watch(root, RecursiveMode::Recursive).with_context(|| format!("watch {}", root.display()))?;
    }
    let before = old.map(Walked::watched).unwrap_or_default();
    for d in now.watched().iter().filter(|d| !d.starts_with(root) && !before.contains(*d)) {
        let _ = watcher.watch(d, RecursiveMode::NonRecursive);
    }
    Ok(now)
}

/// macOS: the FSEvents stream covers the root's whole tree. A stream on a directory above the root
/// would report every change under it (the home directory, or `/`), so the rule files outside the
/// root go to `Poll`.
#[cfg(target_os = "macos")]
fn sync_watches(_: &mut fsevents::Stream, root: &Path, code: &CodeWalk, _: Option<&Walked>) -> Result<Walked> {
    let mut now = Walked::new(root, code);
    now.poll_files = now.rules.iter().filter(|r| !r.starts_with(root)).cloned().collect();
    now.poll_files.sort();
    Ok(now)
}

/// `SCRUTATE_WATCH_DEBUG`: one line on stderr per burst, saying what it costs.
fn debug() -> bool {
    std::env::var_os("SCRUTATE_WATCH_DEBUG").is_some_and(|v| !v.is_empty())
}

/// Watch `root` until `stop` is set. Once the watch is in place `on_change` runs, then `on_ready`;
/// after that `on_change` runs once per debounced burst that changes what an index holds, with the
/// burst's change set: the files it names when it does not walk the tree. An error from the first
/// `on_change`, or the root going away, ends the watch; later update errors are reported.
pub fn run(root: &Path, stop: &AtomicBool, on_ready: impl FnOnce(), mut on_change: impl FnMut(&Changes) -> Result<()>) -> Result<()> {
    let root = std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    let (tx, rx) = mpsc::channel();
    #[cfg(target_os = "macos")]
    let Some(mut watcher) = fsevents::Stream::start(&root, tx) else {
        eprintln!("[scrutate] FSEvents could not be loaded or started; checking the tree on a timer instead");
        return run_timer(&root, stop, on_ready, on_change);
    };
    #[cfg(not(target_os = "macos"))]
    let mut watcher = notify::recommended_watcher(tx).context("start the file watcher")?;
    let code = CodeWalk::new(&root);
    let mut walked = sync_watches(&mut watcher, &root, &code, None)?;
    let mut poll = Poll::new(&walked);
    on_change(&Changes::All)?;
    on_ready();
    let debug = debug();
    // a burst whose update failed is tried again, with a walk of the tree, by the next burst or
    // after a pause that doubles up to 30 s, whichever comes first
    let (mut failed, mut retry_at, mut pause) = (false, None::<Instant>, RETRY_FIRST);
    while !stop.load(Ordering::Relaxed) {
        root_present(&root)?;
        let mut burst = Burst { full: failed, ..Burst::default() };
        let mut wanted = retry_at.is_some_and(|t| Instant::now() >= t);
        for ev in poll.check() {
            wanted |= burst.note(&root, &walked, &ev);
        }
        if !wanted {
            let ev = match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(ev) => ev,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => bail!("the file watcher stopped"),
            };
            if !burst.note(&root, &walked, &ev) {
                continue;
            }
        }
        let t0 = Instant::now();
        while t0.elapsed() < MAX_WAIT {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(ev) => {
                    burst.note(&root, &walked, &ev);
                }
                Err(_) => break,
            }
        }
        root_present(&root)?;
        let files = if burst.full { None } else { burst.files(&root, &code, &walked) };
        if files.as_ref().is_some_and(|f| f.code.is_empty() && f.text.is_empty()) {
            continue;
        }
        let named = files.filter(|_| !walked.due(&root));
        if named.is_none() {
            walked = sync_watches(&mut watcher, &root, &code, Some(&walked))?;
            poll = Poll::new(&walked);
        }
        if debug {
            eprintln!("[scrutate] burst: {} paths, full walk: {}", burst.paths.len(), if named.is_none() { "yes" } else { "no" });
        }
        failed = on_change(&named.map_or(Changes::All, Changes::Named)).inspect_err(|e| eprintln!("[scrutate] update failed: {e:#}")).is_err();
        (retry_at, pause) = if failed { (Some(Instant::now() + pause), (pause * 2).min(RETRY_MOST)) } else { (None, RETRY_FIRST) };
    }
    Ok(())
}

/// macOS without FSEvents: the index's own incremental check runs on a timer. It compares sizes and
/// nanosecond times, and the pause grows with what a check costs (about a tenth of the time at most).
#[cfg(target_os = "macos")]
fn run_timer(root: &Path, stop: &AtomicBool, on_ready: impl FnOnce(), mut on_change: impl FnMut(&Changes) -> Result<()>) -> Result<()> {
    on_change(&Changes::All)?;
    on_ready();
    let mut pause = POLL_MIN;
    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        while t0.elapsed() < pause && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        root_present(root)?;
        let t1 = Instant::now();
        if let Err(e) = on_change(&Changes::All) {
            eprintln!("[scrutate] update failed: {e:#}");
        }
        pause = (t1.elapsed() * 10).clamp(POLL_MIN, POLL_MAX);
    }
    Ok(())
}

/// FSEvents, loaded with `dlopen` while `watch` runs: linking CoreServices would load it into every
/// command (about 1 ms of start-up and 4 MB). Every function, CoreFoundation's and libdispatch's
/// too, comes from `dlsym` on that handle, so the executable imports nothing new.
#[cfg(target_os = "macos")]
mod fsevents {
    use std::ffi::{c_char, c_int, c_void, CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null;
    use std::sync::mpsc::Sender;

    use notify::event::{CreateKind, DataChange, Flag, ModifyKind, RemoveKind, RenameMode};
    use notify::{Event, EventKind};

    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    const RTLD_NOW: c_int = 0x2;
    const RTLD_LOCAL: c_int = 0x4;
    // FSEvents.h
    const CREATE_NO_DEFER: u32 = 0x2;
    const CREATE_FILE_EVENTS: u32 = 0x10;
    const SINCE_NOW: u64 = u64::MAX;
    /// MustScanSubDirs, UserDropped, KernelDropped, RootChanged, Mount, Unmount: what the stream
    /// says no longer holds, so the tree is walked
    const RESCAN: u32 = 0x1 | 0x2 | 0x4 | 0x20 | 0x40 | 0x80;
    const ITEM_CREATED: u32 = 0x100;
    const ITEM_REMOVED: u32 = 0x200;
    const ITEM_RENAMED: u32 = 0x800;
    const ITEM_IS_DIR: u32 = 0x20000;
    /// seconds the stream gathers events for before it calls back; the burst's own wait follows
    const LATENCY: f64 = 0.05;

    type Callback = unsafe extern "C" fn(*const c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);
    type Retain = unsafe extern "C" fn(*const c_void) -> *const c_void;
    type Release = unsafe extern "C" fn(*const c_void);
    type CfStringCreate = unsafe extern "C" fn(*const c_void, *const c_char) -> *const c_void;
    type CfArrayCreate = unsafe extern "C" fn(*const c_void, *const *const c_void, isize, *const c_void) -> *const c_void;
    type CfRelease = unsafe extern "C" fn(*const c_void);
    type QueueCreate = unsafe extern "C" fn(*const c_char, *const c_void) -> *mut c_void;
    type StreamCreate = unsafe extern "C" fn(*const c_void, Callback, *mut Context, *const c_void, u64, f64, u32) -> *mut c_void;
    type SetQueue = unsafe extern "C" fn(*mut c_void, *mut c_void);
    type Start = unsafe extern "C" fn(*mut c_void) -> u8;
    type Op = unsafe extern "C" fn(*mut c_void);

    /// `FSEventStreamContext`
    #[repr(C)]
    struct Context {
        version: isize,
        info: *mut c_void,
        retain: Option<Retain>,
        release: Option<Release>,
        copy_description: Option<Retain>,
    }

    struct Api {
        cf_string: CfStringCreate,
        cf_array: CfArrayCreate,
        cf_release: CfRelease,
        /// `&kCFTypeArrayCallBacks`
        array_callbacks: *const c_void,
        queue_create: QueueCreate,
        queue_release: Op,
        create: StreamCreate,
        set_queue: SetQueue,
        start: Start,
        stop: Op,
        invalidate: Op,
        release: Op,
    }

    impl Api {
        fn load() -> Option<Api> {
            // SAFETY: a nul-terminated path; a missing framework gives a null handle
            let lib = unsafe { dlopen(c"/System/Library/Frameworks/CoreServices.framework/CoreServices".as_ptr(), RTLD_NOW | RTLD_LOCAL) };
            if lib.is_null() {
                return None;
            }
            // SAFETY: a nul-terminated name on a handle `dlopen` returned
            let sym = |name: &CStr| Some(unsafe { dlsym(lib, name.as_ptr()) }).filter(|p| !p.is_null());
            // SAFETY (every transmute below): each name is the C function of that signature in
            // the macOS SDK headers (FSEvents.h, CFString.h, CFArray.h, CFBase.h, dispatch/queue.h)
            unsafe {
                Some(Api {
                    cf_string: std::mem::transmute::<*mut c_void, CfStringCreate>(sym(c"CFStringCreateWithFileSystemRepresentation")?),
                    cf_array: std::mem::transmute::<*mut c_void, CfArrayCreate>(sym(c"CFArrayCreate")?),
                    cf_release: std::mem::transmute::<*mut c_void, CfRelease>(sym(c"CFRelease")?),
                    array_callbacks: sym(c"kCFTypeArrayCallBacks")?.cast_const(),
                    queue_create: std::mem::transmute::<*mut c_void, QueueCreate>(sym(c"dispatch_queue_create")?),
                    queue_release: std::mem::transmute::<*mut c_void, Op>(sym(c"dispatch_release")?),
                    create: std::mem::transmute::<*mut c_void, StreamCreate>(sym(c"FSEventStreamCreate")?),
                    set_queue: std::mem::transmute::<*mut c_void, SetQueue>(sym(c"FSEventStreamSetDispatchQueue")?),
                    start: std::mem::transmute::<*mut c_void, Start>(sym(c"FSEventStreamStart")?),
                    stop: std::mem::transmute::<*mut c_void, Op>(sym(c"FSEventStreamStop")?),
                    invalidate: std::mem::transmute::<*mut c_void, Op>(sym(c"FSEventStreamInvalidate")?),
                    release: std::mem::transmute::<*mut c_void, Op>(sym(c"FSEventStreamRelease")?),
                })
            }
        }
    }

    type Tx = Sender<notify::Result<Event>>;

    /// A started stream on one directory, scheduled on its own serial queue; its events go to the
    /// channel as `notify` events.
    pub struct Stream {
        api: Api,
        stream: *mut c_void,
        queue: *mut c_void,
    }

    impl Stream {
        /// None when CoreServices or one of its functions is missing, or the stream does not start.
        pub fn start(root: &Path, tx: Tx) -> Option<Stream> {
            let path = CString::new(root.as_os_str().as_bytes()).ok()?;
            let api = Api::load()?;
            // SAFETY: each call is made as its header declares, with pointers checked for null
            // before use. The context's `info` is a boxed sender that `release_tx` frees when the
            // stream is deallocated, or freed here when no stream was made.
            unsafe {
                let queue = (api.queue_create)(c"scrutate.watch".as_ptr(), null());
                if queue.is_null() {
                    return None;
                }
                let s = (api.cf_string)(null(), path.as_ptr());
                let paths = if s.is_null() { null() } else { (api.cf_array)(null(), &s, 1, api.array_callbacks) };
                if !s.is_null() {
                    (api.cf_release)(s);
                }
                if paths.is_null() {
                    (api.queue_release)(queue);
                    return None;
                }
                let info = Box::into_raw(Box::new(tx)).cast::<c_void>();
                let mut context = Context { version: 0, info, retain: None, release: Some(release_tx), copy_description: None };
                let stream = (api.create)(null(), callback, &mut context, paths, SINCE_NOW, LATENCY, CREATE_FILE_EVENTS | CREATE_NO_DEFER);
                (api.cf_release)(paths);
                if stream.is_null() {
                    drop(Box::from_raw(info.cast::<Tx>()));
                    (api.queue_release)(queue);
                    return None;
                }
                (api.set_queue)(stream, queue);
                if (api.start)(stream) == 0 {
                    (api.invalidate)(stream);
                    (api.release)(stream);
                    (api.queue_release)(queue);
                    return None;
                }
                Some(Stream { api, stream, queue })
            }
        }
    }

    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: a started stream on its queue, torn down in the order FSEvents.h gives
            unsafe {
                (self.api.stop)(self.stream);
                (self.api.invalidate)(self.stream);
                (self.api.release)(self.stream);
                (self.api.queue_release)(self.queue);
            }
        }
    }

    /// The stream's release callback for its `info`.
    unsafe extern "C" fn release_tx(info: *const c_void) {
        // SAFETY: `info` is the box `Stream::start` made, released once with the stream
        drop(unsafe { Box::from_raw(info as *mut Tx) });
    }

    /// `FSEventStreamCallback`. Without `kFSEventStreamCreateFlagUseCFTypes`, `paths` is an array
    /// of `n` C strings next to `n` flag words.
    unsafe extern "C" fn callback(_: *const c_void, info: *mut c_void, n: usize, paths: *mut c_void, flags: *const u32, _: *const u64) {
        if n == 0 || paths.is_null() || flags.is_null() {
            return;
        }
        // SAFETY: `info` is the live boxed sender (the stream outlives its callbacks), and FSEvents
        // passes `n` valid entries in each array
        let (tx, paths, flags) =
            unsafe { (&*info.cast::<Tx>(), std::slice::from_raw_parts(paths.cast::<*const c_char>(), n), std::slice::from_raw_parts(flags, n)) };
        for (&p, &f) in paths.iter().zip(flags) {
            // SAFETY: a nul-terminated path that lives until the callback returns
            let p = Path::new(OsStr::from_bytes(unsafe { CStr::from_ptr(p) }.to_bytes()));
            let _ = tx.send(Ok(event(p, f)));
        }
    }

    /// One FSEvents event as `notify` names it.
    pub fn event(path: &Path, flags: u32) -> Event {
        if flags & RESCAN != 0 {
            return Event::new(EventKind::Other).set_flag(Flag::Rescan);
        }
        let kind = if flags & ITEM_CREATED != 0 {
            EventKind::Create(if flags & ITEM_IS_DIR != 0 { CreateKind::Folder } else { CreateKind::Any })
        } else if flags & ITEM_REMOVED != 0 {
            EventKind::Remove(RemoveKind::Any)
        } else if flags & ITEM_RENAMED != 0 {
            EventKind::Modify(ModifyKind::Name(RenameMode::Any))
        } else {
            EventKind::Modify(ModifyKind::Data(DataChange::Any))
        };
        Event::new(kind).add_path(path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};

    /// A repository with a text index (its manifest is enough for the walks).
    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_burst_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [".git/info", "src/deep", "gone_dir", ".scrutate/text"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join(".scrutate/text/manifest.json"), "{}").unwrap();
        std::fs::write(root.join(".gitignore"), "*.tmp\ngen/\n").unwrap();
        std::fs::write(root.join("src/a.py"), "x\n").unwrap();
        std::fs::canonicalize(&root).unwrap()
    }

    fn ev(kind: EventKind, paths: &[&Path]) -> notify::Result<Event> {
        let mut e = Event::new(kind);
        for p in paths {
            e = e.add_path(p.to_path_buf());
        }
        Ok(e)
    }

    /// The walk of the tree as it is now, then these events; -> the burst's files, None when it
    /// walks the tree. `change` runs between the walk and the events.
    fn burst(root: &Path, change: impl FnOnce(), events: &[notify::Result<Event>]) -> Option<Named> {
        let code = CodeWalk::new(root);
        let walked = Walked::new(root, &code);
        change();
        let mut b = Burst::default();
        for e in events {
            b.note(root, &walked, e);
        }
        if b.full {
            return None;
        }
        b.files(root, &code, &walked)
    }

    fn both(paths: &[&str]) -> Option<Named> {
        let v: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        Some(Named { code: v.clone(), text: v })
    }

    #[test]
    fn a_burst_of_file_events_names_its_files() {
        let root = tree("files");
        let modify = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        let a = root.join("src/a.py");
        assert_eq!(burst(&root, || (), &[ev(modify, &[&a])]), both(&["src/a.py"]));
        // Windows names no kind of entry: the path is looked at
        let created = root.join("src/deep/b.py");
        let write = || std::fs::write(&created, "y\n").unwrap();
        assert_eq!(burst(&root, write, &[ev(EventKind::Create(CreateKind::Any), &[&created])]), both(&["src/deep/b.py"]));
        let tmp = root.join("src/.a.py.swp");
        let rename = EventKind::Modify(ModifyKind::Name(RenameMode::Both));
        assert_eq!(burst(&root, || (), &[ev(rename, &[&tmp, &a])]), both(&["src/.a.py.swp", "src/a.py"]));
        let removed = root.join("src/old.py");
        assert_eq!(burst(&root, || (), &[ev(EventKind::Remove(RemoveKind::Any), &[&removed])]), both(&["src/old.py"]));
        // ignored files, the index's own files, reads and files in directories no walk enters
        // name nothing
        std::fs::write(root.join("src/x.tmp"), "x\n").unwrap();
        std::fs::create_dir_all(root.join("gen")).unwrap();
        std::fs::write(root.join("gen/g.py"), "x\n").unwrap();
        std::fs::create_dir_all(root.join("src/new.tmp")).unwrap();
        for e in [
            ev(modify, &[&root.join("src/x.tmp")]),
            ev(modify, &[&root.join("gen/g.py")]),
            ev(EventKind::Access(notify::event::AccessKind::Any), &[&a]),
            ev(modify, &[&root.join(".scrutate/index.db")]),
            ev(modify, &[&root.join(".git/index")]),
            // Windows: the write time of a directory an entry was added to
            ev(EventKind::Modify(ModifyKind::Any), &[&root.join("src")]),
            // a new directory the walks do not enter
            ev(EventKind::Create(CreateKind::Folder), &[&root.join("src/new.tmp")]),
        ] {
            assert_eq!(burst(&root, || (), &[e]), Some(Named::default()));
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn directories_rules_and_lost_events_walk_the_tree() {
        let modify = EventKind::Modify(ModifyKind::Any);
        type Change = fn(&Path);
        let cases: [(&str, Change, EventKind, &str); 12] = [
            (
                "a directory moved away and another made in its place",
                |r| std::fs::rename(r.join("src/deep"), r.join("src/deeper")).and_then(|_| std::fs::create_dir(r.join("src/deep"))).unwrap(),
                EventKind::Create(CreateKind::Folder),
                "src/deep",
            ),
            ("a new directory (inotify)", |r| std::fs::create_dir(r.join("src/x")).unwrap(), EventKind::Create(CreateKind::Folder), "src/x"),
            ("a new directory (Windows)", |r| std::fs::create_dir(r.join("src/y")).unwrap(), EventKind::Create(CreateKind::Any), "src/y"),
            ("a removed directory the walk found", |r| std::fs::remove_dir(r.join("gone_dir")).unwrap(), EventKind::Remove(RemoveKind::Any), "gone_dir"),
            (
                "a directory renamed away",
                |r| std::fs::rename(r.join("src/deep"), r.join("src/deeper")).unwrap(),
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                "src/deep",
            ),
            (
                "a directory replaced by a file",
                |r| std::fs::remove_dir(r.join("gone_dir")).and_then(|_| std::fs::write(r.join("gone_dir"), "x")).unwrap(),
                modify,
                "gone_dir",
            ),
            ("an ignore file", |_| (), modify, "src/.ignore"),
            ("a .gitignore", |_| (), modify, ".gitignore"),
            ("git's exclude", |_| (), modify, ".git/info/exclude"),
            ("the configuration file", |_| (), modify, ".scrutate.toml"),
            ("a ripgrep ignore file", |_| (), modify, ".rgignore"),
            ("a removed .gitignore", |_| (), EventKind::Remove(RemoveKind::File), "src/deep/.gitignore"),
        ];
        for (what, change, kind, rel) in cases {
            let root = tree("full");
            let a = root.join("src/a.py");
            assert_eq!(burst(&root, || change(&root), &[ev(modify, &[&a]), ev(kind, &[&root.join(rel)])]), None, "{what}");
            std::fs::remove_dir_all(&root).unwrap();
        }
        let root = tree("lost");
        let a = root.join("src/a.py");
        for (what, e) in
            [("a lost queue", Ok(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan))), ("a watcher error", Err(notify::Error::generic("test")))]
        {
            assert_eq!(burst(&root, || (), &[ev(modify, &[&a]), e]), None, "{what}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn each_index_gets_the_files_its_walk_lists() {
        let root = tree("split");
        let names = ["src/a.py", "src/.hidden.py", "src/gone.py", "src/a.tmp", "src/img.png"];
        for name in names {
            std::fs::write(root.join(name), "x\n").unwrap();
        }
        std::fs::create_dir_all(root.join("node_modules/m")).unwrap();
        std::fs::write(root.join("node_modules/m/i.js"), "x\n").unwrap();
        let modify = EventKind::Modify(ModifyKind::Any);
        let gone = || std::fs::remove_file(root.join("src/gone.py")).unwrap();
        let mut events: Vec<_> = names.iter().map(|n| ev(modify, &[&root.join(n)])).collect();
        events.push(ev(modify, &[&root.join("node_modules/m/i.js")]));
        let f = burst(&root, gone, &events).unwrap();
        assert_eq!(f.code, ["src/.hidden.py", "src/a.py", "src/gone.py"]);
        assert_eq!(f.text, ["node_modules/m/i.js", "src/a.py", "src/gone.py", "src/img.png"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_rules_of_a_nested_repository_decide_a_burst() {
        let root = tree("nested");
        std::fs::create_dir_all(root.join("vendor/lib/.git")).unwrap();
        std::fs::create_dir_all(root.join("vendor/lib/out")).unwrap();
        std::fs::write(root.join(".gitignore"), "*.gen.py\nout/\n").unwrap();
        std::fs::write(root.join("vendor/lib/x.gen.py"), "x\n").unwrap();
        std::fs::write(root.join("vendor/lib/out/o.py"), "x\n").unwrap();
        std::fs::write(root.join("src/y.gen.py"), "x\n").unwrap();
        let modify = EventKind::Modify(ModifyKind::Any);
        let events: Vec<_> = ["vendor/lib/x.gen.py", "vendor/lib/out/o.py", "src/y.gen.py"].iter().map(|n| ev(modify, &[&root.join(n)])).collect();
        assert_eq!(burst(&root, || (), &events), both(&["vendor/lib/out/o.py", "vendor/lib/x.gen.py"]));
        let code = CodeWalk::new(&root);
        let walked = Walked::new(&root, &code);
        assert!(walked.code.contains(&root.join("vendor/lib/out")) && walked.text.contains(&root.join("vendor/lib/out")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_linked_worktree_watches_the_exclude_file_of_its_main_repository() {
        let base = tree("wt");
        let main = base.join("main_repo/.git");
        std::fs::create_dir_all(main.join("info")).unwrap();
        std::fs::create_dir_all(main.join("worktrees/wt")).unwrap();
        std::fs::write(main.join("worktrees/wt/commondir"), "../..\n").unwrap();
        let wt = base.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", main.join("worktrees/wt").display())).unwrap();
        assert_eq!(git_exclude(&wt), Some(main.join("info/exclude")));
        assert_eq!(git_exclude(&base.join("src")), Some(base.join(".git/info/exclude")));
        let walked = Walked::new(&wt, &CodeWalk::new(&wt));
        assert!(walked.watched().contains(&main.join("info")));
        assert!(walked.sets_rules(&wt, &main.join("info/exclude")));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_burst_walks_once_the_last_walk_is_old_or_the_text_index_changed() {
        let root = tree("due");
        let mut w = Walked::new(&root, &CodeWalk::new(&root));
        assert!(!w.due(&root));
        // a directory without a watch is polled, so it is no reason to walk
        w.unwatched = 1;
        assert!(!w.due(&root), "a directory without a watch");
        w.unwatched = 0;
        if let Some(old) = Instant::now().checked_sub(WALK_EVERY) {
            w.at = old;
            assert!(w.due(&root), "an old walk");
            w.at = Instant::now();
        }
        std::fs::remove_file(root.join(".scrutate/text/manifest.json")).unwrap();
        assert!(w.due(&root), "the text index went away");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `Poll` on `dirs` and `files` of a fresh walk, then `change`; -> the burst its next look makes.
    fn polled(root: &Path, dirs: &[&str], files: &[&str], change: impl FnOnce()) -> Burst {
        let mut walked = Walked::new(root, &CodeWalk::new(root));
        walked.poll_dirs = dirs.iter().map(|d| root.join(d)).collect();
        walked.poll_files = files.iter().map(|f| root.join(f)).collect();
        let mut poll = Poll::new(&walked);
        assert!(poll.check().is_empty(), "nothing before the pause is over");
        change();
        let mut b = Burst::default();
        for e in poll.check_now() {
            b.note(root, &walked, &e);
        }
        assert!(poll.check_now().is_empty(), "a second look reports nothing new");
        b
    }

    #[test]
    fn a_poll_reports_what_changed_in_a_directory_without_a_watch() {
        let root = tree("poll");
        let code = CodeWalk::new(&root);
        std::fs::write(root.join("src/deep/old.py"), "x\n").unwrap();
        let change = || {
            std::fs::write(root.join("src/deep/new.py"), "y\n").unwrap();
            std::fs::write(root.join("src/a.py"), "a longer text\n").unwrap();
            std::fs::remove_file(root.join("src/deep/old.py")).unwrap();
        };
        let b = polled(&root, &["src", "src/deep"], &[], change);
        assert!(!b.full);
        let walked = Walked::new(&root, &code);
        assert_eq!(b.files(&root, &code, &walked), both(&["src/a.py", "src/deep/new.py", "src/deep/old.py"]));
        // a new directory walks the tree, as its event from a watch does
        let b = polled(&root, &["src"], &[], || std::fs::create_dir(root.join("src/sub")).unwrap());
        assert_eq!(b.files(&root, &code, &Walked::new(&root, &code)), None);
        // a polled rule file walks the tree
        let b = polled(&root, &[], &[".git/info/exclude"], || std::fs::write(root.join(".git/info/exclude"), "a.py\n").unwrap());
        assert!(b.full, "a changed rule file");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A watcher that sets `left` watches, then fails as inotify does at the limit.
    #[cfg(target_os = "linux")]
    struct Limited {
        left: usize,
    }

    #[cfg(target_os = "linux")]
    impl Watcher for Limited {
        fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
            Ok(Limited { left: 0 })
        }
        fn watch(&mut self, _: &Path, _: RecursiveMode) -> notify::Result<()> {
            if self.left == 0 {
                return Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch));
            }
            self.left -= 1;
            Ok(())
        }
        fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
            Ok(())
        }
        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn directories_past_the_watch_limit_are_polled() {
        let root = tree("limit");
        let code = CodeWalk::new(&root);
        // the root and gone_dir (shallow, then by path); src and src/deep get none
        let walked = sync_watches(&mut Limited { left: 2 }, &root, &code, None).unwrap();
        assert_eq!(walked.unwatched, 2);
        let mut dirs = walked.poll_dirs.clone();
        dirs.sort();
        assert_eq!(dirs, [root.join("src"), root.join("src/deep")]);
        assert!(walked.poll_files.contains(&root.join(".git/info/exclude")), "{:?}", walked.poll_files);
        assert!(!walked.due(&root), "no walk on every burst");
        let mut poll = Poll::new(&walked);
        std::fs::write(root.join("src/deep/b.py"), "y\n").unwrap();
        let mut b = Burst::default();
        for e in poll.check_now() {
            b.note(&root, &walked, &e);
        }
        assert_eq!(b.files(&root, &code, &walked), both(&["src/deep/b.py"]));
        let e = notify::Error::new(notify::ErrorKind::MaxFilesWatch);
        assert!(unwatched_message(2, 4, &e).contains("fs.inotify.max_user_watches"));
        let e = notify::Error::io(std::io::Error::from_raw_os_error(13));
        assert!(!unwatched_message(2, 4, &e).contains("max_user_watches"), "another error names no limit");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fsevents_flags_map_to_the_events_a_burst_reads() {
        let p = Path::new("/r/a.py");
        let kind = |flags| fsevents::event(p, flags).kind;
        assert_eq!(kind(0x100 | 0x10000), EventKind::Create(CreateKind::Any));
        assert_eq!(kind(0x100 | 0x20000), EventKind::Create(CreateKind::Folder));
        assert_eq!(kind(0x200), EventKind::Remove(RemoveKind::Any));
        assert_eq!(kind(0x800), EventKind::Modify(ModifyKind::Name(RenameMode::Any)));
        assert_eq!(kind(0x1000), EventKind::Modify(ModifyKind::Data(DataChange::Any)));
        for lost in [0x1, 0x2, 0x4, 0x20, 0x40, 0x80] {
            assert!(fsevents::event(p, lost | 0x1000).need_rescan(), "{lost:#x}");
        }
        assert_eq!(fsevents::event(p, 0x1000).paths, [p]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_fsevents_stream_reports_a_file_written_under_its_root() {
        let root = tree("fsevents");
        let (tx, rx) = mpsc::channel();
        let stream = fsevents::Stream::start(&root, tx).expect("FSEvents loads and starts");
        let file = root.join("src/deep/written.py");
        std::fs::write(&file, "x\n").unwrap();
        let t0 = Instant::now();
        let seen = loop {
            let left = Duration::from_secs(10).checked_sub(t0.elapsed()).expect("an event for the file within 10 s");
            match rx.recv_timeout(left) {
                Ok(Ok(e)) if e.paths.contains(&file) => break e,
                Ok(_) => {}
                Err(e) => panic!("no event for the file: {e}"),
            }
        };
        assert!(matches!(seen.kind, EventKind::Create(_) | EventKind::Modify(_)), "{seen:?}");
        drop(stream);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn watches_are_set_root_first_then_shallow_directories_by_path() {
        let root = PathBuf::from("/r");
        let dirs: HashSet<PathBuf> = ["/r/b/c", "/r/a/z/y", "/r", "/r/b", "/r/a", "/r/a/z", "/r/c/a"].iter().map(PathBuf::from).collect();
        let order: Vec<String> = watch_order(&root, &dirs).iter().map(|p| p.display().to_string()).collect();
        assert_eq!(order, ["/r", "/r/a", "/r/b", "/r/a/z", "/r/b/c", "/r/c/a", "/r/a/z/y"]);
    }
}
