//! Structural rewrite: each match of a tree-sitter query replaced with a template of its
//! captures, shown as a unified diff or written file by file, whole, by rename.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

/// One file the rewrite changes.
pub struct FileChange {
    pub path: PathBuf,
    /// where it was under the root when read, with no link on the way
    inside: PathBuf,
    /// the path as `tsquery` prints it
    pub shown: String,
    /// the bytes read, which must still be on disk when the file is written
    pub read: Vec<u8>,
    pub new: Vec<u8>,
    /// matches replaced
    pub replaced: usize,
    /// each replaced range: (old start, old end, new start, new end) in bytes
    spans: Vec<(usize, usize, usize, usize)>,
}

/// What a rewrite would do: the files it changes, in path order, and why it leaves the others
/// and some matches alone.
pub struct Plan {
    /// the root as the plan opened it
    root: Dir,
    pub files: Vec<FileChange>,
    /// a file left alone, as `PATH: why`
    pub left: Vec<String>,
    /// a match inside another one, as `PATH:LINE:COLUMN: why`
    pub skipped: Vec<String>,
}

enum Piece {
    Text(String),
    Capture(usize),
}

/// The template's pieces: `$NAME` and `${NAME}` the text of capture `@NAME`, `$$` a dollar.
fn template(t: &str, names: &[&str]) -> Result<Vec<Piece>> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = t;
    while let Some(i) = rest.find('$') {
        text.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('$') {
            text.push('$');
            rest = r;
            continue;
        }
        let (name, after) = if let Some(r) = rest.strip_prefix('{') {
            let Some(end) = r.find('}') else { bail!("the template has a ${{ without its }}") };
            (&r[..end], &r[end + 1..])
        } else {
            let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
            (&rest[..end], &rest[end..])
        };
        if name.is_empty() {
            bail!("the template has a $ without a capture name: write $$ for a dollar");
        }
        let Some(k) = names.iter().position(|n| *n == name) else {
            bail!("the template names ${name}, but the query has no @{name} (it has {})", names.iter().map(|n| format!("@{n}")).collect::<Vec<_>>().join(", "));
        };
        out.push(Piece::Text(std::mem::take(&mut text)));
        out.push(Piece::Capture(k));
        rest = after;
    }
    text.push_str(rest);
    out.push(Piece::Text(text));
    Ok(out)
}

/// Whether an error or missing node of the tree touches `start..end`.
fn broken(root: tree_sitter::Node, start: usize, end: usize) -> bool {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if !n.has_error() || n.end_byte() < start || n.start_byte() > end {
            continue;
        }
        if n.is_error() || n.is_missing() {
            return true;
        }
        let mut c = n.walk();
        stack.extend(n.children(&mut c));
    }
    false
}

