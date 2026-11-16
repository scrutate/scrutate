//! Python: `import` and `from .. import` bindings, `self` and `cls`, `super()`, comprehension
//! scopes, the method resolution order with bases from outside, modules as files on `sys.path`.

use super::*;

pub(super) struct Python;

impl Rules for Python {
    const GRAMMARS: &'static [&'static str] = &["python"];
    // `x` of `x.f()` may be a module
    const METHODS_ONLY: bool = false;
    const SELF_NAMES: &'static [&'static str] = &["self", "cls"];
    const BLOCKS: &'static [&'static str] = &["list_comprehension", "set_comprehension", "dictionary_comprehension", "generator_expression"];
    const COMPREHENSIONS: bool = true;

    fn bindings(n: Node, _path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
        py_bindings(n, src, out)
    }

    fn is_super(q: Node, src: &str) -> bool {
        q.kind() == "call" && q.child_by_field_name("function").is_some_and(|f| f.kind() == "identifier" && text(f, src) == "super")
    }

    fn is_receiver(_q: Node, qt: &str, _node: Node) -> bool {
        qt == "self" || qt == "cls"
    }

    fn constructed_call(r: &mut Resolver, path: &str, fi: &FileInfo, v: Node, f: Node) -> Option<Ty> {
        let (src, line) = (fi.src.as_str(), v.start_position().row as i64 + 1);
        let leaf = member_parts(f).map(|(_, m)| m).unwrap_or(f);
        let found = match qualifier(leaf) {
            Some(q) => r.resolve_qualified(path, fi, Lang::Python, q, leaf, text(leaf, src), line),
            None => r.resolve_bare(path, fi, Lang::Python, leaf, text(leaf, src), line),
        };
        found.ok().map(|(id, _)| id).filter(|&id| r.def_kind(id).is_some_and(|k| concrete(&k))).map(Ty::Local)
    }

    fn unwraps(name: &str) -> bool {
        name == "Optional"
    }

    fn type_alias(fi: &FileInfo, name: &str) -> bool {
        top_level_binder(fi, name)
    }

    // every class ends with `object`: it keeps no place before an in-repo base
    fn outside_base(name: &str) -> bool {
        name != "object"
    }

    fn module_of_chain(r: &mut Resolver, path: &str, fi: &FileInfo, segs: &[&str]) -> Option<Vec<String>> {
        let b = fi.bindings.iter().find(|b| b.local == segs[0])?;
        let mut m = match &b.item {
            Some(it) if b.module.ends_with('.') => format!("{}{it}", b.module),
            Some(it) => format!("{}.{it}", b.module),
            None => b.module.clone(),
        };
        for s in &segs[1..] {
            m = format!("{m}.{s}");
        }
        r.module_files(path, Lang::Python, &m)
    }

    fn module_files(r: &mut Resolver, from: &str, module: &str) -> Option<Vec<String>> {
        r.py_module(from, module).map(|f| vec![f])
    }

    fn import_files(r: &mut Resolver, from: &str, fi: &FileInfo, line: i64, module: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for b in statement_bindings(fi, line, &["import_from_statement"]) {
            // `from m import x` names the submodule `m.x` when there is one, else `m`
            let Some(item) = &b.item else { continue };
            let sub = if b.module.ends_with('.') { format!("{}{item}", b.module) } else { format!("{}.{item}", b.module) };
            out.extend(
                r.py_rooted(from, &sub).or_else(|| r.py_rooted(from, &b.module)).or_else(|| r.py_beside(from, &sub)).or_else(|| r.py_beside(from, &b.module)),
            );
        }
        if out.is_empty() {
            out.extend(r.py_module(from, module));
        }
        Some(out)
    }
}

