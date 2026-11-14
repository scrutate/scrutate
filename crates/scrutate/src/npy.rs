//! Minimal reader/writer for NumPy `.npy` files (memory-mapped or in memory) and `.npz` archives.
//!
//! The segments and the model files are in numpy's own layout, so the training scripts read and
//! write them as they are. Only the dtypes the
//! engine uses are supported: i64, i32, u64, u32, u8, i8, f32, f64, f16, bool and fixed-width unicode strings.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use half::f16;
use memmap2::Mmap;

const MAGIC: &[u8] = b"\x93NUMPY";
const ALIGN: usize = 64;

#[cfg(not(target_endian = "little"))]
compile_error!("the on-disk format is little-endian; big-endian targets are not supported");

pub(crate) enum Backing {
    Mmap(Mmap),
    Owned(Vec<u8>),
}

impl std::ops::Deref for Backing {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Backing::Mmap(m) => m,
            Backing::Owned(v) => v,
        }
    }
}

/// `file` mapped, or read into memory where the system cannot map files (wasi).
pub(crate) fn map(file: &File, path: &Path) -> Result<Backing> {
    match unsafe { Mmap::map(file) } {
        Ok(m) => Ok(Backing::Mmap(m)),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
            let mut v = Vec::new();
            (&*file).read_to_end(&mut v).with_context(|| format!("read {}", path.display()))?;
            Ok(Backing::Owned(v))
        }
        Err(e) => Err(e).with_context(|| format!("mmap {}", path.display())),
    }
}

pub struct Npy {
    pub descr: String,
    pub shape: Vec<usize>,
    pub fortran: bool, // column-major data (only accepted for in-memory arrays; see to_vec)
    offset: usize,
    backing: Backing,
}

/// Plain-old-data element types that can be viewed directly in the file bytes.
///
/// # Safety
/// Implementors must be `repr(C)`/primitive types for which every bit pattern of `size_of::<Self>()`
/// bytes is a valid value, and `DESCR` must list exactly the numpy dtype strings of that layout.
pub unsafe trait Pod: Copy + 'static {
    const DESCR: &'static [&'static str];
}
unsafe impl Pod for i64 {
    const DESCR: &'static [&'static str] = &["<i8"];
}
unsafe impl Pod for i32 {
    const DESCR: &'static [&'static str] = &["<i4"];
}
unsafe impl Pod for u32 {
    const DESCR: &'static [&'static str] = &["<u4"];
}
unsafe impl Pod for u64 {
    const DESCR: &'static [&'static str] = &["<u8"];
}
unsafe impl Pod for u8 {
    const DESCR: &'static [&'static str] = &["|u1"];
}
unsafe impl Pod for i8 {
    const DESCR: &'static [&'static str] = &["|i1"];
}
unsafe impl Pod for f32 {
    const DESCR: &'static [&'static str] = &["<f4"];
}
unsafe impl Pod for f64 {
    const DESCR: &'static [&'static str] = &["<f8"];
}
unsafe impl Pod for f16 {
    const DESCR: &'static [&'static str] = &["<f2"];
}

fn parse_header(bytes: &[u8]) -> Result<(String, Vec<usize>, bool, usize)> {
    if bytes.len() < 10 || &bytes[..6] != MAGIC {
        bail!("not an .npy file");
    }
    let (major, minor) = (bytes[6], bytes[7]);
    let (hlen, start) = match major {
        1 => (u16::from_le_bytes([bytes[8], bytes[9]]) as usize, 10usize),
        2 | 3 => {
            if bytes.len() < 12 {
                bail!("truncated .npy header");
            }
            (u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize, 12usize)
        }
        _ => bail!("unsupported .npy version {major}.{minor}"),
    };
    let end = start + hlen;
    if bytes.len() < end {
        bail!("truncated .npy header");
    }
    let header = std::str::from_utf8(&bytes[start..end]).context("header is not utf8")?;
    let descr = extract(header, "'descr'").ok_or_else(|| anyhow!("no descr in header"))?;
    let descr = descr.trim().trim_matches(|c| c == '\'' || c == '"').to_string();
    let fortran = extract(header, "'fortran_order'").map(|v| v.trim() == "True").unwrap_or(false);
    let shape_s = extract(header, "'shape'").ok_or_else(|| anyhow!("no shape in header"))?;
    let shape_s = shape_s.trim().trim_start_matches('(').trim_end_matches(')');
    let shape: Vec<usize> = shape_s
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<usize>().map_err(|e| anyhow!("bad shape {s}: {e}")))
        .collect::<Result<_>>()?;
    Ok((descr, shape, fortran, end))
}