/// The rewrite of each file of `lang` under `paths` with `template`, read and parsed now (the
/// capture cache is not used). Files outside `root`, and links, are left alone.
pub fn plan(lang: &str, query: &str, template_text: &str, paths: &[PathBuf], root: &Path) -> Result<Plan> {
    use rayon::prelude::*;
    let (spec, language, q) = crate::intel::compile_query(lang, query)?;
    let names = q.capture_names();
    let pieces = template(template_text, names)?;
    let outer = names.iter().position(|n| *n == "match");
    // opened before its name is resolved: a swap after this point changes neither what is read
    // nor where it is written
    let dir = Dir::root(root)?;
    let top = std::fs::canonicalize(root)?;
    let files = crate::intel::query_files(spec, paths, true);
    let results: Vec<(Option<FileChange>, Option<String>, Vec<String>)> = files
        .par_iter()
        .map_init(
            || {
                let mut p = tree_sitter::Parser::new();
                let ok = p.set_language(&language).is_ok();
                (p, ok)
            },
            |(parser, ok), f| {
                let mut skipped = Vec::new();
                let alone = |why: &str, skipped: Vec<String>| (None, Some(format!("{}: {why}, left alone", f.shown)), skipped);
                if !*ok {
                    return alone("the parser did not load", skipped);
                }
                let Some(inside) = std::fs::canonicalize(&f.path).ok().and_then(|p| p.strip_prefix(&top).ok().map(Path::to_path_buf)) else {
                    return alone("outside the root", skipped);
                };
                let (parent, name) = split(&inside);
                let Ok(at) = dir.below(parent) else { return alone("could not be read", skipped) };
                if !at.regular(name).is_ok_and(|p| p.is_some()) {
                    return alone("not a regular file", skipped);
                }
                let Ok(raw) = at.read(name) else { return alone("could not be read", skipped) };
                let ours = |src: &str| crate::parse::spec_for(&f.rel, src).map(|s| s.id) == Some(spec.id);
                let src = match crate::index::decode_source(&raw) {
                    Some(std::borrow::Cow::Borrowed(s)) if ours(s) => s,
                    // UTF-16, or bytes that are not UTF-8: the parsed text is not the file
                    Some(std::borrow::Cow::Owned(s)) if ours(&s) => return alone("not UTF-8 text", skipped),
                    _ => return (None, None, skipped),
                };
                let Some(tree) = parser.parse(src, None) else { return alone("did not parse", skipped) };
                // (start, end, replacement, pattern)
                let mut edits: Vec<(usize, usize, String, usize)> = Vec::new();
                let mut cursor = tree_sitter::QueryCursor::new();
                cursor.set_match_limit(1 << 16);
                let mut matches = cursor.matches(&q, tree.root_node(), src.as_bytes());
                use tree_sitter::StreamingIterator;
                while let Some(m) = matches.next() {
                    // a capture's text runs from its first node to its last
                    let span = |k: usize| {
                        let nodes = m.captures.iter().filter(|c| c.index as usize == k).map(|c| c.node.byte_range());
                        nodes.fold(None, |acc: Option<(usize, usize)>, r| Some(acc.map_or((r.start, r.end), |(s, e)| (s.min(r.start), e.max(r.end)))))
                    };
                    // without @match the widest capture, the first written on a tie
                    let target = outer.and_then(span).or_else(|| (0..names.len()).filter_map(span).min_by_key(|&(s, e)| (s, std::cmp::Reverse(e))));
                    let Some((start, end)) = target else { continue };
                    let mut repl = String::new();
                    for p in &pieces {
                        match p {
                            Piece::Text(t) => repl.push_str(t),
                            Piece::Capture(k) => repl.push_str(span(*k).map_or("", |(s, e)| &src[s..e])),
                        }
                    }
                    if repl != src[start..end] {
                        edits.push((start, end, repl, m.pattern_index));
                    }
                }
                // outermost first; a match that starts inside a kept one is not rewritten
                edits.sort_by_key(|e| (e.0, std::cmp::Reverse(e.1), e.3));
                let mut kept: Vec<(usize, usize, String)> = Vec::new();
                for (start, end, repl, _) in edits {
                    if let Some(last) = kept.last().filter(|k| start < k.1 || (start, end) == (k.0, k.1)) {
                        let at = |b: usize| (src[..b].matches('\n').count() + 1, b - src[..b].rfind('\n').map_or(0, |i| i + 1) + 1);
                        let ((l, c), (ol, oc)) = (at(start), at(last.0));
                        skipped.push(format!("{}:{l}:{c}: inside the match at {ol}:{oc}, not rewritten", f.shown));
                        continue;
                    }
                    kept.push((start, end, repl));
                }
                if kept.is_empty() {
                    return (None, None, skipped);
                }
                if kept.iter().any(|k| broken(tree.root_node(), k.0, k.1)) {
                    return alone("a match holds text that does not parse", skipped);
                }
                let mut new = String::with_capacity(src.len());
                let mut at = 0;
                let mut spans = Vec::with_capacity(kept.len());
                for (start, end, repl) in &kept {
                    new.push_str(&src[at..*start]);
                    spans.push((*start, *end, new.len(), new.len() + repl.len()));
                    new.push_str(repl);
                    at = *end;
                }
                new.push_str(&src[at..]);
                let parsed = parser.parse(&new, None);
                if !parsed.is_some_and(|t| !spans.iter().any(|s| broken(t.root_node(), s.2, s.3))) {
                    return alone("the rewritten text does not parse", skipped);
                }
                let change = FileChange { path: f.path.clone(), inside, shown: f.shown.clone(), replaced: kept.len(), new: new.into_bytes(), read: raw, spans };
                (Some(change), None, skipped)
            },
        )
        .collect();
    let mut plan = Plan { root: dir, files: Vec::new(), left: Vec::new(), skipped: Vec::new() };
    for (change, left, skipped) in results {
        plan.files.extend(change);
        plan.left.extend(left);
        plan.skipped.extend(skipped);
    }
    Ok(plan)
}

