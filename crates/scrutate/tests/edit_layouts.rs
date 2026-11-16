//! `edit_symbol` keeps every definition outside the one it edits, whatever shares its lines: files
//! built from small definition shapes laid out on shared and separate lines in random orders, a
//! random target and a random replacement. Either the edit is refused and the file is byte for
//! byte as it was, or every byte outside the replaced lines is as it was and every other
//! definition's text is still there, in order.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use scrutate::session::{Every, Session};
use serde_json::json;

/// A definition as a file holds it, and the names an edit can target in it: each with the lines
/// that replace its own, and its own text in the piece, which starts its first replaced line.
struct Piece {
    text: String,
    targets: Vec<(String, String, String)>,
}

fn one(text: String, name: &str, new: String) -> Piece {
    Piece { targets: vec![(name.to_string(), new + "\n", text.clone())], text }
}

/// A piece of two names, each a target whose own text is the whole piece.
fn both(text: String, (a, a_new): (String, String), (b, b_new): (String, String)) -> Piece {
    Piece { targets: vec![(a, a_new, text.clone()), (b, b_new, text.clone())], text }
}

/// The shapes of one language, `c` and `i` naming them apart (every name is the only one of its
/// spelling in the repository) and `v` their value.
fn shape(lang: &str, k: usize, c: usize, i: usize, v: u64) -> Piece {
    let (n, w) = (format!("a{c}_{i}"), v + 100);
    let cap = format!("A{c}_{i}");
    match (lang, k) {
        ("go", 0) => one(format!("const {cap} = {v}"), &cap, format!("const {cap} = {w}")),
        ("go", 1) => one(format!("var {n} = {v}"), &n, format!("var {n} = {w}")),
        ("go", 2) => one(format!("func {cap}() int {{ return {v} }}"), &cap, format!("func {cap}() int {{ return {w} }}")),
        ("go", 3) => one(format!("func {cap}() int {{\n\treturn {v}\n}}"), &cap, format!("func {cap}() int {{\n\treturn {w}\n}}")),
        ("go", 4) => one(format!("type {cap} int // the {i}"), &cap, format!("type {cap} int64 // the {i}")),
        ("go", 5) => Piece {
            text: format!("const (\n\t{cap} = {v}\n\t{cap}b = {v}\n)"),
            targets: vec![
                (cap.clone(), format!("\t{cap} = {w}\n"), format!("\t{cap} = {v}")),
                (format!("{cap}b"), format!("\t{cap}b = {w}\n"), format!("\t{cap}b = {v}")),
            ],
        },
        ("go", _) => both(
            format!("var {n}, {n}b = {v}, {v}"),
            (n.clone(), format!("var {n}, {n}b = {w}, {v}\n")),
            (format!("{n}b"), format!("var {n}, {n}b = {v}, {w}\n")),
        ),
        ("javascript" | "typescript", 0) => one(format!("const {n} = {v};"), &n, format!("const {n} = {w};")),
        ("javascript" | "typescript", 1) => one(format!("function {n}() {{ return {v} }}"), &n, format!("function {n}() {{ return {w} }}")),
        ("javascript" | "typescript", 2) => one(format!("function {n}() {{\n  return {v}\n}}"), &n, format!("function {n}() {{\n  return {w}\n}}")),
        ("javascript" | "typescript", 3) => {
            let text = format!("class {cap} {{ f = {v}; m{c}_{i}() {{ return {v} }} }}");
            Piece {
                targets: vec![
                    (cap.clone(), format!("class {cap} {{ f = {w}; m{c}_{i}() {{ return {v} }} }}\n"), text.clone()),
                    (format!("m{c}_{i}"), format!("m{c}_{i}() {{ return {w} }}\n"), format!("m{c}_{i}() {{ return {v} }}")),
                ],
                text,
            }
        }
        ("javascript" | "typescript", 4) => both(
            format!("const {n} = {v}, {n}b = {v};"),
            (n.clone(), format!("const {n} = {w}, {n}b = {v};\n")),
            (format!("{n}b"), format!("const {n} = {v}, {n}b = {w};\n")),
        ),
        ("typescript", 5) => one(format!("type {cap} = {v};"), &cap, format!("type {cap} = {w};")),
        ("javascript" | "typescript", _) => one(format!("let {n} = {v}; // {i}"), &n, format!("let {n} = {w}; // {i}")),
        ("rust", 0) => one(format!("const {cap}: u8 = {};", v % 200), &cap, format!("const {cap}: u8 = {};", w % 200)),
        ("rust", 1) => one(format!("static {cap}: u8 = {};", v % 200), &cap, format!("static {cap}: u8 = {};", w % 200)),
        ("rust", 2) => one(format!("fn {n}() -> u64 {{ {v} }}"), &n, format!("fn {n}() -> u64 {{ {w} }}")),
        ("rust", 3) => one(format!("fn {n}() -> u64 {{\n    {v}\n}} // {i}"), &n, format!("fn {n}() -> u64 {{\n    {w}\n}} // {i}")),
        ("rust", _) => one(format!("struct {cap}(u{});", [8, 16, 32][v as usize % 3]), &cap, format!("struct {cap}(u64);")),
        ("python", 0) => one(format!("{cap} = {v}"), &cap, format!("{cap} = {w}")),
        ("python", 1) => one(format!("{cap} = {v}  # {i}"), &cap, format!("{cap} = {w}  # {i}")),
        ("python", _) => one(format!("def {n}():\n    return {v}"), &n, format!("def {n}():\n    return {w}")),
        ("c", 0) => one(format!("const int {n} = {v};"), &n, format!("const int {n} = {w};")),
        ("c", 1) => one(format!("int {n}(void) {{ return {v}; }}"), &n, format!("int {n}(void) {{ return {w}; }}")),
        ("c", 2) => one(format!("int {n}(void) {{\n  return {v};\n}}"), &n, format!("int {n}(void) {{\n  return {w};\n}}")),
        ("c", 3) => both(
            format!("const int {n} = {v}, {n}b = {v};"),
            (n.clone(), format!("const int {n} = {w}, {n}b = {v};\n")),
            (format!("{n}b"), format!("const int {n} = {v}, {n}b = {w};\n")),
        ),
        (_, _) => one(format!("#define {cap} {v}"), &cap, format!("#define {cap} {w}")),
    }
}

