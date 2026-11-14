//! Java: imports, static imports and packages by directory, locals and fields, classes around
//! and anonymous classes, supertypes in header order, overloads picked by the number of arguments.

use super::*;

pub(super) struct Java;

impl Rules for Java {
    const GRAMMARS: &'static [&'static str] = &["java"];

    fn bindings(n: Node, _path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
        java_bindings(n, src, out)
    }

    // a static import brings in every overload of the name
    fn import_site(r: &mut Resolver, _path: &str, _lang: Lang, b: &Binding) -> Resolution {
        r.java_binding(b).map(|id| r.java_overload(id, None, "import"))
    }

    fn occurrence(r: &mut Resolver, path: &str, fi: &FileInfo, node: Node, name: &str, _line: i64) -> Option<Resolution> {
        Some(r.java_resolve(path, fi, node, name))
    }

    fn bare(r: &mut Resolver, _path: &str, fi: &FileInfo, _lang: Lang, node: Node, name: &str, _line: i64) -> Resolution {
        r.java_bare(fi, node, name)
    }

    fn qualified(r: &mut Resolver, path: &str, fi: &FileInfo, _lang: Lang, q: Node, node: Node, name: &str, _line: i64) -> Resolution {
        r.java_qualified(path, fi, q, node, name)
    }

    fn variable_ty(r: &mut Resolver, path: &str, fi: &FileInfo, _lang: Lang, e: Node) -> Option<Ty> {
        r.java_variable_ty(path, fi, e)
    }

    fn expression_ty(r: &mut Resolver, path: &str, fi: &FileInfo, e: Node) -> Option<Option<Ty>> {
        (e.kind() == "method_invocation").then(|| r.java_call_ty(path, fi, e))
    }

    fn return_type(node: Node) -> Option<Node> {
        let java = node.kind() == "method_declaration";
        node.child_by_field_name("return_type").or_else(|| node.child_by_field_name(if java { "type" } else { "result" }))
    }

    fn field_ty(r: &mut Resolver, owner: i64, name: &str, _path: &str, _fi: &FileInfo, _lang: Lang) -> Option<Ty> {
        r.java_field_ty(owner, name, 0, &mut HashMap::new())
    }

    // an interface types a value as well as a class does: its methods are declared in it
    fn named_ty(r: &mut Resolver, _path: &str, fi: &FileInfo, _lang: Lang, n: Node) -> Option<Ty> {
        let leaf = type_name(n)?;
        let name = text(leaf, &fi.src);
        match r.java_type_node(fi, n) {
            Ok(id) => Some(Ty::Local(id)),
            Err("external") => Some(Ty::Foreign(name.to_string(), Vec::new())),
            Err(_) => None,
        }
    }

    fn inherited(r: &mut Resolver, id: i64, name: &str, depth: usize, _path: &str, _qualname: &str) -> Option<i64> {
        r.inherited_through(id, name, depth, |r, id| r.java_bases(id).iter().filter_map(|b| b.ok()).collect())
    }

    fn bases(r: &mut Resolver, id: i64, _path: &str, _fi: &FileInfo, _d: &Def, _lang: Lang) -> Vec<std::result::Result<i64, &'static str>> {
        r.java_bases(id).iter().copied().filter(|&b| b != Ok(id)).collect()
    }

    fn arity(fi: &FileInfo, id: i64) -> Option<(usize, bool)> {
        java_params(fi, id)
    }

    fn module_files(r: &mut Resolver, _from: &str, module: &str) -> Option<Vec<String>> {
        r.java_package_files(module)
    }

    fn import_files(r: &mut Resolver, _from: &str, _fi: &FileInfo, _line: i64, module: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        // `a.b.*` names a package, or the members of a class (`import static a.b.C.*`)
        let name = module.trim_end_matches(".*");
        if module.ends_with(".*") {
            out.extend(r.java_package_files(name).unwrap_or_default());
        }
        if out.is_empty() {
            out.extend(r.java_path(name).ok().and_then(|id| r.def_row(id)).map(|(path, _)| path));
        }
        Some(out)
    }
}

impl Resolver<'_> {
    /// The nearest local variable or parameter named `name` that `node` sees, in the method around
    /// it and the methods around that (an anonymous class or a lambda sees the locals around it).
    fn java_local<'t>(&mut self, node: Node<'t>, name: &str, src: &str) -> Option<Node<'t>> {
        const SCOPES: &[&str] =
            &["method_declaration", "constructor_declaration", "compact_constructor_declaration", "lambda_expression", "static_initializer"];
        let at = node.start_byte();
        // an instance initializer is a block directly in a class body, or in an enum's after its constants
        let scope = |n: &Node| {
            SCOPES.contains(&n.kind()) || n.kind() == "block" && n.parent().is_some_and(|p| matches!(p.kind(), "class_body" | "enum_body_declarations"))
        };
        let top = std::iter::successors(node.parent(), |n| n.parent()).filter(scope).last()?;
        // per binder: its scope and its span
        let rows = self.found_in(top, "java binders", name, || {
            let binds = |n: Node| n.kind() == "identifier" && text(n, src) == name && java_binder(n);
            let mut rows: Vec<[(usize, usize); 2]> = Self::binders_in(top, binds)
                .into_iter()
                .filter_map(|(start, end)| {
                    let s = java_scope(top.descendant_for_byte_range(start, end)?)?;
                    Some([(s.start_byte(), s.end_byte()), (start, end)])
                })
                .collect();
            rows.sort_unstable();
            rows.concat()
        });
        let (rows, _) = rows.as_chunks::<2>();
        // the scopes that hold the use are the nodes around it
        std::iter::successors(Some(node), |n| n.parent())
            .filter(|n| holds((n.start_byte(), n.end_byte()), at))
            .filter_map(|n| {
                let scope = (n.start_byte(), n.end_byte());
                let rows = &rows[rows.partition_point(|r| r[0] < scope)..];
                let rows = &rows[..rows.partition_point(|r| r[0] == scope)];
                let before = rows.partition_point(|r| r[1].0 <= at);
                before.checked_sub(1).map(|i| (rows[i][1], n))
            })
            .max_by_key(|&((start, _), _)| start)
            // a field of a class body between the binder's scope and the use hides the binder
            .filter(|&(_, scope)| {
                !std::iter::successors(node.parent(), |n| n.parent())
                    .take_while(|n| n.id() != scope.id())
                    .any(|n| n.kind() == "class_body" && java_declares_field(n, name, src))
            })
            .and_then(|((start, end), n)| n.descendant_for_byte_range(start, end))
    }
}

