//! The artifact store: what parsing and tokenizing make of one file, keyed by the file's content
//! and the parts of its path the parser reads, so that the same bytes under any path, in this
//! checkout or in the main checkout of a linked git work tree, are not parsed again.
//!
//! An artifact holds a file's chunks (kind, names, lines, calls), its imports, and per chunk and
//! field the words with their counts: the subwords the vectors are made from and the whole
//! identifiers the postings add. Its chunk texts and vectors travel beside it (`Found`): while a
//! path names it they are the rows and segment rows of the code index, and when the last path
//! that named it drops it they move into the store (`kept`), bounded and least recently used
//! first. Everything a path decides (the path itself, the test flag, the path field) is made by
//! `derive`, the one function that turns an artifact into the rows and postings of a path.
//!
//! The store is a table of the code index's database (`index.rs` keeps it), so it commits in the
//! same transaction as the rows made from it.

use std::collections::HashMap;
use std::io::{Read, Write};

use anyhow::{Result, bail};

use crate::index::{ChunkRow, NewRow, TF_CAP};
use crate::intel::DupKeys;
use crate::lex::{FileLex, NF, QVecs, tok_key};
use crate::semantic::SemanticModel;
use crate::text::{is_test_path, name_key, tokenize2};

/// The layout of an encoded artifact; part of every key, so another layout is never read.
const FORMAT: u32 = 2;
const MAGIC: &[u8] = b"scrutate-artifact 2\n";
const TEXTS_MAGIC: &[u8] = b"scrutate-texts 1\n";
/// No artifact holds more chunks, words or bytes than this; a larger count is damage.
const MAX_ITEMS: usize = 1 << 24;

pub type Key = [u8; 16];

/// The key of the artifact of a file with this content hash at `rel`: besides the bytes, a parse
/// reads the file's name (the language, the module chunk's name, comment marks) and, for an
/// `index` file, its directory's name (an anonymous default export), under the parsing rules of
/// this `CONTENT_REVISION` and of the custom language that claims the file, if one does.
pub fn key(content_hash: &str, rel: &str) -> Key {
    let p = std::path::Path::new(rel);
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let dir = match p.file_stem() {
        Some(s) if s == "index" => p.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
        _ => String::new(),
    };
    let rev = format!("{FORMAT} {}", crate::index::CONTENT_REVISION);
    match crate::custom::current().for_path(rel) {
        Some(l) => crate::tscache::hash(&[b"artifact", rev.as_bytes(), content_hash.as_bytes(), base.as_bytes(), dir.as_bytes(), l.sig().as_bytes()]),
        None => crate::tscache::hash(&[b"artifact", rev.as_bytes(), content_hash.as_bytes(), base.as_bytes(), dir.as_bytes()]),
    }
}

/// The words of one field of a chunk with their counts, each list sorted by word.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Field {
    /// subwords, stemmed: what the vectors and the length of a doc or body count
    pub subs: Vec<(String, u32)>,
    /// whole identifiers
    pub wholes: Vec<(String, u32)>,
}

impl Field {
    pub fn of((subs, wholes): (Vec<String>, Vec<String>)) -> Field {
        Field { subs: counts(&subs), wholes: counts(&wholes) }
    }

    fn len(&self) -> i64 {
        self.subs.iter().map(|w| w.1 as i64).sum()
    }
}

fn counts(words: &[String]) -> Vec<(String, u32)> {
    let mut m: HashMap<&str, u32> = HashMap::with_capacity(words.len());
    for w in words {
        *m.entry(w.as_str()).or_insert(0) += 1;
    }
    let mut v: Vec<(String, u32)> = m.into_iter().map(|(w, c)| (w.to_string(), c)).collect();
    v.sort_unstable();
    v
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chunk {
    pub kind: String,
    pub name: String,
    pub qualname: String,
    pub start: i64,
    pub end: i64,
    /// the name's subwords, each once, in order
    pub name_tok: String,
    pub calls: Vec<String>,
    /// name, qualifier, signature, doc, body
    pub fields: [Field; 5],
    /// what `dupes` compares, for a definition of `CODE_KINDS`
    pub dup: Option<DupKeys>,
}

impl Chunk {
    /// A constant or a field is a symbol only: no postings, no vector.
    fn lexed(&self) -> bool {
        !crate::parse::SYMBOL_ONLY.contains(&self.kind.as_str())
    }
}

/// What a parse makes of a file that depends on nothing but the inputs of its key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Artifact {
    pub chunks: Vec<Chunk>,
    pub imports: Vec<(i64, String)>,
}

