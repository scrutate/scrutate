//! Segment store: memory-mapped BM25 postings + semantic vectors (Lucene-style).
//!
//! An index is a list of immutable *segments* (`<index>/seg/sNNNNNN/`). Every update that adds chunks
//! writes one new segment; deletions are not written anywhere (a chunk is alive iff its id is still
//! in the SQLite `chunks` table) and small segments are merged by compaction. Lexical scoring is
//! Okapi BM25 with field weights multiplying the term frequency, document length = tokens over all
//! fields, idf = ln((N - df + 0.5) / (df + 0.5)) floored at 1e-6, all computed over the live chunks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use rayon::prelude::*;

use crate::npy::{self, Npy};

pub const FIELDS: [&str; 6] = ["name", "qual", "sig", "doc", "body", "path"];
pub const NF: usize = 6;
pub const K1: f32 = 1.2;
pub const B: f32 = 0.75;
pub const WEIGHTS: [f32; NF] = [10.0, 3.0, 3.0, 5.0, 1.0, 2.0];
pub const IDF_FLOOR: f32 = 1e-6;

/// Vector scan. Segments with at least IVF_MIN_ROWS chunks get an inverted-file index: the vectors are
/// clustered (spherical k-means) and stored in cluster order; a query scores the centroids and scans
/// only the IVF_NPROBE closest clusters. Smaller segments are scanned exhaustively.
pub const IVF_MIN_ROWS: usize = 20_000;
pub const IVF_NPROBE: usize = 48;
pub const IVF_SAMPLE: usize = 25_000;
pub const IVF_ITERS: usize = 10;

/// Stable 63-bit key of a token (no vocabulary file: segments merge without remapping).
pub fn tok_key(tok: &str) -> i64 {
    let mut h = Blake2bVar::new(8).expect("blake2b-64");
    h.update(tok.as_bytes());
    let mut out = [0u8; 8];
    h.finalize_variable(&mut out).expect("blake2b finalize");
    (u64::from_le_bytes(out) >> 1) as i64
}

/// Lexical + vector data of the chunks of one file.
pub struct FileLex {
    pub dl: Vec<f32>,
    pub keys: Vec<Vec<i64>>, // per field: term keys, chunk after chunk
    pub tf: Vec<Vec<u8>>,    // per field: term frequencies
    pub cnt: Vec<Vec<i32>>,  // per field: distinct terms per chunk
    pub vec: Option<QVecs>,
}

/// Semantic vectors as stored: int8 codes [m, 2, k] row-major and one scale per vector [m, 2].
#[derive(Default)]
pub struct QVecs {
    pub q: Vec<i8>,
    pub scale: Vec<f32>,
}

/// Symmetric int8 quantization of one vector (value = q * scale), written into `q`; returns the
/// scale. Float32 steps, ties rounded to even.
pub fn quantize(v: &[f32], q: &mut [i8]) -> f32 {
    let s = v.iter().fold(0f32, |m, x| m.max(x.abs())) / 127.0;
    let d = if s > 0.0 { s } else { 1.0 };
    for (o, &x) in q.iter_mut().zip(v) {
        *o = (x / d).round_ties_even().clamp(-127.0, 127.0) as i8;
    }
    s
}

pub fn dequantize(q: &[i8], s: f32, out: &mut [f32]) {
    for (o, &x) in out.iter_mut().zip(q) {
        *o = x as f32 * s;
    }
}

/// A stored value points outside its segment: the caller rebuilds the index, as for a corrupt database.
fn damaged() -> anyhow::Error {
    crate::index::Damaged("a segment array names a row outside the segment").into()
}

// --------------------------------------------------------------------------- segments
pub struct Segment {
    pub path: PathBuf,
    pub ids: Npy,
    pub dl: Npy,
    pub keys: Npy,
    pub df: Npy,
    pub ptr: Vec<Npy>,
    pub doc: Vec<Npy>,
    pub tf: Vec<Npy>,
    pub vec: Option<Npy>,    // int8 [n, 2, k]
    pub vscale: Option<Npy>, // float32 [n, 2]
    pub n: usize,
    pub k: usize,
    pub ivf: Option<Ivf>,
}

pub struct Ivf {
    pub cent: Vec<f32>, // [K, k]
    pub ptr: Vec<i64>,  // [K + 1]
    pub order: Npy,     // i32 [n]: segment row at each sorted position
    pub inv: Npy,       // i32 [n]: sorted position of each segment row
    pub nclusters: usize,
}

impl Segment {
    pub fn open(path: &Path) -> Result<Segment> {
        let ld = |name: &str| Npy::open(&path.join(format!("{name}.npy")));
        let ids = ld("ids")?;
        let n = ids.len();
        let vp = path.join("vec.npy");
        let vec = if vp.exists() { Some(Npy::open(&vp)?) } else { None };
        let vscale = if vec.is_some() { Some(ld("vscale")?) } else { None };
        let k = vec.as_ref().map(|v| if v.shape.len() == 3 { v.shape[2] } else { 0 }).unwrap_or(0);
        let ivf = if path.join("ivf_order.npy").exists() {
            let cent = ld("ivf_cent")?;
            let ptr = ld("ivf_ptr")?;
            let nclusters = ptr.len() - 1;
            Some(Ivf { cent: cent.to_vec::<f32>()?, ptr: ptr.to_vec::<i64>()?, order: ld("ivf_order")?, inv: ld("ivf_inv")?, nclusters })
        } else {
            None
        };
        let seg = Segment {
            path: path.to_path_buf(),
            ids,
            dl: ld("dl")?,
            keys: ld("keys")?,
            df: ld("df")?,
            ptr: (0..NF).map(|c| ld(&format!("ptr{c}"))).collect::<Result<_>>()?,
            doc: (0..NF).map(|c| ld(&format!("doc{c}"))).collect::<Result<_>>()?,
            tf: (0..NF).map(|c| ld(&format!("tf{c}"))).collect::<Result<_>>()?,
            vec,
            vscale,
            n,
            k,
            ivf,
        };
        seg.validate().with_context(|| format!("segment {} is corrupt", path.display()))?;
        Ok(seg)
    }

    /// Dtype, length and shape of every array (file-size level checks: no data is read), so the
    /// accessors below, which unwrap, cannot fail on a truncated or mistyped file. Damage inside the
    /// data (a wrong row number, say) is caught by `validate_deep`, or by bounds checks at use.
    fn validate(&self) -> Result<()> {
        let n = self.n;
        self.ids.view::<i64>()?;
        if self.dl.view::<f32>()?.len() != n {
            bail!("dl length");
        }
        let nv = self.keys.view::<i64>()?.len();
        if self.df.view::<i32>()?.len() != nv {
            bail!("df length");
        }
        for c in 0..NF {
            let ptr = self.ptr[c].view::<i64>()?;
            let doc = self.doc[c].view::<i32>()?;
            let tf = self.tf[c].view::<u8>()?;
            if ptr.len() != nv + 1 || ptr.first().copied().unwrap_or(1) != 0 {
                bail!("field {c}: postings pointer table");
            }
            let e = *ptr.last().unwrap_or(&0);
            if e < 0 || doc.len() != e as usize || tf.len() != e as usize {
                bail!("field {c}: postings length");
            }
        }
        if let Some(v) = &self.vec {
            if v.shape.len() != 3 || v.shape[0] != n || v.shape[1] != 2 || v.shape[2] != self.k {
                bail!("vector shape {:?}", v.shape);
            }
            v.view::<i8>()?;
            let sc = self.vscale.as_ref().ok_or_else(|| anyhow::anyhow!("vector scales missing"))?;
            if sc.view::<f32>()?.len() != 2 * n {
                bail!("vector scale length");
            }
        }
        if let Some(ivf) = &self.ivf {
            let order = ivf.order.view::<i32>()?;
            let inv = ivf.inv.view::<i32>()?;
            if order.len() != n || inv.len() != n || ivf.ptr.len() != ivf.nclusters + 1 || ivf.cent.len() != ivf.nclusters * self.k {
                bail!("ivf table lengths");
            }
            if ivf.ptr.last().copied().unwrap_or(-1) != n as i64 || ivf.ptr[0] < 0 || ivf.ptr.windows(2).any(|w| w[0] > w[1]) {
                bail!("ivf cluster pointers");
            }
        }
        Ok(())
    }

