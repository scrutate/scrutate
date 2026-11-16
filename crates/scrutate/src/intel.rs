//! Engine B intelligence beyond ranked search: outlines, similar and duplicate code,
//! and tree-sitter structural queries.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

use crate::fmt::{echo, shown};
use crate::index::{ChunkRow, Index};
use crate::search::{CODE_KINDS, Hit, Searcher};
use crate::text::{BODY_CAP, tokenize2};
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
    let want = k.saturating_mul(3);
    let mut near = s.ix.lex()?.scan_b(&b, want, row.id)?;
    // scan_b keeps every tie at its cut: those tied at the cut are taken by place, up to `want`
    if near.len() > want {
        let last = near[want - 1].1;
        let from = near.iter().position(|x| x.1 == last).unwrap_or(want);
        let places = s.ix.places(&near[from..].iter().map(|x| x.0).collect::<Vec<_>>())?;
        near[from..].sort_by(|a, b| places.get(&a.0).cmp(&places.get(&b.0)));
        near.truncate(want);
    }
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
    }
    // equal cosines by the token overlap, then by place
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.2.total_cmp(&a.2)).then_with(|| (&a.0.path, a.0.start).cmp(&(&b.0.path, b.0.start))));
    out.truncate(k);
    Ok(out)
}

// ------------------------------------------------------------------------- duplicates (MinHash LSH)
const HASHES: usize = 64;
pub(crate) const BANDS: usize = 16;
/// The MinHash values each band joins into its key.
pub(crate) const ROWS: usize = HASHES / BANDS;
/// The most distinct bodies a MinHash bucket may hold and be compared pair by pair: a bucket of m
/// costs m squared comparisons, so a larger one is not compared and its bodies are named.
pub(crate) const MAX_BUCKET: usize = 64;

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
    std::array::from_fn(|b| mh[b * ROWS..(b + 1) * ROWS].iter().fold(0xcbf29ce484222325u64, |key, v| mix64(key ^ *v)))
}

/// The share of the pairs at similarity `j` that `dupes` compares: a band holds a pair with
/// probability j^ROWS, and a pair no band holds shares no bucket.
pub fn found_share(j: f64) -> f64 {
    1.0 - (1.0 - j.powi(ROWS as i32)).powi(BANDS as i32)
}

