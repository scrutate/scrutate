//! C#: a type is looked up in the classes around, its file's namespace and the namespaces around
//! it (block or file-scoped), then the `using` namespaces and aliases; members through the
//! classes around, typed locals, fields and properties, base types and partial classes.

use super::*;

pub(super) struct CSharp;

const CS_CLASS_NODES: &[&str] = &["class_declaration", "struct_declaration", "interface_declaration", "record_declaration", "enum_declaration"];
/// Methods every C# value has from `object`.
const CS_OBJECT_METHODS: &[&str] = &["ToString", "Equals", "GetHashCode", "GetType", "MemberwiseClone", "ReferenceEquals", "Finalize"];
/// The nodes a local declared in them does not leave.
const CS_SCOPES: &[&str] = &[
    "block",
    "for_statement",
    "foreach_statement",
    "using_statement",
    "fixed_statement",
    "catch_clause",
    "switch_section",
    "lambda_expression",
    "anonymous_method_expression",
    "local_function_statement",
    "method_declaration",
    "constructor_declaration",
    "accessor_declaration",
    "operator_declaration",
    "conversion_operator_declaration",
];

static CS_FILE_NAMESPACE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"(?m)^\s*namespace\s+([A-Za-z_][\w.]*)\s*;").unwrap());

impl Rules for CSharp {
    const GRAMMARS: &'static [&'static str] = &["csharp"];

    fn bindings(n: Node, _path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
        cs_bindings(n, src, out)
    }

    fn occurrence(r: &mut Resolver, _path: &str, fi: &FileInfo, node: Node, name: &str, _line: i64) -> Option<Resolution> {
        Some(r.cs_resolve(fi, node, name))
    }

    fn inherited(r: &mut Resolver, id: i64, name: &str, depth: usize, _path: &str, _qualname: &str) -> Option<i64> {
        r.inherited_through(id, name, depth, |r, id| r.cs_bases(id).iter().filter_map(|b| b.ok()).collect())
    }

    fn bases(r: &mut Resolver, id: i64, _path: &str, _fi: &FileInfo, _d: &Def, _lang: Lang) -> Vec<std::result::Result<i64, &'static str>> {
        r.cs_bases(id).iter().copied().filter(|&b| b != Ok(id)).collect()
    }

    fn module_files(r: &mut Resolver, _from: &str, module: &str) -> Option<Vec<String>> {
        r.cs_namespaces().get(module).cloned()
    }

    // a `using` names a namespace, or a type with `using static` and an alias
    fn import_files(r: &mut Resolver, from: &str, _fi: &FileInfo, _line: i64, module: &str) -> Option<Vec<String>> {
        let found = Self::module_files(r, from, module)
            .or_else(|| r.cs_dotted(module).ok().and_then(|id| r.def_row(id)).map(|(path, _)| vec![path]))
            .unwrap_or_default();
        Some(found)
    }
}

/// `using N;` binds the types of namespace `N` (`*`), `using static T;` the members of `T`
/// (`*` with an item), `using A = N.T;` the name `A`.
fn cs_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    if n.kind() != "using_directive" {
        // usings come at file level or at the top of a namespace
        return !matches!(n.kind(), "compilation_unit" | "namespace_declaration" | "declaration_list");
    }
    let mut cur = n.walk();
    let alias = n.child_by_field_name("name");
    let Some(target) = n.named_children(&mut cur).filter(|c| Some(c.id()) != alias.map(|a| a.id())).find(|c| !c.kind().ends_with("comment")) else {
        return true;
    };
    let module: String = text(target, src).chars().filter(|c| !c.is_whitespace()).collect();
    let is_static = text(n, src).trim_start_matches("global").trim_start().trim_start_matches("using").trim_start().starts_with("static ");
    match alias {
        Some(a) => out.push(Binding { local: text(a, src).into(), module, item: None, sites: vec![] }),
        None => out.push(Binding { local: "*".into(), module, item: is_static.then(|| "*".to_string()), sites: vec![] }),
    }
    true
}

fn is_field(p: Node, f: &str, n: Node) -> bool {
    p.child_by_field_name(f).is_some_and(|c| c.id() == n.id())
}

