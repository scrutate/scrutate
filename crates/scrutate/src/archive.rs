//! What `grep -z` reads: the members of zip and tar archives, and the contents of compressed
//! files. gzip (alone or around a tar) is decompressed in process; bzip2, xz, zstd, lz4, brotli and
//! `.Z` by the system's own tools, as ripgrep's `-z` does.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Decompressed bytes allowed per archive or compressed file: a small zip bomb must not keep a
/// search busy for hours. Memory is bounded by `MAX_LINE`, except under `-U`, which holds a whole
/// member, as ripgrep does.
pub const MAX_EXPANDED: u64 = 8 << 30;
/// The longest line of an archive member or a compressed file that is searched.
pub const MAX_LINE: usize = 64 << 20;
/// Members searched per archive.
pub const MAX_MEMBERS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Zip,
    Tar,
    TarGz,
    /// a tar inside another compression format
    TarOther,
    Gz,
    /// one compressed file, decompressed by an external tool
    Other,
}

const OTHER: &[&str] = &[".bz2", ".tbz", ".tbz2", ".xz", ".txz", ".lzma", ".zst", ".zstd", ".tzst", ".lz4", ".br", ".z"];

impl Kind {
    /// an archive, not one compressed file
    pub fn has_members(self) -> bool {
        !matches!(self, Kind::Gz | Kind::Other)
    }

    pub fn of(path: &str) -> Option<Kind> {
        let l = path.to_ascii_lowercase();
        let tar_inside = |ext: &str| l.strip_suffix(ext).is_some_and(|s| s.ends_with(".tar"));
        Some(if l.ends_with(".zip") {
            Kind::Zip
        } else if l.ends_with(".tar") {
            Kind::Tar
        } else if l.ends_with(".tgz") || tar_inside(".gz") {
            Kind::TarGz
        } else if l.ends_with(".gz") {
            Kind::Gz
        } else if [".tbz", ".tbz2", ".txz", ".tzst"].iter().any(|e| l.ends_with(e)) || OTHER.iter().any(|e| tar_inside(e)) {
            Kind::TarOther
        } else if OTHER.iter().any(|e| l.ends_with(e)) && !(l.ends_with(".z") && !path.ends_with(".Z")) {
            Kind::Other
        } else {
            return None;
        })
    }
}

/// Counts what is read and fails past `MAX_EXPANDED`.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.left = self.left.checked_sub(n as u64).ok_or_else(|| std::io::Error::other(format!("expands beyond {} GiB; stopped", MAX_EXPANDED >> 30)))?;
        Ok(n)
    }
}

fn external(path: &Path) -> std::io::Result<Box<dyn Read>> {
    let r = grep_cli::DecompressionReaderBuilder::new().build(path).map_err(std::io::Error::other)?;
    Ok(Box::new(r))
}

/// Calls `each(member, reader)` for every regular file inside `path` (member None: the
/// decompressed file itself). `each` returns false to stop. Errors of one member are passed to
/// `failed` and the walk goes on; an error that ends the walk is returned.
pub fn walk(path: &Path, kind: Kind, each: &mut dyn FnMut(Option<&str>, &mut dyn Read) -> bool, failed: &mut dyn FnMut(String)) -> std::io::Result<()> {
    match kind {
        Kind::Gz => {
            let mut r = Capped { inner: flate2::read::MultiGzDecoder::new(File::open(path)?), left: MAX_EXPANDED };
            each(None, &mut r);
            Ok(())
        }
        Kind::Other => {
            let mut r = Capped { inner: external(path)?, left: MAX_EXPANDED };
            each(None, &mut r);
            Ok(())
        }
        Kind::Tar => tar(Capped { inner: Box::new(File::open(path)?) as Box<dyn Read>, left: MAX_EXPANDED }, each, failed),
        Kind::TarGz => tar(Capped { inner: Box::new(flate2::read::MultiGzDecoder::new(File::open(path)?)) as Box<dyn Read>, left: MAX_EXPANDED }, each, failed),
        Kind::TarOther => tar(Capped { inner: external(path)?, left: MAX_EXPANDED }, each, failed),
        Kind::Zip => {
            let mut z = zip::ZipArchive::new(File::open(path)?).map_err(std::io::Error::other)?;
            let mut left = MAX_EXPANDED;
            for i in 0..z.len().min(MAX_MEMBERS) {
                let mut e = match z.by_index(i) {
                    Ok(e) => e,
                    Err(err) => {
                        failed(format!("member {i}: {err}"));
                        continue;
                    }
                };
                if !e.is_file() {
                    continue;
                }
                let name = e.name().map(|n| n.into_owned()).unwrap_or_else(|_| format!("member {i}"));
                let mut r = Capped { inner: &mut e, left };
                if !each(Some(&name), &mut r) {
                    break;
                }
                left = r.left;
            }
            Ok(())
        }
    }
}

/// The longest member name kept from a GNU long-name or PAX header; a zip name is at most 64 KiB
/// by its format.
const MAX_NAME: u64 = 64 << 10;

/// A member name from an extension header (None: it gives none); Err: longer than `MAX_NAME`.
type Name = Result<Option<Vec<u8>>, ()>;

