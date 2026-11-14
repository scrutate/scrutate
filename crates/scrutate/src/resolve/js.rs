//! JavaScript and TypeScript: ES module and CommonJS bindings, `this` and object literals, `super`,
//! `var` scoping, literal and `Promise` types, modules by relative path, `tsconfig.json` and
//! workspace `package.json` names.

use super::*;

pub(super) struct Js;

impl Rules for Js {
    const GRAMMARS: &'static [&'static str] = &["javascript", "typescript", "tsx"];
    const REQUIRE: bool = true;

    fn bindings(n: Node, path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
        js_bindings(n, path, src, out)
    }

    fn occurrence(r: &mut Resolver, path: &str, fi: &FileInfo, node: Node, _name: &str, line: i64) -> Option<Resolution> {
        if node.kind() != "super" {
            return None;
        }
        // `super(..)` calls the constructor of the base class, which stands for it as `new C()` does
        let called = node.parent().and_then(|p| p.child_by_field_name("function")).is_some_and(|f| f.id() == node.id());
        if !called {
            return Some(Err("keyword"));
        }
        // a class expression (`const Kid = class extends Base {..}`) is no definition of the index:
        // its base is the one its `extends` names
        let class =
            std::iter::successors(node.parent(), |n| n.parent()).find(|n| matches!(n.kind(), "class" | "class_declaration" | "abstract_class_declaration"));
        if let Some(c) = class.filter(|c| c.kind() == "class") {
            let mut cur = c.walk();
            let heritage = c.children(&mut cur).find(|h| h.kind() == "class_heritage");
            let base = heritage.and_then(|h| h.named_child(0)).and_then(|b| {
                if b.kind() == "extends_clause" {
                    b.child_by_field_name("value").or_else(|| b.named_child(0))
                } else {
                    Some(b)
                }
            });
            let Some(leaf) = base.and_then(type_name) else { return Some(Err("receiver")) };
            return Some(r.type_leaf(path, fi, Lang::Js, leaf, line).map(|(id, _)| (id, "super")));
        }
        let Some(Ty::Local(c)) = r.container_at(fi, line) else { return Some(Err("receiver")) };
        let (order, outside, unknown) = r.linearized(c, 0, &mut HashMap::new());
        Some(match order.get(1) {
            Some(&b) => Ok((b, "super")),
            None => Err(if outside && !unknown { "external" } else { "not found" }),
        })
    }

    fn is_super(q: Node, _src: &str) -> bool {
        q.kind() == "super"
    }

    fn is_receiver(_q: Node, qt: &str, _node: Node) -> bool {
        qt == "this"
    }

    fn literal_owner(fi: &FileInfo, q: Node) -> Option<String> {
        literal_owner(fi, q)
    }

    fn awaited(t: Ty) -> Option<Ty> {
        match t {
            Ty::Foreign(n, args) if n == "Promise" => args.into_iter().next().flatten(),
            t => Some(t),
        }
    }

    fn literal_class(kind: &str) -> Option<&'static str> {
        Some(match kind {
            "number" => "Number",
            "array" => "Array",
            "regex" => "RegExp",
            _ => "String",
        })
    }

    fn function_wide(b: Node) -> bool {
        is_js_var(b)
    }

    fn module_of_chain(r: &mut Resolver, path: &str, fi: &FileInfo, segs: &[&str]) -> Option<Vec<String>> {
        r.bound_module(path, fi, Lang::Js, segs)
    }

    fn module_files(r: &mut Resolver, from: &str, module: &str) -> Option<Vec<String>> {
        r.js_module(from, module).map(|f| vec![f])
    }

    fn import_files(r: &mut Resolver, from: &str, _fi: &FileInfo, _line: i64, module: &str) -> Option<Vec<String>> {
        Some(r.js_module(from, module).into_iter().collect())
    }
}

