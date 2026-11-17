//! A dependency's source at the version the repository's lockfile pins (`symbol --deps`,
//! `read --deps`, the tools' `scope: "deps"`). The package an imported name belongs to comes from
//! the lockfile, its files from where that version is installed (the cargo registry,
//! `node_modules`, `.venv`, the Go module cache), and its answers from an index of a copy of it in
//! the user cache. The repository's own index never holds it. Every name and version a lockfile
//! gives is untrusted: it must fit its ecosystem's grammar, and the path built from it must stay in
//! its store.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

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
    /// a go.mod requirement marked `// indirect`: no package of the repository imports it
    pub indirect: bool,
    /// a go.mod requirement replaced by another module: the path the code imports, where `name`
    /// is the replacement's, which the store holds
    pub imported_as: Option<String>,
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
                Some(s) => cargo_source(s),
                None => From::Path(path_of(&name).unwrap_or_default()),
            };
            Some(Locked { eco: Eco::Cargo, name, version, lockfile: "Cargo.lock", from, indirect: false, imported_as: None })
        })
        .collect()
}

/// Where a Cargo.lock `source` (`registry+URL`, `git+URL`) has a crate come from.
fn cargo_source(s: &str) -> From {
    if s.starts_with("registry+") || s.starts_with("sparse+") { From::Store } else { From::Other(s.split('+').next().unwrap_or(s).to_string()) }
}

fn npm_lock(text: &str) -> Vec<Locked> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let lock = |name: String, version: String, from: From| Locked {
        eco: Eco::Npm,
        name,
        version,
        lockfile: "package-lock.json",
        from,
        indirect: false,
        imported_as: None,
    };
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
            Some(Locked { eco: Eco::Python, name, version, lockfile: "uv.lock", from, indirect: false, imported_as: None })
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
            Some(Locked { eco: Eco::Python, name, version, lockfile: "poetry.lock", from, indirect: false, imported_as: None })
        })
        .collect()
}

