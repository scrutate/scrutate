//! Engine A: universal instant search. No index, no parsers, no background process.
//!
//! Files are walked in parallel with the same ignore rules as ripgrep (the `ignore` crate: .gitignore,
//! .ignore, hidden files, binary detection), searched line by line with ripgrep's `grep-searcher` and,
//! for literal / regex / identifier queries, Rust's linear-time regex engine (no catastrophic
//! backtracking; compiled size is capped). Two capabilities go beyond grep-class tools:
//! * identifier mode: one spelling finds every casing style (`refreshToken` = `refresh_token` = `REFRESH_TOKEN`);
//! * fuzzy mode: matches within an edit distance (Myers' bit-parallel algorithm, O(n) per file).
//!
//! One way in, `search`: a `Query` (what to match, how to report it), a `Files` (a walk with its
//! rules, or a list) and a cancel flag the search reads and never sets. Results stream out as they
//! are found: `Record`s, each of one kind, or plain lines written by the search threads.

// wasi's `schedule` leaves out the thread rules and the code that starts threads
#![cfg_attr(target_os = "wasi", allow(dead_code))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use grep_matcher::{LineTerminator, Match as MatchSpan, Matcher, NoCaptures, NoError};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{overrides::OverrideBuilder, WalkBuilder, WalkState};

#[path = "foldfind.rs"]
mod foldfind;
#[path = "policy.rs"]
mod policy;
#[path = "source.rs"]
mod source;
pub use policy::MAX_THREADS;
use policy::{Machine, Policy, Shown, System};
use source::{file_worker, FileWorker};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Regex,
    Literal,
    /// `refreshToken` matches refresh_token / RefreshToken / REFRESH-TOKEN
    Identifier,
    /// literal within `max_edits` insertions, deletions or substitutions (pattern up to 64 bytes)
    Fuzzy {
        max_edits: u8,
    },
    /// PCRE2: look-around and backreferences, by backtracking within PCRE2's match limit
    Pcre2,
    /// a regex within `max_edits` errors (byte-oriented, at most 64 character positions)
    FuzzyRegex {
        max_edits: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Sensitive,
    Insensitive,
    /// insensitive unless the pattern contains an uppercase letter
    Smart,
}

/// What to match.
#[derive(Clone, Debug)]
pub struct Pattern {
    pub text: String,
    pub mode: Mode,
    pub case: Case,
    pub word: bool,
    pub multiline: bool,
    /// regex size limits (resource exhaustion protection)
    pub regex_size_limit: usize,
    pub dfa_size_limit: usize,
}

impl Default for Pattern {
    fn default() -> Self {
        Pattern {
            text: String::new(),
            mode: Mode::Regex,
            case: Case::Sensitive,
            word: false,
            multiline: false,
            regex_size_limit: 20 * (1 << 20),
            dfa_size_limit: 10 * (1 << 20),
        }
    }
}

/// What a search reports for each file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Report {
    /// its matching lines (and context lines): `Record::Match`, `Record::Context`, `Record::Note`
    #[default]
    Lines,
    /// how many lines match: one `Record::Count`; no line text leaves the worker threads
    Counts,
    /// that it matches: one `Record::File`, after its first match
    Files,
}

/// What a search asks for: what to match and how to report it.
#[derive(Clone, Debug)]
pub struct Query {
    pub pattern: Pattern,
    /// the lines that do not match are the matches
    pub invert: bool,
    pub report: Report,
    /// context lines before and after each match
    pub before: usize,
    pub after: usize,
    /// matches of one file at most
    pub max_count: Option<u64>,
    /// matches of the search at most: it stops there
    pub max_results: Option<u64>,
    /// the records arrive in the order of `rg --sort path`: the roots as given, each one's files by
    /// their path compared part by part (`a/b` before `a-b`), then by line; with `max_results`
    /// they are exactly the first `max_results` matches in that order, the same on every run
    /// (files are listed and sorted first, and searched in that order until enough matches are
    /// in). Off, a capped search keeps whichever matches it finds first. Needs `Output::Records`
    pub first_in_path_order: bool,
    /// collect bytes searched (one stat per file; only when statistics are wanted)
    pub want_bytes: bool,
    /// compute the byte ranges of the matches inside each line (colour / JSON output); off, the
    /// line is reported without a second pass of the matcher over it
    pub want_spans: bool,
    /// the ranges and `Line::column` count bytes of `Line::text` (invalid UTF-8 shown as U+FFFD,
    /// no line end) instead of bytes of the line as read
    pub text_spans: bool,
    /// archive member names are reported as they are (for JSON, which escapes them); off, with
    /// control characters shown as escapes (`fmt::shown`), so a name cannot forge an output line
    pub raw_member_names: bool,
    /// plain output starts each line with the file's path (`-H`); off, `line:text` as for one file
    pub with_filename: bool,
    /// plain output has line numbers (`-N` turns them off; records always carry them)
    pub line_numbers: bool,
}

impl Default for Query {
    fn default() -> Self {
        Query {
            pattern: Pattern::default(),
            invert: false,
            report: Report::Lines,
            before: 0,
            after: 0,
            max_count: None,
            max_results: None,
            first_in_path_order: false,
            want_bytes: false,
            want_spans: false,
            text_spans: false,
            raw_member_names: false,
            with_filename: true,
            line_numbers: true,
        }
    }
}

/// Where the files of a search come from.
#[derive(Clone, Debug)]
pub enum Paths {
    /// the files under these roots that the walk rules of `Files` let through
    Walk(Vec<PathBuf>),
    /// these files, relative to `root` (as an index lists them); the globs and types of `Files`
    /// still apply, its walk rules do not
    List { root: PathBuf, files: Vec<String> },
    /// text held in memory, each reported under its name and searched as a file of those bytes
    /// would be (an editor's unsaved buffer, a file at a revision); one after another in the
    /// order given, by name with `Query::first_in_path_order`. No glob, type or walk rule applies
    Memory(Vec<(String, Vec<u8>)>),
}

/// Which files a search reads, and how many threads read them.
#[derive(Clone, Debug)]
pub struct Files {
    pub paths: Paths,
    pub hidden: bool,
    pub no_ignore: bool,
    pub follow_links: bool,
    pub max_filesize: Option<u64>,
    pub max_depth: Option<usize>,
    pub globs: Vec<String>,
    /// globs matched from one directory, whatever path is searched (a configuration file's)
    pub scoped_globs: Option<Arc<ScopedGlobs>>,
    /// language ids / type names from the registry (`py`, `rust`, ...); negative with a leading
    /// `!`. Matched on the file name after the ignore rules, as ripgrep's `--type` and `--type-not`
    pub types: Vec<String>,
    /// search the members of zip/tar archives and the contents of compressed files (`-z`)
    pub search_zip: bool,
    /// report each path as the walk reached it from the root it was given (`dir/a.rs`, `./a.rs`),
    /// as grep does; otherwise relative to the root. Also as ripgrep: a root that is a file is
    /// searched whatever its bytes (a NUL byte ends a line, and line output stops at the first
    /// match after one, with a `Record::Note`), and the root `-` is standard input
    pub paths_as_given: bool,
    /// the process's current directory as `std::env::current_dir` gave it, when the caller has it:
    /// the walk then does not look it up again (0.17 M instructions on macOS)
    pub cwd: Option<PathBuf>,
    pub threads: usize,
    /// files the one-thread walk takes before the parallel walk does the rest (0 skips it); `None`
    /// chooses from the roots (`small_tree_files`)
    pub small_tree: Option<usize>,
    /// files at least this large are searched as several ranges of whole lines at once (an eighth of
    /// it each): one huge file would otherwise keep a single core busy while the others idle
    pub split_file_bytes: u64,
    /// the custom languages `-t` names, when not those `custom::current` gives (a caller that
    /// searches for one repository among several in the process)
    pub customs: Option<Arc<crate::custom::Customs>>,
}

impl Default for Files {
    fn default() -> Self {
        Files {
            paths: Paths::Walk(Vec::new()),
            hidden: false,
            no_ignore: false,
            follow_links: false,
            max_filesize: None,
            max_depth: None,
            globs: Vec::new(),
            scoped_globs: None,
            types: Vec::new(),
            search_zip: false,
            paths_as_given: false,
            cwd: None,
            threads: 0,
            small_tree: None,
            split_file_bytes: 64 << 20,
            customs: None,
        }
    }
}

impl Files {
    /// The files under `roots`, by the default walk rules.
    pub fn walk(roots: Vec<PathBuf>) -> Files {
        Files { paths: Paths::Walk(roots), ..Files::default() }
    }

    /// These files, relative to `root`.
    pub fn list(root: PathBuf, files: Vec<String>) -> Files {
        Files { paths: Paths::List { root, files }, ..Files::default() }
    }
}

/// One line of a file. `line` is 1-based; `column` is the 1-based byte offset of the first match
/// in the line (0 when not applicable, e.g. inverted matches), in `text` with `Query::text_spans`.
#[derive(Clone, Debug)]
pub struct Line {
    /// shared by every record of one file (a refcount per line instead of a string copy)
    pub path: Arc<str>,
    /// the bytes of `path` when it is not UTF-8 (on Unix; `path` then shows U+FFFD in their place)
    pub path_bytes: Option<Arc<[u8]>>,
    pub line: u64,
    pub column: u64,
    pub text: String,
    /// byte ranges of the matches: within `text` with `Query::text_spans`, else within the line
    /// as read (they differ where it holds invalid UTF-8, and a match may end in its line break)
    pub spans: Vec<(usize, usize)>,
}

/// One result of a search; `Query::report` decides which kinds it gives.
#[derive(Clone, Debug)]
pub enum Record {
    Match(Line),
    /// a line around a match (`Query::before`, `Query::after`)
    Context(Line),
    /// the matching lines of one file
    Count {
        path: Arc<str>,
        path_bytes: Option<Arc<[u8]>>,
        count: u64,
    },
    /// a file with a match
    File {
        path: Arc<str>,
        path_bytes: Option<Arc<[u8]>>,
    },
    /// a note on a file, after its lines: a named file with a NUL byte matched
    /// (`Files::paths_as_given`), or the search of a walked file that matched stopped at a NUL byte
    Note {
        path: Arc<str>,
        path_bytes: Option<Arc<[u8]>>,
        text: String,
    },
}

impl Record {
    pub fn path(&self) -> &Arc<str> {
        match self {
            Record::Match(l) | Record::Context(l) => &l.path,
            Record::Count { path, .. } | Record::File { path, .. } | Record::Note { path, .. } => path,
        }
    }

    pub fn path_bytes(&self) -> Option<&[u8]> {
        match self {
            Record::Match(l) | Record::Context(l) => l.path_bytes.as_deref(),
            Record::Count { path_bytes, .. } | Record::File { path_bytes, .. } | Record::Note { path_bytes, .. } => path_bytes.as_deref(),
        }
    }

    /// Where the record sorts among those of its file: lines by number, a note after them.
    pub fn order(&self) -> u64 {
        match self {
            Record::Match(l) | Record::Context(l) => l.line,
            Record::Note { .. } => u64::MAX,
            Record::Count { .. } | Record::File { .. } => 0,
        }
    }
}

/// Where the results of a search go.
pub enum Output<'a> {
    /// every record, to this function on the calling thread, as found (in path order with
    /// `Query::first_in_path_order`)
    Records(&'a mut dyn FnMut(Record)),
    /// plain `path:line:text` output, written by the search threads: no record is made
    Plain(Arc<PlainSink>),
}

/// The path a search reports for standard input.
pub const STDIN_LABEL: &str = "<stdin>";

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Stats {
    pub files_searched: u64,
    pub files_matched: u64,
    pub matches: u64,
    pub bytes_searched: u64,
    pub errors: u64,
    pub elapsed_ms: f64,
    pub cancelled: bool,
    /// the first few files that could not be searched (or not to the end), with the reason
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
}

// ------------------------------------------------------------------------- fuzzy matcher
/// Approximate matching of a literal (Myers 1999, bit-parallel). Lines are never spanned. An edit
/// is one character: an ASCII byte, a UTF-8 encoded character, or a byte of invalid UTF-8 (which
/// no pattern character equals). A pigeonhole prefilter (one of k+1 pieces of the pattern must
/// occur verbatim) finds candidate lines with a multi-substring scan, so only those lines pay for
/// the edit-distance computation.
#[derive(Clone)]
pub struct FuzzyMatcher {
    fwd: Peq,
    /// the pattern reversed, for the start of a match
    rev: Peq,
    m: usize,
    k: usize,
    prefilter: Option<aho_corasick::AhoCorasick>,
    /// patterns longer than 128 characters: the pattern and its reverse in 64-bit blocks
    long: Option<Box<(Blocks, Blocks)>>,
}

/// The longest fuzzy pattern accepted, in characters (the work per character grows with its length / 64).
pub const FUZZY_MAX: usize = 4096;

/// The non-ASCII character at `hay[i]` and its length, or `None` and 1 for a byte of invalid UTF-8.
#[inline]
pub(crate) fn wide_char(hay: &[u8], i: usize) -> (Option<char>, usize) {
    let n = match hay[i] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return (None, 1),
    };
    match hay.get(i..i + n).and_then(|b| std::str::from_utf8(b).ok()) {
        Some(s) => (s.chars().next(), n),
        None => (None, 1),
    }
}

#[inline]
fn unit_len(hay: &[u8], i: usize) -> usize {
    if hay[i] < 0x80 {
        1
    } else {
        wide_char(hay, i).1
    }
}

/// The characters equal to `c` under case folding: the simple case folding of the regex engine,
/// so `-i` means the same with and without `--fuzzy`.
fn fold_class(c: char) -> Vec<char> {
    use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};
    let mut cls = ClassUnicode::new([ClassUnicodeRange::new(c, c)]);
    if cls.try_case_fold_simple().is_err() {
        return vec![c];
    }
    cls.iter().flat_map(|r| (r.start() as u32..=r.end() as u32).filter_map(char::from_u32)).collect()
}

/// For each pattern position, the characters that match it.
fn fuzzy_classes(pattern: &str, fold: bool) -> Vec<Vec<char>> {
    pattern.chars().map(|c| if fold { fold_class(c) } else { vec![c] }).collect()
}

