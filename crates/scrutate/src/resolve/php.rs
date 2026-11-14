//! PHP: `namespace` and `use`, classes, traits and their methods, `new` and typed locals and
//! parameters, typed properties, `$this`, `self::`, `static::` and `parent::`, the return type a
//! method declares. A class is looked up by its name in the index and kept when the file that
//! defines it declares the namespace the reference names.

use super::*;

pub(super) struct Php;

impl Rules for Php {
    const GRAMMARS: &'static [&'static str] = &["php"];
    // `.` joins strings: `$a . f()` may call a free function
    const METHODS_ONLY: bool = false;
    const MEMBERS_ANY_CASE: bool = true;

    fn bindings(n: Node, _path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
        if n.kind() != "namespace_use_declaration" {
            return false;
        }
        let mut prefix = "";
        let mut cur = n.walk();
        for ch in n.named_children(&mut cur) {
            match ch.kind() {
                "namespace_name" => prefix = text(ch, src),
                "namespace_use_clause" => php_use_clause(ch, prefix, src, out),
                "namespace_use_group" => {
                    let mut c2 = ch.walk();
                    for cl in ch.named_children(&mut c2).filter(|c| c.kind() == "namespace_use_clause") {
                        php_use_clause(cl, prefix, src, out);
                    }
                }
                _ => {}
            }
        }
        true
    }

    fn import_site(r: &mut Resolver, _path: &str, _lang: Lang, b: &Binding) -> Resolution {
        r.php_used(&php_full(b)).map(|id| (id, "import"))
    }

    fn occurrence(r: &mut Resolver, _path: &str, fi: &FileInfo, node: Node, name: &str, _line: i64) -> Option<Resolution> {
        Some(r.php_resolve(fi, node, name))
    }

    fn bases(r: &mut Resolver, id: i64, _path: &str, fi: &FileInfo, _d: &Def, _lang: Lang) -> Vec<std::result::Result<i64, &'static str>> {
        r.php_bases(fi, id)
    }

    fn module_files(r: &mut Resolver, _from: &str, module: &str) -> Option<Vec<String>> {
        let id = r.php_used(module.trim_start_matches('\\')).ok()?;
        r.def_row(id).map(|(path, _)| vec![path])
    }

    // a `require` of a path is left to the mapping of paths onto files
    fn reads_import(module: &str) -> bool {
        let name = module.trim_start_matches('\\');
        !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '\\')
    }

    fn import_files(r: &mut Resolver, from: &str, _fi: &FileInfo, _line: i64, module: &str) -> Option<Vec<String>> {
        Some(Self::module_files(r, from, module).unwrap_or_default())
    }
}

/// The functions whose variables are their own: an arrow function sees the ones around it.
const PHP_FUNCTIONS: &[&str] = &["function_definition", "method_declaration", "anonymous_function"];

fn php_use_clause(cl: Node, prefix: &str, src: &str, out: &mut Vec<Binding>) {
    let mut cur = cl.walk();
    let Some(path) = cl.named_children(&mut cur).find(|c| matches!(c.kind(), "qualified_name" | "name")) else { return };
    let rel = text(path, src);
    let full = if prefix.is_empty() { rel.to_string() } else { format!("{prefix}\\{rel}") };
    let full = full.trim_start_matches('\\');
    let (module, item) = full.rsplit_once('\\').unwrap_or(("", full));
    let last = path.named_child(path.named_child_count().saturating_sub(1) as u32).unwrap_or(path);
    let alias = cl.child_by_field_name("alias");
    let mut sites = vec![last.start_byte()];
    sites.extend(alias.map(|a| a.start_byte()));
    out.push(Binding { local: alias.map_or(item, |a| text(a, src)).to_string(), module: module.to_string(), item: Some(item.to_string()), sites });
}

/// The class names `n` lists (`extends A`, `implements B, C`, `use T;`).
fn php_names<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut cur = n.walk();
    n.named_children(&mut cur).filter(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name")).collect()
}

