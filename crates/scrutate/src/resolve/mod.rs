//! Scope-aware reference resolution: which definition an occurrence of a name refers to.
//!
//! Query time only. Definitions come from the index (`chunks`); scopes, qualifiers and import
//! bindings come from parsing the files that hold occurrences (and the modules they import) again
//! with tree-sitter. Python, JavaScript/TypeScript, Rust, Go, Java, PHP, C, C++ and C# are
//! understood; other languages leave their occurrences unresolved.
//!
//! This module is the walker: the steps every language shares (scopes from the definitions of
//! the index, the lookup memo, the depth guards, the order of the steps). What a language decides
//! differently is its rule set, one file each, behind the `Rules` trait.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use anyhow::Result;
use tree_sitter::{Node, Parser, Point, Tree};

use crate::fmt::shown;
use crate::index::{ChunkRow, Index, Symbol};

mod c;
mod csharp;
mod go;
mod java;
mod js;
mod php;
mod python;
mod rust;

/// A language the resolver reads, named by its rule set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Python,
    Js,
    Rust,
    Go,
    Java,
    Php,
    /// C and C++
    C,
    CSharp,
}

const LANGS: [Lang; 8] = [Lang::Python, Lang::Js, Lang::Rust, Lang::Go, Lang::Java, Lang::Php, Lang::C, Lang::CSharp];

/// `$e` with `$r` naming the rule set of `$lang`.
macro_rules! rules {
    ($lang:expr, $r:ident => $e:expr) => {
        match $lang {
            Lang::Python => {
                type $r = python::Python;
                $e
            }
            Lang::Js => {
                type $r = js::Js;
                $e
            }
            Lang::Rust => {
                type $r = rust::Rust;
                $e
            }
            Lang::Go => {
                type $r = go::Go;
                $e
            }
            Lang::Java => {
                type $r = java::Java;
                $e
            }
            Lang::Php => {
                type $r = php::Php;
                $e
            }
            Lang::C => {
                type $r = c::C;
                $e
            }
            Lang::CSharp => {
                type $r = csharp::CSharp;
                $e
            }
        }
    };
}

/// What one language decides when a name is resolved. The walker asks at each step where the
/// languages differ; a step a rule set leaves out is the walker's own.
trait Rules {
    /// The grammars (`LangSpec::id`) read with these rules.
    const GRAMMARS: &'static [&'static str];
    /// `x.f()` calls a method, never a free function `f`.
    const METHODS_ONLY: bool = true;
    /// What separates the parts of a qualified name.
    const PATH_SEP: &'static str = ".";
    /// The names of the value a method runs on.
    const SELF_NAMES: &'static [&'static str] = &[];
    /// The name of the type around.
    const SELF_TYPE: Option<&'static str> = None;
    /// The nodes a local bound in them does not leave.
    const BLOCKS: &'static [&'static str] = &["statement_block", "block", "compound_statement", "for_statement", "for_in_statement", "for_expression"];
    /// A comprehension binds its variables after the element that uses them, and a method does
    /// not see the variables of its class body.
    const COMPREHENSIONS: bool = false;
    /// A struct field without a name is named after its type.
    const EMBEDDED_FIELDS: bool = false;
    /// A constant bound to `require(..)` is an import of a module.
    const REQUIRE: bool = false;
    /// A method is named in any case (PHP); fields and constants are not.
    const MEMBERS_ANY_CASE: bool = false;

    /// Adds the bindings of import statement `n` to `out`; false when `n` is none.
    fn bindings(n: Node, path: &str, src: &str, out: &mut Vec<Binding>) -> bool;

    /// Whether the identifier spelled `spelled` is the name `name` written another way.
    fn raw(_spelled: &str, _name: &str) -> bool {
        false
    }

    /// The occurrence at an import statement's name of binding `b`.
    fn import_site(r: &mut Resolver, path: &str, lang: Lang, b: &Binding) -> Resolution {
        match r.resolve_binding(path, lang, b, 0) {
            Some(id) => Ok((id, "import")),
            None => Err(r.binding_failure(path, lang, b)),
        }
    }

    /// An answer for `node` before the walker's qualified and bare steps, when the language has one.
    fn occurrence(_r: &mut Resolver, _path: &str, _fi: &FileInfo, _node: Node, _name: &str, _line: i64) -> Option<Resolution> {
        None
    }

    /// A name with nothing in front.
    #[allow(clippy::too_many_arguments)]
    fn bare(r: &mut Resolver, path: &str, fi: &FileInfo, lang: Lang, node: Node, name: &str, line: i64) -> Resolution {
        r.scoped_bare(path, fi, lang, node, name, line)
    }

    /// `q.name`, `q::name`.
    #[allow(clippy::too_many_arguments)]
    fn qualified(r: &mut Resolver, path: &str, fi: &FileInfo, lang: Lang, q: Node, node: Node, name: &str, line: i64) -> Resolution {
        r.walk_qualified(path, fi, lang, q, node, name, line)
    }

    /// The other files whose top-level definitions a file sees without an import.
    fn package_files(_r: &mut Resolver, _path: &str) -> Vec<String> {
        Vec::new()
    }

    /// Whether qualifier `q` stands for the base classes (`super`).
    fn is_super(_q: Node, _src: &str) -> bool {
        false
    }

    /// Whether qualifier `q`, spelled `qt`, of the member `node` is the value a method runs on.
    fn is_receiver(_q: Node, _qt: &str, _node: Node) -> bool {
        false
    }

    /// When the value a method at `q` runs on is an object literal: its qualified name.
    fn literal_owner(_fi: &FileInfo, _q: Node) -> Option<String> {
        None
    }

    /// The type of the variable `e` names.
    fn variable_ty(r: &mut Resolver, path: &str, fi: &FileInfo, lang: Lang, e: Node) -> Option<Ty> {
        r.variable_ty(path, fi, lang, e)
    }

    /// The type of an expression of a kind only this language has; None when `e` is none.
    fn expression_ty(_r: &mut Resolver, _path: &str, _fi: &FileInfo, _e: Node) -> Option<Option<Ty>> {
        None
    }

    /// The type of the value an `await` of a value of type `t` gives.
    fn awaited(t: Ty) -> Option<Ty> {
        Some(t)
    }

    /// The class of a literal of node kind `kind`.
    fn literal_class(_kind: &str) -> Option<&'static str> {
        None
    }

    /// The return type a function or method declaration states.
    fn return_type(node: Node) -> Option<Node> {
        node.child_by_field_name("return_type").or_else(|| node.child_by_field_name("result"))
    }

    /// The declared type of the field `name` of container `owner`, which `fi` at `path` declares.
    fn field_ty(r: &mut Resolver, owner: i64, name: &str, path: &str, fi: &FileInfo, lang: Lang) -> Option<Ty> {
        r.declared_field_ty(owner, name, path, fi, lang)
    }

    /// The type a call `v` of `f` builds, when it is a constructor form.
    fn constructed_call(_r: &mut Resolver, _path: &str, _fi: &FileInfo, _v: Node, _f: Node) -> Option<Ty> {
        None
    }

    /// The type a bare name `v` builds as a value (a unit struct).
    fn constructed_name(_r: &mut Resolver, _path: &str, _fi: &FileInfo, _v: Node) -> Option<Ty> {
        None
    }

    /// The type of call `e` of `f`, a function or a method from outside the repository.
    fn outside_call(_r: &mut Resolver, _path: &str, _fi: &FileInfo, _lang: Lang, _e: Node, _f: Node) -> Option<Ty> {
        None
    }

    /// Whether a type from outside named `name` stands for the type it holds (`Box<T>`, `Optional[T]`).
    fn unwraps(_name: &str) -> bool {
        false
    }

    /// The type a name of a type `n` stands for.
    fn named_ty(r: &mut Resolver, path: &str, fi: &FileInfo, lang: Lang, n: Node) -> Option<Ty> {
        r.walk_named_ty(path, fi, lang, n)
    }

    /// Whether the file binds `name` as a value that may stand for a type (an alias).
    fn type_alias(_fi: &FileInfo, _name: &str) -> bool {
        false
    }

    /// `name` as a member container `id` (`qualname` in `path`) inherits.
    fn inherited(r: &mut Resolver, id: i64, name: &str, _depth: usize, _path: &str, _qualname: &str) -> Option<i64> {
        r.ordered_member(id, name)
    }

    /// The bases of container `d` of `fi` at `path`, each resolved, in header order.
    fn bases(r: &mut Resolver, id: i64, path: &str, fi: &FileInfo, d: &Def, lang: Lang) -> Vec<std::result::Result<i64, &'static str>> {
        r.header_bases(id, path, fi, d, lang)
    }

    /// Whether a base `name` from outside keeps its place in the method resolution order.
    fn outside_base(_name: &str) -> bool {
        false
    }

    /// The number of parameters of method `id`, and whether its last takes the rest, where
    /// overloads of other arities are other methods.
    fn arity(_fi: &FileInfo, _id: i64) -> Option<(usize, bool)> {
        None
    }

    /// Whether binder `b` belongs to the whole function around it, past the blocks.
    fn function_wide(_b: Node) -> bool {
        false
    }

    /// Files of the module a qualifier chain names, if it is one.
    fn module_of_chain(_r: &mut Resolver, _path: &str, _fi: &FileInfo, _segs: &[&str]) -> Option<Vec<String>> {
        None
    }

    /// The files of `module` as an import of `from` names it.
    fn module_files(r: &mut Resolver, from: &str, module: &str) -> Option<Vec<String>>;

    /// The definition `item` of a `module` no file holds (a Rust inline `mod`), as `from` names it.
    fn unfiled_item(_r: &mut Resolver, _from: &str, _module: &str, _item: &str) -> Option<i64> {
        None
    }

    /// Whether the resolver maps an import recorded as `module`; the generic mapping of paths
    /// onto files takes the others, without a parse.
    fn reads_import(_module: &str) -> bool {
        true
    }

    /// The files the import statement of `from` at `line`, recorded as `module`, names; None
    /// when the resolver leaves that statement to the generic mapping.
    fn import_files(r: &mut Resolver, from: &str, fi: &FileInfo, line: i64, module: &str) -> Option<Vec<String>>;
}

fn lang_of(spec_id: &str) -> Option<Lang> {
    LANGS.into_iter().find(|&l| rules!(l, R => R::GRAMMARS.contains(&spec_id)))
}

/// The language the resolver reads a file of language `id` in (`langs::detect_at` decides `id`,
/// from the text where an extension is shared, as the parser does).
fn lang_of_id(id: Option<&str>) -> Option<Lang> {
    lang_of(id?)
}

/// Whether the resolver reads the repository file `rel`.
pub(crate) fn reads(root: &std::path::Path, rel: &str) -> bool {
    lang_of_id(crate::langs::detect_at(root, rel).map(|l| l.id)).is_some()
}

/// Whether an import in language `id` can bind a name under another (`import a as b`).
pub(crate) fn may_alias(id: Option<&str>) -> bool {
    matches!(lang_of_id(id), Some(Lang::Python | Lang::Js | Lang::Rust | Lang::Php))
}

/// Whether `x.f()` in language `id` calls a method, never a free function `f`.
pub(crate) fn methods_only(id: Option<&str>) -> bool {
    lang_of_id(id).is_some_and(|l| rules!(l, R => R::METHODS_ONLY))
}

/// Definition chosen for an occurrence and how it was found, or why none was.
pub type Resolution = std::result::Result<(i64, &'static str), &'static str>;

#[derive(Debug, Clone)]
struct Def {
    id: i64,
    name: String,
    qualname: String,
    kind: String,
    start: i64,
    end: i64,
    /// innermost enclosing definition (index into the file's defs)
    parent: Option<usize>,
}

/// A name an import statement brings into a file. `item: None` binds the module itself;
/// `local == "*"` is a wildcard import; an empty `module` means the file itself (JS `export {a as b}`).
#[derive(Debug, Clone, PartialEq)]
struct Binding {
    local: String,
    module: String,
    item: Option<String>,
    /// start bytes of the identifiers in the import statement that name this binding
    sites: Vec<usize>,
}

/// Why an occurrence stays unresolved in a language that has a grammar but no resolver rules.
pub const UNREAD: &str = "not resolved in this language";

/// Why an occurrence in a file whose tree is deeper than `MAX_DEPTH` stays unresolved.
pub const TOO_DEEP: &str = "nested too deeply to read";

/// Deeper trees are not read: a climb with `Node::parent` costs the depth squared, and the type
/// walks recurse.
const MAX_DEPTH: usize = 2000;

pub(crate) struct FileInfo {
    lang: Option<Lang>,
    pub(crate) src: String,
    /// the bytes on disk and where each line starts, when decoding `src` replaced invalid UTF-8:
    /// Engine A's columns count those bytes
    raw: Option<(Vec<u8>, Vec<usize>)>,
    tree: Option<Tree>,
    /// the tree was dropped for passing `MAX_DEPTH`
    deep: bool,
    defs: Vec<Def>,
    bindings: Vec<Binding>,
}

pub struct Resolver<'a> {
    ix: &'a Index,
    files: HashSet<String>,
    cache: HashMap<String, Rc<FileInfo>>,
    /// the language each file asked about is read in
    langs: std::cell::RefCell<HashMap<String, Option<Lang>>>,
    parsers: HashMap<&'static str, Parser>,
    crates: Option<HashMap<String, String>>,
    go_dirs: Option<HashMap<String, Vec<String>>>,
    go_imports: Option<HashMap<String, String>>,
    /// by directory: the config found there, or that there is none
    ts: HashMap<String, Option<Rc<js::TsPaths>>>,
    js_packages: Option<HashMap<String, Vec<Rc<js::JsPackage>>>>,
    /// current depth of `member` / `lookup_top` calls: import and base-class cycles stop here
    nest: usize,
    /// (files, name) of the `lookup_top` calls made for the current `resolve_at`, and the answer
    /// (None while the lookup runs)
    looked_up: HashMap<(String, String), Option<i64>>,
    /// by definition: the type its values have (`def_ty`)
    def_tys: HashMap<i64, Option<Ty>>,
    /// names a trait or interface of the repository declares as methods
    trait_members: Option<HashSet<String>>,
    /// (type from outside, member name) -> whether the repository extends the type with it
    extended: HashMap<(String, String), bool>,
    /// Java: directory -> its `.java` files, and package -> the directories that declare it
    java_dirs: Option<HashMap<String, Vec<String>>>,
    java_pkgs: Option<HashMap<String, Vec<String>>>,
    /// Java: a dotted name (`a.b.C.m`) -> the definition it names
    java_paths: HashMap<String, std::result::Result<i64, &'static str>>,
    /// Java: by class, the types it extends and implements, and the fields it declares
    java_bases: HashMap<i64, Rc<Vec<std::result::Result<i64, &'static str>>>>,
    java_fields: HashMap<i64, Rc<HashSet<String>>>,
    /// PHP: a full name and the kinds looked for -> the definition it names
    php_paths: HashMap<String, std::result::Result<i64, &'static str>>,
    /// C and C++: file name -> the repository files of that name, and by declaration the
    /// definition it stands for (None: several)
    c_names: Option<HashMap<String, Vec<String>>>,
    c_defs: HashMap<i64, Option<i64>>,
    /// C#: type name -> (definition, namespace, class it is nested in); namespace -> its files;
    /// by type, the types it extends and implements
    cs_types: HashMap<String, CsTypes>,
    cs_namespaces: Option<HashMap<String, Vec<String>>>,
    cs_bases: HashMap<i64, Rc<Vec<std::result::Result<i64, &'static str>>>>,
    /// by class: the classes it inherits from, in method resolution order (`linearized`)
    orders: HashMap<i64, Rc<Vec<i64>>>,
    /// by scope node id, what was looked for and name: the byte ranges found, so a use in a big
    /// function does not walk all of it
    found: HashMap<(usize, &'static str, String), Spans>,
    /// by node id: the type `type_of` found for it
    tys: HashMap<usize, Ty>,
}

type Spans = Rc<Vec<(usize, usize)>>;
/// C# types of one name: (definition, its namespace, the class it is nested in)
type CsTypes = Rc<Vec<(i64, String, Option<i64>)>>;

/// Whether the tree has a node more than `MAX_DEPTH` levels below its root.
fn too_deep(t: &Tree) -> bool {
    let (mut c, mut depth) = (t.walk(), 0);
    loop {
        // a subtree of k nodes is at most k - 1 levels deep: most are not entered
        let shallow = depth + c.node().descendant_count() <= MAX_DEPTH + 1;
        if !shallow && c.goto_first_child() {
            depth += 1;
            if depth > MAX_DEPTH {
                return true;
            }
            continue;
        }
        while !c.goto_next_sibling() {
            if !c.goto_parent() {
                return false;
            }
            depth -= 1;
        }
    }
}

fn text<'s>(n: Node, src: &'s str) -> &'s str {
    n.utf8_text(src.as_bytes()).unwrap_or("")
}

/// The text of `rel` under `root` when it is a regular file inside `root` of at most
/// `MAX_FILE_BYTES`: a FIFO would block the read, and a symlink could lead out of the repository.
pub fn repo_text(root: &std::path::Path, rel: &str) -> Option<String> {
    crate::index::decode_source(&repo_bytes(root, rel)?).map(|t| t.into_owned())
}

fn repo_bytes(root: &std::path::Path, rel: &str) -> Option<Vec<u8>> {
    crate::answers::noted(root, rel);
    let p = root.join(rel);
    let meta = std::fs::symlink_metadata(&p).ok()?;
    if !meta.is_file() || meta.len() > crate::index::MAX_FILE_BYTES || !std::fs::canonicalize(&p).ok()?.starts_with(root) {
        return None;
    }
    std::fs::read(&p).ok()
}

