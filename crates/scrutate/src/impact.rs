//! What a change touches: the definitions a git diff changes, the callers that reach them with
//! their distances, and the test files to run. The diff comes from the `git` program; the
//! definitions of the tree as it is from the index, those of the base from a parse of each file's
//! base version; the callers from `Names`, as `callers` gives them.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::findings::Finding;
use crate::fmt::{echo, shown};
use crate::git::{Entry, Git, New};
use crate::names::Names;
use crate::search::{Hit, Searcher};

/// Steps of callers followed by default: past three, the callers of a much used function grow
/// toward most of the repository and say little about what to test.
pub const DEPTH: usize = 3;
pub const MAX_DEPTH: usize = 10;
/// Callers one step asks for, as `callers --limit 500`.
pub const STEP_CALLERS: usize = 500;
/// Callers one answer lists at most; the walk stops there.
pub const MAX_CALLERS: usize = 2000;

/// A definition the diff changed. `status`: `added`, `edited` or `deleted` (then named, with
/// its lines, as the base has it). Kind `file`: lines outside every definition, with no window.
pub struct Changed {
    pub hit: Hit,
    pub status: &'static str,
}

/// A definition that calls a changed one, `distance` steps away; `via` is the location of what it calls.
pub struct Caller {
    pub hit: Hit,
    pub distance: usize,
    pub via: String,
    /// the resolver could not tell whether it calls that definition or another of its name
    pub undecided: bool,
    /// it calls a definition the change deleted, by a name no other definition answers
    pub calls_deleted: bool,
}

/// A test file to run, and the symbol in it that reached a change (none: the diff changed the file).
pub struct Test {
    pub path: String,
    pub distance: usize,
    pub via: Option<Hit>,
}

pub struct Impact {
    /// the revision as given (`HEAD` by default) and the commit it names
    pub against: String,
    pub base: String,
    pub staged: bool,
    pub depth: usize,
    pub changed: Vec<Changed>,
    pub callers: Vec<Caller>,
    pub tests: Vec<Test>,
    /// (path, note): a binary file, a submodule, a file the index leaves out, a rename
    pub notes: Vec<(String, Note)>,
    pub stopped: bool,
    /// the head commit, when the change is between two commits
    pub head: Option<String>,
    /// what a reviewer would want named: new duplicates, then changes no test reaches
    pub findings: Vec<Finding>,
    /// more duplicate groups were new than `findings::MAX_DUPLICATES`
    pub more_findings: bool,
    /// (path, bytes) of the base copy of each changed file the index would hold
    pub(crate) base_copies: Vec<(String, Vec<u8>)>,
    /// the path each changed file has after the change
    pub(crate) head_paths: Vec<String>,
    /// every call the walk met
    pub(crate) edges: Vec<Edge>,
}

/// What a changed file's line under `notes` says.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub enum Note {
    /// why it gives no symbol
    Said(&'static str),
    /// the path it was renamed from, which the pull request's author wrote
    Renamed(String),
}

impl Note {
    /// The note, a path in it written by `path` (in Markdown, as a code span).
    pub fn text(&self, path: impl Fn(&str) -> String) -> String {
        match self {
            Note::Said(s) => s.to_string(),
            Note::Renamed(old) => format!("renamed from {}", path(old)),
        }
    }
}

/// A call: the caller's chunk id and the location of the definition it calls.
pub(crate) type Edge = (i64, String);

/// What the working tree (with `staged`, the staged changes) changes against `base` (`HEAD`, a
/// revision, or `A...B` for the merge base of the two), on the index as it is.
pub fn impact(s: &Searcher, base: Option<&str>, staged: bool, depth: usize) -> Result<Impact> {
    compare(s, base.unwrap_or("HEAD"), if staged { New::Staged } else { New::Tree }, depth)
}

/// What commit `head` changes against `base` (a revision or `A...B`), read from git's copies and
/// named where the index, the working tree's, holds each definition.
pub fn between(s: &Searcher, base: &str, head: &str, depth: usize) -> Result<Impact> {
    compare(s, base, New::Commit(head.to_string()), depth)
}

