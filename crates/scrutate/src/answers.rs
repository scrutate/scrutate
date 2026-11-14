//! Stored resolution answers: what `Names` made of a name's occurrences in some lines of a file
//! (`Uses`) for one `callers` or `callees` request, kept under `.scrutate/answers/`, so the same
//! request on the same index and files resolves nothing again.
//!
//! One file per request kind and name. It holds the answers of one run and what that run read:
//! the code index's database (the file's identity and its data version, which every update that
//! changes a row moves), this executable, the root, and the `stat` of every file the resolver read
//! from disk (`noted`), absent ones included. A run uses a file only when all of those are the
//! same now and the stored run went as far (`limit`); it then walks the same call sites in the same
//! order on the same inputs, so every answer is the one the stored run made, which is the one a
//! fresh run makes. Anything else, a damaged file included, reads as no file. Files are written
//! from a read, whole, to a temporary name and renamed, so no lock is taken and a reader never sees
//! half of one; two writers lose one write, never mix them. The least recently used go first
//! beyond `MAX_BYTES`.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Result};

use crate::index::Stat;
use crate::names::Uses;
use crate::search::Searcher;

/// Total size of `.scrutate/answers/`.
pub const MAX_BYTES: u64 = 64 << 20;
const MAGIC: &[u8] = b"scrutate-answers 1\n";
const MAX_ITEMS: usize = 1 << 24;

/// A file's state on disk: a regular file and its stat, or anything else.
type Seen = Option<Stat>;

thread_local! {
    /// The files the resolvers of this thread read while a `Names` is open.
    static READS: RefCell<Reads> = RefCell::default();
}

#[derive(Default)]
struct Reads {
    open: usize,
    seen: HashMap<PathBuf, Seen>,
    /// a file read twice changed in between
    moved: bool,
}

fn seen(p: &Path) -> Seen {
    std::fs::symlink_metadata(p).ok().filter(|m| m.is_file()).map(|m| Stat::of(p, &m))
}

/// Called before the resolver reads `rel`: its stat joins what the open requests read.
pub(crate) fn noted(root: &Path, rel: &str) {
    READS.with(|r| {
        let mut r = r.borrow_mut();
        if r.open == 0 {
            return;
        }
        let p = root.join(rel);
        let now = seen(&p);
        match r.seen.get(&p) {
            Some(was) => r.moved |= *was != now,
            None => {
                r.seen.insert(p, now);
            }
        }
    })
}

/// While one is alive, the reads of the resolvers of this thread are noted.
pub(crate) struct Reading;

impl Reading {
    pub(crate) fn start() -> Reading {
        READS.with(|r| r.borrow_mut().open += 1);
        Reading
    }
}

impl Drop for Reading {
    fn drop(&mut self) {
        READS.with(|r| {
            let mut r = r.borrow_mut();
            r.open -= 1;
            if r.open == 0 {
                *r = Reads::default();
            }
        })
    }
}

/// (path, first line, last line, name)
pub(crate) type Site = (String, i64, i64, String);

/// The stored answers of one request and the ones this run adds.
pub(crate) struct Stored {
    dir: PathBuf,
    path: PathBuf,
    header: Vec<u8>,
    version: Option<String>,
    old: HashMap<Site, Uses>,
    new: HashMap<Site, Uses>,
    /// a site the stored run did not answer was asked: this run is not that run
    strayed: bool,
}

impl Stored {
    /// The answers a run of `kind name` that goes up to `limit` results may use.
    pub(crate) fn open(s: &Searcher, kind: &str, name: &str, limit: usize) -> Stored {
        let dir = s.ix.root.join(crate::files::INDEX_DIR).join("answers");
        let version = s.ix.meta("data_version");
        let db = database(&s.ix.root);
        let mut header = MAGIC.to_vec();
        for part in [crate::VERSION, &build(), &db, version.as_deref().unwrap_or(""), &s.ix.root.to_string_lossy(), kind, name] {
            put_str(&mut header, part);
        }
        // one file per request: the next state's run replaces it
        let path = dir.join(format!("{}.bin", hex(&crate::tscache::hash(&[kind.as_bytes(), name.as_bytes()]))));
        // without a database identity or a data version nothing is known to be the same
        let known = version.is_some() && !db.is_empty();
        let old = known.then(|| crate::files::read_regular(&path, MAX_BYTES)).flatten().and_then(|b| decode(&b, &header, limit).ok()).unwrap_or_default();
        Stored { dir, path, header, version: version.filter(|_| known), old, new: HashMap::new(), strayed: false }
    }