/// What a `tsconfig.json` or `jsconfig.json` says about import names, as repository paths.
#[derive(Clone, Default)]
pub(super) struct TsPaths {
    /// `baseUrl`: the directory a name that is not relative is looked up in
    base: Option<String>,
    /// `paths`: a pattern (`@lib/*`) and the places it stands for (`src/lib/*`)
    aliases: Vec<(String, Vec<String>)>,
    /// the folder of the config that sets `paths`, which its places are relative to without a `baseUrl`
    paths_dir: String,
}

impl TsPaths {
    /// The places of `paths` as repository paths: relative to the `baseUrl` the merged configs end
    /// with, inherited or not, else to the folder of the config that sets them. A `${configDir}`
    /// at the start of a place or of `baseUrl` stands for `config_dir`, the folder of the config
    /// the lookup started from.
    fn joined(mut self, config_dir: &str) -> TsPaths {
        let sub = |p: &str| p.strip_prefix(CONFIG_DIR).map(|rest| join(config_dir, &format!("./{rest}")));
        self.base = self.base.map(|b| sub(&b).unwrap_or(b));
        let at = self.base.clone().unwrap_or_else(|| self.paths_dir.clone());
        for (_, places) in &mut self.aliases {
            *places = places.iter().map(|p| sub(p).unwrap_or_else(|| join(&at, p))).collect();
        }
        self
    }
}

/// What a `package.json` of the repository says about the file an import of its name opens.
pub(super) struct JsPackage {
    dir: String,
    exports: serde_json::Value,
    module: Option<String>,
    main: Option<String>,
}

