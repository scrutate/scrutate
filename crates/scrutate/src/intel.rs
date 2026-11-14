//! Engine B intelligence beyond ranked search: outlines, similar and duplicate code,
//! and tree-sitter structural queries.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use crate::fmt::{echo, shown};
use crate::index::{ChunkRow, Index};
use crate::search::{Hit, Searcher, CODE_KINDS};
use crate::text::{tokenize2, BODY_CAP};
use crate::tscache::Key;

/// The path and line a location names: `path:line`, or `path:start-end` as every answer prints
/// it, so a location copied from one answer can be handed to the next call.
pub fn location(target: &str) -> Option<(&str, i64)> {
    let (path, tail) = target.rsplit_once(':')?;
    let line = tail.split_once('-').map_or(tail, |(start, _)| start).parse().ok()?;
    Some((path.trim_start_matches("./"), line))
}

/// The chunk a target refers to: a location (a window's too, which has vectors like any chunk), a
/// qualified name, or a plain name (first definition).
pub fn locate(s: &Searcher, target: &str) -> Result<Option<ChunkRow>> {
    if let Some((path, line)) = location(target) {
        return s.ix.chunk_at(&crate::ops::repo_path(s, path), line);
    }
    let hits = s.find_symbol(target, 1)?;
    match hits.first() {
        Some(h) => s.ix.chunk(h.id),
        None => Ok(None),
    }
}

/// Chunks whose implementation vector is closest to the target's, with token-set Jaccard as a second opinion.
pub fn similar(s: &mut Searcher, target: &str, k: usize) -> Result<Vec<(Hit, f32, f32)>> {
    let Some(row) = locate(s, target)? else { bail!("no symbol or location matches {}", echo(target)) };
    if crate::parse::SYMBOL_ONLY.contains(&row.kind.as_str()) {
        bail!("{} is a {}: constants and fields have no implementation vector and are not compared", shown(&row.qualname), row.kind);
    }
    let (_, b) = s.ix.lex()?.vec_of(row.id)?.ok_or_else(|| anyhow!("the index has no vectors (semantic model disabled)"))?;
    if b.iter().all(|x| *x == 0.0) {
        bail!("{} has no implementation vector (its body is too short to compare)", shown(&row.qualname));
    }
    let near = s.ix.lex()?.scan_b(&b, k.saturating_mul(3), row.id)?;
    let my_tokens: HashSet<String> = tokenize2(&row.text, Some(BODY_CAP)).0.into_iter().collect();
    let mut out = Vec::new();
    for (id, cos) in near {
        let Some(r) = s.ix.chunk(id)? else { continue };
        if !CODE_KINDS.contains(&r.kind.as_str()) && r.kind != "module" {
            continue;
        }
        let toks: HashSet<String> = tokenize2(&r.text, Some(BODY_CAP)).0.into_iter().collect();
        let inter = my_tokens.intersection(&toks).count() as f32;
        let union = my_tokens.union(&toks).count().max(1) as f32;
        let cos = cos.clamp(-1.0, 1.0); // quantized storage can push a unit dot product a hair past 1
        let mut h = Hit::from_row(&r, true);
        h.score = cos as f64;
        out.push((h, cos, inter / union));
        if out.len() >= k {
            break;
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------------- duplicates (MinHash LSH)
const HASHES: usize = 64;
pub(crate) const BANDS: usize = 16; // 16 bands x 4 rows: pairs with Jaccard >= 0.7 collide with probability ~0.98

/// What `dupes` compares of one definition, made once from its text as the index stores it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DupKeys {
    /// the body below the signature line, runs of whitespace squeezed (`body_key`)
    pub body: u64,
    /// the body's word tokens in order: bodies that differ in operators, numbers or spacing
    pub words: u64,
    /// how many word tokens the body has, at most `BODY_CAP`
    pub ntok: u32,
    /// the MinHash band keys of the body's three-word shingles, when it has 8 or more
    pub bands: Option<[u64; BANDS]>,
}

impl DupKeys {
    pub fn of(text: &str) -> DupKeys {
        // the signature line differs between copies of one body
        let body = text.split_once('\n').map(|(_, b)| b).unwrap_or("");
        DupKeys::of_tokens(text, &tokenize2(body, Some(BODY_CAP)).0)
    }

    /// `of(text)`, given the words of the text below its signature line as `tokenize2` makes them.
    pub fn of_tokens(text: &str, toks: &[String]) -> DupKeys {
        let mut words = 1469598103934665603u64;
        for t in toks {
            for b in t.as_bytes() {
                words = (words ^ *b as u64).wrapping_mul(1099511628211);
            }
            words = (words ^ 0x2e).wrapping_mul(1099511628211);
        }
        let sh = shingles(toks);
        let bands = (sh.len() >= 8).then(|| band_keys(&minhash(&sh)));
        DupKeys { body: body_key(text), words, ntok: toks.len() as u32, bands }
    }
}

fn band_keys(mh: &[u64; HASHES]) -> [u64; BANDS] {
    let rows_per_band = HASHES / BANDS;
    std::array::from_fn(|b| mh[b * rows_per_band..(b + 1) * rows_per_band].iter().fold(0xcbf29ce484222325u64, |key, v| mix64(key ^ *v)))
}

fn mix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    x
}

fn shingles(tokens: &[String]) -> HashSet<u64> {
    let mut out = HashSet::new();
    if tokens.len() < 3 {
        return out;
    }
    for w in tokens.windows(3) {
        let mut h = 1469598103934665603u64;
        for t in w {
            for b in t.as_bytes() {
                h = (h ^ *b as u64).wrapping_mul(1099511628211);
            }
            h = (h ^ 0x1f).wrapping_mul(1099511628211);
        }
        out.insert(h);
    }
    out
}

fn minhash(sh: &HashSet<u64>) -> [u64; HASHES] {
    let mut mh = [u64::MAX; HASHES];
    for &s in sh {
        for (i, slot) in mh.iter_mut().enumerate() {
            let v = mix64(s ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15));
            if v < *slot {
                *slot = v;
            }
        }
    }
    mh
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DupGroup {
    pub kind: &'static str, // "exact" (same body text up to whitespace) or "near"
    pub similarity: f32,
    pub members: Vec<(String, String, i64, i64)>, // path, qualname, start, end
    pub tokens: usize,
}

/// Duplicate and near-duplicate functions: the same body text up to whitespace, then MinHash/LSH
/// candidates verified by Jaccard similarity of word 3-gram sets (>= `threshold`; 1 = exact only).
pub fn dupes(ix: &Index, min_tokens: usize, threshold: f32, limit: usize) -> Result<Vec<DupGroup>> {
    duplicates(ix, &KeyRows::load(ix, min_tokens)?, None, threshold, limit)
}

/// What `exact` duplicates share: the body below the signature line, runs of whitespace squeezed.
pub(crate) fn body_key(text: &str) -> u64 {
    let body = text.split_once('\n').map(|(_, b)| b).unwrap_or("");
    let mut seq = 1469598103934665603u64;
    for w in body.split_whitespace() {
        for b in w.as_bytes() {
            seq = (seq ^ *b as u64).wrapping_mul(1099511628211);
        }
        seq = (seq ^ 0x20).wrapping_mul(1099511628211);
    }
    seq
}

/// The keys of the index's definitions of `CODE_KINDS` with at least `min_tokens` body tokens, by
/// id: read once, for every search over them.
pub(crate) struct KeyRows {
    rows: Vec<(i64, DupKeys)>,
    min_tokens: usize,
}

impl KeyRows {
    pub(crate) fn load(ix: &Index, min_tokens: usize) -> Result<KeyRows> {
        let mut rows = Vec::new();
        ix.dup_keys(min_tokens, |id, k| rows.push((id, k)))?;
        Ok(KeyRows { rows, min_tokens })
    }
}

/// Where `duplicates` looks when it is not after every group.
pub(crate) struct Focus<'a> {
    /// the definitions of these paths are left out of the index's ...
    pub left_out: &'a HashSet<&'a str>,
    /// ... and these bodies put in after them, as `Index::bodies` gives them
    pub put_in: &'a [&'a crate::index::Body],
    /// the groups wanted share a key with a put-in body, with a definition of one of these paths,
    /// or with one of these keys
    pub paths: &'a HashSet<&'a str>,
    pub keys: &'a [DupKeys],
}