    /// Full content check (reads every array): ids sorted, postings rows and pointers in range,
    /// IVF permutation a bijection. Used by `scrutate check` and before trusting a recovered index.
    pub fn validate_deep(&self) -> Result<()> {
        let n = self.n;
        let ids = self.ids();
        if ids.windows(2).any(|w| w[0] >= w[1]) {
            bail!("ids not strictly increasing");
        }
        for c in 0..NF {
            let ptr = self.ptr(c);
            if ptr.windows(2).any(|w| w[0] > w[1]) {
                bail!("field {c}: pointer table not monotonic");
            }
            if self.doc(c).iter().any(|&d| d < 0 || d as usize >= n) {
                bail!("field {c}: posting row out of range");
            }
        }
        if let Some(ivf) = &self.ivf {
            if ivf.ptr.windows(2).any(|w| w[0] > w[1]) {
                bail!("ivf cluster pointers not monotonic");
            }
            let order = ivf.order.view::<i32>()?;
            let inv = ivf.inv.view::<i32>()?;
            let mut seen = vec![false; n];
            for &r in order {
                if r < 0 || r as usize >= n || seen[r as usize] {
                    bail!("ivf order is not a permutation");
                }
                seen[r as usize] = true;
            }
            if (0..n).any(|r| inv[r] < 0 || inv[r] as usize >= n || order[inv[r] as usize] as usize != r) {
                bail!("ivf inverse permutation mismatch");
            }
        }
        Ok(())
    }

    pub fn ids(&self) -> &[i64] {
        self.ids.view::<i64>().expect("ids dtype")
    }
    pub fn dl(&self) -> &[f32] {
        self.dl.view::<f32>().expect("dl dtype")
    }
    pub fn keys(&self) -> &[i64] {
        self.keys.view::<i64>().expect("keys dtype")
    }
    pub fn df(&self) -> &[i32] {
        self.df.view::<i32>().expect("df dtype")
    }
    pub fn ptr(&self, c: usize) -> &[i64] {
        self.ptr[c].view::<i64>().expect("ptr dtype")
    }
    pub fn doc(&self, c: usize) -> &[i32] {
        self.doc[c].view::<i32>().expect("doc dtype")
    }
    pub fn tf(&self, c: usize) -> &[u8] {
        self.tf[c].view::<u8>().expect("tf dtype")
    }
    /// All vectors as stored: int8 codes [n, 2, k] and scales [n, 2], in *cluster order* when the
    /// segment has an inverted-file index (one probe reads one contiguous block), else in row order.
    pub fn vec(&self) -> Option<(&[i8], &[f32])> {
        let q = self.vec.as_ref()?.view::<i8>().expect("vec dtype");
        let s = self.vscale.as_ref()?.view::<f32>().expect("vscale dtype");
        Some((q, s))
    }

    /// The postings of term `j` in field `c`: segment rows and their term counts.
    fn postings(&self, c: usize, j: usize) -> Result<(&[i32], &[u8])> {
        let ptr = self.ptr(c);
        let (a, b) = (ptr[j] as usize, ptr[j + 1] as usize);
        self.doc(c).get(a..b).zip(self.tf(c).get(a..b)).ok_or_else(damaged)
    }

    /// The stored (A view, B view) vector of segment row `r`: `2 * k` codes and 2 scales. None
    /// when the segment has no vectors, or its row table names a place outside them.
    pub fn vec_row(&self, r: usize) -> Option<(&[i8], &[f32])> {
        let (q, s) = self.vec()?;
        let k = self.k;
        let pos = match &self.ivf {
            Some(ivf) => *ivf.inv.view::<i32>().expect("inv dtype").get(r)? as usize,
            None => r,
        };
        if pos >= self.n {
            return None;
        }
        Some((&q[pos * 2 * k..(pos + 1) * 2 * k], &s[pos * 2..pos * 2 + 2]))
    }

    /// The (A view, B view) vector of segment row `r` as float32, into `out` (`2 * k` values).
    pub fn vec_row_f32(&self, r: usize, out: &mut [f32]) -> bool {
        let Some((q, s)) = self.vec_row(r) else { return false };
        let k = self.k;
        dequantize(&q[..k], s[0], &mut out[..k]);
        dequantize(&q[k..], s[1], &mut out[k..2 * k]);
        true
    }

    /// Index of each key in this segment's vocabulary, -1 when absent.
    pub fn locate(&self, keys: &[i64]) -> Vec<i64> {
        let vocab = self.keys();
        keys.iter().map(|k| vocab.binary_search(k).map(|i| i as i64).unwrap_or(-1)).collect()
    }

    /// Position of `id` in this segment, if present.
    pub fn row_of(&self, id: i64) -> Option<usize> {
        self.ids().binary_search(&id).ok()
    }
}

fn save_i64(p: &Path, name: &str, v: &[i64]) -> Result<()> {
    npy::save(&p.join(format!("{name}.npy")), v, &[v.len()])
}

