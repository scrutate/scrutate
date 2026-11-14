//! Candidate lines for a case-insensitive literal: two bytes of the needle, each in either case
//! form, at a fixed distance, then a check of the stretch of the needle around them.
//!
//! The needle is the regex engine's own view of the pattern (parsed with case folding), so each
//! position holds exactly the characters the regex accepts there. Offsets between positions are
//! fixed only across a run of positions whose every form has the same byte length: `k` matches
//! KELVIN SIGN (3 bytes) and `ß` matches U+1E9E (3 bytes), so a run ends before such a position.
//! The two scanned bytes lie in one run, each at an offset where the forms hold one byte value or
//! two that differ only in bit 0x20 (`a`/`A`, the last byte of `ö`/`Ö`); the candidate offset
//! reported is the run's start, which lies inside any match. A candidate is checked against the
//! first `CHECKED` bytes of the run only, so the scan stays linear on repetitive text; the
//! searcher confirms the whole line anyway.

use regex_syntax::hir::{Class, Hir, HirKind};

/// One position of the needle: the byte strings that match it, or `None` when they differ in length.
type Forms = Option<Vec<Vec<u8>>>;

#[derive(Clone, Debug)]
pub struct FoldFinder {
    /// the forms of each position of the run's first `CHECKED` bytes, all of one length per position
    run: Vec<Vec<Vec<u8>>>,
    len: usize,
    a: usize,
    b: usize,
    /// a byte `x` at a scanned offset fits when `x | mask == target`
    mask_a: u8,
    target_a: u8,
    mask_b: u8,
    target_b: u8,
}

/// How much of the run a candidate is checked against.
const CHECKED: usize = 16;

/// Bytes common in source code, the most common first; all others count as rare.
const COMMON: &[u8] = b" etianosrlcdu_()hp,mf;=g*b.yvwx/-0>1\"{}[]2#q&j!z<3:+%";

fn commonness(lower: u8) -> usize {
    COMMON.iter().position(|&c| c == lower).map_or(0, |p| COMMON.len() - p)
}

fn positions(hir: &Hir, out: &mut Vec<Forms>) -> Option<()> {
    match hir.kind() {
        HirKind::Literal(lit) => out.extend(lit.0.iter().map(|&b| Some(vec![vec![b]]))),
        HirKind::Class(Class::Unicode(c)) => {
            let chars: Vec<char> = c.iter().flat_map(|r| r.start()..=r.end()).take(5).collect();
            if chars.is_empty() || chars.len() > 4 {
                out.push(None);
            } else {
                let forms: Vec<Vec<u8>> = chars.iter().map(|ch| ch.to_string().into_bytes()).collect();
                out.push(forms.iter().all(|f| f.len() == forms[0].len()).then_some(forms));
            }
        }
        HirKind::Class(Class::Bytes(c)) => {
            let bytes: Vec<u8> = c.iter().flat_map(|r| r.start()..=r.end()).take(5).collect();
            out.push((1..=4).contains(&bytes.len()).then(|| bytes.iter().map(|&b| vec![b]).collect()));
        }
        HirKind::Concat(xs) => xs.iter().try_for_each(|x| positions(x, out))?,
        _ => return None,
    }
    Some(())
}

/// The scan test for byte `k` of a position's forms: a byte `x` fits when `x | mask == target`.
fn scan_test(forms: &[Vec<u8>], k: usize) -> Option<(u8, u8)> {
    let mut bytes: Vec<u8> = forms.iter().map(|f| f[k]).collect();
    bytes.sort_unstable();
    bytes.dedup();
    match bytes[..] {
        [x] => Some((0, x)),
        [x, y] if x ^ y == 0x20 => Some((0x20, y)),
        _ => None,
    }
}

