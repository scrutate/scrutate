//! Approximate regular expressions (`grep --fuzzy-regex=N`): a line matches when some part of it is
//! within N insertions, deletions or substitutions of a string the pattern matches. The pattern
//! becomes a Glushkov automaton over characters (one state per character position, at most 64),
//! and the search runs it bit-parallel with one state set per number of errors (Navarro and
//! Raffinot, "Flexible Pattern Matching in Strings", section 6.5). An edit is one character: an
//! ASCII byte, a UTF-8 encoded character, or a byte of invalid UTF-8 (which no position matches).
//! Case folds as the regex engine folds it. Anchors and word boundaries are not supported.

use anyhow::{bail, Context, Result};
use grep_matcher::{LineTerminator, Match, Matcher, NoCaptures, NoError};
use regex_syntax::hir::{Class, Hir, HirKind};

pub const MAX_POSITIONS: usize = 64;

/// The characters of a position: inclusive code point ranges.
type Ranges = Vec<(u32, u32)>;

#[derive(Clone, Debug)]
pub struct FuzzyRegex {
    /// positions whose class holds the ASCII character
    ascii: [u64; 128],
    /// non-ASCII characters in ranges: `wide[i]` holds the positions of `cuts[i]..cuts[i + 1]`
    cuts: Vec<u32>,
    wide: Vec<u64>,
    /// follow sets by 8-bit chunk of a state set: chunk c, value v -> union of follow(i), i in v
    follow: Box<[[u64; 256]; 8]>,
    first: u64,
    last: u64,
    k: usize,
    /// state sets before any text byte, per number of errors (deletions from the start)
    init: Vec<u64>,
    /// the reversed pattern, which finds where a match that ends at a known byte starts
    rev: Option<Box<FuzzyRegex>>,
}

struct Build {
    classes: Vec<Ranges>,
    follow: Vec<u64>,
}

/// `h` matching the reversed strings: a concatenation and each literal read back to front.
fn reversed(h: &Hir) -> Hir {
    use regex_syntax::hir::Repetition;
    match h.kind() {
        HirKind::Literal(lit) => match std::str::from_utf8(&lit.0) {
            Ok(t) => Hir::literal(t.chars().rev().collect::<String>().into_bytes()),
            Err(_) => h.clone(),
        },
        HirKind::Capture(cap) => reversed(&cap.sub),
        HirKind::Concat(items) => Hir::concat(items.iter().rev().map(reversed).collect()),
        HirKind::Alternation(items) => Hir::alternation(items.iter().map(reversed).collect()),
        HirKind::Repetition(r) => Hir::repetition(Repetition { min: r.min, max: r.max, greedy: r.greedy, sub: Box::new(reversed(&r.sub)) }),
        _ => h.clone(),
    }
}

