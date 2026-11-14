//! Keeps every store under `.scrutate/` current: the code index (`index.rs`), the text index
//! (`textindex.rs`), the `tsquery` cache (`tscache.rs`) and the answers `callers` and `callees`
//! keep (`answers.rs`). A caller names what it is doing (a
//! `Writer`, or a read through `Repo`) and what changed (`Changes`); this module decides which
//! stores that touches, when it looks for changes, and what happens when a store is damaged.
//!
//! When each operation looks for changes, and what it keeps current:
//!
//! | Operation | Looks for changes | Keeps current |
//! |---|---|---|
//! | `index` | every file; waits for another writer, up to 10 minutes or a cancel | code index, text index (built when missing) |
//! | `index --no-text` | every file; waits | code index |
//! | `index --text` | every file; waits | text index (built when missing) |
//! | `grep --index --update` | every file; waits | text index (built when missing) |
//! | `grep --index` | does not look: answers from the last text index build | nothing |
//! | `watch`, `serve --watch` | the files a burst names, else every file; waits | code index, text index when one exists |
//! | `reindex` (MCP, `batch`), bindings `index()` | every file; waits | code index, text index when one exists |
//! | an index command, `batch` | every file before the first operation; never waits | code index |
//! | an MCP tool call, a bindings call, a language server request | every file, at most once per refresh interval of the session; never waits | code index |
//! | an index command a running `serve` answers | every file, in that session, before the operation; never waits (else the command does the work itself) | code index |
//! | any operation on `Repo` | the files its answer cites and its arguments name, one `stat` each | code index |
//! | `serve` and `lsp` start, an `lsp` save | every file; waits | code index |
//! | `tsquery` | its own walk, one `stat` per file against the code index's record | `tsquery` cache |
//! | `callers`, `callees` | as any index command, then one `stat` per file the kept run read | its answers |
//!
//! Recovery: a store found damaged is built again from the sources, once, by the process that
//! found it, which says so on stderr; an operation that met the damage then runs again. The code
//! index's files are deleted first (`Repo::recover`); the text index is rebuilt in place, its
//! manifest swapped last. `grep --index` without `--update` reports a damaged text index instead,
//! since it promises not to walk the tree.
//!
//! Locking: each store has its own writer lock (the code index a SQLite write transaction, the
//! text index `.scrutate/text/lock`, the `tsquery` cache and the kept answers none: their files
//! are written by readers and replaced by rename),
//! and each commits in one step (a transaction; a manifest rename; a file rename), so a reader of
//! one store sees its last commit and never half of one. Writers that keep both indexes take the
//! two locks one after the other, code index first, and never hold both; no answer reads both.
//! The artifact store is a table of the code index and commits with it; the main checkout's
//! index a linked work tree reads is read in read transactions only.
//! Inside one process the repositories of one root that `Repo::another` opened share one more
//! lock, held by every write any of them makes (an update, the scan before a read, the update of
//! the files an answer cites, a rebuild), so they write one at a time and a rebuild one of them
//! makes is not made again by the next; their reads take no lock and run side by side.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;

use crate::files::{self, Found};
use crate::index::{self, UpdateStats};
use crate::models::ModelSet;
use crate::ops;
use crate::search::Searcher;
use crate::textindex::{self, TextStats};

/// What changed since the stores were last brought up to date.
#[derive(Clone, Debug, PartialEq)]
pub enum Changes {
    /// any file of the tree: walk it
    All,
    /// these files alone (a watch burst that named files only)
    Named(Named),
}

/// Files relative to the root, as each index's walk lists them in their directories; a file that
/// is gone is in both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Named {
    pub code: Vec<String>,
    pub text: Vec<String>,
}

/// An operation that writes the stores (the rows of the table above that keep more than the code
/// index before a read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writer {
    /// `scrutate index`
    Index,
    /// `scrutate index --no-text`
    IndexCode,
    /// `scrutate index --text`
    IndexText,
    /// `grep --index --update`
    GrepUpdate,
    /// a watch burst, `scrutate watch` or `serve --watch`
    Watch,
    /// MCP and `batch` `reindex`, bindings `index()`
    Reindex,
}

