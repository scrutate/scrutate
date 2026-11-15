//! A dependency's source at the version the repository's lockfile pins (`symbol --deps`,
//! `read --deps`, the tools' `scope: "deps"`). The package an imported name belongs to comes from
//! the lockfile, its files from where that version is installed (the cargo registry,
//! `node_modules`, `.venv`, the Go module cache), and its answers from an index of a copy of it in
//! the user cache. The repository's own index never holds it. Every name and version a lockfile
//! gives is untrusted: it must fit its ecosystem's grammar, and the path built from it must stay in
//! its store.

use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::ops::{Answer, Op};
use crate::search::Searcher;

/// Files and bytes one package's copy holds at most; a larger one is not indexed.
pub const MAX_FILES: usize = 20_000;
pub const MAX_BYTES: u64 = 256 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eco {
    Cargo,
    Npm,
    Python,
    Go,
}

impl Eco {
    fn key(self) -> &'static str {
        match self {
            Eco::Cargo => "cargo",
            Eco::Npm => "npm",
            Eco::Python => "python",
            Eco::Go => "go",
        }
    }

    fn store(self) -> &'static str {
        match self {
            Eco::Cargo => "the cargo registry",
            Eco::Npm => "node_modules",
            Eco::Python => ".venv",
            Eco::Go => "the Go module cache",
        }
    }

    fn sync(self, lockfile: &str) -> &'static str {
        match (self, lockfile) {
            (Eco::Cargo, _) => "cargo fetch",
            (Eco::Npm, _) => "npm ci",
            (Eco::Python, "poetry.lock") => "poetry install --sync",
            (Eco::Python, _) => "uv sync",
            (Eco::Go, _) => "go mod download",
        }
    }
}

/// Where a locked package's files come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum From {
    /// the ecosystem's store of downloaded packages
    Store,
    /// a directory the lockfile or manifest names, relative to the root ("" for a workspace member)
    Path(String),
    /// a source this does not read (git, a URL)
    Other(String),
}

#[derive(Clone, Debug)]
pub struct Locked {
    pub eco: Eco,
    pub name: String,
    pub version: String,
    pub lockfile: &'static str,
    pub from: From,
}

pub const LOCKFILES: [&str; 5] = ["Cargo.lock", "package-lock.json", "uv.lock", "poetry.lock", "go.mod"];

/// The packages every lockfile at `root` pins, and the lockfiles read.
pub fn locked(root: &Path) -> (Vec<Locked>, Vec<&'static str>) {
    let mut out = Vec::new();
    let mut read = Vec::new();
    for lockfile in LOCKFILES {
        let Ok(text) = std::fs::read_to_string(root.join(lockfile)) else { continue };
        read.push(lockfile);
        out.extend(match lockfile {
            "Cargo.lock" => cargo_lock(&text, &std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default()),
            "package-lock.json" => npm_lock(&text),
            "uv.lock" => uv_lock(&text),
            "poetry.lock" => poetry_lock(&text),
            _ => go_mod(&text),
        });
    }
    (out, read)
}

fn cargo_lock(text: &str, manifest: &str) -> Vec<Locked> {
    let Ok(t) = text.parse::<toml::Table>() else { return Vec::new() };
    let manifest = manifest.parse::<toml::Table>().unwrap_or_default();
    // a path dependency of the root manifest, by the package it names
    let path_of = |name: &str| -> Option<String> {
        let ws = manifest.get("workspace").and_then(|w| w.get("dependencies"));
        let tables = ["dependencies", "dev-dependencies", "build-dependencies"].map(|k| manifest.get(k));
        tables.into_iter().chain([ws]).flatten().filter_map(|d| d.as_table()).flat_map(|d| d.iter()).find_map(|(k, d)| {
            let named = d.get("package").and_then(|p| p.as_str()).unwrap_or(k);
            (named == name).then(|| d.get("path")?.as_str().map(str::to_string)).flatten()
        })
    };
    let packages = t.get("package").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    packages
        .iter()
        .filter_map(|p| {
            let (name, version) = (p.get("name")?.as_str()?.to_string(), p.get("version")?.as_str()?.to_string());
            let from = match p.get("source").and_then(|s| s.as_str()) {
                Some(s) if s.starts_with("registry+") || s.starts_with("sparse+") => From::Store,
                Some(s) => From::Other(s.split('+').next().unwrap_or(s).to_string()),
                None => From::Path(path_of(&name).unwrap_or_default()),
            };
            Some(Locked { eco: Eco::Cargo, name, version, lockfile: "Cargo.lock", from })
        })
        .collect()
}