/// fields[c] = (row i32[E], key i64[E], tf u8[E]); if `row_sorted` the entries are already in
/// ascending row order (true for freshly indexed chunks, false when merging). `vec` is [n, 2, k],
/// with its `k`.
pub fn write_segment(
    stop: Stop,
    dest: &Path,
    ids: &[i64],
    dl: &[f32],
    fields: &[(Vec<i32>, Vec<i64>, Vec<u8>)],
    vec: Option<(&QVecs, usize)>,
    row_sorted: bool,
) -> Result<()> {
    let n = ids.len();
    let tmp = PathBuf::from(format!("{}.tmp", dest.display()));
    let _ = std::fs::remove_dir_all(&tmp);
    // a write that stops or fails takes its half-written directory with it; after the rename
    // there is nothing left to take
    struct Leftover<'a>(&'a Path);
    impl Drop for Leftover<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0);
        }
    }
    let _leftover = Leftover(&tmp);
    std::fs::create_dir_all(&tmp).with_context(|| format!("create {}", tmp.display()))?;
    let mut vocab: Vec<i64> = fields.iter().flat_map(|f| f.1.iter().copied()).collect();
    vocab.sort_unstable();
    vocab.dedup();
    let nv = vocab.len();
    let mut pairs: Vec<i64> = Vec::new();
    for (c, (rows, keys, tfs)) in fields.iter().enumerate() {
        let mut counts = vec![0i64; nv];
        let (doc, tfo) = if !keys.is_empty() {
            let vi: Vec<i32> = keys.iter().map(|k| vocab.binary_search(k).unwrap() as i32).collect();
            let mut order: Vec<usize> = (0..keys.len()).collect();
            if row_sorted {
                order.sort_by_key(|&i| vi[i]); // stable: rows stay ascending within a term
            } else {
                order.sort_by_key(|&i| (vi[i], rows[i]));
            }
            for &v in &vi {
                counts[v as usize] += 1;
            }
            pairs.extend(vi.iter().zip(rows.iter()).map(|(&v, &r)| v as i64 * n as i64 + r as i64));
            let doc: Vec<i32> = order.iter().map(|&i| rows[i]).collect();
            let tfo: Vec<u8> = order.iter().map(|&i| tfs[i]).collect();
            (doc, tfo)
        } else {
            (Vec::new(), Vec::new())
        };
        let mut ptr = vec![0i64; nv + 1];
        for v in 0..nv {
            ptr[v + 1] = ptr[v] + counts[v];
        }
        save_i64(&tmp, &format!("ptr{c}"), &ptr)?;
        npy::save(&tmp.join(format!("doc{c}.npy")), &doc, &[doc.len()])?;
        npy::save(&tmp.join(format!("tf{c}.npy")), &tfo, &[tfo.len()])?;
    }
    // documents containing the term in *any* field
    pairs.sort_unstable();
    pairs.dedup();
    let mut df = vec![0i32; nv];
    for p in pairs {
        df[(p / n as i64) as usize] += 1;
    }
    save_i64(&tmp, "keys", &vocab)?;
    npy::save(&tmp.join("df.npy"), &df, &[nv])?;
    save_i64(&tmp, "ids", ids)?;
    npy::save(&tmp.join("dl.npy"), dl, &[n])?;
    if let Some((v, k)) = vec {
        if n >= IVF_MIN_ROWS {
            let order = build_ivf(stop, &tmp, v, n, k)?;
            let mut q = vec![0i8; v.q.len()];
            let mut sc = vec![0f32; v.scale.len()];
            for (pos, &r) in order.iter().enumerate() {
                q[pos * 2 * k..(pos + 1) * 2 * k].copy_from_slice(&v.q[r * 2 * k..(r + 1) * 2 * k]);
                sc[pos * 2..pos * 2 + 2].copy_from_slice(&v.scale[r * 2..r * 2 + 2]);
            }
            npy::save(&tmp.join("vec.npy"), &q, &[n, 2, k])?;
            npy::save(&tmp.join("vscale.npy"), &sc, &[n, 2])?;
        } else {
            npy::save(&tmp.join("vec.npy"), &v.q, &[n, 2, k])?;
            npy::save(&tmp.join("vscale.npy"), &v.scale, &[n, 2])?;
        }
    }
    crate::index::sync_dir(&tmp)?;
    crate::index::rename_settled(&tmp, dest).with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
    dest.parent().map_or(Ok(()), crate::index::sync_dir)?;
    Ok(())
}

/// One file's postings of one field inside a `SegmentBuilder`: (first row, row counts, keys, tf).
type FieldPart = (usize, Vec<i32>, Vec<i64>, Vec<u8>);

/// Accumulates FileLex batches (in chunk-id order) and writes one segment.
pub struct SegmentBuilder {
    ids: Vec<i64>,
    dl: Vec<f32>,
    parts: Vec<Vec<FieldPart>>, // per field
    vec: Vec<QVecs>,
    pub n: usize,
    pub k: usize,
}

impl SegmentBuilder {
    pub fn new(k: usize) -> Self {
        SegmentBuilder { ids: Vec::new(), dl: Vec::new(), parts: (0..NF).map(|_| Vec::new()).collect(), vec: Vec::new(), n: 0, k }
    }

    pub fn add_file(&mut self, first_id: i64, lex: FileLex) {
        let m = lex.dl.len();
        if m == 0 {
            return;
        }
        self.ids.extend((0..m as i64).map(|i| first_id + i));
        self.dl.extend_from_slice(&lex.dl);
        let FileLex { keys, tf, cnt, vec, .. } = lex;
        for (c, ((k, t), cn)) in keys.into_iter().zip(tf).zip(cnt).enumerate() {
            self.parts[c].push((self.n, cn, k, t));
        }
        if let Some(v) = vec {
            self.vec.push(v);
        }
        self.n += m;
    }

    pub fn write(&self, stop: Stop, dest: &Path) -> Result<()> {
        let mut fields = Vec::with_capacity(NF);
        for c in 0..NF {
            let mut rows = Vec::new();
            let mut keys = Vec::new();
            let mut tfs = Vec::new();
            for (first, cnt, k, t) in &self.parts[c] {
                for (i, &cn) in cnt.iter().enumerate() {
                    for _ in 0..cn {
                        rows.push((*first + i) as i32);
                    }
                }
                keys.extend_from_slice(k);
                tfs.extend_from_slice(t);
            }
            fields.push((rows, keys, tfs));
        }
        let vec = (!self.vec.is_empty()).then(|| QVecs {
            q: self.vec.iter().flat_map(|v| v.q.iter().copied()).collect(),
            scale: self.vec.iter().flat_map(|v| v.scale.iter().copied()).collect(),
        });
        write_segment(stop, dest, &self.ids, &self.dl, &fields, vec.as_ref().map(|v| (v, self.k)), true)
    }
}

/// Where the vectors of a merged segment come from.
pub enum Vectors<'a> {
    /// the ones the segments hold
    Kept,
    /// none: the index keeps no vectors
    Dropped,
    /// made anew: the (A view, B view) codes and scales of a chunk id, `2 * k` and 2 values
    Made(usize, &'a dyn Fn(i64) -> Option<(&'a [i8], &'a [f32])>),
}

/// Write one segment holding only the alive documents of `segs` (any order, ids may interleave).
pub fn merge_segments(stop: Stop, dest: &Path, segs: &[&Segment], alive: &[&[bool]]) -> Result<()> {
    merge_segments_with(stop, dest, segs, alive, Vectors::Kept)
}