impl Plan {
    /// A unified diff of every change, three lines of context, paths as shown (`patch -p0` applies it).
    pub fn diff(&self) -> String {
        let mut out = String::new();
        for f in &self.files {
            // both sides are UTF-8: the plan takes no other file
            out.push_str(&unified(&f.shown, &String::from_utf8_lossy(&f.read), &String::from_utf8_lossy(&f.new), &f.spans));
        }
        out
    }

    /// Write each changed file whole by rename, only while its bytes on disk are those read.
    /// Returns, per file in order, why it was left alone (`PATH: why, left alone`) or nothing.
    pub fn write(&self) -> Vec<Option<String>> {
        self.files.iter().map(|f| write_one(&self.root, f).err().map(|why| format!("{}: {why}, left alone", f.shown))).collect()
    }
}

/// A directory of the repository, opened to read and write the files in it by name: a handle.
/// The root is opened once when the plan starts, and each directory below it one name at a time,
/// never through a link (`O_NOFOLLOW` on Unix, `FILE_OPEN_REPARSE_POINT` on Windows, where a
/// junction is refused too), so a directory swapped for a link afterwards (the root included) is
/// never followed.
struct Dir(
    #[cfg(unix)] rustix::fd::OwnedFd,
    #[cfg(windows)] std::os::windows::io::OwnedHandle,
    // shortcut: goes by path where neither openat nor NtCreateFile is used (wasi); upgrade if
    // rewrite runs there on a tree others can change
    #[cfg(not(any(unix, windows)))] PathBuf,
);

#[cfg(unix)]
const DIR_FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY.union(rustix::fs::OFlags::DIRECTORY).union(rustix::fs::OFlags::CLOEXEC);

#[cfg(windows)]
use windows_sys::{
    Wdk::Storage::FileSystem::{FILE_CREATE, FILE_DELETE_ON_CLOSE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN},
    Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_TRAVERSE,
    },
};

/// The two NT calls the Windows `Dir` needs: an open and a rename relative to a directory handle.
#[cfg(windows)]
mod nt {
    use std::ffi::OsStr;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FileRenameInformation, NtCreateFile, NtSetInformationFile, FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT,
    };
    use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING};
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE};
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    /// `name` as UTF-16 and its length in bytes.
    fn wide(name: &OsStr) -> std::io::Result<(Vec<u16>, u16)> {
        let w: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(name).collect();
        let bytes = u16::try_from(w.len() * 2).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        Ok((w, bytes))
    }

    /// `name`, one name, opened in `dir`; the name itself is never followed, a link or junction
    /// opening as itself.
    pub(super) fn open(dir: &OwnedHandle, name: &OsStr, access: u32, disposition: u32, options: u32) -> std::io::Result<OwnedHandle> {
        let (mut w, bytes) = wide(name)?;
        let object = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: w.as_mut_ptr() };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: dir.as_raw_handle() as HANDLE,
            ObjectName: &object,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let (mut handle, mut io): (HANDLE, IO_STATUS_BLOCK) = (std::ptr::null_mut(), IO_STATUS_BLOCK::default());
        let options = options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT;
        // SAFETY: every pointer is to a local alive for the call (the name, its UNICODE_STRING,
        // the attributes, the status block), and a handle returned on success is new and ours alone
        unsafe {
            let status = NtCreateFile(
                &mut handle,
                access | SYNCHRONIZE,
                &attributes,
                &mut io,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                disposition,
                options,
                std::ptr::null(),
                0,
            );
            if status < 0 {
                return Err(std::io::Error::from_raw_os_error(RtlNtStatusToDosError(status) as i32));
            }
            Ok(OwnedHandle::from_raw_handle(handle as _))
        }
    }

    /// The file `from` holds renamed to `to`, one name in `dir`, over a file of that name.
    pub(super) fn rename(from: &OwnedHandle, dir: &OwnedHandle, to: &OsStr) -> std::io::Result<()> {
        let (w, bytes) = wide(to)?;
        let at = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
        let len = (at + usize::from(bytes)).max(size_of::<FILE_RENAME_INFORMATION>());
        // zeroed, and aligned for the struct (its widest field is a pointer)
        let mut buf = vec![0u64; len.div_ceil(8)];
        let mut io = IO_STATUS_BLOCK::default();
        // SAFETY: buf holds len bytes, at least the struct, 8-byte aligned; the name's bytes go
        // from FileName's offset and end within len; the call reads len bytes of buf
        unsafe {
            let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
            (*info).Anonymous.ReplaceIfExists = true;
            (*info).RootDirectory = dir.as_raw_handle() as HANDLE;
            (*info).FileNameLength = u32::from(bytes);
            std::ptr::copy_nonoverlapping(w.as_ptr(), buf.as_mut_ptr().cast::<u8>().add(at).cast::<u16>(), w.len());
            let status = NtSetInformationFile(from.as_raw_handle() as HANDLE, &mut io, buf.as_ptr().cast(), len as u32, FileRenameInformation);
            if status < 0 {
                return Err(std::io::Error::from_raw_os_error(RtlNtStatusToDosError(status) as i32));
            }
        }
        Ok(())
    }
}