fn go_mod(text: &str) -> Vec<Locked> {
    let mut required = Vec::new();
    let mut replaced: Vec<(String, Option<String>, String, Option<String>)> = Vec::new();
    let mut block = "";
    for line in text.lines() {
        let (line, comment) = line.split_once("//").unwrap_or((line, ""));
        let line = line.trim();
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
                required.push((m.to_string(), v.to_string(), comment.trim().starts_with("indirect")));
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
        .map(|(module, version, indirect)| {
            let r = replaced.iter().find(|(m, v, ..)| *m == module && v.as_ref().is_none_or(|v| *v == version));
            let (name, version, from, imported_as) = match r {
                Some((_, _, to, None)) => (module, version, From::Path(to.clone()), None),
                Some((_, _, to, Some(v))) if to.starts_with('.') || to.starts_with('/') => (module, v.clone(), From::Path(to.clone()), None),
                // another module: the store holds it by its own path, the code imports the required one
                Some((_, _, to, Some(v))) => (to.clone(), v.clone(), From::Store, Some(module).filter(|m| m != to)),
                None => (module, version, From::Store, None),
            };
            Locked { eco: Eco::Go, name, version, lockfile: "go.mod", from, indirect, imported_as }
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

/// `SCRUTATE_CACHE_DIR`, read once where a searcher is opened and carried from there.
pub fn env_cache_dir() -> Option<PathBuf> {
    env_dir("SCRUTATE_CACHE_DIR")
}

/// Where the copies live: `given` (`SCRUTATE_CACHE_DIR` as the searcher read it), else the
/// platform's user cache.
pub fn cache_dir(given: Option<&Path>) -> Result<PathBuf> {
    if let Some(d) = given {
        return Ok(d.to_path_buf());
    }
    user_cache(cfg!(windows), cfg!(target_os = "macos"), env_dir, home())
}

fn user_cache(windows: bool, macos: bool, var: impl Fn(&str) -> Option<PathBuf>, home: Option<PathBuf>) -> Result<PathBuf> {
    // Not XDG_CACHE_HOME on Windows: MSYS and Git Bash set it, and no ownership check runs there.
    if let Some(d) = var(if windows { "LOCALAPPDATA" } else { "XDG_CACHE_HOME" }) {
        return Ok(d.join("scrutate"));
    }
    let home = home.ok_or_else(|| anyhow!("no user cache directory: set SCRUTATE_CACHE_DIR"))?;
    Ok(if macos { home.join("Library/Caches/scrutate") } else { home.join(".cache/scrutate") })
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
    /// The names it is imported by: the first part of each file outside its metadata, and under a
    /// namespace package (a directory with no `__init__.py`, which several distributions share,
    /// as google-auth and protobuf share `google`, and google-cloud-core and google-cloud-storage
    /// `google.cloud`; or one whose `__init__.py` more than one distribution ships, `shared`, the
    /// pkgutil style of zope and backports) each part on down to the first package of its own
    /// (`google.cloud.storage`, `zope.interface`).
    fn tops(&self, shared: &std::collections::HashSet<String>) -> Vec<String> {
        let inits: std::collections::HashSet<&str> =
            self.files.iter().filter_map(|f| f.strip_suffix("/__init__.py")).filter(|d| !shared.contains(*d)).collect();
        let module = |t: &str| t.strip_suffix(".py").unwrap_or(t).to_string();
        let mut out: Vec<String> = Vec::new();
        for f in &self.files {
            let parts: Vec<&str> = f.split('/').collect();
            let Some(top) = parts.first().filter(|t| !t.ends_with(".dist-info") && !t.ends_with(".data") && **t != "__pycache__" && **t != "..") else {
                continue;
            };
            let mut name = module(top);
            out.push(name.clone());
            let mut depth = 1;
            while depth < parts.len() && parts[depth] != "__pycache__" && !inits.contains(parts[..depth].join("/").as_str()) {
                name = format!("{name}.{}", module(parts[depth]));
                out.push(name.clone());
                depth += 1;
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// The `site-packages` directories of `.venv` (`lib/*/site-packages`, `Lib/site-packages`) that
/// are the repository's own, reached through no link; and those that a link reaches.
fn site_packages(root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut found = vec![PathBuf::from(".venv/Lib/site-packages")];
    if own(root, Path::new(".venv/lib"))
        && let Ok(rd) = std::fs::read_dir(root.join(".venv/lib"))
    {
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| Path::new(".venv/lib").join(e.file_name()).join("site-packages")).collect();
        dirs.sort();
        found.extend(dirs);
    }
    let (mut mine, mut linked) = (Vec::new(), Vec::new());
    if !own(root, Path::new(".venv/lib")) && root.join(".venv/lib").is_dir() {
        linked.push(root.join(".venv/lib"));
    }
    for rel in found {
        if own(root, &rel) {
            mine.push(root.join(&rel));
        } else if root.join(&rel).is_dir() {
            linked.push(root.join(&rel));
        }
    }
    (mine, linked)
}

/// Whether `rel` is a directory of the repository reached from the root through no link.
fn own(root: &Path, rel: &Path) -> bool {
    let mut at = root.to_path_buf();
    rel.components().all(|c| {
        at.push(c);
        matches!(c, Component::Normal(_)) && std::fs::symlink_metadata(&at).is_ok_and(|m| m.is_dir() && !is_link(&m))
    })
}

fn dists(root: &Path) -> Vec<Dist> {
    let mut out = Vec::new();
    for site in site_packages(root).0 {
        let Ok(rd) = std::fs::read_dir(&site) else { continue };
        let mut infos: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".dist-info")).collect();
        infos.sort();
        for info in infos {
            let Some((name, version)) = info.trim_end_matches(".dist-info").split_once('-') else { continue };
            let record = std::fs::read_to_string(site.join(&info).join("RECORD")).unwrap_or_default();
            let files = record
                .lines()
                .filter_map(|l| if let Some(q) = l.strip_prefix('"') { q.split('"').next() } else { l.split(',').next() })
                // Separators are made `/` before the check: on Unix `a\..\..` is one Normal part.
                .map(|f| f.replace('\\', "/"))
                .filter(|f| !f.is_empty() && Path::new(f).components().all(|c| matches!(c, Component::Normal(_))))
                .collect();
            out.push(Dist { name: name.to_string(), version: version.to_string(), site: site.clone(), files });
        }
    }
    out
}

/// `vN`, a Go major version suffix.
fn go_major(p: &str) -> bool {
    p.len() > 1 && p.starts_with('v') && p[1..].bytes().all(|b| b.is_ascii_digit())
}

/// A Go module path's last part, a major version suffix `/vN` skipped.
fn go_last(module: &str) -> &str {
    let parts: Vec<&str> = module.split('/').collect();
    match parts[..] {
        [.., before, last] if go_major(last) => before,
        [.., last] => last,
        [] => "",
    }
}

/// The names `l` is imported by.
fn import_names(l: &Locked, dists: &[Dist]) -> Vec<String> {
    match l.eco {
        Eco::Cargo => vec![l.name.replace('-', "_")],
        Eco::Npm => vec![l.name.clone()],
        Eco::Go => {
            // the path the code imports, and the replacement's own when a fork of another name is used
            let path = l.imported_as.as_deref().unwrap_or(&l.name);
            let last = go_last(path);
            // the package name the code uses when it is not the last part: gopkg.in/yaml.v3 is
            // `yaml`, github.com/mattn/go-sqlite3 is `sqlite3`
            let cut = last.rsplit_once('.').filter(|(_, v)| go_major(v)).map_or(last, |(b, _)| b);
            let cut = cut.strip_prefix("go-").filter(|c| !c.is_empty()).unwrap_or(cut);
            let mut names = vec![path.to_string(), last.to_string()];
            if cut != last {
                names.push(cut.to_string());
            }
            if path != l.name {
                names.push(l.name.clone());
            }
            names
        }
        Eco::Python => {
            // a package directory whose __init__.py two distributions ship is a namespace
            let mut ships: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
            for d in dists {
                let own: std::collections::HashSet<&str> = d.files.iter().filter_map(|f| f.strip_suffix("/__init__.py")).collect();
                own.into_iter().for_each(|dir| *ships.entry(dir).or_default() += 1);
            }
            let shared: std::collections::HashSet<String> = ships.into_iter().filter(|(_, n)| *n > 1).map(|(d, _)| d.to_string()).collect();
            let mine: Vec<String> = dists.iter().filter(|d| normalized(&d.name) == normalized(&l.name)).flat_map(|d| d.tops(&shared)).collect();
            if mine.is_empty() { vec![normalized(&l.name).replace('-', "_")] } else { mine }
        }
    }
}

/// The package `name` starts with, the rest of it, and the package's directory inside it ("" for
/// its root): the longest start that is a locked package's import name, before a `.` or `::`; for
/// Go also a package in a module's subdirectory, by its import path or its name.
fn split<'n>(root: &Path, name: &'n str, imported: &dyn Fn(&str) -> bool) -> Result<(Locked, &'n str, String)> {
    let (all, read) = locked(root);
    if read.is_empty() {
        bail!("no lockfile at the root ({}): --deps reads the version a lockfile pins", LOCKFILES.join(", "));
    }
    let dists = if all.iter().any(|l| l.eco == Eco::Python) { dists(root) } else { Vec::new() };
    let names: Vec<(Vec<String>, &Locked)> = all.iter().map(|l| (import_names(l, &dists), l)).collect();
    let mut cuts: Vec<(usize, usize)> = name.match_indices("::").map(|(i, _)| (i, 2)).chain(name.match_indices('.').map(|(i, _)| (i, 1))).collect();
    cuts.push((name.len(), 0));
    cuts.sort_by_key(|c| std::cmp::Reverse(c.0));
    let mut missed = None;
    for (at, len) in cuts {
        let start = &name[..at];
        let mut hits: Vec<&Locked> = names.iter().filter(|(n, _)| n.iter().any(|n| n == start)).map(|(_, l)| *l).collect();
        if hits.iter().all(|l| l.eco == Eco::Go) && hits.iter().any(|l| l.name != hits[0].name) {
            // the repository's code imports only its direct requirements (an `// indirect` one
            // only a dependency imports); of two direct ones, the one its own Go files import, as
            // Go takes one name for one import in a file (`yaml redeclared` for two)
            if hits.iter().any(|l| !l.indirect) {
                hits.retain(|l| !l.indirect);
            }
            let mut modules: Vec<&str> = hits.iter().map(|l| l.imported_as.as_deref().unwrap_or(&l.name)).collect();
            modules.dedup();
            if modules.len() > 1 {
                let mine: Vec<&str> = modules.iter().copied().filter(|m| imported(m)).collect();
                let [one] = mine[..] else {
                    let echo = crate::fmt::echo;
                    bail!(
                        "{} names {} modules go.mod requires, {}, and the repository's Go files import {}: --deps cannot tell which; name the module by its import path ({}.NAME)",
                        echo(start),
                        modules.len(),
                        modules.iter().map(|m| echo(m)).collect::<Vec<_>>().join(" and "),
                        if mine.is_empty() { "neither" } else { "more than one" },
                        echo(modules[0])
                    );
                };
                hits.retain(|l| l.imported_as.as_deref().unwrap_or(&l.name) == one);
            }
        }
        let mut sub = String::new();
        if hits.is_empty()
            && at < name.len()
            && let Some((l, dir)) = go_subpackage(root, &all, start, &mut missed)?
        {
            hits.push(l);
            sub = dir;
        }
        let Some(first) = hits.first() else { continue };
        let echo = crate::fmt::echo;
        if let Some(other) = hits.iter().find(|l| l.eco != first.eco || l.name != first.name) {
            bail!(
                "{} names two packages, {} in {} and {} in {}: --deps cannot tell which",
                echo(start),
                echo(&first.name),
                first.lockfile,
                echo(&other.name),
                other.lockfile
            );
        }
        let first = chosen(root, &hits, &dists)?;
        valid(first)?;
        let rest = &name[(at + len).min(name.len())..];
        if rest.is_empty() {
            let sep = if first.eco == Eco::Cargo { "::" } else { "." };
            bail!("name a definition of {}: {}{sep}NAME (--deps takes PACKAGE.NAME)", echo(start), echo(start));
        }
        return Ok((first.clone(), rest, sub));
    }
    let hint = "--deps takes PACKAGE.NAME, PACKAGE as the code imports it";
    if let Some(missed) = missed {
        bail!("{missed}: {hint}");
    }
    let mut absent = 0;
    if let Some((package, rest)) = name.split_once('.')
        && !rest.is_empty()
        && !package.is_empty()
        && !package.starts_with(|c: char| c.is_ascii_digit())
        && package.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        // the repository's code imports only its direct requirements
        let go: Vec<&Locked> = all.iter().filter(|l| l.eco == Eco::Go).collect();
        let direct: Vec<&Locked> = if go.iter().any(|l| !l.indirect) { go.into_iter().filter(|l| !l.indirect).collect() } else { go };
        let mut found: Vec<(&Locked, String)> = Vec::new();
        for l in direct {
            match go_module_dir(root, l) {
                Ok(Some(dir)) => found.extend(go_dirs_named(&dir, package).into_iter().map(|sub| (l, sub))),
                _ => absent += 1,
            }
        }
        let path = |(l, sub): &(&Locked, String)| format!("{}/{sub}", l.imported_as.as_deref().unwrap_or(&l.name));
        if found.len() > 1 {
            let mine: Vec<(&Locked, String)> = found.iter().filter(|f| imported(&path(f))).cloned().collect();
            if mine.len() == 1 {
                found = mine;
            } else {
                let echo = crate::fmt::echo;
                let paths: Vec<String> = found.iter().map(path).collect();
                // the first three, as client-go has 59 packages named `fake`
                let shown: Vec<String> = paths.iter().take(3).map(|p| echo(p)).collect();
                let listed = match paths.len() {
                    2 => shown.join(" and "),
                    3 => format!("{}, {} and {}", shown[0], shown[1], shown[2]),
                    n => format!("{} and {} more", shown.join(", "), n - 3),
                };
                let import = match (mine.is_empty(), paths.len()) {
                    (true, 2) => "neither",
                    (true, _) => "none of them",
                    (false, _) => "more than one",
                };
                bail!(
                    "{} names {} packages of modules go.mod requires, {listed}, and the repository's Go files import {import}: --deps cannot tell which; name the package by its import path ({}.NAME)",
                    echo(package),
                    paths.len(),
                    echo(&paths[0])
                );
            }
        }
        if let Some((l, sub)) = found.pop() {
            valid(l)?;
            return Ok((l.clone(), rest, sub));
        }
    }
    let unread =
        if absent > 0 { format!("; {absent} of the modules go.mod requires are not in the Go module cache: run go mod download") } else { String::new() };
    bail!("no package of {} names {}: {hint}{unread}", read.join(", "), crate::fmt::echo(name))
}

/// What a lockfile names is shown and used only when it is one of its ecosystem's.
fn valid(l: &Locked) -> Result<()> {
    if !valid_name(l.eco, &l.name) {
        return Err(refused(l, "that is no package name"));
    }
    if !l.version.is_empty() && !valid_version(l.eco, &l.version) {
        return Err(refused(l, "that is no version"));
    }
    Ok(())
}

/// A Go module's directory: the module cache's copy, or the directory a `replace` names inside the
/// repository (None when it names a drive, a share or a root, or leaves the repository).
fn go_module_dir(root: &Path, l: &Locked) -> Result<Option<PathBuf>> {
    match &l.from {
        From::Store => installed(root, l).map(|i| Some(i.dir)),
        From::Path(d) if !names_a_root(d) => Ok(std::fs::canonicalize(root.join(d.trim_start_matches("./"))).ok().filter(|f| f.starts_with(root))),
        From::Path(_) | From::Other(_) => Ok(None),
    }
}

/// The Go package at import path `start` in a subdirectory of a locked module: the module with
/// the longest path that `start` starts with, and the directory after it, when the module has a
/// package there. A module without one is kept in `missed` for the error.
fn go_subpackage<'l>(root: &Path, all: &'l [Locked], start: &str, missed: &mut Option<String>) -> Result<Option<(&'l Locked, String)>> {
    let path = |l: &'l Locked| l.imported_as.as_deref().unwrap_or(&l.name);
    let under = |l: &&'l Locked| l.eco == Eco::Go && start.strip_prefix(path(l)).is_some_and(|s| s.starts_with('/'));
    let Some(l) = all.iter().filter(under).max_by_key(|l| path(l).len()) else { return Ok(None) };
    let sub = &start[path(l).len() + 1..];
    if !sub.split('/').all(|p| plain_part(p) && !p.starts_with('.')) {
        return Ok(None);
    }
    let Some(dir) = go_module_dir(root, l)? else { return Ok(None) };
    if inside(l, &dir, &dir.join(sub))?.is_some_and(|d| go_package(&d).is_some()) {
        return Ok(Some((l, sub.to_string())));
    }
    let echo = crate::fmt::echo;
    missed.get_or_insert_with(|| format!("{} {} has no package {}", echo(path(l)), echo(&l.version), echo(sub)));
    Ok(None)
}

/// The package clause of a Go package's directory: that of its first Go file, not a test's.
fn go_package(dir: &Path) -> Option<String> {
    use std::io::BufRead;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| p.to_str().is_some_and(|n| n.ends_with(".go") && !n.ends_with("_test.go")))
        .collect();
    files.sort();
    let file = std::fs::File::open(files.first()?).ok()?;
    std::io::BufReader::new(file).lines().map_while(Result::ok).take(200).find_map(|l| l.trim().strip_prefix("package ").map(|p| p.trim().to_string()))
}

/// The directories under a Go module's `dir` named `package` whose package clause is that name,
/// relative to it: none under `internal`, which only the module's own code imports, and none Go
/// leaves out of a module (`testdata`, `vendor`, `.` and `_` names, another module's directory).
fn go_dirs_named(dir: &Path, package: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut todo = vec![(dir.to_path_buf(), String::new())];
    while let Some((at, rel)) = todo.pop() {
        for e in std::fs::read_dir(&at).into_iter().flatten().flatten() {
            let Ok(m) = e.metadata() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if !m.is_dir() || is_link(&m) || name.starts_with(['.', '_']) || ["testdata", "vendor", "internal"].contains(&name.as_str()) {
                continue;
            }
            let path = e.path();
            if path.join("go.mod").is_file() {
                continue;
            }
            let sub = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            if name == package && go_package(&path).as_deref() == Some(package) {
                found.push(sub.clone());
            }
            todo.push((path, sub));
        }
    }
    found.sort();
    found
}

/// The one of `hits`, one package that may be locked at several versions, that the repository
/// uses: for Cargo the version its own packages depend on, for Python the one `.venv` has; never
/// the first in the lockfile.
fn chosen<'l>(root: &Path, hits: &[&'l Locked], dists: &[Dist]) -> Result<&'l Locked> {
    // one version from two sources (the registry's and a git copy) is two packages
    let mut versions: Vec<&'l Locked> = Vec::new();
    for l in hits {
        if !versions.iter().any(|v| v.version == l.version && v.from == l.from) {
            versions.push(l);
        }
    }
    let [first, ..] = versions[..] else { bail!("no package") };
    if versions.len() == 1 {
        return Ok(first);
    }
    let used: Vec<(String, Option<From>)> = match first.eco {
        Eco::Cargo => cargo_direct(root, &first.name),
        Eco::Python => dists.iter().filter(|d| normalized(&d.name) == normalized(&first.name)).map(|d| (d.version.clone(), None)).collect(),
        Eco::Npm | Eco::Go => Vec::new(),
    };
    let mine: Vec<&'l Locked> =
        versions.iter().copied().filter(|l| used.iter().any(|(v, from)| *v == l.version && from.as_ref().is_none_or(|f| *f == l.from))).collect();
    if let [one] = mine[..] {
        return Ok(one);
    }
    let echo = crate::fmt::echo;
    // a version locked from two sources is named with each source's kind
    let twice = |l: &Locked| versions.iter().filter(|o| o.version == l.version).count() > 1;
    let kind = |l: &Locked| match &l.from {
        From::Store => "registry".to_string(),
        From::Path(_) => "path".to_string(),
        From::Other(how) => how.clone(),
    };
    let listed =
        versions.iter().map(|l| if twice(l) { format!("{} ({})", echo(&l.version), kind(l)) } else { echo(&l.version) }).collect::<Vec<_>>().join(" and ");
    let name = echo(&first.name);
    match first.eco {
        Eco::Cargo => bail!(
            "{name} is locked at {listed}, and the repository's own packages depend on {} directly: --deps cannot tell which; `cargo tree -i {name}` shows what pulls in each",
            if mine.is_empty() { "neither" } else { "more than one" }
        ),
        Eco::Python if used.is_empty() => Err(not_in_venv(first, &name, &listed)),
        Eco::Python => bail!(
            "{name} is locked at {listed} but .venv has {}: run {}",
            used.iter().map(|(v, _)| echo(v)).collect::<Vec<_>>().join(" and "),
            first.eco.sync(first.lockfile)
        ),
        Eco::Npm | Eco::Go => bail!("{name} is locked at {listed}: --deps cannot tell which"),
    }
}