/// The segments of a module path below the repository's own package, when the checkout is that
/// package (`networkx.utils` inside `networkx/` -> `utils`). Any other leading name is a package
/// from outside, even when the rest matches a local file.
pub fn below_own_package<'s>(root: &std::path::Path, segs: &'s [&'s str]) -> Option<&'s [&'s str]> {
    let own = root.file_name()?.to_str()?;
    let first = *segs.first()?;
    (segs.len() > 1 && (first == own || first == own.replace('-', "_"))).then(|| &segs[1..])
}

fn dir_of(p: &str) -> &str {
    p.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// `rel` from the directory `base`; a path that climbs above the root keeps its leading `..`, so it
/// names no file of the repository.
pub(crate) fn join(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if base.is_empty() { Vec::new() } else { base.split('/').collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// The path a Python relative import (`.`, `.x`, `..pkg.mod`) of `from` names, before a suffix.
pub(crate) fn py_relative(from: &str, module: &str) -> String {
    let ups = module.chars().take_while(|c| *c == '.').count();
    join(dir_of(from), &format!("{}{}", "../".repeat(ups.saturating_sub(1)), module[ups..].replace('.', "/")))
}

/// `Foo<'a, T>.new` -> `Foo.new`: impl blocks carry their generics in the qualified name.
fn strip_generics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.replace(' ', "")
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

const CONTAINER_KINDS: &[&str] = &["class", "impl", "trait", "struct", "interface", "enum"];
const SCOPE_KINDS: &[&str] = &["function", "method", "module"];
const FUNCTION_NODES: &[&str] = &[
    "function_definition",
    "lambda",
    "function_declaration",
    "generator_function_declaration",
    "generator_function",
    "function_expression",
    "function",
    "arrow_function",
    "method_definition",
    "function_item",
    "closure_expression",
    "method_declaration",
    "func_literal",
    "constructor_declaration",
    "lambda_expression",
];
/// (parent kind, field holding the bound name; "" = any child) of local variable and parameter bindings
const LOCAL_BINDERS: &[(&str, &str)] = &[
    ("parameters", ""),
    ("lambda_parameters", ""),
    ("formal_parameters", ""),
    ("closure_parameters", ""),
    ("typed_parameter", ""),
    ("list_splat_pattern", ""),
    ("dictionary_splat_pattern", ""),
    ("pattern_list", ""),
    ("tuple_pattern", ""),
    ("array_pattern", ""),
    ("default_parameter", "name"),
    ("typed_default_parameter", "name"),
    ("assignment", "left"),
    ("for_statement", "left"),
    ("for_in_clause", "left"),
    ("for_in_statement", "left"),
    ("as_pattern_target", ""),
    ("assignment_pattern", "left"),
    ("required_parameter", "pattern"),
    ("optional_parameter", "pattern"),
    ("variable_declarator", "name"),
    ("let_declaration", "pattern"),
    ("parameter", "pattern"),
    ("for_expression", "pattern"),
    ("parameter_declaration", "name"),
    ("variadic_parameter_declaration", "name"),
    ("var_spec", "name"),
    ("const_spec", "name"),
    ("pair_pattern", "value"),
    ("rest_pattern", ""),
    ("catch_clause", "parameter"),
    ("arrow_function", "parameter"),
    // a JavaScript function expression binds its name inside itself only (binder_reach)
    ("function_expression", "name"),
    ("function", "name"),
    ("generator_function", "name"),
    ("named_expression", "name"),
    ("splat_pattern", ""),
    // C and C++: `int x = 1, *p;`, parameters, `T &r`, `auto [a, b]`
    ("declaration", "declarator"),
    ("init_declarator", "declarator"),
    ("parameter_declaration", "declarator"),
    ("optional_parameter_declaration", "declarator"),
    ("pointer_declarator", "declarator"),
    ("array_declarator", "declarator"),
    ("reference_declarator", ""),
    ("structured_binding_declarator", ""),
    // C#: parameters, `foreach`, `catch`, `out var x`, `is T t`
    ("parameter", "name"),
    ("foreach_statement", "left"),
    ("catch_declaration", "name"),
    ("declaration_expression", "name"),
    ("declaration_pattern", "name"),
];

/// Rust patterns whose plain names bind; a capitalized one names a constant or a variant.
const RUST_PATTERNS: &[&str] = &[
    "tuple_struct_pattern",
    "field_pattern",
    "mut_pattern",
    "ref_pattern",
    "reference_pattern",
    "captured_pattern",
    "slice_pattern",
    "or_pattern",
    "match_pattern",
    "let_condition",
];

fn is_local_binder(n: Node, src: &str) -> bool {
    if matches!(n.kind(), "shorthand_property_identifier_pattern" | "shorthand_field_identifier") {
        return true;
    }
    let Some(p) = n.parent() else { return false };
    let is = |f: &str, c: Node| p.child_by_field_name(f).map(|x| x.id()) == Some(c.id());
    match p.kind() {
        // Go `a, b := ...`, `for k, v := range`, `case v := <-ch` and `switch v := x.(type)`
        "expression_list" => {
            return p.parent().is_some_and(|g| {
                let field = if g.kind() == "type_switch_statement" { "alias" } else { "left" };
                matches!(g.kind(), "short_var_declaration" | "range_clause" | "receive_statement" | "type_switch_statement")
                    && g.child_by_field_name(field).map(|l| l.id()) == Some(p.id())
            })
        }
        // a Python `case` capture is one plain name; `case Color.RED` compares with a value
        "dotted_name" => return p.named_child_count() == 1 && p.parent().is_some_and(|g| matches!(g.kind(), "case_pattern" | "keyword_pattern")),
        "as_pattern" => return p.parent().is_some_and(|g| g.kind() == "case_pattern") && n.next_named_sibling().is_none(),
        "tuple" => return p.parent().is_some_and(|g| g.kind() == "as_pattern_target"),
        k if RUST_PATTERNS.contains(&k) => {
            let field = if k == "field_pattern" || k == "let_condition" { "pattern" } else { "" };
            return !text(n, src).starts_with(|c: char| c.is_uppercase()) && !is("type", n) && (field.is_empty() || is(field, n));
        }
        _ => {}
    }
    LOCAL_BINDERS.iter().any(|(k, f)| *k == p.kind() && (f.is_empty() || is(f, n)))
}

/// Whether the binder `b` reaches byte `at`: an `if let`, `while let` or `match` arm binds only
/// in its own branch, a `catch` or Go statement initializer only in that statement, a Python
/// parameter only in the function body, and a `let` or `for` not in its own value.
fn visible(b: Node, at: usize) -> bool {
    b.start_byte() == at || sees(visible_spans(b), at)
}

const EVERYWHERE: (usize, usize) = (0, usize::MAX);
const NOWHERE: (usize, usize) = (0, 0);

fn holds((start, end): (usize, usize), at: usize) -> bool {
    start <= at && at < end
}

/// Whether byte `at` is in the first span and in neither of the other two.
fn sees([within, not, nor]: [(usize, usize); 3], at: usize) -> bool {
    holds(within, at) && !holds(not, at) && !holds(nor, at)
}

/// Where the binder `b` is seen, for `visible`: a span a use must be in, and two it must not be in.
fn visible_spans(b: Node) -> [(usize, usize); 3] {
    let span = |n: Option<Node>| n.map_or(NOWHERE, |n| (n.start_byte(), n.end_byte()));
    let only = |n: Option<Node>| [span(n), NOWHERE, NOWHERE];
    let mut n = b;
    while let Some(p) = n.parent() {
        let k = p.kind();
        if k.ends_with("pattern") || matches!(k, "expression_list" | "let_chain" | "default_parameter" | "typed_default_parameter" | "typed_parameter") {
            n = p;
            continue;
        }
        return match k {
            "let_condition" => {
                let mut c = p;
                while let Some(g) = c.parent().filter(|g| g.kind() == "let_chain") {
                    c = g;
                }
                let branch = c.parent();
                [span(branch), span(p.child_by_field_name("value")), span(branch.and_then(|g| g.child_by_field_name("alternative")))]
            }
            "let_declaration" => [EVERYWHERE, span(p.child_by_field_name("value")), span(p.child_by_field_name("alternative"))],
            "for_expression" => [EVERYWHERE, span(p.child_by_field_name("value")), NOWHERE],
            "match_arm" | "catch_clause" | "type_switch_statement" => only(Some(p)),
            // Python evaluates defaults and annotations in the scope around the function
            "parameters" | "lambda_parameters" => only(p.parent().and_then(|f| f.child_by_field_name("body"))),
            "receive_statement" | "range_clause" => only(p.parent()),
            "short_var_declaration" => match p.parent() {
                Some(g) if g.kind() == "for_clause" => only(g.parent()),
                Some(g) if g.child_by_field_name("initializer").map(|i| i.id()) == Some(p.id()) => only(Some(g)),
                _ => [EVERYWHERE, NOWHERE, NOWHERE],
            },
            _ => [EVERYWHERE, NOWHERE, NOWHERE],
        };
    }
    [EVERYWHERE, NOWHERE, NOWHERE]
}

/// The receiver or path in front of a member name (`obj` of `obj.N`, `a::b` of `a::b::N`).
fn qualifier(n: Node) -> Option<Node> {
    let p = n.parent()?;
    const PAIRS: &[(&str, &str)] =
        &[("object", "attribute"), ("object", "property"), ("path", "name"), ("value", "field"), ("operand", "field"), ("package", "name"), ("module", "name")];
    for (obj, mem) in PAIRS {
        if p.child_by_field_name(mem).map(|c| c.id()) == Some(n.id()) {
            if let Some(o) = p.child_by_field_name(obj) {
                return Some(o);
            }
        }
    }
    None
}

/// What the binder `b` of a variable is declared with: (its type annotation, its value).
fn declared(b: Node) -> (Option<Node>, Option<Node>) {
    let Some(p) = b.parent() else { return (None, None) };
    if p.kind() == "expression_list" {
        // Go `a, b := x, y`: the value at the binder's place
        let mut cur = p.walk();
        let place = p.named_children(&mut cur).position(|c| c.id() == b.id());
        let value = place.and_then(|i| p.parent()?.child_by_field_name("right")?.named_child(i as u32));
        return (None, value);
    }
    if p.kind().ends_with("pattern") {
        // the `type` of `Some(x)` or `P { x }` is the pattern's, not the binder's
        return (None, None);
    }
    let value = match p.kind() {
        "assignment" => p.child_by_field_name("right"),
        "variable_declarator" | "let_declaration" | "static_item" | "const_item" => p.child_by_field_name("value"),
        "var_spec" | "const_spec" => p.child_by_field_name("value").and_then(|v| if v.kind() == "expression_list" { v.named_child(0) } else { Some(v) }),
        _ => None,
    };
    (p.child_by_field_name("type"), value)
}

/// The name of a type from outside that holds no repository type and whose name is not known:
/// what a method of such a type returns (`file.metadata()`).
const OUTSIDE: &str = "?";

/// What the resolver knows of the type of a value.
#[derive(Clone, Debug)]
enum Ty {
    /// a class, struct or other concrete type defined in the repository
    Local(i64),
    /// a concrete type defined outside it (`Vec`, `str`, `list`), with its type arguments; one
    /// unknown argument when they are not written (`HashMap::new()`)
    Foreign(String, Vec<Option<Ty>>),
}

/// The identifier that names the type in a type expression, past references, pointers and
/// generic arguments; the last name of a path (`a.b.T`, `a::T`, `pkg.T`).
fn type_name(mut t: Node) -> Option<Node> {
    loop {
        t = match t.kind() {
            "identifier" | "type_identifier" => return Some(t),
            "attribute" => t.child_by_field_name("attribute")?,
            "member_expression" => t.child_by_field_name("property")?,
            // a Java `a.b.C` names its parts by position only
            "scoped_identifier" | "scoped_type_identifier" | "qualified_type" | "nested_type_identifier" => {
                t.child_by_field_name("name").or_else(|| t.named_child(t.named_child_count().checked_sub(1)? as u32))?
            }
            "generic_type" => t.child_by_field_name("type").or_else(|| t.child_by_field_name("name"))?,
            "reference_type" => t.child_by_field_name("type")?,
            "type" | "type_annotation" | "pointer_type" | "parenthesized_type" => t.named_child(0)?,
            _ => return None,
        };
    }
}

fn string_content(n: Node, src: &str) -> String {
    text(n, src).trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string()
}

// ------------------------------------------------------------------------- import bindings
fn bindings_of(lang: Lang, path: &str, root: Node, src: &str) -> Vec<Binding> {
    rules!(lang, R => bindings_by::<R>(path, root, src))
}

fn bindings_by<R: Rules>(path: &str, root: Node, src: &str) -> Vec<Binding> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if !R::bindings(n, path, src, &mut out) {
            let mut cur = n.walk();
            let kids: Vec<Node> = n.named_children(&mut cur).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    out
}

// ------------------------------------------------------------------------- resolver
impl<'a> Resolver<'a> {
    /// The language the resolver reads `path` in, as the parser takes the file.
    fn path_lang(&self, path: &str) -> Option<Lang> {
        if let Some(l) = self.langs.borrow().get(path) {
            return *l;
        }
        let l = lang_of_id(crate::langs::detect_at(&self.ix.root, path).map(|l| l.id));
        self.langs.borrow_mut().insert(path.to_string(), l);
        l
    }

    /// `names::Names` builds the one resolver of a request.
    pub(crate) fn new(ix: &'a Index) -> Result<Resolver<'a>> {
        let files = ix.files()?.into_iter().collect();
        Ok(Resolver {
            ix,
            files,
            cache: HashMap::new(),
            langs: Default::default(),
            parsers: HashMap::new(),
            crates: None,
            go_dirs: None,
            go_imports: None,
            ts: HashMap::new(),
            js_packages: None,
            nest: 0,
            looked_up: HashMap::new(),
            def_tys: HashMap::new(),
            trait_members: None,
            extended: HashMap::new(),
            java_dirs: None,
            java_pkgs: None,
            java_paths: HashMap::new(),
            java_bases: HashMap::new(),
            java_fields: HashMap::new(),
            php_paths: HashMap::new(),
            c_names: None,
            c_defs: HashMap::new(),
            cs_types: HashMap::new(),
            cs_namespaces: None,
            cs_bases: HashMap::new(),
            orders: HashMap::new(),
            found: HashMap::new(),
            tys: HashMap::new(),
        })
    }

    /// The repository's files (`files` rows that were not skipped).
    pub(crate) fn files(&self) -> &HashSet<String> {
        &self.files
    }

    /// `path` as the resolver reads it: loaded once, then shared by every use of it.
    pub(crate) fn file(&mut self, path: &str) -> Rc<FileInfo> {
        if let Some(f) = self.cache.get(path) {
            return Rc::clone(f);
        }
        let fi = Rc::new(self.load(path));
        self.cache.insert(path.to_string(), Rc::clone(&fi));
        fi
    }

    /// The names `path` imports under a local name of its own, as (local, name of the definition
    /// it reaches, else the imported name): `from util import helper as h` is ("h", "helper"), and
    /// so is `const h = require("./util")` when util sets `module.exports = helper`.
    pub(crate) fn aliases(&mut self, path: &str) -> Vec<(String, String)> {
        self.alias_sites(path).into_iter().map(|(local, item, _)| (local, item)).collect()
    }

    /// Whether `path` imports a name as `name` from another module (`from .util import helper`).
    pub(crate) fn imports_as(&mut self, path: &str, name: &str) -> bool {
        self.file(path).bindings.iter().any(|b| b.local == name && !b.module.is_empty())
    }

    /// `aliases`, each with the start bytes of the import statement's identifiers that name it.
    pub(crate) fn alias_sites(&mut self, path: &str) -> Vec<(String, String, Vec<usize>)> {
        // a lookup cut at the depth limit is no answer for a lookup that starts here
        self.looked_up.clear();
        let fi = self.file(path);
        let Some(lang) = fi.lang else { return Vec::new() };
        let mut out = Vec::new();
        for b in fi.bindings.iter().filter(|b| !b.module.is_empty()) {
            let reached = self.resolve_binding(path, lang, b, 0).and_then(|id| self.ix.symbol(id).ok().flatten()).map(|d| d.name);
            if let Some(item) = reached.or_else(|| b.item.clone()).filter(|i| *i != b.local) {
                out.push((b.local.clone(), item, b.sites.clone()));
            }
        }
        out
    }

    /// Drops the loaded copy of `path`; a later use loads it again.
    pub fn forget(&mut self, path: &str) {
        self.cache.remove(path);
        // node ids name nodes of trees still loaded
        self.found.clear();
        self.tys.clear();
    }

    /// `find()`, run once per scope, `what` and name.
    fn found_in(&mut self, scope: Node, what: &'static str, name: &str, find: impl FnOnce() -> Vec<(usize, usize)>) -> Spans {
        let key = (scope.id(), what, name.to_string());
        if let Some(f) = self.found.get(&key) {
            return Rc::clone(f);
        }
        let f = Rc::new(find());
        self.found.insert(key, Rc::clone(&f));
        f
    }

    /// Whether `name` is a type parameter of an item around `n` (`fn f<T>`, `impl<T>`, `class C<T>`).
    fn type_parameter(&mut self, n: Node, name: &str, src: &str) -> bool {
        let mut a = Some(n);
        while let Some(x) = a {
            // the children of a block around many uses are read once
            let lists = self.found_in(x, "type parameters", "", || {
                let mut cur = x.walk();
                x.named_children(&mut cur)
                    .filter(|c| matches!(c.kind(), "type_parameters" | "type_parameter_list"))
                    .map(|c| (c.start_byte(), c.end_byte()))
                    .collect()
            });
            if lists.iter().any(|&(s, e)| src.get(s..e).unwrap_or("").split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).any(|w| w == name)) {
                return true;
            }
            a = x.parent();
        }
        false
    }

    /// The nodes in `scope` that `binder` takes.
    fn binders_in(scope: Node, binder: impl Fn(Node) -> bool) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut stack = vec![scope];
        while let Some(n) = stack.pop() {
            if binder(n) {
                out.push((n.start_byte(), n.end_byte()));
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        out
    }

    /// A match at bytes `a..b` of `line` as Engine A reads `path` -> the same span in the decoded
    /// line Engine A prints, and the column `resolve_at` takes, which counts a BOM on line 1.
    pub(crate) fn decoded_span(&mut self, path: &str, line: u64, (a, b): (usize, usize)) -> ((usize, usize), usize) {
        if self.path_lang(path).is_none() {
            return ((a, b), a);
        }
        let fi = self.file(path);
        let (a, b) = match &fi.raw {
            Some((bytes, starts)) => {
                let start = starts.get(line.saturating_sub(1) as usize).copied().unwrap_or(bytes.len());
                let at = |x: usize| String::from_utf8_lossy(&bytes[start..(start + x).min(bytes.len())]).len();
                (at(a), at(b))
            }
            None => (a, b),
        };
        let bom = if line == 1 && fi.src.starts_with('\u{FEFF}') { '\u{FEFF}'.len_utf8() } else { 0 };
        ((a, b), a + bom)
    }

    fn load(&mut self, path: &str) -> FileInfo {
        let mut bytes = repo_bytes(&self.ix.root, path).unwrap_or_default();
        let src = crate::index::decode_source(&bytes).map(|t| t.into_owned()).unwrap_or_default();
        // Engine A reads UTF-16 decoded, and UTF-8 without its BOM
        let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
        let mut text = src.as_str();
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            bytes.drain(..3);
            // `src` is empty when the bytes hold a NUL
            text = src.strip_prefix('\u{FEFF}').unwrap_or(&src);
        }
        let raw = (!utf16 && !src.is_empty() && text.as_bytes() != bytes).then(|| {
            let starts = std::iter::once(0).chain(bytes.iter().enumerate().filter(|(_, &c)| c == b'\n').map(|(i, _)| i + 1)).collect();
            (bytes, starts)
        });
        let spec = crate::langs::detect(path, Some(&src));
        let lang = spec.and_then(|s| lang_of(s.id));
        let tree = match (spec, lang) {
            (Some(spec), Some(_)) => {
                let p = self.parsers.entry(spec.id).or_insert_with(|| {
                    let mut p = Parser::new();
                    let _ = p.set_language(&(spec.language)());
                    p
                });
                p.parse(src.as_bytes(), None)
            }
            _ => None,
        };
        let deep = tree.as_ref().is_some_and(too_deep);
        let tree = tree.filter(|_| !deep);
        let bindings = match (lang, &tree) {
            (Some(l), Some(t)) => bindings_of(l, path, t.root_node(), &src),
            _ => Vec::new(),
        };
        let defs = self.defs_of(path).unwrap_or_default();
        FileInfo { lang, src, raw, tree, deep, defs, bindings }
    }

    fn defs_of(&self, path: &str) -> Result<Vec<Def>> {
        let base = path.rsplit('/').next().unwrap_or(path);
        let stem = match base.rfind('.') {
            Some(i) if i > 0 => &base[..i],
            _ => base,
        };
        let rows = self.ix.symbols_in(path)?;
        let mut defs: Vec<Def> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for Symbol { id, name, qualname, kind, start, end, .. } in rows {
            // the header chunk (docstring and imports) is named after the file, not defined in it;
            // a definition that starts on line 1 too sorts before it
            if kind == "module" && start == 1 && name == stem && qualname == stem {
                continue;
            }
            while stack.last().is_some_and(|&t| defs[t].end < start) {
                stack.pop();
            }
            let parent = stack.last().copied().filter(|&t| defs[t].start <= start && end <= defs[t].end);
            defs.push(Def { id, name, qualname, kind, start, end, parent });
            stack.push(defs.len() - 1);
        }
        Ok(defs)
    }

    /// Resolve the occurrence of `name` at 1-based `line`, 0-based byte `col` of `path`.
    pub fn resolve_at(&mut self, path: &str, line: u64, col: usize, name: &str) -> Resolution {
        self.looked_up.clear();
        if self.path_lang(path).is_none() {
            // whether a grammar exists does not depend on the content the sniff reads
            return Err(if crate::langs::detect(path, Some("")).is_some() { UNREAD } else { "no grammar" });
        }
        let fi = self.file(path);
        let (Some(lang), Some(tree)) = (fi.lang, fi.tree.as_ref()) else {
            if fi.deep {
                return Err(TOO_DEEP);
            }
            return Err(if crate::langs::detect(path, Some(&fi.src)).is_some() { UNREAD } else { "no grammar" });
        };
        let row = line.saturating_sub(1) as usize;
        let Some(node) = tree.root_node().descendant_for_point_range(Point { row, column: col }, Point { row, column: col + name.len() }) else {
            return Err("not found");
        };
        // identifiers inside a string are code only in interpolations (f-strings, template literals)
        let mut a = Some(node);
        while let Some(n) = a {
            if n.kind().contains("comment") || (n.kind().contains("string") && !node.kind().ends_with("identifier")) {
                return Err("comment or string");
            }
            a = n.parent();
        }
        let raw = rules!(lang, R => R::raw(text(node, &fi.src), name));
        if text(node, &fi.src) != name && !raw {
            return Err("not an identifier");
        }
        let line = line as i64;
        if let Some(b) = fi.bindings.iter().find(|b| b.sites.contains(&node.start_byte())) {
            return rules!(lang, R => R::import_site(self, path, lang, b));
        }
        if let Some(p) = node.parent() {
            let spec = crate::langs::detect(path, Some(&fi.src));
            let is_def_node = spec.is_some_and(|s| s.defs.iter().any(|d| d.node == p.kind()));
            if is_def_node && p.child_by_field_name("name").map(|c| c.id()) == Some(node.id()) {
                if let Some(d) = fi.defs.iter().filter(|d| d.name == name && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start) {
                    return Ok((d.id, "definition"));
                }
            }
            if p.kind() == "keyword_argument" && p.child_by_field_name("name").map(|c| c.id()) == Some(node.id()) {
                return Err("keyword argument");
            }
        }
        if let Some(r) = rules!(lang, R => R::occurrence(self, path, &fi, node, name, line)) {
            return r;
        }
        if let Some(q) = qualifier(node) {
            return self.resolve_qualified(path, &fi, lang, q, node, name, line);
        }
        if matches!(node.kind(), "property_identifier" | "field_identifier") {
            return Err("field or key");
        }
        self.resolve_bare(path, &fi, lang, node, name, line)
    }

    fn binding_failure(&mut self, path: &str, lang: Lang, b: &Binding) -> &'static str {
        if b.module.is_empty() || self.module_files(path, lang, &b.module).is_some() {
            "not found"
        } else {
            "external"
        }
    }

    fn resolve_bare(&mut self, path: &str, fi: &FileInfo, lang: Lang, node: Node, name: &str, line: i64) -> Resolution {
        rules!(lang, R => R::bare(self, path, fi, lang, node, name, line))
    }

    /// A name with nothing in front, in the scopes the definitions of the index make: a local
    /// definition, a local import, a local variable, a top-level definition, an import, the
    /// package, a wildcard import.
    fn scoped_bare(&mut self, path: &str, fi: &FileInfo, lang: Lang, node: Node, name: &str, line: i64) -> Resolution {
        // enclosing scopes, innermost first; a class body is a scope only for code directly in it
        let mut chain: Vec<usize> = fi.defs.iter().enumerate().filter(|(_, d)| d.start <= line && line <= d.end).map(|(i, _)| i).collect();
        chain.sort_by_key(|&i| fi.defs[i].end - fi.defs[i].start);
        for (k, &s) in chain.iter().enumerate() {
            let kind = fi.defs[s].kind.as_str();
            if SCOPE_KINDS.contains(&kind) || (k == 0 && CONTAINER_KINDS.contains(&kind)) {
                if let Some(d) = fi.defs.iter().find(|d| d.name == name && d.parent == Some(s) && d.kind != "impl") {
                    return Ok((d.id, "local"));
                }
            }
        }
        // an import in the function around the use (Python `import os as name`, a `use` in a Rust body)
        let func = enclosing_function(node).map(|f| f.byte_range());
        let local_import = |b: &&Binding| b.local == name && func.as_ref().is_some_and(|r| b.sites.iter().any(|s| r.contains(s)));
        let local_import = fi.bindings.iter().find(local_import).cloned();
        if local_import.is_none() && self.shadowed(lang, node, name, &fi.src) {
            return Err("local variable");
        }
        // `const app = require("./app")` is a constant and an import: the import says what it is
        let required = |d: &&Def| rules!(lang, R => R::REQUIRE) && d.kind == "constant" && fi.bindings.iter().any(|b| b.local == name);
        if let Some(d) = top_level(fi, name).filter(|d| local_import.is_none() && !required(d)) {
            return Ok((d.id, "module"));
        }
        if let Some(b) = local_import.or_else(|| fi.bindings.iter().find(|b| b.local == name).cloned()) {
            return match self.resolve_binding(path, lang, &b, 0) {
                Some(id) => Ok((id, "import")),
                None => Err(self.binding_failure(path, lang, &b)),
            };
        }
        for f in rules!(lang, R => R::package_files(self, path)) {
            if f != path {
                if let Some(d) = top_level(&self.file(&f), name) {
                    return Ok((d.id, "package"));
                }
            }
        }
        for b in fi.bindings.iter().filter(|b| b.local == "*") {
            if let Some(files) = self.module_files(path, lang, &b.module) {
                if let Some(id) = self.lookup_top(&files, name, 1) {
                    return Ok((id, "import"));
                }
            }
        }
        Err("not found")
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_qualified(&mut self, path: &str, fi: &FileInfo, lang: Lang, q: Node, node: Node, name: &str, line: i64) -> Resolution {
        rules!(lang, R => R::qualified(self, path, fi, lang, q, node, name, line))
    }

    /// `q.name`: a member of the type around, of a module the chain `q` names, of the definition
    /// its head names, or of the type of the value `q` is.
    #[allow(clippy::too_many_arguments)]
    fn walk_qualified(&mut self, path: &str, fi: &FileInfo, lang: Lang, q: Node, node: Node, name: &str, line: i64) -> Resolution {
        let head = if q.kind() == "identifier" || q.named_child_count() == 0 { q } else { first_leaf(q) };
        self.walk_path(path, fi, lang, q, text(q, &fi.src).trim(), head, node, name, line)
    }

    /// `walk_qualified` of a qualifier written `qt` whose first segment is `head`, also where no
    /// node spans it (a path in a Rust macro's tokens).
    #[allow(clippy::too_many_arguments)]
    fn walk_path(&mut self, path: &str, fi: &FileInfo, lang: Lang, q: Node, qt: &str, head: Node, node: Node, name: &str, line: i64) -> Resolution {
        if rules!(lang, R => R::is_super(q, &fi.src)) {
            return self.super_member(path, fi, lang, q, name, line);
        }
        if rules!(lang, R => R::is_receiver(q, qt, node)) {
            if let Some(owner) = rules!(lang, R => R::literal_owner(fi, q)) {
                if owner.is_empty() {
                    return Err("receiver");
                }
                let want = format!("{owner}.{name}");
                return fi.defs.iter().find(|d| d.qualname == want).map(|d| (d.id, "member")).ok_or("not found");
            }
            let Some(c) =
                fi.defs.iter().filter(|d| CONTAINER_KINDS.contains(&d.kind.as_str()) && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start)
            else {
                return Err("receiver");
            };
            return self.member(c.id, name, 0).map(|id| (id, "member")).ok_or("not found");
        }
        let sep = rules!(lang, R => R::PATH_SEP);
        let stripped = strip_generics(qt);
        let segs: Vec<&str> = stripped.split(sep).collect();
        if segs.iter().any(|s| !is_ident(s)) {
            return self.through_type(path, fi, lang, q, name);
        }
        for k in (1..=segs.len()).rev() {
            let Some(files) = self.module_of_chain(path, fi, lang, &segs[..k]) else { continue };
            let mut cur: Option<i64> = None;
            for seg in &segs[k..] {
                cur = match cur {
                    None => self.lookup_top(&files, seg, 0),
                    Some(id) => self.member(id, seg, 0),
                };
                if cur.is_none() {
                    return Err("not found");
                }
            }
            let hit = match cur {
                None => self.lookup_top(&files, name, 0),
                Some(id) => self.member(id, name, 0),
            };
            return hit.map(|id| (id, "qualified")).ok_or("not found");
        }
        // `Class.N`, `Type::N`: the head names a definition
        let Ok((mut cur, _)) = self.resolve_bare(path, fi, lang, head, segs[0], line) else {
            let imported = fi.bindings.iter().any(|b| b.local == segs[0]) || matches!(segs[0], "std" | "core" | "alloc");
            if imported {
                return Err("external");
            }
            // `x.N` with `x` a variable: the type its declaration states, when it states one
            return self.through_type(path, fi, lang, q, name);
        };
        let head_def = cur;
        for seg in segs[1..].iter().copied().chain(std::iter::once(name)) {
            match self.member(cur, seg, 0) {
                Some(id) => cur = id,
                // a module-level variable is a definition too: its type has the member
                None if cur == head_def && !self.def_kind(cur).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str())) => {
                    return self.through_type(path, fi, lang, q, name)
                }
                None => return Err("not found"),
            }
        }
        Ok((cur, "qualified"))
    }

    /// `q.name` decided by the type of `q`: a member of a repository type, or nothing of the
    /// repository when the type is from outside and no definition of the repository extends it.
    fn through_type(&mut self, path: &str, fi: &FileInfo, lang: Lang, q: Node, name: &str) -> Resolution {
        match self.type_of(path, fi, lang, q) {
            Some(Ty::Local(owner)) => self.member(owner, name, 0).map(|id| (id, "variable")).ok_or("not found"),
            Some(Ty::Foreign(t, _)) if !self.extended(&t, name) && !self.trait_member(name) => Err("external"),
            _ => Err("receiver"),
        }
    }

    /// Whether the repository defines `name` as a member of something named `ty` (a trait
    /// implemented for a type from outside, a prototype extension).
    fn extended(&mut self, ty: &str, name: &str) -> bool {
        let key = (ty.to_string(), name.to_string());
        if let Some(&e) = self.extended.get(&key) {
            return e;
        }
        // asked for each call of a common method on a type from outside
        let e = match self.ix.named(name) {
            Ok(rows) => rows
                .iter()
                .any(|r| r.qualname.rsplit_once('.').is_some_and(|(owner, _)| owner.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == ty))),
            Err(_) => true,
        };
        self.extended.insert(key, e);
        e
    }

    /// Whether a trait or interface of the repository declares a method `name`: a blanket `impl`
    /// or an interface merged into a type from outside may give that type the method.
    fn trait_member(&mut self, name: &str) -> bool {
        if self.trait_members.is_none() {
            let mut words = HashSet::new();
            for t in self.ix.texts_of_kinds(&["trait", "interface"]).unwrap_or_default() {
                let mut rest = t.as_str();
                while let Some(i) = rest.find(|c: char| c.is_alphanumeric() || c == '_') {
                    let word_len = rest[i..].find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len() - i);
                    let (word, after) = (&rest[i..i + word_len], rest[i + word_len..].trim_start_matches(['?', ' ']));
                    if after.starts_with(['(', '<']) {
                        words.insert(word.to_string());
                    }
                    rest = &rest[i + word_len..];
                }
            }
            self.trait_members = Some(words);
        }
        self.trait_members.as_ref().is_some_and(|w| w.contains(name))
    }

    fn def_kind(&self, id: i64) -> Option<String> {
        self.ix.symbol(id).ok().flatten().map(|d| d.kind)
    }

    /// The type of the value expression `e` of `path`, when declarations say it: a variable's
    /// annotation or value, a call's declared return type, a field's declared type, a
    /// constructor. None when they do not.
    fn type_of(&mut self, path: &str, fi: &FileInfo, lang: Lang, e: Node) -> Option<Ty> {
        // a chain asks for the type of its receiver once to resolve the method and once to type the call
        if let Some(t) = self.tys.get(&e.id()) {
            return Some(t.clone());
        }
        let t = self.nested(|s| s.type_of_inner(path, fi, lang, e));
        if let Some(t) = &t {
            self.tys.insert(e.id(), t.clone());
        }
        t
    }

    /// `f` one level deeper in the type walks, or None past 32 levels: the walks recurse on the
    /// shape of the tree.
    fn nested(&mut self, f: impl FnOnce(&mut Self) -> Option<Ty>) -> Option<Ty> {
        if self.nest > 32 {
            return None;
        }
        self.nest += 1;
        let t = f(self);
        self.nest -= 1;
        t
    }

    fn type_of_inner(&mut self, path: &str, fi: &FileInfo, lang: Lang, e: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let line = e.start_position().row as i64 + 1;
        if let Some(t) = rules!(lang, R => R::expression_ty(self, path, fi, e)) {
            return t;
        }
        match e.kind() {
            "self" | "this" => self.container_at(fi, line),
            "identifier" if rules!(lang, R => R::SELF_NAMES).contains(&text(e, src)) => self.container_at(fi, line),
            "object_creation_expression" => self.named_ty(path, fi, lang, e.child_by_field_name("type")?),
            "cast_expression" => self.ty_of_type(path, fi, lang, e.child_by_field_name("type")?),
            "string_literal" => Some(Ty::Foreign("String".to_string(), Vec::new())),
            "identifier" => rules!(lang, R => R::variable_ty(self, path, fi, lang, e)),
            "parenthesized_expression" | "reference_expression" | "non_null_expression" | "await" | "unary_expression" => {
                self.type_of(path, fi, lang, e.child_by_field_name("operand").or_else(|| e.named_child(0))?)
            }
            "await_expression" => rules!(lang, R => R::awaited(self.type_of(path, fi, lang, e.named_child(0)?)?)),
            // Rust `x?`: the value inside a `Result` or an `Option`
            "try_expression" => match self.type_of(path, fi, lang, e.named_child(0)?)? {
                Ty::Foreign(n, args) if n == "Result" || n == "Option" => args.into_iter().next().flatten(),
                Ty::Foreign(n, _) if n == OUTSIDE => Some(Ty::Foreign(n, Vec::new())),
                _ => None,
            },
            "new_expression" => self.named_ty(path, fi, lang, e.child_by_field_name("constructor")?),
            "string" | "template_string" | "number" | "array" | "regex" => {
                rules!(lang, R => R::literal_class(e.kind())).map(|c| Ty::Foreign(c.to_string(), Vec::new()))
            }
            "struct_expression" => self.named_ty(path, fi, lang, e.child_by_field_name("name")?),
            "composite_literal" => self.ty_of_type(path, fi, lang, e.child_by_field_name("type")?),
            "call" | "call_expression" => {
                let f = e.child_by_field_name("function")?;
                // `a.m()`, `pkg.f()`, `T::f()`, `f()`: what the callee resolves to says what it returns
                let leaf = match member_parts(f) {
                    Some((_, m)) => m,
                    None if f.kind() == "scoped_identifier" => f.child_by_field_name("name")?,
                    None if f.kind() == "identifier" => f,
                    None => return None,
                };
                let found = match qualifier(leaf) {
                    Some(q) => self.resolve_qualified(path, fi, lang, q, leaf, text(leaf, src), line),
                    None => self.resolve_bare(path, fi, lang, leaf, text(leaf, src), line),
                };
                match found {
                    Ok((id, _)) => self.def_ty(id),
                    // `Type::new()` of a type from outside builds that type, its type arguments not known
                    Err(_) if f.kind() == "scoped_identifier" && matches!(text(leaf, src), "new" | "default" | "with_capacity" | "from") => {
                        match self.named_ty(path, fi, lang, f.child_by_field_name("path")?)? {
                            Ty::Foreign(n, args) if args.is_empty() => Some(Ty::Foreign(n, vec![None])),
                            t => Some(t),
                        }
                    }
                    Err("external") => rules!(lang, R => R::outside_call(self, path, fi, lang, e, f)),
                    Err(_) => None,
                }
            }
            _ => {
                let (obj, field) = member_parts(e)?;
                match self.type_of(path, fi, lang, obj)? {
                    Ty::Local(owner) => self.field_ty(owner, text(field, src)),
                    Ty::Foreign(..) => None,
                }
            }
        }
    }

    /// The innermost container around `line`, as the type of `self`.
    fn container_at(&self, fi: &FileInfo, line: i64) -> Option<Ty> {
        let c = fi.defs.iter().filter(|d| CONTAINER_KINDS.contains(&d.kind.as_str()) && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start)?;
        Some(Ty::Local(c.id))
    }

    /// What a value of definition `id` is: an instance of a container, or the declared return
    /// value of a function.
    fn def_ty(&mut self, id: i64) -> Option<Ty> {
        if let Some(t) = self.def_tys.get(&id) {
            return t.clone();
        }
        let t = self.def_ty_uncached(id);
        self.def_tys.insert(id, t.clone());
        t
    }

    fn def_ty_uncached(&mut self, id: i64) -> Option<Ty> {
        let kind = self.def_kind(id)?;
        if CONTAINER_KINDS.contains(&kind.as_str()) || kind == "type" {
            return concrete(&kind).then_some(Ty::Local(id));
        }
        let (path, _) = self.def_row(id)?;
        let fi = self.file(&path);
        let lang = fi.lang?;
        let node = def_node(&fi, id)?;
        let ret = rules!(lang, R => R::return_type(node))?;
        self.ty_of_type(&path, &fi, lang, ret)
    }

    /// The declared type of the field `name` of container `owner`: a Rust or Go struct field, a
    /// class field with a type or a value, a Python attribute set on `self` in a method.
    fn field_ty(&mut self, owner: i64, name: &str) -> Option<Ty> {
        let (path, _) = self.def_row(owner)?;
        let fi = self.file(&path);
        let lang = fi.lang?;
        rules!(lang, R => R::field_ty(self, owner, name, &path, &fi, lang))
    }

    fn declared_field_ty(&mut self, owner: i64, name: &str, path: &str, fi: &FileInfo, lang: Lang) -> Option<Ty> {
        let d = fi.defs.iter().find(|d| d.id == owner)?;
        // the fields of a Rust type are on its struct, not on an impl block
        let id = if d.kind == "impl" { top_level(fi, &strip_generics(&d.name)).filter(|s| s.kind != "impl")?.id } else { owner };
        let node = def_node(fi, id)?;
        let src = fi.src.as_str();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            let named = |f: &str| n.child_by_field_name(f).is_some_and(|x| text(x, src) == name);
            let hit = match n.kind() {
                "field_declaration" if named("name") => Some((n.child_by_field_name("type"), None)),
                // a Go embedded field is named after its type
                "field_declaration"
                    if rules!(lang, R => R::EMBEDDED_FIELDS)
                        && n.child_by_field_name("name").is_none()
                        && n.child_by_field_name("type").and_then(type_name).is_some_and(|t| text(t, src) == name) =>
                {
                    Some((n.child_by_field_name("type"), None))
                }
                "public_field_definition" | "field_definition" if named("name") || named("property") => {
                    Some((n.child_by_field_name("type"), n.child_by_field_name("value")))
                }
                "assignment" => {
                    let left = n.child_by_field_name("left");
                    let on_self = left.is_some_and(|l| {
                        l.kind() == "attribute"
                            && l.child_by_field_name("object").is_some_and(|o| text(o, src) == "self")
                            && l.child_by_field_name("attribute").is_some_and(|a| text(a, src) == name)
                    });
                    // a class-level `name: T`, not a local of a method
                    let in_body = left.is_some_and(|l| l.kind() == "identifier" && text(l, src) == name) && !has_function_between(n, node);
                    (on_self || in_body).then(|| (n.child_by_field_name("type"), n.child_by_field_name("right")))
                }
                _ => None,
            };
            if let Some((ty, value)) = hit {
                if let Some(t) = ty.and_then(|t| self.ty_of_type(path, fi, lang, t)) {
                    return Some(t);
                }
                if let Some(t) = value.and_then(|v| self.type_of(path, fi, lang, v)) {
                    return Some(t);
                }
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        None
    }

    /// The type of the variable `at` names: its nearest declaration before the use that reaches
    /// it, in the function around it and the functions around that, else a declaration at the top
    /// level of the file.
    fn variable_ty(&mut self, path: &str, fi: &FileInfo, lang: Lang, at: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let name = text(at, src);
        let mut scope = at;
        while let Some(p) = scope.parent() {
            scope = p;
            if FUNCTION_NODES.contains(&p.kind()) {
                break;
            }
        }
        // a closure sees the locals of the functions around it; a Rust `fn` item does not
        while let Some(outer) = enclosing_function(scope).filter(|_| scope.kind() != "function_item") {
            scope = outer;
        }
        let binds = |n: Node| {
            matches!(n.kind(), "identifier" | "shorthand_property_identifier_pattern" | "shorthand_field_identifier")
                && text(n, src) == name
                && (is_local_binder(n, src)
                    || n.parent().is_some_and(|p| matches!(p.kind(), "static_item" | "const_item") || (p.id() != scope.id() && names_definition(p, n))))
        };
        let in_scope = self.found_in(scope, "binder", name, || {
            let (mut out, mut stack) = (Vec::new(), vec![scope]);
            while let Some(n) = stack.pop() {
                if binds(n) {
                    out.push((n.start_byte(), n.end_byte()));
                }
                let mut cur = n.walk();
                stack.extend(n.named_children(&mut cur));
            }
            out.sort();
            out
        });
        let root = fi.tree.as_ref()?.root_node();
        let at_byte = at.start_byte();
        let reach = self.found_in(scope, "binder reach", name, || {
            in_scope.iter().map(|&(a, b)| root.named_descendant_for_byte_range(a, b).map_or((0, 0), |n| binder_reach(lang, n, scope))).collect()
        });
        let comprehensions = rules!(lang, R => R::COMPREHENSIONS);
        // a Python comprehension binds its variables after the element that uses them
        let comprehension = (in_scope.partition_point(|b| b.0 < at_byte)..in_scope.len())
            .filter(|&i| comprehensions && holds(reach[i], at_byte) && reach[i] != (scope.start_byte(), scope.end_byte()))
            .filter(|&i| root.named_descendant_for_byte_range(reach[i].0, reach[i].1).is_some_and(|n| rules!(lang, R => R::BLOCKS).contains(&n.kind())))
            .min_by_key(|&i| reach[i].1 - reach[i].0)
            .map(|i| in_scope[i]);
        // a Python method does not see the variables of its class body
        let func = enclosing_function(at).map_or(0, |f| f.start_byte());
        let class_body = |(s, e): (usize, usize)| {
            comprehensions && s < func && func < e && root.named_descendant_for_byte_range(s, e).is_some_and(|n| n.kind() == "class_definition")
        };
        // else the latest binder that reaches the use: not one of a block that has ended, a closure or a branch
        let mut binder = comprehension.or_else(|| {
            (0..in_scope.partition_point(|b| b.0 < at_byte)).rev().map(|i| (in_scope[i], reach[i])).find_map(|((a, b), (s, e))| {
                let seen =
                    s <= at_byte && at_byte < e && !class_body((s, e)) && root.named_descendant_for_byte_range(a, b).is_some_and(|n| visible(n, at_byte));
                seen.then_some((a, b))
            })
        });
        if binder.is_none() && scope.id() != root.id() {
            // a module-level or static variable, declared anywhere at the top of the file
            binder = self
                .found_in(root, "top binder", name, || {
                    let mut stack = vec![root];
                    while let Some(n) = stack.pop() {
                        if binds(n) {
                            return vec![(n.start_byte(), n.end_byte())];
                        }
                        if n.id() == root.id() || !(FUNCTION_NODES.contains(&n.kind()) || CLASS_NODES.contains(&n.kind())) {
                            let mut cur = n.walk();
                            stack.extend(n.named_children(&mut cur));
                        }
                    }
                    Vec::new()
                })
                .first()
                .copied();
        }
        let (a, b) = binder?;
        let binder = root.named_descendant_for_byte_range(a, b)?;
        let (ty, value) = declared(binder);
        if let Some(t) = ty {
            return self.built_ty(path, fi, lang, binder, value, at).or_else(|| self.ty_of_type(path, fi, lang, t));
        }
        self.type_of(path, fi, lang, value?)
    }

    /// `Base b = new Big()` and its likes: the type the constructor form `value` of the binder
    /// builds, which a use `at` in the same function sees until an assignment to the variable.
    fn built_ty(&mut self, path: &str, fi: &FileInfo, lang: Lang, binder: Node, value: Option<Node>, at: Node) -> Option<Ty> {
        let func = enclosing_function(at);
        if func.map(|f| f.id()) != enclosing_function(binder).map(|f| f.id()) {
            return None;
        }
        let scope = func.or_else(|| fi.tree.as_ref().map(|t| t.root_node()))?;
        let name = text(at, &fi.src);
        let assigned = self.found_in(scope, "assignment", name, || assignments(scope, name, &fi.src));
        if assigned.iter().any(|&(s, _)| binder.end_byte() <= s && s < at.start_byte()) {
            return None;
        }
        self.constructed(path, fi, lang, value?)
    }

    /// The repository type a constructor form builds: `new T()`, `T{}`, `&T{}`, a Python call of a
    /// class, a Rust unit struct and `Box::new(..)` of one of these.
    fn constructed(&mut self, path: &str, fi: &FileInfo, lang: Lang, v: Node) -> Option<Ty> {
        self.nested(|s| s.constructed_inner(path, fi, lang, v))
    }

    fn constructed_inner(&mut self, path: &str, fi: &FileInfo, lang: Lang, v: Node) -> Option<Ty> {
        let built = match v.kind() {
            "new_expression" | "object_creation_expression" | "composite_literal" | "struct_expression" => self.type_of(path, fi, lang, v),
            "unary_expression" | "reference_expression" | "parenthesized_expression" => {
                return self.constructed(
                    path,
                    fi,
                    lang,
                    v.child_by_field_name("operand").or_else(|| v.child_by_field_name("value")).or_else(|| v.named_child(0))?,
                )
            }
            "call" | "call_expression" => {
                let f = v.child_by_field_name("function")?;
                return rules!(lang, R => R::constructed_call(self, path, fi, v, f));
            }
            "identifier" => rules!(lang, R => R::constructed_name(self, path, fi, v)),
            _ => None,
        };
        built.filter(|t| matches!(t, Ty::Local(_)))
    }

    /// The type a type expression names, past references, pointers, `Box`-like holders and
    /// `Optional[..]`; None for a trait object, a union, a type parameter or anything unclear.
    fn ty_of_type(&mut self, path: &str, fi: &FileInfo, lang: Lang, t: Node) -> Option<Ty> {
        self.nested(|s| s.ty_of_type_inner(path, fi, lang, t))
    }

    fn ty_of_type_inner(&mut self, path: &str, fi: &FileInfo, lang: Lang, t: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        match t.kind() {
            "reference_type" => self.ty_of_type(path, fi, lang, t.child_by_field_name("type")?),
            "type" | "type_annotation" | "parenthesized_type" | "pointer_type" | "readonly_type" => self.ty_of_type(path, fi, lang, t.named_child(0)?),
            "primitive_type" | "predefined_type" | "integral_type" | "floating_point_type" | "boolean_type" => {
                Some(Ty::Foreign(text(t, src).to_string(), Vec::new()))
            }
            "array_type" | "slice_type" | "map_type" | "string_type" => Some(Ty::Foreign(t.kind().to_string(), Vec::new())),
            "generic_type" | "subscript" => {
                let base = t
                    .child_by_field_name("type")
                    .or_else(|| t.child_by_field_name("name"))
                    .or_else(|| t.child_by_field_name("value"))
                    .or_else(|| t.named_child(0))?;
                let mut args = Vec::new();
                let mut cur = t.walk();
                for c in t.named_children(&mut cur).filter(|c| c.id() != base.id()) {
                    let mut c2 = c.walk();
                    let list: Vec<Node> = if matches!(c.kind(), "type_arguments" | "type_parameter") { c.named_children(&mut c2).collect() } else { vec![c] };
                    // a Rust lifetime is no type: `Cow<'a, str>` holds a `str`
                    for a in list.into_iter().filter(|a| a.kind() != "lifetime") {
                        args.push(self.ty_of_type(path, fi, lang, a));
                    }
                }
                match self.named_ty(path, fi, lang, base)? {
                    Ty::Foreign(n, _) if rules!(lang, R => R::unwraps(&n)) => args.into_iter().next().flatten(),
                    Ty::Foreign(n, _) => Some(Ty::Foreign(n, args)),
                    local => Some(local),
                }
            }
            "type_identifier"
            | "identifier"
            | "scoped_type_identifier"
            | "scoped_identifier"
            | "qualified_type"
            | "nested_type_identifier"
            | "attribute"
            | "member_expression" => self.named_ty(path, fi, lang, t),
            _ => None,
        }
    }

    /// The type a name of a type stands for.
    fn named_ty(&mut self, path: &str, fi: &FileInfo, lang: Lang, n: Node) -> Option<Ty> {
        rules!(lang, R => R::named_ty(self, path, fi, lang, n))
    }

    fn walk_named_ty(&mut self, path: &str, fi: &FileInfo, lang: Lang, n: Node) -> Option<Ty> {
        let leaf = type_name(n)?;
        let src = fi.src.as_str();
        let name = text(leaf, src);
        let line = leaf.start_position().row as i64 + 1;
        if rules!(lang, R => R::SELF_TYPE) == Some(name) {
            return self.container_at(fi, line);
        }
        if self.type_parameter(leaf, name, src) {
            return None;
        }
        let q = qualifier(leaf);
        let found = match q {
            Some(q) => self.resolve_qualified(path, fi, lang, q, leaf, name, line),
            None => self.resolve_bare(path, fi, lang, leaf, name, line),
        };
        match found {
            Ok((id, _)) => {
                let kind = self.def_kind(id)?;
                concrete(&kind).then_some(Ty::Local(id))
            }
            Err("external") => Some(Ty::Foreign(name.to_string(), Vec::new())),
            // a path that does not resolve (`Self::Item`, `T::Output`) says nothing
            Err(_) if q.is_some() => None,
            // a type of no file of the repository: the language's own (`Vec`, `str`, `list`)
            Err(_) if !self.defined_anywhere(name) && !rules!(lang, R => R::type_alias(fi, name)) => Some(Ty::Foreign(name.to_string(), Vec::new())),
            Err(_) => None,
        }
    }

    fn defined_anywhere(&self, name: &str) -> bool {
        self.ix.defines(name, &[CONTAINER_KINDS, &["type"]].concat()).unwrap_or(true)
    }

    fn def_row(&self, id: i64) -> Option<(String, String)> {
        self.ix.symbol(id).ok().flatten().map(|d| (d.path, d.qualname))
    }

    /// `name` as a member of the container definition `id` (methods, nested classes), base classes included.
    fn member(&mut self, id: i64, name: &str, depth: usize) -> Option<i64> {
        if self.nest > 32 {
            return None;
        }
        self.nest += 1;
        let r = self.member_inner(id, name, depth);
        self.nest -= 1;
        r
    }

    /// `name` as a member container `id` itself declares; a field is no such member: a method
    /// of its name is called, a field read.
    fn declared_member(&mut self, id: i64, name: &str) -> Option<i64> {
        self.declared(id, name, false)
    }

    /// A field or constant `name` container `id` itself declares.
    fn declared_field(&mut self, id: i64, name: &str) -> Option<i64> {
        self.declared(id, name, true)
    }

    fn declared(&mut self, id: i64, name: &str, field: bool) -> Option<i64> {
        let (path, qualname) = self.def_row(id)?;
        let want = format!("{}.{name}", strip_generics(&qualname));
        let fi = self.file(&path);
        let fold = !field && fi.lang.is_some_and(|l| rules!(l, R => R::MEMBERS_ANY_CASE));
        let same = |a: &str, b: &str| if fold { a.eq_ignore_ascii_case(b) } else { a == b };
        let wanted =
            |d: &&Def| same(&d.name, name) && (d.kind == "field" || field && d.kind == "constant") == field && same(&strip_generics(&d.qualname), &want);
        fi.defs.iter().find(wanted).map(|d| d.id)
    }

    fn member_inner(&mut self, id: i64, name: &str, depth: usize) -> Option<i64> {
        if let Some(d) = self.declared_member(id, name) {
            return Some(d);
        }
        let (path, qualname) = self.def_row(id)?;
        match self.file(&path).lang {
            Some(lang) => rules!(lang, R => R::inherited(self, id, name, depth, &path, &qualname)),
            None => self.ordered_member(id, name),
        }
    }

    /// `name` as a member of a base of container `id`, in method resolution order.
    fn ordered_member(&mut self, id: i64, name: &str) -> Option<i64> {
        let order = match self.orders.get(&id) {
            Some(o) => Rc::clone(o),
            None => {
                let o = Rc::new(self.linearized(id, 0, &mut HashMap::new()).0);
                self.orders.insert(id, Rc::clone(&o));
                o
            }
        };
        // past a base from outside, which may define `name`, the answer is not known
        order[1..].iter().take_while(|&&b| b >= 0).find_map(|&b| self.declared_member(b, name))
    }

    /// The types container `id` extends or implements, each resolved: Java supertypes, Python
    /// and JavaScript base classes, TypeScript `extends` and `implements`, the trait of a Rust
    /// `impl Trait for T`.
    fn bases(&mut self, id: i64) -> Vec<i64> {
        self.base_results(id).into_iter().filter_map(|b| b.ok()).filter(|&b| b >= 0).collect()
    }

    /// The bases of `bases`, in header order, with why one is not of the repository: "external"
    /// for an import from outside or a name the repository defines nowhere (`Error`, `object`).
    /// A Python base from outside other than `object` is a negative id of its name, which keeps
    /// its place in the method resolution order.
    fn base_results(&mut self, id: i64) -> Vec<std::result::Result<i64, &'static str>> {
        let Some((path, _)) = self.def_row(id) else { return Vec::new() };
        let fi = self.file(&path);
        let (Some(lang), Some(_), Some(d)) = (fi.lang, fi.tree.as_ref(), fi.defs.iter().find(|d| d.id == id)) else { return Vec::new() };
        rules!(lang, R => R::bases(self, id, &path, &fi, d, lang))
    }

    /// The bases a class header names (`class C(A, B)`, `extends`, `implements`), or the trait of
    /// an `impl Trait for T`.
    fn header_bases(&mut self, id: i64, path: &str, fi: &FileInfo, d: &Def, lang: Lang) -> Vec<std::result::Result<i64, &'static str>> {
        let Some(tree) = fi.tree.as_ref() else { return Vec::new() };
        let mut named: Vec<Node> = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            let (s, e) = (n.start_position().row as i64 + 1, n.end_position().row as i64 + 1);
            if e < d.start || s > d.end {
                continue;
            }
            if n.kind() == "impl_item" && e == d.end {
                named.extend(n.child_by_field_name("trait"));
                break;
            }
            if matches!(n.kind(), "class_definition" | "class_declaration" | "abstract_class_declaration" | "class")
                && n.child_by_field_name("name").map(|x| text(x, &fi.src)) == Some(d.name.as_str())
            {
                let mut cur = n.walk();
                for ch in n.children(&mut cur).filter(|c| matches!(c.kind(), "argument_list" | "class_heritage")) {
                    let mut c2 = ch.walk();
                    for b in ch.named_children(&mut c2) {
                        if matches!(b.kind(), "extends_clause" | "implements_clause") {
                            let mut c3 = b.walk();
                            named.extend(b.named_children(&mut c3));
                        } else {
                            named.push(b);
                        }
                    }
                }
                break;
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        let mut out = Vec::new();
        for b in named {
            let Some(leaf) = type_name(b) else { continue };
            let leaf_name = text(leaf, &fi.src);
            let found = match self.type_leaf(path, fi, lang, leaf, d.start) {
                Err("not found") if !self.defined_anywhere(leaf_name) => Err("external"),
                f => f,
            };
            match found {
                Ok((x, _)) if x == id => {}
                Ok((x, _)) => out.push(Ok(x)),
                // every class ends with `object`: it keeps no place before an in-repo base
                Err("external") if rules!(lang, R => R::outside_base(leaf_name)) => {
                    use std::hash::BuildHasher;
                    out.push(Ok(-1 - (std::hash::BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default().hash_one(leaf_name) >> 2) as i64));
                }
                Err(e) => out.push(Err(e)),
            }
        }
        out
    }

    /// The definition a type name `leaf` written on `line` stands for: through its qualifier
    /// (`base.Base`, `a::T`) when it has one, else as a plain name in scope.
    fn type_leaf(&mut self, path: &str, fi: &FileInfo, lang: Lang, leaf: Node, line: i64) -> Resolution {
        let name = text(leaf, &fi.src);
        match qualifier(leaf) {
            Some(q) => self.resolve_qualified(path, fi, lang, q, leaf, name, line),
            None => self.resolve_bare(path, fi, lang, leaf, name, line),
        }
    }

    /// Container `id` and the classes it inherits from, in Python's method resolution order (C3,
    /// which for one base per class is the chain upwards), and whether a base is from outside and
    /// whether one could not be found.
    fn linearized(&mut self, id: i64, depth: usize, memo: &mut HashMap<i64, (Vec<i64>, bool, bool)>) -> (Vec<i64>, bool, bool) {
        if let Some(l) = memo.get(&id) {
            return l.clone();
        }
        // a class that comes round again through its own bases has none the second time
        memo.insert(id, (vec![id], false, false));
        let (mut seqs, mut direct, mut outside, mut unknown) = (Vec::new(), Vec::new(), false, false);
        for b in if depth < 8 { self.base_results(id) } else { Vec::new() } {
            match b {
                Ok(b) if b < 0 => {
                    outside = true;
                    seqs.push(vec![b]);
                    direct.push(b);
                }
                Ok(b) => {
                    let (l, o, u) = self.linearized(b, depth + 1, memo);
                    (outside, unknown) = (outside || o, unknown || u);
                    seqs.push(l);
                    direct.push(b);
                }
                Err("external") => outside = true,
                Err(_) => unknown = true,
            }
        }
        seqs.push(direct);
        let mut out = vec![id];
        loop {
            seqs.retain(|s| !s.is_empty());
            let Some(head) = seqs.iter().map(|s| s[0]).find(|h| !seqs.iter().any(|s| s[1..].contains(h))) else { break };
            out.push(head);
            for s in seqs.iter_mut().filter(|s| s[0] == head) {
                s.remove(0);
            }
        }
        // an order C3 cannot keep (Python refuses the class): the rest in header order
        for b in seqs.into_iter().flatten() {
            if !out.contains(&b) {
                out.push(b);
            }
        }
        let l = (out, outside, unknown);
        memo.insert(id, l.clone());
        l
    }

    /// `super.name` (JavaScript), `super().name` and `super(C, self).name` (Python): the first
    /// class after the one around (or `C`) in its resolution order that declares `name`.
    fn super_member(&mut self, path: &str, fi: &FileInfo, lang: Lang, q: Node, name: &str, line: i64) -> Resolution {
        if rules!(lang, R => R::literal_owner(fi, q)).is_some() {
            return Err("receiver");
        }
        let around = match self.container_at(fi, line) {
            Some(Ty::Local(c)) => Some(c),
            _ => None,
        };
        let named = match q.child_by_field_name("arguments").and_then(|a| a.named_child(0)) {
            Some(c) => {
                let leaf = type_name(c).ok_or("receiver")?;
                let found = match qualifier(leaf) {
                    Some(q) => self.resolve_qualified(path, fi, lang, q, leaf, text(leaf, &fi.src), line),
                    None => self.resolve_bare(path, fi, lang, leaf, text(leaf, &fi.src), line),
                };
                Some(found.map_err(|_| "receiver")?.0)
            }
            None => None,
        };
        // `super(C, self)` searches the order of the class the call sits in, after `C`; outside a
        // class only the order of `C` itself is known
        let (start, after) = match (around, named) {
            (Some(a), Some(c)) => (a, c),
            (None, Some(c)) => (c, c),
            (Some(a), None) => (a, a),
            (None, None) => return Err("receiver"),
        };
        let (order, outside, unknown) = self.linearized(start, 0, &mut HashMap::new());
        let at = order.iter().position(|&c| c == after).ok_or("receiver")?;
        for &c in order[at + 1..].iter().take_while(|&&c| c >= 0) {
            if let Some(m) = self.declared_member(c, name) {
                return Ok((m, "super"));
            }
        }
        // a class with no base in the repository inherits from outside (`object`, `Error`)
        Err(if !unknown && (outside || order.len() == at + 1) { "external" } else { "not found" })
    }

    /// The methods method `id` overrides or implements: the ones of its name declared in the
    /// types its owner extends or implements, at any depth; a Java overload of another arity is none.
    pub fn overridden(&mut self, id: i64) -> Vec<i64> {
        let Some((path, _)) = self.def_row(id) else { return Vec::new() };
        // an owner whose header names no base needs no parse
        let defs = self.defs_of(&path).unwrap_or_default();
        let Some(owner) = defs.iter().find(|d| d.id == id).and_then(|d| d.parent) else { return Vec::new() };
        let head: String = self.ix.texts(&[defs[owner].id]).ok().and_then(|mut t| t.remove(&defs[owner].id)).unwrap_or_default();
        let head = &head[..head.find('{').into_iter().chain(head.find(":\n")).min().unwrap_or(head.len())];
        // a C++ or C# base list follows a `:`
        if !head.contains(['(', ':']) && !head.split(|c: char| !c.is_alphanumeric()).any(|w| matches!(w, "for" | "extends" | "implements")) {
            return Vec::new();
        }
        let fi = self.file(&path);
        let Some(d) = fi.defs.iter().find(|d| d.id == id) else { return Vec::new() };
        let Some(owner) = d.parent.map(|p| &fi.defs[p]).filter(|o| CONTAINER_KINDS.contains(&o.kind.as_str())) else { return Vec::new() };
        let Some(lang) = fi.lang else { return Vec::new() };
        let arity = rules!(lang, R => R::arity(&fi, id));
        let (mut out, mut todo, mut seen) = (Vec::new(), self.bases(owner.id), HashSet::from([owner.id]));
        while let Some(b) = todo.pop() {
            if seen.len() > 64 || !seen.insert(b) {
                continue;
            }
            let Some((bpath, _)) = self.def_row(b) else { continue };
            let bf = self.file(&bpath);
            let declared = bf.defs.iter().filter(|m| m.name == d.name && m.parent.is_some_and(|p| bf.defs[p].id == b));
            out.extend(declared.filter(|m| arity.is_none() || rules!(lang, R => R::arity(&bf, m.id)) == arity).map(|m| m.id));
            todo.extend(self.bases(b));
        }
        out
    }

    /// Definition `name` at the top level of one of `files`, following re-exports and wildcard imports.
    fn lookup_top(&mut self, files: &[String], name: &str, depth: usize) -> Option<i64> {
        // a lookup that comes round again (wildcard import rings) finds nothing; a repeat one
        // (the base of a class named again) gets the first answer
        let key = (files.join("\n"), name.to_string());
        if let Some(r) = self.looked_up.get(&key) {
            return *r;
        }
        if self.nest > 32 {
            return None;
        }
        self.looked_up.insert(key.clone(), None);
        self.nest += 1;
        let r = self.lookup_top_inner(files, name, depth);
        self.nest -= 1;
        self.looked_up.insert(key, r);
        r
    }

    fn lookup_top_inner(&mut self, files: &[String], name: &str, depth: usize) -> Option<i64> {
        // `depth` counts the imports followed from one file into another: 6 hops at most
        if depth > 6 {
            return None;
        }
        for f in files {
            let fi = self.file(f);
            // `const helper = require('./m')` is a constant and an import: the import says what it is
            let required =
                |d: &&Def| fi.lang.is_some_and(|l| rules!(l, R => R::REQUIRE)) && d.kind == "constant" && fi.bindings.iter().any(|b| b.local == name);
            if let Some(d) = top_level(&fi, name).filter(|d| !required(d)) {
                return Some(d.id);
            }
        }
        for f in files {
            let fi = self.file(f);
            let Some(lang) = fi.lang else { continue };
            for b in fi.bindings.iter().filter(|b| b.local == name) {
                if let Some(id) = self.resolve_binding(f, lang, b, depth) {
                    return Some(id);
                }
            }
        }
        for f in files {
            let fi = self.file(f);
            let Some(lang) = fi.lang else { continue };
            for b in fi.bindings.iter().filter(|b| b.local == "*") {
                if let Some(mf) = self.module_files(f, lang, &b.module) {
                    if mf.iter().all(|m| files.contains(m)) {
                        continue;
                    }
                    if let Some(id) = self.lookup_top(&mf, name, depth + 1) {
                        return Some(id);
                    }
                }
            }
        }
        None
    }

    fn resolve_binding(&mut self, from: &str, lang: Lang, b: &Binding, depth: usize) -> Option<i64> {
        let item = match b.item.as_deref() {
            Some(item) => item,
            // a required module, called or named bare: what it exports as `module.exports`
            None if rules!(lang, R => R::REQUIRE) && !b.module.is_empty() && self.required(from, b) => "default",
            None => return None,
        };
        if b.module.is_empty() && b.local == item {
            return None; // `export {a}` names the file's own `a`, already looked up
        }
        let files = if b.module.is_empty() {
            vec![from.to_string()]
        } else {
            match self.module_files(from, lang, &b.module) {
                Some(files) => files,
                None => return rules!(lang, R => R::unfiled_item(self, from, &b.module, item)),
            }
        };
        // a name the file binds from itself (`module.exports = helper`) is no hop
        self.lookup_top(&files, item, depth + usize::from(!b.module.is_empty()))
    }

    /// Files of the module a qualifier chain (`nx.utils`, `crate::a`, `pkg`) names, if it is one.
    fn module_of_chain(&mut self, path: &str, fi: &FileInfo, lang: Lang, segs: &[&str]) -> Option<Vec<String>> {
        rules!(lang, R => R::module_of_chain(self, path, fi, segs))
    }

    /// The files of the module the one name `segs` binds to as a whole (`import * as ns`, a Go package).
    fn bound_module(&mut self, path: &str, fi: &FileInfo, lang: Lang, segs: &[&str]) -> Option<Vec<String>> {
        let b = fi.bindings.iter().find(|b| b.local == segs[0])?;
        if b.item.is_some() || segs.len() > 1 {
            return None;
        }
        self.module_files(path, lang, &b.module)
    }

    fn module_files(&mut self, from: &str, lang: Lang, module: &str) -> Option<Vec<String>> {
        rules!(lang, R => R::module_files(self, from, module))
    }

    fn first_file(&self, cands: &[String]) -> Option<String> {
        cands.iter().find(|c| self.files.contains(c.as_str())).cloned()
    }

    /// The repository files the import statement of `from` at `line`, recorded as `module`, names:
    /// the files `refs` would look its names up in. None when the resolver does not read the
    /// language of `from`; an empty list when it reads it and finds no file.
    pub fn import_files(&mut self, from: &str, line: i64, module: &str) -> Option<Vec<String>> {
        if !rules!(self.path_lang(from)?, R => R::reads_import(module)) {
            return None;
        }
        let fi = self.file(from);
        let lang = fi.lang?;
        let mut out = rules!(lang, R => R::import_files(self, from, &fi, line, module))?;
        out.retain(|f| f != from);
        let mut seen = HashSet::new();
        out.retain(|f| seen.insert(f.clone()));
        Some(out)
    }
}