/// The keys a focus wants, each kind apart.
#[derive(Default)]
struct Wanted {
    bodies: HashSet<u64>,
    words: HashSet<u64>,
    bands: HashSet<(usize, u64)>,
}

impl Wanted {
    fn add(&mut self, k: &DupKeys) {
        self.bodies.insert(k.body);
        self.words.insert(k.words);
        self.bands.extend(k.bands.iter().flatten().copied().enumerate());
    }

    fn shares(&self, k: &DupKeys) -> bool {
        self.bodies.contains(&k.body) || self.words.contains(&k.words) || k.bands.iter().flatten().enumerate().any(|(b, key)| self.bands.contains(&(b, *key)))
    }
}

/// The groups `dupes_in` finds over the index's definitions of `CODE_KINDS` with at least
/// `min_tokens` body tokens, read from the keys the index keeps: a text is read only for a
/// definition in a group or in a bucket of 2 to 64. With `focus`, only the groups that share a key
/// with it, each whole: every definition with a wanted key is read, so no group is cut short.
pub(crate) fn duplicates(ix: &Index, keys: &KeyRows, focus: Option<&Focus>, threshold: f32, limit: usize) -> Result<Vec<DupGroup>> {
    let min_tokens = keys.min_tokens;
    let all = focus.is_none();
    let put: Vec<DupKeys> = focus.map(|f| f.put_in.iter().map(|b| DupKeys::of(&b.5)).collect()).unwrap_or_default();
    let (mut left, mut want) = (HashSet::new(), Wanted::default());
    if let Some(f) = focus {
        for p in f.left_out {
            left.extend(ix.chunk_ids(p)?);
        }
        let mut ids = Vec::new();
        for p in f.paths {
            ids.extend(ix.chunk_ids(p)?);
        }
        for k in f.keys.iter().chain(&put).chain(ix.dup_keys_of(&ids)?.iter().map(|(_, k)| k)) {
            want.add(k);
        }
    }
    // one pass over the keys: every definition that shares a wanted key, in id order, then the
    // put-in bodies, in the order `dupes_in` reads them (id 0 marks them)
    let mut rows: Vec<(i64, DupKeys)> = keys.rows.iter().filter(|(id, k)| !left.contains(id) && (all || want.shares(k))).copied().collect();
    let first_put = rows.len();
    rows.extend(put.iter().map(|k| (0, *k)));

    // a group is formed only from a key all of whose definitions were read: a wanted one, or the
    // body of a definition read for its wanted words (the same body has the same words)
    let mut exact: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut same_words: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut buckets: HashMap<(usize, u64), Vec<usize>> = HashMap::new();
    for (i, (_, k)) in rows.iter().enumerate() {
        let words_wanted = all || want.words.contains(&k.words);
        if k.ntok as usize >= min_tokens {
            if words_wanted || want.bodies.contains(&k.body) {
                exact.entry(k.body).or_default().push(i);
            }
            if words_wanted {
                same_words.entry(k.words).or_default().push(i);
            }
        }
        for (b, key) in k.bands.iter().flatten().enumerate() {
            if all || want.bands.contains(&(b, *key)) {
                buckets.entry((b, *key)).or_default().push(i);
            }
        }
    }
    let near_on = threshold < 1.0;
    // the place of each definition a group or a pair may hold, and the text of each a pair may hold
    let mut needed: HashSet<usize> = exact.values().chain(same_words.values()).filter(|m| m.len() > 1).flatten().copied().collect();
    let texts_needed: HashSet<usize> = buckets.values().filter(|m| near_on && m.len() > 1 && m.len() <= 64).flatten().copied().collect();
    needed.extend(&texts_needed);
    let ids: Vec<i64> = needed.iter().filter(|&&i| i < first_put).map(|&i| rows[i].0).collect();
    let mut place: HashMap<usize, (String, String, i64, i64, String)> = HashMap::new();
    let at: HashMap<i64, usize> = needed.iter().filter(|&&i| i < first_put).map(|&i| (rows[i].0, i)).collect();
    for (id, path, qual, start, end, text) in ix.bodies_by_id(&ids)? {
        if let Some(&i) = at.get(&id) {
            place.insert(i, (path, qual, start, end, text));
        }
    }
    for (k, b) in focus.map(|f| f.put_in).unwrap_or_default().iter().enumerate() {
        place.insert(first_put + k, (b.1.clone(), b.2.clone(), b.3, b.4, b.5.clone()));
    }
    let member = |i: usize| {
        let p = &place[&i];
        (p.0.clone(), p.1.clone(), p.2, p.3)
    };
    let overlap = |a: usize, b: usize| {
        let (x, y) = (&place[&a], &place[&b]);
        x.0 == y.0 && x.2 <= y.3 && y.2 <= x.3
    };

    let mut out: Vec<DupGroup> = Vec::new();
    let mut in_exact: HashMap<usize, u64> = HashMap::new();
    for (&key, members) in exact.iter().filter(|(_, m)| m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort();
        ms.dedup_by(|b, a| overlap(*a, *b));
        if ms.len() < 2 {
            continue;
        }
        in_exact.extend(ms.iter().map(|&i| (i, key)));
        out.push(DupGroup { kind: "exact", similarity: 1.0, tokens: rows[ms[0]].1.ntok as usize, members: ms.iter().map(|&i| member(i)).collect() });
    }
    for members in same_words.values().filter(|m| near_on && m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort();
        ms.dedup_by(|b, a| overlap(*a, *b));
        if ms.len() < 2 || ms.iter().all(|i| in_exact.contains_key(i) && in_exact.get(i) == in_exact.get(&ms[0])) {
            continue;
        }
        out.push(DupGroup { kind: "near", similarity: 1.0, tokens: rows[ms[0]].1.ntok as usize, members: ms.iter().map(|&i| member(i)).collect() });
    }
    let sets: HashMap<usize, HashSet<u64>> = texts_needed
        .iter()
        .map(|&i| {
            let text = &place[&i].4;
            let body = text.split_once('\n').map(|(_, b)| b).unwrap_or("");
            (i, shingles(&tokenize2(body, Some(BODY_CAP)).0))
        })
        .collect();
    let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut near: Vec<(f32, usize, usize)> = Vec::new();
    for members in buckets.values().filter(|m| near_on && m.len() > 1 && m.len() <= 64) {
        for a in 0..members.len() {
            for b in a + 1..members.len() {
                let (i, j) = (members[a].min(members[b]), members[a].max(members[b]));
                if i == j || rows[i].1.words == rows[j].1.words || !seen_pairs.insert((i, j)) {
                    continue;
                }
                // a container and the symbol nested inside it share text: not a duplicate
                if overlap(i, j) {
                    continue;
                }
                let inter = sets[&i].intersection(&sets[&j]).count() as f32;
                let union = sets[&i].union(&sets[&j]).count().max(1) as f32;
                let jac = inter / union;
                if jac >= threshold {
                    near.push((jac, i, j));
                }
            }
        }
    }
    near.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (jac, i, j) in near.into_iter().take(limit) {
        let tokens = (rows[i].1.ntok as usize).min(rows[j].1.ntok as usize);
        out.push(DupGroup { kind: "near", similarity: jac, tokens, members: vec![member(i), member(j)] });
    }
    // members last: the exact groups come from a hash map, and a cut must not depend on its order
    out.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(b.similarity.partial_cmp(&a.similarity).unwrap()).then_with(|| a.members.cmp(&b.members)));
    out.truncate(limit);
    Ok(out)
}