const LANGS: [(&str, &str, usize, &str, &str); 6] = [
    ("go", "go", 7, "package p\n\n", "; "),
    ("javascript", "js", 6, "", " "),
    ("typescript", "ts", 7, "", " "),
    ("rust", "rs", 5, "", " "),
    ("python", "py", 3, "", "; "),
    ("c", "c", 5, "", " "),
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// One case: a file, the name to edit and the new text.
struct Case {
    file: String,
    text: String,
    pieces: Vec<Piece>,
    /// where each piece starts in `text`
    offsets: Vec<usize>,
    target: (usize, usize),
}

impl Case {
    fn name(&self) -> &str {
        &self.pieces[self.target.0].targets[self.target.1].0
    }

    fn new_text(&self) -> &str {
        &self.pieces[self.target.0].targets[self.target.1].1
    }

    /// The bytes of the lines the edit must replace: the target's own text, whole lines.
    fn lines(&self) -> (usize, usize) {
        let (t, j) = self.target;
        let own = &self.pieces[t].targets[j].2;
        let at = self.offsets[t] + self.pieces[t].text.find(own.as_str()).unwrap();
        let a = self.text[..at].rfind('\n').map_or(0, |i| i + 1);
        let end = at + own.len();
        let b = self.text[end..].find('\n').map_or(self.text.len(), |i| end + i + 1);
        (a, b)
    }
}

/// The Auditor's inputs (six refused, then those of a constant on line 1, applied), the edits that
/// change what a definition holds or are a file's whole text (applied), then `n` generated ones.
/// Rows of the fixed list: (language, file, name, new text, the target's own old text).
const FIXED: [(&str, &str, &str, &str, &str); 15] = [
    ("go", "package p\n\nconst A = 1; const B = 2\n\nfunc G() int { return B }\n", "A", "const A = 5\n", "const A"),
    ("go", "package p\n\nconst A = 1; const B = 2\n\nfunc G() int { return B }\n", "B", "const B = 5\n", "const B"),
    ("go", "package p\n\nconst A = 1; func G() int { return A }\n", "A", "const A = 5\n", "const A"),
    ("go", "package p\n\nvar A = 1; var B = 2\n", "A", "var A = 5\n", "var A"),
    ("go", "package p\n\nconst ( A = 1; B = 2 )\n", "B", "const B = 5\n", "B = 2"),
    ("go", "package p\n\nconst A = 1; const B = 2 // two\n", "A", "const A = 5\n", "const A"),
    ("javascript", "const a = 1;\nconst b = 2;\n", "a", "const a = 5;\n", "const a = 1;"),
    ("typescript", "const a = 1;\nconst b = 2;\n", "a", "const a = 5;\n", "const a = 1;"),
    ("python", "A = 1\nB = 2\n", "A", "A = 5\n", "A = 1"),
    ("rust", "pub const A: u8 = 1;\npub const B: u8 = 2;\n", "A", "pub const A: u8 = 5;\n", "pub const A: u8 = 1;"),
    ("c", "const int a = 1;\nconst int b = 2;\n", "a", "const int a = 5;\n", "const int a = 1;"),
    ("javascript", "class C {\n  m() { return 1 }\n}\n", "C", "class C {\n  m() { return 2 }\n}\n", "class C {\n  m() { return 1 }\n}"),
    (
        "python",
        "class K:\n    def m(self):\n        return 1\n",
        "K",
        "class K:\n    def m(self):\n        return 2\n",
        "class K:\n    def m(self):\n        return 1",
    ),
    ("python", "A = 1  # c\n", "A", "A = 2  # c\n", "A = 1  # c"),
    ("go", "package p\n\nconst A = 1\n", "A", "const A = 2\n", "const A = 1"),
];

/// How many of `FIXED` lead it and must be refused.
const REFUSED_ROWS: usize = 6;

fn cases(n: usize) -> Vec<Case> {
    let mut out = Vec::new();
    for (k, (lang, text, name, new, own)) in FIXED.into_iter().enumerate() {
        let ext = LANGS.iter().find(|l| l.0 == lang).unwrap().1;
        let body = text.strip_prefix("package p\n\n").unwrap_or(text).to_string();
        let offsets = vec![text.len() - body.len()];
        let piece = Piece { text: body, targets: vec![(name.to_string(), new.to_string(), own.to_string())] };
        out.push(Case { file: format!("fixed{k}.{ext}"), text: text.to_string(), pieces: vec![piece], offsets, target: (0, 0) });
    }
    let mut rng = Rng(0x5eed_0fed);
    for c in 0..n {
        let (lang, ext, shapes, head, sep) = LANGS[c % LANGS.len()];
        let count = 2 + rng.below(4);
        let pieces: Vec<Piece> = (0..count).map(|i| shape(lang, rng.below(shapes), c, i, rng.next() % 90 + 1)).collect();
        let mut text = head.to_string();
        let mut offsets = Vec::new();
        for (i, p) in pieces.iter().enumerate() {
            offsets.push(text.len());
            text.push_str(&p.text);
            let next_one_line = pieces.get(i + 1).is_some_and(|q| !q.text.contains('\n'));
            // a line comment or a `#define` runs to the end of its line: what followed would be in it
            let runs_to_end = p.text.contains("//") || p.text.contains('#');
            text.push_str(if !p.text.contains('\n') && !runs_to_end && next_one_line && rng.below(3) == 0 { sep } else { "\n\n" });
        }
        let text = text.trim_end().to_string() + "\n";
        let t = rng.below(pieces.len());
        let target = (t, rng.below(pieces[t].targets.len()));
        out.push(Case { file: format!("gen{c}.{ext}"), text, pieces, offsets, target });
    }
    out
}

/// The lines `S-E` the dry run says it replaces, as byte offsets of `text`.
fn replaced(text: &str, dry: &str, file: &str) -> (usize, usize) {
    let at = dry.find(&format!("{file}:")).unwrap_or_else(|| panic!("{dry}")) + file.len() + 1;
    let range: String = dry[at..].chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    let (s, e) = range.split_once('-').unwrap_or_else(|| panic!("{dry}"));
    let (s, e): (usize, usize) = (s.parse().unwrap(), e.parse().unwrap());
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let at = |l: usize| starts.get(l - 1).copied().unwrap_or(text.len());
    let a = if s == 1 && text.starts_with('\u{feff}') { 3 } else { at(s) };
    (a, at(e + 1))
}

#[test]
fn an_edit_keeps_every_other_definition_whatever_shares_its_lines() {
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_layouts_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let cases = cases(300);
    // a session per few files, so a call's look at its tree stays small
    let mut sessions = Vec::new();
    for (g, group) in cases.chunks(32).enumerate() {
        let root = base.join(format!("g{g}"));
        fs::create_dir_all(root.join(".git")).unwrap();
        for c in group {
            fs::write(root.join(&c.file), &c.text).unwrap();
        }
        sessions.push((root.clone(), Session::open(&root, Every::given(Some(0.0))).unwrap()));
    }
    let edits = scrutate::mcp::Settings::new(None).allowing_edits();
    let cancel = Arc::default();
    let (mut applied, mut refused) = (0, 0);
    let mut why: std::collections::BTreeMap<String, usize> = Default::default();
    let mut reason = |text: &str| {
        let t = text.trim_start_matches("error: ");
        let w = t.split_once(':').map_or(t, |x| x.0);
        let w: String = w
            .split_whitespace()
            .filter(|x| !x.chars().any(|c| c.is_ascii_digit()) && x.chars().next().is_some_and(char::is_lowercase))
            .collect::<Vec<_>>()
            .join(" ");
        *why.entry(w).or_default() += 1;
    };
    let mut broken = Vec::new();
    for (k, c) in cases.iter().enumerate() {
        let (root, session) = &sessions[k / 32];
        let call = |args: serde_json::Value| scrutate::mcp::call_tool(session, "edit_symbol", &args, &edits, &cancel);
        let args = json!({"symbol": c.name(), "path": c.file, "new_text": c.new_text()});
        let dry = call(args.clone());
        let now = || fs::read_to_string(root.join(&c.file)).unwrap();
        if dry.is_error {
            refused += 1;
            reason(&dry.text);
            if now() != c.text {
                broken.push(format!("{}: a refused dry run changed the file", c.file));
            }
            if k < REFUSED_ROWS && !dry.text.contains("nothing written") || (REFUSED_ROWS..FIXED.len()).contains(&k) {
                broken.push(format!("{}: {}", c.file, dry.text));
            }
            continue;
        }
        if k < REFUSED_ROWS {
            broken.push(format!("{}: the Auditor's shared-line case was not refused: {}", c.file, dry.text));
        }
        let hash = dry.text.lines().find_map(|l| l.strip_prefix("hash: ")).unwrap_or_else(|| panic!("{}", dry.text)).trim().to_string();
        let mut apply = args;
        apply["apply"] = json!(true);
        apply["hash"] = json!(hash);
        let done = call(apply);
        let new = now();
        if done.is_error {
            refused += 1;
            reason(&done.text);
            if new != c.text {
                broken.push(format!("{}: a refused edit changed the file", c.file));
            }
            continue;
        }
        applied += 1;
        // the lines the tool says it replaced are the target's own, known from how the file was built
        let (a, b) = c.lines();
        if replaced(&c.text, &dry.text, &c.file) != (a, b) {
            broken.push(format!("{}: replaced {:?} where the target is {:?}: {}", c.file, replaced(&c.text, &dry.text, &c.file), (a, b), dry.text));
            continue;
        }
        if !new.starts_with(&c.text[..a]) || !new.ends_with(&c.text[b..]) {
            broken.push(format!("{}: bytes outside lines it replaced changed:\n{:?}\n->\n{new:?}", c.file, c.text));
            continue;
        }
        // every other definition's text, in order; a member's piece keeps its other members
        let mut from = 0;
        for (i, p) in c.pieces.iter().enumerate() {
            let texts: Vec<&str> = if i == c.target.0 {
                p.targets.iter().enumerate().filter(|(j, _)| *j != c.target.1).map(|(_, t)| t.0.as_str()).collect()
            } else {
                vec![p.text.as_str()]
            };
            for t in texts {
                match new[from..].find(t) {
                    Some(at) => from += at + if i == c.target.0 { 0 } else { t.len() },
                    None => broken.push(format!("{}: {t:?} is gone:\n{:?}\n->\n{new:?}", c.file, c.text)),
                }
            }
        }
    }
    eprintln!("edit layouts: {} cases, {applied} applied, {refused} refused: {why:?}", cases.len());
    assert!(applied > cases.len() / 4 && refused > cases.len() / 10, "{applied} applied, {refused} refused");
    assert!(broken.is_empty(), "{}", broken.join("\n\n"));
    drop(sessions);
    let _ = fs::remove_dir_all(&base);
}
