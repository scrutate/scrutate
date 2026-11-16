//! `edit_symbol` (`serve --allow-edits`): one whole definition replaced with new text, shown as a
//! diff unless asked to write, written only when the file's hash is the one the caller saw, and
//! only when the new file parses and keeps every definition outside the edit.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::ops::{Answer, Op};
use crate::search::Searcher;

/// What an edit did or would do.
#[derive(Debug)]
pub struct Edited {
    pub path: String,
    /// the definition's lines before and after
    pub old: (i64, i64),
    pub new: (i64, i64),
    pub applied: bool,
    /// the file's hash: as it is for a dry run, as written for an applied edit
    pub hash: String,
    pub diff: String,
}

impl Edited {
    pub fn text(&self) -> String {
        let (p, (a, b), (c, d)) = (crate::fmt::shown(&self.path), self.old, self.new);
        let head = if self.applied {
            format!("edited {p}:{a}-{b}, now {p}:{c}-{d}")
        } else {
            format!("dry run: {p}:{a}-{b} would become {p}:{c}-{d}, nothing written; to write it, call again with apply: true and this hash")
        };
        format!("{head}\nhash: {}\n{}", self.hash, self.diff.trim_end())
    }

    pub fn json(&self) -> Value {
        json!({"path": self.path, "start": self.old.0, "end": self.old.1, "new_start": self.new.0, "new_end": self.new.1,
               "applied": self.applied, "hash": self.hash, "diff": self.diff})
    }
}

/// The hash an edit is checked against: 16 hex digits of the file's content hash, so no secret
/// detector takes it for a key and an answer shows it as it is.
fn content_hash(bytes: &[u8]) -> String {
    crate::index::content_hash(bytes)[..16].to_string()
}

/// The directory of `.git` or `.scrutate` that `rel` lies in, at any depth: by its text, under
/// every spelling git's own rules take for one (`files::own_dir_name`), and by identity with the
/// root's own, so a link to either counts too.
fn guarded(root: &std::path::Path, rel: &str) -> Option<&'static str> {
    let dirs: Vec<(&'static str, std::path::PathBuf)> = [".git", ".scrutate"].into_iter().map(|d| (d, root.join(d))).filter(|(_, p)| p.exists()).collect();
    let mut at = root.to_path_buf();
    for part in rel.split('/') {
        if let Some(own) = crate::files::own_dir_name(part) {
            return Some(own);
        }
        at.push(part);
        if let Some((d, _)) = dirs.iter().find(|(_, p)| same_file::is_same_file(&at, p).unwrap_or(false)) {
            return Some(d);
        }
    }
    None
}

/// A definition as the parse of a file gives it.
#[derive(Clone, Debug, PartialEq)]
struct Def {
    qualname: String,
    kind: &'static str,
    start: i64,
    end: i64,
    bytes: Option<(usize, usize)>,
}

/// Unfilled, so each keeps where it lies in `source`.
fn defs(path: &str, source: &str) -> Vec<Def> {
    crate::parse::parse_file_with(path, source, None)
        .0
        .chunks
        .into_iter()
        .map(|c| Def { bytes: c.bytes(), qualname: c.qualname, kind: c.kind, start: c.start, end: c.end })
        .collect()
}

/// The error and missing nodes of a tree, and the line of the first.
fn errors(root: tree_sitter::Node) -> (usize, usize) {
    let (mut n, mut first) = (0, usize::MAX);
    let mut stack = vec![root];
    while let Some(x) = stack.pop() {
        if !x.has_error() {
            continue;
        }
        if x.is_error() || x.is_missing() {
            n += 1;
            first = first.min(x.start_position().row + 1);
        }
        let mut c = x.walk();
        stack.extend(x.children(&mut c));
    }
    (n, first)
}

/// `range` widened to each node around it that holds it alone and lies within the lines `a..b`
/// (`export` before a function), so their words are the definition's; never to the file's root.
fn wrapped(root: tree_sitter::Node, range: (usize, usize), (a, b): (usize, usize)) -> (usize, usize) {
    let (mut from, mut to) = range;
    let Some(mut n) = root.descendant_for_byte_range(from, to) else { return range };
    while let Some(p) = n.parent() {
        let alone = p.named_children(&mut p.walk()).filter(|k| !k.kind().contains("comment")).count() == 1;
        if p.parent().is_none() || p.start_byte() < a || p.end_byte() > b || !alone {
            break;
        }
        (from, to) = (from.min(p.start_byte()), to.max(p.end_byte()));
        n = p;
    }
    (from, to)
}

