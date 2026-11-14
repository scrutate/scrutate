//! Which Repository files changed since a token: the change set (GLOSSARY). A session asks this
//! before every read and brings the code index up to date with what it says, so a read costs a
//! `stat` per changed file, not one per file of the tree. Two adapters answer it: the walk, which
//! knows nothing and says "every file", and the watcher (`watch.rs`), which names the files its
//! events touched and says "every file" whenever it cannot vouch for them.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::files::Found;
use crate::stores::Changes;

/// A point in a freshness's history; a session keeps the token of the last change set it applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Token(u64);

impl Token {
    /// The token after this one.
    pub fn next(self) -> Token {
        Token(self.0 + 1)
    }

    pub(crate) fn at(n: u64) -> Token {
        Token(n)
    }

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
