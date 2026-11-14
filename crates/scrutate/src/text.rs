//! Tokenization shared by indexing and querying.
//!
//! Everything is lowercase, identifier-aware (camelCase / snake_case are split) and stemmed with the
//! Snowball English stemmer, so `refreshTokens`, `refresh_token` and "refreshing tokens" all meet at
//! `refresh`, `token`. Compound identifiers also emit one joined token (`sendrequest`).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use rust_stemmers::{Algorithm, Stemmer};
use unicode_normalization::UnicodeNormalization;

const ENGLISH: &str = "a an the of to in on at by for with from as and or nor but so if then than that this
these those it its is are was were be been being am do does did done has have had having not no
yes can could should would will shall may might must i we you he she they them his her our your
their my me us about into onto over under up down out off also very just only more most such
there here when where which who whom whose why how what whatever any all each both either some
other another same own too via per etc eg ie e g";
const CODE: &str = "self cls this super none null nil true false def class import from return pass lambda
yield async await elif else try except finally raise with assert break continue global nonlocal
del var let const function new void typeof undefined public private protected static final
fn func pub mut impl struct enum trait use mod crate self_ int str bool float";
const QUESTION: &str = "find show give get where which implemented implement implementation handled
handle handles handling happens happen code codebase file files function functions method
methods class classes defined define used uses using use look looking search want need
please tell list";

pub static INDEX_STOP: LazyLock<HashSet<&'static str>> = LazyLock::new(|| ENGLISH.split_whitespace().chain(CODE.split_whitespace()).collect());
pub static QUERY_STOP: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut s: HashSet<&'static str> = INDEX_STOP.iter().copied().collect();
    s.extend(QUESTION.split_whitespace());
    s
});

/// Maximum identifiers of a body that are indexed / embedded.
pub const BODY_CAP: usize = 2000;

thread_local! {
    static STEMMER: Stemmer = Stemmer::create(Algorithm::English);
    static STEM_CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static IDENT_CACHE: RefCell<HashMap<String, (Vec<String>, String)>> = RefCell::new(HashMap::new());
}

/// Free this thread's word caches (an index build fills them with millions of entries).
pub fn clear_caches() {
    STEM_CACHE.with(|c| drop(c.take()));
    IDENT_CACHE.with(|c| drop(c.take()));
}

/// Entries in this thread's word caches.
pub fn cache_len() -> usize {
    STEM_CACHE.with(|c| c.borrow().len()) + IDENT_CACHE.with(|c| c.borrow().len())
}

pub fn stem(w: &str) -> String {
    if let Some(s) = STEM_CACHE.with(|c| c.borrow().get(w).cloned()) {
        return s;
    }
    let s = STEMMER.with(|st| st.stem(w).into_owned());
    STEM_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() < 2_000_000 {
            c.insert(w.to_string(), s.clone());
        }
    });
    s
}

/// Words of a text. An all-ASCII text gives `[A-Za-z_][A-Za-z0-9_]*` (like the Python regex). Any
/// other text gives maximal runs of letters, marks, decimal digits and `_` of every script; a run
/// that is all ASCII loses its leading digits, so ASCII words come out as they always did.
pub struct Words<'a> {
    bytes: &'a [u8],
    pos: usize,
    runs: Option<regex::Matches<'static, 'a>>,
}

static RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{M}\p{Nd}_]+").unwrap());

impl<'a> Words<'a> {
    pub fn new(text: &'a str) -> Self {
        let runs = if text.is_ascii() { None } else { Some(RUN.find_iter(text)) };
        Words { bytes: text.as_bytes(), pos: 0, runs }
    }
}