fn py_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    match n.kind() {
        "import_statement" => {
            let mut cur = n.walk();
            for ch in n.children_by_field_name("name", &mut cur) {
                if ch.kind() == "aliased_import" {
                    let (Some(name), Some(alias)) = (ch.child_by_field_name("name"), ch.child_by_field_name("alias")) else { continue };
                    out.push(Binding { local: text(alias, src).into(), module: text(name, src).into(), item: None, sites: vec![alias.start_byte()] });
                } else if let Some(first) = ch.named_child(0) {
                    // `import a.b.c` binds `a`
                    let head = text(first, src).to_string();
                    out.push(Binding { local: head.clone(), module: head, item: None, sites: vec![first.start_byte()] });
                }
            }
            true
        }
        "import_from_statement" => {
            let Some(m) = n.child_by_field_name("module_name") else { return true };
            let module: String = text(m, src).chars().filter(|c| !c.is_whitespace()).collect();
            let mut cur = n.walk();
            for ch in n.named_children(&mut cur) {
                if ch.kind() == "wildcard_import" {
                    out.push(Binding { local: "*".into(), module: module.clone(), item: None, sites: vec![] });
                }
            }
            let mut cur = n.walk();
            for ch in n.children_by_field_name("name", &mut cur) {
                let (name, alias) =
                    if ch.kind() == "aliased_import" { (ch.child_by_field_name("name"), ch.child_by_field_name("alias")) } else { (Some(ch), None) };
                let Some(name) = name else { continue };
                let item = text(name, src).to_string();
                let mut sites = vec![name.start_byte()];
                sites.extend(alias.map(|a| a.start_byte()));
                out.push(Binding {
                    local: alias.map(|a| text(a, src).to_string()).unwrap_or_else(|| item.clone()),
                    module: module.clone(),
                    item: Some(item),
                    sites,
                });
            }
            true
        }
        _ => false,
    }
}

impl Resolver<'_> {
    fn py_module(&self, from: &str, module: &str) -> Option<String> {
        self.py_rooted(from, module).or_else(|| self.py_beside(from, module))
    }

    fn py_try_base(&self, base: &str) -> Option<String> {
        self.first_file(&[format!("{base}.py"), format!("{base}/__init__.py"), format!("{base}.pyi"), format!("{base}/__init__.pyi")])
    }

    /// An absolute import in the directory of `from`: Python puts a script's own directory first
    /// on `sys.path`, and pytest a test's, so a sibling wins even over a standard library module.
    /// A package's directory is never on `sys.path`, so a file in one gets no such answer.
    fn py_beside(&self, from: &str, module: &str) -> Option<String> {
        let dir = dir_of(from);
        let in_package = || ["py", "pyi"].iter().any(|e| self.files.contains(format!("{dir}/__init__.{e}").as_str()));
        if dir.is_empty() || module.starts_with('.') || in_package() {
            return None;
        }
        self.py_try_base(&format!("{dir}/{}", module.replace('.', "/")))
    }

    fn py_rooted(&self, from: &str, module: &str) -> Option<String> {
        let try_base = |base: &str| self.py_try_base(base);
        if module.starts_with('.') {
            let target = py_relative(from, module);
            return if target.is_empty() { self.first_file(&["__init__.py".to_string()]) } else { try_base(&target) };
        }
        let slashed = module.replace('.', "/");
        if let Some(f) = try_base(&slashed).or_else(|| try_base(&format!("src/{slashed}"))) {
            return Some(f);
        }
        let segs: Vec<&str> = slashed.split('/').collect();
        below_own_package(&self.ix.root, &segs).and_then(|rest| try_base(&rest.join("/")))
    }
}

/// Whether the file assigns `name` at its top level (a Python `T = TypeVar("T")` or alias).
fn top_level_binder(fi: &FileInfo, name: &str) -> bool {
    let Some(tree) = fi.tree() else { return false };
    let root = tree.root_node();
    let mut cur = root.walk();

    root.named_children(&mut cur).any(|s| {
        s.kind() == "expression_statement"
            && s.named_child(0).is_some_and(|a| a.kind() == "assignment" && a.child_by_field_name("left").is_some_and(|l| text(l, &fi.src) == name))
    })
}