/// The namespace around `node`: the file-scoped one, then the blocks around it, outermost first.
fn cs_namespace(src: &str, node: Node) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut a = node.parent();
    while let Some(n) = a {
        if n.kind() == "namespace_declaration" {
            parts.extend(n.child_by_field_name("name").map(|x| text(x, src).chars().filter(|c| !c.is_whitespace()).collect()));
        }
        if n.kind() == "compilation_unit" {
            let mut cur = n.walk();
            let file = n.named_children(&mut cur).find(|c| c.kind() == "file_scoped_namespace_declaration");
            parts.extend(file.and_then(|f| f.child_by_field_name("name")).map(|x| text(x, src).chars().filter(|c| !c.is_whitespace()).collect()));
        }
        a = n.parent();
    }
    parts.reverse();
    parts.join(".")
}

/// Whether `pos` stands where a type is written: a declaration's type, a base, a type argument.
fn cs_type_position(pos: Node) -> bool {
    let Some(p) = pos.parent() else { return false };
    is_field(p, "type", pos)
        || is_field(p, "returns", pos)
        || matches!(
            p.kind(),
            "base_list" | "type_argument_list" | "nullable_type" | "array_type" | "type_parameter_constraint" | "typeof_expression" | "attribute"
        )
}

impl Resolver<'_> {
    fn cs_resolve(&mut self, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        if node.kind() != "identifier" {
            return Err("keyword");
        }
        let src = fi.src.as_str();
        // a part of a namespace or of the name a `using` imports
        for a in std::iter::successors(node.parent(), |n| n.parent()) {
            match a.kind() {
                "namespace_declaration" | "file_scoped_namespace_declaration"
                    if a.child_by_field_name("name").is_some_and(|x| x.byte_range().contains(&node.start_byte())) =>
                {
                    return Err("namespace");
                }
                "using_directive" => {
                    let upto = node.parent().filter(|p| p.kind() == "qualified_name" && is_field(*p, "name", node)).unwrap_or(node);
                    let full: String = text(upto, src).chars().filter(|c| !c.is_whitespace()).collect();
                    return self.cs_dotted(&full).map(|id| (id, "import")).map_err(|_| "namespace");
                }
                _ => {}
            }
        }
        // `List<T>` and `M<T>()` stand where their name does
        let pos = node.parent().filter(|p| p.kind() == "generic_name").unwrap_or(node);
        let Some(p) = pos.parent() else { return Err("not found") };
        match p.kind() {
            "member_access_expression" if is_field(p, "name", pos) => {
                let q = p.child_by_field_name("expression").ok_or("receiver")?;
                self.cs_qualified(fi, q, pos, name)
            }
            "qualified_name" if is_field(p, "name", pos) => {
                let q = p.child_by_field_name("qualifier").ok_or("receiver")?;
                self.cs_qualified(fi, q, pos, name)
            }
            // `x?.M()`: the type of `x` is not followed
            "member_binding_expression" => Err("receiver"),
            _ => self.cs_bare(fi, node, pos, name),
        }
    }

    /// A name with nothing in front: a local, a member of a class around, a type, a member a
    /// `using static` brings in.
    fn cs_bare(&mut self, fi: &FileInfo, node: Node, pos: Node, name: &str) -> Resolution {
        let src = fi.src.as_str();
        let typed = cs_type_position(pos);
        if !typed {
            if self.cs_local(node, name, src).is_some() {
                return Err("local variable");
            }
            for c in self.cs_classes(fi, node) {
                if let Some(m) = self.cs_member(c, name) {
                    return Ok((m, "member"));
                }
                if self.cs_has_field(c, name) {
                    return Err("field or key");
                }
            }
        }
        match self.cs_type(fi, node, name) {
            Ok(id) => return Ok((id, "type")),
            Err("external") if self.cs_heads_chain(pos) && self.cs_is_namespace(name) => return Err("namespace"),
            Err(_) => {}
        }
        let statics: Vec<String> = fi.bindings.iter().filter(|b| b.local == "*" && b.item.is_some()).map(|b| b.module.clone()).collect();
        for m in statics {
            if let Ok(c) = self.cs_dotted(&m) {
                if let Some(found) = self.cs_member(c, name) {
                    return Ok((found, "import"));
                }
            }
        }
        Err(if self.cs_defined(name) { "not found" } else { "external" })
    }

    /// `q.name`: a type of a namespace (`A.B.C`), else a member of the type `q` stands for.
    fn cs_qualified(&mut self, fi: &FileInfo, q: Node, pos: Node, name: &str) -> Resolution {
        if q.kind() == "base" {
            let mut outside = CS_OBJECT_METHODS.contains(&name);
            for c in self.cs_classes(fi, q).into_iter().take(1) {
                for b in self.cs_bases(c).iter() {
                    match *b {
                        Ok(bid) => {
                            if let Some(m) = self.cs_member(bid, name) {
                                return Ok((m, "super"));
                            }
                        }
                        Err(_) => outside = true,
                    }
                }
            }
            return Err(if outside { "external" } else { "not found" });
        }
        let call = pos.parent().and_then(|p| p.parent()).is_some_and(|c| c.kind() == "invocation_expression");
        if !call && cs_dotted_name(q) {
            let full = format!("{}.{name}", cs_full(fi, q));
            if let Ok(id) = self.cs_dotted(&full) {
                return Ok((id, "qualified"));
            }
        }
        match self.cs_owner(fi, q)? {
            Ty::Local(c) => match self.cs_member(c, name) {
                Some(m) => Ok((m, "member")),
                None if self.cs_has_field(c, name) => Err("field or key"),
                None if CS_OBJECT_METHODS.contains(&name) => Err("external"),
                None if self.cs_bases(c).iter().any(|b| b.is_err()) => Err("external"),
                None => Err("not found"),
            },
            Ty::Foreign(..) => Err("external"),
        }
    }

    /// What the qualifier `q` stands for: the type of a value, or a type named (`Util.M()`).
    fn cs_owner(&mut self, fi: &FileInfo, q: Node) -> std::result::Result<Ty, &'static str> {
        let src = fi.src.as_str();
        match q.kind() {
            "this" => self.cs_classes(fi, q).first().map(|&c| Ty::Local(c)).ok_or("receiver"),
            "predefined_type" => Ok(Ty::Foreign(text(q, src).to_string(), Vec::new())),
            "identifier" | "generic_name" => {
                let leaf = if q.kind() == "generic_name" { q.named_child(0).ok_or("receiver")? } else { q };
                let name = text(leaf, src);
                // a variable hides a type
                if let Some(t) = self.cs_value_ty(fi, leaf) {
                    return Ok(t);
                }
                match self.cs_type(fi, leaf, name) {
                    Ok(id) => Ok(Ty::Local(id)),
                    Err("external") => Ok(Ty::Foreign(name.to_string(), Vec::new())),
                    Err(_) => Err("receiver"),
                }
            }
            _ if cs_dotted_name(q) => {
                // a variable at the head is a value, else the chain names a type
                let mut head = q;
                while let Some(h) = head.child_by_field_name("expression").or_else(|| head.child_by_field_name("qualifier")) {
                    head = h;
                }
                if self.cs_value_ty(fi, head).is_some() {
                    return self.cs_expr_ty(fi, q).ok_or("receiver");
                }
                match self.cs_dotted(&cs_full(fi, q)) {
                    Ok(id) => Ok(Ty::Local(id)),
                    Err(_) => self.cs_expr_ty(fi, q).ok_or("receiver"),
                }
            }
            _ => self.cs_expr_ty(fi, q).ok_or("receiver"),
        }
    }

    /// The type of the value expression `e`, when declarations say it.
    fn cs_expr_ty(&mut self, fi: &FileInfo, e: Node) -> Option<Ty> {
        self.nested(|s| s.cs_expr_ty_inner(fi, e))
    }

    fn cs_expr_ty_inner(&mut self, fi: &FileInfo, e: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        match e.kind() {
            "identifier" => self.cs_value_ty(fi, e),
            "this" => self.cs_classes(fi, e).first().map(|&c| Ty::Local(c)),
            "parenthesized_expression" => self.cs_expr_ty(fi, e.named_child(0)?),
            "object_creation_expression" | "cast_expression" => self.cs_type_ty(fi, e.child_by_field_name("type")?),
            "invocation_expression" => {
                let f = e.child_by_field_name("function")?;
                let leaf = match f.kind() {
                    "member_access_expression" => f.child_by_field_name("name")?,
                    _ => f,
                };
                let leaf = if leaf.kind() == "generic_name" { leaf.named_child(0)? } else { leaf };
                let (id, _) = self.cs_resolve(fi, leaf, text(leaf, src)).ok()?;
                self.cs_def_ty(id)
            }
            "member_access_expression" => {
                let owner = self.cs_owner(fi, e.child_by_field_name("expression")?).ok()?;
                let Ty::Local(c) = owner else { return None };
                let name = text(e.child_by_field_name("name")?, src).to_string();
                match self.cs_member(c, &name) {
                    Some(m) => self.cs_def_ty(m),
                    None => self.cs_field_ty(c, &name),
                }
            }
            _ => None,
        }
    }

    /// The type of the variable `e` names: a local's declaration, else a field or property of a
    /// class around. None when `e` names neither.
    fn cs_value_ty(&mut self, fi: &FileInfo, e: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let name = text(e, src).to_string();
        if let Some(b) = self.cs_local(e, &name, src) {
            let p = b.parent()?;
            return match p.kind() {
                "variable_declarator" => {
                    let t = p.parent().and_then(|d| d.child_by_field_name("type")).filter(|t| t.kind() != "implicit_type");
                    match t {
                        Some(t) => self.cs_type_ty(fi, t),
                        None => {
                            let mut cur = p.walk();
                            let value = p.named_children(&mut cur).find(|c| c.id() != b.id())?;
                            self.cs_expr_ty(fi, value)
                        }
                    }
                }
                _ => self.cs_type_ty(fi, p.child_by_field_name("type").filter(|t| t.kind() != "implicit_type")?),
            };
        }
        for c in self.cs_classes(fi, e) {
            if let Some(t) = self.cs_field_ty(c, &name) {
                return Some(t);
            }
            if let Some(m) = self.cs_member(c, &name) {
                return self.cs_def_ty(m);
            }
        }
        None
    }

    /// What a value of definition `id` is: an instance of a type, what a method returns, the
    /// type of a property.
    fn cs_def_ty(&mut self, id: i64) -> Option<Ty> {
        let kind = self.def_kind(id)?;
        if CONTAINER_KINDS.contains(&kind.as_str()) {
            return Some(Ty::Local(id));
        }
        let (path, _) = self.def_row(id)?;
        let fi = self.file(&path);
        let node = def_node(&fi, id)?;
        let t = node.child_by_field_name("returns").or_else(|| node.child_by_field_name("type"))?;
        self.cs_type_ty(&fi, t)
    }

    /// The declared type of the field `name` of class `id`, inherited ones included.
    fn cs_field_ty(&mut self, id: i64, name: &str) -> Option<Ty> {
        let (path, _) = self.def_row(id)?;
        let fi = self.file(&path);
        let node = def_node(&fi, id)?;
        match cs_fields(node).into_iter().find(|(n, _)| text(*n, &fi.src) == name) {
            Some((_, Some(t))) => self.cs_type_ty(&fi, t),
            Some((_, None)) => Some(Ty::Local(id)),
            None => {
                self.cs_bases(id).iter().filter_map(|b| b.ok()).filter(|&b| b != id).collect::<Vec<_>>().into_iter().find_map(|b| self.cs_field_ty(b, name))
            }
        }
    }

    /// The type a type expression `t` of `fi` names.
    fn cs_type_ty(&mut self, fi: &FileInfo, t: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        match t.kind() {
            "identifier" => match self.cs_type(fi, t, text(t, src)) {
                Ok(id) => Some(Ty::Local(id)),
                Err("external") => Some(Ty::Foreign(text(t, src).to_string(), Vec::new())),
                Err(_) => None,
            },
            "generic_name" => self.cs_type_ty(fi, t.named_child(0)?),
            "nullable_type" => self.cs_type_ty(fi, t.child_by_field_name("type")?),
            "qualified_name" => {
                let full = cs_full(fi, t);
                match self.cs_dotted(&strip_generics(&full)) {
                    Ok(id) => Some(Ty::Local(id)),
                    Err("external") => Some(Ty::Foreign(full, Vec::new())),
                    Err(_) => None,
                }
            }
            "predefined_type" | "array_type" | "tuple_type" | "pointer_type" => Some(Ty::Foreign(text(t, src).to_string(), Vec::new())),
            _ => None,
        }
    }

    /// The nearest parameter or local named `name` that `node` sees.
    fn cs_local<'t>(&mut self, node: Node<'t>, name: &str, src: &str) -> Option<Node<'t>> {
        let at = node.start_byte();
        let top = std::iter::successors(node.parent(), |n| n.parent()).filter(|n| CS_SCOPES.contains(&n.kind())).last()?;
        let mut best: Option<Node> = None;
        let mut stack = vec![top];
        while let Some(n) = stack.pop() {
            let binds = match n.kind() {
                "identifier" => text(n, src) == name && cs_binder(n),
                "implicit_parameter" => text(n, src) == name,
                _ => false,
            };
            if binds && n.start_byte() <= at {
                let scope = cs_binder_scope(n);
                if scope.is_some_and(|s| s.byte_range().contains(&at)) && best.is_none_or(|b| b.start_byte() < n.start_byte()) {
                    best = Some(n);
                }
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        best
    }

    /// The classes around `node`, innermost first.
    fn cs_classes(&self, fi: &FileInfo, node: Node) -> Vec<i64> {
        let mut out = Vec::new();
        for n in std::iter::successors(node.parent(), |n| n.parent()).filter(|n| CS_CLASS_NODES.contains(&n.kind())) {
            let Some(nm) = n.child_by_field_name("name") else { continue };
            let (nm, line) = (text(nm, &fi.src), nm.start_position().row as i64 + 1);
            let d = fi.defs.iter().filter(|d| d.name == nm && d.start <= line && line <= d.end && CONTAINER_KINDS.contains(&d.kind.as_str()));
            out.extend(d.min_by_key(|d| d.end - d.start).map(|d| d.id));
        }
        out
    }

    /// A member `name` of type `id`: its own, those of the other parts of a partial class, those
    /// of its bases.
    fn cs_member(&mut self, id: i64, name: &str) -> Option<i64> {
        // every part before the bases, which `cs_bases` gathers from all parts
        for part in std::iter::once(id).chain(self.cs_parts(id)) {
            if let Some(m) = self.declared_member(part, name) {
                return Some(m);
            }
        }
        self.member(id, name, 0)
    }

    /// The other definitions of a partial type: types of its name and namespace, top-level or in
    /// the same class.
    fn cs_parts(&mut self, id: i64) -> Vec<i64> {
        let Some(row) = self.ix.symbol(id).ok().flatten() else { return Vec::new() };
        let all = self.cs_types_named(&row.name);
        let Some((_, ns, outer)) = all.iter().find(|t| t.0 == id).cloned() else { return Vec::new() };
        all.iter().filter(|t| t.0 != id && t.1 == ns && t.2 == outer).map(|t| t.0).collect()
    }

    fn cs_has_field(&mut self, id: i64, name: &str) -> bool {
        let Some((path, _)) = self.def_row(id) else { return false };
        let fi = self.file(&path);
        def_node(&fi, id).is_some_and(|n| cs_fields(n).iter().any(|(f, _)| text(*f, &fi.src) == name))
    }

    /// The types class `id` extends and implements, each resolved.
    fn cs_bases(&mut self, id: i64) -> Rc<Vec<std::result::Result<i64, &'static str>>> {
        if let Some(b) = self.cs_bases.get(&id) {
            return Rc::clone(b);
        }
        self.cs_bases.insert(id, Rc::new(Vec::new()));
        let mut out = Vec::new();
        for part in std::iter::once(id).chain(self.cs_parts(id)) {
            let Some((path, _)) = self.def_row(part) else { continue };
            let fi = self.file(&path);
            let Some(node) = def_node(&fi, part) else { continue };
            let mut cur = node.walk();
            let Some(list) = node.named_children(&mut cur).find(|c| c.kind() == "base_list") else { continue };
            let mut c2 = list.walk();
            let types: Vec<Node> =
                list.named_children(&mut c2).map(|t| if t.kind() == "primary_constructor_base_type" { t.named_child(0).unwrap_or(t) } else { t }).collect();
            for t in types {
                out.push(match self.cs_type_ty(&fi, t) {
                    Some(Ty::Local(b)) => Ok(b),
                    Some(Ty::Foreign(..)) => Err("external"),
                    None => Err("not found"),
                });
            }
        }
        let out = Rc::new(out);
        self.cs_bases.insert(id, Rc::clone(&out));
        out
    }

    /// The type `name` seen where `at` is: one nested in a class around, one of the namespace
    /// around or a namespace around that, one an alias names, one of a `using` namespace.
    fn cs_type(&mut self, fi: &FileInfo, at: Node, name: &str) -> std::result::Result<i64, &'static str> {
        let cands = self.cs_types_named(name);
        let classes = self.cs_classes(fi, at);
        for &c in &classes {
            if let Some(&(id, _, _)) = cands.iter().find(|(id, _, outer)| *id == c || *outer == Some(c)) {
                return Ok(id);
            }
        }
        let mut ns = cs_namespace(&fi.src, at);
        loop {
            if let Some(&(id, _, _)) = cands.iter().find(|(_, n, outer)| outer.is_none() && *n == ns) {
                return Ok(id);
            }
            if ns.is_empty() {
                break;
            }
            ns = ns.rsplit_once('.').map_or(String::new(), |(a, _)| a.to_string());
        }
        if let Some(b) = fi.bindings.iter().find(|b| b.local == name) {
            return self.cs_dotted(&b.module.clone());
        }
        for b in fi.bindings.iter().filter(|b| b.local == "*" && b.item.is_none()) {
            if let Some(&(id, _, _)) = cands.iter().find(|(_, n, outer)| outer.is_none() && *n == b.module) {
                return Ok(id);
            }
        }
        Err(if cands.is_empty() { "external" } else { "not found" })
    }

    /// The type a dotted name stands for: a type of a namespace (`A.B.C`), a nested one (`A.B.C.D`).
    fn cs_dotted(&mut self, full: &str) -> std::result::Result<i64, &'static str> {
        let segs: Vec<&str> = full.split('.').collect();
        let mut found = Err("external");
        for k in (0..segs.len()).rev() {
            let ns = segs[..k].join(".");
            let cands = self.cs_types_named(segs[k]);
            if !cands.is_empty() && found.is_err() {
                found = Err("not found");
            }
            let Some(&(mut cur, _, _)) = cands.iter().find(|(_, n, outer)| outer.is_none() && *n == ns) else { continue };
            for s in &segs[k + 1..] {
                match self.declared_member(cur, s).filter(|&m| self.def_kind(m).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str()))) {
                    Some(m) => cur = m,
                    None => return Err("not found"),
                }
            }
            return Ok(cur);
        }
        found
    }

    /// The C# types named `name`: (definition, its namespace, the class it is nested in).
    fn cs_types_named(&mut self, name: &str) -> CsTypes {
        if let Some(t) = self.cs_types.get(name) {
            return Rc::clone(t);
        }
        let mut rows: Vec<Symbol> = self.ix.named(name).unwrap_or_default();
        rows.retain(|r| (CONTAINER_KINDS.contains(&r.kind.as_str()) || r.kind == "type") && self.path_lang(&r.path) == Some(Lang::CSharp));
        rows.sort_by(|a, b| (&a.path, a.start).cmp(&(&b.path, b.start)));
        let mut out = Vec::new();
        for r in rows {
            let fi = self.file(&r.path);
            let Some(d) = fi.defs.iter().find(|d| d.id == r.id) else { continue };
            let outer = d.parent.map(|p| &fi.defs[p]).filter(|p| CONTAINER_KINDS.contains(&p.kind.as_str())).map(|p| p.id);
            let ns = def_node(&fi, r.id).map(|n| cs_namespace(&fi.src, n)).unwrap_or_default();
            out.push((r.id, ns, outer));
        }
        let out = Rc::new(out);
        self.cs_types.insert(name.to_string(), Rc::clone(&out));
        out
    }

    /// Namespace -> the C# files that declare it.
    fn cs_namespaces(&mut self) -> &HashMap<String, Vec<String>> {
        if self.cs_namespaces.is_none() {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            let mut files: Vec<String> = self.files.iter().filter(|f| self.path_lang(f) == Some(Lang::CSharp)).cloned().collect();
            files.sort();
            for f in files {
                let stem = f.rsplit('/').next().unwrap_or(&f).rsplit_once('.').map_or(f.as_str(), |(s, _)| s).to_string();
                let rows = self.ix.symbols_in(&f).unwrap_or_default();
                // the module chunk of the file's head is named after the file
                let blocks = rows.into_iter().filter(|r| r.kind == "module" && !(r.start == 1 && r.name == stem && r.qualname == stem)).map(|r| r.qualname);
                let head = super::java::java_head(&self.ix.root, &f);
                let scoped = CS_FILE_NAMESPACE.captures(&head).map(|c| c[1].to_string());
                for ns in blocks.chain(scoped) {
                    let v = m.entry(ns).or_default();
                    if v.last() != Some(&f) {
                        v.push(f.clone());
                    }
                }
            }
            self.cs_namespaces = Some(m);
        }
        self.cs_namespaces.as_ref().unwrap()
    }

    fn cs_is_namespace(&mut self, name: &str) -> bool {
        self.cs_namespaces().keys().any(|ns| ns == name || ns.starts_with(&format!("{name}.")))
    }

    /// Whether `pos` is the head of a dotted chain (`A` of `A.B`).
    fn cs_heads_chain(&self, pos: Node) -> bool {
        pos.parent().is_some_and(|p| is_field(p, "expression", pos) && p.kind() == "member_access_expression" || is_field(p, "qualifier", pos))
    }

    /// Whether a C# file of the repository defines `name`.
    fn cs_defined(&self, name: &str) -> bool {
        self.ix.named(name).map(|rows| rows.iter().any(|r| r.kind != "window" && self.path_lang(&r.path) == Some(Lang::CSharp))).unwrap_or(true)
    }
}

