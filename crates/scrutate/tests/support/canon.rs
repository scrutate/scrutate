//! A path as scrutate shows it and hands it on: canonical, and on Windows without the verbatim
//! prefix `fs::canonicalize` puts in front (`\\?\C:\x`, `\\?\UNC\host\share\x`).

use std::path::{Path, PathBuf};

pub fn canon(p: impl AsRef<Path>) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap();
    scrutate::paths::plain_path(&c).into_owned()
}

/// The canonical path with the verbatim prefix kept, the form a session keys its root by (its
/// Windows pipe's name hashes it).
#[allow(dead_code)]
pub fn as_the_session_keys_it(p: impl AsRef<Path>) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}