/// An artifact with the chunk texts (compressed as the index stores them) and, when a store had
/// them for the model in use, the vectors of its ranked chunks.
pub struct Found {
    pub art: Artifact,
    pub texts: Vec<Vec<u8>>,
    pub vecs: Option<QVecs>,
}

impl Artifact {
    /// The words of a parsed chunk's fields, its name's subwords each once in order, and its body's
    /// subwords in order.
    pub fn tokenize(c: &crate::parse::Chunk) -> ([Field; 5], String, Vec<String>) {
        let qual = c.qualname.rfind('.').map_or("", |i| &c.qualname[..i]);
        let name = tokenize2(&c.name, None);
        let mut seen = std::collections::HashSet::new();
        let name_tok: Vec<&str> = name.0.iter().map(String::as_str).filter(|s| seen.insert(*s)).collect();
        let name_tok = name_tok.join(" ");
        let body = tokenize2(&c.body, Some(crate::text::BODY_CAP));
        let body_subs = body.0.clone();
        let fields = [
            Field::of(name),
            Field::of(tokenize2(qual, None)),
            Field::of(tokenize2(&c.sig, Some(80))),
            Field::of(tokenize2(&c.doc, Some(200))),
            Field::of(body),
        ];
        (fields, name_tok, body_subs)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::with_capacity(256 + 64 * self.chunks.len());
        w.extend(MAGIC);
        put_n(&mut w, self.chunks.len());
        for c in &self.chunks {
            for s in [&c.kind, &c.name, &c.qualname, &c.name_tok] {
                put_str(&mut w, s);
            }
            w.extend(c.start.to_le_bytes());
            w.extend(c.end.to_le_bytes());
            put_n(&mut w, c.calls.len());
            for s in &c.calls {
                put_str(&mut w, s);
            }
            for f in &c.fields {
                for list in [&f.subs, &f.wholes] {
                    put_n(&mut w, list.len());
                    for (s, n) in list {
                        put_str(&mut w, s);
                        w.extend(n.to_le_bytes());
                    }
                }
            }
            // 0: no keys, 1: keys, 2: keys and bands
            match &c.dup {
                None => w.push(0),
                Some(d) => {
                    w.push(if d.bands.is_some() { 2 } else { 1 });
                    w.extend(d.body.to_le_bytes());
                    w.extend(d.words.to_le_bytes());
                    w.extend(d.ntok.to_le_bytes());
                    for b in d.bands.iter().flatten() {
                        w.extend(b.to_le_bytes());
                    }
                }
            }
        }
        put_n(&mut w, self.imports.len());
        for (line, module) in &self.imports {
            w.extend(line.to_le_bytes());
            put_str(&mut w, module);
        }
        deflate(&w)
    }

    /// Read an encoded artifact; anything that is not one in full is an error.
    pub fn decode(bytes: &[u8]) -> Result<Artifact> {
        let raw = inflate(bytes)?;
        let mut r = Reader { b: &raw, pos: 0 };
        if r.take(MAGIC.len())? != MAGIC {
            bail!("not an artifact");
        }
        let mut chunks = Vec::new();
        for _ in 0..r.n()? {
            let (kind, name, qualname, name_tok) = (r.string()?, r.string()?, r.string()?, r.string()?);
            let (start, end) = (r.i64()?, r.i64()?);
            let calls = (0..r.n()?).map(|_| r.string()).collect::<Result<_>>()?;
            let mut fields: [Field; 5] = Default::default();
            for f in &mut fields {
                for list in [&mut f.subs, &mut f.wholes] {
                    for _ in 0..r.n()? {
                        list.push((r.string()?, r.u32()?));
                    }
                }
            }
            let dup = match r.take(1)?[0] {
                0 => None,
                with @ (1 | 2) => {
                    let (body, words, ntok) = (r.u64()?, r.u64()?, r.u32()?);
                    let bands = if with == 2 {
                        let mut b = [0u64; crate::intel::BANDS];
                        for v in &mut b {
                            *v = r.u64()?;
                        }
                        Some(b)
                    } else {
                        None
                    };
                    Some(DupKeys { body, words, ntok, bands })
                }
                other => bail!("unknown duplicate keys mark {other}"),
            };
            chunks.push(Chunk { kind, name, qualname, start, end, name_tok, calls, fields, dup });
        }
        let imports = (0..r.n()?).map(|_| Ok((r.i64()?, r.string()?))).collect::<Result<_>>()?;
        if r.pos != raw.len() {
            bail!("trailing bytes");
        }
        Ok(Artifact { chunks, imports })
    }

