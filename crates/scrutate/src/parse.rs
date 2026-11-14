//! Source parsing: split files into symbol chunks with separate fields.
//!
//! Three tiers, so every text file is searchable and most code gets real symbols:
//! 1. tree-sitter grammars (langs.rs): precise definitions, signatures, docs, bodies, call edges;
//! 2. heuristic symbols for languages without a grammar: definition lines introduced by the usual
//!    keywords (`def`, `fn`, `function`, `class`, `sub`, ...), or matched by the patterns of a
//!    custom language (custom.rs), with the block below them;
//! 3. overlapping line windows for everything else.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

use crate::langs::{self, head_word, ImportHow, LangSpec, NameRule};
use crate::text::split_lines;

#[derive(Debug, Clone, Default)]
pub struct Chunk {
    pub name: String,
    pub qualname: String,
    pub kind: &'static str,
    pub start: i64, // 1-based inclusive
    pub end: i64,
    pub text: String, // full text for display (containers truncated)
    pub sig: String,
    pub doc: String,
    pub body: String,
    pub calls: BTreeSet<String>,
    /// where `text` and `body` are cut from the source when they are first needed
    pub(crate) lazy: Option<Lazy>,
    /// the lines of the documentation above the definition (1-based, inclusive), not indexed
    pub(crate) doc_lines: Option<(i64, i64)>,
}

/// Source ranges of a definition's text and body (start bytes, their shared end, the line cap).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Lazy {
    text: usize,
    body: usize,
    end: usize,
    lines: usize,
}

impl Chunk {
    /// Cuts `text` and `body` from `src`, the file's chunks in their order and from one `budget`.
    /// The index does it one chunk at a time, so a file of thousands of nested definitions never
    /// holds all their copies at once.
    pub fn fill(&mut self, src: &[u8], budget: &mut TextBudget) {
        if let Some(l) = self.lazy.take() {
            // windows do not nest, so their text grows with the file alone
            if self.kind == "window" {
                // its lines as `split_lines` gives them (no "\r"); past twice the cap is cut anyway
                let s = txt(src, l.text, l.end.min(l.text + 2 * MAX_CHUNK_BYTES + 8));
                self.text = capped(split_lines(&s).join("\n"));
                self.body = self.text.clone();
                return;
            }
            let (lines, max) = if budget.0 == 0 { (1, CUT_CHUNK_BYTES) } else { (l.lines, MAX_CHUNK_BYTES) };
            self.text = head_bytes(src, l.text, l.end, lines, max);
            self.body = head_bytes(src, l.body, l.end, lines, max);
            budget.0 = budget.0.saturating_sub(self.text.len() + self.body.len());
        }
    }
}

/// What the definitions of one file keep of its text, text and body together: `FILE_TEXT_FLOOR`
/// bytes plus `FILE_TEXT_PER_BYTE` for each byte of the file. A definition filled once it is
/// spent keeps its first line, at most `CUT_CHUNK_BYTES` of it, then "\n...": the text of a file
/// of deeply nested definitions grows with the file, not with the nesting.
pub struct TextBudget(usize);

impl TextBudget {
    pub fn of(source: &[u8]) -> TextBudget {
        TextBudget(FILE_TEXT_FLOOR.saturating_add(FILE_TEXT_PER_BYTE.saturating_mul(source.len())))
    }
}

/// Everything extracted from one file.
#[derive(Debug, Default)]
pub struct Parsed {
    pub chunks: Vec<Chunk>,
    /// (1-based line, imported module as written, normalised)
    pub imports: Vec<(i64, String)>,
}

pub const CONTAINERS: &[&str] = &["class", "impl", "interface", "trait", "enum", "struct", "type", "module", "block", "section", "key"];
/// Kinds that are symbols only: no postings, no vector, and after every other chunk of a file.
pub const SYMBOL_ONLY: &[&str] = &["constant", "field"];
const COMMENT_TYPES: &[&str] =
    &["comment", "line_comment", "block_comment", "doc_comment", "documentation_comment", "comment_block", "statement_comment", "module_comment", "haddock"];
/// Nodes that list the statements of a body or file: a comment above the first one sits outside the list.
const BODY_LISTS: &[&str] =
    &["declarations", "statement_list", "non_empty_declarative_part", "binding_set", "body", "body_statement", "document", "block_node", "block_mapping"];
pub const MAX_CONTAINER_LINES: usize = 40;
pub const MAX_CHUNK_LINES: usize = 200;
/// Chunk text and body keep at most this many bytes (cut at a character, then "\n..."): a file
/// on one line with thousands of nested functions would otherwise hold its whole line per chunk.
/// The longest chunk of this crate's sources is 9 KB, of networkx 15 KB.
pub const MAX_CHUNK_BYTES: usize = 32 * 1024;
/// `TextBudget`: all the chunks of a file keep at most 5.1 times its size and 1.8 MB (6,262 files
/// of Rust, Python, C, C++, JavaScript, Markdown and JSON measured), so normal files stay under it.
pub const FILE_TEXT_FLOOR: usize = 1 << 20;
pub const FILE_TEXT_PER_BYTE: usize = 8;
/// What a definition keeps of its first line once its file's budget is spent.
pub const CUT_CHUNK_BYTES: usize = 256;

/// `s[..]` cut to `MAX_CHUNK_BYTES` at a character boundary, and whether it was cut.
fn cap_bytes(s: &str) -> (&str, bool) {
    if s.len() <= MAX_CHUNK_BYTES {
        return (s, false);
    }
    (&s[..s.floor_char_boundary(MAX_CHUNK_BYTES)], true)
}

/// Files that are text but not worth indexing (data, generated, lock files).
/// Never indexed by Engine B: generated/data text that is noise for symbol search, and binary formats
/// that would only cost a read before the NUL-byte check rejects them. Engine A has its own rules.
const EXCLUDED_EXTS: &[&str] = &[
    // data and generated text
    ".csv",
    ".tsv",
    ".log",
    ".min.js",
    ".min.css",
    ".map",
    ".lock",
    ".lockb",
    ".lockfile",
    ".sum",
    ".snap",
    ".pem",
    ".crt",
    ".key",
    ".der",
    ".asc",
    ".sig",
    ".bin",
    ".dat",
    ".parquet",
    ".ipynb_checkpoints",
    // archives and compiled artefacts
    ".gz",
    ".tgz",
    ".zip",
    ".tar",
    ".xz",
    ".bz2",
    ".zst",
    ".7z",
    ".rar",
    ".jar",
    ".war",
    ".whl",
    ".egg",
    ".pyc",
    ".pyo",
    ".so",
    ".dylib",
    ".dll",
    ".exe",
    ".o",
    ".a",
    ".lib",
    ".class",
    ".wasm",
    ".rlib",
    ".pdb",
    // media and documents
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".bmp",
    ".ico",
    ".icns",
    ".webp",
    ".tif",
    ".tiff",
    ".psd",
    ".mp3",
    ".mp4",
    ".wav",
    ".ogg",
    ".flac",
    ".mov",
    ".avi",
    ".mkv",
    ".webm",
    ".pdf",
    ".woff",
    ".woff2",
    ".ttf",
    ".otf",
    ".eot",
    ".sqlite",
    ".db",
];

/// Lock files whose extension is that of hand-written files, by lowercase file name.
const EXCLUDED_NAMES: &[&str] =
    &["package-lock.json", "npm-shrinkwrap.json", "pnpm-lock.yaml", "packages.lock.json", "package.resolved", ".terraform.lock.hcl"];

pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Every text file is indexable unless its extension marks it as data; binary files are detected
/// by content when read.
pub fn is_indexable(path: &str) -> bool {
    let lower = basename(path).to_lowercase();
    !EXCLUDED_EXTS.iter().any(|e| lower.ends_with(e)) && !EXCLUDED_NAMES.contains(&lower.as_str())
}

/// The grammar that parses this file, if any (shared extensions are decided by the text).
pub fn spec_for(path: &str, source: &str) -> Option<&'static LangSpec> {
    langs::detect(path, Some(source))
}

