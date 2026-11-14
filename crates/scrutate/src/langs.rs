//! Language registry: which grammar parses which files, and which nodes are definitions and calls.
//!
//! Every entry was derived from the grammar's actual parse tree of a sample file (engine/tests/samples)
//! and is covered by a test that asserts the symbols found in that sample. Languages without an entry
//! here still get heuristic symbols (see parse::heuristic_chunks) and text search.

use tree_sitter::{Language, Node};

/// How to find the name of a definition node.
#[derive(Clone, Copy)]
pub enum NameRule {
    /// text of this field
    Field(&'static str),
    /// follow this field down (C declarators: `declarator` -> `function_declarator` -> ...), then the
    /// text of the innermost node whose kind is in the given list (or the innermost node when empty)
    Dig(&'static str, &'static [&'static str]),
    /// first child (any depth 1) whose kind is in the list
    ChildKind(&'static [&'static str]),
    /// first named child
    FirstNamed,
    /// text of the node itself
    Own,
    /// the n-th named child
    NamedChild(usize),
    /// the first argument of a call-like node (Elixir `def name(args)`, Lisp `(defun name ...)`)
    Custom(fn(Node, &[u8]) -> Option<String>),
}

pub struct Def {
    pub node: &'static str,
    pub kind: &'static str,
    pub name: &'static [NameRule],
    /// only when the `value` field has one of these kinds (JS `const f = () => ...`)
    pub value_kinds: &'static [&'static str],
    /// only when the first child's text is one of these (Elixir `def`/`defmodule` calls)
    pub head_words: &'static [&'static str],
    /// only when this field is present (C `struct point {...}` defines; `struct point p;` does not)
    pub needs_field: &'static str,
    /// the chunk kind comes from the declaration keyword (`struct`/`enum`/`interface`...) found in
    /// the node's first line, falling back to `kind` (grammars that use one node for several kinds)
    pub kind_by_head: bool,
    /// only when the nearest enclosing node that is not an `outer` wrapper has one of these kinds
    /// (C++ `Shape();` declares a constructor in a class body; at file level it is a call)
    pub parents: &'static [&'static str],
}

pub struct Call {
    pub node: &'static str,
    /// field holding the callee expression, else the first child of that kind ("" = first named child)
    pub field: &'static str,
    /// callee names that are syntax, not calls (Elixir `def`, `alias`, `if`)
    pub skip: &'static [&'static str],
}

/// Where an import statement keeps the imported module.
#[derive(Clone, Copy)]
pub enum ImportHow {
    /// every child with this field name (Python `import a, b`)
    Fields(&'static str),
    /// the first string literal anywhere below the node (`require("x")`, `#include "x.h"`)
    FirstString,
    /// the first child whose kind is in the list
    ChildKind(&'static [&'static str]),
    /// the node's own text (keywords are stripped afterwards)
    Text,
    /// the first argument of the call when it is a literal of one of these kinds with no interpolation
    Arg(&'static [&'static str]),
    /// each name of a PHP `use`, a grouped `use A\{B, C}` prefixed by its namespace
    UseClauses,
}

pub struct ImportRule {
    pub node: &'static str,
    pub how: ImportHow,
    /// only when the text of this field (the callee / command name) is one of `when_head`;
    /// an empty field means the first named child
    pub head_field: &'static str,
    pub when_head: &'static [&'static str],
}

const fn imp(node: &'static str, how: ImportHow) -> ImportRule {
    ImportRule { node, how, head_field: "", when_head: &[] }
}
const fn imp_call(node: &'static str, head_field: &'static str, when_head: &'static [&'static str], how: ImportHow) -> ImportRule {
    ImportRule { node, how, head_field, when_head }
}

pub struct LangSpec {
    pub id: &'static str,
    pub exts: &'static [&'static str],
    pub files: &'static [&'static str],
    pub language: fn() -> Language,
    pub defs: &'static [Def],
    pub calls: &'static [Call],
    /// a string statement that opens a body is documentation (Python, Elixir @doc is not this)
    pub docstring: bool,
    /// wrapper node kinds a leading comment may sit above (decorators, `export`, `const`)
    pub wrappers: &'static [&'static str],
    /// wrapper node kinds whose extent belongs to the chunk itself (decorated definitions, templates)
    pub outer: &'static [&'static str],
    /// field name of the body node ("" = none: the whole node after the name is the body)
    pub body: &'static str,
    /// how imports / includes / requires are recognised (empty: none extracted)
    pub imports: &'static [ImportRule],
}

const fn d(node: &'static str, kind: &'static str, name: &'static [NameRule]) -> Def {
    Def { node, kind, name, value_kinds: &[], head_words: &[], needs_field: "", kind_by_head: false, parents: &[] }
}
/// a definition that needs a body to count (declarations/forward references do not)
const fn db(node: &'static str, kind: &'static str, name: &'static [NameRule]) -> Def {
    Def { node, kind, name, value_kinds: &[], head_words: &[], needs_field: "body", kind_by_head: false, parents: &[] }
}
/// a definition whose kind is read from its keyword
const fn dk(node: &'static str, kind: &'static str, name: &'static [NameRule]) -> Def {
    Def { node, kind, name, value_kinds: &[], head_words: &[], needs_field: "", kind_by_head: true, parents: &[] }
}

/// Declaration keywords and the chunk kind they denote (for `kind_by_head`).
pub const KIND_WORDS: &[(&str, &str)] = &[
    ("class", "class"),
    ("struct", "struct"),
    ("interface", "interface"),
    ("protocol", "interface"),
    ("trait", "trait"),
    ("enum", "enum"),
    ("extension", "impl"),
    ("object", "class"),
    ("actor", "class"),
    ("record", "class"),
    ("union", "struct"),
    ("module", "module"),
    ("namespace", "module"),
    ("type", "type"),
    ("typealias", "type"),
    ("mixin", "trait"),
    ("annotation", "class"),
    // Lisp forms
    ("defun", "function"),
    ("define", "function"),
    ("defmacro", "macro"),
    ("define-syntax", "macro"),
    ("define-syntax-rule", "macro"),
    ("defclass", "class"),
    ("defstruct", "struct"),
    ("define-struct", "struct"),
    ("define-record-type", "struct"),
    ("defvar", "key"),
    ("defparameter", "key"),
    ("defconstant", "key"),
    ("deftype", "type"),
    ("defgeneric", "function"),
    ("defmethod", "function"),
];
const fn c(node: &'static str, field: &'static str) -> Call {
    Call { node, field, skip: &[] }
}

const N: &[NameRule] = &[NameRule::Field("name")];
const ELIXIR_SYNTAX: &[&str] = &[
    "def",
    "defp",
    "defmacro",
    "defmacrop",
    "defguard",
    "defguardp",
    "defdelegate",
    "defmodule",
    "defprotocol",
    "defimpl",
    "defstruct",
    "defexception",
    "defoverridable",
    "use",
    "import",
    "alias",
    "require",
    "if",
    "unless",
    "case",
    "cond",
    "with",
    "for",
    "receive",
    "try",
    "quote",
    "unquote",
];
const ID_KINDS: &[&str] = &[
    "identifier",
    "field_identifier",
    "type_identifier",
    "qualified_identifier",
    "scoped_identifier",
    "destructor_name",
    "operator_name",
    "template_function",
    "structured_binding_declarator",
    "constant",
    "scope_resolution",
];

fn text(src: &[u8], a: usize, b: usize) -> String {
    String::from_utf8_lossy(&src[a.min(src.len())..b.min(src.len())]).into_owned()
}

/// name of a Lisp-style `(defun NAME ...)` / Elixir `def NAME(...)` form: first identifier after the head
fn second_symbol(node: Node, src: &[u8]) -> Option<String> {
    // Elixir: call(target: identifier, arguments (call (target: identifier) ...) | alias | identifier)
    let mut cur0 = node.walk();
    let args = node.children(&mut cur0).find(|c| c.kind() == "arguments");
    if let Some(args) = args {
        let mut cur = args.walk();
        for ch in args.children(&mut cur) {
            if !ch.is_named() {
                continue;
            }
            // `defguard is_big(x) when x > 100`: the head is left of `when`
            let ch = if ch.kind() == "binary_operator" { ch.child_by_field_name("left").unwrap_or(ch) } else { ch };
            let t = if ch.kind() == "call" {
                ch.child_by_field_name("target").map(|n| text(src, n.start_byte(), n.end_byte()))
            } else if ch.kind() == "alias" || ch.kind() == "identifier" {
                Some(text(src, ch.start_byte(), ch.end_byte()))
            } else {
                None
            };
            return t;
        }
        return None;
    }
    // Lisp: the second named child
    let mut cur = node.walk();
    let mut seen = 0;
    for ch in node.children(&mut cur) {
        if ch.is_named() {
            seen += 1;
            if seen == 2 {
                return Some(text(src, ch.start_byte(), ch.end_byte()));
            }
        }
    }
    None
}

/// R: `name <- function(...)` is a binary_operator; name is its lhs when rhs is a function_definition
fn r_assignment(node: Node, src: &[u8]) -> Option<String> {
    let rhs = node.child_by_field_name("rhs")?;
    if rhs.kind() != "function_definition" && rhs.kind() != "lambda_function" {
        return None;
    }
    let lhs = node.child_by_field_name("lhs")?;
    Some(text(src, lhs.start_byte(), lhs.end_byte()))
}

/// The text of `node`'s child `field` when that child is of `kind`.
fn field_text(node: Node, field: &str, kind: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field).filter(|n| n.kind() == kind).map(|n| text(src, n.start_byte(), n.end_byte()))
}