/// `h` unless it is a link or junction (opened as itself).
#[cfg(windows)]
fn not_a_link(h: std::os::windows::io::OwnedHandle) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::MetadataExt;
    let f = std::fs::File::from(h);
    if f.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::other("a link or junction"));
    }
    Ok(f)
}

impl Dir {
    fn root(root: &Path) -> std::io::Result<Dir> {
        #[cfg(unix)]
        return Ok(Dir(rustix::fs::openat(rustix::fs::CWD, root, DIR_FLAGS, rustix::fs::Mode::empty())?));
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let f = std::fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(root)?;
            Ok(Dir(f.into()))
        }
        #[cfg(not(any(unix, windows)))]
        return Ok(Dir(root.to_path_buf()));
    }

    /// The directory `below` (names under this one) is, opened one name at a time.
    fn below(&self, below: &Path) -> std::io::Result<Dir> {
        #[cfg(unix)]
        {
            use rustix::fs::{openat, Mode, OFlags};
            let mut d = openat(&self.0, ".", DIR_FLAGS, Mode::empty())?;
            for c in below.components() {
                d = openat(&d, c.as_os_str(), DIR_FLAGS | OFlags::NOFOLLOW, Mode::empty())?;
            }
            Ok(Dir(d))
        }
        #[cfg(windows)]
        {
            let mut d = self.0.try_clone()?;
            for c in below.components() {
                d = not_a_link(nt::open(&d, c.as_os_str(), FILE_TRAVERSE | FILE_READ_ATTRIBUTES, FILE_OPEN, FILE_DIRECTORY_FILE)?)?.into();
            }
            Ok(Dir(d))
        }
        #[cfg(not(any(unix, windows)))]
        return Ok(Dir(self.0.join(below)));
    }

    /// The permissions of `name` when it is a regular file, not a link.
    fn regular(&self, name: &std::ffi::OsStr) -> std::io::Result<Option<std::fs::Permissions>> {
        #[cfg(unix)]
        {
            use rustix::fs::{statat, AtFlags, FileType};
            use std::os::unix::fs::PermissionsExt;
            let st = statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
            let file = FileType::from_raw_mode(st.st_mode as _) == FileType::RegularFile;
            Ok(file.then(|| std::fs::Permissions::from_mode(st.st_mode as u32 & 0o7777)))
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            let md = std::fs::File::from(nt::open(&self.0, name, FILE_READ_ATTRIBUTES, FILE_OPEN, 0)?).metadata()?;
            Ok((md.is_file() && md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0).then(|| md.permissions()))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let md = std::fs::symlink_metadata(self.0.join(name))?;
            Ok(md.file_type().is_file().then(|| md.permissions()))
        }
    }

    fn read(&self, name: &std::ffi::OsStr) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let mut out = Vec::new();
        #[cfg(unix)]
        {
            use rustix::fs::{openat, Mode, OFlags};
            std::fs::File::from(openat(&self.0, name, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?).read_to_end(&mut out)?;
        }
        #[cfg(windows)]
        not_a_link(nt::open(&self.0, name, FILE_GENERIC_READ, FILE_OPEN, FILE_NON_DIRECTORY_FILE)?)?.read_to_end(&mut out)?;
        #[cfg(not(any(unix, windows)))]
        std::fs::File::open(self.0.join(name))?.read_to_end(&mut out)?;
        Ok(out)
    }

    /// A new file, never one already there (so nothing planted under that name is opened).
    fn create_new(&self, name: &str) -> std::io::Result<std::fs::File> {
        #[cfg(unix)]
        {
            use rustix::fs::{openat, Mode, OFlags};
            let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            Ok(std::fs::File::from(openat(&self.0, name, flags, Mode::RUSR | Mode::WUSR)?))
        }
        #[cfg(windows)]
        return Ok(nt::open(&self.0, name.as_ref(), FILE_GENERIC_WRITE, FILE_CREATE, FILE_NON_DIRECTORY_FILE)?.into());
        #[cfg(not(any(unix, windows)))]
        return std::fs::OpenOptions::new().write(true).create_new(true).open(self.0.join(name));
    }

    fn rename(&self, from: &str, to: &std::ffi::OsStr) -> std::io::Result<()> {
        #[cfg(unix)]
        return Ok(rustix::fs::renameat(&self.0, from, &self.0, to)?);
        #[cfg(windows)]
        return nt::rename(&nt::open(&self.0, from.as_ref(), DELETE, FILE_OPEN, FILE_NON_DIRECTORY_FILE)?, &self.0, to);
        #[cfg(not(any(unix, windows)))]
        return std::fs::rename(self.0.join(from), self.0.join(to));
    }

    fn remove(&self, name: &str) {
        #[cfg(unix)]
        let _ = rustix::fs::unlinkat(&self.0, name, rustix::fs::AtFlags::empty());
        // a link named so is removed itself, never what it points to
        #[cfg(windows)]
        let _ = nt::open(&self.0, name.as_ref(), DELETE, FILE_OPEN, FILE_NON_DIRECTORY_FILE | FILE_DELETE_ON_CLOSE);
        #[cfg(not(any(unix, windows)))]
        let _ = std::fs::remove_file(self.0.join(name));
    }
}

