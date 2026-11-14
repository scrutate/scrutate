//! Custom languages: a `[[language]]` table in the root's own `.scrutate.toml` or in the user's
//! configuration file names a language the binary does not know, its extensions, its comment
//! syntax and one regular expression per kind of definition, whose first group is the name.
//!
//! The tables are data: nothing in them runs, and no key names another file. The root's file is
//! read as the index reads repository files (a regular file inside the root, links resolved);
//! the user's file is `$SCRUTATE_CONFIG`, else `$XDG_CONFIG_HOME/scrutate/config.toml` (or
//! `~/.config/scrutate/config.toml`). A table that does not hold is one error naming the file
//! and the line. The tables in effect are process-wide (`current`): every command and every
//! index update loads them for its root first.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};

use anyhow::{anyhow, Result};
use regex::Regex;

use crate::config::{at, LanguageCfg};

/// The kinds a definition pattern can give a symbol.
pub const KINDS: &[&str] = &["class", "constant", "enum", "function", "impl", "interface", "macro", "method", "module", "struct", "trait", "type"];

/// One language a table adds.
pub struct Lang {
    pub id: String,
    /// lowercase, with the dot, as the built-in languages list theirs
    pub exts: Vec<String>,
    pub line_comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    pub defs: Vec<(&'static str, Regex)>,
    /// `.scrutate.toml` for the root's file, else the path of the user's file
    pub source: String,
    sig: String,
}

impl Lang {
    /// The kind and name of the definition `line` opens: the first pattern that matches, its first group.
    pub fn definition(&self, line: &str) -> Option<(&'static str, String)> {
        self.defs.iter().find_map(|(kind, re)| re.captures(line)?.get(1).filter(|m| !m.as_str().is_empty()).map(|m| (*kind, m.as_str().to_string())))
    }

    /// A hash of everything in the table that decides how a file is parsed.
    pub fn sig(&self) -> &str {
        &self.sig
    }

    pub fn kinds(&self) -> Vec<&'static str> {
        let mut k: Vec<&'static str> = self.defs.iter().map(|d| d.0).collect();
        k.sort();
        k.dedup();
        k
    }
}

/// The custom languages in effect, and the texts they were read from.
#[derive(Default)]
pub struct Customs {
    pub langs: Vec<Lang>,
    files: Vec<(PathBuf, String)>,
}

impl std::fmt::Debug for Customs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.langs.iter().map(|l| &l.id)).finish()
    }
}

impl Customs {
    /// The custom language whose extension `path` ends with (the longest one).
    pub fn for_path(&self, path: &str) -> Option<&Lang> {
        if self.langs.is_empty() {
            return None;
        }
        let lower = path.rsplit('/').next().unwrap_or(path).to_lowercase();
        let owners = self.langs.iter().flat_map(|l| l.exts.iter().map(move |e| (l, e)));
        owners.filter(|(_, e)| lower.len() > e.len() && lower.ends_with(e.as_str())).max_by_key(|(_, e)| e.len()).map(|(l, _)| l)
    }

    pub fn by_id(&self, id: &str) -> Option<&Lang> {
        self.langs.iter().find(|l| l.id == id)
    }

    /// Each extension with the signature of its language, as the index's meta keeps it.
    pub fn stamp(&self) -> String {
        let m: BTreeMap<&str, &str> = self.langs.iter().flat_map(|l| l.exts.iter().map(|e| (e.as_str(), l.sig.as_str()))).collect();
        serde_json::to_string(&m).unwrap_or_default()
    }
}

/// The extensions whose language differs between two stamps (added, removed or changed).
pub fn changed(old: &str, new: &str) -> Vec<String> {
    let parse = |s: &str| serde_json::from_str::<BTreeMap<String, String>>(s).unwrap_or_default();
    let (old, new) = (parse(old), parse(new));
    let mut exts: Vec<String> = old.keys().chain(new.keys()).filter(|e| old.get(*e) != new.get(*e)).cloned().collect();
    exts.sort();
    exts.dedup();
    exts
}

static CURRENT: LazyLock<RwLock<Arc<Customs>>> = LazyLock::new(RwLock::default);

thread_local! {
    static SCOPED: std::cell::RefCell<Option<Arc<Customs>>> = const { std::cell::RefCell::new(None) };
}

