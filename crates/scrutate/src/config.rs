//! Configuration files. Values become defaults for the matching command-line options by being
//! exported as the options' environment variables before argument parsing, so the precedence is
//! always: command line > environment > configuration file > built-in default, and every option
//! documents the variable that configures it (`scrutate grep --help`).
//!
//! Resolution order (first file found wins): `$SCRUTATE_CONFIG`, the nearest `.scrutate.toml` from
//! the current directory up to the top of the repository (or `<root>/.scrutate.toml` with `--root`),
//! `$XDG_CONFIG_HOME/scrutate/config.toml` (or `~/.config/scrutate/config.toml`).
//!
//! ```toml
//! [grep]
//! hidden = true            # SCRUTATE_GREP_HIDDEN
//! no_ignore = false        # SCRUTATE_GREP_NO_IGNORE
//! threads = 4              # SCRUTATE_THREADS
//! max_filesize = "10M"     # SCRUTATE_GREP_MAX_FILESIZE
//! globs = ["!vendor/**"]   # a `.scrutate.toml`: matched from the file's directory; else SCRUTATE_GREP_GLOBS
//! types = ["py", "rs"]     # SCRUTATE_GREP_TYPES
//! smart_case = true        # SCRUTATE_GREP_SMART_CASE
//! max_bytes = 24000        # SCRUTATE_GREP_MAX_BYTES (a bounded answer; piped output is whole without it)
//!
//! [index]
//! exclude_dirs = ["generated", "third_party"]   # SCRUTATE_INDEX_EXCLUDE_DIRS; read from the root's own file only
//! refresh_interval = 2.0                         # SCRUTATE_REFRESH_INTERVAL (seconds, `serve`)
//! max_files = 500000                             # SCRUTATE_INDEX_MAX_FILES (no limit by default)
//! max_bytes = "4G"                               # SCRUTATE_INDEX_MAX_BYTES (bytes of the files read; or a number)
//!
//! [[language]]                                   # a language of its own: see custom.rs
//! name = "mylang"
//! extensions = ["myl"]
//! line_comment = "--"
//! definitions = [{ kind = "function", pattern = '^\s*fn\s+([A-Za-z_]\w*)' }]
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub grep: Grep,
    #[serde(default)]
    pub index: IndexCfg,
    #[serde(default)]
    pub language: Vec<LanguageCfg>,
}

/// One `[[language]]` table, as written; `custom.rs` checks it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanguageCfg {
    pub name: toml::Spanned<String>,
    pub extensions: toml::Spanned<Vec<String>>,
    pub line_comment: Option<toml::Spanned<String>>,
    pub block_comment: Option<toml::Spanned<Vec<String>>>,
    #[serde(default)]
    pub definitions: Vec<DefinitionCfg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefinitionCfg {
    pub kind: toml::Spanned<String>,
    pub pattern: toml::Spanned<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grep {
    pub hidden: Option<bool>,
    pub no_ignore: Option<bool>,
    pub follow: Option<bool>,
    pub smart_case: Option<bool>,
    pub threads: Option<usize>,
    pub max_filesize: Option<String>,
    pub max_depth: Option<usize>,
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub globs: Vec<String>,
    #[serde(default)]
    pub types: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexCfg {
    #[serde(default)]
    pub exclude_dirs: Vec<String>,
    pub refresh_interval: Option<f64>,
    pub max_files: Option<u64>,
    pub max_bytes: Option<Size>,
}

/// A size as a number of bytes or with a `K`, `M` or `G` suffix (`"4G"`).
#[derive(Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Size {
    Bytes(u64),
    Text(String),
}

/// The configuration file that applies to `root`, if any.
pub fn locate(root: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SCRUTATE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let local = root.join(".scrutate.toml");
    if local.is_file() {
        return Some(local);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join("scrutate").join("config.toml")).filter(|p| p.is_file())
}

/// Takes out of a repository's own file what only the user may decide. `follow` would let a
/// search of a cloned repository leave it through a link; the file's other settings stay inside
/// the tree. Says so once on stderr.
pub fn confine(cfg: &mut Config) {
    if cfg.grep.follow == Some(true) {
        cfg.grep.follow = None;
        eprintln!(
            "[scrutate] follow = true in this repository's .scrutate.toml is ignored: only you can make a search follow links (-L, SCRUTATE_GREP_FOLLOW, or your own configuration file)"
        );
    }
}

pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(path, &text)
}

/// `text` of the file `path` as a configuration; an error is one line that names the file and the line.
pub fn parse(path: &Path, text: &str) -> Result<Config> {
    toml::from_str(text).map_err(|e| anyhow::anyhow!("{}: {}", at(path, text, e.span().map_or(0, |s| s.start)), e.message().trim().replace('\n', "; ")))
}