/// Value text after `key:` up to the next top-level comma (shapes contain their own commas).
fn extract(header: &str, key: &str) -> Option<String> {
    let i = header.find(key)? + key.len();
    let rest = &header[i..];
    let rest = &rest[rest.find(':')? + 1..];
    let mut depth = 0i32;
    let mut out = String::new();
    for c in rest.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                out.push(c);
            }
            ')' | ']' => {
                depth -= 1;
                out.push(c);
            }
            ',' | '}' if depth <= 0 => break,
            _ => out.push(c),
        }
    }
    Some(out)
}

impl Npy {
    pub fn open(path: &Path) -> Result<Npy> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let backing = map(&file, path)?;
        let (descr, shape, fortran, offset) = parse_header(&backing)?;
        if fortran {
            bail!("fortran-ordered arrays cannot be memory-mapped: {}", path.display());
        }
        Ok(Npy { descr, shape, fortran, offset, backing })
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Npy> {
        let (descr, shape, fortran, offset) = parse_header(&bytes)?;
        if fortran && shape.len() > 2 {
            bail!("fortran-ordered arrays with more than two dimensions are not supported");
        }
        Ok(Npy { descr, shape, fortran, offset, backing: Backing::Owned(bytes) })
    }

    /// Number of elements; a shape whose product overflows (a damaged header) saturates, so every
    /// size check against the data fails instead of wrapping.
    pub fn len(&self) -> usize {
        self.shape.iter().try_fold(1usize, |a, &d| a.checked_mul(d)).unwrap_or(usize::MAX)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn bytes(&self) -> &[u8] {
        let all = match &self.backing {
            Backing::Mmap(m) => &m[..],
            Backing::Owned(v) => &v[..],
        };
        &all[self.offset..]
    }

    /// Zero-copy typed view (file data is 64-byte aligned, so this is always sound for mmaps).
    pub fn view<T: Pod>(&self) -> Result<&[T]> {
        if !T::DESCR.contains(&self.descr.as_str()) {
            bail!("dtype {} does not match requested type", self.descr);
        }
        if self.fortran {
            bail!("fortran-ordered array cannot be viewed in place");
        }
        let n = self.len();
        let bytes = self.bytes();
        let need = n.saturating_mul(std::mem::size_of::<T>());
        if bytes.len() < need {
            bail!("array data truncated");
        }
        let ptr = bytes.as_ptr();
        if !(ptr as usize).is_multiple_of(std::mem::align_of::<T>()) {
            bail!("unaligned array data");
        }
        Ok(unsafe { std::slice::from_raw_parts(ptr as *const T, n) })
    }

    /// Owned copy in row-major order (works for unaligned in-memory buffers and Fortran-ordered 2-D arrays).
    pub fn to_vec<T: Pod>(&self) -> Result<Vec<T>> {
        if !T::DESCR.contains(&self.descr.as_str()) {
            bail!("dtype {} does not match requested type", self.descr);
        }
        let n = self.len();
        let size = std::mem::size_of::<T>();
        let bytes = self.bytes();
        if bytes.len() < n.saturating_mul(size) {
            bail!("array data truncated");
        }
        let mut out: Vec<T> = Vec::with_capacity(n);
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr() as *mut u8, n * size);
            out.set_len(n);
        }
        if self.fortran && self.shape.len() == 2 {
            let (rows, cols) = (self.shape[0], self.shape[1]);
            let mut t: Vec<T> = Vec::with_capacity(n);
            for r in 0..rows {
                for c in 0..cols {
                    t.push(out[c * rows + r]); // column-major (r, c) lives at c * rows + r
                }
            }
            return Ok(t);
        }
        Ok(out)
    }

    /// Fixed-width little-endian UCS-4 strings (`<U{n}`), zero padding stripped.
    pub fn to_strings(&self) -> Result<Vec<String>> {
        let width: usize = self.descr.strip_prefix("<U").and_then(|w| w.parse().ok()).ok_or_else(|| anyhow!("not a unicode array: {}", self.descr))?;
        let n = self.len();
        let bytes = self.bytes();
        if n.checked_mul(width).and_then(|x| x.checked_mul(4)).is_none_or(|need| bytes.len() < need) {
            bail!("string array data truncated");
        }
        if width == 0 && n > 0 {
            // zero-width strings take no bytes, so the shape alone would size the allocation
            bail!("zero-width string array");
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let mut s = String::new();
            for j in 0..width {
                let p = (i * width + j) * 4;
                let cp = u32::from_le_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]);
                if cp == 0 {
                    break;
                }
                s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
            }
            out.push(s);
        }
        Ok(out)
    }
}

