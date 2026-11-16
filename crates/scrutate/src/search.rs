//! Hybrid retrieval.
//!
//! Candidates come from three sources: field-weighted BM25 over the segment store, semantic vectors,
//! and exact name / qualified-name lookup. Each candidate gets 48 features (per-field BM25, semantic
//! similarities, exactness, literal occurrences, structure, query shape) and the learned ranker
//! orders the best 200 by a linear blend. Without a ranker the linear blend itself is used.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use crate::index::{ChunkRow, Index};
use crate::lex::tok_key;
use crate::text::{QUERY_STOP, Words, literal_candidates, name_key, query_terms, query_wholes};

pub const PRUNE: usize = 200; // the learned ranker sees (at most) this many candidates
pub const LEX_LIMIT: usize = 100;
pub const SEM_LIMIT: usize = 100;
/// Candidates the exact source gives.
const EXACT_LIMIT: usize = 60;
/// A part of a filter that keeps at most 1/NARROW of the files or the chunks is cheap to gather.
const NARROW: usize = 8;
/// How much larger a filtered search makes its exact pool each time too few hits pass, and the most.
const FILTER_WIDEN: usize = 8;
const FILTER_WIDEN_MAX: usize = 64;
/// What a filtered search says when its widened pool stopped at the bound short of k.
pub const FILTER_STOP: &str = "the filtered search read its candidates up to 64 times their usual number: narrow the query or the filter";
/// What a vendored hit loses from its ranker score: the bench/quality sweep put 0.5 and 1 at the
/// unpenalized MRR@10 and 4 well below, while a strict tier lost a fifth of the queries.
const VENDORED_MARGIN: f64 = 1.0;
pub const CODE_KINDS: &[&str] = &["function", "method", "class", "struct", "trait", "interface", "type", "enum", "impl"];

pub use crate::models::FEATURES;
pub const NFEAT: usize = FEATURES.len();

pub const fn fi(name: &str) -> usize {
    let mut i = 0;
    while i < NFEAT {
        if const_eq(FEATURES[i], name) {
            return i;
        }
        i += 1;
    }
    panic!("unknown feature")
}

const fn const_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

pub type Features = [f32; NFEAT];

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Hit {
    pub id: i64,
    pub path: String,
    pub start: i64,
    pub end: i64,
    pub name: String,
    pub qualname: String,
    pub kind: String,
    #[serde(skip)]
    pub text: String,
    pub score: f64,
    pub why: Vec<(String, serde_json::Value)>,
    /// how a near match matched the name asked for (`symmatch::How`); None for the name itself
    pub matched: Option<&'static str>,
}

impl Hit {
    pub fn loc(&self) -> String {
        format!("{}:{}-{}", self.path, self.start, self.end)
    }

    pub fn from_row(r: &ChunkRow, with_text: bool) -> Hit {
        Hit {
            id: r.id,
            path: r.path.clone(),
            start: r.start,
            end: r.end,
            name: r.name.clone(),
            qualname: r.qualname.clone(),
            kind: r.kind.clone(),
            text: if with_text { r.text.clone() } else { String::new() },
            score: 0.0,
            why: Vec::new(),
            matched: None,
        }
    }