/// The full name a `use` binding names.
fn php_full(b: &Binding) -> String {
    let item = b.item.as_deref().unwrap_or("");
    if b.module.is_empty() {
        item.to_string()
    } else {
        format!("{}\\{item}", b.module)
    }
}

/// The namespace that holds 0-based line `row` of the file: the braced `namespace` block around
/// it, else the last `namespace X;` above it, else the global one ("").
fn php_namespace(fi: &FileInfo, row: usize) -> String {
    let Some(tree) = fi.tree.as_ref() else { return String::new() };
    let root = tree.root_node();
    let mut cur = root.walk();
    let mut ns = String::new();
    for c in root.named_children(&mut cur) {
        if c.start_position().row > row {
            break;
        }
        if c.kind() != "namespace_definition" {
            continue;
        }
        let name = c.child_by_field_name("name").map_or("", |n| text(n, &fi.src)).to_string();
        match c.child_by_field_name("body") {
            Some(_) if c.end_position().row >= row => return name,
            Some(_) => ns = String::new(),
            None => ns = name,
        }
    }
    ns
}

/// The full name a class or function name written at `at` stands for: through a `use` that binds
/// its first part, else in the namespace around.
fn php_qualify(fi: &FileInfo, at: Node, written: &str) -> String {
    if let Some(full) = written.strip_prefix('\\') {
        return full.to_string();
    }
    let (first, rest) = match written.split_once('\\') {
        Some((f, r)) => (f, Some(r)),
        None => (written, None),
    };
    if let Some(b) = fi.bindings.iter().find(|b| b.local.eq_ignore_ascii_case(first)) {
        let base = php_full(b);
        return rest.map_or(base.clone(), |r| format!("{base}\\{r}"));
    }
    let rel = written.strip_prefix("namespace\\").unwrap_or(written);
    let ns = php_namespace(fi, at.start_position().row);
    if ns.is_empty() {
        rel.to_string()
    } else {
        format!("{ns}\\{rel}")
    }
}

/// The variables named `var` in `scope` that are bound before a use sees them: an assignment
/// (its whole span), a `foreach` target or a caught exception (the variable). Not in a nested
/// function or class.
fn php_binders(scope: Node, var: &str, src: &str) -> Vec<(usize, usize)> {
    let (mut out, mut stack) = (Vec::new(), vec![scope]);
    while let Some(n) = stack.pop() {
        match n.kind() {
            "assignment_expression" if n.child_by_field_name("left").is_some_and(|l| l.kind() == "variable_name" && text(l, src) == var) => {
                out.push((n.start_byte(), n.end_byte()));
            }
            // the value of `foreach ($xs as $k => $v)`, after the list it walks
            "variable_name"
                if text(n, src) == var
                    && n.parent().is_some_and(|p| match p.kind() {
                        "foreach_statement" => n.prev_named_sibling().is_some(),
                        "pair" => p.parent().is_some_and(|g| g.kind() == "foreach_statement"),
                        k => k == "catch_clause",
                    }) =>
            {
                out.push((n.start_byte(), n.end_byte()));
            }
            k if n.id() != scope.id() && (PHP_FUNCTIONS.contains(&k) || matches!(k, "arrow_function" | "class_declaration")) => continue,
            _ => {}
        }
        let mut cur = n.walk();
        stack.extend(n.named_children(&mut cur));
    }
    out.sort_unstable_by_key(|&(_, end)| end);
    out
}