fn is_word_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl<'a> Iterator for Words<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if let Some(runs) = &mut self.runs {
            for m in runs.by_ref() {
                let w = m.as_str();
                if !w.is_ascii() {
                    return Some(w);
                }
                let w = w.trim_start_matches(|c: char| c.is_ascii_digit());
                if !w.is_empty() {
                    return Some(w);
                }
            }
            return None;
        }
        let b = self.bytes;
        while self.pos < b.len() && !is_word_start(b[self.pos]) {
            self.pos += 1;
        }
        if self.pos >= b.len() {
            return None;
        }
        let start = self.pos;
        while self.pos < b.len() && is_word_char(b[self.pos]) {
            self.pos += 1;
        }
        // ASCII boundaries are always char boundaries
        Some(unsafe { std::str::from_utf8_unchecked(&b[start..self.pos]) })
    }
}

/// Scripts written without spaces between words: Han, Hiragana, Katakana, and Thai, Lao,
/// Tibetan, Myanmar, Khmer, the Tai scripts, Balinese and Javanese.
fn unspaced(c: char) -> bool {
    matches!(c as u32, 0x0E00..=0x109F | 0x1780..=0x17FF | 0x1950..=0x19DF | 0x1A20..=0x1AAF | 0x1B00..=0x1B7F | 0xA980..=0xA9FF | 0xAA60..=0xAADF | 0x3005..=0x3007 | 0x3040..=0x30FF | 0x31F0..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF66..=0xFF9F | 0x20000..=0x323AF)
}

/// Traditional Han or Japanese shinjitai -> one key, from Unihan `kSimplifiedVariant` and OpenCC (tools/gen_han_fold.py).
static HAN_FOLD: LazyLock<HashMap<char, char>> = LazyLock::new(|| {
    let pairs: Vec<char> = include_str!("../data/han_fold.txt").lines().filter(|l| !l.starts_with('#')).flat_map(str::chars).collect();
    pairs.as_chunks::<2>().0.iter().map(|&[t, s]| (t, s)).collect()
});

/// Lowercase with the final sigma folded, so a word compares the same in any case.
fn fold(s: &str) -> String {
    s.to_lowercase().replace('ς', "σ")
}

/// Tokens of a word that is not all ASCII: NFC first; runs of unspaced scripts have traditional
/// Han folded to simplified and give overlapping character bigrams (a lone character itself),
/// other `_`-separated parts are one folded token each, and an ASCII part goes through the
/// identifier rules (split, stemmed).
pub fn unicode_tokens(word: &str, out: &mut Vec<String>) {
    let w: Vec<char> = word.nfc().collect();
    let mut i = 0;
    while i < w.len() {
        let u = unspaced(w[i]);
        let mut j = i + 1;
        while j < w.len() && unspaced(w[j]) == u {
            j += 1;
        }
        let seg = &w[i..j];
        if !u {
            let seg: String = seg.iter().collect();
            for part in seg.split('_').filter(|p| !p.is_empty()) {
                if part.is_ascii() {
                    out.extend(ident_tokens(part).0);
                } else {
                    out.push(fold(part));
                }
            }
        } else {
            let seg: Vec<char> = seg.iter().map(|c| HAN_FOLD.get(c).copied().unwrap_or(*c)).collect();
            if seg.len() == 1 {
                out.push(seg[0].to_string());
            } else {
                out.extend(seg.windows(2).map(|p| p.iter().collect::<String>()));
            }
        }
        i = j;
    }
}