    /// Up to `max_lines` lines of the chunk, starting just above the first line mentioning a query word.
    pub fn snippet(&self, words: &[String], max_lines: usize) -> String {
        let lines: Vec<&str> = self.text.split('\n').collect();
        let mut at = 0usize;
        if !words.is_empty() {
            let low: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
            for (i, ln) in lines.iter().enumerate() {
                let l = ln.to_lowercase();
                if low.iter().any(|w| l.contains(w.as_str())) {
                    at = i.saturating_sub(1);
                    break;
                }
            }
        }
        lines[at..(at + max_lines).min(lines.len())].join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct QueryInfo {
    pub text: String,
    pub terms: Vec<String>,
    pub wholes: Vec<String>,
    pub lits: Vec<String>,
    pub words: Vec<String>,  // raw lowercase words
    pub keys: Vec<String>,   // underscore-insensitive keys for exact name lookup
    pub dotted: Vec<String>, // Class.method style keys
}

/// The query cut at blanks only, so a name keeps its punctuation.
fn typed_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split([' ', '\t', '\n', '\r', '\x0b', '\x0c']).filter(|t| !t.is_empty())
}

/// The last part of a dotted token (`Rec.valid?`, `Shape::~Shape`, `Rec#valid?`); the token itself
/// when it has no such part.
fn typed_tail(token: &str) -> &str {
    token.rsplit(['.', ':', '#']).next().filter(|t| !t.is_empty()).unwrap_or(token)
}

/// Whether a token holds an ASCII character that is no letter, digit or underscore.
fn punctuated(token: &str) -> bool {
    token.bytes().any(|b| b.is_ascii() && !b.is_ascii_alphanumeric() && b != b'_')
}

impl QueryInfo {
    pub fn parse(text: &str) -> QueryInfo {
        let words: Vec<String> = Words::new(text).map(|w| w.to_lowercase()).collect();
        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        for w in &words {
            if w.chars().count() >= 3 && !QUERY_STOP.contains(w.as_str()) {
                let k = name_key(w);
                if seen.insert(k.clone()) {
                    keys.push(k);
                }
            }
        }
        // one word, or stop words alone, is most likely a name: `find`, `get`, `id`
        if keys.is_empty() || words.len() == 1 {
            keys.extend(words.iter().map(|w| name_key(w)).filter(|k| !k.is_empty() && seen.insert(k.clone())));
        }
        // a name written with punctuation is stored whole: `valid?`, `Get-Greeting`, `list->vector`
        let tokens: Vec<&str> = typed_tokens(text).collect();
        for t in &tokens {
            for name in [*t, typed_tail(t)] {
                if punctuated(name) && (name.chars().count() >= 3 || tokens.len() == 1) {
                    let k = name_key(name);
                    if seen.insert(k.clone()) {
                        keys.push(k);
                    }
                }
            }
        }
        let dotted_re = Regex::new(r"[\p{XID_Start}_]\p{XID_Continue}*(?:\.[\p{XID_Start}_]\p{XID_Continue}*)+").unwrap();
        let mut dotted = Vec::new();
        let mut dseen = HashSet::new();
        for m in dotted_re.find_iter(text) {
            let d: Vec<String> = m.as_str().split('.').map(name_key).collect();
            let d = d.join(".");
            if dseen.insert(d.clone()) {
                dotted.push(d);
            }
        }
        QueryInfo { text: text.to_string(), terms: query_terms(text), wholes: query_wholes(text), lits: literal_candidates(text), words, keys, dotted }
    }
}

pub fn lex_keys(q: &QueryInfo) -> Vec<i64> {
    let mut seen = HashSet::new();
    q.terms.iter().chain(&q.wholes).filter(|t| seen.insert((*t).clone())).filter(|t| !t.is_empty()).map(|t| tok_key(t)).collect()
}

pub struct Searcher {
    pub ix: Index,
    ver: Option<String>,
    callers_cache: HashMap<String, i64>,
    /// set when the call being answered was cancelled: `search_many` stops between queries
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    /// a session's kept look of the tree, for a search the text index narrows (`stores::grep_files`)
    pub look: Option<crate::files::KeptLook>,
    /// where dependency copies are kept (`SCRUTATE_CACHE_DIR` as read at open); None: the user cache
    pub cache_dir: Option<std::path::PathBuf>,
}

/// A source of candidates. The candidates are the union of the sources' ids in `Source::ALL`
/// order; a source with a rank feature sets it to 1 / (1 + rank) for each candidate it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// definitions whose name or qualified name the query has (`QueryInfo::keys`, `dotted`)
    Exact,
    /// field-weighted BM25 over the segment store
    Lexical,
    /// the vector model's nearest chunks
    Semantic,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Exact, Source::Lexical, Source::Semantic];

    pub fn name(self) -> &'static str {
        match self {
            Source::Exact => "exact",
            Source::Lexical => "lex",
            Source::Semantic => "sem",
        }
    }

    fn rank_feature(self) -> Option<usize> {
        match self {
            Source::Exact => None,
            Source::Lexical => Some(fi("lex_rank")),
            Source::Semantic => Some(fi("sem_rank")),
        }
    }

    /// How many candidates the source gives at most, ties at its cut set apart.
    fn bound(self) -> Option<usize> {
        match self {
            Source::Exact => None,
            Source::Lexical => Some(LEX_LIMIT),
            Source::Semantic => Some(SEM_LIMIT),
        }
    }
}

/// The BM25 fields as `field_scores` returns them (all fields first), each with its two features.
const LEX_FIELDS: [(usize, usize); 7] = [
    (fi("lex_log"), fi("lex_ratio")),
    (fi("name_log"), fi("name_ratio")),
    (fi("qual_log"), fi("qual_ratio")),
    (fi("sig_log"), fi("sig_ratio")),
    (fi("doc_log"), fi("doc_ratio")),
    (fi("body_log"), fi("body_ratio")),
    (fi("path_log"), fi("path_ratio")),
];

type RowFeature = fn(&ChunkRow) -> f32;
type QueryFeature = fn(&QueryInfo) -> f32;

/// Features of a candidate's own row.
const ROW_FEATURES: [(usize, RowFeature); 11] = [
    (fi("k_function"), |r| (r.kind == "function") as i32 as f32),
    (fi("k_method"), |r| (r.kind == "method") as i32 as f32),
    (fi("k_class"), |r| matches!(r.kind.as_str(), "class" | "struct" | "trait" | "interface" | "type" | "enum" | "impl") as i32 as f32),
    (fi("k_module"), |r| (r.kind == "module") as i32 as f32),
    (fi("k_other"), |r| (r.kind == "window") as i32 as f32),
    (fi("is_test"), |r| r.is_test as f32),
    (fi("log_nbody"), |r| (r.nbody as f64).ln_1p() as f32),
    (fi("has_doc"), |r| (r.ndoc > 0) as i32 as f32),
    (fi("log_ndoc"), |r| (r.ndoc as f64).ln_1p() as f32),
    (fi("qual_depth"), |r| r.qualname.matches('.').count() as f32),
    (fi("is_private"), |r| (r.name.starts_with('_') && !r.name.starts_with("__")) as i32 as f32),
];

/// Features of the query alone, the same for every candidate.
const QUERY_FEATURES: [(usize, QueryFeature); 4] = [
    (fi("n_terms"), |q| q.terms.len() as f32),
    (fi("n_lits"), |q| q.lits.len() as f32),
    (fi("n_words"), |q| q.words.len() as f32),
    (fi("has_ident"), |q| (!q.keys.is_empty() && (!q.dotted.is_empty() || q.text.contains('_') || !q.wholes.is_empty())) as i32 as f32),
];