impl FoldFinder {
    /// The finder for `pattern` searched case-insensitively, when it is a literal with two
    /// usable positions in one run.
    pub fn new(pattern: &str) -> Option<FoldFinder> {
        let hir = regex_syntax::ParserBuilder::new().case_insensitive(true).build().parse(pattern).ok()?;
        let mut pos = Vec::new();
        positions(&hir, &mut pos)?;
        if pos.iter().flatten().flatten().any(|f| f.contains(&b'\n')) {
            return None;
        }
        let mut best: Option<(usize, FoldFinder)> = None;
        for run in pos.split(Option::is_none) {
            let mut run: Vec<Vec<Vec<u8>>> = run.iter().flatten().cloned().collect();
            let mut at = 0;
            let mut cands = Vec::new();
            for forms in &run {
                for k in 0..forms[0].len() {
                    if let Some((mask, target)) = scan_test(forms, k) {
                        cands.push((commonness(target), at + k, mask, target));
                    }
                }
                at += forms[0].len();
            }
            if cands.len() < 2 {
                continue;
            }
            // the rarest byte, then the rarest other one, farther away on a tie
            cands.sort_by_key(|c| c.0);
            let first = cands[0];
            let second = *cands[1..].iter().min_by_key(|c| (c.0, usize::MAX - c.1.abs_diff(first.1))).unwrap();
            let score = first.0 + second.0;
            if best.as_ref().is_some_and(|(s, _)| *s <= score) {
                continue;
            }
            let (x, y) = if first.1 < second.1 { (first, second) } else { (second, first) };
            let mut off = 0;
            let checked = run
                .iter()
                .take_while(|f| {
                    off += f[0].len();
                    off - f[0].len() < CHECKED
                })
                .count();
            run.truncate(checked);
            let f = FoldFinder { run, len: at, a: x.1, b: y.1, mask_a: x.2, target_a: x.3, mask_b: y.2, target_b: y.3 };
            best = Some((score, f));
        }
        best.map(|(_, f)| f)
    }

    fn fits(&self, hay: &[u8], i: usize) -> bool {
        (hay[i + self.a] | self.mask_a) == self.target_a && (hay[i + self.b] | self.mask_b) == self.target_b
    }

    /// Whether the checked part of the run matches at `i`; `i + len <= hay.len()`.
    fn run_at(&self, hay: &[u8], i: usize) -> bool {
        let mut at = i;
        for forms in &self.run {
            if !forms.iter().any(|f| hay[at..].starts_with(f)) {
                return false;
            }
            at += forms[0].len();
        }
        true
    }

    /// The first offset where the run matches: no match of the pattern starts its run earlier.
    pub fn find(&self, hay: &[u8]) -> Option<usize> {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU has AVX2
            return unsafe { self.find_avx2(hay) };
        }
        self.scan(hay)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn find_avx2(&self, hay: &[u8]) -> Option<usize> {
        self.scan(hay)
    }

    /// Plain slice code that the compiler turns into vector compares (NEON, SSE2, AVX2).
    #[inline(always)]
    fn scan(&self, hay: &[u8]) -> Option<usize> {
        const W: usize = 64;
        let ends = hay.len().checked_sub(self.len)? + 1;
        let mut i = 0;
        while i + W <= ends {
            // in bounds: i + b + W <= ends - 1 + len = hay.len(), since b < len
            let x: &[u8; W] = hay[i + self.a..i + self.a + W].try_into().unwrap();
            let y: &[u8; W] = hay[i + self.b..i + self.b + W].try_into().unwrap();
            let mut any = 0u8;
            for k in 0..W {
                any |= (((x[k] | self.mask_a) == self.target_a) & ((y[k] | self.mask_b) == self.target_b)) as u8;
            }
            if any != 0 {
                let mut hits = 0u64;
                for k in 0..W {
                    hits |= ((((x[k] | self.mask_a) == self.target_a) & ((y[k] | self.mask_b) == self.target_b)) as u64) << k;
                }
                while hits != 0 {
                    let j = i + hits.trailing_zeros() as usize;
                    if self.run_at(hay, j) {
                        return Some(j);
                    }
                    hits &= hits - 1;
                }
            }
            i += W;
        }
        (i..ends).find(|&j| self.fits(hay, j) && self.run_at(hay, j))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_positions_inside_one_run() {
        // K and the KELVIN SIGN differ in length, so the run is "ÖNIG"
        let f = FoldFinder::new("KÖNIG").unwrap();
        assert_eq!(f.len, 5);
        assert_eq!(f.find("der könig".as_bytes()), Some(5));
        assert_eq!(f.find("der K\u{212A}ÖNIG".as_bytes()), Some(8));
        assert_eq!(f.find(b"koenig"), None);
        assert!(FoldFinder::new("ks").is_none());
        assert!(FoldFinder::new("a+b").is_none());
        assert!(FoldFinder::new("ab").is_some());
    }

    #[test]
    fn a_class_that_matches_nothing_is_no_position_to_scan_for() {
        for empty in [r"ab[^\s\S]", r"(?-u)ab[^\x00-\xff]"] {
            if let Some(f) = FoldFinder::new(empty) {
                assert_eq!(f.find(b"xxab"), Some(2), "{empty}");
            }
        }
    }
}