/// `parseHTTPResponse_v2` -> ["parse", "http", "response", "v"] (lowercase, unstemmed). This is the
/// Python regex `[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+` applied to every `_`-separated part.
pub fn raw_subwords(ident: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in ident.split('_') {
        if part.is_empty() {
            continue;
        }
        let b = part.as_bytes();
        let n = b.len();
        let mut i = 0;
        while i < n {
            let c = b[i];
            if c.is_ascii_uppercase() {
                // alternative 1: a run of capitals followed by Capital+lowercase (greedy, backtracking)
                let mut run = i;
                while run < n && b[run].is_ascii_uppercase() {
                    run += 1;
                }
                let mut matched = None;
                let mut l = run - i;
                while l >= 1 {
                    let j = i + l;
                    if j + 1 < n && b[j].is_ascii_uppercase() && b[j + 1].is_ascii_lowercase() {
                        matched = Some(l);
                        break;
                    }
                    l -= 1;
                }
                if let Some(l) = matched {
                    out.push(part[i..i + l].to_ascii_lowercase());
                    i += l;
                    continue;
                }
                // alternative 2: one capital + lowercase run
                if i + 1 < n && b[i + 1].is_ascii_lowercase() {
                    let mut j = i + 1;
                    while j < n && b[j].is_ascii_lowercase() {
                        j += 1;
                    }
                    out.push(part[i..j].to_ascii_lowercase());
                    i = j;
                    continue;
                }
                // alternative 3: the whole run of capitals
                out.push(part[i..run].to_ascii_lowercase());
                i = run;
            } else if c.is_ascii_lowercase() {
                let mut j = i + 1;
                while j < n && b[j].is_ascii_lowercase() {
                    j += 1;
                }
                out.push(part[i..j].to_string());
                i = j;
            } else {
                i += 1; // digits and anything else are skipped
            }
        }
    }
    out
}

/// -> (stemmed subwords without stopwords, joined whole token or "").
pub fn ident_tokens(ident: &str) -> (Vec<String>, String) {
    if let Some(hit) = IDENT_CACHE.with(|c| c.borrow().get(ident).cloned()) {
        return hit;
    }
    let subs = raw_subwords(ident);
    let toks: Vec<String> = subs.iter().filter(|w| w.len() > 1 && !INDEX_STOP.contains(w.as_str())).map(|w| stem(w)).collect();
    let whole = if subs.len() > 1 && !toks.is_empty() { subs.concat() } else { String::new() };
    let hit = (toks, whole);
    IDENT_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() < 2_000_000 {
            c.insert(ident.to_string(), hit.clone());
        }
    });
    hit
}

/// -> (stemmed subwords, joined whole identifiers) of a text; `cap` limits the number of words read.
pub fn tokenize2(text: &str, cap: Option<usize>) -> (Vec<String>, Vec<String>) {
    let mut subs = Vec::new();
    let mut wholes = Vec::new();
    let mut n = 0;
    for w in Words::new(text) {
        if w.is_ascii() {
            let (t, whole) = ident_tokens(w);
            subs.extend(t);
            if !whole.is_empty() {
                wholes.push(whole);
            }
        } else {
            unicode_tokens(w, &mut subs);
        }
        n += 1;
        if let Some(c) = cap {
            if n >= c {
                break;
            }
        }
    }
    (subs, wholes)
}

/// Case- and underscore-insensitive key in NFC: `send_request`, `sendRequest` -> `sendrequest`.
pub fn name_key(name: &str) -> String {
    if name.is_ascii() {
        return name.chars().filter(|c| *c != '_').map(|c| c.to_ascii_lowercase()).collect();
    }
    fold(&name.nfc().filter(|c| *c != '_').collect::<String>())
}

/// Paths of code a repository holds but did not write: dependency directories and minified
/// files. A subset of GitHub linguist's `lib/linguist/vendor.yml` (commit befd3af, 2026-09-03;
/// MIT, Copyright (c) 2017 GitHub, Inc.), plus CocoaPods' `Pods/` and `jspm_packages/`, which it
/// lacks. Its broad patterns (`dist/`, `deps/`, `cache/`, `extern/`, `testdata/`, `.d.ts`,
/// framework file names) are left out: they are often the project's own code.
const VENDORED: &[&str] = &[
    r"(^|/)vendors?/",
    r"(^|/)[Vv]+endor/",
    r"(3rd|[Tt]hird)[-_]?[Pp]arty/",
    r"(^|/)node_modules/",
    r"(^|/)bower_components/",
    r"(^|/)Godeps/_workspace/",
    r"(^|/)Carthage/",
    r"(\.|-)min\.(js|css)$",
    r"(^|/)Pods/",
    r"(^|/)jspm_packages/",
];