fn npm_lock(text: &str) -> Vec<Locked> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let lock = |name: String, version: String, from: From| Locked { eco: Eco::Npm, name, version, lockfile: "package-lock.json", from };
    if let Some(packages) = v["packages"].as_object() {
        return packages
            .iter()
            .filter_map(|(key, e)| {
                // only the packages node_modules holds at its top: a nested one is another's copy
                let name = key.strip_prefix("node_modules/").filter(|n| !n.contains("/node_modules/"))?.to_string();
                let resolved = e["resolved"].as_str().unwrap_or("");
                if e["link"].as_bool() == Some(true) {
                    let version = packages.get(resolved).and_then(|t| t["version"].as_str()).unwrap_or("").to_string();
                    return Some(lock(name, version, From::Path(resolved.to_string())));
                }
                let version = e["version"].as_str()?.to_string();
                let from = match resolved {
                    r if r.starts_with("git") => From::Other("git".into()),
                    r if r.starts_with("file:") => From::Path(r["file:".len()..].to_string()),
                    _ => From::Store,
                };
                Some(lock(name, version, from))
            })
            .collect();
    }
    let deps = v["dependencies"].as_object().cloned().unwrap_or_default();
    deps.into_iter()
        .filter_map(|(name, e)| {
            let version = e["version"].as_str()?.to_string();
            let from = match version.as_str() {
                v if v.starts_with("file:") => From::Path(v["file:".len()..].to_string()),
                v if v.starts_with("git") || v.contains("://") => From::Other("git".into()),
                _ => From::Store,
            };
            Some(lock(name, version, from))
        })
        .collect()
}

fn uv_lock(text: &str) -> Vec<Locked> {
    let Ok(t) = text.parse::<toml::Table>() else { return Vec::new() };
    let packages = t.get("package").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    packages
        .iter()
        .filter_map(|p| {
            let (name, version) = (p.get("name")?.as_str()?.to_string(), p.get("version").and_then(|v| v.as_str()).unwrap_or("").to_string());
            let source = p.get("source").and_then(|s| s.as_table());
            let at = |k: &str| source.and_then(|s| s.get(k)).and_then(|v| v.as_str()).map(str::to_string);
            let from = if source.is_none() || at("registry").is_some() {
                From::Store
            } else if let Some(dir) = at("editable").or_else(|| at("virtual")).or_else(|| at("directory")).or_else(|| at("path")) {
                From::Path(dir)
            } else {
                From::Other(source.and_then(|s| s.keys().next().cloned()).unwrap_or_default())
            };
            Some(Locked { eco: Eco::Python, name, version, lockfile: "uv.lock", from })
        })
        .collect()
}

fn poetry_lock(text: &str) -> Vec<Locked> {
    let Ok(t) = text.parse::<toml::Table>() else { return Vec::new() };
    let packages = t.get("package").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    packages
        .iter()
        .filter_map(|p| {
            let (name, version) = (p.get("name")?.as_str()?.to_string(), p.get("version")?.as_str()?.to_string());
            let source = p.get("source").and_then(|s| s.as_table());
            let kind = source.and_then(|s| s.get("type")).and_then(|v| v.as_str()).unwrap_or("");
            let url = source.and_then(|s| s.get("url")).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let from = match kind {
                "directory" | "file" => From::Path(url),
                "git" | "url" => From::Other(kind.to_string()),
                _ => From::Store,
            };
            Some(Locked { eco: Eco::Python, name, version, lockfile: "poetry.lock", from })
        })
        .collect()
}

