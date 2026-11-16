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

/// A definition as a file holds it, its kind as the run counts it, and the names an edit can
/// target in it: each with the lines that replace its own, and its own text in the piece, which
/// starts its first replaced line.
struct Piece {
    kind: &'static str,
    text: String,
    targets: Vec<(String, String, String)>,
    /// the kind of the targets after the first, each a member on lines of its own inside the piece
    member: Option<&'static str>,
}

fn one(kind: &'static str, text: String, name: &str, new: String) -> Piece {
    Piece { kind, targets: vec![(name.to_string(), new + "\n", text.clone())], text, member: None }
}

/// A piece of two names, each a target whose own text is the whole piece.
fn both(kind: &'static str, text: String, (a, a_new): (String, String), (b, b_new): (String, String)) -> Piece {
    Piece { kind, targets: vec![(a, a_new, text.clone()), (b, b_new, text.clone())], text, member: None }
}

/// An owner and its member `{owner}.{name}` on lines of its own: the piece is `head`, the member's
/// old `body`, then `tail`; the owner as a target is the whole piece.
fn owned(kind: &'static str, (owner, name): (&str, &str), (head, body, new_body, tail): (String, String, String, String)) -> Piece {
    let text = format!("{head}{body}{tail}");
    let whole = format!("{head}{new_body}{tail}\n");
    Piece { kind, member: Some("method"), targets: vec![(owner.to_string(), whole, text.clone()), (format!("{owner}.{name}"), new_body + "\n", body)], text }
}

/// A type with its method beside it, not inside: the type's own line, then the method's.
fn beside(kind: &'static str, (ty, ty_new): (String, String), rest: &str, (method, body, new_body): (String, String, String)) -> Piece {
    Piece {
        kind,
        member: Some("method"),
        text: format!("{ty}{rest}{body}"),
        targets: vec![(String::new(), ty_new + "\n", ty), (method, new_body + "\n", body)],
    }
}

