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
//! per update, a segment manifest swapped in that transaction).
//!
//! A command line call may use the session `serve` runs, through a socket under the
//! repository's `.scrutate/` (`listen`, `ask`; Unix only). The session is a cache of work there:
//! anything it cannot vouch for makes the command line do the work itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::index::{self, FileCount, UpdateStats};
use crate::ops::{Answer, Op};
use crate::search::Searcher;
use crate::Repo;

/// How long a call waits for an index another process is building before it gives that answer
/// (well under the 60 s after which Codex gives a call up).
pub const BUILD_WAIT: Duration = Duration::from_secs(20);

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

#[derive(Default)]
struct Pool {
    idle: Vec<Repo>,
    open: usize,
}

#[derive(Default)]
struct Clock {
    last: Option<Instant>,
    cost: f64,
}

pub struct Session {
    root: PathBuf,
    every: Every,
    max_readers: usize,
    /// writes, and the repository the readers are opened from
    seed: Mutex<Repo>,
    writer: Arc<Mutex<()>>,
    pool: Mutex<Pool>,
    freed: Condvar,
    build: Mutex<Build>,
    built: Condvar,
    clock: Mutex<Clock>,
    /// one scan decision at a time: a call that waited for another's scan does not scan again
    scanning: Mutex<()>,
    writing: AtomicUsize,
    /// the files the writes of `seed` read, the first build's among them
    written: Arc<FileCount>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
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
        Ok(Session {
            root: seed.ix.root.clone(),
            every,
            max_readers,
            writer: seed.writer(),
            seed: Mutex::new(seed),
            pool: Mutex::default(),
            freed: Condvar::new(),
            build: Mutex::new(Build::None),
            built: Condvar::new(),
            clock: Mutex::default(),
            scanning: Mutex::new(()),
            writing: AtomicUsize::new(0),
            written,
        })
    }

    /// The canonical root.
    pub fn root(&self) -> &Path {
        &self.root
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
        self.writing.fetch_add(1, Ordering::AcqRel);
        let mut seed = lock(&self.seed);
        let r = caught(|| f(&mut seed));
        let updated = seed.take_updated();
        let names = updated.then(|| seed.ix.segments());
        drop(seed);
        if let Some(names) = names {
            self.release_idle(&names);
        }
        self.writing.fetch_sub(1, Ordering::AcqRel);
        self.note_update(updated);
        r
    }

    /// Run one operation on an index scanned at most once per refresh interval (one that does
    /// not read the index is not scanned for), waiting up to `BUILD_WAIT` for another process's
    /// first build. A panic is the error `Panicked`.
    pub fn run(&self, op: &Op, cancel: &Arc<AtomicBool>) -> Result<Answer> {
        self.run_counted(op, cancel, &Arc::default())
    }

    /// `run`, counting the files the operation reads in `count` as it goes.
    pub fn run_counted(&self, op: &Op, cancel: &Arc<AtomicBool>, count: &Arc<FileCount>) -> Result<Answer> {
        self.waiting_for_others(cancel, || {
            self.call(cancel, Some(count), None, |repo| {
                if op.reads_index() {
                    self.scan_if_due(repo)?;
                }
                repo.run_as_is(op)
            })
        })
    }

    /// Run `f` on the searcher, as `run` runs an operation: with `refresh` the index is scanned
    /// as for an operation; a damaged index is built again and `f` run once more.
    pub fn read<T>(&self, cancel: &Arc<AtomicBool>, refresh: bool, mut f: impl FnMut(&mut Searcher) -> Result<T>) -> Result<T> {
        self.waiting_for_others(cancel, || {
            self.call(cancel, None, None, |repo| {
                if refresh {
                    self.scan_if_due(repo)?;
                }
                repo.recovering(&mut f)
            })
        })
    }

    /// A command line call: every file is looked at first, as a fresh process does, then `op`
    /// runs. None when the session cannot answer now without waiting (a write running, every
    /// reader busy, another process holding the write lock) or a panic: the caller does the work.
    pub fn run_scanned(&self, op: &Op) -> Option<(UpdateStats, Result<Answer>)> {
        const WAIT: Duration = Duration::from_millis(50);
        if self.writing.load(Ordering::Acquire) > 0 || self.is_building() {
            return None;
        }
        let cancel = Arc::default();
        let r = self.call(&cancel, None, Some(WAIT), |repo| {
            let mut st = UpdateStats::default();
            if op.reads_index() {
                let t0 = Instant::now();
                let scanning = loop {
                    match self.scanning.try_lock() {
                        Ok(g) => break g,
                        Err(std::sync::TryLockError::Poisoned(p)) => break p.into_inner(),
                        Err(_) if t0.elapsed() < WAIT => std::thread::sleep(Duration::from_micros(500)),
                        Err(_) => return Ok(None),
                    }
                };
                let t = Instant::now();
                st = repo.refresh(0.0)?.unwrap_or_default();
                if repo.is_stale() {
                    return Ok(None);
                }
                self.scanned(t, true);
                drop(scanning);
            }
            Ok(Some((st, repo.run_as_is(op))))
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
        let Some(mut repo) = self.reader(cancel, wait)? else { return Err(anyhow::anyhow!("every reader is busy")) };
        repo.set_cancel(Arc::clone(cancel));
        if let Some(count) = count {
            repo.set_count(Arc::clone(count));
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut repo))) {
            Ok(r) => {
                repo.set_cancel(Arc::default());
                repo.set_count(Arc::default());
                let updated = repo.take_updated();
                self.give_back(Some(repo));
                self.note_update(updated);
                r
            }
            Err(_) => {
                drop(repo);
                self.give_back(None);
                Err(Panicked.into())
            }
        }
    }

    fn reader(&self, cancel: &AtomicBool, wait: Option<Duration>) -> Result<Option<Repo>> {
        let started = Instant::now();
        let mut pool = lock(&self.pool);
        loop {
            if let Some(repo) = pool.idle.pop() {
                return Ok(Some(repo));
            }
            if pool.open < self.max_readers {
                pool.open += 1;
                drop(pool);
                let opened = caught(|| lock(&self.seed).another());
                if opened.is_err() {
                    self.give_back(None);
                }
                return opened.map(Some);
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

    /// A reader back to the pool, or None for one that was closed. What it just read is the
    /// newest the others can know of: one that holds other segments lets them go.
    fn give_back(&self, repo: Option<Repo>) {
        let mut pool = lock(&self.pool);
        match repo {
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

    /// The scan before a read, when the refresh interval has passed since the last one.
    fn scan_if_due(&self, repo: &mut Repo) -> Result<()> {
        if !self.due() {
            return Ok(());
        }
        let _scanning = lock(&self.scanning);
        if !self.due() {
            return Ok(());
        }
        let t = Instant::now();
        let r = repo.refresh(0.0);
        // a scan that could not run does not start a floor's interval: the next call tries again
        self.scanned(t, r.is_ok() && !repo.is_stale() || !self.every.floor);
        r.map(|_| ())
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

#[cfg(unix)]
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
pub fn listen(_session: &Arc<Session>, _answer: fn(Answer, &Request) -> Result<Printed>) -> Option<Listening> {
    None
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::{UnixListener, UnixStream};

    /// How long the command line waits for a session's greeting.
    const HELLO: Duration = Duration::from_millis(20);

    /// `HELLO`, or `SCRUTATE_SESSION_WAIT_MS` for a machine too busy to greet that fast.
    fn hello() -> Duration {
        std::env::var("SCRUTATE_SESSION_WAIT_MS").ok().and_then(|v| v.parse().ok()).map_or(HELLO, Duration::from_millis)
    }
    /// How long the command line waits for an answer before it does the work itself.
    const ANSWER: Duration = Duration::from_secs(60);
    /// How long a session waits for a request after its greeting.
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
    pub fn listen(session: &Arc<Session>, answer: fn(Answer, &Request) -> Result<Printed>) -> Option<Listening> {
        let path = socket_path(session.root());
        let Some(me) = Identity::here(session.root()) else {
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
        let session = Arc::clone(session);
        let open = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("scrutate-session".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    let Ok(conn) = conn else { continue };
                    // another user's process gets nothing, not even the greeting
                    if !same_user(&conn) || open.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                        continue;
                    }
                    open.fetch_add(1, Ordering::AcqRel);
                    let (session, hello, done) = (Arc::clone(&session), hello.clone(), Arc::clone(&open));
                    let served = std::thread::Builder::new().name("scrutate-session-call".into()).spawn(move || {
                        let _ = serve_one(&session, conn, &hello, answer);
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

    fn serve_one(session: &Session, conn: UnixStream, hello: &str, answer: fn(Answer, &Request) -> Result<Printed>) -> Result<()> {
        let mut w = conn.try_clone()?;
        writeln!(w, "{hello}")?;
        conn.set_read_timeout(Some(REQUEST))?;
        let mut line = Vec::new();
        BufReader::new(std::io::Read::take(&conn, MAX_REQUEST)).read_until(b'\n', &mut line)?;
        let request: Request = serde_json::from_slice(&line)?;
        let printed = session.run_scanned(&request.op).and_then(|(st, a)| Some((st, answer(a.ok()?, &request).ok()?)));
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
        writeln!(w, "{}", serde_json::to_string(request).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
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

    /// A repository of one function, its session, and its first build done.
    fn fixture(name: &str) -> (PathBuf, Session) {
        let root = std::env::temp_dir().join(format!("scrutate_session_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let s = Session::open(&root, Every::given(Some(3600.0))).unwrap();
        s.build().unwrap();
        (root, s)
    }

    /// A read while a write of this process runs (one that waits for another process's lock, as
    /// `serve --watch` does) answers from the committed index at once, as against another
    /// process's writer.
    #[test]
    fn a_read_does_not_wait_for_a_write_of_this_process() {
        let root = std::env::temp_dir().join(format!("scrutate_session_own_writer_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let s = Session::open(&root, Every::given(Some(0.0))).unwrap();
        s.build().unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 2\n\n\ndef more():\n    return 3\n").unwrap();
        let writer = lock(&s.seed).writer();
        let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        std::thread::scope(|sc| {
            sc.spawn(move || {
                let _w = writer.lock().unwrap();
                held.0.send(()).unwrap();
                let _ = release.1.recv_timeout(Duration::from_secs(20));
            });
            held.1.recv().unwrap();
            let t0 = Instant::now();
            let r = s.run(&Op::Outline { path: "src/geo.py".into() }, &Arc::default());
            let took = t0.elapsed();
            release.0.send(()).unwrap();
            assert!(r.is_ok(), "{:?}", r.err());
            assert!(took < Duration::from_secs(3), "{took:?}");
        });
        let _ = std::fs::remove_dir_all(root);
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
        let op = Op::Symbol { name: "make_point".into(), limit: 5 };
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
        let op = Op::Symbol { name: "make_point".into(), limit: 5 };
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
        assert!(s.run_scanned(&Op::Symbol { name: "first".into(), limit: 5 }).is_none(), "a command line call does not wait");
        std::thread::scope(|sc| {
            let waiter = sc.spawn(|| s.wait_built(&never, None, |_| {}));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!waiter.is_finished());
            s.build().unwrap();
            assert!(waiter.join().unwrap());
        });
        assert!(s.run_scanned(&Op::Symbol { name: "first".into(), limit: 5 }).is_some_and(|(_, a)| a.is_ok_and(|a| !a.is_empty())));
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
        let op = Op::Symbol { name: "make_point".into(), limit: 5 };
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

    #[test]
    fn a_command_line_call_scans_every_file_first() {
        let (root, s) = fixture("scanned");
        let op = Op::Symbol { name: "brand_new".into(), limit: 5 };
        assert!(s.run(&op, &Arc::default()).is_ok_and(|a| a.is_empty()));
        std::fs::write(root.join("src/new.py"), "def brand_new():\n    return 3\n").unwrap();
        assert!(s.run(&op, &Arc::default()).is_ok_and(|a| a.is_empty()), "inside the interval a call reads the index as it is");
        let (st, a) = s.run_scanned(&op).unwrap();
        assert_eq!(st.files_changed, 1);
        assert!(a.is_ok_and(|a| !a.is_empty()));
        let _ = std::fs::remove_dir_all(root);
    }
}