fn go_mod(text: &str) -> Vec<Locked> {
    let mut required = Vec::new();
    let mut replaced: Vec<(String, Option<String>, String, Option<String>)> = Vec::new();
    let mut block = "";
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if line == ")" {
            block = "";
            continue;
        }
        let (verb, rest) = match line.split_once(char::is_whitespace) {
            Some((v @ ("require" | "replace"), r)) => (v, r.trim()),
            _ if !block.is_empty() => (block, line),
            _ => continue,
        };
        if rest == "(" {
            block = verb;
            continue;
        }
        if verb == "require" {
            if let [m, v] = rest.split_whitespace().collect::<Vec<_>>()[..] {
                required.push((m.to_string(), v.to_string()));
            }
        } else if let Some((from, to)) = rest.split_once("=>") {
            let (f, t): (Vec<&str>, Vec<&str>) = (from.split_whitespace().collect(), to.split_whitespace().collect());
            if let (Some(fm), Some(tm)) = (f.first(), t.first()) {
                replaced.push((fm.to_string(), f.get(1).map(|s| s.to_string()), tm.to_string(), t.get(1).map(|s| s.to_string())));
            }
        }
    }
    required
        .into_iter()
        .map(|(module, version)| {
            let r = replaced.iter().find(|(m, v, ..)| *m == module && v.as_ref().is_none_or(|v| *v == version));
            let (name, version, from) = match r {
                Some((_, _, to, None)) => (module, version, From::Path(to.clone())),
                Some((_, _, to, Some(v))) if to.starts_with('.') || to.starts_with('/') => (module, v.clone(), From::Path(to.clone())),
                Some((_, _, to, Some(v))) => (to.clone(), v.clone(), From::Store),
                None => (module, version, From::Store),
            };
            Locked { eco: Eco::Go, name, version, lockfile: "go.mod", from }
        })
        .collect()
}

/// One part of a path: no separator, no `.` or `..`, nothing a file system reads otherwise.
fn plain_part(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || "-._~+".contains(c))
}

fn valid_name(eco: Eco, name: &str) -> bool {
    let simple = |s: &str| plain_part(s) && !s.starts_with('.') && !s.contains('+');
    match eco {
        Eco::Cargo | Eco::Python => simple(name) && name.len() <= 214,
        Eco::Npm => {
            let parts: Vec<&str> = name.splitn(2, '/').collect();
            name.len() <= 214
                && match parts[..] {
                    [one] => simple(one),
                    [scope, pkg] => scope.len() > 1 && scope.starts_with('@') && simple(&scope[1..]) && simple(pkg),
                    _ => false,
                }
        }
        Eco::Go => name.len() <= 500 && name.split('/').all(simple),
    }
}

fn valid_version(eco: Eco, v: &str) -> bool {
    plain_part(v) && !v.starts_with('.') && !v.contains("..") && v.len() <= 128 && (eco != Eco::Go || v.starts_with('v'))
}

/// PEP 503: lower case, every run of `-`, `_` and `.` one `-`.
fn normalized(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// The module cache's spelling: an upper-case letter as `!` and its lower case.
fn go_escaped(s: &str) -> String {
    s.chars().flat_map(|c| if c.is_ascii_uppercase() { vec!['!', c.to_ascii_lowercase()] } else { vec![c] }).collect()
}

fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"].iter().find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty())).map(PathBuf::from)
}

fn env_dir(k: &str) -> Option<PathBuf> {
    std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Where the copies live: `SCRUTATE_CACHE_DIR`, else the platform's user cache.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(d) = env_dir("SCRUTATE_CACHE_DIR") {
        return Ok(d);
    }
    if let Some(d) = env_dir("XDG_CACHE_HOME") {
        return Ok(d.join("scrutate"));
    }
    if cfg!(windows) {
        if let Some(d) = env_dir("LOCALAPPDATA") {
            return Ok(d.join("scrutate"));
        }
    }
    let home = home().ok_or_else(|| anyhow!("no user cache directory: set SCRUTATE_CACHE_DIR"))?;
    Ok(if cfg!(target_os = "macos") { home.join("Library/Caches/scrutate") } else { home.join(".cache/scrutate") })
}

/// A Python distribution `.venv` holds.
struct Dist {
    name: String,
    version: String,
    site: PathBuf,
    /// its files, relative to `site`
    files: Vec<String>,
}