/// The directory of `inside` (a path under the root) and its file name.
fn split(inside: &Path) -> (&Path, &std::ffi::OsStr) {
    (inside.parent().unwrap_or(Path::new("")), inside.file_name().unwrap_or_default())
}

/// The file is written through its directory as `Dir` opens it: the temporary file, the check
/// that the bytes are those read, and the rename all go by name in that directory.
fn write_one(root: &Dir, f: &FileChange) -> Result<(), String> {
    use std::io::Write;
    let (parent, name) = split(&f.inside);
    let dir = root.below(parent).map_err(|e| e.to_string())?;
    let perm = dir.regular(name).map_err(|e| e.to_string())?.ok_or("not a regular file")?;
    let tmp = format!(".{}.scrutate-{}.tmp", name.to_string_lossy(), std::process::id());
    let written = (|| -> std::io::Result<bool> {
        let mut out = dir.create_new(&tmp)?;
        out.write_all(&f.new)?;
        out.set_permissions(perm)?;
        out.sync_all()?;
        drop(out);
        if dir.read(name)? != f.read {
            return Ok(false);
        }
        dir.rename(&tmp, name)?;
        Ok(true)
    })();
    if !matches!(written, Ok(true)) {
        dir.remove(&tmp);
    }
    match written {
        Ok(true) => Ok(()),
        Ok(false) => Err("changed since it was read".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// `diff -u` of a file whose edits are known: `spans` are (old start, old end, new start, new
/// end) byte ranges in order, and the text between them is the same on both sides.
fn unified(path: &str, old: &str, new: &str, spans: &[(usize, usize, usize, usize)]) -> String {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let line = |t: &str, at: usize| t[..at].matches('\n').count();
    // per run of edits on the same or neighbouring lines: (old first line, old end line, new first line, new end line)
    let mut runs: Vec<(usize, usize, usize, usize)> = Vec::new();
    for &(os, oe, ns, ne) in spans {
        let r = (line(old, os), line(old, oe) + 1, line(new, ns), line(new, ne) + 1);
        match runs.last_mut() {
            Some(last) if r.0 <= last.1 => (last.1, last.3) = (r.1, r.3),
            _ => runs.push(r),
        }
    }
    // lines a run holds alike on both sides are context
    for r in &mut runs {
        (r.1, r.3) = (r.1.min(a.len()), r.3.min(b.len()));
        while r.0 < r.1 && r.2 < r.3 && a[r.0] == b[r.2] {
            (r.0, r.2) = (r.0 + 1, r.2 + 1);
        }
        while r.0 < r.1 && r.2 < r.3 && a[r.1 - 1] == b[r.3 - 1] {
            (r.1, r.3) = (r.1 - 1, r.3 - 1);
        }
    }
    let mut out = format!("--- {path}\n+++ {path}\n");
    let push = |out: &mut String, mark: char, l: &str| {
        out.push(mark);
        out.push_str(l);
        if !l.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    };
    let mut r = 0;
    while r < runs.len() {
        let mut last = r;
        while last + 1 < runs.len() && runs[last + 1].0 <= runs[last].1 + 6 {
            last += 1;
        }
        let o0 = runs[r].0.saturating_sub(3);
        let n0 = runs[r].2 - (runs[r].0 - o0);
        let o1 = (runs[last].1 + 3).min(a.len());
        let n1 = runs[last].3 + (o1 - runs[last].1);
        let start = |s: usize, n: usize| if n == 0 { s } else { s + 1 };
        out.push_str(&format!("@@ -{},{} +{},{} @@\n", start(o0, o1 - o0), o1 - o0, start(n0, n1 - n0), n1 - n0));
        let mut at = o0;
        for run in &runs[r..=last] {
            a[at..run.0].iter().for_each(|l| push(&mut out, ' ', l));
            a[run.0..run.1].iter().for_each(|l| push(&mut out, '-', l));
            b[run.2..run.3].iter().for_each(|l| push(&mut out, '+', l));
            at = run.1;
        }
        a[at..o1].iter().for_each(|l| push(&mut out, ' ', l));
        r = last + 1;
    }
    out
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// Every step of a write names its entry by handle with FILE_OPEN_REPARSE_POINT: each one
    /// meets a junction to a directory outside the root as itself, and never touches what it
    /// points to.
    #[test]
    fn each_operation_meets_a_junction_as_itself() {
        let base = std::env::temp_dir().join(format!("scrutate_dir_junction_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, outside) = (base.join("root"), base.join("outside"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("x.py"), "print(secret)\n").unwrap();
        std::fs::write(root.join("a.py"), "print(x)\n").unwrap();
        let out = std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(root.join("j")).arg(&outside).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        let dir = Dir::root(&root).unwrap();
        let j = std::ffi::OsStr::new("j");
        assert!(dir.below(Path::new("j")).is_err(), "below");
        assert!(dir.regular(j).unwrap().is_none(), "regular");
        assert!(dir.read(j).is_err(), "read");
        assert!(dir.create_new("j").is_err(), "create_new");
        assert!(dir.rename("j", std::ffi::OsStr::new("k")).is_err(), "rename from it");
        let mut tmp = dir.create_new("t.tmp").unwrap();
        std::io::Write::write_all(&mut tmp, b"log(x)\n").unwrap();
        drop(tmp);
        assert!(dir.rename("t.tmp", j).is_err(), "rename over it");
        dir.remove("j");
        assert!(root.join("j").exists(), "remove");
        // the junction's target, untouched by every step
        let there: Vec<_> = std::fs::read_dir(&outside).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(there, ["x.py"]);
        assert_eq!(std::fs::read_to_string(outside.join("x.py")).unwrap(), "print(secret)\n");
        // a plain file still goes through each step
        assert!(dir.regular(std::ffi::OsStr::new("a.py")).unwrap().is_some());
        dir.rename("t.tmp", std::ffi::OsStr::new("a.py")).unwrap();
        assert_eq!(dir.read(std::ffi::OsStr::new("a.py")).unwrap(), b"log(x)\n");
        std::fs::remove_dir(root.join("j")).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }
}
