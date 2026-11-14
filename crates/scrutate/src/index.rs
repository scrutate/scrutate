//! Incremental on-disk index: SQLite (chunks, call refs, manifest) + memory-mapped segments
//! (BM25 postings, vectors).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use sha1::{Digest, Sha1};

use crate::artifacts::{Artifact, Found, Key};
use crate::lex::{merge_segments, FileLex, LexStore, QVecs, SegmentBuilder};
use crate::models::ModelSet;
use crate::parse::{parse_file_with, PrevParse};
use crate::semantic::SemanticModel;
use crate::text::name_key;

pub const SCHEMA_VERSION: u32 = 14;
/// What the parser and tokenizer make of a file. Bump it with every change of the rules in
/// `langs.rs`, `parse.rs` or `text.rs`:
/// an index of an older revision reads every file again on its next update, and one of a newer
/// revision is read as it is and never updated.
pub const CONTENT_REVISION: u32 = 8;
pub use crate::models::REVISION as MODEL_REVISION;
pub const MAX_SEGMENTS: usize = 6; // more live segments than this triggers a merge of the small ones
pub const SMALL_INDEX_ROWS: usize = 8000; // an index this small is kept as ONE segment
/// Rows per segment flushed during one update: bounds the memory of building the inverted index
/// (the postings of a whole monorepository would not fit) at the price of more, smaller segments,
/// which compaction merges later.
pub const SEGMENT_ROWS: usize = 100_000;
/// Compaction never builds a segment larger than this, so a merge's memory is bounded too.
pub const MAX_MERGE_ROWS: usize = 300_000;
/// Databases up to this size get a full `PRAGMA quick_check` on every open (milliseconds); larger
/// ones are checked on demand (`scrutate check`) and recovered when an operation reports corruption.
pub const QUICK_CHECK_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const GC_AGE_S: u64 = 600; // unreferenced segment directories younger than this may be in flight
pub const MAX_FILE_BYTES: u64 = 1_000_000;
/// Source bytes of the syntax trees an open index keeps for incremental reparsing (the trees
/// themselves take several times this in memory).
pub const TREE_CACHE_BYTES: usize = 2 << 20;
pub const TF_CAP: u32 = 4; // a token counts at most this many times per field

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime_ns INTEGER, hash TEXT, ctime_ns INTEGER, art BLOB);
CREATE TABLE IF NOT EXISTS chunks(
  id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, qualname TEXT,
  name_key TEXT, qual_key TEXT, start INTEGER, end INTEGER, is_test INTEGER,
  ndoc INTEGER, nbody INTEGER, text BLOB, name_tok TEXT);
CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path, start);
CREATE INDEX IF NOT EXISTS chunks_name ON chunks(name_key);
CREATE INDEX IF NOT EXISTS chunks_qual ON chunks(qual_key);
CREATE TABLE IF NOT EXISTS refs(chunk_id INTEGER NOT NULL, callee TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS refs_callee ON refs(callee);
CREATE INDEX IF NOT EXISTS refs_chunk ON refs(chunk_id);
CREATE TABLE IF NOT EXISTS imports(path TEXT NOT NULL, module TEXT NOT NULL, line INTEGER);
CREATE INDEX IF NOT EXISTS imports_path ON imports(path);
CREATE INDEX IF NOT EXISTS imports_module ON imports(module);
CREATE TABLE IF NOT EXISTS artifacts(key BLOB PRIMARY KEY, body BLOB NOT NULL, kept BLOB, bytes INTEGER NOT NULL, used INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS artifacts_kept ON artifacts(used) WHERE kept IS NOT NULL;
";
/// Made once the `files` table has the `art` column, which an index of an older schema lacks.
const ART_INDEX: &str = "CREATE INDEX IF NOT EXISTS files_art ON files(art) WHERE art IS NOT NULL;";
/// Artifacts no file names are kept up to a quarter of the database's size, and at least this.
pub const KEPT_MIN_BYTES: i64 = 4 << 20;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct UpdateStats {
    pub files_total: usize,
    pub files_changed: usize,
    pub files_removed: usize,
    pub chunks: i64,
    pub seconds: f64,
    pub semantic: bool,
}

/// Result of `Index::check`.
#[derive(Debug, serde::Serialize)]
pub struct CheckReport {
    pub database_ok: bool,
    pub segments: usize,
    pub segments_ok: bool,
    /// chunks with a segment row: every chunk but the constants and fields
    pub chunks_db: i64,
    pub chunks_alive: i64,
    pub files_indexed: usize,
    pub files_changed: usize,
    pub files_missing: usize,
    pub problems: Vec<String>,
}

impl Default for CheckReport {
    fn default() -> Self {
        CheckReport {
            database_ok: true,
            segments: 0,
            segments_ok: true,
            chunks_db: 0,
            chunks_alive: 0,
            files_indexed: 0,
            files_changed: 0,
            files_missing: 0,
            problems: Vec::new(),
        }
    }
}

impl CheckReport {
    pub fn ok(&self) -> bool {
        self.database_ok && self.segments_ok && self.chunks_db == self.chunks_alive && self.problems.is_empty()
    }
}

/// One row of the `chunks` table as the search layer reads it.
/// Chunk text as stored: zlib-compressed UTF-8 (2.4x smaller on source code, ~6 us to inflate).
fn pack_text(text: &str) -> Vec<u8> {
    thread_local! {
        // a deflate state is ~300 KB to initialise: reuse one per thread instead of paying that per chunk
        static COMP: std::cell::RefCell<flate2::Compress> = std::cell::RefCell::new(flate2::Compress::new(flate2::Compression::new(6), true));
    }
    COMP.with(|c| {
        let mut c = c.borrow_mut();
        c.reset();
        let mut out = Vec::with_capacity(text.len() / 2 + 32);
        let mut input = text.as_bytes();
        loop {
            let before_in = c.total_in();
            let status = c.compress_vec(input, &mut out, flate2::FlushCompress::Finish).expect("in-memory deflate");
            input = &input[(c.total_in() - before_in) as usize..];
            match status {
                flate2::Status::StreamEnd => break,
                _ => out.reserve(out.len() / 2 + 64), // output buffer full: grow and continue
            }
        }
        out
    })
}

/// Inverse of `pack_text`; accepts legacy plain text and NULL.
fn unpack_text(v: rusqlite::types::ValueRef<'_>) -> rusqlite::Result<String> {
    use std::io::Read;
    match v {
        rusqlite::types::ValueRef::Null => Ok(String::new()),
        rusqlite::types::ValueRef::Text(t) => Ok(String::from_utf8_lossy(t).into_owned()),
        rusqlite::types::ValueRef::Blob(b) => {
            let mut out = String::new();
            flate2::read::ZlibDecoder::new(b)
                .read_to_string(&mut out)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Blob, Box::new(e)))?;
            Ok(out)
        }
        other => Err(rusqlite::Error::InvalidColumnType(12, "text".into(), other.data_type())),
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChunkRow {
    pub id: i64,
    pub path: String,
    pub kind: String,
    pub name: String,
    pub qualname: String,
    pub name_key: String,
    pub qual_key: String,
    pub start: i64,
    pub end: i64,
    pub is_test: i64,
    pub ndoc: i64,
    pub nbody: i64,
    pub text: String,
    pub name_tok: String,
}

const CHUNK_COLS: &str = "id,path,kind,name,qualname,name_key,qual_key,start,end,is_test,ndoc,nbody,text,name_tok";

impl ChunkRow {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<ChunkRow> {
        Ok(ChunkRow {
            id: r.get(0)?,
            path: r.get(1)?,
            kind: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            name: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            qualname: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            name_key: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            qual_key: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            start: r.get(7)?,
            end: r.get(8)?,
            is_test: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
            ndoc: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
            nbody: r.get::<_, Option<i64>>(11)?.unwrap_or(0),
            text: unpack_text(r.get_ref(12)?)?,
            name_tok: r.get::<_, Option<String>>(13)?.unwrap_or_default(),
        })
    }
}

/// Id, path, qualified name, start and end line, and text of a chunk.
pub type Body = (i64, String, String, i64, i64, String);

/// A chunk without its text and search fields: what name resolution reads.
#[derive(Debug, Clone, Default)]
pub struct Symbol {
    pub id: i64,
    pub path: String,
    pub kind: String,
    pub name: String,
    pub qualname: String,
    pub start: i64,
    pub end: i64,
}

impl Symbol {
    fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Symbol> {
        Ok(Symbol {
            id: r.get(0)?,
            path: r.get(1)?,
            kind: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            name: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            qualname: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            start: r.get(5)?,
            end: r.get(6)?,
        })
    }
}

/// `kinds` as an SQL list of string literals.
fn quoted(kinds: &[&str]) -> String {
    kinds.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",")
}

/// The order of a name lookup, `typed` being the parameter that holds the last part of the name as
/// typed: that exact name before its case-folded namesakes, then non-test files, types, functions,
/// the rest, and a definition before its prototype.
fn symbol_order(typed: &str) -> String {
    format!(
        "ORDER BY (name IS NOT {typed}), is_test, \
         CASE WHEN kind IN ('class','struct','enum','trait','interface','type','impl') THEN 0 WHEN kind IN ('function','method') THEN 1 ELSE 2 END, \
         (nbody = 0)"
    )
}

/// Parse + tokenize one file -> (rows with their callee names, FileLex, imports). Pure function.
/// A chunk ready for insertion: the row, its callees, and the compressed text (packed here, in
/// the parallel stage, so the single-writer insert loop only copies bytes).
pub type NewRow = (ChunkRow, Vec<String>, Vec<u8>);

/// Cumulative nanoseconds per indexing phase, printed by `update()` when SCRUTATE_PROFILE is set.
pub mod prof {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static PARSE: AtomicU64 = AtomicU64::new(0);
    pub static TREESITTER: AtomicU64 = AtomicU64::new(0);
    pub static TOKENIZE: AtomicU64 = AtomicU64::new(0);
    pub static ENCODE: AtomicU64 = AtomicU64::new(0);
    pub static PACK: AtomicU64 = AtomicU64::new(0);
    pub static INSERT: AtomicU64 = AtomicU64::new(0);
    pub static SEGMENT: AtomicU64 = AtomicU64::new(0);
    pub static MODEL: AtomicU64 = AtomicU64::new(0);
    pub static READ: AtomicU64 = AtomicU64::new(0);
    pub static LOOKUP: AtomicU64 = AtomicU64::new(0);
    pub fn add(c: &AtomicU64, t0: std::time::Instant) {
        c.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    pub fn enabled() -> bool {
        std::env::var_os("SCRUTATE_PROFILE").is_some()
    }
    pub fn report(wall: f64) {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed) as f64 / 1e9;
        eprintln!(
            "[scrutate profile] wall {wall:.2}s | cpu: read {:.2}s parse {:.2}s (tree-sitter {:.2}s) tokenize {:.2}s encode {:.2}s pack {:.2}s | serial: model {:.2}s store {:.2}s insert {:.2}s segment {:.2}s",
            g(&READ), g(&PARSE), g(&TREESITTER), g(&TOKENIZE), g(&ENCODE), g(&PACK), g(&MODEL), g(&LOOKUP), g(&INSERT), g(&SEGMENT)
        );
    }
}

/// Rows, postings, imports and syntax tree of a file, and its artifact as the store keeps it.
pub type BuiltRows = (Vec<NewRow>, FileLex, Vec<(i64, String)>, Option<PrevParse>, Vec<u8>);

/// Parse and tokenize a file into its artifact, then derive the rows of `rel` from it, as the rows
/// of a file found in the store are derived.
pub fn build_rows(rel: &str, source: &str, model: Option<&SemanticModel>, prev: Option<PrevParse>) -> BuiltRows {
    let prof = prof::enabled();
    let t = Instant::now();
    let (parsed, tree) = parse_file_with(rel, source, prev);
    if prof {
        prof::add(&prof::PARSE, t);
    }
    let mut chunks = Vec::with_capacity(parsed.chunks.len());
    let mut texts = Vec::with_capacity(parsed.chunks.len());
    let mut budget = crate::parse::TextBudget::of(source.as_bytes());
    for mut c in parsed.chunks {
        let t = Instant::now();
        c.fill(source.as_bytes(), &mut budget);
        let (fields, name_tok) = Artifact::tokenize(&c);
        if prof {
            prof::add(&prof::TOKENIZE, t);
        }
        let t = Instant::now();
        texts.push(pack_text(&c.text));
        let calls = c.calls.into_iter().collect();
        chunks.push(crate::artifacts::Chunk {
            kind: c.kind.to_string(),
            name: c.name,
            qualname: c.qualname,
            start: c.start,
            end: c.end,
            name_tok,
            calls,
            fields,
        });
        if prof {
            prof::add(&prof::PACK, t);
        }
    }
    let art = Artifact { chunks, imports: parsed.imports };
    let t = Instant::now();
    let body = art.encode();
    if prof {
        prof::add(&prof::PACK, t);
    }
    let (rows, lex, imports) = crate::artifacts::derive(rel, Found { art, texts, vecs: None }, model);
    (rows, lex, imports, tree, body)
}

enum Processed {
    Gone,
    Skip(Stat),
    Same(Stat),
    /// stat, content hash, artifact key, rows, postings, imports, syntax tree, the artifact to
    /// store (None when this store holds it already), and whether the file was parsed
    New(Stat, String, Key, Vec<NewRow>, Box<FileLex>, Vec<(i64, String)>, Option<PrevParse>, Option<Vec<u8>>, bool),
}

/// What reading a file found, before it is parsed or found in a store.
enum Read {
    Gone,
    Skip(Stat),
    Same(Stat),
    /// stat, content hash, artifact key, bytes
    Fresh(Stat, String, Key, Vec<u8>),
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// What one `stat` says about a file, as the `files` table records it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stat {
    pub size: i64,
    pub mtime: i64,
    /// status change time: moves on a chmod, which leaves size and mtime alone (0 where unknown)
    pub ctime: i64,
}

impl Stat {
    /// The stat of the file at `path`, whose metadata (of the link itself or of its target) is `md`.
    pub fn of(path: &Path, md: &std::fs::Metadata) -> Stat {
        #[cfg(unix)]
        let ctime = {
            use std::os::unix::fs::MetadataExt;
            let _ = path;
            md.ctime().saturating_mul(1_000_000_000).saturating_add(md.ctime_nsec())
        };
        #[cfg(windows)]
        let ctime = change_time::of(path, md.file_type().is_symlink());
        #[cfg(not(any(unix, windows)))]
        let ctime = {
            let _ = path;
            0
        };
        Stat { size: md.len() as i64, mtime: mtime_ns(md), ctime }
    }

    /// Whether a recorded row still describes the file; a row without a ctime (written
    /// before the column existed) is read again once.
    fn matches(&self, size: i64, mtime: i64, ctime: Option<i64>) -> bool {
        self.size == size && self.mtime == mtime && ctime == Some(self.ctime)
    }
}

/// The stats of the files one walk lists. On Windows a directory entry carries no change time, so
/// each directory is listed once more, by handle, for the change times of all its entries.
#[derive(Default)]
pub struct Listed {
    #[cfg(windows)]
    dirs: HashMap<PathBuf, HashMap<std::ffi::OsString, i64>>,
}

impl Listed {
    /// `Stat::of` for a file the walk listed, with `md` from its directory entry.
    pub fn stat(&mut self, path: &Path, md: &std::fs::Metadata) -> Stat {
        #[cfg(windows)]
        if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
            if !self.dirs.contains_key(dir) {
                self.dirs.insert(dir.to_path_buf(), change_time::listing(dir));
            }
            if let Some(ctime) = self.dirs.get_mut(dir).and_then(|names| names.remove(name)) {
                return Stat { size: md.len() as i64, mtime: mtime_ns(md), ctime };
            }
        }
        Stat::of(path, md)
    }
}

/// Status change times on Windows, read as CPython's `os.stat` reads them.
#[cfg(windows)]
mod change_time {
    use std::collections::HashMap;
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::sync::OnceLock;

    const FILE_LIST_DIRECTORY: u32 = 0x1;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_SHARE_ALL: u32 = 0x7;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    // FILE_INFO_BY_HANDLE_CLASS and FILE_INFO_BY_NAME_CLASS values
    const FILE_BASIC_INFO: i32 = 0;
    const FILE_ID_BOTH_DIRECTORY_INFO: i32 = 10;
    const FILE_STAT_BASIC_BY_NAME_INFO: i32 = 3;

    type ByName = unsafe extern "system" fn(name: *const u16, class: i32, out: *mut c_void, size: u32) -> i32;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandleEx(file: *mut c_void, class: i32, out: *mut c_void, size: u32) -> i32;
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    /// 100 ns ticks since 1601 as nanoseconds since 1970.
    fn ns(ticks: i64) -> i64 {
        ticks.saturating_sub(116_444_736_000_000_000).saturating_mul(100)
    }

    /// `GetFileInformationByName`, which Windows 11 24H2 and later have.
    fn by_name() -> Option<ByName> {
        static FOUND: OnceLock<Option<ByName>> = OnceLock::new();
        *FOUND.get_or_init(|| {
            let dll: Vec<u16> = "api-ms-win-core-file-l2-1-4".encode_utf16().chain([0]).collect();
            // SAFETY: nul-terminated names; the module stays loaded, and a non-null address is the
            // function winbase.h declares with this signature
            unsafe {
                let module = LoadLibraryW(dll.as_ptr());
                let f = if module.is_null() { std::ptr::null_mut() } else { GetProcAddress(module, c"GetFileInformationByName".as_ptr().cast()) };
                (!f.is_null()).then(|| std::mem::transmute::<*mut c_void, ByName>(f))
            }
        })
    }

