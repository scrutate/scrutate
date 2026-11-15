//! A session holds one repository open for many calls. The long-lived front ends (the MCP
//! server, the language server, the Python bindings, `watch`) are its adapters: they keep no
//! repository of their own, and say only what their protocol needs to say.
//!
//! What a session does for every call: it waits for the first build of the index, scans the tree
//! at most once per refresh interval, gives each call a cancel flag of its own, keeps a panic
//! inside the call that made it, and has a damaged index built again (`stores.rs`). Reads run
//! side by side, each on a repository of its own (`Repo::another`: a SQLite connection, its
//! statement cache and its segment maps); up to `max_readers` are opened, when calls need them,
//! and kept. Writes (the first build, a watch burst, a save, the scan before a read) take the
//! writer lock the repositories share, so they run one at a time; a read beside a write sees
//! the index as of its last commit, never half of one (SQLite's write-ahead log, one transaction
//! per update, a segment manifest swapped in that transaction). The scan before a read waits for
//! a write of this process up to `stores::OWN_WRITE_WAIT`, then answers with what is `Pending`.
//!
//! A command line call may use the session `serve` runs, through a socket under the
//! repository's `.scrutate/` (`listen`, `ask`; Unix only). The session is a cache of work there:
//! anything it cannot vouch for makes the command line do the work itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::fresh::{Freshness, Token};
use crate::index::{self, FileCount, UpdateStats};
use crate::ops::{Answer, Op};
use crate::search::Searcher;
use crate::stores::{self, Changes};
use crate::Repo;

/// How long a call waits for an index another process is building before it gives that answer
/// (well under the 60 s after which Codex gives a call up).
pub const BUILD_WAIT: Duration = Duration::from_secs(20);

/// How often a watched session's idle readers let go of segments another process replaced.
const RELEASE_EVERY: Duration = Duration::from_secs(5);

/// A call that panicked; the session goes on.
#[derive(Debug)]
pub struct Panicked;

impl std::fmt::Display for Panicked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("internal failure (a bug: please report it)")
    }
}

impl std::error::Error for Panicked {}

/// When a read looks at every file of the tree first.
#[derive(Clone, Copy, Debug)]
pub struct Every {
    pub seconds: f64,
    /// `seconds` is a floor: no scan sooner, however cheap scans are; without it a scan comes
    /// once ten scans' time has passed, if that is sooner
    pub floor: bool,
}

impl Every {
    /// A refresh interval given (`serve --refresh-interval`), or the default of one second.
    pub fn given(seconds: Option<f64>) -> Every {
        Every { seconds: seconds.unwrap_or(1.0), floor: seconds.is_some() }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Build {
    /// not asked for: the first call's scan builds the index
    None,
    Building,
    Built,
    /// the first build panicked and no update went through since
    Failed,
}

/// What a read's answer does not hold yet: the change set its scan could not apply within
/// `stores::OWN_WRITE_WAIT` (a write of this process ran on, or another process held the lock).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending(pub Option<Changes>);

impl Pending {
    /// The files, by path relative to the root; every file for a walk that could not run.
    pub fn files(&self) -> Option<&[String]> {
        match &self.0 {
            Some(Changes::Named(n)) => Some(&n.code),
            _ => None,
        }
    }

    /// One sentence for the person or model reading the answer; None when nothing is pending.
    pub fn note(&self) -> Option<String> {
        const SHOWN: usize = 10;
        let files = match &self.0 {
            None => return None,
            Some(Changes::All) => return Some("files changed since the last scan may be missing from this answer: an index update is still running".into()),
            Some(Changes::Named(n)) => &n.code,
        };
        let mut list = files.iter().take(SHOWN).map(String::as_str).collect::<Vec<_>>().join(", ");
        if files.len() > SHOWN {
            list.push_str(&format!(" and {} more", files.len() - SHOWN));
        }
        Some(format!("{} changed file(s) not in this answer yet, an index update is still running: {list}", files.len()))
    }

    /// The JSON form a structured answer carries under `pending`.
    pub fn json(&self) -> Option<serde_json::Value> {
        self.0.as_ref().map(|_| match self.files() {
            Some(f) => serde_json::json!({"files": f, "all": false}),
            None => serde_json::json!({"files": [], "all": true}),
        })
    }
}

#[derive(Default)]
struct Pool {
    idle: Vec<Repo>,
    open: usize,
    /// moves on when the root's path names another tree: a reader of an older one is dropped
    generation: u64,
}

#[derive(Default)]
struct Clock {
    last: Option<Instant>,
    cost: f64,
}

pub struct Session {
    /// the canonical root now: the tree `given` names, which a link pointed elsewhere moves
    root: RwLock<PathBuf>,
    /// the root as the session was opened with it, absolute, links kept
    given: PathBuf,
    every: Every,
    max_readers: usize,
    /// writes
    seed: Mutex<Repo>,
    /// the repository the readers are opened from, never written: a write holds `seed` throughout
    opener: Mutex<Repo>,
    writer: Arc<Mutex<()>>,
    pool: Mutex<Pool>,
    freed: Condvar,
    build: Mutex<Build>,
    built: Condvar,
    clock: Mutex<Clock>,
    /// one scan decision at a time: a call that waited for another's scan does not scan again
    scanning: Mutex<()>,
    /// the files the writes of `seed` read, the first build's among them
    written: Arc<FileCount>,
    fresh: RwLock<Box<dyn Freshness>>,
    /// the token of the newest change set applied
    applied: Mutex<Token>,
    /// the look of the tree a search narrowed by the text index starts from, patched with each
    /// change set before a read (`files::patch_look`), so it costs no walk while a watch vouches
    kept: crate::files::KeptLook,
    /// what `watch` keeps for the reads, from `open_watched`
    notes: RwLock<Option<crate::fresh::Watching>>,
    /// the socket `listen` offers this session at, while it is the root's
    listening: Mutex<Option<PathBuf>>,
    /// ends that watch when the session is dropped
    unwatch: Arc<AtomicBool>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.unwatch.store(true, Ordering::Relaxed);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn read<T>(m: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    m.read().unwrap_or_else(|p| p.into_inner())
}

fn write<T>(m: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    m.write().unwrap_or_else(|p| p.into_inner())
}

fn caught<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(Panicked.into()))
}

impl Session {
    /// Open (or create) the index of `root`, without updating it.
    pub fn open(root: &Path, every: Every) -> Result<Session> {
        let mut seed = Repo::open(root)?;
        let written = Arc::new(FileCount::default());
        seed.set_count(Arc::clone(&written));
        let max_readers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
        let fresh: Box<dyn Freshness> = Box::new(crate::fresh::Walk::new(&seed.ix.root));
        Ok(Session {
            root: RwLock::new(seed.ix.root.clone()),
            given: std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf()),
            every,
            max_readers,
            writer: seed.writer(),
            opener: Mutex::new(seed.another()?),
            seed: Mutex::new(seed),
            pool: Mutex::default(),
            freed: Condvar::new(),
            build: Mutex::new(Build::None),
            built: Condvar::new(),
            clock: Mutex::default(),
            scanning: Mutex::new(()),
            written,
            fresh: RwLock::new(fresh),
            applied: Mutex::default(),
            notes: RwLock::default(),
            listening: Mutex::default(),
            kept: Default::default(),
            unwatch: Arc::default(),
        })
    }

    /// `open`, with reads told what changed by the events of a watch (`watch::Notes`) once
    /// `watch` starts it; until then, and when the watch cannot start or stops, reads walk the
    /// tree as without one.
    pub fn open_watched(root: &Path, every: Every) -> Result<Arc<Session>> {
        let s = Session::open(root, every)?;
        let (fresh, notes) = crate::fresh::for_session(&s.root());
        let s = s.with_freshness(fresh);
        *write(&s.notes) = notes;
        Ok(Arc::new(s))
    }