/// The similarity at which `found_share` reaches 99%: an answer to a lower threshold says that
/// near pairs are sampled.
pub fn sampling_floor() -> f64 {
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..60 {
        let mid = (lo + hi) / 2.0;
        if found_share(mid) < 0.99 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
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

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct DupGroup {
    pub kind: &'static str, // "exact" (same body text up to whitespace) or "near"
    pub similarity: f32,
    pub members: Vec<(String, String, i64, i64)>, // path, qualname, start, end
    pub tokens: usize,
    /// for a near pair one of whose members stands for an exact group: how many copies each
    /// member stands for, in member order (empty when each is one function)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub copies: Vec<usize>,
}

/// What `dupes` answers: its groups, and the definitions it did not compare.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dupes {
    pub groups: Vec<DupGroup>,
    /// one place for each distinct body of a bucket of more than `MAX_BUCKET` distinct bodies, in
    /// path and line order: a near pair among them that shares no other bucket is not in the answer
    pub crowded: Vec<(String, String, i64, i64)>,
    /// with a threshold under `sampling_floor()`: the threshold, and the share of the pairs at
    /// it that are found
    pub sampled: Option<(f32, f64)>,
}

/// Duplicate and near-duplicate functions: the same body text up to whitespace, then MinHash/LSH
/// candidates verified by Jaccard similarity of word 3-gram sets (>= `threshold`; 1 = exact only).
pub fn dupes(ix: &Index, min_tokens: usize, threshold: f32, limit: usize) -> Result<Dupes> {
    let mut d = duplicates(ix, &KeyRows::load(ix, min_tokens)?, None, threshold, limit)?;
    if threshold < 1.0 && f64::from(threshold) < sampling_floor() {
        d.sampled = Some((threshold, found_share(f64::from(threshold))));
    }
    Ok(d)
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
pub(crate) fn duplicates(ix: &Index, keys: &KeyRows, focus: Option<&Focus>, threshold: f32, limit: usize) -> Result<Dupes> {
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
    // a bucket counts distinct bodies: the copies of one body stand as one member, so a body
    // copied many times is still compared with the others
    let distinct = |m: &[usize]| m.iter().map(|&i| rows[i].1.body).collect::<HashSet<u64>>().len();
    let (compared, crowded): (Vec<&Vec<usize>>, Vec<&Vec<usize>>) =
        buckets.values().filter(|m| near_on && distinct(m) > 1).partition(|m| distinct(m) <= MAX_BUCKET);
    // the place of each definition a group or a pair may hold, and the text of each a pair may hold
    let mut needed: HashSet<usize> = exact.values().chain(same_words.values()).filter(|m| m.len() > 1).flatten().copied().collect();
    needed.extend(compared.iter().copied().flatten());
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
    // by path and line, never by chunk id, which follows the index's history
    let by_place = |a: &usize, b: &usize| {
        let (x, y) = (&place[a], &place[b]);
        (&x.0, x.2, x.3, &x.1).cmp(&(&y.0, y.2, y.3, &y.1))
    };

    let mut out: Vec<DupGroup> = Vec::new();
    let mut in_exact: HashMap<usize, u64> = HashMap::new();
    for (&key, members) in exact.iter().filter(|(_, m)| m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort_by(by_place);
        ms.dedup_by(|b, a| overlap(*a, *b));
        if ms.len() < 2 {
            continue;
        }
        in_exact.extend(ms.iter().map(|&i| (i, key)));
        out.push(DupGroup {
            kind: "exact",
            similarity: 1.0,
            tokens: rows[ms[0]].1.ntok as usize,
            members: ms.iter().map(|&i| member(i)).collect(),
            copies: Vec::new(),
        });
    }
    for members in same_words.values().filter(|m| near_on && m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort_by(by_place);
        ms.dedup_by(|b, a| overlap(*a, *b));
        if ms.len() < 2 || ms.iter().all(|i| in_exact.contains_key(i) && in_exact.get(i) == in_exact.get(&ms[0])) {
            continue;
        }
        out.push(DupGroup {
            kind: "near",
            similarity: 1.0,
            tokens: rows[ms[0]].1.ntok as usize,
            members: ms.iter().map(|&i| member(i)).collect(),
            copies: Vec::new(),
        });
    }
    // each bucket as its distinct bodies: the first copy by place, and how many copies it stands for
    let mut copies: HashMap<usize, usize> = HashMap::new();
    let bodies: Vec<Vec<usize>> = compared
        .iter()
        .map(|m| {
            let mut of_body: HashMap<u64, Vec<usize>> = HashMap::new();
            for &i in m.iter() {
                of_body.entry(rows[i].1.body).or_default().push(i);
            }
            of_body
                .into_values()
                .map(|mut cs| {
                    cs.sort_by(by_place);
                    cs.dedup_by(|b, a| overlap(*a, *b));
                    copies.insert(cs[0], cs.len());
                    cs[0]
                })
                .collect()
        })
        .collect();
    let sets: HashMap<usize, HashSet<u64>> = copies
        .keys()
        .map(|&i| {
            let text = &place[&i].4;
            let body = text.split_once('\n').map(|(_, b)| b).unwrap_or("");
            (i, shingles(&tokenize2(body, Some(BODY_CAP)).0))
        })
        .collect();
    let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut near: Vec<(f32, usize, usize)> = Vec::new();
    for members in &bodies {
        for a in 0..members.len() {
            for b in a + 1..members.len() {
                let (i, j) = (members[a].min(members[b]), members[a].max(members[b]));
                if rows[i].1.words == rows[j].1.words || !seen_pairs.insert((i, j)) {
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
    // every pair goes to the one sort below, so `limit` cuts them in the answer's order
    for (jac, i, j) in near {
        let tokens = (rows[i].1.ntok as usize).min(rows[j].1.ntok as usize);
        let (i, j) = if by_place(&i, &j).is_le() { (i, j) } else { (j, i) };
        let n = [copies[&i], copies[&j]];
        let copies = if n == [1, 1] { Vec::new() } else { n.to_vec() };
        out.push(DupGroup { kind: "near", similarity: jac, tokens, members: vec![member(i), member(j)], copies });
    }
    // members last: the exact groups come from a hash map, and a cut must not depend on its order
    out.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(b.similarity.partial_cmp(&a.similarity).unwrap()).then_with(|| a.members.cmp(&b.members)));
    out.truncate(limit);
    Ok(Dupes { groups: out, crowded: crowded_places(ix, &rows, &crowded, &place)?, sampled: None })
}

/// One place for each distinct body of the crowded buckets, the first of its copies by place, in
/// path and line order.
fn crowded_places(
    ix: &Index,
    rows: &[(i64, DupKeys)],
    crowded: &[&Vec<usize>],
    place: &HashMap<usize, (String, String, i64, i64, String)>,
) -> Result<Vec<(String, String, i64, i64)>> {
    let members: HashSet<usize> = crowded.iter().copied().flatten().copied().collect();
    let unplaced: Vec<i64> = members.iter().filter(|i| !place.contains_key(i)).map(|&i| rows[i].0).collect();
    let more: HashMap<i64, (String, String, i64, i64)> = ix.places_by_id(&unplaced)?.into_iter().map(|(id, p, q, a, b)| (id, (p, q, a, b))).collect();
    let mut first: HashMap<u64, (String, String, i64, i64)> = HashMap::new();
    for &i in &members {
        let at = match place.get(&i) {
            Some(p) => (p.0.clone(), p.1.clone(), p.2, p.3),
            None => more[&rows[i].0].clone(),
        };
        let slot = first.entry(rows[i].1.body).or_insert_with(|| at.clone());
        if (&at.0, at.2, at.3, &at.1) < (&slot.0, slot.2, slot.3, &slot.1) {
            *slot = at;
        }
    }
    let mut out: Vec<(String, String, i64, i64)> = first.into_values().collect();
    out.sort_by(|x, y| (&x.0, x.2, x.3, &x.1).cmp(&(&y.0, y.2, y.3, &y.1)));
    Ok(out)
}

/// `dupes` over these definitions, each text read whole and every pair of distinct bodies
/// compared, with no bucket and no cap: the oracle the tests of `duplicates` compare with.
#[cfg(test)]
pub(crate) fn dupes_in(rows: &[&crate::index::Body], min_tokens: usize, threshold: f32, limit: usize) -> Vec<DupGroup> {
    let mut exact: HashMap<u64, Vec<usize>> = HashMap::new();
    // the same words in the same order: bodies that differ in operators, numbers or spacing
    let mut same_words: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut word_key: Vec<u64> = Vec::with_capacity(rows.len());
    let mut sets: Vec<HashSet<u64>> = Vec::with_capacity(rows.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(rows.len());
    let mut of_body: HashMap<u64, Vec<usize>> = HashMap::new();
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
        // what has MinHash bands is what `duplicates` can pair
        if sh.len() >= 8 {
            of_body.entry(seq).or_default().push(i);
        }
        sets.push(sh);
    }
    let by_place = |a: &usize, b: &usize| (&rows[*a].1, rows[*a].3, rows[*a].4, &rows[*a].2).cmp(&(&rows[*b].1, rows[*b].3, rows[*b].4, &rows[*b].2));
    let mut out: Vec<DupGroup> = Vec::new();
    let mut in_exact: HashMap<usize, u64> = HashMap::new();
    for (&key, members) in exact.iter().filter(|(_, m)| m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort_by(by_place);
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
            copies: Vec::new(),
        });
    }
    // a threshold of 1 asks for exact duplicates only
    for members in same_words.values().filter(|m| threshold < 1.0 && m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort_by(by_place);
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
            copies: Vec::new(),
        });
    }
    // a container and the symbol nested inside it share text: not a duplicate
    let overlap = |a: usize, b: usize| rows[a].1 == rows[b].1 && rows[a].3 <= rows[b].4 && rows[b].3 <= rows[a].4;
    // one member per distinct body: the first of its copies by place, and how many there are
    let mut bodies: Vec<(usize, usize)> = of_body
        .into_values()
        .map(|mut cs| {
            cs.sort_by(by_place);
            cs.dedup_by(|b, a| overlap(*a, *b));
            (cs[0], cs.len())
        })
        .collect();
    bodies.sort_unstable();
    // a body, and how many copies it stands for
    type Counted = (usize, usize);
    let mut near: Vec<(f32, Counted, Counted)> = Vec::new();
    for a in 0..bodies.len() {
        for b in a + 1..bodies.len() {
            let (i, j) = (bodies[a].0, bodies[b].0);
            if threshold >= 1.0 || word_key[i] == word_key[j] || overlap(i, j) {
                continue;
            }
            let inter = sets[i].intersection(&sets[j]).count() as f32;
            let union = sets[i].union(&sets[j]).count().max(1) as f32;
            let jac = inter / union;
            if jac >= threshold {
                near.push((jac, bodies[a], bodies[b]));
            }
        }
    }
    for (jac, x, y) in near {
        let ((i, ni), (j, nj)) = if by_place(&x.0, &y.0).is_le() { (x, y) } else { (y, x) };
        out.push(DupGroup {
            kind: "near",
            similarity: jac,
            tokens: sizes[i].min(sizes[j]),
            members: vec![(rows[i].1.clone(), rows[i].2.clone(), rows[i].3, rows[i].4), (rows[j].1.clone(), rows[j].2.clone(), rows[j].3, rows[j].4)],
            copies: if (ni, nj) == (1, 1) { Vec::new() } else { vec![ni, nj] },
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

/// Each searched path from `top` ("" for `top` itself), or as given when it lies outside.
fn scopes(top: &Path, roots: &[PathBuf]) -> Vec<String> {
    let top = std::fs::canonicalize(top).ok();
    let from_top = |r: &PathBuf| Some(crate::instant::slashed(std::fs::canonicalize(r).ok()?.strip_prefix(top.as_ref()?).ok()?.to_string_lossy()));
    roots.iter().map(|r| from_top(r).unwrap_or_else(|| r.to_string_lossy().into_owned())).collect()
}

/// For each pattern of `q`, strings one of which a file's text holds whenever the pattern matches
/// in it: the strings of an `#eq?` or `#any-of?` on a capture that is in every match (quantifier
/// one, or one or more). None when some pattern has none, so that every file is parsed.
fn required_literals(q: &tree_sitter::Query, source: &str) -> Option<Vec<Vec<String>>> {
    static PREDICATE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"\(#(eq|any-of)\?\s+@([\w.-]+)\s+((?:"(?:[^"\\]|\\.)*"\s*)+)\)"#).unwrap());
    static STRING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#""((?:[^"\\]|\\.)*)""#).unwrap());
    let unescape = |s: &str| {
        let (mut out, mut chars) = (String::new(), s.chars());
        while let Some(c) = chars.next() {
            out.push(match (c, c == '\\') {
                (_, true) => match chars.next() {
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    Some('0') => '\0',
                    Some(other) => other,
                    None => return None,
                },
                (c, false) => c,
            });
        }
        Some(out)
    };
    (0..q.pattern_count())
        .map(|p| {
            // a predicate in a `;` comment is none
            let text = uncommented(source.get(q.start_byte_for_pattern(p)..q.end_byte_for_pattern(p))?);
            let quantifiers = q.capture_quantifiers(p);
            PREDICATE.captures_iter(&text).find_map(|c| {
                let capture = q.capture_index_for_name(&c[2])? as usize;
                if !matches!(quantifiers.get(capture), Some(tree_sitter::CaptureQuantifier::One | tree_sitter::CaptureQuantifier::OneOrMore)) {
                    return None;
                }
                let strings: Vec<String> = STRING.captures_iter(&c[3]).map(|s| unescape(&s[1])).collect::<Option<_>>()?;
                (&c[1] == "any-of" || strings.len() == 1).then_some(strings).filter(|s| !s.is_empty())
            })
        })
        .collect()
}

/// Whether a file's text holds a literal of some pattern (`required_literals`).
fn may_match(required: &[Vec<String>], text: &str) -> bool {
    required.iter().any(|one_of| one_of.iter().any(|l| text.contains(l.as_str())))
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

/// `query` with each `;` comment outside a string blanked to spaces, its line ends kept: what
/// tree-sitter reads of it.
fn uncommented(query: &str) -> String {
    let (mut out, mut chars) = (String::with_capacity(query.len()), query.chars());
    while let Some(c) = chars.next() {
        match c {
            ';' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                    out.push(' ');
                }
            }
            '"' => {
                out.push('"');
                while let Some(c) = chars.next() {
                    out.push(c);
                    match c {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// The line of a predicate written at the top level of `query`, outside every pattern's
/// parentheses: tree-sitter accepts it and applies it to nothing.
fn loose_predicate(query: &str) -> Option<usize> {
    let (mut depth, mut line) = (0usize, 1usize);
    let query = uncommented(query);
    let mut chars = query.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => line += 1,
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

    /// `dupes_in`'s groups less the pairs `duplicates` cannot see, cut to `limit`: two bodies are
    /// compared only in a MinHash bucket they share (a band of the same index with the same key)
    /// of at most `MAX_BUCKET` distinct bodies. A pair that shares a bucket, each one crowded,
    /// must have both its members among `crowded`; one that shares none is what the sampling
    /// misses.
    pub(crate) fn seen(mut groups: Vec<DupGroup>, rows: &[&crate::index::Body], crowded: &[(String, String, i64, i64)], limit: usize) -> Vec<DupGroup> {
        let keys: HashMap<(&str, &str, i64, i64), DupKeys> = rows.iter().map(|r| ((r.1.as_str(), r.2.as_str(), r.3, r.4), DupKeys::of(&r.5))).collect();
        let key = |m: &(String, String, i64, i64)| keys[&(m.0.as_str(), m.1.as_str(), m.2, m.3)];
        let mut bucket: HashMap<(usize, u64), HashSet<u64>> = HashMap::new();
        for k in keys.values() {
            for (b, band) in k.bands.iter().flatten().enumerate() {
                bucket.entry((b, *band)).or_default().insert(k.body);
            }
        }
        // the buckets two bodies share, each as whether it is crowded
        let shared = |x: &DupKeys, y: &DupKeys| -> Vec<bool> {
            x.bands.zip(y.bands).map_or(Vec::new(), |(p, q)| {
                p.iter().zip(&q).enumerate().filter(|(_, (u, v))| u == v).map(|(b, (u, _))| bucket[&(b, *u)].len() > MAX_BUCKET).collect()
            })
        };
        groups.retain(|g| match g.members.as_slice() {
            [x, y] if key(x).words != key(y).words => {
                let buckets = shared(&key(x), &key(y));
                if !buckets.is_empty() && buckets.iter().all(|&full| full) {
                    assert!(crowded.contains(x) && crowded.contains(y), "{x:?} and {y:?} share only crowded buckets and are not named");
                }
                buckets.contains(&false)
            }
            _ => true,
        });
        groups.truncate(limit);
        groups
    }

    // of the oracle's pairs, `seen` drops those sharing no bucket (sampling) and asserts those sharing only crowded ones are named; the rest must be equal
    fn agree(root: &Path) -> std::result::Result<(), TestCaseError> {
        let mut ix = Index::open(root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        for (min, threshold, limit) in [(30, 0.75, 50), (8, 0.5, 3), (5, 1.0, 1000), (30, 0.75, usize::MAX)] {
            let rows = ix.bodies(CODE_KINDS, min).unwrap();
            let rows: Vec<&crate::index::Body> = rows.iter().collect();
            let got = duplicates(&ix, &KeyRows::load(&ix, min).unwrap(), None, threshold, limit).unwrap();
            let want = seen(dupes_in(&rows, min, threshold, usize::MAX), &rows, &got.crowded, limit);
            prop_assert_eq!(got.groups, want, "min {} threshold {} limit {}", min, threshold, limit);
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

    /// A word of its own for `n`: consonants only, so no stem changes it and no stop list holds it.
    fn word(n: usize) -> String {
        let letters = b"bcdfgkmnprtvwxz";
        (0..4).map(|k| letters[n / letters.len().pow(k) % letters.len()] as char).collect()
    }

    /// A tree of Python functions, one per file, each body the words it is given.
    fn index_of(name: &str, bodies: &[(String, Vec<String>)]) -> (PathBuf, Index) {
        let root = std::env::temp_dir().join(format!("scrutate_dupes_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for (file, words) in bodies {
            let rows: String = words.chunks(6).map(|c| format!("        {},\n", c.join(", "))).collect();
            std::fs::write(root.join(file), format!("def f(p):\n    return [\n{rows}    ]\n")).unwrap();
        }
        let mut ix = Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        (root, ix)
    }

    #[test]
    fn a_near_copy_of_a_body_copied_more_than_a_bucket_holds_is_found_against_its_copies() {
        let body: Vec<String> = (0..60).map(word).collect();
        let mut variant = body.clone();
        variant[30] = word(1000);
        for copies in [63, 64, 65] {
            let mut files: Vec<(String, Vec<String>)> = (0..copies).map(|i| (format!("c{i:03}.py"), body.clone())).collect();
            files.push(("variant.py".into(), variant.clone()));
            let (root, ix) = index_of(&format!("copies{copies}"), &files);
            let got = dupes(&ix, 30, 0.75, usize::MAX).unwrap();
            let exact: Vec<&DupGroup> = got.groups.iter().filter(|g| g.kind == "exact").collect();
            assert_eq!(exact.iter().map(|g| g.members.len()).collect::<Vec<_>>(), [copies], "{copies}");
            let near: Vec<&DupGroup> = got.groups.iter().filter(|g| g.kind == "near").collect();
            assert_eq!(near.len(), 1, "{copies}: {near:?}");
            let names: Vec<&str> = near[0].members.iter().map(|m| m.0.as_str()).collect();
            assert_eq!((names, near[0].copies.clone()), (vec!["c000.py", "variant.py"], vec![copies, 1]), "{copies}");
            assert!(got.crowded.is_empty());
            drop(ix);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn the_sampling_figures_cli_md_states_follow_from_the_bands() {
        let cli = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/CLI.md")).expect("docs/CLI.md beside the crate");
        // "practically every": over 99% at the default threshold
        assert!(cli.contains("at the default threshold practically every near pair is found"));
        assert!(found_share(f64::from(crate::ops::DUPES_THRESHOLD)) > 0.99, "{}", found_share(f64::from(crate::ops::DUPES_THRESHOLD)));
        // "about two in three" at 0.5: within 5 points of 2/3
        assert!(cli.contains("at `--threshold 0.5` about two in three of the pairs at that similarity are"));
        assert!((found_share(0.5) - 2.0 / 3.0).abs() < 0.05, "{}", found_share(0.5));
        assert!(f64::from(crate::ops::DUPES_THRESHOLD) >= sampling_floor(), "the default threshold would carry the sampling note");
        // the cap the page names is the one the search keeps
        assert!(cli.contains(&format!("A bucket of more than {MAX_BUCKET} bodies is not compared pair by pair")));
    }

    #[test]
    fn under_the_sampling_floor_the_answer_says_how_many_pairs_at_its_threshold_it_finds() {
        // 200 pairs of 38-word bodies, the last 12 words of one of their own: Jaccard 24/48 = 0.5
        let mut next = 0;
        let mut fresh = |n: usize| {
            next += n;
            (next - n..next).map(word).collect::<Vec<String>>()
        };
        let mut files = Vec::new();
        for i in 0..200 {
            let a = fresh(38);
            let b: Vec<String> = a[..26].iter().cloned().chain(fresh(12)).collect();
            files.push((format!("p{i:03}_a.py"), a));
            files.push((format!("p{i:03}_b.py"), b));
        }
        let (root, ix) = index_of("sampling", &files);
        let got = dupes(&ix, 30, 0.5, usize::MAX).unwrap();
        let (threshold, share) = got.sampled.unwrap();
        assert_eq!(threshold, 0.5);
        assert!((share - found_share(0.5)).abs() < 1e-12 && (share - 0.644).abs() < 0.001, "{share}");
        let found = got.groups.iter().filter(|g| g.kind == "near" && g.members[0].0[..4] == g.members[1].0[..4]).count();
        assert_eq!(found, got.groups.len(), "a pair of bodies with no word in common");
        // about 129 of 200; the band holds four standard deviations of a binomial at 0.644
        assert!((100..=158).contains(&found), "{found} of 200");
        assert_eq!(dupes(&ix, 30, 0.75, usize::MAX).unwrap().sampled, None);
        assert_eq!(dupes(&ix, 30, 1.0, usize::MAX).unwrap().sampled, None);
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bucket_of_more_distinct_bodies_than_it_compares_names_them_and_hides_no_other_pair() {
        let base: Vec<String> = (0..120).map(word).collect();
        for (n, crowded) in [(64, false), (90, true)] {
            let files: Vec<(String, Vec<String>)> = (0..n)
                .map(|i| {
                    let mut ws = base.clone();
                    ws[5 + i] = word(2000 + i);
                    (format!("v{i:03}.py"), ws)
                })
                .collect();
            let (root, ix) = index_of(&format!("crowded{n}"), &files);
            let got = dupes(&ix, 30, 0.75, usize::MAX).unwrap();
            assert_eq!(!got.crowded.is_empty(), crowded, "{n}: {:?}", got.crowded);
            let rows = ix.bodies(CODE_KINDS, 30).unwrap();
            let rows: Vec<&crate::index::Body> = rows.iter().collect();
            let all = dupes_in(&rows, 30, 0.75, usize::MAX);
            assert_eq!(got.groups, seen(all.clone(), &rows, &got.crowded, usize::MAX), "{n}");
            if crowded {
                // pairs lost to the cap: in the oracle at 0.75 or more, not in the answer, both members named
                let lost: Vec<&DupGroup> = all.iter().filter(|g| !got.groups.contains(g)).collect();
                assert!(!lost.is_empty(), "{n}: the crowded bucket lost no pair");
                assert!(lost.iter().all(|g| g.members.iter().all(|m| got.crowded.contains(m))), "{n}: {lost:?}");
            }
            drop(ix);
            let _ = std::fs::remove_dir_all(&root);
        }
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

/// A file of the query's language a structural query leaves out: it is over `MAX_FILE_BYTES`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TooLarge {
    pub path: String,
    /// named on the command line, not met in a walk
    pub named: bool,
}

impl TooLarge {
    /// The line for a file named on the command line: no match in it is known.
    pub fn alone(&self) -> String {
        format!("{}: over 1 MB, left alone", self.path)
    }

    /// The one note for the files a walk met over the bound, with the first names, or None.
    pub fn walked_note(all: &[TooLarge]) -> Option<String> {
        let walked: Vec<&str> = all.iter().filter(|t| !t.named).map(|t| t.path.as_str()).collect();
        let (n, first) = (walked.len(), walked.iter().take(3).copied().collect::<Vec<_>>().join(", "));
        let more = if n > 3 { format!(" and {} more", n - 3) } else { String::new() };
        match n {
            0 => None,
            1 => Some(format!("1 file over 1 MB was not searched: {first}")),
            _ => Some(format!("{n} files over 1 MB were not searched: {first}{more}")),
        }
    }
}

/// The files of up to 1 MB under `roots` that `spec` may hold by name, in path order, as Engine
/// A's default walk gives them (ignore files read, hidden files and links left out), and those of
/// them over the bound.
pub(crate) fn query_files(spec: &crate::langs::LangSpec, roots: &[PathBuf], as_given: bool) -> (Vec<QueryFile>, Vec<TooLarge>) {
    let (mut files, mut large) = (Vec::new(), Vec::new());
    for (i, root) in roots.iter().enumerate() {
        let Ok(walk) = crate::instant::sorted_walk(root) else { continue };
        for entry in walk.flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let Ok(md) = entry.metadata() else { continue };
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
                if md.len() > crate::index::MAX_FILE_BYTES {
                    large.push(TooLarge { path: shown, named: entry.depth() == 0 });
                } else {
                    files.push(QueryFile { path: path.to_path_buf(), rel, shown, md, root: i });
                }
            }
        }
    }
    (files, large)
}

/// Run a tree-sitter query (S-expression pattern language) over the files of one language under
/// `roots`, delivering every capture to `on_capture` in path order. No index is needed; files are
/// parsed in parallel in bounded chunks, so memory stays flat however many matches there are.
/// With `cache_root`, each file's captures are kept under `cache_root/.scrutate/tsquery/` by
/// content hash and files seen before are not parsed again (see `tscache`); the output is the same.
/// With `as_given`, paths are printed as `grep PATH...` prints them (the root as given, then the
/// path below it), except under `.`; without, from the root that holds them.
/// Returns the number of captures delivered (stops at `limit`), and the files of the language
/// left out for their size.
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
) -> Result<(usize, Vec<TooLarge>)> {
    use rayon::prelude::*;
    if cfg!(target_os = "wasi") {
        bail!("tsquery is not in the wasi build: it needs threads");
    }
    let (spec, _, q) = compile_query(lang, query)?;
    // a file that holds none of the literals every match needs is not parsed: its answer is empty
    let gate = required_literals(&q, query);
    let names: Vec<String> = q.capture_names().iter().map(|n| n.to_string()).collect();
    // the code index's record saves reading a file whose captures are cached
    let recorded = cache_root.map(crate::stores::recorded).unwrap_or_default();
    let top = cache_root.filter(|_| !recorded.is_empty()).and_then(|r| std::fs::canonicalize(r).ok());
    let under: Vec<Option<String>> = roots
        .iter()
        .map(|root| top.as_ref().and_then(|t| std::fs::canonicalize(root).ok()?.strip_prefix(t).ok().map(|p| crate::instant::slashed(p.to_string_lossy()))))
        .collect();
    // (path, path from its root, path shown, the code index's hash of it when its stat is current)
    let (files, large) = query_files(spec, roots, as_given);
    let files: Vec<(PathBuf, String, String, Option<String>)> = files
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
        let gate_ref = &gate;
        let names_ref = &names;
        let stop_ref = &stop;
        let cached = cache.as_ref().map(|c| &c.entries);
        scope.spawn(move || {
            files_ref.par_iter().enumerate().for_each_init(
                || {
                    let mut parser = tree_sitter::Parser::new();
                    // a worker makes its own: a Language is not Sync on wasm
                    let ok = parser.set_language(&(spec.language)()).is_ok();
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
                        if let Some((raw, src)) = raw.as_deref().and_then(|raw| Some((raw, crate::index::decode_source(raw)?.into_owned())))
                            && crate::parse::spec_for(rel, &src).map(|s| s.id) == Some(spec.id)
                        {
                            key = cached.map(|_| crate::tscache::key(&crate::index::content_hash(raw), rel));
                            if let Some(caps) = hit(key) {
                                let _ = tx.send((i, key, caps, true));
                                return;
                            }
                            if gate_ref.as_ref().is_some_and(|g| !may_match(g, &src)) {
                                let _ = tx.send((i, key, Vec::new(), true));
                                return;
                            }
                            match parser.parse(src.as_bytes(), None) {
                                Some(tree) => {
                                    let mut cursor = tree_sitter::QueryCursor::new();
                                    cursor.set_match_limit(1 << 16);
                                    let mut matches = cursor.matches(q_ref, tree.root_node(), src.as_bytes());
                                    use tree_sitter::StreamingIterator;
                                    while let Some(m) = matches.next() {
                                        for c in m.captures() {
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
                                }
                                _ => {
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
            return Ok((delivered, large)); // too large to cache: the stored entries stay as they were
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
    Ok((delivered, large))
}

/// What a long search reports to and is stopped by: the files it read, and the call's cancel flag.
#[derive(Clone, Copy)]
pub struct Watch<'a> {
    pub count: &'a crate::index::FileCount,
    pub cancel: &'a std::sync::atomic::AtomicBool,
}

/// `tsquery_each` collected into a vector (MCP and bindings; the CLI streams).
pub fn tsquery(lang: &str, query: &str, roots: &[PathBuf], limit: usize, cache_root: Option<&Path>, watch: Watch) -> Result<(Vec<Capture>, Vec<TooLarge>)> {
    let mut out = Vec::new();
    let (_, large) = tsquery_each(lang, query, roots, false, limit, cache_root, watch, &mut |c| out.push(c))?;
    Ok((out, large))
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
    let narrowed = crate::stores::grep_files(&s.ix, q, files, s.look.as_ref());
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
    references_in(s, name, max_results, None)
}

/// `references` with what `scope` admits: Engine A walks only the files it can admit, and the
/// definitions, callers and groups outside it are left out.
pub fn references_in(s: &Searcher, name: &str, max_results: u64, scope: Option<&crate::filter::Compiled>) -> Result<References> {
    let bare = bare_name(s, name)?;
    let mut definitions = s.ix.definitions(&bare)?;
    let mut callers = s.callers(name, 200)?;
    let walk = crate::files::repository_files(&s.ix.root);
    let walk = match scope {
        Some(f) => f.walk(walk),
        None => walk,
    };
    // one line past max_results says whether more exist: the search stopping at exactly max_results does not
    let q = crate::resolve::name_search(&bare, max_results + 1);
    let narrowed = crate::stores::grep_files(&s.ix, &q, &walk, s.look.as_ref());
    let (mut found, stopped) = occurrences_in(&s.ix.root, narrowed.as_ref().unwrap_or(&walk), &q, &s.cancel)?;
    let past = found.iter().filter(|r| matches!(r, crate::instant::Record::Match(_))).count() as u64 > max_results;
    if past {
        let last = found.iter().rposition(|r| matches!(r, crate::instant::Record::Match(_)));
        found.remove(last.unwrap_or_default());
    }
    let truncated = past || stopped && !past;
    let mut groups = group_by_symbol(&s.ix, found)?;
    if let Some(f) = scope {
        definitions.retain(|d| f.admits(&d.path, &d.kind));
        callers.retain(|h| f.admits_path(&h.path));
        groups.retain(|g| f.admits_path(&g.path));
    }
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

#[cfg(test)]
mod literal_gate {
    use super::{may_match, required_literals};

    fn query(lang: &crate::langs::LangSpec, q: &str) -> Option<tree_sitter::Query> {
        tree_sitter::Query::new(&(lang.language)(), q).ok()
    }

    #[test]
    fn only_a_literal_every_match_needs_gates_a_file() {
        let rust = crate::langs::by_id("rust").unwrap();
        let some = |q: &str| required_literals(&query(rust, q).unwrap(), q);
        assert_eq!(some(r#"((identifier) @i (#eq? @i "kmalloc"))"#), Some(vec![vec!["kmalloc".to_string()]]));
        assert_eq!(some(r#"((identifier) @i (#any-of? @i "a" "b\"c"))"#), Some(vec![vec!["a".to_string(), "b\"c".to_string()]]));
        // nothing every match needs: every file is parsed
        assert_eq!(some("(identifier) @i"), None);
        assert_eq!(some(r#"((identifier) @i (#not-eq? @i "x"))"#), None);
        assert_eq!(some(r#"((identifier) @i (#eq? @i @i))"#), None);
        assert_eq!(some(r#"((identifier) @i (#match? @i "^x$"))"#), None);
        assert_eq!(some(r#"((line_comment)* @c . (identifier) @i (#eq? @c "x"))"#), None);
        assert_eq!(some(r#"((identifier) @a (#eq? @a "x")) (identifier) @b"#), None);
        // two patterns that each need one
        assert_eq!(some(r#"((identifier) @a (#eq? @a "x")) ((field_identifier) @b (#eq? @b "y"))"#), Some(vec![vec!["x".to_string()], vec!["y".to_string()]]));
        // a predicate in a `;` comment is no predicate: it gates nothing
        assert_eq!(some("((identifier) @i\n ; (#eq? @i \"oldname\")\n (#match? @i \"^km\"))"), None);
        assert_eq!(some("((identifier) @i\n (#match? @i \"^km\") ; was (#eq? @i \"oldname\")\n)"), None);
        // a `;` inside a string starts no comment
        assert_eq!(some(r#"((identifier) @i (#eq? @i "a;b"))"#), Some(vec![vec!["a;b".to_string()]]));
    }

    /// Random queries over the kinds and texts of every sample's tree: alternations, quantifiers,
    /// several patterns, `#eq?`, `#any-of?`, `#match?` (case-insensitive too), `not-` forms and a
    /// capture compared with a capture. Wherever the gate turns a file away, the query has no
    /// match in it, so the answer with the gate is the answer without it.
    #[test]
    fn random_queries_answer_the_same_with_the_gate() {
        use tree_sitter::StreamingIterator;
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move |n: usize| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((x >> 33) as usize) % n.max(1)
        };
        let (mut compiled, mut gated, mut turned_away) = (0, 0, 0);
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let Some(lang) = crate::parse::spec_for(&path.to_string_lossy(), &src) else { continue };
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&(lang.language)()).unwrap();
            let tree = parser.parse(&src, None).unwrap();
            // the named kinds and some short texts of this tree
            let (mut kinds, mut texts) = (Vec::new(), Vec::new());
            let mut cursor = tree.walk();
            let mut stack = vec![tree.root_node()];
            while let Some(n) = stack.pop() {
                if n.is_named() && !kinds.contains(&n.kind()) {
                    kinds.push(n.kind());
                }
                if n.child_count() == 0
                    && let Ok(t) = n.utf8_text(src.as_bytes())
                    && !t.is_empty()
                    && t.len() < 20
                    && !t.contains(['"', '\\', '\n'])
                    && texts.len() < 50
                {
                    texts.push(t.to_string());
                }
                stack.extend(n.children(&mut cursor));
            }
            if kinds.is_empty() || texts.is_empty() {
                continue;
            }
            for _ in 0..30 {
                let patterns = 1 + next(2);
                let mut text = String::new();
                for p in 0..patterns {
                    let (a, b) = (format!("a{p}"), format!("b{p}"));
                    let kind = kinds[next(kinds.len())];
                    let other = kinds[next(kinds.len())];
                    let quant = ["", "", "", "*", "?", "+"][next(6)];
                    let node = match next(3) {
                        0 => format!("({kind}){quant} @{a}"),
                        1 => format!("[({kind}) ({other})]{quant} @{a}"),
                        _ => format!("({kind}) @{a} ({other}) @{b}"),
                    };
                    // a third of the literals are in no file
                    let pick = |next: &mut dyn FnMut(usize) -> usize| if next(3) == 0 { "zzqqxx_absent".to_string() } else { texts[next(texts.len())].clone() };
                    let (lit, lit2) = (pick(&mut next), pick(&mut next));
                    let predicate = match next(9) {
                        0 => String::new(),
                        1 | 2 => format!("(#eq? @{a} \"{lit}\")"),
                        3 => format!("(#any-of? @{a} \"{lit}\" \"{lit2}\")"),
                        4 => format!("(#match? @{a} \"^{}$\")", regex::escape(&lit).replace('\\', "\\\\")),
                        5 => format!("(#match? @{a} \"(?i){}\")", regex::escape(&lit.to_uppercase()).replace('\\', "\\\\")),
                        6 => format!("(#not-eq? @{a} \"{lit}\")"),
                        7 => format!("(#eq? @{a} @{a})"),
                        _ => format!("(#any-eq? @{a} \"{lit}\")"),
                    };
                    text.push_str(&format!("({node} {predicate})\n"));
                }
                let Ok(q) = tree_sitter::Query::new(&(lang.language)(), &text) else { continue };
                compiled += 1;
                let Some(gate) = required_literals(&q, &text) else { continue };
                gated += 1;
                if may_match(&gate, &src) {
                    continue;
                }
                turned_away += 1;
                let mut qc = tree_sitter::QueryCursor::new();
                let mut it = qc.matches(&q, tree.root_node(), src.as_bytes());
                assert!(it.next().is_none(), "{}: the gate turned away a file that matches:\n{text}", path.display());
            }
        }
        println!("{compiled} queries compiled, {gated} gated, {turned_away} files turned away");
        assert!(compiled > 1000 && gated > 100 && turned_away > 30, "{compiled} compiled, {gated} gated, {turned_away} turned away");
    }

    /// Over every sample file: a file the gate turns away has no match, so the answer with the gate
    /// is the answer without it.
    #[test]
    fn a_file_turned_away_has_no_match() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let Some(lang) = crate::parse::spec_for(&path.to_string_lossy(), &src) else { continue };
            let Some(all) = query(lang, "(identifier) @i") else { continue };
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&(lang.language)()).unwrap();
            let tree = parser.parse(&src, None).unwrap();
            let matches = |q: &tree_sitter::Query| {
                use tree_sitter::StreamingIterator;
                let mut cursor = tree_sitter::QueryCursor::new();
                let mut it = cursor.matches(q, tree.root_node(), src.as_bytes());
                let mut n = 0;
                while it.next().is_some() {
                    n += 1;
                }
                n
            };
            let words: Vec<String> = {
                use tree_sitter::StreamingIterator;
                let mut cursor = tree_sitter::QueryCursor::new();
                let mut it = cursor.captures(&all, tree.root_node(), src.as_bytes());
                let mut w = Vec::new();
                while let Some((m, i)) = it.next() {
                    w.push(m.captures()[*i].node.utf8_text(src.as_bytes()).unwrap().to_string());
                }
                w.truncate(5);
                w
            };
            for word in words.iter().map(String::as_str).chain(["zzqqxx_absent", ""]) {
                for text in [format!(r#"((identifier) @i (#eq? @i "{word}"))"#), format!(r#"((identifier) @i (#any-of? @i "{word}" "zzqqxx_other"))"#)] {
                    let q = query(lang, &text).unwrap();
                    let gate = required_literals(&q, &text).expect("a literal");
                    if !may_match(&gate, &src) {
                        assert_eq!(matches(&q), 0, "{}: {text}", path.display());
                    }
                    if !word.is_empty() && words.iter().any(|w| w == word) {
                        assert!(may_match(&gate, &src) && matches(&q) > 0, "{}: {text}", path.display());
                    }
                    checked += 1;
                }
            }
        }
        assert!(checked > 100, "{checked}");
    }
}
