//! C and C++: an `#include` names a repository file (beside the including file, else by a path
//! suffix), names resolve through the files a file includes, C++ namespaces and `::`, members of
//! the class around and its bases, and a declaration stands for the definition it declares.

use super::*;

pub(super) struct C;

/// Of the files that end with an include's path, the ones read are those that share the most
/// directories with the including file; past this many includes the walk stops.
const MAX_INCLUDED: usize = 64;

impl Rules for C {
    const GRAMMARS: &'static [&'static str] = &["c", "cpp"];
    const PATH_SEP: &'static str = "::";

    fn bindings(n: Node, _path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
        c_bindings(n, src, out)
    }

    fn occurrence(r: &mut Resolver, _path: &str, fi: &FileInfo, node: Node, name: &str, line: i64) -> Option<Resolution> {
        r.c_occurrence(fi, node, name, line)
    }

    fn bare(r: &mut Resolver, path: &str, fi: &FileInfo, lang: Lang, node: Node, name: &str, line: i64) -> Resolution {
        r.c_bare(path, fi, lang, node, name, line)
    }

    fn inherited(r: &mut Resolver, id: i64, name: &str, depth: usize, _path: &str, _qualname: &str) -> Option<i64> {
        r.inherited_through(id, name, depth, |r, id| r.c_bases(id))
    }

    fn bases(r: &mut Resolver, id: i64, _path: &str, _fi: &FileInfo, _d: &Def, _lang: Lang) -> Vec<std::result::Result<i64, &'static str>> {
        r.c_bases(id).into_iter().map(Ok).collect()
    }

    fn module_files(r: &mut Resolver, from: &str, module: &str) -> Option<Vec<String>> {
        r.c_include(from, module)
    }

    fn import_files(r: &mut Resolver, from: &str, fi: &FileInfo, line: i64, _module: &str) -> Option<Vec<String>> {
        let module = statement_bindings(fi, line, &["preproc_include"]).first().map(|b| b.module.clone());
        Some(module.and_then(|m| r.c_include(from, &m)).unwrap_or_default())
    }
}

/// An `#include` binds every top-level name of the file it names; a system include keeps its `<>`.
fn c_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    match n.kind() {
        "preproc_include" => {
            if let Some(p) = n.child_by_field_name("path") {
                let module = if p.kind() == "system_lib_string" { text(p, src).to_string() } else { string_content(p, src) };
                out.push(Binding { local: "*".into(), module, item: None, sites: vec![p.start_byte()] });
            }
            true
        }
        // includes sit at file level, in a namespace, an `extern "C"` block or a conditional
        "function_definition" | "compound_statement" | "field_declaration_list" | "declaration" | "expression_statement" => true,
        _ => false,
    }
}

fn is_field(p: Node, f: &str, n: Node) -> bool {
    p.child_by_field_name(f).is_some_and(|c| c.id() == n.id())
}

/// Whether `node` names a function or method where it is declared or defined (`int f(..)`,
/// `void C::m(..) {..}`, `T *g(..);`).
fn declared_name(node: Node) -> bool {
    let mut n = node;
    while let Some(q) = n.parent().filter(|q| q.kind() == "qualified_identifier" && is_field(*q, "name", n)) {
        n = q;
    }
    let Some(mut f) = n.parent().filter(|f| f.kind() == "function_declarator" && is_field(*f, "declarator", n)) else { return false };
    while let Some(p) = f.parent().filter(|p| p.kind() == "pointer_declarator" && is_field(*p, "declarator", f) || p.kind() == "reference_declarator") {
        f = p;
    }
    f.parent().is_some_and(|d| matches!(d.kind(), "function_definition" | "declaration" | "field_declaration") && is_field(d, "declarator", f))
}