/// Strings of which every match of the fuzzy pattern holds one verbatim, up to ASCII case when
/// `fold`: `max_edits` edits change at most that many of `max_edits + 1` pieces of the pattern.
/// With `fold` a piece is given in each spelling that differs beyond ASCII case (`é`/`É`, `k` and
/// the Kelvin sign). None when the pieces would be empty or there would be more than 64 strings.
pub fn fuzzy_pieces(pattern: &str, max_edits: u8, fold: bool) -> Option<Vec<Vec<u8>>> {
    let classes = fuzzy_classes(pattern, fold);
    let n = max_edits as usize + 1;
    let len = classes.len() / n;
    if len == 0 {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for i in 0..n {
        let mut spellings = vec![String::new()];
        for cls in &classes[i * len..if i == n - 1 { classes.len() } else { (i + 1) * len }] {
            let mut alts: Vec<char> = cls.iter().copied().filter(|c| !c.is_ascii()).collect();
            alts.extend(cls.iter().copied().find(char::is_ascii));
            spellings = spellings.iter().flat_map(|s| alts.iter().map(move |a| format!("{s}{a}"))).collect();
            if out.len() + spellings.len() > 64 {
                return None;
            }
        }
        out.extend(spellings);
    }
    Some(out.into_iter().map(String::into_bytes).collect())
}

/// Myers' match masks of a pattern of up to 128 characters: bit j is set for the characters that
/// match position j.
#[derive(Clone)]
struct Peq {
    ascii: [u128; 128],
    /// sorted by character
    wide: Vec<(char, u128)>,
}

impl Peq {
    fn new(classes: &[Vec<char>]) -> Peq {
        let mut p = Peq { ascii: [0; 128], wide: Vec::new() };
        for (j, cls) in classes.iter().enumerate().take(128) {
            for &c in cls {
                if c.is_ascii() {
                    p.ascii[c as usize] |= 1 << j;
                } else if let Some(e) = p.wide.iter_mut().find(|e| e.0 == c) {
                    e.1 |= 1 << j;
                } else {
                    p.wide.push((c, 1 << j));
                }
            }
        }
        p.ascii[b'\n' as usize] = 0;
        p.wide.sort_by_key(|e| e.0);
        p
    }

    /// The mask of the character at `hay[i]` and its length.
    #[inline]
    fn at(&self, hay: &[u8], i: usize) -> (u128, usize) {
        let b = hay[i];
        if b < 0x80 {
            return (self.ascii[b as usize], 1);
        }
        let (c, n) = wide_char(hay, i);
        (c.and_then(|c| self.wide.binary_search_by_key(&c, |e| e.0).ok()).map_or(0, |x| self.wide[x].1), n)
    }
}

/// Myers' recurrence on 64-bit blocks, for patterns longer than one machine word: the addition
/// and the two shifts carry from block to block (Hyyrö 2003).
#[derive(Clone)]
struct Blocks {
    words: usize,
    m: usize,
    /// `words` masks per ASCII byte
    ascii: Vec<u64>,
    /// the non-ASCII characters of the pattern, sorted; `rows` holds `words` masks for each
    wide: Vec<char>,
    rows: Vec<u64>,
    zero: Vec<u64>,
}

impl Blocks {
    fn new(classes: &[Vec<char>]) -> Blocks {
        let words = classes.len().div_ceil(64);
        let mut ascii = vec![0u64; 128 * words];
        let mut wide: std::collections::BTreeMap<char, Vec<u64>> = Default::default();
        for (j, cls) in classes.iter().enumerate() {
            for &c in cls {
                if c.is_ascii() {
                    ascii[c as usize * words + j / 64] |= 1 << (j % 64);
                } else {
                    wide.entry(c).or_insert_with(|| vec![0; words])[j / 64] |= 1 << (j % 64);
                }
            }
        }
        for w in 0..words {
            ascii[b'\n' as usize * words + w] = 0;
        }
        let (wide, rows): (Vec<char>, Vec<Vec<u64>>) = wide.into_iter().unzip();
        Blocks { words, m: classes.len(), ascii, wide, rows: rows.concat(), zero: vec![0; words] }
    }

    /// The masks of the character at `hay[i]` and its length.
    #[inline]
    fn eq_at(&self, hay: &[u8], i: usize) -> (&[u64], usize) {
        let b = hay[i];
        if b < 0x80 {
            let r = b as usize * self.words;
            return (&self.ascii[r..r + self.words], 1);
        }
        let (c, n) = wide_char(hay, i);
        match c.and_then(|c| self.wide.binary_search(&c).ok()) {
            Some(r) => (&self.rows[r * self.words..(r + 1) * self.words], n),
            None => (&self.zero, n),
        }
    }

    fn start(&self) -> (Vec<u64>, Vec<u64>) {
        let mut pv = vec![u64::MAX; self.words];
        if !self.m.is_multiple_of(64) {
            pv[self.words - 1] = (1u64 << (self.m % 64)) - 1;
        }
        (pv, vec![0; self.words])
    }

    /// One text character (its masks `eq`): updates the column and returns the change of the last
    /// row's distance.
    fn step(&self, pv: &mut [u64], mv: &mut [u64], eq: &[u64]) -> isize {
        let last = self.words - 1;
        let top = 1u64 << ((self.m - 1) % 64);
        let (mut carry, mut ph_in, mut mh_in, mut delta) = (0u64, 0u64, 0u64, 0isize);
        for w in 0..self.words {
            let eq = eq[w];
            let xv = eq | mv[w];
            let (s1, o1) = (eq & pv[w]).overflowing_add(pv[w]);
            let (s2, o2) = s1.overflowing_add(carry);
            carry = (o1 | o2) as u64;
            let xh = (s2 ^ pv[w]) | eq;
            let ph = mv[w] | !(xh | pv[w]);
            let mh = pv[w] & xh;
            if w == last {
                delta = if ph & top != 0 {
                    1
                } else if mh & top != 0 {
                    -1
                } else {
                    0
                };
            }
            let ph_s = (ph << 1) | ph_in;
            let mh_s = (mh << 1) | mh_in;
            ph_in = ph >> 63;
            mh_in = mh >> 63;
            pv[w] = mh_s | !(xv | ph_s);
            mv[w] = ph_s & xv;
        }
        if !self.m.is_multiple_of(64) {
            let mask = (1u64 << (self.m % 64)) - 1;
            pv[last] &= mask;
            mv[last] &= mask;
        }
        delta
    }
}

/// One step of Myers' recurrence on a pattern of up to 128 characters (`top` and `mask` from its
/// length): updates `pv` and `mv` and returns the change of the distance.
#[inline]
fn myers_step(eq: u128, pv: &mut u128, mv: &mut u128, top: u128, mask: u128) -> isize {
    let xv = eq | *mv;
    let xh = (((eq & *pv).wrapping_add(*pv)) ^ *pv) | eq;
    let mut ph = *mv | !(xh | *pv);
    let mut mh = *pv & xh;
    let delta = if ph & top != 0 {
        1
    } else if mh & top != 0 {
        -1
    } else {
        0
    };
    ph = (ph << 1) & mask;
    mh = (mh << 1) & mask;
    *pv = (mh | !(xv | ph)) & mask;
    *mv = ph & xv;
    delta
}

impl FuzzyMatcher {
    pub fn new(pattern: &str, max_edits: u8, fold_case: bool) -> Result<FuzzyMatcher> {
        let classes = fuzzy_classes(pattern, fold_case);
        let m = classes.len();
        if m == 0 || m > FUZZY_MAX {
            bail!("fuzzy patterns must be 1..={FUZZY_MAX} characters");
        }
        if max_edits as usize >= m {
            bail!("the number of edits ({max_edits}) must be smaller than the pattern length ({m} characters)");
        }
        let reversed: Vec<Vec<char>> = classes.iter().rev().cloned().collect();
        let prefilter = fuzzy_pieces(pattern, max_edits, fold_case)
            .filter(|ps| ps.iter().all(|p| p.len() >= 2))
            .and_then(|ps| aho_corasick::AhoCorasick::builder().ascii_case_insensitive(fold_case).build(ps).ok());
        let long = (m > 128).then(|| Box::new((Blocks::new(&classes), Blocks::new(&reversed))));
        Ok(FuzzyMatcher { fwd: Peq::new(&classes), rev: Peq::new(&reversed), m, k: max_edits as usize, prefilter, long })
    }

    /// Byte range of the line containing `pos` within `hay[at..]`.
    fn line_bounds(hay: &[u8], at: usize, pos: usize) -> (usize, usize) {
        let start = hay[at..pos].iter().rposition(|&b| b == b'\n').map(|i| at + i + 1).unwrap_or(at);
        let end = hay[pos..].iter().position(|&b| b == b'\n').map(|i| pos + i).unwrap_or(hay.len());
        (start, end)
    }

    /// First match end in `hay[at..]`: candidate lines from the prefilter, verified with Myers.
    fn first_end_filtered(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        let Some(ac) = &self.prefilter else {
            return self.first_end(hay, at);
        };
        let mut from = at;
        while from < hay.len() {
            let found = ac.find(&hay[from..])?;
            let (ls, le) = Self::line_bounds(hay, from, from + found.start());
            if let Some((end, d)) = self.first_end(&hay[..le], ls) {
                return Some((end, d));
            }
            from = le + 1;
        }
        None
    }

    /// End positions (exclusive) in `hay[at..]` where a match with <= k edits ends, with its distance.
    fn first_end(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        let mut i = at;
        if let Some(b) = &self.long {
            let (fwd, _) = &**b;
            let (mut pv, mut mv) = fwd.start();
            let mut score = self.m as isize;
            while i < hay.len() {
                if hay[i] == b'\n' {
                    (pv, mv) = fwd.start();
                    score = self.m as isize;
                    i += 1;
                    continue;
                }
                let (eq, n) = fwd.eq_at(hay, i);
                score += fwd.step(&mut pv, &mut mv, eq);
                i += n;
                if score <= self.k as isize {
                    return Some((i, score as usize));
                }
            }
            return None;
        }
        let m = self.m;
        let top = 1u128 << (m - 1);
        let mask = if m == 128 { u128::MAX } else { (1u128 << m) - 1 };
        let mut pv = mask;
        let mut mv = 0u128;
        let mut score = m as isize;
        while i < hay.len() {
            let c = hay[i];
            let (eq, n) = if c < 0x80 {
                if c == b'\n' {
                    pv = mask;
                    mv = 0;
                    score = m as isize;
                    i += 1;
                    continue;
                }
                (self.fwd.ascii[c as usize], 1)
            } else {
                self.fwd.at(hay, i)
            };
            score += myers_step(eq, &mut pv, &mut mv, top, mask);
            i += n;
            if score <= self.k as isize {
                return Some((i, score as usize));
            }
        }
        None
    }

    /// Start of the match that ends at `end` with distance `d`: run the algorithm on the reversed
    /// pattern over the reversed window and take the farthest position still within `d` edits (the
    /// longest match, which highlights the whole misspelt word rather than a fragment of it).
    fn start_for(&self, hay: &[u8], end: usize, d: usize) -> usize {
        let m = self.m;
        // the window: back to the line start or m + k characters, whichever is nearer
        let units = m + self.k;
        let (mut lo, mut n) = (end, 0);
        while lo > 0 && n < units && hay[lo - 1] != b'\n' && end - lo < 4 * units {
            lo -= 1;
            if hay[lo] & 0xC0 != 0x80 {
                n += 1;
            }
        }
        if n < units && end - lo >= 4 * units {
            let stop = (lo + 3).min(end);
            while lo < stop && hay[lo] & 0xC0 == 0x80 {
                lo += 1;
            }
        }
        let mut starts = Vec::with_capacity(units);
        let mut i = lo;
        while i < end {
            starts.push(i);
            i += unit_len(hay, i);
        }
        let mut best = None;
        if let Some(b) = &self.long {
            let (_, rev) = &**b;
            let (mut pv, mut mv) = rev.start();
            let mut score = m as isize;
            for &u in starts.iter().rev() {
                score += rev.step(&mut pv, &mut mv, rev.eq_at(hay, u).0);
                if score <= d as isize {
                    best = Some(u);
                }
            }
        } else {
            let top = 1u128 << (m - 1);
            let mask = if m == 128 { u128::MAX } else { (1u128 << m) - 1 };
            let (mut pv, mut mv, mut score) = (mask, 0u128, m as isize);
            for &u in starts.iter().rev() {
                score += myers_step(self.rev.at(hay, u).0, &mut pv, &mut mv, top, mask);
                if score <= d as isize {
                    best = Some(u);
                }
            }
        }
        best.unwrap_or_else(|| starts.len().checked_sub(m).map_or(lo, |j| starts[j]))
    }
}

impl Matcher for FuzzyMatcher {
    type Captures = NoCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<MatchSpan>, NoError> {
        match self.first_end_filtered(haystack, at) {
            Some((end, d)) => {
                let start = self.start_for(haystack, end, d);
                Ok(Some(MatchSpan::new(start.max(at), end)))
            }
            None => Ok(None),
        }
    }

    fn new_captures(&self) -> Result<NoCaptures, NoError> {
        Ok(NoCaptures::new())
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        Some(LineTerminator::default()) // matches never span a line (see first_end)
    }
}

/// `-F` with one case-sensitive literal of one line: a substring search finds what the regex
/// engine would, without building it (0.4 M instructions and 30 pages touched at start-up).
#[derive(Clone, Debug)]
pub struct Literal(memchr::memmem::Finder<'static>);

impl Literal {
    fn of(o: &Pattern, insensitive: bool) -> Option<Literal> {
        let p = o.text.as_bytes();
        let plain = o.mode == Mode::Literal && !insensitive && !o.word && !o.multiline;
        // the length bound keeps it to patterns the regex engine's size limit lets through
        (plain && !p.is_empty() && p.len() <= 1024 && !p.contains(&b'\n')).then(|| Literal(memchr::memmem::Finder::new(p).into_owned()))
    }
}

impl Matcher for Literal {
    type Captures = NoCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<MatchSpan>, NoError> {
        let n = self.0.needle().len();
        Ok(haystack.get(at..).and_then(|h| self.0.find(h)).map(|i| MatchSpan::new(at + i, at + i + n)))
    }

    fn new_captures(&self) -> Result<NoCaptures, NoError> {
        Ok(NoCaptures::new())
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        Some(LineTerminator::default())
    }

    fn find_candidate_line(&self, haystack: &[u8]) -> Result<Option<grep_matcher::LineMatchKind>, NoError> {
        Ok(self.0.find(haystack).map(grep_matcher::LineMatchKind::Confirmed))
    }
}

// ------------------------------------------------------------------------- matcher selection
#[derive(Clone)]
pub enum AnyMatcher {
    Literal(Literal),
    Regex(LineRegex),
    Fuzzy(Box<FuzzyMatcher>),
    #[cfg(feature = "pcre2")]
    Pcre2(grep_pcre2::RegexMatcher),
    FuzzyRegex(Box<crate::fuzzyre::FuzzyRegex>),
}

impl AnyMatcher {
    /// A match may hold a line break (the searcher's own test for searching across lines).
    fn spans_lines(&self) -> bool {
        fn can<M: Matcher>(m: &M) -> bool {
            m.line_terminator().is_none() && !m.non_matching_bytes().is_some_and(|b| b.contains(b'\n'))
        }
        match self {
            AnyMatcher::Literal(m) => can(m),
            AnyMatcher::Regex(m) => can(m),
            AnyMatcher::Fuzzy(m) => can(m.as_ref()),
            #[cfg(feature = "pcre2")]
            AnyMatcher::Pcre2(m) => can(m),
            AnyMatcher::FuzzyRegex(m) => can(m.as_ref()),
        }
    }

    fn search_slice<S: Sink>(&self, searcher: &mut Searcher, slice: &[u8], sink: S) -> Result<(), S::Error> {
        match self {
            AnyMatcher::Literal(m) => searcher.search_slice(m, slice, sink),
            AnyMatcher::Regex(m) => searcher.search_slice(m, slice, sink),
            AnyMatcher::Fuzzy(m) => searcher.search_slice(m.as_ref(), slice, sink),
            #[cfg(feature = "pcre2")]
            AnyMatcher::Pcre2(m) => searcher.search_slice(m, slice, sink),
            AnyMatcher::FuzzyRegex(m) => searcher.search_slice(m.as_ref(), slice, sink),
        }
    }

    fn search_file<S: Sink>(&self, searcher: &mut Searcher, file: &std::fs::File, sink: S) -> Result<(), S::Error> {
        match self {
            AnyMatcher::Literal(m) => searcher.search_file(m, file, sink),
            AnyMatcher::Regex(m) => searcher.search_file(m, file, sink),
            AnyMatcher::Fuzzy(m) => searcher.search_file(m.as_ref(), file, sink),
            #[cfg(feature = "pcre2")]
            AnyMatcher::Pcre2(m) => searcher.search_file(m, file, sink),
            AnyMatcher::FuzzyRegex(m) => searcher.search_file(m.as_ref(), file, sink),
        }
    }

    fn search_reader<R: std::io::Read, S: Sink>(&self, searcher: &mut Searcher, reader: R, sink: S) -> Result<(), S::Error> {
        match self {
            AnyMatcher::Literal(m) => searcher.search_reader(m, reader, sink),
            AnyMatcher::Regex(m) => searcher.search_reader(m, reader, sink),
            AnyMatcher::Fuzzy(m) => searcher.search_reader(m.as_ref(), reader, sink),
            #[cfg(feature = "pcre2")]
            AnyMatcher::Pcre2(m) => searcher.search_reader(m, reader, sink),
            AnyMatcher::FuzzyRegex(m) => searcher.search_reader(m.as_ref(), reader, sink),
        }
    }
}

/// grep-regex's matcher, except that the regex engine is kept from running its automaton over every
/// byte in two cases: a regex with no usable literal prefix whose every match ends in one of a few
/// literals (`\w+_(suspend|resume)\(`) finds candidate lines by searching for those literals, and a
/// regex whose every match starts a line (`^(a|aa)+$`) looks only at line starts. A case-insensitive
/// literal finds candidate lines with its own finder.
#[derive(Clone, Debug)]
pub struct LineRegex {
    inner: grep_regex::RegexMatcher,
    suffixes: Option<regex::bytes::Regex>,
    line_start: Option<LineStart>,
    fold: Option<foldfind::FoldFinder>,
}

/// For a regex whose every match begins at a line start: the line starts that begin with one of its
/// prefix literals, and a search anchored at a line start. Used only on text without a line break
/// past the start (grep-regex removes `\n` from what a line regex can match; this regex does not).
#[derive(Clone, Debug)]
struct LineStart {
    anchored: regex_automata::meta::Regex,
    prefixes: Vec<Vec<u8>>,
    /// a line break followed by one of `prefixes`
    starts: regex::bytes::Regex,
}

impl LineRegex {
    fn new(inner: grep_regex::RegexMatcher, pattern: &str, insensitive: bool) -> LineRegex {
        let lines = inner.line_terminator().is_some();
        let suffixes = if lines { suffix_searcher(pattern, insensitive) } else { None };
        let line_start = if lines && suffixes.is_none() { line_start(pattern, insensitive) } else { None };
        LineRegex { inner, suffixes, line_start, fold: None }
    }

    fn plain(inner: grep_regex::RegexMatcher) -> LineRegex {
        LineRegex { inner, suffixes: None, line_start: None, fold: None }
    }

    /// The end of the match anchored at `at`, when `at` starts a line of a haystack without line
    /// breaks after it; Err when the line-start path does not apply.
    fn at_line_start(&self, haystack: &[u8], at: usize, earliest: bool) -> Result<Option<MatchSpan>, ()> {
        let ls = self.line_start.as_ref().ok_or(())?;
        if at > haystack.len() || memchr::memchr(b'\n', &haystack[at..]).is_some() {
            return Err(());
        }
        if at > 0 && haystack[at - 1] != b'\n' {
            return Ok(None);
        }
        let input = regex_automata::Input::new(haystack).range(at..).anchored(regex_automata::Anchored::Yes).earliest(earliest);
        Ok(ls.anchored.search(&input).map(|m| MatchSpan::new(m.start(), m.end())))
    }
}

/// The line-start path for `pattern`, when every match begins at a line start and begins with one
/// of a few literals.
fn line_start(pattern: &str, insensitive: bool) -> Option<LineStart> {
    use regex_syntax::hir::{
        literal::{ExtractKind, Extractor},
        Look,
    };
    let hir = regex_syntax::ParserBuilder::new().case_insensitive(insensitive).multi_line(true).utf8(false).build().parse(pattern).ok()?;
    // `\A` and `\z` mean the line's ends when the searcher checks a line, the buffer's ends here
    let looks = hir.properties().look_set();
    if !hir.properties().look_set_prefix().contains(Look::StartLF) || looks.contains(Look::Start) || looks.contains(Look::End) {
        return None;
    }
    let lits = Extractor::new().kind(ExtractKind::Prefix).extract(&hir);
    let lits = lits.literals()?;
    if lits.is_empty() || lits.len() > 64 || lits.iter().any(|l| l.is_empty() || l.as_bytes().contains(&b'\n')) {
        return None;
    }
    let prefixes: Vec<Vec<u8>> = lits.iter().map(|l| l.as_bytes().to_vec()).collect();
    let alts: Vec<String> = prefixes.iter().map(|l| l.iter().map(|b| format!("\\x{b:02X}")).collect()).collect();
    let starts = regex::bytes::Regex::new(&format!("(?-u:\\n(?:{}))", alts.join("|"))).ok()?;
    let anchored = regex_automata::meta::Regex::builder().configure(regex_automata::meta::Config::new().utf8_empty(false)).build_from_hir(&hir).ok()?;
    Some(LineStart { anchored, prefixes, starts })
}

/// A searcher for the literals every match of `pattern` ends with, when that beats the regex engine.
/// An alternation of literals compiles to the engine's vectorised multi-literal search.
fn suffix_searcher(pattern: &str, insensitive: bool) -> Option<regex::bytes::Regex> {
    use regex_syntax::hir::literal::{ExtractKind, Extractor};
    let hir = regex_syntax::ParserBuilder::new().case_insensitive(insensitive).utf8(false).build().parse(pattern).ok()?;
    let prefixes = Extractor::new().kind(ExtractKind::Prefix).extract(&hir);
    if prefixes.literals().is_some_and(|l| !l.is_empty() && l.iter().all(|x| x.len() >= 2)) {
        return None; // the engine's own prefilter already searches for these
    }
    let suffixes = Extractor::new().kind(ExtractKind::Suffix).extract(&hir);
    let lits = suffixes.literals()?;
    if lits.is_empty() || lits.len() > 64 || lits.iter().any(|l| l.len() < 3 || l.as_bytes().contains(&b'\n')) {
        return None;
    }
    let alts: Vec<String> = lits.iter().map(|l| l.as_bytes().iter().map(|b| format!("\\x{b:02X}")).collect()).collect();
    regex::bytes::Regex::new(&format!("(?-u:{})", alts.join("|"))).ok()
}

impl Matcher for LineRegex {
    type Captures = grep_regex::RegexCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<MatchSpan>, NoError> {
        match self.at_line_start(haystack, at, false) {
            Ok(m) => Ok(m),
            Err(()) => self.inner.find_at(haystack, at),
        }
    }
    fn new_captures(&self) -> Result<Self::Captures, NoError> {
        self.inner.new_captures()
    }
    fn capture_count(&self) -> usize {
        self.inner.capture_count()
    }
    fn capture_index(&self, name: &str) -> Option<usize> {
        self.inner.capture_index(name)
    }
    fn try_find_iter<F, E>(&self, haystack: &[u8], matched: F) -> Result<Result<(), E>, NoError>
    where
        F: FnMut(MatchSpan) -> Result<bool, E>,
    {
        self.inner.try_find_iter(haystack, matched)
    }
    fn captures_at(&self, haystack: &[u8], at: usize, caps: &mut Self::Captures) -> Result<bool, NoError> {
        self.inner.captures_at(haystack, at, caps)
    }
    fn shortest_match_at(&self, haystack: &[u8], at: usize) -> Result<Option<usize>, NoError> {
        match self.at_line_start(haystack, at, true) {
            Ok(m) => Ok(m.map(|m| m.end())),
            Err(()) => self.inner.shortest_match_at(haystack, at),
        }
    }
    fn non_matching_bytes(&self) -> Option<&grep_matcher::ByteSet> {
        self.inner.non_matching_bytes()
    }
    fn line_terminator(&self) -> Option<LineTerminator> {
        self.inner.line_terminator()
    }
    fn find_candidate_line(&self, haystack: &[u8]) -> Result<Option<grep_matcher::LineMatchKind>, NoError> {
        if let Some(f) = &self.fold {
            return Ok(f.find(haystack).map(grep_matcher::LineMatchKind::Candidate));
        }
        if let Some(ls) = &self.line_start {
            // the searcher calls this at the start of a line, with whole lines
            let mut next = ls.prefixes.iter().any(|p| haystack.starts_with(p)).then_some(0);
            let mut from = 0;
            loop {
                let at = match next.take() {
                    Some(at) => at,
                    None => match ls.starts.find_at(haystack, from) {
                        Some(m) => m.start() + 1,
                        None => return Ok(None),
                    },
                };
                let input = regex_automata::Input::new(haystack).range(at..).anchored(regex_automata::Anchored::Yes).earliest(true);
                match ls.anchored.search_half(&input) {
                    // no match here, so none for grep-regex's line regex either (it matches less)
                    None => from = at,
                    // a match within the line is one for grep-regex too
                    Some(m) if memchr::memchr(b'\n', &haystack[at..m.offset()]).is_none() => return Ok(Some(grep_matcher::LineMatchKind::Confirmed(at))),
                    Some(_) => return Ok(Some(grep_matcher::LineMatchKind::Candidate(at))),
                }
            }
        }
        match &self.suffixes {
            Some(re) => Ok(re.find(haystack).map(|m| grep_matcher::LineMatchKind::Candidate(m.start()))),
            None => self.inner.find_candidate_line(haystack),
        }
    }
}

/// The parts of an identifier in any casing style: `XMLHttpRequest2` -> xml, http, request, 2;
/// `make_widget_3` -> make, widget, 3. Boundaries: separators (`_`, `-`, `.`, `:`), lower->upper,
/// letter<->digit, and the last capital of an acronym (`XMLHttp` -> `XML`, `Http`).
pub fn ident_parts(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(prev) = cur.chars().last() {
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_ascii_digit() != c.is_ascii_digit())
                || (prev.is_uppercase() && c.is_uppercase() && chars.get(i + 1).map(|n| n.is_lowercase()).unwrap_or(false));
            if boundary {
                parts.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// Regex for an identifier in any casing style: `refreshToken` -> `(?i)\brefresh[_\-]?token\b`,
/// matching `refresh_token`, `RefreshToken`, `REFRESH_TOKEN` and `refresh-token`, but not
/// `refresh_tokens` or `refreshTokenOnce` (the whole identifier must match).
pub fn identifier_regex(ident: &str) -> String {
    let parts = ident_parts(ident);
    if parts.is_empty() {
        return regex::escape(ident);
    }
    let body = parts.iter().map(|p| regex::escape(p)).collect::<Vec<_>>().join("[_\\-]?");
    format!("(?i)\\b{body}\\b")
}

/// Several patterns as one that matches where any of them does, and the mode to search it with:
/// literals and identifiers become regex alternatives. `fixed` is `-F`, which escapes `-P` patterns.
pub fn join_patterns(patterns: &[String], mode: Mode, fixed: bool) -> Result<(String, Mode)> {
    if let [one] = patterns {
        let one = if mode == Mode::Pcre2 && fixed { regex::escape(one) } else { one.clone() };
        return Ok((one, mode));
    }
    if matches!(mode, Mode::Fuzzy { .. }) {
        bail!("--fuzzy takes one pattern");
    }
    let part = |p: &str| match mode {
        Mode::Literal => regex::escape(p),
        Mode::Pcre2 if fixed => regex::escape(p),
        Mode::Identifier => identifier_regex(p),
        _ => p.to_string(),
    };
    let joined = patterns.iter().map(|p| format!("(?:{})", part(p))).collect::<Vec<_>>().join("|");
    Ok((joined, if matches!(mode, Mode::Pcre2 | Mode::FuzzyRegex { .. }) { mode } else { Mode::Regex }))
}

/// What the options ask every matcher of them to find; `build_matcher` and the text index both
/// start from this, so a file the index lets through is judged by the same pattern.
pub struct Translated<'a> {
    /// case folding on top of `pattern` (the smart-case rule applied)
    pub insensitive: bool,
    /// regex source for Regex, Literal and Identifier; the pattern as given for the other modes
    pub pattern: std::borrow::Cow<'a, str>,
}

impl Pattern {
    pub fn translate(&self) -> Translated<'_> {
        let insensitive = match self.case {
            Case::Sensitive => false,
            Case::Insensitive => true,
            Case::Smart => smart_folds(&self.text, matches!(self.mode, Mode::Regex | Mode::Pcre2 | Mode::FuzzyRegex { .. })),
        };
        match self.mode {
            Mode::Literal => Translated { insensitive, pattern: regex::escape(&self.text).into() },
            // the identifier regex carries its own (?i)
            Mode::Identifier => Translated { insensitive: false, pattern: identifier_regex(&self.text).into() },
            _ => Translated { insensitive, pattern: self.text.as_str().into() },
        }
    }
}

impl Files {
    /// Rejects walk rules the text index cannot answer for: it holds only the files of the default walk.
    pub fn check_indexable(&self) -> Result<()> {
        if self.search_zip {
            bail!("--index does not cover archive members; search with -z and without --index");
        }
        if self.hidden || self.no_ignore || self.follow_links || self.max_depth.is_some() || self.max_filesize.is_some() {
            bail!("--index searches the files of the default walk; drop --hidden/--no-ignore/-L/--max-depth/--max-filesize or search without --index");
        }
        Ok(())
    }
}

/// The smart-case rule of ripgrep: fold when the pattern has a literal and none of its literals is
/// uppercase. In a regex, classes (`\w`, `\p{Ll}`, `[[:lower:]]`) are not literals; `[a-z]` is.
fn smart_folds(pattern: &str, regex: bool) -> bool {
    use regex_syntax::ast::{Ast, ClassSet, ClassSetItem};
    fn lit(c: char, seen: &mut (bool, bool)) {
        seen.0 = true;
        seen.1 |= c.is_uppercase();
    }
    fn item(i: &ClassSetItem, seen: &mut (bool, bool)) {
        match i {
            ClassSetItem::Literal(l) => lit(l.c, seen),
            ClassSetItem::Range(r) => {
                lit(r.start.c, seen);
                lit(r.end.c, seen);
            }
            ClassSetItem::Bracketed(b) => set(&b.kind, seen),
            ClassSetItem::Union(u) => u.items.iter().for_each(|x| item(x, seen)),
            _ => {}
        }
    }
    fn set(s: &ClassSet, seen: &mut (bool, bool)) {
        match s {
            ClassSet::Item(i) => item(i, seen),
            ClassSet::BinaryOp(op) => {
                set(&op.lhs, seen);
                set(&op.rhs, seen);
            }
        }
    }
    fn walk(a: &Ast, seen: &mut (bool, bool)) {
        match a {
            Ast::Literal(l) => lit(l.c, seen),
            Ast::ClassBracketed(c) => set(&c.kind, seen),
            Ast::Repetition(r) => walk(&r.ast, seen),
            Ast::Group(g) => walk(&g.ast, seen),
            Ast::Alternation(x) => x.asts.iter().for_each(|x| walk(x, seen)),
            Ast::Concat(x) => x.asts.iter().for_each(|x| walk(x, seen)),
            _ => {}
        }
    }
    if !regex {
        return !uppercase_literal(pattern, false);
    }
    match regex_syntax::ast::parse::Parser::new().parse(pattern) {
        Ok(ast) => {
            let mut seen = (false, false);
            walk(&ast, &mut seen);
            seen.0 && !seen.1
        }
        // PCRE2-only syntax
        Err(_) => !uppercase_literal(pattern, true),
    }
}

/// Whether the pattern has an uppercase letter that stands for itself (the smart-case rule, as in
/// ripgrep): in a regex, `\S`, `\W`, `\pL` and `\p{Lu}` are not such letters.
fn uppercase_literal(pattern: &str, regex: bool) -> bool {
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if regex && c == '\\' {
            match chars.next() {
                Some('p' | 'P') if chars.peek() == Some(&'{') => {
                    chars.by_ref().find(|&d| d == '}');
                }
                Some('p' | 'P') => {
                    chars.next();
                }
                // a letter by its code point stands for itself: `\x41`, `\x{41}`, `\u0041`, `\U00000041`
                Some(kind @ ('x' | 'u' | 'U')) => {
                    let digits: String = if chars.peek() == Some(&'{') {
                        chars.by_ref().skip(1).take_while(|&d| d != '}').collect()
                    } else {
                        chars
                            .by_ref()
                            .take(if kind == 'x' {
                                2
                            } else if kind == 'u' {
                                4
                            } else {
                                8
                            })
                            .collect()
                    };
                    if u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32).is_some_and(char::is_uppercase) {
                        return true;
                    }
                }
                _ => {}
            }
        } else if c.is_uppercase() {
            return true;
        }
    }
    false
}