/// Whether `path` is vendored code (`VENDORED`): search ranks it below the project's own.
pub fn is_vendored_path(path: &str) -> bool {
    static SET: LazyLock<regex::RegexSet> = LazyLock::new(|| regex::RegexSet::new(VENDORED).expect("vendored patterns"));
    SET.is_match(&path.replace('\\', "/"))
}

const TEST_PARTS: &[&str] = &["test", "tests", "testing", "__tests__", "spec", "specs", "e2e", "fixtures", "mocks"];

pub fn is_test_path(path: &str) -> bool {
    let path = path.replace('\\', "/");
    let low = path.to_lowercase();
    let parts: Vec<&str> = low.split('/').collect();
    let base = parts[parts.len() - 1];
    let (stem, ext) = base.rsplit_once('.').unwrap_or((base, ""));
    // a C# test project is a directory `MyApp.Tests`; with the capital, `MyApp.Latest` is none
    let dirs = path.rsplit_once('/').map_or("", |(d, _)| d);
    let project = dirs.split('/').any(|d| d.ends_with(".Tests") || d.ends_with(".Test"));
    // `button.test.jsx`, `card.spec.ts`: a JavaScript or TypeScript test beside its code
    let script = matches!(ext, "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts" | "cts") && (stem.ends_with(".test") || stem.ends_with(".spec"));
    // `parser_test.rs`, `parser_spec.rb`, `config_test.cc`
    let suffixed = !ext.is_empty() && (stem.ends_with("_test") || stem.ends_with("_spec"));
    // JUnit and its kin name a test class `FooTest` or `FooTests`: with the capital, `Contest.java`
    // is none. The name as written: lower case can have another length (`İ`, the Kelvin sign).
    let written = path.rsplit('/').next().unwrap_or(&path);
    let name = written.rsplit_once('.').map_or(written, |(n, _)| n);
    let class = matches!(ext, "java" | "kt" | "cs" | "swift" | "php") && (name.ends_with("Test") || name.ends_with("Tests"));
    parts[..parts.len() - 1].iter().any(|p| TEST_PARTS.contains(p))
        || project
        || base.starts_with("test_")
        || script
        || suffixed
        || class
        || matches!(base, "conftest.py" | "tests.py")
}

// --- query analysis -------------------------------------------------------------------------

/// Unique stemmed content terms of a query (question words and stopwords removed), in order.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut seen = Vec::new();
    let mut set = HashSet::new();
    for w in Words::new(query) {
        if !w.is_ascii() {
            let mut toks = Vec::new();
            unicode_tokens(w, &mut toks);
            seen.extend(toks.into_iter().filter(|t| set.insert(t.clone())));
            continue;
        }
        for s in raw_subwords(w) {
            if s.len() > 1 && !QUERY_STOP.contains(s.as_str()) {
                let st = stem(&s);
                if set.insert(st.clone()) {
                    seen.push(st);
                }
            }
        }
    }
    seen
}

/// The longest prefix of `s` of at most `n` bytes that ends on a char boundary.
pub fn prefix_bytes(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut i = n;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// Joined tokens for identifier-like words of the query (`send_request` -> `sendrequest`).
pub fn query_wholes(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut set = HashSet::new();
    for w in Words::new(query).filter(|w| w.is_ascii()) {
        let subs = raw_subwords(w);
        if subs.len() > 1 && (w.contains('_') || w.bytes().skip(1).any(|c| c.is_ascii_uppercase())) {
            let j = subs.concat();
            if set.insert(j.clone()) {
                out.push(j);
            }
        }
    }
    out
}

static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]+)"|'([^']+)'|`([^`]+)`"#).unwrap());
static QUOTED_ANY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""[^"]*"|'[^']*'|`[^`]*`"#).unwrap());
static LIT_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z_][\w.]*[\w]").unwrap());

