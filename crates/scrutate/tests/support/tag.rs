//! One temp tree per test: two tests of a binary given the same tag would remove and rewrite each
//! other's files when they run in parallel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Claims `dir` for the calling test; panics, naming both tests, when another test holds it.
pub fn claim(dir: &Path) {
    static HELD: Mutex<Option<HashMap<PathBuf, String>>> = Mutex::new(None);
    let me = std::thread::current().name().unwrap_or("?").to_string();
    let mut held = HELD.lock().unwrap_or_else(|p| p.into_inner());
    let other = held.get_or_insert_with(HashMap::new).entry(dir.to_path_buf()).or_insert_with(|| me.clone()).clone();
    assert!(other == me, "{} is the temp tree of both {other} and {me}: give each test its own tag", dir.display());
}