    /// The change time of the file at `path` (0 when it cannot be read); `link` reads a symbolic
    /// link itself, else a link is followed.
    pub fn of(path: &Path, link: bool) -> i64 {
        let name: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        if let (Some(f), false) = (by_name(), name[..name.len() - 1].contains(&0)) {
            // FILE_STAT_BASIC_INFORMATION: 104 bytes, ChangeTime at 32, FileAttributes at 56
            let mut info = [0u64; 13];
            // SAFETY: a nul-terminated path and a buffer of the size passed
            if unsafe { f(name.as_ptr(), FILE_STAT_BASIC_BY_NAME_INFO, info.as_mut_ptr().cast(), 104) } != 0 {
                // a reparse point by name is the point itself: one to follow takes the handle
                if link || (info[7] as u32) & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
                    return ns(info[4] as i64);
                }
            }
        }
        let flags = FILE_FLAG_BACKUP_SEMANTICS | if link { FILE_FLAG_OPEN_REPARSE_POINT } else { 0 };
        let Ok(file) = std::fs::OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES).share_mode(FILE_SHARE_ALL).custom_flags(flags).open(path) else {
            return 0;
        };
        // FILE_BASIC_INFO: 40 bytes, ChangeTime at 24
        let mut info = [0u64; 5];
        // SAFETY: an open handle and a buffer of the size passed
        match unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), FILE_BASIC_INFO, info.as_mut_ptr().cast(), 40) } {
            0 => 0,
            _ => ns(info[3] as i64),
        }
    }

    /// The change time of each entry of the directory `dir`, by name (empty when it cannot be listed).
    pub fn listing(dir: &Path) -> HashMap<OsString, i64> {
        let mut out = HashMap::new();
        let opened = std::fs::OpenOptions::new().access_mode(FILE_LIST_DIRECTORY).share_mode(FILE_SHARE_ALL).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(dir);
        let Ok(d) = opened else {
            return out;
        };
        // the entries must start on 8-byte boundaries
        let mut buf = vec![0u64; 8192];
        // SAFETY: an open directory handle and a buffer of the size passed
        while unsafe { GetFileInformationByHandleEx(d.as_raw_handle(), FILE_ID_BOTH_DIRECTORY_INFO, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32) } != 0 {
            let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_le_bytes()).collect();
            let u32_at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]) as usize;
            // FILE_ID_BOTH_DIR_INFO: NextEntryOffset at 0, ChangeTime at 32, FileNameLength at 60, FileName at 104
            let mut at = 0;
            while at + 104 <= bytes.len() {
                let change = i64::from_le_bytes(bytes[at + 32..at + 40].try_into().unwrap_or_default());
                let Some(name) = bytes.get(at + 104..at + 104 + u32_at(at + 60)) else { break };
                let name: Vec<u16> = name.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
                out.insert(OsString::from_wide(&name), ns(change));
                match u32_at(at) {
                    0 => break,
                    next => at += next,
                }
            }
        }
        out
    }
}

fn read_file(root: &Path, rel: &str, known_hash: Option<&str>) -> Read {
    let full = root.join(rel);
    let Ok(md) = std::fs::metadata(&full) else {
        return Read::Gone;
    };
    let stat = Stat::of(&full, &md);
    if md.len() > MAX_FILE_BYTES {
        return Read::Skip(stat);
    }
    let Ok(raw) = std::fs::read(&full) else {
        return Read::Gone; // unreadable now: its old chunks must not answer
    };
    if decode_source(&raw).is_none() {
        return Read::Skip(stat);
    }
    let h = content_hash(&raw);
    if Some(h.as_str()) == known_hash {
        return Read::Same(stat);
    }
    let key = crate::artifacts::key(&h, rel);
    Read::Fresh(stat, h, key, raw)
}

/// A file read as new content: its rows from the artifact a store found (with the body to store
/// here when it came from another store), else from a parse.
fn process(rel: &str, read: Read, found: Option<(Found, Option<Vec<u8>>)>, model: Option<&SemanticModel>, prev: Option<PrevParse>) -> Processed {
    let (stat, h, key, raw) = match read {
        Read::Gone => return Processed::Gone,
        Read::Skip(s) => return Processed::Skip(s),
        Read::Same(s) => return Processed::Same(s),
        Read::Fresh(s, h, k, raw) => (s, h, k, raw),
    };
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match found {
        Some((found, body)) => {
            let (rows, lex, imports) = crate::artifacts::derive(rel, found, model);
            (rows, lex, imports, None, body, false)
        }
        None => {
            let source = decode_source(&raw).unwrap_or_default();
            let (rows, lex, imports, tree, body) = build_rows(rel, &source, model, prev);
            (rows, lex, imports, tree, Some(body), true)
        }
    }));
    match built {
        Ok((rows, lex, imports, tree, body, parsed)) => Processed::New(stat, h, key, rows, Box::new(lex), imports, tree, body, parsed),
        Err(_) => {
            eprintln!("[scrutate] skipped {rel}: parser failure");
            Processed::Skip(stat)
        }
    }
}

/// The segments of one index, opened when the vectors of an artifact are first wanted.
struct Segs {
    dir: PathBuf,
    names: Vec<String>,
    /// the vector model whose vectors they hold
    sig: String,
    open: Option<Vec<crate::lex::Segment>>,
}

impl Segs {
    fn new(dir: PathBuf, names: Vec<String>, sig: String) -> Segs {
        Segs { dir, names, sig, open: None }
    }

    /// The stored vectors of chunks `ids` in order, when each has a segment row with one `k` wide.
    fn vectors(&mut self, ids: &[i64], k: usize) -> Option<QVecs> {
        let (dir, names) = (&self.dir, &self.names);
        let segs = self.open.get_or_insert_with(|| names.iter().filter_map(|n| crate::lex::Segment::open(&dir.join(n)).ok()).collect());
        let mut v = QVecs { q: Vec::with_capacity(ids.len() * 2 * k), scale: Vec::with_capacity(ids.len() * 2) };
        for &id in ids {
            let (q, s) = segs.iter().find_map(|g| g.row_of(id).and_then(|r| g.vec_row(r)))?;
            if q.len() != 2 * k || s.len() != 2 {
                return None;
            }
            v.q.extend_from_slice(q);
            v.scale.extend_from_slice(s);
        }
        Some(v)
    }
}

/// The artifact of `key` in the index `db`, with its chunk texts, and with its vectors when
/// `vectors` (the model's width and signature) is given and the segments hold that model's.
/// Damage to the store is an error with `own` (this checkout's index, rebuilt by the one rule)
/// and a miss without (another checkout's, which this one never writes). -> the artifact and
/// its encoded body.
fn find_artifact(db: &Connection, segs: &mut Segs, key: &Key, vectors: Option<(usize, &str)>, own: bool) -> Result<Option<(Found, Vec<u8>)>> {
    let damaged = |why: &'static str| -> Result<Option<(Found, Vec<u8>)>> {
        match own {
            true => Err(Damaged(why).into()),
            false => Ok(None),
        }
    };
    let row: Option<(Vec<u8>, Option<Vec<u8>>)> =
        db.prepare_cached("SELECT body, kept FROM artifacts WHERE key=?1")?.query_row([&key[..]], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let Some((body, kept)) = row else { return Ok(None) };
    let Ok(art) = Artifact::decode(&body) else { return damaged("an artifact does not decode") };
    let (texts, ids) = match kept {
        Some(k) => match crate::artifacts::decode_texts(&k) {
            Ok(t) => (t, None),
            Err(_) => return damaged("the texts of an artifact do not decode"),
        },
        None => {
            // the rows of a path that names it hold its texts and vectors
            let mut q = db.prepare_cached("SELECT id, text FROM chunks WHERE path=(SELECT path FROM files WHERE art=?1 LIMIT 1) ORDER BY id")?;
            let rows: Vec<(i64, Vec<u8>)> = q.query_map([&key[..]], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            let ids: Vec<i64> = rows.iter().zip(&art.chunks).filter(|(_, c)| !crate::parse::SYMBOL_ONLY.contains(&c.kind.as_str())).map(|(r, _)| r.0).collect();
            (rows.into_iter().map(|r| r.1).collect(), Some(ids))
        }
    };
    if texts.len() != art.chunks.len() {
        return damaged("an artifact and the rows made from it disagree");
    }
    let vecs = match (vectors, ids) {
        (Some((k, sig)), Some(ids)) if segs.sig == sig && ids.len() == art.lexed() => segs.vectors(&ids, k),
        _ => None,
    };
    Ok(Some((Found { art, texts, vecs }, body)))
}

/// The code index of the main checkout of the linked git work tree at `root`. Each lookup reads
/// in a snapshot of its own, so the other checkout's writer is held back by none longer than one
/// lookup; its segments are those its manifest listed when this was opened.
struct Other {
    db: Connection,
    segs: Segs,
}

/// `Other` for `root`, when it is a linked work tree whose main checkout has a code index of
/// this schema that is no symbolic link and holds none. Opened read-only: nothing of the other
/// checkout is written (SQLite's shared lock marks its `-shm` file, as every reader's does).
fn open_other(root: &Path) -> Option<Other> {
    let main = crate::files::main_checkout(root)?;
    let dir = crate::files::index_dir(&main).ok()?;
    let db_path = dir.join("index.db");
    if !std::fs::symlink_metadata(&db_path).ok()?.is_file() {
        return None;
    }
    let db = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    db.busy_timeout(Duration::from_secs(1)).ok()?;
    db.execute_batch("BEGIN").ok()?;
    let get = |k: &str| db.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten();
    let (version, names, sig) = (get("schema_version"), get("segments"), get("model_sig"));
    db.execute_batch("COMMIT").ok()?;
    if version? != SCHEMA_VERSION.to_string() {
        return None;
    }
    let names: Vec<String> = serde_json::from_str(&names.unwrap_or_else(|| "[]".into())).ok()?;
    if !names.iter().all(|n| segment_name(n)) {
        return None;
    }
    Some(Other { segs: Segs::new(dir.join("seg"), names, sig.unwrap_or_default()), db })
}

/// A segment name this program writes (`s000001`), so a copied manifest names no other place.
fn segment_name(n: &str) -> bool {
    n.len() > 1 && n.starts_with('s') && n[1..].bytes().all(|b| b.is_ascii_digit())
}

/// A new index of a linked git work tree starts as a copy of its main checkout's index of this
/// schema, parsing rules and vector model: the database read in one snapshot (`VACUUM INTO`, which
/// leaves the other one as it is) and the segments it lists copied. The first update then reads
/// and hashes every file and parses only those whose content differs. When anything fails no
/// database is left and the index is built as usual; copied segments stay, since a process that
/// seeded at the same time may list them. -> whether it was seeded.
fn seed(root: &Path, dir: &Path, sig: &str) -> bool {
    let Some(main) = crate::files::main_checkout(root) else { return false };
    let Ok(main_dir) = crate::files::index_dir(&main) else { return false };
    let src = main_dir.join("index.db");
    if !std::fs::symlink_metadata(&src).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    #[cfg(not(target_os = "wasi"))]
    let tag = format!("seed-{}-{nanos}", std::process::id());
    // wasi has no process id
    #[cfg(target_os = "wasi")]
    let tag = format!("seed-{nanos}");
    let tmp = dir.join(format!("index.db.{tag}"));
    let copied = (|| -> Result<()> {
        let db = Connection::open_with_flags(&src, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_secs(1))?;
        let get = |k: &str| db.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional();
        let same = [("schema_version", SCHEMA_VERSION.to_string()), ("content_rev", CONTENT_REVISION.to_string()), ("model_sig", sig.to_string())];
        for (k, want) in same {
            if get(k)?.as_deref() != Some(want.as_str()) {
                bail!("built by another version or model");
            }
        }
        db.execute("VACUUM INTO ?1", [tmp.to_string_lossy()])?;
        drop(db);
        let names: Vec<String> = {
            let copy = Connection::open(&tmp)?;
            let listed = copy.query_row("SELECT value FROM meta WHERE key='segments'", [], |r| r.get::<_, String>(0)).optional()?;
            serde_json::from_str(&listed.unwrap_or_else(|| "[]".into()))?
        };
        let seg_dir = dir.join("seg");
        std::fs::create_dir_all(&seg_dir)?;
        for n in &names {
            if !segment_name(n) {
                bail!("a segment name this program does not write");
            }
            copy_segment(&main_dir.join("seg").join(n), &seg_dir, n, &tag)?;
        }
        // the main checkout removes a segment it replaced right after its commit, so one the
        // snapshot lists may be gone, or half gone while it was copied: every one must open
        for n in &names {
            crate::lex::Segment::open(&seg_dir.join(n))?;
        }
        // another process that made one first wins; segment copies are kept either way, as its
        // database may name them (an unlisted one goes with gc_segments)
        std::fs::hard_link(&tmp, dir.join("index.db"))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    copied.is_ok()
}

/// The files of the segment directory `from` copied into `seg_dir/name` (a copy of the same
/// name that is there already is the same segment: names are never reused).
fn copy_segment(from: &Path, seg_dir: &Path, name: &str, tag: &str) -> Result<()> {
    let dest = seg_dir.join(name);
    if dest.exists() {
        return Ok(());
    }
    let tmp = seg_dir.join(format!("{name}.{tag}"));
    std::fs::create_dir(&tmp)?;
    let copied = (|| -> Result<()> {
        for e in std::fs::read_dir(from)? {
            let e = e?;
            if !e.file_type()?.is_file() {
                bail!("a segment holds something that is not a file");
            }
            std::fs::copy(e.path(), tmp.join(e.file_name()))?;
        }
        std::fs::rename(&tmp, &dest)?;
        Ok(())
    })();
    if copied.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    copied.or_else(|e| if dest.is_dir() { Ok(()) } else { Err(e) })
}

/// The hash of a file's bytes the index records for it.
pub fn content_hash(raw: &[u8]) -> String {
    format!("{:x}", Sha1::digest(raw))
}

/// Of the files the index of `root` holds, the stat and content hash it recorded, by path; empty
/// when there is no index or it cannot be read. Opens the database read-only and creates nothing.
pub fn recorded(root: &Path) -> HashMap<String, (Stat, String)> {
    let read = || -> rusqlite::Result<HashMap<String, (Stat, String)>> {
        let db = Connection::open_with_flags(root.join(".scrutate").join("index.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version: Option<String> = db.query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| r.get(0)).optional()?;
        if version.as_deref() != Some(SCHEMA_VERSION.to_string().as_str()) {
            return Ok(HashMap::new());
        }
        let mut q = db.prepare("SELECT path, size, mtime_ns, ctime_ns, hash FROM files WHERE hash != 'SKIP' AND ctime_ns IS NOT NULL")?;
        let rows = q.query_map([], |r| Ok((r.get::<_, String>(0)?, (Stat { size: r.get(1)?, mtime: r.get(2)?, ctime: r.get(3)? }, r.get::<_, String>(4)?))))?;
        rows.collect()
    };
    if !exists(root) {
        return HashMap::new();
    }
    read().unwrap_or_default()
}

/// Source text of a file: UTF-16 with a byte-order mark is transcoded (Windows tools write it), a
/// NUL byte in the first 4 KB means binary (None), anything else is lossy UTF-8.
pub fn decode_source(raw: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    let utf16 = |be: bool| {
        let units = raw[2..].chunks(2).map(|c| match c {
            [a, b] if be => u16::from_be_bytes([*a, *b]),
            [a, b] => u16::from_le_bytes([*a, *b]),
            _ => 0xFFFD, // odd trailing byte
        });
        std::borrow::Cow::Owned(char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect())
    };
    if raw.starts_with(&[0xFF, 0xFE]) {
        return Some(utf16(false));
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        return Some(utf16(true));
    }
    if raw[..raw.len().min(4096)].contains(&0u8) {
        return None;
    }
    Some(String::from_utf8_lossy(raw))
}

/// The chunk count `chunks_at` ("<data version> <count>") records, when it was written for `ver`.
fn counted(at: Option<&str>, ver: Option<&str>) -> Option<i64> {
    let (v, n) = at?.split_once(' ')?;
    (Some(v) == ver).then(|| n.parse().ok()).flatten()
}

/// SQLite errors that mean the file is not a usable database (as opposed to busy or I/O trouble).
fn is_corruption(e: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode::*;
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if matches!(f.code, DatabaseCorrupt | NotADatabase))
}

/// Write a file under the index directory as a new file: whatever was at `path` (a planted link
/// included) is removed first, so the write never lands outside.
pub fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::File::options().write(true).create_new(true).open(path)?.write_all(bytes)
}

/// `std::fs::rename`, tried again for up to about two seconds on Windows when it is refused: a
/// virus scanner or the search indexer briefly holds files the index has just written, and a
/// rename of their directory, or over a file it has open, is then denied.
pub fn rename_settled(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut wait = std::time::Duration::from_millis(10);
    loop {
        match std::fs::rename(from, to) {
            // 32 is ERROR_SHARING_VIOLATION, which std leaves uncategorised.
            Err(e)
                if cfg!(windows)
                    && (e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32))
                    && wait < std::time::Duration::from_millis(1300) =>
            {
                std::thread::sleep(wait);
                wait *= 2;
            }
            r => return r,
        }
    }
}

/// The directory's entries written to disk (Unix; Windows has no handle to a directory to flush,
/// and NTFS journals its entries).
pub fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// `.gitignore` of the index directory, unless something is there already.
pub(crate) fn write_gitignore(dir: &Path) -> std::io::Result<()> {
    let gi = dir.join(".gitignore");
    if std::fs::symlink_metadata(&gi).is_ok() {
        return Ok(());
    }
    // the index is a cache; never commit it
    match std::fs::File::options().write(true).create_new(true).open(&gi) {
        Ok(mut f) => std::io::Write::write_all(&mut f, b"*\n"),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether `root` has an index database (built or being built).
pub fn exists(root: &Path) -> bool {
    root.join(".scrutate").join("index.db").exists()
}

/// Index files that are not what this program wrote; handled like SQLite corruption.
#[derive(Debug)]
pub struct Damaged(pub &'static str);

impl std::fmt::Display for Damaged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Damaged {}

/// Another process holds the write lock.
#[derive(Debug)]
pub struct Busy;

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another process is updating the index")
    }
}

impl std::error::Error for Busy {}

/// A read found no committed file while another process holds the write lock: a first build.
#[derive(Debug)]
pub struct BeingBuilt;

impl std::fmt::Display for BeingBuilt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the index is being built by another process; try again when it finishes")
    }
}

impl std::error::Error for BeingBuilt {}

/// The index was built by a newer scrutate. A newer content revision alone is `readable`: it is
/// answered from as it is and never updated. A newer schema or model revision is refused.
#[derive(Debug)]
pub struct Newer {
    pub readable: bool,
    message: String,
}

impl std::fmt::Display for Newer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Newer {}

impl Newer {
    /// Of the meta values `get` reads, the first one higher than this binary's.
    fn of(get: impl Fn(&str) -> Option<String>) -> Option<Newer> {
        let checks = [
            ("schema_version", "schema version", SCHEMA_VERSION),
            ("model_rev", "model revision", MODEL_REVISION),
            ("content_rev", "content revision", CONTENT_REVISION),
        ];
        checks.into_iter().find_map(|(key, what, ours)| {
            let stored = get(key)?.parse::<u32>().ok().filter(|&v| v > ours)?;
            let readable = key == "content_rev";
            let does = if readable { "reads it and does not update it" } else { "cannot read it" };
            let message = format!(
                "index built by a newer scrutate ({what} {stored}, this version has {ours}): this version {does}; use the newer scrutate, or `scrutate index --force` to rebuild it with this one"
            );
            Some(Newer { readable, message })
        })
    }

    /// Say it on stderr, once per process.
    fn note(&self) {
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[scrutate] {self}");
        }
    }
}