    /// How many chunks have postings and vectors.
    pub fn lexed(&self) -> usize {
        self.chunks.iter().filter(|c| c.lexed()).count()
    }

    /// The vectors `m` makes of the ranked chunks, from the counts alone, as `derive` makes them.
    pub fn vectors(&self, m: &SemanticModel) -> QVecs {
        let mut v = QVecs::default();
        for c in self.chunks.iter().filter(|c| c.lexed()) {
            push_vectors(m, c, &mut v);
        }
        v
    }
}

/// The chunk texts of an artifact no path names, as the store keeps them.
pub fn encode_texts(texts: &[Vec<u8>]) -> Vec<u8> {
    let mut w = TEXTS_MAGIC.to_vec();
    put_n(&mut w, texts.len());
    for t in texts {
        put_n(&mut w, t.len());
        w.extend(t);
    }
    w
}

pub fn decode_texts(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut r = Reader { b: bytes, pos: 0 };
    if r.take(TEXTS_MAGIC.len())? != TEXTS_MAGIC {
        bail!("not the texts of an artifact");
    }
    let texts = (0..r.n()?).map(|_| r.n().and_then(|n| r.take(n)).map(<[u8]>::to_vec)).collect::<Result<_>>()?;
    if r.pos != bytes.len() {
        bail!("trailing bytes");
    }
    Ok(texts)
}

/// The rows and postings of the file at `rel` from its artifact: the path, the test flag and the
/// path field are this path's; the vectors are `vecs` when they fit, else made from the counts.
pub fn derive(rel: &str, found: Found, model: Option<&SemanticModel>) -> (Vec<NewRow>, FileLex, Vec<(i64, String)>) {
    let prof = crate::index::prof::enabled();
    let Found { art, texts, vecs } = found;
    let is_test = is_test_path(rel) as i64;
    let path_text = rel.replace(['/', '.', '_'], " ");
    let path_words = counts(&tokenize2(&path_text, None).0);
    let m = art.lexed();
    let vecs = match (model, vecs) {
        (Some(md), Some(v)) if v.q.len() == m * 2 * md.k && v.scale.len() == m * 2 => Some(v),
        _ => None,
    };
    let make = model.filter(|_| vecs.is_none());
    let mut made = QVecs::default();
    let mut lex = FileLex {
        dl: Vec::with_capacity(m),
        keys: (0..NF).map(|_| Vec::new()).collect(),
        tf: (0..NF).map(|_| Vec::new()).collect(),
        cnt: (0..NF).map(|_| Vec::with_capacity(m)).collect(),
        vec: None,
    };
    let mut rows = Vec::with_capacity(art.chunks.len());
    for (c, text) in art.chunks.into_iter().zip(texts) {
        let t = std::time::Instant::now();
        if c.lexed() {
            let mut total = 0u64;
            for (f, field) in c.fields.iter().enumerate() {
                let cap = (2..=4).contains(&f).then_some(TF_CAP);
                total += push_field(&mut lex, f, merged(&field.subs, &field.wholes), cap);
            }
            total += push_field(&mut lex, 5, path_words.iter().map(|(w, n)| (w.as_str(), *n)), None);
            lex.dl.push(total as f32);
        }
        if prof {
            crate::index::prof::add(&crate::index::prof::TOKENIZE, t);
        }
        let t = std::time::Instant::now();
        if let Some(md) = make.filter(|_| c.lexed()) {
            push_vectors(md, &c, &mut made);
        }
        if prof {
            crate::index::prof::add(&crate::index::prof::ENCODE, t);
        }
        let [_, _, _, doc, body] = &c.fields;
        let (ndoc, nbody) = (doc.len(), body.len());
        rows.push((
            ChunkRow {
                id: 0,
                path: rel.to_string(),
                name_key: name_key(&c.name),
                qual_key: name_key(&c.qualname),
                kind: c.kind,
                name: c.name,
                qualname: c.qualname,
                start: c.start,
                end: c.end,
                is_test,
                ndoc,
                nbody,
                text: String::new(),
                name_tok: c.name_tok,
            },
            c.calls,
            text,
            c.dup,
        ));
    }
    if model.is_some() && !rows.is_empty() {
        lex.vec = Some(vecs.unwrap_or(made));
    }
    (rows, lex, art.imports)
}