/// The custom languages of the `with` this thread runs in, else those the last `load` found (none
/// before it).
pub fn current() -> Arc<Customs> {
    if let Some(c) = SCOPED.with(|s| s.borrow().clone()) {
        return c;
    }
    Arc::clone(&CURRENT.read().unwrap_or_else(|p| p.into_inner()))
}

/// Runs `f` with `c` as this thread's `current`: an update parses under its own root's tables
/// while another root's are loaded (two `Repo`s of the Python module side by side).
pub fn with<T>(c: &Arc<Customs>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<Customs>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SCOPED.with(|s| *s.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(SCOPED.with(|s| s.replace(Some(Arc::clone(c)))));
    f()
}

/// The user's configuration file, when there is one.
fn user_file() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SCRUTATE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join("scrutate").join("config.toml")).filter(|p| p.is_file())
}

/// Reads the tables that apply to `root`, checks them, and makes them `current`.
pub fn load(root: &Path) -> Result<Arc<Customs>> {
    let c = of_root(root, None)?;
    *CURRENT.write().unwrap_or_else(|p| p.into_inner()) = Arc::clone(&c);
    Ok(c)
}

/// The tables of the index a search started in `dir` uses (the one above it, else `dir`'s own), as
/// the command line reads them; `current` is left alone.
pub fn for_search(dir: &Path) -> Result<Arc<Customs>> {
    of_root(&crate::cli::index_above(dir).unwrap_or_else(|| dir.to_path_buf()), None)
}

/// The tables that apply to `root`, checked, with `current` left alone: `known` (or `current`)
/// when its texts are the ones found, so an unchanged table is not compiled again.
pub fn of_root(root: &Path, known: Option<&Arc<Customs>>) -> Result<Arc<Customs>> {
    let own = root.join(".scrutate.toml");
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    if let Some(text) = crate::files::root_config_text(root) {
        files.push((own.clone(), text));
    }
    if let Some(user) = user_file() {
        let same = std::fs::canonicalize(&user).ok().is_some_and(|u| std::fs::canonicalize(&own).ok() == Some(u));
        if let (false, Ok(text)) = (same, std::fs::read_to_string(&user)) {
            files.push((user, text));
        }
    }
    if let Some(k) = known.filter(|k| k.files == files) {
        return Ok(Arc::clone(k));
    }
    let cur = current();
    if cur.files == files {
        return Ok(cur);
    }
    let mut tables = Vec::new();
    for (i, (path, text)) in files.iter().enumerate() {
        let source = if i == 0 && *path == own { ".scrutate.toml".to_string() } else { path.display().to_string() };
        tables.push((path, source, text, crate::config::parse(path, text)?.language));
    }
    // every pattern of every table shares the size budget one regex has: a repository's table of
    // many large patterns is refused at once, where each pattern had it whole (gigabytes)
    let patterns = tables.iter().flat_map(|t| &t.3).map(|l| l.definitions.len()).sum::<usize>().max(1);
    let all = crate::instant::Pattern::default();
    let limits = (all.regex_size_limit / patterns, all.dfa_size_limit / patterns);
    let mut langs: Vec<Lang> = Vec::new();
    for (path, source, text, table) in tables {
        for l in table {
            let lang = check(l, path, &source, text, &langs, limits)?;
            langs.push(lang);
        }
    }
    Ok(Arc::new(Customs { langs, files }))
}

/// The first line of a regex error that says what is wrong.
fn regex_error(e: &regex::Error) -> String {
    let s = e.to_string();
    s.lines().find_map(|l| l.strip_prefix("error: ")).unwrap_or(&s).trim().replace('\n', " ")
}