// ------------------------------------------------------------------------- helpers
static PYSTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^[rRuUbBfF]{0,2}("""|'''|"|')"#).unwrap());
static COMMENT_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^\s*(/\*+|\*+/|\*|//+!?|<#|#>|#+|--\s?[|^]|--+|;+|%+|'|!|"|\{-\|?|-\})\s?"#).unwrap());
static IDENT_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_$][\w$]*(?:-[A-Za-z_]\w*)*").unwrap());

fn expandtabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    for c in s.chars() {
        match c {
            '\t' => {
                let n = 8 - (col % 8);
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\n' | '\r' => {
                out.push(c);
                col = 0;
            }
            _ => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// inspect.cleandoc: dedent uniformly, drop leading/trailing blank lines.
pub fn cleandoc(doc: &str) -> String {
    let expanded = expandtabs(doc);
    let mut lines: Vec<String> = expanded.split('\n').map(|s| s.to_string()).collect();
    let mut margin = usize::MAX;
    for line in &lines[1..] {
        let content = line.trim_start_matches(' ').chars().count();
        if content > 0 {
            margin = margin.min(line.chars().count() - content);
        }
    }
    if let Some(first) = lines.first_mut() {
        *first = first.trim_start_matches(' ').to_string();
    }
    if margin < usize::MAX {
        for line in lines.iter_mut().skip(1) {
            *line = line.chars().skip(margin).collect();
        }
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    while lines.first().map(|l| l.is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    lines.join("\n")
}

fn py_string_content(raw: &str) -> String {
    let Some(m) = PYSTR.captures(raw) else {
        return raw.to_string();
    };
    let q = m.get(1).unwrap().as_str();
    let mut inner = &raw[m.get(0).unwrap().end()..];
    if inner.ends_with(q) {
        inner = &inner[..inner.len() - q.len()];
    }
    cleandoc(inner)
}

pub fn clean_comment(raw: &str) -> String {
    let mut out = Vec::new();
    // a PowerShell block closes on the line of its text: `<# Gets x. #>`
    let block = raw.trim_start().starts_with("<#");
    for ln in raw.split('\n') {
        let s = COMMENT_MARK.replace(ln, "");
        let mut s = s.trim_end_matches(['*', '/', ' ']).trim_end();
        if block {
            s = s.strip_suffix("#>").unwrap_or(s).trim_end();
        }
        if !s.trim().is_empty() {
            out.push(s.to_string());
        }
    }
    out.join("\n").trim().to_string()
}

/// The first `n` lines of `text`, at most `MAX_CHUNK_BYTES` of them, "\n..." when cut.
fn head(text: &str, n: usize) -> String {
    let end = text.match_indices('\n').nth(n - 1).map_or(text.len(), |(i, _)| i);
    let (s, cut) = cap_bytes(&text[..end]);
    if cut || end < text.len() {
        format!("{s}\n...")
    } else {
        s.to_string()
    }
}

/// `text` cut to `MAX_CHUNK_BYTES` ("\n..." when cut).
fn capped(text: String) -> String {
    match cap_bytes(&text) {
        (s, true) => format!("{s}\n..."),
        _ => text,
    }
}

pub fn window_chunks(source: &str) -> Vec<Chunk> {
    window_chunks_from(source, 0)
}

/// Overlapping 50-line windows (10 lines overlap) from 0-based line `first_line` on.
pub fn window_chunks_from(source: &str, first_line: usize) -> Vec<Chunk> {
    let lines = split_lines(source);
    let mut out = windows_of(source, &lines, first_line.min(lines.len()), lines.len(), "");
    let mut budget = TextBudget::of(source.as_bytes());
    out.iter_mut().for_each(|c| c.fill(source.as_bytes(), &mut budget));
    out
}

/// Overlapping 50-line windows (10 lines overlap) of the 0-based lines `a..b`, their text cut when stored.
fn windows_of(source: &str, lines: &[&str], a: usize, b: usize, qualname: &str) -> Vec<Chunk> {
    let (size, overlap) = (50usize, 10usize);
    let at = |l: &str| l.as_ptr() as usize - source.as_ptr() as usize;
    let mut out = Vec::new();
    let mut i = a;
    while i < b {
        // as many lines as the text cap holds, one at least: a window of very long lines is short
        let (mut n, mut bytes) = (0, 0);
        while n < size && i + n < b {
            let more = lines[i + n].len() + (n > 0) as usize;
            if n > 0 && bytes + more > MAX_CHUNK_BYTES {
                break;
            }
            bytes += more;
            n += 1;
        }
        let seg = &lines[i..i + n];
        if seg.iter().any(|l| !l.trim().is_empty()) {
            let last = seg[seg.len() - 1];
            // through the last line's newline, so the lines read back are these lines
            let end = source[at(last) + last.len()..].find('\n').map_or(source.len(), |p| at(last) + last.len() + p + 1);
            out.push(Chunk {
                qualname: qualname.to_string(),
                kind: "window",
                start: i as i64 + 1,
                end: (i + seg.len()) as i64,
                lazy: Some(Lazy { text: at(seg[0]), body: at(seg[0]), end, lines: size }),
                ..Default::default()
            });
        }
        if i + n >= b {
            break;
        }
        // a window the cap cut short holds no line twice
        i += if n < size { n } else { size - overlap };
    }
    out
}

/// Whole lines that `head_bytes` keeps of `s` cut at `n` lines (`usize::MAX` when it keeps them all).
fn kept_lines(s: &[u8], n: usize) -> usize {
    let scan = &s[..s.len().min(MAX_CHUNK_BYTES + 1)];
    let (mut all, mut whole) = (0, 0);
    for i in memchr::memchr_iter(b'\n', scan) {
        all += 1;
        if all >= n {
            return n;
        }
        whole += (i < MAX_CHUNK_BYTES) as usize;
    }
    if s.len() <= MAX_CHUNK_BYTES {
        usize::MAX
    } else {
        whole.max(1)
    }
}

/// Brackets alone or a block's closing word: nothing to search for.
fn is_closer(line: &str) -> bool {
    let t = line.trim();
    matches!(t.trim_end_matches([';', ',']), "end" | "fi" | "done" | "esac") || t.chars().all(|c| "{}[]();,".contains(c))
}

/// How an attribute line above a definition starts in a language that writes them outside the
/// definition's node. Elsewhere a line that starts so is something else: a Makefile recipe
/// (`@echo`), a Ruby instance variable, an Objective-C directive.
fn attr_mark(lang: &str) -> &'static [u8] {
    match lang {
        "rust" | "php" => b"#[",
        "javascript" | "typescript" | "tsx" | "python" | "java" | "kotlin" | "scala" | "groovy" | "dart" | "swift" | "elixir" | "gleam" => b"@",
        _ => b"",
    }
}

/// Where a definition's text starts when the attribute lines right above `at` are its own, and
/// how many lines that adds.
fn attr_start(src: &[u8], mut at: usize, mark: &[u8]) -> (usize, usize) {
    let mut above = 0;
    if mark.is_empty() {
        return (at, 0);
    }
    loop {
        // only blanks may stand before `at` on its line (scanning no further keeps it linear)
        let Some(nl) = src[..at].iter().rposition(|b| !b.is_ascii_whitespace() || *b == b'\n').filter(|&i| src[i] == b'\n') else {
            return (at, above);
        };
        let line = nl + 1;
        let prev = src[..line - 1].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let indent = src[prev..line].iter().take_while(|&&b| b == b' ' || b == b'\t').count();
        let t = &src[prev + indent..line];
        if !t.starts_with(mark) {
            return (at, above);
        }
        at = prev + indent;
        above += 1;
    }
}

/// Windows over every stretch of lines that no span keeps, named after the innermost definition
/// holding the stretch (the module outside them). A stretch of brackets and closing words alone makes none.
fn uncovered_windows(source: &str, lines: &[&str], spans: &[(i64, i64)], defs: &[(i64, i64, &str)], stem: &str) -> Vec<Chunk> {
    let n = lines.len();
    let mut depth = vec![0i64; n + 1];
    for &(a, b) in spans {
        if a <= b && a >= 1 && a as usize <= n {
            depth[a as usize - 1] += 1;
            depth[(b as usize).min(n)] -= 1;
        }
    }
    let mut level = 0;
    let covered: Vec<bool> = depth[..n]
        .iter()
        .map(|d| {
            level += d;
            level > 0
        })
        .collect();
    let mut order: Vec<usize> = (0..defs.len()).collect();
    order.sort_by_key(|&k| (defs[k].0, -defs[k].1));
    let (mut owner, mut stack, mut next) = (vec![usize::MAX; n], Vec::new(), 0);
    for (i, o) in owner.iter_mut().enumerate() {
        let line = i as i64 + 1;
        while next < order.len() && defs[order[next]].0 <= line {
            stack.push(order[next]);
            next += 1;
        }
        while stack.last().is_some_and(|&k| defs[k].1 < line) {
            stack.pop();
        }
        *o = stack.last().copied().unwrap_or(usize::MAX);
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if covered[i] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < n && !covered[j] && owner[j] == owner[i] {
            j += 1;
        }
        let (mut a, mut b) = (i, j);
        while a < b && lines[a].trim().is_empty() {
            a += 1;
        }
        while b > a && lines[b - 1].trim().is_empty() {
            b -= 1;
        }
        if lines[a..b].iter().any(|l| !l.trim().is_empty() && !is_closer(l)) {
            out.extend(windows_of(source, lines, a, b, defs.get(owner[i]).map_or(stem, |d| d.2)));
        }
        i = j;
    }
    out
}

/// Qualified names keep at most this many segments, so deep nesting stays linear.
pub const MAX_QUAL_DEPTH: usize = 32;

fn qualify(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        return name.to_string();
    }
    let parent = if parent.matches('.').count() + 1 >= MAX_QUAL_DEPTH { parent.split_once('.').map_or(parent, |(_, r)| r) } else { parent };
    format!("{parent}.{name}")
}

/// `head` of src[a..b], copying only what it keeps.
fn head_bytes(src: &[u8], a: usize, b: usize, n: usize, max: usize) -> String {
    let (a, b) = (a.min(src.len()), b.min(src.len()));
    let b = b.max(a);
    // past `max` the text is cut anyway: scanning further makes nested chunks quadratic
    let lim = b.min(a + max + 1);
    // as str, so the search runs on the standard library's memchr; `lim` may split a character
    let s = std::str::from_utf8(&src[a..lim]).unwrap_or_else(|e| std::str::from_utf8(&src[a..a + e.valid_up_to()]).unwrap_or_default());
    let end = s.match_indices('\n').nth(n - 1).map_or(b, |(i, _)| a + i);
    let mut cut = end < b;
    let mut e = end;
    if e - a > max {
        e = a + max;
        while e > a && src[e] & 0xC0 == 0x80 {
            e -= 1; // not inside a character
        }
        cut = true;
    }
    if cut {
        txt(src, a, e) + "\n..."
    } else {
        txt(src, a, e)
    }
}

/// End of the first line of src[a..b], at most 1 KB on: keywords sit at the start of a line.
pub fn first_line_end(src: &[u8], a: usize, b: usize) -> usize {
    let (a, b) = (a.min(src.len()), b.min(src.len()).min(a + 1024));
    src[a..b.max(a)].iter().position(|&c| c == b'\n').map_or(b, |i| a + i)
}

/// `s` without its template or generic arguments: `ns::Map<K, std::vector<V>>` gives `ns::Map`.
fn strip_generics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// The owner a definition names outside its own body: a Go method's receiver type (`*Store[K]`
/// gives `Store`), the class of a C++ method defined out of line (`Widget::draw`), the table of
/// a Lua function (`M.sub.deep`), the namespace of an Elixir module (`MyApp.Accounts.User`).
/// A Rust impl is named by its bare type: `impl<T> crate::Stack<T>` gives `Stack`.
fn split_owner(lang: &str, node: Node, name: String, src: &[u8]) -> (Option<String>, String) {
    // `r#type` is the name `type`, as a call of it is
    let name = match name.strip_prefix("r#") {
        Some(raw) if lang == "rust" => raw.to_string(),
        _ => name,
    };
    if lang == "rust" && node.kind() == "impl_item" {
        let bare = name.split('<').next().unwrap_or(&name).rsplit("::").next().unwrap_or("").trim();
        return (None, if bare.is_empty() { name.clone() } else { bare.to_string() });
    }
    if lang == "lua" && node.kind() == "field" {
        return (lua_table_owner(node, src), name);
    }
    if lang == "lua" || lang == "elixir" {
        if let Some(i) = name.rfind(|c| c == '.' || lang == "lua" && c == ':') {
            if i > 0 && i + 1 < name.len() {
                return (Some(name[..i].replace(':', ".")), name[i + 1..].to_string());
            }
        }
        return (None, name);
    }
    if lang == "julia" {
        // `Base.show` is `show` owned by `Base`, `Base.:+` is `+`
        return match name.rsplit_once('.') {
            Some((owner, last)) if !owner.is_empty() && owner.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '!')) => {
                (Some(owner.to_string()), last.trim_start_matches(':').to_string())
            }
            Some((_, last)) => (None, last.to_string()),
            None => (None, name),
        };
    }
    if lang == "r" && node.kind() == "argument" {
        return (crate::langs::r6_class(node, src), name);
    }
    if matches!(lang, "javascript" | "typescript" | "tsx") && matches!(node.kind(), "method_definition" | "pair") {
        return (object_owner(node, src), name);
    }
    if matches!(lang, "javascript" | "typescript" | "tsx") && node.kind() == "assignment_expression" {
        if let Some((owner, last)) = name.rsplit_once('.') {
            return (Some(owner.to_string()), last.to_string());
        }
    }
    if lang == "go" && node.kind() == "method_declaration" {
        let mut stack: Vec<Node> = node.child_by_field_name("receiver").into_iter().collect();
        while let Some(n) = stack.pop() {
            if n.kind() == "type_identifier" {
                return (Some(txt(src, n.start_byte(), n.end_byte())), name);
            }
            let kids: Vec<Node> = n.children(&mut n.walk()).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    if matches!(lang, "c" | "cpp") {
        // `int& operator*()`: the declarator's reference or pointer mark is not part of the name
        let name = name.trim_start_matches(['&', '*', ' ']).to_string();
        if let Some(i) = name.rfind("::") {
            let owner = strip_generics(&name[..i]).replace("::", ".");
            if !owner.is_empty() {
                return (Some(owner), name[i + 2..].to_string());
            }
        }
        return (None, name);
    }
    (None, name)
}

/// The names that hold the object literal around a JS/TS method: `const store = { sub: { m() {} } }`
/// gives `store.sub`, `module.exports.tool = {...}` gives `tool`. None for a literal nothing names.
fn object_owner<'t>(node: Node<'t>, src: &[u8]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut obj = node.parent().filter(|p| p.kind() == "object")?;
    // `{...} as const`, `{...} satisfies T` and `({...})` are held by the name around them
    let unwrap = |mut n: Node<'t>| {
        while let (true, Some(p)) = (matches!(n.kind(), "as_expression" | "satisfies_expression" | "parenthesized_expression"), n.parent()) {
            n = p;
        }
        n
    };
    while let Some(holder) = obj.parent().map(unwrap) {
        let named = match holder.kind() {
            "pair" => holder.child_by_field_name("key").filter(|k| k.kind() == "property_identifier"),
            "variable_declarator" => holder.child_by_field_name("name").filter(|n| n.kind() == "identifier"),
            // `module.exports = {...}` exports its members under their own names
            "assignment_expression" => {
                holder.child_by_field_name("left").filter(|l| txt(src, l.start_byte(), l.end_byte()) != "module.exports").and_then(|l| match l.kind() {
                    "identifier" => Some(l),
                    "member_expression" => l.child_by_field_name("property"),
                    _ => None,
                })
            }
            _ => None,
        };
        let Some(n) = named else { break };
        parts.push(txt(src, n.start_byte(), n.end_byte()));
        match holder.parent().filter(|p| holder.kind() == "pair" && p.kind() == "object") {
            Some(p) => obj = p,
            None => break,
        }
    }
    parts.reverse();
    (!parts.is_empty()).then(|| parts.join("."))
}

/// The names that hold the Lua table around a field: `local obj = { sub = { f = function ...` gives
/// `obj.sub`. None for a table nothing names.
fn lua_table_owner(node: Node, src: &[u8]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut table = node.parent().filter(|p| p.kind() == "table_constructor")?;
    while let Some(holder) = table.parent() {
        if holder.kind() == "field" {
            let Some(n) = holder.child_by_field_name("name").filter(|n| n.kind() == "identifier") else { break };
            parts.push(txt(src, n.start_byte(), n.end_byte()));
            match holder.parent().filter(|p| p.kind() == "table_constructor") {
                Some(p) => table = p,
                None => break,
            }
            continue;
        }
        let assignment = holder.parent().filter(|a| holder.kind() == "expression_list" && a.kind() == "assignment_statement");
        if let Some(name) =
            assignment.and_then(crate::langs::lua_single_assignment).filter(|(_, v)| *v == table).and_then(|(t, _)| crate::langs::lua_target(t, src))
        {
            parts.push(name);
        }
        break;
    }
    parts.reverse();
    (!parts.is_empty()).then(|| parts.join("."))
}

/// The name of a file's anonymous default export: the file's stem, or its directory's name for an index file.
pub(crate) fn default_export_name(path: &str) -> String {
    let p = std::path::Path::new(path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match p.parent().and_then(|d| d.file_name()) {
        Some(dir) if stem == "index" => dir.to_string_lossy().into_owned(),
        _ => stem,
    }
}

fn txt(src: &[u8], a: usize, b: usize) -> String {
    String::from_utf8_lossy(&src[a.min(src.len())..b.min(src.len())]).into_owned()
}

/// A Lisp symbol in call position: one name whatever it holds (`list->vec`, `set-car!`, `empty?`).
static LISP_SYMBOL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^[^\s()\[\]{}"'`,;#\d][^\s()\[\]{}"'`,;]*"#).unwrap());

/// One file's source and, per separator a call target is cut at, the start of each of its
/// occurrences in the file, overlapping ones too: a call target holds every call before it in a
/// chain, so looking for the last separator in its whole text would make a chain quadratic.
struct Seps<'s> {
    src: &'s str,
    found: Vec<(&'static str, Vec<usize>)>,
}

impl<'s> Seps<'s> {
    fn new(src: &'s str) -> Self {
        Seps { src, found: Vec::new() }
    }

    /// Where `src[s..e].rfind(sep)` finds `sep`, as an offset in `src`.
    fn last_in(&mut self, sep: &'static str, s: usize, e: usize) -> Option<usize> {
        let at = match self.found.iter().position(|(k, _)| *k == sep) {
            Some(at) => at,
            None => {
                let b = sep.as_bytes();
                self.found.push((sep, self.src.as_bytes().windows(b.len()).enumerate().filter(|(_, w)| *w == b).map(|(i, _)| i).collect()));
                self.found.len() - 1
            }
        };
        let list = &self.found[at].1;
        list[..list.partition_point(|&p| p + sep.len() <= e)].last().copied().filter(|&p| p >= s)
    }
}

fn callee_name(lang: &str, node: Node, field: &str, seps: &mut Seps) -> Option<String> {
    let src = seps.src.as_bytes();
    let mut target =
        if field.is_empty() { None } else { node.child_by_field_name(field).or_else(|| node.children(&mut node.walk()).find(|c| c.kind() == field)) };
    if target.is_none() && node.named_child_count() > 0 {
        target = node.named_child(0);
    }
    let mut target = target?;
    match (lang, node.kind()) {
        // `$(call g,1)` calls `g`; `$(subst ...)` and the other built-ins are no calls
        ("make", "function_call") => {
            let f = node.child_by_field_name("function")?;
            if src.get(f.start_byte()..f.end_byte()) != Some(&b"call"[..]) {
                return None;
            }
            // the name ends before a comma or a space, so the arguments after the first need no split
            let args = seps.src.get(target.start_byte()..target.end_byte())?;
            return IDENT_START.find(args.trim_start()).map(|m| m.as_str().to_string());
        }
        ("make", "raw_text") => return None,
        // a call with arguments is its own `exprCall`; any other statement is no call
        ("pascal", "statement") if node.named_child_count() != 1 || !matches!(target.kind(), "identifier" | "exprDot") => return None,
        ("verilog", "data_declaration") if !verilog_typeless_call(node, target, src) => return None,
        _ => {}
    }
    if target.kind() == "method_index_expression" {
        // Lua `obj:meth(x)` calls `meth`
        target = target.child_by_field_name("method")?;
    }
    if target.kind() == "generic_function" {
        // `f::<T>(x)`: the name is before the turbofish
        target = target.child_by_field_name("function")?;
    }
    if target.kind() == "extract_operator" {
        // R `obj$meth(x)` calls `meth`
        target = target.child_by_field_name("rhs")?;
    }
    if matches!(lang, "scheme" | "racket" | "elisp" | "commonlisp") {
        // the symbol ends at a space or a bracket: a deep nested form's whole text is not copied
        let s = &src[target.start_byte().min(src.len())..target.end_byte().min(src.len())];
        let lead = s.iter().take_while(|b| b.is_ascii_whitespace()).count();
        let end = s[lead..].iter().position(|b| b.is_ascii_whitespace() || b"()[]{}\"'`,;".contains(b)).map_or(s.len(), |i| lead + i);
        let t = String::from_utf8_lossy(&s[..end]);
        // an operator such as `-` or `<=` is not a name; a Common Lisp package prefix is not part of it
        let name = LISP_SYMBOL.find(t.trim())?.as_str();
        let name = if lang == "commonlisp" { name.rsplit(':').next().unwrap_or(name) } else { name };
        return name.chars().any(char::is_alphabetic).then(|| name.to_string());
    }
    // the target's text is cut by moving its start; only its two ends are read
    let (mut s, e) = (target.start_byte(), target.end_byte());
    let t = seps.src.get(s..e)?;
    if lang == "cpp" {
        // `a.operator+(b)` parses as a call of `a` with `.operator+` in an error node
        let lost = node.children(&mut node.walk()).find(|c| c.is_error()).map(|c| (c.start_byte(), c.end_byte()));
        let (a, b) = lost.filter(|&(a, b)| seps.last_in("operator", a, b).is_some()).unwrap_or((s, e));
        if let Some(name) = cpp_operator_or_destructor(seps, a, b) {
            return Some(name);
        }
    }
    // D and Solidity `new Point(1, 2)` is a call whose callee is `new Point`
    if let Some(rest) = t.strip_prefix("new").filter(|r| r.starts_with(char::is_whitespace)) {
        s = e - rest.trim_start().len();
    }
    // a dot is part of an R name and `::` part of a Bash one
    let cuts: &[&'static str] = match lang {
        "r" => &["::", "->"],
        "bash" => &["->"],
        "php" => &["::", ".", "->", "\\"],
        _ => &["::", ".", "->"],
    };
    for sep in cuts {
        if let Some(i) = seps.last_in(sep, s, e) {
            s = i + sep.len();
        }
    }
    let t = seps.src[s..e].trim().trim_start_matches('@');
    // a `!` is cut for a Rust macro; in Ruby, Elixir and Julia it ends the name
    let t = if matches!(lang, "ruby" | "elixir" | "julia") { t } else { t.trim_end_matches('!') };
    // a JavaScript private method is named with its `#`
    if let Some(private) = t.strip_prefix('#').filter(|_| matches!(lang, "javascript" | "typescript" | "tsx")) {
        return IDENT_START.find(private).map(|m| format!("#{}", m.as_str()));
    }
    let t = if lang == "rust" { t.strip_prefix("r#").unwrap_or(t) } else { t };
    // Julia `==(a, b)` and `Base.:(==)(a, b)` call the operator a method definition names
    let op = t.trim_start_matches(':').trim_start_matches('(').trim_end_matches(')');
    if lang == "julia" && !op.is_empty() && op.chars().all(|c| c.is_ascii_punctuation()) {
        return Some(op.to_string());
    }
    name_rule(lang).find(t).map(|m| m.as_str().to_string())
}

/// `operator==` or `~Shape` at the end of the C++ call target `s..e`, as their definitions are named.
fn cpp_operator_or_destructor(seps: &mut Seps, s: usize, e: usize) -> Option<String> {
    let src = seps.src;
    let word = |c: char| c.is_alphanumeric() || c == '_';
    if let Some(i) = seps.last_in("operator", s, e).filter(|&i| !src[s..i].ends_with(word)) {
        // the longest operators (`->*`, `<=>`) have three marks: in `a.operator()()()` only the first call is one
        let rest = &src[i + "operator".len()..e];
        let op: String = if rest.len() <= 64 { rest.split_whitespace().collect() } else { String::new() };
        if (1..=3).contains(&op.len()) && op.chars().all(|c| c.is_ascii_punctuation()) {
            return Some(format!("operator{op}"));
        }
    }
    let after = [".", ":", ">"].into_iter().filter_map(|c| seps.last_in(c, s, e)).max().map_or(s, |p| p + 1);
    let tail = src[after..e].trim();
    IDENT_START.find(tail.strip_prefix('~')?).map(|m| format!("~{}", m.as_str()))
}

/// The pattern of a name at the start of a text in `lang`.
pub(crate) fn name_rule(lang: &str) -> &'static Regex {
    match lang {
        "ruby" | "elixir" => &PREDICATE_NAME,
        "julia" => &BANG_NAME,
        "r" => &R_NAME,
        "bash" => &BASH_NAME,
        "haskell" | "ocaml" | "fsharp" | "nix" => &PRIMED_NAME,
        _ => &IDENT_START,
    }
}

// the name characters some languages allow past a plain identifier, as their definitions keep them
static PREDICATE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*[?!]?").unwrap());
static BANG_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*!?").unwrap());
static R_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z.][\w.]*").unwrap());
static BASH_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w$]*(?:(?:-|::|\.)[A-Za-z_]\w*)*").unwrap());
static PRIMED_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w']*(?:-[A-Za-z_][\w']*)*").unwrap());

const ASM_CALLS: &[&str] = &["call", "callq", "calll", "bl", "jal"];
const ASM_SECTIONS: &[&str] = &[".text", ".data", ".bss", ".rodata", ".section"];

/// The last node of an assembly label's block, the instructions after it up to the next label
/// or section directive, and the calls among them. A local label (`.L2:`) does not end a global one.
fn asm_block<'t>(label: Node<'t>, siblings: &[Node<'t>], at: usize, src: &[u8], calls: &mut BTreeSet<String>) -> Node<'t> {
    let text_of = |n: Node| txt(src, n.start_byte(), n.end_byte());
    let local = |n: Node| text_of(n).starts_with(|c: char| c == '.' || c.is_ascii_digit());
    let in_local = local(label);
    let mut last = label;
    for &s in siblings.iter().skip(at + 1) {
        let word = || s.child_by_field_name("kind").map(|k| text_of(k).to_lowercase()).unwrap_or_default();
        match s.kind() {
            "label" if in_local || !local(s) => break,
            "meta" if ASM_SECTIONS.contains(&word().as_str()) => break,
            "line_comment" | "block_comment" => continue,
            _ if !s.is_named() => continue,
            "instruction" if ASM_CALLS.contains(&word().as_str()) => {
                let target = s.named_children(&mut s.walk()).find(|c| c.kind() == "ident");
                calls.extend(target.and_then(|t| IDENT_START.find(&text_of(t)).map(|m| m.as_str().to_string())));
            }
            _ => {}
        }
        last = s;
    }
    last
}

/// `$(call g,1)` or `${call g}` in text the Make grammar leaves unparsed.
static MAKE_CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$[({]call\s+([A-Za-z_][\w-]*)").unwrap());

/// The callee of a Jsonnet `suffix_apply`, which follows its callee as a sibling: `g` of `g(x)`,
/// `method` of `obj.method(x)`.
fn sibling_callee(siblings: &[Node], at: usize, src: &[u8]) -> Option<String> {
    let prev = siblings.get(at.checked_sub(1)?)?;
    let name = match prev.kind() {
        "ident_ref" => *prev,
        "suffix_index" => prev.named_child(0).filter(|n| n.kind() == "ident")?,
        _ => return None,
    };
    Some(txt(src, name.start_byte(), name.end_byte()))
}

/// Whether a Verilog declaration with no type, `target` its one name, is a task call: `h(1);`,
/// where an ERROR holding the arguments follows, or `h;` in a block, since a declaration
/// without a type needs `var` (IEEE 1800, the rule under `data_declaration`).
fn verilog_typeless_call(node: Node, target: Node, src: &[u8]) -> bool {
    let one_name = node.child(0) == Some(target) && target.kind() == "list_of_variable_decl_assignments" && target.named_child_count() == 1;
    let args = target.next_named_sibling().is_some_and(|n| n.kind() == "ERROR" && src.get(n.start_byte()) == Some(&b'('));
    let bare = target.next_sibling().is_some_and(|n| n.kind() == ";")
        && target.named_child(0).is_some_and(|v| v.child_count() == 1)
        && node.parent().is_some_and(|p| p.kind() == "block_item_declaration");
    one_name && (args || bare)
}

/// A VHDL name with arguments, `f(x)` or `work.pkg.f(x)`, outside a type (`std_logic_vector(7 downto 0)`),
/// or the entity or component an instance names (`u1: entity work.adder port map (...)`).
fn vhdl_callee(node: Node, parent: Node, src: &[u8]) -> Option<String> {
    if node.kind() == "component_instantiation_statement" {
        let unit = node.child_by_field_name("component").or_else(|| {
            let u = node.children(&mut node.walk()).find(|c| c.kind() == "instantiated_unit")?;
            u.child_by_field_name("entity").or_else(|| u.child_by_field_name("component"))
        })?;
        let id = if unit.kind() == "identifier" { unit } else { unit.named_children(&mut unit.walk()).find(|c| c.kind() == "identifier")? };
        return Some(txt(src, id.start_byte(), id.end_byte()));
    }
    if parent.kind() == "subtype_indication" {
        return None;
    }
    let kids: Vec<Node> = node.named_children(&mut node.walk()).collect();
    let args = kids.iter().position(|k| k.kind() == "parenthesis_group").filter(|&i| i > 0)?;
    let name = match kids[args - 1].kind() {
        "identifier" => kids[args - 1],
        "selection" => kids[args - 1].named_child(0).filter(|n| n.kind() == "identifier")?,
        _ => return None,
    };
    Some(txt(src, name.start_byte(), name.end_byte()))
}

/// The signals, variables, constants, files and ports a VHDL file declares: `a(i)` of one is an
/// element, not a call, as the grammar gives both one shape.
fn vhdl_objects(root: Node, src: &[u8]) -> HashSet<String> {
    const OBJECTS: &[&str] = &[
        "signal_declaration",
        "variable_declaration",
        "private_variable_declaration",
        "constant_declaration",
        "file_declaration",
        "interface_declaration",
        "interface_signal_declaration",
        "interface_variable_declaration",
        "interface_constant_declaration",
        "interface_file_declaration",
    ];
    let mut out = HashSet::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if OBJECTS.contains(&n.kind()) {
            for list in n.named_children(&mut n.walk()).filter(|c| c.kind() == "identifier_list") {
                out.extend(
                    list.named_children(&mut list.walk()).filter(|i| i.kind() == "identifier").map(|i| txt(src, i.start_byte(), i.end_byte()).to_lowercase()),
                );
            }
        }
        stack.extend(n.named_children(&mut n.walk()));
    }
    out
}

/// The part of a definition that names it or lists its parameters and that the grammar parses as
/// a call: Elixir `def f(x)`, Scheme `(define (f x) ...)`, a Lisp parameter list `(x y)`.
fn def_head<'t>(lang: &str, node: Node<'t>) -> Option<Node<'t>> {
    match lang {
        "elixir" => {
            let head = node.children(&mut node.walk()).find(|c| c.kind() == "arguments")?.named_child(0)?;
            if head.kind() == "binary_operator" {
                head.child_by_field_name("left")
            } else {
                Some(head)
            }
        }
        "scheme" | "racket" => node.named_child(1).filter(|n| n.kind() == "list"),
        "elisp" => node.child_by_field_name("parameters"),
        "commonlisp" => node.named_child(0)?.child_by_field_name("lambda_list"),
        _ => None,
    }
}

/// 0-based last line holding the node's text: a node that ends with its line break (a C
/// `#define`) ends at column 0 of the next line.
fn last_row(node: Node) -> i64 {
    let end = node.end_position();
    (end.row - usize::from(end.column == 0 && end.row > node.start_position().row)) as i64
}

/// Calls inside a Rust macro's token trees, which tree-sitter does not parse as expressions: a
/// name directly followed by `(...)` (`a::b(x)`, `x.m(y)`), and the name of a nested macro.
fn macro_arg_calls(node: Node, src: &[u8], out: &mut BTreeSet<String>) {
    // after the first comma of `matches!(x, Some(_))` comes a pattern, not calls
    let is_matches = |n: Node| txt(src, n.start_byte(), n.end_byte()).ends_with("matches");
    let pattern_after_comma = node.child_by_field_name("macro").is_some_and(is_matches);
    let mut stack: Vec<(Node, bool)> = node.children(&mut node.walk()).filter(|c| c.kind() == "token_tree").map(|c| (c, pattern_after_comma)).collect();
    while let Some((tt, stop_at_comma)) = stack.pop() {
        let kids: Vec<Node> = tt.children(&mut tt.walk()).collect();
        let kind = |i: usize| kids.get(i).map_or("", |k| k.kind());
        for (i, k) in kids.iter().enumerate() {
            match k.kind() {
                "," if stop_at_comma => break,
                // `#[derive(Debug)]` and `#![...]` are attributes, not calls
                "token_tree" if !(i >= 1 && kind(i - 1) == "#" || i >= 2 && kind(i - 1) == "!" && kind(i - 2) == "#") => {
                    stack.push((*k, i >= 2 && kind(i - 1) == "!" && kind(i - 2) == "identifier" && is_matches(kids[i - 2])));
                }
                "identifier" => {
                    let parens = kind(i + 1) == "token_tree" && kids[i + 1].child(0).is_some_and(|p| p.kind() == "(");
                    let declared = i >= 1 && matches!(kind(i - 1), "fn" | "struct" | "enum" | "union");
                    let nested_macro = kind(i + 1) == "!" && kind(i + 2) == "token_tree";
                    if parens && !declared || nested_macro {
                        out.insert(txt(src, k.start_byte(), k.end_byte()));
                    }
                }
                _ => {}
            }
        }
    }
}

static CAPS_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^_*[A-Z][A-Z0-9_]*$").unwrap());

/// The constants declared at `node`: their names and the node whose text each chunk shows. Constants are
/// module or type level: Rust `const`/`static`, Go package `const`/`var`, a JS/TS top-level
/// `const` that is not a function, a TS enum member, a Java `static final` field, a Python
/// module- or class-level ALL_CAPS assignment, a TS `static readonly` and JS ALL_CAPS `static`
/// class field, a C/C++ `#define NAME value`, enumerator and file or namespace `const` with a value
/// (GLSL and Objective-C too), a C++ `static const` or `constexpr` class member with a value, and a
/// Ruby assignment to a capitalized name at file, module or class level. Java also gives its
/// enum constants and interface constants, and its other fields and record components as `field`.
/// `scope` is the kind of the nearest ancestor that is no `#if` block or include guard.
/// -> (names, the node shown, the kind)
fn constant_of<'t>(
    lang: &str,
    node: Node<'t>,
    ancestors: &[(Node<'t>, Vec<Node<'t>>)],
    scope: Option<&str>,
    src: &[u8],
) -> Option<(Vec<String>, Node<'t>, &'static str)> {
    let up = |i: usize| ancestors.len().checked_sub(i).map(|j| ancestors[j].0);
    let kind_up = |i: usize| up(i).map_or("", |n| n.kind());
    let text_of = |n: Node| txt(src, n.start_byte(), n.end_byte());
    let field = |n: Node<'t>, f: &str| n.child_by_field_name(f);
    let mut kind = "constant";
    let (names, shown) = match (lang, node.kind()) {
        ("rust", "const_item" | "static_item") if matches!(kind_up(1), "source_file" | "declaration_list") => (vec![text_of(field(node, "name")?)], node),
        ("go", "const_spec" | "var_spec") => {
            let at = if kind_up(1) == "var_spec_list" { 2 } else { 1 };
            if !matches!(kind_up(at), "const_declaration" | "var_declaration") || kind_up(at + 1) != "source_file" {
                return None;
            }
            let decl = up(at)?;
            // `const A, B = 1, 2` declares both names; the field also holds the commas
            let names: Vec<String> = node.children_by_field_name("name", &mut node.walk()).filter(|n| n.kind() == "identifier").map(text_of).collect();
            (names, if at == 1 && decl.named_child_count() == 1 { decl } else { node })
        }
        ("javascript" | "typescript" | "tsx", "variable_declarator") => {
            let decl = up(1).filter(|d| d.kind() == "lexical_declaration" && d.child(0).is_some_and(|k| k.kind() == "const"))?;
            let shown = match kind_up(2) {
                "program" => decl,
                "export_statement" if kind_up(3) == "program" => up(2)?,
                _ => return None,
            };
            let name = field(node, "name").filter(|n| n.kind() == "identifier")?;
            if field(node, "value").is_some_and(|v| crate::langs::JS_FUNCS.contains(&v.kind())) {
                return None;
            }
            (vec![text_of(name)], shown)
        }
        ("typescript" | "tsx", "property_identifier") if kind_up(1) == "enum_body" => (vec![text_of(node)], node),
        ("typescript" | "tsx", "enum_assignment") if kind_up(1) == "enum_body" => (vec![text_of(field(node, "name")?)], node),
        ("java", "field_declaration" | "constant_declaration") => {
            let mods = node.children(&mut node.walk()).find(|c| c.kind() == "modifiers");
            let words: Vec<&str> = mods.map(|m| m.children(&mut m.walk()).map(|c| c.kind()).collect()).unwrap_or_default();
            // a field of an interface is a constant
            if node.kind() == "field_declaration" && !(words.contains(&"static") && words.contains(&"final")) {
                kind = "field";
            }
            let names: Vec<String> = node.children_by_field_name("declarator", &mut node.walk()).filter_map(|d| field(d, "name")).map(text_of).collect();
            (names, node)
        }
        ("java", "enum_constant") => (vec![text_of(field(node, "name")?)], node),
        ("java", "formal_parameter") if kind_up(2) == "record_declaration" => {
            kind = "field";
            (vec![text_of(field(node, "name")?)], node)
        }
        ("python", "expression_statement")
            if (kind_up(1) == "module" || kind_up(1) == "block" && kind_up(2) == "class_definition") && node.named_child_count() == 1 =>
        {
            let a = node.named_child(0).filter(|a| a.kind() == "assignment")?;
            let left = field(a, "left")?;
            let names = match left.kind() {
                "identifier" => vec![text_of(left)],
                "pattern_list" | "tuple_pattern" | "list_pattern" => {
                    left.named_children(&mut left.walk()).filter(|n| n.kind() == "identifier").map(text_of).collect()
                }
                _ => return None,
            };
            (names, node)
        }
        ("javascript" | "typescript" | "tsx", "field_definition" | "public_field_definition") if kind_up(1) == "class_body" => {
            let words: Vec<&str> = node.children(&mut node.walk()).map(|c| c.kind()).collect();
            // JavaScript has no `readonly`: an ALL_CAPS static field is its constant
            let ts = lang != "javascript";
            if !words.contains(&"static") || ts && !words.contains(&"readonly") {
                return None;
            }
            let name = field(node, "name").or_else(|| field(node, "property")).filter(|n| n.kind() == "property_identifier")?;
            let name = text_of(name);
            if !ts && !CAPS_NAME.is_match(&name) {
                return None;
            }
            (vec![name], node)
        }
        ("c" | "cpp" | "glsl" | "objc", "declaration") => {
            if !matches!(scope, Some("translation_unit" | "declaration_list" | "linkage_specification")) {
                return None;
            }
            let is_const = |n: Node| n.children(&mut n.walk()).any(|c| c.kind() == "type_qualifier" && matches!(text_of(c).as_str(), "const" | "constexpr"));
            let const_type = is_const(node);
            // only a declarator with a value: `extern const int X;` defines nothing; `T *const p` is constant too
            let names: Vec<String> = node
                .children_by_field_name("declarator", &mut node.walk())
                .filter(|d| d.kind() == "init_declarator")
                .filter(|d| const_type || field(*d, "declarator").is_some_and(|p| p.kind() == "pointer_declarator" && is_const(p)))
                .filter_map(|d| dig(d, "declarator", &["identifier", "qualified_identifier"]))
                .filter(|n| matches!(n.kind(), "identifier" | "qualified_identifier"))
                .map(text_of)
                .collect();
            (names, node)
        }
        ("cpp", "field_declaration") if kind_up(1) == "field_declaration_list" => {
            let words: Vec<String> =
                node.children(&mut node.walk()).filter(|c| matches!(c.kind(), "storage_class_specifier" | "type_qualifier")).map(text_of).collect();
            if !words.iter().any(|w| w == "static") || !words.iter().any(|w| w == "const" || w == "constexpr") {
                return None;
            }
            // a member declared without its value is defined, and is the constant, outside the class
            let mut names = Vec::new();
            let mut cur = node.walk();
            let mut declarator = None;
            for (i, c) in node.children(&mut cur).enumerate() {
                match node.field_name_for_child(i as u32) {
                    Some("declarator") => declarator = Some(c),
                    Some("default_value") => names.extend(declarator.take().and_then(|d| dig(d, "declarator", &["field_identifier"]).or(Some(d)))),
                    _ => {}
                }
            }
            (names.into_iter().filter(|n| n.kind() == "field_identifier").map(text_of).collect(), node)
        }
        ("gdscript", "const_statement") if matches!(kind_up(1), "source" | "class_body") => (vec![text_of(field(node, "name")?)], node),
        ("c" | "cpp", "preproc_def") if field(node, "value").is_some() => (vec![text_of(field(node, "name")?)], node),
        ("c" | "cpp", "enumerator") => (vec![text_of(field(node, "name")?)], node),
        // a capitalized name is a constant in Ruby
        ("ruby", "assignment") if kind_up(1) == "program" || kind_up(1) == "body_statement" && matches!(kind_up(2), "class" | "module") => {
            (vec![text_of(field(node, "left").filter(|l| l.kind() == "constant")?)], node)
        }
        _ => return None,
    };
    let names: Vec<String> = names.into_iter().filter(|n| n != "_" && (lang != "python" || CAPS_NAME.is_match(n))).collect();
    (!names.is_empty()).then_some((names, shown, kind))
}

/// The chunk kind named by the first declaration keyword on the node's first line.
fn kind_from_head(node: Node, src: &[u8]) -> Option<&'static str> {
    let t = txt(src, node.start_byte(), first_line_end(src, node.start_byte(), node.end_byte()));
    let line = t.lines().next().unwrap_or("");
    for w in line.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '{' | ':' | '<' | ',' | '[')) {
        if let Some((_, k)) = crate::langs::KIND_WORDS.iter().find(|(kw, _)| *kw == w) {
            return Some(k);
        }
    }
    None
}

/// The block of an Objective-C method definition, which the grammar gives no `body` field.
fn objc_method_body(node: Node) -> Option<Node> {
    let def = node.named_child(0).filter(|_| node.kind() == "implementation_definition")?;
    def.children(&mut def.walk()).find(|c| c.kind() == "compound_statement")
}

fn dig<'t>(mut node: Node<'t>, field: &str, kinds: &[&str]) -> Option<Node<'t>> {
    let mut last = None;
    // the `(*cb)` of `typedef int (*cb)(int);` holds its declarator as a plain child
    let inner = |n: Node<'t>| n.named_children(&mut n.walk()).find(|c| c.kind().ends_with("_declarator") || kinds.contains(&c.kind()));
    while let Some(ch) = node.child_by_field_name(field).or_else(|| if node.kind() == "parenthesized_declarator" { inner(node) } else { None }) {
        if kinds.contains(&ch.kind()) {
            return Some(ch);
        }
        last = Some(ch);
        node = ch;
    }
    if kinds.is_empty() {
        return last;
    }
    // the innermost node reached may itself contain the identifier as a child (parenthesized declarators)
    last.and_then(|n| {
        let mut cur = n.walk();
        let found = n.children(&mut cur).find(|c| kinds.contains(&c.kind()));
        found
    })
    .or(last)
}

fn def_name(node: Node, rules: &[NameRule], src: &[u8]) -> Option<String> {
    for rule in rules {
        let got = match rule {
            NameRule::Field(f) => node.child_by_field_name(f).map(|n| txt(src, n.start_byte(), n.end_byte())),
            NameRule::Dig(f, kinds) => dig(node, f, kinds).map(|n| txt(src, n.start_byte(), n.end_byte())),
            NameRule::ChildKind(kinds) => {
                let mut cur = node.walk();
                let found = node.children(&mut cur).find(|c| kinds.contains(&c.kind()));
                found.map(|n| txt(src, n.start_byte(), n.end_byte()))
            }
            NameRule::FirstNamed => node.named_child(0).map(|n| txt(src, n.start_byte(), n.end_byte())),
            NameRule::Own => Some(txt(src, node.start_byte(), node.end_byte())),
            NameRule::NamedChild(i) => node.named_child(*i as u32).map(|n| txt(src, n.start_byte(), n.end_byte())),
            NameRule::Custom(f) => f(node, src),
        };
        if let Some(n) = got {
            let n = n.trim().trim_matches(|c| c == '"' || c == '`');
            // a prime ends a Haskell name (`step'`); only a quoted name starts with one
            let n = if n.starts_with('\'') { n.trim_matches('\'') } else { n };
            let n = if n.starts_with('[') && n.ends_with(']') && n.len() > 2 { &n[1..n.len() - 1] } else { n };
            // the C++ call operator is named with its parentheses
            let call_op = n.ends_with("operator()");
            let n = if call_op { n } else { n.strip_suffix("()").unwrap_or(n) };
            let n = match n.find('(') {
                // a signature slipped in: keep the callee
                Some(i) if i > 0 && !call_op => &n[..i],
                _ => n,
            };
            let n = n.trim().to_string();
            // a long capture with spaces is an expression, never a name
            if !n.is_empty() && (n.len() <= 200 || !n.contains(char::is_whitespace)) {
                return Some(n);
            }
        }
    }
    None
}

fn first_string<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "string"
                | "encapsed_string"
                | "string_literal"
                | "system_lib_string"
                | "interpreted_string_literal"
                | "raw_string_literal"
                | "string_value"
                | "quoted_string"
                | "str_lit"
        ) {
            return Some(n);
        }
        let mut cur = n.walk();
        let kids: Vec<Node> = n.children(&mut cur).collect();
        // a nested node of the same kind is read on its own visit: `include include ...` stays linear
        for k in kids.into_iter().rev().filter(|k| k.kind() != node.kind()) {
            stack.push(k);
        }
    }
    None
}

const IMPORT_WORDS: &[&str] =
    &["import", "using", "use", "static", "qualified", "require", "require_relative", "include", "from", "open", "library", "source", "export"];

/// Module names as people refer to them: quotes, keywords, aliases and brace lists removed.
pub fn normalize_module(raw: &str) -> String {
    let mut t = raw.trim().trim_end_matches(';').trim().to_string();
    if t.starts_with('@') {
        t = t.trim_start_matches("@import").trim_start_matches('(').trim_end_matches(')').to_string();
    }
    // leading keywords
    loop {
        let mut changed = false;
        for w in IMPORT_WORDS {
            if let Some(rest) = t.strip_prefix(w) {
                if rest.starts_with(|c: char| c.is_whitespace() || c == '(') {
                    t = rest.trim_start().trim_start_matches('(').to_string();
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    if let Some(i) = t.find(" as ") {
        t.truncate(i);
    }
    if let Some(i) = t.find(['(', '{']) {
        t.truncate(i);
    }
    let t = t.trim().trim_end_matches("::").trim_end_matches(')').trim();
    let t = if t.chars().all(|c| c == '.') { t } else { t.trim_end_matches('.') }; // `from . import x` keeps its dot
    let t = t.trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '<' || c == '>');
    // Julia `import Base: show` names the module before the colon
    t.split_whitespace().next().unwrap_or("").trim_end_matches(':').to_string()
}

/// The first argument of a call, when it is a literal of one of `kinds` with no interpolation.
fn literal_arg<'t>(call: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let args = call.child_by_field_name("arguments").or_else(|| call.named_children(&mut call.walk()).find(|c| c.kind() == "argument_list"))?;
    let mut a = args.named_children(&mut args.walk()).find(|c| !c.kind().contains("comment"))?;
    if a.kind() == "argument" {
        a = a.child_by_field_name("value")?; // R
    }
    let holes = ["template_substitution", "interpolation", "string_interpolation"];
    if !kinds.contains(&a.kind()) || a.named_children(&mut a.walk()).any(|c| holes.contains(&c.kind())) {
        return None;
    }
    // Lua long brackets and R quotes: the content is the module
    Some(a.child_by_field_name("content").unwrap_or(a))
}

/// Each name of a PHP `use`: `use A\B, C;`, `use function A\f;` and `use A\{B, C as D};`.
fn php_use_names(node: Node, src: &[u8], out: &mut Vec<String>) {
    let clause = |cl: Node, prefix: &str| {
        cl.named_children(&mut cl.walk())
            .find(|c| matches!(c.kind(), "qualified_name" | "name"))
            .map(|n| format!("{prefix}{}", txt(src, n.start_byte(), n.end_byte())))
    };
    let mut prefix = String::new();
    for ch in node.named_children(&mut node.walk()) {
        match ch.kind() {
            "namespace_name" => prefix = format!("{}\\", txt(src, ch.start_byte(), ch.end_byte())),
            "namespace_use_clause" => out.extend(clause(ch, "")),
            "namespace_use_group" => {
                out.extend(ch.named_children(&mut ch.walk()).filter(|c| c.kind() == "namespace_use_clause").filter_map(|c| clause(c, &prefix)))
            }
            _ => {}
        }
    }
}

fn imports_of(node: Node, spec: &LangSpec, src: &[u8], out: &mut Vec<(i64, String)>) {
    if !node.is_named() {
        return; // keyword tokens share their kind name with the statement (`import`)
    }
    for rule in spec.imports {
        if rule.node != node.kind() {
            continue;
        }
        if !rule.when_head.is_empty() {
            let head = if rule.head_field.is_empty() { node.named_child(0) } else { node.child_by_field_name(rule.head_field) };
            let Some(h) = head else { continue };
            if node.child_by_field_name("receiver").is_some() {
                continue; // Ruby `YAML.load(...)` is a method call, not `load`
            }
            let ht = &src[h.start_byte().min(src.len())..h.end_byte().min(src.len())];
            if !rule.when_head.iter().any(|w| w.as_bytes() == ht) {
                continue;
            }
        }
        let line = node.start_position().row as i64 + 1;
        let mut texts: Vec<String> = Vec::new();
        match rule.how {
            ImportHow::Fields(f) => {
                let mut cur = node.walk();
                for (i, ch) in node.children(&mut cur).enumerate() {
                    if node.field_name_for_child(i as u32) == Some(f) {
                        texts.push(txt(src, ch.start_byte(), ch.end_byte()));
                    }
                }
            }
            ImportHow::FirstString => {
                if let Some(n) = first_string(node) {
                    texts.push(txt(src, n.start_byte(), n.end_byte()));
                }
            }
            ImportHow::ChildKind(kinds) => {
                let mut cur = node.walk();
                for (i, ch) in node.children(&mut cur).enumerate() {
                    // a child named `name` is an alias (C# `using IO = System.IO;`)
                    if kinds.contains(&ch.kind()) && node.field_name_for_child(i as u32) != Some("name") {
                        texts.push(txt(src, ch.start_byte(), ch.end_byte()));
                    }
                }
            }
            ImportHow::Text => texts.push(txt(src, node.start_byte(), node.end_byte())),
            ImportHow::Arg(kinds) => texts.extend(literal_arg(node, kinds).map(|n| txt(src, n.start_byte(), n.end_byte()))),
            ImportHow::UseClauses => php_use_names(node, src, &mut texts),
        }
        for t in texts {
            let m = normalize_module(&t);
            if !m.is_empty() && m.len() <= 300 && !IMPORT_WORDS.contains(&m.as_str()) {
                out.push((line, m));
            }
        }
        break;
    }
}

/// Contiguous comment block directly above a definition.
/// `ancestors` are the node's ancestors, root first, each with its children; `pos[i]` is the
/// position of `ancestors[i]` in its parent and `pos[len]` the node's (Node::parent and
/// prev_sibling walk down from the root, which makes deep nesting quadratic). The `lead`
/// siblings right above the node belong to the definition (a type signature).
/// Also returns the documentation's lines (1-based) when it has text.
fn leading_comment(
    lang: &str,
    node: Node,
    ancestors: &[(Node, Vec<Node>)],
    pos: &[usize],
    src: &[u8],
    wrappers: &[&str],
    lead: usize,
) -> (String, Option<(i64, i64)>) {
    let mut up = ancestors.len();
    let mut at = pos[up] - lead;
    let mut anchor = if lead > 0 { ancestors[up - 1].1[at] } else { node };
    // the definition's own header line: a comment there is not about a nested definition
    let mut header_row = -1;
    while up > 0 {
        let p = ancestors[up - 1].0;
        // a Common Lisp defun is its list's only child; a Jsonnet bind follows the `local` keyword;
        // the comment above the first statement of a body or file sits outside the node that lists them
        let py_body = at == 0 && p.kind() == "block" && up >= 2 && matches!(ancestors[up - 2].0.kind(), "class_definition" | "function_definition");
        let first_in_body = py_body || at == 0 && BODY_LISTS.contains(&p.kind());
        // the list's owner opens on the header line, unless the owner is the file
        if first_in_body && (py_body || up > 2) {
            header_row = ancestors[up - 2].0.start_position().row as i64;
        }
        // Go's `type T ...` on its own line: the comment stands above the declaration that holds T
        let lone_type = p.kind() == "type_declaration" && at > 0 && ancestors[up - 1].1[at - 1].kind() == "type";
        if !(wrappers.contains(&p.kind())
            || p.kind() == "decorated_definition"
            || first_in_body
            || lone_type
            || matches!((anchor.kind(), p.kind()), ("defun", "list_lit") | ("bind", "local_expr")))
        {
            break;
        }
        up -= 1;
        anchor = p;
        at = pos[up];
    }
    let siblings: &[Node] = if up > 0 { &ancestors[up - 1].1 } else { &[] };
    let mut prev_sibling = || -> Option<Node> {
        at = at.checked_sub(1)?;
        Some(siblings[at])
    };
    let mut parts: Vec<String> = Vec::new();
    let mut last = -1;
    let mut want = anchor.start_position().row as i64;
    let mut prev = prev_sibling();
    while let Some(p) = prev {
        // fish and assembly put line break tokens, assembly also empty nodes, below a comment
        if p.kind() == "\n" || p.start_byte() == p.end_byte() {
            prev = prev_sibling();
            continue;
        }
        if last_row(p) >= want - 1 && p.start_position().row as i64 != header_row {
            if let Some(doc) = doc_form(lang, p, src) {
                // an empty form (`@doc false`) says there is none, whatever comment follows it
                if parts.is_empty() || doc.is_empty() {
                    let rows = (!doc.is_empty()).then_some((p.start_position().row as i64 + 1, last_row(p) + 1));
                    return (doc, rows);
                }
                break;
            }
            if is_attribute(lang, p, src) {
                want = p.start_position().row as i64;
                prev = prev_sibling();
                continue;
            }
        }
        if COMMENT_TYPES.contains(&p.kind()) && last_row(p) >= want - 1 && p.start_position().row as i64 != header_row {
            let t = txt(src, p.start_byte(), p.end_byte());
            if t.starts_with("#!") {
                break; // a shebang is not documentation
            }
            if parts.is_empty() {
                last = last_row(p);
            }
            parts.push(t);
            want = p.start_position().row as i64;
            prev = prev_sibling();
        } else {
            break;
        }
    }
    if parts.is_empty() {
        return (String::new(), None);
    }
    parts.reverse();
    let doc = clean_comment(&parts.join("\n"));
    let rows = (!doc.is_empty()).then_some((want + 1, last + 1));
    (doc, rows)
}

/// The name of an Elixir module attribute (`spec` for `@spec ...`) and its call.
fn elixir_attribute<'t>(node: Node<'t>, src: &[u8]) -> Option<(String, Node<'t>)> {
    let call = node.child_by_field_name("operand").filter(|c| node.kind() == "unary_operator" && c.kind() == "call")?;
    let name = call.child_by_field_name("target")?;
    Some((txt(src, name.start_byte(), name.end_byte()), call))
}

/// Text of an Elixir `@doc` or `@moduledoc` given as a string or a sigil.
fn elixir_doc(node: Node, src: &[u8], word: &str) -> Option<String> {
    let (_, call) = elixir_attribute(node, src).filter(|(n, _)| n == word)?;
    let args = call.named_children(&mut call.walk()).find(|c| c.kind() == "arguments")?;
    let arg = args.named_child(0)?;
    // `@doc false` hides the function: it has no documentation
    if arg.kind() == "boolean" && txt(src, arg.start_byte(), arg.end_byte()) == "false" {
        return Some(String::new());
    }
    let arg = Some(arg).filter(|a| matches!(a.kind(), "string" | "sigil"))?;
    let text: String = arg.named_children(&mut arg.walk()).filter(|c| c.kind() == "quoted_content").map(|c| txt(src, c.start_byte(), c.end_byte())).collect();
    Some(cleandoc(&text))
}

/// Documentation a language writes above a definition as something other than a comment.
fn doc_form(lang: &str, p: Node, src: &[u8]) -> Option<String> {
    match (lang, p.kind()) {
        ("elixir", "unary_operator") => elixir_doc(p, src, "doc"),
        ("julia", "string_literal") => Some(py_string_content(&txt(src, p.start_byte(), p.end_byte())).trim().to_string()),
        ("erlang", "wild_attribute") => {
            let name = p.child_by_field_name("name")?.child_by_field_name("name")?;
            if txt(src, name.start_byte(), name.end_byte()) != "doc" {
                return None;
            }
            let v = p.child_by_field_name("value")?;
            let t = txt(src, v.start_byte(), v.end_byte());
            match v.kind() {
                "string" => Some(py_string_content(&t).trim().to_string()),
                "atom" if t == "false" => Some(String::new()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// An attribute, decorator, annotation or type spec written between a definition and its documentation.
fn is_attribute(lang: &str, p: Node, src: &[u8]) -> bool {
    match (lang, p.kind()) {
        (_, "attribute_item" | "decorator" | "annotation") | ("erlang", "spec") => true,
        ("elixir", "unary_operator") => elixir_attribute(p, src).is_some_and(|(n, _)| matches!(n.as_str(), "spec" | "impl" | "deprecated")),
        _ => false,
    }
}

/// A GDScript `class_name` holds the `extends` line after it, and Godot writes the class's
/// documentation below the two: a comment block there is the class's when no member follows it at once.
fn gdscript_header_doc(c: &mut Chunk, after: &[Node], src: &[u8]) {
    // 0-based row of the line right below what the chunk holds
    let mut next_row = c.end;
    let mut end = None;
    let mut rest = after;
    if let Some(e) = after.first().filter(|n| n.kind() == "extends_statement" && n.start_position().row as i64 == next_row) {
        (end, next_row, rest) = (Some(*e), last_row(*e) + 1, &after[1..]);
    }
    let mut block = Vec::new();
    let mut block_end = next_row;
    for n in rest {
        if n.kind() != "comment" || n.start_position().row as i64 != block_end {
            // a member right below the block owns it
            if n.start_position().row as i64 == block_end {
                block.clear();
            }
            break;
        }
        block.push(*n);
        block_end = last_row(*n) + 1;
    }
    if c.doc.is_empty() && !block.is_empty() {
        c.doc = clean_comment(&block.iter().map(|n| txt(src, n.start_byte(), n.end_byte())).collect::<Vec<_>>().join("\n"));
        end = block.last().copied();
    }
    let Some(end) = end else { return };
    c.end = last_row(end) + 1;
    if let Some(l) = c.lazy.as_mut() {
        l.end = end.end_byte();
    }
}

/// Position among `siblings` of the type signature Haskell and Elm write right above function `name`.
fn type_signature(lang: &str, name: &str, siblings: &[Node], at: usize, src: &[u8]) -> Option<usize> {
    let kind = match lang {
        "haskell" => "signature",
        "elm" => "type_annotation",
        _ => return None,
    };
    let i = at.checked_sub(1)?;
    let s = siblings[i];
    (s.kind() == kind && s.child_by_field_name("name").is_some_and(|n| txt(src, n.start_byte(), n.end_byte()) == name)).then_some(i)
}

/// Lisp forms: a string literal that follows the parameter list is documentation.
fn lisp_docstring(node: Node) -> Option<Node> {
    if !matches!(node.kind(), "list_lit" | "list" | "defun" | "function_definition" | "macro_definition") {
        return None;
    }
    let mut cur = node.walk();
    let kids: Vec<Node> = node.children(&mut cur).filter(|c| c.is_named()).collect();
    // (head name (params) "doc" ...) or defun > defun_header, "doc"
    let mut seen_params = false;
    for k in kids {
        if matches!(k.kind(), "defun_header") {
            seen_params = true;
            continue;
        }
        if matches!(k.kind(), "list_lit" | "list") && !seen_params {
            seen_params = true;
            continue;
        }
        if seen_params && matches!(k.kind(), "str_lit" | "string") {
            return Some(k);
        }
    }
    None
}

fn py_docstring_node<'t>(block: Node<'t>) -> Option<(Node<'t>, Node<'t>)> {
    let mut cursor = block.walk();
    for stmt in block.children(&mut cursor) {
        if stmt.kind() == "comment" {
            continue;
        }
        if stmt.kind() == "expression_statement" && stmt.named_child_count() == 1 {
            let s = stmt.named_child(0).unwrap();
            if matches!(s.kind(), "string" | "concatenated_string") {
                return Some((s, stmt));
            }
        }
        return None;
    }
    None
}

thread_local! {
    static PARSERS: std::cell::RefCell<HashMap<&'static str, Option<Parser>>> = std::cell::RefCell::new(HashMap::new());
}

fn with_parser<R>(spec: &'static LangSpec, f: impl FnOnce(&mut Parser) -> R) -> Option<R> {
    PARSERS.with(|p| {
        let mut p = p.borrow_mut();
        let parser = p.entry(spec.id).or_insert_with(|| {
            let mut parser = Parser::new();
            match parser.set_language(&(spec.language)()) {
                Ok(()) => Some(parser),
                Err(e) => {
                    eprintln!("[scrutate] grammar {} unusable: {e}", spec.id);
                    None
                }
            }
        });
        parser.as_mut().map(f)
    })
}

/// Module-level header chunk (module docstring / imports / constants) + fallbacks shared by all tiers.
/// A call outside every definition goes to the chunk that holds its line: the module chunk, else
/// the first window that does.
fn finish(path: &str, source: &str, mut chunks: Vec<Chunk>, module_doc: String, loose: Vec<(i64, String)>) -> Vec<Chunk> {
    let base = basename(path);
    let stem_name = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    };
    let first_def = chunks.iter().filter(|c| !SYMBOL_ONLY.contains(&c.kind)).map(|c| c.start).min();
    let lines = split_lines(source);
    let head_end = match first_def {
        Some(fd) => (fd - 1).max(0) as usize,
        None => lines.len(),
    }
    .min(60);
    let head_all = lines[..head_end.min(lines.len())].join("\n");
    // the lines each chunk's text keeps whole; constants and fields have no postings, so they keep none
    let mut spans = vec![(1, kept_lines(head_all.as_bytes(), usize::MAX).min(head_end) as i64)];
    for c in chunks.iter().filter(|c| !SYMBOL_ONLY.contains(&c.kind)) {
        let kept = match c.lazy {
            Some(l) => kept_lines(&source.as_bytes()[l.text.min(l.end)..l.end.min(source.len())], l.lines),
            None => kept_lines(c.text.as_bytes(), if CONTAINERS.contains(&c.kind) { MAX_CONTAINER_LINES } else { MAX_CHUNK_LINES }),
        };
        spans.push((c.start, c.end.min(c.start.saturating_add(kept.min(i64::MAX as usize) as i64 - 1))));
        // its documentation is searchable in its doc field
        spans.extend(c.doc_lines);
    }
    let defs: Vec<(i64, i64, &str)> = chunks.iter().filter(|c| !SYMBOL_ONLY.contains(&c.kind)).map(|c| (c.start, c.end, c.qualname.as_str())).collect();
    let mut windows = uncovered_windows(source, &lines, &spans, &defs, stem_name);
    let mut module_calls = BTreeSet::new();
    for (line, callee) in loose {
        if line <= head_end as i64 {
            module_calls.insert(callee);
            continue;
        }
        // window ends rise with their starts
        let w = windows.partition_point(|w| w.end < line);
        if let Some(w) = windows.get_mut(w).filter(|w| w.start <= line) {
            w.calls.insert(callee);
        }
    }
    let head_txt = capped(head_all);
    if !head_txt.trim().is_empty() {
        chunks.insert(
            0,
            Chunk {
                name: stem_name.to_string(),
                qualname: stem_name.to_string(),
                kind: "module",
                start: 1,
                end: head_end as i64,
                text: head_txt.clone(),
                sig: String::new(),
                doc: module_doc,
                body: head_txt,
                calls: module_calls,
                lazy: None,
                doc_lines: None,
            },
        );
    }
    // every line no chunk keeps is searchable as windows
    chunks.extend(windows);
    // constants and fields last: the index gives the chunks before them their lexical rows (they get none)
    chunks.sort_by_key(|c| SYMBOL_ONLY.contains(&c.kind));
    if chunks.is_empty() {
        return window_chunks(source);
    }
    chunks
}

// ------------------------------------------------------------------------- tier 1: grammars
/// (chunks, module docstring, imports as (line, module), calls outside every definition as (line, callee))
type GrammarOutput = (Vec<Chunk>, String, Vec<(i64, String)>, Vec<(i64, String)>);

/// A file's last grammar parse, kept in memory so the next version of the file is parsed
/// incrementally (tree-sitter reuses the unchanged subtrees).
pub struct PrevParse {
    lang: &'static str,
    source: String,
    tree: Tree,
}

impl PrevParse {
    pub fn source_len(&self) -> usize {
        self.source.len()
    }
}

/// Grammars reparsed incrementally: those without an external scanner. With a scanner (Python,
/// JavaScript, Rust, Elm, reStructuredText...) a reparse was seen to differ from a fresh parse.
const INCREMENTAL: &[&str] = &[
    "ada",
    "asm",
    "c",
    "commonlisp",
    "elisp",
    "glsl",
    "go",
    "graphql",
    "groovy",
    "java",
    "json",
    "llvm",
    "make",
    "objc",
    "pascal",
    "prisma",
    "puppet",
    "scheme",
    "solidity",
    "verilog",
    "zig",
];

fn point_at(s: &[u8], at: usize) -> Point {
    let line_start = memchr::memrchr(b'\n', &s[..at]).map(|i| i + 1).unwrap_or(0);
    Point { row: memchr::memchr_iter(b'\n', &s[..at]).count(), column: at - line_start }
}

/// The edit from `old` to `new` as one replaced range: common prefix and suffix are kept.
pub fn single_edit(old: &[u8], new: &[u8]) -> InputEdit {
    let pre = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let max_suf = old.len().min(new.len()) - pre;
    let suf = old.iter().rev().zip(new.iter().rev()).take(max_suf).take_while(|(a, b)| a == b).count();
    let (old_end, new_end) = (old.len() - suf, new.len() - suf);
    InputEdit {
        start_byte: pre,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point_at(new, pre),
        old_end_position: point_at(old, old_end),
        new_end_position: point_at(new, new_end),
    }
}

fn parse_tree(spec: &'static LangSpec, source: &str, prev: Option<PrevParse>) -> Option<Tree> {
    let src = source.as_bytes();
    let old = prev.filter(|p| p.lang == spec.id).map(|mut p| {
        p.tree.edit(&single_edit(p.source.as_bytes(), src));
        p.tree
    });
    let tree = with_parser(spec, |p| p.parse(src, old.as_ref()))??;
    if old.is_some() && tree.root_node().has_error() {
        // error recovery can settle differently when it starts from an old tree
        return with_parser(spec, |p| p.parse(src, None))?;
    }
    Some(tree)
}

fn grammar_chunks(spec: &'static LangSpec, path: &str, source: &str, prev: Option<PrevParse>) -> Option<(GrammarOutput, Option<PrevParse>)> {
    let src = source.as_bytes();
    if spec.defs.is_empty() && spec.calls.is_empty() && spec.imports.is_empty() && !spec.docstring && !crate::langs::CONSTANT_LANGS.contains(&spec.id) {
        // the walk would find nothing, and HTML's scanner takes time quadratic in the nesting depth
        return Some(((Vec::new(), String::new(), Vec::new(), Vec::new()), None));
    }
    let t = std::time::Instant::now();
    let tree = parse_tree(spec, source, prev)?;
    if crate::index::prof::enabled() {
        crate::index::prof::add(&crate::index::prof::TREESITTER, t);
    }
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut consts: Vec<Chunk> = Vec::new();
    let mut imports: Vec<(i64, String)> = Vec::new();
    // calls outside every definition, by line: the module chunk or a window takes them
    let mut loose: Vec<(i64, String)> = Vec::new();
    let mut def_stack: Vec<usize> = Vec::new();
    let mut heads: HashSet<usize> = HashSet::new();
    // ancestors in `heads`, so a call need not scan its ancestors
    let mut open_heads = 0usize;
    let mut stack: Vec<(Node, bool, bool, usize)> = vec![(tree.root_node(), false, false, 0)];
    let mut ancestors: Vec<(Node, Vec<Node>)> = Vec::new();
    let mut pos: Vec<usize> = Vec::new();
    // indexes in `ancestors` of those that are no `#if` or include guard: no scope of their own
    let mut scopes: Vec<usize> = Vec::new();
    let mut seps = Seps::new(source);
    // a GDScript file that declares `class_name X` is the class X, and its top-level definitions are X's members
    let file_class = (spec.id == "gdscript").then(|| tree.root_node()).and_then(|r| {
        let decl = r.named_children(&mut r.walk()).find(|c| c.kind() == "class_name_statement")?;
        decl.child_by_field_name("name").map(|n| txt(src, n.start_byte(), n.end_byte()))
    });
    while let Some((node, exiting, pushed, at)) = stack.pop() {
        if exiting {
            ancestors.pop();
            if scopes.last() == Some(&ancestors.len()) {
                scopes.pop();
            }
            pos.pop();
            open_heads -= usize::from(heads.contains(&node.id()));
            if pushed {
                def_stack.pop();
            }
            continue;
        }
        pos.push(at);
        let t = node.kind();
        if !spec.imports.is_empty() {
            imports_of(node, spec, src, &mut imports);
        }
        let mut matched: Option<(&'static str, String)> = None;
        for def in spec.defs {
            if def.node != t {
                continue;
            }
            if !def.value_kinds.is_empty() {
                let ok = node.child_by_field_name("value").map(|v| def.value_kinds.contains(&v.kind())).unwrap_or(false);
                if !ok {
                    continue;
                }
            }
            if !def.head_words.is_empty() && !def.head_words.contains(&head_word(node, src).as_str()) {
                continue;
            }
            if !def.needs_field.is_empty() && node.child_by_field_name(def.needs_field).is_none() {
                continue;
            }
            if !def.parents.is_empty() {
                let parent = scopes.iter().rev().map(|&i| ancestors[i].0.kind()).find(|k| !spec.outer.contains(k));
                if !parent.is_some_and(|k| def.parents.contains(&k)) {
                    continue;
                }
            }
            if let Some(name) = def_name(node, def.name, src) {
                let kind = if def.kind_by_head { kind_from_head(node, src).unwrap_or(def.kind) } else { def.kind };
                matched = Some((kind, name));
                break;
            }
        }
        let js = matches!(spec.id, "javascript" | "typescript" | "tsx");
        let this_member = js && t == "assignment_expression" && matches!(matched, Some(("method", _))) && crate::langs::js_this_member(node, src).is_some();
        if this_member && def_stack.is_empty() {
            matched = None;
        }
        if matched.is_none() && js {
            if let Some(kind) = crate::langs::js_default_export(node, src) {
                let name = node.child_by_field_name("name").map(|n| txt(src, n.start_byte(), n.end_byte()));
                matched = Some((kind, name.unwrap_or_else(|| default_export_name(path))));
            }
        }
        let mut pushed_now = false;
        if matches!(matched, Some(("key", _))) && def_stack.len() >= 2 {
            matched = None; // deeply nested keys of data files are searchable text, not symbols
        }
        if let Some((mut kind, name)) = matched {
            let file_owner = file_class.as_deref().filter(|_| def_stack.is_empty() && t != "class_name_statement");
            if kind == "function" {
                if let Some(&top) = def_stack.last() {
                    // a C++ struct body is a class body
                    if chunks[top].kind == "class" || spec.id == "cpp" && chunks[top].kind == "struct" {
                        kind = "method";
                    }
                } else if file_owner.is_some() {
                    kind = "method";
                }
            }
            let (prefix, name) = split_owner(spec.id, node, name, src);
            let mut parent_q = def_stack.last().map(|&i| chunks[i].qualname.as_str()).or(file_owner).unwrap_or_default();
            if this_member && def_stack.last().is_some_and(|&i| chunks[i].kind == "method") {
                // `this` in a method or a constructor is the method's owner
                parent_q = parent_q.rsplit_once('.').map_or("", |(owner, _)| owner);
            }
            let q = match &prefix {
                Some(p) => qualify(&qualify(parent_q, p), &name),
                None => qualify(parent_q, &name),
            };
            let mut outer = node;
            for (p, _) in ancestors.iter().rev() {
                if spec.outer.contains(&p.kind()) {
                    outer = *p;
                } else {
                    break;
                }
            }
            let mut lead = 0;
            if let Some(i) = ancestors.last().and_then(|(_, kids)| type_signature(spec.id, &name, kids, at, src)) {
                outer = ancestors.last().unwrap().1[i];
                lead = at - i;
            }
            let body_node = if spec.body.is_empty() { None } else { node.child_by_field_name(spec.body).or_else(|| objc_method_body(node)) };
            let sig_end = match body_node {
                Some(b) => b.start_byte(),
                None => node.end_byte().min(node.start_byte() + 200),
            };
            let (text_start, above) = attr_start(src, outer.start_byte(), attr_mark(spec.id));
            let sig = txt(src, text_start, sig_end).trim().to_string();
            let mut body_start = sig_end;
            let (mut doc, doc_lines) = match node.child_by_field_name("docstring").or_else(|| lisp_docstring(node)) {
                Some(ds) if !spec.docstring => (py_string_content(&txt(src, ds.start_byte(), ds.end_byte())), None),
                _ => leading_comment(spec.id, node, &ancestors, &pos, src, spec.wrappers, lead),
            };
            if doc.is_empty() && spec.id == "powershell" {
                // comment-based help may open the body instead of standing above it
                let mut cur = node.walk();
                let help = node.children(&mut cur).take_while(|c| c.kind() != "script_block").find(|c| c.kind() == "comment");
                let help = help.map(|c| txt(src, c.start_byte(), c.end_byte())).filter(|t| t.starts_with("<#"));
                doc = help.map(|t| clean_comment(&t.lines().map(str::trim_start).collect::<Vec<_>>().join("\n"))).unwrap_or_default();
            }
            if doc.is_empty() && spec.id == "elixir" && kind == "module" {
                let body = node.named_children(&mut node.walk()).find(|c| c.kind() == "do_block");
                doc = body.and_then(|b| b.named_children(&mut b.walk()).find_map(|c| elixir_doc(c, src, "moduledoc"))).unwrap_or_default();
            }
            if let Some((s, stmt)) = body_node.filter(|_| spec.docstring).and_then(py_docstring_node) {
                // the comment above comes first, then the docstring
                let ds = py_string_content(&txt(src, s.start_byte(), s.end_byte()));
                doc = if doc.is_empty() { ds } else { format!("{doc}\n{ds}") };
                body_start = stmt.end_byte();
            }
            let cap = if CONTAINERS.contains(&kind) { MAX_CONTAINER_LINES } else { MAX_CHUNK_LINES };
            chunks.push(Chunk {
                name,
                qualname: q,
                kind,
                start: (outer.start_position().row - above) as i64 + 1,
                end: last_row(node) + 1,
                sig,
                doc,
                lazy: Some(Lazy { text: text_start, body: body_start, end: node.end_byte(), lines: cap }),
                doc_lines,
                ..Default::default()
            });
            if let (true, Some((_, siblings))) = (spec.id == "asm" && t == "label", ancestors.last()) {
                let c = chunks.last_mut().unwrap();
                let last = asm_block(node, siblings, at, src, &mut c.calls);
                c.end = last_row(last) + 1;
                if let Some(l) = c.lazy.as_mut() {
                    l.end = last.end_byte();
                }
            }
            if let (true, Some((_, siblings))) = (spec.id == "gdscript" && t == "class_name_statement", ancestors.last()) {
                gdscript_header_doc(chunks.last_mut().unwrap(), &siblings[at + 1..], src);
            }
            def_stack.push(chunks.len() - 1);
            pushed_now = true;
            heads.extend(def_head(spec.id, node).map(|h| h.id()));
        } else if let Some((names, shown, kind)) = constant_of(spec.id, node, &ancestors, scopes.last().map(|&i| ancestors[i].0.kind()), src) {
            // not on def_stack: calls in the initializer stay with the enclosing definition
            let parent_q = def_stack.last().map(|&i| chunks[i].qualname.as_str()).or(file_class.as_deref()).unwrap_or_default();
            // the comment sits above the node the chunk shows: a Go `const` above its only spec
            let up = if shown == node { ancestors.len() } else { ancestors.iter().rposition(|(a, _)| a.id() == shown.id()).unwrap_or(ancestors.len()) };
            let doc = leading_comment(spec.id, shown, &ancestors[..up], &pos[..=up], src, spec.wrappers, 0).0;
            let text = head_bytes(src, shown.start_byte(), shown.end_byte(), MAX_CHUNK_LINES, MAX_CHUNK_BYTES);
            for name in names {
                // `const int Config::kSize = 2;` is owned by `Config`
                let (owner, name) = split_owner(spec.id, node, name, src);
                let parent_q = owner.map_or_else(|| parent_q.to_string(), |o| qualify(parent_q, &o));
                consts.push(Chunk {
                    qualname: qualify(&parent_q, &name),
                    name,
                    kind,
                    start: shown.start_position().row as i64 + 1,
                    end: last_row(shown) + 1,
                    text: text.clone(),
                    doc: doc.clone(),
                    ..Default::default()
                });
            }
        } else if let Some(call) = spec.calls.iter().find(|c| c.node == t) {
            let in_signature = ancestors.last().map(|p| matches!(p.0.kind(), "signature" | "function_signature" | "method_signature")).unwrap_or(false);
            let in_head = open_heads > 0 || heads.contains(&node.id());
            if !(in_signature || in_head) {
                let mut outside = BTreeSet::new();
                let calls = match def_stack.last() {
                    Some(&top) => &mut chunks[top].calls,
                    None => &mut outside,
                };
                let callee = match (spec.id, t, ancestors.last()) {
                    ("jsonnet", _, Some((_, siblings))) => sibling_callee(siblings, at, src),
                    ("vhdl", _, Some((p, _))) => vhdl_callee(node, *p, src),
                    _ => callee_name(spec.id, node, call.field, &mut seps),
                };
                if let Some(c) = callee.filter(|c| !call.skip.contains(&c.as_str())) {
                    calls.insert(c);
                }
                if t == "macro_invocation" && spec.id == "rust" {
                    macro_arg_calls(node, src, calls);
                }
                if t == "raw_text" && spec.id == "make" {
                    let raw = txt(src, node.start_byte(), node.end_byte());
                    calls.extend(MAKE_CALL.captures_iter(&raw).map(|m| m[1].to_string()));
                }
                let line = node.start_position().row as i64 + 1;
                loose.extend(outside.into_iter().map(|c| (line, c)));
            }
        }
        stack.push((node, true, pushed_now, at));
        let kids: Vec<Node> = node.children(&mut node.walk()).collect();
        stack.extend(kids.iter().enumerate().rev().map(|(i, &ch)| (ch, false, false, i)));
        if !t.starts_with("preproc_") {
            scopes.push(ancestors.len());
        }
        ancestors.push((node, kids));
        open_heads += usize::from(heads.contains(&node.id()));
    }
    let mut mdoc = String::new();
    if spec.docstring {
        if let Some((s, _)) = py_docstring_node(tree.root_node()) {
            mdoc = py_string_content(&txt(src, s.start_byte(), s.end_byte()));
        }
    }
    if spec.id == "vhdl" {
        let objects = vhdl_objects(tree.root_node(), src);
        for c in &mut chunks {
            c.calls.retain(|n| !objects.contains(&n.to_lowercase()));
        }
        loose.retain(|(_, n)| !objects.contains(&n.to_lowercase()));
    }
    // a tree with errors is not kept: the next parse would likely redo it from scratch anyway
    let keep = (!tree.root_node().has_error() && INCREMENTAL.contains(&spec.id)).then(|| PrevParse { lang: spec.id, source: source.to_string(), tree });
    chunks.extend(consts);
    Some(((chunks, mdoc, imports, loose), keep))
}

// ------------------------------------------------------------------------- tier 2: heuristics
const FUNC_WORDS: &[&str] = &[
    "def",
    "fn",
    "func",
    "function",
    "sub",
    "proc",
    "procedure",
    "method",
    "defn",
    "defun",
    "defmacro",
    "fun",
    "macro",
    "iterator",
    "template",
    "task",
    "subroutine",
    "defp",
    "lambda",
    "let",
    "val",
    "var",
    "const",
];
const TYPE_WORDS: &[(&str, &str)] = &[
    ("class", "class"),
    ("struct", "struct"),
    ("interface", "interface"),
    ("trait", "trait"),
    ("enum", "enum"),
    ("module", "module"),
    ("object", "class"),
    ("type", "type"),
    ("record", "struct"),
    ("protocol", "interface"),
    ("extension", "impl"),
    ("actor", "class"),
    ("contract", "class"),
    ("service", "class"),
    ("package", "module"),
    ("namespace", "module"),
    ("union", "struct"),
    ("typedef", "type"),
    ("entity", "class"),
    ("component", "class"),
    ("mixin", "trait"),
    ("impl", "impl"),
    ("instance", "impl"),
    ("data", "type"),
    ("newtype", "type"),
];
const MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "final",
    "abstract",
    "async",
    "export",
    "default",
    "inline",
    "virtual",
    "override",
    "unsafe",
    "extern",
    "pub",
    "local",
    "global",
    "my",
    "our",
    "open",
    "sealed",
    "partial",
    "declare",
    "native",
    "synchronized",
    "constexpr",
    "shared",
    "mut",
    "operator",
    "friend",
    "readonly",
    "lazy",
    "implicit",
    "suspend",
    "tailrec",
    "infix",
    "inner",
    "data",
    "case",
];
static DEF_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ \t]*(?:[A-Za-z_@][\w@.()<>\[\]]*[ \t]+){0,4}?([A-Za-z_]\w*)[ \t]+([A-Za-z_][\w.:$'!?-]*)").unwrap());
static SHELL_FN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]*([A-Za-z_][\w.:-]*)[ \t]*\([ \t]*\)[ \t]*\{?[ \t]*$").unwrap());
static LISP_DEF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]*\((def[a-z!*-]*|define(?:-[a-z-]+)?)[ \t]+\(?([^\s()]+)").unwrap());
static SQL_DEF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[ \t]*create[ \t]+(?:or[ \t]+replace[ \t]+)?(?:temp(?:orary)?[ \t]+)?(?:materialized[ \t]+)?(function|procedure|table|view|trigger|index|type|schema)[ \t]+(?:if[ \t]+not[ \t]+exists[ \t]+)?([\w.\x22`\[\]]+)").unwrap()
});

fn is_keyword(w: &str) -> bool {
    MODIFIERS.contains(&w) || FUNC_WORDS.contains(&w) || TYPE_WORDS.iter().any(|(t, _)| *t == w)
}

/// `line` without its leading modifier words (`private def x` -> `def x`); a modifier that is
/// followed by a name stays (`data Foo = ...`).
fn strip_modifiers(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some((w, after)) = rest.split_once([' ', '\t']) {
        let after = after.trim_start();
        let next = after.split([' ', '\t']).next().unwrap_or("");
        if !(MODIFIERS.contains(&w) && is_keyword(next)) {
            break;
        }
        rest = after;
    }
    rest
}

/// The words OCaml and F# allow between `let` and the name it binds.
const ML_MODIFIERS: &[&str] = &["rec", "mutable", "inline", "private", "public", "internal"];

/// The name `modifier` modifies in `rest`, and where it ends: `fact` in `let rec fact n`. After
/// `let` only OCaml's and F#'s own words modify (`let data x y` binds `data`); after another word
/// a modifier is a name unless another name with more after it follows (`proc inner()`).
fn modified<'a>(word: &str, modifier: &str, rest: &'a str) -> Option<(&'a str, usize)> {
    let ml = word == "let" && ML_MODIFIERS.contains(&modifier);
    if !ml && (word == "let" || !MODIFIERS.contains(&modifier)) {
        return None;
    }
    let body = rest.trim_start();
    if !body.starts_with(|c: char| c.is_alphabetic() || c == '_') {
        return None;
    }
    let len = body.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '\'')).unwrap_or(body.len());
    let after = body[len..].trim_start();
    if !ml && (after.is_empty() || after.starts_with(['=', '(', ':'])) {
        return None;
    }
    Some((&body[..len], rest.len() - body.len() + len))
}

fn heuristic_def(line: &str) -> Option<(&'static str, String)> {
    let line = strip_modifiers(line);
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with(['#', '/', '*', ';', '-', '%', '\'', '"', '<']) {
        return None;
    }
    if let Some(m) = SQL_DEF.captures(line) {
        let kind = match m[1].to_lowercase().as_str() {
            "function" | "procedure" | "trigger" => "function",
            "table" | "view" | "type" => "type",
            _ => "block",
        };
        return Some((kind, m[2].trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']').to_string()));
    }
    if let Some(m) = LISP_DEF.captures(line) {
        let kind = if m[1].starts_with("defmacro") {
            "macro"
        } else if m[1].contains("struct") || m[1].contains("class") || m[1].contains("record") {
            "struct"
        } else {
            "function"
        };
        return Some((kind, m[2].to_string()));
    }
    if let Some(m) = DEF_LINE.captures(line) {
        let (mut word, mut name, mut end) = (m.get(1).unwrap().as_str(), m.get(2).unwrap().as_str(), m.get(0).unwrap().end());
        while let Some((next, at)) = modified(word, name, &line[end..]) {
            // `let mutable x = 0` binds a value, as `val x = 0` does
            if name == "mutable" {
                word = "val";
            }
            (name, end) = (next, end + at);
        }
        // `let inline (+.)`: what follows is no name the tier can take
        if name.len() > 100 || (word == "let" && ML_MODIFIERS.contains(&name)) {
            return None;
        }
        if FUNC_WORDS.contains(&word) {
            // `let x = 5` is not a function: require a following `(` / `=` + lambda-ish for binding words
            if matches!(word, "let" | "val" | "var" | "const" | "lambda") {
                let rest = &line[end..];
                let r = rest.trim_start();
                // OCaml and F#: `let add x y =` takes parameters, `let limit = 5` does not
                let ml_params = word == "let"
                    && r.split_once('=')
                        .is_some_and(|(p, _)| !p.trim().is_empty() && p.split_whitespace().all(|w| w.starts_with(|c: char| c.is_alphabetic() || c == '_')));
                let is_fn = r.starts_with('(') || r.contains("=>") || r.contains("fun ") || r.contains("function") || r.contains("\\") || ml_params;
                if !is_fn {
                    return None;
                }
            }
            return Some(("function", name.trim_end_matches([':', '!']).to_string()));
        }
        if let Some((_, kind)) = TYPE_WORDS.iter().find(|(w, _)| *w == word) {
            return Some((kind, name.trim_end_matches([':', '{', '(']).to_string()));
        }
    }
    if let Some(m) = SHELL_FN.captures(line) {
        let name = &m[1];
        if !["if", "while", "for", "switch", "return", "else", "elif", "catch", "sizeof"].contains(&name) {
            return Some(("function", name.to_string()));
        }
    }
    None
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || t.starts_with('#')
        || t.starts_with("--")
        || t.starts_with(';')
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with('%')
        || t.starts_with("///")
}

/// Comment marks of the keyword tier beyond the shared ones, by file extension: `!`, `"`, `(*`
/// and `{` start code in other languages.
struct Marks<'a> {
    lines: &'a [&'a str],
    /// (opener, closer) of block comments whose inner lines may carry no mark
    blocks: &'a [(&'a str, &'a str)],
    /// fixed-form Fortran: `c` or `C` in the first column
    fixed: bool,
    /// the marks most languages share (`//`, `#`, `--`, `/* */` ...) count too; a custom language has only its own
    shared: bool,
}

fn heuristic_marks(path: &str) -> Marks<'static> {
    let ext = basename(path).rsplit_once('.').map_or("", |(_, e)| e);
    let (lines, blocks): (&'static [&'static str], &'static [(&'static str, &'static str)]) = match ext {
        "ml" | "mli" | "mll" | "mly" | "sml" | "sig" | "fs" | "fsi" | "fsx" => (&["(*"], &[("(*", "*)")]),
        "tla" => (&["(*", "\\*"], &[("(*", "*)")]),
        "pas" | "dpr" | "lpr" | "pp" => (&["(*", "{"], &[("(*", "*)"), ("{", "}")]),
        "f90" | "f95" | "f03" | "f08" | "F90" | "f" | "for" | "F" | "f77" | "ftn" => (&["!"], &[]),
        "vim" => (&["\""], &[]),
        "hs" | "lhs" | "elm" | "purs" | "idr" | "agda" | "dhall" => (&["{-"], &[("{-", "-}")]),
        "ps1" | "psm1" | "psd1" => (&["<#"], &[("<#", "#>")]),
        "lisp" | "cl" | "el" | "scm" | "ss" | "rkt" | "clj" => (&[], &[("#|", "|#")]),
        "nim" | "nims" => (&[], &[("#[", "]#")]),
        "d" | "di" => (&[], &[("/+", "+/")]),
        "m" => (&[], &[("%{", "%}")]),
        _ => (&[], &[]),
    };
    Marks { lines, blocks, fixed: matches!(ext, "f" | "for" | "F" | "f77" | "ftn"), shared: true }
}

fn is_marked(line: &str, m: &Marks) -> bool {
    let t = line.trim_start();
    let b = line.as_bytes();
    (m.shared && is_comment_line(line))
        || m.lines.iter().any(|k| t.starts_with(k))
        || m.fixed && matches!(b.first(), Some(b'c' | b'C')) && !b.get(1).is_some_and(|c| c.is_ascii_alphanumeric())
}

/// A block comment searched upward for its opener spans at most this many lines.
const MAX_BLOCK_LINES: usize = 200;

/// The first line of the comment block that ends right above line `at` (0-based, exclusive),
/// no higher than `floor`; `at` when there is none.
fn comment_top(lines: &[&str], floor: usize, mut at: usize, m: &Marks) -> usize {
    while at > floor {
        // the last line of a block comment: up to the line that opens it
        let t = lines[at - 1].trim();
        let shared: &[(&str, &str)] = if m.shared { &[("/*", "*/")] } else { &[] };
        let block = shared.iter().chain(m.blocks).find(|(o, c)| t.ends_with(c) && !t.starts_with(o));
        let opener = block.and_then(|&(open, close)| {
            let from = at.saturating_sub(MAX_BLOCK_LINES).max(floor);
            (from..at - 1).rev().take_while(|&j| !lines[j].contains(close)).find(|&j| lines[j].trim_start().starts_with(open))
        });
        match opener {
            Some(j) => at = j,
            None if is_marked(lines[at - 1], m) => at -= 1,
            None => break,
        }
    }
    at
}

/// Keyword-introduced definitions for languages without a grammar (or whose grammar found nothing).
pub fn heuristic_chunks(path: &str, source: &str) -> Vec<Chunk> {
    line_chunks(source, &heuristic_marks(path), heuristic_def, clean_comment)
}

/// The definitions of a custom language: lines its patterns match, outside its comments, with
/// the block below each as the keyword tier cuts it, and the comment above as the doc.
pub fn custom_chunks(lang: &crate::custom::Lang, source: &str) -> Vec<Chunk> {
    let (open, close) = lang.block_comment.as_ref().map_or(("", ""), |(o, c)| (o.as_str(), c.as_str()));
    let lines: Vec<&str> = lang.line_comment.iter().map(String::as_str).chain((!open.is_empty()).then_some(open)).collect();
    let blocks: Vec<(&str, &str)> = lang.block_comment.iter().map(|(o, c)| (o.as_str(), c.as_str())).collect();
    let marks = Marks { lines: &lines, blocks: &blocks, fixed: false, shared: false };
    let doc = |raw: &str| {
        let strip = |l: &str| {
            let mut t = l.trim();
            t = lines.iter().find_map(|m| t.strip_prefix(m)).unwrap_or(t);
            t = (!close.is_empty()).then(|| t.strip_suffix(close)).flatten().unwrap_or(t);
            t.trim().to_string()
        };
        raw.split('\n').map(strip).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
    };
    line_chunks(source, &marks, |l| if is_marked(l, &marks) { None } else { lang.definition(l) }, doc)
}

/// Chunks of the lines `def_of` names a definition: each block ends before the next definition
/// at the same or a shallower indentation, and `doc_of` cleans the comment lines above it.
fn line_chunks(source: &str, marks: &Marks, def_of: impl Fn(&str) -> Option<(&'static str, String)>, doc_of: impl Fn(&str) -> String) -> Vec<Chunk> {
    let lines = split_lines(source);
    let mut defs: Vec<(usize, &'static str, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some((kind, name)) = def_of(line) {
            defs.push((i, kind, name));
        }
    }
    if defs.is_empty() {
        return Vec::new();
    }
    let indents: Vec<usize> = defs.iter().map(|(i, ..)| indent_of(lines[*i])).collect();
    let mut out = Vec::new();
    for (di, (start, kind, name)) in defs.iter().enumerate() {
        // the block ends before the next definition at the same or a shallower indentation
        let end = (di + 1..defs.len()).find(|&j| indents[j] <= indents[di]).map_or(lines.len(), |j| defs[j].0);
        let cap = if CONTAINERS.contains(kind) { MAX_CONTAINER_LINES } else { MAX_CHUNK_LINES };
        // trailing blank/comment lines belong to the next definition
        let mut e = end;
        loop {
            while e > start + 1 && lines[e - 1].trim().is_empty() {
                e -= 1;
            }
            match comment_top(&lines, start + 1, e, marks) {
                top if top < e => e = top,
                _ => break,
            }
        }
        // a line past either cap for the text and for the body, so that `head` marks the cut
        let seg = &lines[*start..e.max(start + 1)];
        let mut bytes = 0;
        let body_lines = seg[1..]
            .iter()
            .take(cap + 1)
            .take_while(|l| {
                let before = bytes;
                bytes += l.len() + 1;
                before <= MAX_CHUNK_BYTES + 1
            })
            .count();
        let shown = &seg[..1 + body_lines];
        let text = head(&shown.join("\n"), cap);
        let body = head(&shown[1..].join("\n"), cap);
        // doc: comment lines directly above
        let d = comment_top(&lines, 0, *start, marks);
        let doc = if d < *start { doc_of(&lines[d..*start].join("\n")) } else { String::new() };
        let doc_lines = (!doc.is_empty()).then_some((d as i64 + 1, *start as i64));
        out.push(Chunk {
            name: name.clone(),
            qualname: name.clone(),
            kind,
            start: *start as i64 + 1,
            end: e.max(start + 1) as i64,
            text,
            sig: lines[*start].trim().to_string(),
            doc,
            body,
            calls: BTreeSet::new(),
            lazy: None,
            doc_lines,
        });
    }
    out
}

/// Indented tree of (kind, field, first line of text) for a file: how grammar rules get written.
pub fn sexp(spec: &'static LangSpec, source: &str) -> String {
    let src = source.as_bytes();
    let Some(Some(tree)) = with_parser(spec, |p| p.parse(src, None)) else {
        return "(parse failed)".into();
    };
    let mut out = String::new();
    let mut stack: Vec<(Node, usize, Option<&'static str>)> = vec![(tree.root_node(), 0, None)];
    while let Some((node, depth, field)) = stack.pop() {
        if !node.is_named() {
            continue;
        }
        let t = txt(src, node.start_byte(), node.end_byte());
        let first = t.lines().next().unwrap_or("");
        let first: String = first.chars().take(60).collect();
        out.push_str(&format!(
            "{}{}{} [{}-{}] {:?}\n",
            "  ".repeat(depth),
            field.map(|f| format!("{f}: ")).unwrap_or_default(),
            node.kind(),
            node.start_position().row + 1,
            node.end_position().row + 1,
            first
        ));
        let n = node.child_count();
        for i in (0..n).rev() {
            if let Some(ch) = node.child(i as u32) {
                stack.push((ch, depth + 1, node.field_name_for_child(i as u32)));
            }
        }
    }
    out
}

// ------------------------------------------------------------------------- main
/// Chunks and imports of one file (a custom language, else the grammar tier, then heuristics, then windows).
pub fn parse_file(path: &str, source: &str) -> Parsed {
    let mut parsed = parse_file_with(path, source, None).0;
    let mut budget = TextBudget::of(source.as_bytes());
    parsed.chunks.iter_mut().for_each(|c| c.fill(source.as_bytes(), &mut budget));
    parsed
}

/// `parse_file`, reusing the tree of the file's previous version when there is one; also returns
/// this version's tree for the next call. The result is the same as a fresh parse.
pub fn parse_file_with(path: &str, source: &str, prev: Option<PrevParse>) -> (Parsed, Option<PrevParse>) {
    let custom = crate::custom::current();
    if let Some(lang) = custom.for_path(path) {
        return (Parsed { chunks: finish(path, source, custom_chunks(lang, source), String::new(), Vec::new()), imports: Vec::new() }, None);
    }
    if let Some(spec) = spec_for(path, source) {
        if let Some(((chunks, mdoc, imports, loose), tree)) = grammar_chunks(spec, path, source, prev) {
            // a parsed file is the grammar's verdict even when it holds no definitions: the
            // keyword heuristics would invent symbols from prose ("namespace so the ...")
            return (Parsed { chunks: finish(path, source, chunks, mdoc, loose), imports }, tree);
        }
    }
    // prose has keyword-looking lines ("module will be called foo" in Kconfig help text)
    let base = basename(path);
    let prose = base.starts_with("Kconfig") || base.to_ascii_lowercase().ends_with(".txt");
    let chunks = if prose { Vec::new() } else { heuristic_chunks(path, source) };
    (Parsed { chunks: finish(path, source, chunks, String::new(), Vec::new()), imports: Vec::new() }, None)
}

pub fn parse_source(path: &str, source: &str) -> Vec<Chunk> {
    parse_file(path, source).chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The callee rule as it was before it worked on offsets: it copies the target's whole text.
    fn callee_name_by_whole_text(lang: &str, node: Node, field: &str, src: &[u8]) -> Option<String> {
        let mut target =
            if field.is_empty() { None } else { node.child_by_field_name(field).or_else(|| node.children(&mut node.walk()).find(|c| c.kind() == field)) };
        if target.is_none() && node.named_child_count() > 0 {
            target = node.named_child(0);
        }
        let mut target = target?;
        match (lang, node.kind()) {
            // `$(call g,1)` calls `g`; `$(subst ...)` and the other built-ins are no calls
            ("make", "function_call") => {
                let f = node.child_by_field_name("function")?;
                if txt(src, f.start_byte(), f.end_byte()) != "call" {
                    return None;
                }
                let args = txt(src, target.start_byte(), target.end_byte());
                return IDENT_START.find(args.split(',').next()?.trim()).map(|m| m.as_str().to_string());
            }
            ("make", "raw_text") => return None,
            ("verilog", "data_declaration") if !verilog_typeless_call(node, target, src) => return None,
            _ => {}
        }
        if target.kind() == "method_index_expression" {
            // Lua `obj:meth(x)` calls `meth`
            target = target.child_by_field_name("method")?;
        }
        if target.kind() == "generic_function" {
            // `f::<T>(x)`: the name is before the turbofish
            target = target.child_by_field_name("function")?;
        }
        if target.kind() == "extract_operator" {
            // R `obj$meth(x)` calls `meth`
            target = target.child_by_field_name("rhs")?;
        }
        if matches!(lang, "scheme" | "racket" | "elisp" | "commonlisp") {
            // the symbol ends at a space or a bracket: a deep nested form's whole text is not copied
            let s = &src[target.start_byte().min(src.len())..target.end_byte().min(src.len())];
            let lead = s.iter().take_while(|b| b.is_ascii_whitespace()).count();
            let end = s[lead..].iter().position(|b| b.is_ascii_whitespace() || b"()[]{}\"'`,;".contains(b)).map_or(s.len(), |i| lead + i);
            let t = String::from_utf8_lossy(&s[..end]);
            // an operator such as `-` or `<=` is not a name; a Common Lisp package prefix is not part of it
            let name = LISP_SYMBOL.find(t.trim())?.as_str();
            let name = if lang == "commonlisp" { name.rsplit(':').next().unwrap_or(name) } else { name };
            return name.chars().any(char::is_alphabetic).then(|| name.to_string());
        }
        let mut t = txt(src, target.start_byte(), target.end_byte());
        if lang == "cpp" {
            // `a.operator+(b)` parses as a call of `a` with `.operator+` in an error node
            let lost = node.children(&mut node.walk()).find(|c| c.is_error()).map(|c| txt(src, c.start_byte(), c.end_byte()));
            if let Some(name) = cpp_operator_or_destructor_by_whole_text(lost.as_deref().filter(|l| l.contains("operator")).unwrap_or(&t)) {
                return Some(name);
            }
        }
        // D and Solidity `new Point(1, 2)` is a call whose callee is `new Point`
        if let Some(rest) = t.strip_prefix("new").filter(|r| r.starts_with(char::is_whitespace)) {
            t = rest.trim_start().to_string();
        }
        // a dot is part of an R name and `::` part of a Bash one
        let seps: &[&str] = match lang {
            "r" => &["::", "->"],
            "bash" => &["->"],
            "php" => &["::", ".", "->", "\\"],
            _ => &["::", ".", "->"],
        };
        for sep in seps {
            if let Some(i) = t.rfind(sep) {
                t = t[i + sep.len()..].to_string();
            }
        }
        let t = t.trim().trim_start_matches('@');
        // a `!` is cut for a Rust macro; in Ruby, Elixir and Julia it ends the name
        let t = if matches!(lang, "ruby" | "elixir" | "julia") { t } else { t.trim_end_matches('!') };
        // a JavaScript private method is named with its `#`
        if let Some(private) = t.strip_prefix('#').filter(|_| matches!(lang, "javascript" | "typescript" | "tsx")) {
            return IDENT_START.find(private).map(|m| format!("#{}", m.as_str()));
        }
        let t = if lang == "rust" { t.strip_prefix("r#").unwrap_or(t) } else { t };
        // Julia `==(a, b)` and `Base.:(==)(a, b)` call the operator a method definition names
        let op = t.trim_start_matches(':').trim_start_matches('(').trim_end_matches(')');
        if lang == "julia" && !op.is_empty() && op.chars().all(|c| c.is_ascii_punctuation()) {
            return Some(op.to_string());
        }
        name_rule(lang).find(t).map(|m| m.as_str().to_string())
    }

    fn cpp_operator_or_destructor_by_whole_text(t: &str) -> Option<String> {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        if let Some(i) = t.rfind("operator").filter(|&i| !t[..i].ends_with(word)) {
            let op: String = t[i + "operator".len()..].split_whitespace().collect();
            if !op.is_empty() && op.chars().all(|c| c.is_ascii_punctuation()) {
                return Some(format!("operator{op}"));
            }
        }
        let tail = t.rsplit(['.', ':', '>']).next()?.trim();
        IDENT_START.find(tail.strip_prefix('~')?).map(|m| format!("~{}", m.as_str()))
    }

    #[test]
    fn python_chunks() {
        let src = "\"\"\"Mod doc.\"\"\"\nimport os\n\n\nclass A:\n    \"\"\"Doc A.\"\"\"\n\n    def m(self, x):\n        '''Do m.\n\n        more'''\n        return os.path.join(x)\n\n\ndef f():\n    return A().m(1)\n";
        let ch = parse_source("a.py", src);
        let names: Vec<&str> = ch.iter().map(|c| c.qualname.as_str()).collect();
        assert_eq!(names, vec!["a", "A", "A.m", "f"]);
        assert_eq!(ch[0].kind, "module");
        assert_eq!(ch[0].doc, "Mod doc.");
        assert_eq!(ch[2].kind, "method");
        assert_eq!(ch[2].doc, "Do m.\n\nmore");
        assert_eq!(ch[2].sig, "def m(self, x):");
        assert!(ch[2].calls.contains("join"));
        assert!(ch[3].calls.contains("A") && ch[3].calls.contains("m"));
        assert_eq!((ch[1].start, ch[1].end), (5, 12));
    }

    #[test]
    fn cleandoc_like_python() {
        assert_eq!(cleandoc("\n    Hello\n      world\n    "), "Hello\n  world");
        assert_eq!(cleandoc("  First line\n\n    indented"), "First line\n\nindented");
    }

    #[test]
    fn js_leading_comment_and_arrow() {
        let src = "// Adds things.\n// Really.\nexport const add = (a, b) => a + b;\n\n/**\n * Greets.\n */\nfunction greet(n) { return hi(n); }\n";
        let ch = parse_source("x.js", src);
        let g = ch.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(g.doc, "Greets.");
        assert!(g.calls.contains("hi"));
        let a = ch.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(a.doc, "Adds things.\nReally.");
    }

    #[test]
    fn windows() {
        let src: String = (0..120).map(|i| format!("line {i}\n")).collect();
        let w = window_chunks(&src);
        assert_eq!(w.len(), 3);
        assert_eq!((w[0].start, w[0].end), (1, 50));
        assert_eq!((w[1].start, w[1].end), (41, 90));
        assert_eq!((w[2].start, w[2].end), (81, 120));
    }

    #[test]
    fn imports_are_extracted_and_normalised() {
        let py = parse_file("i.py", "import os.path\nfrom a.b import c as d\nimport x, y\nfrom . import z\n");
        let mods: Vec<&str> = py.imports.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(mods, vec!["os.path", "a.b", "x", "y", "."]);
        let js = parse_file("i.js", "import {a} from \"./mod\";\nconst b = require(\"fs\");\nexport {c} from \"../c\";\n");
        assert_eq!(js.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["./mod", "fs", "../c"]);
        let rs = parse_file("i.rs", "use std::fmt;\nuse crate::lex::{a, b};\n");
        assert_eq!(rs.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["std::fmt", "crate::lex"]);
        let c = parse_file("i.c", "#include <stdio.h>\n#include \"x.h\"\n");
        assert_eq!(c.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["stdio.h", "x.h"]);
        let rb = parse_file("i.rb", "require \"json\"\nrequire_relative \"lib/x\"\nputs \"no\"\n");
        assert_eq!(rb.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["json", "lib/x"]);
        let go = parse_file("i.go", "package main\nimport (\n  \"fmt\"\n  m \"github.com/x/y\"\n)\n");
        assert_eq!(go.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["fmt", "github.com/x/y"]);
        let java = parse_file("I.java", "import java.util.List;\nimport static java.lang.Math.max;\n");
        assert_eq!(java.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["java.util.List", "java.lang.Math.max"]);
        let hs = parse_file("i.hs", "import qualified Data.Map as M\nimport Data.List (sort)\n");
        assert_eq!(hs.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["Data.Map", "Data.List"]);
    }

    #[test]
    fn heuristics_find_keyword_definitions() {
        let src = "# A perl-ish file\nuse strict;\n\n# Says hello.\nsub greet {\n    print \"hi\";\n}\n\npackage Foo::Bar;\n\nsub new {\n    return bless {}, shift;\n}\n";
        let ch = parse_source("x.pl", src);
        let names: Vec<(&str, &str)> = ch.iter().map(|c| (c.kind, c.name.as_str())).collect();
        assert_eq!(names, vec![("module", "x"), ("function", "greet"), ("module", "Foo::Bar"), ("function", "new")]);
        assert_eq!(ch[1].doc, "Says hello.");
        let sql = "CREATE OR REPLACE FUNCTION add_one(i int) RETURNS int AS $$ SELECT i + 1 $$;\n\ncreate table if not exists users (\n  id int\n);\n";
        let ch = parse_source("q.sql", sql);
        let names: Vec<(&str, &str)> = ch.iter().filter(|c| c.kind != "module").map(|c| (c.kind, c.name.as_str())).collect();
        assert_eq!(names, vec![("function", "add_one"), ("type", "users")]);
        let sh = "my_func() {\n  echo hi\n}\n";
        let ch = parse_source("x.unknownext", sh);
        assert!(ch.iter().any(|c| c.name == "my_func" && c.kind == "function"));
        let plain = "just some prose\nwithout definitions\n";
        assert!(parse_source("notes.unknownext", plain).iter().all(|c| c.kind == "module"));
    }

    /// The keyword chunker (OCaml and F# have no grammar in the default build): `rec`, `mutable`,
    /// `inline` and `private` before a name modify it, and a function may be named after a
    /// modifier word when nothing follows that could be the name (`let data x`, `proc inner()`).
    #[test]
    fn heuristics_skip_modifiers_before_a_name_and_keep_a_name_that_is_a_modifier_word() {
        let src = "let rec fact n =\n  if n = 0 then 1 else n * fact (n - 1)\n\nlet inline square x = x * x\n\nlet private helper y =\n  y + 1\n\nlet mutable count = 0\n\nlet f x =\n  x\n\nlet data x = x\n\nproc inner() =\n  discard\n";
        let ch = parse_source("f.unknownext", src);
        let names: Vec<(&str, &str, i64)> = ch.iter().filter(|c| c.kind != "module").map(|c| (c.kind, c.name.as_str(), c.start)).collect();
        assert_eq!(
            names,
            vec![
                ("function", "fact", 1),
                ("function", "square", 4),
                ("function", "helper", 6),
                ("function", "f", 11),
                ("function", "data", 14),
                ("function", "inner", 16)
            ]
        );
        assert_eq!(ch.iter().find(|c| c.name == "f").map(|c| c.end), Some(12));
        // after `let` only OCaml's and F#'s own words modify: `data` with two parameters is a
        // name, and an operator after `inline` is no definition the tier can name
        let ml = "let data x y = x + y\n\nlet inline (+.) a b = a + b\n\nlet rec fact n = n\n\nlet private helper z = z\n";
        let ch = parse_source("g.unknownext", ml);
        let names: Vec<(&str, i64)> = ch.iter().filter(|c| c.kind != "module").map(|c| (c.name.as_str(), c.start)).collect();
        assert_eq!(names, vec![("data", 1), ("fact", 5), ("helper", 7)]);
    }

    #[test]
    fn form_feeds_and_lone_cr_do_not_shift_line_numbers() {
        // found by fuzzing: Python-style splitlines counted \x0c as a line break
        for c in parse_source("f.ts", "\x0c\x0c\x04") {
            assert!(c.start == 1 && c.end == 1, "{}..{}", c.start, c.end);
        }
        let src = "x = 1\x0c\n\x0c\ndef f():\r    pass\n";
        let total = src.lines().count() as i64;
        for c in parse_source("g.py", src) {
            assert!(c.end <= total, "{} {}..{} of {total}", c.name, c.start, c.end);
        }
    }

    type Difference = (String, Option<String>, Option<String>);

    /// Names the offset rule gives that differ from the whole-text rule, for every node of `source`
    /// handed to it with each call field of the language and with none, and how many names they share.
    fn callee_differences(path: &str, source: &str) -> (usize, Vec<Difference>) {
        let Some(spec) = spec_for(path, source) else { return (0, Vec::new()) };
        let tree = parse_tree(spec, source, None).unwrap();
        let mut fields: Vec<&str> = spec.calls.iter().map(|c| c.field).collect();
        fields.push("");
        let (mut seps, mut same, mut diffs) = (Seps::new(source), 0, Vec::new());
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            stack.extend(node.children(&mut node.walk()));
            for &field in &fields {
                let (new, old) = (callee_name(spec.id, node, field, &mut seps), callee_name_by_whole_text(spec.id, node, field, source.as_bytes()));
                if new != old {
                    diffs.push((format!("{path} {}", txt(source.as_bytes(), node.start_byte(), node.end_byte())), old, new));
                } else {
                    same += usize::from(new.is_some());
                }
            }
        }
        (same, diffs)
    }

    #[test]
    fn callee_names_by_offsets_are_the_names_of_the_whole_text() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let mut dirs = vec![dir.join("samples")];
        if cfg!(feature = "extra-langs") {
            dirs.push(dir.join("samples_extra"));
        }
        let mut files: Vec<(String, String)> = Vec::new();
        for d in dirs {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                files.push((p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read_to_string(&p).unwrap()));
            }
        }
        let chain = |head: &str, link: &str| format!("{head}{}", link.repeat(40));
        let shapes: Vec<(&str, String)> = vec![
            ("a.js", format!("{};\n{};\n{};\nnew X(1);\nnew  \n Y.Z(2);\na(b.c)();\na.b(c.d)(e.f);\nthis.#p();\nthis.#q.r();\nobj?.m();\nnaïve.mé();\n$a.$b();\nx.();\nnew ();\n(a.b)();\n", chain("a", "()"), chain("x", ".a()"), chain("x", "?.a()"))),
            ("a.ts", format!("{};\nf<T>(x);\na.b<T>().c();\n", chain("x", ".a()"))),
            ("a.py", format!("{}\n{}\nf(b.c)()\nnaïve()\nx.y.z(1)\n", chain("a", "()"), chain("x", ".a()"))),
            ("a.rs", format!("fn f() {{ {}; {}; a::b::c(); r#match(); println!(\"x\"); f::<T>(x); a:::b(); self.f()(); a::<T>::b(); x.r#try(); }}\n", chain("a", "()"), chain("x", ".a()"))),
            ("a.go", format!("package a\nfunc f() {{ {}; {}; fmt.Println(1); a.b.c() }}\n", chain("a", "()"), chain("x", ".a()"))),
            ("A.java", format!("class A {{ void f() {{ {}; new B(); a.b.c(); this.<T>m(); }} }}\n", chain("x", ".a()"))),
            ("a.cpp", format!("void f() {{ {}; {}; a.operator+(b); p->~Shape(); x.operator()(1); operator<<(o, x); a::b::c(); p->q->r(); A::~A(); x.operator==(y); std::operator<<(o, x); a::operator new(1); x.template f<T>(); }}\n", chain("a", "()"), chain("x", ".a()"))),
            ("b.cpp", "void f() { a.operator()()()(); p->operator->*(q); x.operator<=>(y); x.operator  () (1); a.operator[](1)(2); }\n".to_string()),
            ("a.c", format!("void f() {{ {}; p->q(); s.f(); }}\n", chain("a", "()"))),
            ("a.d", "void f() { auto p = new Point(1, 2); a.b(); }\n".to_string()),
            ("a.sol", "contract C { function f() public { new D(1); a.b(); } }\n".to_string()),
            ("Makefile", "x := $(call f,$(call g,1),2)\ny := $(call  h ,x)\nz := $(call\tk)\n$(info $(call m))\nw := $(subst a,b,$(call n,1))\nv := $(call , x)\n".to_string()),
            ("a.php", format!("<?php\n\\A\\b();\nA::b();\n$a->b();\n{};\na\\b\\c();\nA::b()::c();\n$a::b();\n", chain("$a", "->b()"))),
            ("a.r", format!("a$b()\na.b()\npkg::f()\n{}\nx@y()\n", chain("x", "$a()"))),
            ("a.sh", "a::b\nfoo-bar x\na.b c\n$x y\n".to_string()),
            ("a.lua", format!("o:m()\na.b.c()\n{}\n", chain("o", ":m()"))),
            ("a.lisp", "(foo (bar 1))\n(pkg:baz)\n(- 1 2)\n(list->vec x)\n(cl-user::f)\n".to_string()),
            ("a.scm", "(foo (bar 1))\n(- 1 2)\n(list->vec x)\n(set-car! a b)\n(empty? x)\n".to_string()),
            ("a.el", "(foo (bar 1))\n(my-f x)\n".to_string()),
            ("a.jl", format!("==(a, b)\nBase.:(==)(a, b)\nf!(x)\nx.f()\n{}\n+(1, 2)\n", chain("a", "()"))),
            ("a.rb", format!("a.b?\nx.y!\nfoo(1)\n@a.b\n{}\nA::B.c\n", chain("x", ".a"))),
            ("a.ex", "a.b?()\nFoo.bar!(1)\nfoo(1)\n".to_string()),
            ("a.kt", format!("fun f() {{ {}; a?.b() }}\n", chain("x", ".a()"))),
            ("a.swift", format!("{}\n", chain("x", ".a()"))),
            ("a.cs", format!("class A {{ void F() {{ {}; new B(); }} }}\n", chain("x", ".A()"))),
            ("a.hs", "f = g x\nh = A.b c\n".to_string()),
            ("a.ml", "let f = g x\nlet h = A.b c\n".to_string()),
        ];
        let (mut same, mut diffs) = (0, Vec::new());
        for (path, source) in files.iter().map(|(p, s)| (p.as_str(), s.clone())).chain(shapes) {
            let (n, d) = callee_differences(path, &source);
            same += n;
            diffs.extend(d);
        }
        assert!(same > 9_000, "{same} names compared");
        // a C++ operator name has at most three marks: text after `operator` with more is no operator's name
        let (window, others): (Vec<Difference>, Vec<Difference>) = diffs.into_iter().partition(|(_, old, new)| {
            old.as_deref().and_then(|o| o.strip_prefix("operator")).is_some_and(|op| op.len() > 3) && new.as_deref() == Some("operator")
        });
        assert!(others.is_empty(), "{others:#?}");
        assert!(!window.is_empty());
    }
}