/// The class of an R6 method `add = function(n) ...` in `R6Class("Counter", public = list(...))`:
/// the class name string, else the name the class is assigned to.
pub(crate) fn r6_class(node: Node, src: &[u8]) -> Option<String> {
    fn call_of<'t>(n: Node<'t>, f: &str, src: &[u8]) -> Option<Node<'t>> {
        let call = n.parent().filter(|a| a.kind() == "arguments")?.parent()?;
        let function = call.child_by_field_name("function")?;
        // package code writes `R6::R6Class(...)`, since a package cannot attach R6
        let named = if function.kind() == "namespace_operator" { function.child_by_field_name("rhs")? } else { function };
        (named.kind() == "identifier" && text(src, named.start_byte(), named.end_byte()) == f).then_some(call)
    }
    let list = call_of(node, "list", src)?;
    let section = list
        .parent()
        .filter(|a| a.kind() == "argument" && matches!(field_text(*a, "name", "identifier", src).as_deref(), Some("public" | "private" | "active")))?;
    let class = call_of(section, "R6Class", src)?;
    let args = class.child_by_field_name("arguments")?;
    let named = args.children_by_field_name("argument", &mut args.walk()).find(|a| a.child_by_field_name("name").is_none());
    let string =
        named.and_then(|a| a.child_by_field_name("value")).filter(|v| v.kind() == "string").and_then(|v| field_text(v, "content", "string_content", src));
    string.or_else(|| class.parent().filter(|p| p.kind() == "binary_operator").and_then(|p| field_text(p, "lhs", "identifier", src)))
}

/// An R6 method: a function given as a named argument of a section list of `R6Class`.
fn r6_method(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("value").filter(|v| v.kind() == "function_definition")?;
    r6_class(node, src)?;
    field_text(node, "name", "identifier", src)
}

/// A Haskell function or binding: `f x = ...`, the operator of `a <+> b = ...` and `(<+>) a b = ...`.
fn haskell_name(node: Node, src: &[u8]) -> Option<String> {
    if let Some(n) = node.child_by_field_name("name") {
        let n = if n.kind() == "prefix_id" { n.named_child(0)? } else { n };
        return Some(text(src, n.start_byte(), n.end_byte()));
    }
    let op = node.named_children(&mut node.walk()).find(|c| c.kind() == "infix")?.child_by_field_name("operator")?;
    let op = if op.kind() == "infix_id" { op.named_child(0)? } else { op };
    Some(text(src, op.start_byte(), op.end_byte()))
}

/// Markdown / RST sections: the heading text
fn heading_text(node: Node, src: &[u8]) -> Option<String> {
    let t = text(src, node.start_byte(), node.end_byte());
    let line = t.lines().next().unwrap_or("").trim().trim_start_matches('#').trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

/// A function declared without a body at file or namespace scope (`int add(int, int);`).
const PROTOTYPE: Def =
    Def { parents: &["translation_unit", "declaration_list", "linkage_specification"], ..d("declaration", "function", &[NameRule::Custom(cpp_member_decl)]) };
const C_DEFS: &[Def] = &[
    d("function_definition", "function", &[NameRule::Dig("declarator", ID_KINDS)]),
    PROTOTYPE,
    d("preproc_function_def", "macro", N),
    db("struct_specifier", "struct", N),
    db("union_specifier", "struct", N),
    db("enum_specifier", "enum", N),
    d("type_definition", "type", &[NameRule::Dig("declarator", ID_KINDS)]),
];
const CPP_DEFS: &[Def] = &[
    d("function_definition", "function", &[NameRule::Dig("declarator", ID_KINDS)]),
    d("preproc_function_def", "macro", N),
    db("class_specifier", "class", N),
    db("struct_specifier", "struct", N),
    db("union_specifier", "struct", N),
    db("enum_specifier", "enum", N),
    d("namespace_definition", "module", N),
    d("type_definition", "type", &[NameRule::Dig("declarator", ID_KINDS)]),
    d("alias_declaration", "type", N),
    d("field_declaration", "function", &[NameRule::Custom(cpp_member_decl)]),
    // constructors, destructors, conversion operators and member templates declared in a class body
    Def { parents: &["field_declaration_list"], ..d("declaration", "method", &[NameRule::Custom(cpp_member_decl)]) },
    PROTOTYPE,
];
const C_CALLS: &[Call] = &[c("call_expression", "function"), c("new_expression", "type")];
const CSHARP_DEFS: &[Def] = &[
    d("class_declaration", "class", N),
    d("struct_declaration", "struct", N),
    d("record_declaration", "class", N),
    d("interface_declaration", "interface", N),
    d("enum_declaration", "enum", N),
    d("method_declaration", "method", N),
    d("constructor_declaration", "method", N),
    d("property_declaration", "method", N),
    d("namespace_declaration", "module", N),
    d("delegate_declaration", "type", N),
];
pub(crate) const JS_FUNCS: &[&str] = &["arrow_function", "function_expression", "function"];
const fn dv(node: &'static str, name: &'static [NameRule]) -> Def {
    Def { node, kind: "function", name, value_kinds: JS_FUNCS, head_words: &[], needs_field: "", kind_by_head: false, parents: &[] }
}
/// `a.b.c` of a member expression made of names only (no `this` or call), `a["b"]` read as `a.b`.
pub(crate) fn js_member_path(node: Node, src: &[u8]) -> Option<String> {
    let key = match node.kind() {
        "identifier" => return Some(text(src, node.start_byte(), node.end_byte())),
        "member_expression" => node.child_by_field_name("property").filter(|p| p.kind() == "property_identifier")?,
        // a computed key (`handlers[i]`) is no name
        "subscript_expression" => node.child_by_field_name("index").filter(|i| i.kind() == "string" && i.named_child_count() == 1)?.named_child(0)?,
        _ => return None,
    };
    let key = text(src, key.start_byte(), key.end_byte());
    if key.is_empty() || key.contains(|c: char| c == '.' || c.is_whitespace()) {
        return None;
    }
    Some(format!("{}.{key}", js_member_path(node.child_by_field_name("object")?, src)?))
}
/// The member path a function is assigned to: `Foo.prototype.bar = function ...`, `$.fn.plugin = () => ...`
fn js_assigned_path(node: Node, src: &[u8]) -> Option<String> {
    if !JS_FUNCS.contains(&node.child_by_field_name("right")?.kind()) {
        return None;
    }
    js_member_path(node.child_by_field_name("left").filter(|l| l.kind() != "identifier")?, src)
}
/// `Foo.prototype.bar = function ...` is the method `Foo.bar`.
fn js_prototype_name(node: Node, src: &[u8]) -> Option<String> {
    let path = js_assigned_path(node, src)?;
    let (owner, name) = path.rsplit_once(".prototype.")?;
    (!name.contains('.')).then(|| format!("{owner}.{name}"))
}
/// A function assigned to a member: `Foo.staticFn` keeps its owner; CommonJS
/// `module.exports.NAME` and `exports.NAME` are the bare `NAME`, `exports.a.b` is `b` of `a`.
fn js_member_name(node: Node, src: &[u8]) -> Option<String> {
    let path = js_assigned_path(node, src)?;
    match path.strip_prefix("module.exports.").or_else(|| path.strip_prefix("exports.")) {
        Some(name) => Some(name.to_string()),
        None if path == "module.exports" => None,
        None => Some(path),
    }
}
/// `this.x = function ...`: the member `x` of what the enclosing function or class builds.
pub(crate) fn js_this_member(node: Node, src: &[u8]) -> Option<String> {
    if !JS_FUNCS.contains(&node.child_by_field_name("right")?.kind()) {
        return None;
    }
    let left = node.child_by_field_name("left")?;
    left.child_by_field_name("object").filter(|o| o.kind() == "this")?;
    let key = match left.kind() {
        "member_expression" => left.child_by_field_name("property").filter(|p| p.kind() == "property_identifier")?,
        // `this["k"]` is `this.k`
        "subscript_expression" => left.child_by_field_name("index").filter(|i| i.kind() == "string" && i.named_child_count() == 1)?.named_child(0)?,
        _ => return None,
    };
    let key = text(src, key.start_byte(), key.end_byte());
    (!key.is_empty() && !key.contains(|c: char| c == '.' || c.is_whitespace())).then_some(key)
}
/// The kind of a file's default export written as an expression: `export default function () {}`,
/// `export default class {}`, `module.exports = () => ...`. A declaration is a definition already.
pub(crate) fn js_default_export(node: Node, src: &[u8]) -> Option<&'static str> {
    let kind = match node.kind() {
        "class" => "class",
        k if JS_PROPERTY_FUNCS.contains(&k) => "function",
        _ => return None,
    };
    // the `function` and `class` keywords are nodes too, and finding a parent costs the depth
    if !node.is_named() {
        return None;
    }
    let p = node.parent()?;
    let exported = match p.kind() {
        "export_statement" => p.child_by_field_name("value") == Some(node) && p.children(&mut p.walk()).any(|c| c.kind() == "default"),
        "assignment_expression" => {
            p.child_by_field_name("right") == Some(node)
                && p.child_by_field_name("left").and_then(|l| js_member_path(l, src)).as_deref() == Some("module.exports")
        }
        _ => false,
    };
    exported.then_some(kind)
}
/// The values that make an object literal's property a method: `post: function () {}`, `put: () => 3`.
const JS_PROPERTY_FUNCS: &[&str] = &["arrow_function", "function_expression", "function", "generator_function"];
const JS_PROPERTY_METHOD: Def = Def { value_kinds: JS_PROPERTY_FUNCS, ..d("pair", "method", &[NameRule::ChildKind(&["property_identifier"])]) };
const JS_DEFS: &[Def] = &[
    d("function_declaration", "function", N),
    d("generator_function_declaration", "function", N),
    d("class_declaration", "class", N),
    d("method_definition", "method", N),
    JS_PROPERTY_METHOD,
    dv("variable_declarator", N),
    dv("field_definition", &[NameRule::Field("property")]),
    JS_PROTOTYPE_METHOD,
    JS_MEMBER_FUNCTION,
    JS_THIS_MEMBER,
];
const JS_THIS_MEMBER: Def = d("assignment_expression", "method", &[NameRule::Custom(js_this_member)]);
const JS_PROTOTYPE_METHOD: Def = d("assignment_expression", "method", &[NameRule::Custom(js_prototype_name)]);
const JS_MEMBER_FUNCTION: Def = d("assignment_expression", "function", &[NameRule::Custom(js_member_name)]);
const TS_DEFS: &[Def] = &[
    d("function_declaration", "function", N),
    d("generator_function_declaration", "function", N),
    d("class_declaration", "class", N),
    d("abstract_class_declaration", "class", N),
    d("method_definition", "method", N),
    d("interface_declaration", "interface", N),
    d("type_alias_declaration", "type", N),
    d("enum_declaration", "enum", N),
    JS_PROPERTY_METHOD,
    d("abstract_method_signature", "method", N),
    d("function_signature", "function", N),
    d("internal_module", "module", N),
    dv("variable_declarator", N),
    dv("public_field_definition", N),
    JS_PROTOTYPE_METHOD,
    JS_MEMBER_FUNCTION,
    JS_THIS_MEMBER,
];
const JS_CALLS: &[Call] = &[c("call_expression", "function"), c("new_expression", "constructor")];
// an assignment and its statement: the comment above `Foo.prototype.f = function` is about `f`
const JS_WRAPPERS: &[&str] = &["export_statement", "lexical_declaration", "variable_declaration", "decorator", "assignment_expression", "expression_statement"];
const JS_IMPORTS: &[ImportRule] = &[
    imp("import_statement", ImportHow::Fields("source")),
    imp("import_require_clause", ImportHow::Fields("source")),
    // an exported declaration is an export_statement too, and only `export ... from` has a source
    imp("export_statement", ImportHow::Fields("source")),
    imp_call("call_expression", "function", &["require", "import"], ImportHow::Arg(&["string", "template_string"])),
];
const C_IMPORTS: &[ImportRule] = &[imp("preproc_include", ImportHow::FirstString), imp("module_import", ImportHow::Text)];
const NO_IMPORTS: &[ImportRule] = &[];
const JULIA_MODULES: &[&str] = &["identifier", "scoped_identifier", "import_path", "selected_import", "import_alias"];