    /// Start the watch of a session `open_watched` opened; with `apply` (`serve --watch`) each
    /// burst also brings the stores up to date.
    pub fn watch(self: &Arc<Self>, apply: bool) -> Result<()> {
        // an idle reader is in no call that would see another process's compaction; without
        // this it keeps the deleted segment files mapped, their disk space held, until it is used
        let (session, stop) = (Arc::downgrade(self), Arc::clone(&self.unwatch));
        std::thread::Builder::new().name("scrutate-release".into()).spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(RELEASE_EVERY);
                let Some(s) = session.upgrade() else { break };
                s.release_replaced();
            }
        })?;
        if self.notes().is_none() && !apply {
            return Ok(());
        }
        let (session, stop, root) = (Arc::downgrade(self), Arc::clone(&self.unwatch), self.given.clone());
        std::thread::Builder::new().name("scrutate-watch".into()).spawn(move || {
            let mut update = |changes: &Changes| match (apply, session.upgrade()) {
                (true, Some(s)) => s.write(|repo| repo.update_stores(crate::stores::Writer::Watch, changes, false)).map(|_| ()),
                _ => Ok(()),
            };
            let ready: Box<dyn FnOnce()> = Box::new(|| {
                if apply {
                    eprintln!("[scrutate] watching {}", crate::paths::plain_path(&root).display())
                }
            });
            // the session's writer lock orders a burst's update with its other writes; the index's
            // own locks order it with other processes
            // reads that walk (a network file system) leave `serve --watch` its plain watch
            let mut ready = Some(ready);
            loop {
                let ready = ready.take().unwrap_or(Box::new(|| {}));
                // a reopen on another tree remakes them
                let notes = session.upgrade().and_then(|s| s.notes());
                let r = match &notes {
                    Some(n) => n.run(&root, &stop, ready, &mut update),
                    None => crate::watch::run(&root, &stop, ready, &mut update),
                };
                match r {
                    Err(e) if e.is::<crate::watch::RootReplaced>() => {
                        eprintln!("[scrutate] {}: root replaced; watching the new tree", crate::paths::plain_path(&root).display());
                        match session.upgrade().map(|s| s.reopen().map(|moved| (s, moved))) {
                            Some(Ok((s, moved))) => {
                                // a watch that applies builds the new tree in its first update; one
                                // that does not leaves it to this, or reads see its empty index
                                if moved && !apply {
                                    s.building();
                                    let _ = s.build();
                                }
                                continue;
                            }
                            Some(Err(e)) => eprintln!("[scrutate] watching stopped: the new tree could not be opened: {e:#}"),
                            None => {}
                        }
                    }
                    Err(e) if apply => eprintln!("[scrutate] watching stopped: {e:#}"),
                    Err(e) => debug(&format!("the watch stopped ({e:#}); reads walk the tree")),
                    Ok(()) => {}
                }
                break;
            }
        })?;
        Ok(())
    }

    /// The root's path names another tree now (`watch::RootReplaced`): its stores are opened
    /// afresh there, an index made if it has none (the next update builds it), and the readers of
    /// the old tree are dropped, the busy ones when they come back. -> whether the root moved to
    /// another tree (a link the session was opened through is pointed elsewhere).
    pub fn reopen(&self) -> Result<bool> {
        let mut seed = lock(&self.seed);
        let now = std::fs::canonicalize(&self.given).with_context(|| format!("open {}", self.given.display()))?;
        let moved = now != self.root();
        let mut fresh = if moved { seed.opened_at(&now)? } else { seed.opened_again()? };
        fresh.set_count(Arc::clone(&self.written));
        let opener = fresh.another()?;
        *seed = fresh;
        *lock(&self.opener) = opener;
        if moved {
            // no call made in the old tree is answered from this one
            if let Some(socket) = lock(&self.listening).take() {
                let _ = std::fs::remove_file(socket);
            }
            *write(&self.root) = now.clone();
            if self.notes().is_some() {
                let (fresh, notes) = crate::fresh::for_session(&now);
                *write(&self.fresh) = fresh;
                *write(&self.notes) = notes;
            }
        }
        let mut pool = lock(&self.pool);
        pool.open -= pool.idle.len();
        pool.idle.clear();
        pool.generation += 1;
        Ok(moved)
    }

    /// This session, told what changed by `fresh` instead of a walk.
    pub fn with_freshness(mut self, fresh: Box<dyn Freshness>) -> Session {
        self.fresh = RwLock::new(fresh);
        self.applied = Mutex::default();
        self.kept = Default::default();
        self
    }

    fn changes(&self) -> (Changes, Token) {
        read(&self.fresh).changes_since(*lock(&self.applied))
    }

    fn applied_up_to(&self, t: Token) {
        let mut a = lock(&self.applied);
        *a = (*a).max(t);
    }

    /// The canonical root.
    pub fn root(&self) -> PathBuf {
        read(&self.root).clone()
    }

    /// The watch's notes for the reads, from `open_watched`.
    fn notes(&self) -> Option<crate::fresh::Watching> {
        read(&self.notes).clone()
    }

    /// Readers opened at most.
    pub fn max_readers(&self) -> usize {
        self.max_readers
    }

    /// Mark the first build as started, before the thread that runs `build` is: a call that
    /// arrives meanwhile waits for it.
    pub fn building(&self) {
        *lock(&self.build) = Build::Building;
    }

    /// The first build: every file, waiting for another writer. A panic in it is an error, and
    /// `build_failed` says so until an update goes through.
    pub fn build(&self) -> Result<UpdateStats> {
        self.build_with(|repo| repo.update(false))
    }

    #[cfg(test)]
    pub(crate) fn build_failing_for_test(&self) {
        let _ = self.build_with(|_| panic!("a first build that panics"));
    }

    fn build_with(&self, f: impl FnOnce(&mut Repo) -> Result<UpdateStats>) -> Result<UpdateStats> {
        self.building();
        let r = self.write(f);
        let state = if r.as_ref().is_err_and(|e| e.is::<Panicked>()) { Build::Failed } else { Build::Built };
        *lock(&self.build) = state;
        self.built.notify_all();
        r
    }

    /// Whether the first build is running.
    pub fn is_building(&self) -> bool {
        *lock(&self.build) == Build::Building
    }

    /// Files the first build (and any other write of the session's own) has read, and has to read.
    pub fn build_count(&self) -> (usize, usize) {
        self.written.get()
    }

    /// Whether the first build panicked and no update went through since.
    pub fn build_failed(&self) -> bool {
        *lock(&self.build) == Build::Failed
    }

    /// Wait while the first build runs, at most `limit`, until `cancel` is set; `tick` runs
    /// every 20 ms with the time waited. -> whether the build is over.
    pub fn wait_built(&self, cancel: &AtomicBool, limit: Option<Duration>, mut tick: impl FnMut(Duration)) -> bool {
        let started = Instant::now();
        let mut state = lock(&self.build);
        while *state == Build::Building {
            if cancel.load(Ordering::Relaxed) || limit.is_some_and(|l| started.elapsed() >= l) {
                return false;
            }
            state = self.built.wait_timeout(state, Duration::from_millis(20)).unwrap_or_else(|p| p.into_inner()).0;
            tick(started.elapsed());
        }
        true
    }

    /// Run `f` on the repository writes use, with its panic kept inside this call.
    pub fn write<T>(&self, f: impl FnOnce(&mut Repo) -> Result<T>) -> Result<T> {
        let mut seed = lock(&self.seed);
        let r = caught(|| f(&mut seed));
        let updated = seed.take_updated();
        let names = updated.then(|| seed.ix.segments());
        drop(seed);
        if let Some(names) = names {
            self.release_idle(&names);
        }
        self.note_update(updated);
        r
    }

    /// Run one operation on an index scanned at most once per refresh interval (one that does
    /// not read the index is not scanned for), waiting up to `BUILD_WAIT` for another process's
    /// first build. A panic is the error `Panicked`.
    pub fn run(&self, op: &Op, cancel: &Arc<AtomicBool>) -> Result<Answer> {
        self.run_counted(op, cancel, &Arc::default()).map(|(a, _)| a)
    }

    /// `run`, counting the files the operation reads in `count` as it goes, with what its answer
    /// does not hold yet.
    pub fn run_counted(&self, op: &Op, cancel: &Arc<AtomicBool>, count: &Arc<FileCount>) -> Result<(Answer, Pending)> {
        self.waiting_for_others(cancel, || {
            self.call(cancel, Some(count), None, |repo| {
                let pending = if op.reads_index() { self.scan(repo, false)?.1 } else { Pending::default() };
                Ok((repo.run_as_is(op)?, pending))
            })
        })
    }

    /// Run `f` on the searcher, as `run` runs an operation: with `refresh` the index is scanned
    /// as for an operation; a damaged index is built again and `f` run once more.
    pub fn read<T>(&self, cancel: &Arc<AtomicBool>, refresh: bool, mut f: impl FnMut(&mut Searcher) -> Result<T>) -> Result<T> {
        if refresh {
            return self.read_pending(cancel, f).map(|(v, _)| v);
        }
        self.waiting_for_others(cancel, || self.call(cancel, None, None, |repo| repo.recovering(&mut f)))
    }

    /// `read` after a scan, with what the scan could not bring into the index yet.
    pub fn read_pending<T>(&self, cancel: &Arc<AtomicBool>, mut f: impl FnMut(&mut Searcher) -> Result<T>) -> Result<(T, Pending)> {
        self.waiting_for_others(cancel, || {
            self.call(cancel, None, None, |repo| {
                let (_, pending) = self.scan(repo, false)?;
                Ok((repo.recovering(&mut f)?, pending))
            })
        })
    }

    /// A command line call: every file is looked at first, as a fresh process does, then `op`
    /// runs; a write of this process is waited for as long as a read waits for one. None when the
    /// session cannot answer (nothing is committed while a first build runs, or a panic): the
    /// caller does the work.
    pub fn run_scanned(&self, op: &Op) -> Option<(UpdateStats, Result<Answer>)> {
        self.run_scanned_pending(op).map(|(st, a, _)| (st, a))
    }

    /// `run_scanned`, with what the answer does not hold yet.
    pub fn run_scanned_pending(&self, op: &Op) -> Option<(UpdateStats, Result<Answer>, Pending)> {
        self.run_scanned_for(op, None)
    }

    /// `run_scanned_pending` for a caller of the tree at `root`: None, with nothing run, when the
    /// reader in hand is another tree's (the session followed its link since the caller came).
    pub(crate) fn run_scanned_for(&self, op: &Op, root: Option<&Path>) -> Option<(UpdateStats, Result<Answer>, Pending)> {
        let r = self.call(&Arc::default(), None, None, |repo| {
            if root.is_some_and(|r| repo.ix.root != r) || self.is_building() && !repo.ix.has_files()? {
                return Ok(None);
            }
            let (st, pending) = if op.reads_index() { self.scan(repo, true)? } else { Default::default() };
            Ok(Some((st, repo.run_as_is(op), pending)))
        });
        r.ok().flatten()
    }

    /// Call `f` again while it meets another process's first build, up to `BUILD_WAIT`.
    fn waiting_for_others<T>(&self, cancel: &AtomicBool, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        let started = Instant::now();
        loop {
            match f() {
                Err(e) if e.is::<index::BeingBuilt>() && started.elapsed() < BUILD_WAIT && !cancel.load(Ordering::Relaxed) => {
                    std::thread::sleep(Duration::from_millis(200))
                }
                r => return r,
            }
        }
    }

    /// Run `f` on a reader of its own with `cancel` as its cancel flag, counting the files it
    /// reads in `count`. A reader whose call panicked is closed, not used again. `wait`: how long
    /// to wait for a reader when all are busy (None: as long as it takes, unless `cancel` is set).
    fn call<T>(&self, cancel: &Arc<AtomicBool>, count: Option<&Arc<FileCount>>, wait: Option<Duration>, f: impl FnOnce(&mut Repo) -> Result<T>) -> Result<T> {
        let Some((generation, mut repo)) = self.reader(cancel, wait)? else { return Err(anyhow::anyhow!("every reader is busy")) };
        repo.set_cancel(Arc::clone(cancel));
        repo.set_look(Some(Arc::clone(&self.kept)));
        if let Some(count) = count {
            repo.set_count(Arc::clone(count));
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut repo))) {
            Ok(r) => {
                repo.set_cancel(Arc::default());
                repo.set_count(Arc::default());
                let updated = repo.take_updated();
                self.give_back(generation, Some(repo));
                self.note_update(updated);
                r
            }
            Err(_) => {
                drop(repo);
                self.give_back(generation, None);
                Err(Panicked.into())
            }
        }
    }

    /// A reader, with the pool's generation when it was taken or opened.
    fn reader(&self, cancel: &AtomicBool, wait: Option<Duration>) -> Result<Option<(u64, Repo)>> {
        let started = Instant::now();
        let mut pool = lock(&self.pool);
        loop {
            let generation = pool.generation;
            if let Some(repo) = pool.idle.pop() {
                return Ok(Some((generation, repo)));
            }
            if pool.open < self.max_readers {
                pool.open += 1;
                drop(pool);
                let opened = caught(|| lock(&self.opener).another());
                if opened.is_err() {
                    self.give_back(generation, None);
                }
                return opened.map(|r| Some((generation, r)));
            }
            if cancel.load(Ordering::Relaxed) || wait.is_some_and(|w| started.elapsed() >= w) {
                return Ok(None);
            }
            pool = self.freed.wait_timeout(pool, Duration::from_millis(20)).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    /// The idle readers close their segment store unless it holds `names`, the segments of the
    /// index now.
    fn release_idle(&self, names: &[String]) {
        for r in lock(&self.pool).idle.iter_mut() {
            r.ix.release_segments_other_than(names);
        }
    }

    /// The idle readers, and the writes' repository when no write runs, close their segment
    /// store unless it holds the segments the index names now, which another process may have
    /// changed.
    fn release_replaced(&self) {
        let mut pool = lock(&self.pool);
        let Some(names) = pool.idle.iter().find(|r| r.ix.held_segments().is_some()).map(|r| r.ix.segments()) else { return };
        pool.idle.iter_mut().for_each(|r| r.ix.release_segments_other_than(&names));
        drop(pool);
        if let Ok(mut seed) = self.seed.try_lock() {
            seed.ix.release_segments_other_than(&names);
        }
    }

    /// A reader back to the pool, or None for one that was closed. What it just read is the
    /// newest the others can know of: one that holds other segments lets them go.
    fn give_back(&self, generation: u64, repo: Option<Repo>) {
        let mut pool = lock(&self.pool);
        match repo.filter(|_| generation == pool.generation) {
            Some(r) => {
                if let Some(names) = r.ix.held_segments() {
                    pool.idle.iter_mut().for_each(|i| i.ix.release_segments_other_than(names));
                }
                pool.idle.push(r)
            }
            None => pool.open -= 1,
        }
        drop(pool);
        self.freed.notify_one();
    }

    fn note_update(&self, updated: bool) {
        if updated {
            let mut b = lock(&self.build);
            if *b == Build::Failed {
                *b = Build::Built;
            }
        }
    }

    /// The scan before a read: the files of the change set, or, when it is every file, a walk once
    /// the refresh interval has passed since the last one (at once with `now`). A write of this
    /// process is waited for up to `OWN_WRITE_WAIT`; what could not be applied by then is pending.
    fn scan(&self, repo: &mut Repo, now: bool) -> Result<(UpdateStats, Pending)> {
        let (changes, token) = self.changes();
        crate::files::patch_look(&self.root(), &self.kept, &changes);
        let Changes::Named(n) = changes else { return self.walk_if_due(repo, token, now) };
        let mut st = UpdateStats::default();
        if !n.code.is_empty() {
            st = repo.refresh_named(&n.code)?.unwrap_or_default();
            if repo.is_stale() {
                // not applied: the next call asks for these files again
                return Ok((st, Pending(Some(Changes::Named(n)))));
            }
        }
        self.applied_up_to(token);
        Ok((st, Pending::default()))
    }

    /// A watch that asks for a walk gets it at once, unless the interval is a floor the user gave
    /// and the watch's change sets are a proof (the floor holds back what they cannot vouch for).
    fn walk_if_due(&self, repo: &mut Repo, token: Token, now: bool) -> Result<(UpdateStats, Pending)> {
        let (watching, owed, proves) = {
            let f = read(&self.fresh);
            (f.watching(), f.owes_a_walk(), f.order_proves())
        };
        let asked = now || owed || watching && (!self.every.floor || !proves);
        let nothing = Ok((UpdateStats::default(), Pending::default()));
        if !asked && !self.due() {
            debug("the refresh interval holds that walk back");
            // what the watch named is applied whatever the interval; the walk stays owed
            let applied = *lock(&self.applied);
            let named = read(&self.fresh).named_since(applied);
            if let Some(n) = named.filter(|n| !n.code.is_empty()) {
                let st = repo.refresh_named(&n.code)?.unwrap_or_default();
                return Ok((st, Pending(repo.is_stale().then_some(Changes::Named(n)))));
            }
            return nothing;
        }
        let Some(_scanning) = stores::hold_within(&self.scanning, stores::OWN_WRITE_WAIT) else {
            return Ok((UpdateStats::default(), Pending(Some(Changes::All))));
        };
        // only a watch's tokens move: without one every token is the first
        let walked_meanwhile = asked && watching && *lock(&self.applied) >= token;
        if walked_meanwhile || !asked && !self.due() {
            return nothing;
        }
        let t = Instant::now();
        let r = repo.refresh(0.0);
        let walked = r.is_ok() && !repo.is_stale();
        if walked {
            self.applied_up_to(token);
        }
        // a scan that could not run does not start a floor's interval: the next call tries again
        self.scanned(t, walked || !self.every.floor);
        let st = r?.unwrap_or_default();
        Ok((st, Pending((!walked).then_some(Changes::All))))
    }

    fn due(&self) -> bool {
        let c = lock(&self.clock);
        let wait = if self.every.floor { self.every.seconds } else { self.every.seconds.min(10.0 * c.cost) };
        c.last.is_none_or(|t| t.elapsed().as_secs_f64() >= wait)
    }

    fn scanned(&self, started: Instant, starts_interval: bool) {
        let mut c = lock(&self.clock);
        c.cost = started.elapsed().as_secs_f64();
        if starts_interval {
            c.last = Some(Instant::now());
        }
    }

    /// No write starts after this, until the process exits: the first build is waited for, then
    /// the writer lock is taken and kept, so an exit cuts no transaction.
    pub fn hold_writes(&self) {
        self.wait_built(&AtomicBool::new(false), None, |_| {});
        std::mem::forget(lock(&self.writer));
    }
}

/// Whether the command line may use a running session (`SCRUTATE_NO_SESSION` turns it off).
pub fn wanted() -> bool {
    std::env::var_os("SCRUTATE_NO_SESSION").is_none_or(|v| v.is_empty() || v == "0")
}

fn debug(what: &str) {
    if std::env::var_os("SCRUTATE_SESSION_DEBUG").is_some_and(|v| !v.is_empty() && v != "0") {
        eprintln!("[scrutate] session: {what}");
    }
}

/// What a command line call asks a session: its operation and how to print the answer.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub op: Op,
    pub json: bool,
    pub max_bytes: usize,
    pub offset: usize,
    pub outside: Option<String>,
    /// a ranked search's `--why`
    pub why: bool,
}