/// The bindings of the import statement of one of `kinds` that starts on (1-based) `line`.
fn statement_bindings<'f>(fi: &'f FileInfo, line: i64, kinds: &[&str]) -> Vec<&'f Binding> {
    let range = statement_at(fi, line, kinds);
    fi.bindings.iter().filter(|b| range.as_ref().is_some_and(|r| b.sites.first().is_some_and(|s| r.contains(s)))).collect()
}

/// Byte range of the statement of one of `kinds` that starts on (1-based) `line`.
fn statement_at(fi: &FileInfo, line: i64, kinds: &[&str]) -> Option<std::ops::Range<usize>> {
    let row = usize::try_from(line - 1).ok()?;
    let text_line = fi.src.lines().nth(row)?;
    let column = text_line.len() - text_line.trim_start().len();
    let at = Point { row, column };
    let mut n = fi.tree.as_ref()?.root_node().named_descendant_for_point_range(at, at)?;
    loop {
        if kinds.contains(&n.kind()) && n.start_position().row == row {
            return Some(n.byte_range());
        }
        n = n.parent()?;
    }
}

fn first_leaf(mut n: Node) -> Node {
    while let Some(c) = n.named_child(0) {
        n = c;
    }
    n
}

/// (object, member name) of a member access: `a.b`, `a.b` in Rust, `pkg.F` in Go.
fn member_parts(e: Node) -> Option<(Node, Node)> {
    let (obj, mem) = match e.kind() {
        "attribute" => ("object", "attribute"),
        "member_expression" => ("object", "property"),
        "field_expression" => ("value", "field"),
        "selector_expression" => ("operand", "field"),
        "field_access" => ("object", "field"),
        _ => return None,
    };
    Some((e.child_by_field_name(obj)?, e.child_by_field_name(mem)?))
}

