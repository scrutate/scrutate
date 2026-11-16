//! A kqueue with one `EVFILT_VNODE` watch per file and directory: the macOS proof a session read
//! rests on. XNU posts a vnode's note inside the syscall that changes it (`VNOP_WRITE`,
//! `VNOP_CREATE`, `VNOP_REMOVE`, `VNOP_RENAME`, `VNOP_SETATTR` in bsd/vfs/kpi_vfs.c, under the
//! vnode lock), so a drain with a zero timeout at the start of a read sees every change made by a
//! call that returned before it. FSEvents, which numbers events in a daemon, gives no such order
//! (docs/adr/0001-change-sets-and-the-cookie-barrier.md).

use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const NOTES: u32 = libc::NOTE_WRITE | libc::NOTE_EXTEND | libc::NOTE_ATTRIB | libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_LINK | libc::NOTE_REVOKE;
/// Descriptors kept back for the index's own files, maps and the rest of the process.
const RESERVE: usize = 1024;
/// A directory listed this many times without two listings agreeing is changing too fast to vouch for.
const LISTINGS: usize = 8;

/// Why the kqueue cannot vouch for the tree: its reads take the look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Off {
    /// more files and directories than half the descriptors the process may open, less a reserve
    Budget,
    /// the system refused a descriptor or a watch
    Refused,
    /// a probe file's write gave no note: this volume does not report through kqueue
    Probe,
    /// the tree spans more than one device
    Devices,
    /// a directory changed too fast to list twice alike
    Churn,
}

impl Off {
    pub(crate) fn why(self) -> &'static str {
        match self {
            Off::Budget => "the tree has more files than the kqueue's share of descriptors",
            Off::Refused => "the system refused a kqueue watch",
            Off::Probe => "this volume reports no kqueue notes",
            Off::Devices => "the tree spans more than one device",
            Off::Churn => "a directory changed too fast to watch",
        }
    }
}

/// What a drain found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// a file's contents or attributes changed, or it was replaced at its path
    Modified(PathBuf),
    /// a new entry of a watched directory, already watched itself when it is a file
    Created(PathBuf),
    /// an entry that is gone from its directory
    Removed(PathBuf),
    /// this directory came, went or moved: the watches must be set again from a new walk
    Directory(PathBuf),
}

struct Entry {
    path: PathBuf,
    fd: Option<OwnedFd>,
    /// a directory's entries as last listed
    names: Option<HashSet<OsString>>,
}

/// One kqueue watching every file and directory of a tree.
pub(crate) struct Registry {
    kq: OwnedFd,
    entries: Vec<Entry>,
    by_path: HashMap<PathBuf, usize>,
    open: usize,
    budget: usize,
    dev: u64,
}

/// The descriptors this process may open: the soft limit raised to what the system allows a
/// process (as Go's runtime does at start), so a tree of thousands of files fits.
fn descriptor_limit() -> usize {
    let mut most: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: an int and its size, for an int sysctl
    let per_process = unsafe { libc::sysctlbyname(c"kern.maxfilesperproc".as_ptr(), (&raw mut most).cast(), &mut size, std::ptr::null_mut(), 0) } == 0;
    let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: a rlimit to fill
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } != 0 {
        return 0;
    }
    let want = if per_process && most > 0 { rl.rlim_max.min(most as libc::rlim_t) } else { rl.rlim_cur };
    if want > rl.rlim_cur {
        let raised = libc::rlimit { rlim_cur: want, rlim_max: rl.rlim_max };
        // SAFETY: a rlimit within the hard limit
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            rl.rlim_cur = want;
        }
    }
    usize::try_from(rl.rlim_cur).unwrap_or(usize::MAX)
}

fn refused(e: &std::io::Error) -> Off {
    match e.raw_os_error() {
        Some(libc::EMFILE | libc::ENFILE) => Off::Budget,
        _ => Off::Refused,
    }
}

