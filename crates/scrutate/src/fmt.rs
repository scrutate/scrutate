//! Plain-text rendering of hits.

use std::borrow::Cow;

use crate::search::Hit;

/// What `shown` wrote, read back: `\\n`, `\\r`, `\\t`, `\\xHH` and `\\u{H}` become the characters,
/// and `\\\\` one backslash.
pub fn unshown(s: &str) -> Cow<'_, str> {
    if !s.contains('\\') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let hex = |digits: Option<&str>| digits.and_then(|d| u32::from_str_radix(d, 16).ok()).and_then(char::from_u32);
        let (c, used) = match tail.as_bytes().first() {
            Some(b'\\') => (Some('\\'), 1),
            Some(b'n') => (Some('\n'), 1),
            Some(b'r') => (Some('\r'), 1),
            Some(b't') => (Some('\t'), 1),
            Some(b'x') => (hex(tail.get(1..3)), 3),
            Some(b'u') => match tail.find('}') {
                Some(end) if tail.starts_with("u{") => (hex(tail.get(2..end)), end + 1),
                _ => (None, 0),
            },
            _ => (None, 0),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &tail[used..];
            }
            None => {
                out.push('\\');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Text as one line a terminal shows as it is: control characters written as escapes (`\n`,
/// `\x1b`), so a file name cannot start a line of its own or move the cursor, and a backslash
/// written as two, so an escape and a real backslash never print alike.
pub fn shown(s: &str) -> Cow<'_, str> {
    escaped(s, true)
}

/// A character a terminal does not show as itself: a control character, one that reorders the text
/// around it (bidi marks, the Arabic letter mark included, embeddings, overrides and isolates: `bidi\u{202e}yp.txt` displays as
/// `biditxt.py`) or one that breaks the line.
fn hidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}')
}

fn escaped(s: &str, backslash: bool) -> Cow<'_, str> {
    if !s.chars().any(|c| hidden(c) || (backslash && c == '\\')) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' if backslash => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() && (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if hidden(c) => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Characters of an argument an error message repeats.
const ECHO_CHARS: usize = 200;

/// An argument as an error message repeats it: control characters escaped as `shown` does (a
/// backslash stays one, as the caller wrote it), and cut to its first 200 characters.
pub fn echo(s: &str) -> String {
    let n = s.chars().count();
    if n <= ECHO_CHARS {
        return escaped(s, false).into_owned();
    }
    let head: String = s.chars().take(ECHO_CHARS).collect();
    format!("{}... (cut: {n} characters in all)", escaped(&head, false))
}

/// `path:start-end` of a hit, as text.
pub fn loc(h: &Hit) -> String {
    format!("{}:{}-{}", shown(&h.path), h.start, h.end)
}

pub fn fmt_hits(hits: &[Hit], words: &[String], snippet: bool, why: bool) -> String {
    if hits.is_empty() {
        return "(no results)".to_string();
    }
    hit_blocks(hits, words, snippet, why).join("\n")
}

/// One block of text per hit: its location line and, with `snippet`, the lines that matter.
pub fn hit_blocks(hits: &[Hit], words: &[String], snippet: bool, why: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(hits.len());
    for h in hits {
        let name = if h.qualname.is_empty() { &h.name } else { &h.qualname };
        let mut head = format!("{}  [{}] {}", loc(h), h.kind, shown(name));
        if why && !h.why.is_empty() {
            head.push_str(&format!("  ({})", h.why.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if snippet {
            let body = h.snippet(words, 8);
            head.push('\n');
            head.push_str(&body.split('\n').map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"));
        }
        out.push(head);
    }
    out
}

pub fn hits_json(hits: &[Hit]) -> serde_json::Value {
    serde_json::Value::Array(
        hits.iter()
            .map(|h| {
                let why: serde_json::Map<String, serde_json::Value> = h.why.iter().cloned().collect();
                serde_json::json!({
                    "id": h.loc(), "path": h.path, "start": h.start, "end": h.end,
                    "name": if h.qualname.is_empty() { &h.name } else { &h.qualname },
                    "kind": h.kind, "score": h.score, "why": why,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_that_reorder_or_break_a_line_are_shown_escaped_and_read_back() {
        for (name, want) in [
            ("bidi\u{202e}yp.txt", r"bidi\u{202e}yp.txt"),
            ("a\u{2066}b\u{2069}c", r"a\u{2066}b\u{2069}c"),
            ("x\u{200f}y", r"x\u{200f}y"),
            ("a\u{61c}b.py", r"a\u{61c}b.py"),
            ("one\u{2028}two\u{2029}", r"one\u{2028}two\u{2029}"),
        ] {
            assert_eq!(shown(name), want);
            assert_eq!(unshown(want), name);
        }
        assert!(matches!(shown("plain é.py"), Cow::Borrowed(_)));
    }
}