pub fn build_matcher(p: &Pattern) -> Result<AnyMatcher> {
    if matches!(p.mode, Mode::Fuzzy { .. } | Mode::FuzzyRegex { .. }) {
        // neither matcher looks beyond a line or at word boundaries: saying so beats finding nothing
        if p.word {
            bail!("whole-word matching (-w) is not available with --fuzzy or --fuzzy-regex");
        }
        if p.multiline {
            bail!("multiline matching (-U) is not available with --fuzzy or --fuzzy-regex");
        }
    }
    #[cfg(not(feature = "pcre2"))]
    if p.mode == Mode::Pcre2 {
        bail!("-P/--pcre2 (mode pcre2) is not in this build: it was built without the pcre2 feature");
    }
    let Translated { insensitive, pattern: pat } = p.translate();
    if let Some(m) = Literal::of(p, insensitive) {
        return Ok(AnyMatcher::Literal(m));
    }
    match p.mode {
        Mode::Fuzzy { max_edits } => Ok(AnyMatcher::Fuzzy(Box::new(FuzzyMatcher::new(&pat, max_edits, insensitive)?))),
        Mode::FuzzyRegex { max_edits } => Ok(AnyMatcher::FuzzyRegex(Box::new(crate::fuzzyre::FuzzyRegex::new(&pat, max_edits, insensitive)?))),
        #[cfg(feature = "pcre2")]
        Mode::Pcre2 => {
            let m = grep_pcre2::RegexMatcherBuilder::new()
                .caseless(insensitive)
                .word(p.word)
                .multi_line(true)
                .dotall(false)
                .utf(true)
                .ucp(true)
                .jit_if_available(true)
                .build(&pat)
                .with_context(|| format!("invalid PCRE2 pattern {:?}", p.text))?;
            Ok(AnyMatcher::Pcre2(m))
        }
        _ => {
            let mut b = RegexMatcherBuilder::new();
            // ^ and $ are line anchors in every mode, as in ripgrep; without it the searcher has to
            // run the regex one line at a time
            b.case_insensitive(insensitive)
                .word(p.word)
                .multi_line(true)
                .dot_matches_new_line(false)
                .size_limit(p.regex_size_limit)
                .dfa_size_limit(p.dfa_size_limit)
                .line_terminator(if p.multiline { None } else { Some(b'\n') });
            let m = b.build(&pat).with_context(|| format!("invalid pattern {:?}", p.text))?;
            let suffix_ok = !p.word && p.mode != Mode::Literal;
            let mut m = if suffix_ok { LineRegex::new(m, &pat, insensitive) } else { LineRegex::plain(m) };
            if insensitive && m.line_terminator().is_some() {
                m.fold = foldfind::FoldFinder::new(&pat);
            }
            Ok(AnyMatcher::Regex(m))
        }
    }
}

// ------------------------------------------------------------------------- file types
/// Named groups of extensions for `--type`, from the language registry plus common aliases, then
/// the custom languages in effect.
pub fn type_globs(name: &str) -> Option<Vec<String>> {
    builtin_type_globs(name).or_else(|| crate::custom::current().by_id(name).map(|l| l.exts.iter().map(|e| format!("*{e}")).collect()))
}

/// `type_globs` of the built-in languages and aliases only.
pub fn builtin_type_globs(name: &str) -> Option<Vec<String>> {
    let id = match name {
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "rb" => "ruby",
        "rs" => "rust",
        "cs" => "csharp",
        "kt" => "kotlin",
        "hs" => "haskell",
        "ex" => "elixir",
        "sh" | "shell" => "bash",
        "md" => "markdown",
        "yml" => "yaml",
        "tf" | "terraform" => "hcl",
        other => other,
    };
    let spec = crate::langs::by_id(id)?;
    let mut globs: Vec<String> = spec.exts.iter().map(|e| format!("*{e}")).collect();
    // an extension shared with another language is the language's too (`-t cpp` takes `.h`)
    let shared = crate::langs::BY_CONTENT.iter().filter(|(e, ids)| ids.contains(&spec.id) && !spec.exts.contains(e));
    globs.extend(shared.map(|(e, _)| format!("*{e}")));
    globs.extend(spec.files.iter().map(|f| f.to_string()));
    Some(globs)
}

pub fn type_list() -> Vec<(String, Vec<String>)> {
    let custom = crate::custom::current();
    let ids = crate::langs::LANGS.iter().map(|s| s.id).chain(custom.langs.iter().map(|l| l.id.as_str()));
    ids.map(|id| (id.to_string(), type_globs(id).unwrap_or_default())).collect()
}

// ------------------------------------------------------------------------- the search
/// Plain `path:line:text` output written by the search threads themselves: each file's lines are
/// formatted into a buffer and handed over in one call, so no per-line allocation or channel message.
/// Used for the common piped case (no colour, no JSON, no sorting); the bytes are the same as the
/// CLI's own formatting of records.
///
/// Output that follows other output within `HOLD_NS` waits in a buffer, so the lines of many files
/// leave in one write: a write per matching file wakes the reader of a pipe every time (88,000
/// writes for `return` in the Linux tree). Output after a quiet spell leaves at once.
pub struct PlainSink {
    out: Mutex<std::io::BufWriter<Box<dyn Write + Send>>>,
    /// when output last left, in nanoseconds since `start`
    left: AtomicU64,
    /// the buffer holds output
    holding: AtomicBool,
    start: Instant,
    /// a write failed: the reader went away (`| head`), or `failed` says why
    pub broken: AtomicBool,
    /// the first write error that is not a closed pipe (a full disk, a file size limit)
    failed: Mutex<Option<std::io::Error>>,
    /// a file searched with context lines has written output (later files start with `--`)
    grouped: AtomicBool,
}

/// The longest output waits for more to leave with it, unless no thread starts a file meanwhile.
const HOLD_NS: u64 = 1_000_000;

impl PlainSink {
    pub fn new(out: Box<dyn Write + Send>) -> PlainSink {
        PlainSink {
            out: Mutex::new(std::io::BufWriter::with_capacity(1 << 16, out)),
            left: AtomicU64::new(0),
            holding: AtomicBool::new(false),
            start: Instant::now(),
            broken: AtomicBool::new(false),
            failed: Mutex::new(None),
            grouped: AtomicBool::new(false),
        }
    }

    fn waited(&self) -> Option<u64> {
        let now = self.start.elapsed().as_nanos() as u64;
        (now.saturating_sub(self.left.load(Ordering::Relaxed)) >= HOLD_NS).then_some(now)
    }

    /// Runs `f` on the writer as one unit (no other thread's output inside it), then lets the held
    /// output go if it has waited long enough.
    fn with(&self, stop: &Stop, f: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) {
        if self.broken.load(Ordering::Relaxed) {
            return;
        }
        let res = match self.out.lock() {
            Ok(mut w) => f(&mut *w).and_then(|_| {
                if let Some(now) = self.waited().filter(|_| !w.buffer().is_empty()) {
                    self.left.store(now, Ordering::Relaxed);
                    w.flush()?;
                }
                self.holding.store(!w.buffer().is_empty(), Ordering::Relaxed);
                Ok(())
            }),
            Err(_) => Err(std::io::Error::other("output lock poisoned")),
        };
        if let Err(e) = res {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                self.failed.lock().unwrap_or_else(|p| p.into_inner()).get_or_insert(e);
            }
            self.broken.store(true, Ordering::Relaxed);
            stop.set();
        }
    }

    /// `first`: the first output of a file searched with context, which another file's output
    /// before it separates with `--`.
    fn write(&self, buf: &[u8], first: bool, stop: &Stop) {
        if !buf.is_empty() {
            self.with(stop, |w| self.separate(w, first).and_then(|_| w.write_all(buf)));
        }
    }

    /// `before`, `head`, `text` with invalid UTF-8 replaced, and a line break, as one unit. Err when
    /// reading a text in a file failed: the line then ends where the read stopped.
    fn write_line(&self, before: &[u8], head: &[u8], text: Text, first: bool, stop: &Stop) -> std::io::Result<()> {
        let mut read = Ok(());
        self.with(stop, |w| {
            self.separate(w, first)?;
            w.write_all(before)?;
            w.write_all(head)?;
            match text {
                Text::Here(b) => write_lossy(w, b, 1 << 18)?,
                Text::At(at, len) => read = write_lossy_from(w, at, len)?,
            }
            w.write_all(b"\n")
        });
        read
    }

    fn separate(&self, w: &mut dyn Write, first: bool) -> std::io::Result<()> {
        if first && self.grouped.swap(true, Ordering::Relaxed) {
            w.write_all(b"--\n")?;
        }
        Ok(())
    }

    /// Lets held output go once it has waited. Threads call this between files, so output waits
    /// no longer than `HOLD_NS` or the longest file being searched.
    fn tick(&self, stop: &Stop) {
        if self.holding.load(Ordering::Relaxed) && self.waited().is_some() {
            self.with(stop, |_| Ok(()));
        }
    }

    /// The write error that stopped the output, unless it was a closed pipe.
    pub fn failure(&self) -> Option<std::io::Error> {
        self.failed.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    pub fn flush(&self) -> std::io::Result<()> {
        match self.out.lock() {
            Ok(mut w) => w.flush(),
            Err(_) => Err(std::io::Error::other("output lock poisoned")),
        }
    }
}

/// Line text as UTF-8, invalid bytes replaced. Validates with `from_utf8` first: its word-at-a-time
/// check is several times faster than the replacing decoder on the (common) valid input.
fn lossy(b: &[u8]) -> std::borrow::Cow<'_, str> {
    match std::str::from_utf8(b) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => String::from_utf8_lossy(b),
    }
}

/// `b` written as `lossy(b)` would be, `piece` bytes at a time: each piece is checked just before it
/// is written, while the reader drains the previous one, instead of the whole line up front.
fn write_lossy(w: &mut dyn Write, b: &[u8], piece: usize) -> std::io::Result<()> {
    let mut at = 0;
    while at < b.len() {
        let end = b.len().min(at + piece.max(4));
        at = end - write_lossy_part(w, &b[at..end], end < b.len())?;
    }
    Ok(())
}

/// `b` written as `lossy(b)` would be; with `more`, a character cut by its end is left for the
/// next part and its length returned.
fn write_lossy_part(w: &mut dyn Write, b: &[u8], more: bool) -> std::io::Result<usize> {
    let mut at = 0;
    while at < b.len() {
        match std::str::from_utf8(&b[at..]) {
            Ok(_) => {
                w.write_all(&b[at..])?;
                at = b.len();
            }
            Err(e) => {
                let ok = at + e.valid_up_to();
                w.write_all(&b[at..ok])?;
                match e.error_len() {
                    Some(bad) => {
                        w.write_all("\u{FFFD}".as_bytes())?;
                        at = ok + bad;
                    }
                    None if more => return Ok(b.len() - ok),
                    None => {
                        w.write_all("\u{FFFD}".as_bytes())?;
                        at = b.len();
                    }
                }
            }
        }
    }
    Ok(0)
}

