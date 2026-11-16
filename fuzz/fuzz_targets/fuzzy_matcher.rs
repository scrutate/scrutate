//! The bit-parallel edit-distance matcher on arbitrary patterns and haystacks: no panic, and every
//! reported match lies inside the haystack on one line.
#![no_main]
use grep_matcher::Matcher;
use libfuzzer_sys::fuzz_target;
use scrutate::instant::FuzzyMatcher;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let k = data[0] % 4;
    let fold = data[1] & 1 == 1;
    let split = 2 + (data[2] as usize % 130).min(data.len() - 3);
    let (pat, hay) = data[3..].split_at((split - 2).min(data.len() - 3));
    let Ok(p) = std::str::from_utf8(pat) else {
        return;
    };
    let Ok(m) = FuzzyMatcher::new(p, k, fold) else {
        return;
    };
    let mut at = 0;
    while at <= hay.len() {
        match m.find_at(hay, at).unwrap() {
            Some(s) => {
                assert!(s.start() <= s.end() && s.end() <= hay.len());
                assert!(!hay[s.start()..s.end()].contains(&b'\n'));
                at = s.end().max(at + 1);
            }
            None => break,
        }
    }
});