impl Resolver<'_> {
    fn c_occurrence(&mut self, fi: &FileInfo, node: Node, name: &str, line: i64) -> Option<Resolution> {
        let p = node.parent()?;
        if declared_name(node) {
            let d = fi.defs.iter().filter(|d| d.name == name && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start);
            // a declaration is one of the definition it declares
            let id = d.map(|d| d.id).ok_or("declaration");
            return Some(id.map(|id| (self.c_definition(id).unwrap_or(id), "definition")));
        }
        if p.kind() == "qualified_identifier" {
            if is_field(p, "name", node) {
                return Some(self.c_qualified(fi, node, name));
            }
            if is_field(p, "scope", node) {
                return Some(self.c_scope(name));
            }
        }
        if node.kind() == "field_identifier" && p.kind() == "field_expression" {
            // a C++ method, or a function a C struct points to: the type of the value says which
            let called = p.parent().is_some_and(|c| c.kind() == "call_expression" && is_field(c, "function", p));
            return Some(Err(if called { "receiver" } else { "field or key" }));
        }
        None
    }

    /// A name with nothing in front: a local, a member of the class around, a name of the file or
    /// of a namespace around, then one of the files the file includes.
    fn c_bare(&mut self, path: &str, fi: &FileInfo, lang: Lang, node: Node, name: &str, line: i64) -> Resolution {
        if !self.shadowed(lang, node, name, &fi.src) {
            if let Some(class) = self.c_class_at(fi, line) {
                if let Some(m) = self.member(class, name, 0) {
                    return self.c_definition(m).map(|d| (d, "member")).ok_or("not found");
                }
                // a base the repository does not hold may declare it
                let (_, missing) = self.c_base_names(class);
                if missing {
                    return Err("not found");
                }
            }
        }
        match self.scoped_bare(path, fi, lang, node, name, line) {
            Ok((id, why)) => self.c_definition(id).map(|d| (d, why)).ok_or("not found"),
            Err("not found") => self.c_included(path, fi, name, line),
            Err(e) => Err(e),
        }
    }

    /// `name` in the files `path` includes, itself first, at file level, in a namespace around
    /// the use or as an enumerator of an unscoped enum there.
    fn c_included(&mut self, path: &str, fi: &FileInfo, name: &str, line: i64) -> Resolution {
        let mut scopes: Vec<String> = fi.defs.iter().filter(|d| d.kind == "module" && d.start <= line && line <= d.end).map(|d| d.qualname.clone()).collect();
        scopes.push(String::new());
        let (mut todo, mut seen) = (vec![path.to_string()], HashSet::new());
        while let Some(f) = todo.pop() {
            if seen.len() >= MAX_INCLUDED || !seen.insert(f.clone()) {
                continue;
            }
            let ff = if f == path { None } else { Some(self.file(&f)) };
            let at = ff.as_deref().unwrap_or(fi);
            // an enumerator is in the scope of its enum
            let owner = |d: &Def| {
                let enumerated = d.parent.map(|p| &at.defs[p]).filter(|e| d.kind == "constant" && e.kind == "enum");
                let q = enumerated.map_or(d.qualname.as_str(), |e| e.qualname.as_str());
                q.rsplit_once('.').map_or("", |(o, _)| o).to_string()
            };
            let hit = at.defs.iter().find(|d| d.name == name && d.kind != "impl" && scopes.contains(&owner(d))).map(|d| d.id);
            if let Some(id) = hit {
                return self.c_definition(id).map(|d| (d, if f == path { "module" } else { "import" })).ok_or("not found");
            }
            let includes: Vec<String> = at.bindings.iter().filter(|b| b.local == "*").map(|b| b.module.clone()).collect();
            for m in includes.iter().rev() {
                todo.extend(self.c_include(&f, m).unwrap_or_default());
            }
        }
        Err(if self.c_defined(name) { "not found" } else { "external" })
    }

    /// `a::b::name`: the definition of that qualified name, else one inside a namespace of that
    /// name (`inner::f` written in namespace `outer`).
    fn c_qualified(&mut self, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        let (mut parts, mut cur) = (Vec::new(), node);
        while let Some(p) = cur.parent().filter(|p| p.kind() == "qualified_identifier" && is_field(*p, "name", cur)) {
            parts.extend(p.child_by_field_name("scope").map(|s| strip_generics(text(s, &fi.src)).replace("::", ".")));
            cur = p;
        }
        parts.reverse();
        let want = if parts.is_empty() { name.to_string() } else { format!("{}.{name}", parts.join(".")) };
        let rows: Vec<Symbol> =
            self.ix.named(name).unwrap_or_default().into_iter().filter(|r| r.kind != "window" && self.path_lang(&r.path) == Some(Lang::C)).collect();
        let exact: Vec<i64> = rows.iter().filter(|r| strip_generics(&r.qualname) == want).map(|r| r.id).collect();
        let near: Vec<i64> =
            if exact.is_empty() { rows.iter().filter(|r| strip_generics(&r.qualname).ends_with(&format!(".{want}"))).map(|r| r.id).collect() } else { exact };
        let mut found: Vec<i64> = Vec::new();
        for id in near {
            match self.c_definition(id) {
                Some(d) if !found.contains(&d) => found.push(d),
                Some(_) => {}
                None => return Err("not found"),
            }
        }
        match found[..] {
            [one] => Ok((one, "qualified")),
            [] if parts.first().is_none_or(|p| !self.c_defined(p)) => Err("external"),
            _ => Err("not found"),
        }
    }

    /// The `a` of `a::b`: a class or struct, else a namespace.
    fn c_scope(&mut self, name: &str) -> Resolution {
        let rows: Vec<Symbol> = self.ix.named(name).unwrap_or_default().into_iter().filter(|r| self.path_lang(&r.path) == Some(Lang::C)).collect();
        if let Some(t) = rows.iter().find(|r| matches!(r.kind.as_str(), "class" | "struct" | "enum" | "type")) {
            return Ok((t.id, "type"));
        }
        Err(if rows.iter().any(|r| r.kind == "module") { "namespace" } else { "external" })
    }

    /// The class whose member the code at `line` is: the class around, or the one a method
    /// defined outside it names (`void Shape::scale() {..}`).
    fn c_class_at(&mut self, fi: &FileInfo, line: i64) -> Option<i64> {
        let around =
            |kinds: &[&str]| fi.defs.iter().filter(|d| kinds.contains(&d.kind.as_str()) && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start);
        if let Some(c) = around(&["class", "struct"]) {
            return Some(c.id);
        }
        let f = around(&["function", "method"])?;
        let (owner, _) = f.qualname.rsplit_once('.')?;
        let owner = strip_generics(owner);
        let last = owner.rsplit('.').next().unwrap_or(&owner).to_string();
        let rows = self.ix.named(&last).ok()?;
        let ends = |q: &str| q == owner || q.ends_with(&format!(".{owner}")) || owner.ends_with(&format!(".{q}"));
        rows.into_iter()
            .find(|r| matches!(r.kind.as_str(), "class" | "struct") && self.path_lang(&r.path) == Some(Lang::C) && ends(&strip_generics(&r.qualname)))
            .map(|r| r.id)
    }

    /// The names of the bases of class `id` (`class D : public B`), and whether one of them is
    /// not a class of the repository.
    fn c_base_names(&mut self, id: i64) -> (Vec<i64>, bool) {
        let Some((path, _)) = self.def_row(id) else { return (Vec::new(), false) };
        let fi = self.file(&path);
        let Some(node) = def_node(&fi, id) else { return (Vec::new(), false) };
        let mut cur = node.walk();
        let Some(clause) = node.children(&mut cur).find(|c| c.kind() == "base_class_clause") else { return (Vec::new(), false) };
        let mut c2 = clause.walk();
        let names: Vec<String> = clause
            .named_children(&mut c2)
            .filter(|b| matches!(b.kind(), "type_identifier" | "qualified_identifier" | "template_type"))
            .map(|b| strip_generics(text(b, &fi.src)).replace("::", "."))
            .collect();
        let (mut out, mut missing) = (Vec::new(), false);
        for n in names {
            let last = n.rsplit('.').next().unwrap_or(&n).to_string();
            let rows = self.ix.named(&last).unwrap_or_default();
            let hit = rows.iter().find(|r| {
                let q = strip_generics(&r.qualname);
                matches!(r.kind.as_str(), "class" | "struct") && self.path_lang(&r.path) == Some(Lang::C) && (q == n || q.ends_with(&format!(".{n}")))
            });
            match hit {
                Some(r) if r.id != id => out.push(r.id),
                _ => missing = true,
            }
        }
        (out, missing)
    }

    fn c_bases(&mut self, id: i64) -> Vec<i64> {
        self.c_base_names(id).0
    }

    /// The definition a function or method declaration `id` stands for: the one definition of
    /// its name with a body, the one of the file named like the declaring one when there are
    /// several (`a.h` and `a.c`), `id` itself when there is none; None when several remain.
    fn c_definition(&mut self, id: i64) -> Option<i64> {
        if let Some(d) = self.c_defs.get(&id) {
            return *d;
        }
        let found = self.c_definition_uncached(id);
        self.c_defs.insert(id, found);
        found
    }

    fn c_definition_uncached(&mut self, id: i64) -> Option<i64> {
        let Some(row) = self.ix.symbol(id).ok().flatten() else { return Some(id) };
        if !matches!(row.kind.as_str(), "function" | "method") || self.c_bodied(&row.path, id) {
            return Some(id);
        }
        let want = strip_generics(&row.qualname);
        let same = |q: &str| q == want || q.ends_with(&format!(".{want}")) || want.ends_with(&format!(".{q}"));
        let rows: Vec<Symbol> = self.ix.named(&row.name).unwrap_or_default();
        let mut defs: Vec<Symbol> = Vec::new();
        for r in rows {
            if matches!(r.kind.as_str(), "function" | "method")
                && self.path_lang(&r.path) == Some(Lang::C)
                && same(&strip_generics(&r.qualname))
                && self.c_bodied(&r.path, r.id)
            {
                defs.push(r);
            }
        }
        let stem = |p: &str| p.rsplit_once('.').map_or(p, |(s, _)| s).to_string();
        match &defs[..] {
            [] => Some(id),
            [one] => Some(one.id),
            _ => {
                let mine: Vec<&Symbol> = defs.iter().filter(|d| stem(&d.path) == stem(&row.path)).collect();
                if let [one] = mine[..] {
                    Some(one.id)
                } else {
                    None
                }
            }
        }
    }

    /// Whether definition `id` of `path` has a body: a function definition lies within its lines.
    fn c_bodied(&mut self, path: &str, id: i64) -> bool {
        let fi = self.file(path);
        let Some(d) = fi.defs.iter().find(|d| d.id == id) else { return false };
        let (first, last) = ((d.start - 1).max(0) as usize, (d.end - 1).max(0) as usize);
        let Some(tree) = fi.tree.as_ref() else { return false };
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            if n.end_position().row < first || n.start_position().row > last {
                continue;
            }
            if n.kind() == "function_definition" && n.start_position().row >= first && n.end_position().row <= last {
                return true;
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        false
    }

    /// Whether a C or C++ file of the repository defines `name`.
    fn c_defined(&self, name: &str) -> bool {
        self.ix.named(name).map(|rows| rows.iter().any(|r| r.kind != "window" && self.path_lang(&r.path) == Some(Lang::C))).unwrap_or(true)
    }

    /// The files an `#include` of `from` names: a quoted path beside `from`, else the files whose
    /// path ends with it, those nearest `from` when several do. A system include (`<a/b.h>`)
    /// skips the first step. Include paths of the build are not read.
    fn c_include(&mut self, from: &str, module: &str) -> Option<Vec<String>> {
        let (system, m) = match module.strip_prefix('<').and_then(|m| m.strip_suffix('>')) {
            Some(m) => (true, m),
            None => (false, module),
        };
        if !system {
            let near = join(dir_of(from), m);
            if self.files.contains(&near) {
                return Some(vec![near]);
            }
        }
        let m = join("", m);
        if m.is_empty() || m.starts_with("..") {
            return None;
        }
        let base = m.rsplit('/').next().unwrap_or(&m).to_string();
        let suffix = format!("/{m}");
        let cands: Vec<String> = self.c_names().get(&base)?.iter().filter(|p| **p == m || p.ends_with(&suffix)).cloned().collect();
        let shared = |p: &str| p.split('/').zip(from.split('/')).take_while(|(a, b)| a == b).count();
        let best = cands.iter().map(|p| shared(p)).max()?;
        Some(cands.into_iter().filter(|p| shared(p) == best).collect())
    }

    fn c_names(&mut self) -> &HashMap<String, Vec<String>> {
        if self.c_names.is_none() {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for f in &self.files {
                m.entry(f.rsplit('/').next().unwrap_or(f).to_string()).or_default().push(f.clone());
            }
            for v in m.values_mut() {
                v.sort();
            }
            self.c_names = Some(m);
        }
        self.c_names.as_ref().unwrap()
    }
}
