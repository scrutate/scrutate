//! Per-file results of `tsquery`, so files whose content did not change are not parsed again.
//!
//! One file per (scrutate version, language, query text, searched paths from the root) under
//! `.scrutate/tsquery/`, mapping a file's content hash and name (`key`) to that file's captures
//! (paths are filled in on use). The content hash is the one the code index records, so a file
//! whose stat the code index says is current is not read at all on a hit. A complete run keeps
//! only the entries it used, so a file holds the current tree's results; the files themselves are
//! evicted least recently used first once together they exceed `MAX_BYTES`. Files are replaced by
//! rename, so a reader or a crash never sees half of one; anything unreadable counts as empty.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Result};
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;

use crate::intel::Capture;

/// Total size of `.scrutate/tsquery/`; the least recently used query files go first beyond it.
pub const MAX_BYTES: u64 = 128 << 20;
const MAGIC: &[u8] = b"scrutate-tsquery-cache 1\n";

pub type Key = [u8; 16];

pub fn hash(parts: &[&[u8]]) -> Key {
    let mut h = Blake2bVar::new(16).expect("blake2b-128");
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p);
    }
    let mut out = [0u8; 16];
    h.finalize_variable(&mut out).expect("blake2b finalize");
    out
}

/// The key of one file's captures: the content hash the code index records for such bytes, and
/// the file name, which with the content decides the file's language.
pub fn key(content_hash: &str, path: &str) -> Key {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    hash(&[content_hash.as_bytes(), name.as_bytes()])
}

pub struct Cache {
    dir: PathBuf,
    path: PathBuf,
    header: Vec<u8>,
    pub entries: HashMap<Key, Vec<Capture>>,
}

fn hex(k: &Key) -> String {
    k.iter().map(|b| format!("{b:02x}")).collect()
}

impl Cache {
    /// The cache of one query under `root/.scrutate/`, empty when there is none or it is unreadable.
    /// The searched paths (`scopes`, from the root, so that a copy of the tree at another place
    /// finds the same file) are part of the name, so runs over different subtrees do not evict
    /// each other's entries.
    pub fn open(root: &Path, lang: &str, query: &str, scopes: &[String]) -> Cache {
        let dir = root.join(".scrutate").join("tsquery");
        let paths = scopes;
        let mut header = MAGIC.to_vec();
        for s in [crate::VERSION, lang, query, &paths.join("\n")] {
            header.extend((s.len() as u32).to_le_bytes());
            header.extend(s.as_bytes());
        }
        let path = dir.join(format!("{}.bin", hex(&hash(&[&header]))));
        let entries = crate::files::read_regular(&path, MAX_BYTES).and_then(|b| decode(&b, &header).ok()).unwrap_or_default();
        Cache { dir, path, header, entries }
    }

    /// Replace the stored entries with `entries`, then evict old query files. Errors are ignored by
    /// callers: the cache only saves time.
    pub fn store(&self, entries: &HashMap<Key, Vec<Capture>>) -> Result<()> {
        let mut out = self.header.clone();
        let mut keys: Vec<&Key> = entries.keys().collect();
        keys.sort();
        out.extend((keys.len() as u64).to_le_bytes());
        for k in keys {
            let caps = &entries[k];
            out.extend(k);
            out.extend((caps.len() as u32).to_le_bytes());
            for c in caps {
                out.extend(c.line.to_le_bytes());
                out.extend(c.column.to_le_bytes());
                for s in [&c.capture, &c.kind, &c.text] {
                    out.extend((s.len() as u32).to_le_bytes());
                    out.extend(s.as_bytes());
                }
            }
            if out.len() as u64 > MAX_BYTES {
                bail!("too large to cache");
            }
        }
        let scrutate = self.dir.parent().unwrap_or(&self.dir);
        crate::files::make_index_dir(scrutate.parent().unwrap_or(scrutate))?;
        std::fs::create_dir_all(&self.dir)?;
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let tmp = self.dir.join(format!(".tmp-{}-{nanos}", std::process::id()));
        // not flushed to disk: a cache, built again when it is lost or does not parse
        crate::index::write_new(&tmp, &out)?;
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        self.evict();
        Ok(())
    }

    /// Mark this query as just used without rewriting it.
    pub fn touch(&self) {
        if let Ok(f) = std::fs::File::options().write(true).open(&self.path) {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    fn evict(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
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
            if total > MAX_BYTES && p != self.path {
                let _ = std::fs::remove_file(&p);
            }
        }
    }
}

fn decode(b: &[u8], header: &[u8]) -> Result<HashMap<Key, Vec<Capture>>> {
    let mut r = Reader { b, pos: 0 };
    if r.take(header.len())? != header {
        bail!("another query or version");
    }
    let n = r.u64()?;
    let mut out = HashMap::new();
    for _ in 0..n {
        let k: Key = r.take(16)?.try_into()?;
        let ncaps = r.u32()?;
        let mut caps = Vec::new();
        for _ in 0..ncaps {
            let line = r.u64()? as i64;
            let column = r.u64()? as i64;
            let capture = r.string()?;
            let kind = r.string()?;
            let text = r.string()?;
            caps.push(Capture { path: String::new(), line, column, capture, kind, text });
        }
        out.insert(k, caps);
    }
    if r.pos != b.len() {
        bail!("trailing bytes");
    }
    Ok(out)
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
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8(self.take(n)?.to_vec())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_damage() {
        let root = std::env::temp_dir().join(format!("scrutate_tscache_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let c = Cache::open(&root, "c", "(identifier) @i", &[]);
        assert!(c.entries.is_empty());
        let cap = Capture { path: String::new(), line: 3, column: 7, capture: "i".into(), kind: "identifier".into(), text: "kmalloc".into() };
        let entries = HashMap::from([(hash(&[b"a"]), vec![cap.clone(), cap]), (hash(&[b"b"]), vec![])]);
        c.store(&entries).unwrap();
        let again = Cache::open(&root, "c", "(identifier) @i", &[]);
        assert_eq!(again.entries.len(), 2);
        assert_eq!(again.entries[&hash(&[b"a"])][1].text, "kmalloc");
        assert!(Cache::open(&root, "c", "(identifier) @other", &[]).entries.is_empty(), "keyed by the query text");
        let bytes = std::fs::read(&c.path).unwrap();
        std::fs::write(&c.path, &bytes[..bytes.len() - 3]).unwrap();
        assert!(Cache::open(&root, "c", "(identifier) @i", &[]).entries.is_empty(), "a damaged file reads as empty");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
