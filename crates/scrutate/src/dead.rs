//! Dead code: the definitions nothing in the repository names. This module reads the text: every
//! whole-word occurrence of each definition's name in every file of the repository, and the rules
//! that make a definition an entry point of its language (`main`, tests, exports, decorators).
//! What an occurrence refers to is asked of `Names` (`Names::dead`), which owns that question.

use std::collections::HashMap;
use std::path::Path;

use rayon::prelude::*;

use crate::index::Symbol;

/// The kinds a definition can be dead of: what a call or a type use names.
const KINDS: &[&str] = &["function", "method", "class", "struct", "trait", "interface", "type", "enum"];
/// Read with them: what holds a method and says what it implements.
const HOLDERS: &[&str] = &["impl"];
/// A file larger than this is not read: a definition it may name is not called dead.
const MAX_READ: u64 = 64 << 20;
/// Lines of a name the resolver is asked about at most, per definition.
pub(crate) const ASKED: usize = 16;

/// The answer: definitions nothing uses, and those the resolver could not decide, each with why.
#[derive(Debug, Default)]
pub struct Dead {
    pub dead: Vec<Symbol>,
    pub undecided: Vec<(Symbol, String)>,
}

/// What the text says of one definition that is not an entry point.
pub(crate) struct Candidate {
    pub sym: Symbol,
    /// how many lines outside it name it, and how many of those are in files the resolver does not read
    pub named: usize,
    pub unread: usize,
    /// the first `ASKED` of those in files the resolver reads, as (file, line): its own file first
    pub lines: Vec<(String, i64)>,
    /// with no line naming it, why it may still be used (its class extends another)
    pub base: Option<String>,
    /// uses on its first or last line that no span tells from its own
    pub own_line: usize,
}

/// What the pass over one file found: (name, line, byte) of each occurrence, the rule of each
/// wanted definition in it, whether it was too large to read, and the byte span of each wanted
/// definition a use on its first or last line may lie outside of (None: no grammar gave one).
type Scanned = (Vec<(u32, i64, usize)>, Vec<(i64, Rule)>, bool, Vec<(i64, Option<(usize, usize)>)>);

/// What a definition is to its language before any use is looked for.
enum Rule {
    Plain,
    /// an entry point: never dead
    Entry,
    /// used without being named when its base type's code calls it
    Base(String),
}