/// The versions of `name` that the repository's own packages (those Cargo.lock gives no source:
/// the root, the workspace members, path dependencies) depend on, with the source when it names
/// one; Cargo.lock writes `NAME VERSION` in `dependencies` when it locks more than one version of
/// NAME, and `NAME VERSION (SOURCE)` when it locks one version from more than one source.
fn cargo_direct(root: &Path, name: &str) -> Vec<(String, Option<From>)> {
    let text = std::fs::read_to_string(root.join("Cargo.lock")).unwrap_or_default();
    let Ok(t) = text.parse::<toml::Table>() else { return Vec::new() };
    let packages = t.get("package").and_then(|p| p.as_array()).cloned().unwrap_or_default();
    let mut out: Vec<(String, Option<From>)> = packages
        .iter()
        .filter(|p| p.get("source").is_none())
        .filter_map(|p| p.get("dependencies").and_then(|d| d.as_array()).cloned())
        .flatten()
        .filter_map(|d| {
            let mut parts = d.as_str()?.splitn(3, ' ');
            if parts.next()? != name {
                return None;
            }
            let version = parts.next()?.to_string();
            let source = parts.next().and_then(|s| s.strip_prefix('(')?.strip_suffix(')')).map(cargo_source);
            Some((version, source))
        })
        .collect();
    out.dedup();
    out
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

/// The error for a Python package `.venv` lacks. poetry keeps its environment under its cache
/// directory unless the project has a `.venv` or sets `virtualenvs.in-project`, so a poetry user
/// is told how to have it in `.venv`, which is where --deps reads.
fn not_in_venv(l: &Locked, name: &str, versions: &str) -> anyhow::Error {
    let are = if versions.contains(" and ") { "are" } else { "is" };
    let sync = l.eco.sync(l.lockfile);
    if l.lockfile == "poetry.lock" {
        return anyhow!(
            "{name} {versions} {are} not in .venv, where --deps reads, and poetry keeps its environment under its cache directory unless the project has a .venv or sets virtualenvs.in-project: run `poetry config virtualenvs.in-project true --local`, remove the environment `poetry env info --path` names, then {sync}"
        );
    }
    anyhow!("{name} {versions} {are} not in .venv: run {sync}")
}

fn refused(l: &Locked, why: &str) -> anyhow::Error {
    anyhow!("refused: {} names {} at {}: {why}", l.lockfile, crate::fmt::echo(&l.name), crate::fmt::echo(&l.version))
}

/// Whether `m` is a link: a symbolic link, or on Windows any reparse point (a junction too).
fn is_link(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}

/// `rel` under the repository's `root` (canonical), when every part of it from the root down is a
/// directory and none is a link: a store the repository holds is read through no link, so none
/// can move it out of the repository. A link on the way is refused before anything is read.
fn own_dir(l: &Locked, root: &Path, rel: &Path) -> Result<Option<PathBuf>> {
    let mut at = root.to_path_buf();
    for part in rel.components() {
        let Component::Normal(p) = part else { return Err(refused(l, "its directory leaves the repository")) };
        at.push(p);
        let Ok(m) = std::fs::symlink_metadata(&at) else { return Ok(None) };
        if is_link(&m) {
            let shown = crate::fmt::echo(&shown_path(root, &at));
            return Err(refused(l, &format!("{shown} is a link, and a store in the repository is read through none")));
        }
        if !m.is_dir() {
            return Ok(None);
        }
    }
    Ok(Some(at))
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
            Err(missing(format!("the cargo registry ({})", crate::paths::plain_path(&src).display())))
        }
        Eco::Npm => {
            let dir = own_dir(l, root, &Path::new("node_modules").join(&l.name))?.ok_or_else(|| missing("node_modules".into()))?;
            let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap_or_default()).unwrap_or_default();
            match manifest["version"].as_str() {
                Some(v) if v == l.version => Ok(whole(dir)),
                Some(v) => bail!("{} is locked at {} but node_modules has {}: run npm ci", l.name, l.version, crate::fmt::echo(v)),
                None => Err(missing("node_modules".into())),
            }
        }
        Eco::Python => {
            if let Some(linked) = site_packages(root).1.first() {
                let shown = crate::fmt::echo(&shown_path(root, linked));
                return Err(refused(l, &format!("{shown} is reached through a link, and a store in the repository is read through none")));
            }
            let all = dists(root);
            let mine: Vec<&Dist> = all.iter().filter(|d| normalized(&d.name) == normalized(&l.name)).collect();
            let Some(d) = mine.iter().find(|d| d.version == l.version) else {
                return match mine.first() {
                    Some(d) => bail!("{} is locked at {} but .venv has {}: run {}", l.name, l.version, crate::fmt::echo(&d.version), l.eco.sync(l.lockfile)),
                    None => Err(not_in_venv(l, &l.name, &l.version)),
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
            Ok(whole(inside(l, &cache, &dir)?.ok_or_else(|| missing(format!("the Go module cache ({})", crate::paths::plain_path(&cache).display())))?))
        }
    }
}

