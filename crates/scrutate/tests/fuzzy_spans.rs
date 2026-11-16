//! The spans of `grep --fuzzy` and `grep --fuzzy-regex` against a brute-force reference written
//! from their definition alone. An occurrence is a part of the line within N edits of the pattern
//! (for a regex: of some text the regex matches whole); an edit is one character inserted, deleted
//! or substituted. At either end of an occurrence the character is matched as it stands, or is a
//! word character substituted for a pattern character: a space or a punctuation mark is never a
//! substitute at an end, and an inserted character is never at an end. Inside, any edit.
//! Spans are taken fewest edits first, then leftmost start, then longest; a span taken removes
//! every occurrence that overlaps it, and the spans are reported left to right as byte offsets.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/tag.rs"]
mod tag;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");
// a combining acute accent is a word character for `\w` and '²' is not, though Rust calls it alphanumeric
const ALPHABET: [char; 10] = ['a', 'b', '1', '2', 'é', '日', ' ', '(', '\u{301}', '²'];
const LINES_PER_PATTERN: usize = 200;

type Spans = Vec<(usize, usize)>;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn char(&mut self) -> char {
        ALPHABET[self.below(ALPHABET.len())]
    }
    fn text(&mut self, from: usize, to: usize) -> String {
        let n = from + self.below(to - from + 1);
        (0..n).map(|_| self.char()).collect()
    }
}

/// A regex as a tree, which prints itself and matches whole strings on its own.
#[derive(Clone, Debug)]
enum Re {
    Char(char),
    Set(Vec<char>),
    Word,
    Cat(Vec<Re>),
    Group(Box<Re>),
    /// at least m copies, at most n (None: no bound)
    Rep(Box<Re>, usize, Option<usize>),
    Alt(Vec<Re>),
}

impl Re {
    fn print(&self) -> String {
        match self {
            Re::Char(c) if "()[]{}|*+?.\\^$".contains(*c) => format!("\\{c}"),
            Re::Char(c) => c.to_string(),
            Re::Set(v) => format!("[{}]", v.iter().collect::<String>()),
            Re::Word => r"\w".to_string(),
            Re::Cat(v) => v.iter().map(|r| if matches!(r, Re::Alt(_)) { format!("({})", r.print()) } else { r.print() }).collect(),
            Re::Group(r) => format!("({})", r.print()),
            Re::Rep(r, m, n) => {
                let atom = match **r {
                    Re::Char(_) | Re::Set(_) | Re::Word | Re::Group(_) => r.print(),
                    _ => format!("({})", r.print()),
                };
                let q = match (*m, *n) {
                    (0, None) => "*".to_string(),
                    (1, None) => "+".to_string(),
                    (0, Some(1)) => "?".to_string(),
                    (m, None) => format!("{{{m},}}"),
                    (m, Some(n)) if m == n => format!("{{{m}}}"),
                    (m, Some(n)) => format!("{{{m},{n}}}"),
                };
                atom + &q
            }
            Re::Alt(v) => v.iter().map(Re::print).collect::<Vec<_>>().join("|"),
        }
    }

    /// Every position where a match of `self` that starts at `i` of `s` can end.
    fn ends(&self, s: &[char], i: usize) -> BTreeSet<usize> {
        let one = |ok: bool| if ok { BTreeSet::from([i + 1]) } else { BTreeSet::new() };
        match self {
            Re::Char(c) => one(s.get(i) == Some(c)),
            Re::Set(v) => one(s.get(i).is_some_and(|c| v.contains(c))),
            Re::Word => one(s.get(i).is_some_and(|c| word(*c))),
            Re::Cat(v) => v.iter().fold(BTreeSet::from([i]), |at, r| at.iter().flat_map(|&p| r.ends(s, p)).collect()),
            Re::Group(r) => r.ends(s, i),
            Re::Alt(v) => v.iter().flat_map(|r| r.ends(s, i)).collect(),
            Re::Rep(r, m, n) => {
                // past m + len copies every further copy matches nothing new: it can only be empty
                let most = n.unwrap_or(m + s.len() + 1);
                let mut at = BTreeSet::from([i]);
                let mut out = if *m == 0 { at.clone() } else { BTreeSet::new() };
                for k in 1..=most {
                    at = at.iter().flat_map(|&p| r.ends(s, p)).collect();
                    if at.is_empty() {
                        break;
                    }
                    if k >= *m {
                        out.extend(&at);
                    }
                }
                out
            }
        }
    }

    fn matches(&self, s: &[char]) -> bool {
        self.ends(s, 0).contains(&s.len())
    }