fn compare(s: &Searcher, against: &str, new: New, depth: usize) -> Result<Impact> {
    let git = Git::new(&s.ix.root)?;
    let against = against.to_string();
    let base = git.base(&against)?;
    let new = match new {
        New::Commit(rev) => New::Commit(git.commit(&rev)?),
        other => other,
    };
    let entries = git.diff(&base, &new)?;
    let (staged, head) = match new {
        New::Tree => (false, None),
        New::Staged => (true, None),
        New::Commit(c) => (false, Some(c)),
    };
    let from_blobs = staged || head.is_some();
    let mut b = Builder {
        s,
        from_blobs,
        changed: Vec::new(),
        notes: Vec::new(),
        seen: HashSet::new(),
        files: Vec::new(),
        base_copies: Vec::new(),
        head_paths: Vec::new(),
    };
    let blobs = git.blobs(&entries.iter().flat_map(|e| b.blobs_wanted(e)).collect::<BTreeSet<_>>())?;
    for e in &entries {
        b.entry(e, &blobs)?;
    }
    let Builder { mut changed, mut notes, files, base_copies, head_paths, .. } = b;
    changed.sort_by(|a, b| {
        (&a.hit.path, a.hit.start, a.hit.end, &a.hit.kind, &a.hit.qualname).cmp(&(&b.hit.path, b.hit.start, b.hit.end, &b.hit.kind, &b.hit.qualname))
    });
    notes.sort();
    let (callers, edges, stopped) = walk(s, &changed, depth)?;
    let tests = test_files(&changed, &callers, &files);
    let mut i = Impact {
        against,
        base,
        staged,
        depth,
        changed,
        callers,
        tests,
        notes,
        stopped,
        head,
        findings: Vec::new(),
        more_findings: false,
        base_copies,
        head_paths,
        edges,
    };
    (i.findings, i.more_findings) = crate::findings::find(s, &i)?;
    Ok(i)
}

/// Kinds whose callers are not asked for: lines outside every definition, and a module.
fn walkable(h: &Hit) -> bool {
    h.id != 0 && !matches!(h.kind.as_str(), "window" | "module" | "file")
}

/// Callers breadth first, each at the smallest distance it is reached by, in (distance, path, line)
/// order, and every call met on the way.
fn walk(s: &Searcher, changed: &[Changed], depth: usize) -> Result<(Vec<Caller>, Vec<Edge>, bool)> {
    let mut seen: HashSet<i64> = changed.iter().map(|c| c.hit.id).filter(|id| *id != 0).collect();
    let mut frontier: Vec<Hit> = changed.iter().map(|c| c.hit.clone()).filter(walkable).collect();
    // a deleted definition has no index entry to ask `callers` about: its callers are found by its name
    let gone: Vec<&Hit> =
        changed.iter().filter(|c| c.status == "deleted" && !matches!(c.hit.kind.as_str(), "window" | "module" | "file")).map(|c| &c.hit).collect();
    let (mut callers, mut edges) = (Vec::new(), Vec::new());
    let mut names = Names::new(s)?;
    for distance in 1..=depth {
        let mut next = Vec::new();
        let deleted = if distance == 1 { gone.len() } else { 0 };
        for k in 0..deleted + frontier.len() {
            if s.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("cancelled");
            }
            let (target, decided, hits) = match k.checked_sub(deleted) {
                None => {
                    let hits = names.callers_of_deleted(gone[k], STEP_CALLERS)?;
                    (gone[k], hits.len(), hits)
                }
                Some(j) => {
                    let (hits, undecided) = names.callers(&frontier[j].loc(), STEP_CALLERS)?;
                    (&frontier[j], hits.len() - undecided, hits)
                }
            };
            for (i, h) in hits.into_iter().enumerate() {
                edges.push((h.id, target.loc()));
                if !seen.insert(h.id) {
                    continue;
                }
                if callers.len() >= MAX_CALLERS {
                    return Ok((callers, edges, true));
                }
                if walkable(&h) {
                    next.push(h.clone());
                }
                callers.push(Caller { hit: h, distance, via: target.loc(), undecided: i >= decided, calls_deleted: k < deleted });
            }
        }
        next.sort_by(|a, b| (&a.path, a.start, a.end).cmp(&(&b.path, b.start, b.end)));
        frontier = next;
    }
    callers.sort_by(|a, b| (a.distance, &a.hit.path, a.hit.start, a.hit.end).cmp(&(b.distance, &b.hit.path, b.hit.start, b.hit.end)));
    Ok((callers, edges, false))
}