fn header_bytes(descr: &str, shape: &[usize]) -> Vec<u8> {
    let shape_s = match shape.len() {
        0 => "()".to_string(),
        1 => format!("({},)", shape[0]),
        _ => format!("({})", shape.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")),
    };
    let mut dict = format!("{{'descr': '{descr}', 'fortran_order': False, 'shape': {shape_s}, }}");
    let base = 10 + dict.len() + 1;
    let pad = (ALIGN - base % ALIGN) % ALIGN;
    dict.push_str(&" ".repeat(pad));
    dict.push('\n');
    let mut out = Vec::with_capacity(10 + dict.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[1, 0]);
    out.extend_from_slice(&(dict.len() as u16).to_le_bytes());
    out.extend_from_slice(dict.as_bytes());
    out
}

pub fn save<T: Pod>(path: &Path, data: &[T], shape: &[usize]) -> Result<()> {
    debug_assert_eq!(shape.iter().product::<usize>(), data.len());
    let mut f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(&header_bytes(T::DESCR[0], shape))?;
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, std::mem::size_of_val(data)) };
    f.write_all(bytes)?;
    // on disk before the database names the segment, which a checkpoint makes durable
    f.sync_all()?;
    Ok(())
}

/// Arrays of an `.npz` archive, by entry name (without the `.npy`).
pub fn load_npz(path: &Path) -> Result<Vec<(String, Npy)>> {
    let bytes = std::fs::read(path).with_context(|| format!("open {}", path.display()))?;
    load_npz_bytes(&bytes).with_context(|| format!("read zip {}", path.display()))
}

pub fn load_npz_bytes(bytes: &[u8]) -> Result<Vec<(String, Npy)>> {
    let mut out = Vec::new();
    for (name, data) in zip_entries(bytes)? {
        out.push((name.trim_end_matches(".npy").to_string(), Npy::from_bytes(data)?));
    }
    Ok(out)
}