impl Dist {
    /// The names it is imported by: the first part of each file outside its metadata.
    fn tops(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .files
            .iter()
            .filter_map(|f| f.split('/').next())
            .filter(|t| !t.ends_with(".dist-info") && !t.ends_with(".data") && *t != "__pycache__" && *t != "..")
            .map(|t| t.strip_suffix(".py").unwrap_or(t).to_string())
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

fn site_packages(root: &Path) -> Vec<PathBuf> {
    let venv = root.join(".venv");
    let mut out = vec![venv.join("Lib/site-packages")];
    if let Ok(rd) = std::fs::read_dir(venv.join("lib")) {
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("site-packages")).collect();
        dirs.sort();
        out.extend(dirs);
    }
    out.retain(|d| d.is_dir());
    out
}

fn dists(root: &Path) -> Vec<Dist> {
    let mut out = Vec::new();
    for site in site_packages(root) {
        let Ok(rd) = std::fs::read_dir(&site) else { continue };
        let mut infos: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".dist-info")).collect();
        infos.sort();
        for info in infos {
            let Some((name, version)) = info.trim_end_matches(".dist-info").split_once('-') else { continue };
            let record = std::fs::read_to_string(site.join(&info).join("RECORD")).unwrap_or_default();
            let files = record
                .lines()
                .filter_map(|l| if let Some(q) = l.strip_prefix('"') { q.split('"').next() } else { l.split(',').next() })
                .filter(|f| !f.is_empty() && Path::new(f).components().all(|c| matches!(c, Component::Normal(_))))
                .map(|f| f.replace('\\', "/"))
                .collect();
            out.push(Dist { name: name.to_string(), version: version.to_string(), site: site.clone(), files });
        }
    }
    out
}

/// The names `l` is imported by.
fn import_names(l: &Locked, dists: &[Dist]) -> Vec<String> {
    match l.eco {
        Eco::Cargo => vec![l.name.replace('-', "_")],
        Eco::Npm => vec![l.name.clone()],
        Eco::Go => {
            let parts: Vec<&str> = l.name.split('/').collect();
            let major = |p: &str| p.len() > 1 && p.starts_with('v') && p[1..].bytes().all(|b| b.is_ascii_digit());
            let last = match parts[..] {
                [.., before, last] if major(last) => before,
                [.., last] => last,
                [] => "",
            };
            vec![l.name.clone(), last.to_string()]
        }
        Eco::Python => {
            let mine: Vec<String> = dists.iter().filter(|d| normalized(&d.name) == normalized(&l.name)).flat_map(Dist::tops).collect();
            if mine.is_empty() {
                vec![normalized(&l.name).replace('-', "_")]
            } else {
                mine
            }
        }
    }
}

/// The package `name` starts with and the rest of it: the longest start that is a locked package's
/// import name, before a `.` or `::`.
fn split<'n>(root: &Path, name: &'n str) -> Result<(Locked, &'n str)> {
    let (all, read) = locked(root);
    if read.is_empty() {
        bail!("no lockfile at the root ({}): --deps reads the version a lockfile pins", LOCKFILES.join(", "));
    }
    let dists = if all.iter().any(|l| l.eco == Eco::Python) { dists(root) } else { Vec::new() };
    let names: Vec<(Vec<String>, &Locked)> = all.iter().map(|l| (import_names(l, &dists), l)).collect();
    let mut cuts: Vec<(usize, usize)> = name.match_indices("::").map(|(i, _)| (i, 2)).chain(name.match_indices('.').map(|(i, _)| (i, 1))).collect();
    cuts.push((name.len(), 0));
    cuts.sort_by_key(|c| std::cmp::Reverse(c.0));
    for (at, len) in cuts {
        let start = &name[..at];
        let hits: Vec<&Locked> = names.iter().filter(|(n, _)| n.iter().any(|n| n == start)).map(|(_, l)| *l).collect();
        let Some(first) = hits.first() else { continue };
        if let Some(other) = hits.iter().find(|l| l.eco != first.eco || l.name != first.name) {
            bail!("{start} names two packages, {} in {} and {} in {}: --deps cannot tell which", first.name, first.lockfile, other.name, other.lockfile);
        }
        let rest = &name[(at + len).min(name.len())..];
        if rest.is_empty() {
            let sep = if first.eco == Eco::Cargo { "::" } else { "." };
            bail!("name a definition of {start}: {start}{sep}NAME (--deps takes PACKAGE.NAME)");
        }
        return Ok(((*first).clone(), rest));
    }
    bail!("no package of {} names {}: --deps takes PACKAGE.NAME, PACKAGE as the code imports it", read.join(", "), crate::fmt::echo(name))
}