impl Writer {
    fn code(self) -> bool {
        !matches!(self, Writer::IndexText | Writer::GrepUpdate)
    }

    /// The one place that decides whether the text index is kept: `index` (unless `--no-text`) and
    /// `grep --index --update` build it, the other writers keep one that exists and never make one.
    fn text(self, root: &Path) -> bool {
        match self {
            Writer::IndexCode => false,
            Writer::Index | Writer::IndexText | Writer::GrepUpdate => true,
            Writer::Watch | Writer::Reindex => textindex::exists(root),
        }
    }

    /// Progress lines on a terminal: the `index` command's alone.
    fn progress(self) -> bool {
        matches!(self, Writer::Index | Writer::IndexCode | Writer::IndexText) && std::io::IsTerminal::is_terminal(&std::io::stderr())
    }
}

/// What an update did, store by store; None for a store it did not keep.
#[derive(Debug, Default)]
pub struct Updated {
    pub code: Option<UpdateStats>,
    pub text: Option<TextStats>,
}

/// Bring the stores `w` keeps up to date with every file of the tree at `root`. The code index is
/// opened only when `w` keeps it; with `force` every file is read again, and a code index of a
/// newer scrutate that this one cannot read is replaced.
pub fn update(root: &Path, w: Writer, force: bool) -> Result<Updated> {
    if w.code() {
        let mut repo = Repo::open_to_update(root, force)?;
        return repo.update_stores(w, &Changes::All, force);
    }
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    Ok(Updated { code: None, text: Some(update_text(&root, &Changes::All, force, w.progress(), None)?) })
}

/// The text index brought up to date, from what a look found when one did; a damaged one is
/// built again.
fn update_text(root: &Path, changes: &Changes, force: bool, progress: bool, found: Option<&[Found]>) -> Result<TextStats> {
    let r = match (changes, found) {
        (Changes::Named(n), _) if !force => textindex::update_paths(root, &n.text),
        (_, Some(found)) => textindex::update_found(root, force, progress, found),
        _ => textindex::update(root, force, progress),
    };
    match r {
        Err(e) if e.is::<textindex::Damaged>() => {
            eprintln!("[scrutate] text index damaged ({e:#}); rebuilding");
            textindex::update(root, true, progress)
        }
        r => r,
    }
}

/// An open repository: the searcher on its code index, which it derefs to, and the stores kept
/// current around it.
pub struct Repo {
    /// `None` while a damaged index is being replaced, and after a rebuild that could not reopen it
    searcher: Option<Searcher>,
    root: PathBuf,
    /// the last refresh failed and stderr said why
    stale: bool,
    /// an update of the index went through since `take_updated` was last asked
    updated: bool,
    models: Arc<ModelSet>,
    /// held by every write this repository makes; shared with the ones `another` opened
    writer: Arc<Mutex<()>>,
    /// the custom languages of `root` as its last operation read them
    customs: Option<Arc<crate::custom::Customs>>,
}

/// The writer lock, also after a panic in a write that held it: the stores commit whole or not at all.
fn hold(writer: &Mutex<()>) -> MutexGuard<'_, ()> {
    writer.lock().unwrap_or_else(|p| p.into_inner())
}

/// The writer lock when no write of this process holds it: a read does not wait for one, which may
/// itself be waiting for another process's lock (`serve --watch`).
fn hold_if_free(writer: &Mutex<()>) -> Option<MutexGuard<'_, ()>> {
    match writer.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

impl Repo {
    /// Open (or create) the index of `root`, without updating it.
    pub fn open(root: &Path) -> Result<Repo> {
        Ok(Repo::around(crate::open(root)?, Arc::default()))
    }