pub struct Candidates {
    pub q: QueryInfo,
    pub rows: Vec<ChunkRow>,
    pub x: Vec<Features>,
    /// the ids each source found, best first, in `Source::ALL` order
    pub found: [Vec<i64>; 3],
}

impl Candidates {
    pub fn ids(&self, source: Source) -> &[i64] {
        &self.found[source as usize]
    }
}

/// A symbol name with every separator a language writes (`::`, `->`, `#`) as a dot. A separator
/// stands between two names, so the arrow of `operator->` stays.
pub(crate) fn dotted(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(c) = rest.chars().next() {
        let sep = ["::", "->", "#"].into_iter().find(|s| rest.starts_with(s));
        let follows = |s: &str| rest[s.len()..].chars().next().is_some_and(|n| crate::ident::is_continue(n) || n == '~');
        match sep {
            Some(s) if !out.ends_with("operator") && follows(s) => {
                // a leading separator (`::make`) names the top level: nothing before the name
                if !out.is_empty() {
                    out.push('.');
                }
                rest = &rest[s.len()..];
            }
            _ => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// A dotted name with a PHP namespace's `\` as a dot and generic arguments (`<T>`, `<'a, T>`)
/// dropped, so `Stack<T>.push` and `App\Models\User.find` compare with `Stack.push`, `User.find`.
fn plain(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut depth = 0usize;
    for (i, c) in name.char_indices() {
        match c {
            '<' if depth > 0 || (!out.ends_with("operator") && out.ends_with(crate::ident::is_continue) && name[i..].contains('>')) => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            '\\' => out.push('.'),
            _ => out.push(c),
        }
    }
    out
}

impl Searcher {
    pub fn new(ix: Index) -> Searcher {
        Searcher { ix, ver: None, callers_cache: HashMap::new(), cancel: Arc::default(), look: None, cache_dir: crate::packages::env_cache_dir() }
    }

    fn sync(&mut self) -> Result<()> {
        self.ix.lex()?;
        // the database's identity too: one built again in its place can reach the same version
        let ver = Some(format!("{:?} {:?}", self.ix.db_id(), self.ix.meta("data_version")));
        if ver != self.ver {
            self.ver = ver;
            self.callers_cache.clear();
        }
        Ok(())
    }

    fn exact(&self, q: &QueryInfo, limit: usize) -> Result<Vec<i64>> {
        let mut out: Vec<i64> = Vec::new();
        if !q.keys.is_empty() {
            out = self.ix.ids_named(&q.keys, limit)?;
        }
        for d in &q.dotted {
            let last = d.rsplit('.').next().unwrap_or(d);
            for (id, qk) in self.ix.qual_keys_named(last)? {
                if qk == *d || qk.ends_with(&format!(".{d}")) {
                    out.push(id);
                }
            }
        }
        let mut seen = HashSet::new();
        out.retain(|i| seen.insert(*i));
        Ok(out)
    }

    /// chunk id -> (lines mentioning an identifier-like query token, is that token the chunk's name)
    fn literal_hits(lits: &[String], rows: &[ChunkRow]) -> HashMap<i64, (i64, i64)> {
        let pats: Vec<(String, String)> = lits.iter().take(3).map(|l| (l.to_lowercase(), name_key(l))).collect();
        let mut out = HashMap::new();
        for row in rows {
            let low = row.text.to_lowercase();
            let hits: i64 = low.split('\n').map(|ln| pats.iter().filter(|(p, _)| ln.contains(p.as_str())).count() as i64).sum();
            let is_def = pats.iter().any(|(_, k)| row.name_key == *k) as i64;
            if hits > 0 || is_def > 0 {
                out.insert(row.id, (hits, is_def));
            }
        }
        out
    }

    fn ncallers(&mut self, names: &[String]) -> Result<()> {
        let mut need: Vec<&String> = names.iter().filter(|n| !n.is_empty() && !self.callers_cache.contains_key(*n)).collect();
        need.sort();
        need.dedup();
        let got = self.ix.call_counts(&need)?;
        for n in need {
            self.callers_cache.insert(n.clone(), got.get(n).copied().unwrap_or(0));
        }
        Ok(())
    }

    // ------------------------------------------------------------ candidates + features
    pub fn candidates(&mut self, query: &str, use_literal: bool) -> Result<Candidates> {
        self.candidates_in(query, use_literal, 1, None, None)
    }

    /// `candidates` keeping the rows `filter` admits, the lexical and semantic pools drawn from the
    /// chunks `allow` holds and the exact pool `widen` times its size.
    fn candidates_in(
        &mut self,
        query: &str,
        use_literal: bool,
        widen: usize,
        filter: Option<&crate::filter::Compiled>,
        allow: Option<&crate::lex::Allow>,
    ) -> Result<Candidates> {
        let q = QueryInfo::parse(query);
        self.sync()?;
        let keys = lex_keys(&q);
        let models = Arc::clone(&self.ix.models);
        let model = if q.terms.is_empty() || !self.ix.vectors_fit() { None } else { models.semantic()? };
        let qz: Option<Vec<f32>> = model.and_then(|m| m.encode_query(&q.terms));
        let mut found: [Vec<i64>; 3] = Default::default();
        let mut scored: [Vec<f32>; 3] = Default::default();
        for s in Source::ALL {
            (found[s as usize], scored[s as usize]) = self.find(s, &q, &keys, qz.as_deref(), widen, allow)?;
        }
        let want_lit = use_literal && !q.lits.is_empty();
        // within equal scores a source's list is in id order, which follows how the index was
        // written, and it holds every tie at its cut: put those runs in order of place, then cut
        // the list at its bound, so the pool and the rank features do not depend on it
        // the runs of equal scores that start inside the bound: the one at the cut is read whole,
        // those past it are cut whole
        let runs = |sc: &[f32], bound: usize| {
            let mut out = Vec::new();
            let mut at = 0;
            while at < sc.len().min(bound) {
                let n = sc[at..].iter().take_while(|&&v| v == sc[at]).count();
                if n > 1 {
                    out.push(at..at + n);
                }
                at += n;
            }
            out
        };
        let bound = |s: &Source| s.bound().unwrap_or(usize::MAX);
        let mut tied: Vec<i64> = Vec::new();
        for (s, (list, sc)) in Source::ALL.iter().zip(found.iter().zip(&scored)) {
            if sc.len() == list.len() {
                tied.extend(runs(sc, bound(s)).into_iter().flat_map(|r| list[r].to_vec()));
            }
        }
        let places = self.ix.places(&tied)?;
        for (s, (list, sc)) in Source::ALL.iter().zip(found.iter_mut().zip(&scored)) {
            if sc.len() == list.len() {
                for r in runs(sc, bound(s)) {
                    list[r].sort_by(|a, b| places.get(a).cmp(&places.get(b)));
                }
            }
            if let Some(bound) = s.bound() {
                list.truncate(bound);
            }
        }
        let mut ids: Vec<i64> = Vec::new();
        let mut seen = HashSet::new();
        for &i in found.iter().flatten() {
            if seen.insert(i) {
                ids.push(i);
            }
        }
        if ids.is_empty() {
            return Ok(Candidates { q, rows: Vec::new(), x: Vec::new(), found });
        }
        let by_id = self.ix.chunks(&ids)?;
        ids.retain(|i| by_id.get(i).is_some_and(|r| filter.is_none_or(|f| f.admits(&r.path, &r.kind))));
        let rows: Vec<ChunkRow> = ids.iter().map(|i| by_id[i].clone()).collect();
        let n = ids.len();
        let lit = if want_lit { Self::literal_hits(&q.lits, &rows) } else { HashMap::new() };

        let mut x: Vec<Features> = vec![[0f32; NFEAT]; n];
        for s in Source::ALL {
            let Some(f) = s.rank_feature() else { continue };
            let rank: HashMap<i64, usize> = found[s as usize].iter().enumerate().map(|(r, &c)| (c, r + 1)).collect();
            for r in 0..n {
                x[r][f] = rank.get(&ids[r]).map(|&k| 1.0 / (1.0 + k as f32)).unwrap_or(0.0);
            }
        }
        // --- lexical
        let s = self.ix.lex()?.field_scores(&keys, &ids)?;
        for (j, &(lg, ra)) in LEX_FIELDS.iter().enumerate() {
            let mx = s.iter().map(|r| r[j]).fold(f32::NEG_INFINITY, f32::max).max(1e-9);
            for (r, row) in s.iter().enumerate() {
                let col = row[j].max(0.0);
                x[r][lg] = (col as f64).ln_1p() as f32;
                x[r][ra] = col / mx;
            }
        }
        let lx: Vec<f32> = s.iter().map(|r| r[0]).collect();
        let (mean, std) = mean_std(&lx);
        for r in 0..n {
            x[r][fi("lex_z")] = (lx[r] - mean) / (std + 1e-9);
        }
        // --- semantic
        if let Some(z) = &qz {
            let (sa, sb) = self.ix.lex()?.sem_pair(&ids, z)?;
            let mix: Vec<f32> = sa.iter().zip(&sb).map(|(a, b)| 0.5 * (a + b)).collect();
            for (src, dst, zdst) in [(&sa, "sem_a", "sem_a_z"), (&sb, "sem_b", "sem_b_z"), (&mix, "sem_mix", "sem_mix_z")] {
                let (m, sd) = mean_std(src);
                for r in 0..n {
                    x[r][fi(dst)] = src[r];
                    x[r][fi(zdst)] = (src[r] - m) / (sd + 1e-9);
                }
            }
        }
        // --- exactness / name overlap / structure
        let tset: HashSet<&str> = q.terms.iter().map(|s| s.as_str()).collect();
        let exact_set: HashSet<i64> = found[Source::Exact as usize].iter().copied().collect();
        let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        for (r, row) in rows.iter().enumerate() {
            let cid = ids[r];
            let nt: HashSet<&str> = row.name_tok.split_whitespace().collect();
            let inter = nt.intersection(&tset).count();
            let f = &mut x[r];
            f[fi("exact_name")] = (exact_set.contains(&cid) && q.keys.contains(&row.name_key)) as i32 as f32;
            f[fi("exact_qual")] = (exact_set.contains(&cid) && !q.dotted.is_empty()) as i32 as f32;
            f[fi("name_overlap")] = if tset.is_empty() { 0.0 } else { inter as f32 / tset.len() as f32 };
            f[fi("name_cover")] = if nt.is_empty() { 0.0 } else { inter as f32 / nt.len() as f32 };
            f[fi("name_same_set")] = (!nt.is_empty() && nt == tset) as i32 as f32;
            if let Some(&(hits, is_def)) = lit.get(&cid) {
                f[fi("lit_hits")] = (hits as f64).ln_1p() as f32;
                f[fi("lit_def")] = is_def as f32;
            }
            for (i, feature) in ROW_FEATURES {
                f[i] = feature(row);
            }
        }
        // a name the query has as typed, in one Unicode form: a whole token first, punctuation and
        // case included (`valid?` is not `valid`, `BurstFiles` not `burst_files`), then the last
        // part of a dotted token, then a word of letters and digits; when no candidate is spelled
        // as typed, every exact name is as near as it gets
        let nfc = |s: &str| s.nfc().collect::<String>();
        let named: Vec<String> = names.iter().map(|nm| nfc(nm)).collect();
        let spelled = |typed: HashSet<String>| {
            let hits: Vec<bool> = named.iter().map(|nm| !nm.is_empty() && typed.contains(nm)).collect();
            hits.contains(&true).then_some(hits)
        };
        let as_typed = spelled(typed_tokens(&q.text).map(nfc).collect())
            .or_else(|| spelled(typed_tokens(&q.text).map(|t| nfc(typed_tail(t))).collect()))
            .or_else(|| spelled(Words::new(&q.text).map(nfc).collect()));
        for r in 0..names.len() {
            x[r][fi("exact_case")] = match &as_typed {
                Some(hits) => hits[r] as i32 as f32,
                None => x[r][fi("exact_name")],
            };
        }
        self.ncallers(&names)?;
        for (r, nm) in names.iter().enumerate() {
            let c = self.callers_cache.get(nm).copied().unwrap_or(0);
            x[r][fi("log_ncallers")] = (c as f64).ln_1p() as f32;
        }
        // --- query level + file cohesion
        let query_level = QUERY_FEATURES.map(|(i, feature)| (i, feature(&q)));
        let mut top_paths: HashMap<&str, usize> = HashMap::new();
        for cid in found[Source::Lexical as usize].iter().take(30) {
            if let Some(r) = by_id.get(cid) {
                *top_paths.entry(r.path.as_str()).or_insert(0) += 1;
            }
        }
        for (r, row) in rows.iter().enumerate() {
            let f = &mut x[r];
            for (i, v) in query_level {
                f[i] = v;
            }
            f[fi("file_frac")] = top_paths.get(row.path.as_str()).copied().unwrap_or(0) as f32 / 30.0;
        }
        Ok(Candidates { q, rows, x, found })
    }

    /// The ids `source` finds for the query, best first.
    /// A source's candidates, best first, with their scores where the source scores them.
    fn find(
        &mut self,
        source: Source,
        q: &QueryInfo,
        keys: &[i64],
        qz: Option<&[f32]>,
        widen: usize,
        allow: Option<&crate::lex::Allow>,
    ) -> Result<(Vec<i64>, Vec<f32>)> {
        Ok(match (source, qz) {
            (Source::Exact, _) => (self.exact(q, EXACT_LIMIT * widen)?, Vec::new()),
            (Source::Lexical, _) => self.ix.lex()?.search_in(keys, LEX_LIMIT, allow)?,
            (Source::Semantic, Some(z)) => self.ix.lex()?.semantic_scored_in(z, SEM_LIMIT, None, allow)?,
            (Source::Semantic, None) => Default::default(),
        })
    }

    // ---------------------------------------------------------------------- scoring
    /// Scores of candidate feature rows: the ranker's, or the linear blend without one. A ranker
    /// that does not fit this engine's features is an error.
    pub fn score(&self, x: &[Features]) -> Result<Vec<f64>> {
        let n = x.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let lin: Vec<f32> = x.iter().map(linear_score).collect();
        let Some(ranker) = self.ix.models.ranker()? else {
            return Ok(lin.iter().map(|&v| v as f64).collect());
        };
        let keep = prune_indices(&lin, PRUNE);
        let mut out = vec![-1e9f64; n];
        for &i in &keep {
            let row: Vec<f64> = x[i].iter().map(|&v| v as f64).collect();
            out[i] = ranker.predict_row(&row);
        }
        Ok(out)
    }

    pub fn hits(&self, c: &Candidates, scores: &[f64], k: usize, with_text: bool) -> Result<Vec<Hit>> {
        // a name typed with its punctuation (`valid?`, `Get-Greeting`, `list->vector`, the last
        // part of `Rec.valid?`) comes first: no name the ranker was fitted on is written so
        let typed: HashSet<String> = typed_tokens(&c.q.text).flat_map(|t| [t, typed_tail(t)]).filter(|t| punctuated(t)).map(|t| t.nfc().collect()).collect();
        let first: Vec<bool> = c.rows.iter().map(|r| !typed.is_empty() && r.kind != "window" && typed.contains(&r.name.nfc().collect::<String>())).collect();
        // vendored code ranks lower by a fixed margin, so a clearly better vendored match still
        // comes first; a name only it defines, searched for as itself, keeps its place
        let whole = crate::text::name_key(c.q.text.trim());
        let is_vendored = crate::text::Vendored::of(&self.ix.root);
        let vendored: Vec<bool> = c.rows.iter().map(|r| is_vendored.is(&r.path)).collect();
        let named = |r: usize| c.rows[r].kind != "window" && c.rows[r].name_key == whole;
        let project_named = (0..c.rows.len()).any(|r| !vendored[r] && named(r));
        let demoted: Vec<bool> = (0..c.rows.len()).map(|r| vendored[r] && (project_named || !named(r))).collect();
        let ranked: Vec<f64> = (0..c.rows.len()).map(|r| scores[r] - if demoted[r] { VENDORED_MARGIN } else { 0.0 }).collect();
        let mut order: Vec<usize> = (0..c.rows.len()).collect();
        order.sort_by(|&a, &b| {
            // equal scores by place, not by id: ids follow the order the index was written in
            first[b]
                .cmp(&first[a])
                .then(ranked[b].partial_cmp(&ranked[a]).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| c.rows[a].path.cmp(&c.rows[b].path))
                .then_with(|| c.rows[a].start.cmp(&c.rows[b].start))
                .then(c.rows[a].id.cmp(&c.rows[b].id))
        });
        let mut picked: Vec<Hit> = Vec::new();
        let mut best_by_key: HashMap<(String, String, bool), usize> = HashMap::new();
        for r in order {
            let row = &c.rows[r];
            let mut h = Hit::from_row(row, false);
            h.score = scores[r];
            h.why = why(&c.x[r]);
            // a struct and its impl block are two hits
            let key = (h.path.clone(), h.qualname.clone(), h.kind == "impl");
            // a window is a part of the definition it names: one hit per definition, its best part
            let grouped = !h.qualname.is_empty() && (CODE_KINDS.contains(&h.kind.as_str()) || h.kind == "window");
            if grouped && let Some(&pi) = best_by_key.get(&key) {
                // overload / redefinition: keep the longer body
                if h.kind != "window" && picked[pi].kind != "window" && (h.end - h.start) > (picked[pi].end - picked[pi].start) {
                    picked[pi] = h;
                }
                continue;
            }
            if matches!(h.kind.as_str(), "module" | "window") && picked.iter().any(|p| p.path == h.path && !(h.end < p.start || h.start > p.end)) {
                continue;
            }
            if grouped {
                best_by_key.insert(key, picked.len());
            }
            picked.push(h);
            if picked.len() >= k {
                break;
            }
        }
        if with_text {
            self.fill_text(&mut picked)?;
        }
        Ok(picked)
    }

    fn fill_text(&self, hits: &mut [Hit]) -> Result<()> {
        if hits.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
        let mut texts = self.ix.texts(&ids)?;
        for h in hits {
            h.text = texts.remove(&h.id).unwrap_or_default();
        }
        Ok(())
    }

    pub fn search(&mut self, query: &str, k: usize, with_text: bool) -> Result<Vec<Hit>> {
        let c = self.candidates(query, true)?;
        let scores = self.score(&c.x)?;
        self.hits(&c, &scores, k, with_text)
    }

    /// `search` keeping the definitions `filter` admits. Its lexical and semantic pools are drawn
    /// from the chunks the filter can keep, so they hold `k` that pass whenever that many exist. A
    /// part of the filter that keeps at most 1/`NARROW` of the files or the chunks narrows the
    /// pools at once; a broader one, whose ids cost more to gather, first filters the candidates
    /// of a search, and narrows the pools only when fewer than `k` of those pass. The exact pool
    /// is filtered as its rows load, so while fewer than `k` hits pass, the search runs again with
    /// an exact pool `FILTER_WIDEN` times as large, until it comes back short.
    pub fn search_filtered(&mut self, query: &str, k: usize, with_text: bool, filter: &crate::filter::Compiled) -> Result<Vec<Hit>> {
        Ok(self.search_filtered_bounded(query, k, with_text, filter)?.0)
    }

    /// `search_filtered`, and whether the widening stopped at its bound with fewer than `k`.
    pub fn search_filtered_bounded(&mut self, query: &str, k: usize, with_text: bool, filter: &crate::filter::Compiled) -> Result<(Vec<Hit>, bool)> {
        let files = if filter.has_files() { self.ix.files()? } else { Vec::new() };
        let parts = self.parts(filter, &files)?;
        if parts.iter().any(Part::keeps_nothing) {
            return Ok((Vec::new(), false));
        }
        let narrow = self.allow_of(parts.iter().filter(|p| p.narrow()))?;
        if parts.iter().any(|p| !p.narrow()) {
            let c = self.candidates_in(query, true, 1, Some(filter), narrow.as_ref())?;
            let scores = self.score(&c.x)?;
            let hits = self.hits(&c, &scores, k, with_text)?;
            if hits.len() >= k {
                return Ok((hits, false));
            }
        }
        let broad = self.allow_of(parts.iter().filter(|p| !p.narrow()))?;
        let allow = match (narrow, broad) {
            (Some(a), Some(b)) => Some(a.and(b)),
            (a, b) => a.or(b),
        };
        let run = |widen: usize| -> Result<(Vec<Hit>, bool)> {
            let c = self.candidates_in(query, true, widen, Some(filter), allow.as_ref())?;
            let short = c.found[Source::Exact as usize].len() < EXACT_LIMIT * widen;
            let scores = self.score(&c.x)?;
            Ok((self.hits(&c, &scores, k, with_text)?, short))
        };
        let ((hits, _), at_bound) = crate::ops::grow_until(1, FILTER_WIDEN, FILTER_WIDEN_MAX, run, |(hits, short), _| hits.len() >= k || *short)?;
        Ok((hits, at_bound))
    }

    /// The parts of `filter`'s allow set: the files of `files` whose names it can keep, and the
    /// chunks of its kinds.
    fn parts<'a>(&mut self, filter: &'a crate::filter::Compiled, files: &'a [String]) -> Result<Vec<Part<'a>>> {
        let mut parts = Vec::new();
        if filter.has_files() {
            let (kept, out) = filter.split_by_name(files);
            parts.push(Part::Files { kept, out });
        }
        if filter.has_kinds() {
            let kinds = filter.kinds();
            let some = self.ix.kind_count(&kinds)?;
            let all = if some == 0 { 0 } else { self.ix.lex()?.alive_counts().iter().sum() };
            parts.push(Part::Kinds { kinds, some, all });
        }
        Ok(parts)
    }

    /// The allow set of `parts`, each built from the fewer ids, the ones kept or the ones left out.
    fn allow_of<'p, 'a: 'p>(&self, parts: impl Iterator<Item = &'p Part<'a>>) -> Result<Option<crate::lex::Allow>> {
        use crate::lex::Allow;
        let mut allow: Option<Allow> = None;
        for p in parts {
            let part = match p {
                Part::Files { kept, out } if kept.len() <= out.len() => Allow::of(&self.ix.chunk_ids_in(kept)?),
                Part::Files { out, .. } => Allow::except(&self.ix.chunk_ids_in(out)?),
                Part::Kinds { kinds, some, all } if some * 2 <= *all => Allow::of(&self.ix.chunk_ids_of_kinds(kinds)?),
                Part::Kinds { kinds, .. } => {
                    let others: Vec<&str> = crate::filter::kinds().into_iter().filter(|k| !kinds.contains(k)).collect();
                    Allow::except(&self.ix.chunk_ids_of_kinds(&others)?)
                }
            };
            allow = Some(match allow {
                Some(a) => a.and(part),
                None => part,
            });
        }
        Ok(allow)
    }

    /// Several phrasings of one need (synonyms, guessed identifiers): rank-fuse the results.
    pub fn search_many(&mut self, queries: &[String], k: usize, with_text: bool) -> Result<Vec<Hit>> {
        self.search_many_in(queries, k, with_text, None)
    }

    /// `search_many`, each phrasing searched with `search_filtered` when there is a filter.
    pub fn search_many_in(&mut self, queries: &[String], k: usize, with_text: bool, filter: Option<&crate::filter::Compiled>) -> Result<Vec<Hit>> {
        Ok(self.search_many_bounded(queries, k, with_text, filter)?.0)
    }

    /// `search_many_in`, and whether a filtered search of one query stopped at its bound.
    pub fn search_many_bounded(&mut self, queries: &[String], k: usize, with_text: bool, filter: Option<&crate::filter::Compiled>) -> Result<(Vec<Hit>, bool)> {
        let mut at_bound = false;
        let mut fused: HashMap<i64, f64> = HashMap::new();
        let mut meta: HashMap<i64, Hit> = HashMap::new();
        let mut first_seen: Vec<i64> = Vec::new();
        for q in queries {
            if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                anyhow::bail!("cancelled");
            }
            let found = match filter {
                Some(f) => {
                    let (found, bound) = self.search_filtered_bounded(q, 30, false, f)?;
                    at_bound |= bound;
                    found
                }
                None => self.search(q, 30, false)?,
            };
            for (r, h) in found.into_iter().enumerate() {
                *fused.entry(h.id).or_insert(0.0) += 1.0 / (60.0 + (r + 1) as f64);
                meta.entry(h.id).or_insert_with(|| {
                    first_seen.push(h.id);
                    h
                });
            }
        }
        let mut ids: Vec<i64> = first_seen;
        ids.sort_by(|a, b| fused[b].partial_cmp(&fused[a]).unwrap().then_with(|| (&meta[a].path, meta[a].start).cmp(&(&meta[b].path, meta[b].start))));
        ids.truncate(k);
        let mut out: Vec<Hit> = ids
            .iter()
            .map(|i| {
                let mut h = meta.remove(i).unwrap();
                h.score = fused[i];
                h
            })
            .collect();
        if with_text {
            self.fill_text(&mut out)?;
        }
        Ok((out, at_bound))
    }

    // ----------------------------------------------------------------- navigation
    /// The definitions `name` names. Tried in order, the first that finds any wins: the name as
    /// typed (whole stored name, qualified name, or a tail of one); its dotted form (`::`, `->`
    /// and `#` as dots); the same compared with stored qualified names whose owner loses its
    /// generics and path (`Stack<T>.push`, `crate::Thing.run`), either side a tail of the other;
    /// last, for a name written as a path, a top-level definition of its last part
    /// (`crate::util::helper` finds `helper`).
    pub fn find_symbol(&self, name: &str, limit: usize) -> Result<Vec<Hit>> {
        self.find_symbol_where(name, limit, None, None)
    }

    /// `find_symbol` in the file `path` when one is given, keeping only the definitions `keep`
    /// admits, before `limit` cuts.
    pub fn find_symbol_where(&self, name: &str, limit: usize, path: Option<&str>, keep: Option<&dyn Fn(&ChunkRow) -> bool>) -> Result<Vec<Hit>> {
        // a Rust raw identifier `r#type` is indexed as `type`
        let name = name.trim();
        let unraw = name.strip_prefix("r#").unwrap_or(name).replace("::r#", "::").replace(".r#", ".");
        let typed = unraw.as_str();
        let d = dotted(typed);
        let d = d.trim_start_matches('.');
        let rows = |n: &str| -> Result<Vec<Hit>> {
            let found = match (keep, path) {
                (Some(keep), path) => self.ix.symbols_named_where(n, limit, &|r| path.is_none_or(|p| r.path == p) && keep(r))?,
                (None, Some(path)) => self.ix.symbols_named_in(n, path, limit)?,
                (None, None) => self.ix.symbols_named(n, limit)?,
            };
            Ok(found.iter().map(|r| Hit::from_row(r, true)).collect())
        };
        let mut out = rows(typed)?;
        if out.is_empty() && d != typed {
            out = rows(d)?;
        }
        let q = name_key(&plain(d));
        let Some((_, last)) = q.rsplit_once('.') else { return Ok(out) };
        let typed_last = plain(d).rsplit('.').next().unwrap_or_default().to_string();
        let cands = self.ix.qualnames_named(last, &typed_last)?;
        let stored: Vec<(i64, String)> = cands.into_iter().map(|(id, qn)| (id, name_key(&plain(&dotted(&qn))))).collect();
        let tail = |a: &str, b: &str| a == b || a.ends_with(&format!(".{b}"));
        let ids: Vec<i64> = if !out.is_empty() {
            // an out-of-line `Box<T>::put` beside the `Box.put` declaration the lookup found
            stored.iter().filter(|(id, s)| *s == q && !out.iter().any(|h| h.id == *id)).map(|c| c.0).collect()
        } else {
            let owned: Vec<i64> = stored.iter().filter(|(_, s)| s.contains('.') && (tail(s, &q) || tail(&q, s))).map(|c| c.0).collect();
            // only a path (`::`, `\`) can name a module; `obj.helper` says nothing of a free `helper`
            if owned.is_empty() && (typed.contains("::") || typed.contains('\\')) {
                stored.iter().filter(|(_, s)| !s.contains('.')).map(|c| c.0).collect()
            } else {
                owned
            }
        };
        for id in ids {
            if out.len() >= limit {
                break;
            }
            if let Some(r) = self.ix.chunk(id)?.filter(|r| path.is_none_or(|p| r.path == p) && keep.is_none_or(|keep| keep(r))) {
                out.push(Hit::from_row(&r, true));
            }
        }
        Ok(out)
    }

    /// Chunks whose stored name or qualified name is `name`, or whose qualified name ends in it.
    pub(crate) fn symbol_rows(&self, name: &str, limit: usize) -> Result<Vec<Hit>> {
        Ok(self.ix.symbols_named(name, limit)?.iter().map(|r| Hit::from_row(r, true)).collect())
    }

    /// The definitions that call what `name` stands for, as the `callers` tool answers.
    pub fn callers(&self, name: &str, limit: usize) -> Result<Vec<Hit>> {
        Ok(crate::names::Names::new(self)?.callers(name, limit)?.0)
    }

    /// -> (resolved definitions, unresolved external names), as the `callees` tool answers.
    pub fn callees(&self, name: &str, limit: usize) -> Result<(Vec<Hit>, Vec<String>)> {
        crate::names::Names::new(self)?.callees(name, limit).map(|(resolved, external, _)| (resolved, external))
    }
}

/// A part of a filter's allow set.
enum Part<'a> {
    /// the indexed files whose names the filter can keep, and the rest
    Files { kept: Vec<&'a str>, out: Vec<&'a str> },
    /// the filter's kinds, how many chunks have them, and how many chunks there are
    Kinds { kinds: Vec<&'a str>, some: usize, all: usize },
}

impl Part<'_> {
    fn keeps_nothing(&self) -> bool {
        match self {
            Part::Files { kept, .. } => kept.is_empty(),
            Part::Kinds { some, .. } => *some == 0,
        }
    }

    fn narrow(&self) -> bool {
        match self {
            Part::Files { kept, out } => kept.len() * NARROW <= kept.len() + out.len(),
            Part::Kinds { some, all, .. } => some * NARROW <= *all,
        }
    }
}