/// The definitions under `under` (a directory or file from the root, or all) that are not entry
/// points, each with the lines of the repository that name it, and the files too large to read.
pub(crate) fn candidates(ix: &crate::index::Index, under: Option<&str>) -> anyhow::Result<(Vec<Candidate>, Vec<String>)> {
    let root = ix.root.as_path();
    let all = ix.symbols_of_kinds(&[KINDS, HOLDERS].concat())?;
    let inside = |p: &str| under.is_none_or(|u| p == u || p.starts_with(&format!("{}/", u.trim_end_matches('/'))));
    let mut by_file: HashMap<&str, Vec<&Symbol>> = HashMap::new();
    for s in &all {
        by_file.entry(s.path.as_str()).or_default().push(s);
    }
    let wanted: Vec<&Symbol> = all.iter().filter(|s| KINDS.contains(&s.kind.as_str()) && !s.name.is_empty() && inside(&s.path)).collect();
    // the names uses are found by, and the writers' (`label=`), each with its own number
    let (mut name_ids, mut writer_ids): (HashMap<&str, u32>, HashMap<&str, u32>) = Default::default();
    // where each definition starts, by qualified name: a declaration and its body (Pascal's
    // `procedure Tidy;` in the class and `procedure TFoo.Tidy`) do not use each other
    let mut starts: HashMap<&str, Vec<(&str, i64)>> = HashMap::new();
    let mut id_of: Vec<u32> = Vec::with_capacity(wanted.len());
    for s in &wanted {
        let n = (name_ids.len() + writer_ids.len()) as u32;
        let (key, writer) = looked_for(&s.name);
        id_of.push(*if writer { writer_ids.entry(key) } else { name_ids.entry(key) }.or_insert(n));
        starts.entry(s.qualname.as_str()).or_default().push((s.path.as_str(), s.start));
    }
    // a name in a language that ignores case is found in any case, by its lowercase form; ASCII
    // letters only, as Fortran, Pascal, Ada and PHP fold no others in their names
    let mut folded: HashMap<String, Vec<u32>> = HashMap::new();
    let mut lang_of: HashMap<&str, Option<&'static str>> = HashMap::new();
    for (s, &id) in wanted.iter().zip(&id_of).filter(|(s, _)| s.name.is_ascii()) {
        let lang = *lang_of.entry(s.path.as_str()).or_insert_with(|| crate::langs::detect_at(root, &s.path).map(|l| l.id));
        if !crate::langs::ignores_case_at(&s.path, lang, &s.kind) {
            continue;
        }
        let ids = folded.entry(looked_for(&s.name).0.to_ascii_lowercase()).or_default();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let id_by_sym: HashMap<i64, u32> = wanted.iter().zip(&id_of).map(|(s, &id)| (s.id, id)).collect();
    let odd: Vec<&str> = name_ids.keys().copied().filter(|n| !n.chars().all(word_char)).collect();
    let odd_finder = aho_corasick::AhoCorasick::new(&odd)?;
    let files = crate::files::every_file(root);
    let scanned: Vec<Scanned> = files
        .par_iter()
        .map(|rel| {
            let Some(raw) = crate::files::read_regular(&root.join(rel), MAX_READ) else {
                let big = std::fs::metadata(root.join(rel)).is_ok_and(|m| m.len() > MAX_READ);
                return (Vec::new(), Vec::new(), big, Vec::new());
            };
            let Some(src) = crate::index::decode_source(&raw) else { return (Vec::new(), Vec::new(), false, Vec::new()) };
            let lang = crate::langs::detect_at(root, rel).map(|l| l.id);
            let found = occurrences_at(&src, lang, &name_ids, &writer_ids, &folded, &odd, &odd_finder);
            let mine: Vec<&Symbol> = by_file
                .get(rel.as_str())
                .map(|v| v.iter().copied().filter(|s| KINDS.contains(&s.kind.as_str()) && inside(&s.path)).collect())
                .unwrap_or_default();
            // parsed once, when a definition shares a line with another or names itself again there
            let chunks: std::cell::OnceCell<Vec<crate::parse::Chunk>> = Default::default();
            let parsed = || chunks.get_or_init(|| crate::parse::parse_file_with(rel, &src, None).0.chunks).as_slice();
            let rules = if mine.is_empty() { Vec::new() } else { file_rules(root, rel, &src, &mine, &by_file[rel.as_str()], &parsed) };
            let spans = edge_spans(&mine, &found, &id_by_sym, &parsed);
            (found, rules, false, spans)
        })
        .collect();
    let too_large: Vec<String> = files.iter().zip(&scanned).filter(|(_, s)| s.2).map(|(f, _)| f.clone()).collect();
    let readable: Vec<bool> = files.iter().map(|f| crate::resolve::reads(root, f)).collect();
    let mut rules: HashMap<i64, Rule> = HashMap::new();
    let mut spans: HashMap<i64, Option<(usize, usize)>> = HashMap::new();
    let mut at: Vec<Vec<(u32, i64, usize)>> = vec![Vec::new(); name_ids.len() + writer_ids.len()];
    for (fi, (found, file_rules, _, file_spans)) in scanned.into_iter().enumerate() {
        rules.extend(file_rules);
        spans.extend(file_spans);
        for (n, line, byte) in found {
            at[n as usize].push((fi as u32, line, byte));
        }
    }
    let mut out = Vec::new();
    for (s, id) in wanted.into_iter().zip(id_of) {
        let rule = rules.remove(&s.id).unwrap_or(Rule::Plain);
        if matches!(rule, Rule::Entry) {
            continue;
        }
        // another definition of the name starts on the line; its own start is the span's to decide
        let twin = |f: u32, line: i64| {
            starts[s.qualname.as_str()].iter().any(|&(p, at)| files[f as usize] == p && at == line && (p, at) != (s.path.as_str(), s.start))
        };
        // a use is the definition's own inside its own text: on a line within it, or on its first
        // or last line where its span holds the use (another definition may share the line); with
        // no span for it, such a line is neither
        let mut own_line = 0;
        let mut found: Vec<(u32, i64)> = Vec::new();
        for &(f, line, byte) in &at[id as usize] {
            if twin(f, line) {
                continue;
            }
            if files[f as usize] == s.path && s.start <= line && line <= s.end {
                match spans.get(&s.id) {
                    Some(Some((a, b))) if (line == s.start || line == s.end) && !(*a <= byte && byte < *b) => {}
                    Some(None) if line == s.start || line == s.end => {
                        own_line += 1;
                        continue;
                    }
                    _ => continue,
                }
            }
            found.push((f, line));
        }
        found.dedup();
        let unread = found.iter().filter(|o| !readable[o.0 as usize]).count();
        let mine = |o: &&(u32, i64)| files[o.0 as usize] == s.path;
        let read = |o: &&(u32, i64)| readable[o.0 as usize];
        let lines = found
            .iter()
            .filter(mine)
            .filter(read)
            .chain(found.iter().filter(|o| !mine(o)).filter(read))
            .take(ASKED)
            .map(|&(f, l)| (files[f as usize].clone(), l))
            .collect();
        let base = match rule {
            Rule::Base(why) => Some(why),
            _ => None,
        };
        out.push(Candidate { sym: s.clone(), named: found.len(), unread, lines, base, own_line });
    }
    Ok((out, too_large))
}

/// The text a use of the definition `name` is written with: `Tidy` for Pascal's `TFoo.Tidy`
/// (called `o.Tidy`), `label` for Ruby's writer `label=` (assigned `v.label = x`); and whether it
/// is a writer, used only where it is called (`.label =`, `:label=`), not where a variable of the
/// same name (`@label`) is read.
fn looked_for(name: &str) -> (&str, bool) {
    if let Some(bare) = name.strip_suffix('=').filter(|n| !n.is_empty() && n.chars().all(word_char)) {
        return (bare, true);
    }
    match name.rsplit_once(['.', ':']) {
        Some((_, last)) if !last.is_empty() && last.chars().all(word_char) => (last, false),
        _ => (name, false),
    }
}

/// A character of a name.
fn word_char(c: char) -> bool {
    crate::ident::is_continue(c)
}

/// Ruby methods the interpreter or the core library calls without the code naming them.
const RUBY_HOOKS: [&str; 30] = [
    // a call of a method the object lacks, and `respond_to?` about one
    "method_missing",
    "respond_to_missing?",
    // arithmetic with a built-in number on the left: `1 + money`
    "coerce",
    // implicit conversions: interpolation and `puts`, `String#+`, splats and `Array()`, `&obj`,
    // `**obj` and `Hash()`, `Integer()` and array indexes
    "to_s",
    "to_str",
    "to_ary",
    "to_a",
    "to_proc",
    "to_hash",
    "to_h",
    "to_int",
    "to_i",
    // `p` and the console
    "inspect",
    // a Hash key and `uniq`
    "hash",
    "eql?",
    // Enumerable's methods all call it
    "each",
    // `dup` and `clone`
    "initialize_copy",
    "initialize_dup",
    "initialize_clone",
    // the class and module hooks: a subclass, `include`, `extend`, `prepend`, a `def`, a constant
    // not found
    "inherited",
    "included",
    "extended",
    "prepended",
    "method_added",
    "singleton_method_added",
    "const_missing",
    // Marshal
    "marshal_dump",
    "marshal_load",
    "_dump",
    "_load",
];

/// The (name, 1-based line) of every whole-word occurrence in `src` of a name of `names`.
#[cfg(test)]
fn occurrences(
    src: &str,
    lang: Option<&str>,
    names: &HashMap<&str, u32>,
    writers: &HashMap<&str, u32>,
    folded: &HashMap<String, Vec<u32>>,
    odd: &[&str],
    odd_finder: &aho_corasick::AhoCorasick,
) -> Vec<(u32, i64)> {
    occurrences_at(src, lang, names, writers, folded, odd, odd_finder).into_iter().map(|(n, line, _)| (n, line)).collect()
}

/// `occurrences`, each with the byte its word starts at.
fn occurrences_at(
    src: &str,
    lang: Option<&str>,
    names: &HashMap<&str, u32>,
    writers: &HashMap<&str, u32>,
    folded: &HashMap<String, Vec<u32>>,
    odd: &[&str],
    odd_finder: &aho_corasick::AhoCorasick,
) -> Vec<(u32, i64, usize)> {
    use unicode_normalization::UnicodeNormalization;
    let mut out = Vec::new();
    let (mut line, mut start) = (1i64, None::<usize>);
    let flush = |from: usize, to: usize, line: i64, out: &mut Vec<(u32, i64, usize)>| {
        let w = &src[from..to];
        let hit = if w.is_ascii() { names.get(w).copied() } else { names.get(w.nfc().collect::<String>().as_str()).copied() };
        if let Some(n) = hit {
            out.push((n, line, from));
        }
        if let Some(&n) = writers.get(w).filter(|_| src[..from].ends_with(['.', ':'])) {
            out.push((n, line, from));
        }
        if !folded.is_empty() && w.is_ascii() {
            out.extend(folded.get(&w.to_ascii_lowercase()).into_iter().flatten().filter(|&&n| hit != Some(n)).map(|&n| (n, line, from)));
        }
    };
    // a word is the name the file's language writes (`ident::inside_name`): `foo'`, `$init` and
    // `push!` are names of their own, `'helper` and `s"v$version"` name `helper` and `version`
    let joins = |c: char, inside: bool| match lang {
        Some(l) if inside => crate::ident::joins_after(l, c),
        Some(l) => crate::ident::joins_before(l, c),
        None => word_char(c),
    };
    let marks = lang.map_or("", crate::ident::trailing);
    for (i, c) in src.char_indices() {
        if joins(c, start.is_some()) {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            let mark = marks.contains(c) && !src[i + 1..].starts_with(['=', '~']);
            flush(s, if mark { i + 1 } else { i }, line, &mut out);
        }
        if c == '\n' {
            line += 1;
        }
    }
    if let Some(s) = start {
        flush(s, src.len(), line, &mut out);
    }
    if !odd.is_empty() {
        for m in odd_finder.find_overlapping_iter(src) {
            let (a, b) = (m.start(), m.end());
            let inside = match lang {
                Some(l) => crate::ident::inside_name(l, src, a, b),
                None => src[..a].chars().next_back().is_some_and(word_char) || src[b..].chars().next().is_some_and(word_char),
            };
            if inside {
                continue;
            }
            let line = 1 + src.as_bytes()[..m.start()].iter().filter(|&&b| b == b'\n').count() as i64;
            out.push((names[odd[m.pattern().as_usize()]], line, a));
        }
        out.sort_by_key(|o| o.1);
    }
    out
}

/// The byte where `name` first stands as a whole word in `line`.
fn word_at(line: &str, name: &str) -> Option<usize> {
    let ident = |c: Option<char>| c.is_some_and(word_char);
    line.match_indices(name).map(|(i, _)| i).find(|&i| !ident(line[..i].chars().next_back()) && !ident(line[i + name.len()..].chars().next()))
}

fn has_word(text: &str, word: &str) -> bool {
    word_at(text, word).is_some()
}

/// Attributes and decorators that change nothing about who calls a definition.
const INERT: &[&str] = &[
    // Rust
    "doc",
    "allow",
    "warn",
    "deny",
    "forbid",
    "expect",
    "inline",
    "must_use",
    "cold",
    "track_caller",
    "deprecated",
    "derive",
    "cfg",
    "rustfmt",
    "clippy",
    "non_exhaustive",
    "repr",
    // Python
    "property",
    "staticmethod",
    "classmethod",
    "cached_property",
    "abstractmethod",
    "override",
    "overload",
    "lru_cache",
    "cache",
    "setter",
    "getter",
    "deleter",
    "dataclass",
    "total_ordering",
    "final",
    "contextmanager",
    "asynccontextmanager",
    "wraps",
    // Java and Kotlin
    "Deprecated",
    "SuppressWarnings",
    "SafeVarargs",
    "Nullable",
    "NonNull",
    "NotNull",
    "CheckForNull",
    "CanIgnoreReturnValue",
    "CheckReturnValue",
    "VisibleForTesting",
    "GwtIncompatible",
    "GwtCompatible",
    "J2ktIncompatible",
    "J2ObjCIncompatible",
    "ParametricNullness",
    "Beta",
    "FunctionalInterface",
    "Contract",
    "Immutable",
    "ThreadSafe",
    "GuardedBy",
    "LazyInit",
    "Weak",
    "RetainedWith",
    "IgnoreJRERequirement",
    "ElementTypesAreNonnullByDefault",
];

/// Facts of one file the rules of its definitions read.
struct Facts<'a> {
    lang: &'a str,
    lines: Vec<&'a str>,
    /// Rust: the file belongs to a library crate
    rust_lib: bool,
    /// Go: the package the file declares
    go_package: Option<&'a str>,
    /// JavaScript and TypeScript: the lines that export something
    exports: Vec<&'a str>,
    /// Python: the file lists names in `__all__`
    py_all: bool,
    /// TypeScript: the file adds to a module or the global scope, whose interfaces merge with its own
    augments: bool,
    src: &'a str,
    /// the spans of the definitions that share their first line with another (None: no grammar gave one)
    own: HashMap<i64, Option<(usize, usize)>>,
}

