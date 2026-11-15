//! Scope filters for the index answers (search, symbol, refs): grep's globs and file types, the
//! language a file was parsed as, and the kind of a definition. Values of one flag are any of
//! them; the flags given must all hold.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Result};

use crate::instant::Files;

/// The filter as asked for, before its names are checked.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Filter {
    /// grep's `-g`: globs from the root, `!` to leave out
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub globs: Vec<String>,
    /// grep's `-t`: file type names (`--type-list`), `!` to leave out
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    /// language ids (`scrutate languages`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub langs: Vec<String>,
    /// definition kinds
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.globs.is_empty() && self.types.is_empty() && self.langs.is_empty() && self.kinds.is_empty()
    }

    /// The filter for the repository at `root`; an unknown type, language or kind, or a glob that
    /// does not parse, is an error.
    pub fn compile(&self, root: &Path) -> Result<Compiled> {
        let files = Files { globs: self.globs.clone(), types: self.types.clone(), ..Files::walk(vec![root.to_path_buf()]) };
        let ov = crate::instant::overrides(root, &files)?;
        let types = crate::instant::file_types(&files)?;
        let custom = crate::custom::current();
        for l in &self.langs {
            if crate::langs::by_id(l).is_none() && custom.by_id(l).is_none() {
                bail!("unknown language {} (see `scrutate languages`)", crate::fmt::echo(l));
            }
        }
        let all = kinds();
        for k in &self.kinds {
            if !all.contains(k.as_str()) {
                bail!("unknown kind {k:?} (one of {})", all.iter().copied().collect::<Vec<_>>().join(", "));
            }
        }
        Ok(Compiled { root: root.to_path_buf(), filter: self.clone(), ov, types, langs: Mutex::new(HashMap::new()) })
    }
}

/// A checked filter.
pub struct Compiled {
    root: PathBuf,
    filter: Filter,
    ov: ignore::overrides::Override,
    types: ignore::types::Types,
    /// each file's verdict on the languages, as a file holds many definitions
    langs: Mutex<HashMap<String, bool>>,
}

impl Compiled {
    pub fn is_empty(&self) -> bool {
        self.filter.is_empty()
    }

    /// Whether a definition of `kind` in the file `rel` (a path from the root) passes.
    pub fn admits(&self, rel: &str, kind: &str) -> bool {
        self.admits_kind(kind) && self.admits_path(rel)
    }

    /// Whether the file `rel` passes the globs, the types and the languages.
    pub fn admits_path(&self, rel: &str) -> bool {
        // unlike grep, where a glob that matches decides alone, the globs and the types must both hold
        if crate::instant::filtered_out(&self.ov, rel) || self.types.matched(rel, false).is_ignore() {
            return false;
        }
        if self.filter.langs.is_empty() {
            return true;
        }
        let mut seen = self.langs.lock().unwrap_or_else(|e| e.into_inner());
        *seen.entry(rel.to_string()).or_insert_with(|| {
            let lang = match crate::custom::current().for_path(rel) {
                Some(l) => Some(l.id.clone()),
                None => crate::langs::detect_at(&self.root, rel).map(|s| s.id.to_string()),
            };
            lang.is_some_and(|l| self.filter.langs.contains(&l))
        })
    }

    /// `files` split into the ones whose names the globs, the types and the languages can keep,
    /// and the rest. A file whose extension languages share is kept, and `admits_path` reads its
    /// text to decide. Every extension starts with a dot, so a name's language is judged once
    /// for each ending from its first dot, or each name a language claims whole.
    pub fn split_by_name<'a>(&self, files: &'a [String]) -> (Vec<&'a str>, Vec<&'a str>) {
        let custom = crate::custom::current();
        let mut by_name: HashMap<(bool, String), bool> = HashMap::new();
        files.iter().map(String::as_str).partition(|rel: &&str| {
            if crate::instant::filtered_out(&self.ov, rel) || self.types.matched(rel, false).is_ignore() {
                return false;
            }
            if self.filter.langs.is_empty() {
                return true;
            }
            let base = rel.rsplit('/').next().unwrap_or(rel);
            let whole = crate::langs::LANGS.iter().any(|s| s.files.iter().any(|f| f.eq_ignore_ascii_case(base)));
            let key = match base.find('.') {
                Some(i) if i > 0 && !whole => (false, base[i..].to_lowercase()),
                _ => (true, base.to_lowercase()),
            };
            *by_name.entry(key).or_insert_with(|| {
                let lower = base.to_lowercase();
                if crate::langs::BY_CONTENT.iter().any(|(e, _)| lower.ends_with(e)) {
                    return true;
                }
                let lang = match custom.for_path(rel) {
                    Some(l) => Some(l.id.clone()),
                    None => crate::langs::spec_for(rel).map(|s| s.id.to_string()),
                };
                lang.is_some_and(|l| self.filter.langs.contains(&l))
            })
        })
    }

    /// The kinds the filter keeps, none for any.
    pub fn kinds(&self) -> Vec<&str> {
        self.filter.kinds.iter().map(String::as_str).collect()
    }

    /// Whether the filter keeps files by name or language.
    pub fn has_files(&self) -> bool {
        !(self.filter.globs.is_empty() && self.filter.types.is_empty() && self.filter.langs.is_empty())
    }

    /// Whether the filter names kinds, which only a definition has.
    pub fn has_kinds(&self) -> bool {
        !self.filter.kinds.is_empty()
    }

    pub fn admits_kind(&self, kind: &str) -> bool {
        self.filter.kinds.is_empty() || self.filter.kinds.iter().any(|k| k == kind)
    }

    /// `base`, a walk of the repository, narrowed to the files the globs and types (or, with no
    /// type, the languages' file names) can admit; `admits_path` decides each file it finds.
    pub fn walk(&self, base: Files) -> Files {
        let mut f = base;
        // the repository's own exclusions stay last, where a later glob wins, as grep_code's do
        let skip = std::mem::take(&mut f.globs);
        f.globs = self.filter.globs.iter().cloned().chain(skip).collect();
        f.types = if self.filter.types.is_empty() { self.filter.langs.clone() } else { self.filter.types.clone() };
        f
    }
}