/// A package's files where its locked version is installed.
struct Installed {
    /// the package's directory, canonical
    dir: PathBuf,
    /// what the copy's paths are relative to: `dir`, or for Python the site-packages directory
    base: PathBuf,
    /// the files to copy, relative to `base` (Python: its RECORD); none: every file under `dir`
    files: Option<Vec<String>>,
}

fn refused(l: &Locked, why: &str) -> anyhow::Error {
    anyhow!("refused: {} names {} at {}: {why}", l.lockfile, crate::fmt::echo(&l.name), crate::fmt::echo(&l.version))
}

/// `dir` canonical, when it stays under `store`.
fn inside(l: &Locked, store: &Path, dir: &Path) -> Result<Option<PathBuf>> {
    let Ok(store) = std::fs::canonicalize(store) else { return Ok(None) };
    match std::fs::canonicalize(dir) {
        Ok(d) if d.starts_with(&store) && d != store => Ok(Some(d)),
        Ok(_) => Err(refused(l, &format!("its directory leaves {}", l.eco.store()))),
        Err(_) => Ok(None),
    }
}

fn installed(root: &Path, l: &Locked) -> Result<Installed> {
    if !valid_name(l.eco, &l.name) {
        return Err(refused(l, "that is no package name"));
    }
    if !valid_version(l.eco, &l.version) {
        return Err(refused(l, "that is no version"));
    }
    let missing = |where_: String| anyhow!("{} {} is not in {where_}: run {}", l.name, l.version, l.eco.sync(l.lockfile));
    let whole = |dir: PathBuf| Installed { base: dir.clone(), dir, files: None };
    match l.eco {
        Eco::Cargo => {
            let home = env_dir("CARGO_HOME").or_else(|| home().map(|h| h.join(".cargo"))).ok_or_else(|| missing("the cargo registry".into()))?;
            let src = home.join("registry").join("src");
            let mut indexes: Vec<PathBuf> = std::fs::read_dir(&src).into_iter().flatten().flatten().map(|e| e.path()).collect();
            indexes.sort();
            for index in indexes {
                if let Some(dir) = inside(l, &src, &index.join(format!("{}-{}", l.name, l.version)))? {
                    return Ok(whole(dir));
                }
            }
            Err(missing(format!("the cargo registry ({})", src.display())))
        }
        Eco::Npm => {
            let modules = root.join("node_modules");
            let dir = inside(l, &modules, &modules.join(&l.name))?.ok_or_else(|| missing("node_modules".into()))?;
            let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap_or_default()).unwrap_or_default();
            match manifest["version"].as_str() {
                Some(v) if v == l.version => Ok(whole(dir)),
                Some(v) => bail!("{} is locked at {} but node_modules has {}: run npm ci", l.name, l.version, crate::fmt::echo(v)),
                None => Err(missing("node_modules".into())),
            }
        }
        Eco::Python => {
            let all = dists(root);
            let mine: Vec<&Dist> = all.iter().filter(|d| normalized(&d.name) == normalized(&l.name)).collect();
            let Some(d) = mine.iter().find(|d| d.version == l.version) else {
                return match mine.first() {
                    Some(d) => bail!("{} is locked at {} but .venv has {}: run {}", l.name, l.version, crate::fmt::echo(&d.version), l.eco.sync(l.lockfile)),
                    None => Err(missing(".venv".into())),
                };
            };
            let site = std::fs::canonicalize(&d.site)?;
            let files = d.files.iter().filter(|f| !f.split('/').next().unwrap_or("").ends_with(".dist-info")).cloned().collect();
            Ok(Installed { dir: site.clone(), base: site, files: Some(files) })
        }
        Eco::Go => {
            let cache = env_dir("GOMODCACHE")
                .or_else(|| {
                    std::env::var_os("GOPATH").and_then(|p| std::env::split_paths(&p).next()).filter(|p| !p.as_os_str().is_empty()).map(|p| p.join("pkg/mod"))
                })
                .or_else(|| home().map(|h| h.join("go/pkg/mod")))
                .ok_or_else(|| missing("the Go module cache".into()))?;
            let dir = cache.join(format!("{}@{}", go_escaped(&l.name), go_escaped(&l.version)));
            Ok(whole(inside(l, &cache, &dir)?.ok_or_else(|| missing(format!("the Go module cache ({})", cache.display())))?))
        }
    }
}