fn file_rules<'a>(root: &Path, rel: &str, src: &str, mine: &[&Symbol], all: &[&Symbol], parsed: &dyn Fn() -> &'a [crate::parse::Chunk]) -> Vec<(i64, Rule)> {
    let lang = crate::langs::detect(rel, Some(src)).map_or("", |s| s.id);
    // a definition whose first line another one starts or ends on: its modifiers are read from its
    // own span, as a neighbour's would be read as its own from the line
    let mut edges: HashMap<i64, usize> = HashMap::new();
    for s in all {
        *edges.entry(s.start).or_default() += 1;
        if s.end != s.start {
            *edges.entry(s.end).or_default() += 1;
        }
    }
    let own: HashMap<i64, Option<(usize, usize)>> =
        all.iter().filter(|d| edges.get(&d.start).copied().unwrap_or(0) > 1).map(|d| (d.id, span_of(parsed(), d))).collect();
    let lines: Vec<&str> = src.lines().collect();
    let facts = Facts {
        lang,
        rust_lib: lang == "rust" && rust_library(root, rel),
        go_package: (lang == "go").then(|| lines.iter().find_map(|l| l.trim().strip_prefix("package ")).map(|p| p.trim())).flatten(),
        exports: if matches!(lang, "javascript" | "typescript" | "tsx") {
            lines.iter().copied().filter(|l| has_word(l, "export") || has_word(l, "exports")).collect()
        } else {
            Vec::new()
        },
        py_all: lang == "python" && src.contains("__all__"),
        augments: matches!(lang, "typescript" | "tsx") && (src.contains("declare module") || src.contains("declare global")),
        lines,
        src,
        own,
    };
    mine.iter()
        .map(|d| {
            // the innermost holder: a class, a type or an impl around it; by their spans where they
            // share a line (a one-line class), else by their lines
            // (methods and functions alone: a C `typedef struct s {..} s;` is no class and member)
            let holds = |p: &Symbol| match (facts.own.get(&p.id), facts.own.get(&d.id)) {
                (Some(Some((pa, pb))), Some(Some((da, db)))) if matches!(d.kind.as_str(), "method" | "function") => {
                    pa <= da && db <= pb && (pa, pb) != (da, db)
                }
                _ => p.start <= d.start && d.end <= p.end && (p.start, p.end) != (d.start, d.end),
            };
            let parent = all.iter().filter(|p| p.id != d.id && holds(p)).min_by_key(|p| p.end - p.start);
            (d.id, rule(rel, &facts, d, parent.copied()))
        })
        .collect()
}