/// A kind of definition whose values have its members: not a trait or an interface.
fn concrete(kind: &str) -> bool {
    (CONTAINER_KINDS.contains(&kind) || kind == "type") && !matches!(kind, "trait" | "interface")
}

const CLASS_NODES: &[&str] = &["class_definition", "class_declaration", "class", "struct_item", "impl_item", "trait_item"];

/// The syntax node of definition `id` of the file: the first node on its first line that
/// carries its name.
fn def_node(fi: &FileInfo, id: i64) -> Option<Node<'_>> {
    let d = fi.defs.iter().find(|d| d.id == id)?;
    let (first, last) = ((d.start - 1).max(0) as usize, (d.end - 1).max(0) as usize);
    let mut stack = vec![fi.tree.as_ref()?.root_node()];
    while let Some(n) = stack.pop() {
        if n.end_position().row < first || n.start_position().row > last {
            continue;
        }
        let named = n.child_by_field_name("name").is_some_and(|x| text(x, &fi.src) == d.name);
        if named && n.start_position().row >= first {
            return Some(n);
        }
        let mut cur = n.walk();
        let kids: Vec<Node> = n.named_children(&mut cur).collect();
        stack.extend(kids.into_iter().rev());
    }
    None
}

/// Whether a function lies between `n` and its ancestor `top`.
fn has_function_between(n: Node, top: Node) -> bool {
    let mut a = n.parent();
    while let Some(x) = a {
        if x.id() == top.id() {
            return false;
        }
        if FUNCTION_NODES.contains(&x.kind()) {
            return true;
        }
        a = x.parent();
    }
    false
}

fn top_level<'f>(fi: &'f FileInfo, name: &str) -> Option<&'f Def> {
    let mut it = fi.defs.iter().filter(|d| d.name == name && d.parent.is_none());
    it.clone().find(|d| d.kind != "impl").or_else(|| it.next())
}

impl Resolver<'_> {
    /// A parameter or local variable named `name` in the functions around `node` (a closure sees
    /// the locals of the function it is written in) hides outer definitions; one bound in a nested
    /// function, block or comprehension that does not hold `node` does not.
    fn shadowed(&mut self, lang: Lang, node: Node, name: &str, src: &str) -> bool {
        // a JavaScript class expression binds its name inside the class only
        if lang == Lang::Js
            && std::iter::successors(node.parent(), |n| n.parent())
                .any(|c| c.kind() == "class" && c.child_by_field_name("name").is_some_and(|x| text(x, src) == name))
        {
            return true;
        }
        let at = node.start_byte();
        // a Python class body is a scope of its own for code directly in it, not for its methods or
        // comprehensions (save the first iterable of one, which runs in the class body)
        if lang == Lang::Python {
            let around = std::iter::successors(Some(node), |n| n.parent())
                .zip(std::iter::successors(node.parent(), |n| n.parent()))
                .find(|(_, p)| FUNCTION_NODES.contains(&p.kind()) || p.kind() == "class_definition" || PY_COMPREHENSIONS.contains(&p.kind()));
            let in_body = around.filter(|(child, p)| {
                !PY_COMPREHENSIONS.contains(&p.kind()) || python_first_iterable(*p).is_some_and(|it| it.byte_range().contains(&child.start_byte()))
            });
            if let Some((_, p)) = in_body {
                // the first iterable of a comprehension: its class body, if there is one around
                let class = std::iter::successors(Some(p), |n| n.parent()).find(|n| FUNCTION_NODES.contains(&n.kind()) || n.kind() == "class_definition");
                if let Some(body) = class.filter(|c| c.kind() == "class_definition").and_then(|c| c.child_by_field_name("body")) {
                    if python_class_binds(body, name, src, node) {
                        return true;
                    }
                }
            }
        }
        let Some(mut func) = enclosing_function(node) else { return false };
        // a Rust `fn` item does not see the locals of the function around it
        while let Some(outer) = enclosing_function(func).filter(|_| func.kind() != "function_item") {
            func = outer;
        }
        // per binder: the block or function a use must be in to see it, its span, `visible_spans`
        let rows = self.found_in(func, "shadowing binders", name, || {
            let binds = |n: Node| {
                matches!(n.kind(), "identifier" | "shorthand_property_identifier_pattern" | "shorthand_field_identifier")
                    && text(n, src) == name
                    && is_local_binder(n, src)
            };
            let mut rows: Vec<[(usize, usize); 5]> = Self::binders_in(func, binds)
                .into_iter()
                .filter_map(|(start, end)| {
                    let b = func.descendant_for_byte_range(start, end)?;
                    let [within, not, nor] = visible_spans(b);
                    Some([shadow_scope(lang, b, func), (b.start_byte(), b.end_byte()), within, not, nor])
                })
                .collect();
            rows.sort_unstable();
            rows.concat()
        });
        let (rows, _) = rows.as_chunks::<5>();
        let seen_in = |scope: (usize, usize)| {
            let rows = &rows[rows.partition_point(|r| r[0] < scope)..];
            rows.iter().take_while(|r| r[0] == scope).any(|&[_, (b, _), within, not, nor]| b == at || sees([within, not, nor], at))
        };
        // the scopes that hold the use are the nodes around it
        seen_in(EVERYWHERE)
            || std::iter::successors(Some(node), |n| n.parent())
                .take_while(|n| n.id() != func.id())
                .map(|n| (n.start_byte(), n.end_byte()))
                .any(|scope| holds(scope, at) && seen_in(scope))
    }
}

/// The innermost block or function between binder `b` and its function `func` (a JavaScript
/// `var` skips blocks), else everywhere: a use outside it does not see `b`.
fn shadow_scope(lang: Lang, b: Node, func: Node) -> (usize, usize) {
    let blocks = block_scoped(lang, b);
    std::iter::successors(Some(b), |n| n.parent())
        .take_while(|n| n.id() != func.id())
        .find(|n| blocks.contains(&n.kind()) || FUNCTION_NODES.contains(&n.kind()))
        .map_or(EVERYWHERE, |n| (n.start_byte(), n.end_byte()))
}

/// The nodes binder `b` does not leave: the language's blocks, none for a binder of the whole function.
fn block_scoped(lang: Lang, b: Node) -> &'static [&'static str] {
    rules!(lang, R => if R::function_wide(b) { &[] } else { R::BLOCKS })
}

/// The bytes a binder `b` in the function `scope` can reach: the innermost block, function or
/// class around it (a JavaScript `var` skips blocks), else all of `scope`.
fn binder_reach(lang: Lang, b: Node, scope: Node) -> (usize, usize) {
    let blocks = block_scoped(lang, b);
    let mut n = b.parent();
    // the name of a function or class binds around it; that of a function expression only inside
    if let Some(d) = n.filter(|&d| names_definition(d, b) && !matches!(d.kind(), "function_expression" | "function" | "generator_function" | "class")) {
        n = d.parent();
    }
    while let Some(p) = n.filter(|p| p.id() != scope.id()) {
        if blocks.contains(&p.kind()) || FUNCTION_NODES.contains(&p.kind()) || CLASS_NODES.contains(&p.kind()) {
            return (p.start_byte(), p.end_byte());
        }
        n = p.parent();
    }
    (scope.start_byte(), scope.end_byte())
}

/// Whether `n` is the name of the function or class `d`.
fn names_definition(d: Node, n: Node) -> bool {
    (FUNCTION_NODES.contains(&d.kind()) || CLASS_NODES.contains(&d.kind())) && d.child_by_field_name("name").is_some_and(|x| x.id() == n.id())
}

/// The Python nodes whose names do not see a class body's: comprehensions.
const PY_COMPREHENSIONS: &[&str] = &["list_comprehension", "set_comprehension", "dictionary_comprehension", "generator_expression"];

/// The iterable of a comprehension's first `for`, which runs in the scope around it.
fn python_first_iterable(c: Node) -> Option<Node> {
    let mut cur = c.walk();
    let first = c.named_children(&mut cur).find(|n| n.kind() == "for_in_clause");
    first?.child_by_field_name("right")
}

/// The names a Python assignment or `for` target binds: `h`, `h, x`, `[a, (b, *c)]`.
fn python_targets(t: Node) -> Vec<Node> {
    let mut out = Vec::new();
    let mut stack = vec![t];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "identifier" => out.push(n),
            "pattern_list" | "tuple_pattern" | "list_pattern" | "parenthesized_expression" | "list_splat_pattern" => {
                let mut cur = n.walk();
                stack.extend(n.named_children(&mut cur));
            }
            _ => {}
        }
    }
    out
}

/// Whether Python class body `body` has bound `name` when `use_` runs: a binding (an assignment,
/// a tuple or `for` target, a walrus, a `def` or `class`) anywhere in the body's own statements
/// that is done by then. `h = h` reads the outer `h` on its right side; a nested function, class,
/// lambda or comprehension binds nothing here.
fn python_class_binds(body: Node, name: &str, src: &str, use_: Node) -> bool {
    let at = use_.start_byte();
    let targets = python_targets;
    let named = |ns: &[Node]| ns.iter().any(|t| text(*t, src) == name);
    let target_is_use = |ns: &[Node]| ns.iter().any(|t| t.id() == use_.id());
    let mut stack = vec![body];
    while let Some(n) = stack.pop() {
        let done = n.end_byte() <= at;
        let bound = match n.kind() {
            // `h: int` alone binds nothing
            "assignment" if n.child_by_field_name("right").is_some() => {
                let ts = n.child_by_field_name("left").map(targets).unwrap_or_default();
                named(&ts) && (done || target_is_use(&ts))
            }
            "augmented_assignment" => {
                let ts = n.child_by_field_name("left").map(targets).unwrap_or_default();
                named(&ts) && (done || target_is_use(&ts))
            }
            // the target is bound before the body runs
            "for_statement" => {
                let ts = n.child_by_field_name("left").map(targets).unwrap_or_default();
                let after_iter = n.child_by_field_name("right").is_some_and(|r| r.end_byte() <= at);
                named(&ts) && (after_iter || target_is_use(&ts))
            }
            "named_expression" => n.child_by_field_name("name").is_some_and(|x| text(x, src) == name && (done || x.id() == use_.id())),
            "function_definition" | "class_definition" => n.child_by_field_name("name").is_some_and(|x| text(x, src) == name && done),
            _ => false,
        };
        if bound {
            return true;
        }
        // a nested scope binds nothing in the class body; a decorated definition is its definition
        if n.id() == body.id() || !(FUNCTION_NODES.contains(&n.kind()) || PY_COMPREHENSIONS.contains(&n.kind()) || n.kind() == "class_definition") {
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
    }
    false
}

/// The innermost function around `node`.
fn enclosing_function(node: Node) -> Option<Node> {
    let mut f = node.parent();
    while let Some(n) = f {
        if FUNCTION_NODES.contains(&n.kind()) {
            return Some(n);
        }
        f = n.parent();
    }
    None
}

/// The byte ranges of the assignments to the variable `name` in `scope`.
fn assignments(scope: Node, name: &str, src: &str) -> Vec<(usize, usize)> {
    let (mut out, mut stack) = (Vec::new(), vec![scope]);
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "assignment_expression" | "assignment" | "assignment_statement") {
            if let Some(l) = n.child_by_field_name("left") {
                let mut cur = l.walk();
                let hit = if l.kind() == "expression_list" { l.named_children(&mut cur).any(|x| text(x, src) == name) } else { text(l, src) == name };
                if hit {
                    out.push((n.start_byte(), n.end_byte()));
                }
            }
        }
        let mut cur = n.walk();
        stack.extend(n.named_children(&mut cur));
    }
    out
}

impl Resolver<'_> {
    /// The methods of the file of `id` with its qualified name, in source order: its overloads,
    /// itself included. Only `id` for a definition that has none.
    pub fn overloads(&mut self, id: i64) -> Vec<i64> {
        let Some((path, qualname)) = self.def_row(id) else { return vec![id] };
        let fi = self.file(&path);
        let all: Vec<i64> = fi.defs.iter().filter(|d| d.qualname == qualname && d.kind == "method").map(|d| d.id).collect();
        if all.contains(&id) {
            all
        } else {
            vec![id]
        }
    }
}

impl Resolver<'_> {
    /// `name` as a member type `id` inherits, by `bases` (Java, C#, C++): its bases in header
    /// order, each with its own bases first, at most 8 deep. A type the walk came to before at
    /// that depth or less is not walked again, so a diamond of many paths reads each type once
    /// per depth.
    pub(crate) fn inherited_through(&mut self, id: i64, name: &str, depth: usize, bases: fn(&mut Resolver, i64) -> Vec<i64>) -> Option<i64> {
        self.inherited_walk(id, name, depth, bases, &mut HashMap::new())
    }

    fn inherited_walk(&mut self, id: i64, name: &str, depth: usize, bases: fn(&mut Resolver, i64) -> Vec<i64>, seen: &mut HashMap<i64, usize>) -> Option<i64> {
        if depth >= 8 || self.nest > 32 {
            return None;
        }
        self.nest += 1;
        let mut found = None;
        for bid in bases(self, id) {
            if bid != id && !seen_at(seen, bid, depth + 1) {
                found = self.declared_member(bid, name).or_else(|| self.inherited_walk(bid, name, depth + 1, bases, seen));
                if found.is_some() {
                    break;
                }
            }
        }
        self.nest -= 1;
        found
    }
}

/// Whether a walk over base types already came to `id` at `depth` or less, whose answer stands
/// for this visit too; marks it otherwise. A type reached by many paths is read once per depth.
pub(crate) fn seen_at(seen: &mut HashMap<i64, usize>, id: i64, depth: usize) -> bool {
    if seen.get(&id).is_some_and(|&d| d <= depth) {
        return true;
    }
    seen.insert(id, depth);
    false
}

// ------------------------------------------------------------------------- refs
/// Whether two spellings are one name: they compare in NFC.
pub fn same_name(a: &str, b: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;
    a == b || a.nfc().eq(b.nfc())
}

pub struct Occurrence {
    pub path: String,
    pub line: u64,
    /// 1-based byte column
    pub column: u64,
    /// the line, shared by every use on it
    pub text: std::sync::Arc<str>,
    /// where the use starts in `text`, in bytes: not `column - 1` after a byte that is not UTF-8
    pub at: usize,
    pub resolution: Resolution,
}

impl Occurrence {
    /// The line as an answer shows it, and whether it was cut: whole when short, else the part
    /// around the use. A minified line holds thousands of uses, and each would print all of it.
    pub fn shown_line(&self) -> (std::borrow::Cow<'_, str>, bool) {
        const WHOLE: usize = 400;
        const SIDE: usize = 150;
        let t = &*self.text;
        if t.len() <= WHOLE {
            return (t.trim().into(), false);
        }
        let at = self.at.min(t.len());
        let (mut a, mut b) = (at.saturating_sub(SIDE), (at + SIDE).min(t.len()));
        while !t.is_char_boundary(a) {
            a -= 1;
        }
        while !t.is_char_boundary(b) {
            b += 1;
        }
        let (before, after) = (if a > 0 { "..." } else { "" }, if b < t.len() { "..." } else { "" });
        (format!("{before}{}{after}", &t[a..b]).into(), true)
    }
}

pub struct Resolved {
    /// definitions named `name`, plus any other definition an occurrence resolved to
    pub definitions: Vec<ChunkRow>,
    pub occurrences: Vec<Occurrence>,
    pub truncated: bool,
    /// files whose search stopped at a NUL byte, each with what the search said: a use after
    /// that byte is not among the occurrences
    pub notes: Vec<(String, String)>,
    /// by overloaded definition: the first of its overloads, which stands for them all where the
    /// arguments of a call do not pick one
    pub(crate) first_overload: HashMap<i64, i64>,
}

impl Resolved {
    pub fn of(&self, id: i64) -> impl Iterator<Item = &Occurrence> {
        let first = self.first_overload.get(&id).copied();
        self.occurrences.iter().filter(move |o| matches!(o.resolution, Ok((d, why)) if d == id || (why == "overload" && Some(d) == first)))
    }
    pub fn unresolved(&self) -> impl Iterator<Item = &Occurrence> {
        self.occurrences.iter().filter(|o| o.resolution.is_err())
    }
}

/// The Engine A search for the whole-word occurrences of `bare`, the first `max_results` in path
/// order, in its composed and its decomposed spelling: names compare in NFC, and a file may hold
/// either.
pub fn name_search(bare: &str, max_results: u64) -> crate::instant::Query {
    use unicode_normalization::UnicodeNormalization;
    let (nfc, nfd): (String, String) = (bare.nfc().collect(), bare.nfd().collect());
    let (text, mode) = if nfc == nfd {
        (bare.to_string(), crate::instant::Mode::Literal)
    } else {
        (format!("(?:{}|{})", regex::escape(&nfc), regex::escape(&nfd)), crate::instant::Mode::Regex)
    };
    let pattern = crate::instant::Pattern { text, mode, word: true, ..Default::default() };
    crate::instant::Query { pattern, max_results: Some(max_results), first_in_path_order: true, ..Default::default() }
}