    /// The shortest and longest string the tree matches (None: no bound).
    fn lengths(&self) -> (usize, Option<usize>) {
        match self {
            Re::Char(_) | Re::Set(_) | Re::Word => (1, Some(1)),
            Re::Group(r) => r.lengths(),
            Re::Cat(v) => v.iter().map(Re::lengths).fold((0, Some(0)), |(a, b), (c, d)| (a + c, b.zip(d).map(|(b, d)| b + d))),
            Re::Alt(v) => v.iter().map(Re::lengths).fold((usize::MAX, Some(0)), |(a, b), (c, d)| (a.min(c), b.zip(d).map(|(b, d)| b.max(d)))),
            Re::Rep(r, m, n) => {
                let (lo, hi) = r.lengths();
                (lo * m, if hi == Some(0) { Some(0) } else { hi.zip(*n).map(|(h, n)| h * n) })
            }
        }
    }

    /// The characters the tree names, beside the alphabet: what an edit may need to write.
    fn chars(&self, out: &mut BTreeSet<char>) {
        match self {
            Re::Char(c) => {
                out.insert(*c);
            }
            Re::Set(v) => out.extend(v),
            Re::Word => {}
            Re::Cat(v) | Re::Alt(v) => v.iter().for_each(|r| r.chars(out)),
            Re::Group(r) | Re::Rep(r, ..) => r.chars(out),
        }
    }
}

/// A word character as `\w` takes it.
fn word(c: char) -> bool {
    static W: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    W.get_or_init(|| regex::Regex::new(r"^\w$").unwrap()).is_match(c.encode_utf8(&mut [0; 4]))
}

/// The fewest edits, at most `n` (0 or 1), from a string `re` matches whole to `u`, with u's ends
/// as the definition allows: u itself; u with one character inserted anywhere, its ends included
/// (the pattern had one u lacks); u with one interior character deleted; u with one character
/// substituted, an end one only when it is a word character. Never u with an end deleted.
fn regex_cost(re: &Re, u: &[char], n: usize, chars: &[char]) -> Option<usize> {
    assert!(n <= 1, "the regex reference tries edit scripts of one edit at most");
    if u.is_empty() {
        return None;
    }
    if re.matches(u) {
        return Some(0);
    }
    if n == 0 {
        return None;
    }
    let last = u.len() - 1;
    let inserted = (0..=u.len()).any(|i| chars.iter().any(|&c| re.matches(&[&u[..i], &[c], &u[i..]].concat())));
    let deleted = (1..last).any(|i| re.matches(&[&u[..i], &u[i + 1..]].concat()));
    let substituted = (0..u.len())
        .filter(|&i| (i != 0 && i != last) || word(u[i]))
        .any(|i| chars.iter().any(|&c| c != u[i] && re.matches(&[&u[..i], &[c], &u[i + 1..]].concat())));
    (inserted || deleted || substituted).then_some(1)
}

/// A pattern character against an end character of the line: 0 when equal, 1 when the line's is
/// a word character substituted for it, else not allowed.
fn end(p: char, u: char) -> Option<usize> {
    if p == u {
        Some(0)
    } else {
        word(u).then_some(1)
    }
}

/// The cost of `u` against the literal `p`: over pattern positions i <= j (i = j when u is one
/// character), the least of i leading pattern characters deleted, p[i] against u's first, the
/// Levenshtein distance between what lies between, p[j] against u's last, and the trailing
/// pattern characters deleted.
fn literal_cost(p: &[char], u: &[char]) -> Option<usize> {
    let (first, last) = (*u.first()?, *u.last()?);
    let mut best: Option<usize> = None;
    for i in 0..p.len() {
        for j in i..p.len() {
            if (u.len() == 1) != (i == j) {
                continue;
            }
            let Some(a) = end(p[i], first) else { continue };
            let c = if i == j {
                i + a + (p.len() - 1 - j)
            } else {
                let Some(b) = end(p[j], last) else { continue };
                i + a + levenshtein(&p[i + 1..j], &u[1..u.len() - 1]) + b + (p.len() - 1 - j)
            };
            best = Some(best.map_or(c, |x| x.min(c)));
        }
    }
    best
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(here + 1);
            prev = here;
        }
    }
    row[b.len()]
}

enum Pattern {
    Literal(String),
    Regex(Re),
}

impl Pattern {
    fn arg(&self) -> String {
        match self {
            Pattern::Literal(s) => s.clone(),
            Pattern::Regex(r) => r.print(),
        }
    }
    fn flag(&self, n: usize) -> String {
        match self {
            Pattern::Literal(_) => format!("--fuzzy={n}"),
            Pattern::Regex(_) => format!("--fuzzy-regex={n}"),
        }
    }