/// The test files (`is_test_path`, the index's test flag) that hold a changed symbol or a caller,
/// and those the diff changed, each once at its smallest distance; a file the change deletes is
/// none.
fn test_files(changed: &[Changed], callers: &[Caller], files: &[String]) -> Vec<Test> {
    let mut out: BTreeMap<String, Test> = BTreeMap::new();
    let mut offer = |path: &str, distance: usize, via: Option<&Hit>| {
        if !crate::text::is_test_path(path) {
            return;
        }
        let t = out.entry(path.to_string()).or_insert(Test { path: path.to_string(), distance, via: via.cloned() });
        if distance < t.distance {
            *t = Test { path: path.to_string(), distance, via: via.cloned() };
        }
    };
    for c in changed.iter().filter(|c| c.status != "deleted") {
        offer(&c.hit.path, 0, (c.hit.kind != "file").then_some(&c.hit));
    }
    for c in callers {
        offer(&c.hit.path, c.distance, Some(&c.hit));
    }
    for f in files {
        offer(f, 0, None);
    }
    let mut tests: Vec<Test> = out.into_values().collect();
    tests.sort_by(|a, b| (a.distance, &a.path).cmp(&(b.distance, &b.path)));
    tests
}

/// A definition of one side of a file.
#[derive(Clone)]
struct Def {
    kind: String,
    name: String,
    qualname: String,
    start: i64,
    end: i64,
    /// the index's chunk id; 0 for a definition parsed from git's copy
    id: i64,
}

impl Def {
    fn hit(&self, path: &str) -> Hit {
        Hit {
            id: self.id,
            path: path.to_string(),
            start: self.start,
            end: self.end,
            name: self.name.clone(),
            qualname: self.qualname.clone(),
            kind: self.kind.clone(),
            ..Hit::default()
        }
    }

    fn same(&self, other: &Def) -> bool {
        self.kind == other.kind && self.qualname == other.qualname
    }
}

/// The definitions of `src` as the index would hold them, windows aside.
fn parsed(path: &str, src: &[u8]) -> Vec<Def> {
    if src.len() as u64 > crate::index::MAX_FILE_BYTES || !crate::parse::is_indexable(path) {
        return Vec::new();
    }
    let src = String::from_utf8_lossy(src);
    crate::parse::parse_file(path, &src)
        .chunks
        .into_iter()
        .filter(|c| c.kind != "window")
        .map(|c| Def { kind: c.kind.to_string(), name: c.name, qualname: c.qualname, start: c.start, end: c.end, id: 0 })
        .collect()
}

/// Of `defs`, the innermost one around each line of `ranges` (first and last line), and the
/// lines no definition holds.
fn touched(defs: &[Def], ranges: &[(i64, i64)]) -> (BTreeSet<usize>, Vec<i64>) {
    let top = defs.iter().map(|d| d.end).max().unwrap_or(0).max(0) as usize;
    let mut owner = vec![usize::MAX; top + 1];
    let mut order: Vec<usize> = (0..defs.len()).collect();
    // larger first, so an inner definition paints over the one around it
    order.sort_by_key(|&i| std::cmp::Reverse(defs[i].end - defs[i].start));
    for i in order {
        for l in defs[i].start.max(1)..=defs[i].end {
            owner[l as usize] = i;
        }
    }
    let (mut hit, mut outside) = (BTreeSet::new(), Vec::new());
    for &(a, b) in ranges {
        for l in a.max(1)..=b {
            match owner.get(l as usize) {
                Some(&i) if i != usize::MAX => {
                    hit.insert(i);
                }
                _ => outside.push(l),
            }
        }
    }
    (hit, outside)
}