/// Minimal ZIP reader for `.npz` archives (what `numpy.savez` / `savez_compressed` write): entries
/// are read from the central directory and are either stored or deflated. Nothing is ever written
/// to disk, no file names are interpreted as paths, and ZIP64 or encrypted archives are rejected.
fn zip_entries(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let u16_at = |i: usize| -> Result<usize> { bytes.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or_else(|| anyhow!("truncated zip")) };
    let u32_at = |i: usize| -> Result<usize> {
        bytes.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or_else(|| anyhow!("truncated zip"))
    };
    // end of central directory record: signature 0x06054b50, searched backwards over the comment
    let min_eocd = 22;
    if bytes.len() < min_eocd {
        bail!("not a zip archive");
    }
    let mut eocd = None;
    let lo = bytes.len().saturating_sub(min_eocd + 65_535);
    let mut i = bytes.len() - min_eocd;
    loop {
        if &bytes[i..i + 4] == b"PK\x05\x06" {
            eocd = Some(i);
            break;
        }
        if i == lo {
            break;
        }
        i -= 1;
    }
    let eocd = eocd.ok_or_else(|| anyhow!("zip: no end-of-central-directory record"))?;
    let entries = u16_at(eocd + 10)?;
    let cd_size = u32_at(eocd + 12)?;
    let cd_off = u32_at(eocd + 16)?;
    if entries == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF {
        bail!("zip64 archives are not supported");
    }
    let mut out = Vec::with_capacity(entries);
    let mut p = cd_off;
    for _ in 0..entries {
        if u32_at(p)? != 0x0201_4b50 {
            bail!("zip: bad central directory entry");
        }
        let flags = u16_at(p + 8)?;
        let method = u16_at(p + 10)?;
        let csize = u32_at(p + 20)?;
        let usize_ = u32_at(p + 24)?;
        let name_len = u16_at(p + 28)?;
        let extra_len = u16_at(p + 30)?;
        let comment_len = u16_at(p + 32)?;
        let local = u32_at(p + 42)?;
        let name = String::from_utf8_lossy(bytes.get(p + 46..p + 46 + name_len).ok_or_else(|| anyhow!("truncated zip"))?).into_owned();
        if flags & 0x1 != 0 {
            bail!("zip: encrypted entry {name}");
        }
        // local header: signature, then the name and extra field lengths at 26 and 28
        if u32_at(local)? != 0x0403_4b50 {
            bail!("zip: bad local header for {name}");
        }
        let lname = u16_at(local + 26)?;
        let lextra = u16_at(local + 28)?;
        let start = local + 30 + lname + lextra;
        let comp = bytes.get(start..start + csize).ok_or_else(|| anyhow!("zip: truncated data for {name}"))?;
        let data = match method {
            0 => comp.to_vec(),
            8 => {
                let mut d = Vec::with_capacity(usize_);
                flate2::read::DeflateDecoder::new(comp).read_to_end(&mut d).with_context(|| format!("zip: inflate {name}"))?;
                d
            }
            m => bail!("zip: unsupported compression method {m} for {name}"),
        };
        if data.len() != usize_ {
            bail!("zip: size mismatch for {name}");
        }
        out.push((name, data));
        p += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_matches_numpy_layout() {
        let dir = std::env::temp_dir().join(format!("npy_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.npy");
        let data: Vec<i64> = (0..10).collect();
        save(&p, &data, &[10]).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[..6], MAGIC);
        assert_eq!(bytes.len() % 64, 80 % 64); // 64-byte header + 80 data bytes
        let n = Npy::open(&p).unwrap();
        assert_eq!(n.shape, vec![10]);
        assert_eq!(n.view::<i64>().unwrap(), &data[..]);
        let p2 = dir.join("b.npy");
        let f: Vec<f16> = vec![f16::from_f32(1.5); 6];
        save(&p2, &f, &[3, 2]).unwrap();
        let m = Npy::open(&p2).unwrap();
        assert_eq!(m.shape, vec![3, 2]);
        assert_eq!(m.view::<f16>().unwrap()[5].to_f32(), 1.5);
        let p3 = dir.join("e.npy");
        save::<i32>(&p3, &[], &[0]).unwrap();
        assert_eq!(Npy::open(&p3).unwrap().view::<i32>().unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;

    #[test]
    fn parses_numpy_headers() {
        let h = "{'descr': '<f2', 'fortran_order': False, 'shape': (11127, 256), }      \n";
        assert_eq!(extract(h, "'descr'").unwrap().trim(), "'<f2'");
        assert_eq!(extract(h, "'fortran_order'").unwrap().trim(), "False");
        assert_eq!(extract(h, "'shape'").unwrap().trim(), "(11127, 256)");
        let h2 = "{'descr': '<U102', 'fortran_order': False, 'shape': (), }\n";
        assert_eq!(extract(h2, "'shape'").unwrap().trim(), "()");
    }

    #[test]
    fn damaged_headers_are_errors_not_panics() {
        // found by fuzzing: a string array shorter than its shape claims
        let mut bytes = header_bytes("<U13", &[47]);
        bytes.extend_from_slice(&[0u8; 88]);
        assert!(Npy::from_bytes(bytes).unwrap().to_strings().is_err());
        // a shape whose element count overflows must not wrap into a small one
        let mut bytes = header_bytes("<i8", &[1 << 62, 8]);
        bytes.extend_from_slice(&[0u8; 64]);
        let a = Npy::from_bytes(bytes).unwrap();
        assert_eq!(a.len(), usize::MAX);
        assert!(a.view::<i64>().is_err() && a.to_vec::<i64>().is_err());
        // zero-width strings with a huge shape: an error, not an allocation of the shape's size
        let mut bytes = header_bytes("<U0", &[1 << 40]);
        bytes.extend_from_slice(&[0u8; 8]);
        assert!(Npy::from_bytes(bytes).unwrap().to_strings().is_err());
    }
}