    fn around(searcher: Searcher, writer: Arc<Mutex<()>>) -> Repo {
        let (root, models) = (searcher.ix.root.clone(), Arc::clone(&searcher.ix.models));
        Repo { root, searcher: Some(searcher), stale: false, updated: false, models, writer, customs: None }
    }

    /// Another open repository on the same index, to read beside this one: a connection of its
    /// own, the same model set, and this one's writer lock.
    pub fn another(&self) -> Result<Repo> {
        let ix = match &self.searcher {
            Some(s) => s.ix.another()?,
            None => index::Index::open(&self.root, Arc::clone(&self.models))?,
        };
        Ok(Repo::around(Searcher::new(ix), Arc::clone(&self.writer)))
    }

    /// `open` for an update; with `force`, an index of a newer scrutate that this one cannot read
    /// is deleted, to be built again by this one.
    pub fn open_to_update(root: &Path, force: bool) -> Result<Repo> {
        match Repo::open(root) {
            Err(e) if force && e.is::<index::Newer>() => {
                index::wipe_index(root);
                Repo::open(root)
            }
            r => r,
        }
    }

    /// The lock every write of this repository holds, shared with the ones `another` opened.
    pub fn writer(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.writer)
    }

    pub fn into_searcher(self) -> Searcher {
        self.searcher.expect("the index was not reopened after a rebuild")
    }

    /// Whether the last refresh could not update the index, so answers come from it as it was.
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Whether an update of the index went through since this was last asked.
    pub fn take_updated(&mut self) -> bool {
        std::mem::take(&mut self.updated)
    }

    /// The cancel flag of the next operation; nothing to set while the index is closed.
    pub fn set_cancel(&mut self, cancel: Arc<AtomicBool>) {
        if let Some(s) = &mut self.searcher {
            // an update the operation runs (a reindex, the scan before a read) stops at its next batch
            s.ix.cancel = Arc::clone(&cancel);
            s.cancel = cancel;
        }
    }

    /// Where the next operation counts the files it reads (`index::FileCount`).
    pub fn set_count(&mut self, count: Arc<index::FileCount>) {
        if let Some(s) = &mut self.searcher {
            s.ix.count = count;
        }
    }

    /// An index a failed rebuild left closed is opened and built again before anything reads it.
    /// An index a newer scrutate took over since it was opened, in a form this one cannot read,
    /// is an error for every operation until the process is restarted with that scrutate, or
    /// `index --force` builds a new database in its place.
    fn reopened(&mut self) -> Result<()> {
        match &mut self.searcher {
            None => {
                self.reopen(false)?;
            }
            Some(s) => {
                s.ix.reopen_if_replaced()?;
                if let Some(n) = s.ix.newer().filter(|n| !n.readable) {
                    return Err(n.into());
                }
            }
        }
        Ok(())
    }

    /// Bring the code index up to date with every file, as a read that waits for another writer
    /// does (`serve` and `lsp` start).
    pub fn update(&mut self, force: bool) -> Result<UpdateStats> {
        self.update_code(&Changes::All, force, false, false).map(|(st, _)| st)
    }

    /// The code index brought up to date with `changes`; a damaged one is built again. With
    /// `text`, a look at every file finds the text index's files too, and comes back with them.
    fn update_code(&mut self, changes: &Changes, force: bool, progress: bool, text: bool) -> Result<(UpdateStats, Option<Vec<Found>>)> {
        let writer = Arc::clone(&self.writer);
        let _w = hold(&writer);
        self.reopened()?;
        self.ix.progress |= progress;
        let mut found = None;
        let r = match changes {
            Changes::Named(n) if !force => self.ix.update_paths(&n.code, true),
            _ if text => {
                let f = found.insert(files::look(&self.root, true)?);
                self.ix.update_found(force, f)
            }
            _ => self.ix.update(force),
        };
        let r = match r {
            Err(e) if index::error_is_corruption(&e) => self.rebuild(&e),
            other => other,
        };
        self.updated |= r.is_ok();
        r.map(|st| (st, found))
    }

    /// Bring the stores `w` keeps up to date with `changes`: the code index first, then the text
    /// index, each under its own writer lock. When both look at every file, one walk and one
    /// `stat` per file serve both.
    pub fn update_stores(&mut self, w: Writer, changes: &Changes, force: bool) -> Result<Updated> {
        let text = w.text(&self.root);
        let (code, found) = match w.code() {
            true => self.update_code(changes, force, w.progress(), text).map(|(st, f)| (Some(st), f))?,
            false => (None, None),
        };
        let text = if text { Some(update_text(&self.root, changes, force, w.progress(), found.as_deref())?) } else { None };
        Ok(Updated { code, text })
    }

    /// `update_stores` for a `reindex` that stops between batches of files once `cancel` is set,
    /// and then leaves the code index as it was.
    // shortcut: a rebuild after damage and the text index do not see the flag, give them the flag if that matters
    pub fn update_until(&mut self, force: bool, cancel: Arc<AtomicBool>) -> Result<Updated> {
        self.reopened()?;
        self.ix.cancel = cancel;
        let r = self.update_stores(Writer::Reindex, &Changes::All, force);
        if let Some(s) = &mut self.searcher {
            s.ix.cancel = Default::default();
        }
        r
    }

    /// `update` before a read, unless the index was checked less than `min_interval` seconds
    /// ago. When it cannot run (another process holds the write lock, the index directory is
    /// read-only) the read answers from the last committed index, and stderr says why once. A
    /// lock held while the committed index has no file is a first build: `index::BeingBuilt`.
    pub fn refresh(&mut self, min_interval: f64) -> Result<Option<UpdateStats>> {
        let writer = Arc::clone(&self.writer);
        let Some(_w) = hold_if_free(&writer) else {
            // a write of this process runs: answer from the committed index, as against another
            // process's writer, unless nothing is committed yet
            return match &self.searcher {
                Some(s) if s.ix.has_files()? => Ok(None),
                _ => Err(index::BeingBuilt.into()),
            };
        };
        self.reopened()?;
        match self.ix.refresh_or_fail(min_interval) {
            Err(e) if index::error_is_corruption(&e) => self.rebuild(&e).map(Some),
            Err(e) if e.is::<index::Busy>() && !self.ix.has_files()? => Err(index::BeingBuilt.into()),
            Err(e) if e.is::<index::Newer>() => Err(e),
            Err(e) => {
                if !self.stale {
                    eprintln!("[scrutate] index not updated ({e:#}); answering from it as it is");
                    self.stale = true;
                }
                Ok(None)
            }
            Ok(st) => {
                self.stale &= st.is_none();
                self.updated |= st.is_some();
                Ok(st)
            }
        }
    }

    /// Run one operation on an index refreshed as `refresh` does (operations that do not read
    /// the index skip that). A `reindex` keeps the text index too, when there is one.
    pub fn run(&mut self, op: &ops::Op, min_interval: f64) -> Result<ops::Answer> {
        if op.reads_index() {
            self.refresh(min_interval)?;
        }
        self.run_as_is(op)
    }

    /// Run one operation on the index without a scan of the tree. The files the answer cites
    /// (and the ones its arguments name) are checked against the index, one `stat` each: when
    /// one changed or is gone, or a named one is new and a scan would index it, those files
    /// alone are updated and the answer computed again, once. A file no answer cites or names
    /// waits for the next scan. A damaged database is rebuilt and the operation run again. A
    /// `reindex` brings every store up to date (`Writer::Reindex`).
    pub fn run_as_is(&mut self, op: &ops::Op) -> Result<ops::Answer> {
        // under this repository's custom languages, whichever another one in the process loaded
        // last (`impact` parses files itself)
        let customs = crate::custom::of_root(&self.root, self.customs.as_ref())?;
        self.customs = Some(Arc::clone(&customs));
        crate::custom::with(&customs, || self.run_scoped(op))
    }

    fn run_scoped(&mut self, op: &ops::Op) -> Result<ops::Answer> {
        if let ops::Op::Reindex { force } = op {
            let up = self.update_stores(Writer::Reindex, &Changes::All, *force)?;
            return Ok(ops::Answer::Reindexed(up.code.unwrap_or_default()));
        }
        let first = self.recovering(|s| ops::run(s, op));
        if self.searcher.is_none() {
            return first;
        }
        let named = op.named_paths();
        let mut paths = named.clone();
        if let Ok(a) = &first {
            paths.extend(a.cited().into_iter().map(str::to_string));
        }
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let stale = match self.ix.stale(&paths) {
            Ok(mut s) => {
                s.extend(self.ix.unindexed(&named));
                s
            }
            Err(_) => return first,
        };
        if stale.is_empty() {
            return first;
        }
        let updated = {
            let writer = Arc::clone(&self.writer);
            let Some(_w) = hold_if_free(&writer) else { return first };
            match self.ix.update_paths(&stale, false) {
                Err(e) if index::error_is_corruption(&e) => self.rebuild_unless_rebuilt(&e).map(|_| true),
                // another writer holds the lock, or the index cannot be written: answer as it is
                r => Ok(r.is_ok()),
            }
        };
        match updated {
            Ok(true) => self.recovering(|s| ops::run(s, op)),
            Ok(false) => first,
            Err(e) => Err(e),
        }
    }

    /// Run `f` on the searcher; when it meets a damaged code index, the index is built again and
    /// `f` runs once more.
    pub fn recovering<T>(&mut self, mut f: impl FnMut(&mut Searcher) -> Result<T>) -> Result<T> {
        {
            // a closed index is built again before the read: a write
            let writer = Arc::clone(&self.writer);
            let _w = self.searcher.is_none().then(|| hold(&writer));
            self.reopened()?;
        }
        match f(self) {
            Err(e) if index::error_is_corruption(&e) => {
                let writer = Arc::clone(&self.writer);
                let w = hold(&writer);
                self.rebuild_unless_rebuilt(&e)?;
                drop(w);
                f(self)
            }
            other => other,
        }
    }

    /// The recovery of a damaged code index: its files are deleted (the index is a cache, the
    /// sources are the truth) and it is built again from every file.
    pub fn recover(&mut self, why: &anyhow::Error) -> Result<UpdateStats> {
        let writer = Arc::clone(&self.writer);
        let _w = hold(&writer);
        self.rebuild(why)
    }

    /// `rebuild`, unless a repository sharing this one's writer lock built the index again since
    /// this one opened it: then this one opens the new index. Called with the writer lock held.
    fn rebuild_unless_rebuilt(&mut self, why: &anyhow::Error) -> Result<()> {
        if let Some(s) = &mut self.searcher {
            let before = s.ix.db_id();
            if before.is_some() && s.ix.reopen_if_replaced().is_ok() && s.ix.db_id() != before {
                return Ok(());
            }
        }
        self.rebuild(why).map(|_| ())
    }

    /// `recover` with the writer lock already held.
    fn rebuild(&mut self, why: &anyhow::Error) -> Result<UpdateStats> {
        eprintln!("[scrutate] index damaged ({why:#}); rebuilding");
        let progress = self.searcher.as_ref().is_some_and(|s| s.ix.progress);
        let count = self.searcher.as_ref().map(|s| Arc::clone(&s.ix.count)).unwrap_or_default();
        self.searcher = None; // the database is closed before its files are deleted
        let gone = index::wipe_index(&self.root);
        let mut s = Searcher::new(index::Index::open(&self.root, Arc::clone(&self.models))?);
        if !gone {
            // another handle holds the files (Windows does not delete an open file): no old row or
            // segment may answer, so the database is emptied where it is
            s.ix.clear_in_place()?;
        }
        s.ix.progress = progress;
        s.ix.count = count;
        let st = s.ix.update(true);
        self.searcher = Some(s);
        st
    }

    fn reopen(&mut self, progress: bool) -> Result<UpdateStats> {
        let mut s = Searcher::new(index::Index::open(&self.root, Arc::clone(&self.models))?);
        s.ix.progress = progress;
        let st = s.ix.update(true);
        self.searcher = Some(s);
        st
    }
}