/// The field `name` the body of anonymous class `anon` declares.
fn java_anon_field(fi: &FileInfo, anon: Node, name: &str) -> Option<i64> {
    let mut cur = anon.walk();
    let body = anon.named_children(&mut cur).find(|c| c.kind() == "class_body")?;
    if !java_declares_field(body, name, &fi.src) {
        return None;
    }
    let (s, e) = (body.start_position().row as i64 + 1, body.end_position().row as i64 + 1);
    fi.defs.iter().find(|d| d.name == name && d.kind == "field" && s <= d.start && d.end <= e).map(|d| d.id)
}

/// Whether class body `body` declares a field `name`.
fn java_declares_field(body: Node, name: &str, src: &str) -> bool {
    let mut cur = body.walk();
    for d in body.named_children(&mut cur).filter(|d| d.kind() == "field_declaration") {
        let mut c2 = d.walk();
        for v in d.children_by_field_name("declarator", &mut c2) {
            if v.child_by_field_name("name").is_some_and(|x| text(x, src) == name) {
                return true;
            }
        }
    }
    false
}

// ------------------------------------------------------------------------- Java

/// A class around a Java occurrence: one the index holds, or the body of an anonymous class
/// (whose methods the index holds under the method around it).
#[derive(Clone, Copy)]
enum JClass<'t> {
    Named(i64),
    Anon(Node<'t>),
}

const JAVA_CLASS_NODES: &[&str] = &["class_declaration", "interface_declaration", "enum_declaration", "record_declaration"];
/// Methods a Java class has from `Object`, and an enum from `Enum`: none is a repository definition.
const JAVA_OBJECT_METHODS: &[&str] = &["equals", "hashCode", "toString", "getClass", "notify", "notifyAll", "wait", "clone", "finalize"];
const JAVA_ENUM_METHODS: &[&str] = &["values", "valueOf", "ordinal", "name", "compareTo", "getDeclaringClass", "describeConstable"];

fn java_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    if n.kind() != "import_declaration" {
        // imports come first in a file, outside any class
        return n.kind() != "program";
    }
    let mut cur = n.walk();
    let kids: Vec<Node> = n.named_children(&mut cur).collect();
    let Some(&path) = kids.iter().find(|k| matches!(k.kind(), "scoped_identifier" | "identifier")) else { return true };
    let full: String = text(path, src).chars().filter(|c| !c.is_whitespace()).collect();
    let is_static = text(n, src).trim_start_matches("import").trim_start().starts_with("static");
    if kids.iter().any(|k| k.kind() == "asterisk") {
        // `import static a.C.*` brings in members of a class, `import a.*` the classes of a package
        out.push(Binding { local: "*".into(), module: full, item: is_static.then(|| "*".to_string()), sites: vec![] });
    } else {
        let (module, item) = full.rsplit_once('.').unwrap_or(("", &full));
        let site = path.child_by_field_name("name").unwrap_or(path);
        out.push(Binding { local: item.into(), module: module.into(), item: Some(item.into()), sites: vec![site.start_byte()] });
    }
    true
}

/// The package a Java source declares (`a.b`), "" for the default package.
pub(super) fn java_package(src: &str) -> String {
    let mut rest = src.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("//") {
            rest = r.split_once('\n').map_or("", |(_, r)| r);
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, r)| r);
        } else if let Some(r) = rest.strip_prefix('@') {
            // an annotation of the package (package-info.java), with its arguments
            rest = r.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '.' || c == '$').trim_start();
            if rest.starts_with('(') {
                let mut depth = 0;
                let end = rest.char_indices().find(|&(_, c)| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                });
                rest = end.map_or("", |(i, _)| &rest[i + 1..]);
            }
        } else {
            break;
        }
    }
    match rest.strip_prefix("package") {
        Some(r) if r.starts_with(char::is_whitespace) => r.split(';').next().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect(),
        _ => String::new(),
    }
}