/// The printed answer: standard output, standard error and the exit code, as the command line
/// prints them; `scan` is what the scan before it updated.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Printed {
    pub out: String,
    pub err: String,
    pub code: i32,
}

#[cfg(unix)]
#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct Reply {
    printed: Option<Printed>,
    changed: usize,
    removed: usize,
    chunks: i64,
    seconds: f64,
}

#[cfg(unix)]
/// Everything an answer depends on besides the files and the request: a session answers only a
/// command line whose identity is the same.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct Identity {
    version: String,
    /// the executable file: device, inode, size, modification time
    build: String,
    schema: u32,
    content: u32,
    models: String,
    env: Vec<(String, String)>,
    root: String,
}

#[cfg(unix)]
/// Variables that do not change an answer through a session: the root is compared on its own, the
/// size of an answer travels with each request, and these switches are the command line's own.
const NOT_ANSWERS: &[&str] =
    &["SCRUTATE_ROOT", "SCRUTATE_NO_SESSION", "SCRUTATE_SESSION_DEBUG", "SCRUTATE_SESSION_WAIT_MS", "SCRUTATE_MAX_BYTES", "SCRUTATE_REFRESH_INTERVAL"];

#[cfg(unix)]
impl Identity {
    fn here(root: &Path) -> Option<Identity> {
        let file_id = |p: &Path| -> Option<String> {
            let m = std::fs::metadata(p).ok()?;
            let s = index::Stat::of(p, &m);
            use std::os::unix::fs::MetadataExt;
            let (dev, ino) = (m.dev(), m.ino());
            Some(format!("{dev} {ino} {} {} {}", s.size, s.mtime, s.ctime))
        };
        let models = crate::models::ModelSet::from_env().signature(&root.join(crate::files::INDEX_DIR)).ok()?;
        let ranker = match std::env::var_os("SCRUTATE_RANKER") {
            Some(p) if !p.is_empty() && p != "none" => file_id(Path::new(&p))?,
            _ => String::new(),
        };
        let mut env: Vec<(String, String)> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.to_string_lossy().into_owned())))
            .filter(|(k, _)| k.starts_with("SCRUTATE_") && !NOT_ANSWERS.contains(&k.as_str()))
            .collect();
        env.sort();
        Some(Identity {
            version: crate::VERSION.to_string(),
            build: file_id(&std::env::current_exe().ok()?)?,
            schema: index::SCHEMA_VERSION,
            content: index::CONTENT_REVISION,
            models: format!("{models} {ranker}"),
            env,
            root: root.to_string_lossy().into_owned(),
        })
    }
}

/// The socket of a session of `root` (canonical).
pub fn socket_path(root: &Path) -> PathBuf {
    root.join(crate::files::INDEX_DIR).join("session.sock")
}

#[cfg(unix)]
pub use unix::{ask, listen, Listening};

/// No session use on Windows: a command line call does the work itself.
#[cfg(not(unix))]
pub fn ask(_root: &Path, _request: &Request) -> Result<Option<(Printed, UpdateStats)>> {
    Ok(None)
}

#[cfg(not(unix))]
pub struct Listening;