impl std::ops::Deref for Repo {
    type Target = Searcher;
    fn deref(&self) -> &Searcher {
        self.searcher.as_ref().expect("the index was not reopened after a rebuild")
    }
}

impl std::ops::DerefMut for Repo {
    fn deref_mut(&mut self) -> &mut Searcher {
        self.searcher.as_mut().expect("the index was not reopened after a rebuild")
    }
}

/// The files a search of the repository at `root` reads in place of the walk `walk` (one
/// `files::repository_files` gave, perhaps under a directory and with globs that exclude), when
/// the text index can say which of them hold no match: of the files a look made now finds there,
/// those the text index holds as they are now and that can match, and every other one. None
/// when the walk must run: no text index, a pattern its trigrams do not narrow, a glob that brings
/// files in, a scope that is no directory of the tree, a name that is not UTF-8.
pub fn grep_files(ix: &index::Index, q: &crate::instant::Query, walk: &crate::instant::Files) -> Option<crate::instant::Files> {
    use crate::instant::Paths;
    let Paths::Walk(roots) = &walk.paths else { return None };
    let plain = walk.hidden && !walk.no_ignore && !walk.follow_links && walk.max_filesize.is_none() && walk.max_depth.is_none();
    let plain = plain && walk.types.is_empty() && walk.scoped_globs.is_none() && !walk.search_zip && walk.globs.iter().all(|g| g.starts_with('!'));
    let ([at], true) = (&roots[..], plain) else { return None };
    let under = match walk.paths_as_given {
        false if *at == ix.root => String::new(),
        // a directory named as it is, so the walk's names and these agree
        true => {
            let rel = at.strip_prefix(&ix.root).ok()?;
            let normal = rel.components().all(|c| matches!(c, std::path::Component::Normal(_)) && c.as_os_str().to_str().is_some());
            if !normal || rel.as_os_str().is_empty() || std::fs::canonicalize(at).ok()? != *at {
                return None;
            }
            format!("{}/", slashed_rel(rel))
        }
        false => return None,
    };
    let cond = textindex::query_for(q).ok().filter(|c| *c != textindex::Q::All)?;
    let ti = textindex::TextIndex::open(&ix.root).ok()??;
    // a look of this call's own, so a file written just before it is read (a kept look missed it)
    let found = files::look(&ix.root, false).ok()?;
    if found.iter().any(|f| f.repo && f.lossy) {
        return None;
    }
    let mut there = found.iter().filter(|f| f.repo && f.path.starts_with(under.as_str())).map(|f| (f.path.as_str(), f.stat)).peekable();
    // a directory the look did not enter is searched as the walk searches it
    there.peek()?;
    let read = ti.to_read(&cond, q.pattern.multiline, there).ok()?;
    // named from the directory searched, as its walk names them
    let files = read.into_iter().map(|p| p[under.len()..].to_string()).collect();
    Some(crate::instant::Files { paths: Paths::List { root: at.clone(), files }, paths_as_given: false, ..walk.clone() })
}

