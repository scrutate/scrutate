//! Paths as people and other programs read them.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// `path` without Windows' verbatim prefix, which `fs::canonicalize` gives there and git, a hook's
/// host and a reader of the output do not take: `\\?\C:\x` is `C:\x`, `\\?\UNC\host\share\x` is
/// `\\host\share\x`. Anything else, and every path on other platforms, is returned as it is.
pub fn plain(path: &str) -> Cow<'_, str> {
    plain_on(path, cfg!(windows))
}

/// `plain` for a path.
pub fn plain_path(path: &Path) -> Cow<'_, Path> {
    match path.to_str().map(plain) {
        Some(Cow::Owned(p)) => Cow::Owned(PathBuf::from(p)),
        _ => Cow::Borrowed(path),
    }
}

/// `plain` as it is on Windows (`windows`) or elsewhere.
pub(crate) fn plain_on(path: &str, windows: bool) -> Cow<'_, str> {
    if windows {
        if let Some(rest) = path.strip_prefix(r"\\?\") {
            if rest.as_bytes().get(1) == Some(&b':') {
                return rest.into();
            }
            if let Some(share) = rest.strip_prefix(r"UNC\") {
                return format!(r"\\{share}").into();
            }
        }
    }
    path.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verbatim_path_is_shown_plain_a_share_included() {
        assert_eq!(plain_on(r"\\?\C:\work\repo", true), r"C:\work\repo");
        assert_eq!(plain_on(r"\\?\UNC\host\share\repo", true), r"\\host\share\repo", "not UNC\\host\\share");
        assert_eq!(plain_on(r"\\?\GLOBALROOT\Device\x", true), r"\\?\GLOBALROOT\Device\x", "no plain form: kept");
        assert_eq!(plain_on(r"C:\work", true), r"C:\work");
        assert_eq!(plain_on(r"\\?\C:\work", false), r"\\?\C:\work", "only Windows has the prefix");
    }

    /// The `.rs` files under `dir` of this crate, by path relative to the crate.
    fn rust_files(dir: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut dirs = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/")).join(dir)];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).unwrap().map(Result::unwrap) {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let rel = p.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().to_string_lossy().replace('\\', "/");
                    out.push((rel.trim_start_matches('/').to_string(), std::fs::read_to_string(&p).unwrap()));
                }
            }
        }
        out
    }

    #[test]
    fn verbatim_paths_are_made_plain_in_one_place() {
        // split so these lines do not match themselves
        let (prefix, call) = (["r\"\\\\", "?\\\""].concat(), ["canonical", "ize("].concat());
        let mut found = Vec::new();
        for (rel, text) in rust_files("src") {
            for (i, line) in text.lines().enumerate() {
                let hand = line.contains(&prefix) && (line.contains("trim_start_matches(") || line.contains("strip_prefix("));
                if hand && rel != "src/paths.rs" {
                    found.push(format!("{rel}:{}", i + 1));
                }
            }
        }
        for (rel, text) in rust_files("tests") {
            for (i, line) in text.lines().enumerate() {
                if line.contains(&call) && !line.trim_start().starts_with("//") && !rel.starts_with("tests/support/") {
                    found.push(format!("{rel}:{}", i + 1));
                }
            }
        }
        assert!(found.is_empty(), "use crate::paths::plain in src and support::canon in tests: {found:?}");
    }
}
