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
    walk_upto(path, kind, MAX_MEMBERS, each, failed)
}

fn walk_upto(
    path: &Path,
    kind: Kind,
    max_members: usize,
    each: &mut dyn FnMut(Option<&str>, &mut dyn Read) -> bool,
    failed: &mut dyn FnMut(String),
) -> std::io::Result<()> {
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
        Kind::Tar => tar(Capped { inner: Box::new(File::open(path)?) as Box<dyn Read>, left: MAX_EXPANDED }, max_members, each, failed),
        Kind::TarGz => tar(
            Capped { inner: Box::new(flate2::read::MultiGzDecoder::new(File::open(path)?)) as Box<dyn Read>, left: MAX_EXPANDED },
            max_members,
            each,
            failed,
        ),
        Kind::TarOther => tar(Capped { inner: external(path)?, left: MAX_EXPANDED }, max_members, each, failed),
        Kind::Zip => {
            let mut z = zip::ZipArchive::new(File::open(path)?).map_err(std::io::Error::other)?;
            let mut left = MAX_EXPANDED;
            let mut stopped = false;
            for i in 0..z.len().min(max_members) {
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
                    stopped = true;
                    break;
                }
                left = r.left;
            }
            if !stopped && z.len() > max_members {
                return Err(too_many(max_members));
            }
            Ok(())
        }
    }
}

fn too_many(max_members: usize) -> std::io::Error {
    std::io::Error::other(format!("more than {max_members} members; stopped"))
}

/// The longest member name kept from a GNU long-name or PAX header; a zip name is at most 64 KiB
/// by its format.
const MAX_NAME: u64 = 64 << 10;

/// A member name from an extension header (None: it gives none); Err: longer than `MAX_NAME`.
type Name = Result<Option<Vec<u8>>, ()>;

/// The tar crate reads a long-name or PAX header whole, up to gigabytes, so they are read here.
fn tar<R: Read>(r: R, max_members: usize, each: &mut dyn FnMut(Option<&str>, &mut dyn Read) -> bool, failed: &mut dyn FnMut(String)) -> std::io::Result<()> {
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
    // only regular files are members: extension headers (PAX, GNU long names), directories, links
    // and other entries are bounded on their own, so a tar of nothing else still ends
    let max_headers = max_members.saturating_mul(4);
    let mut members = 0;
    for (n, entry) in a.entries()?.raw(true).enumerate() {
        if n >= max_headers {
            return Err(std::io::Error::other(format!("more than {max_headers} tar headers; stopped")));
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
        if members == max_members {
            return Err(too_many(max_members));
        }
        members += 1;
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

    /// A tar and a zip of three members, `foo` in the last.
    fn three_members(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let t = dir.join("three.tar");
        let mut b = tar::Builder::new(File::create(&t).unwrap());
        for (name, body) in [("a.txt", "x\n"), ("b.txt", "x\n"), ("c.txt", "foo\n")] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, body.as_bytes()).unwrap();
        }
        b.finish().unwrap();
        let z = dir.join("three.zip");
        let mut w = zip::ZipWriter::new(File::create(&z).unwrap());
        for (name, body) in [("a.txt", "x\n"), ("b.txt", "x\n"), ("c.txt", "foo\n")] {
            w.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
            std::io::Write::write_all(&mut w, body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
        (t, z)
    }

    #[test]
    fn an_archive_past_the_member_cap_ends_with_an_error() {
        let dir = std::env::temp_dir().join(format!("scrutate_archive_cap_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (t, z) = three_members(&dir);
        for (path, kind) in [(&t, Kind::Tar), (&z, Kind::Zip)] {
            let mut seen = Vec::new();
            let r = walk_upto(
                path,
                kind,
                2,
                &mut |m, _| {
                    seen.push(m.unwrap().to_string());
                    true
                },
                &mut |_| {},
            );
            let e = r.expect_err("a member past the cap is not read, and the walk says so");
            assert!(e.to_string().contains("more than 2 members"), "{kind:?}: {e}");
            assert_eq!(seen, ["a.txt", "b.txt"], "{kind:?}");
            // at the cap or under it, every member is read and nothing is said
            let mut n = 0;
            walk_upto(
                path,
                kind,
                3,
                &mut |_, _| {
                    n += 1;
                    true
                },
                &mut |_| {},
            )
            .unwrap();
            assert_eq!(n, 3, "{kind:?}");
            // a stop asked for by the search is no error
            assert!(walk_upto(path, kind, 2, &mut |_, _| false, &mut |_| {}).is_ok(), "{kind:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_member_cap_counts_members_not_their_extension_headers() {
        let dir = std::env::temp_dir().join(format!("scrutate_archive_pax_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("pax.tar");
        let mut b = tar::Builder::new(File::create(&t).unwrap());
        for (name, body) in [("a.txt", "x\n"), ("b.txt", "x\n"), ("c.txt", "foo\n")] {
            b.append_pax_extensions([("comment", &b"a PAX header of its own"[..])]).unwrap();
            let mut h = tar::Header::new_ustar();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, body.as_bytes()).unwrap();
        }
        b.finish().unwrap();
        // three members, each behind a PAX header: six entries, three members
        let mut seen = Vec::new();
        walk_upto(
            &t,
            Kind::Tar,
            3,
            &mut |m, _| {
                seen.push(m.unwrap().to_string());
                true
            },
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(seen, ["a.txt", "b.txt", "c.txt"]);
        let e = walk_upto(&t, Kind::Tar, 2, &mut |_, _| true, &mut |_| {}).unwrap_err();
        assert!(e.to_string().contains("more than 2 members"), "{e}");
        // nothing but extension headers still ends
        let h = dir.join("headers.tar");
        let mut b = tar::Builder::new(File::create(&h).unwrap());
        for _ in 0..10 {
            b.append_pax_extensions([("comment", &b"no member follows"[..])]).unwrap();
        }
        b.finish().unwrap();
        let e = walk_upto(&h, Kind::Tar, 2, &mut |_, _| true, &mut |_| {}).unwrap_err();
        assert!(e.to_string().contains("headers"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capped_reader_stops() {
        let data = [b'a'; 100];
        let mut r = Capped { inner: &data[..], left: 10 };
        let mut buf = Vec::new();
        assert!(r.read_to_end(&mut buf).is_err());
    }
}