/// Python's `str.isupper()`: at least one cased character and no lowercase ones.
fn py_isupper(s: &str) -> bool {
    let mut cased = false;
    for c in s.chars() {
        if c.is_lowercase() {
            return false;
        }
        if c.is_uppercase() {
            cased = true;
        }
    }
    cased
}

/// Pieces of the query that look like exact code strings: quoted strings and identifier-ish words
/// (snake_case, camelCase, dotted, CONSTANTS).
pub fn literal_candidates(query: &str) -> Vec<String> {
    let mut lits: Vec<String> = Vec::new();
    for m in QUOTED.captures_iter(query) {
        for i in 1..=3 {
            if let Some(g) = m.get(i) {
                lits.push(g.as_str().to_string());
                break;
            }
        }
    }
    let stripped = QUOTED_ANY.replace_all(query, " ");
    for m in LIT_WORD.find_iter(&stripped) {
        let w = m.as_str();
        let tail_upper = w.chars().skip(1).any(|c| c.is_uppercase());
        let any_lower = w.chars().any(|c| c.is_lowercase());
        if w.contains('_') || w.contains('.') || (tail_upper && any_lower) || (py_isupper(w) && w.chars().count() > 2) {
            lits.push(w.to_string());
        }
    }
    let mut seen = HashSet::new();
    lits.retain(|l| seen.insert(l.clone()));
    lits
}

/// Lines as grep-class tools number them: split at `\n`, a `\r` before it dropped, no empty
/// last line for a trailing newline. Form feeds, lone `\r` and Unicode separators are text, so a
/// chunk's line numbers agree with Engine A's and tree-sitter's.
pub fn split_lines(s: &str) -> Vec<&str> {
    s.lines().collect()
}

