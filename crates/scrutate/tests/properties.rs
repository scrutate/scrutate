//! Property-based tests (proptest): invariants that must hold for *any* input, not just the
//! examples in the unit tests.

use grep_matcher::Matcher;
use proptest::prelude::*;
use scrutate::instant::{FuzzyMatcher, ident_parts, identifier_regex};
use scrutate::npy::{Npy, save};
use scrutate::parse::parse_source;
use scrutate::text::{name_key, prefix_bytes, query_terms, tokenize2};

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, .. ProptestConfig::default() })]

    /// A byte prefix never splits a character and is the longest one that fits.
    #[test]
    fn prefix_bytes_is_valid_and_maximal(s in "\\PC{0,64}", n in 0usize..80) {
        let p = prefix_bytes(&s, n);
        prop_assert!(p.len() <= n || s.len() <= n);
        prop_assert!(s.starts_with(p));
        prop_assert!(s.is_char_boundary(p.len()));
        if p.len() < s.len() {
            // the next character would not fit
            let next = s[p.len()..].chars().next().unwrap();
            prop_assert!(p.len() + next.len_utf8() > n);
        }
    }

    /// Tokenization never panics and only yields non-empty lowercase tokens without spaces; an
    /// ASCII token is lowercase letters and digits.
    #[test]
    fn tokenize_total_and_normalised(s in "\\PC{0,200}", cap in proptest::option::of(1usize..300)) {
        let (stems, words) = tokenize2(&s, cap);
        for t in stems.iter().chain(words.iter()) {
            prop_assert!(!t.is_empty() && !t.contains(char::is_whitespace) && t.to_lowercase() == *t, "token {t:?}");
            if t.is_ascii() {
                prop_assert!(t.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()), "token {t:?}");
            }
        }
        let _ = query_terms(&s);
        let k = name_key(&s);
        prop_assert!(k == k.to_lowercase() && !k.contains('_'));
    }

    /// name_key is a normal form: applying it twice changes nothing, and casing / separators do not matter.
    #[test]
    fn name_key_is_idempotent_and_case_insensitive(s in "[A-Za-z_][A-Za-z0-9_]{0,30}") {
        let k = name_key(&s);
        prop_assert_eq!(name_key(&k), k.clone());
        prop_assert_eq!(name_key(&s.to_uppercase()), k.clone());
        prop_assert_eq!(name_key(&s.replace('_', "")), k);
    }

    /// Identifier parts lose nothing but separators and casing, and the regex they build matches
    /// the snake_case, camelCase and SCREAMING_CASE spellings of the same identifier.
    #[test]
    fn identifier_regex_matches_all_casings(parts in proptest::collection::vec("[a-z][a-z0-9]{1,6}", 1..5)) {
        // parts have at least two characters: a run of single capitals (`aAA`) is an acronym by
        // convention and cannot be told apart from three one-letter words
        let snake = parts.join("_");
        let camel: String = parts.iter().enumerate().map(|(i, p)| if i == 0 { p.clone() } else { let mut c = p.chars(); c.next().unwrap().to_uppercase().collect::<String>() + c.as_str() }).collect();
        let scream = snake.to_uppercase();
        let joined: String = ident_parts(&snake).join("").to_lowercase();
        prop_assert_eq!(joined, snake.replace('_', ""));
        for spelling in [&snake, &camel, &scream] {
            let re = regex::Regex::new(&identifier_regex(spelling)).unwrap();
            prop_assert!(re.is_match(&format!("x = {snake}(1)")), "{spelling} should match {snake}");
            prop_assert!(re.is_match(&scream), "{spelling} should match {scream}");
            prop_assert!(!re.is_match(&format!("{snake}_extra")), "{spelling} must not match a longer identifier");
        }
    }

    /// The fuzzy matcher finds a pattern after at most k random single-character edits, and
    /// finds the unedited pattern with distance 0.
    #[test]
    fn fuzzy_finds_edited_patterns(pat in "[a-z]{4,20}", k in 1u8..3, edits in proptest::collection::vec((0usize..20, 0u8..3, 0u8..26), 0..3), prefix in "[a-z ]{0,10}", suffix in "[a-z ]{0,10}") {
        prop_assume!((k as usize) < pat.len());
        let m = FuzzyMatcher::new(&pat, k, false).unwrap();
        let mut text: Vec<u8> = pat.as_bytes().to_vec();
        for (pos, kind, ch) in edits.iter().take(k as usize) {
            if text.is_empty() { break; }
            let p = pos % text.len();
            match kind {
                0 => text[p] = b'a' + ch,                 // substitute
                1 => { text.remove(p); }                  // delete
                _ => text.insert(p, b'a' + ch),           // insert
            }
        }
        let hay = format!("{prefix}{}{suffix}\n", String::from_utf8_lossy(&text));
        prop_assert!(m.find_at(hay.as_bytes(), 0).unwrap().is_some(), "pattern {pat:?} not found in {hay:?} within {k} edits");
        // the unedited pattern is always found; the reported span is the *first* approximate match
        // on the line (it may end before the exact occurrence when a nearby substring is within k
        // edits), which is what line-oriented reporting needs
        let exact = format!("{prefix}{pat}{suffix}\n");
        let hit = m.find_at(exact.as_bytes(), 0).unwrap().expect("exact occurrence found");
        prop_assert!(hit.end() < exact.len() && hit.start() < hit.end(), "{hit:?}");
    }

    /// Any source text parses into well-formed chunks for any extension: ordered 1-based ranges
    /// within the file, no panics.
    #[test]
    fn parser_yields_well_formed_chunks(src in "[ -~\\n\\t]{0,400}", ext in prop::sample::select(vec!["py", "js", "ts", "go", "rs", "java", "c", "rb", "sh", "json", "md", "txt", "zig", "hs"])) {
        let n_lines = src.split('\n').count() as i64;
        for c in parse_source(&format!("f.{ext}"), &src) {
            prop_assert!(c.start >= 1 && c.end >= c.start, "{c:?}");
            prop_assert!(c.end <= n_lines.max(1), "chunk {}-{} beyond {} lines", c.start, c.end, n_lines);
        }
    }

    /// .npy round trip for arbitrary i64 / f32 data and shapes.
    #[test]
    fn npy_roundtrip(data in proptest::collection::vec(any::<i64>(), 0..64), cols in 1usize..5) {
        let dir = std::env::temp_dir().join(format!("scrutate_prop_{}_{}", std::process::id(), data.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let rows = data.len() / cols;
        let data = &data[..rows * cols];
        let p = dir.join("a.npy");
        save(&p, data, &[rows, cols]).unwrap();
        let n = Npy::open(&p).unwrap();
        prop_assert_eq!(n.shape.clone(), vec![rows, cols]);
        prop_assert_eq!(n.view::<i64>().unwrap(), data);
        prop_assert!(n.view::<f32>().is_err(), "dtype must be checked");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
