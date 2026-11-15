//! How a typed name matches a definition's name, for the lookups that rank near misses: the
//! language server's workspace symbols, and `symbol` / `find_symbol` when no definition has the
//! name itself.

use anyhow::Result;

use crate::index::{ChunkRow, Index};
use crate::text::name_key;

/// How a query matches a name, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum How {
    Exact,
    Prefix,
    Humps,
    Contains,
}

impl How {
    pub fn as_str(self) -> &'static str {
        match self {
            How::Exact => "exact",
            How::Prefix => "prefix",
            How::Humps => "humps",
            How::Contains => "contains",
        }
    }
}

/// Named definitions `query` matches, best first: exact, prefix, humps, contains (a key of three
/// characters or more), each class by name (shorter first), each name's definitions as the index
/// orders them (outside tests, shorter, by path and line); at most `limit`.
pub fn ranked(ix: &Index, query: &str, limit: usize) -> Result<Vec<(ChunkRow, How)>> {
    // an empty key is a prefix of every name: the language server's empty query lists them all
    let key = name_key(query);
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut seen = std::collections::HashSet::new();
    let mut keys: Vec<(String, How)> = Vec::new();
    let take = |found: Vec<String>, how: How, keys: &mut Vec<(String, How)>| keys.extend(found.into_iter().map(|k| (k, how)));
    let found = ix.names_with_prefix(&key, limit, |k, _| seen.insert(k.to_string()))?;
    let (exact, prefix): (Vec<String>, Vec<String>) = found.into_iter().partition(|k| *k == key);
    take(exact, How::Exact, &mut keys);
    take(prefix, How::Prefix, &mut keys);
    let parts = parts(query);
    if parts.len() >= 2 {
        let initials: String = parts.iter().filter_map(|p| p.chars().next()).collect();
        let found = ix.names_with_initials(&initials, limit, |k, name| humps(&parts, &words(name)) && seen.insert(k.to_string()))?;
        take(found, How::Humps, &mut keys);
    }
    if key.chars().count() >= 3 {
        let found = ix.names_containing(&key, limit, |k, _| seen.insert(k.to_string()))?;
        take(found, How::Contains, &mut keys);
    }
    let mut out = Vec::new();
    for (k, how) in keys {
        // the spellings of a key match on their own: FB is humps of FooBar, not of foobar
        let own = |r: &ChunkRow| if how == How::Humps { classify(query, &r.name) } else { Some(how) };
        out.extend(ix.defs_named(&k)?.into_iter().filter_map(|r| own(&r).map(|h| (r, h))));
        if out.len() >= limit {
            break;
        }
    }
    out.truncate(limit);
    Ok(out)
}

/// The first letter of each word of `name`, lowercase: the key humps are looked up by.
pub fn initials(name: &str) -> String {
    words(name).iter().filter_map(|w| w.chars().next()).collect()
}

/// How `query` matches `name`, if at all. Exact, prefix and contains compare the names' keys (case
/// and `_` aside); humps needs two or more parts in the query.
pub fn classify(query: &str, name: &str) -> Option<How> {
    let (q, n) = (name_key(query), name_key(name));
    if q.is_empty() {
        None
    } else if q == n {
        Some(How::Exact)
    } else if n.starts_with(&q) {
        Some(How::Prefix)
    } else if humps(&parts(query), &words(name)) {
        Some(How::Humps)
    } else if n.contains(&q) {
        Some(How::Contains)
    } else {
        None
    }
}

/// The words of a name, lowercase: split at `_`, case changes and any character that is not a
/// name's (crate::ident), an acronym one word (`HTTPServer` is `http`, `server`), a run of digits
/// a word of its own (`widget2` is `widget`, `2`); a mark such as a virama stays in its word.
fn words(name: &str) -> Vec<String> {
    let c: Vec<char> = name.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &ch) in c.iter().enumerate() {
        if separates(ch) {
            out.extend((!cur.is_empty()).then(|| std::mem::take(&mut cur)));
            continue;
        }
        let prev = i.checked_sub(1).map(|j| c[j]);
        let next_lower = c.get(i + 1).is_some_and(|n| n.is_lowercase());
        let starts = ch.is_uppercase() && prev.is_some_and(|p| p.is_lowercase() || (p.is_uppercase() && next_lower))
            || prev.is_some_and(|p| p.is_numeric() != ch.is_numeric());
        if starts && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(ch.to_lowercase());
    }
    out.extend((!cur.is_empty()).then_some(cur));
    out
}