/// The byte spans of the definitions in `mine` that a use on their first or last line may lie
/// outside of: their name more than once on their first line, or on a last line of their own.
/// Such a file is parsed again for its definitions' spans (`parse::Chunk::bytes`); one that needs
/// none is not.
fn edge_spans<'a>(
    mine: &[&Symbol],
    found: &[(u32, i64, usize)],
    id_by_sym: &HashMap<i64, u32>,
    parsed: &dyn Fn() -> &'a [crate::parse::Chunk],
) -> Vec<(i64, Option<(usize, usize)>)> {
    let edge = |s: &Symbol| {
        let Some(&id) = id_by_sym.get(&s.id) else { return false };
        let on = |line: i64| found.iter().filter(|&&(n, l, _)| n == id && l == line).count();
        on(s.start) > 1 || s.end != s.start && on(s.end) > 0
    };
    let wanted: Vec<&Symbol> = mine.iter().copied().filter(|s| edge(s)).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    wanted.into_iter().map(|s| (s.id, span_of(parsed(), s))).collect()
}

/// The span the grammar gave the chunk of `s`.
fn span_of(chunks: &[crate::parse::Chunk], s: &Symbol) -> Option<(usize, usize)> {
    chunks.iter().find(|c| c.qualname == s.qualname && c.kind == s.kind && (c.start, c.end) == (s.start, s.end)).and_then(|c| c.bytes())
}