    /// A cost function for the parts of lines: the fewest edits within `n`, else None.
    fn coster(&self, n: usize) -> impl FnMut(&[char]) -> Option<usize> + '_ {
        let mut memo: HashMap<Vec<char>, Option<usize>> = HashMap::new();
        let mut alphabet: BTreeSet<char> = ALPHABET.into_iter().collect();
        if let Pattern::Regex(r) = self {
            r.chars(&mut alphabet);
        }
        let alphabet: Vec<char> = alphabet.into_iter().collect();
        move |s: &[char]| {
            if let Some(c) = memo.get(s) {
                return *c;
            }
            let c = match self {
                Pattern::Literal(p) => literal_cost(&p.chars().collect::<Vec<_>>(), s).filter(|&c| c <= n),
                Pattern::Regex(r) => {
                    let (lo, hi) = r.lengths();
                    if s.len() + n < lo || hi.is_some_and(|hi| s.len() > hi + n) {
                        None
                    } else {
                        regex_cost(r, s, n, &alphabet)
                    }
                }
            };
            memo.insert(s.to_vec(), c);
            c
        }
    }
}

/// The reference's spans of one line, as byte offsets.
fn reference(line: &str, cost: &mut dyn FnMut(&[char]) -> Option<usize>) -> Spans {
    let chars: Vec<char> = line.chars().collect();
    let at: Vec<usize> = line.char_indices().map(|(b, _)| b).chain([line.len()]).collect();
    let mut found: Vec<(usize, usize, usize)> = Vec::new();
    for a in 0..=chars.len() {
        for b in a..=chars.len() {
            if let Some(c) = cost(&chars[a..b]) {
                found.push((c, a, b));
            }
        }
    }
    found.sort_by_key(|&(c, a, b)| (c, a, std::cmp::Reverse(b)));
    let mut taken: Vec<(usize, usize)> = Vec::new();
    for (_, a, b) in found {
        if taken.iter().all(|&(x, y)| b <= x || y <= a) {
            taken.push((a, b));
        }
    }
    taken.sort();
    taken.into_iter().map(|(a, b)| (at[a], at[b])).collect()
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let root = std::env::temp_dir().join(format!("scrutate_fuzzy_spans_{name}_{}", std::process::id()));
        tag::claim(&root);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        Dir(root)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The binary's spans of each line it prints (1-based line number), or its message when it refuses
/// the pattern (exit 2).
fn binary(dir: &Path, flag: &str, pattern: &str, lines: &[String]) -> Result<HashMap<usize, Spans>, String> {
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(dir.join("lines.txt"), body).unwrap();
    let mut c = Command::new(BIN);
    c.args(["grep", "--json", flag, "--", pattern, "lines.txt"]).current_dir(dir).stdin(Stdio::null());
    for v in ["SCRUTATE_GREP_SMART_CASE", "SCRUTATE_GREP_HIDDEN", "SCRUTATE_GREP_NO_IGNORE"] {
        c.env_remove(v);
    }
    let out = c.output().expect("run scrutate");
    let code = out.status.code().unwrap_or(-1);
    if code == 2 {
        return Err(String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").to_string());
    }
    assert!(code == 0 || code == 1, "{flag} {pattern:?}: exit {code}: {}", String::from_utf8_lossy(&out.stderr));
    let mut got = HashMap::new();
    for l in String::from_utf8_lossy(&out.stdout).lines() {
        let v: serde_json::Value = serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"));
        if v["type"] != "match" {
            continue;
        }
        let spans = v["spans"].as_array().unwrap().iter().map(|s| (s["start"].as_u64().unwrap() as usize, s["end"].as_u64().unwrap() as usize)).collect();
        got.insert(v["line"].as_u64().unwrap() as usize, spans);
    }
    Ok(got)
}

/// What disagrees between the reference and the binary for one pattern, one entry per line.
fn compare(dir: &Path, p: &Pattern, n: usize, lines: &[String]) -> Result<Vec<String>, String> {
    let got = binary(dir, &p.flag(n), &p.arg(), lines)?;
    let mut cost = p.coster(n);
    let mut wrong = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let want = reference(line, &mut cost);
        let have = got.get(&(i + 1)).cloned().unwrap_or_default();
        let say = |what: &str| format!("{} {:?} on {line:?}: {what}; reference {want:?}, binary {have:?}", p.flag(n), p.arg());
        if want.is_empty() != !got.contains_key(&(i + 1)) {
            wrong.push(say(if want.is_empty() { "printed a line with no occurrence" } else { "left out a line with an occurrence" }));
        } else if want != have {
            wrong.push(say("other spans"));
        }
        for &(a, b) in &have {
            let text = line.get(a..b).map(|t| t.chars().collect::<Vec<_>>());
            if text.and_then(|t| cost(&t)).is_none() {
                wrong.push(say(&format!("the span ({a}, {b}) is not within {n} edits")));
            }
        }
    }
    Ok(wrong)
}

fn gen_re(rng: &mut Rng, depth: usize) -> Re {
    let set = |rng: &mut Rng| {
        let mut v: Vec<char> = (0..2 + rng.below(3)).map(|_| rng.char()).collect();
        v.sort();
        v.dedup();
        Re::Set(v)
    };
    if depth == 0 {
        return if rng.below(3) == 0 { set(rng) } else { Re::Char(rng.char()) };
    }
    match rng.below(7) {
        0 => Re::Char(rng.char()),
        1 => set(rng),
        2 => Re::Cat((0..2 + rng.below(2)).map(|_| gen_re(rng, depth - 1)).collect()),
        3 => Re::Group(Box::new(gen_re(rng, depth - 1))),
        4 => Re::Alt((0..2).map(|_| gen_re(rng, depth - 1)).collect()),
        _ => {
            let (m, n) = [(0, None), (1, None), (0, Some(1)), (1, Some(2)), (2, None), (2, Some(2)), (0, Some(2))][rng.below(7)];
            let inner = gen_re(rng, depth - 1);
            // a repeat of a repeat is written with a group between
            let inner = if matches!(inner, Re::Rep(..)) { Re::Group(Box::new(inner)) } else { inner };
            Re::Rep(Box::new(inner), m, n)
        }
    }
}

/// Fixed cases, each with its answer by hand: the reference must give it, and so must the binary.
#[test]
fn fuzzy_spans_of_the_fixed_cases() {
    let dir = Dir::new("fixed");
    let word2 = Re::Rep(Box::new(Re::Word), 2, Some(2));
    let lit = |p: &str| Pattern::Literal(p.into());
    let cases: Vec<(Pattern, usize, &str, Spans)> = vec![
        // each by hand; "sub" is a word character of the line substituted for a pattern one at an end
        // h..o against h..p: o to p at the end, "ell" equal inside: 1
        // a combining acute accent is a word character, so it may stand for c at the end: "ab" and
        // the mark (two bytes), a and the mark at the ends, "b" equal inside: 1, longer than "ab"
        (lit("abc"), 1, "ab\u{301}", vec![(0, 4)]),
        // '²' is no word character for `\w` (Rust's is_alphanumeric takes it), so no occurrence
        // ends on it; "ab" is a..b with c deleted: 1
        (lit("abc"), 1, "ab²", vec![(0, 2)]),
        (lit("hello"), 1, "hellp", vec![(0, 5)]),
        // h to j at the start, the rest equal: 1
        (lit("hello"), 1, "jello", vec![(0, 5)]),
        // "ell" against "all" inside, one substitution: 1
        (lit("hello"), 1, "hallo", vec![(0, 5)]),
        // c to d at the end: 1
        (lit("kmalloc"), 1, "kmallod", vec![(0, 7)]),
        // "(" is no word character, so no occurrence ends on it; "kmallo" ends on o with c deleted: 1
        (lit("kmalloc"), 1, "kmallo(", vec![(0, 6)]),
        // "ab" is a..b with c deleted: 1; "ab x" ends on x for c, but "b" against "b " inside is a
        // second edit; "x" alone is a substitution and two deletions
        (lit("abc"), 1, "ab x", vec![(0, 2)]),
        // a and c at the ends, "b" against "b " inside, one insertion: 1, longer than "ab"
        (lit("abc"), 1, "ab c", vec![(0, 4)]),
        // x for a at the start, "bc" equal: 1, leftmost before "bc" (1, 3)
        (lit("abc"), 1, "xbc", vec![(0, 3)]),
        // a space never stands for a at the start; "bc" is b..c with a deleted: 1
        (lit("abc"), 1, " bc", vec![(1, 3)]),
        // "ab" (1, 3) exact; then at cost 1 "aab" and "aa" (a, then a for b) overlap it, and "a"
        // (0, 1) with b deleted only touches it
        (lit("ab"), 1, "aab", vec![(0, 1), (1, 3)]),
        // the three exact "ab"; the space between is no character of the pattern
        (lit("ab"), 1, "abab ab", vec![(0, 2), (2, 4), (5, 7)]),
        // b2日 within 2 on "a123 b": nothing within 1 (no b before a 2, no 日). Within 2: "123"
        // (1 for b, "2" equal, 3 for 日), "2" and "b" (two ends deleted each), and others. From
        // start 0 nothing is within 2 ("a1", "a12", "a123" are 3), so "123" (1, 4) is first and
        // overlaps every other part of "123"; "b" (5, 6) is left
        (lit("b2日"), 2, "a123 b", vec![(1, 4), (5, 6)]),
        (lit("aa"), 0, "aaaa", vec![(0, 2), (2, 4)]),
        (Pattern::Regex(Re::Cat(vec![Re::Char('é'), Re::Rep(Box::new(Re::Set(('0'..='9').collect())), 1, None)])), 0, "xé12", vec![(1, 5)]),
        (
            Pattern::Regex(Re::Cat(vec![Re::Rep(Box::new(Re::Group(Box::new(Re::Group(Box::new(word2))))), 1, Some(2)), Re::Char('a')])),
            0,
            "b12aa1日a1日",
            vec![(0, 5), (5, 10)],
        ),
        (Pattern::Regex(Re::Alt(vec![Re::Char('a'), Re::Cat(vec![Re::Char('a'), Re::Char('b')])])), 0, "ab", vec![(0, 2)]),
    ];
    let regexes = ["é[0123456789]+", r"((\w{2})){1,2}a", "a|ab"];
    let printed: Vec<String> = cases.iter().map(|c| c.0.arg()).collect();
    assert!(printed.ends_with(&regexes.map(String::from)), "the trees print as other regexes: {printed:?}");
    let mut wrong = Vec::new();
    for ((p, n, line, want), shown) in cases.iter().zip(&printed) {
        let mut cost = p.coster(*n);
        assert_eq!(&reference(line, &mut cost), want, "the reference: {} {shown:?} on {line:?}", p.flag(*n));
        match binary(&dir.0, &p.flag(*n), &p.arg(), &[line.to_string()]) {
            Err(e) => wrong.push(format!("{} {shown:?}: refused: {e}", p.flag(*n))),
            Ok(got) => {
                let have = got.get(&1).cloned().unwrap_or_default();
                if &have != want {
                    wrong.push(format!("{} {shown:?} on {line:?}: reference {want:?}, binary {have:?}", p.flag(*n)));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{} of {} fixed cases differ:\n{}", wrong.len(), cases.len(), wrong.join("\n"));
}

/// Generated literals at N = 0, 1, 2 and regexes at N = 0, 1, a few hundred lines each, against
/// the reference. Patterns the binary refuses are counted and skipped. `FUZZY_SPANS_MORE=K` adds K
/// more fixed seeds of 95 patterns each (off by default, so the suite keeps its few seconds).
#[test]
fn fuzzy_spans_of_generated_patterns() {
    let dir = Dir::new("generated");
    let more: u64 = std::env::var("FUZZY_SPANS_MORE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let (mut skipped, mut wrong, mut run) = (0, Vec::new(), 0);
    for seed in 0..=more {
        let mut rng = Rng(0x5eed_f022_75a5_0001 + seed * 0x9e37_79b9);
        let mut cases: Vec<(Pattern, usize)> = Vec::new();
        // a pattern within N edits of the empty string is refused: draw again until it is not, so
        // nearly every case is compared; any the binary still refuses is counted below
        for n in 0..=2 {
            for _ in 0..15 {
                cases.push((Pattern::Literal(rng.text((n + 1).min(4), 4)), n));
            }
        }
        for n in 0..=1 {
            for _ in 0..25 {
                let re = std::iter::repeat_with(|| gen_re(&mut rng, 3)).find(|r| r.lengths().0 > n).unwrap();
                cases.push((Pattern::Regex(re), n));
            }
        }
        for (p, n) in &cases {
            let lines: Vec<String> = (0..LINES_PER_PATTERN).map(|_| rng.text(0, 10)).collect();
            match compare(&dir.0, p, *n, &lines) {
                Err(e) => {
                    skipped += 1;
                    eprintln!("skipped {} {:?}: {e}", p.flag(*n), p.arg());
                }
                Ok(w) => {
                    run += 1;
                    wrong.extend(w);
                }
            }
        }
    }
    eprintln!("fuzzy spans: {} seeds, {run} patterns compared, {skipped} refused by the binary and skipped", more + 1);
    let shown: Vec<&String> = wrong.iter().take(40).collect();
    assert!(
        wrong.is_empty(),
        "{} lines differ over {run} patterns ({skipped} skipped); the first {}:\n{}",
        wrong.len(),
        shown.len(),
        shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
    );
}