/// A field's words onto the postings of one chunk, capped at `cap` each; -> their total count.
fn push_field<'a>(lex: &mut FileLex, f: usize, words: impl Iterator<Item = (&'a str, u32)>, cap: Option<u32>) -> u64 {
    let (mut total, mut n) = (0u64, 0i32);
    for (w, c) in words {
        let c = cap.map_or(c, |cap| c.min(cap));
        lex.keys[f].push(tok_key(w));
        lex.tf[f].push(c.min(255) as u8);
        total += c as u64;
        n += 1;
    }
    lex.cnt[f].push(n);
    total
}

/// Two sorted word lists as one, counts of a word in both added.
fn merged<'a>(a: &'a [(String, u32)], b: &'a [(String, u32)]) -> impl Iterator<Item = (&'a str, u32)> {
    let (mut i, mut j) = (0, 0);
    std::iter::from_fn(move || {
        let next = match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x.0 == y.0 => {
                i += 1;
                j += 1;
                (x.0.as_str(), x.1 + y.1)
            }
            (Some(x), Some(y)) if x.0 < y.0 => {
                i += 1;
                (x.0.as_str(), x.1)
            }
            (_, Some(y)) => {
                j += 1;
                (y.0.as_str(), y.1)
            }
            (Some(x), None) => {
                i += 1;
                (x.0.as_str(), x.1)
            }
            (None, None) => return None,
        };
        Some(next)
    })
}

/// The (A view, B view) vector of one chunk from its word counts, quantized onto `vecs`.
fn push_vectors(m: &SemanticModel, c: &Chunk, vecs: &mut QVecs) {
    let k = m.k;
    let mut z = vec![0f32; 2 * k];
    let [name, qual, sig, doc, body] = &c.fields;
    if c.kind != "window" {
        let mut a: HashMap<&str, u32> = HashMap::new();
        for (w, n) in &name.subs {
            *a.entry(w.as_str()).or_insert(0) += 3 * n;
        }
        for (w, n) in qual.subs.iter().chain(&sig.subs).chain(&doc.subs) {
            *a.entry(w.as_str()).or_insert(0) += n;
        }
        if let Some(za) = m.encode_counts(&a, &HashMap::new()) {
            z[..k].copy_from_slice(&za);
        }
    }
    let b: HashMap<&str, u32> = body.subs.iter().map(|(w, n)| (w.as_str(), *n)).collect();
    if let Some(zb) = m.encode_counts(&HashMap::new(), &b) {
        z[k..].copy_from_slice(&zb);
    }
    let mut q = vec![0i8; 2 * k];
    let (qa, qb) = q.split_at_mut(k);
    vecs.scale.push(crate::lex::quantize(&z[..k], qa));
    vecs.scale.push(crate::lex::quantize(&z[k..], qb));
    vecs.q.extend(q);
}

fn put_n(w: &mut Vec<u8>, n: usize) {
    w.extend((n as u32).to_le_bytes());
}

fn put_str(w: &mut Vec<u8>, s: &str) {
    put_n(w, s.len());
    w.extend(s.as_bytes());
}

fn deflate(raw: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::with_capacity(raw.len() / 3 + 64), flate2::Compression::fast());
    e.write_all(raw).expect("in-memory deflate");
    e.finish().expect("in-memory deflate")
}

fn inflate(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(bytes).take(MAX_ITEMS as u64 * 8).read_to_end(&mut out)?;
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

    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn n(&mut self) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > MAX_ITEMS || n > self.b.len() - self.pos.min(self.b.len()) {
            bail!("a count larger than what follows");
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String> {
        let n = self.n()?;
        Ok(String::from_utf8(self.take(n)?.to_vec())?)
    }
}