    pub(crate) fn get(&mut self, site: &Site) -> Option<&Uses> {
        let found = self.old.get(site).or_else(|| self.new.get(site));
        self.strayed |= found.is_none() && !self.old.is_empty();
        found
    }

    pub(crate) fn put(&mut self, site: Site, uses: Uses) {
        self.new.insert(site, uses);
    }

    /// Keep this run's answers when it read everything it answered from: a run that reached its
    /// end stands for any limit (`complete`), else for its own. Errors are ignored: the store only
    /// saves time.
    pub(crate) fn save(self, s: &Searcher, limit: usize, complete: bool) {
        if !self.old.is_empty() {
            touch(&self.path);
        }
        if self.new.is_empty() || self.strayed || self.version.is_none() || s.ix.meta("data_version") != self.version {
            return;
        }
        let Some(reads) = READS.with(|r| {
            let r = r.borrow();
            (!r.moved).then(|| r.seen.iter().map(|(p, s)| (p.clone(), *s)).collect::<Vec<_>>())
        }) else {
            return;
        };
        let _ = self.write(&reads, if complete { u64::MAX } else { limit as u64 });
    }

    fn write(&self, reads: &[(PathBuf, Seen)], limit: u64) -> Result<()> {
        let mut out = self.header.clone();
        out.extend(limit.to_le_bytes());
        put_n(&mut out, reads.len());
        for (p, s) in reads {
            put_str(&mut out, &p.to_string_lossy());
            match s {
                Some(s) => {
                    out.push(1);
                    for v in [s.size, s.mtime, s.ctime] {
                        out.extend(v.to_le_bytes());
                    }
                }
                None => out.push(0),
            }
        }
        put_n(&mut out, self.new.len());
        for ((path, start, end, name), u) in &self.new {
            put_str(&mut out, path);
            out.extend(start.to_le_bytes());
            out.extend(end.to_le_bytes());
            put_str(&mut out, name);
            for ids in [&u.got, &u.dispatched] {
                let mut ids: Vec<i64> = ids.iter().copied().collect();
                ids.sort_unstable();
                put_n(&mut out, ids.len());
                for id in ids {
                    out.extend(id.to_le_bytes());
                }
            }
            out.push(u.unsure as u8 | (u.unsure_free as u8) << 1 | (u.own_file as u8) << 2);
        }
        if out.len() as u64 > MAX_BYTES {
            bail!("too large to keep");
        }
        let scrutate = self.dir.parent().unwrap_or(&self.dir);
        crate::files::make_index_dir(scrutate.parent().unwrap_or(scrutate))?;
        std::fs::create_dir_all(&self.dir)?;
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        #[cfg(not(target_os = "wasi"))]
        let tmp = self.dir.join(format!(".tmp-{}-{:?}-{nanos}", std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
        // wasi has no process id, and its one thread is the only writer
        #[cfg(target_os = "wasi")]
        let tmp = self.dir.join(format!(".tmp-{nanos}"));
        // not flushed to disk: a cache, built again when it is lost or does not parse
        crate::index::write_new(&tmp, &out)?;
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        evict(&self.dir, &self.path);
        Ok(())
    }
}

/// This executable, so a rebuilt binary (whose rules may differ) answers anew.
fn build() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(&p).ok().map(|m| format!("{} {} {}", identity(&m), m.len(), Stat::of(&p, &m).mtime)))
        .unwrap_or_default()
}

/// The code index's database file: a database deleted and built again is another one, whatever its
/// data version says. Empty where the file's creation time is not known.
fn database(root: &Path) -> String {
    let Ok(m) = std::fs::metadata(root.join(crate::files::INDEX_DIR).join("index.db")) else { return String::new() };
    if m.created().is_err() {
        return String::new();
    }
    identity(&m)
}

/// The file's device, number and creation time: none of them moves while it is written.
fn identity(m: &std::fs::Metadata) -> String {
    let born = m.created().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (m.dev(), m.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0, 0);
    format!("{dev} {ino} {born}")
}

fn hex(k: &[u8]) -> String {
    k.iter().map(|b| format!("{b:02x}")).collect()
}