/// The first bytes of a repository file: enough for its package declaration.
pub(super) fn java_head(root: &std::path::Path, rel: &str) -> String {
    use std::io::Read;
    crate::answers::noted(root, rel);
    let p = root.join(rel);
    if !std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
        return String::new();
    }
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(&p) {
        let _ = f.take(8192).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// The receiver or path in front of a Java member name: `obj` of `obj.m()`, `obj.f`, `T::m`, `a.b` of `a.b.C`.
fn java_qualifier(n: Node) -> Option<Node> {
    let p = n.parent()?;
    let is = |f: &str| p.child_by_field_name(f).map(|c| c.id()) == Some(n.id());
    match p.kind() {
        "method_invocation" if is("name") => p.child_by_field_name("object"),
        "field_access" if is("field") => p.child_by_field_name("object"),
        "scoped_identifier" if is("name") => p.child_by_field_name("scope"),
        "method_reference" => p.named_child(0).filter(|f| f.id() != n.id()),
        "scoped_type_identifier" => n.prev_named_sibling().filter(|s| !s.kind().ends_with("annotation")),
        _ => None,
    }
}

/// The identifiers of a dotted Java name (`a.b.C`, `Outer.Inner<T>`); false when it is not one.
fn java_segments<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) -> bool {
    match n.kind() {
        "identifier" | "type_identifier" => {
            out.push(n);
            true
        }
        "scoped_type_identifier" | "scoped_identifier" | "field_access" | "generic_type" => {
            let mut cur = n.walk();
            let kids: Vec<Node> = n.named_children(&mut cur).filter(|c| c.kind() != "type_arguments" && !c.kind().ends_with("annotation")).collect();
            kids.into_iter().all(|c| java_segments(c, out))
        }
        _ => false,
    }
}

/// The classes around `node`, innermost first.
fn java_classes<'t>(fi: &FileInfo, node: Node<'t>) -> Vec<JClass<'t>> {
    let mut out = Vec::new();
    let mut a = node.parent();
    while let Some(n) = a {
        if JAVA_CLASS_NODES.contains(&n.kind()) {
            if let Some(id) = java_def(fi, n) {
                out.push(JClass::Named(id));
            }
        } else if n.kind() == "class_body" {
            if let Some(p) = n.parent().filter(|p| p.kind() == "object_creation_expression") {
                out.push(JClass::Anon(p));
            }
        }
        a = n.parent();
    }
    out
}

/// The definition of the index a Java class declaration is.
fn java_def(fi: &FileInfo, n: Node) -> Option<i64> {
    let name = n.child_by_field_name("name")?;
    let (name, line) = (text(name, &fi.src), name.start_position().row as i64 + 1);
    let d = fi.defs.iter().filter(|d| d.name == name && d.start <= line && line <= d.end && CONTAINER_KINDS.contains(&d.kind.as_str()));
    d.min_by_key(|d| d.end - d.start).map(|d| d.id)
}

/// Whether the identifier `n` declares a local variable or a parameter.
fn java_binder(n: Node) -> bool {
    let Some(p) = n.parent() else { return false };
    let is = |f: &str| p.child_by_field_name(f).map(|c| c.id()) == Some(n.id());
    match p.kind() {
        // a record's components are its fields
        "formal_parameter" => is("name") && p.parent().and_then(|l| l.parent()).is_some_and(|o| o.kind() != "record_declaration"),
        "catch_formal_parameter" | "enhanced_for_statement" | "resource" | "instanceof_expression" => is("name"),
        "variable_declarator" => is("name") && p.parent().is_some_and(|d| matches!(d.kind(), "local_variable_declaration" | "spread_parameter")),
        "inferred_parameters" | "type_pattern" | "record_pattern_component" => true,
        "lambda_expression" => is("parameters"),
        _ => false,
    }
}

/// Whether the identifier `n` names a field, an enum constant or a record component where it is declared.
fn java_field_site(n: Node) -> bool {
    let Some(p) = n.parent() else { return false };
    let named = p.child_by_field_name("name").is_some_and(|c| c.id() == n.id());
    named
        && match p.kind() {
            "variable_declarator" => p.parent().is_some_and(|d| matches!(d.kind(), "field_declaration" | "constant_declaration")),
            "enum_constant" => true,
            "formal_parameter" => p.parent().and_then(|l| l.parent()).is_some_and(|r| r.kind() == "record_declaration"),
            _ => false,
        }
}

/// The code a Java local binder `b` is visible in.
fn java_scope(b: Node) -> Option<Node> {
    let p = b.parent()?;
    match p.kind() {
        "formal_parameter" | "resource" => p.parent()?.parent(),
        "variable_declarator" => {
            let d = p.parent()?;
            if d.kind() == "spread_parameter" {
                d.parent()?.parent()
            } else {
                d.parent()
            }
        }
        "catch_formal_parameter" | "inferred_parameters" => p.parent(),
        "enhanced_for_statement" | "lambda_expression" => Some(p),
        // a pattern variable: the block around it, also after an `if (!(x instanceof T t)) return;`
        _ => std::iter::successors(Some(p), |x| x.parent())
            .find(|x| matches!(x.kind(), "block" | "switch_rule" | "switch_block_statement_group" | "lambda_expression" | "method_declaration")),
    }
}

/// What a Java local binder is declared with: (its type unless `var`, its value).
fn java_declared<'t>(b: Node<'t>, src: &str) -> (Option<Node<'t>>, Option<Node<'t>>) {
    let Some(p) = b.parent() else { return (None, None) };
    let typed = |n: Option<Node<'t>>| n.and_then(|n| n.child_by_field_name("type")).filter(|t| text(*t, src) != "var");
    match p.kind() {
        "variable_declarator" => (typed(p.parent()), p.child_by_field_name("value")),
        "formal_parameter" | "enhanced_for_statement" => (typed(Some(p)), None),
        "resource" => (typed(Some(p)), p.child_by_field_name("value")),
        "catch_formal_parameter" => {
            let mut cur = p.walk();
            let caught = p.named_children(&mut cur).find(|c| c.kind() == "catch_type").filter(|c| c.named_child_count() == 1);
            (caught.and_then(|c| c.named_child(0)), None)
        }
        "instanceof_expression" => (p.child_by_field_name("right"), None),
        "type_pattern" => (p.named_child(0), None),
        _ => (None, None),
    }
}