const CORE: &[LangSpec] = &[
    LangSpec {
        id: "python",
        exts: &[".py", ".pyi"],
        files: &[],
        language: || tree_sitter_python::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N), d("class_definition", "class", N)],
        calls: &[c("call", "function")],
        docstring: true,
        wrappers: &["decorated_definition"],
        outer: &["decorated_definition"],
        body: "body",
        imports: &[imp("import_statement", ImportHow::Fields("name")), imp("import_from_statement", ImportHow::Fields("module_name"))],
    },
    LangSpec {
        id: "gdscript",
        exts: &[".gd"],
        files: &[],
        language: || tree_sitter_gdscript::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", N),
            d("constructor_definition", "function", &[NameRule::ChildKind(&["_init"])]),
            d("class_definition", "class", N),
            d("class_name_statement", "class", N),
            d("signal_statement", "signal", N),
            d("enum_definition", "enum", N),
            Def { parents: &["source", "class_body"], ..d("variable_statement", "key", N) },
        ],
        calls: &[c("call", ""), c("attribute_call", ""), c("base_call", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "javascript",
        exts: &[".js", ".jsx", ".mjs", ".cjs"],
        files: &[],
        language: || tree_sitter_javascript::LANGUAGE.into(),
        defs: JS_DEFS,
        calls: JS_CALLS,
        docstring: false,
        wrappers: JS_WRAPPERS,
        outer: &[],
        body: "body",
        imports: JS_IMPORTS,
    },
    LangSpec {
        id: "typescript",
        exts: &[".ts", ".mts", ".cts"],
        files: &[],
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        defs: TS_DEFS,
        calls: JS_CALLS,
        docstring: false,
        wrappers: JS_WRAPPERS,
        outer: &[],
        body: "body",
        imports: JS_IMPORTS,
    },
    LangSpec {
        id: "tsx",
        exts: &[".tsx"],
        files: &[],
        language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        defs: TS_DEFS,
        calls: JS_CALLS,
        docstring: false,
        wrappers: JS_WRAPPERS,
        outer: &[],
        body: "body",
        imports: JS_IMPORTS,
    },
    LangSpec {
        id: "go",
        exts: &[".go"],
        files: &[],
        language: || tree_sitter_go::LANGUAGE.into(),
        defs: &[d("function_declaration", "function", N), d("method_declaration", "method", N), d("type_spec", "type", N), d("type_alias", "type", N)],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import_spec", ImportHow::FirstString)],
    },
    LangSpec {
        id: "rust",
        exts: &[".rs"],
        files: &[],
        language: || tree_sitter_rust::LANGUAGE.into(),
        defs: &[
            d("function_item", "function", N),
            d("struct_item", "struct", N),
            d("enum_item", "enum", N),
            d("trait_item", "trait", N),
            d("impl_item", "impl", &[NameRule::Field("name"), NameRule::Field("type")]),
            d("mod_item", "module", N),
            d("type_item", "type", N),
            d("macro_definition", "macro", N),
            d("function_signature_item", "function", N),
        ],
        calls: &[c("call_expression", "function"), c("macro_invocation", "macro")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp(
            "use_declaration",
            ImportHow::ChildKind(&[
                "scoped_identifier",
                "scoped_use_list",
                "identifier",
                "use_as_clause",
                "use_list",
                "use_wildcard",
                "crate",
                "self",
                "super",
            ]),
        )],
    },
    LangSpec {
        id: "java",
        exts: &[".java"],
        files: &[],
        language: || tree_sitter_java::LANGUAGE.into(),
        defs: &[
            d("class_declaration", "class", N),
            d("interface_declaration", "interface", N),
            d("method_declaration", "method", N),
            d("constructor_declaration", "method", N),
            d("enum_declaration", "enum", N),
            d("record_declaration", "class", N),
        ],
        calls: &[c("method_invocation", "name"), c("object_creation_expression", "type")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import_declaration", ImportHow::Text)],
    },
    LangSpec {
        id: "c",
        exts: &[".c", ".h"],
        files: &[],
        language: || tree_sitter_c::LANGUAGE.into(),
        defs: C_DEFS,
        calls: C_CALLS,
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "cpp",
        exts: &[".cc", ".cpp", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h++", ".inl", ".ipp"],
        files: &[],
        language: || tree_sitter_cpp::LANGUAGE.into(),
        defs: CPP_DEFS,
        calls: C_CALLS,
        docstring: false,
        wrappers: &["template_declaration"],
        outer: &["template_declaration"],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "glsl",
        exts: &[".glsl", ".vert", ".frag", ".geom", ".comp", ".tesc", ".tese"],
        files: &[],
        language: || tree_sitter_glsl::LANGUAGE_GLSL.into(),
        defs: C_DEFS,
        calls: C_CALLS,
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "objc",
        exts: &[".mm"],
        files: &[],
        language: || tree_sitter_objc::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", &[NameRule::Dig("declarator", ID_KINDS)]),
            d("class_interface", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier"])]),
            d("class_implementation", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier"])]),
            d("protocol_declaration", "interface", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier"])]),
            d("implementation_definition", "method", &[NameRule::Custom(objc_selector)]),
            d("method_declaration", "method", &[NameRule::Custom(objc_selector)]),
            // a property is a method, as in C#: its accessor `- (int)count` shares the name
            d("property_declaration", "method", &[NameRule::Custom(objc_property_name)]),
            db("struct_specifier", "struct", N),
            db("enum_specifier", "enum", N),
        ],
        calls: &[c("call_expression", "function"), c("message_expression", "method")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "csharp",
        exts: &[".cs"],
        files: &[],
        language: || tree_sitter_c_sharp::LANGUAGE.into(),
        defs: CSHARP_DEFS,
        calls: &[c("invocation_expression", "function"), c("object_creation_expression", "type")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("using_directive", ImportHow::ChildKind(&["identifier", "qualified_name"]))],
    },
    LangSpec {
        id: "ruby",
        exts: &[".rb", ".rake", ".gemspec"],
        files: &["Gemfile", "Rakefile"],
        language: || tree_sitter_ruby::LANGUAGE.into(),
        defs: &[d("method", "function", N), d("singleton_method", "function", N), d("class", "class", N), d("module", "module", N)],
        calls: &[c("call", "method")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp_call("call", "method", &["require", "require_relative", "load"], ImportHow::Arg(&["string"]))],
    },
    LangSpec {
        id: "php",
        exts: &[".php", ".phtml"],
        files: &[],
        language: || tree_sitter_php::LANGUAGE_PHP.into(),
        defs: &[
            d("function_definition", "function", N),
            d("method_declaration", "method", N),
            d("class_declaration", "class", N),
            d("interface_declaration", "interface", N),
            d("trait_declaration", "trait", N),
            d("enum_declaration", "enum", N),
        ],
        calls: &[
            c("function_call_expression", "function"),
            c("member_call_expression", "name"),
            c("nullsafe_member_call_expression", "name"),
            c("scoped_call_expression", "name"),
            c("object_creation_expression", ""),
        ],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[
            imp("namespace_use_declaration", ImportHow::UseClauses),
            imp("require_expression", ImportHow::FirstString),
            imp("require_once_expression", ImportHow::FirstString),
            imp("include_expression", ImportHow::FirstString),
            imp("include_once_expression", ImportHow::FirstString),
        ],
    },
    LangSpec {
        id: "bash",
        exts: &[".sh", ".bash", ".zsh", ".ksh"],
        files: &[".bashrc", ".zshrc", ".profile"],
        language: || tree_sitter_bash::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N)],
        calls: &[c("command", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp_call("command", "name", &["source", "."], ImportHow::ChildKind(&["word", "string", "raw_string"]))],
    },
    LangSpec {
        id: "fish",
        exts: &[".fish"],
        files: &[],
        language: || tree_sitter_fish::language(),
        defs: &[d("function_definition", "function", N)],
        calls: &[c("command", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "powershell",
        exts: &[".ps1", ".psm1", ".psd1"],
        files: &[],
        language: || tree_sitter_powershell::LANGUAGE.into(),
        defs: &[
            d("function_statement", "function", &[NameRule::ChildKind(&["function_name"])]),
            d("class_statement", "class", &[NameRule::ChildKind(&["simple_name"])]),
            d("class_method_definition", "method", &[NameRule::ChildKind(&["simple_name"])]),
        ],
        calls: &[c("command", ""), c("invokation_expression", "member_name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "kotlin",
        exts: &[".kt", ".kts"],
        files: &[],
        language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier", "simple_identifier"])]),
            dk("class_declaration", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier", "type_identifier", "simple_identifier"])]),
            d("object_declaration", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier", "type_identifier", "simple_identifier"])]),
            d("type_alias", "type", &[NameRule::Field("type")]),
        ],
        calls: &[c("call_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import", ImportHow::Text)],
    },
    LangSpec {
        id: "swift",
        exts: &[".swift"],
        files: &[],
        language: || tree_sitter_swift::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", &[NameRule::Field("name"), NameRule::ChildKind(&["simple_identifier"])]),
            dk("class_declaration", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["type_identifier", "user_type"])]),
            d("protocol_declaration", "interface", &[NameRule::Field("name"), NameRule::ChildKind(&["type_identifier"])]),
            d("init_declaration", "method", &[NameRule::ChildKind(&["init"])]),
            d("protocol_function_declaration", "function", N),
        ],
        calls: &[c("call_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import_declaration", ImportHow::ChildKind(&["identifier"]))],
    },
    LangSpec {
        id: "scala",
        exts: &[".scala", ".sc"],
        files: &[],
        language: || tree_sitter_scala::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", N),
            d("function_declaration", "function", N),
            d("class_definition", "class", N),
            d("object_definition", "class", N),
            d("trait_definition", "trait", N),
            d("enum_definition", "enum", N),
        ],
        calls: &[c("call_expression", "function"), c("instance_expression", ""), c("infix_expression", "operator")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import_declaration", ImportHow::Text)],
    },
    LangSpec {
        id: "groovy",
        exts: &[".groovy", ".gradle", ".gvy"],
        files: &["Jenkinsfile"],
        language: || tree_sitter_groovy::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", N),
            d("method_declaration", "method", N),
            d("class_declaration", "class", N),
            d("interface_declaration", "interface", N),
            d("enum_declaration", "enum", N),
        ],
        calls: &[c("method_invocation", "name"), c("juxt_function_call", "name"), c("object_creation_expression", "type")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import", ImportHow::Text), imp("import_declaration", ImportHow::Text)],
    },
    LangSpec {
        id: "lua",
        exts: &[".lua"],
        files: &[],
        language: || tree_sitter_lua::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", N),
            d("assignment_statement", "function", &[NameRule::Custom(lua_assigned_function)]),
            // `parents` skips most anonymous functions before the name rule walks up the tree
            Def { parents: &["expression_list"], ..d("function_definition", "function", &[NameRule::Custom(lua_listed_function)]) },
            Def { value_kinds: &["function_definition"], ..d("field", "function", N) },
        ],
        calls: &[c("function_call", "name")],
        docstring: false,
        wrappers: &["variable_declaration"],
        outer: &[],
        body: "body",
        imports: &[imp_call("function_call", "name", &["require"], ImportHow::Arg(&["string"]))],
    },
    LangSpec {
        id: "elixir",
        exts: &[".ex", ".exs"],
        files: &[],
        language: || tree_sitter_elixir::LANGUAGE.into(),
        defs: &[
            Def {
                node: "call",
                kind: "function",
                name: &[NameRule::Custom(second_symbol)],
                value_kinds: &[],
                head_words: &["def", "defp", "defmacro", "defmacrop", "defguard", "defguardp", "defdelegate"],
                needs_field: "",
                kind_by_head: false,
                parents: &[],
            },
            Def {
                node: "call",
                kind: "module",
                name: &[NameRule::Custom(second_symbol)],
                value_kinds: &[],
                head_words: &["defmodule", "defprotocol", "defimpl"],
                needs_field: "",
                kind_by_head: false,
                parents: &[],
            },
        ],
        calls: &[Call { skip: ELIXIR_SYNTAX, ..c("call", "target") }],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp_call("call", "target", &["alias", "import", "use", "require"], ImportHow::ChildKind(&["arguments"]))],
    },
    LangSpec {
        id: "erlang",
        exts: &[".erl", ".hrl"],
        files: &[],
        language: || tree_sitter_erlang::LANGUAGE.into(),
        defs: &[d("fun_decl", "function", &[NameRule::Custom(erlang_fun_name)]), d("record_decl", "struct", N), d("type_alias", "type", N)],
        calls: &[c("call", "expr")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "haskell",
        exts: &[".hs", ".lhs"],
        files: &[],
        language: || tree_sitter_haskell::LANGUAGE.into(),
        defs: &[
            d("function", "function", &[NameRule::Custom(haskell_name)]),
            d("bind", "function", &[NameRule::Custom(haskell_name)]),
            d("data_type", "type", N),
            d("newtype", "type", N),
            d("type_synonym", "type", N),
            d("class", "class", N),
            d("instance", "impl", N),
        ],
        calls: &[c("apply", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp("import", ImportHow::Text)],
    },
    LangSpec {
        id: "elm",
        exts: &[".elm"],
        files: &[],
        language: || tree_sitter_elm::LANGUAGE.into(),
        defs: &[
            d("value_declaration", "function", &[NameRule::Custom(elm_value_name)]),
            d("type_declaration", "type", N),
            d("type_alias_declaration", "type", N),
        ],
        calls: &[c("function_call_expr", "target")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp("import_clause", ImportHow::Text)],
    },
    LangSpec {
        id: "zig",
        exts: &[".zig", ".zon"],
        files: &[],
        language: || tree_sitter_zig::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", N),
            d("variable_declaration", "type", &[NameRule::Custom(zig_container_name)]),
            d("test_declaration", "function", &[NameRule::ChildKind(&["string", "identifier"])]),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("builtin_function", ImportHow::FirstString)],
    },
    LangSpec {
        id: "julia",
        exts: &[".jl"],
        files: &[],
        language: || tree_sitter_julia::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", &[NameRule::Custom(julia_name)]),
            d("assignment", "function", &[NameRule::Custom(julia_assignment)]),
            d("struct_definition", "struct", &[NameRule::Custom(julia_type_head)]),
            d("abstract_definition", "type", &[NameRule::Custom(julia_type_head)]),
            d("macro_definition", "macro", &[NameRule::Custom(julia_name)]),
            d("module_definition", "module", N),
        ],
        calls: &[c("call_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[
            imp("using_statement", ImportHow::ChildKind(JULIA_MODULES)),
            imp("import_statement", ImportHow::ChildKind(JULIA_MODULES)),
            imp_call("call_expression", "", &["include"], ImportHow::Arg(&["string_literal"])),
        ],
    },
    LangSpec {
        id: "dart",
        exts: &[".dart"],
        files: &[],
        language: || tree_sitter_dart::LANGUAGE.into(),
        defs: &[
            d("class_declaration", "class", N),
            d("function_declaration", "function", &[NameRule::Custom(dart_function_name)]),
            d("function_signature", "function", &[NameRule::Custom(dart_function_name)]),
            d("method_declaration", "method", &[NameRule::Custom(dart_method_name)]),
            d("method_signature", "method", &[NameRule::Custom(dart_method_name)]),
            d("enum_declaration", "enum", N),
            d("mixin_declaration", "trait", N),
            d("extension_declaration", "impl", N),
        ],
        calls: &[c("call_expression", "function"), c("new_expression", "type"), c("const_object_expression", "type"), c("cascade_call_expression", "property")],
        docstring: false,
        wrappers: &["member_declaration"],
        outer: &[],
        body: "body",
        imports: &[imp("import_or_export", ImportHow::FirstString)],
    },
    LangSpec {
        id: "r",
        exts: &[".r", ".R", ".rmd"],
        files: &[],
        language: || tree_sitter_r::LANGUAGE.into(),
        defs: &[d("binary_operator", "function", &[NameRule::Custom(r_assignment)]), d("argument", "method", &[NameRule::Custom(r6_method)])],
        calls: &[c("call", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp_call("call", "function", &["library", "require", "requireNamespace", "source"], ImportHow::Arg(&["identifier", "string"]))],
    },
    LangSpec {
        id: "nix",
        exts: &[".nix"],
        files: &[],
        language: || tree_sitter_nix::LANGUAGE.into(),
        defs: &[d("binding", "key", &[NameRule::Custom(nix_binding)])],
        calls: &[c("apply_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp_call("apply_expression", "function", &["import"], ImportHow::Text)],
    },
    LangSpec {
        id: "hcl",
        exts: &[".tf", ".tfvars", ".hcl", ".nomad"],
        files: &[],
        language: || tree_sitter_hcl::LANGUAGE.into(),
        defs: &[d("block", "block", &[NameRule::Custom(hcl_block_name)])],
        calls: &[c("function_call", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "solidity",
        exts: &[".sol"],
        files: &[],
        language: || tree_sitter_solidity::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", N),
            d("contract_declaration", "class", N),
            d("interface_declaration", "interface", N),
            d("library_declaration", "module", N),
            d("struct_declaration", "struct", N),
            d("event_definition", "type", N),
            d("modifier_definition", "function", N),
            d("enum_declaration", "enum", N),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "pascal",
        exts: &[".pas", ".dpr", ".lpr"],
        files: &[],
        language: || tree_sitter_pascal::LANGUAGE.into(),
        defs: &[
            d("declProc", "function", &[NameRule::Custom(pascal_proc_name)]),
            d("defProc", "function", &[NameRule::Custom(pascal_proc_name)]),
            // a class is a `declType` whose value is a `declClass`, which has no name of its own
            d("declType", "type", N),
        ],
        // a statement that is a name alone calls it: `DoWork;`, `Obj.Tidy;`
        calls: &[c("exprCall", ""), c("statement", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "ada",
        exts: &[".adb", ".ads", ".ada"],
        files: &[],
        language: || tree_sitter_ada::LANGUAGE.into(),
        defs: &[
            d("subprogram_body", "function", &[NameRule::Custom(ada_name)]),
            d("subprogram_declaration", "function", &[NameRule::Custom(ada_name)]),
            d("package_declaration", "module", &[NameRule::Custom(ada_name)]),
            d("package_body", "module", &[NameRule::Custom(ada_name)]),
            d("full_type_declaration", "type", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[c("function_call", ""), c("procedure_call_statement", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "matlab",
        exts: &[],
        files: &[],
        language: || tree_sitter_matlab::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N), d("class_definition", "class", N)],
        calls: &[c("function_call", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "elisp",
        exts: &[".el"],
        files: &[],
        language: || tree_sitter_elisp::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N), d("macro_definition", "macro", N), d("special_form", "key", &[NameRule::Custom(elisp_defvar)])],
        calls: &[c("list", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "scheme",
        exts: &[".scm", ".ss"],
        files: &[],
        language: || tree_sitter_scheme::LANGUAGE.into(),
        defs: &[Def {
            node: "list",
            kind: "function",
            name: &[NameRule::Custom(lisp_define_name)],
            value_kinds: &[],
            head_words: &["define", "define-syntax", "define-record-type"],
            needs_field: "",
            kind_by_head: true,
            parents: &[],
        }],
        calls: &[c("list", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "racket",
        exts: &[".rkt"],
        files: &[],
        language: || tree_sitter_racket::LANGUAGE.into(),
        defs: &[Def {
            node: "list",
            kind: "function",
            name: &[NameRule::Custom(lisp_define_name)],
            value_kinds: &[],
            head_words: &["define", "define-syntax", "define-syntax-rule", "struct", "define-struct"],
            needs_field: "",
            kind_by_head: true,
            parents: &[],
        }],
        calls: &[c("list", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "commonlisp",
        exts: &[".lisp", ".cl", ".asd"],
        files: &[],
        language: || tree_sitter_commonlisp::LANGUAGE_COMMONLISP.into(),
        defs: &[
            dk("defun", "function", &[NameRule::Custom(cl_defun_name)]),
            Def {
                node: "list_lit",
                kind: "function",
                name: &[NameRule::Custom(lisp_define_name)],
                value_kinds: &[],
                head_words: &["defgeneric", "defmethod", "defclass", "defstruct", "defvar", "defparameter", "defconstant", "deftype"],
                needs_field: "",
                kind_by_head: true,
                parents: &[],
            },
        ],
        calls: &[c("list_lit", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "vim",
        exts: &[".vim"],
        files: &[".vimrc"],
        language: || tree_sitter_vim::language(),
        defs: &[d("function_definition", "function", &[NameRule::Custom(vim_fn_name)])],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "gleam",
        exts: &[".gleam"],
        files: &[],
        language: || tree_sitter_gleam::LANGUAGE.into(),
        defs: &[
            d("function", "function", N),
            d("type_definition", "type", &[NameRule::ChildKind(&["type_name"])]),
            d("type_alias", "type", &[NameRule::ChildKind(&["type_name"])]),
            d("external_function", "function", N),
        ],
        calls: &[c("function_call", "function"), c("record", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import", ImportHow::Text)],
    },
    LangSpec {
        id: "starlark",
        exts: &[".bzl", ".star", ".bazel"],
        files: &["BUILD", "WORKSPACE", "BUILD.bazel", "WORKSPACE.bazel", "MODULE.bazel"],
        language: || tree_sitter_starlark::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N)],
        calls: &[c("call", "function")],
        docstring: true,
        wrappers: &["decorated_definition"],
        outer: &["decorated_definition"],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "cmake",
        exts: &[".cmake"],
        files: &["CMakeLists.txt"],
        language: || tree_sitter_cmake::LANGUAGE.into(),
        defs: &[d("function_def", "function", &[NameRule::Custom(cmake_def_name)]), d("macro_def", "macro", &[NameRule::Custom(cmake_def_name)])],
        calls: &[c("normal_command", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "make",
        exts: &[".mk", ".mak"],
        files: &["Makefile", "makefile", "GNUmakefile"],
        language: || tree_sitter_make::LANGUAGE.into(),
        defs: &[d("rule", "function", &[NameRule::ChildKind(&["targets"])]), d("variable_assignment", "key", N), d("define_directive", "macro", N)],
        // a `define` body is raw text the grammar does not parse: its calls are read from the text
        calls: &[c("function_call", "arguments"), c("raw_text", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "graphql",
        exts: &[".graphql", ".gql", ".graphqls"],
        files: &[],
        language: || tree_sitter_graphql::LANGUAGE.into(),
        defs: &[
            d("object_type_definition", "type", &[NameRule::ChildKind(&["name"])]),
            d("interface_type_definition", "interface", &[NameRule::ChildKind(&["name"])]),
            d("enum_type_definition", "enum", &[NameRule::ChildKind(&["name"])]),
            d("input_object_type_definition", "type", &[NameRule::ChildKind(&["name"])]),
            d("union_type_definition", "type", &[NameRule::ChildKind(&["name"])]),
            d("scalar_type_definition", "type", &[NameRule::ChildKind(&["name"])]),
            d("operation_definition", "function", &[NameRule::ChildKind(&["name"])]),
            d("fragment_definition", "function", &[NameRule::ChildKind(&["fragment_name"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "prisma",
        exts: &[".prisma"],
        files: &[],
        language: || tree_sitter_prisma::LANGUAGE.into(),
        defs: &[
            d("model_declaration", "class", &[NameRule::ChildKind(&["identifier"])]),
            d("enum_declaration", "enum", &[NameRule::ChildKind(&["identifier"])]),
            d("datasource_declaration", "block", &[NameRule::ChildKind(&["identifier"])]),
            d("generator_declaration", "block", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "puppet",
        exts: &[],
        files: &[],
        language: || tree_sitter_puppet::LANGUAGE.into(),
        defs: &[
            d("class_definition", "class", &[NameRule::ChildKind(&["identifier", "class_identifier"])]),
            d("defined_resource_type", "function", &[NameRule::ChildKind(&["class_identifier", "identifier"])]),
            d("node_definition", "block", &[NameRule::ChildKind(&["node_name"])]),
            d("function_declaration", "function", &[NameRule::ChildKind(&["identifier", "class_identifier"])]),
        ],
        calls: &[c("function_call", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "bicep",
        exts: &[".bicep"],
        files: &[],
        language: || tree_sitter_bicep::LANGUAGE.into(),
        defs: &[
            d("resource_declaration", "block", &[NameRule::ChildKind(&["identifier"])]),
            d("module_declaration", "module", &[NameRule::ChildKind(&["identifier"])]),
            d("parameter_declaration", "type", &[NameRule::ChildKind(&["identifier"])]),
            d("variable_declaration", "type", &[NameRule::ChildKind(&["identifier"])]),
            d("output_declaration", "type", &[NameRule::ChildKind(&["identifier"])]),
            d("user_defined_function", "function", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "asm",
        exts: &[".s", ".S", ".asm"],
        files: &[],
        language: || tree_sitter_asm::LANGUAGE.into(),
        defs: &[d("label", "function", &[NameRule::ChildKind(&["ident", "word", "identifier"])])],
        // the instructions after a label are its siblings: parse::asm_block gives them to the label
        calls: &[c("instruction", "kind")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "jsonnet",
        exts: &[".jsonnet", ".libsonnet"],
        files: &[],
        language: || tree_sitter_jsonnet::LANGUAGE.into(),
        defs: &[
            d("bind", "function", &[NameRule::Custom(jsonnet_function)]),
            d("bind", "key", &[NameRule::ChildKind(&["ident", "id"])]),
            d("field", "method", &[NameRule::Custom(jsonnet_function)]),
        ],
        // the callee is the sibling before the arguments: see parse::sibling_callee
        calls: &[c("suffix_apply", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "ini",
        exts: &[".ini", ".cfg", ".conf", ".properties", ".editorconfig", ".gitconfig"],
        files: &[".gitconfig", ".npmrc", "setup.cfg"],
        language: || tree_sitter_ini::LANGUAGE.into(),
        defs: &[d("section", "block", &[NameRule::ChildKind(&["section_name"])])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "toml",
        exts: &[".toml"],
        files: &["Pipfile"],
        language: || tree_sitter_toml_ng::LANGUAGE.into(),
        defs: &[
            d("table", "block", &[NameRule::ChildKind(&["bare_key", "dotted_key", "quoted_key"])]),
            d("table_array_element", "block", &[NameRule::ChildKind(&["bare_key", "dotted_key", "quoted_key"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "yaml",
        exts: &[".yaml", ".yml"],
        files: &[],
        language: || tree_sitter_yaml::LANGUAGE.into(),
        defs: &[d("block_mapping_pair", "key", &[NameRule::Field("key")])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "value",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "json",
        exts: &[".json", ".jsonc", ".json5", ".webmanifest"],
        files: &[],
        language: || tree_sitter_json::LANGUAGE.into(),
        defs: &[d("pair", "key", &[NameRule::Field("key")])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "value",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "xml",
        exts: &[".xml", ".xsd", ".xsl", ".xslt", ".svg", ".plist", ".csproj", ".pom"],
        files: &["pom.xml"],
        language: || tree_sitter_xml::LANGUAGE_XML.into(),
        defs: &[],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "html",
        exts: &[".html", ".htm", ".xhtml"],
        files: &[],
        language: || tree_sitter_html::LANGUAGE.into(),
        defs: &[],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "css",
        exts: &[".css", ".scss", ".less"],
        files: &[],
        language: || tree_sitter_css::LANGUAGE.into(),
        defs: &[
            d("rule_set", "block", &[NameRule::ChildKind(&["selectors"])]),
            d("keyframes_statement", "block", &[NameRule::ChildKind(&["keyframes_name"])]),
            d("media_statement", "block", &[NameRule::ChildKind(&["feature_query", "keyword_query", "binary_query"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "block",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "markdown",
        exts: &[".md", ".markdown", ".mdx"],
        files: &["README"],
        language: || tree_sitter_md::LANGUAGE.into(),
        defs: &[d("section", "section", &[NameRule::Custom(heading_text)])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "rst",
        exts: &[".rst"],
        files: &[],
        language: || tree_sitter_rst::LANGUAGE.into(),
        defs: &[d("section", "section", &[NameRule::Custom(heading_text)])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
];
// ERB/EJS templates are deliberately not in the registry: the embedded-template grammar only
// delimits directives, so the keyword heuristics (which see the embedded `def`s) do better.

/// D declarations carry the name as their first identifier child (no `name` field).
#[cfg(feature = "extra-langs")]
const D_NAME: &[NameRule] = &[NameRule::Field("name"), NameRule::ChildKind(&["identifier"])];

/// LLVM IR: `define i32 @f(...)` keeps the name in the header's `name` field.
#[cfg(feature = "extra-langs")]
fn llvm_fn_name(n: Node, src: &[u8]) -> Option<String> {
    let mut cur = n.walk();
    let header = n.children(&mut cur).find(|c| c.kind() == "function_header")?;
    let name = header.child_by_field_name("name")?;
    std::str::from_utf8(src.get(name.start_byte()..name.end_byte())?).ok().map(|s| s.trim_start_matches('@').to_string())
}

#[cfg(feature = "extra-langs")]
fn llvm_type_name(n: Node, src: &[u8]) -> Option<String> {
    child_text(n, src, &["local_var"]).map(|s| s.trim_start_matches('%').to_string())
}

#[cfg(feature = "extra-langs")]
const QML_IMPORTS: &[ImportRule] = &[
    imp("ui_import", ImportHow::Fields("source")),
    imp("import_statement", ImportHow::Fields("source")),
    imp("export_statement", ImportHow::Fields("source")),
    imp_call("call_expression", "function", &["require", "import"], ImportHow::Arg(&["string", "template_string"])),
];

#[cfg(feature = "extra-langs")]
const EXTRA: &[LangSpec] = &[
    LangSpec {
        id: "cuda",
        exts: &[".cu", ".cuh"],
        files: &[],
        language: || tree_sitter_cuda::LANGUAGE.into(),
        defs: CPP_DEFS,
        calls: C_CALLS,
        docstring: false,
        wrappers: &["template_declaration"],
        outer: &["template_declaration"],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "ocaml",
        exts: &[".ml"],
        files: &[],
        language: || tree_sitter_ocaml::LANGUAGE_OCAML.into(),
        defs: &[
            d("let_binding", "function", &[NameRule::Field("pattern")]),
            d("type_binding", "type", N),
            d("module_binding", "module", &[NameRule::ChildKind(&["module_name"])]),
            d("class_binding", "class", &[NameRule::ChildKind(&["class_name"])]),
        ],
        calls: &[c("application_expression", "function")],
        docstring: false,
        wrappers: &["value_definition", "type_definition", "module_definition"],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "ocaml_interface",
        exts: &[".mli"],
        files: &[],
        language: || tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(),
        defs: &[
            d("value_specification", "function", &[NameRule::ChildKind(&["value_name"])]),
            d("type_binding", "type", N),
            // the interface grammar has no module_declaration: `module M : sig` is a module_binding
            d("module_binding", "module", &[NameRule::ChildKind(&["module_name"])]),
            d("module_type_definition", "interface", &[NameRule::ChildKind(&["module_type_name"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &["type_definition", "module_definition"],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "fsharp",
        exts: &[".fs", ".fsx", ".fsi"],
        files: &[],
        language: || tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        defs: &[
            d("function_or_value_defn", "function", &[NameRule::Custom(fsharp_name)]),
            d("type_definition", "type", &[NameRule::Custom(fsharp_type_name)]),
            d("module_defn", "module", &[NameRule::ChildKind(&["identifier", "long_identifier"])]),
        ],
        calls: &[c("application_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "fortran",
        exts: &[".f", ".f90", ".f95", ".f03", ".f08", ".for", ".F", ".F90"],
        files: &[],
        language: || tree_sitter_fortran::LANGUAGE.into(),
        defs: &[
            d("function", "function", &[NameRule::Custom(fortran_name)]),
            d("subroutine", "function", &[NameRule::Custom(fortran_name)]),
            d("module", "module", &[NameRule::Custom(fortran_name)]),
            d("program", "module", &[NameRule::Custom(fortran_name)]),
            d("derived_type_definition", "type", &[NameRule::Custom(fortran_name)]),
        ],
        calls: &[c("call_expression", ""), c("subroutine_call", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "d",
        exts: &[".d", ".di"],
        files: &[],
        language: || tree_sitter_d::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", D_NAME),
            d("class_declaration", "class", D_NAME),
            d("struct_declaration", "struct", D_NAME),
            d("interface_declaration", "interface", D_NAME),
            d("enum_declaration", "enum", D_NAME),
            d("template_declaration", "macro", D_NAME),
        ],
        calls: &[c("call_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "verilog",
        exts: &[".v", ".sv", ".svh", ".vh"],
        files: &[],
        language: || tree_sitter_verilog::LANGUAGE.into(),
        defs: &[
            d("module_declaration", "module", &[NameRule::Custom(verilog_module_name)]),
            d("function_declaration", "function", &[NameRule::Custom(verilog_fn_name)]),
            d("task_declaration", "function", &[NameRule::Custom(verilog_fn_name)]),
            d("class_declaration", "class", &[NameRule::ChildKind(&["class_identifier", "simple_identifier"])]),
        ],
        // a bare task call `h(1);` or `h;` parses as a typeless declaration of `h`, and `h(1, 2);`
        // as a checker instance without a name
        calls: &[c("tf_call", ""), c("module_instantiation", "simple_identifier"), c("checker_instantiation", "checker_identifier"), c("data_declaration", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "vhdl",
        exts: &[".vhd", ".vhdl"],
        files: &[],
        language: || tree_sitter_vhdl::LANGUAGE.into(),
        defs: &[
            d("entity_declaration", "class", &[NameRule::ChildKind(&["identifier"])]),
            d("architecture_definition", "impl", &[NameRule::Field("architecture")]),
            d("subprogram_definition", "function", &[NameRule::Custom(vhdl_name)]),
            d("package_declaration", "module", &[NameRule::ChildKind(&["identifier"])]),
            d("package_definition", "module", &[NameRule::Field("package")]),
        ],
        // `f(x)` and `a(i)` are one shape in VHDL: parse.rs drops the names the file declares as objects
        calls: &[c("name", ""), c("component_instantiation_statement", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "odin",
        exts: &[".odin"],
        files: &[],
        language: || tree_sitter_odin::LANGUAGE.into(),
        defs: &[
            d("procedure_declaration", "function", &[NameRule::ChildKind(&["identifier"])]),
            d("struct_declaration", "struct", &[NameRule::ChildKind(&["identifier"])]),
            d("enum_declaration", "enum", &[NameRule::ChildKind(&["identifier"])]),
            d("union_declaration", "struct", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "llvm",
        exts: &[".ll"],
        files: &[],
        language: || tree_sitter_llvm::LANGUAGE.into(),
        defs: &[d("fn_define", "function", &[NameRule::Custom(llvm_fn_name)]), d("global_type", "type", &[NameRule::Custom(llvm_type_name)])],
        calls: &[c("instruction_call", "callee")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "qml",
        exts: &[".qml"],
        files: &[],
        language: || tree_sitter_qmljs::LANGUAGE.into(),
        defs: &[d("ui_object_definition", "class", &[NameRule::Field("type_name")]), d("function_declaration", "function", N), d("ui_property", "type", N)],
        calls: JS_CALLS,
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: QML_IMPORTS,
    },
    LangSpec {
        id: "tlaplus",
        exts: &[".tla"],
        files: &[],
        language: || tree_sitter_tlaplus::LANGUAGE.into(),
        defs: &[d("operator_definition", "function", N), d("module", "module", N), d("function_definition", "function", N)],
        calls: &[c("bound_op", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "autohotkey",
        exts: &[".ahk"],
        files: &[],
        language: || tree_sitter_autohotkey::LANGUAGE.into(),
        defs: &[d("function_declaration", "function", N), d("class_declaration", "class", N), d("method_declaration", "method", N)],
        calls: &[c("function_call", "function"), c("call_statement", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
];

/// All languages this build knows (the `extra-langs` feature adds the heavy grammars).
pub static LANGS: std::sync::LazyLock<Vec<&'static LangSpec>> = std::sync::LazyLock::new(|| {
    #[allow(unused_mut)]
    let mut v: Vec<&'static LangSpec> = CORE.iter().collect();
    #[cfg(feature = "extra-langs")]
    v.extend(EXTRA.iter());
    v
});

// ---- custom name extractors (one per grammar quirk) --------------------------------------------

fn child_text(node: Node, src: &[u8], kinds: &[&str]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if kinds.contains(&ch.kind()) {
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

/// `M.a.b` of a Lua assignment target made of names, `M["b"]` read as `M.b`.
pub(crate) fn lua_target(node: Node, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text(src, node.start_byte(), node.end_byte())),
        "dot_index_expression" | "bracket_index_expression" => {
            let mut field = node.child_by_field_name("field")?;
            if node.kind() == "bracket_index_expression" {
                field = field.child_by_field_name("content").filter(|_| field.kind() == "string")?;
            }
            Some(format!("{}.{}", lua_target(node.child_by_field_name("table")?, src)?, text(src, field.start_byte(), field.end_byte())))
        }
        _ => None,
    }
}

/// The one value and the one target of a Lua assignment (`local f = ...`, `M.f = ...`).
pub(crate) fn lua_single_assignment<'t>(node: Node<'t>) -> Option<(Node<'t>, Node<'t>)> {
    let one = |kind: &str| node.named_children(&mut node.walk()).find(|c| c.kind() == kind).filter(|l| l.named_child_count() == 1)?.named_child(0);
    Some((one("variable_list")?, one("expression_list")?))
}

/// Lua `local f = function ...`, `M.f = function ...`, `M["f"] = function ...`
fn lua_assigned_function(node: Node, src: &[u8]) -> Option<String> {
    let (target, value) = lua_single_assignment(node)?;
    (value.kind() == "function_definition").then(|| lua_target(target, src))?
}

/// A function in a multiple assignment `local a, M.b = function ... end, function ... end`: named
/// by the target in its position.
fn lua_listed_function(node: Node, src: &[u8]) -> Option<String> {
    let values = node.parent().filter(|p| p.kind() == "expression_list")?;
    let assignment = values.parent().filter(|a| a.kind() == "assignment_statement")?;
    if lua_single_assignment(assignment).is_some() {
        return None;
    }
    let at = values.named_children(&mut values.walk()).position(|v| v == node)?;
    let targets = assignment.named_children(&mut assignment.walk()).find(|c| c.kind() == "variable_list")?;
    lua_target(targets.named_child(at as u32)?, src)
}

/// Jsonnet `local f(x) = ...` and the object method `f(x): ...`: a binding with parameters.
fn jsonnet_function(node: Node, src: &[u8]) -> Option<String> {
    node.children(&mut node.walk()).any(|c| c.kind() == "params").then(|| child_text(node, src, &["ident", "field_name"]))?
}

fn erlang_fun_name(node: Node, src: &[u8]) -> Option<String> {
    // fun_decl > function_clause > (name: atom)
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "function_clause" {
            if let Some(n) = ch.child_by_field_name("name") {
                return Some(text(src, n.start_byte(), n.end_byte()));
            }
        }
    }
    None
}

fn nix_binding(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("attrpath").map(|n| text(src, n.start_byte(), n.end_byte()))
}

#[cfg(feature = "extra-langs")]
fn fsharp_name(node: Node, src: &[u8]) -> Option<String> {
    // function_or_value_defn > function_declaration_left > identifier
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "function_declaration_left" | "value_declaration_left") {
            return child_text(ch, src, &["identifier", "identifier_pattern", "long_identifier"])
                .or_else(|| ch.named_child(0).map(|n| text(src, n.start_byte(), n.end_byte())));
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn fsharp_type_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind().starts_with("type_") || ch.kind().ends_with("_type_defn") {
            if let Some(n) = ch.child_by_field_name("type_name") {
                return Some(text(src, n.start_byte(), n.end_byte()));
            }
            return child_text(ch, src, &["type_name", "identifier"]);
        }
    }
    None
}

fn elm_value_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "function_declaration_left" {
            return child_text(ch, src, &["lower_case_identifier"]);
        }
    }
    None
}

fn zig_container_name(node: Node, src: &[u8]) -> Option<String> {
    // `const Name = struct { ... }` : only when the value is a container
    let mut cur = node.walk();
    let mut has_container = false;
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "struct_declaration" | "enum_declaration" | "union_declaration" | "opaque_declaration") {
            has_container = true;
        }
    }
    if !has_container {
        return None;
    }
    child_text(node, src, &["identifier"])
}

fn julia_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "signature" {
            // signature > call_expression > identifier (or a dotted / typed callee)
            let mut inner = ch.named_child(0)?;
            // `function Base.show(...)` keeps its qualifier: the owner comes from it
            while !matches!(inner.kind(), "identifier" | "field_expression") && inner.named_child_count() > 0 {
                inner = inner.named_child(0)?;
            }
            return Some(julia_unquote(text(src, inner.start_byte(), inner.end_byte())));
        }
        if ch.kind() == "identifier" {
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

/// Julia struct / abstract type: the identifier inside `type_head`
fn julia_type_head(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "type_head" {
            return child_text(ch, src, &["identifier"]).or_else(|| Some(text(src, ch.start_byte(), ch.end_byte())));
        }
    }
    None
}

/// Julia `f(x) = ...` (an assignment whose left side is a call)
fn julia_assignment(node: Node, src: &[u8]) -> Option<String> {
    let lhs = node.named_child(0)?;
    if lhs.kind() != "call_expression" {
        return None;
    }
    let f = lhs.named_child(0)?;
    Some(julia_unquote(text(src, f.start_byte(), f.end_byte())))
}

/// `Base.:(==)` is `Base.:==`: the parentheses only quote the operator.
fn julia_unquote(name: String) -> String {
    match name.split_once(".:(").and_then(|(owner, op)| Some((owner, op.strip_suffix(')')?))) {
        Some((owner, op)) => format!("{owner}.:{op}"),
        None => name,
    }
}

/// Dart: a function_signature directly inside a method_signature is the method itself (already a chunk)
/// A Dart function is named by its signature; a signature inside a declaration is not a second symbol.
fn dart_function_name(node: Node, src: &[u8]) -> Option<String> {
    if node.parent().is_some_and(|p| matches!(p.kind(), "method_signature" | "function_declaration")) {
        return None;
    }
    let node = if node.kind() == "function_declaration" { node.child_by_field_name("signature")? } else { node };
    node.child_by_field_name("name").map(|n| text(src, n.start_byte(), n.end_byte()))
}

fn dart_method_name(node: Node, src: &[u8]) -> Option<String> {
    if node.parent().is_some_and(|p| p.kind() == "method_declaration") {
        return None;
    }
    let node = if node.kind() == "method_declaration" { node.child_by_field_name("signature")? } else { node };
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "function_signature" | "getter_signature" | "setter_signature" | "constructor_signature" | "factory_constructor_signature") {
            return ch.child_by_field_name("name").map(|n| text(src, n.start_byte(), n.end_byte())).or_else(|| child_text(ch, src, &["identifier"]));
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn fortran_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind().ends_with("_statement") {
            if let Some(n) = ch.child_by_field_name("name") {
                return Some(text(src, n.start_byte(), n.end_byte()));
            }
            return child_text(ch, src, &["name", "identifier"]);
        }
    }
    None
}

fn hcl_block_name(node: Node, src: &[u8]) -> Option<String> {
    // block: identifier string_lit* { ... }  -> "resource aws_instance web"
    let mut parts = Vec::new();
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        match ch.kind() {
            "identifier" => parts.push(text(src, ch.start_byte(), ch.end_byte())),
            "string_lit" => parts.push(text(src, ch.start_byte(), ch.end_byte()).trim_matches('"').to_string()),
            "block_start" | "body" | "{" => break,
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

/// A Pascal procedure with a body is named by its header; the header is not a second symbol.
fn pascal_proc_name(node: Node, src: &[u8]) -> Option<String> {
    if node.parent().is_some_and(|p| p.kind() == "defProc") {
        return None;
    }
    let node = node.child_by_field_name("header").unwrap_or(node);
    node.child_by_field_name("name")
        .map(|n| text(src, n.start_byte(), n.end_byte()))
        .or_else(|| child_text(node, src, &["identifier", "genericDot", "procName"]))
}

fn ada_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "procedure_specification" | "function_specification") {
            return ch
                .child_by_field_name("name")
                .map(|n| text(src, n.start_byte(), n.end_byte()))
                .or_else(|| child_text(ch, src, &["identifier", "selected_component"]));
        }
        if ch.kind() == "identifier" {
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn verilog_module_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "module_header" | "module_nonansi_header" | "module_ansi_header") {
            return child_text(ch, src, &["simple_identifier", "module_identifier"]).or_else(|| {
                let mut c2 = ch.walk();
                for g in ch.children(&mut c2) {
                    if g.kind() == "module_identifier" || g.kind() == "simple_identifier" {
                        return Some(text(src, g.start_byte(), g.end_byte()));
                    }
                }
                None
            });
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn verilog_fn_name(node: Node, src: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "function_identifier" | "task_identifier") {
            return Some(text(src, n.start_byte(), n.end_byte()));
        }
        let mut cur = n.walk();
        let kids: Vec<Node> = n.children(&mut cur).collect();
        for k in kids.into_iter().rev() {
            if matches!(k.kind(), "function_body_declaration" | "task_body_declaration" | "function_identifier" | "task_identifier") {
                stack.push(k);
            }
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn vhdl_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "function_specification" | "procedure_specification") {
            return child_text(ch, src, &["identifier", "operator_symbol"]);
        }
        if ch.kind() == "identifier" {
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

fn lisp_define_name(node: Node, src: &[u8]) -> Option<String> {
    // (define (name args) ...) | (define name ...) | (defun name ...)
    let mut cur = node.walk();
    let mut seen = 0;
    for ch in node.children(&mut cur) {
        if !ch.is_named() {
            continue;
        }
        seen += 1;
        if seen == 2 {
            if matches!(ch.kind(), "list" | "list_lit") {
                return ch.named_child(0).map(|n| text(src, n.start_byte(), n.end_byte()));
            }
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

/// C++ member function declared in a class body (`void resize(int w);`), not a data member or a
/// function pointer
fn cpp_member_decl(node: Node, src: &[u8]) -> Option<String> {
    let mut f = node.child_by_field_name("declarator")?;
    // `T& operator[](int)`, `Node* next()`: the return type's mark wraps the function declarator
    while matches!(f.kind(), "pointer_declarator" | "reference_declarator") {
        f = f.child_by_field_name("declarator").or_else(|| f.named_child(f.named_child_count().checked_sub(1)? as u32))?;
    }
    if f.kind() == "operator_cast" {
        // `operator bool() const`: the name is the text before the parameter list
        let params = f.child_by_field_name("declarator")?;
        return Some(text(src, f.start_byte(), params.start_byte()).split_whitespace().collect::<Vec<_>>().join(" "));
    }
    let n = f.child_by_field_name("declarator").filter(|n| matches!(n.kind(), "field_identifier" | "identifier" | "destructor_name" | "operator_name"))?;
    (f.kind() == "function_declarator").then(|| text(src, n.start_byte(), n.end_byte()))
}

/// Objective-C method implementation or declaration: the selector (first keyword part or unary selector)
fn objc_selector(node: Node, src: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "method_selector" | "unary_selector" | "keyword_selector") {
            let t = text(src, n.start_byte(), n.end_byte());
            let first = t.split(':').next().unwrap_or(&t).split_whitespace().last().unwrap_or("").to_string();
            return if first.is_empty() { None } else { Some(first) };
        }
        if n.kind() == "identifier" && n.parent().is_some_and(|p| matches!(p.kind(), "implementation_definition" | "method_definition" | "method_declaration"))
        {
            return Some(text(src, n.start_byte(), n.end_byte()));
        }
        let mut cur = n.walk();
        let kids: Vec<Node> = n.children(&mut cur).collect();
        for k in kids.into_iter().rev() {
            if !matches!(k.kind(), "compound_statement") {
                stack.push(k);
            }
        }
    }
    None
}

/// The first name of `@property (nonatomic) NSString *name;`
fn objc_property_name(node: Node, src: &[u8]) -> Option<String> {
    let decl = node.named_children(&mut node.walk()).find(|c| c.kind() == "struct_declaration")?;
    let mut n = decl.named_children(&mut decl.walk()).find(|c| c.kind() == "struct_declarator")?;
    while n.kind() != "identifier" {
        n = n
            .child_by_field_name("declarator")
            .or_else(|| n.named_children(&mut n.walk()).find(|c| c.kind().ends_with("declarator") || c.kind() == "identifier"))?;
    }
    Some(text(src, n.start_byte(), n.end_byte()))
}

/// Elisp `(defvar name ...)` / `(defcustom name ...)`: special_form whose first symbol is a def* word
fn elisp_defvar(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    let syms: Vec<Node> = node.children(&mut cur).filter(|c| c.kind() == "symbol").collect();
    if syms.len() < 2 {
        return None;
    }
    let head = text(src, syms[0].start_byte(), syms[0].end_byte());
    if matches!(head.as_str(), "defvar" | "defcustom" | "defconst" | "defgroup" | "defface") {
        Some(text(src, syms[1].start_byte(), syms[1].end_byte()))
    } else {
        None
    }
}

/// Common Lisp `defun` node: defun_header > function_name
fn cl_defun_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "defun_header" {
            return ch.child_by_field_name("function_name").map(|n| text(src, n.start_byte(), n.end_byte()));
        }
    }
    None
}

fn vim_fn_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "function_declaration" {
            return child_text(ch, src, &["identifier", "scoped_identifier", "field_expression"]);
        }
    }
    None
}

fn cmake_def_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "function_command" | "macro_command") {
            let mut c2 = ch.walk();
            for g in ch.children(&mut c2) {
                if g.kind() == "argument_list" {
                    return child_text(g, src, &["argument"]);
                }
            }
        }
    }
    None
}

/// The first word of a node's text (for `head_words` matching).
pub fn head_word(node: Node, src: &[u8]) -> String {
    let t = text(src, node.start_byte(), crate::parse::first_line_end(src, node.start_byte(), node.end_byte()));
    t.trim_start_matches(['(', ' ', '\t']).split(|c: char| c.is_whitespace() || c == '(').next().unwrap_or("").to_string()
}

pub fn by_id(id: &str) -> Option<&'static LangSpec> {
    LANGS.iter().find(|s| s.id == id).copied()
}

/// A line Objective-C starts with a keyword of its own; a Doxygen `@class` in a C++ comment is
/// not one.
static OBJC_HEADER: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"(?m)^[ \t]*@(?:interface|protocol|implementation|class)\b").unwrap());

/// The first statement of a Pascal unit, program or library: `unit unit1;`.
static PASCAL_HEADER: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"(?im)^[ \t]*(?:unit|program|library)[ \t]+[\w.]+[ \t]*;").unwrap());

/// Extensions shared by several languages are decided by looking at the text.
/// Lines only C++ writes: what tells a C++ header named `.h` from a C one.
static CPP_HEADER: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r#"(?m)^[ \t]*(?:namespace\b[^;]*\{|namespace\s+\w[\w:]*\s*$|class\s+\w+|template\s*<|using\s+namespace\b|(?:public|private|protected)\s*:|extern\s+"C\+\+"|#\s*include\s*<[a-z_]+>)"#,
    )
    .unwrap()
});

/// The languages `parse::constant_of` reads constants in.
pub const CONSTANT_LANGS: &[&str] = &["c", "cpp", "gdscript", "glsl", "go", "java", "javascript", "objc", "python", "ruby", "rust", "tsx", "typescript"];

/// The definition kinds the index gives a language: the kinds of its rules, `method` for a
/// function in a class, `constant` where constants are read, `field` in Java, and for a rule that takes its kind
/// from the keyword at the head of the definition, the kinds of the keywords the language has.
pub fn definition_kinds(spec: &LangSpec) -> Vec<&'static str> {
    let mut kinds: Vec<&str> = spec.defs.iter().map(|d| d.kind).collect();
    if kinds.contains(&"function") && (kinds.contains(&"class") || spec.id == "cpp") {
        kinds.push("method");
    }
    if CONSTANT_LANGS.contains(&spec.id) {
        kinds.push("constant");
    }
    if spec.id == "java" {
        kinds.push("field");
    }
    // a rule that lists its head keywords takes the kind of each; the three that do not list
    // them read `interface`, `enum class`, `struct`, `extension` and `defmacro` off the head
    for d in spec.defs.iter().filter(|d| d.kind_by_head) {
        kinds.extend(d.head_words.iter().filter_map(|w| KIND_WORDS.iter().find(|(kw, _)| kw == w).map(|(_, k)| *k)));
    }
    kinds.extend(match spec.id {
        "kotlin" => &["interface", "enum"][..],
        "swift" => &["struct", "enum", "impl"][..],
        "commonlisp" => &["macro"][..],
        _ => &[][..],
    });
    kinds.sort();
    kinds.dedup();
    kinds
}

/// The extensions `sniff` reads the text of, with the languages it picks between.
pub const BY_CONTENT: &[(&str, &[&str])] =
    &[(".m", &["objc", "matlab"]), (".h", &["objc", "cpp"]), (".pp", &["pascal", "puppet"]), (".cls", &["latex", "apex"])];

pub fn sniff(path: &str, source: &str) -> Option<&'static LangSpec> {
    let lower = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let head = crate::text::prefix_bytes(source, 4000);
    let id = if lower.ends_with(".m") {
        if ["@interface", "@implementation", "#import", "@end", "@protocol"].iter().any(|k| head.contains(k)) {
            "objc"
        } else {
            "matlab"
        }
    } else if lower.ends_with(".h") {
        if OBJC_HEADER.is_match(source) {
            "objc"
        } else if CPP_HEADER.is_match(source) {
            "cpp"
        } else {
            return None;
        }
    } else if lower.ends_with(".pp") {
        // a unit or program header is Pascal whatever directives follow (`{$mode objfpc}`); else
        // a `$` is taken for a Puppet variable
        let pascal_words = ["procedure ", "function ", "begin", "end;", "unit ", "program "].iter().any(|k| source.contains(k));
        if PASCAL_HEADER.is_match(head) || (pascal_words && !head.contains("$")) {
            "pascal"
        } else {
            "puppet"
        }
    } else if lower.ends_with(".cls") {
        if head.contains("\\NeedsTeXFormat") || head.contains("\\ProvidesClass") {
            "latex"
        } else {
            "apex"
        }
    } else {
        return None;
    };
    by_id(id)
}

/// Whether a definition in language `lang` is named in any case: Fortran, Pascal and Ada names,
/// and PHP functions, methods, classes, interfaces, traits and enums (not constants or properties).
pub fn ignores_case(lang: Option<&str>, kind: &str) -> bool {
    match lang {
        Some("fortran" | "pascal" | "ada") => true,
        Some("php") => matches!(kind, "function" | "method" | "class" | "interface" | "trait" | "enum"),
        _ => false,
    }
}

/// The grammar of the repository file `rel` as the parser takes it: by its name, and by its text
/// where its extension is shared by several languages (`.pp`, `.h`, `.m`, `.cls`).
pub fn detect_at(root: &std::path::Path, rel: &str) -> Option<&'static LangSpec> {
    let lower = rel.to_lowercase();
    let shared = BY_CONTENT.iter().any(|(e, _)| lower.ends_with(e));
    let text = shared.then(|| crate::resolve::repo_text(root, rel)).flatten();
    detect(rel, text.as_deref())
}

/// The grammar of `path`: by its name, and by `text` where the extension is shared by several
/// languages. The one way the parser, the resolver and the answers about names tell a language.
pub fn detect(path: &str, text: Option<&str>) -> Option<&'static LangSpec> {
    text.and_then(|t| sniff(path, t)).or_else(|| spec_for(path))
}

pub fn spec_for(path: &str) -> Option<&'static LangSpec> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let lower = base.to_lowercase();
    for spec in LANGS.iter() {
        if spec.files.iter().any(|f| f.eq_ignore_ascii_case(base)) {
            return Some(*spec);
        }
    }
    // longest matching extension first (".d.ts" style suffixes are listed explicitly)
    let mut best: Option<(&'static LangSpec, usize)> = None;
    for spec in LANGS.iter() {
        for ext in spec.exts {
            let e = ext.to_lowercase();
            if lower.ends_with(&e) && lower.len() > e.len() && best.map(|(_, l)| e.len() > l).unwrap_or(true) {
                best = Some((*spec, e.len()));
            }
        }
    }
    // case-sensitive extensions (.R vs .r, .S vs .s) are the same grammar here, so lowercase is fine
    best.map(|(s, _)| s)
}