/// True when an error anywhere in `e`'s chain is SQLite corruption or other damage to the index
/// files (a chunk text that does not inflate, `Damaged`).
pub fn error_is_corruption(e: &anyhow::Error) -> bool {
    // a `Damaged` context is not one of the chain's links
    e.downcast_ref::<Damaged>().is_some()
        || e.chain().any(|c| {
            c.is::<Damaged>()
                || match c.downcast_ref::<rusqlite::Error>() {
                    Some(rusqlite::Error::FromSqlConversionFailure(_, rusqlite::types::Type::Blob, _)) => true,
                    Some(e) => is_corruption(e),
                    None => false,
                }
        })
}

/// Delete the index files of `root` (used after an operation reported database corruption on a
/// database too large for the open-time integrity check). The caller reopens afterwards.
/// Whether the database file is gone: on Windows a file another handle holds open stays.
pub fn wipe_index(root: &Path) -> bool {
    match crate::files::index_dir(root) {
        Ok(dir) => {
            Index::wipe(&dir, &dir.join("seg"));
            !dir.join("index.db").exists()
        }
        Err(_) => false,
    }
}

/// The metadata of the file at `rel` (from the canonical `root`), when it is a file and not a link,
/// as the walk sees it, and resolves inside the root: a linked folder on the way may lead out.
pub(crate) fn file_inside(root: &Path, rel: &str) -> Option<std::fs::Metadata> {
    let full = root.join(rel);
    let md = std::fs::symlink_metadata(&full).ok().filter(|m| m.is_file())?;
    std::fs::canonicalize(&full).ok()?.starts_with(root).then_some(md)
}

#[cfg(test)]
thread_local! {
    /// Run once at the next `Index::chunks`: a test's writer commits between a query's reads.
    pub static BEFORE_CHUNKS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    /// Run once right after an update loads its root's custom languages.
    pub static AFTER_LANGUAGES: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

/// Whether this process has opened an index, the code index or the text index, so a panic may
/// come from one.
pub static OPENED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long SQLite's busy handler waits for a lock before a statement fails with SQLITE_BUSY;
/// short in this crate's own tests, so a wait past it takes seconds there.
const BUSY: Duration = if cfg!(test) { Duration::from_secs(2) } else { Duration::from_secs(30) };

/// A flag a front end sets to stop (`watch`'s Ctrl-C), which ends every wait for a lock.
static STOP: std::sync::OnceLock<Arc<std::sync::atomic::AtomicBool>> = std::sync::OnceLock::new();

/// Make `flag` end every wait of this process for a lock another process holds.
pub fn stop_waits_on(flag: Arc<std::sync::atomic::AtomicBool>) {
    let _ = STOP.set(flag);
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if matches!(f.code, rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked))
}

/// Whether the front end asked every wait and update to stop (`stop_waits_on`, Ctrl-C under
/// `watch`).
pub(crate) fn stopped() -> bool {
    STOP.get().is_some_and(|s| s.load(std::sync::atomic::Ordering::Relaxed))
}

/// Whether an operation with this `cancel` flag should stop: the flag, or `stopped`.
pub(crate) fn stopping(cancel: &std::sync::atomic::AtomicBool) -> bool {
    cancel.load(std::sync::atomic::Ordering::Relaxed) || stopped()
}