/// The one of `defs` that is `d` (same kind and qualified name), nearest its lines.
fn nearest<'a>(defs: &'a [Def], d: &Def) -> Option<&'a Def> {
    defs.iter().filter(|n| n.same(d)).min_by_key(|n| (n.start - d.start).abs())
}

struct Builder<'a> {
    s: &'a Searcher,
    /// the changed side is read from git's copies (staged, or a commit), not the working tree
    from_blobs: bool,
    changed: Vec<Changed>,
    notes: Vec<(String, Note)>,
    /// chunk ids, and (path, start, kind, qualname) of what has no id, already listed
    seen: HashSet<(i64, String, i64, String, String)>,
    /// every path the diff changed, as it is after the change: a deleted file has none
    files: Vec<String>,
    base_copies: Vec<(String, Vec<u8>)>,
    head_paths: Vec<String>,
}

impl Builder<'_> {
    fn blobs_wanted(&self, e: &Entry) -> Vec<String> {
        if e.submodule || e.binary {
            return Vec::new();
        }
        let mut v = Vec::new();
        if e.old.is_some() {
            v.push(e.old_blob.clone());
        }
        if self.from_blobs && e.new.is_some() && !e.untracked {
            v.push(e.new_blob.clone());
        }
        v
    }

    fn push(&mut self, hit: Hit, status: &'static str) {
        let key = if hit.id != 0 {
            (hit.id, String::new(), 0, String::new(), String::new())
        } else {
            (0, hit.path.clone(), hit.start, hit.kind.clone(), hit.qualname.clone())
        };
        if self.seen.insert(key) {
            self.changed.push(Changed { hit, status });
        }
    }

    fn entry(&mut self, e: &Entry, blobs: &BTreeMap<String, Vec<u8>>) -> Result<()> {
        let path = e.new.as_deref().or(e.old.as_deref()).unwrap_or_default().to_string();
        self.files.extend(e.new.clone());
        self.head_paths.extend(e.new.clone());
        if let (Some(old), Some(new)) = (&e.old, &e.new) {
            if old != new {
                self.notes.push((new.clone(), Note::Renamed(old.clone())));
            }
        }
        if e.submodule {
            self.notes.push((path, Note::Said("a submodule: its commits are not read")));
            return Ok(());
        }
        if e.binary {
            self.notes.push((path, Note::Said("a binary file")));
            return Ok(());
        }
        if let Some(new) = e.new.as_deref().filter(|n| !self.s.ix.has_file(n)) {
            self.notes.push((new.to_string(), Note::Said("not in the index: it has no definitions to name")));
            return Ok(());
        }
        let blob = |sha: &str| blobs.get(sha).map(Vec::as_slice).unwrap_or_default();
        if let Some(old) = &e.old {
            self.base_copies.push((old.clone(), blob(&e.old_blob).to_vec()));
        }
        let old_defs = match &e.old {
            Some(old) => parsed(old, blob(&e.old_blob)),
            None => Vec::new(),
        };
        let added = e.old.is_none();
        let ix = &self.s.ix;
        // the tree as it is: the index's definitions; the staged copy: a parse of it, matched to the index's
        let (new_defs, indexed): (Vec<Def>, Vec<Def>) = match &e.new {
            Some(new) => {
                let rows: Vec<Def> = ix
                    .outline(new)?
                    .into_iter()
                    .map(|r| Def { kind: r.kind, name: r.name, qualname: r.qualname, start: r.start, end: r.end, id: r.id })
                    .collect();
                if self.from_blobs && !e.untracked {
                    (parsed(new, blob(&e.new_blob)), rows)
                } else {
                    (rows.clone(), rows)
                }
            }
            None => (Vec::new(), Vec::new()),
        };
        let new_ranges = if e.untracked { vec![(1, line_count(&ix.root.join(&path)))] } else { e.new_lines.clone() };
        let (new_hit, new_outside) = touched(&new_defs, &new_ranges);
        let (old_hit, old_outside) = touched(&old_defs, &e.old_lines);
        let new_path = e.new.clone().unwrap_or_default();
        // a staged definition is named where the index holds it now, when it does
        let current = |d: &Def| -> Hit {
            match nearest(&indexed, d).filter(|_| d.id == 0) {
                Some(i) => i.hit(&new_path),
                None => d.hit(&new_path),
            }
        };
        for i in new_hit {
            let d = &new_defs[i];
            let status = if old_defs.iter().any(|o| o.same(d)) { "edited" } else { "added" };
            let h = current(d);
            self.push(h, status);
        }
        for i in old_hit {
            let d = &old_defs[i];
            match nearest(&new_defs, d) {
                Some(n) => {
                    let h = current(n);
                    self.push(h, "edited");
                }
                None => self.push(d.hit(e.old.as_deref().unwrap_or_default()), "deleted"),
            }
        }
        // lines outside every definition: the window that holds them, as callers names such a caller
        let mut bare = !old_outside.is_empty();
        let from_index = e.new.is_some() && (!self.from_blobs || e.untracked);
        let mut after = 0;
        for l in new_outside {
            if !from_index {
                bare = true;
                break;
            }
            if l <= after {
                continue;
            }
            match ix.window_at(&path, l)? {
                Some(w) => {
                    after = w.end;
                    self.push(Hit::from_row(&w, false), if added { "added" } else { "edited" });
                }
                None => bare = true,
            }
        }
        let named = self.changed.iter().any(|c| c.hit.path == path && matches!(c.hit.kind.as_str(), "window" | "module"));
        if bare && !named {
            let status = if e.new.is_none() {
                "deleted"
            } else if added {
                "added"
            } else {
                "edited"
            };
            self.push(Hit { path, kind: "file".into(), ..Hit::default() }, status);
        }
        Ok(())
    }
}