/// `merge_segments`, with the vectors `vectors` names.
pub fn merge_segments_with(stop: Stop, dest: &Path, segs: &[&Segment], alive: &[&[bool]], vectors: Vectors) -> Result<()> {
    // the loops below index the arrays directly
    for s in segs {
        s.validate_deep().map_err(|_| damaged())?;
    }
    let mut ids: Vec<i64> = Vec::new();
    for (s, a) in segs.iter().zip(alive) {
        ids.extend(s.ids().iter().zip(a.iter()).filter(|(_, &al)| al).map(|(&i, _)| i));
    }
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.sort_by_key(|&i| ids[i]);
    let mut rank = vec![0usize; ids.len()];
    for (new, &old) in order.iter().enumerate() {
        rank[old] = new;
    }
    let mut dl_cat: Vec<f32> = Vec::new();
    for (s, a) in segs.iter().zip(alive) {
        dl_cat.extend(s.dl().iter().zip(a.iter()).filter(|(_, &al)| al).map(|(&d, _)| d));
    }
    let dl: Vec<f32> = order.iter().map(|&i| dl_cat[i]).collect();
    let (k, has_vec) = match vectors {
        Vectors::Kept => (segs.iter().map(|s| s.k).max().unwrap_or(0), segs.iter().any(|s| s.vec.is_some())),
        Vectors::Dropped => (0, false),
        Vectors::Made(k, _) => (k, true),
    };
    let vec: Option<QVecs> = if has_vec && k > 0 {
        let mut cat = QVecs::default();
        for (s, a) in segs.iter().zip(alive) {
            if s.vec.is_some() || matches!(vectors, Vectors::Made(..)) {
                for (r, &al) in a.iter().enumerate() {
                    if al {
                        let (q, sc) = match &vectors {
                            Vectors::Made(_, of) => of(s.ids()[r]).ok_or(crate::index::Damaged("a chunk has no vector"))?,
                            _ => s.vec_row(r).ok_or_else(damaged)?,
                        };
                        cat.q.extend_from_slice(q);
                        cat.scale.extend_from_slice(sc);
                    }
                }
            }
        }
        let mut out = QVecs { q: Vec::with_capacity(cat.q.len()), scale: Vec::with_capacity(cat.scale.len()) };
        for &i in &order {
            out.q.extend_from_slice(&cat.q[i * 2 * k..(i + 1) * 2 * k]);
            out.scale.extend_from_slice(&cat.scale[i * 2..i * 2 + 2]);
        }
        Some(out)
    } else {
        None
    };
    let mut fields: Vec<(Vec<i32>, Vec<i64>, Vec<u8>)> = (0..NF).map(|_| (Vec::new(), Vec::new(), Vec::new())).collect();
    let mut offset = 0usize;
    for (s, a) in segs.iter().zip(alive) {
        // position among the concatenated alive docs, for every original row
        let mut newrow = vec![0usize; s.n];
        let mut acc = 0usize;
        for (r, &al) in a.iter().enumerate() {
            if al {
                newrow[r] = offset + acc;
                acc += 1;
            }
        }
        let keys = s.keys();
        for c in 0..NF {
            let ptr = s.ptr(c);
            if ptr[ptr.len() - 1] == 0 {
                continue;
            }
            let doc = s.doc(c);
            let tf = s.tf(c);
            for t in 0..keys.len() {
                for e in ptr[t] as usize..ptr[t + 1] as usize {
                    let r = doc[e] as usize;
                    if a[r] {
                        fields[c].0.push(rank[newrow[r]] as i32);
                        fields[c].1.push(keys[t]);
                        fields[c].2.push(tf[e]);
                    }
                }
            }
        }
        offset += acc;
    }
    let ids_sorted: Vec<i64> = order.iter().map(|&i| ids[i]).collect();
    write_segment(stop, dest, &ids_sorted, &dl, &fields, vec.as_ref().map(|v| (v, k)), false)
}

// ------------------------------------------------------------------------- inverted file
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    /// `m` distinct indices below `n`, sorted.
    fn sample(&mut self, n: usize, m: usize) -> Vec<usize> {
        if m >= n {
            return (0..n).collect();
        }
        let mut pool: Vec<usize> = (0..n).collect();
        for i in 0..m {
            let j = i + self.below(n - i);
            pool.swap(i, j);
        }
        let mut s = pool[..m].to_vec();
        s.sort_unstable();
        s
    }
}

/// Whether the operation that writes a segment was asked to stop: clustering a large segment's
/// vectors takes minutes in a debug build, and a stop is seen within one block of rows.
pub type Stop<'a> = &'a (dyn Fn() -> bool + Sync);

/// Nearest centroid of each row of `x` (unit rows, unit centroids): the centroid with the largest
/// `dot_seq64`, the lowest cluster number on a tie. A float32 product in any summation order is
/// within k * 2^-24 of the true dot product, so the winner always scores within `margin` of the
/// float32 maximum; only rows with more than one centroid that close are decided by `dot_seq64`.
fn nearest(stop: Stop, x: &[f32], k: usize, cent: &[f32], nk: usize) -> Vec<i32> {
    let margin = 4.0 * k as f32 * f32::EPSILON;
    let n = x.len() / k;
    let block = 2048;
    let mut out = vec![0i32; n];
    out.par_chunks_mut(block).enumerate().for_each(|(bi, chunk)| {
        if stop() {
            return;
        }
        let r0 = bi * block;
        let rows = chunk.len();
        let mut c = vec![0f32; rows * nk];
        unsafe {
            matrixmultiply::sgemm(rows, k, nk, 1.0, x[r0 * k..].as_ptr(), k as isize, 1, cent.as_ptr(), 1, k as isize, 0.0, c.as_mut_ptr(), nk as isize, 1);
        }
        for r in 0..rows {
            let row = &c[r * nk..(r + 1) * nk];
            let floor = row.iter().copied().fold(f32::NEG_INFINITY, f32::max) - margin;
            let mut cand = (0..nk).filter(|&j| row[j] >= floor);
            let first = cand.next().unwrap_or(0);
            let mut best = first;
            if let Some(second) = cand.next() {
                let xr = &x[(r0 + r) * k..(r0 + r + 1) * k];
                let mut bv = f64::NEG_INFINITY;
                for j in [first, second].into_iter().chain(cand) {
                    let s = dot_seq64(xr, &cent[j * k..(j + 1) * k]);
                    if s > bv {
                        bv = s;
                        best = j;
                    }
                }
            }
            chunk[r] = best as i32;
        }
    });
    out
}

/// a . b from exact float64 products in a fixed order, the same value in both engines: lane l sums
/// j = l, l + 4, ... in turn, the lanes add as (0 + 1) + (2 + 3), then the last k % 4 terms in turn.
fn dot_seq64(a: &[f32], b: &[f32]) -> f64 {
    let ((ca, ta), (cb, tb)) = (a.as_chunks::<4>(), b.as_chunks::<4>());
    let mut acc = [0f64; 4];
    for (x, y) in ca.iter().zip(cb) {
        for l in 0..4 {
            acc[l] += x[l] as f64 * y[l] as f64;
        }
    }
    let mut s = (acc[0] + acc[1]) + (acc[2] + acc[3]);
    for (x, y) in ta.iter().zip(tb) {
        s += *x as f64 * *y as f64;
    }
    s
}

/// Divide each row by its norm; the squares are summed in float32 in index order.
fn normalize_rows(m: &mut [f32], k: usize) {
    for row in m.chunks_mut(k) {
        let mut s = 0f32;
        for v in row.iter() {
            s += v * v;
        }
        let n = s.sqrt().max(1e-6);
        for v in row.iter_mut() {
            *v /= n;
        }
    }
}