/// Every kind a chunk can carry.
pub fn kinds() -> BTreeSet<&'static str> {
    let mut all: BTreeSet<&'static str> = crate::langs::LANGS.iter().flat_map(|s| crate::langs::definition_kinds(s)).collect();
    all.extend(crate::langs::KIND_WORDS.iter().map(|(_, k)| *k));
    all.extend(crate::custom::KINDS.iter().chain(crate::parse::CONTAINERS).chain(crate::parse::SYMBOL_ONLY));
    all.insert("window");
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_filter_{name}_{}", std::process::id()));
        crate::claim_test_tree(&root);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        // a file's text is read only below the root as canonicalize spells it, as an index root is
        std::fs::canonicalize(root).unwrap()
    }

    fn of(globs: &[&str], types: &[&str], langs: &[&str], kinds: &[&str]) -> Filter {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect();
        Filter { globs: v(globs), types: v(types), langs: v(langs), kinds: v(kinds) }
    }

    #[test]
    fn globs_admit_what_they_name_and_leave_out_what_they_negate() {
        let root = tree("globs", &[]);
        let f = of(&["src/**", "!src/gen/**"], &[], &[], &[]).compile(&root).unwrap();
        assert!(f.admits_path("src/a.rs") && f.admits_path("src/deep/b.py"));
        assert!(!f.admits_path("lib/c.rs") && !f.admits_path("src/gen/d.rs"));
        let only_out = of(&["!vendor/**"], &[], &[], &[]).compile(&root).unwrap();
        assert!(only_out.admits_path("src/a.rs") && !only_out.admits_path("vendor/x/y.rs"));
    }

    #[test]
    fn types_are_any_of_their_names_and_a_negated_type_is_left_out() {
        let root = tree("types", &[]);
        let f = of(&[], &["rust", "py"], &[], &[]).compile(&root).unwrap();
        assert!(f.admits_path("a.rs") && f.admits_path("b/c.py") && !f.admits_path("d.go"));
        let not = of(&[], &["!rust"], &[], &[]).compile(&root).unwrap();
        assert!(!not.admits_path("a.rs") && not.admits_path("d.go"));
    }

    #[test]
    fn a_language_is_the_one_the_file_is_parsed_as() {
        let cpp = "#include <vector>\nnamespace geo {\nclass Point {\n public:\n  int x;\n};\n}\n";
        let c = "#include <stdio.h>\nstruct point { int x; };\nint area(struct point p);\n";
        let root = tree("langs", &[("geo.h", cpp), ("point.h", c), ("main.cpp", "int main() { return 0; }\n"), ("a.py", "x = 1\n")]);
        let as_cpp = crate::langs::detect_at(&root, "geo.h").map(|s| s.id);
        let as_c = crate::langs::detect_at(&root, "point.h").map(|s| s.id);
        assert_ne!(as_cpp, as_c, "the fixture's headers must read as two languages");
        let f = of(&[], &[], &[as_cpp.unwrap()], &[]).compile(&root).unwrap();
        assert!(f.admits_path("geo.h") && !f.admits_path("point.h") && !f.admits_path("a.py"));
        let both = of(&[], &[], &[as_cpp.unwrap(), "python"], &[]).compile(&root).unwrap();
        assert!(both.admits_path("geo.h") && both.admits_path("a.py") && !both.admits_path("point.h"));
    }

    #[test]
    fn kinds_are_any_of_their_names_and_the_flags_must_all_hold() {
        let root = tree("kinds", &[]);
        let f = of(&["src/**"], &["rust"], &[], &["function", "method"]).compile(&root).unwrap();
        assert!(f.admits("src/a.rs", "function") && f.admits("src/a.rs", "method"));
        assert!(!f.admits("src/a.rs", "struct"), "a kind not asked for");
        assert!(!f.admits("src/a.py", "function"), "a type not asked for");
        assert!(!f.admits("lib/a.rs", "function"), "a glob not asked for");
        let none = Filter::default().compile(&root).unwrap();
        assert!(none.admits("anything.txt", "window"));
    }

    #[test]
    fn an_unknown_name_or_a_bad_glob_is_an_error_that_says_where_the_names_are() {
        let root = tree("errors", &[]);
        let err = |f: Filter| f.compile(&root).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err(of(&[], &["nosuchtype"], &[], &[])).contains("--type-list"));
        assert!(err(of(&[], &[], &["nosuchlang"], &[])).contains("scrutate languages"));
        let kind = err(of(&[], &[], &[], &["functon"]));
        assert!(kind.contains("unknown kind \"functon\"") && kind.contains("function, ") && kind.contains("window"), "{kind}");
        assert!(err(of(&["src/[a"], &[], &[], &[])).contains("invalid glob"));
    }

    #[test]
    fn the_kinds_hold_every_kind_a_language_or_a_custom_language_gives() {
        let all = kinds();
        for spec in crate::langs::LANGS.iter() {
            for k in crate::langs::definition_kinds(spec) {
                assert!(all.contains(k), "{} gives {k}", spec.id);
            }
        }
        for k in crate::custom::KINDS.iter().chain(crate::parse::CONTAINERS).chain(crate::parse::SYMBOL_ONLY).chain(&["window"]) {
            assert!(all.contains(k), "{k}");
        }
    }
}