fn slashed_rel(rel: &Path) -> String {
    rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// The code index's record of each file it holds, for the `tsquery` cache: the stat and content
/// hash by path from the root. Empty when there is no code index: this never creates one.
pub fn recorded(root: &Path) -> std::collections::HashMap<String, (index::Stat, String)> {
    index::recorded(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_operation_reads_one_commit_while_a_writer_commits_another() {
        let root = std::env::temp_dir().join(format!("scrutate_ops_snapshot_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f.py"), "def needle_alpha():\n    return 1\n").unwrap();
        std::fs::write(root.join("g.py"), "def other():\n    return 2\n").unwrap();
        let mut ix = crate::index::Index::open(&root, ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let mut repo = Repo::around(Searcher::new(ix), Arc::default());
        // another process rewrites f.py and commits after the search picked its rows, before it reads them
        let r = root.clone();
        crate::index::BEFORE_CHUNKS.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                std::fs::write(r.join("f.py"), "def needle_alpha():\n    return 3\n").unwrap();
                crate::index::Index::open(&r, crate::models::ModelSet::none()).unwrap().update(false).unwrap();
            }))
        });
        let ops::Answer::Hits { hits, .. } = repo.run_as_is(&ops::Op::Search { query: "needle_alpha".into(), alt: Vec::new(), k: 5, snippets: false }).unwrap()
        else {
            panic!("a search answers hits")
        };
        assert!(crate::index::BEFORE_CHUNKS.with(|h| h.borrow().is_none()), "the writer ran");
        // the row of the commit the search started on, which the writer has since deleted
        let row = repo.ix.chunk(hits.iter().find(|h| h.name == "needle_alpha").map(|h| h.id).unwrap_or(-1)).unwrap();
        assert!(hits.iter().any(|h| h.name == "needle_alpha"), "{hits:?}");
        assert!(row.is_none(), "the writer's commit replaced the row");
        drop(repo);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rebuild_that_could_not_reopen_the_index_is_an_error_until_it_can() {
        let root = std::env::temp_dir().join(format!("scrutate_reopen_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut repo =
            Repo { searcher: None, root: root.clone(), stale: false, updated: false, models: ModelSet::from_env(), writer: Arc::default(), customs: None };
        let op = ops::Op::Symbol { name: "f".into(), limit: 5 };
        repo.set_cancel(Arc::default());
        assert!(repo.update(false).is_err());
        assert!(repo.refresh(0.0).is_err());
        assert!(repo.run(&op, 0.0).is_err());
        assert!(repo.run_as_is(&op).is_err());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def f():\n    return 1\n").unwrap();
        assert!(matches!(repo.run_as_is(&op), Ok(ops::Answer::Hits { ref hits, .. }) if hits.len() == 1));
        drop(repo);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The grep Operation's answer through `repo` (which reads the text index when there is one)
    /// and through a walk (the text index set aside), as text and JSON.
    fn both_ways(repo: &mut Repo, op: &ops::Op) -> (String, String) {
        let shown = |a: Result<ops::Answer>| a.map(|a| (a.text(), a.json().to_string())).unwrap_or_else(|e| (format!("{e:#}"), String::new()));
        let a = shown(repo.run(op, 0.0));
        let (dir, aside) = (textindex::dir_of(&repo.root), repo.root.join(".scrutate/text.aside"));
        let had = dir.exists();
        if had {
            std::fs::rename(&dir, &aside).unwrap();
        }
        let walked = shown(repo.run_as_is(op));
        if had {
            std::fs::rename(&aside, &dir).unwrap();
        }
        assert_eq!(a, walked);
        a
    }

    #[test]
    fn the_grep_operation_reads_the_text_index_for_the_files_it_holds_as_they_are_and_answers_as_the_walk() {
        use crate::instant::{Case, Mode, Pattern};
        let root = std::env::temp_dir().join(format!("scrutate_grep_ti_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let put = |p: &str, t: &str| {
            std::fs::create_dir_all(root.join(p).parent().unwrap()).unwrap();
            std::fs::write(root.join(p), t).unwrap();
        };
        put(".git/HEAD", "ref: refs/heads/main\n");
        put(".gitignore", "*.log\n");
        for i in 0..40 {
            put(&format!("src/m{i:02}.py"), &format!("def fn_{i}():\n    return {i}\n"));
        }
        put("src/rare.py", "def needle_token():\n    return 1\n\nclass Holder:\n    def go(self):\n        return needle_token()\n");
        put("lib/other.rs", "fn needle_token() {}\nfn main() { needle_token(); }\n");
        put(".github/ci.yml", "run: needle_token\n");
        put("node_modules/x/i.js", "needle_token\n");
        put("data.bin", "needle_token\0\x01\x02needle_token\n");
        put("notes.log", "needle_token\n");
        update(&root, Writer::Index, false).unwrap();
        let mut repo = Repo::open(&root).unwrap();
        let grep = |text: &str, mode: Mode, case: Case, word: bool, path: Option<&str>, globs: &[&str]| ops::Op::Grep {
            pattern: Pattern { text: text.into(), mode, case, word, ..Pattern::default() },
            scope: ops::Scope { path: path.map(String::from), globs: globs.iter().map(|g| g.to_string()).collect() },
            max_results: 1000,
        };
        let cases = |repo: &mut Repo| {
            let mut out = Vec::new();
            for (text, mode) in [
                ("needle_token", Mode::Literal),
                ("needle_[a-z]+", Mode::Regex),
                ("needleToken", Mode::Identifier),
                ("needle_tokan", Mode::Fuzzy { max_edits: 1 }),
                ("fn_3", Mode::Regex),
                ("ab", Mode::Literal),
            ] {
                for case in [Case::Sensitive, Case::Insensitive, Case::Smart] {
                    for (path, globs) in [
                        (None, &[][..]),
                        (Some("src"), &[][..]),
                        (Some("src/"), &["!rare.py"][..]),
                        (None, &["!*.rs"][..]),
                        (None, &["*.rs"][..]),
                        (Some("lib/other.rs"), &[][..]),
                    ] {
                        out.push(both_ways(repo, &grep(text, mode, case, false, path, globs)));
                    }
                    out.push(both_ways(repo, &grep(text, mode, case, true, None, &[])));
                }
            }
            out
        };
        let fresh = cases(&mut repo);
        assert!(fresh[0].0.contains("src/rare.py") && fresh[0].0.contains(".github/ci.yml") && !fresh[0].0.contains("node_modules"), "{}", fresh[0].0);
        let q = crate::instant::Query { pattern: Pattern { text: "needle_token".into(), ..Pattern::default() }, ..Default::default() };
        let narrowed = grep_files(&repo.ix, &q, &crate::files::repository_files(&repo.root)).expect("the text index narrows a rare word");
        let crate::instant::Paths::List { files: read, .. } = narrowed.paths else { panic!("a list") };
        // the hidden files are not in the text index: they are read directly
        assert_eq!(read, [".github/ci.yml", ".gitignore", "data.bin", "lib/other.rs", "src/rare.py"]);
        // a new file, an edit, an edit that keeps size and time, a removal and a rename since the last build
        put("src/added.py", "x = needle_token\n");
        put("src/m01.py", "def fn_1():\n    return needle_token\n");
        let t = std::fs::metadata(root.join("src/m02.py")).unwrap().modified().unwrap();
        let old = std::fs::read(root.join("src/m02.py")).unwrap().len();
        put("src/m02.py", &format!("{:<w$}\n", "needle_token", w = old - 1));
        std::fs::File::options().write(true).open(root.join("src/m02.py")).unwrap().set_modified(t).unwrap();
        std::fs::remove_file(root.join("src/m03.py")).unwrap();
        std::fs::rename(root.join("src/m04.py"), root.join("src/m44.py")).unwrap();
        let stale = cases(&mut repo);
        assert!(stale[0].0.contains("src/added.py") && stale[0].0.contains("src/m01.py") && stale[0].0.contains("src/m02.py"), "{}", stale[0].0);
        assert_ne!(fresh, stale);
        std::fs::remove_dir_all(textindex::dir_of(&root)).unwrap();
        assert_eq!(cases(&mut repo), stale, "without a text index the walk answers");
        assert!(!textindex::exists(&root), "and no text index is built");
        drop(repo);
        let _ = std::fs::remove_dir_all(&root);
    }
}