impl<'a> Resolver<'a> {
    /// Directory -> its `.java` files; package -> the directories whose first file declares it.
    /// A file's package and its path give its source root, so no list of roots is needed.
    fn java_pkgs(&mut self) -> &HashMap<String, Vec<String>> {
        if self.java_pkgs.is_none() {
            let mut dirs: HashMap<String, Vec<String>> = HashMap::new();
            for f in self.files.iter().filter(|f| f.ends_with(".java")) {
                dirs.entry(dir_of(f).to_string()).or_default().push(f.clone());
            }
            let mut pkgs: HashMap<String, Vec<String>> = HashMap::new();
            for (d, files) in dirs.iter_mut() {
                files.sort();
                pkgs.entry(java_package(&java_head(&self.ix.root, &files[0]))).or_default().push(d.clone());
            }
            for v in pkgs.values_mut() {
                v.sort();
            }
            self.java_dirs = Some(dirs);
            self.java_pkgs = Some(pkgs);
        }
        self.java_pkgs.as_ref().unwrap()
    }

    /// The files of the Java package `pkg`, when the repository holds it.
    fn java_package_files(&mut self, pkg: &str) -> Option<Vec<String>> {
        let dirs = self.java_pkgs().get(pkg)?.clone();
        let by_dir = self.java_dirs.as_ref()?;
        Some(dirs.iter().flat_map(|d| by_dir.get(d).cloned().unwrap_or_default()).collect())
    }

    /// A class, interface, enum or record at the top level of one of `files`.
    fn java_top(&self, files: &[String], name: &str) -> Option<i64> {
        let mut rows: Vec<Symbol> =
            self.ix.named(name).ok()?.into_iter().filter(|r| r.qualname == name && CONTAINER_KINDS.contains(&r.kind.as_str())).collect();
        rows.sort_by(|a, b| (&a.path, a.start).cmp(&(&b.path, b.start)));
        rows.into_iter().find(|r| files.contains(&r.path)).map(|r| r.id)
    }