/// `dupes` over these definitions, each text read whole: the search `duplicates` replaced, kept
/// as the oracle its tests compare with.
#[cfg(test)]
pub(crate) fn dupes_in(rows: &[&crate::index::Body], min_tokens: usize, threshold: f32, limit: usize) -> Vec<DupGroup> {
    let mut exact: HashMap<u64, Vec<usize>> = HashMap::new();
    // the same words in the same order: bodies that differ in operators, numbers or spacing
    let mut same_words: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut word_key: Vec<u64> = Vec::with_capacity(rows.len());
    let mut sets: Vec<HashSet<u64>> = Vec::with_capacity(rows.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(rows.len());
    let mut buckets: HashMap<(usize, u64), Vec<usize>> = HashMap::new();
    for (i, (_, _, _, _, _, text)) in rows.iter().enumerate() {
        // skip the signature line: duplicates differ in name but not in body
        let body = text.split_once('\n').map(|(_, b)| b).unwrap_or("");
        let toks = tokenize2(body, Some(BODY_CAP)).0;
        sizes.push(toks.len());
        // exact: the same body text up to whitespace (the word tokens drop operators and numbers)
        let seq = body_key(text);
        let mut words = 1469598103934665603u64;
        for t in &toks {
            for b in t.as_bytes() {
                words = (words ^ *b as u64).wrapping_mul(1099511628211);
            }
            words = (words ^ 0x2e).wrapping_mul(1099511628211);
        }
        word_key.push(words);
        if toks.len() >= min_tokens {
            exact.entry(seq).or_default().push(i);
            same_words.entry(words).or_default().push(i);
        }
        let sh = shingles(&toks);
        if sh.len() >= 8 {
            let mh = minhash(&sh);
            let rows_per_band = HASHES / BANDS;
            for b in 0..BANDS {
                let mut key = 0xcbf29ce484222325u64;
                for v in &mh[b * rows_per_band..(b + 1) * rows_per_band] {
                    key = mix64(key ^ *v);
                }
                buckets.entry((b, key)).or_default().push(i);
            }
        }
        sets.push(sh);
    }
    let mut out: Vec<DupGroup> = Vec::new();
    let mut in_exact: HashMap<usize, u64> = HashMap::new();
    for (&key, members) in exact.iter().filter(|(_, m)| m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort();
        ms.dedup_by(|b, a| rows[*a].1 == rows[*b].1 && rows[*a].3 <= rows[*b].4 && rows[*b].3 <= rows[*a].4);
        if ms.len() < 2 {
            continue;
        }
        in_exact.extend(ms.iter().map(|&i| (i, key)));
        out.push(DupGroup {
            kind: "exact",
            similarity: 1.0,
            tokens: sizes[ms[0]],
            members: ms.iter().map(|&i| (rows[i].1.clone(), rows[i].2.clone(), rows[i].3, rows[i].4)).collect(),
        });
    }
    // a threshold of 1 asks for exact duplicates only
    for members in same_words.values().filter(|m| threshold < 1.0 && m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort();
        ms.dedup_by(|b, a| rows[*a].1 == rows[*b].1 && rows[*a].3 <= rows[*b].4 && rows[*b].3 <= rows[*a].4);
        // one text for all of them is the exact group above
        if ms.len() < 2 || ms.iter().all(|i| in_exact.contains_key(i) && in_exact.get(i) == in_exact.get(&ms[0])) {
            continue;
        }
        out.push(DupGroup {
            kind: "near",
            similarity: 1.0,
            tokens: sizes[ms[0]],
            members: ms.iter().map(|&i| (rows[i].1.clone(), rows[i].2.clone(), rows[i].3, rows[i].4)).collect(),
        });
    }
    let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut near: Vec<(f32, usize, usize)> = Vec::new();
    for (_, members) in buckets.iter().filter(|(_, m)| threshold < 1.0 && m.len() > 1 && m.len() <= 64) {
        for a in 0..members.len() {
            for b in a + 1..members.len() {
                let (i, j) = (members[a].min(members[b]), members[a].max(members[b]));
                if i == j || word_key[i] == word_key[j] || !seen_pairs.insert((i, j)) {
                    continue;
                }
                // a container and the symbol nested inside it share text: not a duplicate
                if rows[i].1 == rows[j].1 && rows[i].3 <= rows[j].4 && rows[j].3 <= rows[i].4 {
                    continue;
                }
                let inter = sets[i].intersection(&sets[j]).count() as f32;
                let union = sets[i].union(&sets[j]).count().max(1) as f32;
                let jac = inter / union;
                if jac >= threshold {
                    near.push((jac, i, j));
                }
            }
        }
    }
    near.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (jac, i, j) in near.into_iter().take(limit) {
        out.push(DupGroup {
            kind: "near",
            similarity: jac,
            tokens: sizes[i].min(sizes[j]),
            members: vec![(rows[i].1.clone(), rows[i].2.clone(), rows[i].3, rows[i].4), (rows[j].1.clone(), rows[j].2.clone(), rows[j].3, rows[j].4)],
        });
    }
    // members last: the exact groups come from a hash map, and a cut must not depend on its order
    out.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(b.similarity.partial_cmp(&a.similarity).unwrap()).then_with(|| a.members.cmp(&b.members)));
    out.truncate(limit);
    out
}