/// The dotted name `n` of `fi` as written, its first part an alias (`using M = A.B;`) replaced.
fn cs_full(fi: &FileInfo, n: Node) -> String {
    let full: String = text(n, &fi.src).chars().filter(|c| !c.is_whitespace()).collect();
    let (head, rest) = full.split_once('.').unwrap_or((&full, ""));
    match fi.bindings.iter().find(|b| b.local == head) {
        Some(b) if rest.is_empty() => b.module.clone(),
        Some(b) => format!("{}.{rest}", b.module),
        None => full,
    }
}

/// Whether `q` is a dotted name of identifiers only (`A.B.C`).
fn cs_dotted_name(q: Node) -> bool {
    match q.kind() {
        "identifier" => true,
        "member_access_expression" => {
            q.child_by_field_name("expression").is_some_and(cs_dotted_name) && q.child_by_field_name("name").is_some_and(|n| n.kind() == "identifier")
        }
        "qualified_name" => {
            q.child_by_field_name("qualifier").is_some_and(cs_dotted_name) && q.child_by_field_name("name").is_some_and(|n| n.kind() == "identifier")
        }
        _ => false,
    }
}

/// Whether the identifier `n` declares a parameter or a local.
fn cs_binder(n: Node) -> bool {
    let Some(p) = n.parent() else { return false };
    let named = is_field(p, "name", n);
    match p.kind() {
        "parameter" | "catch_declaration" | "declaration_expression" | "declaration_pattern" => named,
        "foreach_statement" => is_field(p, "left", n),
        "variable_declarator" => {
            named && p.parent().and_then(|d| d.parent()).is_some_and(|s| s.kind() != "field_declaration" && s.kind() != "event_field_declaration")
        }
        _ => false,
    }
}