fn js_bindings(n: Node, path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
    // the definition a file's default export expression is: its own name, else the file's
    let default_of = |v: Node| {
        crate::langs::js_default_export(v, src.as_bytes())
            .map(|_| v.child_by_field_name("name").map_or_else(|| crate::parse::default_export_name(path), |n| text(n, src).to_string()))
    };
    match n.kind() {
        "import_statement" => {
            // TypeScript `import m = require('./m')` binds the module as `require` does
            let mut cur = n.walk();
            if let Some(c) = n.named_children(&mut cur).find(|c| c.kind() == "import_require_clause") {
                let mut c2 = c.walk();
                let kids: Vec<Node> = c.named_children(&mut c2).collect();
                if let (Some(id), Some(m)) = (kids.iter().find(|k| k.kind() == "identifier"), kids.iter().find(|k| k.kind() == "string")) {
                    out.push(Binding { local: text(*id, src).into(), module: string_content(*m, src), item: None, sites: vec![id.start_byte()] });
                }
                return true;
            }
            let Some(source) = n.child_by_field_name("source") else { return true };
            let module = string_content(source, src);
            let mut cur = n.walk();
            for clause in n.named_children(&mut cur) {
                let mut c2 = clause.walk();
                let parts: Vec<Node> = if clause.kind() == "import_clause" { clause.named_children(&mut c2).collect() } else { vec![] };
                for p in parts {
                    match p.kind() {
                        "identifier" => {
                            out.push(Binding { local: text(p, src).into(), module: module.clone(), item: Some("default".into()), sites: vec![p.start_byte()] })
                        }
                        "namespace_import" => {
                            if let Some(id) = p.named_child(0) {
                                out.push(Binding { local: text(id, src).into(), module: module.clone(), item: None, sites: vec![id.start_byte()] });
                            }
                        }
                        "named_imports" => js_specifiers(p, &module, src, out),
                        _ => {}
                    }
                }
            }
            true
        }
        "export_statement" => {
            let module = n.child_by_field_name("source").map(|s| string_content(s, src));
            let mut cur = n.walk();
            let kids: Vec<Node> = n.children(&mut cur).collect();
            if kids.iter().any(|k| k.kind() == "default") {
                let target = n
                    .child_by_field_name("declaration")
                    .and_then(|d| d.child_by_field_name("name"))
                    .or_else(|| n.child_by_field_name("value").filter(|v| v.kind() == "identifier"));
                let item = target.map(|t| text(t, src).to_string()).or_else(|| n.child_by_field_name("value").and_then(default_of));
                if let Some(item) = item {
                    out.push(Binding { local: "default".into(), module: String::new(), item: Some(item), sites: vec![] });
                }
                return false; // the declaration inside is still walked
            }
            let clause = kids.iter().find(|k| k.kind() == "export_clause");
            match (clause, &module) {
                (Some(c), m) => js_specifiers(*c, m.as_deref().unwrap_or(""), src, out),
                (None, Some(m)) => match kids.iter().find(|k| k.kind() == "namespace_export").and_then(|k| k.named_child(0)) {
                    Some(id) => out.push(Binding { local: text(id, src).into(), module: m.clone(), item: None, sites: vec![id.start_byte()] }),
                    None => out.push(Binding { local: "*".into(), module: m.clone(), item: None, sites: vec![] }),
                },
                (None, None) => return false,
            }
            true
        }
        "variable_declarator" => {
            // CommonJS: `const x = require('./m')`, `const {a, b: c} = require('./m')`, `const x =
            // require('./m').x`; a dynamic `await import('./m')` is the module too
            let (Some(name), Some(value)) = (n.child_by_field_name("name"), n.child_by_field_name("value")) else { return false };
            let (value, member) = match value.kind() {
                "member_expression" => {
                    (value.child_by_field_name("object"), value.child_by_field_name("property").filter(|p| p.kind() == "property_identifier"))
                }
                "await_expression" => (value.named_child(0), None),
                _ => (Some(value), None),
            };
            let awaited = n.child_by_field_name("value").is_some_and(|v| v.kind() == "await_expression");
            let Some(value) = value.filter(|v| v.kind() == "call_expression") else { return false };
            // `import(..)` without `await` is a promise of the module, not the module
            let wanted = if awaited { "import" } else { "require" };
            if value.child_by_field_name("function").map(|f| text(f, src)) != Some(wanted) {
                return false;
            }
            let Some(arg) = value.child_by_field_name("arguments").and_then(|a| a.named_child(0)).filter(|a| a.kind() == "string") else { return false };
            let module = string_content(arg, src);
            match (name.kind(), member) {
                ("identifier", Some(m)) => {
                    out.push(Binding { local: text(name, src).into(), module, item: Some(text(m, src).into()), sites: vec![name.start_byte(), m.start_byte()] })
                }
                ("identifier", None) => out.push(Binding { local: text(name, src).into(), module, item: None, sites: vec![name.start_byte()] }),
                ("object_pattern", None) => {
                    let mut cur = name.walk();
                    for p in name.named_children(&mut cur) {
                        match p.kind() {
                            "shorthand_property_identifier_pattern" => out.push(Binding {
                                local: text(p, src).into(),
                                module: module.clone(),
                                item: Some(text(p, src).into()),
                                sites: vec![p.start_byte()],
                            }),
                            "pair_pattern" => {
                                if let (Some(k), Some(v)) = (p.child_by_field_name("key"), p.child_by_field_name("value")) {
                                    out.push(Binding {
                                        local: text(v, src).into(),
                                        module: module.clone(),
                                        item: Some(text(k, src).into()),
                                        sites: vec![k.start_byte(), v.start_byte()],
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            true
        }
        "assignment_expression" => {
            let right = n.child_by_field_name("right");
            // `module.exports = helper` exports the file's own `helper` as the default
            let exports = n.child_by_field_name("left").and_then(|l| crate::langs::js_member_path(l, src.as_bytes())).as_deref() == Some("module.exports");
            let named = right.filter(|r| exports && r.kind() == "identifier").map(|r| text(r, src).to_string());
            if let Some(item) = named.or_else(|| right.and_then(default_of)) {
                out.push(Binding { local: "default".into(), module: String::new(), item: Some(item), sites: vec![] });
            }
            false
        }
        _ => false,
    }
}

fn js_specifiers(list: Node, module: &str, src: &str, out: &mut Vec<Binding>) {
    let mut cur = list.walk();
    for s in list.named_children(&mut cur) {
        let Some(name) = s.child_by_field_name("name") else { continue };
        let alias = s.child_by_field_name("alias");
        let item = text(name, src).to_string();
        let mut sites = vec![name.start_byte()];
        sites.extend(alias.map(|a| a.start_byte()));
        out.push(Binding {
            local: alias.map(|a| text(a, src).to_string()).unwrap_or_else(|| item.clone()),
            module: module.to_string(),
            item: Some(item),
            sites,
        });
    }
}

impl Resolver<'_> {
    /// Whether `b` binds `require(...)` in `from`; an `import * as ns` binds the namespace, which
    /// is no definition.
    pub(super) fn required(&mut self, from: &str, b: &Binding) -> bool {
        let fi = self.file(from);
        let (Some(tree), Some(&at)) = (fi.tree.as_ref(), b.sites.first()) else { return false };
        tree.root_node()
            .descendant_for_byte_range(at, at)
            .and_then(|n| n.parent())
            .is_some_and(|p| matches!(p.kind(), "variable_declarator" | "import_require_clause"))
    }
}

impl Resolver<'_> {
    pub(super) fn js_module(&mut self, from: &str, module: &str) -> Option<String> {
        if module.starts_with("./") || module.starts_with("../") || module == "." || module == ".." {
            return self.js_file(&join(dir_of(from), module));
        }
        // a name for the project's own files (`paths`, `baseUrl`), then a workspace package by its
        // `package.json` name; any other package lives outside the repository
        if let Some(ts) = self.ts_paths(from) {
            // the pattern with the longest text before its `*` wins, as in the TypeScript compiler
            let mut aliases: Vec<&(String, Vec<String>)> = ts.aliases.iter().collect();
            aliases.sort_by_key(|(pattern, _)| std::cmp::Reverse(pattern.split('*').next().map_or(0, str::len)));
            for (pattern, targets) in aliases {
                let star = match pattern.split_once('*') {
                    Some((before, after)) => match module.strip_prefix(before).and_then(|m| m.strip_suffix(after)) {
                        Some(star) => star,
                        None => continue,
                    },
                    None if pattern == module => "",
                    None => continue,
                };
                if let Some(f) = targets.iter().find_map(|t| self.js_file(&t.replace('*', star))) {
                    return Some(f);
                }
            }
            if let Some(f) = ts.base.as_ref().and_then(|base| self.js_file(&join(base, module))) {
                return Some(f);
            }
        }
        self.js_package(from, module)
    }

    /// The file a workspace package of the repository opens for `module` (`@acme/utils`,
    /// `@acme/utils/math`): its `exports`, `module` or `main`, else its `index` file or a path inside it.
    fn js_package(&mut self, from: &str, module: &str) -> Option<String> {
        let mut parts = module.splitn(if module.starts_with('@') { 3 } else { 2 }, '/');
        let name = match (parts.next()?, module.starts_with('@')) {
            (scope, true) => format!("{scope}/{}", parts.next()?),
            (name, false) => name.to_string(),
        };
        let sub = parts.next().unwrap_or("");
        let pkgs = self.js_packages().get(&name)?;
        // two packages with one name: the one that shares the most leading directories with `from`
        let shared = |dir: &str| dir.split('/').zip(from.split('/')).take_while(|(a, b)| a == b).count();
        let pkg = Rc::clone(pkgs.iter().max_by_key(|p| (shared(&p.dir), std::cmp::Reverse(p.dir.len())))?);
        let key = if sub.is_empty() { ".".to_string() } else { format!("./{sub}") };
        let mut targets = export_targets(&pkg.exports, &key);
        if sub.is_empty() {
            targets.extend(pkg.module.iter().chain(&pkg.main).cloned());
        }
        // `types` may name a declaration that sits beside the implementation a later target names
        let found: Vec<String> = targets.iter().filter_map(|t| self.package_file(&pkg.dir, t)).collect();
        let declares = |f: &&String| DECLARATIONS.iter().any(|e| f.ends_with(e));
        if let Some(f) = found.iter().find(|f| !declares(f)).or(found.first()) {
            return Some(f.clone());
        }
        let rel = if sub.is_empty() { "index" } else { sub };
        self.js_file(&join(&pkg.dir, rel)).or_else(|| self.js_file(&join(&pkg.dir, &format!("src/{rel}"))))
    }

    /// `target` of the package at `dir`, or when it names a build output the repository does not
    /// hold (`./dist/index.js`), the source at the same path under `src/` (`src/index.ts`).
    fn package_file(&self, dir: &str, target: &str) -> Option<String> {
        let rel = target.trim_start_matches("./");
        if let Some(f) = self.js_file(&join(dir, rel)) {
            return Some(f);
        }
        let (_, below) = rel.split_once('/')?;
        let stem = DECLARATIONS.iter().chain(&[".js", ".mjs", ".cjs"]).find_map(|e| below.strip_suffix(e)).unwrap_or(below);
        self.js_file(&join(dir, &format!("src/{stem}")))
    }

    /// Workspace packages by their `package.json` name -> every package of that name.
    fn js_packages(&mut self) -> &HashMap<String, Vec<Rc<JsPackage>>> {
        if self.js_packages.is_none() {
            let mut m: HashMap<String, Vec<Rc<JsPackage>>> = HashMap::new();
            let mut found: Vec<&String> = self.files.iter().filter(|f| *f == "package.json" || f.ends_with("/package.json")).collect();
            found.sort();
            for f in found {
                let Some(v) = repo_text(&self.ix.root, f).and_then(|t| serde_json::from_str::<serde_json::Value>(&plain_json(&t)).ok()) else { continue };
                let Some(name) = v["name"].as_str().filter(|n| !n.is_empty()) else { continue };
                let field = |k: &str| v[k].as_str().map(str::to_string);
                let pkg = JsPackage { dir: dir_of(f).to_string(), exports: v["exports"].clone(), module: field("module"), main: field("main") };
                m.entry(name.to_string()).or_default().push(Rc::new(pkg));
            }
            self.js_packages = Some(m);
        }
        self.js_packages.as_ref().unwrap()
    }
}

impl Resolver<'_> {
    /// The file an import path without its extension stands for.
    fn js_file(&self, base: &str) -> Option<String> {
        if self.files.contains(base) && !base.ends_with(".js") {
            return Some(base.to_string());
        }
        // TypeScript sources are imported with the `.js` name they compile to
        let stem = base.strip_suffix(".js").or_else(|| base.strip_suffix(".mjs")).unwrap_or(base);
        let mut cands: Vec<String> = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"].iter().map(|e| format!("{stem}{e}")).collect();
        cands.push(base.to_string());
        // a declaration stands in only when no implementation has the name
        cands.extend(DECLARATIONS.iter().map(|e| format!("{stem}{e}")));
        cands.extend(["index.ts", "index.tsx", "index.js", "index.jsx", "index.mjs", "index.d.ts"].iter().map(|i| join(base, i)));
        self.first_file(&cands)
    }

    /// The import names of the nearest `tsconfig.json` or `jsconfig.json` above `from` that sets any.
    fn ts_paths(&mut self, from: &str) -> Option<Rc<TsPaths>> {
        let mut dir = dir_of(from).to_string();
        loop {
            if !self.ts.contains_key(&dir) {
                let found = ["tsconfig.json", "jsconfig.json"]
                    .iter()
                    .find_map(|name| ts_config(&self.ix.root, &join(&dir, name), 0, &mut HashMap::new()))
                    .map(|c| Rc::new(c.joined(&dir)));
                self.ts.insert(dir.clone(), found);
            }
            if let Some(found) = &self.ts[&dir] {
                return Some(Rc::clone(found));
            }
            if dir.is_empty() {
                return None;
            }
            dir = dir_of(&dir).to_string();
        }
    }
}

const DECLARATIONS: [&str; 3] = [".d.ts", ".d.mts", ".d.cts"];

/// The TypeScript 5.5 template for the folder of the config in use, replaced in `paths` and `baseUrl`.
const CONFIG_DIR: &str = "${configDir}";

/// `baseUrl` and `paths` of the config at `path`; what it does not set comes from the configs it
/// extends by a relative path, a later one of an `extends` list winning. The places of `paths` are
/// as written (`TsPaths::joined`). None when neither is set anywhere.
fn ts_config(root: &std::path::Path, path: &str, depth: usize, memo: &mut HashMap<(String, usize), Option<TsPaths>>) -> Option<TsPaths> {
    let key = (path.to_string(), depth);
    if let Some(r) = memo.get(&key) {
        return r.clone();
    }
    let r = ts_config_read(root, path, depth, memo);
    memo.insert(key, r.clone());
    r
}

fn ts_config_read(root: &std::path::Path, path: &str, depth: usize, memo: &mut HashMap<(String, usize), Option<TsPaths>>) -> Option<TsPaths> {
    let text = repo_text(root, path)?;
    let v: serde_json::Value = serde_json::from_str(&plain_json(&text)).ok()?;
    let (dir, options) = (dir_of(path), &v["compilerOptions"]);
    let base = options["baseUrl"].as_str().map(|b| if b.starts_with(CONFIG_DIR) { b.to_string() } else { join(dir, b) });
    let mut out = TsPaths { base, aliases: Vec::new(), paths_dir: dir.to_string() };
    for (pattern, places) in options["paths"].as_object().into_iter().flatten() {
        let places = places.as_array().into_iter().flatten().filter_map(|p| p.as_str()).map(String::from).collect();
        out.aliases.push((pattern.clone(), places));
    }
    let parents: Vec<&str> = match &v["extends"] {
        serde_json::Value::Array(list) => list.iter().filter_map(|e| e.as_str()).collect(),
        e => e.as_str().into_iter().collect(),
    };
    let mut inherited = TsPaths::default();
    for parent in parents.into_iter().filter(|e| e.starts_with('.') && depth < 4) {
        let parent = join(dir, parent);
        let parent = if parent.ends_with(".json") { parent } else { format!("{parent}.json") };
        if let Some(p) = ts_config(root, &parent, depth + 1, memo) {
            inherited.base = p.base.or(inherited.base);
            if !p.aliases.is_empty() {
                (inherited.aliases, inherited.paths_dir) = (p.aliases, p.paths_dir);
            }
        }
    }
    out.base = out.base.or(inherited.base);
    if out.aliases.is_empty() {
        (out.aliases, out.paths_dir) = (inherited.aliases, inherited.paths_dir);
    }
    (out.base.is_some() || !out.aliases.is_empty()).then_some(out)
}

/// The targets `exports` of a `package.json` gives `key` (`.`, `./math`), best first: an exact
/// key, else the pattern (`./*`) with the longest text before its `*`.
fn export_targets(exports: &serde_json::Value, key: &str) -> Vec<String> {
    let map = exports.as_object().filter(|o| o.keys().any(|k| k.starts_with('.')));
    let value = match map {
        // no subpath keys: the whole value is what `.` exports
        None => return if key == "." { conditions(exports, 0) } else { Vec::new() },
        Some(map) => map,
    };
    if let Some(v) = value.get(key) {
        return conditions(v, 0);
    }
    let best = value
        .iter()
        .filter_map(|(pattern, v)| {
            let (before, after) = pattern.split_once('*')?;
            let star = key.strip_prefix(before)?.strip_suffix(after)?;
            Some((before.len(), star, v))
        })
        .max_by_key(|(len, _, _)| *len);
    best.map(|(_, star, v)| conditions(v, 0).into_iter().map(|t| t.replace('*', star)).collect()).unwrap_or_default()
}

/// The paths an `exports` target stands for, best first. `types` leads because it names the
/// TypeScript source or its declarations, where `import` and `require` often name built JavaScript;
/// conditions outside the list (`browser`, `deno`) come last, by name.
fn conditions(v: &serde_json::Value, depth: usize) -> Vec<String> {
    const FIRST: [&str; 5] = ["types", "import", "default", "require", "node"];
    match v {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(a) => a.iter().flat_map(|x| conditions(x, depth)).collect(),
        serde_json::Value::Object(o) if depth < 4 => {
            let mut rest: Vec<_> = o.iter().filter(|(k, _)| !FIRST.contains(&k.as_str())).collect();
            rest.sort_by_key(|(k, _)| k.as_str());
            let rest = rest.into_iter().map(|(_, v)| v);
            FIRST.iter().filter_map(|c| o.get(*c)).chain(rest).flat_map(|x| conditions(x, depth + 1)).collect()
        }
        _ => Vec::new(),
    }
}

/// JSON with the comments and trailing commas a tsconfig may have taken out.
pub(super) fn plain_json(text: &str) -> String {
    let (mut out, mut chars) = (String::with_capacity(text.len()), text.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push(c);
                while let Some(d) = chars.next() {
                    out.push(d);
                    match d {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.by_ref().find(|&d| d == '\n');
                out.push('\n');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for d in chars.by_ref() {
                    if last == '*' && d == '/' {
                        break;
                    }
                    last = d;
                }
            }
            '}' | ']' => {
                // a comma before a closing bracket is the trailing one
                let kept = out.trim_end().len();
                if out[..kept].ends_with(',') {
                    out.truncate(kept - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// When `this` at `n` is bound by a method of an object literal: the literal's qualified name,
/// the method's own before its last dot ("" when the literal has none). An arrow function takes
/// `this` from the code around it.
fn literal_owner(fi: &FileInfo, n: Node) -> Option<String> {
    let f = std::iter::successors(n.parent(), |p| p.parent()).find(|p| FUNCTION_NODES.contains(&p.kind()) && p.kind() != "arrow_function")?;
    let p = f.parent()?;
    let (key, object) = match f.kind() {
        "method_definition" if p.kind() == "object" => (f.child_by_field_name("name")?, p),
        "function_expression" | "function" if p.kind() == "pair" && p.parent()?.kind() == "object" => (p.child_by_field_name("key")?, p.parent()?),
        _ => return None,
    };
    // the owner of the method a key names; a quoted key (`"get": function`) may name none, then a
    // sibling member's owner is the literal's
    let owner = |key: Node| {
        let name = if key.kind() == "string" { string_content(key, &fi.src) } else { text(key, &fi.src).to_string() };
        let line = key.start_position().row as i64 + 1;
        let d = fi.defs.iter().filter(|d| d.name == name && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start);
        d.and_then(|d| d.qualname.rsplit_once('.')).map(|(o, _)| o.to_string())
    };
    let mut cur = object.walk();
    let siblings: Vec<Node> = object.named_children(&mut cur).filter_map(|m| m.child_by_field_name("name").or_else(|| m.child_by_field_name("key"))).collect();
    Some(owner(key).or_else(|| siblings.into_iter().find_map(owner)).unwrap_or_default())
}

/// A binder declared with JavaScript `var`, which belongs to the whole function around it.
fn is_js_var(b: Node) -> bool {
    let mut n = b;
    while let Some(p) = n.parent() {
        match p.kind() {
            "variable_declarator" => return p.parent().is_some_and(|d| d.kind() == "variable_declaration"),
            "for_in_statement" => return p.child_by_field_name("kind").is_some_and(|k| k.kind() == "var"),
            "object_pattern" | "array_pattern" | "pair_pattern" | "assignment_pattern" | "rest_pattern" => n = p,
            _ => return false,
        }
    }
    false
}