/// Spherical k-means on `nsample` rows of the A+B vectors, then one assignment pass over every row:
/// (centroids [nk, k], cluster order, cluster pointers [nk + 1]).
fn kmeans(stop: Stop, vec: &QVecs, n: usize, k: usize, nsample: usize) -> Option<(Vec<f32>, Vec<usize>, Vec<i64>)> {
    let mut unit = vec![0f32; n * k];
    for r in 0..n {
        let (q, s) = (&vec.q[r * 2 * k..(r + 1) * 2 * k], &vec.scale[r * 2..r * 2 + 2]);
        for j in 0..k {
            unit[r * k + j] = q[j] as f32 * s[0] + q[k + j] as f32 * s[1];
        }
    }
    normalize_rows(&mut unit, k);
    let nk = ((2.0 * (n as f64).sqrt()).round() as usize).clamp(32, 1024);
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let sample_idx = rng.sample(n, n.min(nsample));
    let ns = sample_idx.len();
    let mut sample = vec![0f32; ns * k];
    for (i, &r) in sample_idx.iter().enumerate() {
        sample[i * k..(i + 1) * k].copy_from_slice(&unit[r * k..(r + 1) * k]);
    }
    let mut cent = vec![0f32; nk * k];
    for (c, &i) in rng.sample(ns, nk).iter().enumerate() {
        cent[c * k..(c + 1) * k].copy_from_slice(&sample[i * k..(i + 1) * k]);
    }
    for _ in 0..IVF_ITERS {
        let a = nearest(stop, &sample, k, &cent, nk);
        if stop() {
            return None;
        }
        let mut sums = vec![0f32; nk * k];
        let mut counts = vec![0usize; nk];
        for (i, &c) in a.iter().enumerate() {
            let c = c as usize;
            counts[c] += 1;
            let row = &sample[i * k..(i + 1) * k];
            for j in 0..k {
                sums[c * k + j] += row[j];
            }
        }
        for c in 0..nk {
            if counts[c] > 0 {
                cent[c * k..(c + 1) * k].copy_from_slice(&sums[c * k..(c + 1) * k]);
                normalize_rows(&mut cent[c * k..(c + 1) * k], k);
            } else {
                let i = rng.below(ns); // re-seed an empty cluster from a random row
                cent[c * k..(c + 1) * k].copy_from_slice(&sample[i * k..(i + 1) * k]);
            }
        }
    }
    let a = nearest(stop, &unit, k, &cent, nk);
    if stop() {
        return None;
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| a[i]); // stable
    let mut ptr = vec![0i64; nk + 1];
    for &c in &a {
        ptr[c as usize + 1] += 1;
    }
    for c in 0..nk {
        ptr[c + 1] += ptr[c];
    }
    Some((cent, order, ptr))
}

/// Cluster the A+B vectors of a segment (see `kmeans`). Returns the cluster order (segment row at
/// each sorted position); the caller stores the vectors in that order so a query can scan a few
/// clusters contiguously.
pub fn build_ivf(stop: Stop, tmp: &Path, vec: &QVecs, n: usize, k: usize) -> Result<Vec<usize>> {
    let (cent, order, ptr) = kmeans(stop, vec, n, k, IVF_SAMPLE).ok_or_else(|| anyhow::anyhow!("cancelled"))?;
    let nk = ptr.len() - 1;
    let mut inv = vec![0i32; n];
    for (pos, &r) in order.iter().enumerate() {
        inv[r] = pos as i32;
    }
    npy::save(&tmp.join("ivf_cent.npy"), &cent, &[nk, k])?;
    npy::save(&tmp.join("ivf_ptr.npy"), &ptr, &[nk + 1])?;
    let order32: Vec<i32> = order.iter().map(|&r| r as i32).collect();
    npy::save(&tmp.join("ivf_order.npy"), &order32, &[n])?;
    npy::save(&tmp.join("ivf_inv.npy"), &inv, &[n])?;
    Ok(order)
}

// ------------------------------------------------------------------------------ store
/// a . b accumulated in float64 and rounded once (a float32 sum moves with the order of its
/// additions, and near-tied rows would swap places with it).
fn dot64(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f64; 4];
    let chunks = a.len() / 4;
    for i in 0..chunks {
        for j in 0..4 {
            acc[j] += a[i * 4 + j] as f64 * b[i * 4 + j] as f64;
        }
    }
    let mut s: f64 = acc.iter().sum();
    for i in chunks * 4..a.len() {
        s += a[i] as f64 * b[i] as f64;
    }
    s as f32
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let chunks = a.len() / 8;
    for i in 0..chunks {
        for j in 0..8 {
            acc[j] += a[i * 8 + j] * b[i * 8 + j];
        }
    }
    let mut s: f32 = acc.iter().sum();
    for i in chunks * 8..a.len() {
        s += a[i] * b[i];
    }
    s
}

/// Indices of the `limit` best eligible values, deterministically: every row tied with the last one
/// kept is considered and ties are broken by index, so two engines (or two runs) select the same rows.
fn top_indices(sc: &[f32], limit: usize, keep: impl Fn(f32) -> bool) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..sc.len()).filter(|&i| keep(sc[i])).collect();
    if idx.len() > limit {
        idx.select_nth_unstable_by(limit - 1, |&a, &b| sc[b].partial_cmp(&sc[a]).unwrap());
        let thresh = idx[..limit].iter().map(|&i| sc[i]).fold(f32::INFINITY, f32::min);
        idx.retain(|&i| sc[i] >= thresh);
    }
    idx.sort_by(|&a, &b| sc[b].partial_cmp(&sc[a]).unwrap().then(a.cmp(&b)));
    idx.truncate(limit);
    idx
}

/// All segments of an index plus the global statistics needed to score across them.
pub struct LexStore {
    pub base: PathBuf,
    pub names: Vec<String>,
    pub segs: Vec<Segment>,
    w: [f32; NF],
    pub alive: Vec<Vec<bool>>,
    pub has_dead: Vec<bool>,
    dlsum: Vec<f64>,
    adf: HashMap<(usize, usize), i64>,
    norm: Vec<Vec<f32>>,
    mix: Vec<Option<Vec<f32>>>,           // exhaustive scan matrix of the small segments
    alive_sorted: Vec<Option<Vec<bool>>>, // liveness in cluster order (ivf segments)
    pub nprobe: usize,
    pub n_alive: usize,
    pub avgdl: f64,
}

impl LexStore {
    pub fn open(base: &Path, names: &[String]) -> Result<LexStore> {
        let segs = names.iter().map(|n| Segment::open(&base.join(n))).collect::<Result<Vec<_>>>()?;
        Ok(LexStore {
            base: base.to_path_buf(),
            names: names.to_vec(),
            segs,
            w: WEIGHTS,
            alive: Vec::new(),
            has_dead: Vec::new(),
            dlsum: Vec::new(),
            adf: HashMap::new(),
            norm: Vec::new(),
            mix: Vec::new(),
            alive_sorted: Vec::new(),
            nprobe: IVF_NPROBE,
            n_alive: 0,
            avgdl: 1.0,
        })
    }

    /// Statistics from scratch. `live_ids`: sorted ids of the chunks currently in the index, or None
    /// when every row of every segment is known to be alive.
    pub fn prepare(&mut self, live_ids: Option<&[i64]>) -> Result<()> {
        self.alive.clear();
        self.dlsum.clear();
        for s in &self.segs {
            let a: Vec<bool> = match live_ids {
                None => vec![true; s.n],
                Some(live) => s.ids().iter().map(|id| live.binary_search(id).is_ok()).collect(),
            };
            self.dlsum.push(s.dl().iter().zip(&a).filter(|(_, &al)| al).map(|(&d, _)| d as f64).sum());
            self.alive.push(a);
        }
        self.n_alive = self.alive.iter().map(|a| a.iter().filter(|&&x| x).count()).sum();
        self.has_dead = self.alive.iter().map(|a| a.iter().any(|&x| !x)).collect();
        self.mix = self.segs.iter().zip(&self.alive).map(|(s, a)| Self::mix_of(s, a)).collect();
        let in_cluster_order =
            |ivf: &Ivf, a: &[bool]| ivf.order.view::<i32>().unwrap().iter().map(|&r| a.get(r as usize).copied().ok_or_else(damaged)).collect();
        self.alive_sorted =
            self.segs.iter().zip(&self.alive).map(|(s, a)| s.ivf.as_ref().map(|ivf| in_cluster_order(ivf, a)).transpose()).collect::<Result<_>>()?;
        self.adf.clear();
        self.set_norm();
        Ok(())
    }

