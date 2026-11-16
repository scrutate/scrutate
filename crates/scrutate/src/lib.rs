//! Scrutate engine: incremental code index (BM25 postings + semantic vectors + call graph), a learned
//! ranker, and the search layer used by the CLI and the MCP server.

// Index-driven loops over parallel arrays (postings, feature matrices) read better with explicit
// indices than with zipped iterators; this lint is noise here.
#![allow(clippy::needless_range_loop)]

pub mod answers;
pub mod archive;
pub mod artifacts;
pub mod cli;
pub mod config;
pub mod custom;
pub mod dead;
pub mod edit;
pub mod files;
pub mod filter;
pub mod finder;
pub mod findings;
pub mod fmt;
pub mod fresh;
pub mod fuzzyre;
pub mod git;
pub mod hook;
pub mod ident;
pub mod impact;
pub mod index;
pub mod instant;
pub mod intel;
pub mod langs;
pub mod lex;
pub mod lsp;
pub mod mcp;
pub mod models;
pub mod names;
pub mod npy;
pub mod ops;
pub mod packages;
pub mod parse;
pub mod paths;
pub mod rank;
pub mod repomap;
pub mod resolve;
pub mod review;
pub mod rewrite;
pub mod running;
pub mod search;
pub mod secrets;
pub mod semantic;
pub mod session;
pub mod setup;
pub mod shellhook;
pub mod stores;
pub mod symmatch;
pub mod text;
pub mod textindex;
pub mod tscache;
#[cfg(not(target_os = "wasi"))]
pub mod watch;
/// No file events on wasi.
#[cfg(target_os = "wasi")]
pub mod watch {
    pub use crate::fresh::RootReplaced;

    pub fn run(
        _root: &std::path::Path,
        _stop: &std::sync::atomic::AtomicBool,
        _on_ready: impl FnOnce(),
        _on_change: impl FnMut(&crate::stores::Changes) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        anyhow::bail!("watch is not in the wasi build: it has no file events")
    }
}

use std::path::Path;

use anyhow::Result;

/// Tests only: claims `dir` as the calling test's temp tree; panics, naming both tests, when
/// another test holds it, since two tests on one tree remove and rewrite each other's files.
#[cfg(test)]
pub(crate) fn claim_test_tree(dir: &Path) {
    static TEST_TREES: std::sync::Mutex<Option<std::collections::HashMap<std::path::PathBuf, String>>> = std::sync::Mutex::new(None);
    let me = std::thread::current().name().unwrap_or("?").to_string();
    let mut held = TEST_TREES.lock().unwrap_or_else(|p| p.into_inner());
    let other = held.get_or_insert_with(Default::default).entry(dir.to_path_buf()).or_insert_with(|| me.clone()).clone();
    assert!(other == me, "{} is the temp tree of both {other} and {me}: give each test its own tag", dir.display());
}

pub use stores::Repo;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Open (or create) the index of `root` and a searcher on it, with the model set the environment
/// names (`models::ModelSet::from_env`).
pub fn open(root: &Path) -> Result<search::Searcher> {
    Ok(search::Searcher::new(index::Index::open(root, models::ModelSet::from_env())?))
}

/// Open and bring the index up to date: `Repo::update` with `force`, else `Repo::refresh`, which
/// answers from the index as it is when it cannot be updated.
pub fn open_updated(root: &Path, force: bool) -> Result<(Repo, index::UpdateStats)> {
    let mut repo = Repo::open(root)?;
    let st = if force { repo.update(true)? } else { repo.refresh(0.0)?.unwrap_or_default() };
    Ok((repo, st))
}