/// The text of a matching line: here, or the first `len` bytes of a source, read again in pieces
/// instead of being held whole.
#[derive(Clone, Copy)]
enum Text<'t> {
    Here(&'t [u8]),
    At(&'t dyn source::ReadAt, u64),
}

/// The first `len` bytes of `at` written as `write_lossy` writes them. The outer Err is a failed
/// write, the inner one a failed or short read (the output then stops where the read did).
fn write_lossy_from(w: &mut dyn Write, at: &dyn source::ReadAt, len: u64) -> std::io::Result<std::io::Result<()>> {
    const PIECE: usize = 1 << 18;
    let mut buf = vec![0u8; PIECE + 3];
    let (mut pos, mut kept) = (0u64, 0usize);
    while pos < len {
        let want = (PIECE as u64).min(len - pos) as usize;
        let got = match at.read_at(&mut buf[kept..kept + want], pos) {
            Ok(0) => return Ok(Err(std::io::Error::other("changed while it was searched, or a read failed"))),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Ok(Err(e)),
        };
        pos += got as u64;
        let end = kept + got;
        let left = write_lossy_part(w, &buf[..end], pos < len)?;
        buf.copy_within(end - left..end, 0);
        kept = left;
    }
    Ok(Ok(()))
}

/// `path:line:text\n` (`-` separators for context lines), the CLI's plain format; an empty `path`
/// leaves out the name and its separator, a `line` of 0 the numbers. A multi-line match prints
/// each of its lines with its own number, as grep-class tools do.
pub fn format_plain(out: &mut Vec<u8>, path: &[u8], line: u64, text: &[u8], context: bool) {
    let sep = if context { b'-' } else { b':' };
    for (k, part) in text.split(|&b| b == b'\n').enumerate() {
        if !path.is_empty() {
            out.extend_from_slice(path);
            out.push(sep);
        }
        if line != 0 {
            let mut n = line + k as u64;
            let mut digits = [0u8; 20];
            let mut i = digits.len();
            loop {
                i -= 1;
                digits[i] = b'0' + (n % 10) as u8;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            out.extend_from_slice(&digits[i..]);
            out.push(sep);
        }
        out.extend_from_slice(part);
        out.push(b'\n');
    }
}

struct Collector<'a> {
    path: Arc<str>,
    /// `Line::path_bytes`
    raw: Option<Arc<[u8]>>,
    /// None: the records stay in `batch` for the caller to take
    tx: Option<&'a mpsc::SyncSender<Vec<Record>>>,
    counters: &'a Counters,
    /// records of the current file, sent in batches (one channel operation per file or 512 lines)
    batch: Vec<Record>,
    plain: Option<&'a PlainSink>,
    out: Vec<u8>,
    want_spans: bool,
    text_spans: bool,
    stats_matches: &'a AtomicU64,
    max_results: Option<u64>,
    matched: bool,
    stop: &'a Stop,
    matcher: &'a AnyMatcher,
    invert: bool,
    report: Report,
    file_count: u64,
    /// added to every line number (a range of a split file starts mid-file)
    line_base: u64,
    /// keep plain output in `out` instead of writing it (a range of a split file waits its turn)
    hold: bool,
    /// the searcher reported binary data
    binary: bool,
    /// a named file, searched as ripgrep does (`Files::paths_as_given`): NUL bytes end lines
    convert: bool,
    /// where the searcher first saw a NUL byte
    binary_at: Option<u64>,
    /// a match may span lines (otherwise a matched line holds no line break)
    multiline: bool,
    /// context lines are asked for and this file has written no plain output yet
    first: bool,
    /// context lines on several threads: plain output leaves only at the end of a group, so no
    /// other file's lines land inside one
    whole_groups: bool,
    /// plain lines start with `path`
    named: bool,
    /// plain lines have line numbers
    numbered: bool,
    /// `-c` counts each match of a multi-line search, not each merged group of lines
    count_each: bool,
    /// those matches in the group `matched` was given, counted there against the whole buffer
    each: Option<u64>,
}

/// Offsets into `line` (a match may run on into the line end left out of it) as offsets into
/// `text`, `lossy(line)`: inside it and on character boundaries (but an empty match in valid
/// text, as ripgrep gives it), a U+FFFD that a match ends or starts within taken whole.
fn text_offsets(spans: &mut [(usize, usize)], line: &[u8], text: &str, valid: bool) {
    // each piece of the line: where it starts in `line` and in `text`, its valid and invalid bytes
    let whole = [(0, 0, line.len(), 0)];
    let split: Vec<(usize, usize, usize, usize)>;
    let pieces = if valid {
        &whole[..]
    } else {
        let (mut r, mut t) = (0, 0);
        split = line
            .utf8_chunks()
            .map(|c| {
                let p = (r, t, c.valid().len(), c.invalid().len());
                r += p.2 + p.3;
                t += p.2 + if p.3 > 0 { '\u{FFFD}'.len_utf8() } else { 0 };
                p
            })
            .collect();
        &split
    };
    let at = |x: usize, up: bool| {
        let Some(&(r, t, v, n)) = pieces.get(pieces.partition_point(|p| p.0 <= x).saturating_sub(1)) else { return 0 };
        let d = x - r;
        let mut y = if d <= v {
            t + d
        } else if d < v + n {
            t + v + if up { '\u{FFFD}'.len_utf8() } else { 0 }
        } else {
            text.len()
        };
        y = y.min(text.len());
        while !text.is_char_boundary(y) {
            y = if up { y + 1 } else { y - 1 };
        }
        y
    };
    for s in spans {
        *s = match (s.0 == s.1, valid) {
            // an empty match in text that is the line keeps its byte offset, inside a character
            // too, as ripgrep's does; nothing is cut there
            (true, true) => (s.0.min(text.len()), s.0.min(text.len())),
            // elsewhere it stays empty, at the start of the character it falls in
            (true, false) => (at(s.0, false), at(s.0, false)),
            _ => (at(s.0, false), at(s.1, true)),
        };
    }
}

fn spans_in(matcher: &AnyMatcher, line: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    each_match(matcher, line, |a, b| {
        out.push((a, b));
        true
    });
    out
}

/// Calls `f` with each match in `line` until it returns false. As ripgrep, leaves out an empty
/// match right where the match before it ended, and one past the line's end (after its `\n`).
fn each_match(matcher: &AnyMatcher, line: &[u8], f: impl FnMut(usize, usize) -> bool) {
    each_match_in(matcher, line, 0..line.len(), f)
}

/// `each_match` over `hay[lines]`, read with the text around it: `$`, `\B` and word edges see
/// what follows the lines, not their end. Offsets are into `hay`.
fn each_match_in(matcher: &AnyMatcher, hay: &[u8], lines: std::ops::Range<usize>, mut f: impl FnMut(usize, usize) -> bool) {
    let line = hay;
    let mut at = lines.start;
    let content = lines.end - usize::from(hay[lines.clone()].ends_with(b"\n"));
    let mut last_end = None;
    let find = |at: usize| -> Option<MatchSpan> {
        match matcher {
            AnyMatcher::Literal(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::Regex(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::Fuzzy(m) => m.find_at(line, at).ok().flatten(),
            #[cfg(feature = "pcre2")]
            AnyMatcher::Pcre2(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::FuzzyRegex(m) => m.find_at(line, at).ok().flatten(),
        }
    };
    while at <= line.len() {
        match find(at) {
            Some(m) => {
                if m.start() > content {
                    break;
                }
                at = if m.end() > m.start() { m.end() } else { m.end() + 1 };
                let empty = m.start() == m.end();
                if empty && last_end == Some(m.start()) {
                    continue;
                }
                if !empty {
                    last_end = Some(m.end());
                }
                if !f(m.start(), m.end()) {
                    break;
                }
            }
            None => break,
        }
    }
}

impl<'a> Sink for Collector<'a> {
    type Error = std::io::Error;

    fn matched(&mut self, _s: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, std::io::Error> {
        let bytes = mat.bytes();
        if self.count_each {
            // as ripgrep counts: not an empty match at the end of a file with no line end
            let (hay, mut n) = (mat.buffer(), 0);
            each_match_in(self.matcher, hay, mat.bytes_range_in_buffer(), |a, b| {
                n += u64::from(!(a == b && a == hay.len()));
                true
            });
            self.each = Some(n.max(1));
        }
        let line = bytes.strip_suffix(b"\n").map(|b| b.strip_suffix(b"\r").unwrap_or(b)).unwrap_or(bytes);
        self.take_match(bytes, Text::Here(line), mat.line_number())
    }

    fn context(&mut self, _s: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, std::io::Error> {
        let bytes = ctx.bytes();
        let line = bytes.strip_suffix(b"\n").map(|b| b.strip_suffix(b"\r").unwrap_or(b)).unwrap_or(bytes);
        if self.plain.is_some() {
            let name = if self.named { self.raw.as_deref().unwrap_or(self.path.as_bytes()) } else { b"" };
            let number = if self.numbered { ctx.line_number().unwrap_or(0) + self.line_base } else { 0 };
            format_plain(&mut self.out, name, number, lossy(line).as_bytes(), true);
            return Ok(true);
        }
        let text = lossy(line).into_owned();
        self.push(Record::Context(Line {
            path: Arc::clone(&self.path),
            path_bytes: self.raw.clone(),
            line: ctx.line_number().unwrap_or(0) + self.line_base,
            column: 0,
            text,
            spans: Vec::new(),
        }));
        Ok(true)
    }

    fn context_break(&mut self, _s: &Searcher) -> Result<bool, std::io::Error> {
        if let Some(p) = self.plain.filter(|_| self.whole_groups && !self.hold && self.out.len() >= 1 << 16) {
            // the next group is a first output: the sink writes the `--` before it
            p.write(&self.out, std::mem::take(&mut self.first), self.stop);
            self.out.clear();
            self.first = true;
        } else if self.plain.is_some() {
            self.out.extend_from_slice(b"--\n");
        }
        Ok(true)
    }

    fn binary_data(&mut self, _s: &Searcher, offset: u64) -> Result<bool, std::io::Error> {
        self.binary_at = Some(offset);
        Ok(true)
    }

    fn finish(&mut self, _s: &Searcher, f: &grep_searcher::SinkFinish) -> Result<(), std::io::Error> {
        self.binary = f.binary_byte_offset().is_some();
        if let (true, Report::Lines, Some(at)) = (self.matched, self.report, self.binary_at) {
            // as ripgrep: a named file's line output ends here; a walked file stopped being searched
            let note = if self.convert {
                format!("binary file matches (found \"\\0\" byte around offset {at})")
            } else {
                format!("WARNING: stopped searching binary file after match (found \"\\0\" byte around offset {at})")
            };
            if self.plain.is_some() {
                if self.named {
                    self.out.extend_from_slice(self.raw.as_deref().unwrap_or(self.path.as_bytes()));
                    self.out.extend_from_slice(b": ");
                }
                self.out.extend_from_slice(note.as_bytes());
                self.out.push(b'\n');
            } else {
                self.push(Record::Note { path: Arc::clone(&self.path), path_bytes: self.raw.clone(), text: note });
            }
        }
        self.flush();
        Ok(())
    }
}

impl Collector<'_> {
    /// A matching line: `bytes` as the searcher gave it, `text` without its line end, `number`
    /// the searcher's line number.
    fn take_match(&mut self, bytes: &[u8], text: Text, number: Option<u64>) -> Result<bool, std::io::Error> {
        if self.stop.is_set() {
            return Ok(false);
        }
        if self.report != Report::Lines {
            if self.report == Report::Files && self.matched {
                return Ok(true);
            }
            let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
            if self.over_limit(n) {
                return Ok(false);
            }
            self.matched = true;
            // as ripgrep: merged matches that span lines count one by one
            self.file_count += self.each.take().unwrap_or(1);
            if self.max_results.is_some_and(|max| n >= max) {
                self.stop.set();
                return Ok(false);
            }
            // one match is enough to list the file; with -v ripgrep reads on and drops a file
            // with a NUL byte
            return Ok(self.report != Report::Files || (self.invert && !self.convert));
        }
        if self.convert && self.binary_at.is_some() {
            // as ripgrep: a match past a NUL byte ends the output, `finish` writes the note
            let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
            self.matched |= !self.over_limit(n);
            return Ok(false);
        }
        if let Some(p) = self.plain {
            let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
            if self.over_limit(n) {
                return Ok(false);
            }
            self.matched = true;
            let number = if self.numbered { number.unwrap_or(0) + self.line_base } else { 0 };
            let name = if self.named { self.raw.as_deref().unwrap_or(self.path.as_bytes()) } else { b"" };
            match text {
                Text::Here(line) if line.len() < 1 << 16 || self.hold || self.whole_groups || (self.multiline && memchr::memchr(b'\n', line).is_some()) => {
                    format_plain(&mut self.out, name, number, lossy(line).as_bytes(), false);
                }
                _ => {
                    // a very long line is written from where it is instead of being copied into the buffer
                    let mut head = Vec::with_capacity(name.len() + 24);
                    format_plain(&mut head, name, number, b"", false);
                    head.pop(); // the newline: the text goes between
                    let read = p.write_line(&self.out, &head, text, std::mem::take(&mut self.first), self.stop);
                    self.out.clear();
                    read?;
                }
            }
            if self.out.len() >= 1 << 16 && !self.hold && !self.whole_groups {
                p.write(&self.out, std::mem::take(&mut self.first), self.stop);
                self.out.clear();
            }
            if let Some(max) = self.max_results {
                if n >= max {
                    self.stop.set();
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        let Text::Here(line) = text else { return Err(std::io::Error::other("a line not in memory has no hit")) };
        let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
        if self.over_limit(n) {
            return Ok(false);
        }
        let text = lossy(line);
        let mut spans = if self.invert || !self.want_spans { Vec::new() } else { spans_in(self.matcher, bytes) };
        if self.text_spans {
            text_offsets(&mut spans, line, &text, matches!(text, std::borrow::Cow::Borrowed(_)));
        }
        let text = text.into_owned();
        let column = spans.first().map(|s| s.0 as u64 + 1).unwrap_or(0);
        self.matched = true;
        let path_bytes = self.raw.clone();
        self.push(Record::Match(Line { path: Arc::clone(&self.path), path_bytes, line: number.unwrap_or(0) + self.line_base, column, text, spans }));
        if let Some(max) = self.max_results {
            if n >= max {
                self.stop.set();
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Match number `n` (counted over all threads) is past `max_results`: another thread got there
    /// first, so this one is not reported, nor counted.
    fn over_limit(&self, n: u64) -> bool {
        let over = self.max_results.is_some_and(|max| n > max);
        if over {
            self.stats_matches.fetch_sub(1, Ordering::Relaxed);
            self.stop.set();
        }
        over
    }

    fn push(&mut self, h: Record) {
        self.batch.push(h);
        if self.batch.len() >= 512 {
            self.flush();
        }
    }
    fn flush(&mut self) {
        if let (Some(p), false, false) = (self.plain, self.hold, self.out.is_empty()) {
            p.write(&self.out, std::mem::take(&mut self.first), self.stop);
            self.out.clear();
        }
        if let Some(tx) = self.tx.filter(|_| !self.batch.is_empty()) {
            send(self.counters, tx, std::mem::take(&mut self.batch));
        }
    }
}

/// ripgrep's own ignore file: read before `.ignore` and `.gitignore`, switched off with them.
pub const RGIGNORE: &str = ".rgignore";

/// The glob filters of `f`, plus the index directory, which is never searched.
pub fn overrides(root: &Path, f: &Files) -> Result<ignore::overrides::Override> {
    let mut ob = OverrideBuilder::new(root);
    ob.add(&format!("!{}/", crate::files::INDEX_DIR))?;
    for g in &f.globs {
        ob.add(g).with_context(|| format!("invalid glob {g:?}"))?;
    }
    Ok(ob.build()?)
}

/// The file-type filters of `f`, which the walk asks after the globs and the ignore rules.
pub fn file_types(f: &Files) -> Result<ignore::types::Types> {
    let mut tb = ignore::types::TypesBuilder::new();
    for (i, t) in f.types.iter().enumerate() {
        let (neg, name) = match t.strip_prefix('!') {
            Some(n) => (true, n),
            None => (false, t.as_str()),
        };
        // the builder takes only alphanumeric names, and a registry id may hold `_`
        let key = format!("t{i}");
        let globs = match &f.customs {
            Some(c) => builtin_type_globs(name).or_else(|| c.by_id(name).map(|l| l.exts.iter().map(|e| format!("*{e}")).collect())),
            None => type_globs(name),
        };
        for g in globs.ok_or_else(|| anyhow!("unknown file type {name:?} (see --type-list)"))? {
            tb.add(&key, &g)?;
        }
        if neg {
            tb.negate(&key);
        } else {
            tb.select(&key);
        }
    }
    Ok(tb.build()?)
}

/// Globs matched against each path from `dir`, the directory of the file that holds them. A path
/// outside `dir` is not touched; a path named on the command line is searched whatever they say.
#[derive(Debug)]
pub struct ScopedGlobs {
    /// canonical, as the roots are compared with it
    dir: PathBuf,
    ov: ignore::overrides::Override,
}

/// Where a searched root lies from `ScopedGlobs::dir`.
enum Scope {
    /// at or below it: the root's path from it
    Below(PathBuf),
    /// above it: its path from the root
    Above(PathBuf),
    Outside,
}

impl ScopedGlobs {
    pub fn new(dir: &Path, globs: &[String]) -> Result<ScopedGlobs> {
        let mut ob = OverrideBuilder::new(".");
        for g in globs {
            ob.add(g).with_context(|| format!("invalid glob {g:?}"))?;
        }
        Ok(ScopedGlobs { dir: std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()), ov: ob.build()? })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn scope(&self, root: &Path) -> Scope {
        let Ok(root) = std::fs::canonicalize(root) else { return Scope::Outside };
        if let Ok(p) = root.strip_prefix(&self.dir) {
            Scope::Below(p.to_path_buf())
        } else if let Ok(p) = self.dir.strip_prefix(&root) {
            Scope::Above(p.to_path_buf())
        } else {
            Scope::Outside
        }
    }

    /// Whether the globs leave out `rest`, a path below a root in `scope`.
    fn excludes(&self, scope: &Scope, rest: &Path, is_dir: bool) -> bool {
        thread_local! {
            static JOINED: std::cell::RefCell<PathBuf> = const { std::cell::RefCell::new(PathBuf::new()) };
        }
        match scope {
            Scope::Below(p) if p.as_os_str().is_empty() => self.ov.matched(rest, is_dir).is_ignore(),
            Scope::Below(p) => JOINED.with_borrow_mut(|j| {
                j.as_mut_os_string().clear();
                j.push(p);
                j.push(rest);
                self.ov.matched(&*j, is_dir).is_ignore()
            }),
            Scope::Above(p) => rest.strip_prefix(p).is_ok_and(|r| !r.as_os_str().is_empty() && self.ov.matched(r, is_dir).is_ignore()),
            Scope::Outside => false,
        }
    }

    /// Whether the globs leave out `rel`, a file below a root in `scope` named with `/`, or a
    /// directory on the way down to it.
    fn excludes_file(&self, scope: &Scope, rel: &str) -> bool {
        let mut at = 0;
        while let Some(i) = rel[at..].find('/') {
            if self.excludes(scope, Path::new(&rel[..at + i]), true) {
                return true;
            }
            at += i + 1;
        }
        self.excludes(scope, Path::new(rel), false)
    }
}

pub fn build_walker(roots: &[PathBuf], f: &Files) -> Result<ignore::WalkParallel> {
    Ok(walk_builder(roots, f, Policy::read(&System::default(), f.threads).threads())?.build_parallel())
}

/// The rules of the default walk of `root`, asked about one path from it at a time.
pub fn default_rules(root: &Path) -> Result<ignore::IncrementalIgnore> {
    walk_builder(&[root.to_path_buf()], &Files::default(), 1)?.build_matchers().pop().ok_or_else(|| anyhow!("no walk root"))
}

fn walk_builder(roots: &[PathBuf], f: &Files, threads: usize) -> Result<WalkBuilder> {
    let mut wb = WalkBuilder::new(&roots[0]);
    for r in &roots[1..] {
        wb.add(r);
    }
    wb.hidden(!f.hidden)
        .ignore(!f.no_ignore)
        .git_ignore(!f.no_ignore)
        .git_global(!f.no_ignore)
        .git_exclude(!f.no_ignore)
        .parents(!f.no_ignore)
        .follow_links(f.follow_links)
        .max_depth(f.max_depth)
        .max_filesize(f.max_filesize)
        // `grep foo . > out.txt` must not read what it writes, as ripgrep skips it
        .skip_stdout(true)
        .threads(threads);
    if !f.no_ignore {
        wb.add_custom_ignore_filename(RGIGNORE);
    }
    if let Some(cwd) = &f.cwd {
        wb.process_current_dir(cwd);
    }
    // anchored at a root that is a file, the filters would miss the files of the other roots
    let anchor = if roots[0].is_dir() { roots[0].as_path() } else { roots[0].parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")) };
    wb.overrides(overrides(anchor, f)?);
    if !f.types.is_empty() {
        wb.types(file_types(f)?);
    }
    if let Some(sg) = f.scoped_globs.clone() {
        let scopes: Vec<(PathBuf, Scope)> = roots.iter().map(|r| (r.clone(), sg.scope(r))).collect();
        wb.filter_entry(move |e| {
            let path = e.path();
            match scopes.iter().find_map(|(r, s)| path.strip_prefix(r).ok().map(|rest| (s, rest))) {
                Some((s, rest)) => !sg.excludes(s, rest, e.file_type().is_some_and(|t| t.is_dir())),
                None => true,
            }
        });
    }
    Ok(wb)
}

/// How many files the walking thread (`schedule`) takes before the parallel walk does the rest;
/// 0 skips it. Roots holding only files get `SMALL_SEARCH`, 256: starting the parallel walk costs more there than
/// the search itself (three large files: 40.6 ms against 41.9). Any directory to enter gets 0: one
/// thread reading every directory then holds up the searches (the ripgrep repository, 60
/// directories: 5.6 ms in parallel against 6.0). `Files::small_tree` sets the number for any tree
/// (the tests use both walks).
fn small_tree_files(roots: &[PathBuf], f: &Files) -> usize {
    f.small_tree.unwrap_or_else(|| if has_dirs(roots, f) { 0 } else { policy::SMALL_SEARCH })
}

/// Whether a root holds a directory the walk would enter (a hidden one only with `--hidden`).
fn has_dirs(roots: &[PathBuf], f: &Files) -> bool {
    roots.iter().any(|r| {
        std::fs::read_dir(r).is_ok_and(|mut entries| {
            entries.any(|e| {
                e.is_ok_and(|e| {
                    let dir = if f.follow_links { e.path().is_dir() } else { e.file_type().is_ok_and(|t| t.is_dir()) };
                    dir && (f.hidden || !e.file_name().as_encoded_bytes().starts_with(b"."))
                })
            })
        })
    })
}

/// A regular file, or anything but a directory named on the command line (a named pipe,
/// `/dev/stdin`), as ripgrep does; a pipe met in a directory could wait for a writer forever. A
/// root can be a link to a directory, which the walk enters.
fn searchable(e: &ignore::DirEntry) -> bool {
    e.file_type().is_some_and(|t| t.is_file() || (e.depth() == 0 && !t.is_dir() && !e.path().is_dir()))
}

#[derive(Default)]
struct Counters {
    files_searched: AtomicU64,
    /// small files opened and read in one call
    small_reads: AtomicU64,
    /// of those, the reads that took as long as a disk read (`COLD_READ_NS` or more)
    slow_reads: AtomicU64,
    files_matched: AtomicU64,
    matches: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
    failures: Mutex<Vec<String>>,
    /// directories below a root that are on a kernel pseudo filesystem (`met_dir`), and whether
    /// there are any: the flag is all a file pays for until there are
    pseudo_dirs: Mutex<Vec<PathBuf>>,
    has_pseudo_dirs: AtomicBool,
    /// one file is searched and no other at the same time: its output need not wait for the end
    /// of a context group
    alone: AtomicBool,
    /// batches sent and not yet drained, and the most there were
    #[cfg(test)]
    queued: AtomicUsize,
    #[cfg(test)]
    most_queued: AtomicUsize,
    /// threads started for a list or a walking thread
    #[cfg(test)]
    started: AtomicUsize,
    /// the most threads the thread rules wanted for a list or a walking thread: what they decided,
    /// whatever the timing of the starts
    #[cfg(test)]
    most_wanted: AtomicUsize,
}

impl Counters {
    /// A directory the walk met. One named `proc` or `sys` gets a `statfs` (a procfs mounted inside
    /// a tree, such as a container's root filesystem); every other entry, only a name comparison.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    fn met_dir(&self, e: &ignore::DirEntry) {
        #[cfg(target_os = "linux")]
        if e.depth() > 0 && matches!(e.file_name().as_encoded_bytes(), b"proc" | b"sys") && e.file_type().is_some_and(|t| t.is_dir()) && on_pseudo_fs(e.path())
        {
            self.pseudo_dirs.lock().unwrap_or_else(|p| p.into_inner()).push(e.path().to_path_buf());
            self.has_pseudo_dirs.store(true, Ordering::Release);
        }
    }

    /// Roots on a pseudo filesystem (`pseudo_roots`): their files are read to the end too.
    fn pseudo_roots(&self, roots: Vec<PathBuf>) {
        if !roots.is_empty() {
            self.pseudo_dirs.lock().unwrap_or_else(|p| p.into_inner()).extend(roots);
            self.has_pseudo_dirs.store(true, Ordering::Release);
        }
    }

    /// `path` is read until a read returns nothing (it is on a pseudo filesystem).
    fn read_to_end(&self, path: &Path) -> bool {
        self.has_pseudo_dirs.load(Ordering::Acquire) && self.pseudo_dirs.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|p| path.starts_with(p))
    }

    fn failed(&self, what: String) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut v) = self.failures.lock() {
            if v.len() < 10 {
                v.push(what);
            }
        }
    }
}

/// When a search stops early: the caller's cancel signal, which it only reads, or a reason of its
/// own (enough matches, the reader of plain output went away).
struct Stop {
    cancel: Arc<AtomicBool>,
    own: AtomicBool,
}

impl Stop {
    fn new(cancel: Arc<AtomicBool>) -> Stop {
        Stop { cancel, own: AtomicBool::new(false) }
    }

    #[inline]
    fn is_set(&self) -> bool {
        self.own.load(Ordering::Relaxed) || self.cancel.load(Ordering::Relaxed)
    }

    fn set(&self) {
        self.own.store(true, Ordering::Relaxed);
    }
}

/// What every file of one search shares: the matcher, the query, the file set, the counters, the
/// stop flag and where the results go.
#[derive(Clone, Copy)]
struct Run<'a> {
    matcher: &'a AnyMatcher,
    q: &'a Query,
    f: &'a Files,
    /// the thread rules
    p: &'a Policy,
    c: &'a Counters,
    stop: &'a Stop,
    tx: &'a mpsc::SyncSender<Vec<Record>>,
    plain: Option<&'a PlainSink>,
    /// the paths the files are reported under (`label`)
    labels: &'a [PathBuf],
    /// the file being searched: `Line::path_bytes`
    raw: Option<&'a Arc<[u8]>>,
}

impl<'a> Run<'a> {
    fn collector(&self, path: Arc<str>) -> Collector<'a> {
        let o = self.q;
        Collector {
            path,
            raw: self.raw.cloned(),
            tx: Some(self.tx),
            counters: self.c,
            batch: Vec::new(),
            plain: self.plain,
            out: Vec::new(),
            want_spans: o.want_spans,
            text_spans: o.text_spans,
            stats_matches: &self.c.matches,
            max_results: o.max_results,
            matched: false,
            stop: self.stop,
            matcher: self.matcher,
            invert: o.invert,
            report: o.report,
            file_count: 0,
            line_base: 0,
            hold: false,
            binary: false,
            convert: false,
            binary_at: None,
            multiline: o.pattern.multiline,
            first: o.before > 0 || o.after > 0,
            whole_groups: (o.before > 0 || o.after > 0) && self.p.threads() > 1 && !self.c.alone.load(Ordering::Relaxed),
            named: o.with_filename,
            numbered: o.line_numbers,
            count_each: o.pattern.multiline && o.report == Report::Counts && !o.invert && self.matcher.spans_lines(),
            each: None,
        }
    }
}

/// Search one file and send its records to `tx`. `named`: a root that is a file, or `-` for
/// standard input (`Files::paths_as_given`).
fn search_one(w: &mut FileWorker, r: &Run, path: &Path, rel: Arc<str>, size: Option<u64>, named: bool) {
    let raw = label_bytes(path, r.labels, r.f);
    let r = &Run { raw: raw.as_ref(), ..*r };
    count_file(r, size);
    if r.f.search_zip {
        if let Some(kind) = crate::archive::Kind::of(&rel) {
            search_archive(w, r, path, &rel, kind, named);
            return;
        }
    }
    if named && path.as_os_str() == "-" {
        source::search(w, r, &mut source::Stdin, rel, true);
        return;
    }
    match source::Disk::open(path, size, !r.c.read_to_end(path)) {
        Ok(mut file) => source::search(w, r, &mut file, rel, named),
        Err(e) => r.c.failed(format!("{rel}: {e}")),
    }
}

/// A source starts being searched: held plain output may leave, and it counts.
fn count_file(r: &Run, size: Option<u64>) {
    if let Some(p) = r.plain {
        p.tick(r.stop);
    }
    r.c.files_searched.fetch_add(1, Ordering::Relaxed);
    if let Some(n) = size {
        r.c.bytes.fetch_add(n, Ordering::Relaxed);
    }
}

/// The end of one file's search: what it found leaves (a search aborted by an error or
/// cancellation still reports it). As ripgrep, a file the walk found that holds a NUL byte has
/// no count and is not listed. Returns the emptied output buffer, for the next file.
fn report(r: &Run, mut sink: Collector, rel: Arc<str>, named: bool) -> Vec<u8> {
    sink.flush();
    if sink.matched && (named || !sink.binary || r.q.report == Report::Lines) {
        r.c.files_matched.fetch_add(1, Ordering::Relaxed);
        match r.q.report {
            Report::Lines => {}
            Report::Counts => send(r.c, r.tx, vec![Record::Count { path: rel, path_bytes: sink.raw.clone(), count: sink.file_count }]),
            Report::Files => send(r.c, r.tx, vec![Record::File { path: rel, path_bytes: sink.raw.clone() }]),
        }
    }
    let mut out = std::mem::take(&mut sink.out);
    out.clear();
    // a buffer that grew past the usual flush size (a context group held whole) is not kept
    if out.capacity() > 1 << 17 {
        out = Vec::new();
    }
    out
}

/// `-z`: every member of an archive, or the decompressed file, as a file of its own, reported as
/// `archive.zip{member}` (the decompressed file keeps its own name).
fn search_archive(w: &mut FileWorker, run: &Run, path: &Path, rel: &Arc<str>, kind: crate::archive::Kind, named: bool) {
    let (c, stop) = (run.c, run.stop);
    // one compressed file named on the command line is that file, as ripgrep reads it
    let named = named && matches!(kind, crate::archive::Kind::Gz | crate::archive::Kind::Other);
    // the members of one archive share a searcher, made for that archive
    w.member = None;
    let mut each = |member: Option<&str>, r: &mut dyn std::io::Read| -> bool {
        let m = member.map(|m| if run.q.raw_member_names { std::borrow::Cow::Borrowed(m) } else { crate::fmt::shown(m) });
        let label: Arc<str> = match &m {
            Some(m) => Arc::from(format!("{rel}{{{m}}}")),
            None => Arc::clone(rel),
        };
        let raw = match (run.raw, &m) {
            (Some(b), Some(m)) => Some(Arc::from([&b[..], b"{", m.as_bytes(), b"}"].concat())),
            (b, _) => b.cloned(),
        };
        source::search(w, &Run { raw: raw.as_ref(), ..*run }, &mut source::Member(r), label, named);
        !stop.is_set()
    };
    let mut failed = |what: String| c.failed(format!("{rel}: {what}"));
    if let Err(e) = crate::archive::walk(path, kind, &mut each, &mut failed) {
        c.failed(format!("{rel}: {e}"));
    }
}

/// Batches of hits waiting for the caller at most: past it the search threads wait, so a search
/// with more matches than the caller writes holds no more of them.
const HIT_BATCHES: usize = 64;

fn hit_channel() -> (mpsc::SyncSender<Vec<Record>>, mpsc::Receiver<Vec<Record>>) {
    mpsc::sync_channel(HIT_BATCHES)
}

/// Waits while `HIT_BATCHES` batches wait for the caller.
#[cfg(not(target_os = "wasi"))]
fn send(c: &Counters, tx: &mpsc::SyncSender<Vec<Record>>, batch: Vec<Record>) {
    #[cfg(test)]
    c.most_queued.fetch_max(c.queued.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
    #[cfg(not(test))]
    let _ = c;
    let _ = tx.send(batch);
}

/// Hand the records to the caller in arrival order, until every sender is gone.
#[cfg(not(target_os = "wasi"))]
fn drain(rx: mpsc::Receiver<Vec<Record>>, c: &Counters, mut on_hit: impl FnMut(Record)) {
    for batch in rx {
        #[cfg(test)]
        c.queued.fetch_sub(1, Ordering::Relaxed);
        batch.into_iter().for_each(&mut on_hit);
    }
    #[cfg(test)]
    tests::MOST_QUEUED.with(|m| m.set(m.get().max(c.most_queued.load(Ordering::Relaxed))));
    #[cfg(not(test))]
    let _ = c;
}

#[cfg(target_os = "wasi")]
thread_local! {
    /// The batches of the file being searched: on one thread a full channel would wait forever.
    static HELD: std::cell::RefCell<Vec<Vec<Record>>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(target_os = "wasi")]
fn send(_: &Counters, _: &mpsc::SyncSender<Vec<Record>>, batch: Vec<Record>) {
    HELD.with_borrow_mut(|held| held.push(batch));
}

/// Hand the records held so far to the caller, in arrival order.
#[cfg(target_os = "wasi")]
fn take_held(on_hit: &mut dyn FnMut(Record)) {
    for batch in HELD.with_borrow_mut(std::mem::take) {
        batch.into_iter().for_each(&mut *on_hit);
    }
}

fn stats(c: &Counters, t0: Instant, stop: &Stop) -> Stats {
    Stats {
        files_searched: c.files_searched.load(Ordering::Relaxed),
        files_matched: c.files_matched.load(Ordering::Relaxed),
        matches: c.matches.load(Ordering::Relaxed),
        bytes_searched: c.bytes.load(Ordering::Relaxed),
        errors: c.errors.load(Ordering::Relaxed),
        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
        cancelled: stop.is_set(),
        failures: c.failures.lock().map(|v| v.clone()).unwrap_or_default(),
    }
}

/// Search `files` for `query`, the one way into Engine A. The results go to `out` as they are
/// found. The search stops early once `cancel` is set (from another thread), which it reads and
/// never sets. Returns statistics.
pub fn search(query: &Query, files: &Files, cancel: Arc<AtomicBool>, out: Output) -> Result<Stats> {
    search_on(&System::default(), query, files, cancel, out)
}

/// `search`, with the thread rules reading `m`.
fn search_on<M: Machine>(m: &M, query: &Query, files: &Files, cancel: Arc<AtomicBool>, out: Output) -> Result<Stats> {
    let stop = Stop::new(cancel);
    // read without a call through a table: the vtable is touched only by a search that is counted
    let p = Policy::read(m, files.threads);
    let mut none = |_: Record| {};
    let (plain, on_record): (Option<Arc<PlainSink>>, &mut dyn FnMut(Record)) = match out {
        Output::Records(f) => (None, f),
        Output::Plain(p) => (Some(p), &mut none),
    };
    let plain = plain.as_deref();
    match &files.paths {
        Paths::Walk(roots) => run_walk(roots, query, files, &p, m, &stop, plain, on_record),
        Paths::List { root, files: list } => run_files(root, list, query, files, &p, m, &stop, plain, on_record),
        Paths::Memory(texts) => run_memory(texts, query, files, &p, &stop, plain, on_record),
    }
}

/// Search text held in memory on one thread, in the order given or by name.
fn run_memory(
    texts: &[(String, Vec<u8>)],
    q: &Query,
    f: &Files,
    p: &Policy,
    stop: &Stop,
    plain: Option<&PlainSink>,
    on_hit: &mut dyn FnMut(Record),
) -> Result<Stats> {
    let t0 = Instant::now();
    if q.first_in_path_order && plain.is_some() {
        bail!("first_in_path_order needs records, not plain output");
    }
    let matcher = build_matcher(&q.pattern)?;
    let counters = Counters::default();
    let mut order: Vec<&(String, Vec<u8>)> = texts.iter().collect();
    if q.first_in_path_order {
        order.sort_by(|a, b| Path::new(&a.0).cmp(Path::new(&b.0)));
    }
    let (tx, rx) = hit_channel();
    let c = &counters;
    #[cfg(not(target_os = "wasi"))]
    std::thread::scope(|s| {
        let matcher = &matcher;
        s.spawn(move || {
            let mut fw = file_worker(q);
            let r = Run { matcher, q, f, p, c, stop, tx: &tx, plain, labels: &[], raw: None };
            for (name, bytes) in order {
                if stop.is_set() {
                    break;
                }
                count_file(&r, q.want_bytes.then_some(bytes.len() as u64));
                source::search(&mut fw, &r, &mut source::Bytes(bytes), Arc::from(name.as_str()), false);
            }
        });
        drain(rx, c, &mut *on_hit);
    });
    #[cfg(target_os = "wasi")]
    {
        drop(rx);
        let mut fw = file_worker(q);
        let r = Run { matcher: &matcher, q, f, p, c, stop, tx: &tx, plain, labels: &[], raw: None };
        for (name, bytes) in order {
            if stop.is_set() {
                break;
            }
            count_file(&r, q.want_bytes.then_some(bytes.len() as u64));
            source::search(&mut fw, &r, &mut source::Bytes(bytes), Arc::from(name.as_str()), false);
            take_held(on_hit);
        }
    }
    Ok(stats(&counters, t0, stop))
}

#[allow(clippy::too_many_arguments)]
fn run_walk(
    roots: &[PathBuf],
    q: &Query,
    f: &Files,
    p: &Policy,
    m: &dyn Machine,
    stop: &Stop,
    plain: Option<&PlainSink>,
    on_hit: &mut dyn FnMut(Record),
) -> Result<Stats> {
    let t0 = Instant::now();
    if roots.is_empty() {
        bail!("no paths to search");
    }
    let matcher = build_matcher(&q.pattern)?;
    let counters = Counters::default();
    let rest: Vec<PathBuf>;
    let stdin_too = f.paths_as_given && roots.iter().any(|r| r.as_os_str() == "-");
    let given = roots;
    let roots = if stdin_too {
        rest = roots.iter().filter(|r| r.as_os_str() != "-").cloned().collect();
        &rest[..]
    } else {
        roots
    };
    counters.pseudo_roots(pseudo_roots(roots));
    let s = Common { matcher: &matcher, q, f, p, m, c: &counters, stop, plain, labels: roots };
    if q.first_in_path_order {
        if plain.is_some() {
            bail!("first_in_path_order needs records, not plain output");
        }
        // as `rg --sort path`: the roots in the order given, each walked in path order
        let mut files = Vec::new();
        for root in given {
            if stdin_too && root.as_os_str() == "-" {
                files.push((PathBuf::from("-"), Arc::from(STDIN_LABEL), true));
                continue;
            }
            let at = files.len();
            files.extend(list_files(std::slice::from_ref(root), roots, f, p, &counters)?);
            files[at..].sort_by(|a, b| Path::new(&*a.1).cmp(Path::new(&*b.1)));
        }
        return Ok(first_in_path_order(files, &s, on_hit, t0));
    }
    if stdin_too {
        // standard input, alone, before the walk
        let stdin = [(PathBuf::from("-"), Arc::from(STDIN_LABEL), true)];
        counters.alone.store(true, Ordering::Relaxed);
        schedule(Feed::List(&stdin, None), &Common { labels: &[], ..s }, on_hit)?;
        counters.alone.store(false, Ordering::Relaxed);
    }
    if roots.is_empty() {
        return Ok(stats(&counters, t0, stop));
    }
    // the overrides are anchored at one root: a glob naming a directory needs a walk per root
    if roots.len() > 1 && f.globs.iter().any(|g| g.contains('/')) {
        for root in roots.chunks(1) {
            if !stop.is_set() {
                schedule(Feed::Walk(root), &s, on_hit)?;
            }
        }
    } else {
        schedule(Feed::Walk(roots), &s, on_hit)?;
    }
    Ok(stats(&counters, t0, stop))
}

/// What every thread of one search shares: a `Run` but its channel, and the thread rules with the
/// machine they read.
#[derive(Clone, Copy)]
struct Common<'a> {
    matcher: &'a AnyMatcher,
    q: &'a Query,
    f: &'a Files,
    p: &'a Policy,
    m: &'a dyn Machine,
    c: &'a Counters,
    stop: &'a Stop,
    plain: Option<&'a PlainSink>,
    /// the paths the files are reported under (`label`)
    labels: &'a [PathBuf],
}

impl Common<'_> {
    /// One thread's `Run`, with its own matcher (its own regex cache pool instead of contending for
    /// one) and its own end of the channel.
    fn run<'r>(&'r self, matcher: &'r AnyMatcher, tx: &'r mpsc::SyncSender<Vec<Record>>) -> Run<'r> {
        let s = self;
        Run { matcher, q: s.q, f: s.f, p: s.p, c: s.c, stop: s.stop, tx, plain: s.plain, labels: s.labels, raw: None }
    }
}

/// Where the files of one `schedule` come from.
enum Feed<'a> {
    /// the files under these roots: a walk on the calling thread first when no root holds a
    /// directory, then the parallel walk
    Walk(&'a [PathBuf]),
    /// these files (path, path as reported, named), handed out in order; with a number, no file is
    /// started once that many matches are in
    List(&'a [(PathBuf, Arc<str>, bool)], Option<u64>),
}

/// A file the walking thread found: path, path as reported, size, named.
type Found = (PathBuf, Arc<str>, Option<u64>, bool);

/// A file a thread takes, as `Found`.
type Item<'a> = (std::borrow::Cow<'a, Path>, Arc<str>, Option<u64>, bool);

/// The files the threads of a list or of the walking thread take, one at a time.
enum Items<'a> {
    Found(Mutex<mpsc::Receiver<Found>>),
    List { files: &'a [(PathBuf, Arc<str>, bool)], next: AtomicUsize, stop_at: Option<u64> },
}

impl Items<'_> {
    /// Every file of a list was taken (a walk's channel cannot say).
    fn used_up(&self) -> bool {
        matches!(self, Items::List { files, next, .. } if next.load(Ordering::Relaxed) >= files.len())
    }

    fn next(&self, s: &Common) -> Option<Item<'_>> {
        match self {
            Items::Found(rx) => {
                let (path, rel, size, named) = rx.lock().ok()?.recv().ok()?;
                Some((path.into(), rel, size, named))
            }
            Items::List { files, next, stop_at } => {
                // checked before a file is taken, so the files searched are always the first ones
                if stop_at.is_some_and(|n| s.c.matches.load(Ordering::Relaxed) >= n) {
                    return None;
                }
                let (path, rel, named) = files.get(next.fetch_add(1, Ordering::Relaxed))?;
                // standard input has no size, whatever a file named `-` holds
                let size = if s.q.want_bytes && !(*named && path.as_os_str() == "-") { std::fs::metadata(path).ok().map(|m| m.len()) } else { None };
                Some((path.as_path().into(), Arc::clone(rel), size, *named))
            }
        }
    }
}

/// How many threads one stage of a search runs, as the thread rules decide while it runs: its share
/// of the searches running (when it is counted) and the threads its slow reads earn.
struct Threads<'a> {
    s: &'a Common<'a>,
    counted: bool,
    share: AtomicUsize,
    cold: AtomicUsize,
    /// the share plus the cold threads: the threads that should run
    want: Arc<AtomicUsize>,
    /// the threads of a list or of the walking thread that run now
    running: AtomicUsize,
    /// the look at the count after the first few files was taken
    early: AtomicBool,
    shown: Shown,
}

