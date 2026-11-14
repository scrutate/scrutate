//! Soundness of the text index: whenever a pattern matches a text (fuzzy ones through the matcher
//! grep builds), the trigram condition derived from the pattern must accept that text's trigrams and
//! their masks (otherwise `grep --index` would miss the match).
#![no_main]
use grep_matcher::Matcher;
use libfuzzer_sys::fuzz_target;
use scrutate::instant::{build_matcher, AnyMatcher, Case, Mode, Pattern, Query};
use scrutate::textindex::{holds, query_for, trigram_masks};

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else { return };
    let Ok(s) = std::str::from_utf8(rest) else { return };
    let Some((pat, text)) = s.split_once('\u{0}') else { return };
    if pat.len() > 64 {
        return;
    }
    let case = if flags & 1 == 1 { Case::Insensitive } else { Case::Sensitive };
    let mode = match flags >> 1 & 3 {
        0 => Mode::Regex,
        1 => Mode::Literal,
        2 => Mode::Identifier,
        _ => Mode::Fuzzy { max_edits: 1 },
    };
    let q = Query { pattern: Pattern { text: pat.to_string(), mode, case, ..Pattern::default() }, ..Query::default() };
    let Ok(cond) = query_for(&q) else { return };
    let tris = trigram_masks(text.as_bytes());
    if let Mode::Fuzzy { .. } = mode {
        // the matcher grep builds, so the condition is checked against what grep matches
        let Ok(AnyMatcher::Fuzzy(m)) = build_matcher(&q.pattern) else { return };
        for line in text.split('\n') {
            if m.is_match(line.as_bytes()).unwrap() {
                assert!(holds(&cond, &tris), "fuzzy pattern {pat:?} matches {line:?} but {cond:?} rejects the text");
            }
        }
        return;
    }
    let re = match mode {
        Mode::Regex => regex::RegexBuilder::new(pat).case_insensitive(case == Case::Insensitive).size_limit(1 << 20).build(),
        Mode::Literal => regex::RegexBuilder::new(&regex::escape(pat)).case_insensitive(case == Case::Insensitive).build(),
        Mode::Identifier => regex::Regex::new(&scrutate::instant::identifier_regex(pat)),
        Mode::Fuzzy { .. } | Mode::Pcre2 | Mode::FuzzyRegex { .. } => return,
    };
    if let Ok(re) = re {
        for line in text.split('\n') {
            if re.is_match(line) {
                assert!(holds(&cond, &tris), "pattern {pat:?} matches {line:?} but {cond:?} rejects the text");
            }
        }
    }
});