/// Whether `rel` is a source file of a Rust library crate: under the `src/` of the nearest
/// directory with a `Cargo.toml` that has a `src/lib.rs` or a `[lib]` table, not a binary.
fn rust_library(root: &Path, rel: &str) -> bool {
    let mut dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
    loop {
        let base = if dir.is_empty() { root.to_path_buf() } else { root.join(dir) };
        if let Ok(toml) = std::fs::read_to_string(base.join("Cargo.toml")) {
            let below = if dir.is_empty() { rel } else { &rel[dir.len() + 1..] };
            let lib = base.join("src/lib.rs").is_file() || toml.lines().any(|l| l.trim() == "[lib]");
            return lib && below.starts_with("src/") && !below.starts_with("src/bin/") && below != "src/main.rs";
        }
        match dir.rsplit_once('/') {
            Some((up, _)) => dir = up,
            None if !dir.is_empty() => dir = "",
            None => return false,
        }
    }
}

/// The line of a definition that names it, from its first line on and past its attributes,
/// decorators and comments, as (0-based index, text).
fn named_line<'a>(f: &Facts<'a>, d: &Symbol) -> (usize, &'a str) {
    let first = (d.start.max(1) - 1) as usize;
    let last = (d.end.max(d.start) as usize).min(first + 40).min(f.lines.len());
    let aside =
        |l: &str| ["@", "#", "//", "/*", "*", "--"].iter().any(|m| l.trim_start().starts_with(m)) || (f.lang == "csharp" && l.trim_start().starts_with('['));
    (first..last)
        .find(|&i| !aside(f.lines[i]) && word_at(f.lines[i], &d.name).is_some())
        .map_or((first, f.lines.get(first).copied().unwrap_or("")), |i| (i, f.lines[i]))
}