/// Lowercase words of a query, for snippet highlighting (agent.query_words: len > 2).
pub fn query_words(q: &str) -> Vec<String> {
    Words::new(q).filter(|w| w.chars().count() > 2).map(|w| w.to_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vendored_path_is_a_dependency_directory_or_a_minified_file() {
        let yes = [
            "vendor/bundle/ruby/3.3.0/gems/rack/lib/rack.rb",
            "app/vendor/lib.go",
            "Vendor/x.m",
            "Pods/AFNetworking/AFURLSession.m",
            "Carthage/Checkouts/x.swift",
            "third_party/zlib/inflate.c",
            "src/third-party/x.js",
            "web/node_modules/left-pad/index.js",
            "bower_components/x/x.js",
            "jspm_packages/x.js",
            "Godeps/_workspace/src/x.go",
            "static/app.min.js",
            "static/site-min.css",
        ];
        for p in yes {
            assert!(is_vendored_path(p), "{p}");
        }
        // linguist's broad patterns are left out: these are often the project's own code
        let no = ["src/vendors.rs", "src/vendor.rs", "lib/party/x.py", "deps/x.c", "src/dist/x.rs", "cache/x.py", "external/x.c", "x.d.ts", "src/minimal.js"];
        for p in no {
            assert!(!is_vendored_path(p), "{p}");
        }
    }

    #[test]
    fn a_test_file_is_told_by_its_directory_or_its_name() {
        for p in ["pkg/tests/a.py", "a/b_test.go", "conftest.py", "src/main/java/FooTest.java", "a/FooTests.java", "app/tests.py", "web/app.spec.ts"] {
            assert!(is_test_path(p), "{p}");
        }
        for p in ["pkg/mod.py", "src/latest.py", "src/main/java/Contest.java", "a/Latest.java", "a/attest.java"] {
            assert!(!is_test_path(p), "{p}");
        }
        // by the file's name in every language that names tests so, and a C# test project's directory
        for p in [
            "src/button.test.jsx",
            "src/card.spec.jsx",
            "src/mod.test.mjs",
            "src/a.spec.cts",
            "src/config_test.cc",
            "src/config_test.cpp",
            "src/parser_spec.rb",
            "src/parser_test.rb",
            "src/parser_test.rs",
            "MyApp.Tests/Parser.cs",
            "My.Test/Parser.cs",
            "src/ParserTests.cs",
            "src/WidgetTest.kt",
            "src/WidgetTests.swift",
            "src/UserTest.php",
            "src/widget_test.dart",
            "lib/a_test.exs",
        ] {
            assert!(is_test_path(p), "{p}");
        }
        for p in [
            "src/contest.cc",
            "src/latest.rs",
            "src/Contest.kt",
            "src/Latest.cs",
            "src/attest.jsx",
            "src/spec.rb",
            "src/test.cfg/a.py",
            "src/a.test/b.py",
            "MyApp.Testing.Core/a.cs",
            "src/button.test.md",
            "src/_test",
        ] {
            assert!(!is_test_path(p), "{p}");
        }
        // a name whose lower case has another length in bytes: `İ` grows, the Kelvin sign shrinks
        assert!(!is_test_path("\u{130}.cs"));
        assert!(is_test_path("\u{130}Test.java"));
        assert!(is_test_path("a/\u{212a}Test.kt"));
        assert!(!is_test_path("a/\u{212a}test.kt"));
    }

    #[test]
    fn camel_split_matches_python_regex() {
        assert_eq!(raw_subwords("parseHTTPResponse_v2"), vec!["parse", "http", "response", "v"]);
        assert_eq!(raw_subwords("HTTPServer"), vec!["http", "server"]);
        assert_eq!(raw_subwords("ABC"), vec!["abc"]);
        assert_eq!(raw_subwords("getX"), vec!["get", "x"]);
        assert_eq!(raw_subwords("__init__"), vec!["init"]);
        assert_eq!(raw_subwords("x2y"), vec!["x", "y"]);
        assert_eq!(raw_subwords("IOError"), vec!["io", "error"]);
        assert_eq!(raw_subwords("aBCd"), vec!["a", "b", "cd"]);
    }

    #[test]
    fn ident_tokens_and_whole() {
        let (t, w) = ident_tokens("refresh_token");
        assert_eq!(t, vec!["refresh", "token"]);
        assert_eq!(w, "refreshtoken");
        let (t, w) = ident_tokens("self");
        assert!(t.is_empty() && w.is_empty());
    }

    #[test]
    fn splitlines_like_python() {
        assert_eq!(split_lines("a\nb\r\nc\rd"), vec!["a", "b", "c\rd"]);
        assert_eq!(split_lines("a\n"), vec!["a"]);
        assert_eq!(split_lines("a\x0cb"), vec!["a\x0cb"]);
        assert_eq!(split_lines(""), Vec::<&str>::new());
        assert_eq!(split_lines("\n\nx"), vec!["", "", "x"]);
    }

    #[test]
    fn literals() {
        assert_eq!(literal_candidates("where is send_request called, see 'foo bar' and Server.add"), vec!["foo bar", "send_request", "Server.add"]);
        assert_eq!(literal_candidates("MAX_SIZE and ABC and Ab"), vec!["MAX_SIZE", "ABC"]);
    }
}

#[cfg(test)]
mod utf8_tests {
    use super::*;

    #[test]
    fn prefix_never_splits_a_char() {
        let s = "aé€😀".repeat(2000);
        for n in 0..40 {
            let p = prefix_bytes(&s, n);
            assert!(p.len() <= n);
            assert!(s.starts_with(p));
        }
        assert_eq!(prefix_bytes("abc", 10), "abc");
    }

    #[test]
    fn query_wholes_accept_non_ascii_words() {
        // must not panic on a multibyte first character
        let _ = query_wholes("ñandúRequest sendRequest Ωmega_value");
        assert_eq!(query_wholes("sendRequest"), vec!["sendrequest".to_string()]);
    }
}
