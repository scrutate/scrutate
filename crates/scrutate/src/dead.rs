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
}

/// What the pass over one file found: (name, line) of each occurrence, the rule of each wanted
/// definition in it, and whether it was too large to read.
type Scanned = (Vec<(u32, i64)>, Vec<(i64, Rule)>, bool);

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
        if !crate::langs::ignores_case(lang, &s.kind) {
            continue;
        }
        let ids = folded.entry(looked_for(&s.name).0.to_ascii_lowercase()).or_default();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let odd: Vec<&str> = name_ids.keys().copied().filter(|n| !n.chars().all(word_char)).collect();
    let odd_finder = aho_corasick::AhoCorasick::new(&odd)?;
    let files = every_file(root);
    let scanned: Vec<Scanned> = files
        .par_iter()
        .map(|rel| {
            let Some(raw) = crate::files::read_regular(&root.join(rel), MAX_READ) else {
                let big = std::fs::metadata(root.join(rel)).is_ok_and(|m| m.len() > MAX_READ);
                return (Vec::new(), Vec::new(), big);
            };
            let Some(src) = crate::index::decode_source(&raw) else { return (Vec::new(), Vec::new(), false) };
            let found = occurrences(&src, &name_ids, &writer_ids, &folded, &odd, &odd_finder);
            let mine: Vec<&Symbol> = by_file
                .get(rel.as_str())
                .map(|v| v.iter().copied().filter(|s| KINDS.contains(&s.kind.as_str()) && inside(&s.path)).collect())
                .unwrap_or_default();
            let rules = if mine.is_empty() { Vec::new() } else { file_rules(root, rel, &src, &mine, &by_file[rel.as_str()]) };
            (found, rules, false)
        })
        .collect();
    let too_large: Vec<String> = files.iter().zip(&scanned).filter(|(_, s)| s.2).map(|(f, _)| f.clone()).collect();
    let readable: Vec<bool> = files.iter().map(|f| crate::resolve::reads(root, f)).collect();
    let mut rules: HashMap<i64, Rule> = HashMap::new();
    let mut at: Vec<Vec<(u32, i64)>> = vec![Vec::new(); name_ids.len() + writer_ids.len()];
    for (fi, (found, file_rules, _)) in scanned.into_iter().enumerate() {
        rules.extend(file_rules);
        for (n, line) in found {
            at[n as usize].push((fi as u32, line));
        }
    }
    let mut out = Vec::new();
    for (s, id) in wanted.into_iter().zip(id_of) {
        let rule = rules.remove(&s.id).unwrap_or(Rule::Plain);
        if matches!(rule, Rule::Entry) {
            continue;
        }
        let twin = |f: u32, line: i64| starts[s.qualname.as_str()].iter().any(|&(p, at)| files[f as usize] == p && at == line);
        let mut found: Vec<(u32, i64)> =
            at[id as usize].iter().copied().filter(|&(f, line)| !(files[f as usize] == s.path && s.start <= line && line <= s.end) && !twin(f, line)).collect();
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
        out.push(Candidate { sym: s.clone(), named: found.len(), unread, lines, base });
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

/// A character of a word as a whole-word search reads it: a combining mark included.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || unicode_normalization::char::is_combining_mark(c)
}