#[cfg(not(unix))]
pub fn listen(_session: &Arc<Session>, _answer: fn(Answer, &Request, &Path) -> Result<Printed>) -> Option<Listening> {
    None
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::atomic::AtomicUsize;

    /// How long the command line waits for a session's greeting.
    const HELLO: Duration = Duration::from_millis(20);

    /// `HELLO`, or `SCRUTATE_SESSION_WAIT_MS` for a machine too busy to greet that fast.
    fn hello() -> Duration {
        std::env::var("SCRUTATE_SESSION_WAIT_MS").ok().and_then(|v| v.parse().ok()).map_or(HELLO, Duration::from_millis)
    }
    /// How long the command line waits for an answer before it does the work itself.
    const ANSWER: Duration = Duration::from_secs(60);
    /// How long a session waits for a request after its greeting, and a command line for the
    /// session to take it.
    const REQUEST: Duration = Duration::from_secs(2);
    const MAX_REQUEST: u64 = 1 << 20;
    /// Connections served at once; one more is closed, and its command line does the work.
    const MAX_CONNECTIONS: usize = 16;

    extern "C" {
        fn geteuid() -> u32;
        fn fchdir(fd: i32) -> i32;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        fn getpeereid(fd: i32, uid: *mut u32, gid: *mut u32) -> i32;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        fn getsockopt(fd: i32, level: i32, name: i32, value: *mut std::ffi::c_void, len: *mut u32) -> i32;
    }

    /// The user at the other end of a connected socket, as the system knows it.
    fn peer_uid(s: &UnixStream) -> Option<u32> {
        let fd = s.as_raw_fd();
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let (mut uid, mut gid) = (u32::MAX, u32::MAX);
            (unsafe { getpeereid(fd, &mut uid, &mut gid) } == 0).then_some(uid)
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            // struct ucred { pid_t pid; uid_t uid; gid_t gid; }; SOL_SOCKET 1, SO_PEERCRED 17
            let mut cred = [0u32; 3];
            let mut len = std::mem::size_of_val(&cred) as u32;
            (unsafe { getsockopt(fd, 1, 17, cred.as_mut_ptr().cast(), &mut len) } == 0 && len as usize == std::mem::size_of_val(&cred)).then_some(cred[1])
        }
    }

    /// The process could not go back to the working directory `short` left.
    #[derive(Debug)]
    pub(super) struct NotBack(std::io::Error);

    impl std::fmt::Display for NotBack {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "cannot go back to the working directory after reaching the session socket: {}", self.0)
        }
    }

    impl std::error::Error for NotBack {}

    fn not_back(e: &std::io::Error) -> bool {
        e.get_ref().is_some_and(|i| i.is::<NotBack>())
    }

    /// `f` of the socket at `path`. A path too long for a socket address (103 bytes on macOS, 107
    /// on Linux) is named from its directory, which this process enters for the call and leaves
    /// through a descriptor of the directory it was in, so a renamed or unreachable path does not
    /// matter: so only while it runs no other thread (a command line call, a server before it
    /// starts any). An error that holds `NotBack`: the process is still in the socket's directory.
    pub(super) fn short<T>(path: &Path, f: impl FnOnce(&Path) -> std::io::Result<T>) -> std::io::Result<T> {
        const ROOM: usize = if cfg!(any(target_os = "linux", target_os = "android")) { 107 } else { 103 };
        if path.as_os_str().len() <= ROOM {
            return f(path);
        }
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return f(path) };
        let here = std::fs::File::open(".")?;
        std::env::set_current_dir(dir)?;
        let r = f(Path::new(name));
        if unsafe { fchdir(here.as_raw_fd()) } != 0 {
            return Err(std::io::Error::other(NotBack(std::io::Error::last_os_error())));
        }
        r
    }

    fn same_user(s: &UnixStream) -> bool {
        peer_uid(s) == Some(unsafe { geteuid() })
    }

    /// A socket file this user owns, not a link and not reached through one in `.scrutate`.
    fn ours(path: &Path) -> bool {
        let me = unsafe { geteuid() };
        let dir = path.parent().and_then(|d| std::fs::symlink_metadata(d).ok());
        let sock = std::fs::symlink_metadata(path).ok();
        dir.is_some_and(|d| d.is_dir() && d.uid() == me) && sock.is_some_and(|s| s.file_type().is_socket() && s.uid() == me)
    }

    /// The session's end: the socket file is removed when this is dropped, if it is still the one
    /// this session made.
    pub struct Listening {
        path: PathBuf,
        id: (u64, u64),
    }

    impl Drop for Listening {
        fn drop(&mut self) {
            if std::fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.id) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    /// Offer `session` to command line calls of the same user, until the process exits. None when
    /// it cannot (the socket path is too long, another session of this root is running, the
    /// index directory holds something else there): command line calls then do their own work.
    /// `answer` prints an answer as the command line does.
    pub fn listen(session: &Arc<Session>, answer: fn(Answer, &Request, &Path) -> Result<Printed>) -> Option<Listening> {
        let root = session.root();
        let path = socket_path(&root);
        let Some(me) = Identity::here(&root) else {
            debug("no identity to offer");
            return None;
        };
        if std::fs::symlink_metadata(&path).is_ok() {
            if !ours(&path) {
                debug(&format!("{} is not a socket of this user; not listening", path.display()));
                return None;
            }
            match short(&path, |p| UnixStream::connect(p)) {
                Ok(_) => {
                    debug("another session of this root is listening");
                    return None;
                }
                Err(e) if not_back(&e) => {
                    debug(&e.to_string());
                    return None;
                }
                Err(_) => {}
            }
            let _ = std::fs::remove_file(&path); // left by a session that ended without removing it
        }
        let listener = match short(&path, |p| UnixListener::bind(p)) {
            Ok(l) => l,
            Err(e) => {
                if not_back(&e) {
                    // bound, maybe, but a server in the wrong directory offers nothing
                    let _ = std::fs::remove_file(&path);
                }
                debug(&format!("cannot listen at {}: {e}", path.display()));
                return None;
            }
        };
        let md = std::fs::symlink_metadata(&path).ok()?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let hello = serde_json::to_string(&me).ok()?;
        *lock(&session.listening) = Some(path.clone());
        let session = Arc::clone(session);
        let open = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("scrutate-session".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    let Ok(conn) = conn else { continue };
                    // another user's process gets nothing, not even the greeting; once the session
                    // has followed its root's link to another tree, no caller of this tree does
                    if !same_user(&conn) || open.load(Ordering::Acquire) >= MAX_CONNECTIONS || session.root() != root {
                        continue;
                    }
                    open.fetch_add(1, Ordering::AcqRel);
                    let (session, hello, done, root) = (Arc::clone(&session), hello.clone(), Arc::clone(&open), root.clone());
                    let served = std::thread::Builder::new().name("scrutate-session-call".into()).spawn(move || {
                        let _ = serve_one(&session, conn, &hello, &root, answer);
                        done.fetch_sub(1, Ordering::AcqRel);
                    });
                    if served.is_err() {
                        open.fetch_sub(1, Ordering::AcqRel);
                    }
                }
            })
            .ok()?;
        Some(Listening { path, id: (md.dev(), md.ino()) })
    }

    /// One caller of the session listening for `root`: greeted, its request read and answered.
    pub(super) fn serve_one(
        session: &Session,
        conn: UnixStream,
        hello: &str,
        root: &Path,
        answer: fn(Answer, &Request, &Path) -> Result<Printed>,
    ) -> Result<()> {
        let mut w = conn.try_clone()?;
        writeln!(w, "{hello}")?;
        conn.set_read_timeout(Some(REQUEST))?;
        let mut line = Vec::new();
        BufReader::new(std::io::Read::take(&conn, MAX_REQUEST)).read_until(b'\n', &mut line)?;
        let request: Request = serde_json::from_slice(&line)?;
        // greeted for `root`, which the session may have left since for another tree; checked
        // again on the reader the call runs on, which a move may swap in after this
        if session.root() != root {
            return Ok(());
        }
        let printed = session.run_scanned_for(&request.op, Some(root)).and_then(|(st, a, pending)| {
            let mut p = answer(a.ok()?, &request, &session.root()).ok()?;
            if let Some(note) = pending.note() {
                p.err.push_str(&format!("[scrutate] {note}\n"));
            }
            Some((st, p))
        });
        let reply = match printed {
            Some((st, p)) => Reply { printed: Some(p), changed: st.files_changed, removed: st.files_removed, chunks: st.chunks, seconds: st.seconds },
            None => Reply { printed: None, changed: 0, removed: 0, chunks: 0, seconds: 0.0 },
        };
        writeln!(w, "{}", serde_json::to_string(&reply)?)?;
        Ok(())
    }

    /// The answer of the session running on `root` (canonical) to `request`, printed as the
    /// command line prints it, with what the scan before it updated. None, quietly, when there is
    /// no session or anything about it is not as this process would do the work. An error only
    /// when this process could not go back to its working directory: it must stop.
    pub fn ask(root: &Path, request: &Request) -> Result<Option<(Printed, UpdateStats)>> {
        let path = socket_path(root);
        if std::fs::symlink_metadata(&path).is_err() {
            return Ok(None);
        }
        let mut lost = None;
        let r = asked(&path, root, request, &mut lost);
        if let Some(e) = lost {
            return Err(e.into());
        }
        if let Err(why) = &r {
            debug(why);
        }
        Ok(r.ok())
    }

    fn asked(path: &Path, root: &Path, request: &Request, lost: &mut Option<std::io::Error>) -> std::result::Result<(Printed, UpdateStats), String> {
        if !ours(path) {
            return Err(format!("{} is not a socket of this user", path.display()));
        }
        let conn = short(path, |p| UnixStream::connect(p)).map_err(|e| {
            let why = format!("no session at {}: {e}", path.display());
            if not_back(&e) {
                *lost = Some(e);
            }
            why
        })?;
        if !same_user(&conn) {
            return Err("the session runs as another user".into());
        }
        conn.set_read_timeout(Some(hello())).map_err(|e| e.to_string())?;
        let mut r = BufReader::new(&conn);
        let mut line = String::new();
        r.read_line(&mut line).map_err(|e| format!("no greeting: {e}"))?;
        let theirs: Identity = serde_json::from_str(&line).map_err(|e| format!("greeting: {e}"))?;
        let me = Identity::here(root).ok_or("no identity here")?;
        if theirs != me {
            return Err(format!("the session differs: {theirs:?} where this is {me:?}"));
        }
        let mut w = &conn;
        // a session that stops reading leaves a request over the socket's buffer unwritten
        conn.set_write_timeout(Some(REQUEST)).map_err(|e| e.to_string())?;
        writeln!(w, "{}", serde_json::to_string(request).map_err(|e| e.to_string())?).map_err(|e| format!("request not taken: {e}"))?;
        conn.set_read_timeout(Some(ANSWER)).map_err(|e| e.to_string())?;
        line.clear();
        r.read_line(&mut line).map_err(|e| format!("no answer: {e}"))?;
        let reply: Reply = serde_json::from_str(&line).map_err(|e| format!("answer: {e}"))?;
        let printed = reply.printed.ok_or("the session did not answer this call")?;
        debug("answered by the session");
        let st = UpdateStats { files_changed: reply.changed, files_removed: reply.removed, chunks: reply.chunks, seconds: reply.seconds, ..Default::default() };
        Ok((printed, st))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A session that watches the tree answers `grep_code` from its kept look, patched with each
    /// change set, as a look of the tree each call answers it, through edits, creates, deletes
    /// and renames of code, text, binary and unindexed files; and it takes no look meanwhile.
    /// Under a floor (`--refresh-interval`) a new directory's files reach grep_code at once, by the
    /// look it takes while the watches are not set again, and the code index at the next scan.
    /// When that scan comes is the floor's: here it is asked for (`reindex`).
    #[test]
    fn under_a_floor_a_new_directory_reaches_grep_code_at_once_and_the_code_index_at_the_next_scan() {
        let dir = std::env::temp_dir().join(format!("scrutate_floor_new_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(root.join("a.py"), "def foo():\n    return 1\n").unwrap();
        {
            let mut repo = crate::stores::Repo::open(&root).unwrap();
            repo.update_stores(crate::stores::Writer::Index, &crate::stores::Changes::All, false).unwrap();
        }
        let watched = Session::open_watched(&root, Every::given(Some(3600.0))).unwrap();
        // a test of the change set, which macOS reads do without (every read looks there)
        if let Some(notes) = watched.notes() {
            notes.set_order_proves(true);
        }
        watched.watch(false).unwrap();
        let t0 = Instant::now();
        while !read(&watched.fresh).watching() {
            assert!(t0.elapsed() < Duration::from_secs(20), "the watch never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let settings = crate::mcp::Settings::new(None);
        let call = |tool: &str, args: serde_json::Value| crate::mcp::call_tool(&watched, tool, &args, &settings, &Arc::default()).text;
        // the walk that follows the watches coming up is never held back
        call("grep_code", serde_json::json!({"pattern": "foo"}));
        std::fs::create_dir_all(root.join("sub/new")).unwrap();
        std::fs::write(root.join("sub/new/y.py"), "def fn_new_y():\n    return 'tok_new_y'\n").unwrap();
        let grep = call("grep_code", serde_json::json!({"pattern": "tok_new_y"}));
        assert!(grep.contains("sub/new/y.py"), "grep_code: {grep}");
        call("reindex", serde_json::json!({}));
        let symbol = call("find_symbol", serde_json::json!({"name": "fn_new_y"}));
        assert!(symbol.contains("sub/new/y.py"), "the code index after the scan: {symbol}");
        drop(watched);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory an ignore rule leaves out, made while the watch runs, gets no watch and leaves
    /// the reads on their change sets: after the call that sees it, no call looks at the tree.
    #[test]
    fn a_new_ignored_directory_leaves_reads_on_the_change_set() {
        let dir = std::env::temp_dir().join(format!("scrutate_ignored_new_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        let root = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(root.join(".gitignore"), "out2/\n").unwrap();
        std::fs::write(root.join("a.py"), "def foo():\n    return 1\n").unwrap();
        {
            let mut repo = crate::stores::Repo::open(&root).unwrap();
            repo.update_stores(crate::stores::Writer::Index, &crate::stores::Changes::All, false).unwrap();
        }
        let watched = Session::open_watched(&root, Every::given(None)).unwrap();
        // a test of the change set, which macOS reads do without (every read looks there)
        if let Some(notes) = watched.notes() {
            notes.set_order_proves(true);
        }
        watched.watch(false).unwrap();
        let t0 = Instant::now();
        while !read(&watched.fresh).watching() {
            assert!(t0.elapsed() < Duration::from_secs(20), "the watch never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let settings = crate::mcp::Settings::new(None);
        let looks = || crate::files::LOOKS.with(|c| c.get());
        let call = || crate::mcp::call_tool(&watched, "grep_code", &serde_json::json!({"pattern": "foo"}), &settings, &Arc::default());
        call();
        std::fs::create_dir_all(root.join("out2")).unwrap();
        std::fs::write(root.join("out2/x.py"), "x = 'foo'\n").unwrap();
        let mut per_call = Vec::new();
        for i in 0..6 {
            std::fs::write(root.join("a.py"), format!("def foo():\n    return {i}\n")).unwrap();
            let before = looks();
            call();
            per_call.push(looks() - before);
        }
        assert!(per_call[1..].iter().all(|&n| n == 0), "looks per call: {per_call:?}");
        drop(watched);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn grep_code_from_the_kept_look_answers_as_a_look_each_call_does() {
        for with_text in [true, false] {
            let dir = std::env::temp_dir().join(format!("scrutate_kept_look_{}_{}", std::process::id(), with_text));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("sub")).unwrap();
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            let root = std::fs::canonicalize(&dir).unwrap();
            let w = |rel: &str, body: &[u8]| std::fs::write(root.join(rel), body).unwrap();
            w("a.py", b"def foo():\n    return 1\n");
            w("b.txt", b"foo bar_1 here\n");
            w("sub/c.rs", b"fn bar_2() {}\n");
            {
                let mut repo = crate::stores::Repo::open(&root).unwrap();
                // without a text index a lockfile or a binary is in no index: a burst names it for the look alone
                let writer = if with_text { crate::stores::Writer::Index } else { crate::stores::Writer::IndexCode };
                repo.update_stores(writer, &crate::stores::Changes::All, false).unwrap();
            }
            let watched = Session::open_watched(&root, Every::given(None)).unwrap();
            // a test of the change set, which macOS reads do without (every read looks there)
            if let Some(notes) = watched.notes() {
                notes.set_order_proves(true);
            }
            watched.watch(false).unwrap();
            let t0 = Instant::now();
            while !read(&watched.fresh).watching() {
                assert!(t0.elapsed() < Duration::from_secs(20), "the watch never started");
                std::thread::sleep(Duration::from_millis(20));
            }
            let plain = Session::open(&root, Every::given(Some(0.0))).unwrap();
            let settings = crate::mcp::Settings::new(None);
            let looks = || crate::files::LOOKS.with(|c| c.get());
            let watched_looks = std::cell::Cell::new(0);
            let compare = |step: &str| {
                for q in ["foo", "bar_[0-9]+", "zzz_no_match_anywhere", "PNG", "lock foo"] {
                    let args = serde_json::json!({"pattern": q});
                    let before = looks();
                    let a = crate::mcp::call_tool(&watched, "grep_code", &args, &settings, &Arc::default());
                    watched_looks.set(watched_looks.get() + looks() - before);
                    let b = crate::mcp::call_tool(&plain, "grep_code", &args, &settings, &Arc::default());
                    assert_eq!(a.text, b.text, "{step}: {q} (text index: {with_text})");
                }
            };
            compare("built");
            let first = watched_looks.get();
            let steps: [(&str, &dyn Fn()); 10] = [
                ("edit code", &|| w("a.py", b"def foo():\n    return bar_7\n")),
                ("create code", &|| w("sub/new.py", b"x = 'bar_3 foo'\n")),
                ("create binary", &|| w("pic.png", b"\x89PNG\x00\x01foo bar_4\n")),
                ("create lockfile", &|| w("Cargo.lock", b"# lock foo\nbar_5\n")),
                ("create hidden", &|| w(".hidden.txt", b"foo hidden bar_6\n")),
                ("edit binary", &|| w("pic.png", b"\x89PNG\x00\x02foo\n")),
                ("delete text", &|| std::fs::remove_file(root.join("b.txt")).unwrap()),
                ("rename code", &|| std::fs::rename(root.join("sub/c.rs"), root.join("sub/d.rs")).unwrap()),
                ("delete lockfile", &|| std::fs::remove_file(root.join("Cargo.lock")).unwrap()),
                ("edit hidden", &|| w(".hidden.txt", b"nothing here\n")),
            ];
            for (step, change) in steps {
                change();
                compare(step);
            }
            assert_eq!(watched_looks.get(), first, "the watched session looked at the tree again");
            // a directory made after the watch started, then a second file in it before the watch
            // has one on it: until the watches are set again, reads look at the tree
            std::fs::create_dir_all(root.join("sub/new")).unwrap();
            w("sub/new/x.py", b"x = 'foo bar_8'\n");
            compare("make directory");
            w("sub/new/y.py", b"y = 'foo bar_9'\n");
            compare("write in new directory");
            drop((watched, plain));
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// A repository of one function, its session, and its first build done.
    fn fixture(name: &str) -> (PathBuf, Session) {
        let root = std::env::temp_dir().join(format!("scrutate_session_{name}_{}", std::process::id()));
        crate::claim_test_tree(&root);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let s = Session::open(&root, Every::given(Some(3600.0))).unwrap();
        s.build().unwrap();
        (root, s)
    }

    /// A write of `s` (one that holds the writer lock, as an update does) running for `held` in
    /// another thread of this process while `f` runs.
    fn while_own_write_runs<T>(s: &Session, held: Duration, f: impl FnOnce() -> T) -> T {
        let ((taken, is_taken), (release, released)) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        std::thread::scope(|sc| {
            sc.spawn(move || {
                s.write(|repo| {
                    let writer = repo.writer();
                    let _w = writer.lock().unwrap();
                    taken.send(()).unwrap();
                    let _ = released.recv_timeout(held);
                    Ok(())
                })
                .unwrap();
            });
            is_taken.recv().unwrap();
            let r = f();
            let _ = release.send(()); // a short write may be over already
            r
        })
    }

    /// A read while a write of this process runs on (one that waits for another process's lock,
    /// as `serve --watch` does) answers from the committed index once the wait is over, and says
    /// which files it does not hold yet.
    #[test]
    fn a_read_does_not_wait_for_a_write_of_this_process() {
        let (root, s) = watched("own_writer", 1, Every::given(Some(0.0)));
        std::fs::write(root.join("src/d0/late.py"), "def late():\n    return 2\n").unwrap();
        let (took, r) = while_own_write_runs(&s, Duration::from_secs(20), || {
            let t0 = Instant::now();
            let r = s.run_counted(&Op::Symbol { name: "late".into(), limit: 1, exact: true }, &Arc::default(), &Arc::default());
            (t0.elapsed(), r)
        });
        let (a, pending) = r.unwrap();
        assert!(a.is_empty(), "answered from the committed index");
        assert_eq!(pending.files(), Some(&["src/d0/late.py".to_string()][..]));
        assert!(pending.note().is_some_and(|n| n.contains("src/d0/late.py")));
        // the empty answer and the pending file show the write was not waited out: that read
        // would hold late.py; the wait itself was taken
        assert!(took >= stores::OWN_WRITE_WAIT, "{took:?}");
        let (a, pending) = s.run_counted(&Op::Symbol { name: "late".into(), limit: 1, exact: true }, &Arc::default(), &Arc::default()).unwrap();
        assert!(!a.is_empty() && pending == Pending::default(), "the next read applies them");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A write of this process that ends within the wait is waited for: the read's answer holds
    /// the files of its change set, and nothing is pending.
    #[test]
    fn a_read_waits_out_a_short_write_of_this_process() {
        let (root, s) = watched("short_write", 1, Every::given(Some(0.0)));
        std::fs::write(root.join("src/d0/soon.py"), "def soon():\n    return 2\n").unwrap();
        let (a, pending) = while_own_write_runs(&s, Duration::from_millis(50), || {
            s.run_counted(&Op::Symbol { name: "soon".into(), limit: 1, exact: true }, &Arc::default(), &Arc::default())
        })
        .unwrap();
        assert!(!a.is_empty(), "the answer holds the file the write held back");
        assert_eq!(pending, Pending::default());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A command line call is answered while a write of this process runs, where the session
    /// refused it and the command line did the work itself; a walk that could not run is pending.
    #[test]
    fn a_command_line_call_during_a_write_of_this_process_is_answered() {
        let (root, s) = fixture("scanned_during_write");
        std::fs::write(root.join("src/new.py"), "def brand_new():\n    return 3\n").unwrap();
        let op = Op::Symbol { name: "make_point".into(), limit: 5, exact: true };
        let r = while_own_write_runs(&s, Duration::from_secs(20), || s.run_scanned_pending(&op));
        let (_, a, pending) = r.expect("answered");
        assert!(a.is_ok_and(|a| !a.is_empty()));
        assert_eq!(pending, Pending(Some(Changes::All)));
        assert!(pending.note().is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A reader taken out on the old tree while the session follows its link to another one is
    /// not given back, and the next call answers from the new tree.
    #[cfg(unix)]
    #[test]
    fn a_reader_out_while_the_root_moves_to_another_tree_is_not_given_back() {
        let base = std::env::temp_dir().join(format!("scrutate_session_moves_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for (dir, body) in [("v1", "def only_v1():\n    return 1\n"), ("v2", "def only_v2():\n    return 2\n")] {
            std::fs::create_dir_all(base.join(dir).join(".git")).unwrap();
            std::fs::write(base.join(dir).join("a.py"), body).unwrap();
        }
        let cur = base.join("cur");
        std::os::unix::fs::symlink("v1", &cur).unwrap();
        let s = Session::open(&cur, Every::given(Some(3600.0))).unwrap();
        s.build().unwrap();
        s.read(&Arc::default(), false, |_| {
            std::os::unix::fs::symlink("v2", base.join("cur.tmp")).unwrap();
            std::fs::rename(base.join("cur.tmp"), &cur).unwrap();
            s.reopen()
        })
        .unwrap();
        assert!(lock(&s.pool).idle.is_empty(), "the old tree's reader went back into the pool");
        assert_eq!(s.root(), std::fs::canonicalize(base.join("v2")).unwrap());
        let symbol = |name: &str| Op::Symbol { name: name.into(), limit: 1, exact: true };
        let (_, a) = s.run_scanned(&symbol("only_v2")).unwrap();
        assert!(a.is_ok_and(|a| !a.is_empty()), "the new tree's name");
        let (_, a) = s.run_scanned(&symbol("only_v1")).unwrap();
        assert!(a.is_ok_and(|a| a.is_empty()), "the old tree's name");
        let _ = std::fs::remove_dir_all(base);
    }

    /// A call for one tree is run only on a reader of that tree: once the session followed its
    /// link to another, a call for the old tree runs nothing, whatever the root read before it.
    #[cfg(unix)]
    #[test]
    fn a_call_for_the_old_tree_runs_on_no_reader_of_the_new_one() {
        let base = std::env::temp_dir().join(format!("scrutate_session_reader_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for dir in ["v1", "v2"] {
            std::fs::create_dir_all(base.join(dir).join(".git")).unwrap();
            std::fs::write(base.join(dir).join("a.py"), "def f():\n    return 1\n").unwrap();
        }
        let cur = base.join("cur");
        std::os::unix::fs::symlink("v1", &cur).unwrap();
        let s = Session::open(&cur, Every::given(Some(3600.0))).unwrap();
        s.build().unwrap();
        let old = s.root();
        let op = Op::Symbol { name: "f".into(), limit: 1, exact: true };
        assert!(s.run_scanned_for(&op, Some(&old)).is_some(), "the old tree's call while the root is the old tree");
        std::os::unix::fs::symlink("v2", base.join("cur.tmp")).unwrap();
        std::fs::rename(base.join("cur.tmp"), &cur).unwrap();
        assert!(s.reopen().unwrap());
        assert!(s.run_scanned_for(&op, Some(&old)).is_none(), "the old tree's call ran on the new tree's reader");
        assert!(s.run_scanned_for(&op, Some(&s.root())).is_some(), "the new tree's call");
        let _ = std::fs::remove_dir_all(base);
    }

    /// A caller greeted for the old tree before the session followed its link to another one
    /// gets no answer, whenever its request comes.
    #[cfg(unix)]
    #[test]
    fn a_caller_greeted_before_the_root_moved_is_not_answered_from_the_new_tree() {
        use std::io::{Read, Write};
        fn answered(_: Answer, _: &Request, _: &Path) -> Result<Printed> {
            Ok(Printed { out: "answered\n".into(), ..Default::default() })
        }
        let base = std::env::temp_dir().join(format!("scrutate_session_greeted_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for dir in ["v1", "v2"] {
            std::fs::create_dir_all(base.join(dir).join(".git")).unwrap();
            std::fs::write(base.join(dir).join("a.py"), "def f():\n    return 1\n").unwrap();
        }
        let cur = base.join("cur");
        std::os::unix::fs::symlink("v1", &cur).unwrap();
        let s = Session::open(&cur, Every::given(Some(3600.0))).unwrap();
        s.build().unwrap();
        let old = s.root();
        let request = Request { op: Op::Symbol { name: "f".into(), limit: 1, exact: true }, json: false, max_bytes: 0, offset: 0, outside: None, why: false };
        let replies = |s: &Session| {
            let (mut caller, server) = std::os::unix::net::UnixStream::pair().unwrap();
            writeln!(caller, "{}", serde_json::to_string(&request).unwrap()).unwrap();
            unix::serve_one(s, server, "{}", &old, answered).unwrap();
            let mut said = String::new();
            caller.read_to_string(&mut said).unwrap();
            said.lines().count()
        };
        assert_eq!(replies(&s), 2, "the greeting and the answer while the root is the old tree");
        std::os::unix::fs::symlink("v2", base.join("cur.tmp")).unwrap();
        std::fs::rename(base.join("cur.tmp"), &cur).unwrap();
        assert!(s.reopen().unwrap());
        assert_eq!(replies(&s), 1, "only the greeting once the root moved");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn an_idle_reader_lets_go_of_segments_a_write_replaced() {
        let (root, s) = fixture("idle_segments");
        s.read(&Arc::default(), false, |searcher| searcher.search("make point", 5, false)).unwrap();
        let held = || lock(&s.pool).idle.iter().map(|r| r.ix.held_segments().map(<[String]>::to_vec)).collect::<Vec<_>>();
        assert!(held().iter().any(Option::is_some), "the reader holds the segments it searched");
        std::fs::write(root.join("src/more.py"), "def another_point():\n    return 2\n").unwrap();
        s.write(|repo| repo.update(false)).unwrap();
        assert!(held().iter().all(Option::is_none), "{:?}", held());
        let _ = std::fs::remove_dir_all(root);
    }

    /// Files under `root` this process still maps after they were deleted.
    fn deleted_maps(root: &Path) -> Vec<String> {
        let root = root.to_string_lossy().into_owned();
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
        maps.lines().filter(|l| l.contains(&root) && l.ends_with(" (deleted)")).map(|l| l.rsplit(' ').nth(1).unwrap_or(l).to_string()).collect()
    }

    #[test]
    fn an_idle_reader_lets_go_of_segments_another_process_replaced() {
        let (root, s) = watched("idle_other", 40, Every::given(Some(3600.0)));
        s.read(&Arc::default(), false, |searcher| searcher.search("made", 5, false)).unwrap();
        let held = || lock(&s.pool).idle.iter().filter_map(|r| r.ix.held_segments().map(<[String]>::to_vec)).collect::<Vec<_>>();
        let before = held();
        assert!(!before.is_empty(), "the reader holds the segments it searched");
        let mut other = Repo::open(&s.root()).unwrap();
        for k in 0..3 {
            std::fs::write(root.join(format!("src/other{k}.py")), format!("def other_{k}():\n    return {k}\n")).unwrap();
            other.update(false).unwrap();
        }
        let now = other.ix.segments();
        drop(other);
        assert!(before.iter().all(|b| *b != now), "the other process replaced the segments: {before:?} -> {now:?}");
        let t0 = Instant::now();
        while (held().iter().any(|h| *h != now) || !deleted_maps(&s.root()).is_empty()) && t0.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            held().iter().all(|h| *h == now),
            "an idle reader still holds {:?}, the index has {now:?}; deleted files mapped: {}",
            held(),
            deleted_maps(&s.root()).len()
        );
        assert_eq!(deleted_maps(&s.root()), Vec::<String>::new(), "deleted files of the index still mapped");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Run `f` in `n` calls at once; each waits until all `n` are inside, or ten seconds.
    /// -> how many were inside together at most, and each call's result.
    fn together<T: Send>(s: &Session, n: usize, f: impl Fn(usize, &mut Searcher) -> Result<T> + Sync) -> (usize, Vec<Result<T>>) {
        let inside = AtomicUsize::new(0);
        let most = AtomicUsize::new(0);
        let out = std::thread::scope(|sc| {
            let calls: Vec<_> = (0..n)
                .map(|i| {
                    let (inside, most, f) = (&inside, &most, &f);
                    sc.spawn(move || {
                        s.read(&Arc::default(), false, |searcher| {
                            let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                            most.fetch_max(now, Ordering::SeqCst);
                            let t0 = Instant::now();
                            while most.load(Ordering::SeqCst) < n.min(s.max_readers()) && t0.elapsed() < Duration::from_secs(10) {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                            let r = f(i, searcher);
                            inside.fetch_sub(1, Ordering::SeqCst);
                            r
                        })
                    })
                })
                .collect();
            calls.into_iter().map(|c| c.join().unwrap()).collect()
        });
        (most.into_inner(), out)
    }

    #[test]
    fn reads_run_side_by_side_each_on_a_reader_of_its_own() {
        let (root, s) = fixture("side_by_side");
        let n = s.max_readers().min(4);
        let (most, out) = together(&s, n, |_, searcher| searcher.find_symbol("make_point", 5).map(|h| h.len()));
        assert_eq!(most, n, "the reads waited for one another");
        assert!(out.iter().all(|r| matches!(r, Ok(1))), "{out:?}");
        // more calls than readers wait for a reader and are all answered
        let (most, out) = together(&s, s.max_readers() + 2, |_, searcher| searcher.find_symbol("make_point", 5).map(|h| h.len()));
        assert_eq!(most, s.max_readers());
        assert!(out.iter().all(|r| matches!(r, Ok(1))));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_panic_inside_one_call_is_that_calls_error_and_the_session_goes_on() {
        let (root, s) = fixture("panic");
        let (_, out) = together(&s, 2, |i, searcher| {
            if i == 0 {
                panic!("a call that panics");
            }
            searcher.find_symbol("make_point", 5).map(|h| h.len())
        });
        assert!(out[0].as_ref().is_err_and(|e| e.is::<Panicked>()));
        assert!(matches!(out[1], Ok(1)));
        let op = Op::Symbol { name: "make_point".into(), limit: 5, exact: true };
        for _ in 0..3 {
            assert!(matches!(s.run(&op, &Arc::default()), Ok(Answer::Hits { ref hits, .. }) if hits.len() == 1));
        }
        assert!(s.write(|_| -> Result<()> { panic!("a write that panics") }).is_err_and(|e| e.is::<Panicked>()));
        assert!(s.write(|repo| repo.update(false)).is_ok(), "the writer goes on too");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_cancel_stops_its_own_call_and_no_other() {
        let (root, s) = fixture("cancel");
        let flags: Vec<Arc<AtomicBool>> = (0..3).map(|i| Arc::new(AtomicBool::new(i == 1))).collect();
        let seen = std::thread::scope(|sc| {
            let calls: Vec<_> = flags.iter().map(|f| sc.spawn(|| s.read(f, false, |searcher| Ok(searcher.cancel.load(Ordering::Relaxed))).unwrap())).collect();
            calls.into_iter().map(|c| c.join().unwrap()).collect::<Vec<_>>()
        });
        assert_eq!(seen, [false, true, false]);
        // a cancelled reindex stops and leaves the index as it was
        let reindex = s.run(&Op::Reindex { force: true }, &Arc::new(AtomicBool::new(true)));
        assert!(reindex.is_err_and(|e| format!("{e:#}").contains("cancelled")));
        let op = Op::Symbol { name: "make_point".into(), limit: 5, exact: true };
        assert!(matches!(s.run(&op, &Arc::default()), Ok(Answer::Hits { ref hits, .. }) if hits.len() == 1));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_write_runs_beside_reads_and_a_read_sees_the_index_before_it_or_after_it() {
        let (root, s) = fixture("write");
        let reading = AtomicBool::new(false);
        let written = AtomicBool::new(false);
        std::thread::scope(|sc| {
            let read = sc.spawn(|| {
                s.read(&Arc::default(), false, |searcher| {
                    let before = searcher.find_symbol("added_later", 5)?.len();
                    reading.store(true, Ordering::SeqCst);
                    let t0 = Instant::now();
                    while !written.load(Ordering::SeqCst) && t0.elapsed() < Duration::from_secs(20) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Ok((before, searcher.find_symbol("added_later", 5)?.len()))
                })
            });
            while !reading.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            std::fs::write(root.join("src/later.py"), "def added_later():\n    return 2\n").unwrap();
            let st = s.write(|repo| repo.update(false)).unwrap();
            assert_eq!(st.files_changed, 1, "the write did not wait for the read");
            written.store(true, Ordering::SeqCst);
            assert_eq!(read.join().unwrap().unwrap(), (0, 1));
        });
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_call_waits_for_the_first_build_and_a_limit_ends_the_wait() {
        let root = std::env::temp_dir().join(format!("scrutate_session_first_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def first():\n    return 1\n").unwrap();
        let s = Session::open(&root, Every::given(None)).unwrap();
        let never = AtomicBool::new(false);
        assert!(s.wait_built(&never, Some(Duration::ZERO), |_| {}), "no build asked for: nothing to wait for");
        s.building();
        assert!(!s.wait_built(&never, Some(Duration::from_millis(50)), |_| {}), "the limit ends the wait");
        assert!(!s.wait_built(&AtomicBool::new(true), None, |_| {}), "a cancel ends the wait");
        assert!(s.run_scanned(&Op::Symbol { name: "first".into(), limit: 5, exact: false }).is_none(), "a command line call does not wait");
        std::thread::scope(|sc| {
            let waiter = sc.spawn(|| s.wait_built(&never, None, |_| {}));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!waiter.is_finished());
            s.build().unwrap();
            assert!(waiter.join().unwrap());
        });
        assert!(s.run_scanned(&Op::Symbol { name: "first".into(), limit: 5, exact: false }).is_some_and(|(_, a)| a.is_ok_and(|a| !a.is_empty())));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_call_waits_for_another_process_that_builds_the_first_index() {
        let root = std::env::temp_dir().join(format!("scrutate_session_other_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let s = Session::open(&root, Every::given(Some(3600.0))).unwrap();
        // what another process holds while it builds the first index
        let other = rusqlite::Connection::open(root.join(crate::files::INDEX_DIR).join("index.db")).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            other.execute_batch("ROLLBACK").unwrap();
        });
        let op = Op::Symbol { name: "make_point".into(), limit: 5, exact: true };
        assert!(s.run_scanned(&op).is_none(), "a command line call does not wait: it does the work itself");
        let a = s.run(&op, &Arc::default());
        release.join().unwrap();
        assert!(matches!(a, Ok(Answer::Hits { ref hits, .. }) if hits.len() == 1), "{:?}", a.err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_long_socket_path_comes_back_to_the_callers_directory_even_after_it_was_renamed() {
        const NAME: &str = "session::tests::a_long_socket_path_comes_back_to_the_callers_directory_even_after_it_was_renamed";
        // the working directory belongs to the whole process, so this runs alone in one of its own
        if std::env::var_os("SCRUTATE_TEST_SHORT").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", NAME, "--test-threads=1"])
                .env("SCRUTATE_TEST_SHORT", "1")
                .output()
                .unwrap();
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success() && said.contains("1 passed"), "{said}{}", String::from_utf8_lossy(&out.stderr));
            return;
        }
        let base = std::env::temp_dir().join(format!("scrutate_session_short_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (caller, moved, deep) = (base.join("caller"), base.join("moved"), base.join("d".repeat(60)).join("e".repeat(60)));
        std::fs::create_dir_all(&caller).unwrap();
        std::fs::create_dir_all(&deep).unwrap();
        let (base, deep) = (base.canonicalize().unwrap(), deep.canonicalize().unwrap());
        std::env::set_current_dir(&caller).unwrap();
        let sock = deep.join("session.sock");
        assert!(sock.as_os_str().len() > 110);
        let r = unix::short(&sock, |p| {
            assert_eq!(p, Path::new("session.sock"));
            assert_eq!(std::env::current_dir()?.canonicalize()?, deep);
            // the path the caller's directory had is gone by the time the call comes back
            std::fs::rename(base.join("caller"), base.join("moved"))
        });
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(std::env::current_dir().unwrap().canonicalize().unwrap(), moved.canonicalize().unwrap());
        std::env::set_current_dir(std::env::temp_dir()).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A freshness that names the files a test tells it, then nothing.
    struct Told(Arc<Mutex<Vec<String>>>);

    impl crate::fresh::Freshness for Told {
        fn changes_since(&self, t: crate::fresh::Token) -> (crate::stores::Changes, crate::fresh::Token) {
            let code = std::mem::take(&mut *lock(&self.0));
            (crate::stores::Changes::Named(crate::stores::Named { code, ..Default::default() }), t.next())
        }
        fn files(&self, text: bool) -> Result<Vec<crate::files::Found>> {
            crate::files::look(&std::env::temp_dir(), text)
        }
    }

    #[test]
    fn a_scan_applies_the_change_set_and_looks_at_no_other_file() {
        let (root, s) = fixture("change_set");
        let told = Arc::new(Mutex::new(Vec::new()));
        let s = s.with_freshness(Box::new(Told(Arc::clone(&told))));
        std::fs::write(root.join("src/named.py"), "def brand_new():\n    return 3\n").unwrap();
        std::fs::write(root.join("src/unnamed.py"), "def not_named():\n    return 4\n").unwrap();
        *lock(&told) = vec!["src/named.py".into()];
        let (st, a) = s.run_scanned(&Op::Symbol { name: "brand_new".into(), limit: 5, exact: true }).unwrap();
        assert_eq!((st.files_looked, st.files_changed), (1, 1), "{st:?}");
        assert!(a.is_ok_and(|a| !a.is_empty()));
        let (st, a) = s.run_scanned(&Op::Symbol { name: "not_named".into(), limit: 5, exact: true }).unwrap();
        assert_eq!(st.files_looked, 0, "{st:?}");
        assert!(a.is_ok_and(|a| a.is_empty()), "a file the change set does not name is not looked at");
        let _ = std::fs::remove_dir_all(root);
    }

    /// A built session of `files` files under `name` whose reads the watch already tells.
    fn watched(name: &str, files: usize, every: Every) -> (PathBuf, Arc<Session>) {
        watched_with(name, files, every, None)
    }

    /// `watched`, with safety walks at most once per `safety`.
    fn watched_with(name: &str, files: usize, every: Every, safety: Option<Duration>) -> (PathBuf, Arc<Session>) {
        let root = std::env::temp_dir().join(format!("scrutate_session_{name}_{}", std::process::id()));
        crate::claim_test_tree(&root);
        let _ = std::fs::remove_dir_all(&root);
        for i in 0..files {
            let dir = root.join(format!("src/d{}", i % 20));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("m{i}.py")), format!("def made_{i}():\n    return {i}\n")).unwrap();
        }
        std::fs::create_dir_all(root.join("src/d0")).unwrap();
        let s = Session::open_watched(&root, every).unwrap();
        if let (Some(every), Some(notes)) = (safety, s.notes()) {
            notes.set_safety_every(every);
        }
        // these tests are of the change set, which macOS reads do without
        if let Some(notes) = s.notes() {
            notes.set_order_proves(true);
        }
        s.build().unwrap();
        s.watch(false).unwrap();
        let t0 = Instant::now();
        for k in 0.. {
            std::fs::write(root.join("src/d0/probe.py"), format!("def probe_{k}():\n    return 0\n")).unwrap();
            let (st, _) = s.run_scanned(&Op::Symbol { name: format!("probe_{k}"), limit: 1, exact: true }).unwrap();
            if st.files_looked == 1 {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "the watch never told a read what changed");
            std::thread::sleep(Duration::from_millis(50));
        }
        (root, s)
    }

    #[test]
    fn a_file_written_just_before_a_call_is_in_its_answer_every_time() {
        // no safety walk lands in the run: a second file looked at is a second look
        let (root, s) = watched_with("barrier", 1, Every::given(Some(3600.0)), Some(Duration::from_secs(3600)));
        s.notes().unwrap().set_order_proves(crate::watch::ORDER_PROVES);
        // the change set where a cookie proves it, else a look at the tree's 3 files
        let looked = if crate::watch::ORDER_PROVES { 1 } else { 3 };
        for i in 0..1000 {
            std::fs::write(root.join("src/d0/w.py"), format!("def written_{i}():\n    return {i}\n")).unwrap();
            let (st, a) = s.run_scanned(&Op::Symbol { name: format!("written_{i}"), limit: 1, exact: true }).unwrap();
            assert!(a.is_ok_and(|a| !a.is_empty()), "write {i} missed");
            assert_eq!(st.files_looked, looked, "write {i}: {st:?}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// FSEvents as recorded on macOS under load: the cookie's event arrives and the event of a
    /// file written before it does not (it comes later, with a later event id).
    #[test]
    fn a_write_whose_event_comes_after_the_cookie_is_in_the_answer_where_the_order_proves_nothing() {
        assert_eq!(crate::watch::ORDER_PROVES, !cfg!(target_os = "macos"));
        let root = std::env::temp_dir().join(format!("scrutate_session_late_event_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/w.py"), "def written_0():\n    return 0\n").unwrap();
        let s = Session::open_watched(&root, Every::given(Some(3600.0))).unwrap();
        s.build().unwrap();
        let notes = s.notes().unwrap();
        notes.in_place_without_a_watcher();
        notes.set_order_proves(false);
        let stop = Arc::new(AtomicBool::new(false));
        let courier = {
            let (notes, stop) = (notes.clone(), Arc::clone(&stop));
            let cookies = s.root().join(crate::files::INDEX_DIR).join(crate::watch::COOKIES);
            std::thread::spawn(move || {
                let mut sent = std::collections::HashSet::new();
                while !stop.load(Ordering::Relaxed) {
                    for p in std::fs::read_dir(&cookies).into_iter().flatten().flatten().map(|e| e.path()) {
                        if sent.insert(p.clone()) {
                            notes.note(&Ok(notify::Event::new(notify::EventKind::Create(notify::event::CreateKind::File)).add_path(p)));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        std::fs::write(root.join("src/w.py"), "def written_1():\n    return 1\n").unwrap();
        let (_, a) = s.run_scanned(&Op::Symbol { name: "written_1".into(), limit: 1, exact: true }).unwrap();
        stop.store(true, Ordering::Relaxed);
        courier.join().unwrap();
        assert!(a.is_ok_and(|a| !a.is_empty()), "the write was missed");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Where the order proves nothing a read looks at the tree whatever floor the user gave: the
    /// floor holds back only what a proven change set cannot vouch for.
    #[test]
    fn a_write_just_before_a_read_is_in_its_answer_under_a_user_floor_where_the_order_proves_nothing() {
        let root = std::env::temp_dir().join(format!("scrutate_session_floor_look_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/w.py"), "def written_0():\n    return 0\n").unwrap();
        let s = Session::open_watched(&root, Every::given(Some(3600.0))).unwrap();
        s.build().unwrap();
        let notes = s.notes().unwrap();
        notes.in_place_without_a_watcher();
        notes.set_order_proves(false);
        for i in 1..=20 {
            std::fs::write(root.join("src/w.py"), format!("def written_{i}():\n    return {i}\n")).unwrap();
            let a = s.run(&Op::Symbol { name: format!("written_{i}"), limit: 1, exact: true }, &Arc::default()).unwrap();
            assert!(!a.is_empty(), "write {i} missed under the floor");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_read_looks_at_the_changed_files_alone_and_a_walk_at_all() {
        let (root, s) = watched("o_changed", 2000, Every::given(Some(3600.0)));
        std::fs::write(root.join("src/d7/m7.py"), "def edited():\n    return 7\n").unwrap();
        let (st, a) = s.run_scanned(&Op::Symbol { name: "edited".into(), limit: 1, exact: true }).unwrap();
        assert_eq!((st.files_looked, st.files_changed), (1, 1), "{st:?}");
        assert!(a.is_ok_and(|a| !a.is_empty()));
        drop(s);
        let walk = Session::open(&root, Every::given(Some(3600.0))).unwrap();
        let (st, _) = walk.run_scanned(&Op::Symbol { name: "edited".into(), limit: 1, exact: true }).unwrap();
        assert_eq!(st.files_looked, 2001, "every file and the probe: {st:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lost_events_make_the_next_read_walk_and_its_answer_holds_the_change() {
        let (root, s) = watched("lost", 10, Every::given(Some(3600.0)));
        s.notes().unwrap().note(&Err(notify::Error::generic("a lost queue")));
        std::fs::write(root.join("src/d3/m3.py"), "def after_loss():\n    return 3\n").unwrap();
        let (st, a) = s.run_scanned(&Op::Symbol { name: "after_loss".into(), limit: 1, exact: true }).unwrap();
        assert_eq!(st.files_looked, 11, "a walk: {st:?}");
        assert!(a.is_ok_and(|a| !a.is_empty()));
        std::fs::write(root.join("src/d3/m3.py"), "def after_walk():\n    return 3\n").unwrap();
        let (st, _) = s.run_scanned(&Op::Symbol { name: "after_walk".into(), limit: 1, exact: true }).unwrap();
        assert_eq!(st.files_looked, 1, "named again after the walk: {st:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Before its watch is in place a session under a floor walks at every read: nothing names a
    /// change yet, and start-up's walk is owed.
    #[test]
    fn an_edit_before_the_watch_is_in_place_is_seen_under_a_floor() {
        let root = std::env::temp_dir().join(format!("scrutate_session_before_watch_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.py"), "def first():\n    return 0\n").unwrap();
        let s = Session::open_watched(&root, Every::given(Some(3600.0))).unwrap();
        s.build().unwrap();
        assert!(!s.run(&Op::Symbol { name: "first".into(), limit: 1, exact: true }, &Arc::default()).unwrap().is_empty());
        for i in 0..5 {
            std::fs::write(root.join("src/a.py"), format!("def early_{i}():\n    return {i}\n")).unwrap();
            let a = s.run(&Op::Symbol { name: format!("early_{i}"), limit: 1, exact: true }, &Arc::default()).unwrap();
            assert!(!a.is_empty(), "edit {i} before the watch is in place");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_walk_the_watch_asks_for_comes_at_once_unless_the_user_gave_a_floor() {
        for (floor, seen) in [(None, true), (Some(3600.0), false)] {
            let (root, s) = watched(&format!("asked_{seen}"), 10, Every::given(floor));
            s.notes().unwrap().note(&Err(notify::Error::generic("a lost queue")));
            // an edit the watch names is applied under a floor too
            std::fs::write(root.join("src/d3/m3.py"), "def edited_after_loss():\n    return 3\n").unwrap();
            let a = s.run(&Op::Symbol { name: "edited_after_loss".into(), limit: 1, exact: true }, &Arc::default()).unwrap();
            assert!(!a.is_empty(), "a named edit, interval {floor:?}");
            // a file no event names (its directory is new), so only the walk can find it
            std::fs::create_dir_all(root.join("src/after")).unwrap();
            std::fs::write(root.join("src/after/m.py"), "def after_loss():\n    return 3\n").unwrap();
            let a = s.run(&Op::Symbol { name: "after_loss".into(), limit: 1, exact: true }, &Arc::default()).unwrap();
            assert_eq!(!a.is_empty(), seen, "interval {floor:?}");
            let _ = std::fs::remove_dir_all(root);
        }
    }

    /// Where the order proves (Linux, Windows) both sessions read with cookies and each takes only
    /// its own; on macOS neither writes one and every read looks at the tree, so this covers there
    /// only that two sessions of one root answer every write.
    #[test]
    fn two_watched_sessions_of_one_root_in_one_process_keep_their_barriers() {
        let (root, s1) = watched("twin", 1, Every::given(Some(3600.0)));
        s1.notes().unwrap().set_order_proves(crate::watch::ORDER_PROVES);
        let s2 = Session::open_watched(&root, Every::given(Some(3600.0))).unwrap();
        s2.watch(false).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        // s2's reads write cookies whose events reach s1's watch too
        for k in 0..200 {
            let _ = s2.run_scanned(&Op::Symbol { name: format!("warm_{k}"), limit: 1, exact: true }).unwrap();
        }
        let mut missed = Vec::new();
        for i in 0..100 {
            std::fs::write(root.join("src/d0/w.py"), format!("def twin_{i}():\n    return {i}\n")).unwrap();
            let (_, a) = s1.run_scanned(&Op::Symbol { name: format!("twin_{i}"), limit: 1, exact: true }).unwrap();
            if !a.is_ok_and(|a| !a.is_empty()) {
                missed.push(i);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(missed.is_empty(), "{} of 100 writes missed: {missed:?}", missed.len());
    }

    /// On macOS, where no read writes a cookie, this covers only that s1 answers every write.
    #[test]
    fn an_idle_second_watched_session_costs_the_first_no_answer() {
        let (root, s1) = watched("idle_twin", 1, Every::given(Some(3600.0)));
        s1.notes().unwrap().set_order_proves(crate::watch::ORDER_PROVES);
        let s2 = Session::open_watched(&root, Every::given(Some(3600.0))).unwrap();
        s2.watch(false).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let mut missed = Vec::new();
        for i in 0..100 {
            std::fs::write(root.join("src/d0/w.py"), format!("def idle_{i}():\n    return {i}\n")).unwrap();
            let (_, a) = s1.run_scanned(&Op::Symbol { name: format!("idle_{i}"), limit: 1, exact: true }).unwrap();
            if !a.is_ok_and(|a| !a.is_empty()) {
                missed.push(i);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(missed.is_empty(), "{} of 100 writes missed: {missed:?}", missed.len());
    }

    #[test]
    fn a_read_leaves_no_cookie_behind() {
        let (root, s) = watched("cookies", 1, Every::given(Some(3600.0)));
        let notes = s.notes().unwrap();
        for i in 0..1000 {
            let _ = s.run_scanned(&Op::Symbol { name: "made_0".into(), limit: 1, exact: true }).unwrap();
            assert_eq!(notes.cookies_kept(), 0, "after read {i}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_change_set_a_write_of_this_process_held_back_is_applied_by_the_next_read() {
        let (root, s) = watched("held_back", 1, Every::given(Some(3600.0)));
        std::fs::write(root.join("src/d0/h.py"), "def held_back():\n    return 1\n").unwrap();
        let op = Op::Symbol { name: "held_back".into(), limit: 1, exact: true };
        let writer = lock(&s.seed).writer();
        let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        std::thread::scope(|sc| {
            sc.spawn(move || {
                let _w = writer.lock().unwrap();
                held.0.send(()).unwrap();
                let _ = release.1.recv_timeout(Duration::from_secs(20));
            });
            held.1.recv().unwrap();
            let _ = s.run(&op, &Arc::default());
            release.0.send(()).unwrap();
        });
        let a = s.run(&op, &Arc::default()).unwrap();
        assert!(!a.is_empty(), "the file a held-back read named is in the next read's answer");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_command_line_call_scans_every_file_first() {
        let (root, s) = fixture("scanned");
        let op = Op::Symbol { name: "brand_new".into(), limit: 5, exact: true };
        assert!(s.run(&op, &Arc::default()).is_ok_and(|a| a.is_empty()));
        std::fs::write(root.join("src/new.py"), "def brand_new():\n    return 3\n").unwrap();
        assert!(s.run(&op, &Arc::default()).is_ok_and(|a| a.is_empty()), "inside the interval a call reads the index as it is");
        let (st, a) = s.run_scanned(&op).unwrap();
        assert_eq!(st.files_changed, 1);
        assert!(a.is_ok_and(|a| !a.is_empty()));
        let _ = std::fs::remove_dir_all(root);
    }
}