/// `line` (one of `f.lines`) with the text of every definition that shares a line blanked out.
fn outside_definitions(f: &Facts, line: &str) -> String {
    let at = line.as_ptr() as usize - f.src.as_ptr() as usize;
    let mut out = line.as_bytes().to_vec();
    for &(a, b) in f.own.values().flatten() {
        let (from, to) = (a.max(at), b.min(at + line.len()));
        if from < to {
            out[from - at..to - at].fill(b' ');
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The text before `d`'s name that holds its own modifiers and attributes: on the line that names
/// it, or from its own span where another definition shares its first line. None: no span says
/// which part of that line is its own.
fn before<'a>(f: &Facts<'a>, d: &Symbol) -> Option<&'a str> {
    match f.own.get(&d.id) {
        Some(Some((a, b))) => {
            // what is written before its span on its line belongs to it too (`export` before a
            // function node), back to the end of a neighbour or a holder's `{`
            let line = f.src[..*a].rfind('\n').map_or(0, |i| i + 1);
            let from = f.src[line..*a].rfind(['{', '}', ';']).map_or(line, |i| line + i + 1);
            let text = &f.src[from..*b];
            Some(&text[..word_at(text, &d.name).unwrap_or(0)])
        }
        Some(None) => None,
        None => {
            let decl = named_line(f, d).1;
            Some(&decl[..word_at(decl, &d.name).unwrap_or(0)])
        }
    }
}

/// Whether an attribute or decorator of `d` (above the line that names it, or on that line before
/// the name) may register it: any but the inert ones.
fn registered(f: &Facts, d: &Symbol) -> bool {
    let at = named_line(f, d).0;
    let before = before(f, d).unwrap_or("");
    let first = (d.start.max(1) - 1) as usize;
    let above = f.lines.get(first..at).unwrap_or(&[]);
    above.iter().map(|l| l.trim()).chain(before.split_whitespace()).any(|a| {
        let Some(body) = a.strip_prefix("#[").or_else(|| a.strip_prefix('@')).or_else(|| a.strip_prefix('[').filter(|_| f.lang == "csharp")) else {
            return false;
        };
        let head = body.split(['(', ']', ' ']).next().unwrap_or("");
        let last = head.rsplit(['.', ':']).next().unwrap_or(head);
        !(last.is_empty() || (last == "cfg" && !body.starts_with("cfg(test")) || INERT.contains(&last))
    })
}

fn rule(path: &str, f: &Facts, d: &Symbol, parent: Option<&Symbol>) -> Rule {
    let name = d.name.as_str();
    let main = if crate::langs::ignores_case(Some(f.lang), &d.kind) { name.eq_ignore_ascii_case("main") } else { name == "main" };
    if crate::text::is_test_path(path) || main {
        return Rule::Entry;
    }
    let shared = |s: &Symbol| f.own.get(&s.id).is_some_and(Option::is_none);
    if shared(d) || parent.is_some_and(shared) {
        return Rule::Base("another definition shares its line, and no grammar span says which of the line's modifiers are its own".into());
    }
    let in_type = parent.filter(|p| p.kind != "function" && p.kind != "method");
    // an attribute on the type or impl around it registers its methods too (`#[pymethods]`)
    if registered(f, d) || in_type.is_some_and(|p| registered(f, p)) {
        return Rule::Entry;
    }
    let before = before(f, d).unwrap_or("");
    // its own text where it shares its line, else the line that names it
    let decl = match f.own.get(&d.id) {
        Some(Some((a, b))) => &f.src[*a..*b],
        _ => named_line(f, d).1,
    };
    let words: Vec<&str> = before.split(|c: char| !word_char(c)).filter(|w| !w.is_empty()).collect();
    let word = |w: &str| words.contains(&w);
    if word("override") || matches!(name, "constructor" | "initialize") || parent.is_some_and(|p| p.name == name) {
        return Rule::Entry;
    }
    let header = |p: &Symbol| match f.own.get(&p.id) {
        Some(Some((a, b))) => &f.src[*a..*b],
        _ => named_line(f, p).1,
    };
    match f.lang {
        "python" => {
            let quoted = || f.src.contains(&format!("'{name}'")) || f.src.contains(&format!("\"{name}\""));
            // `__x__` and hooks such as `__lldb_init_module` are called by name by their host
            if name.starts_with("__") || name.starts_with("test") || (f.py_all && quoted()) {
                return Rule::Entry;
            }
        }
        "rust" => {
            let trait_impl = in_type.is_some_and(|p| match f.own.get(&p.id) {
                // its own header, up to its body
                Some(Some((a, b))) => {
                    let text = &f.src[*a..*b];
                    text[..text.find('{').unwrap_or(text.len())].contains(" for ")
                }
                _ => {
                    let first = (p.start.max(1) - 1) as usize;
                    f.lines[first..f.lines.len().min(p.end as usize)].iter().find(|l| has_word(l, "impl")).is_some_and(|l| l.contains(" for "))
                }
            });
            let public = |b: &str| b.split_whitespace().any(|w| w == "pub");
            let pub_trait = in_type.is_some_and(|p| p.kind == "trait" && public(header(p)));
            // `write!` calls a `write_fmt` method of its first argument without naming it
            if word("extern") || trait_impl || (f.rust_lib && (public(before) || pub_trait)) || name == "write_fmt" {
                return Rule::Entry;
            }
        }
        "go" => {
            let exported = name.chars().next().is_some_and(char::is_uppercase);
            if name == "init" || (exported && (f.go_package != Some("main") || d.qualname.contains('.'))) {
                return Rule::Entry;
            }
        }
        "javascript" | "typescript" | "tsx" => {
            let exported_class = in_type.is_some_and(|p| has_word(header(p), "export"));
            let merged = f.augments && matches!(d.kind.as_str(), "interface" | "type" | "class");
            // where it shares its line, an export list there is what the line holds outside the
            // definitions' own text: `export function api` exports `api`, not its neighbour
            let listed = match f.own.get(&d.id) {
                Some(Some(_)) => {
                    let rest = outside_definitions(f, named_line(f, d).1);
                    (has_word(&rest, "export") || has_word(&rest, "exports")) && has_word(&rest, name)
                }
                _ => f.exports.iter().any(|l| has_word(l, name)),
            };
            if path.ends_with(".d.ts") || merged || word("export") || exported_class || listed {
                return Rule::Entry;
            }
        }
        "java" | "csharp" => {
            let serial = f.lang == "java" && matches!(name, "readObject" | "writeObject" | "readObjectNoData" | "readResolve" | "writeReplace");
            if word("public") || word("protected") || word("internal") || in_type.is_some_and(|p| p.kind == "interface") || serial {
                return Rule::Entry;
            }
        }
        // public unless marked otherwise
        "kotlin" | "scala" | "groovy" | "php" if !(word("private") || word("internal") || (f.lang == "php" && in_type.is_none())) => return Rule::Entry,
        "php" if name.starts_with("__") => return Rule::Entry,
        // Ruby calls these itself, and an operator method by its operator (`a <=> b`)
        "ruby" if !name.chars().any(word_char) || RUBY_HOOKS.contains(&name) => return Rule::Entry,
        "swift" if word("public") || word("open") => return Rule::Entry,
        "dart" if !name.starts_with('_') => return Rule::Entry,
        // the engine calls `_ready`, `_process` and the other virtual functions
        "gdscript" if name.starts_with('_') => return Rule::Entry,
        // a target is run by its name from outside: `make check`
        "make" => return Rule::Entry,
        _ => {}
    }
    if [".vue", ".svelte", ".astro"].iter().any(|e| path.ends_with(e)) {
        return Rule::Base("nothing else names it, but the template of its component may use it".to_string());
    }
    let class = in_type.is_some_and(|p| matches!(p.kind.as_str(), "class" | "struct" | "interface" | "trait" | "impl" | "enum"));
    if matches!(f.lang, "javascript" | "typescript" | "tsx") && d.kind == "method" && !class {
        return Rule::Base("nothing else names it, but it is a property of an object, which code may call by its key".to_string());
    }
    if let Some(p) = in_type.filter(|p| extends(f.lang, header(p), &p.name)) {
        return Rule::Base(format!("nothing else names it, but its type {} extends another, whose code may call it", p.name));
    }
    if in_type.is_none() && matches!(d.kind.as_str(), "class" | "struct") && extends(f.lang, decl, name) {
        return Rule::Base("nothing else names it, but it extends another type, which may find it without its name".to_string());
    }
    Rule::Plain
}

/// Whether the line that names type `name` says it extends or implements another.
fn extends(lang: &str, header: &str, name: &str) -> bool {
    let Some(at) = word_at(header, name) else { return false };
    let after = header[at + name.len()..].trim().trim_end_matches(|c: char| c == '{' || c == ':' || c.is_whitespace());
    match lang {
        "python" => after.strip_prefix('(').and_then(|a| a.strip_suffix(')')).is_some_and(|bases| !matches!(bases.trim(), "" | "object")),
        "ruby" => after.starts_with('<'),
        _ => after.starts_with(':') || ["extends", "implements", "with", "inherits"].iter().any(|w| has_word(after, w)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no span for a definition that shares its line, rule() cannot say which of the line's
    /// modifiers are its own: the definition is undecided when nothing names it, never dead and
    /// never an entry point, and so is one whose holder is in that state.
    #[test]
    fn a_definition_sharing_its_line_with_no_span_is_undecided() {
        let sym =
            |id: i64, kind: &str, name: &str| Symbol { id, path: "a.kt".into(), kind: kind.into(), name: name.into(), qualname: name.into(), start: 1, end: 1 };
        let (k, x) = (sym(1, "class", "K"), sym(2, "method", "x"));
        let src = "class K { private fun x() = 1; fun api() = x() }";
        let facts = |own: HashMap<i64, Option<(usize, usize)>>| Facts {
            lang: "kotlin",
            lines: vec![src],
            rust_lib: false,
            go_package: None,
            exports: Vec::new(),
            py_all: false,
            augments: false,
            src,
            own,
        };
        let undecided = |r: Rule| matches!(r, Rule::Base(why) if why.contains("no grammar span"));
        assert!(undecided(rule("a.kt", &facts([(2, None)].into_iter().collect()), &x, Some(&k))));
        assert!(undecided(rule("a.kt", &facts([(1, None), (2, Some((10, 29)))].into_iter().collect()), &x, Some(&k))));
        // with its span, its own `private` decides: no entry point
        assert!(matches!(rule("a.kt", &facts([(1, Some((0, src.len()))), (2, Some((10, 29)))].into_iter().collect()), &x, Some(&k)), Rule::Plain));
    }

    #[test]
    fn a_name_counts_as_a_whole_word_in_any_spelling_of_its_letters() {
        let names: HashMap<&str, u32> = [("run", 0), ("café", 1)].into_iter().collect();
        let odd = ["valid?"];
        let mut all = names.clone();
        all.insert("valid?", 2);
        let finder = aho_corasick::AhoCorasick::new(odd).unwrap();
        let src = "run()\nrerun run_it $run\ncafe\u{301} x.valid? valid?x\n";
        assert_eq!(occurrences(src, None, &all, &HashMap::new(), &HashMap::new(), &odd, &finder), [(0, 1), (0, 2), (1, 3), (2, 3)]);
    }

    #[test]
    fn a_header_that_names_a_base_type_extends_it() {
        assert!(extends("python", "class Handler(BaseHandler):", "Handler"));
        assert!(!extends("python", "class Plain:", "Plain") && !extends("python", "class Plain(object):", "Plain"));
        assert!(extends("typescript", "export class A extends B {", "A") && extends("csharp", "class A : B", "A") && extends("ruby", "class A < B", "A"));
        assert!(!extends("java", "class A {", "A"));
    }
}
