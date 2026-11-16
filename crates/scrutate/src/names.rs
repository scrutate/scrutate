//! What a name refers to. Every question about a use of a name asks `Names`: callers, callees,
//! references, the language server's definition, imports and importers. The rules that decide
//! live in `resolve/`; this module owns who asks them, once per request, with each file read and
//! parsed once. What `callers` and `callees` learn of a call site is kept (`answers.rs`), so the
//! same request on the same index and files asks the rules nothing. The caller count ranking
//! reads is the index's (`Index::call_counts`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::answers::{Reading, Stored};
use crate::index::{ChunkRow, Symbol};
use crate::resolve::{Occurrence, Resolution, Resolved, Resolver, join};
use crate::search::{Hit, Searcher};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Dependency {
    pub line: i64,
    pub module: String,
    /// repository file the module resolves to, when it can be found
    pub resolved: Option<String>,
}

/// Whether the occurrence of `word` reads as making an instance: a call, `.new`, or `new` before it.
fn instantiates(o: &Occurrence, word: &str) -> bool {
    let after = o.text.get(o.at + word.len()..).unwrap_or("").trim_start();
    let before = o.text.get(..o.at).unwrap_or("").trim_end();
    after.starts_with('(') || after.starts_with(".new") || before.ends_with("new")
}

/// (importing file, line, module as written)
pub type Importer = (String, i64, String);

/// The most occurrences a narrowed `references` (kinds, `--def`) reads to find max_results of the
/// kept definitions, or max_results when it is larger: a count, never a clock. On the Linux tree a
/// read that runs to it takes about 3.5 to 21 seconds by the name: `refs size --kind function`
/// 3.53 s, `refs kfree --def mm/slub.c:6781 --max-results 20` 21.4 s and 492 MB.
pub const REFS_KIND_RESOLVE_MAX: u64 = 10_000;

/// One request's view of what the names of the repository refer to.
pub struct Names<'a> {
    s: &'a Searcher,
    rv: Resolver<'a>,
    /// by file: the byte each line starts at
    line_starts: HashMap<String, Vec<usize>>,
    /// the kept answers of the request running, and the ones it adds
    stored: Option<Stored>,
    /// every name a call records, by its ASCII lowercase form, read once per request
    callee_cases: Option<HashMap<String, Vec<String>>>,
    /// the language of each file asked about, as the parser takes it
    langs: Langs<'a>,
    /// `callers` met another definition of a name it asked for
    pub(crate) rivals: bool,
    /// what a nested `references` of a class found that reads as an instantiation, by spelling
    /// and limit, for the rest of the request (`NESTED_MEMO`), and the bytes it holds
    instantiations: HashMap<(String, u64), std::rc::Rc<Vec<Occurrence>>>,
    memo_bytes: usize,
    _reading: Reading,
}

/// The most a request keeps of its nested `references` answers.
pub(crate) const NESTED_MEMO: usize = 16 << 20;

/// Whether an occurrence resolved to a definition, for the reason given, is kept.
pub type Keep<'k> = &'k dyn Fn(&ChunkRow, &str) -> bool;