/// The shapes of one language, `c` and `i` naming them apart (every name is the only one of its
/// spelling in the repository) and `v` their value.
fn shape(lang: &str, k: usize, c: usize, i: usize, v: u64) -> Piece {
    let (n, w) = (format!("a{c}_{i}"), v + 100);
    let cap = format!("A{c}_{i}");
    let m = format!("m{c}_{i}");
    match (lang, k) {
        ("go", 0) => one("const", format!("const {cap} = {v}"), &cap, format!("const {cap} = {w}")),
        ("go", 1) => one("var", format!("var {n} = {v}"), &n, format!("var {n} = {w}")),
        ("go", 2) => one("function", format!("func {cap}() int {{ return {v} }}"), &cap, format!("func {cap}() int {{ return {w} }}")),
        ("go", 3) => one("function", format!("func {cap}() int {{\n\treturn {v}\n}}"), &cap, format!("func {cap}() int {{\n\treturn {w}\n}}")),
        ("go", 4) => one("type", format!("type {cap} int // the {i}"), &cap, format!("type {cap} int64 // the {i}")),
        ("go", 5) => Piece {
            member: None,
            kind: "const block",
            text: format!("const (\n\t{cap} = {v}\n\t{cap}b = {v}\n)"),
            targets: vec![
                (cap.clone(), format!("\t{cap} = {w}\n"), format!("\t{cap} = {v}")),
                (format!("{cap}b"), format!("\t{cap}b = {w}\n"), format!("\t{cap}b = {v}")),
            ],
        },
        ("go", 6) => both(
            "var pair",
            format!("var {n}, {n}b = {v}, {v}"),
            (n.clone(), format!("var {n}, {n}b = {w}, {v}\n")),
            (format!("{n}b"), format!("var {n}, {n}b = {v}, {w}\n")),
        ),
        ("go", 7) => one("struct", format!("type {cap} struct {{\n\tx{v} int\n}}"), &cap, format!("type {cap} struct {{\n\tx{w} int\n}}")),
        ("go", 9) => {
            let mut p = beside(
                "struct with a method",
                (format!("type {cap} struct{{ x{v} int }}"), format!("type {cap} struct{{ x{w} int }}")),
                "\n\n",
                (format!("{cap}.{m}"), format!("func (r {cap}) {m}() int {{\n\treturn {v}\n}}"), format!("func (r {cap}) {m}() int {{\n\treturn {w}\n}}")),
            );
            p.targets[0].0 = cap.clone();
            p
        }
        ("go", _) => one("interface", format!("type {cap} interface {{\n\tM{v}()\n}}"), &cap, format!("type {cap} interface {{\n\tM{w}()\n}}")),
        ("javascript" | "typescript", 0) => one("const", format!("const {n} = {v};"), &n, format!("const {n} = {w};")),
        ("javascript" | "typescript", 1) => one("function", format!("function {n}() {{ return {v} }}"), &n, format!("function {n}() {{ return {w} }}")),
        ("javascript" | "typescript", 2) => one("function", format!("function {n}() {{\n  return {v}\n}}"), &n, format!("function {n}() {{\n  return {w}\n}}")),
        ("javascript" | "typescript", 3) => {
            let text = format!("class {cap} {{ f = {v}; {m}() {{ return {v} }} }}");
            Piece {
                member: None,
                kind: "class",
                targets: vec![
                    (cap.clone(), format!("class {cap} {{ f = {w}; {m}() {{ return {v} }} }}\n"), text.clone()),
                    (m.clone(), format!("{m}() {{ return {w} }}\n"), format!("{m}() {{ return {v} }}")),
                ],
                text,
            }
        }
        ("javascript" | "typescript", 4) => both(
            "const pair",
            format!("const {n} = {v}, {n}b = {v};"),
            (n.clone(), format!("const {n} = {w}, {n}b = {v};\n")),
            (format!("{n}b"), format!("const {n} = {v}, {n}b = {w};\n")),
        ),
        ("javascript" | "typescript", 5) => {
            one("export function", format!("export function {n}() {{\n  return {v}\n}}"), &n, format!("export function {n}() {{\n  return {w}\n}}"))
        }
        ("javascript" | "typescript", 6) => {
            one("class", format!("export class {cap} {{\n  {m}() {{ return {v} }}\n}}"), &cap, format!("export class {cap} {{\n  {m}() {{ return {w} }}\n}}"))
        }
        ("typescript", 7) => one("type alias", format!("type {cap} = {v};"), &cap, format!("type {cap} = {w};")),
        ("typescript", 8) => {
            one("interface", format!("export interface {cap} {{\n  x{v}: number;\n}}"), &cap, format!("export interface {cap} {{\n  x{w}: number;\n}}"))
        }
        ("typescript", 9) => one("enum", format!("enum {cap} {{\n  B{v},\n}}"), &cap, format!("enum {cap} {{\n  B{w},\n}}")),
        ("javascript" | "typescript", 10) | ("javascript", 7) => owned(
            "class with a method",
            (&cap, &m),
            (format!("class {cap} {{\n"), format!("  {m}() {{\n    return {v}\n  }}"), format!("  {m}() {{\n    return {w}\n  }}"), "\n}".to_string()),
        ),
        ("typescript", 11) => {
            one("interface };", format!("interface {cap} {{\n  x{v}: number;\n}};"), &cap, format!("interface {cap} {{\n  x{w}: number;\n}};"))
        }
        ("typescript", 12) => one("class };", format!("class {cap} {{\n  x = {v};\n}};"), &cap, format!("class {cap} {{\n  x = {w};\n}};")),
        ("typescript", 13) => one("enum };", format!("enum {cap} {{\n  B{v},\n}};"), &cap, format!("enum {cap} {{\n  B{w},\n}};")),
        ("javascript" | "typescript", _) => one("let (no definition)", format!("let {n} = {v}; // {i}"), &n, format!("let {n} = {w}; // {i}")),
        ("rust", 0) => one("const", format!("const {cap}: u8 = {};", v % 200), &cap, format!("const {cap}: u8 = {};", w % 200)),
        ("rust", 1) => one("static", format!("static {cap}: u8 = {};", v % 200), &cap, format!("static {cap}: u8 = {};", w % 200)),
        ("rust", 2) => one("function", format!("fn {n}() -> u64 {{ {v} }}"), &n, format!("fn {n}() -> u64 {{ {w} }}")),
        ("rust", 3) => one("function", format!("fn {n}() -> u64 {{\n    {v}\n}} // {i}"), &n, format!("fn {n}() -> u64 {{\n    {w}\n}} // {i}")),
        ("rust", 4) => one("struct", format!("struct {cap}(u{});", [8, 16, 32][v as usize % 3]), &cap, format!("struct {cap}(u64);")),
        ("rust", 5) => one(
            "struct",
            format!("#[derive(Debug)]\npub struct {cap} {{\n    x{v}: u8,\n}}"),
            &cap,
            format!("#[derive(Debug)]\npub struct {cap} {{\n    x{w}: u8,\n}}"),
        ),
        ("rust", 6) => one("enum", format!("pub enum {cap} {{\n    B{v},\n}}"), &cap, format!("pub enum {cap} {{\n    B{w},\n}}")),
        ("rust", 7) => one(
            "trait",
            format!("trait {cap} {{\n    fn {m}(&self) -> u64 {{ {v} }}\n}}"),
            &cap,
            format!("trait {cap} {{\n    fn {m}(&self) -> u64 {{ {w} }}\n}}"),
        ),
        ("rust", 9) => {
            let mut p = beside(
                "struct beside its impl",
                (format!("struct {cap}(u8);"), format!("struct {cap}(u16);")),
                &format!("\n\nimpl {cap} {{\n"),
                (
                    format!("{cap}.{m}"),
                    format!("    fn {m}(&self) -> u64 {{\n        {v}\n    }}"),
                    format!("    fn {m}(&self) -> u64 {{\n        {w}\n    }}"),
                ),
            );
            p.text.push_str("\n}");
            p.targets[0].0 = cap.clone();
            p
        }
        ("rust", _) => one("type alias", format!("type {cap} = [u8; {}];", v % 200), &cap, format!("type {cap} = [u8; {}];", w % 200)),
        ("python", 0) => one("constant", format!("{cap} = {v}"), &cap, format!("{cap} = {w}")),
        ("python", 1) => one("constant", format!("{cap} = {v}  # {i}"), &cap, format!("{cap} = {w}  # {i}")),
        ("python", 2) => one("function", format!("def {n}():\n    return {v}"), &n, format!("def {n}():\n    return {w}")),
        ("python", 3) => one("class", format!("class {cap}:\n    v = {v}"), &cap, format!("class {cap}:\n    v = {w}")),
        ("python", 5) => owned(
            "class with a method",
            (&cap, &m),
            (format!("class {cap}:\n"), format!("    def {m}(self):\n        return {v}"), format!("    def {m}(self):\n        return {w}"), String::new()),
        ),
        ("python", _) => one("decorated class", format!("@decorator\nclass {cap}:\n    v = {v}"), &cap, format!("@decorator\nclass {cap}:\n    v = {w}")),
        ("c", 0) => one("const", format!("const int {n} = {v};"), &n, format!("const int {n} = {w};")),
        ("c", 1) => one("function", format!("int {n}(void) {{ return {v}; }}"), &n, format!("int {n}(void) {{ return {w}; }}")),
        ("c", 2) => one("function", format!("int {n}(void) {{\n  return {v};\n}}"), &n, format!("int {n}(void) {{\n  return {w};\n}}")),
        ("c", 3) => both(
            "const pair",
            format!("const int {n} = {v}, {n}b = {v};"),
            (n.clone(), format!("const int {n} = {w}, {n}b = {v};\n")),
            (format!("{n}b"), format!("const int {n} = {v}, {n}b = {w};\n")),
        ),
        ("c", 4) => one("#define", format!("#define {cap} {v}"), &cap, format!("#define {cap} {w}")),
        ("c", 5) => one("struct };", format!("struct {cap} {{\n    int x{v};\n}};"), &cap, format!("struct {cap} {{\n    int x{w};\n}};")),
        ("c", 6) => one("enum };", format!("enum {cap} {{\n    B{v}\n}};"), &cap, format!("enum {cap} {{\n    B{w}\n}};")),
        ("c", 7) => {
            one("struct with a declarator", format!("struct {cap} {{\n    int x{v};\n}} {n};"), &cap, format!("struct {cap} {{\n    int x{w};\n}} {n};"))
        }
        ("c", 8) => one("struct } ;", format!("struct {cap} {{\n    int x{v};\n}} ;"), &cap, format!("struct {cap} {{\n    int x{w};\n}} ;")),
        ("c", 9) => one("struct };;", format!("struct {cap} {{\n    int x{v};\n}};;"), &cap, format!("struct {cap} {{\n    int x{w};\n}};;")),
        ("c", 10) => {
            one("struct }; // c", format!("struct {cap} {{\n    int x{v};\n}}; // c{i}"), &cap, format!("struct {cap} {{\n    int x{w};\n}}; // c{i}"))
        }
        ("c", _) => one("typedef", format!("typedef int {cap};"), &cap, format!("typedef long {cap};")),
        ("cpp", 0) => one(
            "class };",
            format!("class {cap} {{\n    int {m}() {{ return {v}; }}\n}};"),
            &cap,
            format!("class {cap} {{\n    int {m}() {{ return {w}; }}\n}};"),
        ),
        ("cpp", 1) => one("struct };", format!("struct {cap} {{ int x{v}; }};"), &cap, format!("struct {cap} {{ int x{w}; }};")),
        ("cpp", 2) => one("enum };", format!("enum {cap} {{\n    B{v}\n}};"), &cap, format!("enum {cap} {{\n    B{w}\n}};")),
        ("cpp", 3) => one("function", format!("int {n}() {{\n    return {v};\n}}"), &n, format!("int {n}() {{\n    return {w};\n}}")),
        ("cpp", 4) => {
            one("struct with a declarator", format!("struct {cap} {{\n    int x{v};\n}} {n};"), &cap, format!("struct {cap} {{\n    int x{w};\n}} {n};"))
        }
        ("cpp", 6) => owned(
            "class }; with a method",
            (&cap, &m),
            (
                format!("class {cap} {{\npublic:\n"),
                format!("    int {m}() {{\n        return {v};\n    }}"),
                format!("    int {m}() {{\n        return {w};\n    }}"),
                "\n};".to_string(),
            ),
        ),
        ("cpp", 7) => one(
            "template function",
            format!("template <typename T>\nT {n}(T x) {{\n    return x + {v};\n}}"),
            &n,
            format!("template <typename T>\nT {n}(T x) {{\n    return x + {w};\n}}"),
        ),
        ("cpp", 8) => one(
            "template struct };",
            format!("template <typename T>\nstruct {cap} {{\n    T x{v};\n}};"),
            &cap,
            format!("template <typename T>\nstruct {cap} {{\n    T x{w};\n}};"),
        ),
        ("cpp", 9) => one("struct } ;", format!("struct {cap} {{\n    int x{v};\n}} ;"), &cap, format!("struct {cap} {{\n    int x{w};\n}} ;")),
        ("cpp", 10) => one("struct };;", format!("struct {cap} {{\n    int x{v};\n}};;"), &cap, format!("struct {cap} {{\n    int x{w};\n}};;")),
        ("cpp", 11) => one("class }; // c", format!("class {cap} {{\n    int x{v};\n}}; // c{i}"), &cap, format!("class {cap} {{\n    int x{w};\n}}; // c{i}")),
        ("cpp", 12) => one(
            "template class };",
            format!("template <class T>\nclass {cap} {{\n    T x{v};\n}};"),
            &cap,
            format!("template <class T>\nclass {cap} {{\n    T x{w};\n}};"),
        ),
        ("cpp", _) => {
            one("namespace", format!("namespace {n} {{\nint f{v}() {{ return {v}; }}\n}}"), &n, format!("namespace {n} {{\nint f{w}() {{ return {w}; }}\n}}"))
        }
        ("java", 0) => {
            one("class", format!("class {cap} {{\n    int {m}() {{ return {v}; }}\n}}"), &cap, format!("class {cap} {{\n    int {m}() {{ return {w}; }}\n}}"))
        }
        ("java", 1) => one("interface", format!("interface {cap} {{\n    int {m}();\n}}"), &cap, format!("interface {cap} {{\n    long {m}();\n}}")),
        ("java", 2) => one("enum", format!("enum {cap} {{\n    B{v};\n}}"), &cap, format!("enum {cap} {{\n    B{w};\n}}")),
        ("java", 3) => {
            one("annotated class", format!("@Deprecated\nclass {cap} {{\n    int x{v};\n}}"), &cap, format!("@Deprecated\nclass {cap} {{\n    int x{w};\n}}"))
        }
        ("java", 5) => owned(
            "class with a method",
            (&cap, &m),
            (
                format!("class {cap} {{\n"),
                format!("    int {m}() {{\n        return {v};\n    }}"),
                format!("    int {m}() {{\n        return {w};\n    }}"),
                "\n}".to_string(),
            ),
        ),
        (_, _) => one("record", format!("record {cap}(int x{v}) {{}}"), &cap, format!("record {cap}(int x{w}) {{}}")),
    }
}

