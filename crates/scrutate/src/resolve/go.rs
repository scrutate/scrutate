//! Go: import specs bind package names, the files of a package see each other, methods are
//! declared beside their type by receiver, embedded fields, packages by `go.mod` path.

use super::*;

pub(super) struct Go;

impl Rules for Go {
    const GRAMMARS: &'static [&'static str] = &["go"];
    const EMBEDDED_FIELDS: bool = true;

    fn bindings(n: Node, _path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
        go_bindings(n, src, out)
    }

    fn package_files(r: &mut Resolver, path: &str) -> Vec<String> {
        let dir = dir_of(path).to_string();
        r.go_dir(&dir)
    }

    // a method is declared outside its type, in any file of the package: its receiver names the type
    fn inherited(r: &mut Resolver, id: i64, name: &str, depth: usize, path: &str, qualname: &str) -> Option<i64> {
        let owner = strip_generics(qualname);
        for f in r.go_dir(dir_of(path)) {
            let gf = r.file(&f);
            let Some(tree) = gf.tree.as_ref() else { continue };
            let root = tree.root_node();
            let mut cur = root.walk();
            for m in root.named_children(&mut cur).filter(|n| n.kind() == "method_declaration") {
                let receiver = m.child_by_field_name("receiver").and_then(|r| r.named_child(0)).and_then(|r| r.child_by_field_name("type")).and_then(type_name);
                if m.child_by_field_name("name").map(|n| text(n, &gf.src)) == Some(name) && receiver.map(|r| text(r, &gf.src)) == Some(owner.as_str()) {
                    let line = m.start_position().row as i64 + 1;
                    if let Some(d) = gf.defs.iter().find(|d| d.name == name && d.start <= line && line <= d.end) {
                        return Some(d.id);
                    }
                }
            }
        }
        // a method promoted from an embedded field of the struct (`type Wrap struct { *Base }`)
        if depth > 4 {
            return None;
        }
        let fi = r.file(path);
        let mut embedded = Vec::new();
        let mut stack: Vec<Node> = def_node(&fi, id).into_iter().collect();
        while let Some(n) = stack.pop() {
            if n.kind() == "field_declaration" {
                if n.child_by_field_name("name").is_none() {
                    embedded.extend(n.child_by_field_name("type").and_then(type_name));
                }
                continue;
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        // an embedded `base.Base` is the `Base` of package `base`, not one of this package
        for leaf in embedded {
            let line = leaf.start_position().row as i64 + 1;
            let Ok((base, _)) = r.type_leaf(path, &fi, Lang::Go, leaf, line) else { continue };
            if let Some(m) = r.member(base, name, depth + 1) {
                return Some(m);
            }
        }
        None
    }

    fn module_of_chain(r: &mut Resolver, path: &str, fi: &FileInfo, segs: &[&str]) -> Option<Vec<String>> {
        r.bound_module(path, fi, Lang::Go, segs)
    }

    fn module_files(r: &mut Resolver, _from: &str, module: &str) -> Option<Vec<String>> {
        let best = if r.go_imports().is_empty() {
            // no go.mod: a GOPATH tree, where the import path ends with the directory
            r.go_dirs().keys().filter(|d| !d.is_empty() && (module == d.as_str() || module.ends_with(&format!("/{d}")))).max_by_key(|d| d.len())?.clone()
        } else {
            r.go_imports().get(module)?.clone()
        };
        Some(r.go_dir(&best))
    }

    fn import_files(r: &mut Resolver, from: &str, _fi: &FileInfo, _line: i64, module: &str) -> Option<Vec<String>> {
        Some(Self::module_files(r, from, module).unwrap_or_default())
    }
}

fn go_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    if n.kind() != "import_spec" {
        return false;
    }
    let Some(path) = n.child_by_field_name("path") else { return true };
    let module = string_content(path, src);
    let segs: Vec<&str> = module.split('/').collect();
    // `example.com/x/v2` is package `x`
    let mut last = *segs.last().unwrap_or(&"");
    if segs.len() > 1 && last.len() > 1 && last.starts_with('v') && last[1..].chars().all(|c| c.is_ascii_digit()) {
        last = segs[segs.len() - 2];
    }
    match n.child_by_field_name("name") {
        Some(a) if a.kind() == "dot" => out.push(Binding { local: "*".into(), module, item: None, sites: vec![] }),
        Some(a) if a.kind() == "blank_identifier" => {}
        Some(a) => out.push(Binding { local: text(a, src).into(), module, item: None, sites: vec![a.start_byte()] }),
        None => out.push(Binding { local: last.to_string(), module, item: None, sites: vec![] }),
    }
    true
}

impl Resolver<'_> {
    fn go_dirs(&mut self) -> &HashMap<String, Vec<String>> {
        if self.go_dirs.is_none() {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for f in self.files.iter().filter(|f| f.ends_with(".go")) {
                m.entry(dir_of(f).to_string()).or_default().push(f.clone());
            }
            for v in m.values_mut() {
                v.sort();
            }
            self.go_dirs = Some(m);
        }
        self.go_dirs.as_ref().unwrap()
    }

    /// Import path -> directory, for the directories under a `go.mod` (the nearest one above).
    fn go_imports(&mut self) -> &HashMap<String, String> {
        if self.go_imports.is_none() {
            let dirs: Vec<String> = self.go_dirs().keys().cloned().collect();
            let mut declared: HashMap<String, Option<String>> = HashMap::new();
            let mut m = HashMap::new();
            for d in dirs {
                let mut at = d.clone();
                loop {
                    let module = declared.entry(at.clone()).or_insert_with(|| repo_text(&self.ix.root, &join(&at, "go.mod")).and_then(|t| go_module_path(&t)));
                    if let Some(module) = module {
                        let rest = d[at.len()..].trim_start_matches('/');
                        m.insert(if rest.is_empty() { module.clone() } else { format!("{module}/{rest}") }, d.clone());
                        break;
                    }
                    if at.is_empty() {
                        break;
                    }
                    at = dir_of(&at).to_string();
                }
            }
            self.go_imports = Some(m);
        }
        self.go_imports.as_ref().unwrap()
    }

    fn go_dir(&mut self, dir: &str) -> Vec<String> {
        self.go_dirs().get(dir).cloned().unwrap_or_default()
    }
}

fn go_module_path(go_mod: &str) -> Option<String> {
    go_mod
        .lines()
        .find_map(|l| l.trim().strip_prefix("module").filter(|r| r.starts_with(char::is_whitespace)))
        .map(|m| m.split("//").next().unwrap_or("").trim().trim_matches('"').to_string())
        .filter(|m| !m.is_empty())
}