#[cfg(test)]
thread_local! {
    /// How many searches of a name `references` ran, nested ones included.
    pub(crate) static SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What `references` keeps of the occurrences its alias and constructor passes find, by the rule
/// of its main read.
struct Gate<'k, 'f> {
    kinds: Option<Keep<'k>>,
    by_file: Option<&'f crate::filter::Compiled>,
    listed: HashSet<(String, u64, u64, Option<i64>)>,
    kind_of: HashMap<(i64, &'static str), bool>,
    max_results: u64,
}

impl Gate<'_, '_> {
    fn key(o: &Occurrence) -> (String, u64, u64, Option<i64>) {
        (o.path.clone(), o.line, o.column, o.resolution.ok().map(|r| r.0))
    }

    /// Keeps each of `extra` the read keeps, after `occurrences`; counts the others into
    /// `narrowed`, a copy retied to a constructor (marked) aside, as its original was counted.
    fn admit(
        &mut self,
        s: &Searcher,
        extra: Vec<(Occurrence, bool)>,
        occurrences: &mut Vec<Occurrence>,
        narrowed: &mut crate::resolve::Narrowed,
    ) -> Result<()> {
        for (o, copy) in extra {
            if self.by_file.is_some_and(|f| !f.admits_path(&o.path)) || !self.listed.insert(Gate::key(&o)) {
                continue;
            }
            let kept = match (self.kinds, o.resolution) {
                (None, _) => true,
                (Some(f), Ok((id, why))) => match self.kind_of.get(&(id, why)) {
                    Some(&k) => k,
                    None => {
                        let k = s.ix.chunk(id)?.is_some_and(|d| f(&d, why));
                        self.kind_of.insert((id, why), k);
                        k
                    }
                },
                (Some(_), Err(_)) => false,
            };
            if kept {
                occurrences.push(o);
            } else if !copy {
                if o.resolution.is_ok() {
                    narrowed.elsewhere += 1;
                } else {
                    narrowed.unresolved_count += 1;
                    if (narrowed.unresolved.len() as u64) < self.max_results {
                        narrowed.unresolved.push(o);
                    }
                }
            }
        }
        Ok(())
    }
}

impl<'a> Names<'a> {
    pub fn new(s: &'a Searcher) -> Result<Names<'a>> {
        // started first: the store keeps what every read of the request saw
        let reading = Reading::start();
        Ok(Names {
            s,
            rv: Resolver::new(&s.ix)?,
            line_starts: HashMap::new(),
            stored: None,
            callee_cases: None,
            langs: Langs { root: &s.ix.root, seen: Default::default() },
            rivals: false,
            instantiations: HashMap::new(),
            memo_bytes: 0,
            _reading: reading,
        })
    }

    /// The definition the occurrence of `name` at 1-based `line`, 0-based byte `col` of `path`
    /// refers to and how that was found, or why none was.
    pub fn at(&mut self, path: &str, line: u64, col: usize, name: &str) -> Resolution {
        self.tie(path, line, col, name).resolution()
    }

    /// Whether the text of `path` the resolver reads starts with a byte order mark, which its
    /// columns on the first line count.
    pub fn starts_with_bom(&mut self, path: &str) -> bool {
        self.rv.file(path).src.starts_with('\u{FEFF}')
    }

    /// Every whole-word occurrence of `name` (Engine A), each resolved to the definition it refers to.
    pub fn references(&mut self, name: &str, max_results: u64) -> Result<Resolved> {
        self.references_in(name, max_results, None)
    }

    /// `references`, Engine A walking only the files `scope` can admit.
    pub fn references_in(&mut self, name: &str, max_results: u64, scope: Option<&crate::filter::Compiled>) -> Result<Resolved> {
        let kinds = scope.filter(|f| f.has_kinds());
        let keep = |d: &ChunkRow, _: &str| kinds.is_none_or(|f| f.admits_kind(&d.kind));
        let keep: Option<Keep> = if kinds.is_some() { Some(&keep) } else { None };
        self.references_kept(name, max_results, scope, keep)
    }

    /// The first of the overloads of `id`, which stands for them all where a call's arguments do
    /// not pick one; None for a definition without overloads.
    pub fn first_overload(&mut self, id: i64) -> Option<i64> {
        match self.rv.overloads(id)[..] {
            [first, _, ..] => Some(first),
            _ => None,
        }
    }

    /// The definitions `references` of `name` can resolve its occurrences to, as it lists them:
    /// a definition `--def` names is picked among these before anything is read.
    pub fn definitions_of(&self, name: &str) -> Result<Vec<ChunkRow>> {
        let bare = crate::intel::bare_name(self.s, name)?;
        let named = self.named(name)?;
        Ok(self
            .s
            .ix
            .definitions(&bare)?
            .into_iter()
            .filter(|d| crate::resolve::same_name(&d.name, &bare) && named.as_ref().is_none_or(|n| n.contains(&d.id)))
            .collect())
    }

    /// The definitions a qualified name (`Stack::push`, `Stack.push`) names, found as `symbol`
    /// finds them; `None` for a bare name, which stands for every definition of it.
    pub fn named(&self, name: &str) -> Result<Option<Vec<i64>>> {
        let typed = name.trim();
        let bare = crate::intel::bare_name(self.s, typed)?;
        let Some(rest) = typed.strip_suffix(bare.as_str()) else { return Ok(None) };
        if rest.trim_start_matches('.').is_empty() || rest == "r#" {
            return Ok(None);
        }
        let hits = self.s.find_symbol(typed, REFS_KIND_RESOLVE_MAX as usize)?;
        Ok(Some(hits.iter().filter(|h| crate::resolve::same_name(&h.name, &bare)).map(|h| h.id).collect()))
    }

    /// `references_in`, keeping only the occurrences that resolve to a definition `keep` admits,
    /// given the definition and why it was resolved to: `max_results` counts those, as they are
    /// resolved, and the others are dropped.
    pub fn references_kept(&mut self, name: &str, max_results: u64, scope: Option<&crate::filter::Compiled>, keep: Option<Keep>) -> Result<Resolved> {
        #[cfg(test)]
        SEARCHES.with(|c| c.set(c.get() + 1));
        let s = self.s;
        let bare = &crate::intel::bare_name(s, name)?;
        // with `keep` an occurrence counts once it resolves to a kept definition, and with file
        // filters once its file passes them (a language can be read from a file's text, which the
        // walk does not open), so the search reads on, at most REFS_KIND_RESOLVE_MAX (or
        // max_results when larger)
        let kinds = keep;
        let by_file = scope.filter(|f| f.has_files());
        let selective = kinds.is_some() || by_file.is_some();
        let read_max = if selective { max_results.max(REFS_KIND_RESOLVE_MAX) } else { max_results };
        let mut kind_of: HashMap<(i64, &str), bool> = HashMap::new();
        let mut bounded: Option<u64>;
        // one line past the bound says whether more exist: the search stopping at exactly the bound does not
        let mut q = crate::instant::Query { want_spans: true, ..crate::resolve::name_search(bare, read_max + 1) };
        // a name defined in a language that ignores case is looked for in any case, and kept in
        // another case only in a file of such a language
        let mut any_case = bare.is_ascii() && s.ix.named_any_case(bare)?.iter().any(|d| self.langs.folds(&d.path, &d.kind));
        let walk = crate::files::repository_files(&s.ix.root);
        let walk = match scope {
            Some(f) => f.walk(walk),
            None => walk,
        };
        // an alias in such a language (`use function Lib\phpfn as pf`) is used in any case too
        let (mut occurrences, notes, mut truncated, mut narrowed) = loop {
            if any_case {
                q.pattern.case = crate::instant::Case::Insensitive;
            }
            // the text index, when it holds the files as the last look found them, says which can match
            let files = crate::stores::grep_files(&s.ix, &q, &walk, s.look.as_ref());
            let (found, stopped) = crate::intel::occurrences_in(&s.ix.root, files.as_ref().unwrap_or(&walk), &q, &s.cancel)?;
            let mut notes = Vec::new();
            let mut hits = Vec::new();
            for r in found {
                match r {
                    crate::instant::Record::Match(l) => hits.push(l),
                    crate::instant::Record::Note { path, text, .. } => notes.push((path.to_string(), text)),
                    _ => {}
                }
            }
            let mut occurrences = Vec::new();
            let mut narrowed = crate::resolve::Narrowed::default();
            // Engine A stops at `max_results` lines; one long line can hold more uses than that
            let total: usize = hits.iter().map(|h| h.spans.len()).sum();
            // stopped short of the line past the bound: for a reason of its own (a cancel), so cut
            let mut truncated = stopped && hits.len() as u64 <= read_max || total as u64 > read_max;
            let mut resolved = 0u64;
            bounded = None;
            'lines: for h in hits {
                if by_file.is_some_and(|f| !f.admits_path(&h.path)) {
                    continue;
                }
                let line_text: std::sync::Arc<str> = h.text.trim_end_matches(['\n', '\r']).into();
                let folds = !any_case || self.langs.folds(&h.path, "function");
                let spans = h.spans.iter().filter(|&&(a, b)| folds || h.text.get(a..b) == Some(bare.as_str()));
                let lang = self.langs.id(&h.path);
                let room = if selective { usize::MAX } else { max_results.saturating_sub(occurrences.len() as u64) as usize };
                for &(a, b) in spans.take(room) {
                    if selective {
                        // one more than max_results kept says whether the answer is cut
                        if occurrences.len() as u64 > max_results {
                            truncated = true;
                            break 'lines;
                        }
                        if resolved >= read_max {
                            truncated = true;
                            bounded = Some(resolved);
                            break 'lines;
                        }
                    }
                    let ((da, db), col) = self.rv.decoded_span(&h.path, h.line, (a, b));
                    // `foo` in `foo'`, `init` in `$init`: part of another name of the file's language
                    if lang.is_some_and(|l| crate::ident::inside_name(l, &h.text, da, db)) {
                        continue;
                    }
                    // the occurrence as this file spells it
                    let resolution = self.tie(&h.path, h.line, col, h.text.get(da..db).unwrap_or(bare)).resolution();
                    let o = Occurrence { path: h.path.to_string(), line: h.line, column: a as u64 + 1, text: line_text.clone(), at: da, resolution };
                    resolved += 1;
                    if let Some(f) = kinds {
                        // an occurrence of another kind is dropped as it resolves, never held
                        let kept = match resolution {
                            Ok((id, why)) => match kind_of.get(&(id, why)) {
                                Some(&k) => k,
                                None => {
                                    let k = s.ix.chunk(id)?.is_some_and(|d| f(&d, why));
                                    kind_of.insert((id, why), k);
                                    k
                                }
                            },
                            Err(_) => false,
                        };
                        // the others are counted, and the unresolved kept up to max_results
                        if !kept {
                            if scope.is_none_or(|sc| sc.admits_path(&o.path)) {
                                if resolution.is_ok() {
                                    narrowed.elsewhere += 1;
                                } else {
                                    narrowed.unresolved_count += 1;
                                    if (narrowed.unresolved.len() as u64) < max_results {
                                        narrowed.unresolved.push(o);
                                    }
                                }
                            }
                            continue;
                        }
                    }
                    occurrences.push(o);
                }
            }
            // the search itself stopped at the bound's lines with fewer than max_results kept
            if selective && bounded.is_none() && truncated && occurrences.len() as u64 <= max_results {
                bounded = Some(resolved);
            }
            // the one past max_results was kept to tell whether the answer is cut
            if selective && occurrences.len() as u64 > max_results {
                occurrences.truncate(max_results as usize);
                truncated = true;
            }
            let alias = |o: &Occurrence| matches!(o.resolution, Ok((id, _)) if s.ix.chunk(id).ok().flatten().is_some_and(|d| d.name != *bare));
            if any_case || !bare.is_ascii() || !occurrences.iter().any(|o| self.langs.folds(&o.path, "function") && alias(o)) {
                break (occurrences, notes, truncated, narrowed);
            }
            any_case = true;
        };
        // a file that imports the name under an alias uses it under the alias too; the import
        // statement's own alias is listed already, through the name it imports. A file whose
        // import never spells the name (`const h = require("./m")`) is found as callers finds it.
        let mut importing: BTreeSet<String> = occurrences.iter().map(|o| o.path.clone()).collect();
        let mut definitions: Vec<ChunkRow> = s.ix.definitions(bare)?.into_iter().filter(|d| crate::resolve::same_name(&d.name, bare)).collect();
        let defs: Vec<Hit> = definitions.iter().map(|d| Hit::from_row(d, false)).collect();
        let langs = Langs { root: &s.ix.root, seen: Default::default() };
        importing.extend(self.aliases_of(&defs, &langs)?.into_iter().map(|(_, f)| f));
        // `super()` in a JavaScript or TypeScript subclass constructor calls the class it extends
        let def_ids: HashSet<i64> = definitions.iter().map(|d| d.id).collect();
        let classes = definitions.iter().any(|d| d.kind == "class" && matches!(langs.id(&d.path), Some("javascript" | "typescript" | "tsx")));
        // PHP `new self()`, `new static()` and `new parent()` make the class they stand for
        let php_classes = definitions.iter().any(|d| d.kind == "class" && langs.id(&d.path) == Some("php"));
        let ident = |c: Option<char>| c.is_some_and(crate::ident::is_continue);
        // what the alias and constructor passes find, each marked when it is a copy of an occurrence
        // retied to a constructor, kept or not by the same gate as the main read's
        let mut extra: Vec<(Occurrence, bool)> = Vec::new();
        for path in importing {
            let mut aliases: Vec<(String, String, Vec<usize>)> = self.rv.alias_sites(&path).into_iter().filter(|(_, item, _)| item == bare).collect();
            let sites: HashSet<usize> = aliases.iter().flat_map(|(_, _, s)| s.iter().copied()).collect();
            if classes {
                aliases.push(("super".into(), bare.to_string(), Vec::new()));
            }
            if php_classes && langs.id(&path) == Some("php") {
                aliases.extend(["self", "static", "parent"].map(|w| (w.to_string(), bare.to_string(), Vec::new())));
            }
            let fi = self.rv.file(&path);
            // where names ignore case (PHP), `PF()` is a use of the alias `pf`; ASCII lower case
            // keeps every byte where it is
            let folds = self.langs.folds(&path, "function");
            let mut line_start = 0;
            for (i, line) in fi.src.split_inclusive('\n').enumerate() {
                let start = line_start;
                line_start += line.len();
                let line = line.trim_end_matches('\n');
                let hay = if folds { std::borrow::Cow::Owned(line.to_ascii_lowercase()) } else { std::borrow::Cow::Borrowed(line) };
                for local in aliases.iter().map(|(l, _, _)| l) {
                    let needle = if folds { local.to_ascii_lowercase() } else { local.clone() };
                    for (col, _) in hay.match_indices(needle.as_str()) {
                        if ident(line[..col].chars().next_back()) || ident(line[col + local.len()..].chars().next()) || sites.contains(&(start + col)) {
                            continue;
                        }
                        // the alias spelled in a string or bound again locally (`const g = ..`) is no
                        // use of these definitions; only a `super(..)` call names the class
                        let resolution = self.tie(&path, i as u64 + 1, col, &line[col..col + local.len()]).resolution();
                        let called = line[col + local.len()..].trim_start().starts_with('(');
                        let made = line[..col].trim_end().to_ascii_lowercase().ends_with("new") && called;
                        let class_word = matches!(local.as_str(), "self" | "static" | "parent");
                        if !matches!(resolution, Ok((id, _)) if def_ids.contains(&id)) || local == "super" && !called || class_word && !made {
                            continue;
                        }
                        let text = line.trim_end_matches('\r').into();
                        extra.push((Occurrence { path: path.clone(), line: i as u64 + 1, column: col as u64 + 1, text, at: col, resolution }, false));
                    }
                }
            }
        }
        // one gate for both passes: a file the filter refuses gives nothing, an occurrence already
        // listed is not listed again, and one not kept is counted aside; a kept one is never cut
        // for one that is not, and truncated says a kept one lies past max_results
        let mut gate = Gate { kinds, by_file: scope.filter(|f| f.has_files()), listed: occurrences.iter().map(Gate::key).collect(), kind_of, max_results };
        gate.admit(s, std::mem::take(&mut extra), &mut occurrences, &mut narrowed)?;
        // a constructor's uses are its class's instantiations, as callers reads them; once the
        // answer is past max_results nothing more is kept, so no class is searched
        let full = |n: usize, truncated: bool| n as u64 > max_results || truncated && n as u64 >= max_results;
        let mut made_by = Vec::new();
        if !full(occurrences.len(), truncated) {
            for d in &defs {
                let Some(class) = constructed(s, &langs, std::slice::from_ref(d))?.0.pop() else { continue };
                // `super(..)` in a JavaScript or TypeScript constructor runs the base class's
                let mut spelled = vec![class.name.clone()];
                if matches!(langs.id(&class.path), Some("javascript" | "typescript" | "tsx")) {
                    spelled.push("super".to_string());
                }
                made_by.push((d.id, class, spelled));
            }
        }
        // the class names in one search of them all, not one each; `super`, which the memo keeps
        // once, and a name its language reads in any case are searched as `references` reads them
        let batched: BTreeSet<String> = made_by
            .iter()
            .filter(|(_, class, _)| !self.langs.folds(&class.path, "class"))
            .flat_map(|(_, _, words)| words.iter().filter(|w| *w != "super" && !(*w == bare && kinds.is_none())).cloned())
            .collect();
        // one name alone is searched as before, its search memoized
        let batched = if batched.len() > 1 { batched } else { BTreeSet::new() };
        let each = if batched.is_empty() { HashMap::new() } else { self.occurrences_of_each(&batched, read_max)? };
        for (ctor, class, spelled) in made_by {
            if full(occurrences.len(), truncated) {
                truncated = true;
                break;
            }
            for word in spelled {
                let made = |o: &Occurrence| matches!(o.resolution, Ok((id, _)) if id == class.id) && instantiates(o, &word);
                // a narrowed read kept only its own occurrences: the class's are read again
                let of_class: Vec<Occurrence> = if word == bare.as_str() && kinds.is_none() {
                    occurrences.iter().filter(|o| made(o)).map(|o| o.retied(ctor)).collect()
                } else if let Some(found) = each.get(&word) {
                    found.iter().filter(|o| made(o)).map(|o| o.retied(ctor)).collect()
                } else if batched.contains(&word) {
                    Vec::new()
                } else {
                    self.instantiations(&word, read_max)?.iter().filter(|o| made(o)).map(|o| o.retied(ctor)).collect()
                };
                gate.admit(s, of_class.into_iter().map(|o| (o, true)).collect(), &mut occurrences, &mut narrowed)?;
            }
        }
        if occurrences.len() as u64 > max_results {
            occurrences.truncate(max_results as usize);
            truncated = true;
        }
        let mut known: HashSet<i64> = definitions.iter().map(|d| d.id).collect();
        for o in &occurrences {
            if let Ok((id, _)) = o.resolution
                && known.insert(id)
            {
                definitions.extend(s.ix.chunk(id)?);
            }
        }
        let mut first_overload = HashMap::new();
        if occurrences.iter().any(|o| matches!(o.resolution, Ok((_, "overload")))) {
            for d in &definitions {
                if let Some(first) = self.first_overload(d.id) {
                    first_overload.insert(d.id, first);
                }
            }
        }
        let narrowed = kinds.is_some().then_some(narrowed);
        let by_files = by_file.is_some();
        Ok(Resolved { definitions, occurrences, truncated, bounded, by_files, no_kind: false, notes, first_overload, outside: Default::default(), narrowed })
    }

    /// The occurrences of each of `words` as the main read of `references` of it finds them, its
    /// first `max_each`, tied where they are: one search of them all. None may be a name a language
    /// reads in any case.
    fn occurrences_of_each(&mut self, words: &BTreeSet<String>, max_each: u64) -> Result<HashMap<String, Vec<Occurrence>>> {
        use unicode_normalization::UnicodeNormalization;
        #[cfg(test)]
        SEARCHES.with(|c| c.set(c.get() + 1));
        let s = self.s;
        let mut word_of: HashMap<String, String> = HashMap::new();
        for w in words {
            word_of.insert(w.nfc().collect(), w.clone());
            word_of.insert(w.nfd().collect(), w.clone());
        }
        let alternatives: BTreeSet<String> = word_of.keys().map(|k| regex::escape(k)).collect();
        let Some(first) = words.iter().next() else { return Ok(HashMap::new()) };
        let mut q = crate::instant::Query { want_spans: true, ..crate::resolve::name_search(first, 1) };
        q.pattern.text = format!("(?:{})", alternatives.into_iter().collect::<Vec<_>>().join("|"));
        q.pattern.mode = crate::instant::Mode::Regex;
        q.max_results = None;
        let walk = crate::files::repository_files(&s.ix.root);
        let files = crate::stores::grep_files(&s.ix, &q, &walk, s.look.as_ref());
        let (found, _) = crate::intel::occurrences_in(&s.ix.root, files.as_ref().unwrap_or(&walk), &q, &s.cancel)?;
        let (mut out, mut seen): (HashMap<String, Vec<Occurrence>>, HashMap<String, u64>) = Default::default();
        for r in found {
            let crate::instant::Record::Match(h) = r else { continue };
            let line_text: std::sync::Arc<str> = h.text.trim_end_matches(['\n', '\r']).into();
            let lang = self.langs.id(&h.path);
            for &(a, b) in &h.spans {
                let Some(word) = h.text.get(a..b).and_then(|t| word_of.get(t)).cloned() else { continue };
                // the main read takes its first max_results spans, a part of a longer name among them
                let n = seen.entry(word.clone()).or_default();
                *n += 1;
                if *n > max_each {
                    continue;
                }
                let ((da, db), col) = self.rv.decoded_span(&h.path, h.line, (a, b));
                if lang.is_some_and(|l| crate::ident::inside_name(l, &h.text, da, db)) {
                    continue;
                }
                let resolution = self.tie(&h.path, h.line, col, h.text.get(da..db).unwrap_or(&word)).resolution();
                out.entry(word).or_default().push(Occurrence {
                    path: h.path.to_string(),
                    line: h.line,
                    column: a as u64 + 1,
                    text: line_text.clone(),
                    at: da,
                    resolution,
                });
            }
        }
        Ok(out)
    }

    /// The occurrences `references(word, max_results)` gives that read as an instantiation, kept
    /// for the request while `NESTED_MEMO` has room: every class of that name asks the same.
    fn instantiations(&mut self, word: &str, max_results: u64) -> Result<std::rc::Rc<Vec<Occurrence>>> {
        let key = (word.to_string(), max_results);
        if let Some(found) = self.instantiations.get(&key) {
            return Ok(std::rc::Rc::clone(found));
        }
        let found: Vec<Occurrence> = self.references(word, max_results)?.occurrences.into_iter().filter(|o| instantiates(o, word)).collect();
        let found = std::rc::Rc::new(found);
        // the entry, its key and the `Rc`, and each occurrence's buffers by capacity; a line's text
        // is shared by its uses and counted for each
        let bytes = size_of::<((String, u64), std::rc::Rc<Vec<Occurrence>>)>()
            + word.len()
            + 2 * size_of::<usize>()
            + size_of::<Occurrence>() * found.capacity()
            + found.iter().map(|o| o.path.capacity() + o.text.len()).sum::<usize>();
        if self.memo_bytes + bytes <= NESTED_MEMO {
            self.memo_bytes += bytes;
            self.instantiations.insert(key, std::rc::Rc::clone(&found));
        }
        Ok(found)
    }

    /// The one decision about the site of `spelled` at 1-based `line`, 0-based byte `col` of
    /// `path`: what the resolver ties it to; where the resolver does not read the language, the
    /// one definition of the name the call can reach (in the site's own file when there are more).
    pub(crate) fn tie(&mut self, path: &str, line: u64, col: usize, spelled: &str) -> Tie {
        let r = self.rv.resolve_at(path, line, col, spelled);
        let word = match r {
            Ok((_, w)) | Err(w) => w,
        };
        debug_assert!(Reason::of(word).is_some(), "the resolver's word {word:?} is not a Reason");
        let why = match r {
            Ok((id, how)) => {
                let ids = if how == "overload" { self.rv.overloads(id) } else { vec![id] };
                return Tie::Resolved(ids, Reason::of(how).unwrap_or(Reason::Qualified));
            }
            Err(why) => match Reason::of(why) {
                Some(why) => why,
                None => return Tie::Elsewhere(Reason::External),
            },
        };
        match why {
            // a file with no grammar has no call sites in the index, so `callers` could not list it
            Reason::Unread | Reason::TooDeep if self.whole_name(path, line, col, spelled) => {
                let how = if why == Reason::TooDeep { Reason::ByNameTooDeep } else { Reason::ByName };
                self.by_name(path, line, col, spelled, how).unwrap_or(Tie::Undecided(why))
            }
            Reason::Unread | Reason::TooDeep => Tie::Undecided(why),
            Reason::NoGrammar => Tie::Undecided(why),
            Reason::Receiver | Reason::NotFound | Reason::MacroArgument => Tie::Undecided(why),
            _ => Tie::Elsewhere(why),
        }
    }

    /// Whether `spelled` at `col` of `line` is the whole name the language writes there: `valid`
    /// in Ruby's `valid?` is not.
    fn whole_name(&mut self, path: &str, line: u64, col: usize, spelled: &str) -> bool {
        let src = &self.rv.file(path).src;
        let Some(text) = src.lines().nth((line.max(1) - 1) as usize) else { return false };
        let end = col + spelled.len();
        if end > text.len() {
            return false;
        }
        let (a, b) = crate::lsp::widen(text, col, end, path);
        // `x-y` is a subtraction in most languages: a wider name counts only when the index has
        // it, as the language server takes it
        let wider = ((a, b) != (col, end)).then(|| text[a..b].to_string());
        wider.is_none_or(|w| self.s.ix.named(&w).is_ok_and(|d| d.is_empty()))
    }

    /// The by-name tie of a use of `spelled` on `line` of `path`, a file the resolver does not
    /// read: the name of a definition starting there is that definition.
    fn by_name(&mut self, path: &str, line: u64, col: usize, spelled: &str, deep: Reason) -> Option<Tie> {
        let kinds: Vec<&str> = crate::search::CODE_KINDS.to_vec();
        let mut rows: Vec<ChunkRow> = self.s.ix.named_of_kinds(spelled, &kinds).ok()?.into_iter().filter(|r| self.langs.same_family(path, &r.path)).collect();
        if self.langs.folds(path, "function") {
            let more = self.s.ix.named_of_kinds_any_case(spelled, &kinds).ok()?;
            rows.extend(more.into_iter().filter(|r| r.name != spelled && self.langs.folds(&r.path, &r.kind) && self.langs.same_family(path, &r.path)));
        }
        // a definition's own name: on its first line, or starting a line of it at the first line's
        // indent (each clause head of a Haskell function after its signature)
        let src = &self.rv.file(path).src;
        let text = |n: i64| src.lines().nth((n.max(1) - 1) as usize).unwrap_or("");
        let indent = |t: &str| t.len() - t.trim_start().len();
        let here = text(line as i64);
        let head = |r: &ChunkRow| r.start == line as i64 || (r.start..=r.end).contains(&(line as i64)) && indent(here) == col && col == indent(text(r.start));
        if let Some(d) = rows.iter().find(|r| r.path == path && head(r)) {
            return Some(Tie::Resolved(vec![d.id], Reason::Definition));
        }
        let own: Vec<i64> = rows.iter().filter(|r| r.path == path).map(|r| r.id).collect();
        match (&rows[..], &own[..]) {
            ([], _) => None,
            ([one], _) => Some(Tie::ByName(vec![one.id], deep)),
            (_, [mine]) => Some(Tie::ByName(vec![*mine], deep)),
            _ => Some(Tie::Undecided(Reason::Rivals)),
        }
    }

    /// What the resolver makes of the occurrences of `name` in lines `start..=end` of `path`.
    fn uses(&mut self, path: &str, lines: (i64, i64), name: &str) -> Uses {
        let site = (path.to_string(), lines.0, lines.1, name.to_string());
        if let Some(u) = self.stored.as_mut().and_then(|st| st.get(&site)) {
            return u.clone();
        }
        let u = self.resolve_uses(path, lines, name);
        if let Some(st) = &mut self.stored {
            st.put(site, u.clone());
        }
        u
    }

    fn resolve_uses(&mut self, path: &str, (start, end): (i64, i64), name: &str) -> Uses {
        let fi = self.rv.file(path);
        let src = &fi.src;
        // a file of thousands of call sites is not walked from its top for each one
        let starts = self
            .line_starts
            .entry(path.to_string())
            .or_insert_with(|| std::iter::once(0).chain(src.bytes().enumerate().filter(|&(_, c)| c == b'\n').map(|(i, _)| i + 1)).collect());
        let first = (start.max(1) - 1) as usize;
        let from = starts.get(first).copied().unwrap_or(src.len());
        let ident = |c: Option<char>| c.is_some_and(crate::ident::is_continue);
        let methods = crate::resolve::methods_only(self.langs.id(path));
        // ASCII lowercase keeps every byte where it is, so the columns hold
        let any_case = self.langs.folds(path, "function") && name.is_ascii();
        let wanted = if any_case { name.to_ascii_lowercase() } else { name.to_string() };
        let mut uses = Uses { got: HashSet::new(), dispatched: HashSet::new(), unsure: true, unsure_free: true, own_file: false, external_free: false };
        let (mut unsure, mut unsure_free, mut seen, mut elsewhere) = (false, false, false, false);
        for (i, line) in src[from..].lines().enumerate().map(|(i, l)| (i + first, l)).take((end - start + 1).max(0) as usize) {
            let low = if any_case { std::borrow::Cow::Owned(line.to_ascii_lowercase()) } else { std::borrow::Cow::Borrowed(line) };
            for (col, _) in low.match_indices(wanted.as_str()) {
                if ident(line[..col].chars().next_back()) || ident(line[col + name.len()..].chars().next()) {
                    continue;
                }
                match self.tie(path, i as u64 + 1, col, &line[col..col + name.len()]) {
                    // the name of a definition is not a call of it
                    Tie::Resolved(_, Reason::Definition) => {}
                    // a use the arguments do not tie to one overload stands for all of them
                    Tie::Resolved(ids, how) => {
                        seen = true;
                        if how != Reason::Super {
                            uses.dispatched.extend(&ids);
                        }
                        uses.got.extend(ids);
                    }
                    Tie::ByName(ids, _) => {
                        seen = true;
                        uses.dispatched.extend(&ids);
                        uses.got.extend(ids);
                    }
                    Tie::Undecided(why) => {
                        seen = true;
                        unsure = true;
                        unsure_free |= !(methods && line[..col].trim_end().ends_with('.'));
                        let receiver = [".", "->", "::"].iter().any(|r| line[..col].trim_end().ends_with(r));
                        elsewhere |= receiver || !matches!(why, Reason::Rivals | Reason::NoGrammar | Reason::Unread | Reason::TooDeep);
                    }
                    // an import from outside, a local, a field or a string is decided: not this definition
                    Tie::Elsewhere(why) => {
                        seen = true;
                        let receiver = [".", "->", "::"].iter().any(|r| line[..col].trim_end().ends_with(r));
                        uses.external_free |= why == Reason::External && !receiver;
                    }
                }
            }
        }
        uses.unsure = unsure || !seen;
        uses.unsure_free = unsure_free || !seen;
        uses.own_file = unsure && !elsewhere;
        uses
    }

    /// The definitions whose code calls what `name` stands for, and how many of them were kept only
    /// because the resolver could not tell which same-named definition they call. Those come after
    /// the decided ones, first the ones whose code names the target's type, then the ones in its package.
    /// -> (callers: decided, then undecided; how many are undecided)
    pub fn callers(&mut self, name: &str, limit: usize) -> Result<(Vec<Hit>, usize)> {
        self.stored = Some(Stored::open(self.s, "callers", name, limit));
        let r = self.callers_kept(name, limit);
        if let (Some(st), Ok((_, _, complete))) = (self.stored.take(), &r) {
            st.save(self.s, limit, *complete);
        }
        r.map(|(out, undecided, _)| (out, undecided))
    }

    /// `callers`, and whether it went through every call site
    fn callers_kept(&mut self, name: &str, limit: usize) -> Result<(Vec<Hit>, usize, bool)> {
        let s = self.s;
        let langs = Langs { root: &s.ix.root, seen: Default::default() };
        let mut targets = crate::ops::targets(s, name, 500, false)?;
        // a call of a class runs its constructor: `Point(1)`, `new Widget()`, Ruby `Invoice.new`
        let (classes, made_by_new) = constructed(s, &langs, &targets)?;
        targets.extend(classes);
        let ids: HashSet<i64> = targets.iter().map(|t| t.id).collect();
        let mut names: BTreeSet<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        if !made_by_new.is_empty() {
            names.insert("new");
        }
        // `super()` in a JavaScript or TypeScript constructor calls the base class
        if targets.iter().any(|t| t.kind == "class" && matches!(langs.id(&t.path), Some("javascript" | "typescript" | "tsx"))) {
            names.insert("super");
        }
        // PHP `new self()`, `new static()`, `new parent()` make the class the resolver ties them to
        let php_class = targets.iter().any(|t| t.kind == "class" && langs.id(&t.path) == Some("php"));
        if php_class {
            names.extend(["self", "static", "parent"]);
        }
        let owners: HashSet<&str> =
            targets.iter().filter_map(|t| t.qualname.rsplit_once('.').map(|(o, _)| o.rsplit('.').next().unwrap_or(o))).filter(|o| !o.is_empty()).collect();
        let packages: HashSet<&str> = targets.iter().map(|t| package(&t.path)).collect();
        let fits = |path: &str| targets.iter().any(|t| langs.same_family(&t.path, path));
        let (mut out, mut unsure_rows, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
        // a call of a method a target overrides or implements may run the target
        let overridden: HashSet<i64> = targets.iter().flat_map(|t| self.rv.overridden(t.id)).filter(|id| !ids.contains(id)).collect();
        let names_owner = |text: &str| owners.iter().any(|o| has_word(text, o));
        let likely = |r: &ChunkRow| (!names_owner(&r.text), !packages.contains(package(&r.path)), r.path.clone(), r.start);
        let all_free = |n: &str| targets.iter().filter(|t| t.name == n).all(|t| free(&t.kind, &t.qualname));
        for n in names {
            let rows: Vec<Symbol> = s.ix.named(n)?.into_iter().filter(|r| r.kind != "window").collect();
            // with another definition of the name about, the likeliest call sites are asked first
            let rivals = rows.iter().any(|r| !ids.contains(&r.id) && fits(&r.path));
            self.rivals |= rivals;
            let mut sites = s.ix.callers_of(n)?;
            // a call in a language that ignores case may spell the name in another case
            if targets.iter().any(|t| t.name == n && langs.folds(&t.path, &t.kind)) || php_class && matches!(n, "self" | "static" | "parent") {
                for other in self.other_cases(n)? {
                    sites.extend(s.ix.callers_of(&other)?.into_iter().filter(|r| langs.folds(&r.path, "function")));
                }
            }
            // a row is settled once kept; one no call of this name ties may hold another name's call
            let mut once = HashSet::new();
            sites.retain(|r| fits(&r.path) && !seen.contains(&r.id) && once.insert(r.id));
            if n == "new" && !targets.iter().any(|t| t.name == "new") {
                sites.retain(|r| made_by_new.iter().any(|class| has_word(&r.text, &format!("{class}.new"))));
            }
            if rivals {
                sites.sort_by_cached_key(&likely);
            }
            for row in sites {
                // Ruby's `Invoice.new` is a call of the class `Invoice` ties to
                let by_new = if n == "new" { made_by_new.iter().find(|c| has_word(&row.text, &format!("{c}.new"))) } else { None };
                let uses = match by_new {
                    Some(class) => self.uses(&row.path, (row.start, row.end), class),
                    None => self.uses(&row.path, (row.start, row.end), n),
                };
                let unsure = (if all_free(n) { uses.unsure_free } else { uses.unsure }) || !uses.dispatched.is_disjoint(&overridden);
                // a `super` the resolver does not tie to a target is some other class's
                if (n == "super" || php_class && matches!(n, "self" | "static" | "parent")) && uses.got.is_disjoint(&ids) {
                    continue;
                }
                // a call `tie` holds is this definition's; one with only undecided sites of the name
                // may be, or may run it through a method it overrides
                if uses.got.is_disjoint(&ids) {
                    if unsure {
                        seen.insert(row.id);
                        unsure_rows.push(row);
                    }
                    continue;
                }
                seen.insert(row.id);
                out.push(Hit::from_row(&row, true));
                if out.len() >= limit {
                    return Ok((out, 0, false));
                }
            }
        }
        for (local, file) in self.aliases_of(&targets, &langs)? {
            let mut sites = s.ix.callers_of(&local)?;
            // `use function Lib\phpfn as pf` is called `PF()` too where names ignore case
            if langs.folds(&file, "function") {
                for other in self.other_cases(&local)? {
                    sites.extend(s.ix.callers_of(&other)?);
                }
            }
            // a caller kept undecided for another call of the name is decided by this one's tie
            let undecided: HashSet<i64> = unsure_rows.iter().map(|r| r.id).collect();
            sites.retain(|r| r.path == file && (seen.insert(r.id) || undecided.contains(&r.id)));
            for row in sites {
                if !self.uses(&row.path, (row.start, row.end), &local).got.is_disjoint(&ids) {
                    unsure_rows.retain(|r| r.id != row.id);
                    out.push(Hit::from_row(&row, true));
                    if out.len() >= limit {
                        return Ok((out, 0, false));
                    }
                }
            }
        }
        unsure_rows.sort_by_cached_key(&likely);
        let undecided = unsure_rows.len().min(limit - out.len());
        out.extend(unsure_rows.iter().take(undecided).map(|r| Hit::from_row(r, true)));
        Ok((out, undecided, true))
    }

    /// The names the targets are imported under elsewhere, each with the one file that does so:
    /// `from util import helper as h` in `app.py` gives ("h", "app.py"). A call there is stored
    /// under the alias.
    fn aliases_of(&mut self, targets: &[Hit], langs: &Langs) -> Result<Vec<(String, String)>> {
        let mut ids: Vec<i64> = targets.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        if let Some(kept) = self.stored.as_ref().and_then(|st| st.aliases(&ids)) {
            return Ok(kept.clone());
        }
        let found = self.find_aliases(targets, langs)?;
        if let Some(st) = self.stored.as_mut() {
            st.put_aliases(ids, found.clone());
        }
        Ok(found)
    }

    fn find_aliases(&mut self, targets: &[Hit], langs: &Langs) -> Result<Vec<(String, String)>> {
        let targets: Vec<&Hit> = targets.iter().filter(|t| crate::resolve::may_alias(langs.id(&t.path))).collect();
        let names: HashSet<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        // an import names the item (Rust's `use a::helper as h`) or the module that holds it
        let stem = |p: &'_ str| p.rsplit_once('.').map_or(p, |(s, _)| s).to_string();
        let mut needles: BTreeSet<String> = names.iter().map(|n| n.to_string()).collect();
        needles.extend(targets.iter().map(|t| module_name(&stem(&t.path)).to_string()));
        let (mut files, mut asked) = (BTreeSet::new(), BTreeSet::new());
        // a file that imports a target under its own name re-exports it (`pkg/__init__.py`): an
        // import of that file's module may alias it too, a few re-exports deep
        for _ in 0..3 {
            let fresh: Vec<String> = needles.difference(&asked).cloned().collect();
            if fresh.is_empty() {
                break;
            }
            asked.extend(fresh.iter().cloned());
            // past a few names one pass over every import is cheaper than one search per name
            let rows: Vec<(String, String)> = if fresh.len() <= 16 {
                fresh.iter().map(|n| self.s.ix.imports_holding(n)).collect::<Result<Vec<_>>>()?.concat()
            } else {
                self.s.ix.imports()?.into_iter().map(|(path, _, module)| (path, module)).collect()
            };
            for (path, module) in rows {
                let names_it = module.split(|c: char| !crate::ident::is_continue(c)).any(|seg| fresh.contains(&seg.to_string()));
                let kept = names_it && targets.iter().any(|t| langs.same_family(&t.path, &path)) && files.insert(path.clone());
                if kept && names.iter().any(|n| self.rv.imports_as(&path, n)) {
                    needles.insert(module_name(&stem(&path)).to_string());
                }
            }
        }
        let mut out = Vec::new();
        for f in files {
            out.extend(self.rv.aliases(&f).into_iter().filter(|(_, item)| names.contains(item.as_str())).map(|(local, _)| (local, f.clone())));
        }
        Ok(out)
    }

    /// The definitions whose code calls the name of `gone`, a definition the tree no longer has,
    /// where the resolver ties the call to no definition that exists: a namesake the call
    /// resolves to, a local or an import from outside takes it.
    pub fn callers_of_deleted(&mut self, gone: &Hit, limit: usize) -> Result<Vec<Hit>> {
        let s = self.s;
        let langs = Langs { root: &s.ix.root, seen: Default::default() };
        let is_free = free(&gone.kind, &gone.qualname);
        // the call sites `callers` asks for a live definition: the name, its other cases where the
        // language ignores case, and the local names imports give it (their rows still name it)
        let mut sites: Vec<(ChunkRow, String)> = s.ix.callers_of(&gone.name)?.into_iter().map(|r| (r, gone.name.clone())).collect();
        if langs.folds(&gone.path, &gone.kind) {
            for other in self.other_cases(&gone.name)? {
                sites.extend(s.ix.callers_of(&other)?.into_iter().filter(|r| langs.folds(&r.path, "function")).map(|r| (r, other.clone())));
            }
        }
        for (local, file) in self.aliases_of(std::slice::from_ref(gone), &langs)? {
            sites.extend(s.ix.callers_of(&local)?.into_iter().filter(|r| r.path == file).map(|r| (r, local.clone())));
        }
        let (mut out, mut seen) = (Vec::new(), HashSet::new());
        for (row, name) in sites {
            if !langs.same_family(&gone.path, &row.path) || !seen.insert(row.id) {
                continue;
            }
            let uses = self.uses(&row.path, (row.start, row.end), &name);
            if uses.got.is_empty() && (if is_free { uses.unsure_free || uses.external_free } else { uses.unsure }) {
                out.push(Hit::from_row(&row, true));
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// What the code of the definitions `name` stands for calls: definitions of the repository in
    /// the same language family, and the names it calls that have none.
    /// -> (resolved definitions, external names, the files of the definitions whose calls these are)
    pub fn callees(&mut self, name: &str, limit: usize) -> Result<(Vec<Hit>, Vec<String>, Vec<String>)> {
        // every call site is asked whatever the limit, which only cuts the list at the end
        self.stored = Some(Stored::open(self.s, "callees", name, 0));
        let r = self.callees_kept(name, limit);
        if let (Some(st), Ok(_)) = (self.stored.take(), &r) {
            st.save(self.s, 0, true);
        }
        r
    }

    fn callees_kept(&mut self, name: &str, limit: usize) -> Result<(Vec<Hit>, Vec<String>, Vec<String>)> {
        let s = self.s;
        let langs = Langs { root: &s.ix.root, seen: Default::default() };
        let (mut resolved, mut external, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
        let defs = crate::ops::targets(s, name, 5, true)?;
        let from = defs.iter().map(|d| d.path.clone()).collect();
        for d in defs {
            for (n, picked, _) in self.calls(&d, &langs)? {
                if picked.is_empty() && !external.contains(&n) {
                    external.push(n);
                    continue;
                }
                resolved.extend(picked.into_iter().filter(|r| seen.insert(r.id)).map(|r| Hit::from_row(&r, true)));
            }
        }
        external.sort();
        resolved.truncate(limit);
        Ok((resolved, external, from))
    }

    /// What the code of definition `d` calls, name by name: the definitions of the repository each
    /// name reaches, none for a name that reaches none, and whether the resolver tied the calls to
    /// them (else they are the likeliest of the name's definitions).
    fn calls(&mut self, d: &Hit, langs: &Langs) -> Result<Vec<(String, Vec<ChunkRow>, bool)>> {
        let s = self.s;
        let kinds: Vec<&str> = crate::search::CODE_KINDS.iter().copied().chain(["key", "macro"]).collect();
        // a key is what a call runs only in the caller's own language (Nix, whose functions are keys)
        let callable = |from: &str, r: &ChunkRow| langs.same_family(from, &r.path) && (r.kind != "key" || langs.id(from) == langs.id(&r.path));
        let mut out = Vec::new();
        for n in s.ix.callees_of(d.id)? {
            let mut rows: Vec<ChunkRow> = s.ix.named_of_kinds(&n, &kinds)?.into_iter().filter(|r| callable(&d.path, r)).collect();
            // a call in a language that ignores case reaches a definition so named in any case
            if langs.folds(&d.path, "function") {
                let other_case = |r: &ChunkRow| r.name != n && langs.folds(&r.path, &r.kind);
                rows.extend(s.ix.named_of_kinds_any_case(&n, &kinds)?.into_iter().filter(|r| other_case(r) && callable(&d.path, r)));
            }
            // an import alias names what it imports: `h` of `from util import helper as h`
            let folds = langs.folds(&d.path, "function");
            if let Some((_, item)) = self.rv.aliases(&d.path).into_iter().find(|(local, _)| *local == n || folds && local.eq_ignore_ascii_case(&n)) {
                rows.extend(s.ix.named_of_kinds(&item, &kinds)?.into_iter().filter(|r| callable(&d.path, r)));
            }
            // `super(..)`, PHP `new self()` / `static()` / `parent()`: no definition has the name, it
            // stands for the class the resolver ties it to
            let class_word = n == "super" || matches!(n.to_ascii_lowercase().as_str(), "self" | "static" | "parent") && langs.id(&d.path) == Some("php");
            if class_word && rows.is_empty() {
                for id in self.uses(&d.path, (d.start, d.end), &n).got {
                    rows.extend(s.ix.chunk(id)?);
                }
            }
            // Ruby's `Invoice.new` runs the class `Invoice` ties to
            if n == "new" && rows.is_empty() && langs.id(&d.path) == Some("ruby") {
                let made = self.made_by_new(d)?;
                if !made.is_empty() {
                    out.push((n, made, true));
                    continue;
                }
            }
            if rows.is_empty() {
                out.push((n, Vec::new(), false));
                continue;
            }
            let uses = self.uses(&d.path, (d.start, d.end), &n);
            let tied: Vec<&ChunkRow> = rows.iter().filter(|r| uses.got.contains(&r.id)).collect();
            let is_tied = !tied.is_empty();
            let picked: Vec<&ChunkRow> = if is_tied {
                tied
            } else if uses.unsure {
                let mut near: Vec<&ChunkRow> = rows.iter().filter(|r| uses.unsure_free || !free(&r.kind, &r.qualname)).collect();
                let mine = |r: &&ChunkRow| own((&d.path, &d.qualname), &r.path, &r.kind, &r.qualname);
                if uses.own_file && near.iter().any(mine) {
                    near.retain(mine);
                }
                // the nearest three: the caller's file, then its directory
                let dir = |p: &str| p.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
                near.sort_by_key(|r| (r.path != d.path, dir(&r.path) != dir(&d.path)));
                near.truncate(3);
                near
            } else {
                Vec::new()
            };
            let picked = picked.into_iter().cloned().collect();
            out.push((n, picked, is_tied));
        }
        Ok(out)
    }

    /// The classes of the repository the `X.new` calls in the lines of `d` make.
    fn made_by_new(&mut self, d: &Hit) -> Result<Vec<ChunkRow>> {
        let src = self.rv.file(&d.path).src.clone();
        let word = crate::ident::is_continue;
        let mut classes = BTreeSet::new();
        for line in src.lines().skip((d.start.max(1) - 1) as usize).take((d.end - d.start + 1).max(0) as usize) {
            for (at, _) in line.match_indices(".new") {
                if line[at + 4..].chars().next().is_some_and(word) {
                    continue;
                }
                let class = &line[line[..at].rfind(|c: char| !word(c)).map_or(0, |i| i + 1)..at];
                if class.starts_with(char::is_uppercase) {
                    classes.insert(class.to_string());
                }
            }
        }
        let mut out = Vec::new();
        for class in classes {
            for id in self.uses(&d.path, (d.start, d.end), &class).got {
                out.extend(self.s.ix.chunk(id)?.filter(|r| r.kind == "class"));
            }
        }
        Ok(out)
    }

    /// The spellings other than `name` that calls in the index give `name` in another case.
    fn other_cases(&mut self, name: &str) -> Result<Vec<String>> {
        if self.callee_cases.is_none() {
            self.callee_cases = Some(self.s.ix.callees_by_lowercase()?);
        }
        let all = self.callee_cases.as_ref().and_then(|m| m.get(&name.to_ascii_lowercase())).cloned().unwrap_or_default();
        Ok(all.into_iter().filter(|c| c != name).collect())
    }

    /// The definitions under `under` (a directory or file, or all) that nothing in the repository
    /// uses: no line outside one names it (`dead`). One that some line names, none of which the
    /// resolver ties to it, is undecided, with why: a string, a comment, another definition of the
    /// name or a use the resolver cannot place may each still be a use. Entry points are neither.
    pub fn dead(&mut self, under: Option<&str>) -> Result<crate::dead::Dead> {
        self.stored = Some(Stored::open(self.s, "dead", under.unwrap_or(""), 0));
        let r = self.dead_kept(under);
        if let (Some(st), Ok(_)) = (self.stored.take(), &r) {
            st.save(self.s, 0, true);
        }
        r
    }

    fn dead_kept(&mut self, under: Option<&str>) -> Result<crate::dead::Dead> {
        let (candidates, too_large) = crate::dead::candidates(&self.s.ix, under)?;
        let mut out = crate::dead::Dead::default();
        for c in candidates {
            if c.named == 0 && c.own_line > 0 {
                let n = c.own_line;
                out.undecided.push((c.sym, format!("its name stands {n} times on its own first or last line, where no grammar span tells its own from a use")));
                continue;
            }
            if c.named == 0 {
                match (c.base, too_large.first()) {
                    (Some(why), _) => out.undecided.push((c.sym, why)),
                    (None, Some(f)) => out.undecided.push((c.sym, format!("nothing else names it, but {f} is too large to read"))),
                    (None, None) => out.dead.push(c.sym),
                }
                continue;
            }
            let (mut used, mut other, mut unplaced) = (false, 0, 0);
            for (path, line) in &c.lines {
                let u = self.uses(path, (*line, *line), &c.sym.name);
                if u.got.contains(&c.sym.id) {
                    used = true;
                    break;
                }
                if u.unsure {
                    unplaced += 1;
                } else {
                    other += 1;
                }
            }
            if used {
                continue;
            }
            let n = c.named;
            let parts: Vec<String> = [
                (other, "tied to something else (another definition, a local, a comment or a string)"),
                (unplaced, "the resolver cannot place"),
                (c.unread, "in files the resolver does not read"),
                (n - c.unread - c.lines.len(), "not asked"),
            ]
            .iter()
            .filter(|(k, _)| *k > 0)
            .map(|(k, what)| format!("{k} {what}"))
            .collect();
            out.undecided.push((c.sym, format!("named on {n} other line{}: {}", if n == 1 { "" } else { "s" }, parts.join(", "))));
        }
        Ok(out)
    }

    /// The shortest chain of calls from the definition `from` names to the one `to` names, each
    /// call one that `callees` gives, at most `depth` calls long: over the calls the resolver ties
    /// first, and only when they give none, over every call `callees` lists. Ties go to the
    /// definition first by path and line, then to the call on the first line.
    pub fn path(&mut self, from: &str, to: &str, depth: usize) -> Result<CallPath> {
        let s = self.s;
        let (start, goal) = (one_definition(s, from)?, one_definition(s, to)?);
        let langs = Langs { root: &s.ix.root, seen: Default::default() };
        let mut known = HashMap::new();
        let mut walked = self.walk(&start, &goal, depth, true, &langs, &mut known)?;
        if walked.0.is_none() {
            walked = self.walk(&start, &goal, depth, false, &langs, &mut known)?;
        }
        let (steps, stopped) = walked;
        Ok(CallPath { found: steps.is_some(), steps: steps.unwrap_or_default(), from: start, to: goal, depth, stopped })
    }

    /// One breadth-first walk of `path`, through untied calls too unless `tied_only`.
    /// -> (the steps when it reached `goal`, whether `depth` stopped it with definitions left)
    #[allow(clippy::type_complexity)]
    fn walk(
        &mut self,
        start: &Hit,
        goal: &Hit,
        depth: usize,
        tied_only: bool,
        langs: &Langs,
        known: &mut HashMap<i64, Vec<(String, Vec<ChunkRow>, bool)>>,
    ) -> Result<(Option<Vec<Step>>, bool)> {
        let s = self.s;
        // each definition reached, and the call that reached it first
        let mut came: HashMap<i64, Option<(i64, Step)>> = HashMap::from([(start.id, None)]);
        let (mut level, mut taken, mut found) = (vec![start.clone()], 0, start.id == goal.id);
        while !found && !level.is_empty() && taken < depth {
            taken += 1;
            let mut next = Vec::new();
            for d in &level {
                if s.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    anyhow::bail!("cancelled");
                }
                if let std::collections::hash_map::Entry::Vacant(e) = known.entry(d.id) {
                    e.insert(self.calls(d, langs)?);
                }
                let mut edges = Vec::new();
                for (n, picked, tied) in &known[&d.id] {
                    for r in picked.iter().filter(|r| (*tied || !tied_only) && !came.contains_key(&r.id)) {
                        edges.push((self.call_line(&d.path, (d.start, d.end), n, r.id), r.clone(), *tied));
                    }
                }
                edges.sort_by(|a, b| (a.0, &a.1.path, a.1.start).cmp(&(b.0, &b.1.path, b.1.start)));
                for (line, r, tied) in edges {
                    if came.contains_key(&r.id) {
                        continue;
                    }
                    let to = Hit::from_row(&r, false);
                    found |= r.id == goal.id;
                    came.insert(r.id, Some((d.id, Step { path: d.path.clone(), line, to: to.clone(), tied })));
                    next.push(to);
                }
                if found {
                    break;
                }
            }
            next.sort_by(|a, b| (&a.path, a.start, a.end).cmp(&(&b.path, b.start, b.end)));
            level = next;
        }
        if !found {
            return Ok((None, taken == depth && !level.is_empty()));
        }
        let mut steps = Vec::new();
        let mut at = goal.id;
        while let Some(Some((parent, step))) = came.get(&at) {
            steps.push(step.clone());
            at = *parent;
        }
        steps.reverse();
        Ok((Some(steps), false))
    }

    /// The first line in `lines` of `path` where `name` is called and the resolver ties the call to
    /// definition `id`, else the first where `name` is called (a pick the resolver could not tie).
    fn call_line(&mut self, path: &str, (start, end): (i64, i64), name: &str, id: i64) -> i64 {
        let fi = self.rv.file(path);
        let ident = |c: Option<char>| c.is_some_and(crate::ident::is_continue);
        let mut first = None;
        let first_line = start.max(1);
        for (i, line) in fi.src.lines().enumerate().skip(first_line as usize - 1).take((end - first_line + 1).max(0) as usize) {
            let n = i as i64 + 1;
            for (col, _) in line.match_indices(name) {
                if ident(line[..col].chars().next_back()) || ident(line[col + name.len()..].chars().next()) {
                    continue;
                }
                match self.rv.resolve_at(path, n as u64, col, name) {
                    Ok((_, "definition")) => continue,
                    Ok((got, why)) if got == id || (why == "overload" && self.rv.overloads(got).contains(&id)) => return n,
                    _ => {}
                }
                first.get_or_insert(n);
            }
        }
        first.unwrap_or(start)
    }

    /// The weighted edges between `files` (sorted) that the index's names give, without resolving
    /// a call site: a call of a name goes to every other file of the caller's language family
    /// that defines it, its weight split among them, unless the caller's own file defines it; an
    /// import goes to the file its module path maps onto. -> (from, to, weight) by index into `files`
    pub fn file_edges(&mut self, files: &[String]) -> Result<Vec<(usize, usize, f64)>> {
        let s = self.s;
        let langs = Langs { root: &s.ix.root, seen: Default::default() };
        let at = |p: &str| files.binary_search_by(|f| f.as_str().cmp(p)).ok();
        let kinds: Vec<&str> = crate::search::CODE_KINDS.iter().copied().chain(["macro"]).collect();
        let mut definers: HashMap<String, Vec<usize>> = HashMap::new();
        for (name, path) in s.ix.definers(&kinds)? {
            if let Some(i) = at(&path) {
                definers.entry(name).or_default().push(i);
            }
        }
        let fam: Vec<Option<&str>> = files.iter().map(|f| langs.id(f).map(family)).collect();
        let same = |a: usize, b: usize| fam[a].is_none() || fam[b].is_none() || fam[a] == fam[b];
        // by file, the files it calls into and imports, with their weights
        let mut out: Vec<HashMap<usize, f64>> = vec![HashMap::new(); files.len()];
        // rows come by path and name: the calls of one name from one file are counted together
        let calls = s.ix.call_edges()?;
        for run in calls.chunk_by(|a, b| a == b) {
            let (path, callee) = &run[0];
            let (Some(from), Some(defs)) = (at(path), definers.get(callee)) else { continue };
            if defs.contains(&from) {
                continue;
            }
            let to: Vec<usize> = defs.iter().copied().filter(|&t| same(from, t)).collect();
            for &t in &to {
                *out[from].entry(t).or_default() += run.len() as f64 / to.len() as f64;
            }
        }
        // a module is mapped once per directory it is imported from
        let mut mapped: HashMap<(String, String), Option<usize>> = HashMap::new();
        for (path, _, module) in s.ix.imports()? {
            let Some(from) = at(&path) else { continue };
            let dir = path.rsplit_once('/').map_or("", |(d, _)| d).to_string();
            let to =
                *mapped.entry((module, dir)).or_insert_with_key(|(module, _)| module_file(module, &path, self.rv.files(), &s.ix.root).and_then(|f| at(&f)));
            if let Some(to) = to.filter(|&t| t != from) {
                *out[from].entry(to).or_default() += 1.0;
            }
        }
        let mut edges = Vec::new();
        for (from, to) in out.into_iter().enumerate() {
            let mut to: Vec<(usize, f64)> = to.into_iter().collect();
            to.sort_by_key(|e| e.0);
            edges.extend(to.into_iter().map(|(t, w)| (from, t, w)));
        }
        Ok(edges)
    }

    /// Modules a file imports, resolved to repository files where the module path maps onto one.
    /// A statement naming several files (`from . import a, b`) is listed once per file.
    pub fn imports(&mut self, path: &str) -> Result<Vec<Dependency>> {
        let rows = self.s.ix.imports_of(path)?;
        let mut out = Vec::new();
        for (line, module) in rows {
            let mut found = self.import_files(&module, path, line);
            if path.ends_with(".go") && found.len() > 1 {
                // a Go import names a package directory: the file named after it stands for it
                let dir = found[0].rsplit_once('/').map_or("", |(d, _)| d);
                let named = format!("{}/{}.go", dir, dir.rsplit('/').next().unwrap_or(dir));
                let pick = found.iter().find(|f| **f == named).or_else(|| found.iter().find(|f| !f.ends_with("_test.go"))).unwrap_or(&found[0]).clone();
                found = vec![pick];
            }
            if found.is_empty() {
                out.push(Dependency { line, module: module.clone(), resolved: None });
            }
            out.extend(found.into_iter().map(|f| Dependency { line, module: module.clone(), resolved: Some(f) }));
        }
        for (line, name, file) in self.rv.rust_mods(path) {
            out.push(Dependency { line, module: name, resolved: file });
        }
        out.sort_by_key(|d| d.line);
        Ok(out)
    }

    /// The files one recorded import names: through the resolver `refs` uses for the languages it
    /// reads, else by mapping the module path onto the repository's files.
    fn import_files(&mut self, module: &str, from: &str, line: i64) -> Vec<String> {
        match self.rv.import_files(from, line, module) {
            Some(found) => found,
            None => module_file(module, from, self.rv.files(), &self.s.ix.root).into_iter().collect(),
        }
    }

    /// Files that import `target`: a module name as written, or a repository file (every file
    /// whose imports resolve to it, plus modules spelled like its path), and how many imports
    /// resolved to no file although they may import `target`: a relative one whose path is the
    /// target's, or a bare one that names the target's module.
    pub fn importers(&mut self, target: &str) -> Result<(Vec<Importer>, usize)> {
        let rows = self.s.ix.imports()?;
        let is_file = self.rv.files().contains(target);
        let stem = target.rsplit_once('.').map(|(s, _)| s).unwrap_or(target);
        let stem = stem.strip_suffix(".d").unwrap_or(stem);
        let dotted: String = stem.replace('/', ".");
        let dotted_pkg =
            dotted.strip_suffix(".__init__").or_else(|| dotted.strip_suffix(".index")).or_else(|| dotted.strip_suffix(".mod")).map(|s| s.to_string());
        let own = module_name(stem);
        let target_dir = target.rsplit_once('/').map_or("", |(d, _)| d);
        let (mut out, mut unsure) = (Vec::new(), 0);
        let mut prev = String::new();
        for (path, line, module) in rows {
            // rows come by path: an importer is dropped once read, so memory does not grow with their number
            if path != prev {
                self.rv.forget(&prev);
                prev.clone_from(&path);
            }
            if path == target {
                continue;
            }
            let hit = if is_file {
                let found = self.import_files(&module, &path, line);
                let hit = found.iter().any(|f| f == target)
                    || module == dotted
                    || module.replace("::", ".") == dotted
                    || dotted_pkg.as_deref().map(|p| module == p || module.replace("::", ".") == p).unwrap_or(false)
                    // an include found through a search path the index does not know
                    || (found.is_empty() && module.rsplit('/').next() == Some(target.rsplit('/').next().unwrap_or(target)));
                let may_name = match relative_path(&module, &path).filter(|_| module.starts_with('.')) {
                    // a relative import names one path: the file without its extension, or its directory
                    Some(p) => p == stem || p == target || p == target_dir,
                    // a bare name in JavaScript or TypeScript that the resolver did not map is a package
                    None => {
                        !JS_EXTS.iter().any(|e| path.ends_with(e))
                            // a package name with `-` is one segment here, unlike a name
                            && module.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-')).any(|seg| seg == own)
                    }
                };
                if !hit && found.is_empty() && may_name {
                    unsure += 1;
                }
                hit
            } else {
                module == target
                    || module.starts_with(&format!("{target}."))
                    || module.starts_with(&format!("{target}::"))
                    || module.starts_with(&format!("{target}/"))
                    || module.ends_with(&format!(".{target}"))
                    || module.ends_with(&format!("::{target}"))
                    || module.ends_with(&format!("/{target}"))
            };
            if hit {
                out.push((path, line, module));
            }
        }
        if is_file && target.ends_with(".rs") {
            for parent in self.rv.rust_mod_parents(target) {
                for (line, name, file) in self.rv.rust_mods(&parent) {
                    if file.as_deref() == Some(target) {
                        out.push((parent.clone(), line, name));
                    }
                }
            }
            out.sort();
            out.dedup();
        }
        Ok((out, unsure))
    }
}

/// A chain of calls between two definitions; `found` false: none within `depth` calls, and
/// `stopped`: the search stopped there with definitions left to follow.
#[derive(Clone, Debug)]
pub struct CallPath {
    pub from: Hit,
    pub to: Hit,
    pub steps: Vec<Step>,
    pub found: bool,
    pub depth: usize,
    pub stopped: bool,
}

/// One call of a chain: the call site, the definition it reaches, and whether the resolver tied
/// the call to it (else it is one of the likeliest definitions of the name, as `callees` lists).
#[derive(Clone, Debug)]
pub struct Step {
    pub path: String,
    pub line: i64,
    pub to: Hit,
    pub tied: bool,
}

/// The one definition `name` (a name or a location) stands for; several are an error that lists them.
fn one_definition(s: &Searcher, name: &str) -> Result<Hit> {
    const SHOWN: usize = 20;
    let mut found = crate::ops::targets(s, name, SHOWN + 1, false)?;
    // a file's module chunk takes the file's name: `def main` in main.py is the one meant
    if found.iter().any(|h| h.kind != "module") {
        found.retain(|h| h.kind != "module");
    }
    match found.len() {
        0 => anyhow::bail!("no definition of {}", crate::fmt::echo(name)),
        1 => Ok(found.remove(0)),
        n => {
            let count = if n > SHOWN { format!("more than {SHOWN}") } else { n.to_string() };
            found.truncate(SHOWN);
            let listed = crate::fmt::hit_blocks(&found, &[], false, false).join("\n  ");
            anyhow::bail!("{} names {count} definitions: pass one as a location (path:line)\n  {listed}", crate::fmt::echo(name))
        }
    }
}

/// Languages whose code calls each other's definitions directly.
fn family(id: &'static str) -> &'static str {
    match id {
        "javascript" | "typescript" | "tsx" => "js",
        "c" | "cpp" | "cuda" | "objc" => "c",
        "java" | "kotlin" | "scala" | "groovy" => "jvm",
        "erlang" | "elixir" | "gleam" => "beam",
        "ocaml" | "ocaml_interface" => "ocaml",
        "csharp" | "fsharp" => "dotnet",
        id => id,
    }
}

/// The language of each file an answer asks about, by its text where the extension names two.
struct Langs<'a> {
    root: &'a Path,
    seen: std::cell::RefCell<HashMap<String, Option<&'static str>>>,
}

impl Langs<'_> {
    fn id(&self, path: &str) -> Option<&'static str> {
        if let Some(id) = self.seen.borrow().get(path) {
            return *id;
        }
        let id = crate::langs::detect_at(self.root, path).map(|s| s.id);
        self.seen.borrow_mut().insert(path.to_string(), id);
        id
    }

    /// Whether a definition of `path` is named in any case (`langs::ignores_case_at`).
    fn folds(&self, path: &str, kind: &str) -> bool {
        crate::langs::ignores_case_at(path, self.id(path), kind)
    }

    /// A file of no known language may call or be called by any.
    fn same_family(&self, a: &str, b: &str) -> bool {
        match (self.id(a), self.id(b)) {
            (Some(x), Some(y)) => family(x) == family(y),
            _ => true,
        }
    }
}

/// How a site is tied to a definition, or why it is not: the resolver's words, as one closed set
/// every answer shows through `as_str`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Reason {
    Definition,
    Import,
    Member,
    Qualified,
    Super,
    Overload,
    Variable,
    Module,
    Local,
    Package,
    Namespace,
    Type,
    Field,
    Constant,
    Declaration,
    Default,
    /// the resolver does not read the language; the name has one definition the call can reach
    ByName,
    /// by name, in a file nested too deeply for the resolver to read
    ByNameTooDeep,
    /// an instantiation of the class, a use of its constructor
    Class,
    External,
    NotFound,
    Receiver,
    FieldOrKey,
    LocalVariable,
    Keyword,
    KeywordArgument,
    MacroArgument,
    TypeParameter,
    CommentOrString,
    NotAnIdentifier,
    NoGrammar,
    Unread,
    TooDeep,
    /// by name only, and the name has more than one definition the call can reach
    Rivals,
}

impl Reason {
    const ALL: [Reason; 34] = [
        Reason::Definition,
        Reason::Import,
        Reason::Member,
        Reason::Qualified,
        Reason::Super,
        Reason::Overload,
        Reason::Variable,
        Reason::Module,
        Reason::Local,
        Reason::Package,
        Reason::Namespace,
        Reason::Type,
        Reason::Field,
        Reason::Constant,
        Reason::Declaration,
        Reason::Default,
        Reason::ByName,
        Reason::ByNameTooDeep,
        Reason::Class,
        Reason::External,
        Reason::NotFound,
        Reason::Receiver,
        Reason::FieldOrKey,
        Reason::LocalVariable,
        Reason::Keyword,
        Reason::KeywordArgument,
        Reason::MacroArgument,
        Reason::TypeParameter,
        Reason::CommentOrString,
        Reason::NotAnIdentifier,
        Reason::NoGrammar,
        Reason::Unread,
        Reason::TooDeep,
        Reason::Rivals,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Reason::Definition => "definition",
            Reason::Import => "import",
            Reason::Member => "member",
            Reason::Qualified => "qualified",
            Reason::Super => "super",
            Reason::Overload => "overload",
            Reason::Variable => "variable",
            Reason::Module => "module",
            Reason::Local => "local",
            Reason::Package => "package",
            Reason::Namespace => "namespace",
            Reason::Type => "type",
            Reason::Field => "field",
            Reason::Constant => "constant",
            Reason::Declaration => "declaration",
            Reason::Default => "default",
            Reason::ByName => "by name",
            Reason::ByNameTooDeep => "by name: nested too deeply to read",
            Reason::Class => "class",
            Reason::External => "external",
            Reason::NotFound => "not found",
            Reason::Receiver => "receiver",
            Reason::FieldOrKey => "field or key",
            Reason::LocalVariable => "local variable",
            Reason::Keyword => "keyword",
            Reason::KeywordArgument => "keyword argument",
            Reason::MacroArgument => "macro argument",
            Reason::TypeParameter => "type parameter",
            Reason::CommentOrString => "comment or string",
            Reason::NotAnIdentifier => "not an identifier",
            Reason::NoGrammar => "no grammar",
            Reason::Unread => crate::resolve::UNREAD,
            Reason::TooDeep => crate::resolve::TOO_DEEP,
            Reason::Rivals => "by name, with other definitions of the name",
        }
    }

    /// The reason the resolver's word `s` is.
    pub(crate) fn of(s: &str) -> Option<Reason> {
        Reason::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// What one site of a name is tied to: the one decision `refs`, `callers` and `callees` all read.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tie {
    /// the resolver placed it: on these definitions (the overloads a call does not pick between)
    Resolved(Vec<i64>, Reason),
    /// the resolver does not read the language, and the name has one definition the call reaches
    ByName(Vec<i64>, Reason),
    /// read but not placed, or by name with rivals: it may be any definition of the name
    Undecided(Reason),
    /// decided: no definition of the repository (a local, an import from outside, a keyword)
    Elsewhere(Reason),
}

impl Tie {
    /// The site as `refs` lists it: a definition and how, or why it stayed unresolved.
    pub(crate) fn resolution(&self) -> Resolution {
        match self {
            Tie::Resolved(ids, how) => Ok((ids[0], how.as_str())),
            Tie::ByName(ids, how) => Ok((ids[0], how.as_str())),
            Tie::Undecided(why) | Tie::Elsewhere(why) => Err(why.as_str()),
        }
    }
}

/// What the resolver makes of the occurrences of a name in some lines of a file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Uses {
    /// the definitions they resolve to
    pub(crate) got: HashSet<i64>,
    /// the ones of `got` a call reaches other than through `super`, where an override may run instead
    pub(crate) dispatched: HashSet<i64>,
    /// one stayed unresolved (or none was found)
    pub(crate) unsure: bool,
    /// one stayed unresolved that may call a free function: not `x.name(..)` where a method
    /// call never reaches a free function
    pub(crate) unsure_free: bool,
    /// every unresolved one is a call without a receiver in a language the resolver does not
    /// read: it runs the definition in its own file when there is one (`own`)
    pub(crate) own_file: bool,
    /// one without a receiver names what the repository defines nowhere: a call of a definition
    /// the tree no longer has may be
    pub(crate) external_free: bool,
}

/// Whether the definition (`path`, `kind`, `qualname`) is what a call without a receiver in the
/// caller (its path and qualified name) runs: a free function of the caller's file, or a method
/// of the caller's own type.
fn own((from, caller): (&str, &str), path: &str, kind: &str, qualname: &str) -> bool {
    let owner = |q: &str| q.rsplit_once('.').map(|(o, _)| o.to_string());
    path == from && (free(kind, qualname) || owner(qualname) == owner(caller))
}

/// A definition outside any class or type: `x.f()` does not call it where `methods_only` holds.
/// A method of an object literal has no owner in its name and is still called through one.
fn free(kind: &str, qualname: &str) -> bool {
    kind != "method" && !qualname.contains('.')
}

/// The directory a file's package starts at: what comes before its `src/`, else its top directory.
fn package(path: &str) -> &str {
    match path.strip_prefix("src/").map(|_| 0).or_else(|| path.find("/src/")) {
        Some(i) => &path[..i],
        None => path.split_once('/').map_or("", |(top, _)| top),
    }
}

/// The last name of the module a file is, from its path without the extension: `a/b` -> `b`,
/// `a/b/__init__` -> `b`.
fn module_name(stem: &str) -> &str {
    let mut parts = stem.rsplit('/');
    let last = parts.next().unwrap_or(stem);
    if matches!(last, "__init__" | "index" | "mod" | "lib" | "main") { parts.next().unwrap_or(last) } else { last }
}

/// Whether `word` is in `text` where it is not part of a longer identifier.
fn has_word(text: &str, word: &str) -> bool {
    let ident = |c: Option<char>| c.is_some_and(crate::ident::is_continue);
    text.match_indices(word).any(|(i, _)| !ident(text[..i].chars().next_back()) && !ident(text[i + word.len()..].chars().next()))
}

/// The classes whose constructor is among `targets`, and the names of the Ruby ones, which a
/// call of `new` on the class makes.
fn constructed(s: &Searcher, langs: &Langs, targets: &[Hit]) -> Result<(Vec<Hit>, Vec<String>)> {
    let (mut classes, mut made_by_new) = (Vec::<Hit>::new(), Vec::new());
    for t in targets {
        let Some((owner, name)) = t.qualname.rsplit_once('.') else { continue };
        let lang = langs.id(&t.path).unwrap_or("");
        let constructor = match lang {
            "python" => name == "__init__",
            "javascript" | "typescript" | "tsx" => name == "constructor",
            "ruby" => name == "initialize",
            "php" => name == "__construct",
            "swift" => name == "init",
            // Java, C#, C++, Dart: the method named as its class
            _ => name == owner.rsplit('.').next().unwrap_or(owner),
        };
        if !constructor {
            continue;
        }
        let class = s.ix.class_at(&t.path, owner)?;
        let Some(class) = class.filter(|c| !targets.iter().chain(&classes).any(|x| x.id == c.id)) else { continue };
        if lang == "ruby" {
            made_by_new.push(class.name.clone());
        }
        classes.push(Hit::from_row(&class, true));
    }
    Ok((classes, made_by_new))
}

const SOURCE_EXTS: &[&str] = &[
    ".py", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".go", ".rs", ".java", ".rb", ".php", ".kt", ".swift", ".scala", ".dart", ".ex", ".exs", ".hs",
    ".lua", ".c", ".h", ".cc", ".cpp", ".hpp", ".cs", ".jl", ".r", ".R", ".sh", ".zig", ".elm",
];

const JS_EXTS: &[&str] = &[".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"];

/// The repository path a relative import of `from` names before an extension or an index file
/// is tried (`./x`, `../x`, Python's `.x`, `..pkg.mod` and `.`); None for any other module.
fn relative_path(module: &str, from: &str) -> Option<String> {
    let dir = from.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    if module.starts_with("./") || module.starts_with("../") {
        Some(join(dir, module))
    } else if module.starts_with('.') {
        Some(crate::resolve::py_relative(from, module))
    } else {
        None
    }
}

/// For the languages the resolver does not read: a best-effort mapping of an import string onto a
/// file of the repository. Handles dotted/`::`-separated module paths, relative paths (`./x`,
/// `../x`, `from . import`), `crate::` roots and index files.
fn module_file(module: &str, from: &str, files: &HashSet<String>, root: &Path) -> Option<String> {
    let dir = from.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut candidates: Vec<String> = Vec::new();
    if let Some(path) = relative_path(module, from) {
        candidates.push(path);
    } else {
        let m = module.strip_prefix("crate::").map(|m| format!("src/{m}")).unwrap_or_else(|| module.to_string());
        let slashed = m.replace("::", "/").replace('.', "/");
        candidates.push(slashed.clone());
        // java-style: the last segment may be a class inside the file of the previous segment
        if let Some((head, _)) = slashed.rsplit_once('/') {
            candidates.push(head.to_string());
        }
        // same directory first (C includes, sibling modules)
        if !dir.is_empty() {
            candidates.insert(0, join(dir, &slashed));
            candidates.insert(0, join(dir, &m));
        }
        candidates.push(m);
    }
    let try_one = |c: &str| -> Option<String> {
        let c = c.trim_matches('/');
        if files.contains(c) {
            return Some(c.to_string());
        }
        for ext in SOURCE_EXTS {
            let p = format!("{c}{ext}");
            if files.contains(&p) {
                return Some(p);
            }
        }
        for idx in ["__init__.py", "index.js", "index.ts", "index.tsx", "mod.rs", "lib.rs", "index.mjs"] {
            let p = if c.is_empty() { idx.to_string() } else { format!("{c}/{idx}") };
            if files.contains(&p) {
                return Some(p);
            }
        }
        None
    };
    for c in &candidates {
        if let Some(p) = try_one(c) {
            return Some(p);
        }
    }
    if module.starts_with('.') {
        return None;
    }
    let slashed = module.replace("::", "/").replace('.', "/");
    let segs: Vec<&str> = slashed.split('/').filter(|s| !s.is_empty()).collect();
    crate::resolve::below_own_package(root, &segs).and_then(|rest| try_one(&rest.join("/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `m/secrets.py` with `redact`, and thirty files under `app/` that import its module, every
    /// third calling it.
    fn tree(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_names_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("m")).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("m/__init__.py"), "").unwrap();
        std::fs::write(root.join("m/secrets.py"), "def redact(text):\n    return text\n").unwrap();
        for i in 0..30 {
            let body = if i % 3 == 0 {
                format!("import m.secrets\n\n\ndef f{i}():\n    return m.secrets.redact({i})\n")
            } else {
                format!("import m.secrets\n\n\ndef g{i}():\n    return {i}\n")
            };
            std::fs::write(root.join(format!("app/a{i}.py")), body).unwrap();
        }
        root
    }

    /// Whether this file system keeps answers: nothing is kept where it does not say when the
    /// index was made.
    fn keeps(root: &std::path::Path) -> bool {
        let known = std::fs::metadata(root.join(".scrutate/index.db")).unwrap().created().is_ok();
        assert!(known || !cfg!(any(target_os = "linux", target_os = "macos", windows)), "no creation time on a system that has one");
        known
    }

    fn callers(repo: &mut crate::Repo) -> Vec<(String, i64)> {
        let (hits, _) = repo.recovering(|s| Names::new(s)?.callers("redact", 31)).unwrap();
        hits.into_iter().map(|h| (h.path, h.start)).collect()
    }

    /// A repeat of `callers` on an unchanged index and tree answers from what the first run kept:
    /// it loads no file into the resolver, the files that import the target's module included.
    #[test]
    fn a_repeat_callers_loads_no_file_into_the_resolver() {
        let root = tree("kept");
        let (mut repo, _) = crate::open_updated(&root, true).unwrap();
        if !keeps(&root) {
            return;
        }
        let first = callers(&mut repo);
        assert_eq!(first.len(), 10, "{first:?}");
        crate::resolve::LOADS.with(|n| n.set(0));
        assert_eq!(callers(&mut repo), first);
        assert_eq!(crate::resolve::LOADS.with(|n| n.get()), 0, "the repeat loaded files into the resolver");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A kept answer never hides an importer added, edited or deleted since: every update of the
    /// index moves its data version, which the kept file is keyed on, so a file the stored run
    /// never read still counts.
    #[test]
    fn a_kept_answer_never_hides_an_importer_added_edited_or_deleted_since() {
        let root = tree("moved");
        let (mut repo, _) = crate::open_updated(&root, true).unwrap();
        keeps(&root);
        let kept = root.join(".scrutate/answers");
        let step = |repo: &mut crate::Repo, change: &dyn Fn()| -> Vec<(String, i64)> {
            callers(repo);
            change();
            repo.update(false).unwrap();
            let now = callers(repo);
            let _ = std::fs::remove_dir_all(&kept);
            assert_eq!(now, callers(repo), "a run with nothing kept answers otherwise");
            now
        };
        let added = |w: &[(String, i64)], f: &str| w.iter().any(|(p, _)| p == f);
        let now =
            step(&mut repo, &|| std::fs::write(root.join("app/new.py"), "from m.secrets import redact as scrub\n\n\ndef n():\n    return scrub(1)\n").unwrap());
        assert!(added(&now, "app/new.py"), "a new importer: {now:?}");
        let now =
            step(&mut repo, &|| std::fs::write(root.join("app/a1.py"), "from m.secrets import redact as wipe\n\n\ndef g1():\n    return wipe(1)\n").unwrap());
        assert!(added(&now, "app/a1.py"), "an importer by an edit: {now:?}");
        let now = step(&mut repo, &|| std::fs::remove_file(root.join("app/new.py")).unwrap());
        assert!(!added(&now, "app/new.py"), "a deleted importer: {now:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