const LANGS: [(&str, &str, usize, &str, &str); 8] = [
    ("go", "go", 10, "package p\n\n", "; "),
    ("javascript", "js", 9, "", " "),
    ("typescript", "ts", 15, "", " "),
    ("rust", "rs", 10, "", " "),
    ("python", "py", 6, "", "; "),
    ("c", "c", 12, "", " "),
    ("cpp", "cpp", 14, "", " "),
    ("java", "java", 6, "", " "),
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
    /// the target has its lines to itself: no other piece was joined onto them
    alone: bool,
    target: (usize, usize),
}

impl Case {
    fn name(&self) -> &str {
        &self.pieces[self.target.0].targets[self.target.1].0
    }

    /// The target's kind: its piece's, or its member's.
    fn kind(&self) -> &'static str {
        let p = &self.pieces[self.target.0];
        p.member.filter(|_| self.target.1 > 0).unwrap_or(p.kind)
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

/// The kinds refused even with their lines to themselves, each with the reason it must be refused
/// for: a declarator or a second `;` after the brace is other code, two names in one declaration
/// cannot be edited apart, a `let` is not indexed, and a C++ namespace is a module.
const REFUSED_ALONE: [(&str, &str); 6] = [
    ("struct with a declarator", "shares a line with other code that is not a comment"),
    ("struct };;", "shares a line with other code that is not a comment"),
    ("const pair", "is declared together with"),
    ("var pair", "is declared together with"),
    ("let (no definition)", "no definition matches"),
    ("namespace", "names a module"),
];

/// The reason a kind of `REFUSED_ALONE` is refused for, alone.
fn refused_alone(kind: &str) -> Option<&'static str> {
    REFUSED_ALONE.iter().find(|(k, _)| *k == kind).map(|(_, why)| *why)
}