/// The parts of a query, lowercase: split as `words` splits a name, and before every capital, so
/// each capital of `BS` or `HS` starts a part.
fn parts(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<char> = None;
    for ch in query.chars() {
        let digits_turn = prev.is_some_and(|p| p.is_numeric() != ch.is_numeric());
        prev = Some(ch);
        if separates(ch) {
            out.push(String::new());
        } else if ch.is_uppercase() || out.is_empty() || digits_turn {
            out.push(ch.to_lowercase().collect());
        } else if let Some(last) = out.last_mut() {
            last.extend(ch.to_lowercase());
        }
    }
    out.retain(|p| !p.is_empty());
    out
}

/// A character between two words of a name.
fn separates(ch: char) -> bool {
    !crate::ident::is_continue(ch) || ch == '_'
}

/// Whether the query's parts fit the name's first words, one part a word, in order: each part's
/// first letter starts its word, and its other letters follow in order inside that word.
fn humps(parts: &[String], words: &[String]) -> bool {
    fn fits(part: &str, word: &str) -> bool {
        let mut w = word.chars();
        let mut p = part.chars();
        p.next().is_some_and(|f| w.next() == Some(f)) && p.all(|c| w.any(|x| x == c))
    }
    parts.len() >= 2 && parts.len() <= words.len() && parts.iter().zip(words).all(|(p, w)| fits(p, w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_matches_a_name_exactly_by_prefix_by_humps_or_inside_it() {
        assert_eq!(classify("BookingsService", "BookingsService"), Some(How::Exact));
        assert_eq!(classify("bookings_service", "BookingsService"), Some(How::Exact), "case and _ aside");
        assert_eq!(classify("BookingsServ", "BookingsService"), Some(How::Prefix));
        assert_eq!(classify("BkSvc", "BookingsService"), Some(How::Humps));
        assert_eq!(classify("bk_svc", "BookingsService"), Some(How::Humps));
        assert_eq!(classify("BS", "BookingsService"), Some(How::Humps));
        assert_eq!(classify("HS", "HTTPServer"), Some(How::Humps), "an acronym is one word");
        assert_eq!(classify("GrSt", "GrößeStufe"), Some(How::Humps));
        assert_eq!(classify("ingsServ", "BookingsService"), Some(How::Contains));
    }

    #[test]
    fn a_query_that_names_none_of_it_does_not_match() {
        assert_eq!(classify("zzz", "BookingsService"), None);
        assert_eq!(classify("", "BookingsService"), None);
        // the parts must follow the words in order, and the first must start the name
        assert_eq!(classify("SvcBk", "BookingsService"), None);
        assert_eq!(classify("SvcX", "BookingsService"), None);
        // one part is a prefix or nothing: `bk` is not a hump of Bookings
        assert_eq!(classify("bk", "BookingsService"), None);
    }

    #[test]
    fn humps_sit_on_consecutive_words_from_the_first() {
        assert_eq!(classify("MP", "make_point_area"), Some(How::Humps));
        assert_eq!(classify("PA", "make_point_area"), None, "not from the first word: contains finds it if anything");
        assert_eq!(classify("MkA", "make_point_area"), None, "a word skipped");
        assert_eq!(initials("make_point_area"), "mpa");
        assert_eq!(initials("HTTPServer"), "hs");
    }

    #[test]
    fn a_number_is_a_word_of_its_own() {
        assert_eq!(words("make_widget_2"), ["make", "widget", "2"]);
        assert_eq!(words("utf8Decode"), ["utf", "8", "decode"]);
        assert_eq!(classify("make_widget_2", "make_widget_7"), None, "2 is not 7");
        assert_eq!(classify("mw2", "make_widget_2"), None, "mw is one part, and make has no w");
        assert_eq!(classify("MW_2", "make_widget_2"), Some(How::Humps));
    }

    #[test]
    fn a_mark_stays_in_its_word() {
        assert_eq!(words("हिन्दी_नाम"), ["हिन्दी", "नाम"]);
        assert_eq!(initials("हिन्दी_नाम"), "हन");
        assert_eq!(classify("ह_न", "हिन्दी_नाम"), Some(How::Humps));
    }

    #[test]
    fn better_matches_sort_first() {
        assert!(How::Exact < How::Prefix && How::Prefix < How::Humps && How::Humps < How::Contains);
    }
}