impl<'a> Threads<'a> {
    /// `counted`: the stage joins the count of searches running and takes its share; otherwise it
    /// takes `alone` threads.
    fn new(s: &'a Common<'a>, counted: bool, alone: usize) -> Threads<'a> {
        let shown = Shown::default();
        let first = if counted {
            let k = s.m.running();
            let n = s.p.share(k);
            shown.show(s.m, k, n);
            n
        } else {
            alone
        };
        let cold = s.p.cold_extra(s.c.small_reads.load(Ordering::Relaxed), s.c.slow_reads.load(Ordering::Relaxed));
        Threads {
            s,
            counted,
            share: AtomicUsize::new(first),
            cold: AtomicUsize::new(cold),
            want: Arc::new(AtomicUsize::new(first + cold)),
            running: AtomicUsize::new(0),
            early: AtomicBool::new(false),
            shown,
        }
    }

    fn first(&self) -> usize {
        self.share.load(Ordering::Relaxed)
    }

    /// After a file of a thread that has searched `files`: true when the threads wanted changed.
    fn after_file(&self, files: &mut u32) -> bool {
        let (p, c) = (self.s.p, self.s.c);
        let extra = p.cold_extra(c.small_reads.load(Ordering::Relaxed), c.slow_reads.load(Ordering::Relaxed));
        let mut changed = extra > self.cold.load(Ordering::Relaxed);
        if changed {
            self.cold.fetch_max(extra, Ordering::Relaxed);
        }
        if self.counted {
            *files = files.wrapping_add(1);
            let searched = c.files_searched.load(Ordering::Relaxed);
            if p.look_again(*files, searched) || (p.look_early(searched) && !self.early.swap(true, Ordering::Relaxed)) {
                let k = self.s.m.running();
                let now = p.share(k);
                changed |= self.share.swap(now, Ordering::Relaxed) != now;
                self.shown.show(self.s.m, k, now);
            }
        }
        if changed {
            self.want.store(self.share.load(Ordering::Relaxed) + self.cold.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        changed
    }

    /// Starts a thread taking `items` when fewer run than are wanted, or `force`.
    // `try_update`, the new name of `fetch_update`, is newer than the oldest Rust this builds with
    #[allow(deprecated)]
    fn grow<'s>(&'s self, sc: &'s std::thread::Scope<'s, '_>, items: &'s Items<'s>, tx: &mpsc::SyncSender<Vec<Record>>, force: bool) -> bool {
        let want = self.want.load(Ordering::Relaxed);
        #[cfg(test)]
        self.s.c.most_wanted.fetch_max(want, Ordering::Relaxed);
        // a thread started once the list is taken would find nothing to do
        if !force && items.used_up() {
            return false;
        }
        if self.running.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| (force || n < want).then_some(n + 1)).is_err() {
            return false;
        }
        #[cfg(test)]
        self.s.c.started.fetch_add(1, Ordering::Relaxed);
        let tx = tx.clone();
        sc.spawn(move || self.work(sc, items, tx));
        true
    }

    /// One thread: it searches `items` until there are none, starts others when more are wanted and
    /// ends when fewer are.
    #[allow(deprecated)]
    fn work<'s>(&'s self, sc: &'s std::thread::Scope<'s, '_>, items: &'s Items<'s>, tx: mpsc::SyncSender<Vec<Record>>) {
        let s = self.s;
        let matcher = s.matcher.clone();
        let mut fw = file_worker(s.q);
        let mut files = 0u32;
        while !s.stop.is_set() {
            let Some((path, rel, size, named)) = items.next(s) else { break };
            search_one(&mut fw, &s.run(&matcher, &tx), &path, rel, size, named);
            if self.after_file(&mut files) {
                while self.grow(sc, items, &tx, false) {}
            }
            let want = self.want.load(Ordering::Relaxed);
            if self.running.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n > want).then(|| n - 1)).is_ok() {
                return;
            }
        }
        self.running.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Runs one search over `feed`: the threads it starts, how many, and when it adds or pauses some,
/// all as the thread rules (`Policy`) decide. A list and a walk of directories take their share of
/// the searches running from the start; every stage adds threads as its reads turn out to come
/// from the disk.
///
/// A walk of roots without directories walks on the calling thread while a few threads search the
/// files it finds, so a small tree pays neither for a parallel walk nor for walking before
/// searching. The first file waits for a second before any thread starts: a tree of one file is
/// searched on the calling thread, with no thread (four threads cost about 1 M instructions, a
/// third of a one-file search past start-up). Past `small_tree_files` files that walk stops and
/// the parallel walk skips what it searched; walk errors count only when that walk finished (the
/// parallel walk meets them again otherwise).
#[cfg(not(target_os = "wasi"))]
fn schedule(feed: Feed, s: &Common, on_hit: &mut dyn FnMut(Record)) -> Result<()> {
    let (q, f, p, c, stop) = (s.q, s.f, s.p, s.c, s.stop);
    let roots = match feed {
        Feed::List(files, stop_at) => {
            let threads = Threads::new(s, p.counted(files.len() <= policy::SMALL_SEARCH), p.threads());
            let items = Items::List { files, next: AtomicUsize::new(0), stop_at };
            let (tx, rx) = hit_channel();
            std::thread::scope(|sc| {
                for _ in 0..threads.first().min(files.len().max(1)) {
                    threads.grow(sc, &items, &tx, true);
                }
                drop(tx);
                drain(rx, c, &mut *on_hit);
            });
            return Ok(());
        }
        Feed::Walk(roots) => roots,
    };
    let mut seen = std::collections::HashMap::<PathBuf, AtomicUsize>::new();
    let limit = small_tree_files(roots, f);
    if let Some(wb) = (limit > 0).then(|| walk_builder(roots, f, p.threads()).ok()).flatten() {
        let threads = Threads::new(s, false, p.walked_threads());
        let (work_tx, work_rx) = mpsc::channel::<Found>();
        let items = Items::Found(Mutex::new(work_rx));
        let (tx, rx) = hit_channel();
        let (errors, big) = std::thread::scope(|sc| {
            let mut started = false;
            let mut held: Option<Found> = None;
            let mut taken = 0;
            let mut errors = Vec::new();
            let mut big = false;
            for entry in wb.build() {
                if stop.is_set() {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        errors.push(e.to_string());
                        continue;
                    }
                };
                if !searchable(&entry) {
                    c.met_dir(&entry);
                    continue;
                }
                if taken == limit {
                    big = true;
                    break;
                }
                taken += 1;
                let size = if q.want_bytes { entry.metadata().ok().map(|m| m.len()) } else { None };
                let rel = label(entry.path(), s.labels, f);
                let named = f.paths_as_given && entry.depth() == 0;
                let path = entry.into_path();
                *seen.entry(path.clone()).or_default().get_mut() += 1;
                let job = (path, rel, size, named);
                if !started {
                    let Some(first) = held.take() else {
                        held = Some(job);
                        continue;
                    };
                    for _ in 0..threads.first() {
                        threads.grow(sc, &items, &tx, true);
                    }
                    started = true;
                    if work_tx.send(first).is_err() {
                        break;
                    }
                }
                if work_tx.send(job).is_err() {
                    break;
                }
                while let Ok(batch) = rx.try_recv() {
                    #[cfg(test)]
                    c.queued.fetch_sub(1, Ordering::Relaxed);
                    batch.into_iter().for_each(&mut *on_hit);
                }
            }
            drop(work_tx);
            if let Some((path, rel, size, named)) = held.filter(|_| !stop.is_set()) {
                c.alone.store(true, Ordering::Relaxed);
                let tx = tx.clone();
                let one = move || search_one(&mut file_worker(q), &s.run(s.matcher, &tx), &path, rel, size, named);
                // plain output sends no hits; other output is drained here while the file is searched
                if s.plain.is_some() {
                    one();
                } else {
                    sc.spawn(one);
                }
            }
            drop(tx);
            drain(rx, c, &mut *on_hit);
            c.alone.store(false, Ordering::Relaxed);
            (errors, big)
        });
        if !big || stop.is_set() {
            for e in errors {
                c.failed(e);
            }
            return Ok(());
        }
    }
    let threads = Threads::new(s, p.counted(false), p.threads());
    let first = threads.first();
    let mut wb = walk_builder(roots, f, first)?;
    // the share starts, and the rest when the count falls or the reads turn out cold
    if p.grows() {
        wb.grow_threads(p.most().saturating_sub(first), Arc::clone(&threads.want));
    }
    let walker = wb.build_parallel();
    let (tx, rx) = hit_channel();
    let (threads, seen) = (&threads, &seen);
    std::thread::scope(|sc| {
        let walk = sc.spawn(move || {
            walker.run(|| {
                let tx = tx.clone();
                let matcher = s.matcher.clone();
                let mut files = 0u32;
                // made on the first file: the threads kept for a cold walk mostly never run
                let mut fw = None;
                Box::new(move |entry| {
                    if stop.is_set() {
                        return WalkState::Quit;
                    }
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            c.failed(e.to_string());
                            return WalkState::Continue;
                        }
                    };
                    if !searchable(&entry) {
                        c.met_dir(&entry);
                        return WalkState::Continue;
                    }
                    if let Some(n) = if seen.is_empty() { None } else { seen.get(entry.path()) } {
                        let mut left = n.load(Ordering::Relaxed);
                        while left > 0 {
                            match n.compare_exchange_weak(left, left - 1, Ordering::Relaxed, Ordering::Relaxed) {
                                Ok(_) => return WalkState::Continue,
                                Err(now) => left = now,
                            }
                        }
                    }
                    let size = if q.want_bytes { entry.metadata().ok().map(|m| m.len()) } else { None };
                    let rel = label(entry.path(), s.labels, f);
                    let fw = fw.get_or_insert_with(|| file_worker(q));
                    let named = f.paths_as_given && entry.depth() == 0;
                    search_one(fw, &s.run(&matcher, &tx), entry.path(), rel, size, named);
                    threads.after_file(&mut files);
                    if stop.is_set() {
                        WalkState::Quit
                    } else {
                        WalkState::Continue
                    }
                })
            })
        });
        drain(rx, c, &mut *on_hit);
        walk.join().map_err(|_| anyhow!("search worker panicked"))
    })
}

/// `schedule` on wasi, which starts no thread: the calling thread walks in order and searches each
/// file as it is found, handing its records over before the next.
#[cfg(target_os = "wasi")]
fn schedule(feed: Feed, s: &Common, on_hit: &mut dyn FnMut(Record)) -> Result<()> {
    let (q, f, c, stop) = (s.q, s.f, s.c, s.stop);
    let (tx, _rx) = hit_channel();
    let r = s.run(s.matcher, &tx);
    let mut fw = file_worker(q);
    match feed {
        Feed::List(files, stop_at) => {
            let items = Items::List { files, next: AtomicUsize::new(0), stop_at };
            while !stop.is_set() {
                let Some((path, rel, size, named)) = items.next(s) else { break };
                search_one(&mut fw, &r, &path, rel, size, named);
                take_held(on_hit);
            }
        }
        Feed::Walk(roots) => {
            for entry in walk_builder(roots, f, 1)?.build() {
                if stop.is_set() {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        c.failed(e.to_string());
                        continue;
                    }
                };
                if !searchable(&entry) {
                    c.met_dir(&entry);
                    continue;
                }
                let size = if q.want_bytes { entry.metadata().ok().map(|m| m.len()) } else { None };
                let rel = label(entry.path(), s.labels, f);
                let named = f.paths_as_given && entry.depth() == 0;
                search_one(&mut fw, &r, entry.path(), rel, size, named);
                take_held(on_hit);
            }
        }
    }
    Ok(())
}