// ------------------------------------------------------------------------- structural queries
#[derive(Debug, Clone, serde::Serialize)]
pub struct Capture {
    pub path: String,
    pub line: i64,
    pub column: i64,
    pub capture: String,
    pub kind: String,
    pub text: String,
}

/// Run a tree-sitter query (S-expression pattern language) over the files of one language under
/// `roots`, delivering every capture to `on_capture` in path order. No index is needed; files are
/// parsed in parallel in bounded chunks, so memory stays flat however many matches there are.
/// With `cache_root`, each file's captures are kept under `cache_root/.scrutate/tsquery/` by
/// content hash and files seen before are not parsed again (see `tscache`); the output is the same.
/// With `as_given`, paths are printed as `grep PATH...` prints them (the root as given, then the
/// path below it), except under `.`; without, from the root that holds them.
/// Returns the number of captures delivered (stops at `limit`).
/// Each searched path from `top` ("" for `top` itself), or as given when it lies outside.
fn scopes(top: &Path, roots: &[PathBuf]) -> Vec<String> {
    let top = std::fs::canonicalize(top).ok();
    let from_top = |r: &PathBuf| Some(crate::instant::slashed(std::fs::canonicalize(r).ok()?.strip_prefix(top.as_ref()?).ok()?.to_string_lossy()));
    roots.iter().map(|r| from_top(r).unwrap_or_else(|| r.to_string_lossy().into_owned())).collect()
}

/// A tree-sitter query compiled for `lang`, refused when it is empty, has no capture or uses a
/// predicate tree-sitter does not evaluate.
pub(crate) fn compile_query(lang: &str, query: &str) -> Result<(&'static crate::langs::LangSpec, tree_sitter::Language, tree_sitter::Query)> {
    let spec = crate::langs::by_id(lang).ok_or_else(|| anyhow!("unknown language {} (see `scrutate languages`)", echo(lang)))?;
    if query.trim().is_empty() {
        bail!("empty query: give a tree-sitter pattern with a @capture, such as (identifier) @name");
    }
    if let Some(line) = loose_predicate(query) {
        bail!(
            "a predicate on line {line} stands outside every pattern, so tree-sitter ignores it and the pattern matches every node: put it inside the pattern's parentheses, as in ((identifier) @i (#eq? @i \"x\"))"
        );
    }
    let language = (spec.language)();
    let q = tree_sitter::Query::new(&language, query).map_err(|e| anyhow!("bad query: {e}"))?;
    if q.capture_names().is_empty() {
        bail!("the query has no @capture, so it can match nothing to report: add one, such as (identifier) @name");
    }
    // predicates tree-sitter does not evaluate itself would silently match everything
    if let Some(p) = (0..q.pattern_count()).flat_map(|i| q.general_predicates(i)).next() {
        bail!("unknown predicate #{} (supported: #eq?, #match?, #any-of? and their not- and any- forms)", p.operator);
    }
    Ok((spec, language, q))
}