fn touch(path: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

fn evict(dir: &Path, keep: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            Some((md.modified().unwrap_or(SystemTime::UNIX_EPOCH), md.len(), e.path()))
        })
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let mut total = 0u64;
    for (_, len, p) in files {
        total += len;
        if total > MAX_BYTES && p != keep {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// The answers of a stored run that went at least up to `limit` and read only files that are as
/// they were.
fn decode(b: &[u8], header: &[u8], limit: usize) -> Result<HashMap<Site, Uses>> {
    let mut r = Reader { b, pos: 0 };
    if r.take(header.len())? != header {
        bail!("another request, index or build");
    }
    if r.u64()? < limit as u64 {
        bail!("a run that stopped sooner");
    }
    for _ in 0..r.n()? {
        let p = PathBuf::from(r.string()?);
        let was = match r.take(1)?[0] {
            0 => None,
            _ => Some(Stat { size: r.i64()?, mtime: r.i64()?, ctime: r.i64()? }),
        };
        if seen(&p) != was {
            bail!("a file it read changed");
        }
    }
    let mut out = HashMap::new();
    for _ in 0..r.n()? {
        let site = (r.string()?, r.i64()?, r.i64()?, r.string()?);
        let mut ids = || -> Result<HashSet<i64>> { (0..r.n()?).map(|_| r.i64()).collect() };
        let (got, dispatched) = (ids()?, ids()?);
        let flags = r.take(1)?[0];
        out.insert(site, Uses { got, dispatched, unsure: flags & 1 != 0, unsure_free: flags & 2 != 0, own_file: flags & 4 != 0 });
    }
    if r.pos != b.len() {
        bail!("trailing bytes");
    }
    Ok(out)
}

fn put_n(w: &mut Vec<u8>, n: usize) {
    w.extend((n as u32).to_le_bytes());
}

fn put_str(w: &mut Vec<u8>, s: &str) {
    put_n(w, s.len());
    w.extend(s.as_bytes());
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let Some(s) = self.b.get(self.pos..self.pos.saturating_add(n)) else { bail!("truncated") };
        self.pos += n;
        Ok(s)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn n(&mut self) -> Result<usize> {
        let n = u32::from_le_bytes(self.take(4)?.try_into()?) as usize;
        if n > MAX_ITEMS {
            bail!("a count larger than any answer");
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String> {
        let n = self.n()?;
        Ok(String::from_utf8(self.take(n)?.to_vec())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_answers_stand_while_the_files_read_are_as_they_were() {
        let root = std::env::temp_dir().join(format!("scrutate_answers_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def helper():\n    return 1\n").unwrap();
        std::fs::write(root.join("b.py"), "from a import helper\n\n\ndef run():\n    return helper()\n").unwrap();
        let (s, _) = crate::open_updated(&root, false).unwrap();
        let s = s.into_searcher();
        let callers = || crate::names::Names::new(&s).unwrap().callers("helper", 10).unwrap();
        let kept = |limit| Stored::open(&s, "callers", "helper", limit).old.len();
        assert_eq!(kept(10), 0);
        let first = callers();
        assert_eq!(first.0.len(), 1);
        if database(&root).is_empty() {
            // a file system that does not say when the database was made keeps nothing
            assert_eq!(kept(10), 0);
            // Windows deletes no file the open index holds
            drop(s);
            return std::fs::remove_dir_all(&root).unwrap();
        }
        assert_eq!((kept(10), kept(11)), (1, 1), "a run that went through every site stands for any limit");
        assert_eq!(callers().0[0].qualname, first.0[0].qualname);
        // the index is not told: the resolver reads the file as it is now, so the kept answer goes
        // a virus scanner on a Windows runner can hold a file just written for a moment; a handle
        // the open index keeps would be a defect: it would stop an editor saving the file
        let edit = || std::fs::write(root.join("b.py"), "from a import helper as h\n\n\ndef run():\n    return h()\n");
        let mut tries = 0;
        while let Err(e) = edit() {
            tries += 1;
            if tries == 200 {
                drop(s);
                let after = edit();
                panic!("b.py stayed busy for 5 s ({e}); with the open index closed the write gives {after:?}");
            }
            assert_eq!(e.raw_os_error(), Some(32), "{e}");
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert_eq!(kept(10), 0);
        assert!(Stored::open(&s, "callers", "nothing", 10).old.is_empty(), "another request");
        drop(s);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