    fn mix_of(s: &Segment, alive: &[bool]) -> Option<Vec<f32>> {
        if s.ivf.is_some() || s.n == 0 {
            return None;
        }
        s.vec()?;
        let k = s.k;
        let mut m = vec![0f32; s.n * k];
        let mut v = vec![0f32; 2 * k];
        for r in 0..s.n {
            if alive[r] && s.vec_row_f32(r, &mut v) {
                for j in 0..k {
                    m[r * k + j] = v[j] + v[k + j];
                }
            }
        }
        Some(m)
    }

    fn set_norm(&mut self) {
        self.avgdl = if self.n_alive > 0 { self.dlsum.iter().sum::<f64>() / self.n_alive as f64 } else { 1.0 };
        let avg = self.avgdl as f32;
        self.norm = self.segs.iter().map(|s| s.dl().iter().map(|&d| K1 * (1.0 - B + B * d / avg)).collect()).collect();
    }

    /// Patch the statistics after the chunks `removed` died and the segments `added` appeared.
    pub fn apply(&mut self, removed: &[i64], added: &[String]) -> Result<()> {
        let mut removed = removed.to_vec();
        removed.sort_unstable();
        removed.dedup();
        if !removed.is_empty() {
            self.adf.clear();
            for si in 0..self.segs.len() {
                let s = &self.segs[si];
                if s.n == 0 {
                    continue;
                }
                let ids = s.ids();
                let mut rows: Vec<usize> = removed.iter().filter_map(|id| ids.binary_search(id).ok()).collect();
                rows.retain(|&r| self.alive[si][r]);
                if rows.is_empty() {
                    continue;
                }
                let dl = s.dl();
                for &r in &rows {
                    self.alive[si][r] = false;
                    self.dlsum[si] -= dl[r] as f64;
                    if let Some(m) = self.mix[si].as_mut() {
                        for v in &mut m[r * s.k..(r + 1) * s.k] {
                            *v = 0.0;
                        }
                    }
                    if let (Some(a), Some(ivf)) = (self.alive_sorted[si].as_mut(), s.ivf.as_ref()) {
                        *a.get_mut(ivf.inv.view::<i32>().unwrap()[r] as usize).ok_or_else(damaged)? = false;
                    }
                }
                self.n_alive -= rows.len();
                self.has_dead[si] = true;
            }
        }
        for name in added {
            let s = Segment::open(&self.base.join(name))?;
            let a = vec![true; s.n];
            self.dlsum.push(s.dl().iter().map(|&d| d as f64).sum());
            self.n_alive += s.n;
            self.has_dead.push(false);
            self.mix.push(Self::mix_of(&s, &a));
            self.alive_sorted.push(s.ivf.as_ref().map(|_| vec![true; s.n]));
            self.alive.push(a);
            self.names.push(name.clone());
            self.segs.push(s);
        }
        self.set_norm();
        Ok(())
    }

    pub fn alive_counts(&self) -> Vec<usize> {
        self.alive.iter().map(|a| a.iter().filter(|&&x| x).count()).collect()
    }

    // ---- lexical
    fn alive_df(&mut self, si: usize, j: usize) -> Result<i64> {
        if let Some(&v) = self.adf.get(&(si, j)) {
            return Ok(v);
        }
        let s = &self.segs[si];
        let mut seen = vec![false; s.n];
        for c in 0..NF {
            for &d in s.postings(c, j)?.0 {
                *seen.get_mut(d as usize).ok_or_else(damaged)? = true;
            }
        }
        let got = seen.iter().zip(&self.alive[si]).filter(|(&s, &a)| s && a).count() as i64;
        self.adf.insert((si, j), got);
        Ok(got)
    }

    /// idf over the live chunks: ln((N - df + 0.5) / (df + 0.5)), floored.
    fn idf(&mut self, locs: &[Vec<i64>]) -> Result<Vec<f32>> {
        let nt = locs.first().map(|l| l.len()).unwrap_or(0);
        let mut df = vec![0i64; nt];
        for si in 0..self.segs.len() {
            for t in 0..nt {
                let j = locs[si][t];
                if j < 0 {
                    continue;
                }
                if self.has_dead[si] {
                    df[t] += self.alive_df(si, j as usize)?;
                } else {
                    df[t] += self.segs[si].df()[j as usize] as i64;
                }
            }
        }
        let n = self.n_alive as f64;
        let idf = |&d: &i64| {
            let v = ((n - d as f64 + 0.5) / (d as f64 + 0.5)).ln();
            if v.is_finite() && v > 0.0 {
                v as f32
            } else {
                IDF_FLOOR
            }
        };
        Ok(df.iter().map(idf).collect())
    }

    /// Top `limit` chunks by field-weighted BM25 -> (ids, scores), best first.
    pub fn search(&mut self, keys: &[i64], limit: usize) -> Result<(Vec<i64>, Vec<f32>)> {
        if keys.is_empty() || self.n_alive == 0 {
            return Ok((Vec::new(), Vec::new()));
        }
        let locs: Vec<Vec<i64>> = self.segs.iter().map(|s| s.locate(keys)).collect();
        let idf = self.idf(&locs)?;
        let mut out_ids: Vec<i64> = Vec::new();
        let mut out_sc: Vec<f32> = Vec::new();
        for si in 0..self.segs.len() {
            let s = &self.segs[si];
            let present: Vec<usize> = (0..keys.len()).filter(|&t| locs[si][t] >= 0).collect();
            if present.is_empty() || s.n == 0 {
                continue;
            }
            let norm = &self.norm[si];
            let alive = &self.alive[si];
            let mut f = vec![0f32; s.n];
            let mut acc = vec![0f32; s.n];
            let mut touched: Vec<usize> = Vec::new();
            for &t in &present {
                let jt = locs[si][t] as usize;
                touched.clear();
                for c in 0..NF {
                    let (rows, tfs) = s.postings(c, jt)?;
                    for (&d, &tf) in rows.iter().zip(tfs) {
                        let d = d as usize;
                        let fd = f.get_mut(d).ok_or_else(damaged)?;
                        if *fd == 0.0 {
                            touched.push(d);
                        }
                        *fd += self.w[c] * tf as f32;
                    }
                }
                let w = idf[t] * (K1 + 1.0);
                for &d in &touched {
                    let fu = f[d];
                    acc[d] += w * fu / (fu + norm[d]);
                    f[d] = 0.0;
                }
            }
            for (r, al) in alive.iter().enumerate() {
                if !al {
                    acc[r] = 0.0;
                }
            }
            let ids = s.ids();
            for p in top_indices(&acc, limit, |v| v > 0.0) {
                out_ids.push(ids[p]);
                out_sc.push(acc[p]);
            }
        }
        let mut order: Vec<usize> = (0..out_ids.len()).collect();
        order.sort_by(|&a, &b| out_sc[b].partial_cmp(&out_sc[a]).unwrap().then(out_ids[a].cmp(&out_ids[b])));
        order.truncate(limit);
        Ok((order.iter().map(|&i| out_ids[i]).collect(), order.iter().map(|&i| out_sc[i]).collect()))
    }