/// Plain-text rendering of `Names::references`, optionally for the one definition `only`.
pub fn resolved_text(r: &Resolved, only: Option<i64>) -> String {
    let mut out = String::new();
    for d in r.definitions.iter().filter(|d| only.is_none_or(|id| d.id == id)) {
        let occ: Vec<&Occurrence> = r.of(d.id).collect();
        out.push_str(&format!("{}:{}-{}  [{}] {}  ({} references)\n", shown(&d.path), d.start, d.end, d.kind, shown(&d.qualname), occ.len()));
        for o in occ {
            out.push_str(&format!("    {}:{}:{}: {}\n", shown(&o.path), o.line, o.column, o.shown_line().0));
        }
    }
    let unresolved: Vec<&Occurrence> = r.unresolved().collect();
    match only {
        Some(id) => {
            let elsewhere = r.occurrences.len() - unresolved.len() - r.of(id).count();
            out.push_str(&format!("({elsewhere} other occurrences resolve to other definitions, {} are unresolved)\n", unresolved.len()));
        }
        None => {
            out.push_str(&format!("unresolved ({}):\n", unresolved.len()));
            for o in unresolved {
                out.push_str(&format!("    {}:{}:{}: {}  ({})\n", shown(&o.path), o.line, o.column, o.shown_line().0, o.resolution.err().unwrap_or("")));
            }
        }
    }
    for (path, note) in &r.notes {
        out.push_str(&format!("note: {}: {note}\n", shown(path)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::java::java_package;
    use super::js::plain_json;
    use super::rust::cargo_package_name;
    use super::*;
    use crate::search::Searcher;

    fn parse(lang: Lang, id: &str, src: &str) -> Vec<Binding> {
        let spec = crate::langs::by_id(id).unwrap();
        let mut p = Parser::new();
        p.set_language(&(spec.language)()).unwrap();
        let t = p.parse(src.as_bytes(), None).unwrap();
        bindings_of(lang, "m.js", t.root_node(), src).into_iter().map(|b| Binding { sites: vec![], ..b }).collect()
    }

    fn b(local: &str, module: &str, item: Option<&str>) -> Binding {
        Binding { local: local.into(), module: module.into(), item: item.map(String::from), sites: vec![] }
    }

    fn check(got: &[Binding], want: &[Binding]) {
        for w in want {
            assert!(got.contains(w), "missing {w:?} in {got:?}");
        }
        assert_eq!(got.len(), want.len(), "{got:?}");
    }

    #[test]
    fn python_bindings() {
        let got = parse(Lang::Python, "python", "from . import x\nfrom ..a.b import c as d\nfrom ..a.b import *\nimport a.b as e, f.g\n");
        check(&got, &[b("*", "..a.b", None), b("x", ".", Some("x")), b("d", "..a.b", Some("c")), b("e", "a.b", None), b("f", "f", None)]);
    }

    #[test]
    fn js_bindings_cover_imports_reexports_and_require() {
        let src = "import {a as b, c} from './x';\nimport d, * as ns from './y';\nexport {e as f} from './z';\nexport * from './w';\nexport default foo;\nconst {g, h: i} = require('./v');\nconst j = require('./u');\n";
        let got = parse(Lang::Js, "javascript", src);
        check(
            &got,
            &[
                b("b", "./x", Some("a")),
                b("c", "./x", Some("c")),
                b("d", "./y", Some("default")),
                b("ns", "./y", None),
                b("f", "./z", Some("e")),
                b("*", "./w", None),
                b("default", "", Some("foo")),
                b("g", "./v", Some("g")),
                b("i", "./v", Some("h")),
                b("j", "./u", None),
            ],
        );
    }

    #[test]
    fn rust_use_trees_are_flattened() {
        let got = parse(Lang::Rust, "rust", "use crate::a::{self, b::C as D, e::*};\nuse super::x;\nuse std::fmt;\n");
        check(
            &got,
            &[
                b("a", "crate", Some("a")),
                b("D", "crate::a::b", Some("C")),
                b("*", "crate::a::e", None),
                b("x", "super", Some("x")),
                b("fmt", "std", Some("fmt")),
            ],
        );
    }

    #[test]
    fn go_imports_bind_package_names() {
        let got = parse(Lang::Go, "go", "package p\nimport (\n  m \"example.com/x/y\"\n  . \"fmt\"\n  _ \"embed\"\n  \"example.com/z/v2\"\n  \"os\"\n)\n");
        assert_eq!(got, vec![b("m", "example.com/x/y", None), b("*", "fmt", None), b("z", "example.com/z/v2", None), b("os", "os", None)]);
    }

    /// Index `files` in a fresh directory and resolve `name` where it appears inside `needle` on the
    /// line of `path` containing it; answers "path qualname" of the definition or the failure reason.
    fn indexed(tag: &str, files: &[(&str, &str)]) -> (std::path::PathBuf, Searcher) {
        let root = std::env::temp_dir().join(format!("scrutate_resolve_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (p, body) in files {
            let f = root.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, body).unwrap();
        }
        let (s, _) = crate::open_updated(&root, false).unwrap();
        let s = s.into_searcher();
        (root, s)
    }

    fn resolver_case(tag: &str, files: &[(&str, &str)], checks: &[(&str, &str, &str, &str)]) {
        let (root, s) = indexed(tag, files);
        let mut rv = Resolver::new(&s.ix).unwrap();
        for (path, needle, name, want) in checks {
            let src = files.iter().find(|f| f.0 == *path).unwrap().1;
            let (row, l) = src.lines().enumerate().find(|(_, l)| l.contains(needle)).unwrap_or_else(|| panic!("{needle:?} not in {path}"));
            let col = l.find(needle).unwrap() + needle.find(name).unwrap();
            let got = match rv.resolve_at(path, row as u64 + 1, col, name) {
                Ok((id, _)) => rv.def_row(id).map(|(p, q)| format!("{p} {q}")).unwrap(),
                Err(e) => e.to_string(),
            };
            assert_eq!(&got, want, "{path}: {needle}");
        }
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn python_rules() {
        let a = "def f():\n    pass\n\n\nclass C:\n    def m(self):\n        return self.n()\n\n    def n(self):\n        pass\n\n\ndef g():\n    def f():\n        pass\n    return f()\n\n\ndef h(f):\n    return f + 1\n\n\n# f in a comment\n";
        let b = "import a\nfrom a import f as ff\nfrom pkg import w\nimport os\n\n\nclass D(a.C):\n    def k(self):\n        return self.m()\n\n\ndef use():\n    ff()\n    a.f()\n    w()\n    os.f()\n";
        resolver_case(
            "py",
            &[("a.py", a), ("b.py", b), ("pkg/__init__.py", "from .impl import *\n"), ("pkg/impl.py", "def w():\n    pass\n")],
            &[
                ("a.py", "f():", "f", "a.py f"),
                ("a.py", "self.n()", "n", "a.py C.n"),
                ("a.py", "return f()", "f", "a.py g.f"),
                ("a.py", "return f + 1", "f", "local variable"),
                ("a.py", "# f in", "f", "comment or string"),
                ("b.py", "import f as ff", "f", "a.py f"),
                ("b.py", "ff()", "ff", "a.py f"),
                ("b.py", "a.f()", "f", "a.py f"),
                ("b.py", "    w()", "w", "pkg/impl.py w"),
                ("b.py", "self.m()", "m", "a.py C.m"),
                ("b.py", "os.f()", "f", "external"),
            ],
        );
    }

    #[test]
    fn a_bare_python_import_names_the_module_beside_the_file_when_no_root_module_has_the_name() {
        let t = "from helpers import make_items\nimport common\nimport sub.deep\nfrom pkg import w\nimport os\n\n\ndef test_it():\n    make_items()\n    common.fmt()\n    sub.deep.go()\n    w()\n    os.f()\n";
        let run = "import logging\nfrom nowhere import x\n\n\ndef main():\n    logging.info()\n    x()\n";
        let js = "import { u } from 'util';\n\nexport function f() {\n  return u();\n}\n";
        resolver_case(
            "pysib",
            &[
                ("tests/test_it.py", t),
                ("tests/helpers.py", "def make_items():\n    return [1]\n"),
                ("tests/common/__init__.py", "def fmt():\n    pass\n"),
                ("tests/sub/deep.py", "def go():\n    pass\n"),
                ("tests/pkg.py", "def w():\n    pass\n"),
                ("pkg/__init__.py", "def w():\n    pass\n"),
                ("scripts/run.py", run),
                ("scripts/logging.py", "def info():\n    pass\n"),
                ("web/app.js", js),
                ("web/util.js", "export function u() {\n  return 1;\n}\n"),
            ],
            &[
                ("tests/test_it.py", "import make_items", "make_items", "tests/helpers.py make_items"),
                ("tests/test_it.py", "    make_items()", "make_items", "tests/helpers.py make_items"),
                ("tests/test_it.py", "common.fmt()", "fmt", "tests/common/__init__.py fmt"),
                ("tests/test_it.py", "sub.deep.go()", "go", "tests/sub/deep.py go"),
                // a package path from the root wins over a sibling of the same name
                ("tests/test_it.py", "    w()", "w", "pkg/__init__.py w"),
                ("tests/test_it.py", "os.f()", "f", "external"),
                // Python puts the script's own directory first, ahead of the standard library
                ("scripts/run.py", "logging.info()", "info", "scripts/logging.py info"),
                ("scripts/run.py", "    x()", "x", "external"),
                // a bare JavaScript specifier is a package
                ("web/app.js", " u()", "u", "external"),
            ],
        );
    }

    #[test]
    fn js_ts_rules() {
        resolver_case(
            "js",
            &[
                ("x.js", "export function f() {}\n"),
                ("y.js", "export function f() {}\n"),
                ("index.js", "export {f as g} from './y';\n"),
                (
                    "use.js",
                    "import {f} from './x';\nimport {g} from './index';\nimport * as Y from './y';\nconst {f: h} = require('./y');\nf();\ng();\nY.f();\nh();\n",
                ),
                ("m.ts", "export class K {\n  run() { return this.stop(); }\n  stop() {}\n}\n"),
                ("n.ts", "import {K} from './m.js';\nimport React from 'react';\nnew K();\nReact.f();\n"),
            ],
            &[
                ("use.js", "f();", "f", "x.js f"),
                ("use.js", "g();", "g", "y.js f"),
                ("use.js", "Y.f();", "f", "y.js f"),
                ("use.js", "h();", "h", "y.js f"),
                ("n.ts", "new K()", "K", "m.ts K"),
                ("m.ts", "this.stop()", "stop", "m.ts K.stop"),
                ("n.ts", "React.f()", "f", "external"),
            ],
        );
    }

    #[test]
    fn ts_path_aliases_and_base_url() {
        let config = "{\n  // names for our own folders\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    /* longest prefix wins */\n    \"paths\": { \"@lib/*\": [\"src/lib/*\"], \"@lib/deep/*\": [\"src/deep/*\"], \"@cfg\": [\"src/config.ts\"], },\n  },\n}\n";
        resolver_case(
            "tspaths",
            &[
                ("tsconfig.json", config),
                ("src/lib/math.ts", "export function add(a: number, b: number) { return a + b; }\n"),
                ("src/deep/math.ts", "export function mul(a: number, b: number) { return a * b; }\n"),
                ("src/config.ts", "export function load() { return 1; }\n"),
                ("src/app/util.ts", "export function helper() { return 2; }\n"),
                (
                    "src/app/main.ts",
                    "import { add } from \"@lib/math\";\nimport { mul } from \"@lib/deep/math\";\nimport { load } from \"@cfg\";\nimport { helper } from \"src/app/util\";\nimport { x } from \"left-pad\";\n\nexport function run() {\n  return add(load(), mul(helper(), x()));\n}\n",
                ),
                ("tsconfig.base.json", "{\"compilerOptions\": {\"paths\": {\"@shared/*\": [\"packages/shared/src/*\"]}}}\n"),
                ("packages/web/tsconfig.json", "{\"extends\": \"../../tsconfig.base\"}\n"),
                ("packages/shared/src/id.ts", "export function id() { return 3; }\n"),
                ("packages/web/src/page.ts", "import { id } from \"@shared/id\";\n\nexport function page() {\n  return id();\n}\n"),
            ],
            &[
                ("src/app/main.ts", "add(load()", "add", "src/lib/math.ts add"),
                ("src/app/main.ts", "mul(helper()", "mul", "src/deep/math.ts mul"),
                ("src/app/main.ts", "load()", "load", "src/config.ts load"),
                ("src/app/main.ts", "helper()", "helper", "src/app/util.ts helper"),
                ("src/app/main.ts", "x()", "x", "external"),
                ("packages/web/src/page.ts", "return id()", "id", "packages/shared/src/id.ts id"),
            ],
        );
        // the file-level view of the same imports: dependencies and dependents
        let (root, s) = indexed(
            "tsdeps",
            &[
                ("tsconfig.json", config),
                ("src/lib/math.ts", "export function add(a: number, b: number) { return a + b; }\n"),
                (
                    "src/app/main.ts",
                    "import { add } from \"@lib/math\";\nimport { x } from \"left-pad\";\n\nexport function run() {\n  return add(1, x());\n}\n",
                ),
            ],
        );
        let deps = crate::names::Names::new(&s).unwrap().imports("src/app/main.ts").unwrap();
        let resolved: Vec<(&str, Option<&str>)> = deps.iter().map(|d| (d.module.as_str(), d.resolved.as_deref())).collect();
        assert_eq!(resolved, [("@lib/math", Some("src/lib/math.ts")), ("left-pad", None)]);
        let importers = crate::names::Names::new(&s).unwrap().importers("src/lib/math.ts").unwrap().0;
        assert_eq!(importers, [("src/app/main.ts".to_string(), 1, "@lib/math".to_string())]);
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(plain_json("{\"a\": \"x // y\", /* c */ \"b\": [1, 2,], // d\n}"), "{\"a\": \"x // y\",  \"b\": [1, 2]}");
    }

    #[test]
    fn ts_config_dir_stands_for_the_folder_of_the_config_the_lookup_started_from() {
        resolver_case(
            "tsconfigdir",
            &[
                ("tsconfig.base.json", "{\"compilerOptions\": {\"paths\": {\"@app/*\": [\"${configDir}/src/*\"]}}}\n"),
                ("packages/web/tsconfig.json", "{\"extends\": \"../../tsconfig.base.json\"}\n"),
                ("packages/web/src/util.ts", "export function helper() { return 1; }\n"),
                ("packages/web/src/main.ts", "import { helper } from \"@app/util\";\n\nexport function main() {\n  return helper();\n}\n"),
                ("tsconfig.other.json", "{\"compilerOptions\": {\"strict\": true}}\n"),
                ("tsconfig.url.json", "{\"compilerOptions\": {\"baseUrl\": \"${configDir}/src\"}}\n"),
                ("packages/api/tsconfig.json", "{\"extends\": [\"../../tsconfig.other.json\", \"../../tsconfig.url.json\"]}\n"),
                ("packages/api/src/lib/db.ts", "export function db() { return 2; }\n"),
                ("packages/api/src/main.ts", "import { db } from \"lib/db\";\n\nexport function main() {\n  return db();\n}\n"),
                // not at the start: TypeScript leaves the text as it is
                ("packages/mid/tsconfig.json", "{\"compilerOptions\": {\"paths\": {\"@mid/*\": [\"x/${configDir}/*\"]}}}\n"),
                ("packages/mid/x/${configDir}/m.ts", "export function m() { return 3; }\n"),
                ("packages/mid/x/packages/mid/m.ts", "export function m() { return 4; }\n"),
                ("packages/mid/main.ts", "import { m } from \"@mid/m\";\n\nexport function main() {\n  return m();\n}\n"),
            ],
            &[
                ("packages/web/src/main.ts", "return helper()", "helper", "packages/web/src/util.ts helper"),
                ("packages/api/src/main.ts", "return db()", "db", "packages/api/src/lib/db.ts db"),
                ("packages/mid/main.ts", "return m()", "m", "packages/mid/x/${configDir}/m.ts m"),
            ],
        );
    }

    #[test]
    fn workspace_packages_by_their_package_json_name() {
        let files: &[(&str, &str)] = &[
            ("package.json", "{\"name\": \"acme\", \"private\": true, \"workspaces\": [\"packages/*\"]}\n"),
            ("packages/utils/package.json", "{\"name\": \"@acme/utils\", \"exports\": {\".\": \"./src/index.ts\", \"./math\": \"./src/math.ts\", \"./feat/*\": \"./src/feat/*.ts\"}}\n"),
            ("packages/utils/src/index.ts", "export function slugify(s: string) { return s; }\n"),
            ("packages/utils/src/math.ts", "export function half(n: number) { return n / 2; }\n"),
            ("packages/utils/src/feat/a.ts", "export function a() { return 1; }\n"),
            ("packages/legacy/package.json", "{\"name\": \"legacy\", \"main\": \"./dist/index.js\"}\n"),
            ("packages/legacy/src/index.ts", "export function old() { return 1; }\n"),
            ("packages/cond/package.json", "{\"name\": \"cond\", \"exports\": {\".\": {\"require\": \"./src/req.js\", \"import\": \"./src/imp.js\", \"types\": \"./dist/index.d.ts\"}, \"./x\": {\"require\": \"./src/req.js\", \"import\": \"./src/imp.js\"}}}\n"),
            ("packages/cond/src/index.ts", "export function c() { return 1; }\n"),
            ("packages/cond/src/req.js", "exports.r = 1;\n"),
            ("packages/cond/src/imp.js", "export const i = 1;\n"),
            ("packages/nested/package.json", "{\"name\": \"nested\", \"exports\": {\"import\": {\"types\": \"./lib/n.d.ts\", \"default\": \"./lib/n.js\"}}}\n"),
            ("packages/nested/lib/n.js", "export const n = 1;\n"),
            ("packages/plat/package.json", "{\"name\": \"plat\", \"exports\": {\"browser\": \"./src/web.ts\", \"node\": {\"import\": {\"default\": \"./src/node.ts\"}}}}\n"),
            ("packages/plat/src/node.ts", "export const p = 1;\n"),
            ("packages/rt/package.json", "{\"name\": \"rt\", \"exports\": {\"deno\": \"./src/deno.ts\", \"browser\": \"./src/web.ts\"}}\n"),
            ("packages/rt/src/deno.ts", "export const r = 1;\n"),
            ("packages/rt/src/web.ts", "export const r = 2;\n"),
            ("packages/plat/src/web.ts", "export const p = 2;\n"),
            ("packages/typed/package.json", "{\"name\": \"typed\", \"types\": \"index.d.ts\", \"main\": \"index.js\"}\n"),
            ("packages/typed/index.js", "export function t() { return 1; }\n"),
            ("packages/typed/index.d.ts", "export function t(): number;\n"),
            ("packages/typed2/package.json", "{\"name\": \"typed2\", \"exports\": {\"types\": \"./index.d.ts\", \"default\": \"./index.js\"}}\n"),
            ("packages/typed2/index.js", "export function t() { return 1; }\n"),
            ("packages/typed2/index.d.ts", "export function t(): number;\n"),
            ("packages/app/src/m.js", "export function f() { return 1; }\n"),
            ("packages/app/src/m.d.ts", "export function f(): number;\n"),
            ("packages/app/src/only.d.ts", "export function o(): number;\n"),
            ("packages/mod/package.json", "{\"name\": \"modpkg\", \"module\": \"esm/m.js\", \"main\": \"cjs/m.js\"}\n"),
            ("packages/mod/esm/m.js", "export const m = 1;\n"),
            ("packages/mod/cjs/m.js", "exports.m = 1;\n"),
            ("packages/bare/package.json", "{\"name\": \"bare\"}\n"),
            ("packages/bare/index.js", "export const b = 1;\n"),
            ("packages/bare/tools.ts", "export const t = 1;\n"),
            ("packages/bare/dir/index.ts", "export const d = 1;\n"),
            ("packages/srconly/package.json", "{\"name\": \"srconly\", \"main\": \"dist/index.js\"}\n"),
            ("packages/srconly/src/index.ts", "export const s = 1;\n"),
            ("packages/srconly/src/tools.ts", "export const t = 1;\n"),
            ("packages/dup/package.json", "{\"name\": \"dup\"}\n"),
            ("packages/dup/index.ts", "export const d = 1;\n"),
            ("apps/web/package.json", "{\"name\": \"dup\"}\n"),
            ("apps/web/index.ts", "export const d = 2;\n"),
            ("apps/web/tsconfig.json", "{\"compilerOptions\": {\"paths\": {\"legacy\": [\"shim.ts\"]}}}\n"),
            ("apps/web/shim.ts", "export function old() { return 2; }\n"),
            ("apps/web/src/x.ts", "import { d } from \"dup\";\n"),
            (
                "packages/app/src/main.ts",
                "import { slugify } from \"@acme/utils\";\nimport { half } from \"@acme/utils/math\";\nimport { old } from \"legacy\";\nimport React from \"react\";\n\nexport function run() {\n  return old(slugify(\"x\")) + half(2) + React.f();\n}\n",
            ),
        ];
        let (root, s) = indexed("workspace", files);
        let mut rv = Resolver::new(&s.ix).unwrap();
        let app = "packages/app/src/main.ts";
        for (from, spec, want) in [
            (app, "@acme/utils", Some("packages/utils/src/index.ts")),
            (app, "@acme/utils/math", Some("packages/utils/src/math.ts")),
            (app, "@acme/utils/feat/a", Some("packages/utils/src/feat/a.ts")),
            (app, "@acme/utils/feat/zzz", None),
            // `main` names a build output the repository does not hold: its source under `src/`
            (app, "legacy", Some("packages/legacy/src/index.ts")),
            // conditions: `types` first, its `dist` declarations fall back to the source
            (app, "cond", Some("packages/cond/src/index.ts")),
            (app, "cond/x", Some("packages/cond/src/imp.js")),
            (app, "nested", Some("packages/nested/lib/n.js")),
            // no condition of the usual four: `node`, then the others
            (app, "plat", Some("packages/plat/src/node.ts")),
            (app, "rt", Some("packages/rt/src/web.ts")),
            (app, "modpkg", Some("packages/mod/esm/m.js")),
            // a declaration beside its implementation: the implementation
            (app, "./m", Some("packages/app/src/m.js")),
            (app, "./m.js", Some("packages/app/src/m.js")),
            (app, "./only", Some("packages/app/src/only.d.ts")),
            (app, "typed", Some("packages/typed/index.js")),
            (app, "typed2", Some("packages/typed2/index.js")),
            (app, "bare", Some("packages/bare/index.js")),
            (app, "bare/tools", Some("packages/bare/tools.ts")),
            (app, "bare/dir", Some("packages/bare/dir/index.ts")),
            (app, "srconly", Some("packages/srconly/src/index.ts")),
            (app, "srconly/tools", Some("packages/srconly/src/tools.ts")),
            // two packages named `dup`: the one nearest the importing file
            (app, "dup", Some("packages/dup/index.ts")),
            ("apps/web/src/x.ts", "dup", Some("apps/web/index.ts")),
            // `paths` of a tsconfig.json come before a package of that name
            ("apps/web/src/x.ts", "legacy", Some("apps/web/shim.ts")),
            (app, "react", None),
            (app, "@acme/nothing", None),
            (app, "@acme", None),
        ] {
            assert_eq!(rv.js_module(from, spec).as_deref(), want, "{from}: {spec}");
        }
        drop(rv);
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
        resolver_case(
            "workspace_refs",
            files,
            &[
                (app, "slugify(\"x\")", "slugify", "packages/utils/src/index.ts slugify"),
                (app, "half(2)", "half", "packages/utils/src/math.ts half"),
                (app, "old(slugify", "old", "packages/legacy/src/index.ts old"),
                (app, "React.f()", "f", "external"),
            ],
        );
    }

    #[test]
    fn a_method_call_on_a_variable_follows_the_type_its_declaration_states() {
        resolver_case(
            "vartype_py",
            &[
                ("shapes.py", "class Point:\n    def norm(self):\n        return 1\n\n\nclass Line:\n    def norm(self):\n        return 2\n"),
                (
                    "use.py",
                    "import shapes\nfrom shapes import Point, Line\n\n\ndef built():\n    p = Point()\n    return p.norm()\n\n\ndef annotated(q: Line, r: shapes.Point):\n    return q.norm() + r.norm()\n\n\ndef unknown(s):\n    t = make()\n    return s.norm() + t.norm()\n\n\ndef rebound():\n    v = Point()\n    v = Line()\n    return v.norm()\n",
                ),
            ],
            &[
                ("use.py", "p.norm()", "norm", "shapes.py Point.norm"),
                ("use.py", "q.norm()", "norm", "shapes.py Line.norm"),
                ("use.py", "r.norm()", "norm", "shapes.py Point.norm"),
                ("use.py", "s.norm()", "norm", "receiver"),
                ("use.py", "t.norm()", "norm", "receiver"),
                ("use.py", "v.norm()", "norm", "shapes.py Line.norm"),
            ],
        );
        resolver_case(
            "vartype_ts",
            &[
                ("shapes.ts", "export class Point {\n  norm() { return 1; }\n}\nexport class Line {\n  norm() { return 2; }\n}\n"),
                (
                    "use.ts",
                    "import { Point, Line } from './shapes';\n\nexport function built() {\n  const p = new Point();\n  return p.norm();\n}\n\nexport function annotated(q: Line) {\n  const r: Point = make();\n  return q.norm() + r.norm();\n}\n",
                ),
            ],
            &[("use.ts", "p.norm()", "norm", "shapes.ts Point.norm"), ("use.ts", "q.norm()", "norm", "shapes.ts Line.norm"), ("use.ts", "r.norm()", "norm", "shapes.ts Point.norm")],
        );
        resolver_case(
            "vartype_rs",
            &[
                ("lib.rs", "mod shapes;\nmod user;\n"),
                ("shapes.rs", "pub struct Point;\n\nimpl Point {\n    pub fn new() -> Point {\n        Point\n    }\n    pub fn norm(&self) -> u32 {\n        1\n    }\n}\n\npub struct Line;\n\nimpl Line {\n    pub fn norm(&self) -> u32 {\n        2\n    }\n}\n"),
                (
                    "user.rs",
                    "use crate::shapes::{Line, Point};\n\npub fn built() -> u32 {\n    let p = Point::new();\n    let l = Line {};\n    p.norm() + l.norm()\n}\n\npub fn annotated(q: &Line, r: &mut crate::shapes::Point) -> u32 {\n    q.norm() + r.norm()\n}\n",
                ),
            ],
            &[
                ("user.rs", "p.norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "l.norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "q.norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "r.norm()", "norm", "shapes.rs Point.norm"),
            ],
        );
        resolver_case(
            "vartype_go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("shapes/point.go", "package shapes\n\ntype Point struct{}\n\nfunc (p *Point) Norm() int {\n\treturn 1\n}\n"),
                ("shapes/line.go", "package shapes\n\ntype Line struct{}\n\nfunc (l Line) Norm() int {\n\treturn 2\n}\n"),
                (
                    "main.go",
                    "package main\n\nimport \"example.com/m/shapes\"\n\nfunc built() int {\n\tp := &shapes.Point{}\n\tl := shapes.Line{}\n\treturn p.Norm() + l.Norm()\n}\n\nfunc annotated(q *shapes.Point) int {\n\tvar r shapes.Line\n\treturn q.Norm() + r.Norm()\n}\n",
                ),
            ],
            &[
                ("main.go", "p.Norm()", "Norm", "shapes/point.go Point.Norm"),
                ("main.go", "l.Norm()", "Norm", "shapes/line.go Line.Norm"),
                ("main.go", "q.Norm()", "Norm", "shapes/point.go Point.Norm"),
                ("main.go", "r.Norm()", "Norm", "shapes/line.go Line.Norm"),
            ],
        );
    }

    #[test]
    fn a_variable_has_the_type_of_the_latest_binder_in_scope_at_the_use() {
        let (point, line) = ("shapes.py Point.norm", "shapes.py Line.norm");
        resolver_case(
            "shadow_py",
            &[
                ("shapes.py", "class Point:\n    def norm(self):\n        return 1\n\n\nclass Line:\n    def norm(self):\n        return 2\n"),
                (
                    "use.py",
                    "from shapes import Point, Line\n\n\ndef branch(c):\n    p = Point()\n    if c:\n        p = Line()\n    return p.norm() + 1\n\n\ndef lam():\n    p = Point()\n    f = lambda p: p.norm() + 2\n    return p.norm() + 3\n\n\ndef comp(xs):\n    p = Point()\n    ys = [p for p in xs]\n    return p.norm() + 4\n\n\ndef nested():\n    p = Point()\n\n    def inner():\n        p = Line()\n        return p\n    return p.norm() + 5\n\n\ndef arms(c):\n    if c:\n        p = Point()\n        return p.norm() + 6\n    else:\n        p = Line()\n        return p.norm() + 7\n",
                ),
            ],
            &[
                // a function is one scope: an assignment in an `if` rebinds the name
                ("use.py", "p.norm() + 1", "norm", line),
                ("use.py", "p.norm() + 2", "norm", "receiver"),
                ("use.py", "p.norm() + 3", "norm", point),
                ("use.py", "p.norm() + 4", "norm", point),
                ("use.py", "p.norm() + 5", "norm", point),
                ("use.py", "p.norm() + 6", "norm", point),
                ("use.py", "p.norm() + 7", "norm", line),
            ],
        );
        let (point, line) = ("shapes.js Point.norm", "shapes.js Line.norm");
        resolver_case(
            "shadow_js",
            &[
                ("shapes.js", "export class Point {\n  norm() { return 1; }\n}\nexport class Line {\n  norm() { return 2; }\n}\n"),
                (
                    "use.js",
                    "import { Point, Line } from './shapes';\n\nexport function sibling(c) {\n  const p = new Point();\n  if (c) {\n    const p = new Line();\n    p.norm() + 1;\n  }\n  return p.norm() + 2;\n}\n\nexport function enclosing(c) {\n  let p = new Point();\n  if (c) {\n    let p = new Line();\n    if (c) {\n      return p.norm() + 3;\n    }\n  }\n}\n\nexport function arrow() {\n  const p = new Point();\n  const f = (p) => p.norm() + 4;\n  return p.norm() + 5;\n}\n\nexport function hoisted(c) {\n  var p = new Point();\n  if (c) {\n    var p = new Line();\n  }\n  return p.norm() + 6;\n}\n\nexport function loops(v) {\n  const p = new Point();\n  for (const p of [new Line()]) {}\n  try {} catch (p) {}\n  return p.norm() + 7;\n}\n",
                ),
            ],
            &[
                ("use.js", "p.norm() + 1", "norm", line),
                ("use.js", "p.norm() + 2", "norm", point),
                ("use.js", "p.norm() + 3", "norm", line),
                ("use.js", "p.norm() + 4", "norm", "receiver"),
                ("use.js", "p.norm() + 5", "norm", point),
                // `var` belongs to the whole function
                ("use.js", "p.norm() + 6", "norm", line),
                ("use.js", "p.norm() + 7", "norm", point),
            ],
        );
        resolver_case(
            "shadow_ts",
            &[
                ("shapes.ts", "export class Point {\n  norm() { return 1; }\n}\nexport class Line {\n  norm() { return 2; }\n}\n"),
                (
                    "use.ts",
                    "import { Point, Line } from './shapes';\n\nexport function sibling(c: boolean, f: (p: Line) => number) {\n  const p: Point = make();\n  if (c) {\n    const p: Line = make();\n  }\n  return p.norm() + 1;\n}\n",
                ),
            ],
            &[("use.ts", "p.norm() + 1", "norm", "shapes.ts Point.norm")],
        );
        let (point, line) = ("shapes.rs Point.norm", "shapes.rs Line.norm");
        resolver_case(
            "shadow_rs",
            &[
                ("lib.rs", "mod shapes;\nmod user;\n"),
                ("shapes.rs", "pub struct Point;\n\nimpl Point {\n    pub fn new() -> Point {\n        Point\n    }\n    pub fn norm(&self) -> u32 {\n        1\n    }\n}\n\npub struct Line;\n\nimpl Line {\n    pub fn norm(&self) -> u32 {\n        2\n    }\n}\n"),
                (
                    "user.rs",
                    "use crate::shapes::{Line, Point};\n\npub fn sibling(c: bool) -> u32 {\n    let p = Point::new();\n    if c {\n        let p = Line {};\n        p.norm() + 1;\n    }\n    p.norm() + 2\n}\n\npub fn enclosing(c: bool) -> u32 {\n    let p = Point::new();\n    if c {\n        let p = Line {};\n        if c {\n            return p.norm() + 3;\n        }\n    }\n    0\n}\n\npub fn closure() -> u32 {\n    let p = Point::new();\n    let f = |p: Line| p.norm() + 4;\n    p.norm() + 5\n}\n\npub fn branches(o: Option<Line>) -> u32 {\n    let p = Point::new();\n    if let Some(p) = o {\n        return p.norm() + 6;\n    }\n    let n = match o {\n        Some(p) => 1,\n        None => 2,\n    };\n    p.norm() + 7\n}\n\npub fn own_value() -> u32 {\n    let p = Point::new();\n    let p = p.norm() + 8;\n    p\n}\n",
                ),
            ],
            &[
                ("user.rs", "p.norm() + 1", "norm", line),
                ("user.rs", "p.norm() + 2", "norm", point),
                ("user.rs", "p.norm() + 3", "norm", line),
                ("user.rs", "p.norm() + 4", "norm", line),
                ("user.rs", "p.norm() + 5", "norm", point),
                ("user.rs", "p.norm() + 6", "norm", "receiver"),
                ("user.rs", "p.norm() + 7", "norm", point),
                // a `let` does not see itself in its own value
                ("user.rs", "p.norm() + 8", "norm", point),
            ],
        );
        let (point, line) = ("shapes/point.go Point.Norm", "shapes/line.go Line.Norm");
        resolver_case(
            "shadow_go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("shapes/point.go", "package shapes\n\ntype Point struct{}\n\nfunc (p *Point) Norm() int {\n\treturn 1\n}\n"),
                ("shapes/line.go", "package shapes\n\ntype Line struct{}\n\nfunc (l Line) Norm() int {\n\treturn 2\n}\n"),
                (
                    "main.go",
                    "package main\n\nimport \"example.com/m/shapes\"\n\nfunc sibling(c bool) int {\n\tp := &shapes.Point{}\n\tif c {\n\t\tp := shapes.Line{}\n\t\t_ = p.Norm() + 1\n\t}\n\treturn p.Norm() + 2\n}\n\nfunc enclosing(c bool) int {\n\tp := &shapes.Point{}\n\tif c {\n\t\tp := shapes.Line{}\n\t\tif c {\n\t\t\treturn p.Norm() + 3\n\t\t}\n\t}\n\treturn 0\n}\n\nfunc closure() int {\n\tp := &shapes.Point{}\n\tf := func(p shapes.Line) int { return p.Norm() + 4 }\n\t_ = f\n\treturn p.Norm() + 5\n}\n\nfunc switched(v interface{}) int {\n\tp := &shapes.Point{}\n\tswitch p := v.(type) {\n\tcase shapes.Line:\n\t\t_ = p\n\t}\n\tif p := 1; p > 0 {\n\t}\n\treturn p.Norm() + 6\n}\n",
                ),
            ],
            &[
                ("main.go", "p.Norm() + 1", "Norm", line),
                ("main.go", "p.Norm() + 2", "Norm", point),
                ("main.go", "p.Norm() + 3", "Norm", line),
                ("main.go", "p.Norm() + 4", "Norm", line),
                ("main.go", "p.Norm() + 5", "Norm", point),
                ("main.go", "p.Norm() + 6", "Norm", point),
            ],
        );
    }

    #[test]
    fn a_closure_or_nested_function_sees_the_variables_of_the_functions_around_it() {
        let (point, line) = ("shapes.py Point.norm", "shapes.py Line.norm");
        resolver_case(
            "closure_py",
            &[
                ("shapes.py", "class Point:\n    def norm(self):\n        return 1\n\n\nclass Line:\n    def norm(self):\n        return 2\n"),
                (
                    "use.py",
                    "from shapes import Point, Line\n\n\ndef lam(xs):\n    p = Point()\n    return list(map(lambda x: p.norm() + 1, xs))\n\n\ndef nest():\n    p = Point()\n\n    def inner():\n        return p.norm() + 2\n    return inner\n\n\ndef shadow(xs):\n    p = Point()\n    return list(map(lambda p: p.norm() + 3, xs))\n\n\ndef later():\n    def inner():\n        return p.norm() + 4\n    p = Point()\n    return inner()\n\n\ndef deep():\n    p = Point()\n\n    def a():\n        def b():\n            return p.norm() + 5\n        return b\n    return a\n\n\ndef own():\n    p = Point()\n\n    def inner():\n        p = Line()\n        return p.norm() + 6\n    return inner\n",
                ),
            ],
            &[
                ("use.py", "p.norm() + 1", "norm", point),
                ("use.py", "p.norm() + 2", "norm", point),
                // the closure's own parameter wins, and it has no type
                ("use.py", "p.norm() + 3", "norm", "receiver"),
                // a variable bound after the closure is not taken: the call may come before it
                ("use.py", "p.norm() + 4", "norm", "receiver"),
                ("use.py", "p.norm() + 5", "norm", point),
                ("use.py", "p.norm() + 6", "norm", line),
            ],
        );
        let (point, line) = ("shapes.js Point.norm", "shapes.js Line.norm");
        resolver_case(
            "closure_js",
            &[
                ("shapes.js", "export class Point {\n  norm() { return 1; }\n}\nexport class Line {\n  norm() { return 2; }\n}\n"),
                (
                    "use.js",
                    "import { Point, Line } from './shapes';\n\nexport function arrow(xs) {\n  const p = new Point();\n  return xs.map(x => p.norm() + 1);\n}\n\nexport function expr(xs) {\n  const p = new Point();\n  return xs.map(function (x) { return p.norm() + 2; });\n}\n\nexport function shadow(xs) {\n  const p = new Point();\n  return xs.map((p) => p.norm() + 3);\n}\n\nexport function later() {\n  const f = () => p.norm() + 4;\n  const p = new Point();\n  return f();\n}\n\nexport function deep() {\n  const p = new Point();\n  return () => () => p.norm() + 5;\n}\n\nexport function own(c) {\n  const p = new Point();\n  if (c) {\n    const p = new Line();\n  }\n  return () => { const q = 1; return p.norm() + 6; };\n}\n\nexport function inner() {\n  const p = new Point();\n  return () => { const p = new Line(); return p.norm() + 7; };\n}\n",
                ),
            ],
            &[
                ("use.js", "p.norm() + 1", "norm", point),
                ("use.js", "p.norm() + 2", "norm", point),
                ("use.js", "p.norm() + 3", "norm", "receiver"),
                ("use.js", "p.norm() + 4", "norm", "receiver"),
                ("use.js", "p.norm() + 5", "norm", point),
                // a binder in a block that has ended is not seen from a closure after it
                ("use.js", "p.norm() + 6", "norm", point),
                ("use.js", "p.norm() + 7", "norm", line),
            ],
        );
        let (point, line) = ("shapes.ts Point.norm", "shapes.ts Line.norm");
        resolver_case(
            "closure_ts",
            &[
                ("shapes.ts", "export class Point {\n  norm() { return 1; }\n}\nexport class Line {\n  norm() { return 2; }\n}\n"),
                (
                    "use.ts",
                    "import { Point, Line } from './shapes';\n\nexport function f(xs: number[]) {\n  const p = new Point();\n  return xs.map(x => p.norm() + 1);\n}\n\nexport function g(p: Point) {\n  return [1].map(() => p.norm() + 2);\n}\n\nexport function h(xs: Line[]) {\n  const p = new Point();\n  return xs.map((p: Line) => p.norm() + 3);\n}\n",
                ),
            ],
            &[("use.ts", "p.norm() + 1", "norm", point), ("use.ts", "p.norm() + 2", "norm", point), ("use.ts", "p.norm() + 3", "norm", line)],
        );
        let point = "shapes.rs Point.norm";
        resolver_case(
            "closure_rs",
            &[
                ("lib.rs", "mod shapes;\nmod user;\n"),
                ("shapes.rs", "pub struct Point;\n\nimpl Point {\n    pub fn new() -> Point {\n        Point\n    }\n    pub fn norm(&self) -> u32 {\n        1\n    }\n}\n\npub struct Line;\n\nimpl Line {\n    pub fn norm(&self) -> u32 {\n        2\n    }\n}\n"),
                (
                    "user.rs",
                    "use crate::shapes::{Line, Point};\n\npub fn closure(xs: &[u32]) -> u32 {\n    let p = Point::new();\n    xs.iter().map(|x| p.norm() + 1).sum()\n}\n\npub fn shadow(xs: &[Line]) -> u32 {\n    let p = Point::new();\n    xs.iter().map(|p| p.norm() + 2).sum()\n}\n\npub fn deep() -> u32 {\n    let p = Point::new();\n    let f = || {\n        let g = || p.norm() + 3;\n        g()\n    };\n    f()\n}\n\npub fn item() -> u32 {\n    let p = Point::new();\n    fn inner(p: &Line) -> u32 {\n        p.norm() + 4\n    }\n    fn other() -> u32 {\n        let f = || p.norm() + 5;\n        f()\n    }\n    0\n}\n",
                ),
            ],
            &[
                ("user.rs", "p.norm() + 1", "norm", point),
                ("user.rs", "p.norm() + 2", "norm", "receiver"),
                ("user.rs", "p.norm() + 3", "norm", point),
                // a `fn` item does not see the locals of the function around it
                ("user.rs", "p.norm() + 4", "norm", "shapes.rs Line.norm"),
                ("user.rs", "p.norm() + 5", "norm", "receiver"),
            ],
        );
        let point = "shapes/point.go Point.Norm";
        resolver_case(
            "closure_go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("shapes/point.go", "package shapes\n\ntype Point struct{}\n\nfunc (p *Point) Norm() int {\n\treturn 1\n}\n"),
                (
                    "main.go",
                    "package main\n\nimport \"example.com/m/shapes\"\n\nfunc closure() int {\n\tp := &shapes.Point{}\n\tf := func() int { return p.Norm() + 1 }\n\treturn f()\n}\n\nfunc deep() int {\n\tp := &shapes.Point{}\n\tf := func() func() int {\n\t\treturn func() int { return p.Norm() + 2 }\n\t}\n\treturn f()()\n}\n",
                ),
            ],
            &[("main.go", "p.Norm() + 1", "Norm", point), ("main.go", "p.Norm() + 2", "Norm", point)],
        );
    }

    #[test]
    fn an_inner_binding_hides_a_typed_variable_of_the_functions_around_it() {
        let js_shapes = "export class Point {\n  norm() { return 1; }\n}\nexport class Line {\n  norm() { return 2; }\n}\n";
        let js_use = "import { Point } from './shapes';\n\nexport function f(xs) {\n  const p = new Point();\n  xs.map(p => p.norm() + 1);\n  xs.map(({ p }) => p.norm() + 2);\n  xs.map((p = 0) => p.norm() + 3);\n  xs.map((...p) => p.norm() + 4);\n  const k = () => {\n    try { g(); } catch (p) { p.norm() + 5; }\n    for (const p of xs) { p.norm() + 6; }\n    function p() { return 0; }\n    return p.norm() + 11;\n  };\n  const h = function p() { return p.norm() + 7; };\n  xs.map(([a, p]) => p.norm() + 8);\n  xs.map(({ q: p }) => p.norm() + 9);\n  xs.map(function (p) { return p.norm() + 10; });\n  return [h, k];\n}\n";
        // the first line holding `p.norm() + 1` is the one of check 1, not of 10 or 11; the checks
        // in `int` are of a binding with a type from outside
        let calls = |file: &'static str, call: &str, name: &'static str, n: usize, int: &[usize]| -> Vec<(&'static str, String, &'static str, &'static str)> {
            (1..=n).map(|i| (file, format!("{call} + {i}"), name, if int.contains(&i) { "external" } else { "receiver" })).collect()
        };
        let run = |tag: &str, files: &[(&str, &str)], checks: Vec<(&'static str, String, &'static str, &'static str)>| {
            let checks: Vec<_> = checks.iter().map(|(f, n, m, w)| (*f, n.as_str(), *m, *w)).collect();
            resolver_case(tag, files, &checks);
        };
        run("inner_js", &[("shapes.js", js_shapes), ("use.js", js_use)], calls("use.js", "p.norm()", "norm", 11, &[]));
        run("inner_ts", &[("shapes.ts", js_shapes), ("use.ts", &js_use.replace("(xs)", "(xs: any[])"))], calls("use.ts", "p.norm()", "norm", 11, &[]));
        run(
            "inner_py",
            &[("shapes.py", "class Point:\n    def norm(self):\n        return 1\n"), ("use.py", "from shapes import Point\n\n\ndef f1(xs):\n    p = Point()\n    return list(map(lambda p=0: p.norm() + 1, xs))\n\n\ndef f2(xs):\n    p = Point()\n\n    def g():\n        for p in xs:\n            return p.norm() + 2\n    return g\n\n\ndef f3(o):\n    p = Point()\n\n    def g():\n        with open(o) as p:\n            return p.norm() + 3\n    return g\n\n\ndef f4():\n    p = Point()\n\n    def g():\n        try:\n            h()\n        except ValueError as p:\n            return p.norm() + 4\n    return g\n\n\ndef f5(xs):\n    p = Point()\n    return lambda: [p.norm() + 5 for p in xs]\n\n\ndef f6(xs):\n    p = Point()\n\n    def g():\n        if (p := xs[0]):\n            return p.norm() + 6\n    return g\n\n\ndef f7(o):\n    p = Point()\n\n    def g():\n        match o:\n            case p:\n                return p.norm() + 7\n    return g\n\n\ndef f8(xs):\n    p = Point()\n    return list(map(lambda *p: p.norm() + 8, xs))\n\n\ndef f9(xs):\n    p = Point()\n\n    def g():\n        for a, p in xs:\n            return p.norm() + 9\n    return g\n\n\ndef f10(o):\n    p = Point()\n\n    def g():\n        match o:\n            case [a, p]:\n                return p.norm() + 10\n    return g\n\n\ndef f11(o):\n    p = Point()\n\n    def g(*, p=None):\n        return p.norm() + 11\n    return g\n")],
            calls("use.py", "p.norm()", "norm", 11, &[]),
        );
        run(
            "inner_rs",
            &[
                ("lib.rs", "mod shapes;\nmod user;\n"),
                ("shapes.rs", "pub struct Point;\n\nimpl Point {\n    pub fn new() -> Point {\n        Point\n    }\n    pub fn norm(&self) -> u32 {\n        1\n    }\n}\n"),
                ("user.rs", "use crate::shapes::Point;\n\npub fn f(xs: &[(u32, u32)], o: Option<u32>) -> u32 {\n    let p = Point::new();\n    let g = || {\n        xs.iter().map(|(a, p)| p.norm() + 1).count();\n        if let Some(p) = o { p.norm() + 2; }\n        let mut it = xs.iter();\n        while let Some(p) = it.next() { p.norm() + 3; }\n        for p in xs { p.norm() + 4; }\n        match o { Some(p) => p.norm() + 5, None => 0 };\n        xs.iter().map(|&(a, p)| p.norm() + 6).count();\n        if let Some(ref p) = o { p.norm() + 7; }\n        match o { Some(p @ 1..=3) => p.norm() + 8, _ => 0 };\n        for (a, p) in xs { p.norm() + 9; }\n        0\n    };\n    g()\n}\n"),
            ],
            calls("user.rs", "p.norm()", "norm", 9, &[]),
        );
        run(
            "inner_go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("shapes/point.go", "package shapes\n\ntype Point struct{}\n\nfunc (p *Point) Norm() int {\n\treturn 1\n}\n"),
                ("main.go", "package main\n\nimport \"example.com/m/shapes\"\n\nfunc f(xs []int, x interface{}, ch chan int) int {\n\tp := &shapes.Point{}\n\tg := func() int {\n\t\th := func(p int) int { return p.Norm() + 1 }\n\t\tfor _, p := range xs {\n\t\t\tp.Norm() + 2\n\t\t}\n\t\tif p := x; p != nil {\n\t\t\tp.Norm() + 3\n\t\t}\n\t\tswitch p := x.(type) {\n\t\tdefault:\n\t\t\tp.Norm() + 4\n\t\t}\n\t\tk := func() (p int) { return p.Norm() + 5 }\n\t\tfor p := 0; p < 3; p++ {\n\t\t\tp.Norm() + 6\n\t\t}\n\t\tselect {\n\t\tcase p := <-ch:\n\t\t\tp.Norm() + 7\n\t\t}\n\t\tv := func(p ...int) int { return p.Norm() + 8 }\n\t\treturn h(0) + k() + v()\n\t}\n\treturn g()\n}\n"),
            ],
            calls("main.go", "p.Norm()", "Norm", 8, &[1, 5, 8]),
        );
    }

    #[test]
    fn a_python_base_from_outside_keeps_its_place_in_the_method_order() {
        let (helper, mixin) = ("mro.py Helper.setUp", "mro.py Mixin.extra");
        resolver_case(
            "mro_outside",
            &[(
                "mro.py",
                "import unittest\nfrom ext import Other\n\n\nclass Helper(object):\n    def setUp(self):\n        return 1\n\n\nclass Mixin(object):\n    def extra(self):\n        return 2\n\n\nclass Plain:\n    pass\n\n\nclass Deep(Other):\n    pass\n\n\nclass T(unittest.TestCase, Helper):\n    def test(self):\n        self.setUp() + 1\n        super().setUp() + 2\n\n\nclass U(Helper, unittest.TestCase):\n    def test(self):\n        self.setUp() + 3\n        super().setUp() + 4\n\n\nclass V(Other, unittest.TestCase, Helper):\n    def test(self):\n        self.setUp() + 5\n\n\nclass W(Helper, Mixin):\n    def test(self):\n        self.extra() + 6\n\n\nclass X(Deep, Mixin):\n    def test(self):\n        self.extra() + 7\n\n\nclass Y(Plain, Other, Helper):\n    def test(self):\n        super().setUp() + 8\n        super(Plain, self).setUp() + 9\n",
            )],
            &[
                // `TestCase.setUp` comes first
                ("mro.py", "self.setUp() + 1", "setUp", "not found"),
                ("mro.py", "super().setUp() + 2", "setUp", "external"),
                ("mro.py", "self.setUp() + 3", "setUp", helper),
                ("mro.py", "super().setUp() + 4", "setUp", helper),
                ("mro.py", "self.setUp() + 5", "setUp", "not found"),
                // `object` ends every order, so it hides no base
                ("mro.py", "self.extra() + 6", "extra", mixin),
                ("mro.py", "self.extra() + 7", "extra", "not found"),
                ("mro.py", "super().setUp() + 8", "setUp", "external"),
                ("mro.py", "super(Plain, self).setUp() + 9", "setUp", "external"),
            ],
        );
    }

    #[test]
    fn a_method_does_not_see_a_variable_of_its_class_body() {
        let (point, line) = ("shapes.py Point.norm", "shapes.py Line.norm");
        resolver_case(
            "class_body_py",
            &[
                ("shapes.py", "class Point:\n    def norm(self):\n        return 1\n\n\nclass Line:\n    def norm(self):\n        return 2\n"),
                (
                    "use.py",
                    "from shapes import Point, Line\n\np = Point()\n\n\nclass D:\n    p = Line()\n\n    def m(self):\n        return p.norm() + 1\n\n\ndef make():\n    p = Point()\n\n    class C:\n        p = Line()\n        q = p.norm() + 2\n\n        def m(self):\n            return p.norm() + 3\n\n        f = lambda self: p.norm() + 4\n\n        class E:\n            def m(self):\n                return p.norm() + 5\n    return C\n",
                ),
            ],
            &[
                ("use.py", "p.norm() + 1", "norm", point),
                // code directly in the class body sees it
                ("use.py", "p.norm() + 2", "norm", line),
                ("use.py", "p.norm() + 3", "norm", point),
                ("use.py", "p.norm() + 4", "norm", point),
                ("use.py", "p.norm() + 5", "norm", point),
            ],
        );
        let point = "shapes.js Point.norm";
        resolver_case(
            "class_body_js",
            &[
                ("shapes.js", "export class Point {\n  norm() { return 1; }\n}\nexport class Line {\n  norm() { return 2; }\n}\n"),
                (
                    "use.js",
                    "import { Point, Line } from './shapes';\n\nexport function make() {\n  const p = new Point();\n  return class C {\n    p = new Line();\n    static p = new Line();\n    m() { return p.norm() + 1; }\n  };\n}\n",
                ),
            ],
            &[("use.js", "p.norm() + 1", "norm", point)],
        );
        resolver_case(
            "class_body_rs",
            &[
                ("lib.rs", "mod shapes;\nmod user;\n"),
                ("shapes.rs", "pub struct Point;\n\nimpl Point {\n    pub fn new() -> Point {\n        Point\n    }\n    pub fn norm(&self) -> u32 {\n        1\n    }\n}\n\npub struct Line;\n\nimpl Line {\n    pub fn norm(&self) -> u32 {\n        2\n    }\n}\n"),
                (
                    "user.rs",
                    "use crate::shapes::{Line, Point};\n\nstruct S;\n\nimpl S {\n    const p: Line = Line;\n}\n\npub fn f() -> u32 {\n    let p = Point::new();\n    struct T;\n    impl T {\n        const p: Line = Line;\n    }\n    let g = || p.norm() + 1;\n    g()\n}\n\npub fn h() -> u32 {\n    p.norm() + 2\n}\n",
                ),
            ],
            &[("user.rs", "p.norm() + 1", "norm", "shapes.rs Point.norm"), ("user.rs", "p.norm() + 2", "norm", "receiver")],
        );
    }

    #[test]
    fn a_receiver_type_comes_from_return_types_fields_and_module_variables() {
        resolver_case(
            "infer_rs",
            &[
                ("lib.rs", "mod shapes;\nmod user;\n"),
                (
                    "shapes.rs",
                    "pub struct Point;\n\nimpl Point {\n    pub fn norm(&self) -> u32 {\n        1\n    }\n    pub fn twin(&self) -> Point {\n        Point\n    }\n}\n\npub struct Line;\n\nimpl Line {\n    pub fn norm(&self) -> u32 {\n        2\n    }\n}\n\npub fn make() -> Point {\n    Point\n}\n\npub fn boxed() -> Box<Line> {\n    Box::new(Line)\n}\n\npub fn tried() -> Result<Line, String> {\n    Ok(Line)\n}\n\npub struct Holder {\n    pub line: Line,\n}\n\nimpl Holder {\n    pub fn get(&self) -> u32 {\n        self.line.norm()\n    }\n    pub fn line(&self) -> &Line {\n        &self.line\n    }\n}\n\npub static ORIGIN: Point = Point;\n\npub fn origin() -> u32 {\n    ORIGIN.norm()\n}\n",
                ),
                (
                    "user.rs",
                    "use crate::shapes::{boxed, make, tried, Holder};\n\npub fn f(h: &Holder) -> u32 {\n    let v = make();\n    v.norm() + make().norm() + make().twin().norm() + h.line().norm() + boxed().norm()\n}\n\npub fn g(r: Result<u32, String>) -> Result<u32, String> {\n    let t = tried()?;\n    Ok(r.ok().unwrap_or(t.norm()))\n}\n\npub fn h<T: Clone>(x: T, s: &str) -> usize {\n    x.clone();\n    s.len()\n}\n",
                ),
            ],
            &[
                ("shapes.rs", "self.line.norm()", "norm", "shapes.rs Line.norm"),
                ("shapes.rs", "ORIGIN.norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "v.norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "+ make().norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "twin().norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "h.line().norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "boxed().norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "t.norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "r.ok()", "ok", "external"),
                ("user.rs", "s.len()", "len", "external"),
                ("user.rs", "x.clone()", "clone", "receiver"),
            ],
        );
        resolver_case(
            "infer_ts",
            &[
                (
                    "shapes.ts",
                    "export class Point {\n  norm() { return 1; }\n  twin(): Point { return new Point(); }\n}\nexport class Line {\n  norm() { return 2; }\n}\nexport function make(): Point {\n  return new Point();\n}\nexport class Holder {\n  line: Line = new Line();\n  get() { return this.line.norm(); }\n}\nexport const ORIGIN: Point = new Point();\nexport const BUILT = new Line();\nexport function useAll(xs: string[], n: Array<number>) {\n  const v = make();\n  return v.norm() + make().twin().norm() + ORIGIN.norm() + BUILT.norm() + xs.norm() + n.norm();\n}\n",
                ),
            ],
            &[
                ("shapes.ts", "this.line.norm()", "norm", "shapes.ts Line.norm"),
                ("shapes.ts", "v.norm()", "norm", "shapes.ts Point.norm"),
                ("shapes.ts", "twin().norm()", "norm", "shapes.ts Point.norm"),
                ("shapes.ts", "ORIGIN.norm()", "norm", "shapes.ts Point.norm"),
                ("shapes.ts", "BUILT.norm()", "norm", "shapes.ts Line.norm"),
                ("shapes.ts", "xs.norm()", "norm", "external"),
                ("shapes.ts", "n.norm()", "norm", "external"),
            ],
        );
        resolver_case(
            "infer_py",
            &[(
                "shapes.py",
                "class Point:\n    def norm(self):\n        return 1\n\n\nclass Line:\n    def norm(self):\n        return 2\n\n\ndef make() -> Point:\n    return Point()\n\n\nclass Holder:\n    line: Line\n\n    def __init__(self):\n        self.pt = Point()\n\n    def get(self):\n        return self.line.norm() + self.pt.norm()\n\n\nORIGIN = Point()\nTYPED: Line = build()\n\n\ndef use(items: list):\n    v = make()\n    return v.norm() + make().norm() + ORIGIN.norm() + TYPED.norm() + items.norm()\n",
            )],
            &[
                ("shapes.py", "self.line.norm()", "norm", "shapes.py Line.norm"),
                ("shapes.py", "self.pt.norm()", "norm", "shapes.py Point.norm"),
                ("shapes.py", "v.norm()", "norm", "shapes.py Point.norm"),
                ("shapes.py", "make().norm()", "norm", "shapes.py Point.norm"),
                ("shapes.py", "ORIGIN.norm()", "norm", "shapes.py Point.norm"),
                ("shapes.py", "TYPED.norm()", "norm", "shapes.py Line.norm"),
                ("shapes.py", "items.norm()", "norm", "external"),
            ],
        );
        resolver_case(
            "infer_go",
            &[
                ("go.mod", "module example.com/m\n"),
                (
                    "shapes/shapes.go",
                    "package shapes\n\ntype Point struct{}\n\nfunc (p *Point) Norm() int {\n\treturn 1\n}\n\ntype Line struct{}\n\nfunc (l Line) Norm() int {\n\treturn 2\n}\n\nfunc Make() *Point {\n\treturn &Point{}\n}\n\ntype Holder struct {\n\tline Line\n}\n\nfunc (h *Holder) Get() int {\n\treturn h.line.Norm()\n}\n\nvar Origin = Point{}\n\nfunc Use() int {\n\tv := Make()\n\treturn v.Norm() + Make().Norm() + Origin.Norm()\n}\n",
                ),
            ],
            &[
                ("shapes/shapes.go", "h.line.Norm()", "Norm", "shapes/shapes.go Line.Norm"),
                ("shapes/shapes.go", "v.Norm()", "Norm", "shapes/shapes.go Point.Norm"),
                ("shapes/shapes.go", "Make().Norm()", "Norm", "shapes/shapes.go Point.Norm"),
                ("shapes/shapes.go", "Origin.Norm()", "Norm", "shapes/shapes.go Point.Norm"),
            ],
        );
    }

    #[test]
    fn a_call_is_typed_by_the_return_type_its_callee_declares() {
        let report = "pub struct Report;\n\nimpl Report {\n    pub fn load() -> Report {\n        Report\n    }\n    pub fn new() -> Self {\n        Report\n    }\n    pub fn ok(&self) -> bool {\n        true\n    }\n}\n\npub struct Builder;\n\nimpl Builder {\n    pub fn summary(&self) -> Report {\n        Report\n    }\n}\n\npub fn make() -> Builder {\n    Builder\n}\n\npub fn mystery(y: i32) -> Option<i32> {\n    Some(y)\n}\n\npub fn pick<T>(x: T) -> T {\n    x\n}\n\npub fn find() -> Option<Report> {\n    None\n}\n";
        let user = "use crate::report::{find, make, mystery, pick, Report};\nuse cachelib::Cache;\n\npub fn a(y: i32) -> bool {\n    mystery(y).ok().is_some()\n}\n\npub fn b() -> bool {\n    Report::load().ok() && Report::new().ok() && make().summary().ok()\n}\n\npub fn c(file: &std::fs::File, v: Vec<Report>) -> bool {\n    file.metadata().ok().is_some() && v.first().unwrap().ok() && pick(Report).ok() && find().unwrap().ok()\n}\n\npub fn e(k: u8) -> bool {\n    let mut m = std::collections::HashMap::new();\n    m.insert(k, Report::new());\n    let c = Cache::new();\n    m.get(&k).unwrap().ok() && c.lookup(k).unwrap().ok() && m.remove(&k).unwrap().ok()\n}\n\npub fn d(v: &[Report], w: Vec<Report>, s: &str) -> bool {\n    std::fs::metadata(s).ok().is_some() && w.iter().map(|r| r).next().unwrap().ok() && w.iter().rev().next().unwrap().ok() && v.first().unwrap().ok() && s.trim().ok() && s.parse().unwrap().ok()\n}\n";
        let rs = [("lib.rs", "mod report;\nmod user;\n"), ("report.rs", report), ("user.rs", user)];
        let ok = "report.rs Report.ok";
        resolver_case(
            "ret_rs",
            &rs,
            &[
                ("user.rs", "mystery(y).ok()", "ok", "external"),
                ("user.rs", "Report::load().ok()", "ok", ok),
                ("user.rs", "Report::new().ok()", "ok", ok),
                ("user.rs", "make().summary().ok()", "ok", ok),
                // a method of an outside type that holds no repository type returns none
                ("user.rs", "file.metadata().ok()", "ok", "external"),
                ("user.rs", "std::fs::metadata(s).ok()", "ok", "external"),
                ("user.rs", "s.trim().ok()", "ok", "external"),
                // the std methods say where a repository type held by a std type goes
                ("user.rs", "&& v.first().unwrap().ok() && pick", "ok", ok),
                ("user.rs", "find().unwrap().ok()", "ok", ok),
                ("user.rs", "rev().next().unwrap().ok()", "ok", ok),
                // a closure or the caller picks the type: undecided
                ("user.rs", "map(|r| r).next().unwrap().ok()", "ok", "receiver"),
                ("user.rs", "s.parse().unwrap().ok()", "ok", "receiver"),
                // the type arguments of a value built by `new` are not known
                ("user.rs", "m.get(&k).unwrap().ok()", "ok", "receiver"),
                ("user.rs", "(k).unwrap().ok() && m.remove", "ok", "receiver"),
                ("user.rs", "m.remove(&k).unwrap().ok()", "ok", "receiver"),
                ("user.rs", "pick(Report).ok()", "ok", "receiver"),
                // a slice's elements are not known
                ("user.rs", "&& v.first().unwrap().ok() && s", "ok", "receiver"),
            ],
        );
        // a trait of the repository may give an outside type the method
        let okish = "pub trait Okish {\n    fn ok(&self) -> bool;\n}\n\nimpl<T: Copy> Okish for T {\n    fn ok(&self) -> bool {\n        true\n    }\n}\n";
        resolver_case(
            "ret_rs_trait",
            &[rs[0], rs[1], rs[2], ("okish.rs", okish)],
            &[("user.rs", "mystery(y).ok()", "ok", "receiver"), ("user.rs", "Report::load().ok()", "ok", ok)],
        );
        resolver_case(
            "ret_ts",
            &[
                ("report.ts", "export class Report {\n  static load(): Report { return new Report(); }\n  ok(): boolean { return true; }\n}\nexport class Builder {\n  summary(): Report { return new Report(); }\n}\nexport function make(): Builder { return new Builder(); }\nexport function mystery(y: number): number { return y; }\nexport function pick<T>(x: T): T { return x; }\n"),
                ("user.ts", "import { Report, make, mystery, pick } from './report';\n\nexport function a(y: number) {\n  return mystery(y).ok() || Report.load().ok() || make().summary().ok() || new Report().ok() || pick(new Report()).ok();\n}\n"),
                ("aug.ts", "export interface Flagged {\n  flag(): boolean;\n}\nexport function b() {\n  return \"s\".ok() || [1].ok() || \"t\".flag();\n}\n"),
            ],
            &[
                ("user.ts", "mystery(y).ok()", "ok", "external"),
                ("user.ts", "Report.load().ok()", "ok", "report.ts Report.ok"),
                ("user.ts", "make().summary().ok()", "ok", "report.ts Report.ok"),
                ("user.ts", "new Report().ok()", "ok", "report.ts Report.ok"),
                ("user.ts", "pick(new Report()).ok()", "ok", "receiver"),
                ("aug.ts", "\"s\".ok()", "ok", "external"),
                ("aug.ts", "[1].ok()", "ok", "external"),
                ("aug.ts", "\"t\".flag()", "flag", "receiver"),
            ],
        );
        resolver_case(
            "ret_js",
            &[
                ("r.js", "export class Report {\n  ok() { return true; }\n}\n"),
                ("use.js", "import { Report } from './r.js';\n\nfunction mystery() { return 1; }\n\nexport function a() {\n  return new Report().ok() + mystery().ok() + `t`.ok();\n}\n"),
            ],
            &[("use.js", "new Report().ok()", "ok", "r.js Report.ok"), ("use.js", "mystery().ok()", "ok", "receiver"), ("use.js", "`t`.ok()", "ok", "external")],
        );
        resolver_case(
            "ret_go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("report/report.go", "package report\n\ntype Report struct{}\n\nfunc (r *Report) Ok() bool { return true }\n\nfunc NewReport() *Report { return &Report{} }\n\ntype Builder struct{}\n\nfunc (b Builder) Summary() Report { return Report{} }\n\nfunc Build() Builder { return Builder{} }\n\nfunc Mystery(y int) int { return y }\n"),
                ("main.go", "package main\n\nimport \"example.com/m/report\"\n\nfunc main() {\n\t_ = report.NewReport().Ok()\n\t_ = report.Build().Summary().Ok()\n\t_ = report.Mystery(1).Ok()\n}\n"),
            ],
            &[
                ("main.go", "NewReport().Ok()", "Ok", "report/report.go Report.Ok"),
                ("main.go", "Summary().Ok()", "Ok", "report/report.go Report.Ok"),
                ("main.go", "Mystery(1).Ok()", "Ok", "external"),
            ],
        );
        resolver_case(
            "ret_py",
            &[
                ("report.py", "class Report:\n    def ok(self):\n        return True\n\n\nclass Builder:\n    def summary(self) -> Report:\n        return Report()\n\n\ndef make() -> Builder:\n    return Builder()\n\n\ndef mystery(y) -> int:\n    return y\n\n\ndef unknown(y):\n    return y\n"),
                ("use.py", "from report import Report, make, mystery, unknown\n\n\ndef a(y):\n    return Report().ok() or make().summary().ok() or mystery(y).ok() or unknown(y).ok()\n"),
            ],
            &[
                ("use.py", "Report().ok()", "ok", "report.py Report.ok"),
                ("use.py", "summary().ok()", "ok", "report.py Report.ok"),
                ("use.py", "mystery(y).ok()", "ok", "external"),
                ("use.py", "unknown(y).ok()", "ok", "receiver"),
            ],
        );
    }

    #[test]
    fn this_in_a_method_of_an_object_literal_is_the_literal() {
        let js = "export const store = {\n  increment() {\n    return this.total();\n  },\n  total() {\n    return 1;\n  },\n  later: function () {\n    return this.total() + 1;\n  },\n  watch() {\n    return [1].map(() => this.total() + 2);\n  },\n  arrow: () => this.total() + 3,\n  sub: {\n    m() {\n      return this.n();\n    },\n    n() {},\n  },\n};\n\nexport default {\n  methods: {\n    save() {},\n    run() {\n      this.save();\n    },\n  },\n};\n";
        let ts = "export class K {\n  run() {\n    const o = { a() { return this.b(); }, b() {} };\n    return this.stop();\n  }\n  stop() {}\n}\n";
        resolver_case(
            "literal_this",
            &[("store.js", js), ("k.ts", ts)],
            &[
                ("store.js", "return this.total();", "total", "store.js store.total"),
                ("store.js", "this.total() + 1", "total", "store.js store.total"),
                ("store.js", "this.total() + 2", "total", "store.js store.total"),
                ("store.js", "this.total() + 3", "total", "receiver"),
                ("store.js", "this.n()", "n", "store.js store.sub.n"),
                ("store.js", "this.save()", "save", "store.js methods.save"),
                ("k.ts", "this.b()", "b", "k.ts K.run.o.b"),
                ("k.ts", "this.stop()", "stop", "k.ts K.stop"),
            ],
        );
    }

    #[test]
    fn names_in_a_rust_macro_follow_the_tokens_around_them() {
        let rs = "pub fn text() -> u8 {\n    1\n}\n\npub struct Hit {\n    pub text: u8,\n}\n\npub fn run(h: &Hit) -> Vec<Hit> {\n    let s = format!(\"{}\", h.text);\n    assert_eq!(h.text(), 1);\n    println!(\"{} {}\", text(), crate::text());\n    vec![Hit { text: 1 }]\n}\n";
        resolver_case(
            "macro_tokens",
            &[("src/lib.rs", rs), ("Cargo.toml", "[package]\nname = \"demo\"\n")],
            &[
                ("src/lib.rs", "h.text);", "text", "field or key"),
                ("src/lib.rs", "h.text()", "text", "receiver"),
                ("src/lib.rs", "{ text: 1 }", "text", "field or key"),
                ("src/lib.rs", "\"{} {}\", text()", "text", "src/lib.rs text"),
                ("src/lib.rs", "crate::text()", "text", "src/lib.rs text"),
            ],
        );
    }

    #[test]
    fn a_plain_name_in_a_macro_that_may_take_it_as_anything_is_undecided() {
        let rs = "pub fn text() -> u8 {\n    1\n}\n\npub fn run() {\n    let n = 2;\n    mymac!(text, 3);\n    mymac!(text(3));\n    mymac!(n, 4);\n    println!(\"{}\", text);\n    std::println!(\"{} \", text);\n    vec![text; 2];\n    assert!(mymac!(text, 5));\n    stringify!(text);\n    mymac!(crate::text);\n    mymac!(for text in 0..2 { drop(text); });\n}\n";
        resolver_case(
            "macro_plain",
            &[("src/lib.rs", rs), ("Cargo.toml", "[package]\nname = \"demo\"\n")],
            &[
                ("src/lib.rs", "mymac!(text, 3)", "text", "macro argument"),
                ("src/lib.rs", "mymac!(text(3))", "text", "src/lib.rs text"),
                ("src/lib.rs", "mymac!(n, 4)", "n", "local variable"),
                ("src/lib.rs", "\"{}\", text)", "text", "src/lib.rs text"),
                ("src/lib.rs", "\"{} \", text)", "text", "src/lib.rs text"),
                ("src/lib.rs", "vec![text; 2]", "text", "src/lib.rs text"),
                ("src/lib.rs", "mymac!(text, 5)", "text", "macro argument"),
                ("src/lib.rs", "stringify!(text)", "text", "macro argument"),
                ("src/lib.rs", "crate::text);", "text", "src/lib.rs text"),
                ("src/lib.rs", "for text in", "text", "local variable"),
                ("src/lib.rs", "drop(text)", "text", "local variable"),
            ],
        );
    }

    #[test]
    fn a_closure_parameter_or_let_in_a_macro_is_a_local() {
        let rs = "pub fn text() -> u8 {\n    1\n}\n\npub fn run(rows: &[(u8, u8)]) -> Vec<String> {\n    let a = format!(\"{:?}\", rows.iter().map(|(c, text)| c + text).collect::<Vec<_>>());\n    let b = format!(\"{} {}\", text(), { let text = 2; text });\n    vec![a, b]\n}\n\npub fn param(text: u8) -> String {\n    format!(\"{}\", text)\n}\n";
        resolver_case(
            "macro_locals",
            &[("src/lib.rs", rs), ("Cargo.toml", "[package]\nname = \"demo\"\n")],
            &[
                ("src/lib.rs", "|(c, text)|", "text", "local variable"),
                ("src/lib.rs", "c + text)", "text", "local variable"),
                ("src/lib.rs", "text(), {", "text", "src/lib.rs text"),
                ("src/lib.rs", "let text = 2", "text", "local variable"),
                ("src/lib.rs", "2; text }", "text", "local variable"),
                ("src/lib.rs", "\"{}\", text)", "text", "local variable"),
            ],
        );
    }

    #[test]
    fn a_variable_declared_as_a_base_type_has_the_type_its_constructor_builds() {
        let ts = "export class Base {\n  size() { return 0; }\n}\nexport class Big extends Base {\n  size() { return 9; }\n}\nexport function f(): number {\n  const a: Base = new Big();\n  let b: Base = new Big();\n  b = new Base();\n  const c: Base = pick();\n  return a.size() + b.size() + c.size();\n}\nexport function pick(): Base { return new Big(); }\n";
        let py = "class Base:\n    def size(self):\n        return 0\n\n\nclass Big(Base):\n    def size(self):\n        return 9\n\n\ndef f():\n    a: Base = Big()\n    c: Base = make()\n    return a.size() + c.size()\n\n\ndef make():\n    return Big()\n";
        let go = "package g\n\ntype Sizer interface{ Size() int }\n\ntype Big struct{}\n\nfunc (b *Big) Size() int { return 9 }\n\nfunc f() int {\n\tvar a Sizer = &Big{}\n\tvar b Sizer = Big{}\n\tb = nil\n\treturn a.Size() + b.Size()\n}\n";
        let rs = "pub trait Sizer {\n    fn size(&self) -> u8;\n}\n\npub struct Big;\n\nimpl Sizer for Big {\n    fn size(&self) -> u8 {\n        9\n    }\n}\n\npub fn f() -> u8 {\n    let a: Box<dyn Sizer> = Box::new(Big);\n    let b: &dyn Sizer = &Big {};\n    a.size() + b.size()\n}\n";
        let java = "class Base { int size() { return 0; } }\nclass Big extends Base { int size() { return 9; } }\nclass J {\n    int f() {\n        Base a = new Big();\n        Base b = new Big();\n        b = new Base();\n        return a.size() + b.size();\n    }\n}\n";
        resolver_case(
            "built",
            &[("t.ts", ts), ("p.py", py), ("g/g.go", go), ("src/lib.rs", rs), ("J.java", java)],
            &[
                ("t.ts", "a.size()", "size", "t.ts Big.size"),
                ("t.ts", "b.size()", "size", "t.ts Base.size"),
                ("t.ts", "c.size()", "size", "t.ts Base.size"),
                ("p.py", "a.size()", "size", "p.py Big.size"),
                ("p.py", "c.size()", "size", "p.py Base.size"),
                ("g/g.go", "a.Size()", "Size", "g/g.go Big.Size"),
                ("g/g.go", "b.Size()", "Size", "not found"),
                ("src/lib.rs", "a.size()", "size", "src/lib.rs Big.size"),
                ("src/lib.rs", "b.size()", "size", "src/lib.rs Big.size"),
                ("J.java", "a.size()", "size", "J.java Big.size"),
                ("J.java", "b.size()", "size", "J.java Base.size"),
            ],
        );
    }

    #[test]
    fn a_super_call_is_the_method_of_the_nearest_base_that_defines_it() {
        let ts = "import { Outer } from 'outside';\nexport class Base {\n  constructor(n: number) {}\n  size(): number { return 0; }\n}\nexport class Big extends Base {\n  constructor() { super(1); }\n  size(): number { return super.size() + 9; }\n  other(): number { return this.size(); }\n}\nexport class Bigger extends Big {\n  size(): number { return super.size() + 1; }\n}\nexport class Plain extends Big {\n  other(): number { return super.size() + 2; }\n}\nexport class Far extends Outer {\n  size(): number { return super.size() + 3; }\n}\nexport class Err extends Error {\n  size(): number { return super.size() + 4; }\n}\n";
        let js = "export class A {\n  m() { return 0; }\n}\nexport class B extends A {\n  m() { return super.m() + 1; }\n}\n";
        let py = "from outside import Thing\n\n\nclass Base:\n    def size(self):\n        return 0\n\n\nclass Big(Base):\n    def size(self):\n        return super().size() + 9\n\n    def other(self):\n        return self.size()\n\n\nclass Bigger(Big):\n    def size(self):\n        return super(Bigger, self).size() + 1\n\n    def skip(self):\n        return super(Big, self).size() + 2\n\n\nclass Left(Base):\n    pass\n\n\nclass Right(Base):\n    def size(self):\n        return 5\n\n\nclass Both(Left, Right):\n    def size(self):\n        return super().size() + 3\n\n    def load(self):\n        return super(Left, self).size() + 6\n\n    def odd(self):\n        return super(Bigger, self).size() + 7\n\n\nclass Far(Thing):\n    def size(self):\n        return super().size() + 4\n\n\nclass Alone:\n    def __init__(self):\n        super().__init__()\n";
        let go = "package g\n\ntype Base struct{}\n\nfunc (b *Base) Size() int { return 0 }\n\ntype Big struct {\n\tBase\n}\n\nfunc (b *Big) Size() int { return b.Base.Size() + 9 }\n";
        resolver_case(
            "super",
            &[("t.ts", ts), ("j.js", js), ("p.py", py), ("g/g.go", go)],
            &[
                ("t.ts", "super.size() + 9", "size", "t.ts Base.size"),
                ("t.ts", "super.size() + 1", "size", "t.ts Big.size"),
                ("t.ts", "super.size() + 2", "size", "t.ts Big.size"),
                ("t.ts", "this.size()", "size", "t.ts Big.size"),
                ("t.ts", "super.size() + 3", "size", "external"),
                ("t.ts", "super.size() + 4", "size", "external"),
                ("t.ts", "super(1)", "super", "t.ts Base"),
                ("j.js", "super.m()", "m", "j.js A.m"),
                ("p.py", "super().size() + 9", "size", "p.py Base.size"),
                ("p.py", "self.size()", "size", "p.py Big.size"),
                ("p.py", "super(Bigger, self).size()", "size", "p.py Big.size"),
                ("p.py", "super(Big, self).size()", "size", "p.py Base.size"),
                ("p.py", "super().size() + 3", "size", "p.py Right.size"),
                // the class after `Left` in the order of `Both`, the class the call sits in
                ("p.py", "super(Left, self).size() + 6", "size", "p.py Right.size"),
                // a class that is not in the order of `Both`: Python raises, nothing is decided
                ("p.py", "super(Bigger, self).size() + 7", "size", "receiver"),
                ("p.py", "super().size() + 4", "size", "external"),
                ("p.py", "super().__init__()", "__init__", "external"),
                ("g/g.go", "b.Base.Size()", "Size", "g/g.go Base.Size"),
            ],
        );
    }

    #[test]
    fn an_inherited_method_is_the_first_in_the_method_resolution_order() {
        let py = "class A:\n    def m(self):\n        return 1\n\n\nclass B(A):\n    pass\n\n\nclass C(A):\n    def m(self):\n        return 2\n\n\nclass D(B, C):\n    def k(self):\n        return self.m() + 1\n\n\nclass P1:\n    def top(self):\n        return 0\n\n\nclass P2(P1):\n    pass\n\n\nclass P3(P2):\n    pass\n\n\nclass P4(P3):\n    pass\n\n\nclass P5(P4):\n    pass\n\n\nclass X:\n    def w(self):\n        return 3\n\n\nclass Y:\n    def w(self):\n        return 4\n\n\nclass XY(X, Y):\n    pass\n\n\nclass YX(Y, X):\n    pass\n\n\nclass Bad(XY, YX):\n    pass\n\n\ndef use():\n    return D().m() + P5().top() + Bad().w()\n";
        let ts = "export class T1 {\n  m() { return 1; }\n}\nexport class T2 extends T1 {}\nexport class T3 extends T2 {}\nexport class T4 extends T3 {}\nexport class T5 extends T4 {\n  k() { return this.m() + 1; }\n}\nexport interface Named {\n  name(): string;\n}\nexport class N extends T5 implements Named {\n  name() { return 'n'; }\n}\nexport function use(n: N) {\n  return new T5().m() + n.m() + n.name().length;\n}\n";
        let js = "export class J1 {\n  m() { return 1; }\n}\nexport class J2 extends J1 {}\nexport class J3 extends J2 {}\nexport class J4 extends J3 {}\nexport class J5 extends J4 {}\nexport function use() {\n  return new J5().m() + 2;\n}\n";
        resolver_case(
            "mro",
            &[("p.py", py), ("t.ts", ts), ("j.js", js)],
            &[
                // Python's order for D is D, B, C, A
                ("p.py", "self.m() + 1", "m", "p.py C.m"),
                ("p.py", "D().m()", "m", "p.py C.m"),
                ("p.py", "P5().top()", "top", "p.py P1.top"),
                // Python refuses Bad (no consistent order): the bases in header order, XY first
                ("p.py", "Bad().w()", "w", "p.py X.w"),
                ("t.ts", "this.m() + 1", "m", "t.ts T1.m"),
                ("t.ts", "new T5().m()", "m", "t.ts T1.m"),
                ("t.ts", "n.m()", "m", "t.ts T1.m"),
                ("t.ts", "n.name()", "name", "t.ts N.name"),
                ("j.js", "new J5().m()", "m", "j.js J1.m"),
            ],
        );
    }

    #[test]
    fn the_header_chunk_is_no_definition_wherever_it_sorts() {
        let (root, s) = indexed("header", &[("store.js", "export const store = {\n  total() {\n    return 1;\n  },\n};\n")]);
        let rv = Resolver::new(&s.ix).unwrap();
        let defs: Vec<String> = rv.defs_of("store.js").unwrap().iter().map(|d| format!("{}-{} {} {}", d.start, d.end, d.kind, d.qualname)).collect();
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
        assert!(defs.iter().all(|d| !d.contains(" module ")) && !defs.is_empty(), "{defs:?}");
    }

    #[test]
    fn an_import_in_a_language_without_a_resolver_loads_no_file() {
        let (root, s) = indexed("unread_import", &[("a.rb", "def a\nend\n"), ("m.rb", "require_relative 'a'\nputs a\n")]);
        let mut rv = Resolver::new(&s.ix).unwrap();
        let found = rv.import_files("m.rb", 1, "a");
        let used = rv.resolve_at("m.rb", 2, 5, "a");
        let cached = rv.cache.len();
        drop(rv);
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!((found, used, cached), (None, Err(UNREAD), 0));
    }

    #[test]
    fn rust_rules() {
        let lib = "mod a;\nmod b;\npub use b::Thing;\n\nfn top() {}\n";
        let a = "pub fn make() {}\n\npub struct Thing;\n\nimpl Thing {\n    pub fn new() -> Self {\n        Self::helper();\n        Thing\n    }\n    fn helper() {}\n}\n";
        let b = "use crate::a;\nuse crate::a::Thing as AThing;\nuse std::fmt;\n\npub fn make() {}\n\npub struct Thing;\n\nfn use_it() {\n    make();\n    a::make();\n    crate::a::make();\n    super::top();\n    let t = AThing::new();\n    fmt::f();\n}\n";
        let c = "use crate::a::*;\nuse crate::Thing;\n\nfn go(x: Thing) {\n    make();\n    x.make();\n}\n";
        resolver_case(
            "rs",
            &[("Cargo.toml", "[package]\nname = \"demo\"\n"), ("src/lib.rs", lib), ("src/a.rs", a), ("src/b.rs", b), ("src/c.rs", c)],
            &[
                ("src/b.rs", "    make();", "make", "src/b.rs make"),
                ("src/b.rs", "    a::make();", "make", "src/a.rs make"),
                ("src/b.rs", "crate::a::make();", "make", "src/a.rs make"),
                ("src/b.rs", "super::top();", "top", "src/lib.rs top"),
                ("src/b.rs", "AThing::new()", "new", "src/a.rs Thing.new"),
                ("src/b.rs", "AThing::new()", "AThing", "src/a.rs Thing"),
                ("src/a.rs", "Self::helper()", "helper", "src/a.rs Thing.helper"),
                ("src/c.rs", "    make();", "make", "src/a.rs make"),
                ("src/c.rs", "x: Thing", "Thing", "src/b.rs Thing"),
                ("src/c.rs", "x.make()", "make", "not found"), // `x` is a `Thing`, which has no `make`
                ("src/b.rs", "::f()", "f", "external"),
            ],
        );
    }

    #[test]
    fn go_rules() {
        resolver_case(
            "go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("p/a.go", "package p\n\nfunc Make() {}\n"),
                ("p/b.go", "package p\n\nfunc use1() { Make() }\n"),
                ("q/a.go", "package q\n\nfunc Make() {}\n"),
                ("main.go", "package main\n\nimport (\n\t\"example.com/m/p\"\n\tqq \"example.com/m/q\"\n\t\"fmt\"\n)\n\nfunc main() { p.Make(); qq.Make(); fmt.Println() }\n"),
            ],
            &[
                ("p/b.go", "{ Make() }", "Make", "p/a.go Make"),
                ("main.go", "p.Make()", "Make", "p/a.go Make"),
                ("main.go", "qq.Make()", "Make", "q/a.go Make"),
                ("main.go", "fmt.Println()", "Println", "external"),
            ],
        );
    }

    #[test]
    fn php_rules() {
        let log = "<?php\nnamespace Lib {\n    trait Logs { public function log() {} }\n    interface Shows { public function show(); }\n}\n";
        let page = "<?php\nnamespace Web;\n\nclass Page implements \\Lib\\Shows\n{\n    use \\Lib\\Logs;\n    const LIMIT = 3;\n    public $title;\n    public function show() { $this->log(); return $this->title . self::LIMIT; }\n}\n";
        let main = "<?php\nnamespace Web;\n\nfunction run(Page $p, $q)\n{\n    if ($q instanceof Page) { $p->show(); }\n    foreach ([$p] as $p) { $p->show(); }\n    $p = new \\Web\\Page();\n    return $p->show() . \\Lib\\helper() . PHP_EOL . strlen($q);\n}\n";
        resolver_case(
            "php",
            &[("lib/log.php", log), ("web/Page.php", page), ("web/main.php", main), ("lib/helper.php", "<?php\nnamespace Lib;\n\nfunction helper() {}\n")],
            &[
                // a method of a trait the class uses; a braced namespace
                ("web/Page.php", "$this->log()", "log", "lib/log.php Logs.log"),
                ("web/Page.php", "implements \\Lib\\Shows", "Shows", "lib/log.php Shows"),
                ("web/Page.php", "implements \\Lib\\Shows", "Lib", "namespace"),
                ("web/Page.php", "$this->title", "title", "field or key"),
                ("web/Page.php", "self::LIMIT", "LIMIT", "field or key"),
                ("web/main.php", "$q instanceof Page", "Page", "web/Page.php Page"),
                ("web/main.php", "$q instanceof", "q", "local variable"),
                ("web/main.php", "Page) { $p->show()", "show", "web/Page.php Page.show"),
                // a `foreach` target has no type the resolver reads
                ("web/main.php", "as $p) { $p->show()", "show", "receiver"),
                // the latest assignment before the use decides
                ("web/main.php", "return $p->show()", "show", "web/Page.php Page.show"),
                ("web/main.php", "\\Lib\\helper()", "helper", "lib/helper.php helper"),
                ("web/main.php", "PHP_EOL", "PHP_EOL", "constant"),
                ("web/main.php", "strlen($q)", "strlen", "external"),
            ],
        );
    }

    #[test]
    fn a_local_bound_by_a_pattern_import_or_initializer_hides_the_function_only_in_its_scope() {
        let ts = "export function target(): number {\n  return 1;\n}\nexport const a1 = ({ target }: { target: () => number }) => target() + 1;\nexport function a3({ x: target }: { x: () => number }) {\n  return target() + 3;\n}\nexport function a4({ target = () => 2 }) {\n  return target() + 4;\n}\nexport function a5(...target: Array<() => number>) {\n  return target[0]() + 5;\n}\nexport function a6(obj: { target: () => number }) {\n  const { target } = obj;\n  return target() + 6;\n}\nexport function a7(obj: any) {\n  const { a: { b: target }, ...rest } = obj;\n  return target() + 7;\n}\nexport function a9() {\n  try {\n    return 0;\n  } catch (target) {\n    return target + 9;\n  }\n  return target() + 90;\n}\nexport class K {\n  constructor(private target: () => number) {\n    target() + 10;\n  }\n}\nexport function a12(target: () => number) {\n  return [1].map(() => target() + 12);\n}\nexport function real() {\n  return target() + 99;\n}\n";
        let py = "def target():\n    return 1\n\n\ndef b5():\n    import os as target\n    return target.getcwd()\n\n\ndef b6():\n    from os import getcwd as target\n    return target() + 6\n\n\ndef b7(items):\n    if (target := len(items)) > 1:\n        return target + 7\n    return 0\n\n\ndef b8(target=target, *, k=lambda target=target: target):\n    return target() + 8\n\n\ndef b9(v):\n    match v:\n        case [target, *rest]:\n            return target() + 9\n        case P(k=target) | {\"k\": target}:\n            return target() + 90\n    return 0\n\n\ndef b10(a):\n    with a as (target, b):\n        return target() + 10\n\n\ndef b11(x: target) -> target:\n    return x\n\n\ndef b12(items):\n    out = [target for target in items]\n    return target() + 12\n\n\ndef b13():\n    from c import helper as target\n    return target() + 13\n\n\ndef b14(v):\n    match v:\n        case _ as target:\n            return target() + 14\n\n\ndef b15(target):\n    def inner(x=target, y=1):\n        return target() + 15\n    return inner\n\n\ndef b17():\n    def inner(target=target):\n        return target() + 17\n    return inner\n\n\ndef real():\n    return target() + 99\n";
        let rs = "pub fn target() -> u8 {\n    1\n}\n\npub struct P {\n    pub target: fn() -> u8,\n}\n\npub fn r2(o: Option<fn() -> u8>) -> u8 {\n    if let Some(target) = o {\n        return target() + 2;\n    } else {\n        return target() + 20;\n    }\n}\n\npub fn r3(o: Option<fn() -> u8>) -> u8 {\n    let x = match o {\n        Some(target) => target() + 3,\n        None => 0,\n    };\n    x + target() + 30\n}\n\npub fn r6(p: P) -> u8 {\n    let P { target } = p;\n    target() + 6\n}\n\npub fn r9(o: Option<fn() -> u8>) -> u8 {\n    let Some(target) = o else { return target() + 90 };\n    target() + 9\n}\n\npub fn r10(o: Option<fn() -> u8>) -> u8 {\n    while let Some(target) = o {\n        return target() + 10;\n    }\n    target() + 100\n}\n\npub fn r11(o: Option<(u8, fn() -> u8)>) -> u8 {\n    match o {\n        _w @ Some((_, mut target)) => target() + 11,\n        _ => 0,\n    }\n}\n\npub fn r12(p: &P) -> u8 {\n    let &P { ref target } = p;\n    target() + 12\n}\n\npub fn r13() -> u8 {\n    let target = target() + 13;\n    target\n}\n\npub fn r14(v: &[fn() -> u8]) -> u8 {\n    if let [target, ..] = v {\n        return target() + 14;\n    }\n    0\n}\n\npub fn r15(target: fn() -> u8) -> u8 {\n    let f = || target() + 15;\n    f()\n}\n\npub fn r16(target: u8) -> u8 {\n    fn inner() -> u8 {\n        target() + 16\n    }\n    inner() + target\n}\n\npub fn real() -> u8 {\n    target() + 99\n}\n";
        let go = "package gp\n\nfunc target() int { return 1 }\n\nfunc g4(x interface{}) int {\n\tswitch target := x.(type) {\n\tcase func() int:\n\t\treturn target() + 4\n\t}\n\treturn target() + 40\n}\n\nfunc g6(m map[string]func() int) int {\n\tif target, ok := m[\"a\"]; ok {\n\t\treturn target() + 6\n\t}\n\treturn target() + 60\n}\n\nfunc g7(ch chan func() int) int {\n\tselect {\n\tcase target := <-ch:\n\t\treturn target() + 7\n\t}\n\treturn target() + 70\n}\n\nfunc g8(target func() int) func() int {\n\treturn func() int { return target() + 8 }\n}\n\nfunc real() int { return target() + 99 }\n";
        let (local, ts_fn, py_fn, rs_fn, go_fn) = ("local variable", "b.ts target", "a.py target", "src/lib.rs target", "gp/a.go target");
        resolver_case(
            "locals",
            &[
                ("b.ts", ts),
                ("a.py", py),
                ("c.py", "def helper():\n    pass\n"),
                ("Cargo.toml", "[package]\nname = \"demo\"\n"),
                ("src/lib.rs", rs),
                ("go.mod", "module example.com/m\n"),
                ("gp/a.go", go),
            ],
            &[
                ("b.ts", "({ target }", "target", local),
                ("b.ts", "target() + 1;", "target", local),
                ("b.ts", "target() + 3", "target", local),
                ("b.ts", "target() + 4", "target", local),
                ("b.ts", "target[0]() + 5", "target", local),
                ("b.ts", "target() + 6", "target", local),
                ("b.ts", "target() + 7", "target", local),
                ("b.ts", "catch (target)", "target", local),
                ("b.ts", "target + 9", "target", local),
                ("b.ts", "target() + 90", "target", ts_fn),
                ("b.ts", "target() + 10", "target", local),
                ("b.ts", "target() + 12", "target", local),
                ("b.ts", "target() + 99", "target", ts_fn),
                ("a.py", "target.getcwd()", "target", "external"),
                ("a.py", "target() + 6", "target", "external"),
                ("a.py", "(target :=", "target", local),
                ("a.py", "target + 7", "target", local),
                ("a.py", "=target,", "target", py_fn),
                ("a.py", "=target:", "target", py_fn),
                ("a.py", ": target)", "target", local),
                ("a.py", "target() + 8", "target", local),
                ("a.py", "target() + 9", "target", local),
                ("a.py", "target() + 90", "target", local),
                ("a.py", "target() + 10", "target", local),
                ("a.py", "x: target", "target", py_fn),
                ("a.py", "-> target", "target", py_fn),
                ("a.py", "target() + 12", "target", py_fn),
                ("a.py", "target() + 13", "target", "c.py helper"),
                ("a.py", "target() + 14", "target", local),
                ("a.py", "x=target", "target", local),
                ("a.py", "target() + 15", "target", local),
                ("a.py", "inner(target=", "target", local),
                ("a.py", "=target):", "target", py_fn),
                ("a.py", "target() + 17", "target", local),
                ("a.py", "target() + 99", "target", py_fn),
                ("src/lib.rs", "Some(target) = o {", "target", local),
                ("src/lib.rs", "target() + 2;", "target", local),
                ("src/lib.rs", "target() + 20", "target", rs_fn),
                ("src/lib.rs", "target() + 3,", "target", local),
                ("src/lib.rs", "target() + 30", "target", rs_fn),
                ("src/lib.rs", "P { target }", "target", local),
                ("src/lib.rs", "target() + 6", "target", local),
                ("src/lib.rs", "target() + 90", "target", rs_fn),
                ("src/lib.rs", "    target() + 9", "target", local),
                ("src/lib.rs", "target() + 10", "target", local),
                ("src/lib.rs", "target() + 100", "target", rs_fn),
                ("src/lib.rs", "target() + 11", "target", local),
                ("src/lib.rs", "target() + 12", "target", local),
                ("src/lib.rs", "target() + 13", "target", rs_fn),
                ("src/lib.rs", "target() + 14", "target", local),
                ("src/lib.rs", "target() + 15", "target", local),
                ("src/lib.rs", "target() + 16", "target", rs_fn),
                ("src/lib.rs", "target() + 99", "target", rs_fn),
                ("gp/a.go", "switch target", "target", local),
                ("gp/a.go", "target() + 4", "target", local),
                ("gp/a.go", "target() + 40", "target", go_fn),
                ("gp/a.go", "target() + 6", "target", local),
                ("gp/a.go", "target() + 60", "target", go_fn),
                ("gp/a.go", "target() + 7", "target", local),
                ("gp/a.go", "target() + 70", "target", go_fn),
                ("gp/a.go", "target() + 8", "target", local),
                ("gp/a.go", "target() + 99", "target", go_fn),
            ],
        );
    }

    const JAVA_SHAPE: &str = "package com.acme.shapes;

public interface Shape {
    double area();

    default String label() {
        return \"shape \" + area();
    }
}
";
    const JAVA_CIRCLE: &str = "package com.acme.shapes;

import java.util.List;

public class Circle implements Shape {
    private final double r;
    private Report last;

    public Circle(double r) {
        this.r = r;
    }

    public double area() {
        return Math.PI * r * r;
    }

    public static Circle unit() {
        return new Circle(1);
    }

    public Report summary() {
        return new Report(area());
    }

    public boolean check(List<String> names) {
        names.add(label());
        return last.ok() && names.isEmpty();
    }

    public static class Inner {
        public int depth() { return 1; }
    }

    public enum Kind {
        SMALL, LARGE;

        public boolean big() {
            return this == LARGE;
        }
    }
}
";
    const JAVA_MAIN: &str = "package com.acme.app;

import com.acme.shapes.Circle;
import com.acme.shapes.Shape;
import com.acme.shapes.*;
import static com.acme.app.Util.scale;
import static com.acme.app.Util.*;
import java.util.List;
import java.util.function.Supplier;

public class Main extends Base {
    private Circle held = Circle.unit();

    void note(String s) {}

    void note(String s, int level) {}

    <T> T pick(T x) {
        return x;
    }

    void run(List<Shape> shapes, Shape given) {
        Shape s = Circle.unit();
        s.area();
        given.label();
        given.hashCode();
        var c = new Circle(2);
        c.area();
        held.area();
        this.held.summary();
        Circle.unit().summary().ok();
        new Square().area();
        note(\"a\");
        note(\"a\", 1);
        this.note(\"b\");
        Runnable all = this::note;
        scale(2);
        scale(2, 3);
        log(\"x\");
        shapes.forEach(x -> x.area());
        shapes.size();
        for (Shape each : shapes) {
            each.area();
        }
        Supplier<Circle> make = Circle::unit;
        Runnable r = new Runnable() {
            public void run() {
                note(\"x\");
                helper();
            }
        };
        try {
            inherited();
        } catch (IllegalStateException e) {
            e.getMessage();
        }
        Object o = given;
        if (o instanceof Circle k) {
            k.area();
        }
        new Circle.Inner().depth();
        Circle.Kind.SMALL.big();
        int area = 3;
        String t = \"x\";
        t.length();
        super.inherited();
        com.acme.shapes.Circle.unit();
    }

    void helper() {}
}
";

    #[test]
    fn java_rules() {
        let (shapes, app) = ("src/main/java/com/acme/shapes", "src/main/java/com/acme/app");
        let files: Vec<(String, &str)> = vec![
            (format!("{shapes}/Shape.java"), JAVA_SHAPE),
            (format!("{shapes}/Circle.java"), JAVA_CIRCLE),
            (format!("{shapes}/Square.java"), "package com.acme.shapes;\n\npublic class Square implements Shape {\n    public double area() {\n        return 4;\n    }\n\n    public String toString() {\n        return label();\n    }\n}\n"),
            (format!("{shapes}/Report.java"), "package com.acme.shapes;\n\npublic record Report(double value) {\n    public boolean ok() {\n        return value > 0;\n    }\n}\n"),
            (format!("{app}/Util.java"), "package com.acme.app;\n\npublic class Util {\n    public static int scale(int x) {\n        return x * 2;\n    }\n\n    public static int scale(int x, int y) {\n        return x * y;\n    }\n\n    public static void log(String s) {}\n}\n"),
            (format!("{app}/Base.java"), "package com.acme.app;\n\npublic class Base {\n    void inherited() {}\n}\n"),
            (format!("{app}/Main.java"), JAVA_MAIN),
        ];
        let files: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        let (root, s) = indexed("java", &files);
        let mut rv = Resolver::new(&s.ix).unwrap();
        // (file, text the name is in, name, "file:line qualname (how)" or why there is none)
        let checks: &[(&str, &str, &str, &str)] = &[
            ("Main", "import com.acme.shapes.Circle;", "Circle", "Circle:5 Circle (import)"),
            ("Main", "Util.scale;", "scale", "Util:4 Util.scale (overload)"),
            ("Main", "import java.util.List;", "List", "external"),
            ("Main", "import com.acme.shapes.Shape;", "acme", "package"),
            ("Main", "extends Base", "Base", "Base:3 Base (type)"),
            ("Main", "private Circle held", "Circle", "Circle:5 Circle (type)"),
            ("Main", "s.area()", "area", "Shape:4 Shape.area (member)"),
            ("Main", "given.label()", "label", "Shape:6 Shape.label (member)"),
            ("Main", "given.hashCode()", "hashCode", "external"),
            ("Main", "new Circle(2)", "Circle", "Circle:5 Circle (type)"),
            ("Main", "c.area()", "area", "Circle:13 Circle.area (member)"),
            ("Main", "held.area()", "area", "Circle:13 Circle.area (member)"),
            ("Main", "this.held.summary()", "summary", "Circle:21 Circle.summary (member)"),
            ("Main", "summary().ok()", "ok", "Report:4 Report.ok (member)"),
            ("Main", "new Square().area()", "area", "Square:4 Square.area (member)"),
            ("Main", "note(\"a\");", "note", "Main:14 Main.note (member)"),
            ("Main", "note(\"a\", 1)", "note", "Main:16 Main.note (member)"),
            ("Main", "this.note(\"b\")", "note", "Main:14 Main.note (member)"),
            ("Main", "this::note", "note", "Main:14 Main.note (overload)"),
            ("Main", "scale(2);", "scale", "Util:4 Util.scale (import)"),
            ("Main", "scale(2, 3)", "scale", "Util:8 Util.scale (import)"),
            ("Main", "log(\"x\")", "log", "Util:12 Util.log (import)"),
            ("Main", "x.area()", "area", "receiver"),
            ("Main", "shapes.size()", "size", "external"),
            ("Main", "each.area()", "area", "Shape:4 Shape.area (member)"),
            ("Main", "Circle::unit", "unit", "Circle:17 Circle.unit (member)"),
            ("Main", "note(\"x\")", "note", "Main:14 Main.note (member)"),
            ("Main", "helper();", "helper", "Main:70 Main.helper (member)"),
            ("Main", "    inherited();", "inherited", "Base:4 Base.inherited (member)"),
            ("Main", "e.getMessage()", "getMessage", "external"),
            ("Main", "k.area()", "area", "Circle:13 Circle.area (member)"),
            ("Main", "Circle.Inner()", "Inner", "Circle:30 Circle.Inner (qualified)"),
            ("Main", "Inner().depth()", "depth", "Circle:31 Circle.Inner.depth (member)"),
            ("Main", "SMALL.big()", "big", "Circle:37 Circle.Kind.big (member)"),
            ("Main", "SMALL.big()", "SMALL", "Circle:35 Circle.Kind.SMALL (field)"),
            ("Main", "int area = 3", "area", "local variable"),
            ("Main", "t.length()", "length", "external"),
            ("Main", "super.inherited()", "inherited", "Base:4 Base.inherited (super)"),
            ("Main", "shapes.Circle.unit()", "unit", "Circle:17 Circle.unit (member)"),
            ("Main", "shapes.Circle.unit()", "Circle", "Circle:5 Circle (qualified)"),
            ("Main", "T pick(T x)", "T", "type parameter"),
            ("Main", "return x;", "x", "local variable"),
            ("Circle", "names.add(label())", "add", "external"),
            ("Circle", "names.add(label())", "label", "Shape:6 Shape.label (member)"),
            ("Circle", "last.ok()", "ok", "Report:4 Report.ok (member)"),
            ("Circle", "Math.PI", "PI", "external"),
            ("Circle", "new Report(area())", "area", "Circle:13 Circle.area (member)"),
            ("Circle", "this == LARGE", "LARGE", "Circle:35 Circle.Kind.LARGE (field)"),
            ("Circle", "this.r = r", "r", "Circle:6 Circle.r (field)"),
            ("Circle", "= r;", "r", "local variable"),
            ("Square", "return label();", "label", "Shape:6 Shape.label (member)"),
            ("Shape", "+ area()", "area", "Shape:4 Shape.area (member)"),
            ("Report", "value > 0", "value", "Report:3 Report.value (field)"),
        ];
        let mut wrong = Vec::new();
        let file_of = |short: &str| files.iter().find(|(p, _)| p.ends_with(&format!("/{short}.java"))).unwrap();
        for (short, needle, name, want) in checks {
            let (path, src) = file_of(short);
            let (row, l) = src.lines().enumerate().find(|(_, l)| l.contains(needle)).unwrap_or_else(|| panic!("{needle:?} not in {short}"));
            let col = l.find(needle).unwrap() + needle.find(name).unwrap();
            let got = match rv.resolve_at(path, row as u64 + 1, col, name) {
                Ok((id, how)) => {
                    let c = s.ix.chunk(id).unwrap().unwrap();
                    let file = c.path.rsplit('/').next().unwrap().trim_end_matches(".java").to_string();
                    format!("{file}:{} {} ({how})", c.start, c.qualname)
                }
                Err(e) => e.to_string(),
            };
            if got != *want {
                wrong.push(format!("{short}: {needle} -> {got}, want {want}"));
            }
        }
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn java_package_declarations() {
        assert_eq!(java_package("// c\n/* d */\n@Generated(\"x(y)\")\npackage a.b ;\nimport c;"), "a.b");
        assert_eq!(java_package("import c;\nclass K {}"), "");
    }

    #[test]
    fn helpers() {
        assert_eq!(strip_generics("Foo<'a, T>.new"), "Foo.new");
        assert_eq!(join("a/b", "../c/./d"), "a/c/d");
        assert_eq!(join("src", "../../lib"), "../lib");
        assert_eq!(
            (py_relative("src/b.py", "..x.y"), py_relative("src/b.py", "...x"), py_relative("src/b.py", ".")),
            ("x/y".into(), "../x".into(), "src".into())
        );
        assert_eq!(Resolver::rust_mod_dir("src/a/b.rs"), "src/a/b");
        assert_eq!(Resolver::rust_mod_dir("src/a/mod.rs"), "src/a");
        assert_eq!(cargo_package_name("[workspace]\nname = \"no\"\n[package]\nname = \"grep-matcher\"\n"), Some("grep-matcher".into()));
    }
}