/// Lines of a file, the last one counted without its line break.
fn line_count(p: &Path) -> i64 {
    let b = std::fs::read(p).unwrap_or_default();
    let n = b.iter().filter(|&&c| c == b'\n').count();
    (n + usize::from(b.last().is_some_and(|&c| c != b'\n'))) as i64
}

impl Impact {
    /// The first line: what was compared with what.
    pub fn head(&self) -> String {
        let short = self.base.get(..12).unwrap_or(&self.base);
        format!("{} against {} ({short})", if self.staged { "staged changes" } else { "working tree" }, echo(&self.against))
    }

    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.notes.is_empty() && self.tests.is_empty()
    }

    /// The repository files whose indexed definitions this answer shows.
    pub fn cited(&self) -> Vec<&str> {
        let changed = self.changed.iter().filter(|c| c.status != "deleted").map(|c| c.hit.path.as_str());
        let related = self.findings.iter().flat_map(|f| match &f.what {
            crate::findings::What::Duplicate(g) => g.members.iter().map(|m| m.0.as_str()).collect(),
            crate::findings::What::Untested { .. } => Vec::new(),
        });
        changed.chain(self.callers.iter().map(|c| c.hit.path.as_str())).chain(related).collect()
    }

    /// One line per section head and per item, as the text pages them.
    pub fn lines(&self) -> Vec<String> {
        let symbol = |h: &Hit| {
            if h.kind == "file" {
                format!("{}  [file]", shown(&h.path))
            } else {
                format!("{}  [{}] {}", crate::fmt::loc(h), h.kind, shown(if h.qualname.is_empty() { &h.name } else { &h.qualname }))
            }
        };
        let mut out = vec![format!("changed: {}", self.changed.len())];
        out.extend(self.changed.iter().map(|c| {
            let at = if c.status == "deleted" && c.hit.kind != "file" { ", lines of the base" } else { "" };
            format!("{}  ({}{at})", symbol(&c.hit), c.status)
        }));
        out.push(format!("callers: {} (depth {})", self.callers.len(), self.depth));
        out.extend(self.callers.iter().map(|c| {
            let mark = if c.calls_deleted {
                "  (calls a deleted definition)"
            } else if c.undecided {
                "  (undecided)"
            } else {
                ""
            };
            format!("{:>2}  {}{mark}", c.distance, symbol(&c.hit))
        }));
        out.push(format!("tests: {}", self.tests.len()));
        out.extend(self.tests.iter().map(|t| match &t.via {
            Some(h) => format!("{}  (distance {}: {})", shown(&t.path), t.distance, symbol(h)),
            None => format!("{}  (changed)", shown(&t.path)),
        }));
        if !self.findings.is_empty() {
            out.push(format!("findings: {}", self.findings.len()));
            for (k, f) in self.findings.iter().enumerate() {
                out.push(format!("{}:{}-{}  [{}, {}] {}", shown(&f.path), f.start, f.end, f.kind(), f.severity(), shown(&f.text())));
                if self.more_findings && k + 1 == self.duplicate_findings() {
                    out.push(self.more_line());
                }
            }
        }
        if !self.notes.is_empty() {
            out.push(format!("notes: {}", self.notes.len()));
            out.extend(self.notes.iter().map(|(p, n)| format!("{}  {}", shown(p), n.text(|p| shown(p).into_owned()))));
        }
        out
    }

    /// The duplicate findings, which come first in the list.
    pub fn duplicate_findings(&self) -> usize {
        self.findings.iter().take_while(|f| matches!(f.what, crate::findings::What::Duplicate(_))).count()
    }

    /// The line after the duplicate findings that more duplicate groups were new than they are.
    pub fn more_line(&self) -> String {
        format!("(more than {} duplicate groups were new: these are the largest)", crate::findings::MAX_DUPLICATES)
    }

    pub fn stop_line(&self) -> String {
        if self.stopped {
            format!("(the walk stopped at {MAX_CALLERS} callers: pass a smaller depth)")
        } else {
            String::new()
        }
    }

    pub fn json(&self) -> Value {
        let symbol = |h: &Hit| {
            if h.kind == "file" {
                json!({"id": h.path, "path": h.path, "start": null, "end": null, "kind": "file", "qualname": ""})
            } else {
                json!({"id": h.loc(), "path": h.path, "start": h.start, "end": h.end, "kind": h.kind, "qualname": if h.qualname.is_empty() { &h.name } else { &h.qualname }})
            }
        };
        let with = |mut v: Value, k: &str, x: Value| {
            v[k] = x;
            v
        };
        json!({
            "against": self.against,
            "base": self.base,
            "staged": self.staged,
            "depth": self.depth,
            "changed": self.changed.iter().map(|c| with(symbol(&c.hit), "status", json!(c.status))).collect::<Vec<_>>(),
            "callers": self.callers.iter().map(|c| {
                let v = with(symbol(&c.hit), "distance", json!(c.distance));
                with(with(with(v, "via", json!(c.via)), "undecided", json!(c.undecided)), "calls_deleted", json!(c.calls_deleted))
            }).collect::<Vec<_>>(),
            "tests": self.tests.iter().map(|t| json!({"path": t.path, "distance": t.distance, "via": t.via.as_ref().map(symbol)})).collect::<Vec<_>>(),
            "notes": self.notes.iter().map(|(p, n)| json!({"path": p, "note": n.text(|p| shown(p).into_owned())})).collect::<Vec<_>>(),
            "findings": self.findings.iter().map(|f| json!({
                "kind": f.kind(), "severity": f.severity(), "path": f.path, "start": f.start, "end": f.end, "qualname": f.name, "text": f.text(),
                "related": f.related().iter().map(|(path, name, start, end)| json!({"path": path, "qualname": name, "start": start, "end": end})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "more_findings": self.more_findings,
            "stopped": self.stopped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_belongs_to_the_innermost_definition_around_it() {
        let d = |kind: &str, start, end| Def { kind: kind.into(), name: kind.into(), qualname: kind.into(), start, end, id: 0 };
        let defs = [d("class", 1, 10), d("method", 3, 5)];
        let (hit, outside) = touched(&defs, &[(4, 4), (8, 8), (12, 13)]);
        assert_eq!(hit.into_iter().collect::<Vec<_>>(), [0, 1]);
        assert_eq!(outside, [12, 13]);
    }
}