/// (nullable, first, last) of a sub-expression, positions added to `b`.
fn glushkov(h: &Hir, b: &mut Build) -> Result<(bool, u64, u64)> {
    let new_pos = |b: &mut Build, class: Ranges| -> Result<u64> {
        if b.classes.len() == MAX_POSITIONS {
            bail!("fuzzy regex patterns are limited to {MAX_POSITIONS} character positions");
        }
        b.classes.push(class);
        b.follow.push(0);
        Ok(1u64 << (b.classes.len() - 1))
    };
    let concat = |b: &mut Build, (n1, f1, l1): (bool, u64, u64), (n2, f2, l2): (bool, u64, u64)| {
        for i in 0..b.follow.len() {
            if l1 & (1 << i) != 0 {
                b.follow[i] |= f2;
            }
        }
        (n1 && n2, f1 | if n1 { f2 } else { 0 }, l2 | if n2 { l1 } else { 0 })
    };
    Ok(match h.kind() {
        HirKind::Empty => (true, 0, 0),
        HirKind::Literal(lit) => {
            let mut acc = (true, 0, 0);
            for c in std::str::from_utf8(&lit.0).context("a fuzzy regex matches text, not bytes of invalid UTF-8")?.chars() {
                let p = new_pos(b, vec![(c as u32, c as u32)])?;
                acc = concat(b, acc, (false, p, p));
            }
            acc
        }
        HirKind::Class(c) => {
            let class = match c {
                // the parser allows a byte class only where it stays within ASCII
                Class::Bytes(bc) => bc.iter().filter(|r| r.start() < 0x80).map(|r| (r.start() as u32, r.end().min(0x7F) as u32)).collect(),
                Class::Unicode(uc) => uc.iter().map(|r| (r.start() as u32, r.end() as u32)).collect(),
            };
            let p = new_pos(b, class)?;
            (false, p, p)
        }
        HirKind::Look(_) => bail!("anchors and word boundaries are not supported with --fuzzy-regex"),
        HirKind::Capture(cap) => glushkov(&cap.sub, b)?,
        HirKind::Concat(items) => {
            let mut acc = (true, 0, 0);
            for it in items {
                let x = glushkov(it, b)?;
                acc = concat(b, acc, x);
            }
            acc
        }
        HirKind::Alternation(items) => {
            let mut acc = (false, 0, 0);
            for it in items {
                let (n, f, l) = glushkov(it, b)?;
                acc = (acc.0 || n, acc.1 | f, acc.2 | l);
            }
            acc
        }
        HirKind::Repetition(rep) => {
            let mut acc = (true, 0, 0);
            for _ in 0..rep.min {
                let x = glushkov(&rep.sub, b)?;
                acc = concat(b, acc, x);
            }
            match rep.max {
                None => {
                    // a starred copy: its last positions loop back to its first
                    let (n, f, l) = glushkov(&rep.sub, b)?;
                    for i in 0..b.follow.len() {
                        if l & (1 << i) != 0 {
                            b.follow[i] |= f;
                        }
                    }
                    let _ = n;
                    acc = concat(b, acc, (true, f, l));
                }
                Some(max) => {
                    for _ in rep.min..max {
                        let (_, f, l) = glushkov(&rep.sub, b)?;
                        acc = concat(b, acc, (true, f, l));
                    }
                }
            }
            acc
        }
    })
}

impl FuzzyRegex {
    pub fn new(pattern: &str, max_edits: u8, case_insensitive: bool) -> Result<FuzzyRegex> {
        let hir = regex_syntax::ParserBuilder::new()
            .case_insensitive(case_insensitive)
            .build()
            .parse(pattern)
            .with_context(|| format!("invalid pattern {pattern:?}"))?;
        let mut m = Self::of(&hir, max_edits)?;
        m.rev = Some(Box::new(Self::of(&reversed(&hir), max_edits)?));
        Ok(m)
    }