    /// -> [len(ids)][1 + NF]: weighted score, then the score of each single field.
    pub fn field_scores(&mut self, keys: &[i64], ids: &[i64]) -> Result<Vec<[f32; 1 + NF]>> {
        let mut out = vec![[0f32; 1 + NF]; ids.len()];
        if keys.is_empty() || ids.is_empty() || self.n_alive == 0 {
            return Ok(out);
        }
        let locs: Vec<Vec<i64>> = self.segs.iter().map(|s| s.locate(keys)).collect();
        let idf = self.idf(&locs)?;
        for si in 0..self.segs.len() {
            let s = &self.segs[si];
            if s.n == 0 {
                continue;
            }
            let sel: Vec<(usize, usize)> = ids.iter().enumerate().filter_map(|(i, id)| s.row_of(*id).filter(|&r| self.alive[si][r]).map(|r| (i, r))).collect();
            if sel.is_empty() {
                continue;
            }
            let norm = &self.norm[si];
            for t in 0..keys.len() {
                let jt = locs[si][t];
                if jt < 0 {
                    continue;
                }
                let jt = jt as usize;
                let w = idf[t] * (K1 + 1.0);
                for &(i, r) in &sel {
                    let mut tfm = [0f32; NF];
                    for c in 0..NF {
                        let (rows, tfs) = s.postings(c, jt)?;
                        if let Ok(p) = rows.binary_search(&(r as i32)) {
                            tfm[c] = tfs[p] as f32;
                        }
                    }
                    let nm = norm[r];
                    let mut comb = 0f32;
                    for c in 0..NF {
                        comb += tfm[c] * self.w[c];
                    }
                    out[i][0] += w * comb / (comb + nm);
                    for c in 0..NF {
                        out[i][1 + c] += w * tfm[c] / (tfm[c] + nm);
                    }
                }
            }
        }
        Ok(out)
    }

    // ---- semantic
    /// Ids of the `limit` chunks closest to the query vector (A-view + B-view similarity). Segments
    /// with an inverted-file index scan their `nprobe` closest clusters (all of them when nprobe is
    /// at least the number of clusters, which makes the result exact).
    pub fn semantic_top(&self, qz: &[f32], limit: usize, nprobe: Option<usize>) -> Result<Vec<i64>> {
        let nprobe = nprobe.unwrap_or(self.nprobe);
        let mut out_ids: Vec<i64> = Vec::new();
        let mut out_sc: Vec<f32> = Vec::new();
        for si in 0..self.segs.len() {
            let s = &self.segs[si];
            let (part, sc): (Vec<usize>, Vec<f32>) = if s.ivf.is_some() {
                self.ivf_scan(si, qz, limit, nprobe)
            } else {
                let Some(m) = self.mix[si].as_ref() else {
                    continue;
                };
                let k = s.k;
                let alive = &self.alive[si];
                let sc: Vec<f32> = (0..s.n).map(|r| if alive[r] { dot64(&m[r * k..(r + 1) * k], qz) } else { f32::NEG_INFINITY }).collect();
                let part = top_indices(&sc, limit, |v| v.is_finite());
                let scs = part.iter().map(|&p| sc[p]).collect();
                (part, scs)
            };
            let ids = s.ids();
            for (p, v) in part.into_iter().zip(sc) {
                out_ids.push(*ids.get(p).ok_or_else(damaged)?);
                out_sc.push(v);
            }
        }
        let mut order: Vec<usize> = (0..out_ids.len()).collect();
        order.sort_by(|&a, &b| out_sc[b].partial_cmp(&out_sc[a]).unwrap().then(out_ids[a].cmp(&out_ids[b])));
        order.truncate(limit);
        Ok(order.iter().map(|&i| out_ids[i]).collect())
    }

    /// -> (segment rows, scores) of the best `limit` live rows in the `nprobe` closest clusters.
    fn ivf_scan(&self, si: usize, qz: &[f32], limit: usize, nprobe: usize) -> (Vec<usize>, Vec<f32>) {
        let s = &self.segs[si];
        let ivf = s.ivf.as_ref().unwrap();
        let k = s.k;
        let (vq, vs) = s.vec().unwrap(); // cluster order
        let order = ivf.order.view::<i32>().unwrap();
        let alive = self.alive_sorted[si].as_ref().unwrap();
        let ranges: Vec<(usize, usize)> = if nprobe < ivf.nclusters {
            let cs: Vec<f32> = (0..ivf.nclusters).map(|c| dot64(&ivf.cent[c * k..(c + 1) * k], qz)).collect();
            top_indices(&cs, nprobe, |_| true).into_iter().map(|c| (ivf.ptr[c] as usize, ivf.ptr[c + 1] as usize)).collect()
        } else {
            vec![(0, s.n)]
        };
        let total: usize = ranges.iter().map(|(a, b)| b - a).sum();
        let mut positions: Vec<usize> = Vec::with_capacity(total);
        let mut sc: Vec<f32> = Vec::with_capacity(total);
        let mut mix = vec![0f32; k];
        for (a, b) in ranges {
            for p in a..b {
                positions.push(p);
                if !alive[p] {
                    sc.push(f32::NEG_INFINITY);
                    continue;
                }
                let (row, s) = (&vq[p * 2 * k..(p + 1) * 2 * k], &vs[p * 2..p * 2 + 2]);
                for j in 0..k {
                    mix[j] = row[j] as f32 * s[0] + row[k + j] as f32 * s[1];
                }
                sc.push(dot64(&mix, qz));
            }
        }
        let part = top_indices(&sc, limit, |v| v.is_finite());
        let rows = part.iter().map(|&i| order[positions[i]] as usize).collect();
        let scs = part.iter().map(|&i| sc[i]).collect();
        (rows, scs)
    }

    /// The stored (A view, B view) vectors of a chunk, as float32.
    pub fn vec_of(&self, id: i64) -> Result<Option<(Vec<f32>, Vec<f32>)>> {
        for s in &self.segs {
            if let Some(r) = s.row_of(id) {
                if s.vec().is_none() {
                    return Ok(None);
                }
                let k = s.k;
                let mut v = vec![0f32; 2 * k];
                if !s.vec_row_f32(r, &mut v) {
                    return Err(damaged());
                }
                let b = v.split_off(k);
                return Ok(Some((v, b)));
            }
        }
        Ok(None)
    }

    /// Ids of the `limit` live chunks whose B (implementation) vector is closest to `q` (exhaustive;
    /// used by similar-code queries, which are rare and tolerate ~100 ms on 100k chunks).
    pub fn scan_b(&self, q: &[f32], limit: usize, exclude: i64) -> Result<Vec<(i64, f32)>> {
        let mut out: Vec<(i64, f32)> = Vec::new();
        if limit == 0 {
            return Ok(out);
        }
        let mut row = vec![0f32; q.len()];
        for (si, s) in self.segs.iter().enumerate() {
            if s.vec().is_none() {
                continue;
            }
            let k = s.k;
            let ids = s.ids();
            for r in 0..s.n {
                if !self.alive[si][r] || ids[r] == exclude {
                    continue;
                }
                let (vq, vs) = s.vec_row(r).ok_or_else(damaged)?;
                if vs[1] == 0.0 {
                    continue; // no implementation view
                }
                dequantize(&vq[k..], vs[1], &mut row);
                let sc = dot(&row, q);
                if sc.is_nan() {
                    return Err(damaged()); // a scale that is not a number
                }
                if out.len() < limit {
                    out.push((ids[r], sc));
                    if out.len() == limit {
                        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                    }
                } else if sc > out[limit - 1].1 {
                    out[limit - 1] = (ids[r], sc);
                    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                }
            }
        }
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
        Ok(out)
    }

