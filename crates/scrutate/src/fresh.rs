//! Which Repository files changed since a token: the change set (GLOSSARY). A session asks this
//! before every read and brings the code index up to date with what it says, so a read costs a
//! `stat` per changed file, not one per file of the tree. Two adapters answer it: the walk, which
//! knows nothing and says "every file", and the watcher (`watch.rs`), which names the files its
//! events touched and says "every file" whenever it cannot vouch for them.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::files::Found;
use crate::stores::Changes;

/// The watched root's path names another directory now (renamed over, or removed and made
/// again): the watch and the stores opened there belong to the old tree. Here, not in
/// `watch`, since the wasi build has no watch and its callers name it all the same.
#[derive(Debug)]
pub struct RootReplaced;

impl std::fmt::Display for RootReplaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the root was replaced")
    }
}

impl std::error::Error for RootReplaced {}

/// A point in a freshness's history; a session keeps the token of the last change set it applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Token(u64);

impl Token {
    /// The token after this one.
    pub fn next(self) -> Token {
        Token(self.0 + 1)
    }

    /// The watcher's own numbering, which no other adapter has.
    #[cfg(not(target_os = "wasi"))]
    pub(crate) fn at(n: u64) -> Token {
        Token(n)
    }

    #[cfg(not(target_os = "wasi"))]
    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

pub trait Freshness: Send + Sync {
    /// What changed since `t`, a token this gave before, and the token that stands for now. A
    /// file written before this is called is in the answer.
    fn changes_since(&self, t: Token) -> (Changes, Token);

    /// Every file of the repository, as a look at the tree finds it (`files::look`).
    fn files(&self, text: bool) -> Result<Vec<Found>>;

    /// Whether events back the answers: an answer of every file then asks for a walk now.
    fn watching(&self) -> bool {
        false
    }

    /// Whether a change set of named files proves what changed. Where it does not (macOS), every
    /// answer is every file and a read looks at the tree whatever the refresh interval.
    fn order_proves(&self) -> bool {
        true
    }
}

/// What feeds a session's freshness from a watch: the watch's notes, on targets with a file watcher.
#[cfg(not(target_os = "wasi"))]
pub type Watching = crate::watch::Notes;

/// No file watcher on this target: never made.
#[cfg(target_os = "wasi")]
#[derive(Clone)]
pub enum Watching {}

#[cfg(target_os = "wasi")]
impl Watching {
    pub fn run(&self, _: &Path, _: &std::sync::atomic::AtomicBool, _: impl FnOnce(), _: impl FnMut(&Changes) -> Result<()>) -> Result<()> {
        match *self {}
    }
}

/// The freshness a session of `root` (canonical) reads with, and the watch that feeds it: a
/// watch's notes where this target has a watcher and `root` is on a local disk (`watch::local`),
/// the walk elsewhere. The one place that chooses.
pub fn for_session(root: &Path) -> (Box<dyn Freshness>, Option<Watching>) {
    #[cfg(not(target_os = "wasi"))]
    if crate::watch::local(root) {
        let notes = crate::watch::Notes::new(root);
        return (Box::new(notes.clone()), Some(notes));
    }
    if cfg!(not(target_os = "wasi")) && std::env::var_os("SCRUTATE_SESSION_DEBUG").is_some_and(|v| !v.is_empty() && v != "0") {
        eprintln!("[scrutate] session: {} is not on a local disk; every read walks the tree", root.display());
    }
    (Box::new(Walk::new(root)), None)
}

/// No watcher: any file may have changed, so every call walks the tree.
pub struct Walk {
    root: PathBuf,
}

impl Walk {
    pub fn new(root: &Path) -> Walk {
        Walk { root: root.to_path_buf() }
    }
}

impl Freshness for Walk {
    fn changes_since(&self, t: Token) -> (Changes, Token) {
        (Changes::All, t)
    }

    fn files(&self, text: bool) -> Result<Vec<Found>> {
        crate::files::look(&self.root, text)
    }
}