/// What follows a new text in a language to put code outside the definition: a statement, a
/// comment left open, or a string left open (`which` picks one).
fn tainted(ext: &str, which: usize) -> &'static str {
    let [statement, comment, string] = match ext {
        "go" => ["var _ = 1\n", "/*\n", "\"\n"],
        "js" | "ts" => ["console.log(1);\n", "/*\n", "`\n"],
        "rs" => ["use std::fmt;\n", "/*\n", "\"\n"],
        "py" => ["print(1)\n", "\"\"\"\n", "'''\n"],
        "c" => ["_Static_assert(1, \"x\");\n", "/*\n", "\"\n"],
        "cpp" => ["static_assert(true);\n", "/*\n", "\"\n"],
        _ => ["static { }\n", "/*\n", "\"\n"],
    };
    [statement, comment, string][which % 3]
}

/// How many of `FIXED` lead it and must be refused.
const REFUSED_ROWS: usize = 6;

fn cases(n: usize) -> Vec<Case> {
    let mut out = Vec::new();
    for (k, (lang, text, name, new, own)) in FIXED.into_iter().enumerate() {
        let ext = LANGS.iter().find(|l| l.0 == lang).unwrap().1;
        let body = text.strip_prefix("package p\n\n").unwrap_or(text).to_string();
        let offsets = vec![text.len() - body.len()];
        let piece = Piece { kind: "fixed row", text: body, targets: vec![(name.to_string(), new.to_string(), own.to_string())], member: None };
        out.push(Case { file: format!("fixed{k}.{ext}"), text: text.to_string(), pieces: vec![piece], offsets, target: (0, 0), alone: false });
    }
    let mut rng = Rng(0x5eed_0fed);
    for c in 0..n {
        let (lang, ext, shapes, head, sep) = LANGS[c % LANGS.len()];
        let count = 2 + rng.below(4);
        let pieces: Vec<Piece> = (0..count).map(|i| shape(lang, rng.below(shapes), c, i, rng.next() % 90 + 1)).collect();
        let mut text = head.to_string();
        let mut offsets = Vec::new();
        let mut shared = vec![false; pieces.len()];
        for (i, p) in pieces.iter().enumerate() {
            offsets.push(text.len());
            text.push_str(&p.text);
            let next_one_line = pieces.get(i + 1).is_some_and(|q| !q.text.contains('\n'));
            // a line comment or a `#define` runs to the end of its line: what followed would be in it
            let runs_to_end = p.text.contains("//") || p.text.contains('#');
            let joined = !p.text.contains('\n') && !runs_to_end && next_one_line && rng.below(3) == 0;
            if joined {
                shared[i] = true;
                shared[i + 1] = true;
            }
            text.push_str(if joined { sep } else { "\n\n" });
        }
        let text = text.trim_end().to_string() + "\n";
        let t = rng.below(pieces.len());
        let target = (t, rng.below(pieces[t].targets.len()));
        // a member's own text is part of its piece's, which shares its lines (a one-line class)
        // a member's lines are its own; a type beside its method shares the piece with it
        let own_lines = pieces[t].targets[target.1].2 == pieces[t].text || pieces[t].kind == "const block" || pieces[t].member.is_some();
        let alone = !shared[t] && own_lines;
        out.push(Case { file: format!("gen{c}.{ext}"), text, pieces, offsets, target, alone });
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
    let (mut applied, mut refused, mut tainted_cases) = (0, 0, 0);
    // (language, kind) -> (targets reached, applied, refused)
    let mut reached: std::collections::BTreeMap<(String, &str), (usize, usize, usize)> = Default::default();
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
        let key = (c.file.rsplit('.').next().unwrap_or("").to_string(), c.kind());
        reached.entry(key.clone()).or_default().0 += 1;
        let mut dry = call(args.clone());
        let mut args = args;
        // a name two definitions carry (a Rust struct and its impl) is refused with one location
        // to pass, which must then edit the target
        if let Some(at) = dry.text.split_once("pass one location (").and_then(|(_, r)| r.split_once(')')).map(|(at, _)| at.to_string()) {
            args["symbol"] = json!(at);
            dry = call(args.clone());
        }
        let now = || fs::read_to_string(root.join(&c.file)).unwrap();
        // the same new text with code after the definition, or a comment or string left open to
        // swallow what follows: refused, the file as it was
        if k >= FIXED.len() {
            let tail = tainted(c.file.rsplit('.').next().unwrap_or(""), k % 3);
            let mut bad = args.clone();
            bad["new_text"] = json!(format!("{}{tail}", c.new_text()));
            let got = call(bad);
            tainted_cases += 1;
            if !got.is_error {
                broken.push(format!("{}: a new text followed by {tail:?} was not refused: {}", c.file, got.text));
            }
            if now() != c.text {
                broken.push(format!("{}: a new text followed by {tail:?} changed the file", c.file));
            }
        }
        if dry.is_error {
            refused += 1;
            reached.entry(key.clone()).or_default().2 += 1;
            reason(&dry.text);
            if now() != c.text {
                broken.push(format!("{}: a refused dry run changed the file", c.file));
            }
            if k < REFUSED_ROWS && !dry.text.contains("nothing written") || (REFUSED_ROWS..FIXED.len()).contains(&k) {
                broken.push(format!("{}: {}", c.file, dry.text));
            }
            // a definition with its lines to itself is edited, unless its kind cannot be on its own
            if c.alone && !refused_alone(c.kind()).is_some_and(|why| dry.text.contains(why)) {
                broken.push(format!("{}: {} refused with its lines to itself: {}\n{:?}", c.file, c.kind(), dry.text, c.text));
            }
            continue;
        }
        if k < REFUSED_ROWS {
            broken.push(format!("{}: the Auditor's shared-line case was not refused: {}", c.file, dry.text));
        }
        if c.alone && refused_alone(c.kind()).is_some() {
            broken.push(format!("{}: {} is refused by design but its dry run passed: {}", c.file, c.kind(), dry.text));
        }
        let hash = dry.text.lines().find_map(|l| l.strip_prefix("hash: ")).unwrap_or_else(|| panic!("{}", dry.text)).trim().to_string();
        let mut apply = args;
        apply["apply"] = json!(true);
        apply["hash"] = json!(hash);
        let done = call(apply);
        let new = now();
        if done.is_error {
            refused += 1;
            reached.entry(key.clone()).or_default().2 += 1;
            reason(&done.text);
            if new != c.text {
                broken.push(format!("{}: a refused edit changed the file", c.file));
            }
            continue;
        }
        applied += 1;
        reached.entry(key.clone()).or_default().1 += 1;
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
                // a member's text holds its own name, not its owner's qualified one
                p.targets.iter().enumerate().filter(|(j, _)| *j != c.target.1).map(|(_, t)| t.0.rsplit('.').next().unwrap_or("")).collect()
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
    eprintln!("edit layouts: {} cases, {applied} applied, {refused} refused: {why:?}; {tainted_cases} tainted new texts refused", cases.len());
    for ((lang, kind), (n, a, r)) in &reached {
        eprintln!("  {lang:<5} {kind:<26} targets {n:>3}  applied {a:>3}  refused {r:>3}");
    }
    assert!(applied > cases.len() / 4 && refused > cases.len() / 10, "{applied} applied, {refused} refused");
    assert!(broken.is_empty(), "{}", broken.join("\n\n"));
    drop(sessions);
    let _ = fs::remove_dir_all(&base);
}