fn private_dirs(dir: &Path) -> Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir).with_context(|| format!("cannot create {}", dir.display()))
}

/// Copies the package into `to`, never through a link that leaves it, and stops at the caps.
fn copy(l: &Locked, inst: &Installed, to: &Path, (max_files, max_bytes): (usize, u64)) -> Result<()> {
    let (mut files, mut bytes) = (0usize, 0u64);
    let mut one = |from: &Path, rel: &Path| -> Result<()> {
        let Ok(meta) = std::fs::symlink_metadata(from) else { return Ok(()) };
        let real = if meta.file_type().is_symlink() {
            match std::fs::canonicalize(from) {
                Ok(t) if t.starts_with(&inst.dir) && t.is_file() => t,
                _ => return Ok(()),
            }
        } else if meta.is_file() {
            from.to_path_buf()
        } else {
            return Ok(());
        };
        files += 1;
        bytes += std::fs::metadata(&real)?.len();
        if files > max_files || bytes > max_bytes {
            bail!("{} {} is over the copy's cap of {max_files} files and {} MiB: not indexed", l.name, l.version, max_bytes >> 20);
        }
        let dest = to.join(rel);
        private_dirs(dest.parent().unwrap_or(to))?;
        std::fs::copy(&real, &dest).with_context(|| format!("cannot copy {}", real.display()))?;
        Ok(())
    };
    match &inst.files {
        Some(list) => {
            for f in list {
                let rel = Path::new(f);
                // a file under a link to a directory: only the link's own target is checked above
                let parent = inst.base.join(rel).parent().and_then(|p| std::fs::canonicalize(p).ok());
                if parent.is_some_and(|p| p.starts_with(&inst.dir)) {
                    one(&inst.base.join(rel), rel)?;
                }
            }
        }
        None => {
            let mut stack = vec![PathBuf::new()];
            while let Some(rel) = stack.pop() {
                let mut entries: Vec<_> = std::fs::read_dir(inst.dir.join(&rel))?.flatten().collect();
                entries.sort_by_key(|e| e.file_name());
                for e in entries {
                    let name = e.file_name();
                    let r = rel.join(&name);
                    let ty = e.file_type()?;
                    if ty.is_dir() {
                        if !matches!(name.to_str(), Some(".git" | "node_modules" | ".scrutate")) {
                            stack.push(r);
                        }
                    } else {
                        one(&e.path(), &r)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// The cached copy of `l` (its `src` directory), made on first use: copied into a temporary
/// directory and renamed into place, so a reader never sees half a copy.
fn cached(l: &Locked, inst: &Installed, caps: (usize, u64)) -> Result<PathBuf> {
    let eco = cache_dir()?.join("deps").join(l.eco.key());
    let key = match l.eco {
        Eco::Go => format!("{}@{}", go_escaped(&l.name).replace('/', "+"), go_escaped(&l.version)),
        Eco::Python => format!("{}@{}", normalized(&l.name), l.version),
        _ => format!("{}@{}", l.name.replace('/', "+"), l.version),
    };
    let entry = eco.join(key);
    if entry.join("done").is_file() {
        return Ok(entry.join("src"));
    }
    private_dirs(&eco)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = eco.join(format!(".tmp-{}-{nanos}", std::process::id()));
    let made = (|| -> Result<()> {
        private_dirs(&tmp.join("src"))?;
        copy(l, inst, &tmp.join("src"), caps)?;
        std::fs::write(tmp.join("done"), format!("{} {} from {}\n", l.name, l.version, inst.dir.display()))?;
        Ok(())
    })();
    if let Err(e) = made {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if std::fs::rename(&tmp, &entry).is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
        if !entry.join("done").is_file() {
            bail!("cannot put the copy of {} {} in {}", l.name, l.version, entry.display());
        }
    }
    Ok(entry.join("src"))
}

/// What an answer says of the package it comes from.
#[derive(Debug)]
pub struct Note {
    pub name: String,
    pub version: String,
    pub lockfile: &'static str,
    /// the package's directory as an answer names it
    pub path: String,
    /// its files are the repository's own
    pub local: bool,
}

impl Note {
    pub fn line(&self) -> String {
        format!("{} {} ({}): {}", self.name, self.version, self.lockfile, crate::fmt::shown(&self.path))
    }

    pub fn json(&self) -> Value {
        json!({"name": self.name, "version": self.version, "lockfile": self.lockfile, "path": self.path})
    }
}

/// `p` as an answer names it: from the root when it is inside, else whole.
fn shown_path(root: &Path, p: &Path) -> String {
    match p.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

/// The paths of a symbol or read answer, rewritten by `f`; an answer of any other kind as it is.
fn repath(a: &mut Answer, f: impl Fn(&str) -> String) {
    match a {
        Answer::Hits { hits, .. } => hits.iter_mut().for_each(|h| h.path = f(&h.path)),
        Answer::Definitions(defs, ..) => {
            for (row, text, inner) in defs.iter_mut() {
                let new = f(&row.path);
                *text = text.replace(&format!("path={} ", row.path), &format!("path={new} "));
                inner.iter_mut().for_each(|m| m.path = new.clone());
                row.path = new;
            }
        }
        _ => {}
    }
}

/// The definitions `rest` names, by the op asked for: as written, then by its last part.
fn lookup(run: &mut dyn FnMut(&Op) -> Result<Answer>, rest: &str, read: bool, limit: usize, exact: bool) -> Result<Answer> {
    let op = |n: &str| {
        if read {
            Op::Read { path: None, symbol: Some(n.to_string()), start: 1, end: None, history: 0 }
        } else {
            Op::Symbol { name: n.to_string(), limit, exact }
        }
    };
    let last = rest.rsplit("::").next().unwrap_or(rest).rsplit('.').next().unwrap_or(rest);
    let first = run(&op(rest));
    match first {
        Ok(a) if !a.is_empty() => Ok(a),
        _ if last != rest => run(&op(last)),
        r => r,
    }
}

/// The answer to `symbol --deps` (`read`: `read --symbol --deps`).
pub fn run(s: &mut Searcher, name: &str, read: bool, limit: usize, exact: bool) -> Result<Answer> {
    let root = std::fs::canonicalize(&s.ix.root).unwrap_or_else(|_| s.ix.root.clone());
    let (l, rest) = split(&root, name)?;
    match &l.from {
        From::Other(how) => bail!("{} {} comes from {how}, which --deps does not read", l.name, l.version),
        From::Path(dir) => {
            let dir = dir.trim_start_matches("./").trim_end_matches('/');
            let full = std::fs::canonicalize(root.join(dir)).ok().filter(|d| d.starts_with(&root));
            let Some(full) = full else {
                bail!("{} is {} in {}, outside the repository: refused", l.name, crate::fmt::echo(dir), l.lockfile);
            };
            let prefix = shown_path(&root, &full);
            let mut a = lookup(&mut |op| crate::ops::run_in(s, op), rest, read, limit, exact)?;
            if !prefix.is_empty() {
                let under = |p: &str| p.starts_with(&format!("{prefix}/"));
                match &mut a {
                    Answer::Hits { hits, .. } => hits.retain(|h| under(&h.path)),
                    Answer::Definitions(defs, ..) => defs.retain(|d| under(&d.0.path)),
                    _ => {}
                }
            }
            let note = Note { name: l.name.clone(), version: l.version.clone(), lockfile: l.lockfile, path: prefix, local: true };
            Ok(Answer::Package(Box::new(note), Box::new(a)))
        }
        From::Store => {
            let inst = installed(&root, &l)?;
            let src = cached(&l, &inst, (MAX_FILES, MAX_BYTES))?;
            let mut repo = crate::Repo::open_with(&src, crate::models::ModelSet::none())?;
            // a copy never changes once made, so it is indexed once and never looked at again
            let indexed = src.with_file_name("indexed");
            if !indexed.is_file() {
                repo.update(false)?;
                std::fs::write(&indexed, "")?;
            }
            let mut a = lookup(&mut |op| repo.run_as_is(op), rest, read, limit, exact)?;
            repath(&mut a, |p| shown_path(&root, &inst.base.join(p)));
            let note = Note { name: l.name.clone(), version: l.version.clone(), lockfile: l.lockfile, path: shown_path(&root, &inst.dir), local: false };
            Ok(Answer::Package(Box::new(note), Box::new(a)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_versions_keep_to_their_grammar() {
        for bad in ["../../x", "..", ".", "a/../b", "/etc", "a\\b", "a b", "", "a\0b", "C:x"] {
            for eco in [Eco::Cargo, Eco::Npm, Eco::Python, Eco::Go] {
                assert!(!valid_name(eco, bad), "{eco:?} name {bad:?}");
                assert!(!valid_version(eco, bad), "{eco:?} version {bad:?}");
            }
        }
        assert!(valid_name(Eco::Npm, "@types/node") && !valid_name(Eco::Npm, "@/node") && !valid_name(Eco::Npm, "@a/b/c"));
        assert!(valid_name(Eco::Go, "github.com/gin-gonic/gin") && !valid_name(Eco::Go, "github.com/../x"));
        assert!(valid_version(Eco::Go, "v0.0.0-20240101000000-abcdef123456") && !valid_version(Eco::Go, "1.0.0"));
        assert!(valid_version(Eco::Cargo, "1.0.0-rc.1+build.5"));
    }

    #[test]
    fn go_mod_requires_and_replaces() {
        let l = go_mod("module m\n\nrequire (\n\ta.com/x v1.0.0 // indirect\n\tb.com/y v2.0.0\n)\nrequire c.com/z v0.1.0\n\nreplace b.com/y => ./y\nreplace (\n\tc.com/z v0.1.0 => d.com/z v0.2.0\n)\n");
        let got: Vec<(String, String, From)> = l.into_iter().map(|l| (l.name, l.version, l.from)).collect();
        assert_eq!(
            got,
            [
                ("a.com/x".into(), "v1.0.0".into(), From::Store),
                ("b.com/y".into(), "v2.0.0".into(), From::Path("./y".into())),
                ("d.com/z".into(), "v0.2.0".into(), From::Store),
            ]
        );
    }

    #[test]
    fn the_copy_stops_at_its_caps_and_leaves_nothing_behind() {
        let top = std::env::temp_dir().join(format!("scrutate_packages_caps_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        let pkg = top.join("pkg");
        std::fs::create_dir_all(pkg.join("src")).unwrap();
        for i in 0..5 {
            std::fs::write(pkg.join(format!("src/f{i}.rs")), "fn f() {}\n").unwrap();
        }
        let dir = std::fs::canonicalize(&pkg).unwrap();
        let inst = Installed { base: dir.clone(), dir, files: None };
        let l = Locked { eco: Eco::Cargo, name: "p".into(), version: "1.0.0".into(), lockfile: "Cargo.lock", from: From::Store };
        std::env::set_var("SCRUTATE_CACHE_DIR", top.join("cache"));
        let e = cached(&l, &inst, (4, 1 << 20)).unwrap_err().to_string();
        assert!(e.contains("cap of 4 files"), "{e}");
        let e = cached(&l, &inst, (100, 20)).unwrap_err().to_string();
        assert!(e.contains("cap"), "{e}");
        // nothing half made is left to be taken for a copy
        assert_eq!(std::fs::read_dir(top.join("cache/deps/cargo")).unwrap().count(), 0);
        let src = cached(&l, &inst, (5, 1 << 20)).unwrap();
        assert_eq!(std::fs::read_dir(src.join("src")).unwrap().count(), 5);
        std::env::remove_var("SCRUTATE_CACHE_DIR");
        let _ = std::fs::remove_dir_all(&top);
    }
}