    fn of(hir: &Hir, max_edits: u8) -> Result<FuzzyRegex> {
        let mut b = Build { classes: Vec::new(), follow: Vec::new() };
        let (nullable, first, last) = glushkov(hir, &mut b)?;
        if nullable {
            bail!("a fuzzy regex must not match the empty string (every line would match)");
        }
        let mut ascii = [0u64; 128];
        let mut cuts = vec![0x80u32, 0x110000];
        for &(lo, hi) in b.classes.iter().flatten().filter(|r| r.1 >= 0x80) {
            cuts.extend([lo.max(0x80), hi + 1]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        cuts.pop();
        let mut wide = vec![0u64; cuts.len()];
        for (p, class) in b.classes.iter().enumerate() {
            for &(lo, hi) in class {
                for x in lo..=hi.min(0x7F) {
                    ascii[x as usize] |= 1 << p;
                }
                if hi >= 0x80 {
                    let (a, z) = (cuts.partition_point(|&s| s < lo.max(0x80)), cuts.partition_point(|&s| s <= hi));
                    wide[a..z].iter_mut().for_each(|w| *w |= 1 << p);
                }
            }
        }
        let mut follow = Box::new([[0u64; 256]; 8]);
        for (c, table) in follow.iter_mut().enumerate() {
            for (v, slot) in table.iter_mut().enumerate() {
                for bit in 0..8 {
                    let p = c * 8 + bit;
                    if v & (1 << bit) != 0 && p < b.follow.len() {
                        *slot |= b.follow[p];
                    }
                }
            }
        }
        let k = max_edits as usize;
        let mut m = FuzzyRegex { ascii, cuts, wide, follow, first, last, k, init: Vec::new(), rev: None };
        // deleting j pattern characters before any text is read
        let mut d = 0u64;
        for _ in 0..=k {
            m.init.push(d);
            d = m.step(d);
        }
        // as with --fuzzy: k edits of a string of k characters or fewer match every line
        if m.init[k] & m.last != 0 {
            bail!("the number of edits ({k}) must be smaller than the length of the shortest string the pattern matches");
        }
        Ok(m)
    }

    /// The positions reachable in one move from `d`, or from the start (always active: search).
    fn step(&self, d: u64) -> u64 {
        let mut out = self.first;
        for c in 0..8 {
            out |= self.follow[c][((d >> (8 * c)) & 0xff) as usize];
        }
        out
    }

    /// The positions reachable in one move from `d`, the start among them when `start` holds.
    fn step_from(&self, d: u64, start: bool) -> u64 {
        let mut out = if start { self.first } else { 0 };
        for c in 0..8 {
            out |= self.follow[c][((d >> (8 * c)) & 0xff) as usize];
        }
        out
    }

    /// The positions whose class holds the character at `hay[i]`, which is not a line end, and
    /// its length in bytes.
    fn unit(&self, hay: &[u8], i: usize) -> (u64, usize) {
        match hay[i] {
            c @ 0..0x80 => (self.ascii[c as usize], 1),
            _ => {
                let (c, n) = crate::instant::wide_char(hay, i);
                (c.map_or(0, |c| self.wide[self.cuts.partition_point(|&s| s <= c as u32) - 1]), n)
            }
        }
    }

    /// Of a reversed pattern: the first byte of the longest text of `hay[from..end]` that ends at
    /// `end` and is within `d` edits of a string the pattern (read forward) matches.
    fn start_before(&self, hay: &[u8], from: usize, end: usize, d: usize) -> Option<usize> {
        let mut starts = Vec::new();
        let mut i = from;
        while i < end {
            starts.push(i);
            i += self.unit(hay, i).1;
        }
        let mut r = self.init[..=d.min(self.k)].to_vec();
        let mut next = r.clone();
        let mut best = None;
        // anchored at `end`: the start stays reachable at `t` characters in only with `t` insertions
        for (t, &u) in starts.iter().rev().enumerate() {
            let bc = self.unit(hay, u).0;
            next[0] = self.step_from(r[0], t == 0) & bc;
            for j in 1..r.len() {
                next[j] = (self.step_from(r[j], t <= j) & bc) | r[j - 1] | self.step_from(r[j - 1], t < j) | self.step_from(next[j - 1], t + 1 < j);
            }
            std::mem::swap(&mut r, &mut next);
            if r.iter().any(|&s| s & self.last != 0) {
                best = Some(u);
            }
            if t >= r.len() && r.iter().all(|&s| s == 0) {
                break;
            }
        }
        best
    }

    /// End (exclusive) and number of errors of the first match in `hay[at..]`: which lines match.
    /// The spans a line shows come from `spans`.
    fn first_end(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        let k = self.k;
        let mut r = self.init.clone();
        let mut next = vec![0u64; k + 1];
        let mut i = at;
        while i < hay.len() {
            let c = hay[i];
            let bc = if c < 0x80 {
                if c == b'\n' {
                    r.copy_from_slice(&self.init);
                    i += 1;
                    continue;
                }
                i += 1;
                self.ascii[c as usize]
            } else {
                let (c, n) = crate::instant::wide_char(hay, i);
                i += n;
                c.map_or(0, |c| self.wide[self.cuts.partition_point(|&s| s <= c as u32) - 1])
            };
            next[0] = self.step(r[0]) & bc;
            for j in 1..=k {
                // match, insertion (text byte skipped), substitution, deletion (pattern position skipped)
                next[j] = (self.step(r[j]) & bc) | r[j - 1] | self.step(r[j - 1]) | self.step(next[j - 1]);
            }
            std::mem::swap(&mut r, &mut next);
            if let Some(j) = r.iter().position(|&s| s & self.last != 0) {
                return Some((i, j));
            }
        }
        None
    }

    /// The spans of `line`, one line without its line break, by docs/adr/0013-fuzzy-spans.md: the
    /// parts within `k` edits of a string the pattern matches, the fewest edits first, then the
    /// leftmost, then the longest, none overlapping; an end character is matched or is a word
    /// character substituted, never inserted.
    pub fn spans(&self, line: &[u8]) -> Vec<(usize, usize)> {
        let rev = self.rev.as_deref().expect("a pattern built by new has its reverse");
        let mut units = Vec::new();
        let mut i = 0;
        while i < line.len() {
            units.push(i);
            i += self.unit(line, i).1;
        }
        let to_end = self.to_end();
        crate::instant::fuzzy_passes(line.len(), self.k, |c, from, to, out| {
            let gap = &units[units.partition_point(|&u| u < from)..units.partition_point(|&u| u < to)];
            let mut at = from;
            for s in rev.starts(line, gap, c) {
                if s >= at {
                    if let Some(e) = self.longest_from(line, s, to, c, &to_end) {
                        out.push((s, e));
                        at = e;
                    }
                }
            }
        })
    }

    /// Of a reversed pattern, over the units `gap` of `line`: every unit where a part within `c`
    /// edits of the pattern (read forward) starts and ends inside the gap, left to right.
    fn starts(&self, line: &[u8], gap: &[usize], c: usize) -> Vec<usize> {
        let mut r = self.init.clone();
        let mut next = vec![0u64; self.k + 1];
        let mut out = Vec::new();
        for &u in gap.iter().rev() {
            let bc = self.unit(line, u).0;
            next[0] = self.step(r[0]) & bc;
            for j in 1..=self.k {
                next[j] = (self.step(r[j]) & bc) | r[j - 1] | self.step(r[j - 1]) | self.step(next[j - 1]);
            }
            std::mem::swap(&mut r, &mut next);
            if r[..=c].iter().any(|&x| x & self.last != 0) {
                out.push(u);
            }
        }
        out.reverse();
        out
    }

    /// The positions from which the pattern ends after deleting `d` more of its positions, for d in
    /// 0..=k.
    fn to_end(&self) -> Vec<u64> {
        let follow = |p: usize| self.follow[p / 8][1 << (p % 8)];
        let mut out = vec![self.last];
        let mut reached = self.last;
        for _ in 0..self.k {
            let prev = *out.last().unwrap();
            let here = (0..MAX_POSITIONS).filter(|&p| reached & (1 << p) == 0 && follow(p) & prev != 0).fold(0u64, |a, p| a | 1 << p);
            reached |= here;
            out.push(here);
        }
        out
    }

    /// The end of the longest part of `line` from `start` to `to` at most within `c` edits whose
    /// first and last characters are matched or are word characters substituted, if one is.
    fn longest_from(&self, line: &[u8], start: usize, to: usize, c: usize, to_end: &[u64]) -> Option<usize> {
        let costs = |m: &[u64]| (0..=c).any(|j| (0..=c - j).any(|d| m[j] & to_end[d] != 0));
        let (mut r, mut next, mut ended) = (vec![0u64; c + 1], vec![0u64; c + 1], vec![0u64; c + 1]);
        let mut best = None;
        let mut i = start;
        let mut first = true;
        while i < to {
            let (bc, n) = self.unit(line, i);
            let word = crate::instant::word_at(line, i);
            for j in 0..=c {
                // the positions this character ends on, matched or (a word character) substituted
                let (on, sub) = if first {
                    (self.step_from(self.init[j], true), if j > 0 { self.step_from(self.init[j - 1], true) } else { 0 })
                } else {
                    (self.step_from(r[j], false), if j > 0 { self.step_from(r[j - 1], false) } else { 0 })
                };
                ended[j] = (on & bc) | if word { sub } else { 0 };
                next[j] = if first {
                    ended[j]
                } else {
                    // inside, an insertion and any substitution as well
                    (on & bc) | sub | if j > 0 { r[j - 1] } else { 0 }
                };
                if j > 0 {
                    next[j] |= self.step_from(next[j - 1], false);
                }
            }
            std::mem::swap(&mut r, &mut next);
            first = false;
            i += n;
            if costs(&ended) {
                best = Some(i);
            }
            if r.iter().all(|&x| x == 0) {
                break;
            }
        }
        best
    }
}

impl Matcher for FuzzyRegex {
    type Captures = NoCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<Match>, NoError> {
        let Some((end, errors)) = self.first_end(haystack, at) else { return Ok(None) };
        let line_start = haystack[at..end].iter().rposition(|&b| b == b'\n').map(|i| at + i + 1).unwrap_or(at);
        let start = self.rev.as_ref().and_then(|r| r.start_before(haystack, line_start, end, errors)).unwrap_or(line_start);
        Ok(Some(Match::new(start, end)))
    }

    fn new_captures(&self) -> Result<NoCaptures, NoError> {
        Ok(NoCaptures::new())
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        Some(LineTerminator::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(m: &impl Matcher, text: &str) -> Vec<usize> {
        text.lines().enumerate().filter(|(_, l)| m.find_at(l.as_bytes(), 0).ok().flatten().is_some()).map(|(i, _)| i).collect()
    }

    #[test]
    fn errors_and_syntax() {
        let m = FuzzyRegex::new("colou?r", 1, false).unwrap();
        assert_eq!(lines(&m, "color\ncolour\ncolr\ncoluor\nkolor\nxyz"), vec![0, 1, 2, 3, 4]);
        let m = FuzzyRegex::new("(get|set)_value", 1, false).unwrap();
        assert_eq!(lines(&m, "get_value\nset_valu\nput_value\nnet_vlue"), vec![0, 1]);
        let m = FuzzyRegex::new("(get|set)_value", 2, false).unwrap();
        assert_eq!(lines(&m, "get_value\nset_valu\nput_value\nnet_vlue\nfetch_all"), vec![0, 1, 2, 3]);
        let m = FuzzyRegex::new("Get_[a-z]+", 0, true).unwrap();
        assert_eq!(lines(&m, "GET_X\nget_name\nget-name"), vec![0, 1]);
        assert!(FuzzyRegex::new("^abc", 1, false).is_err());
        assert!(FuzzyRegex::new("a*", 1, false).is_err());
        assert!(FuzzyRegex::new(&"a".repeat(65), 1, false).is_err());
        assert!(FuzzyRegex::new("é", 0, false).is_ok());
        assert!(FuzzyRegex::new("é?b", 1, false).is_err());
        assert!(FuzzyRegex::new("(éé|c)d", 1, false).is_ok());
    }

    #[test]
    fn agrees_with_myers_on_literals_and_with_the_regex_engine_exactly() {
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let mut text = String::new();
        for _ in 0..300 {
            let len = next(30);
            text.extend((0..len).map(|_| b"abcd_"[next(5) as usize] as char));
            text.push('\n');
        }
        for _ in 0..200 {
            let m = 2 + next(10) as usize;
            let pat: String = (0..m).map(|_| b"abcd"[next(4) as usize] as char).collect();
            let k = (next(3) as usize).min(m - 1);
            let fr = FuzzyRegex::new(&pat, k as u8, false).unwrap();
            let my = crate::instant::FuzzyMatcher::new(&pat, k as u8, false).unwrap();
            assert_eq!(lines(&fr, &text), lines(&my, &text), "{pat} k={k}");
        }
        for pat in ["ab(c|d)+a", "a[bc]{2,3}d", "(ab|cd)_?(ab)*c", "a.c", "[a-c]+d_b"] {
            let fr = FuzzyRegex::new(pat, 0, false).unwrap();
            let re = regex::Regex::new(pat).unwrap();
            let want: Vec<usize> = text.lines().enumerate().filter(|(_, l)| re.is_match(l)).map(|(i, _)| i).collect();
            assert_eq!(lines(&fr, &text), want, "{pat}");
        }
    }

    #[test]
    fn a_match_starts_where_the_matched_text_starts() {
        let at = |pat: &str, k: u8, hay: &str| {
            let m = FuzzyRegex::new(pat, k, false).unwrap().find_at(hay.as_bytes(), 0).unwrap().unwrap();
            (m.start(), m.end())
        };
        assert_eq!(at("a[0-9]*b", 0, "zzz a12b"), (4, 8));
        assert_eq!(at("a[0-9]*b", 0, "aa12b"), (1, 5));
        // the longest text within the match's edits, as --fuzzy highlights it
        assert_eq!(at("abcd", 1, "zzzz abxd"), (5, 9));
        assert_eq!(at("é[0-9]+", 0, "xé12"), (1, 4));
        // a match on a later line starts on that line
        assert_eq!(at("a[0-9]*b", 0, "a1\nzz a2b"), (6, 9));
    }

    #[test]
    fn spans_take_the_fewest_edits_then_the_leftmost_then_the_longest() {
        let spans = |pat: &str, k: u8, line: &str| FuzzyRegex::new(pat, k, false).unwrap().spans(line.as_bytes());
        assert_eq!(spans("a[0-9]*b", 1, "zzz a12b"), [(4, 8)]);
        assert_eq!(spans("abc[0-9]*d", 2, "zz abc12d"), [(3, 9)]);
        assert_eq!(spans("a[0-9]*b", 1, "a1"), [(0, 2)]);
        assert_eq!(spans("aa", 0, "aaaa"), [(0, 2), (2, 4)]);
        assert_eq!(spans("aab", 1, "aaaaaaaa x"), [(0, 3), (3, 6), (6, 8)]);
        // with no edits, the leftmost-longest match: a quantifier's run whole
        assert_eq!(spans("é[0-9]+", 0, "xé12"), [(1, 5)]);
        assert_eq!(spans("[0-9]+", 0, "x1234y"), [(1, 5)]);
        assert_eq!(spans("((\\w{2})){1,2}a", 0, "b12aa1日a1日"), [(0, 5), (5, 10)]);
        // the longest from the leftmost start, where ripgrep's leftmost-first takes the first branch
        assert_eq!(spans("a|ab", 0, "ab"), [(0, 2)]);
    }

    #[test]
    fn match_starts_agree_with_the_regex_engine_and_with_edit_distance() {
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let word = |len: u64, next: &mut dyn FnMut(u64) -> u64| (0..len).map(|_| b"abc_"[next(4) as usize] as char).collect::<String>();
        let lev = |a: &[u8], b: &[u8]| {
            let mut row: Vec<usize> = (0..=b.len()).collect();
            for (i, &x) in a.iter().enumerate() {
                let mut prev = row[0];
                row[0] = i + 1;
                for (j, &y) in b.iter().enumerate() {
                    let cur = row[j + 1];
                    row[j + 1] = (prev + usize::from(x != y)).min(row[j] + 1).min(cur + 1);
                    prev = cur;
                }
            }
            row[b.len()]
        };
        for _ in 0..400 {
            let hay = word(1 + next(14), &mut next);
            // exact: the longest match that ends where the first match ends, by the regex engine
            for pat in ["a[bc]*_", "(ab|c)+_?b", "b_*a"] {
                let Some(m) = FuzzyRegex::new(pat, 0, false).unwrap().find_at(hay.as_bytes(), 0).unwrap() else { continue };
                let re = regex::Regex::new(&format!("(?:{pat})$")).unwrap();
                assert_eq!(re.find(&hay[..m.end()]).map(|r| r.start()), Some(m.start()), "{pat} on {hay}");
            }
            // with edits: the farthest start within the match's edits of a literal
            let lit = word(3 + next(3), &mut next);
            for k in 1..=2u8 {
                let fr = FuzzyRegex::new(&lit, k, false).unwrap();
                let Some(m) = fr.find_at(hay.as_bytes(), 0).unwrap() else { continue };
                let d = (0..=m.end()).map(|s| lev(&hay.as_bytes()[s..m.end()], lit.as_bytes())).min().unwrap();
                let want = (0..=m.end()).find(|&s| lev(&hay.as_bytes()[s..m.end()], lit.as_bytes()) <= d).unwrap();
                assert_eq!(m.start(), want, "{lit} k={k} on {hay}");
            }
        }
    }
}