/// The cache's `deps` directory, made the user's alone: the cache root must belong to the user and
/// be writable by no one else, or another user could swap a copy for one of their own; a
/// directory of the cache that is open to others is closed (Unix: mode 0700). On Windows the
/// cache is under the user's own `%LOCALAPPDATA%` (never `XDG_CACHE_HOME`), whose access the system
/// gives that user alone; a `SCRUTATE_CACHE_DIR` there is not checked.
fn deps_dir(given: Option<&Path>) -> Result<PathBuf> {
    let root = cache_dir(given)?;
    let deps = root.join("deps");
    private_dirs(&deps)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let me = rustix::process::geteuid().as_raw();
        let m = std::fs::metadata(&root)?;
        if m.uid() != me || m.mode() & 0o022 != 0 {
            bail!(
                "refused: the cache {} belongs to another user or others may write to it: set SCRUTATE_CACHE_DIR to a directory of your own",
                crate::fmt::echo(&crate::paths::plain_path(&root).to_string_lossy())
            );
        }
        let m = std::fs::metadata(&deps)?;
        if m.uid() != me {
            bail!("refused: {} belongs to another user", crate::fmt::echo(&crate::paths::plain_path(&deps).to_string_lossy()));
        }
        if m.mode() & 0o077 != 0 {
            std::fs::set_permissions(&deps, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(deps)
}

/// Temporary directories of copies that a crash left behind: none is younger than a day, and a
/// copy is made in seconds.
fn clear_stale(eco: &Path) {
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in std::fs::read_dir(eco).into_iter().flatten().flatten() {
        let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age > day);
        if old && e.file_name().to_string_lossy().starts_with(".tmp-") {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn private_dirs(dir: &Path) -> Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir).with_context(|| format!("cannot create {}", crate::paths::plain_path(dir).display()))
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
        std::fs::copy(&real, &dest).with_context(|| format!("cannot copy {}", crate::paths::plain_path(&real).display()))?;
        Ok(())
    };
    match &inst.files {
        Some(list) => {
            for f in list {
                // a file of the package, or a directory on its way, that is a link is never read
                let (rel, mut at) = (Path::new(f), inst.base.clone());
                let plain = rel.components().all(|c| {
                    at.push(c);
                    std::fs::symlink_metadata(&at).is_ok_and(|m| !is_link(&m))
                });
                if plain {
                    one(&at, rel)?;
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
/// A copy from a store the repository holds (`node_modules`, `.venv`) is keyed by the repository
/// too, so what one repository installed is never read for another; a copy from a user's store
/// (the cargo registry, the Go module cache) is shared by every repository at that version.
fn cached(l: &Locked, inst: &Installed, caps: (usize, u64), root: &Path, cache: Option<&Path>) -> Result<PathBuf> {
    let eco = deps_dir(cache)?.join(l.eco.key());
    let own = || {
        let k = crate::tscache::hash(&[root.to_string_lossy().as_bytes()]);
        k[..8].iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let key = match l.eco {
        Eco::Go => format!("{}@{}", go_escaped(&l.name).replace('/', "+"), go_escaped(&l.version)),
        Eco::Cargo => format!("{}@{}", l.name, l.version),
        Eco::Python => format!("{}@{}+{}", normalized(&l.name), l.version, own()),
        Eco::Npm => format!("{}@{}+{}", l.name.replace('/', "+"), l.version, own()),
    };
    let entry = eco.join(key);
    if entry.join("done").is_file() {
        return Ok(entry.join("src"));
    }
    private_dirs(&eco)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&eco)?.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(&eco, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    clear_stale(&eco);
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
            bail!("cannot put the copy of {} {} in {}", l.name, l.version, crate::paths::plain_path(&entry).display());
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

/// `p` as an answer names it: from the root when it is inside, else whole; on Windows without the
/// verbatim prefix and with `/` between its parts, as a path inside is written.
fn shown_path(root: &Path, p: &Path) -> String {
    shown_path_on(root, p, cfg!(windows))
}

fn shown_path_on(root: &Path, p: &Path, windows: bool) -> String {
    match p.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => {
            let s = p.to_string_lossy();
            let s = crate::paths::plain_on(&s, windows);
            if windows { s.replace('\\', "/") } else { s.into_owned() }
        }
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

/// The directory, from the root of what is read, whose own files hold the definitions a name
/// means before any other, and their extension: a Go package is the directory at its import path,
/// a Python package's names are in its directory or its module file. None for Cargo and npm, whose
/// modules are not directories (a crate root re-exports what its submodules define).
fn package_dir(l: &Locked, start: &str, prefix: &str) -> Option<(String, &'static str)> {
    match l.eco {
        Eco::Go => Some((prefix.to_string(), ".go")),
        Eco::Python if l.from == From::Store => Some((start.replace('.', "/"), ".py")),
        Eco::Cargo | Eco::Npm | Eco::Python => None,
    }
}

/// The closing line's advice of a `read --deps` with more definitions than it shows: it takes no path.
const DEPS_MORE: &str = "read one by its location (path:start-end), without deps";

/// A read stopped at its definitions advises what `--deps` takes, never a path.
fn deps_stop(a: &mut Answer) {
    if let Answer::Definitions(_, Some(stop), _) = a {
        stop.how = DEPS_MORE;
    }
}

/// `text` as a glob that matches it alone: each glob metacharacter is a class of its own.
fn glob_literal(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '*' | '?' | '[' | '{' | '}' => format!("[{c}]"),
            '\\' if cfg!(unix) => "\\\\".to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// Whether `path` is a file directly in `dir` with extension `ext`, or the module file `dir.ext`.
fn in_package(path: &str, dir: &str, ext: &str) -> bool {
    let inside = if dir.is_empty() { Some(path) } else { path.strip_prefix(&format!("{dir}/")) };
    inside.is_some_and(|f| !f.contains('/') && f.ends_with(ext)) || path == format!("{dir}{ext}")
}

/// `lookup`, with the definitions in the package's own directory first; in each part one whose
/// qualified name is the name as asked (`Equal` the package's function, `Assertions.Equal` the
/// method), then a name as typed (`Marshal`) before one that matches only without its case
/// (`marshal`). The package's own are looked up in that directory on their own first, so none of
/// them is cut by the limit (a read's 20 definitions included: its own are read by their locations).
fn ordered(run: &mut dyn FnMut(&Op) -> Result<Answer>, rest: &str, read: bool, limit: usize, exact: bool, dir: Option<(String, &str)>) -> Result<Answer> {
    let Some((dir, ext)) = dir else { return lookup(run, rest, read, limit, exact) };
    let want = rest.rsplit("::").next().unwrap_or(rest).rsplit('.').next().unwrap_or(rest);
    let own = |p: &str| in_package(p, &dir, ext);
    let at = glob_literal(&dir);
    let globs = if dir.is_empty() { vec![format!("/*{ext}")] } else { vec![format!("/{at}/*{ext}"), format!("/{at}{ext}")] };
    let filter = crate::filter::Filter { globs, ..Default::default() };
    // a read looks a name up as written, with no near matches, and shows at most READ_DEFINITIONS
    let (own_limit, own_exact) = if read { (crate::ops::READ_DEFINITIONS + 1, true) } else { (limit, exact) };
    let scoped = |n: &str| Op::Symbol { name: n.to_string(), limit: own_limit, exact: own_exact }.scoped(filter.clone());
    let mut got = run(&scoped(rest))?;
    if got.is_empty() && want != rest {
        got = run(&scoped(want))?;
    }
    let mut own_hits: Vec<crate::search::Hit> = match got {
        Answer::Hits { hits, .. } => hits.into_iter().filter(|h| own(&h.path)).collect(),
        _ => Vec::new(),
    };
    let mut own_defs = Vec::new();
    if read {
        for h in own_hits.iter().take(crate::ops::READ_DEFINITIONS) {
            let at = Op::Read { path: None, symbol: Some(format!("{}:{}-{}", h.path, h.start, h.end)), start: 1, end: None, history: 0 };
            if let Answer::Definitions(defs, ..) = run(&at)? {
                own_defs.extend(defs);
            }
        }
    }
    let more_own = own_hits.len() > if read { crate::ops::READ_DEFINITIONS } else { limit };
    let mut a = lookup(run, rest, read, limit, exact)?;
    match &mut a {
        Answer::Hits { hits, stopped, .. } => {
            let others: Vec<crate::search::Hit> = std::mem::take(hits).into_iter().filter(|h| !own_hits.iter().any(|o| o.id == h.id)).collect();
            let mut all: Vec<crate::search::Hit> = std::mem::take(&mut own_hits).into_iter().chain(others).collect();
            all.sort_by_key(|h| (!own(&h.path), h.qualname != rest, h.name != want));
            if all.len() > limit {
                all.truncate(limit);
                *stopped = Some(crate::ops::Stop { at: limit, how: crate::ops::RAISE_LIMIT });
            }
            *hits = all;
        }
        Answer::Definitions(defs, stopped, _) => {
            let others: Vec<_> = std::mem::take(defs)
                .into_iter()
                .filter(|d| !own_defs.iter().any(|o: &(crate::index::ChunkRow, String, Vec<crate::index::Symbol>)| o.0.id == d.0.id))
                .collect();
            let mut all: Vec<_> = std::mem::take(&mut own_defs).into_iter().chain(others).collect();
            all.sort_by_key(|d| (!own(&d.0.path), d.0.qualname != rest, d.0.name != want));
            if all.len() > crate::ops::READ_DEFINITIONS || more_own {
                all.truncate(crate::ops::READ_DEFINITIONS);
                *stopped = Some(crate::ops::Stop { at: crate::ops::READ_DEFINITIONS, how: DEPS_MORE });
            }
            *defs = all;
        }
        _ => {}
    }
    Ok(a)
}

/// Judged by its text alone: a path that names a drive, a share or a root is never looked at
/// (Windows would connect to a share).
fn names_a_root(d: &str) -> bool {
    d.contains(['\\', ':']) || d.starts_with('/') || Path::new(d).components().any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
}

/// `op` through `run`, kept inside the directory `dir` ("" for all): a symbol lookup is scoped to
/// it, so its limit counts only what is there, and a read by symbol reads, by their locations,
/// the definitions that lookup finds, so READ_DEFINITIONS counts only those too.
fn in_dir(run: &mut dyn FnMut(&Op) -> Result<Answer>, dir: &str, op: &Op) -> Result<Answer> {
    if dir.is_empty() {
        return run(op);
    }
    let inside = crate::filter::Filter { globs: vec![format!("/{}/**", glob_literal(dir))], ..Default::default() };
    match op {
        Op::Symbol { name, limit, exact } => run(&Op::Symbol { name: name.clone(), limit: *limit, exact: *exact }.scoped(inside)),
        Op::Read { symbol: Some(name), .. } if crate::intel::location(name).is_none() => {
            let n = crate::ops::READ_DEFINITIONS;
            let hits = match run(&Op::Symbol { name: name.clone(), limit: n + 1, exact: true }.scoped(inside))? {
                Answer::Hits { hits, .. } if !hits.is_empty() => hits,
                _ => bail!("no definition matches {}", crate::fmt::echo(name)),
            };
            let mut defs = Vec::new();
            for h in hits.iter().take(n) {
                let at = Op::Read { path: None, symbol: Some(format!("{}:{}-{}", h.path, h.start, h.end)), start: 1, end: None, history: 0 };
                if let Answer::Definitions(d, ..) = run(&at)? {
                    defs.extend(d);
                }
            }
            Ok(Answer::Definitions(defs, (hits.len() > n).then_some(crate::ops::Stop { at: n, how: DEPS_MORE }), None))
        }
        _ => run(op),
    }
}

/// The answer to `symbol --deps` (`read`: `read --symbol --deps`).
pub fn run(s: &mut Searcher, name: &str, read: bool, limit: usize, exact: bool) -> Result<Answer> {
    let root = std::fs::canonicalize(&s.ix.root).unwrap_or_else(|_| s.ix.root.clone());
    // whether a Go file of the repository's own, outside its vendor directory, imports `module`
    let imported = |module: &str| {
        let rows = s.ix.imports_holding(module).unwrap_or_default();
        rows.iter().any(|(p, m)| m.trim_matches(['"', '`']) == module && p.ends_with(".go") && !p.starts_with("vendor/"))
    };
    let (l, rest, sub) = split(&root, name, &imported)?;
    let joined = |a: &str| {
        if a.is_empty() {
            sub.clone()
        } else if sub.is_empty() {
            a.to_string()
        } else {
            format!("{a}/{sub}")
        }
    };
    let start = name[..name.len() - rest.len()].trim_end_matches(['.', ':']);
    match &l.from {
        From::Other(how) => bail!("{} {} comes from {}, which --deps does not read", l.name, l.version, crate::fmt::echo(how)),
        From::Path(dir) => {
            if names_a_root(dir) {
                return Err(refused(&l, &format!("its path dependency {} names a drive, a share or a root", crate::fmt::echo(dir))));
            }
            let dir = dir.trim_start_matches("./").trim_end_matches('/');
            let full = std::fs::canonicalize(root.join(dir)).ok().filter(|d| d.starts_with(&root));
            let Some(full) = full else {
                bail!("{} is {} in {}, outside the repository: refused", l.name, crate::fmt::echo(dir), l.lockfile);
            };
            let prefix = joined(&shown_path(&root, &full));
            let mut inner = |op: &Op| crate::ops::run_in(s, op);
            let mut a = ordered(&mut |op| in_dir(&mut inner, &prefix, op), rest, read, limit, exact, package_dir(&l, start, &prefix))?;
            deps_stop(&mut a);
            let a = crate::ops::noted_under(&s.ix, a, &prefix, &|p| p.to_string());
            let note = Note { name: l.name.clone(), version: l.version.clone(), lockfile: l.lockfile, path: prefix, local: true };
            Ok(Answer::Package(Box::new(note), Box::new(a)))
        }
        From::Store => {
            let inst = installed(&root, &l)?;
            let src = cached(&l, &inst, (MAX_FILES, MAX_BYTES), &root, s.cache_dir.as_deref())?;
            let mut repo = crate::Repo::open_with(&src, crate::models::ModelSet::none())?;
            // a copy never changes once made, so it is indexed once and never looked at again
            let indexed = src.with_file_name("indexed");
            if !indexed.is_file() {
                repo.update(false)?;
                std::fs::write(&indexed, "")?;
            }
            let mut inner = |op: &Op| repo.run_as_is(op);
            let mut a = ordered(&mut |op| in_dir(&mut inner, &sub, op), rest, read, limit, exact, package_dir(&l, start, &sub))?;
            deps_stop(&mut a);
            repath(&mut a, |p| shown_path(&root, &inst.base.join(p)));
            let a = crate::ops::noted_under(&repo.ix, a, &sub, &|p| shown_path(&root, &inst.base.join(p)));
            let dir = if sub.is_empty() { inst.dir.clone() } else { inst.dir.join(&sub) };
            let note = Note { name: l.name.clone(), version: l.version.clone(), lockfile: l.lockfile, path: shown_path(&root, &dir), local: false };
            Ok(Answer::Package(Box::new(note), Box::new(a)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_windows_the_dependency_cache_ignores_xdg_cache_home() {
        let var = |k: &str| match k {
            "XDG_CACHE_HOME" => Some(PathBuf::from("/msys/cache")),
            "LOCALAPPDATA" => Some(PathBuf::from("/local")),
            _ => None,
        };
        assert_eq!(user_cache(true, false, var, None).unwrap(), Path::new("/local/scrutate"));
        assert_eq!(user_cache(false, false, var, None).unwrap(), Path::new("/msys/cache/scrutate"));
        assert!(user_cache(true, false, |k: &str| var(k).filter(|_| k != "LOCALAPPDATA"), None).is_err());
    }

    #[test]
    fn a_store_path_outside_the_root_is_shown_without_the_verbatim_prefix_and_with_slashes_on_windows() {
        let root = Path::new(r"\\?\C:\repo");
        let p = Path::new(r"\\?\C:\home\cargo\registry\src\serde-1.0.200\src\lib.rs");
        assert_eq!(shown_path_on(root, p, true), "C:/home/cargo/registry/src/serde-1.0.200/src/lib.rs");
        let share = Path::new(r"\\?\UNC\host\share\gomod\gin@v1.10.0\gin.go");
        assert_eq!(shown_path_on(root, share, true), "//host/share/gomod/gin@v1.10.0/gin.go");
        assert_eq!(shown_path_on(root, p, false), p.to_string_lossy());
    }

    #[test]
    fn a_directory_s_glob_matches_that_directory_alone() {
        let root = std::env::temp_dir();
        let dir = "a[1]/b{c,d}/e*?";
        let f = crate::filter::Filter { globs: vec![format!("/{}/**", glob_literal(dir))], ..Default::default() }.compile(&root).unwrap();
        assert!(f.admits_path(&format!("{dir}/x.rs")));
        for other in ["a1/bc/e/x.rs", "a[1]/bc/ex/x.rs", "a[1]/b{c,d}/ex/x.rs", "a1/b{c,d}/e*?/x.rs"] {
            assert!(!f.admits_path(other), "{other}");
        }
    }

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
        let l = go_mod(
            "module m\n\nrequire (\n\ta.com/x v1.0.0 // indirect\n\tb.com/y v2.0.0\n)\nrequire c.com/z v0.1.0\n\nreplace b.com/y => ./y\nreplace (\n\tc.com/z v0.1.0 => d.com/z v0.2.0\n)\n",
        );
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
        let l = Locked {
            eco: Eco::Cargo,
            name: "p".into(),
            version: "1.0.0".into(),
            lockfile: "Cargo.lock",
            from: From::Store,
            indirect: false,
            imported_as: None,
        };
        let cache = Some(top.join("cache"));
        let e = cached(&l, &inst, (4, 1 << 20), &top, cache.as_deref()).unwrap_err().to_string();
        assert!(e.contains("cap of 4 files"), "{e}");
        let e = cached(&l, &inst, (100, 20), &top, cache.as_deref()).unwrap_err().to_string();
        assert!(e.contains("cap"), "{e}");
        // nothing half made is left to be taken for a copy
        assert_eq!(std::fs::read_dir(top.join("cache/deps/cargo")).unwrap().count(), 0);
        let src = cached(&l, &inst, (5, 1 << 20), &top, cache.as_deref()).unwrap();
        assert_eq!(std::fs::read_dir(src.join("src")).unwrap().count(), 5);
        let _ = std::fs::remove_dir_all(&top);
    }
}