/// Whether `old[lo..hi]` holds only whitespace and comments that lie within the lines `a..b`, by
/// the parse `root`: a comment that runs past them would be cut in two.
fn comments_only(root: tree_sitter::Node, old: &str, (lo, hi): (usize, usize), (a, b): (usize, usize)) -> bool {
    let mut code: Vec<bool> = old.as_bytes()[lo..hi].iter().map(|c| !c.is_ascii_whitespace()).collect();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.end_byte() <= lo || n.start_byte() >= hi {
            continue;
        }
        if n.kind().contains("comment") && n.start_byte() >= a && n.end_byte() <= b {
            code[n.start_byte().max(lo) - lo..n.end_byte().min(hi) - lo].fill(false);
            continue;
        }
        stack.extend(n.children(&mut n.walk()));
    }
    !code.contains(&true)
}

/// The answer to `edit_symbol`.
pub fn run(s: &mut Searcher, symbol: &str, path: Option<&str>, new_text: &str, apply: bool, given: Option<&str>) -> Result<Answer> {
    let read = Op::Read { path: path.map(str::to_string), symbol: Some(symbol.to_string()), start: 1, end: None, history: 0 };
    let Answer::Definitions(found, ..) = crate::ops::run_in(s, &read)? else { bail!("no definition matches {}", crate::fmt::echo(symbol)) };
    if found.len() != 1 {
        let at: Vec<String> = found.iter().map(|(r, ..)| format!("{}:{}-{}", crate::fmt::shown(&r.path), r.start, r.end)).collect();
        bail!("{} names {} definitions ({}): pass path, or one location, to pick the one to edit", crate::fmt::echo(symbol), at.len(), at.join(", "));
    }
    let row = &found[0].0;
    let rel = row.path.clone();
    if let Some(top) = guarded(&s.ix.root, &rel) {
        bail!("{rel} is under {top}: edit_symbol never writes there");
    }
    let one = crate::rewrite::open_one(&s.ix.root, &rel)?;
    let now = content_hash(&one.read);
    if apply {
        match given {
            None => bail!("apply needs hash, the file's hash as a dry run shows it\nhash: {now}"),
            Some(h) if h != now => bail!("{rel} changed since that hash was taken: a dry run shows the edit against it as it is now\nhash: {now}"),
            Some(_) => {}
        }
    }
    let old = std::str::from_utf8(&one.read).map_err(|_| anyhow!("{rel} is not UTF-8 text: edit_symbol writes UTF-8 files only"))?.to_string();
    let spec = crate::parse::spec_for(&rel, &old)
        .ok_or_else(|| anyhow!("{rel} has no grammar to check an edit with: edit_symbol edits the languages tree-sitter parses"))?;
    let before = defs(&rel, &old);
    let Some(target) = before.iter().find(|d| d.start == row.start && d.qualname == row.qualname).cloned() else {
        bail!("{rel} changed since it was indexed: call again");
    };
    // the definition's lines, whole, and the new text in the file's line endings
    let starts: Vec<usize> = std::iter::once(0).chain(old.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let line_at = |l: i64| starts.get((l - 1) as usize).copied().unwrap_or(old.len());
    let (a, b) = (line_at(target.start), line_at(target.end + 1));
    // a byte-order mark opens the file, not its first definition
    let a = if a == 0 && old.starts_with('\u{feff}') { '\u{feff}'.len_utf8() } else { a };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&(spec.language)())?;
    let Some(t0) = parser.parse(&old, None) else { bail!("{rel} did not parse") };
    // whole lines are replaced, so other code on the first or the last of them would go with them;
    // a comment there is the definition's, as read_code shows its lines
    if let Some((from, to)) = target.bytes.map(|r| wrapped(t0.root_node(), r, (a, b))) {
        let clear = |lo: usize, hi: usize| comments_only(t0.root_node(), &old, (lo, hi), (a, b));
        let before = !clear(a, from.max(a));
        if before || !clear(to.min(b), b) {
            let line = if before { target.start } else { target.end };
            bail!("{} shares a line with other code that is not a comment ({rel}:{line}): edit_symbol replaces whole lines; nothing written", target.qualname);
        }
    }
    // the line ending of the lines it replaces, whatever another line of the file has; a last
    // line with no ending takes the line before it
    let first = old[a..b].split_inclusive('\n').next().unwrap_or("");
    let crlf = if first.ends_with('\n') { first.ends_with("\r\n") } else { old[..a].ends_with("\r\n") };
    let eol = if crlf { "\r\n" } else { "\n" };
    let mut repl = new_text.strip_prefix('\u{feff}').unwrap_or(new_text).replace("\r\n", "\n");
    if eol == "\r\n" {
        repl = repl.replace('\n', "\r\n");
    }
    if old[a..b].ends_with('\n') && !repl.ends_with('\n') {
        repl.push_str(eol);
    }
    let new = format!("{}{repl}{}", &old[..a], &old[b..]);
    let span = (a, b, a, a + repl.len());
    // the new file parses as well as the old one did
    let Some(t1) = parser.parse(&new, None) else { bail!("{rel} did not parse") };
    let in_span = crate::rewrite::broken(t1.root_node(), span.2, span.3) && !crate::rewrite::broken(t0.root_node(), a, b);
    let (e0, e1) = (errors(t0.root_node()), errors(t1.root_node()));
    if in_span || e1.0 > e0.0 {
        bail!("the new text does not parse: an error at line {} of the edited file; nothing written", e1.1);
    }
    // one definition of the same name and kind where the old one began, the rest as they were
    let new_end = target.start + (repl.matches('\n').count() as i64).max(1) - 1;
    let after = defs(&rel, &new);
    let outside = |d: &Def, (lo, hi): (i64, i64)| d.start < lo || d.start > hi;
    let kept: Vec<(&str, &str)> = before.iter().filter(|d| outside(d, (target.start, target.end))).map(|d| (d.qualname.as_str(), d.kind)).collect();
    let now_kept: Vec<(&str, &str)> = after.iter().filter(|d| outside(d, (target.start, new_end))).map(|d| (d.qualname.as_str(), d.kind)).collect();
    let Some(edited) = after.iter().find(|d| d.start == target.start && d.qualname == target.qualname && d.kind == target.kind) else {
        bail!(
            "the new text does not define {} ({}) where it began, at line {}: edit_symbol replaces a definition and never renames it; nothing written",
            target.qualname,
            target.kind,
            target.start
        );
    };
    let beside: Vec<String> = after
        .iter()
        .filter(|d| !outside(d, (target.start, new_end)) && *d != edited && !(d.start > edited.start && d.end <= edited.end))
        .map(|d| format!("{} ({}) at line {}", d.qualname, d.kind, d.start))
        .collect();
    if !beside.is_empty() {
        bail!("the new text defines more than {}: {}; edit_symbol replaces one definition; nothing written", target.qualname, beside.join(", "));
    }
    if kept != now_kept {
        let gone: Vec<&str> = kept.iter().filter(|k| !now_kept.contains(k)).map(|k| k.0).collect();
        let what = if gone.is_empty() { "their order".to_string() } else { gone.join(", ") };
        bail!("the edit changes the definitions around {} (gone or moved: {what}); nothing written", target.qualname);
    }
    let plan = one.plan(new.clone().into_bytes(), span);
    let diff = plan.diff();
    if apply {
        if let Some(why) = plan.write().into_iter().flatten().next() {
            bail!("{why}");
        }
    }
    let hash = if apply { content_hash(new.as_bytes()) } else { now };
    Ok(Answer::Edit(Box::new(Edited { path: rel, old: (target.start, target.end), new: (edited.start, edited.end), applied: apply, hash, diff })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_and_scrutate_are_refused_under_any_spelling_and_an_edit_never_crosses_the_socket() {
        let root = std::env::temp_dir().join(format!("scrutate_edit_guard_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".scrutate")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        assert_eq!(guarded(&root, ".git/hooks/x.py"), Some(".git"));
        assert_eq!(guarded(&root, "src/.scrutate/x.py"), Some(".scrutate"));
        assert_eq!(guarded(&root, "src/a.py"), None);
        // a nested repository's git directory, under the spellings APFS and NTFS take for it
        for p in ["vendor/lib/.GIT/hooks/x.py", "a/.Git./x.py", "a/git~1/x.py", "a/.g\u{200c}it/x.py", "a/.SCRUTATE/x.py"] {
            assert!(guarded(&root, p).is_some(), "{p}");
        }
        assert_eq!(guarded(&root, "a/.github/x.py"), None);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join(".scrutate"), root.join("meta")).unwrap();
            assert_eq!(guarded(&root, "meta/x.py"), Some(".scrutate"));
        }
        // on a file system that ignores case, `.GIT` is `.git`
        if root.join(".GIT").exists() {
            assert_eq!(guarded(&root, ".GIT/x.py"), Some(".git"));
        }
        let _ = std::fs::remove_dir_all(&root);
        let op = Op::Edit { symbol: "f".into(), path: None, new_text: String::new(), apply: true, hash: None };
        assert!(serde_json::to_string(&op).is_err());
        assert!(serde_json::from_str::<Op>(r#"{"Edit": {"symbol": "f", "path": null, "new_text": "", "apply": true, "hash": null}}"#).is_err());
    }
}