/// The tar crate reads a long-name or PAX header whole, up to gigabytes, so they are read here.
fn tar<R: Read>(r: R, each: &mut dyn FnMut(Option<&str>, &mut dyn Read) -> bool, failed: &mut dyn FnMut(String)) -> std::io::Result<()> {
    struct Shared<R>(std::rc::Rc<std::cell::RefCell<R>>);
    impl<R: Read> Read for Shared<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().read(buf)
        }
    }
    let r = std::rc::Rc::new(std::cell::RefCell::new(r));
    let mut a = tar::Archive::new(Shared(r.clone()));
    // the name and size the extension headers give the next member
    let mut name: Name = Ok(None);
    let mut size = None;
    for (n, entry) in a.entries()?.raw(true).enumerate() {
        if n >= MAX_MEMBERS {
            break;
        }
        let mut e = entry?;
        let t = e.header().entry_type();
        // the extension blocks of an old-GNU sparse member sit before its data and are not in its
        // size, so they are read past here where the raw walk does not count them
        if t.is_gnu_sparse() && e.header().as_gnu().is_some_and(|g| g.is_extended()) {
            let mut ext = tar::GnuExtSparseHeader::new();
            ext.set_is_extended(true);
            while ext.is_extended() {
                r.borrow_mut().read_exact(ext.as_mut_bytes())?;
            }
        }
        let known = e.header().as_gnu().is_some() || e.header().as_ustar().is_some();
        if known && t.is_gnu_longname() {
            let mut v = Vec::new();
            (&mut e).take(MAX_NAME + 1).read_to_end(&mut v)?;
            while v.last() == Some(&0) {
                v.pop();
            }
            name = if v.len() as u64 > MAX_NAME { Err(()) } else { Ok(Some(v)) };
            continue;
        }
        if known && t.is_pax_local_extensions() {
            let (path, s) = pax(&mut e)?;
            match path {
                Ok(Some(p)) => name = Ok(Some(p)),
                Err(()) => name = Err(()),
                Ok(None) => {}
            }
            size = size.or(s);
            continue;
        }
        if t.is_gnu_longlink() || t.is_pax_global_extensions() {
            continue;
        }
        let (name, size) = (std::mem::replace(&mut name, Ok(None)), size.take());
        if size.is_some_and(|s| s != e.size()) {
            return Err(std::io::Error::other("a member's size in its PAX header differs from its header; stopped"));
        }
        if !t.is_file() {
            continue;
        }
        let Ok(name) = name else {
            failed(format!("a member name longer than {} KiB; skipped", MAX_NAME >> 10));
            continue;
        };
        let name = match name {
            Some(v) => String::from_utf8_lossy(&v).into_owned(),
            None => String::from_utf8_lossy(&e.header().path_bytes()).into_owned(),
        };
        if !each(Some(&name), &mut e) {
            break;
        }
    }
    Ok(())
}

/// The `path` and `size` records of a PAX header, read record by
/// record so that other records, such as extended attributes, are skipped and not held.
fn pax(r: &mut dyn Read) -> std::io::Result<(Name, Option<u64>)> {
    fn byte(r: &mut dyn Read) -> std::io::Result<Option<u8>> {
        let mut b = [0];
        Ok((r.read(&mut b)? == 1).then_some(b[0]))
    }
    let (mut path, mut size) = (Ok(None), None);
    loop {
        // "<length> <key>=<value>\n", the length counting the whole record
        let (mut len, mut digits) = (0u64, 0u64);
        loop {
            match byte(r)? {
                Some(b @ b'0'..=b'9') if digits < 19 => {
                    len = len * 10 + u64::from(b - b'0');
                    digits += 1;
                }
                Some(b' ') if digits > 0 => break,
                _ => return Ok((path, size)),
            }
        }
        let mut left = len.saturating_sub(digits + 1);
        let mut key = Vec::new();
        while left > 0 {
            left -= 1;
            match byte(r)? {
                Some(b'=') => break,
                Some(b) if key.len() < 64 => key.push(b),
                Some(_) => {}
                None => return Ok((path, size)),
            }
        }
        // the value, then its newline
        let value = left.saturating_sub(1);
        if key == b"path" && value > MAX_NAME {
            path = Err(());
        } else if key == b"path" || (key == b"size" && value <= 20) {
            let mut v = Vec::new();
            (&mut *r).take(value).read_to_end(&mut v)?;
            left -= v.len() as u64;
            if key == b"path" {
                path = Ok(Some(v));
            } else {
                size = std::str::from_utf8(&v).ok().and_then(|s| s.parse().ok());
            }
        }
        std::io::copy(&mut (&mut *r).take(left), &mut std::io::sink())?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_name() {
        for (name, kind) in [
            ("a.zip", Some(Kind::Zip)),
            ("a.tar", Some(Kind::Tar)),
            ("a.tar.gz", Some(Kind::TarGz)),
            ("A.TGZ", Some(Kind::TarGz)),
            ("a.gz", Some(Kind::Gz)),
            ("a.tar.xz", Some(Kind::TarOther)),
            ("a.tbz2", Some(Kind::TarOther)),
            ("a.log.zst", Some(Kind::Other)),
            ("a.Z", Some(Kind::Other)),
            ("a.z", None),
            ("a.txt", None),
            ("zip", None),
        ] {
            assert_eq!(Kind::of(name), kind, "{name}");
        }
    }

    #[test]
    fn capped_reader_stops() {
        let data = [b'a'; 100];
        let mut r = Capped { inner: &data[..], left: 10 };
        let mut buf = Vec::new();
        assert!(r.read_to_end(&mut buf).is_err());
    }
}