fn mean_std(v: &[f32]) -> (f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let n = v.len() as f64;
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n;
    let var = v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / n;
    (mean as f32, var.sqrt() as f32)
}

/// Human-readable evidence for a hit, from its feature vector.
pub fn why(x: &Features) -> Vec<(String, serde_json::Value)> {
    let f = |n: &str| x[fi(n)];
    let r2 = |v: f32| serde_json::json!(((v as f64) * 100.0).round() / 100.0);
    let mut out = Vec::new();
    if f("exact_name") > 0.0 || f("exact_qual") > 0.0 {
        out.push(("exact name".to_string(), serde_json::json!(1)));
    }
    for (label, key) in [("name", "name_ratio"), ("signature", "sig_ratio"), ("docs", "doc_ratio"), ("body", "body_ratio"), ("path", "path_ratio")] {
        if f(key) >= 0.5 {
            out.push((format!("{label} keywords"), r2(f(key))));
        }
    }
    if f("sem_mix") >= 0.25 {
        out.push(("semantic".to_string(), r2(f("sem_mix"))));
    }
    if f("lit_hits") > 0.0 {
        out.push(("literal match".to_string(), serde_json::json!(((f("lit_hits") as f64).exp_m1()).round() as i64)));
    }
    out
}

pub fn prune_indices(lin: &[f32], n: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..lin.len()).collect();
    if idx.len() > n {
        idx.select_nth_unstable_by(n - 1, |&a, &b| lin[b].partial_cmp(&lin[a]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
        idx.truncate(n);
    }
    idx
}

/// Hand-set blend used to pick the ranker's candidates (and as the score without a ranker).
pub fn linear_score(x: &Features) -> f32 {
    let f = |n: &str| x[fi(n)];
    f("lex_z") + 0.9 * f("sem_mix_z") + 2.5 * f("exact_name") + 1.5 * f("exact_qual") + 1.2 * f("lit_def") + 0.3 * f("lit_hits") + 1.0 * f("name_overlap")
        - 0.4 * f("is_test")
}