/// Search the given files (paths relative to `root`, as an index lists them) with exactly the
/// per-file logic of a walk; the glob and type filters of `f` apply to these paths.
#[allow(clippy::too_many_arguments)]
fn run_files(
    root: &Path,
    list: &[String],
    q: &Query,
    f: &Files,
    p: &Policy,
    m: &dyn Machine,
    stop: &Stop,
    plain: Option<&PlainSink>,
    on_hit: &mut dyn FnMut(Record),
) -> Result<Stats> {
    let t0 = Instant::now();
    let matcher = build_matcher(&q.pattern)?;
    let ov = overrides(root, f)?;
    let types = file_types(f)?;
    let scoped = f.scoped_globs.as_ref().map(|sg| (sg, sg.scope(root)));
    let mut files: Vec<(PathBuf, Arc<str>, bool)> = list
        .iter()
        .filter(|rel| {
            // a glob that matches decides alone, as in the walk
            let by_glob = filtered_out(&ov, rel);
            let typed_out = || ov.matched(rel.as_str(), false).is_none() && types.matched(rel.as_str(), false).is_ignore();
            !by_glob && !typed_out() && !scoped.as_ref().is_some_and(|(sg, s)| sg.excludes_file(s, rel))
        })
        .map(|rel| (root.join(rel), Arc::from(rel.as_str()), false))
        .collect();
    // the file standard output writes to is not searched, as the walk skips it
    if let Some(out) = stdout_file() {
        files.retain(|(p, ..)| same_file::Handle::from_path(p).map_or(true, |h| h != out));
    }
    let counters = Counters::default();
    let labels = [root.to_path_buf()];
    let s = Common { matcher: &matcher, q, f, p, m, c: &counters, stop, plain, labels: &labels };
    if q.first_in_path_order {
        if plain.is_some() {
            bail!("first_in_path_order needs records, not plain output");
        }
        files.sort_by(|a, b| Path::new(&*a.1).cmp(Path::new(&*b.1)));
        return Ok(first_in_path_order(files, &s, on_hit, t0));
    }
    schedule(Feed::List(&files, None), &s, on_hit)?;
    Ok(stats(&counters, t0, stop))
}

/// Standard output, when it writes to a regular file.
fn stdout_file() -> Option<same_file::Handle> {
    let h = same_file::Handle::stdout().ok()?;
    h.as_file().metadata().ok()?.is_file().then_some(h)
}

/// The files under `roots` the search would reach (path, path as reported, named on the command
/// line), for `first_in_path_order`.
fn list_files(roots: &[PathBuf], labels: &[PathBuf], f: &Files, p: &Policy, c: &Counters) -> Result<Vec<(PathBuf, Arc<str>, bool)>> {
    let found = Mutex::new(Vec::new());
    walk_builder(roots, f, p.threads())?.build_parallel().run(|| {
        Box::new(|entry| {
            match entry {
                Ok(e) if searchable(&e) => {
                    let item = (e.path().to_path_buf(), label(e.path(), labels, f), f.paths_as_given && e.depth() == 0);
                    found.lock().unwrap_or_else(|p| p.into_inner()).push(item);
                }
                Ok(e) => c.met_dir(&e),
                Err(e) => c.failed(e.to_string()),
            }
            WalkState::Continue
        })
    });
    Ok(found.into_inner().unwrap_or_else(|p| p.into_inner()))
}

/// `Query::first_in_path_order`: search `files` in their order until `max_results` matches are
/// in, let the files started meanwhile finish, then hand over the first `max_results` matches
/// (all without it) in file and line order.
fn first_in_path_order(files: Vec<(PathBuf, Arc<str>, bool)>, s: &Common, on_hit: &mut dyn FnMut(Record), t0: Instant) -> Stats {
    let (c, stop) = (s.c, s.stop);
    let n = s.q.max_results;
    // no file holds more than `n` of the first `n`; the cut below stops the rest
    let q = Query { max_results: None, max_count: n.map(|n| s.q.max_count.map_or(n, |m| m.min(n))).or(s.q.max_count), ..s.q.clone() };
    let mut records = Vec::new();
    if let Err(e) = schedule(Feed::List(&files, n), &Common { q: &q, ..*s }, &mut |r| records.push(r)) {
        c.failed(e.to_string());
    }
    // a file that several roots reach is listed once per root under one label: the k-th copy of a
    // record belongs to its k-th listing
    let mut listed: std::collections::HashMap<&str, Vec<usize>> = Default::default();
    for (i, f) in files.iter().enumerate() {
        listed.entry(&*f.1).or_default().push(i);
    }
    let listing = |label: &str| {
        let mut s = label;
        loop {
            if let Some(v) = listed.get(s) {
                return Some(v);
            }
            // an archive member, `archive.zip{member}`
            s = &s[..s.rfind('{')?];
        }
    };
    let at: Vec<usize> = {
        let mut copies: std::collections::HashMap<(&str, u64, u64), usize> = Default::default();
        records
            .iter()
            .map(|r| {
                let column = if let Record::Match(l) | Record::Context(l) = r { l.column } else { 0 };
                let copy = copies.entry((r.path(), r.order(), column)).or_insert(0);
                *copy += 1;
                listing(r.path()).map_or(usize::MAX, |v| v[(*copy - 1).min(v.len() - 1)])
            })
            .collect()
    };
    let mut keyed: Vec<(usize, Record)> = at.into_iter().zip(records).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path().cmp(b.1.path())).then(a.1.order().cmp(&b.1.order())));
    let cap = n.unwrap_or(u64::MAX);
    let (mut left, mut files_matched, mut last) = (cap, 0u64, None::<Arc<str>>);
    for (_, mut r) in keyed {
        if left == 0 {
            break;
        }
        let take = match &mut r {
            Record::Match(_) | Record::File { .. } => 1,
            Record::Count { count, .. } => {
                *count = (*count).min(left);
                *count
            }
            Record::Context(_) | Record::Note { .. } => 0,
        };
        if take > 0 {
            left -= take;
            if last.as_ref() != Some(r.path()) {
                files_matched += 1;
                last = Some(Arc::clone(r.path()));
            }
        }
        on_hit(r);
    }
    let total = c.matches.load(Ordering::Relaxed);
    match n {
        Some(n) => Stats { matches: n - left, files_matched, cancelled: stop.is_set() || total >= n, ..stats(c, t0, stop) },
        None => stats(c, t0, stop),
    }
}

/// True when the walk with these overrides would not reach `rel` (an ancestor directory or the
/// file itself is excluded).
fn filtered_out(ov: &ignore::overrides::Override, rel: &str) -> bool {
    if ov.is_empty() {
        return false;
    }
    let mut at = 0;
    while let Some(i) = rel[at..].find('/') {
        if ov.matched(&rel[..at + i], true).is_ignore() {
            return true;
        }
        at += i + 1;
    }
    ov.matched(rel, false).is_ignore()
}

/// The roots that lie on a kernel pseudo filesystem: one `statfs` per root, none per file. The
/// walk finds those below a root (`Counters::met_dir`).
#[cfg(target_os = "linux")]
fn pseudo_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots.iter().filter(|r| on_pseudo_fs(r)).cloned().collect()
}

#[cfg(not(target_os = "linux"))]
fn pseudo_roots(_roots: &[PathBuf]) -> Vec<PathBuf> {
    Vec::new()
}

/// `p` is on procfs, sysfs or another kernel pseudo filesystem, whose files report a size of 0
/// and return a page or less per read.
#[cfg(target_os = "linux")]
fn on_pseudo_fs(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn statfs(path: *const std::ffi::c_char, buf: *mut u64) -> std::ffi::c_int;
    }
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
    // larger than `struct statfs` on every Linux ABI; `f_type` is its first field
    let mut buf = [0u64; 32];
    if unsafe { statfs(c.as_ptr(), buf.as_mut_ptr()) } != 0 {
        return false;
    }
    #[cfg(target_arch = "s390x")]
    let f_type = (buf[0] >> 32) as u32;
    #[cfg(not(target_arch = "s390x"))]
    let f_type = unsafe { *(buf.as_ptr() as *const std::ffi::c_long) } as u32;
    // proc, sysfs, debugfs, tracefs, securityfs, configfs, cgroup, cgroup2
    matches!(f_type, 0x9fa0 | 0x6265_6572 | 0x6462_6720 | 0x7472_6163 | 0x7363_6673 | 0x6265_6570 | 0x0027_e0eb | 0x6367_7270)
}

/// How a file found under `roots` is reported (see `Files::paths_as_given`).
fn label(path: &Path, roots: &[PathBuf], f: &Files) -> Arc<str> {
    if !f.paths_as_given {
        return Arc::from(relative(path, roots));
    }
    let s = path.to_string_lossy();
    if cfg!(windows) {
        Arc::from(slashed(s))
    } else {
        Arc::from(&*s)
    }
}

/// The bytes of `label(path, roots, f)` when `path` is not UTF-8, for output that must name the file
/// exactly. Windows has no bytes to give: its names stay as `label` writes them.
#[cfg(unix)]
fn label_bytes(path: &Path, roots: &[PathBuf], f: &Files) -> Option<Arc<[u8]>> {
    use std::os::unix::ffi::OsStrExt;
    if path.to_str().is_some() {
        return None;
    }
    fn bytes(p: &Path) -> &[u8] {
        p.as_os_str().as_bytes()
    }
    let under = if f.paths_as_given { None } else { roots.iter().find_map(|r| Some((r, path.strip_prefix(r).ok()?))) };
    Some(match under {
        Some((r, rel)) if rel.as_os_str().is_empty() => bytes(r).into(),
        Some((_, rel)) if roots.len() == 1 => bytes(rel).into(),
        Some((r, rel)) => {
            let mut root = bytes(r);
            while let [rest @ .., b'/'] = root {
                root = rest;
            }
            [root, b"/", bytes(rel)].concat().into()
        }
        None => bytes(path).into(),
    })
}

#[cfg(not(unix))]
fn label_bytes(_path: &Path, _roots: &[PathBuf], _f: &Files) -> Option<Arc<[u8]>> {
    None
}

/// A path as reported: `/` between its parts. Only Windows has `\` as a separator; elsewhere it is
/// a character of the name.
pub fn slashed(s: std::borrow::Cow<'_, str>) -> String {
    if cfg!(windows) {
        windows_form(&s)
    } else {
        s.into_owned()
    }
}

/// A Windows path as it is printed: `/` between its parts. A verbatim drive path (`\\?\C:\x`,
/// what `canonicalize` gives) takes no forward slashes, so it is printed in its plain form.
fn windows_form(s: &str) -> String {
    let s = s.replace('\\', "/");
    match s.strip_prefix("//?/") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => s,
    }
}