impl Resolver<'_> {
    /// The occurrence of `name` at `node`, a `name` node of a PHP file.
    fn php_resolve(&mut self, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        if node.kind() != "name" {
            return Err("keyword");
        }
        let Some(p) = node.parent() else { return Err("not found") };
        match p.kind() {
            // `public $name;` declares a property
            "variable_name" if p.parent().is_some_and(|g| matches!(g.kind(), "property_element" | "property_promotion_parameter")) => {
                return Err("field or key")
            }
            // `User::$current` reads a static property
            "variable_name" if p.parent().is_some_and(|g| g.kind() == "scoped_property_access_expression") => return Err("field or key"),
            "variable_name" => return Err("local variable"),
            "namespace_name" | "namespace_definition" => return Err("namespace"),
            _ => {}
        }
        // the last part of `A\B\C` stands for the whole name
        let (whole, around) = if matches!(p.kind(), "qualified_name" | "relative_name") { (p, p.parent()) } else { (node, Some(p)) };
        let Some(a) = around else { return Err("not found") };
        let is = |f: &str| a.child_by_field_name(f).map(|c| c.id()) == Some(whole.id());
        match a.kind() {
            "member_call_expression" | "nullsafe_member_call_expression" if is("name") => {
                let obj = a.child_by_field_name("object").ok_or("receiver")?;
                let how = if text(obj, &fi.src) == "$this" { "member" } else { "variable" };
                let owner = self.php_value(fi, obj)?;
                self.php_member(owner, name, how)
            }
            "member_access_expression" | "nullsafe_member_access_expression" if is("name") => Err("field or key"),
            "scoped_call_expression" if is("name") => {
                let scope = a.child_by_field_name("scope").ok_or("receiver")?;
                self.php_static_call(fi, scope, name)
            }
            "class_constant_access_expression" if a.named_child(0).map(|c| c.id()) != Some(whole.id()) => Err("field or key"),
            "function_call_expression" if is("function") => self.php_function(fi, whole),
            "binary_expression" if !is("right") || a.child_by_field_name("operator").is_none_or(|o| o.kind() != "instanceof") => Err("constant"),
            "named_type"
            | "object_creation_expression"
            | "base_clause"
            | "class_interface_clause"
            | "scoped_call_expression"
            | "class_constant_access_expression"
            | "scoped_property_access_expression"
            | "use_declaration"
            | "binary_expression"
            | "attribute" => self.php_type(fi, whole).map(|id| (id, "type")),
            _ => Err("constant"),
        }
    }

    /// `S::name(..)`: a method of the class `S` names, of the class around for `self` and
    /// `static`, of its parent for `parent`, or of the class of the value `S` is.
    fn php_static_call(&mut self, fi: &FileInfo, scope: Node, name: &str) -> Resolution {
        // `self`, `static` and `parent` are keywords, in any case
        match text(scope, &fi.src).trim().to_ascii_lowercase().as_str() {
            "parent" => {
                let c = self.php_around(fi, scope)?;
                let base = self.php_parent(c)?;
                self.php_member(base, name, "super")
            }
            "self" | "static" => {
                let c = self.php_around(fi, scope)?;
                self.php_member(c, name, "member")
            }
            _ if matches!(scope.kind(), "name" | "qualified_name" | "relative_name") => {
                let c = self.php_type(fi, scope)?;
                self.php_member(c, name, "qualified")
            }
            _ => {
                let c = self.php_value(fi, scope)?;
                self.php_member(c, name, "variable")
            }
        }
    }

    /// Method `name` of class `c`, its traits and bases included; "external" when a base from
    /// outside may declare it.
    fn php_member(&mut self, c: i64, name: &str, how: &'static str) -> Resolution {
        if let Some(m) = self.member(c, name, 0) {
            return Ok((m, how));
        }
        // a trait's `$this` is the class that uses it, which may have the method
        if self.def_kind(c).as_deref() == Some("trait") {
            return Err("receiver");
        }
        let (_, outside, unknown) = self.linearized(c, 0, &mut HashMap::new());
        Err(if outside && !unknown { "external" } else { "not found" })
    }

    /// A function called by the name `whole`: in the namespace it names, then a global function
    /// of that name, as PHP falls back for an unqualified call.
    fn php_function(&mut self, fi: &FileInfo, whole: Node) -> Resolution {
        let written = text(whole, &fi.src).trim();
        let full = php_qualify(fi, whole, written);
        match self.php_path(&full, &["function"]) {
            Ok(id) => Ok((id, "namespace")),
            Err(_) if !written.contains('\\') && full != written && !fi.bindings.iter().any(|b| b.local.eq_ignore_ascii_case(written)) => {
                self.php_path(written, &["function"]).map(|id| (id, "namespace"))
            }
            Err(e) => Err(e),
        }
    }

    /// The class a name written at `n` stands for (`User`, `\App\User`, `self`, `parent`).
    fn php_type(&mut self, fi: &FileInfo, n: Node) -> std::result::Result<i64, &'static str> {
        let written = text(n, &fi.src).trim();
        match written.to_ascii_lowercase().as_str() {
            "self" | "static" => return self.php_around(fi, n),
            "parent" => {
                let c = self.php_around(fi, n)?;
                return self.php_parent(c);
            }
            _ => {}
        }
        let full = php_qualify(fi, n, written);
        self.php_path(&full, CONTAINER_KINDS)
    }

    /// The class, interface, trait or enum a `use` names, else the function.
    fn php_used(&mut self, full: &str) -> std::result::Result<i64, &'static str> {
        match self.php_path(full, CONTAINER_KINDS) {
            Ok(id) => Ok(id),
            Err(e) => self.php_path(full, &["function"]).map_err(|_| e),
        }
    }

    /// The definition of one of `kinds` the full name `A\B\C` stands for: one named `C` at the
    /// top of a PHP file whose namespace there is `A\B`. "external" when the repository defines
    /// no such name anywhere.
    fn php_path(&mut self, full: &str, kinds: &[&str]) -> std::result::Result<i64, &'static str> {
        let key = format!("{full}\n{}", kinds.join(","));
        if let Some(r) = self.php_paths.get(&key) {
            return *r;
        }
        let (ns, name) = full.rsplit_once('\\').unwrap_or(("", full));
        // PHP class, function and namespace names ignore case: `FormatPrice(3)` calls `formatPrice`
        let mut rows: Vec<Symbol> = self.ix.named_any_case(name).unwrap_or_default();
        rows.retain(|r| r.qualname.eq_ignore_ascii_case(name) && kinds.contains(&r.kind.as_str()) && self.path_lang(&r.path) == Some(Lang::Php));
        rows.sort_by(|a, b| (&a.path, a.start).cmp(&(&b.path, b.start)));
        let mut found = if rows.is_empty() { Err("external") } else { Err("not found") };
        for r in rows {
            let f = self.file(&r.path);
            if php_namespace(&f, (r.start - 1).max(0) as usize).eq_ignore_ascii_case(ns) {
                found = Ok(r.id);
                break;
            }
        }
        self.php_paths.insert(key, found);
        found
    }

    /// The innermost class around `n`.
    fn php_around(&self, fi: &FileInfo, n: Node) -> std::result::Result<i64, &'static str> {
        match self.container_at(fi, n.start_position().row as i64 + 1) {
            Some(Ty::Local(c)) => Ok(c),
            _ => Err("receiver"),
        }
    }

    /// The class class `c` extends.
    fn php_parent(&mut self, c: i64) -> std::result::Result<i64, &'static str> {
        let (path, _) = self.def_row(c).ok_or("not found")?;
        let fi = self.file(&path);
        let node = def_node(&fi, c).ok_or("not found")?;
        let mut cur = node.walk();
        let base = node.named_children(&mut cur).find(|n| n.kind() == "base_clause").and_then(|b| b.named_child(0)).ok_or("not found")?;
        // `extends parent` would name this class's parent again
        if matches!(text(base, &fi.src).to_ascii_lowercase().as_str(), "self" | "static" | "parent") {
            return Err("not found");
        }
        self.php_type(&fi, base)
    }

    /// The traits class `id` uses, then the class it extends, then the interfaces it implements.
    fn php_bases(&mut self, fi: &FileInfo, id: i64) -> Vec<std::result::Result<i64, &'static str>> {
        let Some(node) = def_node(fi, id) else { return Vec::new() };
        let mut named = Vec::new();
        if let Some(body) = node.child_by_field_name("body") {
            let mut cur = body.walk();
            for u in body.named_children(&mut cur).filter(|c| c.kind() == "use_declaration") {
                named.extend(php_names(u));
            }
        }
        let mut cur = node.walk();
        for c in node.named_children(&mut cur).filter(|c| matches!(c.kind(), "base_clause" | "class_interface_clause")) {
            named.extend(php_names(c));
        }
        named.into_iter().map(|n| self.php_type(fi, n)).filter(|b| *b != Ok(id)).collect()
    }

    /// The class of the value `e` is: `$this`, a variable, `new C`, what a call returns, a typed
    /// property.
    fn php_value(&mut self, fi: &FileInfo, e: Node) -> std::result::Result<i64, &'static str> {
        if self.nest > 32 {
            return Err("receiver");
        }
        self.nest += 1;
        let r = self.php_value_inner(fi, e);
        self.nest -= 1;
        r
    }

    fn php_value_inner(&mut self, fi: &FileInfo, e: Node) -> std::result::Result<i64, &'static str> {
        let src = fi.src.as_str();
        match e.kind() {
            "variable_name" if text(e, src) == "$this" => self.php_around(fi, e),
            "variable_name" => self.php_variable(fi, e),
            "object_creation_expression" => {
                let mut cur = e.walk();
                let class = e.named_children(&mut cur).find(|c| matches!(c.kind(), "name" | "qualified_name" | "relative_name"));
                self.php_type(fi, class.ok_or("receiver")?)
            }
            "parenthesized_expression" => self.php_value(fi, e.named_child(0).ok_or("receiver")?),
            "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression" | "function_call_expression" => {
                let callee = e.child_by_field_name(if e.kind() == "function_call_expression" { "function" } else { "name" }).ok_or("receiver")?;
                let leaf = if callee.kind() == "qualified_name" {
                    callee.named_child(callee.named_child_count().saturating_sub(1) as u32).ok_or("receiver")?
                } else {
                    callee
                };
                let (id, _) = self.php_resolve(fi, leaf, text(leaf, src)).map_err(|_| "receiver")?;
                self.php_returns(id)
            }
            "member_access_expression" | "nullsafe_member_access_expression" => {
                let (obj, prop) = (e.child_by_field_name("object").ok_or("receiver")?, e.child_by_field_name("name").ok_or("receiver")?);
                let c = self.php_value(fi, obj)?;
                self.php_property(c, &format!("${}", text(prop, src)))
            }
            "scoped_property_access_expression" => {
                let (scope, prop) = (e.child_by_field_name("scope").ok_or("receiver")?, e.child_by_field_name("name").ok_or("receiver")?);
                let c = self.php_type(fi, scope)?;
                self.php_property(c, text(prop, src))
            }
            _ => Err("receiver"),
        }
    }

    /// The class of variable `e`: the value of its latest assignment before the use, else the
    /// type its parameter declares.
    fn php_variable(&mut self, fi: &FileInfo, e: Node) -> std::result::Result<i64, &'static str> {
        self.php_variable_above(fi, e, e)
    }

    /// `php_variable` of `e` in the function around `inside`.
    fn php_variable_above(&mut self, fi: &FileInfo, e: Node, inside: Node) -> std::result::Result<i64, &'static str> {
        let src = fi.src.as_str();
        let var = text(e, src);
        let scope =
            std::iter::successors(inside.parent(), |n| n.parent()).find(|n| PHP_FUNCTIONS.contains(&n.kind()) || n.parent().is_none()).ok_or("receiver")?;
        let at = e.start_byte();
        let binders = self.found_in(scope, "php binders", var, || php_binders(scope, var, src));
        let latest = binders[..binders.partition_point(|&(_, end)| end <= at)].last().copied();
        if let Some((s, end)) = latest {
            let b = scope.descendant_for_byte_range(s, end).ok_or("receiver")?;
            return match b.kind() {
                "assignment_expression" => self.php_value(fi, b.child_by_field_name("right").ok_or("receiver")?),
                _ => Err("receiver"),
            };
        }
        let mut cur = scope.walk();
        let param = scope
            .child_by_field_name("parameters")
            .and_then(|ps| ps.named_children(&mut cur).find(|p| p.child_by_field_name("name").is_some_and(|n| text(n, src) == var)));
        if let Some(t) = param.and_then(|p| p.child_by_field_name("type")) {
            return self.php_hint(fi, t);
        }
        // `function () use ($u)` captures `$u` as it is in the code around the closure
        let mut c2 = scope.walk();
        let captured = scope.named_children(&mut c2).filter(|c| c.kind() == "anonymous_function_use_clause").find_map(|u| {
            let mut c3 = u.walk();
            // `use (&$u)` holds the variable in a by_ref node
            let found = u.named_children(&mut c3).map(|v| if v.kind() == "by_ref" { v.named_child(0).unwrap_or(v) } else { v }).find(|v| text(*v, src) == var);
            found
        });
        match captured {
            Some(v) => self.php_variable_above(fi, v, scope),
            None => Err("receiver"),
        }
    }

    /// The class a type declaration names (`User`, `?User`, `self`); None for a primitive or a union.
    fn php_hint(&mut self, fi: &FileInfo, t: Node) -> std::result::Result<i64, &'static str> {
        match t.kind() {
            "named_type" | "optional_type" => self.php_hint(fi, t.named_child(0).ok_or("receiver")?),
            "name" | "qualified_name" | "relative_name" => self.php_type(fi, t),
            _ if matches!(text(t, &fi.src).trim().to_ascii_lowercase().as_str(), "self" | "static") => self.php_around(fi, t),
            _ => Err("receiver"),
        }
    }

    /// The class the function or method `id` declares it returns.
    fn php_returns(&mut self, id: i64) -> std::result::Result<i64, &'static str> {
        let (path, _) = self.def_row(id).ok_or("receiver")?;
        let fi = self.file(&path);
        let t = def_node(&fi, id).and_then(|n| n.child_by_field_name("return_type")).ok_or("receiver")?;
        self.php_hint(&fi, t)
    }

    /// The class of property `prop` (`$name`) of class `c` or a class it inherits from: the type
    /// its declaration or constructor parameter states.
    fn php_property(&mut self, c: i64, prop: &str) -> std::result::Result<i64, &'static str> {
        let (order, _, _) = self.linearized(c, 0, &mut HashMap::new());
        for k in order.into_iter().filter(|&k| k >= 0) {
            let Some((path, _)) = self.def_row(k) else { continue };
            let fi = self.file(&path);
            let Some(body) = def_node(&fi, k).and_then(|n| n.child_by_field_name("body")) else { continue };
            let src = fi.src.as_str();
            let mut stack = vec![body];
            while let Some(n) = stack.pop() {
                let declared = match n.kind() {
                    "property_declaration" => {
                        let mut cur = n.walk();
                        let named = n
                            .named_children(&mut cur)
                            .any(|p| p.kind() == "property_element" && p.child_by_field_name("name").is_some_and(|x| text(x, src) == prop));
                        named.then(|| n.child_by_field_name("type"))
                    }
                    "property_promotion_parameter" if n.child_by_field_name("name").is_some_and(|x| text(x, src) == prop) => {
                        Some(n.child_by_field_name("type"))
                    }
                    "compound_statement" => continue,
                    _ => None,
                };
                if let Some(t) = declared {
                    return self.php_hint(&fi, t.ok_or("receiver")?);
                }
                let mut cur = n.walk();
                stack.extend(n.named_children(&mut cur));
            }
        }
        Err("receiver")
    }
}