/// Run `f` again while another connection holds a lock it needs (SQLITE_BUSY or LOCKED), up to
/// 10 minutes or until `cancel` or the front end's stop (`stop_waits_on`) is set: every write of
/// the index that waits for another writer waits here. Each try waits at most half a second in
/// SQLite's busy handler, so a stop is seen within that.
fn wait_busy<T>(db: &Connection, cancel: &std::sync::atomic::AtomicBool, mut f: impl FnMut(&Connection) -> rusqlite::Result<T>) -> Result<T> {
    let t0 = Instant::now();
    db.busy_timeout(Duration::from_millis(500))?;
    let r = loop {
        match f(db) {
            Err(e) if is_busy(&e) => {
                if stopping(cancel) {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                if t0.elapsed() > Duration::from_secs(600) {
                    break Err(anyhow::anyhow!("another process has held the index write lock for 10 minutes"));
                }
            }
            r => break r.map_err(Into::into),
        }
    };
    db.busy_timeout(BUSY)?;
    r
}

/// Run `f` again while it fails with SQLITE_BUSY/LOCKED (statements the busy handler does not cover,
/// such as the journal-mode switch of a brand-new database opened by several processes at once).
fn retry_busy<T>(mut f: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let t0 = Instant::now();
    loop {
        match f() {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if matches!(e.code, rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked) && t0.elapsed() < BUSY =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

/// The `art` column and its index, on a `files` table an older schema made without it; another
/// process adding it at the same time is no error.
fn add_art(db: &Connection) -> rusqlite::Result<()> {
    // the index exists on every open but the first of an older index
    if retry_busy(|| db.execute_batch(ART_INDEX)).is_ok() {
        return Ok(());
    }
    let has = |db: &Connection| db.prepare("SELECT 1 FROM pragma_table_info('files') WHERE name='art'").and_then(|mut q| q.exists([]));
    if !has(db)? {
        if let Err(e) = retry_busy(|| db.execute_batch("ALTER TABLE files ADD COLUMN art BLOB")) {
            if !has(db)? {
                return Err(e);
            }
        }
    }
    retry_busy(|| db.execute_batch(ART_INDEX))
}

enum Triage {
    /// use the files as they are
    Reuse,
    /// a readable index of another schema or model: empty it inside the database and re-index
    Rebuild(&'static str),
    /// not a usable database: delete the files
    Garbage(&'static str),
    /// built by a newer scrutate that this one cannot read: leave it alone
    Newer(Newer),
}

/// Files a long call has read and has to read, added to as it goes: an update adds the files it
/// reads, so a call that runs two counts both.
#[derive(Default, Debug)]
pub struct FileCount {
    pub done: std::sync::atomic::AtomicUsize,
    pub total: std::sync::atomic::AtomicUsize,
}

impl FileCount {
    /// (done, total) as they are now.
    pub fn get(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.done.load(Relaxed), self.total.load(Relaxed))
    }
}

pub struct Index {
    pub root: PathBuf,
    /// the root as the caller spelled it, made absolute (through a link, macOS's `/tmp`): a path
    /// spelled through it is a path under `root`
    pub given: PathBuf,
    pub dir: PathBuf,
    pub seg_dir: PathBuf,
    pub models: Arc<ModelSet>,
    /// the signature of `models`' vector model, as the index stores it
    sig: String,
    db: Connection,
    /// print indexing progress to stderr when it is a terminal (set by the CLI)
    pub progress: bool,
    /// set to stop an update between batches of files; the update is rolled back
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    /// the files updates read, for the progress a front end reports
    pub count: Arc<FileCount>,
    /// a read snapshot (`begin_read`) is open on `db`
    reading: bool,
    /// the last update could not truncate the log: the next one tries first
    log_left: bool,
    lex: Option<LexStore>,
    lex_ver: Option<String>,
    last_refresh: Option<Instant>,
    scan_cost: f64,
    /// last syntax tree of recently indexed files, with a use counter for eviction
    trees: HashMap<String, (u64, PrevParse)>,
    tree_tick: u64,
    /// most source bytes the tree cache held at once
    #[cfg(test)]
    trees_peak: usize,
    /// which file `db` has open, to notice another process deleting it and building a new one
    db_id: Option<(u64, u64)>,
    /// files this index parsed since it was opened, the others' rows made from a stored artifact
    #[doc(hidden)]
    pub parsed: usize,
    /// the least the artifacts no file names may keep (`KEPT_MIN_BYTES`); a test sets it lower
    #[doc(hidden)]
    pub kept_min: i64,
}

/// Which files an update looks at.
#[derive(Clone, Copy)]
enum Look<'a> {
    /// every file: a walk of the tree
    Walk,
    /// every file, as a look at the tree already found them
    Found(&'a [crate::files::Found]),
    /// these files alone
    Named(&'a [String]),
}

/// Device and inode of a file. None on Windows, where a file SQLite has open cannot be deleted.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

impl Index {
    pub fn open(root: &Path, models: Arc<ModelSet>) -> Result<Index> {
        OPENED.store(true, std::sync::atomic::Ordering::Relaxed);
        crate::files::index_dir(root)?;
        let given = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
        let root = std::fs::canonicalize(root).with_context(|| format!("open {}", root.display()))?;
        let dir = crate::files::make_index_dir(&root)?;
        let seg_dir = dir.join("seg");
        let db_path = dir.join("index.db");
        let sem_sig = models.signature(&dir)?;
        if !db_path.exists() {
            seed(&root, &dir, &sem_sig);
        }
        let wrong_type = db_path.is_dir() || std::fs::symlink_metadata(&seg_dir).is_ok_and(|m| !m.is_dir());
        let triage = if wrong_type { Triage::Garbage("index files of the wrong type") } else { Self::triage(&db_path) };
        if let Triage::Newer(n) = triage {
            return Err(n.into());
        }
        if let Triage::Garbage(why) = triage {
            // nobody can be using a database that does not open: deleting the files is safe
            eprintln!("[scrutate] index database unusable ({why}); rebuilding");
            Self::wipe(&dir, &seg_dir);
        }
        std::fs::create_dir_all(&seg_dir)?;
        // a lock another process took first (even before the database had its WAL) is waited for
        let never = std::sync::atomic::AtomicBool::new(false);
        let setup = |db: &Connection| -> Result<()> {
            wait_busy(db, &never, |db| db.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(())))?;
            // WAL + NORMAL: durable against process crashes, consistent after power loss (at worst the
            // last transaction rolls back); one fsync per checkpoint instead of per transaction; a
            // log cut back at a checkpoint keeps at most 4 MB of its file
            wait_busy(db, &never, |db| {
                db.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-32000; PRAGMA journal_size_limit=4194304;")
            })
        };
        let schema = |db: &Connection| wait_busy(db, &never, |db| db.execute_batch(SCHEMA).and_then(|()| add_art(db)));
        let read_only = |e: &anyhow::Error| matches!(e.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ffi::ErrorCode::ReadOnly);
        let db = Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;
        let db = match setup(&db) {
            Ok(()) => match schema(&db) {
                Ok(()) => Ok(db),
                // a database that refuses the schema is garbage; a wait that ran out is no damage
                Err(e) if e.downcast_ref::<rusqlite::Error>().is_some() => Err(e),
                Err(e) => return Err(e),
            },
            Err(e) if read_only(&e) && matches!(triage, Triage::Reuse) => {
                drop(db);
                Ok(Self::open_immutable(&db_path, &sem_sig)?)
            }
            Err(e) => return Err(e),
        };
        let db = match db {
            Ok(db) => db,
            Err(e) => {
                // opened but cannot hold the schema: garbage after all
                eprintln!("[scrutate] index database unusable ({e}); rebuilding");
                Self::wipe(&dir, &seg_dir);
                std::fs::create_dir_all(&seg_dir)?;
                let db = Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;
                setup(&db)?;
                schema(&db)?;
                db
            }
        };
        // the questions below keep about 30 statements, more than the default cache of 16
        db.set_prepared_statement_cache_capacity(64);
        let mut ix = Index {
            root,
            given,
            dir,
            seg_dir,
            models,
            sig: sem_sig.clone(),
            db,
            progress: false,
            lex: None,
            lex_ver: None,
            last_refresh: None,
            scan_cost: 1.0,
            trees: HashMap::new(),
            tree_tick: 0,
            #[cfg(test)]
            trees_peak: 0,
            cancel: Arc::default(),
            reading: false,
            log_left: false,
            count: Arc::default(),
            db_id: file_id(&db_path),
            parsed: 0,
            kept_min: KEPT_MIN_BYTES,
        };
        if let Triage::Rebuild(why) = triage {
            // an index of another schema: drop its contents *inside* the database, so
            // other processes using the same index keep a consistent (if briefly empty) view
            eprintln!("[scrutate] {why}; rebuilding the index");
            ix.clear_with(Some(sem_sig.as_str()))?;
        } else {
            // a reader must not need the write lock: another process may hold it for minutes
            if ix.meta("schema_version").as_deref() != Some(SCHEMA_VERSION.to_string().as_str()) {
                ix.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
            }
            // an index with files keeps the vectors of its model until an update makes them anew
            if ix.meta("model_sig").as_deref() != Some(sem_sig.as_str()) && !ix.has_files()? {
                ix.set_meta("model_sig", &sem_sig)?;
                ix.set_meta("model_rev", &MODEL_REVISION.to_string())?;
            }
            if ix.newer().is_none() && ix.meta("content_rev").as_deref() != Some(CONTENT_REVISION.to_string().as_str()) {
                if ix.has_files()? {
                    eprintln!("[scrutate] index built with other parsing rules; every file is read again on the next update");
                } else {
                    ix.set_meta("content_rev", &CONTENT_REVISION.to_string())?;
                }
            }
        }
        Ok(ix)
    }

    /// Another connection on the database this one has open, for a reader beside it in the same
    /// process: the same models, no triage and no schema work, which the first open did. A
    /// database that this way cannot open (a read-only directory) is opened as `open` does.
    pub fn another(&self) -> Result<Index> {
        let db_path = self.dir.join("index.db");
        let opened = Connection::open(&db_path).and_then(|db| {
            db.busy_timeout(BUSY)?;
            // a page cache of 2 MB: with 32 MB each, eight readers held 77 MB more under eight
            // calls, and the system's file cache serves the pages they share as fast
            db.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-2000;")?;
            db.query_row("SELECT count(*) FROM meta", [], |_| Ok(()))?;
            Ok(db)
        });
        let Ok(db) = opened else {
            let mut fresh = Index::open(&self.root, Arc::clone(&self.models))?;
            fresh.given.clone_from(&self.given);
            return Ok(fresh);
        };
        db.set_prepared_statement_cache_capacity(64);
        Ok(Index {
            root: self.root.clone(),
            given: self.given.clone(),
            dir: self.dir.clone(),
            seg_dir: self.seg_dir.clone(),
            models: Arc::clone(&self.models),
            sig: self.sig.clone(),
            db,
            progress: false,
            lex: None,
            lex_ver: None,
            last_refresh: None,
            scan_cost: 1.0,
            trees: HashMap::new(),
            tree_tick: 0,
            #[cfg(test)]
            trees_peak: 0,
            cancel: Arc::default(),
            reading: false,
            log_left: false,
            count: Arc::default(),
            db_id: file_id(&db_path),
            parsed: 0,
            kept_min: self.kept_min,
        })
    }

    /// A WAL database in a read-only directory cannot be read the usual way (its `-shm` cannot be
    /// created). Nobody without root can write it there either, so it is read as a fixed file,
    /// unless a `-wal` holds transactions not yet copied into it.
    fn open_immutable(db_path: &Path, sem_sig: &str) -> Result<Connection> {
        let wal = db_path.with_extension("db-wal");
        if std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
            anyhow::bail!("{}: read-only, and its write-ahead log is not empty", db_path.display());
        }
        let path = db_path.to_string_lossy().replace('%', "%25").replace('?', "%3f").replace('#', "%23");
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI;
        let db = Connection::open_with_flags(format!("file:{path}?immutable=1"), flags)?;
        let get = |k: &str| db.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional();
        if let Some(n) = Newer::of(|k| get(k).ok().flatten()).filter(|n| !n.readable) {
            return Err(n.into());
        }
        if get("schema_version")?.as_deref() != Some(SCHEMA_VERSION.to_string().as_str()) || get("model_sig")?.as_deref() != Some(sem_sig) {
            anyhow::bail!("{}: read-only, and built by another version or model", db_path.display());
        }
        Ok(db)
    }

    /// Forget every indexed file and segment (inside one write transaction); with `stamp_for` (a
    /// model signature), of an index triage found to need a rebuild, stamped with this version in
    /// the same transaction. Another process that found the same
    /// index may have done that first and be building it: then nothing is cleared, and its write
    /// lock is not waited for.
    /// Every file's rows and every segment gone, in the database as it is: what a rebuild does when
    /// the files cannot be deleted. Segment names go on from the last one, never reused.
    pub fn clear_in_place(&mut self) -> Result<()> {
        self.clear_with(None)
    }

    fn clear_with(&mut self, stamp_for: Option<&str>) -> Result<()> {
        let done = |db: &Connection| stamp_for.is_some() && Self::rebuild_reason(db).is_none();
        self.lex = None;
        self.lex_ver = None;
        if !self.begin_waiting(done)? {
            return Ok(());
        }
        if done(&self.db) {
            self.db.execute_batch("ROLLBACK")?;
            return Ok(());
        }
        let old = self.segments();
        self.db.execute_batch("DELETE FROM refs; DELETE FROM imports; DELETE FROM chunks; DELETE FROM files; DELETE FROM artifacts;")?;
        self.set_meta("segments", "[]")?;
        let ver = self.bump_version()?;
        self.set_meta("chunks_at", &format!("{ver} 0"))?;
        if let Some(sig) = stamp_for {
            self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
            self.set_meta("model_sig", sig)?;
            self.set_meta("model_rev", &MODEL_REVISION.to_string())?;
            self.set_meta("content_rev", &CONTENT_REVISION.to_string())?;
        }
        retry_busy(|| self.db.execute_batch("COMMIT"))?;
        for n in old {
            let _ = std::fs::remove_dir_all(self.seg_dir.join(n));
        }
        Ok(())
    }

    /// What to do with the index files found on disk. Only an unmistakably damaged database is
    /// declared garbage: a locked one (another process writing) is reused, since deleting the files
    /// of a live database would break every other process using it.
    fn triage(db_path: &Path) -> Triage {
        if !db_path.exists() {
            return Triage::Reuse;
        }
        let con = match Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(c) => c,
            Err(e) => return if is_corruption(&e) { Triage::Garbage("cannot open") } else { Triage::Reuse },
        };
        let _ = con.busy_timeout(BUSY);
        // a damaged header or a foreign file fails the first real read (NOTADB / CORRUPT)
        if let Err(e) = con.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)) {
            return if is_corruption(&e) { Triage::Garbage("not a database") } else { Triage::Reuse };
        }
        // the full page walk is affordable for small databases; large ones rely on the error-driven
        // recovery in `recover_if_corrupt` (and on `scrutate check` for an explicit audit)
        let size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
        let limit = std::env::var("SCRUTATE_QUICK_CHECK_MAX_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(QUICK_CHECK_MAX_BYTES);
        if size <= limit {
            match con.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0)) {
                Ok(s) if s == "ok" => {}
                Ok(_) => return Triage::Garbage("integrity check failed"),
                Err(e) => return if is_corruption(&e) { Triage::Garbage("integrity check failed") } else { Triage::Reuse },
            }
        }
        let get =
            |k: &str| -> Option<String> { con.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten() };
        if let Some(n) = Newer::of(get).filter(|n| !n.readable) {
            return Triage::Newer(n);
        }
        Self::rebuild_reason(&con).map_or(Triage::Reuse, Triage::Rebuild)
    }

    /// Why a readable index of this or an older version must be emptied and built again. Another
    /// vector model is no reason: an update makes the vectors anew and keeps the rest.
    fn rebuild_reason(db: &Connection) -> Option<&'static str> {
        let get = |k: &str| db.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten();
        if get("schema_version").as_deref() != Some(SCHEMA_VERSION.to_string().as_str()) {
            return Some("index schema changed");
        }
        if get("segments").is_some_and(|v| serde_json::from_str::<Vec<String>>(&v).is_err()) {
            return Some("segment list unreadable");
        }
        Self::chunks_lost(db).ok().flatten()
    }

    /// The `chunks` table is empty although the commit that wrote the current data version
    /// counted rows in it. O(1); a version written by another engine is not judged.
    fn chunks_lost(db: &Connection) -> rusqlite::Result<Option<&'static str>> {
        let (ver, at, any): (Option<String>, Option<String>, bool) = db.query_row(
            "SELECT (SELECT value FROM meta WHERE key='data_version'), (SELECT value FROM meta WHERE key='chunks_at'), EXISTS(SELECT 1 FROM chunks)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((!any && counted(at.as_deref(), ver.as_deref()).is_some_and(|n| n > 0)).then_some("the chunks table lost its rows"))
    }

    pub(crate) fn wipe(dir: &Path, seg_dir: &Path) {
        for name in ["index.db", "index.db-wal", "index.db-shm", "vectors.npz"] {
            let p = dir.join(name);
            let _ = std::fs::remove_file(&p).or_else(|_| std::fs::remove_dir_all(&p));
        }
        let _ = std::fs::remove_dir_all(seg_dir).or_else(|_| std::fs::remove_file(seg_dir));
    }

    // ---- meta helpers
    pub fn meta(&self, key: &str) -> Option<String> {
        self.db.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        retry_busy(|| self.db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)", params![key, value]))?;
        Ok(())
    }

    /// Whether a newer scrutate built this index (or took it over since it was opened).
    pub fn newer(&self) -> Option<Newer> {
        Newer::of(|k| self.meta(k))
    }

    pub fn segments(&self) -> Vec<String> {
        serde_json::from_str(&self.meta("segments").unwrap_or_else(|| "[]".into())).unwrap_or_default()
    }

    /// Call inside a write transaction: then a directory already under the new name was left by a
    /// writer stopped before COMMIT, since no live writer holds an uncommitted name.
    fn new_segment_name(&self) -> Result<String> {
        let listed = self.segments();
        let mut n: i64 = self.meta("seg_counter").and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
        while listed.contains(&format!("s{n:06}")) {
            n += 1;
        }
        // a leftover directory another process still holds open cannot be removed on Windows: its
        // name is skipped; anything else in the way is an error, as before
        let free = |n: i64| -> Result<bool> {
            let name = format!("s{n:06}");
            for p in [self.seg_dir.join(&name), self.seg_dir.join(format!("{name}.tmp"))] {
                match std::fs::symlink_metadata(&p) {
                    Err(_) => {}
                    Ok(m) if m.is_dir() => {
                        if std::fs::remove_dir_all(&p).is_err() {
                            return Ok(false);
                        }
                    }
                    Ok(_) => bail!("remove leftover {}: not a directory", p.display()),
                }
            }
            Ok(true)
        };
        let mut skipped = 0;
        while !free(n)? {
            skipped += 1;
            if skipped > 1000 {
                bail!("no segment name is free in {}", self.seg_dir.display());
            }
            n += 1;
            while listed.contains(&format!("s{n:06}")) {
                n += 1;
            }
        }
        self.set_meta("seg_counter", &n.to_string())?;
        Ok(format!("s{n:06}"))
    }

    /// Delete segment directories no manifest refers to (left by a crash), unless brand new. Call
    /// inside a write transaction, so no writer of this machine has one in flight uncommitted.
    fn gc_segments(&self) {
        let keep: HashSet<String> = self.segments().into_iter().collect();
        let Ok(rd) = std::fs::read_dir(&self.seg_dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if keep.contains(&name) {
                continue;
            }
            let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(0);
            if age > GC_AGE_S {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    /// Which database file this is (device and inode), where the system has such a thing.
    pub(crate) fn db_id(&self) -> Option<(u64, u64)> {
        self.db_id
    }

    /// Open the index again when another process deleted the database this one has open and
    /// built a new one in its place (a rebuild after damage, `index --force`): writing on would go
    /// into the deleted file, and its segments into the new one's directory.
    pub fn reopen_if_replaced(&mut self) -> Result<()> {
        let now = file_id(&self.dir.join("index.db"));
        if self.db_id.is_none() || now.is_none() || now == self.db_id {
            return Ok(());
        }
        eprintln!("[scrutate] index database replaced by another process; opening the new one");
        let mut fresh = Index::open(&self.root, Arc::clone(&self.models))?;
        fresh.progress = self.progress;
        fresh.cancel = Arc::clone(&self.cancel);
        fresh.count = Arc::clone(&self.count);
        fresh.given.clone_from(&self.given);
        let reading = self.reading;
        *self = fresh;
        if reading {
            self.begin_read()?;
        }
        Ok(())
    }

    /// Start a read snapshot: every read until `end_read` sees one commit, which a writer's later
    /// commits do not change (WAL: the writer goes on; the log is not truncated past the snapshot
    /// until it ends). -> false when a transaction is open already (an update), which is left alone.
    pub fn begin_read(&mut self) -> Result<bool> {
        if !self.db.is_autocommit() {
            return Ok(false);
        }
        self.db.execute_batch("BEGIN")?;
        self.reading = true;
        Ok(true)
    }

    /// End the snapshot `begin_read` started.
    pub fn end_read(&mut self) {
        if std::mem::take(&mut self.reading) {
            let _ = self.db.execute_batch("COMMIT");
        }
    }

    /// The names of the segments the store holds open, when it is open.
    pub fn held_segments(&self) -> Option<&[String]> {
        self.lex.as_ref().map(|l| l.names.as_slice())
    }

    /// Close the segment store unless it holds `names`: an index kept idle then holds no file a
    /// writer has replaced (on Unix its disk space is freed, on Windows it can be removed).
    pub fn release_segments_other_than(&mut self, names: &[String]) {
        if self.held_segments().is_some_and(|held| held != names) {
            self.lex = None;
            self.lex_ver = None;
        }
    }

    /// What is wrong with the vectors of `store`, in an index whose chunks have them.
    fn vector_damage(&self, store: &LexStore) -> Option<&'static str> {
        let sig = self.meta("model_sig").filter(|s| s != "none")?;
        if store.segs.iter().any(|g| g.n > 0 && g.vec.is_none()) {
            return Some("a segment has no vectors");
        }
        // the width is known when the model that wrote them is the one loaded
        let k = (sig == self.sig).then(|| self.models.semantic().ok().flatten().map(|m| m.k)).flatten()?;
        store.segs.iter().any(|g| g.vec.is_some() && g.k != k).then_some("a segment's vectors are not as wide as the model's")
    }

    /// The segment store, with statistics for the chunks that are alive right now.
    pub fn lex(&mut self) -> Result<&mut LexStore> {
        self.reopen_if_replaced()?;
        let mut ver = self.meta("data_version").unwrap_or_else(|| "0".into());
        let names = self.segments();
        if self.lex.as_ref().map(|l| l.names != names).unwrap_or(true) {
            let mut names = names;
            let mut attempt = 0;
            let store = loop {
                let opened = LexStore::open(&self.seg_dir, &names).and_then(|s| match self.vector_damage(&s) {
                    Some(why) => Err(Damaged(why).into()),
                    None => Ok(s),
                });
                match opened {
                    Ok(s) => break s,
                    Err(e) => {
                        // another process may have just compacted: the manifest we read named
                        // segments that are gone. Re-read it a few times before calling it damage.
                        attempt += 1;
                        std::thread::sleep(Duration::from_millis(20 * attempt));
                        // a snapshot names the segments of its commit, which a writer may have
                        // replaced and removed since: a later snapshot names the ones there now
                        if self.reading {
                            self.db.execute_batch("COMMIT; BEGIN")?;
                            ver = self.meta("data_version").unwrap_or_else(|| "0".into());
                        }
                        let again = self.segments();
                        if again != names && attempt <= 5 {
                            names = again;
                            continue;
                        }
                        if attempt <= 5 {
                            continue;
                        }
                        // a segment vanished or was damaged while we were running: `Repo` rebuilds
                        return Err(e.context(Damaged("a segment is unreadable")));
                    }
                }
            };
            self.lex = Some(store);
            self.lex_ver = None;
        }
        if self.lex_ver.as_deref() != Some(ver.as_str()) {
            let store = self.lex.as_mut().unwrap();
            let n_rows: usize = store.segs.iter().map(|s| s.n).sum();
            // one statement, so the counts and the version they are checked against are of one commit
            let (at_ver, at, n_all, n_live): (Option<String>, Option<String>, i64, i64) = self.db.query_row(
                "SELECT (SELECT value FROM meta WHERE key='data_version'), (SELECT value FROM meta WHERE key='chunks_at'), \
                 (SELECT COUNT(*) FROM chunks), (SELECT COUNT(*) FROM chunks WHERE kind NOT IN ('constant','field'))",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            if counted(at.as_deref(), at_ver.as_deref()).is_some_and(|n| n != n_all) {
                return Err(Damaged("the chunks table lost rows").into());
            }
            if n_rows as i64 == n_live {
                store.prepare(None)?; // nothing was ever deleted: no need to read every id
            } else {
                let mut st = self.db.prepare("SELECT id FROM chunks ORDER BY id")?;
                let live: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                store.prepare(Some(&live))?;
            }
            self.lex_ver = Some(ver);
        }
        Ok(self.lex.as_mut().unwrap())
    }

    // ---- update
    /// Bring the index up to date with every file of the tree; waits for another writer.
    pub fn update(&mut self, force: bool) -> Result<UpdateStats> {
        self.update_with(force, true, Look::Walk)
    }

    /// `update` with the files a look at the tree found (`files::look`), instead of a walk.
    pub fn update_found(&mut self, force: bool, found: &[crate::files::Found]) -> Result<UpdateStats> {
        self.update_with(force, true, Look::Found(found))
    }

    /// Of `paths` (repository paths the index may hold), the ones whose file is gone or whose
    /// stat differs from what the index recorded. One `stat` and one indexed lookup per path.
    pub fn stale(&self, paths: &[&str]) -> Result<Vec<String>> {
        let ctime = if self.has_ctime() { "ctime_ns" } else { "NULL" };
        let mut q = self.db.prepare_cached(&format!("SELECT size, mtime_ns, {ctime} FROM files WHERE path=?1 AND hash != 'SKIP'"))?;
        let mut out: Vec<String> = Vec::new();
        for &p in paths {
            if out.iter().any(|o| o == p) {
                continue;
            }
            let Some((size, mtime, ct)) = q.query_row([p], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?))).optional()? else {
                continue;
            };
            let full = self.root.join(p);
            let fresh = std::fs::metadata(&full).ok().filter(|m| m.is_file()).map(|m| Stat::of(&full, &m));
            // a row without a ctime is judged by size and mtime here; the next scan reads it again
            if fresh.is_none_or(|s| !s.matches(size, mtime, ct.or(Some(s.ctime)))) {
                out.push(p.to_string());
            }
        }
        Ok(out)
    }

    /// `update` of just these files, without a walk of the tree; without `wait` it fails at once
    /// when another process holds the write lock.
    pub fn update_paths(&mut self, paths: &[String], wait: bool) -> Result<UpdateStats> {
        self.update_with(false, wait, Look::Named(paths))
    }

    fn has_ctime(&self) -> bool {
        self.db.prepare_cached("SELECT 1 FROM pragma_table_info('files') WHERE name='ctime_ns'").and_then(|mut q| q.exists([])).unwrap_or(false)
    }

    /// BEGIN IMMEDIATE, waiting for another writer as long as it writes, up to 10 minutes, or
    /// until cancelled; every write path that waits takes the lock here. -> false, with no lock
    /// taken, once `needless` holds (another process did the work meanwhile).
    fn begin_waiting(&self, needless: impl Fn(&Connection) -> bool) -> Result<bool> {
        wait_busy(&self.db, &self.cancel, |db| if needless(db) { Ok(false) } else { db.execute_batch("BEGIN IMMEDIATE").map(|()| true) })
    }

    /// Copy the log into the database and cut it to nothing, waiting about a second for readers
    /// on an older commit (an operation's snapshot lasts that long at most, mostly far less).
    /// -> whether it was done; when not, the next update tries again first.
    fn truncate_log(&self) -> bool {
        let _ = self.db.busy_timeout(Duration::from_secs(1));
        let done = self.db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0)).is_ok_and(|busy| busy == 0);
        let _ = self.db.busy_timeout(BUSY);
        done
    }

    /// `update`; with `wait` false a write lock held by another process is an error at once.
    fn update_with(&mut self, force: bool, wait: bool, look: Look) -> Result<UpdateStats> {
        let only = match look {
            Look::Named(paths) => Some(paths),
            _ => None,
        };
        self.reopen_if_replaced()?;
        if !self.dir.join("index.db").is_file() {
            return Err(Damaged("index database deleted").into());
        }
        // only a forced update replaces a newer index, and only one this version can read
        if let Some(n) = self.newer().filter(|n| !(force && n.readable)) {
            if only.is_some() && n.readable {
                n.note();
            }
            return Err(n.into());
        }
        let t0 = Instant::now();
        let mut st = UpdateStats { semantic: self.models.semantic_enabled(), ..Default::default() };
        // if the statistics held in memory describe exactly the state we start from, they can be
        // patched afterwards instead of recomputed
        let mut removed: Vec<i64> = Vec::new();
        let mut seg_names: Vec<String> = Vec::new();
        let mut replaced: Vec<String> = Vec::new();
        // a panic in an earlier update may have left its transaction open on this connection
        if !self.db.is_autocommit() {
            let _ = self.db.execute_batch("ROLLBACK");
        }
        if self.log_left {
            self.log_left = !self.truncate_log();
        }
        // one writer at a time; readers keep working (WAL)
        if wait {
            self.begin_waiting(|_| false)?;
        } else {
            self.db.busy_timeout(Duration::ZERO)?;
            let r = self.db.execute_batch("BEGIN IMMEDIATE");
            self.db.busy_timeout(BUSY)?;
            match r {
                Err(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ffi::ErrorCode::DatabaseBusy => return Err(Busy.into()),
                r => r?,
            }
        }
        self.gc_segments();
        let cur_ver = self.meta("data_version").unwrap_or_else(|| "0".into());
        // vectors made anew replace every segment: what is held in memory is read again
        let in_sync = self.meta("model_sig").as_deref() == Some(self.sig.as_str())
            && self.lex.is_some()
            && self.lex_ver.as_deref() == Some(cur_ver.as_str())
            && self.lex.as_ref().map(|l| l.names == self.segments()).unwrap_or(false);
        let run =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.update_inner(force, look, &mut st, &mut removed, &mut seg_names, &mut replaced)));
        let changed = match run {
            Ok(Ok(c)) => c,
            failed => {
                let _ = self.db.execute_batch("ROLLBACK");
                self.evict_trees();
                for name in &seg_names {
                    let _ = std::fs::remove_dir_all(self.seg_dir.join(name));
                }
                match failed {
                    Ok(r) => return r.map(|_| st),
                    // the database is as it was; what is held in memory may describe half an update
                    Err(panic) => {
                        self.lex = None;
                        self.lex_ver = None;
                        std::panic::resume_unwind(panic)
                    }
                }
            }
        };
        // read under the write lock: after COMMIT another writer's version could be read instead
        let committed = self.meta("data_version");
        retry_busy(|| self.db.execute_batch("COMMIT"))?;
        for name in &replaced {
            let _ = std::fs::remove_dir_all(self.seg_dir.join(name));
        }
        if changed {
            let mut patched = false;
            if in_sync {
                let added: Vec<String> = seg_names.clone();
                if self.lex.as_mut().unwrap().apply(&removed, &added).is_ok() {
                    self.lex_ver = committed;
                    patched = true;
                }
            }
            if !patched {
                self.lex = None;
                self.lex_ver = None;
            }
            // the update is committed: a merge that fails leaves more segments, not a wrong index
            match self.compact() {
                Err(e) if error_is_corruption(&e) => return Err(e),
                Err(e) => {
                    eprintln!("[scrutate] compaction failed ({e:#}); the update is saved, the next one merges again");
                    if !self.db.is_autocommit() {
                        let _ = self.db.execute_batch("ROLLBACK");
                    }
                    self.lex = None;
                    self.lex_ver = None;
                }
                Ok(()) => {}
            }
            self.log_left = !self.truncate_log();
            // a long-running process would otherwise keep a build's word caches for its whole life
            rayon::broadcast(|_| crate::text::clear_caches());
            crate::text::clear_caches();
        }
        st.seconds = t0.elapsed().as_secs_f64();
        if only.is_none() {
            self.last_refresh = Some(Instant::now());
        }
        if prof::enabled() {
            prof::report(st.seconds);
        }
        Ok(st)
    }

    /// Err when the files the update leaves would pass the quota, counted before anything is
    /// written. `files` is the whole tree, or with `only` the named files, which replace the
    /// `known` rows of those paths.
    fn within_quota(&self, only: bool, known: &HashMap<String, (i64, i64, Option<i64>, String)>, files: &[(String, Option<Stat>)]) -> Result<()> {
        let quota = crate::files::Quota::from_env()?;
        if !quota.is_set() {
            return Ok(());
        }
        // a file over the size limit is never read
        let read = |size: i64| if (0..=MAX_FILE_BYTES as i64).contains(&size) { size as u64 } else { 0 };
        let mut present: HashMap<&str, u64> = files.iter().filter_map(|(p, s)| Some((p.as_str(), read(s.as_ref()?.size)))).collect();
        let (mut n, mut bytes) = (0u64, 0u64);
        if only {
            let (all, sum): (i64, i64) = self.db.query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN size BETWEEN 0 AND ?1 THEN size ELSE 0 END), 0) FROM files",
                [MAX_FILE_BYTES as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            n = (all as u64).saturating_sub(known.len() as u64);
            bytes = (sum as u64).saturating_sub(known.values().map(|k| read(k.0)).sum());
        } else {
            // a file the walk lists but could not stat is held as it was
            present.extend(files.iter().filter(|f| f.1.is_none()).map(|f| (f.0.as_str(), known.get(&f.0).map_or(0, |k| read(k.0)))));
        }
        n += present.len() as u64;
        bytes += present.values().sum::<u64>();
        quota.check("code index", n, bytes)
    }

    /// Body of `update()`, inside the write transaction. -> whether anything changed.
    fn update_inner(
        &mut self,
        force: bool,
        look: Look,
        st: &mut UpdateStats,
        removed: &mut Vec<i64>,
        seg_names: &mut Vec<String>,
        replaced: &mut Vec<String>,
    ) -> Result<bool> {
        if let Some(why) = Self::chunks_lost(&self.db)? {
            return Err(Damaged(why).into());
        }
        if !self.has_ctime() {
            // an index built before the column existed
            self.db.execute_batch("ALTER TABLE files ADD COLUMN ctime_ns INTEGER")?;
        }
        let mut changed = false;
        let rev = CONTENT_REVISION.to_string();
        let only = matches!(look, Look::Named(_));
        let reparse = !only && self.meta("content_rev").as_deref() != Some(rev.as_str());
        let force = force || reparse;
        // the files of a custom language whose table changed are read again; their artifact keys hold the table
        let customs = crate::custom::load(&self.root)?;
        #[cfg(test)]
        if let Some(f) = AFTER_LANGUAGES.with(|h| h.borrow_mut().take()) {
            f();
        }
        let lang_stamp = customs.stamp();
        let old_stamp = self.meta("languages").unwrap_or_else(|| "{}".into());
        let relang = if only { Vec::new() } else { crate::custom::changed(&old_stamp, &lang_stamp) };
        let relang_file = |rel: &str| !relang.is_empty() && relang.iter().any(|e| crate::parse::basename(rel).to_lowercase().ends_with(e.as_str()));
        let scan_t0 = Instant::now();
        type Known = HashMap<String, (i64, i64, Option<i64>, String)>;
        let mut known: Known = HashMap::new();
        let row =
            |r: &rusqlite::Row| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, String>(4)?)));
        let stat_of = |rel: &str| {
            let full = self.root.join(rel);
            std::fs::metadata(&full).ok().map(|m| Stat::of(&full, &m))
        };
        let files: Vec<(String, Option<Stat>)> = match look {
            Look::Walk | Look::Found(_) => {
                let mut q = self.db.prepare("SELECT path,size,mtime_ns,ctime_ns,hash FROM files")?;
                known = q.query_map([], row)?.collect::<rusqlite::Result<Known>>()?;
                let walked;
                let found = match look {
                    Look::Found(f) => f,
                    _ => {
                        walked = crate::files::look(&self.root, false)?;
                        &walked[..]
                    }
                };
                // a name that is not UTF-8 is looked up by the part of it the index can name, as before
                let files: Vec<(String, Option<Stat>)> =
                    found.iter().filter(|f| f.code).map(|f| (f.path.clone(), if f.lossy { stat_of(&f.path) } else { f.stat })).collect();
                st.files_total = files.len();
                files
            }
            Look::Named(paths) => {
                let mut q = self.db.prepare_cached("SELECT path,size,mtime_ns,ctime_ns,hash FROM files WHERE path=?1")?;
                for p in paths {
                    if let Some((p, k)) = q.query_row([p], row).optional()? {
                        known.insert(p, k);
                    }
                }
                paths.iter().filter(|p| file_inside(&self.root, p).is_some()).map(|p| (p.clone(), stat_of(p))).collect()
            }
        };
        self.within_quota(only, &known, &files)?;
        let fileset: HashSet<&String> = files.iter().map(|f| &f.0).collect();
        let mut gone: Vec<String> = known.keys().filter(|p| !fileset.contains(p)).cloned().collect();
        gone.sort();
        let mut todo: Vec<(String, Option<String>)> = Vec::new();
        for (rel, stat) in &files {
            let Some(stat) = stat else {
                continue;
            };
            let force = force || relang_file(rel);
            if !force {
                if let Some(k) = known.get(rel) {
                    if stat.matches(k.0, k.1, k.2) {
                        continue; // stat unchanged: no read, no hash
                    }
                }
            }
            todo.push((rel.clone(), if force { None } else { known.get(rel).map(|k| k.3.clone()) }));
        }
        if !only {
            self.scan_cost = scan_t0.elapsed().as_secs_f64();
        }
        let switching = self.meta("model_sig").as_deref() != Some(self.sig.as_str());
        let models = Arc::clone(&self.models);
        let t = Instant::now();
        let model = if switching || !todo.is_empty() { models.semantic()? } else { None };
        prof::add(&prof::MODEL, t);
        if switching {
            self.vectors_anew(model, &mut todo, &gone, seg_names, replaced)?;
            self.set_meta("model_sig", &self.sig)?;
            self.set_meta("model_rev", &MODEL_REVISION.to_string())?;
            changed = true;
        }
        let mut next_id: i64 = self.meta("next_id").and_then(|v| v.parse().ok()).unwrap_or(1); // ids are never reused
                                                                                               // what this update names or drops is stamped with the data version it commits
        let stamp = self.meta("data_version").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1;
        let mut kept = 0usize;
        for g in &gone {
            removed.extend(self.drop_file(g, stamp, &mut kept)?);
            st.files_removed += 1;
            changed = true;
        }
        let k = model.map(|m| m.k).unwrap_or(0);
        let mut builder = SegmentBuilder::new(k);
        let t = Instant::now();
        if let (Some(m), false) = (model, todo.is_empty()) {
            m.warm(); // build the weight tables once, before the parallel pass
        }
        prof::add(&prof::MODEL, t);
        let root = self.root.clone();
        let progress = self.progress && std::io::IsTerminal::is_terminal(&std::io::stderr());
        let mut done = 0usize;
        let mut last_tick = Instant::now();
        // the trees parsed first fill the cache, and the others are dropped at once, so the peak stays bounded
        let room = std::sync::atomic::AtomicIsize::new(TREE_CACHE_BYTES as isize);
        let (mut own, mut other) = (None, None);
        self.count.total.fetch_add(todo.len(), std::sync::atomic::Ordering::Relaxed);
        for batch in todo.chunks(256) {
            if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                anyhow::bail!("cancelled");
            }
            let reads: Vec<Read> = batch
                .par_iter()
                .map(|(rel, kh)| {
                    let t = Instant::now();
                    let r = crate::custom::with(&customs, || read_file(&root, rel, kh.as_deref()));
                    prof::add(&prof::READ, t);
                    r
                })
                .collect();
            let t = Instant::now();
            // content new to this path is looked for in the store, then in the main checkout's; a
            // forced update parses every file
            let mut found = Vec::with_capacity(reads.len());
            for r in &reads {
                found.push(match r {
                    Read::Fresh(_, _, key, _) if !force => self.find_anywhere(key, model, &mut own, &mut other)?,
                    _ => None,
                });
            }
            prof::add(&prof::LOOKUP, t);
            let prevs: Vec<Option<PrevParse>> = batch.iter().map(|(rel, _)| self.trees.remove(rel).map(|t| t.1)).collect();
            let results: Vec<(String, Processed)> = batch
                .par_iter()
                .zip(reads)
                .zip(found)
                .zip(prevs)
                .map(|((((rel, _), read), found), prev)| {
                    let mut res = crate::custom::with(&customs, || process(rel, read, found, model, prev));
                    if let Processed::New(.., tree, _, _) = &mut res {
                        let n = tree.as_ref().map_or(0, |t| t.source_len() as isize);
                        if room.fetch_sub(n, std::sync::atomic::Ordering::Relaxed) < n {
                            // a tree that does not fit gives its room back, so smaller ones can still use it
                            room.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                            *tree = None;
                        }
                    }
                    (rel.clone(), res)
                })
                .collect();
            done += batch.len();
            self.count.done.fetch_add(batch.len(), std::sync::atomic::Ordering::Relaxed);
            if progress && (last_tick.elapsed().as_millis() >= 200 || done == todo.len()) {
                eprint!("\r[scrutate] indexing {done}/{} files...", todo.len());
                last_tick = Instant::now();
            }
            for (rel, res) in results {
                match res {
                    Processed::Gone => removed.extend(self.drop_file(&rel, stamp, &mut kept)?),
                    Processed::Skip(s) => {
                        // binary / too large / unparsable: remember so we never re-read it
                        removed.extend(self.drop_file(&rel, stamp, &mut kept)?);
                        self.db.execute(
                            "INSERT OR REPLACE INTO files(path,size,mtime_ns,hash,ctime_ns) VALUES(?1,?2,?3,'SKIP',?4)",
                            params![rel, s.size, s.mtime, s.ctime],
                        )?;
                    }
                    Processed::Same(s) => {
                        // content identical, only the stat moved
                        self.db.execute("UPDATE files SET size=?1, mtime_ns=?2, ctime_ns=?3 WHERE path=?4", params![s.size, s.mtime, s.ctime, rel])?;
                    }
                    Processed::New(s, h, key, rows, lex, imports, tree, body, parsed) => {
                        self.parsed += usize::from(parsed);
                        if let Some(tree) = tree.filter(|t| t.source_len() <= TREE_CACHE_BYTES) {
                            self.tree_tick += 1;
                            self.trees.insert(rel.clone(), (self.tree_tick, tree));
                            #[cfg(test)]
                            {
                                self.trees_peak = self.trees_peak.max(self.trees.values().map(|t| t.1.source_len()).sum());
                            }
                        }
                        let t = Instant::now();
                        removed.extend(self.drop_file(&rel, stamp, &mut kept)?);
                        let first = next_id;
                        next_id = self.insert(&rel, rows, next_id)?;
                        prof::add(&prof::INSERT, t);
                        {
                            let mut ins = self.db.prepare_cached("INSERT INTO imports(path,module,line) VALUES(?1,?2,?3)")?;
                            for (line, module) in imports {
                                ins.execute(params![rel, module, line])?;
                            }
                        }
                        builder.add_file(first, *lex);
                        self.name_artifact(&key, body.as_deref(), stamp)?;
                        self.db.execute(
                            "INSERT OR REPLACE INTO files(path,size,mtime_ns,hash,ctime_ns,art) VALUES(?1,?2,?3,?4,?5,?6)",
                            params![rel, s.size, s.mtime, h, s.ctime, &key[..]],
                        )?;
                        st.files_changed += 1;
                        changed = true;
                        if builder.n >= SEGMENT_ROWS {
                            // flush: keeps the inverted-index build bounded on huge updates
                            let full = std::mem::replace(&mut builder, SegmentBuilder::new(k));
                            self.flush_segment(full, seg_names)?;
                        }
                    }
                }
            }
            self.evict_trees();
        }
        if progress && done > 0 {
            eprint!("\r{}\r", " ".repeat(60)); // clear the progress line
        }
        if builder.n > 0 {
            self.flush_segment(builder, seg_names)?;
        }
        if kept > 0 {
            self.collect_kept()?;
        }
        self.set_meta("next_id", &next_id.to_string())?;
        if reparse {
            self.set_meta("content_rev", &rev)?;
        }
        if !only && lang_stamp != old_stamp {
            self.set_meta("languages", &lang_stamp)?;
        }
        changed = changed || !removed.is_empty();
        st.chunks = self.db.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        if changed {
            let ver = self.bump_version()?; // same transaction as the manifest: readers never see one without the other
            self.set_meta("chunks_at", &format!("{ver} {}", st.chunks))?;
        }
        Ok(changed)
    }

    /// Write every segment again with the vectors `model` makes (none without a model) from the
    /// word counts of each file's artifact, and its other arrays as they are, in place of the old
    /// one in the manifest (inside the update's transaction). No source file is read. One
    /// segment's vectors are held at a time. The rows of files in `todo` or `gone` are left out,
    /// as are those of a file that names no artifact: that file joins `todo`, to be indexed whole.
    fn vectors_anew(
        &mut self,
        model: Option<&SemanticModel>,
        todo: &mut Vec<(String, Option<String>)>,
        gone: &[String],
        seg_names: &mut Vec<String>,
        replaced: &mut Vec<String>,
    ) -> Result<()> {
        let mut q = self.db.prepare(
            "SELECT f.path, f.art, MIN(c.id), SUM(c.kind NOT IN ('constant','field')) FROM files f JOIN chunks c ON c.path = f.path \
             WHERE f.hash != 'SKIP' GROUP BY f.path ORDER BY MIN(c.id)",
        )?;
        type Ranked = (String, Option<Vec<u8>>, i64, i64);
        let files: Vec<Ranked> = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
        drop(q);
        let queued: HashSet<&str> = todo.iter().map(|t| t.0.as_str()).chain(gone.iter().map(String::as_str)).collect();
        let files: Vec<_> = files.into_iter().filter(|f| f.3 > 0 && !queued.contains(f.0.as_str())).collect();
        if let (Some(m), false) = (model, files.is_empty()) {
            m.warm();
        }
        let k = model.map_or(0, |m| m.k);
        let mut again = Vec::new();
        let mut names = Vec::new();
        let old = self.segments();
        for name in &old {
            let seg = crate::lex::Segment::open(&self.seg_dir.join(name)).map_err(|_| Damaged("a listed segment does not open"))?;
            let mine: Vec<&Ranked> = files.iter().filter(|f| seg.row_of(f.2).is_some()).collect();
            let mut vecs = QVecs::default();
            let mut at: HashMap<i64, usize> = HashMap::new();
            let mut alive = vec![false; seg.n];
            for batch in mine.chunks(256) {
                if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    anyhow::bail!("cancelled");
                }
                let made: Vec<Option<QVecs>> = match model {
                    Some(m) => {
                        let mut q = self.db.prepare_cached("SELECT body FROM artifacts WHERE key=?1")?;
                        let bodies = batch
                            .iter()
                            .map(|f| match &f.1 {
                                Some(key) => q
                                    .query_row([key], |r| r.get::<_, Vec<u8>>(0))
                                    .optional()?
                                    .map(Some)
                                    .ok_or_else(|| Damaged("a file names an artifact the store does not hold").into()),
                                None => Ok(None),
                            })
                            .collect::<Result<Vec<Option<Vec<u8>>>>>()?;
                        let made: Vec<Result<Option<QVecs>>> = batch
                            .par_iter()
                            .zip(bodies)
                            .map(|(&&(_, _, _, n), body)| {
                                let Some(body) = body else { return Ok(None) };
                                let art = Artifact::decode(&body).map_err(|_| Damaged("an artifact does not decode"))?;
                                if art.lexed() != n as usize {
                                    return Err(Damaged("an artifact and the rows made from it disagree").into());
                                }
                                Ok(Some(art.vectors(m)))
                            })
                            .collect();
                        made.into_iter().collect::<Result<_>>()?
                    }
                    None => batch.iter().map(|_| Some(QVecs::default())).collect(),
                };
                for (&(rel, _, first, n), v) in batch.iter().zip(made) {
                    let Some(v) = v else {
                        again.push((rel.clone(), None));
                        continue;
                    };
                    for i in 0..*n {
                        let r = seg.row_of(first + i).ok_or(Damaged("a chunk has no segment row"))?;
                        alive[r] = true;
                        if model.is_some() {
                            at.insert(first + i, vecs.scale.len() / 2 + i as usize);
                        }
                    }
                    vecs.q.extend_from_slice(&v.q);
                    vecs.scale.extend_from_slice(&v.scale);
                }
            }
            replaced.push(name.clone());
            if !alive.contains(&true) {
                continue;
            }
            let of = |id: i64| {
                let r = *at.get(&id)?;
                Some((&vecs.q[r * 2 * k..(r + 1) * 2 * k], &vecs.scale[r * 2..r * 2 + 2]))
            };
            let vectors = if model.is_some() { crate::lex::Vectors::Made(k, &of) } else { crate::lex::Vectors::Dropped };
            let dest = self.new_segment_name()?;
            seg_names.push(dest.clone());
            let cancel = Arc::clone(&self.cancel);
            crate::lex::merge_segments_with(&|| stopping(&cancel), &self.seg_dir.join(&dest), &[&seg], &[&alive], vectors)?;
            names.push(dest);
        }
        todo.extend(again);
        self.set_meta("segments", &serde_json::to_string(&names)?)?;
        self.lex = None;
        self.lex_ver = None;
        Ok(())
    }

    /// Drop the least recently parsed trees until the kept sources fit in TREE_CACHE_BYTES.
    fn evict_trees(&mut self) {
        let mut total: usize = self.trees.values().map(|t| t.1.source_len()).sum();
        if total <= TREE_CACHE_BYTES {
            return;
        }
        let mut by_age: Vec<(u64, String)> = self.trees.iter().map(|(p, t)| (t.0, p.clone())).collect();
        by_age.sort_unstable();
        for (_, p) in by_age {
            if total <= TREE_CACHE_BYTES {
                break;
            }
            total -= self.trees.remove(&p).map(|t| t.1.source_len()).unwrap_or(0);
        }
    }

    /// Write one segment and register it in the manifest (inside the caller's transaction).
    fn flush_segment(&mut self, builder: SegmentBuilder, seg_names: &mut Vec<String>) -> Result<()> {
        let t = Instant::now();
        let name = self.new_segment_name()?;
        let cancel = Arc::clone(&self.cancel);
        builder.write(&|| stopping(&cancel), &self.seg_dir.join(&name))?;
        prof::add(&prof::SEGMENT, t);
        let mut names = self.segments();
        names.push(name.clone());
        self.set_meta("segments", &serde_json::to_string(&names)?)?;
        seg_names.push(name);
        Ok(())
    }

    /// `update()` for long-running processes: files edited since the last call become searchable.
    /// The directory is scanned at most once per `min_interval` seconds, and a scan that is cheap is
    /// not held back that long: the wait is capped at ten times what looking for changes cost.
    pub fn refresh(&mut self, min_interval: f64) -> Result<Option<UpdateStats>> {
        self.refresh_with(min_interval, true)
    }

    /// `refresh` before a read: when another process holds the write lock it fails at once
    /// instead of waiting, so the read can answer from the last committed index.
    pub fn refresh_or_fail(&mut self, min_interval: f64) -> Result<Option<UpdateStats>> {
        self.refresh_with(min_interval, false)
    }

    fn refresh_with(&mut self, min_interval: f64, wait: bool) -> Result<Option<UpdateStats>> {
        if let Some(last) = self.last_refresh {
            if last.elapsed().as_secs_f64() < min_interval.min(10.0 * self.scan_cost) {
                return Ok(None);
            }
        }
        if let Some(n) = self.newer().filter(|n| n.readable) {
            n.note();
            return Ok(None);
        }
        let st = self.update_with(false, wait, Look::Walk);
        // a failing update is not retried on every call either; a busy lock costs nothing to try
        if !st.as_ref().is_err_and(|e| e.is::<Busy>()) {
            self.last_refresh = Some(Instant::now());
        }
        st.map(Some)
    }

    /// A new data version, inside the caller's write transaction; a chunk count recorded for the
    /// old one carries over (a compaction moves segment rows, not chunks).
    fn bump_version(&self) -> Result<String> {
        let old = self.meta("data_version");
        let v = (old.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1).to_string();
        self.set_meta("data_version", &v)?;
        if let Some(n) = counted(self.meta("chunks_at").as_deref(), old.as_deref()) {
            self.set_meta("chunks_at", &format!("{v} {n}"))?;
        }
        Ok(v)
    }

    /// Drop dead segments and merge small ones so queries touch few files.
    fn compact(&mut self) -> Result<()> {
        let (names, counts, sizes, alive): (Vec<String>, Vec<usize>, Vec<usize>, Vec<Vec<bool>>) = {
            let store = self.lex()?;
            (store.names.clone(), store.alive_counts(), store.segs.iter().map(|s| s.n).collect(), store.alive.clone())
        };
        let live: Vec<usize> = (0..counts.len()).filter(|&i| counts[i] > 0).collect();
        let mut merge: Vec<usize> = Vec::new();
        if !live.is_empty() {
            let rows: usize = live.iter().map(|&i| sizes[i]).sum();
            let dead_frac = |i: usize| 1.0 - counts[i] as f64 / sizes[i].max(1) as f64;
            let total_dead = 1.0 - live.iter().map(|&i| counts[i]).sum::<usize>() as f64 / rows as f64;
            if rows <= SMALL_INDEX_ROWS {
                // a small index is kept as one segment
                if live.len() > 1 || total_dead > 0.0 {
                    merge = live.clone();
                }
            } else {
                // smallest-first group that fits under the merge cap (bounded memory per merge)
                let mut order = live.clone();
                order.sort_by_key(|&i| counts[i]);
                let mut group: Vec<usize> = Vec::new();
                let mut sum = 0usize;
                for &i in &order {
                    if sum + counts[i] <= MAX_MERGE_ROWS {
                        group.push(i);
                        sum += counts[i];
                    }
                }
                let group_dead = group.iter().any(|&i| dead_frac(i) > 0.3);
                let many = live.len() > MAX_SEGMENTS;
                // segments too small for an inverted-file index are folded together once they add
                // up to one; everything in the group is merged when there are too many segments or
                // one of them is mostly dead; a lone mostly-dead segment is rewritten to purge it
                // by its size as written: a segment that has an inverted-file index stays out when a
                // few of its rows die, else every save near that size would write it again
                let smalls: Vec<usize> = group.iter().copied().filter(|&i| sizes[i] < crate::lex::IVF_MIN_ROWS).collect();
                let smalls_sum: usize = smalls.iter().map(|&i| counts[i]).sum();
                if group.len() > 1 && (many || group_dead) {
                    merge = group;
                } else if smalls.len() > 1 && smalls_sum >= crate::lex::IVF_MIN_ROWS {
                    merge = smalls;
                } else if let Some(&i) = live.iter().find(|&&i| dead_frac(i) > 0.3 && counts[i] <= MAX_MERGE_ROWS) {
                    merge = vec![i];
                }
            }
        }
        if live.len() == names.len() && merge.is_empty() {
            return Ok(());
        }
        let mut new_names: Vec<String> = live.iter().filter(|i| !merge.contains(i)).map(|&i| names[i].clone()).collect();
        if !merge.is_empty() {
            self.begin_waiting(|_| false)?;
            let dest = self.new_segment_name()?;
            retry_busy(|| self.db.execute_batch("COMMIT"))?;
            {
                let store = self.lex.as_ref().unwrap();
                let segs: Vec<&crate::lex::Segment> = merge.iter().map(|&i| &store.segs[i]).collect();
                let al: Vec<&[bool]> = merge.iter().map(|&i| alive[i].as_slice()).collect();
                let cancel = Arc::clone(&self.cancel);
                merge_segments(&|| stopping(&cancel), &self.seg_dir.join(&dest), &segs, &al)?;
            }
            new_names.push(dest);
        }
        self.begin_waiting(|_| false)?;
        if self.segments() != names {
            // another writer changed the manifest while we were merging: our plan is stale. Drop the
            // merged segment (nothing refers to it) and let the next update compact again.
            self.db.execute_batch("ROLLBACK")?;
            for n in new_names.iter().filter(|n| !names.contains(n)) {
                let _ = std::fs::remove_dir_all(self.seg_dir.join(n));
            }
            self.lex = None;
            self.lex_ver = None;
            return Ok(());
        }
        self.set_meta("segments", &serde_json::to_string(&new_names)?)?;
        self.bump_version()?;
        retry_busy(|| self.db.execute_batch("COMMIT"))?;
        self.lex = None;
        self.lex_ver = None;
        let keep: HashSet<&String> = new_names.iter().collect();
        for n in &names {
            if !keep.contains(n) {
                let _ = std::fs::remove_dir_all(self.seg_dir.join(n));
            }
        }
        Ok(())
    }

    fn insert(&self, rel: &str, rows: Vec<NewRow>, mut next_id: i64) -> Result<i64> {
        let mut ins = self.db.prepare_cached(
            "INSERT INTO chunks(id,path,kind,name,qualname,name_key,qual_key,start,end,is_test,ndoc,nbody,text,name_tok) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        )?;
        let mut refs = self.db.prepare_cached("INSERT INTO refs(chunk_id,callee) VALUES(?1,?2)")?;
        for (c, calls, blob) in rows {
            let cid = next_id;
            next_id += 1;
            ins.execute(params![cid, rel, c.kind, c.name, c.qualname, c.name_key, c.qual_key, c.start, c.end, c.is_test, c.ndoc, c.nbody, blob, c.name_tok])?;
            for callee in calls {
                refs.execute(params![cid, callee])?;
            }
        }
        Ok(next_id)
    }

    /// The artifact `key` named by a file: stored with `body` when this store did not hold it, and
    /// its texts no longer kept, since the file's rows hold them.
    fn name_artifact(&self, key: &Key, body: Option<&[u8]>, stamp: i64) -> Result<()> {
        match body {
            Some(b) => self
                .db
                .prepare_cached("INSERT OR REPLACE INTO artifacts(key, body, kept, bytes, used) VALUES(?1, ?2, NULL, ?3, ?4)")?
                .execute(params![&key[..], b, b.len() as i64, stamp])?,
            None => self.db.prepare_cached("UPDATE artifacts SET kept=NULL, bytes=length(body), used=?2 WHERE key=?1")?.execute(params![&key[..], stamp])?,
        };
        Ok(())
    }

    /// `delete_file`, after the chunk texts of the file's artifact move into the store when no
    /// other file names it: the same content under another path, or here again after a branch
    /// switch, is then not parsed. `kept` counts the artifacts that moved.
    fn drop_file(&self, rel: &str, stamp: i64, kept: &mut usize) -> Result<Vec<i64>> {
        let art: Option<Vec<u8>> = self.db.prepare_cached("SELECT art FROM files WHERE path=?1")?.query_row([rel], |r| r.get(0)).optional()?.flatten();
        if let Some(a) = art {
            let shared: bool =
                self.db.prepare_cached("SELECT EXISTS(SELECT 1 FROM files WHERE art=?1 AND path!=?2)")?.query_row(params![a, rel], |r| r.get(0))?;
            if !shared {
                let mut q = self.db.prepare_cached("SELECT text FROM chunks WHERE path=?1 ORDER BY id")?;
                let texts: Vec<Vec<u8>> = q.query_map([rel], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                let texts = crate::artifacts::encode_texts(&texts);
                let n = self.db.prepare_cached("UPDATE artifacts SET kept=?1, bytes=length(body)+?2, used=?3 WHERE key=?4")?.execute(params![
                    texts,
                    texts.len() as i64,
                    stamp,
                    a
                ])?;
                if n == 0 {
                    return Err(Damaged("a file names an artifact the store does not hold").into());
                }
                *kept += 1;
            }
        }
        self.delete_file(rel)
    }

    /// Delete the artifacts no file names beyond the budget, least recently dropped first (inside
    /// the update that dropped some): a quarter of the database's size, at least `kept_min`.
    /// A file's own artifact is never deleted. -> how many went.
    fn collect_kept(&self) -> Result<usize> {
        let size: i64 = self.db.query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |r| r.get(0))?;
        let budget = (size / 4).max(self.kept_min);
        let mut q = self.db.prepare_cached("SELECT key, bytes FROM artifacts WHERE kept IS NOT NULL ORDER BY used DESC, key")?;
        let rows: Vec<(Vec<u8>, i64)> = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let mut del = self.db.prepare_cached("DELETE FROM artifacts WHERE key=?1")?;
        let (mut total, mut gone) = (0i64, 0usize);
        for (key, bytes) in rows {
            total += bytes;
            if total > budget {
                del.execute([key])?;
                gone += 1;
            }
        }
        Ok(gone)
    }

    /// The artifact of `key` from this index's store, else from the main checkout's when this is
    /// a linked work tree (opened at the first miss, None when there is none); with the body to
    /// store here when it came from the other one.
    fn find_anywhere(
        &self,
        key: &Key,
        model: Option<&SemanticModel>,
        own: &mut Option<Segs>,
        other: &mut Option<Option<Other>>,
    ) -> Result<Option<(Found, Option<Vec<u8>>)>> {
        let vectors = model.map(|m| (m.k, self.sig.as_str()));
        let own = own.get_or_insert_with(|| Segs::new(self.seg_dir.clone(), self.segments(), self.meta("model_sig").unwrap_or_default()));
        if let Some((found, _)) = find_artifact(&self.db, own, key, vectors, true)? {
            return Ok(Some((found, None)));
        }
        let Some(o) = other.get_or_insert_with(|| open_other(&self.root)) else { return Ok(None) };
        // another checkout's files are data: anything wrong with them is a miss
        let hit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            o.db.execute_batch("BEGIN")?;
            let hit = find_artifact(&o.db, &mut o.segs, key, vectors, false);
            o.db.execute_batch("COMMIT")?;
            hit
        }));
        Ok(match hit {
            Ok(Ok(Some((found, body)))) => Some((found, Some(body))),
            _ => None,
        })
    }

    /// Chunks vanish from SQLite; their segment rows simply stop being alive. Returns the ids that died.
    fn delete_file(&self, rel: &str) -> Result<Vec<i64>> {
        let mut q = self.db.prepare_cached("SELECT id FROM chunks WHERE path=?1")?;
        let ids: Vec<i64> = q.query_map([rel], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        if !ids.is_empty() {
            self.db.execute("DELETE FROM refs WHERE chunk_id IN (SELECT id FROM chunks WHERE path=?1)", [rel])?;
            self.db.execute("DELETE FROM chunks WHERE path=?1", [rel])?;
        }
        self.db.execute("DELETE FROM imports WHERE path=?1", [rel])?;
        self.db.execute("DELETE FROM files WHERE path=?1", [rel])?;
        Ok(ids)
    }

    // ---- lookups
    pub fn chunk(&self, cid: i64) -> Result<Option<ChunkRow>> {
        Ok(self.db.query_row(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE id=?1"), [cid], ChunkRow::from_row).optional()?)
    }

    /// Smallest non-window chunk containing the line (falls back to windows).
    pub fn chunk_at(&self, path: &str, line: i64) -> Result<Option<ChunkRow>> {
        Ok(self
            .db
            .query_row(
                &format!("SELECT {CHUNK_COLS} FROM chunks WHERE path=?1 AND start<=?2 AND end>=?2 ORDER BY (kind='window'), (end-start) LIMIT 1"),
                params![path, line],
                ChunkRow::from_row,
            )
            .optional()?)
    }

    /// The window that holds the line, the first one when windows overlap.
    pub fn window_at(&self, path: &str, line: i64) -> Result<Option<ChunkRow>> {
        let mut q = self
            .db
            .prepare_cached(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE path=?1 AND kind='window' AND start<=?2 AND end>=?2 ORDER BY start LIMIT 1"))?;
        Ok(q.query_row(params![path, line], ChunkRow::from_row).optional()?)
    }

    /// The chunks of these ids, by id.
    pub fn chunks(&self, ids: &[i64]) -> Result<HashMap<i64, ChunkRow>> {
        #[cfg(test)]
        if let Some(f) = BEFORE_CHUNKS.with(|h| h.borrow_mut().take()) {
            f();
        }
        let mut by_id = HashMap::with_capacity(ids.len());
        for part in ids.chunks(500) {
            let ph = vec!["?"; part.len()].join(",");
            let mut st = self.db.prepare(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE id IN ({ph})"))?;
            for r in st.query_map(params_from_iter(part.iter()), ChunkRow::from_row)? {
                let r = r?;
                by_id.insert(r.id, r);
            }
        }
        Ok(by_id)
    }

    /// The text of the chunks of these ids, by id.
    pub fn texts(&self, ids: &[i64]) -> Result<HashMap<i64, String>> {
        let ph = vec!["?"; ids.len()].join(",");
        let mut st = self.db.prepare(&format!("SELECT id, text FROM chunks WHERE id IN ({ph})"))?;
        let mut texts = HashMap::with_capacity(ids.len());
        for r in st.query_map(params_from_iter(ids.iter()), |r| Ok((r.get::<_, i64>(0)?, unpack_text(r.get_ref(1)?)?)))? {
            let (i, t) = r?;
            texts.insert(i, t);
        }
        Ok(texts)
    }

    /// The texts of every chunk of these kinds, in table order.
    pub fn texts_of_kinds(&self, kinds: &[&str]) -> Result<Vec<String>> {
        let mut st = self.db.prepare(&format!("SELECT text FROM chunks WHERE kind IN ({})", quoted(kinds)))?;
        let rows = st.query_map([], |r| unpack_text(r.get_ref(0)?))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The definitions of a file in source order, outer before inner.
    pub fn outline(&self, path: &str) -> Result<Vec<ChunkRow>> {
        let mut st = self.db.prepare_cached(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE path=?1 AND kind != 'window' ORDER BY start, end DESC"))?;
        let rows = st.query_map([path], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// `outline` without the texts, ties in id order: what name resolution reads of a file.
    pub fn symbols_in(&self, path: &str) -> Result<Vec<Symbol>> {
        let mut st = self
            .db
            .prepare_cached("SELECT id, path, kind, name, qualname, start, end FROM chunks WHERE path=?1 AND kind != 'window' ORDER BY start, end DESC, id")?;
        let rows = st.query_map([path], Symbol::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Every chunk of these kinds without its text, by path, then outer before inner.
    pub fn symbols_of_kinds(&self, kinds: &[&str]) -> Result<Vec<Symbol>> {
        let mut st = self.db.prepare(&format!(
            "SELECT id, path, kind, name, qualname, start, end FROM chunks WHERE kind IN ({}) ORDER BY path, start, end DESC, id",
            quoted(kinds)
        ))?;
        let rows = st.query_map([], Symbol::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The chunk of this id without its text.
    pub fn symbol(&self, id: i64) -> Result<Option<Symbol>> {
        let mut st = self.db.prepare_cached("SELECT id, path, kind, name, qualname, start, end FROM chunks WHERE id=?1")?;
        Ok(st.query_row([id], Symbol::from_row).optional()?)
    }

    /// The chunks whose name is exactly `name`, windows too, in the order of the name index.
    pub fn named(&self, name: &str) -> Result<Vec<Symbol>> {
        let mut st = self.db.prepare_cached("SELECT id, path, kind, name, qualname, start, end FROM chunks WHERE name_key=?1 AND name=?2")?;
        let rows = st.query_map(params![name_key(name), name], Symbol::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The definitions named `name` in any ASCII case.
    pub fn named_any_case(&self, name: &str) -> Result<Vec<Symbol>> {
        let mut st = self.db.prepare_cached("SELECT id, path, kind, name, qualname, start, end FROM chunks WHERE name_key=?1")?;
        let rows: Vec<Symbol> = st.query_map([name_key(name)], Symbol::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows.into_iter().filter(|r| r.name.eq_ignore_ascii_case(name)).collect())
    }

    /// Whether a chunk of one of these kinds is named exactly `name`.
    pub fn defines(&self, name: &str, kinds: &[&str]) -> Result<bool> {
        let mut st = self.db.prepare_cached(&format!("SELECT EXISTS(SELECT 1 FROM chunks WHERE name_key=?1 AND name=?2 AND kind IN ({}))", quoted(kinds)))?;
        Ok(st.query_row(params![name_key(name), name], |r| r.get(0))?)
    }

    /// `named_of_kinds` with the name in any ASCII case.
    pub fn named_of_kinds_any_case(&self, name: &str, kinds: &[&str]) -> Result<Vec<ChunkRow>> {
        let mut st = self.db.prepare_cached(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE name_key=?1 AND kind IN ({}) ORDER BY is_test", quoted(kinds)))?;
        let rows: Vec<ChunkRow> = st.query_map(params![name_key(name)], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows.into_iter().filter(|r| r.name.eq_ignore_ascii_case(name)).collect())
    }

    /// The definitions of these kinds named exactly `name`, those outside tests first.
    pub fn named_of_kinds(&self, name: &str, kinds: &[&str]) -> Result<Vec<ChunkRow>> {
        let mut st = self
            .db
            .prepare_cached(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE name_key=?1 AND name=?2 AND kind IN ({}) ORDER BY is_test", quoted(kinds)))?;
        let rows = st.query_map(params![name_key(name), name], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Every definition whose name key is `bare`'s: the exact spelling first, then those outside
    /// tests, then by place.
    pub fn definitions(&self, bare: &str) -> Result<Vec<ChunkRow>> {
        let mut st = self
            .db
            .prepare_cached(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE name_key=?1 AND kind != 'window' ORDER BY (name != ?2), is_test, path, start"))?;
        let rows = st.query_map(params![name_key(bare), bare], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The class or struct of a file with this qualified name.
    pub fn class_at(&self, path: &str, qualname: &str) -> Result<Option<ChunkRow>> {
        let mut st =
            self.db.prepare_cached(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE path=?1 AND qualname=?2 AND kind IN ('class','struct') LIMIT 1"))?;
        Ok(st.query_row(params![path, qualname], ChunkRow::from_row).optional()?)
    }

    /// Ids of the ranked chunks (no windows, constants or fields) whose name key is one of `keys`.
    pub fn ids_named(&self, keys: &[String], limit: usize) -> Result<Vec<i64>> {
        let ph = vec!["?"; keys.len()].join(",");
        let mut st = self.db.prepare(&format!("SELECT id FROM chunks WHERE name_key IN ({ph}) AND kind NOT IN ('window','constant','field') LIMIT {limit}"))?;
        let ids = st.query_map(params_from_iter(keys.iter()), |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Id and qualified name key of the ranked chunks whose name key is `key`, at most 200.
    pub fn qual_keys_named(&self, key: &str) -> Result<Vec<(i64, String)>> {
        let mut st = self.db.prepare_cached("SELECT id, qual_key FROM chunks WHERE name_key=?1 AND kind NOT IN ('window','constant','field') LIMIT 200")?;
        let rows = st.query_map([key], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Id and qualified name of the definitions whose name key is `key`, in the order of
    /// `symbols_named`, at most 10,000.
    pub fn qualnames_named(&self, key: &str, typed: &str) -> Result<Vec<(i64, String)>> {
        let mut st =
            self.db.prepare_cached(&format!("SELECT id, qualname FROM chunks WHERE name_key=?1 AND kind!='window' {} LIMIT 10000", symbol_order("?2")))?;
        let rows = st.query_map(params![key, typed], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Definitions whose name or qualified name is `name`, or whose qualified name ends in it:
    /// the spelling as typed first, then those outside tests, types before functions, and
    /// declarations without a body last.
    pub fn symbols_named(&self, name: &str, limit: usize) -> Result<Vec<ChunkRow>> {
        let key = name_key(name);
        let last = name_key(name.rsplit('.').next().unwrap_or(name));
        // the tail filter must run before LIMIT
        let mut st = self.db.prepare_cached(&format!(
            "SELECT {CHUNK_COLS} FROM chunks WHERE kind!='window' \
             AND (name_key = ?1 OR qual_key = ?1 OR (name_key = ?3 AND substr(qual_key, -(length(?1) + 1)) = '.' || ?1)) \
             {} LIMIT ?2",
            symbol_order("?4")
        ))?;
        let typed_last = name.rsplit('.').next().unwrap_or(name);
        let rows = st.query_map(params![key, limit as i64, last, typed_last], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Definitions whose name key contains `key`: whole keys first, then prefixes, then those
    /// outside tests and shorter names; at most 200.
    pub fn symbols_containing(&self, key: &str) -> Result<Vec<ChunkRow>> {
        let mut st = self.db.prepare_cached(&format!(
            "SELECT {CHUNK_COLS} FROM chunks WHERE kind != 'window' AND name != '' AND instr(name_key, ?1) > 0 \
             ORDER BY name_key != ?1, substr(name_key, 1, length(?1)) != ?1, is_test, length(name), path, start LIMIT 200"
        ))?;
        let rows = st.query_map([key], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Id, path, qualified name, lines and text of the chunks of these kinds with at least
    /// `min_tokens` body tokens, by id.
    pub fn bodies(&self, kinds: &[&str], min_tokens: usize) -> Result<Vec<Body>> {
        let mut st = self
            .db
            .prepare(&format!("SELECT id, path, qualname, start, end, text FROM chunks WHERE kind IN ({}) AND nbody >= ?1 ORDER BY id", quoted(kinds)))?;
        let rows = st
            .query_map([i64::try_from(min_tokens).unwrap_or(i64::MAX)], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default(), r.get(3)?, r.get(4)?, unpack_text(r.get_ref(5)?)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    // ---- edges
    /// The chunks with a call of `callee`, each once, those outside tests first, then by path and
    /// line, so an index brought up to date answers as a fresh one of the same tree.
    pub fn callers_of(&self, callee: &str) -> Result<Vec<ChunkRow>> {
        let cols = CHUNK_COLS.split(',').map(|c| format!("c.{c}")).collect::<Vec<_>>().join(",");
        let mut st =
            self.db.prepare_cached(&format!("SELECT DISTINCT {cols} FROM refs r JOIN chunks c ON c.id=r.chunk_id WHERE r.callee=?1 ORDER BY c.is_test, c.path, c.start, c.end DESC, c.kind, c.qualname"))?;
        let rows = st.query_map([callee], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Every name a call records, by its ASCII lowercase form.
    pub fn callees_by_lowercase(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut st = self.db.prepare_cached("SELECT DISTINCT callee FROM refs")?;
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for c in st.query_map([], |r| r.get::<_, String>(0))? {
            let c = c?;
            out.entry(c.to_ascii_lowercase()).or_default().push(c);
        }
        Ok(out)
    }

    /// The names chunk `id` calls, each once, sorted.
    pub fn callees_of(&self, id: i64) -> Result<Vec<String>> {
        let mut st = self.db.prepare_cached("SELECT DISTINCT callee FROM refs WHERE chunk_id=?1 ORDER BY callee")?;
        let rows = st.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// How many call edges each of `names` has, namesakes included; a name without any is absent.
    pub fn call_counts(&self, names: &[&String]) -> Result<HashMap<String, i64>> {
        let mut got = HashMap::new();
        for part in names.chunks(400) {
            let ph = vec!["?"; part.len()].join(",");
            let mut st = self.db.prepare(&format!("SELECT callee, COUNT(*) FROM refs WHERE callee IN ({ph}) GROUP BY callee"))?;
            for r in st.query_map(params_from_iter(part.iter()), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (n, c) = r?;
                got.insert(n, c);
            }
        }
        Ok(got)
    }

    /// Every call edge as (the calling chunk's path, the name it calls), by path and name.
    pub fn call_edges(&self) -> Result<Vec<(String, String)>> {
        let mut st = self.db.prepare("SELECT c.path, r.callee FROM refs r JOIN chunks c ON c.id=r.chunk_id ORDER BY c.path, r.callee")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// (name, path) of the definitions of these kinds, each pair once, by name and path.
    pub fn definers(&self, kinds: &[&str]) -> Result<Vec<(String, String)>> {
        let mut st = self.db.prepare(&format!("SELECT DISTINCT name, path FROM chunks WHERE kind IN ({}) ORDER BY name, path", quoted(kinds)))?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The imports a file records, as (line, module), by line.
    pub fn imports_of(&self, path: &str) -> Result<Vec<(i64, String)>> {
        let mut st = self.db.prepare_cached("SELECT line, module FROM imports WHERE path=?1 ORDER BY line")?;
        let rows = st.query_map([path], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// The (path, module) of each recorded import whose module text holds `needle`.
    pub fn imports_holding(&self, needle: &str) -> Result<Vec<(String, String)>> {
        let mut st = self.db.prepare_cached("SELECT DISTINCT path, module FROM imports WHERE instr(module, ?1) > 0 ORDER BY path")?;
        let rows = st.query_map([needle], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Every recorded import, as (path, line, module), by path and line.
    pub fn imports(&self) -> Result<Vec<(String, i64, String)>> {
        let mut st = self.db.prepare("SELECT path, line, module FROM imports ORDER BY path, line")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    // ---- files
    /// Whether the index has a row for `path`: a file it indexed or one it skipped.
    pub fn has_file(&self, path: &str) -> bool {
        self.db.prepare_cached("SELECT 1 FROM files WHERE path=?1").and_then(|mut q| q.exists([path])).unwrap_or(false)
    }

    /// The indexed files, the skipped ones aside, in table order.
    pub fn files(&self) -> Result<Vec<String>> {
        let mut st = self.db.prepare_cached("SELECT path FROM files WHERE hash != 'SKIP'")?;
        let files = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(files)
    }

    /// Of `paths` (from the root), the files the index has no row for that a scan would index,
    /// each under the path a scan lists it by: one lookup and one `stat` each, and the ignore
    /// rules read for those alone.
    pub fn unindexed(&self, paths: &[String]) -> Vec<String> {
        let mut membership: Option<crate::files::Membership> = None;
        let skipped = crate::files::skipped_dirs(&self.root);
        let mut out = Vec::new();
        for p in paths {
            let plain = !p.is_empty() && Path::new(p).components().all(|c| matches!(c, std::path::Component::Normal(_)));
            if !plain {
                continue;
            }
            // through a link that stays inside, or in another case, the file would get a second row
            let real = std::fs::canonicalize(self.root.join(p)).ok();
            let Some(p) = real.and_then(|r| r.strip_prefix(&self.root).ok().map(|r| crate::instant::slashed(r.to_string_lossy()))) else { continue };
            if !crate::parse::is_indexable(&p) || p.split('/').any(|n| skipped.iter().any(|s| s == n)) || self.has_file(&p) || out.contains(&p) {
                continue;
            }
            if !file_inside(&self.root, &p).is_some_and(|m| m.len() <= MAX_FILE_BYTES) {
                continue;
            }
            if membership.get_or_insert_with(|| crate::files::Membership::new(&self.root)).may_contain(&self.root.join(&p)) {
                out.push(p);
            }
        }
        out
    }

    /// How many constants and fields the index holds: they have no segment row.
    pub fn constants(&self) -> i64 {
        self.db.query_row("SELECT COUNT(*) FROM chunks WHERE kind IN ('constant','field')", [], |r| r.get(0)).unwrap_or(0)
    }

    /// Runs `sql` on the index's own connection, to damage the index on purpose in a test.
    #[doc(hidden)]
    pub fn damage_for_tests(&self, sql: &str) -> Result<()> {
        Ok(self.db.execute_batch(sql)?)
    }

    /// The bytes of the artifacts no file names, and of the whole database.
    #[doc(hidden)]
    pub fn store_bytes(&self) -> Result<(i64, i64)> {
        let kept = self.db.query_row("SELECT COALESCE(SUM(bytes), 0) FROM artifacts WHERE kept IS NOT NULL", [], |r| r.get(0))?;
        let size = self.db.query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |r| r.get(0))?;
        Ok((kept, size))
    }

    /// How many artifacts files name, and how many no file names.
    #[doc(hidden)]
    pub fn store_counts(&self) -> Result<(i64, i64)> {
        let named = self.db.query_row("SELECT COUNT(DISTINCT art) FROM files WHERE art IS NOT NULL", [], |r| r.get(0))?;
        let kept = self.db.query_row("SELECT COUNT(*) FROM artifacts WHERE kept IS NOT NULL", [], |r| r.get(0))?;
        Ok((named, kept))
    }

    /// Verify the index. Never repairs anything: `update()` / `rebuild()` do that.
    pub fn check(&mut self, files: bool) -> Result<CheckReport> {
        let mut r = CheckReport::default();
        if let Some(n) = self.newer() {
            n.note();
        }
        let integrity: String = self.db.query_row("PRAGMA integrity_check", [], |row| row.get(0)).unwrap_or_else(|e| format!("error: {e}"));
        r.database_ok = integrity == "ok";
        if !r.database_ok {
            r.problems.push(format!("database integrity: {integrity}"));
        }
        r.chunks_db = self.db.query_row("SELECT COUNT(*) FROM chunks WHERE kind NOT IN ('constant','field')", [], |row| row.get(0))?;
        let names = self.segments();
        r.segments = names.len();
        match LexStore::open(&self.seg_dir, &names) {
            Ok(mut store) => {
                for (name, seg) in names.iter().zip(&store.segs) {
                    if let Err(e) = seg.validate_deep() {
                        r.segments_ok = false;
                        r.problems.push(format!("segment {name}: {e}"));
                    }
                }
                if let Some(why) = self.vector_damage(&store) {
                    r.segments_ok = false;
                    r.problems.push(format!("segments: {why}"));
                }
                let mut live: Vec<i64> = Vec::new();
                let mut st = self.db.prepare("SELECT id FROM chunks ORDER BY id")?;
                for id in st.query_map([], |row| row.get::<_, i64>(0))? {
                    live.push(id?);
                }
                match store.prepare(Some(&live)) {
                    Ok(()) => r.chunks_alive = store.n_alive as i64,
                    Err(e) => {
                        r.segments_ok = false;
                        r.problems.push(format!("segments: {e:#}"));
                    }
                }
                // every chunk in the database must have a row in exactly one segment
                let in_segments: usize = store.segs.iter().map(|s| s.ids().iter().filter(|id| live.binary_search(id).is_ok()).count()).sum();
                if in_segments as i64 != r.chunks_db {
                    r.problems.push(format!("{} chunks in the database, {} of them present in segments", r.chunks_db, in_segments));
                }
            }
            Err(e) => {
                r.segments_ok = false;
                r.problems.push(format!("segments unreadable: {e:#}"));
            }
        }
        r.files_indexed = self.db.query_row("SELECT COUNT(*) FROM files WHERE hash != 'SKIP'", [], |row| row.get::<_, i64>(0))? as usize;
        r.problems.extend(self.check_store()?);
        if files {
            let mut st = self.db.prepare("SELECT path, hash FROM files WHERE hash != 'SKIP'")?;
            let rows: Vec<(String, String)> = st.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            for (rel, hash) in rows {
                match std::fs::read(self.root.join(&rel)) {
                    Ok(raw) => {
                        if format!("{:x}", Sha1::digest(&raw)) != hash {
                            r.files_changed += 1;
                        }
                    }
                    Err(_) => r.files_missing += 1,
                }
            }
            if r.files_changed + r.files_missing > 0 {
                r.problems.push(format!(
                    "{} files changed and {} missing since indexing (stale, not corrupt: `scrutate index` refreshes them)",
                    r.files_changed, r.files_missing
                ));
            }
        }
        Ok(r)
    }

    /// The artifact store's problems: artifacts that do not decode, files that name one the store
    /// does not hold, and files whose rows are not the chunks of their artifact.
    fn check_store(&self) -> Result<Vec<String>> {
        let mut chunks_of: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut bad = 0usize;
        let mut st = self.db.prepare("SELECT key, body, kept FROM artifacts")?;
        for row in st.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, Option<Vec<u8>>>(2)?)))? {
            let (key, body, kept) = row?;
            match Artifact::decode(&body) {
                Ok(a) if kept.as_ref().is_none_or(|k| crate::artifacts::decode_texts(k).is_ok_and(|t| t.len() == a.chunks.len())) => {
                    chunks_of.insert(key, a.chunks.len());
                }
                _ => bad += 1,
            }
        }
        let (mut missing, mut other) = (0usize, 0usize);
        let mut st = self.db.prepare("SELECT f.art, (SELECT COUNT(*) FROM chunks c WHERE c.path = f.path) FROM files f WHERE f.art IS NOT NULL")?;
        for row in st.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))? {
            let (art, n) = row?;
            match chunks_of.get(&art) {
                None => missing += 1,
                Some(&m) if m as i64 != n => other += 1,
                _ => {}
            }
        }
        let mut problems = Vec::new();
        if bad > 0 {
            problems.push(format!("{bad} artifacts of the store do not decode"));
        }
        if missing > 0 {
            problems.push(format!("{missing} files name an artifact the store does not hold"));
        }
        if other > 0 {
            problems.push(format!("{other} files hold other chunks than their artifact"));
        }
        Ok(problems)
    }

    /// Whether the committed index holds any file.
    /// Whether the index holds vectors made by this process's vector model, so a query vector
    /// can be compared with them.
    pub fn vectors_fit(&self) -> bool {
        self.sig != "none" && self.meta("model_sig").as_deref() == Some(self.sig.as_str())
    }

    pub fn has_files(&self) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM files)", [], |r| r.get(0))?)
    }

    pub fn stats(&self) -> Result<serde_json::Value> {
        let q = |s: &str| -> Result<i64> { Ok(self.db.query_row(s, [], |r| r.get(0))?) };
        Ok(serde_json::json!({
            "files": q("SELECT COUNT(*) FROM files WHERE hash != 'SKIP'")?,
            "files_skipped": q("SELECT COUNT(*) FROM files WHERE hash = 'SKIP'")?,
            "chunks": q("SELECT COUNT(*) FROM chunks")?,
            "symbols": q("SELECT COUNT(*) FROM chunks WHERE kind NOT IN ('window','module')")?,
            "call_edges": q("SELECT COUNT(*) FROM refs")?,
            "imports": q("SELECT COUNT(*) FROM imports")?,
            "semantic": self.models.semantic_enabled(),
            "segments": self.segments().len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    /// A write that keeps size and mtime moves the change time, and the walk's listing of a
    /// directory gives each file the change time its own stat gives.
    #[test]
    fn the_change_time_moves_on_a_write_and_a_listing_agrees_with_a_stat() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_ctime_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("d")).unwrap();
        let f = root.join("d/a.txt");
        std::fs::write(&f, "hello\n").unwrap();
        std::fs::write(root.join("d/b.txt"), "other\n").unwrap();
        let stat = || super::Stat::of(&f, &std::fs::metadata(&f).unwrap());
        let (before, mtime) = (stat(), std::fs::metadata(&f).unwrap().modified().unwrap());
        if cfg!(any(unix, windows)) {
            assert_ne!(before.ctime, 0);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, "jello\n").unwrap();
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(mtime).unwrap();
        let after = stat();
        assert_eq!((after.size, after.mtime), (before.size, before.mtime));
        if cfg!(any(unix, windows)) {
            assert_ne!(after.ctime, before.ctime);
        }
        let mut listed = super::Listed::default();
        for e in std::fs::read_dir(root.join("d")).unwrap().flatten() {
            let md = e.metadata().unwrap();
            assert_eq!(listed.stat(&e.path(), &md), super::Stat::of(&e.path(), &std::fs::symlink_metadata(e.path()).unwrap()), "{}", e.path().display());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_index_another_process_already_cleared_for_this_version_is_left_alone() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_clear_race_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let files = |ix: &super::Index| ix.db.query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0)).unwrap();
        // what a process that found the old schema meets once another one cleared the index for
        // this version and is building it: this version's stamps, and the write lock held
        let other = rusqlite::Connection::open(ix.dir.join("index.db")).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let t0 = std::time::Instant::now();
        ix.clear_with(Some("none")).unwrap();
        assert!(t0.elapsed().as_secs() < 5, "{:?}", t0.elapsed());
        other.execute_batch("ROLLBACK").unwrap();
        ix.clear_with(Some("none")).unwrap();
        assert_eq!(files(&ix), 1);
        ix.set_meta("schema_version", &(super::SCHEMA_VERSION - 1).to_string()).unwrap();
        ix.clear_with(Some("none")).unwrap();
        assert_eq!(files(&ix), 0);
        assert_eq!(ix.meta("schema_version"), Some(super::SCHEMA_VERSION.to_string()));
        drop((ix, other));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_update_waits_for_a_writer_past_thirty_seconds_and_stops_when_cancelled() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_wait_writer_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        std::fs::write(root.join("b.py"), "def beta():\n    return 2\n").unwrap();
        let db = ix.dir.join("index.db");
        let hold = |secs: u64| {
            let db = db.clone();
            let (locked, done) = std::sync::mpsc::channel();
            let t = std::thread::spawn(move || {
                let other = rusqlite::Connection::open(db).unwrap();
                other.execute_batch("BEGIN IMMEDIATE").unwrap();
                locked.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_secs(secs));
                other.execute_batch("ROLLBACK").unwrap();
            });
            done.recv().unwrap();
            t
        };
        let writer = hold(34);
        let t0 = std::time::Instant::now();
        ix.update(false).unwrap();
        assert!(t0.elapsed().as_secs() >= 30, "{:?}", t0.elapsed());
        assert_eq!(ix.files().unwrap().len(), 2);
        writer.join().unwrap();

        let writer = hold(8);
        let cancel = std::sync::Arc::clone(&ix.cancel);
        let t0 = std::time::Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        assert!(ix.update(false).is_err());
        assert!(t0.elapsed().as_secs() < 4, "{:?}", t0.elapsed());
        writer.join().unwrap();
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rebuild_for_another_schema_waits_for_a_writer_past_thirty_seconds() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_clear_wait_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        ix.set_meta("schema_version", "0").unwrap();
        let db = ix.dir.join("index.db");
        drop(ix);
        let (locked, done) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let other = rusqlite::Connection::open(db).unwrap();
            other.execute_batch("BEGIN IMMEDIATE").unwrap();
            locked.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(34));
            other.execute_batch("ROLLBACK").unwrap();
        });
        done.recv().unwrap();
        let t0 = std::time::Instant::now();
        let ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        assert!(t0.elapsed().as_secs() >= 30, "{:?}", t0.elapsed());
        assert_eq!(ix.meta("schema_version"), Some(super::SCHEMA_VERSION.to_string()));
        writer.join().unwrap();
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_first_open_waits_for_a_lock_held_before_it_and_keeps_the_holders_data() {
        for wal in [false, true] {
            let root = std::env::temp_dir().join(format!("scrutate_ix_lock_first_{wal}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join(".scrutate")).unwrap();
            std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
            let db = root.join(".scrutate/index.db");
            let (locked, done) = std::sync::mpsc::channel();
            let holder = {
                let db = db.clone();
                std::thread::spawn(move || {
                    let other = rusqlite::Connection::open(db).unwrap();
                    if wal {
                        other.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(())).unwrap();
                    }
                    other.execute_batch("BEGIN IMMEDIATE; CREATE TABLE held(x); INSERT INTO held VALUES (7);").unwrap();
                    locked.send(()).unwrap();
                    std::thread::sleep(super::BUSY + std::time::Duration::from_secs(2));
                    other.execute_batch("COMMIT").unwrap();
                })
            };
            done.recv().unwrap();
            let t0 = std::time::Instant::now();
            let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
            assert!(t0.elapsed() > super::BUSY, "wal {wal}: {:?}", t0.elapsed());
            holder.join().unwrap();
            ix.update(false).unwrap();
            assert_eq!(ix.files().unwrap(), ["a.py"], "wal {wal}");
            let held: i64 = rusqlite::Connection::open(&db).unwrap().query_row("SELECT x FROM held", [], |r| r.get(0)).unwrap();
            assert_eq!(held, 7, "wal {wal}: the holder's table is kept");
            drop(ix);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn an_open_that_must_add_to_the_schema_waits_for_a_writer_and_never_wipes() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_old_writer_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let db = ix.dir.join("index.db");
        drop(ix);
        // what an older version left: no artifacts column, and it is writing now
        let old = rusqlite::Connection::open(&db).unwrap();
        old.execute_batch("DROP INDEX files_art; ALTER TABLE files DROP COLUMN art;").unwrap();
        let before = super::file_id(&db);
        let (locked, done) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            old.execute_batch("BEGIN IMMEDIATE; UPDATE meta SET value=value WHERE key='schema_version';").unwrap();
            locked.send(()).unwrap();
            std::thread::sleep(super::BUSY + std::time::Duration::from_secs(2));
            old.execute_batch("COMMIT").unwrap();
        });
        done.recv().unwrap();
        let ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        writer.join().unwrap();
        assert_eq!(super::file_id(&db), before, "the database was replaced");
        assert_eq!(ix.files().unwrap(), ["a.py"]);
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn check_and_a_search_find_a_segment_whose_vectors_are_gone_or_of_another_width() {
        let models = crate::models::ModelSet::from_env();
        if !models.semantic_enabled() {
            return;
        }
        let root = std::env::temp_dir().join(format!("scrutate_ix_vectors_{}", std::process::id()));
        for case in ["gone", "wide"] {
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n\n\ndef beta():\n    return 2\n").unwrap();
            let mut ix = super::Index::open(&root, std::sync::Arc::clone(&models)).unwrap();
            ix.update(false).unwrap();
            let seg = ix.seg_dir.join(&ix.segments()[0]);
            // Windows refuses to replace or remove a file while a map of it is open
            drop(ix);
            if case == "gone" {
                std::fs::remove_file(seg.join("vec.npy")).unwrap();
                std::fs::remove_file(seg.join("vscale.npy")).unwrap();
            } else {
                // twice the model's width, the rows padded with zeros
                let (n, k) = {
                    let v = crate::npy::Npy::open(&seg.join("vec.npy")).unwrap();
                    (v.shape[0], v.shape[2])
                };
                crate::npy::save(&seg.join("vec.npy"), &vec![0i8; n * 2 * 2 * k], &[n, 2, 2 * k]).unwrap();
            }
            let mut ix = super::Index::open(&root, std::sync::Arc::clone(&models)).unwrap();
            let report = ix.check(false).unwrap();
            assert!(!report.ok(), "{case}: check said OK");
            let e = ix.lex().err().unwrap_or_else(|| panic!("{case}: the segment opened"));
            assert!(super::error_is_corruption(&e), "{case}: {e:#}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_update_parses_under_its_own_root_s_languages_while_another_root_s_are_loaded() {
        let base = std::env::temp_dir().join(format!("scrutate_ix_two_roots_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (a, b) = (base.join("a"), base.join("b"));
        for (root, word) in [(&a, "fn"), (&b, "def")] {
            std::fs::create_dir_all(root.join(".git")).unwrap();
            let table = format!(
                "[[language]]\nname = \"twolang\"\nextensions = [\"twl\"]\ndefinitions = [\n  {{ kind = \"function\", pattern = '^{word}\\s+([a-z]+)' }},\n]\n"
            );
            std::fs::write(root.join(".scrutate.toml"), table).unwrap();
        }
        std::fs::write(a.join("x.twl"), "fn alpha\n  1\nend\n").unwrap();
        let other = b.clone();
        super::AFTER_LANGUAGES.with(|h| *h.borrow_mut() = Some(Box::new(move || drop(crate::custom::load(&other).unwrap()))));
        let mut ix = super::Index::open(&a, crate::models::ModelSet::from_env()).unwrap();
        ix.update(false).unwrap();
        let names: Vec<String> =
            ix.db.prepare("SELECT name FROM chunks WHERE kind = 'function'").unwrap().query_map([], |r| r.get(0)).unwrap().map(|n| n.unwrap()).collect();
        assert_eq!(names, ["alpha"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn update_counts_as_a_refresh() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_refresh_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        assert!(ix.last_refresh.is_none());
        ix.update(false).unwrap();
        assert!(ix.last_refresh.is_some());
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn update_frees_the_word_caches_of_every_thread() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_caches_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..40 {
            std::fs::write(root.join(format!("m{i}.py")), format!("def handler_{i}(request):\n    return parse_body(request)\n")).unwrap();
        }
        // a pool of its own: other tests fill the global pool's caches at the same time
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        let left = pool.install(|| {
            let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
            ix.update(false).unwrap();
            rayon::broadcast(|_| crate::text::cache_len()).into_iter().sum::<usize>() + crate::text::cache_len()
        });
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(left, 0);
    }

    #[test]
    fn a_large_segment_is_not_rewritten_when_a_few_of_its_rows_die() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_compact_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let funcs = |from: usize, n: usize| (from..from + n).map(|i| format!("def f{i}():\n    return {i}\n\n")).collect::<String>();
        // one segment just over the size that gets an inverted-file index
        std::fs::write(root.join("big.py"), funcs(0, crate::lex::IVF_MIN_ROWS - 2)).unwrap();
        std::fs::write(root.join("small.py"), funcs(100_000, 10)).unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let first = ix.segments();
        let rows = ix.db.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
        assert!(first.len() == 1 && rows >= crate::lex::IVF_MIN_ROWS && rows - 11 < crate::lex::IVF_MIN_ROWS, "{first:?} {rows}");
        // a few of its rows die, and the edit arrives as a small segment of its own
        std::fs::write(root.join("small.py"), funcs(200_000, 10)).unwrap();
        ix.update(false).unwrap();
        let now = ix.segments();
        assert!(now.contains(&first[0]) && now.len() == 2, "the large segment was written again: {first:?} -> {now:?}");
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_watch_burst_of_files_updates_those_files_alone() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_burst_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for name in ["a.py", "b.py", "c.py"] {
            std::fs::write(root.join(name), "def one():\n    return 1\n").unwrap();
        }
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let paths = |ix: &super::Index| {
            ix.db.prepare("SELECT path FROM files ORDER BY path").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<Vec<String>, _>>().unwrap()
        };
        std::fs::write(root.join("a.py"), "def two():\n    return 2\n").unwrap();
        std::fs::write(root.join("b.py"), "def two():\n    return 2\n").unwrap();
        std::fs::remove_file(root.join("c.py")).unwrap();
        std::fs::write(root.join("d.py"), "def new():\n    return 3\n").unwrap();
        let st = ix.update_paths(&["a.py".into(), "c.py".into(), "d.py".into()], true).unwrap();
        assert_eq!((st.files_changed, st.files_removed), (2, 1));
        assert_eq!(paths(&ix), ["a.py", "b.py", "d.py"]);
        // b.py was not named, so only a walk sees it
        assert_eq!(ix.stale(&["b.py"]).unwrap(), ["b.py"]);
        assert_eq!(ix.update(false).unwrap().files_changed, 1);
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_that_keep_no_tree_take_none_of_the_tree_cache() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_trees_mixed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..20 {
            std::fs::write(root.join(format!("a{i}.c")), format!("int f{i}(int x) {{ return x + {i}; }}\n")).unwrap();
        }
        // listed after the C files, and more of them than the cache holds
        let body: String = (0..400).map(|i| format!("def f{i}(x):\n    return x + {i}\n")).collect();
        for i in 0..300 {
            std::fs::write(root.join(format!("z{i}.py")), &body).unwrap();
        }
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let held = ix.trees.len();
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(held, 20);
    }

    #[test]
    fn a_tree_too_large_for_the_room_left_leaves_that_room_to_smaller_trees() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_trees_large_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // three trees of 0.9 MB, of which two fit; the small trees come in the next batch
        for i in 0..3 {
            std::fs::write(root.join(format!("a{i}.c")), "int g(void);\n".repeat(69_000)).unwrap();
        }
        for i in 0..260 {
            std::fs::write(root.join(format!("b{i:03}.h")), "class B { public: int f(); };\n").unwrap();
        }
        for i in 0..20 {
            std::fs::write(root.join(format!("z{i}.c")), format!("int f{i}(int x) {{ return x + {i}; }}\n")).unwrap();
        }
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let large = ix.trees.keys().filter(|p| p.starts_with('a')).count();
        let small = ix.trees.keys().filter(|p| p.starts_with('z')).count();
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!((large, small), (2, 20));
    }

    #[test]
    fn the_tree_cache_counts_the_language_a_file_parses_as_and_only_files_that_are_parsed() {
        let tree = |name: &str, extra: &dyn Fn(&std::path::Path)| {
            let root = std::env::temp_dir().join(format!("scrutate_ix_trees_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            for i in 0..20 {
                std::fs::write(root.join(format!("a{i}.c")), format!("int f{i}(int x) {{ return x + {i}; }}\n")).unwrap();
            }
            extra(&root);
            let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
            ix.update(false).unwrap();
            let c_trees = ix.trees.keys().filter(|p| p.ends_with(".c")).count();
            let peak = ix.trees_peak;
            drop(ix);
            let _ = std::fs::remove_dir_all(&root);
            (c_trees, peak)
        };
        // C++ headers keep no tree, and a file over the size limit is not parsed at all
        let cpp: String = (0..300).map(|i| format!("class C{i} {{ public: int f(); }};\n")).collect();
        let (c_trees, _) = tree("cpp", &|root| {
            for i in 0..300 {
                std::fs::write(root.join(format!("z{i}.h")), &cpp).unwrap();
            }
            std::fs::write(root.join("zz.c"), "int g(void);\n".repeat(110_000)).unwrap();
        });
        assert_eq!(c_trees, 20);
        // Objective-C `.m` files keep trees, within the bound
        let objc: String = (0..300).map(|i| format!("- (int)f{i} {{ return {i}; }}\n")).collect();
        let (c_trees, peak) = tree("objc", &|root| {
            for i in 0..300 {
                std::fs::write(root.join(format!("m{i}.m")), format!("@implementation M{i}\n{objc}@end\n")).unwrap();
            }
        });
        assert!(peak <= super::TREE_CACHE_BYTES, "peak {peak}");
        assert_eq!(c_trees, 20);
    }

    #[test]
    fn a_build_of_many_c_files_holds_no_more_trees_than_the_cache_bound() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_trees_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let body: String = (0..200).map(|i| format!("int f{i}(int x) {{ return x + {i}; }}\n")).collect();
        for i in 0..1000 {
            std::fs::write(root.join(format!("m{i}.c")), format!("/* {i} */\n{body}")).unwrap();
        }
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let held: usize = ix.trees.values().map(|t| t.1.source_len()).sum();
        let peak = ix.trees_peak;
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
        assert!(held <= super::TREE_CACHE_BYTES, "{held}");
        assert!(peak <= super::TREE_CACHE_BYTES, "peak {peak}");
    }
}