pub fn relative(path: &Path, roots: &[PathBuf]) -> String {
    for r in roots {
        if let Ok(rel) = path.strip_prefix(r) {
            let s = slashed(rel.to_string_lossy());
            if s.is_empty() {
                // the root is the file itself
                return slashed(r.to_string_lossy());
            }
            if roots.len() == 1 {
                return s;
            }
            return format!("{}/{}", r.to_string_lossy().trim_end_matches('/'), s);
        }
    }
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        /// files of one line this thread searched in two passes through small buffers
        pub(super) static ONE_LINE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        /// this thread searches such files as one buffer instead
        pub(super) static NO_ONE_LINE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        /// the most batches of hits that waited for this thread's searches
        pub(super) static MOST_QUEUED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn query(text: &str, mode: Mode) -> Query {
        Query { pattern: Pattern { text: text.into(), mode, ..Pattern::default() }, ..Query::default() }
    }

    fn no_stop() -> Stop {
        Stop::new(Arc::default())
    }

    /// The line of a match or context record.
    fn line(r: Record) -> Line {
        match r {
            Record::Match(l) | Record::Context(l) => l,
            other => panic!("not a line: {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_reported_with_its_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let os = |b: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(b));
        let walked = Files::default();
        let given = Files { paths_as_given: true, ..Files::default() };
        let file = os(b"/r/d\xff/a\xfe.c");
        let bytes = |roots: &[PathBuf], o: &Files| label_bytes(&file, roots, o).map(|b| b.to_vec());
        assert_eq!(label_bytes(&os(b"/r/d/a.c"), &[os(b"/r")], &walked), None);
        assert_eq!(bytes(&[os(b"/r")], &walked).unwrap(), b"d\xff/a\xfe.c");
        assert_eq!(bytes(&[os(b"/r//"), os(b"/x")], &walked).unwrap(), b"/r/d\xff/a\xfe.c");
        assert_eq!(bytes(std::slice::from_ref(&file), &walked).unwrap(), b"/r/d\xff/a\xfe.c");
        assert_eq!(bytes(&[os(b"/r")], &given).unwrap(), b"/r/d\xff/a\xfe.c");
        assert_eq!(&*label(&file, &[os(b"/r")], &walked), "d\u{FFFD}/a\u{FFFD}.c");

        // the plain writer writes the bytes; a hit and a count carry them
        #[derive(Clone)]
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let written = Shared(Arc::default());
        let plain = PlainSink::new(Box::new(written.clone()));
        let raw: Arc<[u8]> = Arc::from(&b"d\xff/a\xfe.c"[..]);
        let (tx, rx) = mpsc::sync_channel(16);
        for (to_plain, kind) in [(true, Report::Lines), (false, Report::Lines), (false, Report::Counts)] {
            let o = Query { report: kind, ..query("x", Mode::Literal) };
            let matcher = build_matcher(&o.pattern).unwrap();
            let (c, stop) = (Counters::default(), no_stop());
            let r = Run {
                matcher: &matcher,
                q: &o,
                f: &walked,
                p: &rules(&walked),
                c: &c,
                stop: &stop,
                tx: &tx,
                plain: to_plain.then_some(&plain),
                labels: &[],
                raw: Some(&raw),
            };
            let rel: Arc<str> = label(&file, &[os(b"/r")], &walked);
            let mut sink = r.collector(Arc::clone(&rel));
            matcher.search_slice(&mut source::make_searcher(&o, BinaryDetection::quit(0), None, true), b"a x\n", &mut sink).unwrap();
            report(&r, sink, rel, false);
        }
        plain.flush().unwrap();
        assert_eq!(*written.0.lock().unwrap(), b"d\xff/a\xfe.c:1:a x\n");
        drop(tx);
        let records: Vec<Record> = rx.into_iter().flatten().collect();
        assert!(matches!(records[..], [Record::Match(_), Record::Count { count: 1, .. }]), "{records:?}");
        assert!(records.iter().all(|r| r.path_bytes() == Some(&raw[..]) && &**r.path() == "d\u{FFFD}/a\u{FFFD}.c"));
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn a_record_costs_no_more_than_the_line_it_carries_and_a_word() {
        // 104 bytes: what one result took when a flag and a sentinel line number told its kind
        assert_eq!(std::mem::size_of::<Line>(), 96);
        assert_eq!(std::mem::size_of::<Record>(), 104);
    }

    #[test]
    fn offsets_in_a_line_with_invalid_utf8_become_offsets_in_its_text() {
        // one invalid piece of three bytes keeps the length; a match inside it takes the U+FFFD whole
        let line = b"a\xF0\x9F\x98b\xFFc";
        let text = lossy(line);
        assert_eq!(text, "a\u{FFFD}b\u{FFFD}c");
        let mut spans = [(0, 1), (2, 3), (1, 4), (4, 5), (5, 6), (6, 7), (6, 9)];
        text_offsets(&mut spans, line, &text, false);
        assert_eq!(spans, [(0, 1), (1, 4), (1, 4), (4, 5), (5, 8), (8, 9), (8, 9)]);
        // valid text: only cut to its end, on a character boundary
        let mut spans = [(1, 2), (2, 7)];
        text_offsets(&mut spans, "aé".as_bytes(), "aé", true);
        assert_eq!(spans, [(1, 3), (1, 3)]);
    }

    #[test]
    fn hits_found_faster_than_the_caller_takes_them_wait_in_the_search_threads() {
        let root = std::env::temp_dir().join(format!("scrutate_bounded_hits_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("many/sub")).unwrap();
        std::fs::create_dir_all(root.join("one")).unwrap();
        let lines = "abc e\n".repeat(100_000);
        std::fs::write(root.join("one/a.txt"), &lines).unwrap();
        for i in 0..6 {
            std::fs::write(root.join(format!("many/sub/{i}.txt")), &lines).unwrap();
        }
        // one file on the calling thread; several on the small walk, the parallel walk and in ranges
        let shapes: [(&str, Option<usize>, u64); 4] =
            [("one", None, 64 << 20), ("many", Some(256), 64 << 20), ("many", None, 64 << 20), ("one", None, 1 << 16)];
        for (dir, small_tree, split_file_bytes) in shapes {
            let o = Query { want_spans: true, ..query("e", Mode::Literal) };
            let f = Files { threads: 3, small_tree, split_file_bytes, ..Files::walk(vec![root.join(dir)]) };
            MOST_QUEUED.with(|m| m.set(0));
            let mut n = 0u64;
            let mut take = |_| {
                if n == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                n += 1;
            };
            let stats = search(&o, &f, Arc::default(), Output::Records(&mut take)).unwrap();
            assert_eq!(n, stats.matches, "{dir}");
            assert_eq!(n, if dir == "one" { 100_000 } else { 600_000 });
            let most = MOST_QUEUED.with(|m| m.get());
            // a sender counts itself before it waits
            assert!(most <= HIT_BATCHES + 1 + f.threads, "{dir} {small_tree:?} {split_file_bytes}: {most} batches waited");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_thread_keeps_no_buffer_the_size_of_a_larger_file() {
        let root = std::env::temp_dir().join(format!("scrutate_thread_buffer_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("big.txt");
        std::fs::write(&path, format!("needle first\n{}needle last\n", "x\n".repeat(150_000))).unwrap();
        let o = query("needle", Mode::Literal);
        let f = Files { threads: 2, ..Files::default() };
        let matcher = build_matcher(&o.pattern).unwrap();
        let (c, stop) = (Counters::default(), no_stop());
        let (tx, rx) = mpsc::sync_channel(1024);
        let mut fw = file_worker(&o);
        for size in [None, Some(std::fs::metadata(&path).unwrap().len())] {
            search_one(
                &mut fw,
                &Run { matcher: &matcher, q: &o, f: &f, p: &rules(&f), c: &c, stop: &stop, tx: &tx, plain: None, labels: &[], raw: None },
                &path,
                Arc::from("big.txt"),
                size,
                false,
            );
        }
        drop(tx);
        let lines: Vec<u64> = rx.into_iter().flatten().map(|r| line(r).line).collect();
        assert_eq!(lines, [1, 150_002, 1, 150_002]);
        assert!(fw.small.capacity() <= source::SMALL_FILE as usize + 1, "{}", fw.small.capacity());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_a_file_of_one_line_is_read_whole_between_probes() {
        let root = std::env::temp_dir().join(format!("scrutate_whole_read_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("a.txt");
        // longer than the 1 MiB pieces `whole` reads
        let line = "x".repeat(3 << 19);
        for threads in [1, 3] {
            let o = query("needle", Mode::Literal);
            let f = Files { threads, ..Files::default() };
            let matcher = build_matcher(&o.pattern).unwrap();
            let (c, stop) = (Counters::default(), no_stop());
            let (tx, _rx) = mpsc::sync_channel(1024);
            let run = Run { matcher: &matcher, q: &o, f: &f, p: &rules(&f), c: &c, stop: &stop, tx: &tx, plain: None, labels: &[], raw: None };
            for (text, whole) in [(format!("{line}\n").repeat(4), false), (format!("{line}\n"), true), (line.clone(), true)] {
                std::fs::write(&path, &text).unwrap();
                let file = std::fs::File::open(&path).unwrap();
                // probes every 64 MiB, so none falls inside this file
                let got = source::split(&run, &file, text.len() as u64, 64 << 20, &Arc::from("a.txt"));
                assert_eq!(matches!(got, source::Outcome::Buffer(_)), whole, "{threads} threads, {} bytes", text.len());
            }
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_file_of_one_line_searched_in_two_passes_reports_what_one_buffer_does() {
        #[derive(Clone)]
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let root = std::env::temp_dir().join(format!("scrutate_one_line_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("a.txt");
        const P: usize = 256 << 10;
        let n = 3 * P + 1000;
        let at = |text: &[u8], put: &[(usize, &[u8])]| {
            let mut b = text.to_vec();
            for (i, s) in put {
                b[*i..*i + s.len()].copy_from_slice(s);
            }
            b
        };
        let x = vec![b'x'; n];
        let lf = |b: Vec<u8>, end: &[u8]| [b, end.to_vec()].concat();
        // (file, whether the two passes apply)
        let cases: Vec<(Vec<u8>, bool)> = vec![
            (at(&x, &[(5000, b"needle")]), true),
            (lf(at(&x, &[(5000, b"needle")]), b"\n"), true),
            (lf(at(&x, &[(5000, b"needle")]), b"\r\n"), true),
            (lf(at(&x, &[(n - 1, b"e")]), b"\r\n"), true),
            (x.clone(), true),
            (lf(x.clone(), b"\n"), true),
            (at(&x, &[(P - 3, b"needle"), (2 * P - 1, b"needle"), (3 * P - 5, b"needle")]), true),
            (at(&x, &[(P - 5, b"needle")]), true),
            (at(&x, &[(P - 1, b"needle")]), true),
            (at(&x, &[(0, b"needle"), (100, b"needle")]), true),
            (at(&x, &[(n - 6, b"needle")]), true),
            (lf(at(&x, &[(n - 6, b"needle")]), b"\n"), true),
            (at(&x, &[(100, b"\0"), (5000, b"needle")]), false),
            (at(&x, &[(5000, b"needle"), (2 * P, b"\0")]), false),
            (at(&x, &[(5000, b"needle"), (n - 1, b"\0")]), false),
            // past the first probe's window and before the second's
            (at(&x, &[(5000, b"needle"), (n / 8 + (64 << 10) + 100, b"\n")]), false),
            (at(&x, &[(5000, b"needle"), (n - 2, b"\n")]), false),
            (at(&x, &[(5000, b"needle"), (7, b"\xFF\xFE"), (P - 1, "\u{1F600}".as_bytes()), (2 * P - 2, "日".as_bytes()), (3 * P - 1, "é".as_bytes())]), true),
            (at(&x, &[(5000, b"needle"), (P - 2, b"\xF0\x9F\x98"), (n - 2, b"\xE6\x97")]), true),
            (lf(at(&x, &[(5000, b"needle"), (n - 1, b"\xC3")]), b"\n"), true),
            (at(&x, &[(0, b"\xEF\xBB\xBF"), (5000, b"needle")]), false),
        ];
        let plain = |with_filename, line_numbers| Query { with_filename, line_numbers, ..Query::default() };
        let given = Files { paths_as_given: true, ..Files::default() };
        let walked = Files::default();
        let pattern = |case, word| Pattern { case, word, ..Pattern::default() };
        let shapes = [
            (plain(true, true), &walked, true),
            (plain(false, false), &walked, true),
            (plain(true, false), &given, true),
            (Query { report: Report::Counts, ..Query::default() }, &walked, true),
            (Query { want_spans: true, ..Query::default() }, &walked, false),
            (Query { invert: true, ..plain(true, true) }, &walked, false),
            (Query { pattern: pattern(Case::Insensitive, false), ..plain(true, true) }, &walked, false),
            (Query { pattern: pattern(Case::Sensitive, true), ..plain(true, true) }, &walked, false),
        ];
        for (k, (text, applies)) in cases.iter().enumerate() {
            std::fs::write(&path, text).unwrap();
            let size = text.len() as u64;
            for (shape, (base, files, shape_applies)) in shapes.iter().enumerate() {
                for (pattern, literal) in [("needle", true), ("e\r", true), ("", false)] {
                    for threads in [1, 3] {
                        let o = Query { pattern: Pattern { text: pattern.into(), mode: Mode::Literal, ..base.pattern.clone() }, ..base.clone() };
                        let f = Files { threads, split_file_bytes: size, ..(*files).clone() };
                        let to_plain = o.report == Report::Lines && !o.want_spans;
                        let matcher = build_matcher(&o.pattern).unwrap();
                        let run = |off: bool| {
                            NO_ONE_LINE.with(|c| c.set(off));
                            ONE_LINE.with(|c| c.set(0));
                            let (c, stop) = (Counters::default(), no_stop());
                            let out = Shared(Arc::new(Mutex::new(Vec::new())));
                            let sink = PlainSink::new(Box::new(out.clone()));
                            let (tx, rx) = mpsc::sync_channel(1024);
                            let r = Run {
                                matcher: &matcher,
                                q: &o,
                                f: &f,
                                p: &rules(&f),
                                c: &c,
                                stop: &stop,
                                tx: &tx,
                                plain: to_plain.then_some(&sink),
                                labels: &[],
                                raw: None,
                            };
                            search_one(&mut file_worker(&o), &r, &path, Arc::from("a.txt"), Some(size), f.paths_as_given);
                            drop(tx);
                            sink.flush().unwrap();
                            let hits: Vec<String> = rx.into_iter().flatten().map(|r| format!("{r:?}")).collect();
                            let stats = (c.files_matched.into_inner(), c.matches.into_inner(), c.errors.into_inner());
                            let got = (out.0.lock().unwrap().clone(), hits, stats);
                            (got, ONE_LINE.with(|c| c.get()))
                        };
                        let (two, ran) = run(false);
                        let (one, _) = run(true);
                        let what = format!("case {k}, shape {shape}, pattern {pattern:?}, {threads} threads");
                        assert!(two == one, "{what}");
                        assert_eq!(ran, usize::from(*applies && *shape_applies && literal), "{what}");
                    }
                }
            }
        }
        NO_ONE_LINE.with(|c| c.set(false));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_empty_file_is_read_whole_on_any_thread_count() {
        let root = std::env::temp_dir().join(format!("scrutate_empty_whole_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("a.txt");
        std::fs::write(&path, b"").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let finder = memchr::memmem::Finder::new(b"abcd");
        for threads in [1, 3] {
            assert_eq!(source::whole(&file, 0, threads), Some(Vec::new()));
            assert_eq!(source::literal_in_pieces(&file, 0, &finder, threads, 4), Some(false));
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_literal_across_pieces_is_found_on_any_thread_count() {
        let root = std::env::temp_dir().join(format!("scrutate_pieces_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("a.txt");
        let finder = memchr::memmem::Finder::new(b"abcd");
        for at in 0..=36 {
            let mut text = vec![b'x'; 40];
            text[at..at + 4].copy_from_slice(b"abcd");
            std::fs::write(&path, &text).unwrap();
            let file = std::fs::File::open(&path).unwrap();
            for threads in [1, 2, 5] {
                for piece in [1, 3, 4, 7] {
                    assert_eq!(source::literal_in_pieces(&file, 40, &finder, threads, piece), Some(true), "at {at}, piece {piece}");
                }
            }
            text[at] = b'\n';
            std::fs::write(&path, &text).unwrap();
            assert_eq!(source::literal_in_pieces(&file, 40, &finder, 2, 4), None, "a line break at {at}");
        }
        std::fs::write(&path, b"xxabcd\n").unwrap();
        assert_eq!(source::literal_in_pieces(&std::fs::File::open(&path).unwrap(), 7, &finder, 2, 2), Some(true));
        std::fs::write(&path, b"xxabcd").unwrap();
        // shorter than the size the first pass was told: a read comes back short
        assert_eq!(source::literal_in_pieces(&std::fs::File::open(&path).unwrap(), 7, &finder, 1, 8), None);
        std::fs::write(&path, b"ab\xC3\xA9cd").unwrap();
        let (mut out, file) = (Vec::new(), std::fs::File::open(&path).unwrap());
        assert!(write_lossy_from(&mut out, &file, 6).unwrap().is_ok());
        assert_eq!(out, "ab\u{e9}cd".as_bytes());
        out.clear();
        assert!(write_lossy_from(&mut out, &file, 9).unwrap().is_err(), "a short read");
        assert_eq!(out, "ab\u{e9}cd".as_bytes());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn text_in_memory_gives_the_records_and_output_of_the_same_bytes_in_a_file() {
        #[derive(Clone)]
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let root = std::env::temp_dir().join(format!("scrutate_in_memory_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let lines = |n: usize, line: &str| line.repeat(n).into_bytes();
        // `sized(n, ..)`: lines up to exactly n bytes, a needle in the first and the last
        let sized = |n: usize, end: &[u8]| {
            let mut b = lines(n / 20 + 1, "a needle line here\nx\n");
            b.truncate(n - end.len());
            b.extend_from_slice(end);
            b
        };
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("one\nneedle two\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let small = source::SMALL_FILE as usize;
        let texts: Vec<(&str, Vec<u8>)> = vec![
            ("plain", b"alpha needle\nbeta\nneedle gamma\nbeta needle\n".to_vec()),
            ("empty", Vec::new()),
            ("no line end", b"one\nneedle".to_vec()),
            ("crlf", b"needle a\r\nb\r\nneedle\r\n".to_vec()),
            ("nul early", b"needle\0 x\nneedle\n".to_vec()),
            ("nul late", [lines(10_000, "needle line\n"), b"\0needle\n".to_vec()].concat()),
            ("nul in a split", [lines(30_000, "needle line\n"), b"\0".to_vec(), lines(30_000, "needle\n")].concat()),
            ("utf-8 bom", b"\xEF\xBB\xBFneedle\nbeta\n".to_vec()),
            ("utf-8 bom, large", [b"\xEF\xBB\xBF".to_vec(), lines(30_000, "needle line\n")].concat()),
            ("utf-16 bom", utf16),
            ("invalid utf-8", b"ne\xffedle needle \xfe\n\xC3needle\n".to_vec()),
            ("one long line", [b"needle".to_vec(), vec![b'x'; 300_000]].concat()),
            ("one long line ending", [vec![b'x'; 300_000], b"needle\r\n".to_vec()].concat()),
            ("one long line, no match", vec![b'x'; 300_000]),
            ("small file", sized(small, b"needle\n")),
            ("past small file", sized(small + 1, b"needle\n")),
            ("one-line threshold", [vec![b'y'; (1 << 16) + 1], b"\n".to_vec()].concat()),
            ("mid file", sized(1 << 20, b"needle\n")),
            ("past mid file", sized((1 << 20) + 1, b"needle\n")),
            ("split", sized(400_000, b"needle\n")),
            ("past split", sized(400_001, b"needle\n")),
        ];
        let pat = |text: &str, mode: Mode| Pattern { text: text.into(), mode, ..Pattern::default() };
        let queries: Vec<Query> = vec![
            query("needle", Mode::Literal),
            Query { want_spans: true, text_spans: true, ..query("needle", Mode::Literal) },
            Query { report: Report::Counts, ..query("needle", Mode::Literal) },
            Query { report: Report::Files, ..query("needle", Mode::Literal) },
            Query { before: 1, after: 1, ..query("needle", Mode::Literal) },
            Query { invert: true, ..query("needle", Mode::Literal) },
            Query { invert: true, report: Report::Counts, ..query("needle", Mode::Literal) },
            Query { pattern: Pattern { multiline: true, ..pat("needle\\s+(beta|line)", Mode::Regex) }, ..Query::default() },
            Query { pattern: Pattern { multiline: true, ..pat("needle\\s+(beta|line)", Mode::Regex) }, report: Report::Counts, ..Query::default() },
            Query { pattern: Pattern { case: Case::Insensitive, ..pat("NEE+DLE", Mode::Regex) }, ..Query::default() },
            Query { max_count: Some(1), ..query("needle", Mode::Literal) },
            Query { max_results: Some(3), ..query("needle", Mode::Literal) },
            query("x", Mode::Literal),
        ];
        let path = root.join("a.txt");
        let mut runs = 0;
        for (what, bytes) in &texts {
            std::fs::write(&path, bytes).unwrap();
            for o in &queries {
                for (threads, split_file_bytes) in [(1, 64 << 20), (3, 64 << 20), (3, 400_000)] {
                    for to_plain in [false, true] {
                        if to_plain && o.report != Report::Lines {
                            continue;
                        }
                        let found = |paths: Paths| {
                            let f = Files { paths, threads, split_file_bytes, ..Files::default() };
                            let o = Query { want_bytes: true, ..o.clone() };
                            let written = Shared(Arc::default());
                            let plain = Arc::new(PlainSink::new(Box::new(written.clone())));
                            let mut records = Vec::new();
                            let mut push = |r: Record| records.push(format!("{r:?}"));
                            let out = if to_plain { Output::Plain(Arc::clone(&plain)) } else { Output::Records(&mut push) };
                            let stats = search(&o, &f, Arc::default(), out).unwrap();
                            plain.flush().unwrap();
                            let stats =
                                (stats.files_searched, stats.files_matched, stats.matches, stats.bytes_searched, stats.errors, stats.cancelled, stats.failures);
                            let printed = written.0.lock().unwrap().clone();
                            (records, String::from_utf8_lossy(&printed).into_owned(), stats)
                        };
                        let on_disk = found(Paths::List { root: root.clone(), files: vec!["a.txt".into()] });
                        let in_memory = found(Paths::Memory(vec![("a.txt".into(), bytes.clone())]));
                        let shape = format!(
                            "{what}: {:?} {:?} report {:?}, {threads} threads, split at {split_file_bytes}, plain {to_plain}",
                            o.pattern.text, o.pattern.mode, o.report
                        );
                        assert_eq!(on_disk.2 .0, 1, "{shape}");
                        assert!(on_disk == in_memory, "{shape}\non disk: {on_disk:?}\nin memory: {in_memory:?}");
                        runs += 1;
                    }
                }
            }
        }
        assert!(runs > 1000, "{runs}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn texts_not_on_disk_are_searched_in_the_order_given_or_by_name() {
        let texts = vec![("b.txt".to_string(), b"needle b\n".to_vec()), ("a.txt".to_string(), b"needle a\nneedle\n".to_vec())];
        let names = |o: &Query| {
            let mut got = Vec::new();
            let f = Files { paths: Paths::Memory(texts.clone()), ..Files::default() };
            search(o, &f, Arc::default(), Output::Records(&mut |r| got.push(format!("{}:{}", r.path().clone(), line(r).line)))).unwrap();
            got
        };
        assert_eq!(names(&query("needle", Mode::Literal)), ["b.txt:1", "a.txt:1", "a.txt:2"]);
        let sorted = Query { first_in_path_order: true, max_results: Some(2), ..query("needle", Mode::Literal) };
        assert_eq!(names(&sorted), ["a.txt:1", "a.txt:2"]);
    }

    /// The lines `schedule` finds in `feed` with the thread rules reading `m`, sorted, and the most
    /// threads the rules wanted for a list or a walking thread.
    fn scheduled(m: &dyn Machine, q: &Query, f: &Files, feed: Feed) -> (Vec<String>, usize) {
        let (matcher, c, stop, p) = (build_matcher(&q.pattern).unwrap(), Counters::default(), no_stop(), Policy::read(m, f.threads));
        let labels = match &f.paths {
            Paths::Walk(roots) => roots.clone(),
            _ => Vec::new(),
        };
        let s = Common { matcher: &matcher, q, f, p: &p, m, c: &c, stop: &stop, plain: None, labels: &labels };
        let mut lines = Vec::new();
        schedule(feed, &s, &mut |r| {
            let path = r.path().to_string();
            lines.push(format!("{path}:{}", line(r).text))
        })
        .unwrap();
        lines.sort();
        (lines, c.most_wanted.load(Ordering::Relaxed))
    }

    fn rules(f: &Files) -> Policy {
        Policy::read(&System::default(), f.threads)
    }

    #[test]
    fn a_tree_of_one_file_is_searched_without_a_thread() {
        let root = std::env::temp_dir().join(format!("scrutate_small_threads_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let o = query("hello", Mode::Literal);
        let m = policy::tests::Given::new(11, Some(5), &WARM_MACOS, Some(1));
        let f = Files { threads: 2, ..Files::walk(vec![root.clone()]) };
        let found = |f: &Files| scheduled(&m, &o, f, Feed::Walk(std::slice::from_ref(&root)));
        assert_eq!(found(&f), (vec!["a.txt:hello".to_string()], 0));
        std::fs::write(root.join("b.txt"), "hello again\n").unwrap();
        assert_eq!(found(&f), (vec!["a.txt:hello".to_string(), "b.txt:hello again".to_string()], 2));
        // a walk cut after the first file still searches it, and the parallel walk the rest
        let cut = Files { small_tree: Some(1), ..f.clone() };
        assert_eq!(found(&cut), (vec!["a.txt:hello".to_string(), "b.txt:hello again".to_string()], 0));
        assert!(m.shown.lock().unwrap().is_empty(), "a number asked for is not counted");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// `n` files of one matching line each in `dir`, under `root`, listed as an index would.
    fn some_files(root: &Path, dir: &str, n: usize) -> Vec<(PathBuf, Arc<str>, bool)> {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        (0..n)
            .map(|i| {
                let rel = format!("{dir}/f{i:03}.txt");
                std::fs::write(root.join(&rel), "needle\n").unwrap();
                (root.join(&rel), Arc::from(rel.as_str()), false)
            })
            .collect()
    }

    /// macOS and Linux with no read slow enough to count as cold, so a busy machine adds no thread.
    static WARM_MACOS: policy::Values = policy::Values { cold_read_ns: u64::MAX, ..policy::MACOS };
    static WARM_LINUX: policy::Values = policy::Values { cold_read_ns: u64::MAX, ..policy::LINUX };

    #[test]
    fn a_file_list_and_a_walk_take_their_share_of_the_searches_running() {
        let root = std::env::temp_dir().join(format!("scrutate_share_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let big = some_files(&root, "big", policy::SMALL_SEARCH + 1);
        let small = some_files(&root, "small", 10);
        let o = query("needle", Mode::Literal);
        let f = Files::list(root.clone(), Vec::new());
        // three searches running on an M3 Pro: five performance cores split three ways
        let m = policy::tests::Given::new(11, Some(5), &WARM_MACOS, Some(3));
        let (lines, started) = scheduled(&m, &o, &f, Feed::List(&big, None));
        assert_eq!((lines.len(), started), (big.len(), 1));
        assert_eq!(*m.shown.lock().unwrap(), [(Some(3), 1)]);
        let m = policy::tests::Given::new(11, Some(5), &WARM_MACOS, Some(2));
        assert_eq!(scheduled(&m, &o, &f, Feed::List(&small, None)).1, 5, "a small list is not counted");
        assert!(m.shown.lock().unwrap().is_empty());
        assert_eq!(scheduled(&m, &o, &f, Feed::List(&big, Some(3))).1, 2, "the share holds with a cap");
        let walk = Files::walk(vec![root.clone()]);
        assert_eq!(scheduled(&m, &o, &walk, Feed::Walk(std::slice::from_ref(&root))).0.len(), big.len() + small.len());
        assert_eq!(m.shown.lock().unwrap().last(), Some(&(Some(2), 2)), "a walk of directories takes its share");
        // Linux counts nothing: the walk says so, a list starts all its threads
        let linux = policy::tests::Given::new(5, None, &WARM_LINUX, Some(3));
        assert_eq!(scheduled(&linux, &o, &walk, Feed::Walk(std::slice::from_ref(&root))).0.len(), big.len() + small.len());
        assert_eq!(scheduled(&linux, &o, &f, Feed::List(&big, None)).1, 5);
        assert_eq!(*linux.shown.lock().unwrap(), [(None, 5), (None, 5)]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_search_takes_threads_back_as_the_others_end() {
        let root = std::env::temp_dir().join(format!("scrutate_share_back_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files = some_files(&root, "d", 2000);
        let (o, f) = (query("needle", Mode::Literal), Files::list(root.clone(), Vec::new()));
        let m = policy::tests::Given::new(11, Some(5), &WARM_MACOS, Some(3));
        let (matcher, c, stop, p) = (build_matcher(&o.pattern).unwrap(), Counters::default(), no_stop(), Policy::read(&m, 0));
        let s = Common { matcher: &matcher, q: &o, f: &f, p: &p, m: &m, c: &c, stop: &stop, plain: None, labels: &[] };
        let mut found = 0;
        // the other two searches end once the first match is in
        schedule(Feed::List(&files, None), &s, &mut |_| {
            found += 1;
            *m.running.lock().unwrap() = Some(1);
        })
        .unwrap();
        assert_eq!(found, files.len());
        let shown = m.shown.lock().unwrap();
        assert_eq!((shown.first(), shown.last()), (Some(&(Some(3), 1)), Some(&(Some(1), 5))), "{shown:?}");
        assert_eq!(c.started.load(Ordering::Relaxed), 5, "four more threads start");
        drop(shown);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_file_list_read_from_the_disk_earns_threads_by_its_slow_reads() {
        // every read counts as slow, as on a cold disk
        static COLD: policy::Values = policy::Values { cold_read_ns: 0, cold_threads: 0, shared: false };
        let root = std::env::temp_dir().join(format!("scrutate_cold_list_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let files = some_files(&root, "d", 200);
        let o = query("needle", Mode::Literal);
        let m = policy::tests::Given::new(2, None, &COLD, None);
        let f = Files::list(root.clone(), Vec::new());
        // two threads, and one more for each 16 slow reads up to twice the cores
        assert_eq!(scheduled(&m, &o, &f, Feed::List(&files, None)), (files.iter().map(|x| format!("{}:needle", x.1)).collect::<Vec<_>>(), 4));
        let asked = Files { threads: 2, ..f.clone() };
        assert_eq!(scheduled(&m, &o, &asked, Feed::List(&files, None)).1, 2, "-j is kept");
        let warm = policy::tests::Given::new(2, None, &WARM_MACOS, None);
        assert_eq!(scheduled(&warm, &o, &f, Feed::List(&files, None)).1, 2, "warm reads add none");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_windows_path_is_printed_with_slashes_and_a_verbatim_drive_path_in_its_plain_form() {
        assert_eq!(windows_form(r"C:\Users\x\a.txt"), "C:/Users/x/a.txt");
        assert_eq!(windows_form(r"\\?\C:\Users\x\a.txt"), "C:/Users/x/a.txt");
        assert_eq!(windows_form(r"dir\a.txt"), "dir/a.txt");
        assert_eq!(windows_form(r"\\server\share\a.txt"), "//server/share/a.txt");
        assert_eq!(windows_form(r"\\?\UNC\server\share\a.txt"), "//?/UNC/server/share/a.txt");
    }

    #[test]
    fn write_lossy_equals_from_utf8_lossy() {
        let parts: &[&[u8]] = &[b"a", b"\xC3\xA9", b"\xE6\x97\xA5", b"\xF0\x9F\x98\x80", b"\xFF", b"\xC3", b"\xE6\x97", b"\xF0\x9F", b"\x80", b"\n"];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        for _ in 0..20_000 {
            let mut b = Vec::new();
            for _ in 0..next(12) {
                b.extend_from_slice(parts[next(parts.len())]);
            }
            let piece = 1 + next(9);
            let mut out = Vec::new();
            write_lossy(&mut out, &b, piece).unwrap();
            assert_eq!(out, String::from_utf8_lossy(&b).as_bytes(), "{b:?} piece {piece}");
        }
    }

    #[test]
    fn suffix_candidates_find_the_same_lines() {
        let words = [
            "dev_resume(",
            "x_suspend(",
            " _resume(",
            "KÖNIG",
            "königin",
            "pm_RESUME(",
            "rc",
            "\t",
            "ing ",
            "sing\n",
            "12px",
            "é_resume(",
            "abc",
            "_",
            "(",
            "\n",
            "\r\n",
            "\u{212A}önig",
        ];
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut hay = String::new();
        for _ in 0..20000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            hay.push_str(words[(state % words.len() as u64) as usize]);
        }
        let lines = |m: &AnyMatcher, invert: bool| {
            let mut v = Vec::new();
            let mut s = SearcherBuilder::new().line_number(true).invert_match(invert).build();
            let AnyMatcher::Regex(m) = m else { unreachable!() };
            s.search_slice(
                m,
                hay.as_bytes(),
                grep_searcher::sinks::Lossy(|n, _| {
                    v.push(n);
                    Ok(true)
                }),
            )
            .unwrap();
            v
        };
        let mut engaged = 0;
        for (pat, case) in [
            (r"\w+_(suspend|resume)\(", Case::Sensitive),
            (r"\w+_resume\(", Case::Insensitive),
            (r"\w+KÖNIG", Case::Insensitive),
            (r"[a-z]+ing\b", Case::Sensitive),
            (r"\d+px", Case::Sensitive),
            (r".*abc", Case::Sensitive),
            (r"\S*_resume\($", Case::Sensitive),
            (r"PM_RESUME", Case::Insensitive),
        ] {
            let o = Pattern { text: pat.to_string(), case, ..Pattern::default() };
            let fast = build_matcher(&o).unwrap();
            let AnyMatcher::Regex(ref f) = fast else { unreachable!() };
            engaged += f.suffixes.is_some() as usize;
            let slow = AnyMatcher::Regex(LineRegex::plain(f.inner.clone()));
            for invert in [false, true] {
                let (a, b) = (lines(&fast, invert), lines(&slow, invert));
                assert_eq!(a, b, "{pat} {case:?} invert={invert}");
                assert!(invert || !a.is_empty(), "{pat}: no matches, the test proves nothing");
            }
        }
        // the rest have a short suffix, a literal prefix, or too many case variants (s folds to ſ too)
        assert_eq!(engaged, 5);
        let o = Pattern { text: "PM_RESUME".into(), ..Pattern::default() };
        let AnyMatcher::Regex(m) = build_matcher(&o).unwrap() else { unreachable!() };
        assert!(m.suffixes.is_none(), "a literal prefix is the engine's job");
    }

    #[test]
    fn line_start_candidates_find_the_same_lines() {
        let words = ["a", "aa", "aaa", "b", "!", "\n", "\r\n", "#include <x>", "x", "A", "é", "É", " ", "\t", "ab", "\n\n", "\nab"];
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut hay = String::new();
        for _ in 0..20000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            hay.push_str(words[(state % words.len() as u64) as usize]);
        }
        let lines = |m: &AnyMatcher, invert: bool| {
            let mut v = Vec::new();
            let mut s = SearcherBuilder::new().line_number(true).invert_match(invert).build();
            let AnyMatcher::Regex(m) = m else { unreachable!() };
            s.search_slice(
                m,
                hay.as_bytes(),
                grep_searcher::sinks::Lossy(|n, _| {
                    v.push(n);
                    Ok(true)
                }),
            )
            .unwrap();
            v
        };
        let mut engaged = 0;
        for (pat, case) in [
            (r"^(a|aa)+$", Case::Sensitive),
            (r"^a", Case::Sensitive),
            (r"^#include", Case::Sensitive),
            (r"^(?:a|b)+!", Case::Insensitive),
            (r"^é", Case::Insensitive),
            (r"^ab?\s*$", Case::Sensitive),
            (r"^a$|^b$", Case::Sensitive),
            (r"^$", Case::Sensitive),
            (r"^\w", Case::Sensitive),
            (r"^a|b", Case::Sensitive),
            (r"^a\z", Case::Sensitive),
            (r"^a\s*", Case::Sensitive),
            (r"^(?:a|ab)(?:\n|b)", Case::Sensitive),
            (r"^a[^x]", Case::Sensitive),
        ] {
            let o = Pattern { text: pat.to_string(), case, ..Pattern::default() };
            let fast = build_matcher(&o).unwrap();
            let AnyMatcher::Regex(ref f) = fast else { unreachable!() };
            engaged += f.line_start.is_some() as usize;
            let slow = AnyMatcher::Regex(LineRegex::plain(f.inner.clone()));
            for invert in [false, true] {
                let (a, b) = (lines(&fast, invert), lines(&slow, invert));
                assert_eq!(a, b, "{pat} {case:?} invert={invert}");
                assert!(invert || !a.is_empty(), "{pat}: no matches, the test proves nothing");
            }
            for line in hay.split('\n').take(3000) {
                assert_eq!(spans_in(&fast, line.as_bytes()), spans_in(&slow, line.as_bytes()), "{pat} on {line:?}");
            }
            assert_eq!(spans_in(&fast, hay.as_bytes()), spans_in(&slow, hay.as_bytes()), "{pat} on the whole text");
            // every short text over a small alphabet, from every offset
            let (AnyMatcher::Regex(fm), AnyMatcher::Regex(sm)) = (&fast, &slow) else { unreachable!() };
            let alphabet: [&[u8]; 5] = [b"a", b"b", b"\n", b"!", "é".as_bytes()];
            let mut texts: Vec<Vec<u8>> = vec![Vec::new()];
            for _ in 0..4 {
                let more: Vec<Vec<u8>> = texts.iter().flat_map(|t| alphabet.iter().map(move |c| [t.as_slice(), c].concat())).collect();
                texts.extend(more);
            }
            for t in &texts {
                for at in 0..=t.len() {
                    assert_eq!(fm.find_at(t, at).unwrap(), sm.find_at(t, at).unwrap(), "{pat} find_at {t:?} {at}");
                    assert_eq!(fm.shortest_match_at(t, at).unwrap().is_some(), sm.shortest_match_at(t, at).unwrap().is_some(), "{pat} {t:?} {at}");
                }
            }
        }
        // `^$`, `^\w` (no finite prefix literals), `^a|b` (not anchored), `^a\z` and a line break in
        // the prefix keep the plain path
        assert_eq!(engaged, 9);
    }

    fn fuzzy_find(pat: &str, k: u8, hay: &str) -> Option<(usize, usize)> {
        let m = FuzzyMatcher::new(pat, k, false).unwrap();
        m.find_at(hay.as_bytes(), 0).unwrap().map(|m| (m.start(), m.end()))
    }

    /// First end (and distance) of a match within `k` edits: the textbook dynamic programme.
    fn naive_first_end(pat: &[u8], hay: &[u8], k: usize) -> Option<(usize, usize)> {
        let m = pat.len();
        let mut col: Vec<usize> = (0..=m).collect();
        for (i, &c) in hay.iter().enumerate() {
            let mut next = vec![0usize; m + 1];
            for j in 1..=m {
                next[j] = (next[j - 1] + 1).min(col[j] + 1).min(col[j - 1] + usize::from(pat[j - 1] != c));
            }
            col = next;
            if col[m] <= k {
                return Some((i + 1, col[m]));
            }
        }
        None
    }

    #[test]
    fn fuzzy_blocks_agree_with_the_dynamic_programme() {
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        for round in 0..400 {
            let m = 1 + next(300) as usize;
            let pat: Vec<u8> = (0..m).map(|_| b"abc"[next(3) as usize]).collect();
            // text: the pattern with a few edits, inside noise, so matches at small distances exist
            let mut hay: Vec<u8> = (0..next(200)).map(|_| b"abcd"[next(4) as usize]).collect();
            let mut copy = pat.clone();
            for _ in 0..next(6) {
                let at = next(copy.len() as u64) as usize;
                match next(3) {
                    0 => copy[at] = b'd',
                    1 => drop(copy.remove(at)),
                    _ => copy.insert(at, b'a'),
                }
                if copy.is_empty() {
                    copy.push(b'b');
                }
            }
            hay.extend_from_slice(&copy);
            hay.extend((0..next(50)).map(|_| b"abcd"[next(4) as usize]));
            let k = (next(8) as usize).min(m - 1);
            let f = FuzzyMatcher::new(std::str::from_utf8(&pat).unwrap(), k as u8, false).unwrap();
            assert_eq!(f.first_end(&hay, 0), naive_first_end(&pat, &hay, k), "round {round}: m={m} k={k}");
            if m <= 128 {
                // the block path on short patterns too, against the single-word one
                let b = Blocks::new(&fuzzy_classes(std::str::from_utf8(&pat).unwrap(), false));
                let (mut pv, mut mv) = b.start();
                let mut score = m as isize;
                let mut got = None;
                for i in 0..hay.len() {
                    score += b.step(&mut pv, &mut mv, b.eq_at(&hay, i).0);
                    if score <= k as isize {
                        got = Some((i + 1, score as usize));
                        break;
                    }
                }
                assert_eq!(got, naive_first_end(&pat, &hay, k), "blocks, round {round}: m={m} k={k}");
            }
        }
        assert!(FuzzyMatcher::new(&"a".repeat(FUZZY_MAX + 1), 1, false).is_err());
    }

    #[test]
    fn fuzzy_exact_and_edits() {
        assert_eq!(fuzzy_find("token", 0, "the token here"), Some((4, 9)));
        assert_eq!(fuzzy_find("token", 1, "a tokne b"), Some((2, 6))); // "tokn" is one deletion away
        assert_eq!(fuzzy_find("token", 1, "a tkoen b"), None); // a transposition is two edits
        assert_eq!(fuzzy_find("token", 2, "a tkoen b"), Some((2, 7)));
        assert_eq!(fuzzy_find("token", 1, "a tokn b"), Some((2, 6))); // one deletion
        assert_eq!(fuzzy_find("token", 1, "a tokken b"), Some((2, 8))); // one insertion
        assert_eq!(fuzzy_find("token", 1, "a tomen b"), Some((2, 7))); // one substitution
        assert_eq!(fuzzy_find("token", 0, "nothing"), None);
        assert_eq!(fuzzy_find("token", 1, "tok\nen"), None); // never across lines
    }

    /// `naive_first_end` over characters: each unit is a character or a byte of invalid UTF-8.
    fn naive_first_end_chars(classes: &[Vec<char>], hay: &[u8], k: usize) -> Option<(usize, usize)> {
        let m = classes.len();
        let mut col: Vec<usize> = (0..=m).collect();
        let mut i = 0;
        while i < hay.len() {
            let (c, n) = if hay[i] < 0x80 { (Some(hay[i] as char), 1) } else { wide_char(hay, i) };
            i += n;
            if c == Some('\n') {
                col = (0..=m).collect();
                continue;
            }
            let mut next = vec![0usize; m + 1];
            for j in 1..=m {
                let same = c.is_some_and(|c| classes[j - 1].contains(&c));
                next[j] = (next[j - 1] + 1).min(col[j] + 1).min(col[j - 1] + usize::from(!same));
            }
            col = next;
            if col[m] <= k {
                return Some((i, col[m]));
            }
        }
        None
    }

    #[test]
    fn fuzzy_edits_are_characters_and_case_folds_as_in_the_regex_engine() {
        assert_eq!(fuzzy_find("naïve", 1, "a naive b"), Some((2, 7)));
        assert_eq!(fuzzy_find("naive", 1, "a naïve b"), Some((2, 8)));
        assert_eq!(fuzzy_find("日本語", 1, "日本誤のテキスト"), Some((0, 6)));
        assert_eq!(fuzzy_find("naive", 1, "na\u{0}ve"), Some((0, 5)));
        let m = FuzzyMatcher::new("naive", 1, false).unwrap();
        assert_eq!(m.find_at(b"x na\xffve", 0).unwrap().map(|s| (s.start(), s.end())), Some((2, 7)), "invalid UTF-8 is one unit per byte");
        assert_eq!(m.find_at(b"\xff\xfe\xc3", 0).unwrap(), None);
        for (pat, hay) in [("ÉTÉ", "été"), ("été", "ÉTÉ"), ("kelvin", "\u{212A}ELVIN"), ("ß", "ẞ"), ("Σ", "ς")] {
            let m = FuzzyMatcher::new(pat, 0, true).unwrap();
            assert!(m.find_at(hay.as_bytes(), 0).unwrap().is_some(), "{pat} in {hay}");
            assert!(regex::RegexBuilder::new(pat).case_insensitive(true).build().unwrap().is_match(hay), "the regex engine agrees on {pat}");
        }
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let alpha = ["a", "b", "é", "É", "ü", "日", "k", "\u{212A}", "\u{ff}"];
        for round in 0..600 {
            let fold = next(2) == 0;
            let long = next(4) == 0;
            let m = if long { 129 + next(60) as usize } else { 1 + next(9) as usize };
            let pat: String = (0..m).map(|_| alpha[next(8) as usize]).collect();
            let mut hay: Vec<u8> = Vec::new();
            for _ in 0..next(if long { 400 } else { 40 }) {
                match next(12) {
                    0 => hay.push(0xff),
                    1 => hay.push(b'\n'),
                    _ => hay.extend_from_slice(alpha[next(alpha.len() as u64) as usize].as_bytes()),
                }
            }
            if next(2) == 0 {
                let at = (next(hay.len() as u64 + 1) as usize).min(hay.len());
                hay.splice(at..at, pat.bytes().take(pat.len() - 1));
            }
            let k = (next(4) as usize).min(m - 1);
            let f = FuzzyMatcher::new(&pat, k as u8, fold).unwrap();
            let want = naive_first_end_chars(&fuzzy_classes(&pat, fold), &hay, k);
            assert_eq!(f.first_end(&hay, 0), want, "round {round}: {pat:?} k={k} fold={fold} in {hay:?}");
            let found = f.find_at(&hay, 0).unwrap();
            assert_eq!(found.map(|s| s.end()), want.map(|w| w.0), "round {round}: the prefilter keeps every match");
            if let Some(s) = found {
                let mut at = 0;
                while at < s.start() {
                    at += unit_len(&hay, at);
                }
                assert_eq!(at, s.start(), "round {round}: a match starts on a character: {s:?} in {hay:?}");
            }
        }
    }

    #[test]
    fn fuzzy_case_fold() {
        let m = FuzzyMatcher::new("Token", 0, true).unwrap();
        assert!(m.find_at(b"a TOKEN b", 0).unwrap().is_some());
        let m = FuzzyMatcher::new("Token", 0, false).unwrap();
        assert!(m.find_at(b"a TOKEN b", 0).unwrap().is_none());
    }

    #[test]
    fn smart_case_counts_only_letters_that_stand_for_themselves() {
        for (pattern, regex, upper) in [
            ("foo", true, false),
            ("Foo", true, true),
            (r"\Sfoo", true, false),
            (r"\W+\Bfoo\d", true, false),
            (r"\pLfoo\p{Lu}x", true, false),
            (r"\SFoo", true, true),
            (r"foo\\Bar", true, true),
            (r"\Sfoo", false, true),
            (r"\x41bc", true, true),
            (r"\x61bc", true, false),
            (r"\x{41}bc", true, true),
            (r"\x{1F600}bc", true, false),
            (r"\u0041bc", true, true),
            (r"\u00e9bc", true, false),
            (r"\U00000041bc", true, true),
            (r"\xzz", true, false),
        ] {
            assert_eq!(uppercase_literal(pattern, regex), upper, "{pattern:?} regex={regex}");
        }
        let o = Pattern { text: "fooz".into(), mode: Mode::Fuzzy { max_edits: 1 }, word: true, ..Pattern::default() };
        assert!(build_matcher(&o).err().unwrap().to_string().contains("-w) is not available with --fuzzy"));
        let o = Pattern { text: "fooz".into(), mode: Mode::FuzzyRegex { max_edits: 1 }, multiline: true, ..Pattern::default() };
        assert!(build_matcher(&o).err().unwrap().to_string().contains("-U) is not available with --fuzzy"));
    }

    #[test]
    fn identifier_parts_cover_digits_and_acronyms() {
        let p = |s: &str| ident_parts(s);
        assert_eq!(p("makeWidget3"), vec!["make", "Widget", "3"]);
        assert_eq!(p("make_widget_3"), vec!["make", "widget", "3"]);
        assert_eq!(p("XMLHttpRequest2"), vec!["XML", "Http", "Request", "2"]);
        assert_eq!(p("HTTP2Server"), vec!["HTTP", "2", "Server"]);
        assert_eq!(p("MAKE_WIDGET_3"), vec!["MAKE", "WIDGET", "3"]);
        let re = regex::Regex::new(&identifier_regex("makeWidget3")).unwrap();
        assert!(re.is_match("x = make_widget_3(1)") && re.is_match("MAKE_WIDGET_3") && re.is_match("makeWidget3"));
        assert!(!re.is_match("make_widget_30") && !re.is_match("make_widget_3x"));
    }

    #[test]
    fn identifier_regex_matches_every_style() {
        let re = regex::Regex::new(&identifier_regex("refreshToken")).unwrap();
        for s in ["refresh_token", "refreshToken", "RefreshToken", "REFRESH_TOKEN", "refresh-token"] {
            assert!(re.is_match(&format!("x = {s}(1)")), "{s}");
        }
        assert!(!re.is_match("refreshTokens"));
        assert!(!re.is_match("refresh tokens"));
    }

    #[test]
    fn joined_patterns_keep_each_mode_meaning() {
        let ps = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(join_patterns(&ps(&["a.b"]), Mode::Pcre2, true).unwrap(), (r"a\.b".to_string(), Mode::Pcre2));
        assert_eq!(join_patterns(&ps(&["a.b"]), Mode::Literal, false).unwrap(), ("a.b".to_string(), Mode::Literal));
        assert_eq!(join_patterns(&ps(&["a.b", "c"]), Mode::Literal, true).unwrap(), (r"(?:a\.b)|(?:c)".to_string(), Mode::Regex));
        assert_eq!(join_patterns(&ps(&["a.b", "c"]), Mode::Pcre2, true).unwrap().1, Mode::Pcre2);
        let (p, m) = join_patterns(&ps(&["fooBar", "x"]), Mode::Identifier, false).unwrap();
        assert_eq!((p.as_str(), m), (r"(?:(?i)\bfoo[_\-]?Bar\b)|(?:(?i)\bx\b)", Mode::Regex));
        let e = join_patterns(&ps(&["a", "b"]), Mode::Fuzzy { max_edits: 1 }, false).unwrap_err();
        assert_eq!(e.to_string(), "--fuzzy takes one pattern");
        let zip = Files { search_zip: true, hidden: true, ..Files::default() };
        assert!(zip.check_indexable().unwrap_err().to_string().starts_with("--index does not cover archive members"));
        assert!(Files { max_depth: Some(1), ..Files::default() }.check_indexable().unwrap_err().to_string().starts_with("--index searches the files"));
        assert!(Files::default().check_indexable().is_ok());
    }

    #[test]
    fn held_output_leaves_in_fewer_writes_and_never_waits_on_a_quiet_sink() {
        struct Writes(Arc<Mutex<Vec<Vec<u8>>>>);
        impl Write for Writes {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().push(b.to_vec());
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writes = Arc::new(Mutex::new(Vec::new()));
        let sink = PlainSink::new(Box::new(Writes(Arc::clone(&writes))));
        let stop = no_stop();
        let written = || writes.lock().unwrap().concat();
        let quiet = || std::thread::sleep(std::time::Duration::from_millis(5));
        let mut all = Vec::new();
        for i in 0..1000 {
            let line = format!("file{i}:1:text\n");
            sink.write(line.as_bytes(), false, &stop);
            all.extend_from_slice(line.as_bytes());
        }
        assert!(writes.lock().unwrap().len() < 500, "a burst leaves in few writes");
        quiet();
        sink.tick(&stop);
        assert_eq!(written(), all, "held output leaves once it has waited");
        quiet();
        sink.write(b"late:1:text\n", false, &stop);
        all.extend_from_slice(b"late:1:text\n");
        assert_eq!(written(), all, "output after a quiet spell leaves at once");
        sink.write(b"last:1:text\n", false, &stop);
        sink.write_line(b"", b"long:1:", Text::Here(b"x\xFFy"), false, &stop).unwrap();
        sink.flush().unwrap();
        all.extend_from_slice("last:1:text\nlong:1:x\u{FFFD}y\n".as_bytes());
        assert_eq!(written(), all);
        assert!(!sink.broken.load(Ordering::Relaxed) && !stop.is_set());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(4000))]
        #[test]
        fn a_case_insensitive_literal_finds_every_line_the_regex_matches(
            needle in proptest::collection::vec(0usize..FOLD_CHARS.len(), 1..8),
            long in proptest::option::of((0usize..FOLD_CHARS.len(), 14usize..40)),
            pieces in proptest::collection::vec((0u8..5, 0usize..80, proptest::collection::vec(0usize..64, 8)), 0..8),
            regex in proptest::prelude::any::<bool>(),
            word in proptest::prelude::any::<bool>(),
        ) {
            let chars: Vec<char> = match long {
                Some((c, n)) => vec![FOLD_CHARS[c]; n],
                None => needle.iter().map(|&i| FOLD_CHARS[i]).collect(),
            };
            let pattern: String = chars.iter().collect();
            let mut hay = Vec::new();
            for (kind, pad, picks) in &pieces {
                // padding puts the piece on either side of 16, 32 and 64 byte boundaries
                hay.extend(std::iter::repeat_n(b'x', *pad));
                match kind {
                    0 | 1 => {
                        let miss = *kind == 1;
                        for (n, &c) in chars.iter().enumerate() {
                            let forms = fold_class(c);
                            let c = if miss && n == picks[0] % chars.len() { FOLD_CHARS[picks[1] % FOLD_CHARS.len()] } else { forms[picks[n % 8] % forms.len()] };
                            hay.extend(c.to_string().as_bytes());
                        }
                    }
                    2 => hay.extend(picks.iter().map(|&p| [b'\n', b' ', 0xFF, 0xC3, b'A', b'a', b'_', b'0'][p % 8])),
                    // a run of the first character one short of the needle, as long, or one longer
                    4 => {
                        let forms = fold_class(chars[0]);
                        for n in 0..chars.len() + picks[0] % 3 - 1 {
                            hay.extend(forms[picks[n % 8] % forms.len()].to_string().as_bytes());
                        }
                    }
                    _ => hay.push(b'\n'),
                }
            }
            let mode = if regex { Mode::Regex } else { Mode::Literal };
            let pattern = if regex { regex::escape(&pattern) } else { pattern };
            let o = Pattern { text: pattern, mode, case: Case::Insensitive, word, ..Pattern::default() };
            let AnyMatcher::Regex(m) = build_matcher(&o).unwrap() else { panic!("not a regex matcher") };
            let mut start = 0;
            for line in hay.split(|&b| b == b'\n') {
                let end = start + line.len();
                if m.inner.is_match(line).unwrap() {
                    let got = m.find_candidate_line(&hay[start..]).unwrap();
                    let at = match got {
                        Some(grep_matcher::LineMatchKind::Candidate(i) | grep_matcher::LineMatchKind::Confirmed(i)) => start + i,
                        None => usize::MAX,
                    };
                    proptest::prop_assert!(at <= end, "{:?} in {:?}: line {start}..{end}, candidate {at}", o.text, String::from_utf8_lossy(&hay));
                }
                start = end + 1;
            }
        }
    }

    #[test]
    fn a_case_insensitive_literal_costs_no_more_on_longer_near_misses() {
        // lines one byte shorter than the needle: almost every position fits the two scanned bytes
        let time = |n: usize| {
            let hay = format!("{}\n", "=".repeat(n - 1)).repeat((256 << 10) / n);
            let o = Pattern { text: "=".repeat(n), mode: Mode::Literal, case: Case::Insensitive, ..Pattern::default() };
            let AnyMatcher::Regex(m) = build_matcher(&o).unwrap() else { panic!("not a regex matcher") };
            assert!(m.fold.is_some());
            let search = || {
                let t = Instant::now();
                let sink = grep_searcher::sinks::Lossy(|_, _| -> std::io::Result<bool> { panic!("no line matches") });
                SearcherBuilder::new().build().search_slice(&m, hay.as_bytes(), sink).unwrap();
                t.elapsed()
            };
            (0..3).map(|_| search()).min().unwrap()
        };
        let (short, long) = (time(32), time(2048));
        assert!(long < short * 4, "needle of 32: {short:?}, of 2048: {long:?}");
    }

    const FOLD_CHARS: &[char] = &[
        'a', 'b', 'c', 'e', 'P', 'm', 'R', 'z', 'k', 'K', 's', 'S', '_', '0', '-', '.', 'ß', 'ẞ', 'σ', 'Σ', 'ς', 'İ', 'ı', 'i', 'I', 'é', 'Ö', 'ö', '\u{212A}',
        '\u{17F}', '\u{301}', ' ',
    ];

    #[test]
    fn a_case_insensitive_literal_has_its_own_finder_only_when_it_can() {
        let fold = |pattern: &str, mode, case| match build_matcher(&Pattern { text: pattern.into(), mode, case, ..Pattern::default() }).unwrap() {
            AnyMatcher::Regex(m) => m.fold.is_some(),
            _ => false,
        };
        assert!(fold("pm_resume", Mode::Literal, Case::Insensitive));
        assert!(fold("KÖNIG", Mode::Regex, Case::Insensitive));
        assert!(fold("error", Mode::Literal, Case::Smart));
        assert!(!fold("error", Mode::Literal, Case::Sensitive));
        assert!(!fold("[A-Z]+_RESUME", Mode::Regex, Case::Insensitive));
        assert!(!fold("ks", Mode::Literal, Case::Insensitive));
        assert!(!fold("Error", Mode::Literal, Case::Smart));
    }
}