/// The code a binder is visible in: the method or lambda of a parameter, else the scope around.
fn cs_binder_scope(b: Node) -> Option<Node> {
    let p = b.parent()?;
    match (b.kind(), p.kind()) {
        ("implicit_parameter", _) => Some(p),
        (_, "parameter") => p.parent()?.parent(),
        _ => std::iter::successors(Some(p), |n| n.parent()).find(|n| CS_SCOPES.contains(&n.kind())),
    }
}

/// The fields a C# type declares directly: (name, declared type; None for an enum member).
fn cs_fields(class: Node) -> Vec<(Node, Option<Node>)> {
    let mut out = Vec::new();
    let Some(body) = class.child_by_field_name("body") else { return out };
    let mut cur = body.walk();
    for m in body.named_children(&mut cur) {
        match m.kind() {
            "field_declaration" | "event_field_declaration" => {
                let mut c2 = m.walk();
                let Some(decl) = m.named_children(&mut c2).find(|c| c.kind() == "variable_declaration") else { continue };
                let ty = decl.child_by_field_name("type");
                let mut c3 = decl.walk();
                for v in decl.named_children(&mut c3).filter(|v| v.kind() == "variable_declarator") {
                    out.extend(v.child_by_field_name("name").map(|n| (n, ty)));
                }
            }
            "enum_member_declaration" => out.extend(m.child_by_field_name("name").map(|n| (n, None))),
            _ => {}
        }
    }
    out
}