/// Every regular file of the repository the code index's walk reaches, binary or too large ones
/// included (their reading decides), from the root, sorted.
fn every_file(root: &Path) -> Vec<String> {
    let walk = crate::files::CodeWalk::new(root);
    let mut out: Vec<String> = walk
        .builder(root)
        .build()
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()) && e.file_name() != ".git")
        .filter_map(|e| Some(e.path().strip_prefix(root).ok()?.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")))
        .collect();
    out.sort();
    out
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
fn occurrences(
    src: &str,
    names: &HashMap<&str, u32>,
    writers: &HashMap<&str, u32>,
    folded: &HashMap<String, Vec<u32>>,
    odd: &[&str],
    odd_finder: &aho_corasick::AhoCorasick,
) -> Vec<(u32, i64)> {
    use unicode_normalization::UnicodeNormalization;
    let mut out = Vec::new();
    let (mut line, mut start) = (1i64, None::<usize>);
    let flush = |from: usize, to: usize, line: i64, out: &mut Vec<(u32, i64)>| {
        let w = &src[from..to];
        let hit = if w.is_ascii() { names.get(w).copied() } else { names.get(w.nfc().collect::<String>().as_str()).copied() };
        if let Some(n) = hit {
            out.push((n, line));
        }
        if let Some(&n) = writers.get(w).filter(|_| src[..from].ends_with(['.', ':'])) {
            out.push((n, line));
        }
        if !folded.is_empty() && w.is_ascii() {
            out.extend(folded.get(&w.to_ascii_lowercase()).into_iter().flatten().filter(|&&n| hit != Some(n)).map(|&n| (n, line)));
        }
    };
    for (i, c) in src.char_indices() {
        if word_char(c) {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take() {
            flush(s, i, line, &mut out);
        }
        if c == '\n' {
            line += 1;
        }
    }
    if let Some(s) = start {
        flush(s, src.len(), line, &mut out);
    }
    if !odd.is_empty() {
        let ident = |c: Option<char>| c.is_some_and(word_char);
        for m in odd_finder.find_overlapping_iter(src) {
            if ident(src[..m.start()].chars().next_back()) || ident(src[m.end()..].chars().next()) {
                continue;
            }
            let line = 1 + src.as_bytes()[..m.start()].iter().filter(|&&b| b == b'\n').count() as i64;
            out.push((names[odd[m.pattern().as_usize()]], line));
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
}

fn file_rules(root: &Path, rel: &str, src: &str, mine: &[&Symbol], all: &[&Symbol]) -> Vec<(i64, Rule)> {
    let lang = crate::langs::detect(rel, Some(src)).map_or("", |s| s.id);
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
    };
    mine.iter()
        .map(|d| {
            // the innermost holder: a class, a type or an impl around it
            let parent = all
                .iter()
                .filter(|p| p.id != d.id && p.start <= d.start && d.end <= p.end && (p.start, p.end) != (d.start, d.end))
                .min_by_key(|p| p.end - p.start);
            (d.id, rule(rel, &facts, d, parent.copied()))
        })
        .collect()
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

/// Whether an attribute or decorator of `d` (above the line that names it, or on that line before
/// the name) may register it: any but the inert ones.
fn registered(f: &Facts, d: &Symbol) -> bool {
    let (at, decl) = named_line(f, d);
    let before = &decl[..word_at(decl, &d.name).unwrap_or(0)];
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
    let in_type = parent.filter(|p| p.kind != "function" && p.kind != "method");
    // an attribute on the type or impl around it registers its methods too (`#[pymethods]`)
    if registered(f, d) || in_type.is_some_and(|p| registered(f, p)) {
        return Rule::Entry;
    }
    let decl = named_line(f, d).1;
    let before = &decl[..word_at(decl, name).unwrap_or(0)];
    let words: Vec<&str> = before.split(|c: char| !word_char(c)).filter(|w| !w.is_empty()).collect();
    let word = |w: &str| words.contains(&w);
    if word("override") || matches!(name, "constructor" | "initialize") || parent.is_some_and(|p| p.name == name) {
        return Rule::Entry;
    }
    let header = |p: &Symbol| named_line(f, p).1;
    match f.lang {
        "python" => {
            let quoted = || f.src.contains(&format!("'{name}'")) || f.src.contains(&format!("\"{name}\""));
            // `__x__` and hooks such as `__lldb_init_module` are called by name by their host
            if name.starts_with("__") || name.starts_with("test") || (f.py_all && quoted()) {
                return Rule::Entry;
            }
        }
        "rust" => {
            let trait_impl = in_type.is_some_and(|p| {
                let first = (p.start.max(1) - 1) as usize;
                f.lines[first..f.lines.len().min(p.end as usize)].iter().find(|l| has_word(l, "impl")).is_some_and(|l| l.contains(" for "))
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
            if path.ends_with(".d.ts") || merged || word("export") || exported_class || f.exports.iter().any(|l| has_word(l, name)) {
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

    #[test]
    fn a_name_counts_as_a_whole_word_in_any_spelling_of_its_letters() {
        let names: HashMap<&str, u32> = [("run", 0), ("café", 1)].into_iter().collect();
        let odd = ["valid?"];
        let mut all = names.clone();
        all.insert("valid?", 2);
        let finder = aho_corasick::AhoCorasick::new(odd).unwrap();
        let src = "run()\nrerun run_it $run\ncafe\u{301} x.valid? valid?x\n";
        assert_eq!(occurrences(src, &all, &HashMap::new(), &HashMap::new(), &odd, &finder), [(0, 1), (0, 2), (1, 3), (2, 3)]);
    }

    #[test]
    fn a_header_that_names_a_base_type_extends_it() {
        assert!(extends("python", "class Handler(BaseHandler):", "Handler"));
        assert!(!extends("python", "class Plain:", "Plain") && !extends("python", "class Plain(object):", "Plain"));
        assert!(extends("typescript", "export class A extends B {", "A") && extends("csharp", "class A : B", "A") && extends("ruby", "class A < B", "A"));
        assert!(!extends("java", "class A {", "A"));
    }
}