    /// -> (sim of the A view, sim of the B view) for each id; 0 where a chunk has no vector.
    /// Accumulated in float64 and rounded to float32.
    pub fn sem_pair(&self, ids: &[i64], qz: &[f32]) -> Result<(Vec<f32>, Vec<f32>)> {
        let mut sa = vec![0f32; ids.len()];
        let mut sb = vec![0f32; ids.len()];
        for s in &self.segs {
            if s.vec().is_none() {
                continue;
            }
            let k = s.k;
            let mut v = vec![0f32; 2 * k];
            for (i, id) in ids.iter().enumerate() {
                if let Some(r) = s.row_of(*id) {
                    if !s.vec_row_f32(r, &mut v) {
                        return Err(damaged());
                    }
                    let (mut a, mut b) = (0f64, 0f64);
                    for j in 0..k {
                        a += v[j] as f64 * qz[j] as f64;
                        b += v[k + j] as f64 * qz[j] as f64;
                    }
                    sa[i] = a as f32;
                    sb[i] = b as f32;
                }
            }
        }
        Ok((sa, sb))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_keys_match_python_blake2b() {
        // int.from_bytes(hashlib.blake2b(word, digest_size=8).digest(), "little") >> 1
        assert_eq!(tok_key("token"), 4201213877827993759);
        assert_eq!(tok_key("refresh"), 8715277808867335775);
        assert_eq!(tok_key("a"), 1702698405120146464);
    }

    #[test]
    fn quantize_rounds_ties_to_even() {
        // -0.25 / scale = -63.5 rounds to even
        let v = [0.5f32, -0.25, 0.125, 0.0, -0.5, 0.3, 0.003, -0.002];
        let mut q = [0i8; 8];
        let s = quantize(&v, &mut q);
        assert_eq!(q, [127, -64, 32, 0, -127, 76, 1, -1]);
        assert_eq!(s.to_bits(), 998310404);
        let mut z = [1i8; 4];
        assert_eq!(quantize(&[0.0; 4], &mut z), 0.0);
        assert_eq!(z, [0; 4]);
        let mut d = [0f32; 8];
        dequantize(&q, s, &mut d);
        assert!(d.iter().zip(&v).all(|(a, b)| (a - b).abs() <= s / 2.0));
    }

    #[test]
    fn dot64_matches_python_reference_bit_for_bit() {
        // the same digest is asserted in python/tests/test_lex.py test_the_scan_dot_product_matches_the_rust_engine_bit_for_bit
        let mut rng = Rng(777);
        let (n, k) = (200, 259);
        let mut value = || (rng.below(20001) as f32 - 10000.0) * 2f32.powi(-14);
        let m: Vec<f32> = (0..n * k).map(|_| value()).collect();
        let q: Vec<f32> = (0..k).map(|_| value()).collect();
        let digest: u64 = (0..n).map(|r| dot64(&m[r * k..(r + 1) * k], &q).to_bits() as u64).sum();
        assert_eq!(digest, DOT_DIGEST);
    }

    const DOT_DIGEST: u64 = 438482546770;

    /// A stop asked for while a segment's vectors are clustered ends the clustering at the next
    /// block of rows, with no file written, and the clustering asked once more finishes.
    #[test]
    fn a_stopped_segment_write_leaves_no_directory_behind() {
        let (n, k) = (IVF_MIN_ROWS, 4);
        let base = std::env::temp_dir().join(format!("scrutate_seg_stop_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ids: Vec<i64> = (0..n as i64).collect();
        let fields: Vec<(Vec<i32>, Vec<i64>, Vec<u8>)> = (0..NF).map(|_| (Vec::new(), Vec::new(), Vec::new())).collect();
        let v = QVecs { q: (0..n * 2 * k).map(|i| (i % 7) as i8).collect(), scale: vec![1.0; n * 2] };
        let e = write_segment(&|| true, &base.join("s000001"), &ids, &vec![1.0; n], &fields, Some((&v, k)), true).unwrap_err();
        assert_eq!(e.to_string(), "cancelled");
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0, "a directory was left");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_stop_ends_the_clustering_of_a_segment_s_vectors() {
        let (n, k) = (3000, 16);
        let v = QVecs { q: (0..n * 2 * k).map(|i| (i % 251) as i8).collect(), scale: vec![1.0; n * 2] };
        let tmp = std::env::temp_dir().join(format!("scrutate_ivf_stop_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let late = || calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 3;
        let e = build_ivf(&late, &tmp, &v, n, k).unwrap_err();
        assert_eq!(e.to_string(), "cancelled");
        assert!(!tmp.join("ivf_order.npy").exists());
        assert_eq!(build_ivf(&|| false, &tmp, &v, n, k).unwrap().len(), n);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn kmeans_matches_python_reference_bit_for_bit() {
        // the same digest is asserted in python/tests/test_lex.py test_kmeans_matches_the_rust_engine_bit_for_bit
        let mut rng = Rng(12345);
        let (n, k) = (3000, 16);
        let mut v = QVecs::default();
        for r in 0..n {
            for _ in 0..2 * k {
                v.q.push((rng.below(255) as i64 - 127) as i8);
            }
            for _ in 0..2 {
                v.scale.push((1 + rng.below(64)) as f32 * (2f32).powi(-10));
            }
            if r % 3 == 2 {
                // duplicate rows make tied centroids: the lower cluster number must win
                v.q.copy_within((r - 2) * 2 * k..(r - 1) * 2 * k, r * 2 * k);
                v.scale.copy_within((r - 2) * 2..(r - 1) * 2, r * 2);
            }
        }
        let (cent, order, ptr) = kmeans(&|| false, &v, n, k, 1000).unwrap();
        let sizes: Vec<i64> = ptr.windows(2).map(|w| w[1] - w[0]).collect();
        let weighted: i64 = sizes.iter().enumerate().map(|(c, &s)| (c as i64 + 1) * s).sum();
        let bits: u64 = cent.iter().map(|x| x.to_bits() as u64).sum();
        assert_eq!((&order[..5], sizes.len(), weighted, bits), (&[21usize, 23, 256, 375, 377][..], 110, 164513, 3739691621945));
    }

    #[test]
    fn similar_with_a_limit_of_zero_is_empty() {
        let root = std::env::temp_dir().join(format!("scrutate_lex_similar0_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..3 {
            let src = format!(
                "def make_{i}(items, limit):\n    total = 0\n    for index, item in enumerate(items):\n        if index >= limit:\n            break\n        total += item * {i}\n    return total\n"
            );
            std::fs::write(root.join(format!("m{i}.py")), src).unwrap();
        }
        let mut repo = crate::Repo::open(&root).unwrap();
        repo.update(true).unwrap();
        assert!(!crate::intel::similar(&mut repo, "make_1", 3).unwrap().is_empty());
        assert!(crate::intel::similar(&mut repo, "make_1", 0).unwrap().is_empty());
        drop(repo);
        let _ = std::fs::remove_dir_all(&root);
    }
}