fn check(l: LanguageCfg, path: &Path, source: &str, text: &str, seen: &[Lang], (size, dfa): (usize, usize)) -> Result<Lang> {
    let err = |span: Range<usize>, msg: String| anyhow!("{}: {msg}", at(path, text, span.start));
    let name = l.name.get_ref().clone();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(err(l.name.span(), format!("language name {name:?}: use letters, digits, `_` and `-`")));
    }
    if crate::langs::by_id(&name).is_some() || crate::instant::builtin_type_globs(&name).is_some() {
        return Err(err(l.name.span(), format!("language name {name:?} is a built-in language or file type")));
    }
    if seen.iter().any(|s| s.id == name) {
        return Err(err(l.name.span(), format!("language {name:?} is defined twice")));
    }
    let span = l.extensions.span();
    if l.extensions.get_ref().is_empty() {
        return Err(err(span, format!("language {name:?} names no extension")));
    }
    let mut exts: Vec<String> = Vec::new();
    for e in l.extensions.get_ref() {
        if e.is_empty() || e.starts_with('.') || !e.chars().all(|c| c.is_ascii_alphanumeric() || "_.+-".contains(c)) {
            return Err(err(span.clone(), format!("extension {e:?}: write it without the dot, in letters, digits, `_`, `+`, `-` and `.`")));
        }
        let ext = format!(".{}", e.to_lowercase());
        let probe = format!("x{ext}");
        // a built-in language that claims a whole file name (`CMakeLists.txt`) claims its extension too
        let named = || crate::langs::LANGS.iter().copied().find(|s| s.files.iter().any(|f| f.to_lowercase().ends_with(&ext)));
        if let Some(spec) = crate::langs::spec_for(&probe).or_else(named) {
            return Err(err(span.clone(), format!("extension {e:?} belongs to the built-in language {}", spec.id)));
        }
        if !crate::parse::is_indexable(&probe) {
            return Err(err(span.clone(), format!("extension {e:?} marks data the index never reads")));
        }
        if let Some(s) = seen.iter().find(|s| s.exts.contains(&ext)) {
            return Err(err(span.clone(), format!("extension {e:?} belongs to the language {} already", s.id)));
        }
        if !exts.contains(&ext) {
            exts.push(ext);
        }
    }
    let line_comment = match l.line_comment {
        Some(c) if c.get_ref().trim().is_empty() => return Err(err(c.span(), "line_comment is empty".into())),
        c => c.map(|c| c.into_inner()),
    };
    let block_comment = match l.block_comment {
        Some(b) => match b.get_ref().as_slice() {
            [open, close] if !open.trim().is_empty() && !close.trim().is_empty() => Some((open.clone(), close.clone())),
            _ => return Err(err(b.span(), "block_comment is two strings: the opener and the closer".into())),
        },
        None => None,
    };
    let mut defs = Vec::new();
    for d in l.definitions {
        let kind = d.kind.get_ref();
        let Some(&kind) = KINDS.iter().find(|k| *k == kind) else {
            return Err(err(d.kind.span(), format!("definition kind {kind:?} is not one of {}", KINDS.join(", "))));
        };
        let re = regex::RegexBuilder::new(d.pattern.get_ref())
            .size_limit(size)
            .dfa_size_limit(dfa)
            .build()
            .map_err(|e| err(d.pattern.span(), format!("the pattern of a {kind} definition does not compile: {}", regex_error(&e))))?;
        if re.captures_len() < 2 {
            return Err(err(d.pattern.span(), format!("the pattern of a {kind} definition has no group: its first group `(...)` is the name")));
        }
        defs.push((kind, re));
    }
    let mut parts: Vec<String> = vec![name.clone(), exts.join(" ")];
    parts.push(line_comment.clone().unwrap_or_default());
    parts.push(block_comment.as_ref().map(|(o, c)| format!("{o} {c}")).unwrap_or_default());
    parts.extend(defs.iter().map(|(k, re)| format!("{k} {}", re.as_str())));
    let bytes: Vec<&[u8]> = parts.iter().map(|p| p.as_bytes()).collect();
    let sig = crate::tscache::hash(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    Ok(Lang { id: name, exts, line_comment, block_comment, defs, source: source.to_string(), sig })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_names_the_extensions_whose_language_changed() {
        assert_eq!(changed("{}", "{}"), Vec::<String>::new());
        assert_eq!(changed("{}", r#"{".myl":"a"}"#), vec![".myl"]);
        assert_eq!(changed(r#"{".myl":"a",".x":"b"}"#, r#"{".myl":"c",".x":"b"}"#), vec![".myl"]);
        assert_eq!(changed(r#"{".myl":"a"}"#, "{}"), vec![".myl"]);
    }
}