    /// The definition a dotted Java name stands for: a class of a package (`a.b.C`), a member of
    /// one (`a.b.C.m`, `a.b.C.Inner`). "external" when no package of the repository starts it.
    fn java_path(&mut self, full: &str) -> std::result::Result<i64, &'static str> {
        if let Some(r) = self.java_paths.get(full) {
            return *r;
        }
        let segs: Vec<&str> = full.split('.').collect();
        let mut found = Err("external");
        for k in (0..segs.len()).rev() {
            let Some(files) = self.java_package_files(&segs[..k].join(".")) else { continue };
            let Some(mut cur) = self.java_top(&files, segs[k]) else {
                if k == 0 {
                    break;
                }
                found = Err("not found");
                break;
            };
            found = Ok(cur);
            for s in &segs[k + 1..] {
                // `import static a.C.counter` names a field
                match self.member(cur, s, 0).or_else(|| self.declared_field(cur, s)) {
                    Some(m) => cur = m,
                    None => {
                        found = Err("not found");
                        break;
                    }
                }
                found = Ok(cur);
            }
            break;
        }
        self.java_paths.insert(full.to_string(), found);
        found
    }

    fn java_binding(&mut self, b: &Binding) -> std::result::Result<i64, &'static str> {
        let item = b.item.as_deref().unwrap_or("");
        let full = if b.module.is_empty() { item.to_string() } else { format!("{}.{item}", b.module) };
        self.java_path(&full)
    }

    fn java_resolve(&mut self, path: &str, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        if !matches!(node.kind(), "identifier" | "type_identifier") {
            return Err("keyword");
        }
        let decl = std::iter::successors(node.parent(), |n| n.parent()).find(|n| matches!(n.kind(), "package_declaration" | "import_declaration"));
        if let Some(decl) = decl {
            // a part of an imported name: the class it names so far, else a package
            let part = node.parent().filter(|p| p.kind() == "scoped_identifier" && decl.kind() == "import_declaration");
            let full: String = part.map(|p| text(p, &fi.src).chars().filter(|c| !c.is_whitespace()).collect()).unwrap_or_default();
            return self.java_path(&full).map(|id| (id, "import")).map_err(|_| "package");
        }
        match java_qualifier(node) {
            Some(q) => self.java_qualified(path, fi, q, node, name),
            None => self.java_bare(fi, node, name),
        }
    }

    /// A Java name with nothing in front: a method of a class around or a static import when
    /// called, else a local, a field, a type.
    fn java_bare(&mut self, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        let p = node.parent();
        if p.is_some_and(|p| p.kind() == "method_invocation" && p.child_by_field_name("name").map(|c| c.id()) == Some(node.id())) {
            return self.java_call(fi, node, name);
        }
        let src = fi.src.as_str();
        let typed = node.kind() == "type_identifier" || p.is_some_and(|p| p.kind().ends_with("annotation"));
        if !typed {
            if self.java_local(node, name, src).is_some() {
                return Err("local variable");
            }
            // the innermost class that has the field, an anonymous class's own fields included
            let found = java_classes(fi, node).into_iter().find_map(|c| match c {
                JClass::Named(id) if self.java_has_field(id, name, 0, &mut HashMap::new()) => Some(self.java_field_def(id, name, 0, &mut HashMap::new())),
                JClass::Anon(a) => java_anon_field(fi, a, name).map(Some),
                _ => None,
            });
            if let Some(found) = found {
                let why = if java_field_site(node) { "definition" } else { "field" };
                return found.map(|f| (f, why)).ok_or("field or key");
            }
        }
        match self.java_type(fi, node, name) {
            Ok(id) => Ok((id, "type")),
            // the head of `com.acme.X`: a package
            Err(_) if !typed && p.is_some_and(|p| matches!(p.kind(), "field_access" | "scoped_identifier")) => Err("package"),
            Err(e) => Err(e),
        }
    }

    /// `m(..)` with no receiver: a method of a class around (inherited ones included), else a
    /// statically imported one.
    fn java_call(&mut self, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        let arity = node.parent().and_then(|p| p.child_by_field_name("arguments")).map(|a| a.named_child_count());
        let (mut outside, mut unknown) = (false, false);
        for c in java_classes(fi, node) {
            let found = match c {
                JClass::Named(id) => {
                    let found = self.member(id, name, 0).or_else(|| self.java_accessor(id, name));
                    if found.is_none() {
                        let (o, u) = self.java_base_state(id, 0, &mut HashMap::new());
                        outside |= o;
                        unknown |= u;
                    }
                    found.ok_or("not found")
                }
                JClass::Anon(a) => self.java_anon_member(fi, a, name),
            };
            match found {
                Ok(m) => return Ok(self.java_overload(m, arity, "member")),
                Err("external") => outside = true,
                Err(_) => {}
            }
        }
        for b in fi.bindings.iter().filter(|b| b.local == name && b.item.is_some()) {
            match self.java_binding(b) {
                Ok(m) => return Ok(self.java_overload(m, arity, "import")),
                Err("external") => return Err("external"),
                Err(_) => unknown = true,
            }
        }
        for b in fi.bindings.iter().filter(|b| b.local == "*" && b.item.is_some()) {
            match self.java_path(&b.module) {
                Ok(c) => {
                    if let Some(m) = self.member(c, name, 0) {
                        return Ok(self.java_overload(m, arity, "import"));
                    }
                }
                Err("external") => outside = true,
                Err(_) => unknown = true,
            }
        }
        Err(if !unknown && (outside || JAVA_OBJECT_METHODS.contains(&name)) { "external" } else { "not found" })
    }

    /// `q.name`, `q.name(..)`, `q::name`, and the `C` of a type `q.C`.
    fn java_qualified(&mut self, path: &str, fi: &FileInfo, q: Node, node: Node, name: &str) -> Resolution {
        let p = node.parent().map(|p| p.kind()).unwrap_or("");
        let call = matches!(p, "method_invocation" | "method_reference");
        let arity = node.parent().filter(|_| p == "method_invocation").and_then(|p| p.child_by_field_name("arguments")).map(|a| a.named_child_count());
        let around = java_classes(fi, node).first().copied();
        if q.kind() == "super" {
            let Some(JClass::Named(c)) = around else { return Err("receiver") };
            let mut outside = JAVA_OBJECT_METHODS.contains(&name);
            for b in self.java_bases(c).iter() {
                match *b {
                    Ok(bid) => match self.java_member(bid, name) {
                        Ok(m) => return Ok(self.java_overload(m, arity, "super")),
                        Err("external") => outside = true,
                        Err(_) => {}
                    },
                    Err("external") => outside = true,
                    Err(_) => {}
                }
            }
            return Err(if outside { "external" } else { "not found" });
        }
        if let (Some(JClass::Anon(a)), "this") = (around, q.kind()) {
            return self.java_anon_member(fi, a, name).map(|m| self.java_overload(m, arity, "member"));
        }
        let owner = match self.java_owner(path, fi, q) {
            Ok(t) => t,
            // `a.b.C` with `a.b` a package
            Err(e) if !call => {
                let mut segs = Vec::new();
                if !java_segments(q, &mut segs) {
                    return Err(e);
                }
                let full = segs.iter().map(|s| text(*s, &fi.src)).chain(std::iter::once(name)).collect::<Vec<_>>().join(".");
                return self.java_path(&full).map(|id| (id, "qualified"));
            }
            Err(e) => return Err(e),
        };
        match owner {
            Ty::Local(c) if call => self.java_member(c, name).map(|m| self.java_overload(m, arity, "member")),
            Ty::Local(c) => {
                // a nested type, else a field or an enum constant
                let nested = self.member(c, name, 0).filter(|&m| self.def_kind(m).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str())));
                match nested {
                    Some(m) => Ok((m, "qualified")),
                    None => self.java_field_def(c, name, 0, &mut HashMap::new()).map(|f| (f, "field")).ok_or("field or key"),
                }
            }
            // a class this default-package file cannot see is from the classpath, whatever extends a namesake
            Ty::Foreign(t, _) if call && !self.java_unseen(fi, &t) && (self.extended(&t, name) || self.trait_member(name)) => Err("receiver"),
            Ty::Foreign(..) => Err("external"),
        }
    }

    /// What the qualifier `q` of a Java member access stands for: the type of a value, or a
    /// class (`Util.m()`, `Outer.Inner`, `a.b.C.m()`).
    fn java_owner(&mut self, path: &str, fi: &FileInfo, q: Node) -> std::result::Result<Ty, &'static str> {
        let src = fi.src.as_str();
        if q.kind() == "this" {
            return self.container_at(fi, q.start_position().row as i64 + 1).ok_or("receiver");
        }
        // `Outer.this` is the instance of the enclosing class `Outer`
        if q.kind() == "field_access" && q.child_by_field_name("field").is_some_and(|f| f.kind() == "this") {
            return self.java_owner(path, fi, q.child_by_field_name("object").ok_or("receiver")?);
        }
        let mut segs = Vec::new();
        if !java_segments(q, &mut segs) {
            return self.type_of(path, fi, Lang::Java, q).ok_or("receiver");
        }
        let (head, hname) = (segs[0], text(segs[0], src));
        // a variable hides a type, a type a package
        if head.kind() == "identifier" && (self.java_local(head, hname, src).is_some() || self.java_is_field(fi, head, hname)) {
            return self.type_of(path, fi, Lang::Java, q).ok_or("receiver");
        }
        let cur = match self.java_type(fi, head, hname) {
            Ok(id) => id,
            Err("type parameter") => return Err("receiver"),
            Err("external") if segs.len() == 1 && self.java_package_files(hname).is_some() => return Err("package"),
            // not one of the repository's same-named classes, which a foreign type may otherwise be
            Err("external") if segs.len() == 1 && self.java_unseen(fi, hname) => return Err("external"),
            Err(e) if segs.len() == 1 => return if e == "external" { Ok(Ty::Foreign(hname.to_string(), Vec::new())) } else { Err(e) },
            Err(_) => {
                let full = segs.iter().map(|s| text(*s, src)).collect::<Vec<_>>().join(".");
                if self.java_package_files(&full).is_some() {
                    return Err("package");
                }
                return match self.java_path(&full) {
                    Ok(id) => Ok(Ty::Local(id)),
                    Err("external") => Ok(Ty::Foreign(text(segs[segs.len() - 1], src).to_string(), Vec::new())),
                    Err(e) => Err(e),
                };
            }
        };
        let mut ty = Ty::Local(cur);
        for s in &segs[1..] {
            let Ty::Local(c) = ty else { return Err("receiver") };
            let s = text(*s, src);
            let nested = self.member(c, s, 0).filter(|&m| self.def_kind(m).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str())));
            ty = match nested {
                Some(m) => Ty::Local(m),
                None => self.java_field_ty(c, s, 0, &mut HashMap::new()).ok_or("receiver")?,
            };
        }
        Ok(ty)
    }

    /// A member `name` of class `id`, inherited ones included; why there is none otherwise.
    fn java_member(&mut self, id: i64, name: &str) -> std::result::Result<i64, &'static str> {
        if let Some(m) = self.member(id, name, 0) {
            return Ok(m);
        }
        let decl = self.def_row(id).and_then(|(path, _)| {
            let fi = self.file(&path);
            def_node(&fi, id).map(|n| n.kind())
        });
        if JAVA_OBJECT_METHODS.contains(&name) || (decl == Some("enum_declaration") && JAVA_ENUM_METHODS.contains(&name)) {
            return Err("external");
        }
        if let Some(f) = self.java_accessor(id, name) {
            return Ok(f);
        }
        match self.java_base_state(id, 0, &mut HashMap::new()) {
            (_, true) => Err("not found"),
            (true, false) => Err("external"),
            _ => Err("not found"),
        }
    }

    /// The component of record `id` whose implicit accessor `name()` is: it stands for the accessor.
    fn java_accessor(&mut self, id: i64, name: &str) -> Option<i64> {
        let (path, _) = self.def_row(id)?;
        let record = def_node(&self.file(&path), id).is_some_and(|n| n.kind() == "record_declaration");
        self.declared_field(id, name).filter(|_| record)
    }

    /// The field or constant `name` of class `id`, inherited ones included.
    fn java_field_def(&mut self, id: i64, name: &str, depth: usize, seen: &mut HashMap<i64, usize>) -> Option<i64> {
        if seen_at(seen, id, depth) {
            return None;
        }
        if let Some(f) = self.declared_field(id, name) {
            return Some(f);
        }
        if depth >= 8 {
            return None;
        }
        let bases = self.java_bases(id);
        bases.iter().find_map(|b| b.ok().filter(|&bid| bid != id).and_then(|bid| self.java_field_def(bid, name, depth + 1, seen)))
    }

    /// Whether class `id` extends or implements a type from outside the repository, and whether
    /// one of the types it extends could not be found, at any depth.
    fn java_base_state(&mut self, id: i64, depth: usize, seen: &mut HashMap<i64, usize>) -> (bool, bool) {
        if seen_at(seen, id, depth) {
            return (false, false);
        }
        let (mut outside, mut unknown) = (false, false);
        for b in self.java_bases(id).iter() {
            match *b {
                Ok(bid) if bid != id && depth < 8 => {
                    let (o, u) = self.java_base_state(bid, depth + 1, seen);
                    outside |= o;
                    unknown |= u;
                }
                Ok(_) => {}
                Err("external") => outside = true,
                Err(_) => unknown = true,
            }
        }
        (outside, unknown)
    }

    /// The types class `id` extends and implements, each resolved.
    fn java_bases(&mut self, id: i64) -> Rc<Vec<std::result::Result<i64, &'static str>>> {
        if let Some(b) = self.java_bases.get(&id) {
            return Rc::clone(b);
        }
        // a class that comes round again through its own bases has none the second time
        self.java_bases.insert(id, Rc::new(Vec::new()));
        let mut out = Vec::new();
        if let Some((path, _)) = self.def_row(id) {
            let fi = self.file(&path);
            if let Some(node) = def_node(&fi, id) {
                let mut types: Vec<Node> = node.child_by_field_name("superclass").and_then(|s| s.named_child(0)).into_iter().collect();
                let mut cur = node.walk();
                for c in node.children(&mut cur).filter(|c| matches!(c.kind(), "super_interfaces" | "extends_interfaces")) {
                    let mut c2 = c.walk();
                    for list in c.named_children(&mut c2) {
                        let mut c3 = list.walk();
                        types.extend(list.named_children(&mut c3));
                    }
                }
                for t in types {
                    out.push(self.java_type_node(&fi, t));
                }
            }
        }
        let out = Rc::new(out);
        self.java_bases.insert(id, Rc::clone(&out));
        out
    }

    /// A method of an anonymous class body, else a member of the type it extends.
    fn java_anon_member(&mut self, fi: &FileInfo, anon: Node, name: &str) -> std::result::Result<i64, &'static str> {
        let mut cur = anon.walk();
        let Some(body) = anon.named_children(&mut cur).find(|c| c.kind() == "class_body") else { return Err("not found") };
        let (s, e) = (body.start_position().row as i64 + 1, body.end_position().row as i64 + 1);
        if let Some(d) = fi.defs.iter().find(|d| d.name == name && d.kind == "method" && s <= d.start && d.end <= e) {
            return Ok(d.id);
        }
        let t = anon.child_by_field_name("type").ok_or("not found")?;
        let base = self.java_type_node(fi, t)?;
        self.java_member(base, name)
    }

    /// One of the methods that share the qualified name of method `id` (overloads): the one the
    /// number of arguments picks, else the first, marked "overload" (it stands for them all).
    fn java_overload(&mut self, id: i64, arity: Option<usize>, why: &'static str) -> (i64, &'static str) {
        let all = self.overloads(id);
        if all.len() < 2 {
            return (id, why);
        }
        let Some((path, _)) = self.def_row(id) else { return (id, why) };
        let fi = self.file(&path);
        if let Some(n) = arity {
            let fits: Vec<i64> = all.iter().copied().filter(|&o| java_params(&fi, o).is_some_and(|(k, varargs)| k == n || (varargs && n + 1 >= k))).collect();
            if let [one] = fits[..] {
                return (one, why);
            }
        }
        (all[0], "overload")
    }

    /// A type named `name` where `at` is: a type parameter, a class around or nested in one
    /// (inherited ones included), a class of the file, an imported one, one of the package, one
    /// of a package imported with `*`.
    fn java_type(&mut self, fi: &FileInfo, at: Node, name: &str) -> std::result::Result<i64, &'static str> {
        if self.type_parameter(at, name, &fi.src) {
            return Err("type parameter");
        }
        let container = |s: &Self, id: i64| s.def_kind(id).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str()));
        for c in java_classes(fi, at) {
            let JClass::Named(id) = c else { continue };
            if fi.defs.iter().any(|d| d.id == id && d.name == name) {
                return Ok(id);
            }
            if let Some(m) = self.member(id, name, 0).filter(|&m| container(self, m)) {
                return Ok(m);
            }
        }
        if let Some(d) = top_level(fi, name).filter(|d| CONTAINER_KINDS.contains(&d.kind.as_str())) {
            return Ok(d.id);
        }
        if let Some(b) = fi.bindings.iter().find(|b| b.local == name && b.item.is_some()) {
            return self.java_binding(b);
        }
        let pkg = java_package(&fi.src);
        if let Some(mut files) = self.java_package_files(&pkg) {
            // the default package is one per directory, not one for the whole repository
            if pkg.is_empty() {
                if let Some(dir) = fi.defs.iter().find_map(|d| self.def_row(d.id)).map(|(p, _)| dir_of(&p).to_string()) {
                    files.retain(|f| dir_of(f) == dir);
                }
            }
            if let Some(id) = self.java_top(&files, name) {
                return Ok(id);
            }
        }
        for b in fi.bindings.iter().filter(|b| b.local == "*") {
            let found = match self.java_package_files(&b.module) {
                Some(files) => self.java_top(&files, name),
                None => self.java_path(&b.module).ok().and_then(|c| self.member(c, name, 0)).filter(|&m| container(self, m)),
            };
            if let Some(id) = found {
                return Ok(id);
            }
        }
        // `java.lang` and the packages a `*` imports from outside
        Err(if self.defined_anywhere(name) && !self.java_unseen(fi, name) { "not found" } else { "external" })
    }

    /// Whether `name`, defined somewhere in the repository, is out of sight of `fi`: a file of the
    /// default package with no `*` import sees only its own directory, so such a class is from the
    /// classpath.
    fn java_unseen(&mut self, fi: &FileInfo, name: &str) -> bool {
        java_package(&fi.src).is_empty() && !fi.bindings.iter().any(|b| b.local == "*") && self.defined_anywhere(name)
    }

    /// The class a Java type expression names (`T`, `a.b.T`, `Outer.Inner<X>`).
    fn java_type_node(&mut self, fi: &FileInfo, t: Node) -> std::result::Result<i64, &'static str> {
        let mut segs = Vec::new();
        if !java_segments(t, &mut segs) {
            return Err("not found");
        }
        let (head, hname) = (segs[0], text(segs[0], &fi.src));
        let mut cur = match self.java_type(fi, head, hname) {
            Ok(id) => id,
            Err(e) if segs.len() == 1 => return Err(e),
            Err(_) => return self.java_path(&segs.iter().map(|s| text(*s, &fi.src)).collect::<Vec<_>>().join(".")),
        };
        for s in &segs[1..] {
            cur = self.member(cur, text(*s, &fi.src), 0).ok_or("not found")?;
        }
        Ok(cur)
    }

    /// The type of the value a method invocation `e` returns.
    fn java_call_ty(&mut self, path: &str, fi: &FileInfo, e: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let leaf = e.child_by_field_name("name")?;
        let found = match java_qualifier(leaf) {
            Some(q) => self.java_qualified(path, fi, q, leaf, text(leaf, src)),
            None => self.java_call(fi, leaf, text(leaf, src)),
        };
        self.def_ty(found.ok()?.0)
    }

    /// The type of a Java variable: a local's declaration, else a field of a class around.
    fn java_variable_ty(&mut self, path: &str, fi: &FileInfo, e: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let name = text(e, src);
        if let Some(b) = self.java_local(e, name, src) {
            let (ty, value) = java_declared(b, src);
            return match ty {
                Some(t) => self.built_ty(path, fi, Lang::Java, b, value, e).or_else(|| self.ty_of_type(path, fi, Lang::Java, t)),
                None => self.type_of(path, fi, Lang::Java, value?),
            };
        }
        for c in java_classes(fi, e) {
            if let JClass::Named(id) = c {
                if self.java_has_field(id, name, 0, &mut HashMap::new()) {
                    return self.java_field_ty(id, name, 0, &mut HashMap::new());
                }
            }
        }
        None
    }

    /// Whether `name` is a field of a class around `node`, inherited ones included.
    fn java_is_field(&mut self, fi: &FileInfo, node: Node, name: &str) -> bool {
        java_classes(fi, node).into_iter().any(|c| match c {
            JClass::Named(id) => self.java_has_field(id, name, 0, &mut HashMap::new()),
            JClass::Anon(a) => java_anon_field(fi, a, name).is_some(),
        })
    }

    fn java_has_field(&mut self, id: i64, name: &str, depth: usize, seen: &mut HashMap<i64, usize>) -> bool {
        if seen_at(seen, id, depth) {
            return false;
        }
        if self.java_field_names(id).contains(name) {
            return true;
        }
        depth < 8 && self.java_bases(id).iter().any(|b| matches!(*b, Ok(bid) if bid != id && self.java_has_field(bid, name, depth + 1, seen)))
    }

    /// The fields class `id` declares: field declarations, interface constants, enum constants
    /// and record components.
    fn java_field_names(&mut self, id: i64) -> Rc<HashSet<String>> {
        if let Some(f) = self.java_fields.get(&id) {
            return Rc::clone(f);
        }
        let mut names = HashSet::new();
        if let Some((path, _)) = self.def_row(id) {
            let fi = self.file(&path);
            if let Some(node) = def_node(&fi, id) {
                for (n, _) in java_field_decls(node) {
                    names.insert(text(n, &fi.src).to_string());
                }
            }
        }
        let names = Rc::new(names);
        self.java_fields.insert(id, Rc::clone(&names));
        names
    }

    /// The declared type of the field `name` of class `id`, inherited ones included; an enum
    /// constant has the type of its enum.
    fn java_field_ty(&mut self, id: i64, name: &str, depth: usize, seen: &mut HashMap<i64, usize>) -> Option<Ty> {
        if seen_at(seen, id, depth) {
            return None;
        }
        let (path, _) = self.def_row(id)?;
        let fi = self.file(&path);
        let node = def_node(&fi, id)?;
        if let Some((_, ty)) = java_field_decls(node).into_iter().find(|(n, _)| text(*n, &fi.src) == name) {
            return match ty {
                Some(t) => self.ty_of_type(&path, &fi, Lang::Java, t),
                None => Some(Ty::Local(id)),
            };
        }
        if depth >= 8 {
            return None;
        }
        for b in self.java_bases(id).iter() {
            if let Ok(bid) = *b {
                if let Some(t) = self.java_field_ty(bid, name, depth + 1, seen).filter(|_| bid != id) {
                    return Some(t);
                }
            }
        }
        None
    }
}