/// `path:line` of the byte `offset` of `text`.
pub fn at(path: &Path, text: &str, offset: usize) -> String {
    let line = text.as_bytes()[..offset.min(text.len())].iter().filter(|&&b| b == b'\n').count() + 1;
    format!("{}:{line}", path.display())
}

/// Whether on/off variable `name` is on: off when unset, empty, `0`, `n`, `no`, `f`, `false` or
/// `off` in any case, as clap reads the flags' variables; a value that is not UTF-8 is off too.
pub fn env_on(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !matches!(v.to_ascii_lowercase().as_str(), "" | "0" | "n" | "no" | "f" | "false" | "off"))
}

/// Export the configured values as environment variables unless the variable is already set
/// (an explicit environment setting beats the file).
pub fn apply_as_env(cfg: &Config) {
    fn set(name: &str, value: String) {
        if std::env::var_os(name).is_none() {
            // SAFETY (soundness, not memory): called once at startup before any thread is spawned,
            // which is the condition under which setting process environment variables is sound.
            unsafe { std::env::set_var(name, value) };
        }
    }
    let b = |v: bool| if v { "true" } else { "false" }.to_string();
    if let Some(v) = cfg.grep.hidden {
        set("SCRUTATE_GREP_HIDDEN", b(v));
    }
    if let Some(v) = cfg.grep.no_ignore {
        set("SCRUTATE_GREP_NO_IGNORE", b(v));
    }
    if let Some(v) = cfg.grep.follow {
        set("SCRUTATE_GREP_FOLLOW", b(v));
    }
    if let Some(v) = cfg.grep.smart_case {
        set("SCRUTATE_GREP_SMART_CASE", b(v));
    }
    if let Some(v) = cfg.grep.threads {
        set("SCRUTATE_THREADS", v.to_string());
    }
    if let Some(v) = &cfg.grep.max_filesize {
        set("SCRUTATE_GREP_MAX_FILESIZE", v.clone());
    }
    if let Some(v) = cfg.grep.max_depth {
        set("SCRUTATE_GREP_MAX_DEPTH", v.to_string());
    }
    if let Some(v) = cfg.grep.max_bytes {
        set("SCRUTATE_GREP_MAX_BYTES", v.to_string());
    }
    if !cfg.grep.globs.is_empty() {
        set("SCRUTATE_GREP_GLOBS", cfg.grep.globs.join(","));
    }
    if !cfg.grep.types.is_empty() {
        set("SCRUTATE_GREP_TYPES", cfg.grep.types.join(","));
    }
    if let Some(v) = cfg.index.refresh_interval {
        set("SCRUTATE_REFRESH_INTERVAL", v.to_string());
    }
    if let Some(v) = cfg.index.max_files {
        set("SCRUTATE_INDEX_MAX_FILES", v.to_string());
    }
    match &cfg.index.max_bytes {
        Some(Size::Bytes(v)) => set("SCRUTATE_INDEX_MAX_BYTES", v.to_string()),
        Some(Size::Text(v)) => set("SCRUTATE_INDEX_MAX_BYTES", v.clone()),
        None => {}
    }
}

/// Root directory named on the command line (`--root`, before parsing), so the local config can be found.
pub fn root_from_args(args: &[String]) -> PathBuf {
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix("--root=") {
            return PathBuf::from(v);
        }
        if a == "--root"
            && let Some(v) = args.get(i + 1)
        {
            return PathBuf::from(v);
        }
    }
    std::env::var_os("SCRUTATE_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_example() {
        let c: Config = toml::from_str(
            "[grep]\nhidden = true\nthreads = 4\nmax_bytes = 4000\nglobs = [\"!vendor/**\"]\n[index]\nexclude_dirs = [\"generated\"]\nrefresh_interval = 2.0\nmax_files = 500000\nmax_bytes = \"4G\"\n",
        )
        .unwrap();
        assert_eq!(c.grep.hidden, Some(true));
        assert_eq!(c.grep.threads, Some(4));
        assert_eq!(c.grep.max_bytes, Some(4000));
        assert_eq!(c.grep.globs, vec!["!vendor/**"]);
        assert_eq!(c.index.exclude_dirs, vec!["generated"]);
        assert_eq!(c.index.refresh_interval, Some(2.0));
        assert_eq!(c.index.max_files, Some(500000));
        assert_eq!(c.index.max_bytes, Some(Size::Text("4G".into())));
        let c: Config = toml::from_str("[index]\nmax_bytes = 4000\n").unwrap();
        assert_eq!(c.index.max_bytes, Some(Size::Bytes(4000)));
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(toml::from_str::<Config>("[grep]\nhiden = true\n").is_err());
    }
}
