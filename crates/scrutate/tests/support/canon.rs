//! A path as scrutate shows it and hands it on: canonical, and on Windows without the verbatim
//! prefix `fs::canonicalize` puts in front (`\\?\C:\x`, `\\?\UNC\host\share\x`).

use std::path::{Path, PathBuf};

pub fn canon(p: impl AsRef<Path>) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap();
    scrutate::paths::plain_path(&c).into_owned()
}