impl Registry {
    /// An empty registry for the tree at `root`, whose directories must all be on its device, with
    /// `budget` descriptors (half the process's, less a reserve, unless a test gives fewer).
    pub(crate) fn new(root: &Path, budget: Option<usize>) -> Result<Registry, Off> {
        // SAFETY: kqueue takes no arguments; a descriptor or -1
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(refused(&std::io::Error::last_os_error()));
        }
        // SAFETY: a descriptor kqueue just returned, owned from here on
        let kq = unsafe { OwnedFd::from_raw_fd(fd) };
        let dev = std::fs::metadata(root).map_err(|_| Off::Refused)?.dev();
        let budget = budget.unwrap_or_else(|| (descriptor_limit() / 2).saturating_sub(RESERVE));
        Ok(Registry { kq, entries: Vec::new(), by_path: HashMap::new(), open: 0, budget, dev })
    }

    /// The watches in place.
    pub(crate) fn watches(&self) -> usize {
        self.open
    }

    /// Watch `path` (a file or a directory) from now on; nothing for one already watched.
    fn add(&mut self, path: &Path, dir: bool) -> Result<(), Off> {
        if self.by_path.get(path).is_some_and(|&i| self.entries[i].fd.is_some()) {
            return Ok(());
        }
        if self.open >= self.budget {
            return Err(Off::Budget);
        }
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| Off::Refused)?;
        // SAFETY: a nul-terminated path; a descriptor for notes only, or -1
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            // gone already: the directory's note says so
            return if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(refused(&e)) };
        }
        // SAFETY: a descriptor open just returned, owned from here on
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let i = match self.by_path.get(path) {
            Some(&i) => i,
            None => {
                self.entries.push(Entry { path: path.to_path_buf(), fd: None, names: None });
                self.by_path.insert(path.to_path_buf(), self.entries.len() - 1);
                self.entries.len() - 1
            }
        };
        let change = libc::kevent {
            ident: fd.as_raw_fd() as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: NOTES,
            data: 0,
            udata: i as *mut libc::c_void,
        };
        // SAFETY: one change from a live struct, no events asked back
        if unsafe { libc::kevent(self.kq.as_raw_fd(), &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) } < 0 {
            return Err(refused(&std::io::Error::last_os_error()));
        }
        self.entries[i].fd = Some(fd);
        if dir && self.entries[i].names.is_none() {
            self.entries[i].names = Some(HashSet::new());
        }
        self.open += 1;
        Ok(())
    }

    fn forget(&mut self, i: usize) {
        if self.entries[i].fd.take().is_some() {
            self.open -= 1;
        }
    }

    /// Watch directory `dir`, then each file in it `keep` accepts, listing it until two listings
    /// agree: a file made while the first listing ran is in the second. Every subdirectory's name
    /// is kept (a new one is a change of the tree's directories); only `add_tree` watches one.
    /// The new entries, files already watched.
    fn add_dir(&mut self, dir: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<Vec<(PathBuf, bool)>, Off> {
        if std::fs::metadata(dir).is_ok_and(|m| m.dev() != self.dev) {
            return Err(Off::Devices);
        }
        self.add(dir, true)?;
        let Some(&d) = self.by_path.get(dir) else { return Ok(Vec::new()) };
        let mut before = self.entries[d].names.clone().unwrap_or_default();
        let mut found = Vec::new();
        for _ in 0..LISTINGS {
            let mut now = HashSet::new();
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = e.path();
                let (is_dir, is_file) = e.file_type().map_or((false, false), |t| (t.is_dir(), t.is_file()));
                if !is_dir && !keep(&path) {
                    continue;
                }
                now.insert(e.file_name());
                // a link, socket or pipe is named alone: its coming and going is the directory's note
                if is_file && !self.by_path.get(&path).is_some_and(|&i| self.entries[i].fd.is_some()) {
                    self.add(&path, false)?;
                }
                if !before.contains(&e.file_name()) {
                    found.push((path, is_dir));
                }
            }
            if now == before {
                if let Some(names) = self.entries[d].names.as_mut() {
                    *names = now;
                }
                found.sort();
                found.dedup();
                return Ok(found);
            }
            before = now;
        }
        Err(Off::Churn)
    }

    /// Watch every directory in `dirs` and the files in them `keep` accepts.
    pub(crate) fn add_tree(&mut self, dirs: &[PathBuf], keep: &dyn Fn(&Path) -> bool) -> Result<(), Off> {
        for d in dirs {
            self.add_dir(d, keep)?;
        }
        Ok(())
    }

    /// Whether this volume reports a write through kqueue: `probe` (a path of the tree's
    /// volume, made and removed here) is watched, written, and its note must come at once.
    pub(crate) fn probe(&mut self, probe: &Path) -> Result<(), Off> {
        std::fs::write(probe, b"").map_err(|_| Off::Probe)?;
        let r = (|| {
            self.add(probe, false)?;
            std::fs::write(probe, b"probe").map_err(|_| Off::Probe)?;
            let notes = self.drain_raw()?;
            let i = self.by_path[probe];
            if notes.iter().any(|&(j, f)| j == i && f & (libc::NOTE_WRITE | libc::NOTE_EXTEND) != 0) {
                Ok(())
            } else {
                Err(Off::Probe)
            }
        })();
        if let Some(&i) = self.by_path.get(probe) {
            self.forget(i);
            self.by_path.remove(probe);
        }
        let _ = std::fs::remove_file(probe);
        r
    }

    /// Every note queued now: (entry, flags).
    fn drain_raw(&mut self) -> Result<Vec<(usize, u32)>, Off> {
        let zero = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        let mut got = Vec::new();
        loop {
            // SAFETY: an empty kevent array of this size, filled by the call
            let mut buf: [libc::kevent; 256] = unsafe { std::mem::zeroed() };
            // SAFETY: no changes in, up to buf.len() events out, a zero timeout
            let n = unsafe { libc::kevent(self.kq.as_raw_fd(), std::ptr::null(), 0, buf.as_mut_ptr(), buf.len() as libc::c_int, &zero) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(Off::Refused);
            }
            for ev in &buf[..n as usize] {
                got.push((ev.udata as usize, ev.fflags));
            }
            if (n as usize) < buf.len() {
                return Ok(got);
            }
        }
    }

    /// What changed since the last drain, every change made before this call included. New
    /// files of a watched directory are watched before they are named.
    pub(crate) fn drain(&mut self, keep: &dyn Fn(&Path) -> bool) -> Result<Vec<Change>, Off> {
        let mut out = Vec::new();
        let mut dirs = Vec::new();
        for (i, flags) in self.drain_raw()? {
            let Some(e) = self.entries.get(i) else { continue };
            if e.fd.is_none() {
                continue;
            }
            let (path, is_dir) = (e.path.clone(), e.names.is_some());
            if is_dir {
                if flags & (libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE) != 0 {
                    out.push(Change::Directory(path));
                } else if flags & (libc::NOTE_WRITE | libc::NOTE_LINK) != 0 {
                    dirs.push(i);
                }
            } else if flags & (libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE) != 0 {
                // the vnode left its path: watch whatever is there now
                self.forget(i);
                match std::fs::symlink_metadata(&path) {
                    Ok(m) => {
                        if m.is_file() {
                            self.add(&path, false)?;
                        }
                        out.push(Change::Modified(path));
                    }
                    Err(_) => out.push(Change::Removed(path)),
                }
            } else {
                out.push(Change::Modified(path));
            }
        }
        dirs.sort_unstable();
        dirs.dedup();
        for d in dirs {
            let dir = self.entries[d].path.clone();
            let old = self.entries[d].names.clone().unwrap_or_default();
            for (path, is_dir) in self.add_dir(&dir, keep)? {
                out.push(if is_dir { Change::Directory(path) } else { Change::Created(path) });
            }
            let now = self.entries[d].names.clone().unwrap_or_default();
            for gone in old.difference(&now) {
                let path = dir.join(gone);
                if let Some(&i) = self.by_path.get(&path) {
                    if self.entries[i].names.is_some() {
                        out.push(Change::Directory(path.clone()));
                    }
                    self.forget(i);
                }
                out.push(Change::Removed(path));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_kq_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/a.py"), "a = 1\n").unwrap();
        std::fs::write(root.join("src/sub/b.py"), "b = 1\n").unwrap();
        std::fs::canonicalize(&root).unwrap()
    }

    fn all(_: &Path) -> bool {
        true
    }

    fn registry(root: &Path, budget: Option<usize>) -> Registry {
        let mut r = Registry::new(root, budget).unwrap();
        r.add_tree(&[root.to_path_buf(), root.join("src"), root.join("src/sub")], &all).unwrap();
        r
    }

    /// The descriptors this process holds on paths under `root` (other tests open their own).
    fn fds_under(root: &Path) -> usize {
        let fds: Vec<i32> = std::fs::read_dir("/dev/fd").unwrap().flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).collect();
        fds.into_iter()
            .filter(|&fd| {
                let mut buf = [0u8; libc::PATH_MAX as usize];
                // SAFETY: F_GETPATH writes at most PATH_MAX bytes into the buffer; a closed fd fails
                (unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } == 0)
                    && Path::new(std::ffi::OsStr::from_bytes(buf.split(|&b| b == 0).next().unwrap_or_default())).starts_with(root)
            })
            .count()
    }

    #[test]
    fn a_write_is_in_the_next_drain_every_time() {
        let root = tree("write");
        let mut r = registry(&root, None);
        for i in 0..1000 {
            std::fs::write(root.join("src/a.py"), format!("a = {i}\n")).unwrap();
            let got = r.drain(&all).unwrap();
            assert!(got.contains(&Change::Modified(root.join("src/a.py"))), "write {i}: {got:?}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_file_replaced_by_a_rename_a_thousand_times_is_named_each_time_and_leaks_no_descriptor() {
        let root = tree("replace");
        let mut r = registry(&root, None);
        let fds = fds_under(&root);
        let watched = r.watches();
        for i in 0..1000 {
            std::fs::write(root.join("src/a.py.tmp"), format!("a = {i}\n")).unwrap();
            std::fs::rename(root.join("src/a.py.tmp"), root.join("src/a.py")).unwrap();
            let got = r.drain(&all).unwrap();
            assert!(got.contains(&Change::Modified(root.join("src/a.py"))), "replace {i}: {got:?}");
        }
        assert_eq!((fds_under(&root), r.watches()), (fds, watched), "descriptors and watches after the churn");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_new_file_is_watched_before_it_is_named() {
        let root = tree("create");
        let mut r = registry(&root, None);
        std::fs::write(root.join("src/new.py"), "n = 1\n").unwrap();
        assert!(r.drain(&all).unwrap().contains(&Change::Created(root.join("src/new.py"))));
        std::fs::write(root.join("src/new.py"), "n = 2\n").unwrap();
        assert!(r.drain(&all).unwrap().contains(&Change::Modified(root.join("src/new.py"))));
        std::fs::remove_file(root.join("src/new.py")).unwrap();
        assert!(r.drain(&all).unwrap().contains(&Change::Removed(root.join("src/new.py"))));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_directory_renamed_away_and_back_asks_for_new_watches() {
        let root = tree("dirmove");
        let mut r = registry(&root, None);
        std::fs::rename(root.join("src/sub"), root.join("away")).unwrap();
        let moved = |c: &[Change]| c.iter().any(|c| matches!(c, Change::Directory(_)));
        assert!(moved(&r.drain(&all).unwrap()));
        std::fs::rename(root.join("away"), root.join("src/sub")).unwrap();
        assert!(moved(&r.drain(&all).unwrap()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_tree_past_the_budget_is_refused_and_keeps_no_descriptor() {
        let root = tree("budget");
        let mut r = Registry::new(&root, Some(2)).unwrap();
        let got = r.add_tree(&[root.clone(), root.join("src"), root.join("src/sub")], &all);
        assert_eq!(got.err(), Some(Off::Budget));
        drop(r);
        assert_eq!(fds_under(&root), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_temporary_directory_reports_kqueue_notes() {
        let root = tree("probe");
        let mut r = Registry::new(&root, None).unwrap();
        assert_eq!(r.probe(&root.join("probe")), Ok(()));
        assert!(!root.join("probe").exists());
        assert_eq!(r.watches(), 0);
        let _ = std::fs::remove_dir_all(root);
    }
}