/// The line of a predicate written at the top level of `query`, outside every pattern's
/// parentheses: tree-sitter accepts it and applies it to nothing.
fn loose_predicate(query: &str) -> Option<usize> {
    let (mut depth, mut line) = (0usize, 1usize);
    let mut chars = query.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => line += 1,
            ';' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                        break;
                    }
                }
            }
            '"' => {
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        '\n' => line += 1,
                        _ => {}
                    }
                }
            }
            '(' | '[' => {
                if depth == 0 && c == '(' {
                    let mut ahead = chars.clone();
                    while ahead.peek().is_some_and(|c| c.is_whitespace()) {
                        ahead.next();
                    }
                    if ahead.peek() == Some(&'#') {
                        return Some(line);
                    }
                }
                depth += 1;
            }
            ')' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod duplicates_tests {
    use super::*;
    use proptest::prelude::*;
    use std::path::PathBuf;

    const SEEDS: [&str; 4] = ["alpha", "beta", "gamma", "delta"];

    /// A function body long enough for `dupes`, its words from `seed`, changed as `how` says.
    fn function(name: &str, seed: &str, how: u8, indent: &str) -> String {
        let lines = [
            "total = 0".to_string(),
            "for row in rows:".to_string(),
            format!("    if row.amount > {seed}_limit and row.status == \"open\":"),
            format!("        total += compute_{seed}_weight(row.amount, row.discount, row.region)"),
            "    elif row.status == \"closed\":".to_string(),
            format!("        total -= penalty_for_{seed}(row.region, row.amount, row.owner)"),
            "    else:".to_string(),
            format!("        total = adjust_{seed}(total, row.created, row.updated)"),
            format!("return normalize_{seed}_total(total, rows, scale_factor, offset_value, rounding_mode)"),
        ];
        let body: Vec<String> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| match how {
                // spacing only: the same text up to whitespace
                1 => l.replace(" = ", "  =  "),
                // other operators: the same words
                2 if i == 3 => l.replace("+=", "*="),
                // one line of other words: near
                3 if i == 5 => l.replace("row.owner", "row.manager_of_the_region"),
                // half the lines of other words
                4 if i >= 4 => format!("{l}  # unrelated_{i}_{seed}_words here"),
                _ => l.clone(),
            })
            .collect();
        let short = if how == 5 { "    return rows\n".to_string() } else { body.iter().map(|l| format!("{indent}    {l}\n")).collect() };
        format!("{indent}def {name}(rows):\n{short}\n")
    }

    pub(crate) type Spec = Vec<Vec<(usize, u8, bool)>>;

    /// Files of functions, some wrapped in a class (a container whose text holds its method's).
    pub(crate) fn files() -> impl Strategy<Value = Vec<Vec<(usize, u8, bool)>>> {
        prop::collection::vec(prop::collection::vec((0..SEEDS.len(), 0u8..6, any::<bool>()), 1..6), 1..5)
    }

    /// The files of a tree: their names and texts.
    pub(crate) fn tree(spec: &[Vec<(usize, u8, bool)>]) -> Vec<(String, String)> {
        let mut n = 0;
        let mut out = Vec::new();
        for (f, defs) in spec.iter().enumerate() {
            let mut text = String::new();
            for &(seed, how, wrap) in defs {
                n += 1;
                if wrap {
                    text.push_str(&format!("class Holder{n}:\n{}", function(&format!("method_{n}"), SEEDS[seed], how, "    ")));
                } else {
                    text.push_str(&function(&format!("fn_{n}"), SEEDS[seed], how, ""));
                }
            }
            out.push((format!("m{f}.py"), text));
        }
        out
    }

    fn write(root: &Path, spec: &[Vec<(usize, u8, bool)>]) {
        let _ = std::fs::remove_dir_all(root);
        std::fs::create_dir_all(root).unwrap();
        for (name, text) in tree(spec) {
            std::fs::write(root.join(name), text).unwrap();
        }
    }

    fn agree(root: &Path) -> std::result::Result<(), TestCaseError> {
        let mut ix = Index::open(root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        for (min, threshold, limit) in [(30, 0.75, 50), (8, 0.5, 3), (5, 1.0, 1000), (30, 0.75, usize::MAX)] {
            let rows = ix.bodies(CODE_KINDS, min).unwrap();
            let want = dupes_in(&rows.iter().collect::<Vec<_>>(), min, threshold, limit);
            prop_assert_eq!(
                duplicates(&ix, &KeyRows::load(&ix, min).unwrap(), None, threshold, limit).unwrap(),
                want,
                "min {} threshold {} limit {}",
                min,
                threshold,
                limit
            );
        }
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, failure_persistence: None, ..ProptestConfig::default() })]
        #[test]
        fn every_group_duplicates_finds_is_the_one_dupes_in_finds(spec in files()) {
            let root = std::env::temp_dir().join(format!("scrutate_duplicates_{}", std::process::id()));
            write(&root, &spec);
            agree(&root)?;
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn a_tree_with_every_kind_of_group_agrees() {
        let root = std::env::temp_dir().join(format!("scrutate_duplicates_kinds_{}", std::process::id()));
        let spec = vec![
            vec![(0, 0, false), (0, 0, false), (0, 1, false), (0, 2, false), (0, 3, false)],
            vec![(1, 0, true), (1, 0, false), (2, 4, false), (2, 0, false), (3, 5, false)],
        ];
        write(&root, &spec);
        let mut ix = Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let rows = ix.bodies(CODE_KINDS, 30).unwrap();
        let groups = dupes_in(&rows.iter().collect::<Vec<_>>(), 30, 0.5, usize::MAX);
        // the tree holds an exact group, a group of the same words and a near pair
        assert!(groups.iter().any(|g| g.kind == "exact"), "{groups:?}");
        assert!(groups.iter().any(|g| g.kind == "near" && g.similarity == 1.0), "{groups:?}");
        assert!(groups.iter().any(|g| g.kind == "near" && g.similarity < 1.0), "{groups:?}");
        drop(ix);
        agree(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn on_this_crate_s_own_source_duplicates_finds_what_dupes_in_finds() {
        let root = std::env::temp_dir().join(format!("scrutate_duplicates_self_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(&src).unwrap().flatten() {
            if entry.path().extension().is_some_and(|e| e == "rs") {
                std::fs::create_dir_all(&root).unwrap();
                std::fs::copy(entry.path(), root.join(entry.file_name())).unwrap();
            }
        }
        agree(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod dup_keys_tests {
    use super::*;

    fn body(seed: &str, op: &str) -> String {
        let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "theta", "kappa", "lambda", "sigma", "omega", "rho"];
        words.iter().map(|w| format!("    total_{seed} = total_{seed} {op} weight_{seed}_{w}(row.amount, row.region)\n")).collect()
    }

    #[test]
    fn the_keys_of_a_body_ignore_its_name_and_spacing_and_tell_its_words_from_its_text() {
        let f = |sig: &str, b: &str| DupKeys::of(&format!("{sig}\n{b}"));
        let a = f("def first(rows):", &body("x", "+"));
        // another name and other spacing: the same body
        let renamed = f("def second(items, more):", &body("x", "+").replace("    ", "\t  "));
        assert_eq!(a, renamed);
        // other operators, the same words in the same order
        let other_op = f("def first(rows):", &body("x", "-"));
        assert_ne!(a.body, other_op.body);
        assert_eq!((a.words, a.ntok, a.bands), (other_op.words, other_op.ntok, other_op.bands));
        // other words
        let other = f("def first(rows):", &body("y", "+"));
        assert_ne!((a.body, a.words), (other.body, other.words));
        assert!(a.ntok >= 30 && a.bands.is_some());
        // under 8 three-word shingles there are no bands
        let short = f("def tiny():", "    return a + b + c\n");
        assert_eq!(short.bands, None);
        assert_eq!(f("def nothing():", ""), DupKeys::of("def nothing():"));
    }
}

#[cfg(test)]
mod loose_predicate_tests {
    #[test]
    fn a_predicate_outside_every_pattern_is_found_by_its_line() {
        use super::loose_predicate;
        assert_eq!(loose_predicate(r#"(identifier) @i (#eq? @i "r")"#), Some(1));
        assert_eq!(loose_predicate("(call) @c\n( #match? @c \"x\")"), Some(2));
        assert_eq!(loose_predicate(r#"[(a) (b)] @x (#eq? @x "y")"#), Some(1));
        assert_eq!(loose_predicate(r#"((identifier) @i (#eq? @i "r"))"#), None);
        assert_eq!(loose_predicate(r#"(call function: (identifier) @f (#eq? @f "print")) @m"#), None);
        assert_eq!(loose_predicate("; (#eq? not code)\n(identifier) @i"), None);
        assert_eq!(loose_predicate(r#"((string) @s (#eq? @s "(#x"))"#), None);
    }
}

/// A file a structural query reads.
pub(crate) struct QueryFile {
    pub path: PathBuf,
    /// from its root
    pub rel: String,
    pub shown: String,
    pub md: std::fs::Metadata,
    /// which of the roots holds it
    pub root: usize,
}

/// The files of up to 1 MB under `roots` that `spec` may hold by name, in path order, as the
/// walker gives them (ignore files read, hidden files and links left out).
pub(crate) fn query_files(spec: &crate::langs::LangSpec, roots: &[PathBuf], as_given: bool) -> Vec<QueryFile> {
    let mut files = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        let mut wb = ignore::WalkBuilder::new(root);
        wb.hidden(true).git_ignore(true).add_custom_ignore_filename(crate::instant::RGIGNORE).sort_by_file_path(|a, b| a.cmp(b));
        for entry in wb.build().flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let Some(md) = entry.metadata().ok().filter(|m| m.len() <= crate::index::MAX_FILE_BYTES) else { continue };
            let path = entry.path();
            let rel = crate::instant::slashed(path.strip_prefix(root).unwrap_or(path).to_string_lossy());
            // a root that is the file itself: report it as given (the extension check needs a name)
            let rel = if rel.is_empty() { crate::instant::slashed(root.to_string_lossy()) } else { rel };
            if !crate::parse::is_indexable(&rel) {
                continue;
            }
            let ext_ok = spec.exts.iter().any(|e| rel.ends_with(e)) || spec.files.iter().any(|f| rel.rsplit('/').next() == Some(f));
            if ext_ok || spec.exts.is_empty() {
                let shown = if as_given && root.components().any(|c| c != std::path::Component::CurDir) {
                    crate::instant::slashed(path.to_string_lossy())
                } else {
                    rel.clone()
                };
                files.push(QueryFile { path: path.to_path_buf(), rel, shown, md, root: i });
            }
        }
    }
    files
}

#[allow(clippy::too_many_arguments)]
pub fn tsquery_each(
    lang: &str,
    query: &str,
    roots: &[PathBuf],
    as_given: bool,
    limit: usize,
    cache_root: Option<&Path>,
    watch: Watch,
    on_capture: &mut dyn FnMut(Capture),
) -> Result<usize> {
    use rayon::prelude::*;
    if cfg!(target_os = "wasi") {
        bail!("tsquery is not in the wasi build: it needs threads");
    }
    let (spec, language, q) = compile_query(lang, query)?;
    let names: Vec<String> = q.capture_names().iter().map(|n| n.to_string()).collect();
    // the code index's record saves reading a file whose captures are cached
    let recorded = cache_root.map(crate::stores::recorded).unwrap_or_default();
    let top = cache_root.filter(|_| !recorded.is_empty()).and_then(|r| std::fs::canonicalize(r).ok());
    let under: Vec<Option<String>> = roots
        .iter()
        .map(|root| top.as_ref().and_then(|t| std::fs::canonicalize(root).ok()?.strip_prefix(t).ok().map(|p| crate::instant::slashed(p.to_string_lossy()))))
        .collect();
    // (path, path from its root, path shown, the code index's hash of it when its stat is current)
    let files: Vec<(PathBuf, String, String, Option<String>)> = query_files(spec, roots, as_given)
        .into_iter()
        .map(|f| {
            let known = under[f.root].as_ref().and_then(|u| {
                let (stat, hash) = recorded.get(&if u.is_empty() { f.rel.clone() } else { format!("{u}/{}", f.rel) })?;
                (crate::index::Stat::of(&f.path, &f.md) == *stat).then(|| hash.clone())
            });
            (f.path, f.rel, f.shown, known)
        })
        .collect();
    let cache = cache_root.map(|r| crate::tscache::Cache::open(r, lang, query, &scopes(r, roots)));
    watch.count.total.fetch_add(files.len(), std::sync::atomic::Ordering::Relaxed);
    // parse in parallel, deliver in path order: workers send (file index, content hash, captures,
    // complete) through a bounded channel; the consumer reorders with a small buffer, so memory
    // stays flat and no thread waits at chunk barriers
    let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, Option<Key>, Vec<Capture>, bool)>(512);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut fresh: HashMap<Key, Vec<Capture>> = HashMap::new();
    let (mut misses, mut fresh_bytes, mut finished) = (0usize, 0u64, true);
    let delivered = std::thread::scope(|scope| -> usize {
        let files_ref = &files;
        let q_ref = &q;
        let names_ref = &names;
        let language_ref = &language;
        let stop_ref = &stop;
        let cached = cache.as_ref().map(|c| &c.entries);
        scope.spawn(move || {
            files_ref.par_iter().enumerate().for_each_init(
                || {
                    let mut parser = tree_sitter::Parser::new();
                    let ok = parser.set_language(language_ref).is_ok();
                    (parser, ok, tx.clone())
                },
                |(parser, ok, tx), (i, (path, rel, shown, known))| {
                    if stop_ref.load(std::sync::atomic::Ordering::Relaxed) || watch.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    watch.count.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut out = Vec::new();
                    let mut key = None;
                    let mut complete = true;
                    let hit = |key: Option<Key>| {
                        key.and_then(|k| cached.and_then(|c| c.get(&k))).map(|h| h.iter().map(|c| Capture { path: shown.clone(), ..c.clone() }).collect())
                    };
                    // an entry is stored only for content of this language, so a hit needs no read
                    let k = known.as_deref().and_then(|h| cached.map(|_| crate::tscache::key(h, rel)));
                    if let Some(caps) = hit(k) {
                        let _ = tx.send((i, k, caps, true));
                        return;
                    }
                    if *ok {
                        let raw = std::fs::read(path).ok();
                        if let Some((raw, src)) = raw.as_deref().and_then(|raw| Some((raw, crate::index::decode_source(raw)?.into_owned()))) {
                            if crate::parse::spec_for(rel, &src).map(|s| s.id) == Some(spec.id) {
                                key = cached.map(|_| crate::tscache::key(&crate::index::content_hash(raw), rel));
                                if let Some(caps) = hit(key) {
                                    let _ = tx.send((i, key, caps, true));
                                    return;
                                }
                                if let Some(tree) = parser.parse(src.as_bytes(), None) {
                                    let mut cursor = tree_sitter::QueryCursor::new();
                                    cursor.set_match_limit(1 << 16);
                                    let mut matches = cursor.matches(q_ref, tree.root_node(), src.as_bytes());
                                    use tree_sitter::StreamingIterator;
                                    while let Some(m) = matches.next() {
                                        for c in m.captures {
                                            let node = c.node;
                                            let text = node.utf8_text(src.as_bytes()).unwrap_or("").lines().next().unwrap_or("").to_string();
                                            out.push(Capture {
                                                path: shown.clone(),
                                                line: node.start_position().row as i64 + 1,
                                                column: node.start_position().column as i64 + 1,
                                                capture: names_ref[c.index as usize].clone(),
                                                kind: node.kind().to_string(),
                                                text,
                                            });
                                        }
                                        if out.len() >= limit {
                                            complete = false; // a cut-off list is not the file's result
                                            break;
                                        }
                                    }
                                } else {
                                    complete = false;
                                }
                            }
                        }
                    }
                    let _ = tx.send((i, key, out, complete));
                },
            );
        });
        let mut next = 0usize;
        let mut pending: std::collections::BTreeMap<usize, (Option<Key>, Vec<Capture>, bool)> = std::collections::BTreeMap::new();
        let mut delivered = 0usize;
        for (i, key, caps, complete) in rx {
            pending.insert(i, (key, caps, complete));
            while let Some((key, caps, complete)) = pending.remove(&next) {
                next += 1;
                if let Some(k) = key {
                    if !complete {
                        finished = false;
                    } else if fresh_bytes <= crate::tscache::MAX_BYTES {
                        misses += usize::from(!cached.is_some_and(|c| c.contains_key(&k)));
                        fresh_bytes += 32 + caps.iter().map(|c| 32 + c.capture.len() + c.kind.len() + c.text.len()).sum::<usize>() as u64;
                        fresh.entry(k).or_insert_with(|| caps.iter().map(|c| Capture { path: String::new(), ..c.clone() }).collect());
                    }
                }
                for cap in caps {
                    if delivered >= limit {
                        stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    on_capture(cap);
                    delivered += 1;
                }
            }
            if delivered >= limit {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
        finished = finished && next == files_ref.len();
        delivered
    });
    if watch.cancel.load(std::sync::atomic::Ordering::Relaxed) {
        bail!("cancelled");
    }
    if let Some(mut cache) = cache {
        if fresh_bytes > crate::tscache::MAX_BYTES {
            return Ok(delivered); // too large to cache: the stored entries stay as they were
        }
        let stale = finished && fresh.len() != cache.entries.len();
        if !finished {
            // files this run did not reach keep their entries
            for (k, v) in cache.entries.drain() {
                fresh.entry(k).or_insert(v);
            }
        }
        if misses > 0 || stale {
            let _ = cache.store(&fresh);
        } else {
            cache.touch();
        }
    }
    Ok(delivered)
}

/// What a long search reports to and is stopped by: the files it read, and the call's cancel flag.
#[derive(Clone, Copy)]
pub struct Watch<'a> {
    pub count: &'a crate::index::FileCount,
    pub cancel: &'a std::sync::atomic::AtomicBool,
}

/// `tsquery_each` collected into a vector (MCP and bindings; the CLI streams).
pub fn tsquery(lang: &str, query: &str, roots: &[PathBuf], limit: usize, cache_root: Option<&Path>, watch: Watch) -> Result<Vec<Capture>> {
    let mut out = Vec::new();
    tsquery_each(lang, query, roots, false, limit, cache_root, watch, &mut |c| out.push(c))?;
    Ok(out)
}

/// One group of identifier occurrences inside the same enclosing symbol (or the same file when none).
#[derive(Debug, Clone)]
pub struct RefGroup {
    pub symbol: Option<ChunkRow>,
    pub path: String,
    pub lines: Vec<crate::instant::Line>,
    /// a note on the file after its last group: a NUL byte stopped its search
    pub note: Option<String>,
}

pub struct References {
    pub definitions: Vec<ChunkRow>,
    pub callers: Vec<Hit>,
    pub groups: Vec<RefGroup>,
    pub truncated: bool,
}

impl References {
    pub fn total_lines(&self) -> usize {
        self.groups.iter().map(|g| g.lines.len()).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty() && self.callers.is_empty() && self.groups.is_empty()
    }
}

/// Engine A matches and notes in the files a search of the repository covers
/// (`files::repository_files`), in path and line order. It stops when `cancel` is set; the flag
/// returned says the search stopped early.
pub fn occurrences(
    root: &std::path::Path,
    q: &crate::instant::Query,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(Vec<crate::instant::Record>, bool)> {
    occurrences_in(root, &crate::files::repository_files(root), q, cancel)
}

/// `occurrences` in the file set `files` of the repository at `root`; a walk of paths as given
/// (a directory of the repository), and a list of files of a directory of it, name their files
/// from `root`, as the index does.
pub fn occurrences_in(
    root: &std::path::Path,
    files: &crate::instant::Files,
    q: &crate::instant::Query,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(Vec<crate::instant::Record>, bool)> {
    use crate::instant::Record;
    let mut found: Vec<Record> = Vec::new();
    // on Windows Engine A prints a verbatim root (`\\?\C:\x`) in its plain form (`C:/x`)
    let printed_root = format!("{}/", crate::instant::slashed(root.to_string_lossy()));
    let from_root = |p: &mut std::sync::Arc<str>| {
        if let Ok(rel) = std::path::Path::new(&**p).strip_prefix(root) {
            *p = crate::instant::slashed(rel.to_string_lossy()).into();
        } else if let Some(rel) = p.strip_prefix(printed_root.as_str()) {
            *p = rel.into();
        }
    };
    let under = match &files.paths {
        crate::instant::Paths::List { root: at, .. } if at != root => {
            at.strip_prefix(root).ok().map(|d| format!("{}/", crate::instant::slashed(d.to_string_lossy())))
        }
        _ => None,
    };
    let below = |p: &mut std::sync::Arc<str>| {
        if let Some(d) = &under {
            *p = format!("{d}{p}").into();
        }
    };
    let mut sink = |mut r: Record| {
        if files.paths_as_given || under.is_some() {
            match &mut r {
                Record::Match(h) => {
                    from_root(&mut h.path);
                    below(&mut h.path)
                }
                Record::Note { path, .. } => {
                    from_root(path);
                    below(path)
                }
                _ => {}
            }
        }
        if matches!(r, Record::Match(_) | Record::Note { .. }) {
            found.push(r)
        }
    };
    let stats = crate::instant::search(q, files, std::sync::Arc::clone(cancel), crate::instant::Output::Records(&mut sink))?;
    found.sort_by(|a, b| a.path().cmp(b.path()).then(a.order().cmp(&b.order())));
    Ok((found, stats.cancelled))
}

/// Matching lines (in path and line order) grouped by the symbol that encloses them; lines outside
/// any symbol form one group per file. A note on a file goes with its last group.
pub fn group_by_symbol(ix: &Index, records: Vec<crate::instant::Record>) -> Result<Vec<RefGroup>> {
    use crate::instant::Record;
    let mut groups: Vec<RefGroup> = Vec::new();
    for r in records {
        let h = match r {
            Record::Match(h) => h,
            Record::Note { path, text, .. } => {
                match groups.last_mut() {
                    Some(g) if *g.path == *path => g.note = Some(text),
                    _ => groups.push(RefGroup { symbol: None, path: path.to_string(), lines: Vec::new(), note: Some(text) }),
                }
                continue;
            }
            _ => continue,
        };
        let row = ix.chunk_at(&h.path, h.line as i64)?.filter(|r| r.kind != "window");
        match groups.last_mut() {
            Some(g) if g.symbol.as_ref().map(|x| x.id) == row.as_ref().map(|x| x.id) && *g.path == *h.path => g.lines.push(h),
            _ => groups.push(RefGroup { symbol: row, path: h.path.to_string(), lines: vec![h], note: None }),
        }
    }
    Ok(groups)
}

/// An Engine A search of the repository with its matches grouped by enclosing symbol: the grep an
/// agent asks for (MCP `grep_code`). It stops when the call is cancelled.
/// The text index, when there is one, says which files can hold no match (`stores::grep_files`).
pub fn grouped_grep(s: &Searcher, q: &crate::instant::Query, files: &crate::instant::Files) -> Result<(Vec<RefGroup>, bool)> {
    let narrowed = crate::stores::grep_files(&s.ix, q, files);
    let (found, truncated) = occurrences_in(&s.ix.root, narrowed.as_ref().unwrap_or(files), q, &s.cancel)?;
    Ok((group_by_symbol(&s.ix, found)?, truncated))
}

/// One line per group and one per matching line under it.
pub fn groups_text(groups: &[RefGroup], out: &mut String) {
    for g in groups {
        out.push_str(&group_text(g));
        out.push('\n');
    }
}

/// A group as text: its symbol, then its matching lines.
pub fn group_text(g: &RefGroup) -> String {
    let mut out = match &g.symbol {
        Some(c) => format!("  {}:{}-{}  [{}] {}", shown(&c.path), c.start, c.end, c.kind, shown(if c.qualname.is_empty() { &c.name } else { &c.qualname })),
        None => format!("  {}  (no enclosing symbol)", shown(&g.path)),
    };
    for h in &g.lines {
        out.push_str(&format!("\n    {}: {}", h.line, h.text.trim_end()));
    }
    if let Some(note) = &g.note {
        out.push_str(&format!("\n    note: {note}"));
    }
    out
}

/// Definitions (index), callers (index) and every whole-word occurrence of `name` (Engine A),
/// grouped by the enclosing symbol. Works without an index too (then only occurrences, ungrouped).
pub fn references(s: &Searcher, name: &str, max_results: u64) -> Result<References> {
    let bare = bare_name(s, name)?;
    let definitions = s.ix.definitions(&bare)?;
    let callers = s.callers(name, 200)?;
    let (groups, truncated) = grouped_grep(s, &crate::resolve::name_search(&bare, max_results), &crate::files::repository_files(&s.ix.root))?;
    Ok(References { definitions, callers, groups, truncated })
}

/// Plain-text rendering of `references`.
pub fn references_render(r: &References) -> String {
    let mut out = String::new();
    if !r.definitions.is_empty() {
        out.push_str("definitions:\n");
        for d in &r.definitions {
            out.push_str(&format!("    {}:{}-{}  [{}] {}\n", shown(&d.path), d.start, d.end, d.kind, shown(&d.qualname)));
        }
    }
    if !r.callers.is_empty() {
        out.push_str("callers:\n");
        for h in &r.callers {
            out.push_str(&format!("    {}  [{}] {}\n", crate::fmt::loc(h), h.kind, shown(&h.qualname)));
        }
    }
    out.push_str(&format!("references ({} lines in {} symbols):\n", r.total_lines(), r.groups.len()));
    groups_text(&r.groups, &mut out);
    out
}

/// The name the occurrences of `name` are written with: the stored name of a definition
/// `find_symbol` finds for it that ends the typed text in NFC (`list->vec`, `#secret`, the `call` of
/// `Foo::Bar.call`), else the last part of its dotted form.
pub fn bare_name(s: &Searcher, name: &str) -> Result<String> {
    let typed = name.trim();
    let hits = s.find_symbol(typed, 50)?;
    let typed_nfc: String = unicode_normalization::UnicodeNormalization::nfc(typed).collect();
    let ends = |n: &str| typed.ends_with(n) || typed_nfc.ends_with(&unicode_normalization::UnicodeNormalization::nfc(n).collect::<String>());
    let found = hits.iter().map(|h| h.name.as_str()).filter(|n| !n.is_empty() && ends(n)).max_by_key(|n| n.len());
    Ok(match found {
        Some(n) => n.to_string(),
        None => crate::search::dotted(typed).rsplit('.').next().unwrap_or(typed).to_string(),
    })
}