/// The fields a Java class declaration holds directly: (name, declared type; None for an enum constant).
fn java_field_decls(class: Node) -> Vec<(Node, Option<Node>)> {
    let mut out = Vec::new();
    if let Some(params) = class.child_by_field_name("parameters").filter(|_| class.kind() == "record_declaration") {
        let mut cur = params.walk();
        for p in params.named_children(&mut cur) {
            out.extend(p.child_by_field_name("name").map(|n| (n, p.child_by_field_name("type"))));
        }
    }
    let Some(body) = class.child_by_field_name("body") else { return out };
    let mut members: Vec<Node> = Vec::new();
    let mut cur = body.walk();
    for m in body.named_children(&mut cur) {
        if m.kind() == "enum_body_declarations" {
            let mut c2 = m.walk();
            members.extend(m.named_children(&mut c2));
        } else {
            members.push(m);
        }
    }
    for m in members {
        match m.kind() {
            "field_declaration" | "constant_declaration" => {
                let mut c2 = m.walk();
                for d in m.children_by_field_name("declarator", &mut c2) {
                    out.extend(d.child_by_field_name("name").map(|n| (n, m.child_by_field_name("type"))));
                }
            }
            "enum_constant" => out.extend(m.child_by_field_name("name").map(|n| (n, None))),
            _ => {}
        }
    }
    out
}

/// The number of parameters of a Java method or constructor, and whether its last is `...`.
fn java_params(fi: &FileInfo, id: i64) -> Option<(usize, bool)> {
    let params = def_node(fi, id)?.child_by_field_name("parameters")?;
    let mut cur = params.walk();
    let kinds: Vec<&str> = params.named_children(&mut cur).map(|p| p.kind()).filter(|k| *k != "receiver_parameter" && !k.ends_with("comment")).collect();
    Some((kinds.len(), kinds.last() == Some(&"spread_parameter")))
}
