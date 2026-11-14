//! Rust: `use` trees, raw identifiers, names in macro arguments, `self` and `Self`, unit structs
//! and `Deref` holders, methods of `impl` blocks anywhere in the crate, modules by `mod` and
//! crate roots.

use super::*;

pub(super) struct Rust;

impl Rules for Rust {
    const GRAMMARS: &'static [&'static str] = &["rust"];
    const PATH_SEP: &'static str = "::";
    const SELF_NAMES: &'static [&'static str] = &["self"];
    const SELF_TYPE: Option<&'static str> = Some("Self");

    fn bindings(n: Node, _path: &str, src: &str, out: &mut Vec<Binding>) -> bool {
        if n.kind() == "use_declaration" {
            if let Some(a) = n.child_by_field_name("argument") {
                rust_use(a, "", src, out);
            }
            true
        } else {
            false
        }
    }

    // a raw identifier `r#type` is the name `type`
    fn raw(spelled: &str, name: &str) -> bool {
        spelled.strip_prefix("r#") == Some(name)
    }

    fn occurrence(r: &mut Resolver, path: &str, fi: &FileInfo, node: Node, name: &str, line: i64) -> Option<Resolution> {
        if node.parent().is_some_and(|p| p.kind() == "token_tree") {
            if node.prev_sibling().is_some_and(|p| p.kind() == "::") {
                return Some(macro_path(r, path, fi, node, name, line));
            }
            if let Some(why) = macro_token(node, &fi.src) {
                return Some(Err(why));
            }
            // a plain name a macro may use as anything: a local still hides it, a definition is not sure
            if macro_plain(node, &fi.src) {
                return Some(match r.resolve_bare(path, fi, Lang::Rust, node, name, line) {
                    Ok(_) => Err("macro argument"),
                    other => other,
                });
            }
        }
        None
    }

    // `self::x` and `super::x` inside inline `mod a { }` blocks count from those blocks first
    #[allow(clippy::too_many_arguments)]
    fn qualified(r: &mut Resolver, path: &str, fi: &FileInfo, lang: Lang, q: Node, node: Node, name: &str, line: i64) -> Resolution {
        let qt: String = text(q, &fi.src).chars().filter(|c| !c.is_whitespace()).collect();
        let segs: Vec<&str> = qt.split("::").collect();
        let ups = segs.iter().take_while(|s| **s == "super").count();
        let mods: Vec<Node> =
            std::iter::successors(node.parent(), |n| n.parent()).filter(|n| n.kind() == "mod_item" && n.child_by_field_name("body").is_some()).collect();
        if mods.is_empty() || !(segs[0] == "self" || ups > 0) || ups > mods.len() {
            return r.walk_qualified(path, fi, lang, q, node, name, line);
        }
        let module = |m: Node| {
            let row = m.start_position().row as i64 + 1;
            fi.defs.iter().find(|d| d.kind == "module" && d.start == row).map(|d| d.id)
        };
        let rest = &segs[if ups == 0 { 1 } else { ups }..];
        let mut cur = match mods.get(ups) {
            Some(&m) => Some(module(m).ok_or("not found")?),
            None => None,
        };
        for seg in rest.iter().copied().chain(std::iter::once(name)) {
            cur = Some(match cur {
                None => top_level(fi, seg).map(|d| d.id).ok_or("not found")?,
                Some(id) => r.member(id, seg, 0).ok_or("not found")?,
            });
        }
        cur.map(|id| (id, "qualified")).ok_or("not found")
    }

    fn is_receiver(_q: Node, qt: &str, node: Node) -> bool {
        qt == "Self" || (qt == "self" && node.parent().is_some_and(|p| p.kind() == "field_expression"))
    }

    fn constructed_call(r: &mut Resolver, path: &str, fi: &FileInfo, v: Node, f: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let holder = f.kind() == "scoped_identifier"
            && f.child_by_field_name("name").is_some_and(|n| text(n, src) == "new")
            && f.child_by_field_name("path").is_some_and(|p| RUST_DEREF.contains(&text(p, src)));
        if holder {
            r.constructed(path, fi, Lang::Rust, v.child_by_field_name("arguments")?.named_child(0)?)
        } else {
            None
        }
    }

    fn constructed_name(r: &mut Resolver, path: &str, fi: &FileInfo, v: Node) -> Option<Ty> {
        let line = v.start_position().row as i64 + 1;
        let id = r.resolve_bare(path, fi, Lang::Rust, v, text(v, &fi.src), line).ok()?.0;
        r.def_kind(id).filter(|k| k == "struct").map(|_| Ty::Local(id))
    }

    fn unwraps(name: &str) -> bool {
        RUST_DEREF.contains(&name)
    }

    fn outside_call(r: &mut Resolver, path: &str, fi: &FileInfo, lang: Lang, e: Node, f: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        // a closure's result can be any type, so is a type the caller picks (`collect`, `into`)
        let mut cur = e.walk();
        let closure = e.child_by_field_name("arguments").is_some_and(|a| a.named_children(&mut cur).any(|x| x.kind() == "closure_expression"));
        if let Some((obj, m)) = member_parts(f) {
            let Some(Ty::Foreign(t, args)) = r.type_of(path, fi, lang, obj) else { return None };
            return std_method(&t, &args, text(m, src)).unwrap_or_else(|| {
                // a method of a type that holds no repository type gives none, unless the caller picks its type
                let closed = !STD_GENERIC.contains(&t.as_str()) && args.iter().all(|a| a.as_ref().is_some_and(outside_only));
                (closed && !closure && !CALLER_TYPED.contains(&text(m, src))).then(|| Ty::Foreign(OUTSIDE.into(), Vec::new()))
            });
        }
        // a function of the std modules whose functions all return a type of their own (`std::fs::read`)
        let segs: Vec<&str> = match f.kind() {
            "scoped_identifier" => text(f.child_by_field_name("path")?, src).split("::").map(str::trim).collect(),
            "identifier" => vec![text(f, src)],
            _ => return None,
        };
        let full: Vec<&str> = match fi.bindings.iter().find(|b| b.local == segs[0]) {
            // `use std::fs;` binds `fs` to `std::fs`, `use std::fs::read;` binds `read` in `std::fs`
            Some(b) if f.kind() == "identifier" => b.module.split("::").collect(),
            Some(b) => b.module.split("::").chain(b.item.as_deref()).chain(segs[1..].iter().copied()).filter(|s| !s.is_empty()).collect(),
            None => segs,
        };
        let concrete = full.len() == 2 && matches!(full[0], "std" | "core") && STD_CONCRETE_MODULES.contains(&full[1]);
        (concrete && !closure).then(|| Ty::Foreign(OUTSIDE.into(), Vec::new()))
    }

    // impl blocks may live in another file of the crate
    fn inherited(r: &mut Resolver, id: i64, name: &str, _depth: usize, path: &str, qualname: &str) -> Option<i64> {
        let want = format!("{}.{name}", strip_generics(qualname));
        let rows = r.ix.named(name).ok()?;
        let common = |p: &str| p.split('/').zip(path.split('/')).take_while(|(a, b)| a == b).count();
        if let Some(own) = rows.into_iter().filter(|r| strip_generics(&r.qualname) == want).max_by_key(|r| common(&r.path)) {
            return Some(own.id);
        }
        // a default method of a trait the type implements (`impl Greet for S {}`, also
        // `impl Greet for inner::S` outside S's module): an impl block whose type is this one
        let owner = strip_generics(qualname);
        let bare = owner.rsplit('.').next().unwrap_or(&owner).to_string();
        let mut impls: Vec<Symbol> = r.ix.named(&bare).ok()?.into_iter().filter(|s| s.kind == "impl").collect();
        impls.retain(|s| strip_generics(&s.qualname) == owner || rust_impl_of(r, s, id));
        impls.sort_by_key(|s| std::cmp::Reverse(common(&s.path)));
        for s in impls {
            for t in r.bases(s.id) {
                if let Some(m) = r.declared_member(t, name) {
                    return Some(m);
                }
            }
        }
        None
    }

    fn module_of_chain(r: &mut Resolver, path: &str, fi: &FileInfo, segs: &[&str]) -> Option<Vec<String>> {
        let b = fi.bindings.iter().find(|b| b.local == segs[0]);
        let mut full: Vec<String> = Vec::new();
        match b {
            Some(b) if !matches!(segs[0], "crate" | "self" | "super") => {
                full.extend(b.module.split("::").filter(|s| !s.is_empty()).map(String::from));
                full.extend(b.item.clone());
            }
            _ => full.push(segs[0].to_string()),
        }
        full.extend(segs[1..].iter().map(|s| s.to_string()));
        r.rust_module(path, &full.iter().map(|s| s.as_str()).collect::<Vec<_>>())
    }

    fn module_files(r: &mut Resolver, from: &str, module: &str) -> Option<Vec<String>> {
        let segs: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
        r.rust_module(from, &segs)
    }

    // the longest prefix of the path a file holds, then the inline modules down to the item
    fn unfiled_item(r: &mut Resolver, from: &str, module: &str, item: &str) -> Option<i64> {
        let segs: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
        let filed = (1..segs.len()).rev().find_map(|k| r.rust_module(from, &segs[..k]).map(|files| (k, files)));
        // no file prefix: an inline module of `from` itself (`use other::work`)
        let own = || (!matches!(*segs.first()?, "crate" | "self" | "super")).then(|| (0, vec![from.to_string()]));
        let (k, files) = filed.or_else(own)?;
        let mut cur = r.lookup_top(&files, segs[k], 0)?;
        for seg in &segs[k + 1..] {
            cur = r.member(cur, seg, 0)?;
        }
        r.member(cur, item, 0)
    }

    fn import_files(r: &mut Resolver, from: &str, fi: &FileInfo, line: i64, module: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for b in statement_bindings(fi, line, &["use_declaration"]) {
            let full = match &b.item {
                Some(item) if b.module.is_empty() => item.clone(),
                Some(item) => format!("{}::{item}", b.module),
                None => b.module.clone(),
            };
            let segs: Vec<&str> = full.split("::").filter(|s| !s.is_empty()).collect();
            // the longest prefix that is a module: `crate::a::thing` is in `crate::a`
            out.extend((1..=segs.len()).rev().find_map(|n| r.rust_module(from, &segs[..n])).and_then(|v| v.into_iter().next()));
        }
        if out.is_empty() {
            out.extend(Self::module_files(r, from, module).and_then(|v| v.into_iter().next()));
        }
        Some(out)
    }
}

/// Whether impl block `s` is one of type `id`: its `for` type resolves to `id`.
fn rust_impl_of(r: &mut Resolver, s: &Symbol, id: i64) -> bool {
    let fi = r.file(&s.path);
    let Some(tree) = fi.tree.as_ref() else { return false };
    // an impl block has no name of its own for def_node to match: it is the impl_item on its line
    let row = (s.start - 1).max(0) as usize;
    let mut stack = vec![tree.root_node()];
    let mut item = None;
    while let Some(n) = stack.pop() {
        if n.start_position().row > row || n.end_position().row < row {
            continue;
        }
        if n.kind() == "impl_item" && n.start_position().row == row {
            item = Some(n);
            break;
        }
        let mut cur = n.walk();
        stack.extend(n.named_children(&mut cur));
    }
    let Some(leaf) = item.and_then(|n| n.child_by_field_name("type")).and_then(type_name) else { return false };
    matches!(r.type_leaf(&s.path, &fi, Lang::Rust, leaf, s.start), Ok((t, _)) if t == id)
}

/// Standard Rust types whose methods a call reaches through `Deref` to the type they hold.
const RUST_DEREF: &[&str] = &["Box", "Rc", "Arc", "Ref", "RefMut", "Cow", "MutexGuard", "RwLockReadGuard", "RwLockWriteGuard", "Pin", "ManuallyDrop"];

/// Standard types that hold values of other types, so a method of one may return a repository type.
const STD_GENERIC: &[&str] = &[
    "Option",
    "Result",
    "Vec",
    "VecDeque",
    "array_type",
    "HashMap",
    "BTreeMap",
    "HashSet",
    "BTreeSet",
    "BinaryHeap",
    ITER,
    "Box",
    "Rc",
    "Arc",
    "Weak",
    "Cell",
    "RefCell",
    "Mutex",
    "RwLock",
    "Cow",
    "OnceCell",
    "OnceLock",
    "LazyCell",
    "LazyLock",
    "Pin",
    "ManuallyDrop",
    "PhantomData",
    "Reverse",
];

/// Methods whose return type the caller picks by inference, whatever the receiver.
const CALLER_TYPED: &[&str] = &[
    "into",
    "try_into",
    "parse",
    "collect",
    "sum",
    "product",
    "get",
    "as_ref",
    "as_mut",
    "borrow",
    "borrow_mut",
    "deref",
    "deref_mut",
    "downcast",
    "downcast_ref",
    "downcast_mut",
];

/// The std modules none of whose functions returns a type its caller picks (std 1.99.1).
const STD_CONCRETE_MODULES: &[&str] = &["env", "fs", "io", "path", "process"];

/// The type an iterator over values of its one argument stands for.
const ITER: &str = "Iterator";

/// Whether a value of type `t` holds no repository type.
fn outside_only(t: &Ty) -> bool {
    match t {
        Ty::Local(_) => false,
        Ty::Foreign(_, args) => args.iter().all(|a| a.as_ref().is_some_and(outside_only)),
    }
}

/// What a method of a standard type returns, from its signature in the std documentation
/// (doc.rust-lang.org/1.99.1/std: `Option`, `Result`, `Vec` and slices, `HashMap`, `HashSet`,
/// `String` and `str`, `Iterator`). None when the method is not listed; Some(None) when its
/// type is the receiver's type argument and that is not known.
fn std_method(t: &str, args: &[Option<Ty>], m: &str) -> Option<Option<Ty>> {
    let arg = |i: usize| args.get(i).cloned().flatten();
    let of = |n: &str, a: Vec<Option<Ty>>| Some(Ty::Foreign(n.to_string(), a));
    let plain = |n: &str| of(n, Vec::new());
    let same = || of(t, args.to_vec());
    if m == "clone" {
        return Some(same());
    }
    let ret = match t {
        "Option" => match m {
            "unwrap" | "expect" | "unwrap_or" | "unwrap_or_default" | "unwrap_or_else" | "unwrap_unchecked" | "insert" | "get_or_insert"
            | "get_or_insert_with" => arg(0),
            "is_some" | "is_none" | "is_some_and" | "is_none_or" => plain("bool"),
            "as_ref" | "as_mut" | "as_deref" | "as_deref_mut" | "cloned" | "copied" | "take" | "take_if" | "or" | "or_else" | "xor" | "filter" | "inspect"
            | "replace" => same(),
            "ok_or" | "ok_or_else" => of("Result", vec![arg(0), None]),
            "iter" | "iter_mut" | "into_iter" => of(ITER, vec![arg(0)]),
            _ => return None,
        },
        "Result" => match m {
            "unwrap" | "expect" | "unwrap_or" | "unwrap_or_default" | "unwrap_or_else" | "unwrap_unchecked" => arg(0),
            "unwrap_err" | "expect_err" => arg(1),
            "ok" => of("Option", vec![arg(0)]),
            "err" => of("Option", vec![arg(1)]),
            "is_ok" | "is_err" | "is_ok_and" | "is_err_and" => plain("bool"),
            "as_ref" | "as_mut" | "inspect" | "inspect_err" => same(),
            "map_err" => of("Result", vec![arg(0), None]),
            "iter" | "into_iter" => of(ITER, vec![arg(0)]),
            _ => return None,
        },
        "Vec" | "VecDeque" | "array_type" => match m {
            "len" | "capacity" => plain("usize"),
            "is_empty" | "contains" | "starts_with" | "ends_with" => plain("bool"),
            "first" | "last" | "first_mut" | "last_mut" | "get_mut" | "pop" | "pop_front" | "pop_back" | "front" | "back" => of("Option", vec![arg(0)]),
            "iter" | "iter_mut" | "into_iter" | "drain" => of(ITER, vec![arg(0)]),
            "remove" | "swap_remove" => arg(0),
            "to_vec" => of("Vec", vec![arg(0)]),
            _ => return None,
        },
        "HashMap" | "BTreeMap" => match m {
            "get_mut" | "remove" | "insert" => of("Option", vec![arg(1)]),
            "contains_key" | "is_empty" => plain("bool"),
            "len" => plain("usize"),
            "keys" | "into_keys" => of(ITER, vec![arg(0)]),
            "values" | "values_mut" | "into_values" => of(ITER, vec![arg(1)]),
            _ => return None,
        },
        "HashSet" | "BTreeSet" => match m {
            "contains" | "insert" | "remove" | "is_empty" => plain("bool"),
            "len" => plain("usize"),
            "iter" | "into_iter" | "drain" => of(ITER, vec![arg(0)]),
            "take" => of("Option", vec![arg(0)]),
            _ => return None,
        },
        "String" | "str" => match m {
            "len" | "capacity" => plain("usize"),
            "is_empty" | "starts_with" | "ends_with" | "contains" | "is_char_boundary" | "eq_ignore_ascii_case" | "is_ascii" => plain("bool"),
            "trim" | "trim_start" | "trim_end" | "trim_matches" | "trim_start_matches" | "trim_end_matches" | "as_str" | "to_string" | "to_owned"
            | "to_lowercase" | "to_uppercase" | "to_ascii_lowercase" | "to_ascii_uppercase" | "replace" | "replacen" | "repeat" => plain("String"),
            "strip_prefix" | "strip_suffix" => of("Option", vec![plain("String")]),
            "find" | "rfind" => of("Option", vec![plain("usize")]),
            "split" | "rsplit" | "splitn" | "rsplitn" | "split_whitespace" | "split_terminator" | "lines" | "matches" => of(ITER, vec![plain("String")]),
            "chars" => of(ITER, vec![plain("char")]),
            "bytes" => of(ITER, vec![plain("u8")]),
            _ => return None,
        },
        ITER => match m {
            "next" | "next_back" | "last" | "nth" | "min" | "max" | "min_by" | "max_by" | "min_by_key" | "max_by_key" | "find" | "reduce" => {
                of("Option", vec![arg(0)])
            }
            "count" => plain("usize"),
            "any" | "all" => plain("bool"),
            "position" | "rposition" => of("Option", vec![plain("usize")]),
            "rev" | "skip" | "take" | "filter" | "peekable" | "fuse" | "step_by" | "cycle" | "inspect" | "skip_while" | "take_while" | "cloned" | "copied"
            | "by_ref" => same(),
            _ => return None,
        },
        _ => return None,
    };
    Some(ret)
}

fn rust_push(full: &str, local: &str, sites: Vec<usize>, out: &mut Vec<Binding>) {
    let (module, item) = full.rsplit_once("::").unwrap_or(("", full));
    out.push(Binding { local: local.to_string(), module: module.to_string(), item: Some(item.to_string()), sites });
}

fn rust_use(n: Node, prefix: &str, src: &str, out: &mut Vec<Binding>) {
    let cat = |p: &str, t: &str| {
        if p.is_empty() {
            t.to_string()
        } else if t.is_empty() {
            p.to_string()
        } else {
            format!("{p}::{t}")
        }
    };
    match n.kind() {
        "self" => {
            let last = prefix.rsplit("::").next().unwrap_or(prefix);
            rust_push(prefix, last, vec![n.start_byte()], out);
        }
        "identifier" | "crate" | "super" | "metavariable" => rust_push(&cat(prefix, text(n, src)), text(n, src), vec![n.start_byte()], out),
        "scoped_identifier" => {
            let name = n.child_by_field_name("name");
            let local = name.map(|x| text(x, src)).unwrap_or("");
            rust_push(&cat(prefix, text(n, src)), local, name.map(|x| vec![x.start_byte()]).unwrap_or_default(), out);
        }
        "use_as_clause" => {
            let (Some(path), Some(alias)) = (n.child_by_field_name("path"), n.child_by_field_name("alias")) else { return };
            let mut sites = vec![alias.start_byte()];
            sites.extend(path.child_by_field_name("name").or(Some(path)).map(|x| x.start_byte()));
            rust_push(&cat(prefix, text(path, src)), text(alias, src), sites, out);
        }
        "scoped_use_list" => {
            let p = n.child_by_field_name("path").map(|x| text(x, src)).unwrap_or("");
            if let Some(list) = n.child_by_field_name("list") {
                rust_use(list, &cat(prefix, p), src, out);
            }
        }
        "use_list" => {
            let mut cur = n.walk();
            for ch in n.named_children(&mut cur) {
                rust_use(ch, prefix, src, out);
            }
        }
        "use_wildcard" => {
            let p = n.named_child(0).map(|x| text(x, src)).unwrap_or("");
            out.push(Binding { local: "*".into(), module: cat(prefix, p), item: None, sites: vec![] });
        }
        _ => {}
    }
}

impl Resolver<'_> {
    /// The `mod name;` declarations of a Rust file: (line, name, the file of that module).
    pub fn rust_mods(&mut self, from: &str) -> Vec<(i64, String, Option<String>)> {
        let fi = self.file(from);
        let (Some(Lang::Rust), Some(tree)) = (fi.lang, &fi.tree) else { return Vec::new() };
        let mut out = Vec::new();
        let mut stack = vec![(tree.root_node(), self.rust_own_dir(from))];
        while let Some((n, dir)) = stack.pop() {
            let mut cur = n.walk();
            for ch in n.named_children(&mut cur) {
                if ch.kind() != "mod_item" {
                    continue;
                }
                let Some(name) = ch.child_by_field_name("name").map(|x| text(x, &fi.src).to_string()) else { continue };
                let sub = join(&dir, &name);
                match ch.child_by_field_name("body") {
                    Some(body) => stack.push((body, sub)),
                    None => out.push((ch.start_position().row as i64 + 1, name, self.rust_mod_file(&sub))),
                }
            }
        }
        out.sort();
        out
    }

    /// The Rust files that may declare `mod` for `path`: the module files of its parent directory.
    pub fn rust_mod_parents(&self, path: &str) -> Vec<String> {
        let d = dir_of(&Self::rust_mod_dir(path)).to_string();
        let mut cands = vec![format!("{d}/mod.rs"), format!("{d}/lib.rs"), format!("{d}/main.rs"), format!("{d}.rs")];
        if d.is_empty() {
            cands = vec!["lib.rs".into(), "main.rs".into()];
        }
        cands.push(join(&d, "build.rs"));
        if self.rust_target_dir(&d) {
            let mut roots: Vec<String> = self.files.iter().filter(|f| f.ends_with(".rs") && dir_of(f) == d).cloned().collect();
            roots.sort();
            cands.extend(roots);
        }
        cands.retain(|c| self.files.contains(c) && c != path && (!c.ends_with("/build.rs") && c != "build.rs" || self.rust_root_file(c)));
        cands.dedup();
        cands
    }
}

impl Resolver<'_> {
    fn rust_mod_file(&self, dir: &str) -> Option<String> {
        if dir.is_empty() {
            return self.first_file(&["lib.rs".into(), "main.rs".into()]);
        }
        self.first_file(&[format!("{dir}.rs"), format!("{dir}/mod.rs"), format!("{dir}/lib.rs"), format!("{dir}/main.rs")])
    }

    /// Directory that stands for the module of a Rust file (`a/b.rs` and `a/b/mod.rs` -> `a/b`).
    pub(super) fn rust_mod_dir(path: &str) -> String {
        let base = path.rsplit('/').next().unwrap_or(path);
        if matches!(base, "mod.rs" | "lib.rs" | "main.rs") {
            dir_of(path).to_string()
        } else {
            join(dir_of(path), base.trim_end_matches(".rs"))
        }
    }

    /// The directory a Rust file's own `mod` and `self::` start at: its module's, or its own
    /// directory when it is a crate root.
    fn rust_own_dir(&self, path: &str) -> String {
        if self.rust_root_file(path) {
            dir_of(path).to_string()
        } else {
            Self::rust_mod_dir(path)
        }
    }

    /// `build.rs` beside a `Cargo.toml`, or a file directly in a target folder: a crate root.
    fn rust_root_file(&self, path: &str) -> bool {
        match path.rsplit_once('/').unwrap_or(("", path)) {
            (dir, "build.rs") => self.files.contains(&join(dir, "Cargo.toml")),
            (dir, base) => base.ends_with(".rs") && self.rust_target_dir(dir),
        }
    }

    /// A package's `src/bin`, `tests`, `examples` or `benches`, where Cargo takes each file for a crate.
    fn rust_target_dir(&self, dir: &str) -> bool {
        let (up, last) = dir.rsplit_once('/').unwrap_or(("", dir));
        let package = match last {
            "bin" => match up.rsplit_once('/').unwrap_or(("", up)) {
                (p, "src") => p,
                _ => return false,
            },
            "tests" | "examples" | "benches" => up,
            _ => return false,
        };
        self.files.contains(&join(package, "Cargo.toml"))
    }

    fn rust_crate_root(&self, path: &str) -> Option<String> {
        if self.rust_root_file(path) {
            return Some(dir_of(path).to_string());
        }
        let mut d = dir_of(path).to_string();
        loop {
            let c = |f: &str| if d.is_empty() { f.to_string() } else { format!("{d}/{f}") };
            // a module below a target folder belongs to the crates of the files there
            if self.files.contains(&c("lib.rs")) || self.files.contains(&c("main.rs")) || self.rust_target_dir(&d) {
                return Some(d);
            }
            if d.is_empty() {
                return None;
            }
            d = dir_of(&d).to_string();
        }
    }

    /// Workspace crates by their `use` name (`grep-matcher` -> `grep_matcher`) -> crate root directory.
    fn crates(&mut self) -> &HashMap<String, String> {
        if self.crates.is_none() {
            let mut m = HashMap::new();
            let mut roots: Vec<&String> = self.files.iter().filter(|f| f.ends_with("/lib.rs") || *f == "lib.rs").collect();
            roots.sort();
            for f in roots {
                let dir = dir_of(f);
                let pkg = if dir == "src" { "" } else { dir.strip_suffix("/src").unwrap_or(dir) };
                let toml = if pkg.is_empty() { "Cargo.toml".to_string() } else { format!("{pkg}/Cargo.toml") };
                let Some(t) = repo_text(&self.ix.root, &toml) else { continue };
                if let Some(name) = cargo_package_name(&t) {
                    m.entry(name.replace('-', "_")).or_insert_with(|| dir.to_string());
                }
            }
            self.crates = Some(m);
        }
        self.crates.as_ref().unwrap()
    }

    fn rust_module(&mut self, from: &str, segs: &[&str]) -> Option<Vec<String>> {
        let first = *segs.first()?;
        let (mut dir, mut i) = match first {
            "crate" => (self.rust_crate_root(from)?, 1),
            "self" => (self.rust_own_dir(from), 1),
            "super" => {
                let mut d = self.rust_own_dir(from);
                let mut i = 0;
                while segs.get(i) == Some(&"super") {
                    d = dir_of(&d).to_string();
                    i += 1;
                }
                (d, i)
            }
            other => {
                // a file beside a crate root such as `tests/it.rs` is its module only when it declares it
                let root = self.rust_root_file(from);
                let declared = !root || self.rust_mods(from).iter().any(|(_, n, _)| n == other);
                if let Some(d) = self.crates().get(other).cloned() {
                    (d, 1)
                } else if declared && self.rust_mod_file(&join(&self.rust_own_dir(from), other)).is_some() {
                    (self.rust_own_dir(from), 0)
                } else if root {
                    return None;
                } else {
                    (self.rust_crate_root(from)?, 0) // 2015-edition paths start at the crate root
                }
            }
        };
        while i < segs.len() {
            dir = join(&dir, segs[i]);
            self.rust_mod_file(&dir)?;
            i += 1;
        }
        if self.rust_root_file(from) && dir == dir_of(from) {
            return Some(vec![from.to_string()]);
        }
        if let Some(f) = self.rust_mod_file(&dir) {
            return Some(vec![f]);
        }
        // the root of a module below a target folder: any of the crates there
        let mut roots: Vec<String> = self.files.iter().filter(|f| f.ends_with(".rs") && dir_of(f) == dir && self.rust_target_dir(&dir)).cloned().collect();
        roots.sort();
        (!roots.is_empty()).then_some(roots)
    }
}

/// What the tokens around a name in a Rust macro argument, which the grammar leaves without
/// syntax, say it is: a field or a method after `.`, a field name in `{ name: value }`.
fn macro_token(n: Node, src: &str) -> Option<&'static str> {
    let next = n.next_sibling();
    if n.prev_sibling().is_some_and(|p| p.kind() == ".") {
        let call = next.is_some_and(|x| x.kind() == "::" || (x.kind() == "token_tree" && text(x, src).starts_with('(')));
        return Some(if call { "receiver" } else { "field or key" });
    }
    if macro_local(n, src) {
        return Some("local variable");
    }
    let braced = n.parent().is_some_and(|p| text(p, src).starts_with('{'));
    (braced && next.is_some_and(|x| x.kind() == ":")).then_some("field or key")
}

/// The standard macros whose arguments are expressions (std 1.99.1, doc.rust-lang.org/1.99.1/std/#macros):
/// a plain name in them is a value, read as outside a macro.
const EXPRESSION_MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "dbg",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "eprint",
    "eprintln",
    "format",
    "format_args",
    "panic",
    "print",
    "println",
    "todo",
    "unimplemented",
    "unreachable",
    "vec",
    "write",
    "writeln",
];

/// Whether `n` is a plain name in the arguments of a macro invocation that may take it as
/// anything: not a call (`f(`), a path (`a::`, `::b`) or a nested macro (`m!`), and not in a
/// standard macro that takes expressions. Names in an attribute or a `macro_rules!` body are not.
fn macro_plain(n: Node, src: &str) -> bool {
    let next = n.next_sibling();
    let call = next.is_some_and(|x| matches!(x.kind(), "::" | "!") || (x.kind() == "token_tree" && text(x, src).starts_with('(')));
    if call || n.prev_sibling().is_some_and(|p| p.kind() == "::") {
        return false;
    }
    // the innermost macro: a nested `m!(..)` is tokens too, named by the identifier before its `!`
    let mut tt = n.parent();
    while let Some(t) = tt.filter(|t| t.kind() == "token_tree") {
        let named = |m: Node| text(m, src).rsplit("::").next().is_some_and(|m| !EXPRESSION_MACROS.contains(&m));
        let bang = t.prev_sibling().filter(|b| b.kind() == "!");
        if let Some(m) = bang.and_then(|b| b.prev_sibling()).filter(|m| m.kind() == "identifier") {
            return named(m);
        }
        match t.parent() {
            Some(p) if p.kind() == "macro_invocation" => return p.child_by_field_name("macro").is_some_and(named),
            p => tt = p,
        }
    }
    false
}

/// `a::b::name` in a macro's tokens, resolved as the same path outside a macro; a path that does
/// not start with a plain segment (`::a::name`, `Vec::<u8>::name`) is not decided.
fn macro_path(r: &mut Resolver, path: &str, fi: &FileInfo, n: Node, name: &str, line: i64) -> Resolution {
    let mut segs = Vec::new();
    let mut sep = n.prev_sibling();
    while let Some(s) = sep.filter(|s| s.kind() == "::") {
        let Some(seg) = s.prev_sibling().filter(|p| matches!(p.kind(), "identifier" | "crate" | "self" | "super")) else {
            return Err("macro argument");
        };
        segs.push(seg);
        sep = seg.prev_sibling();
    }
    segs.reverse();
    let qt = segs.iter().map(|s| text(*s, &fi.src)).collect::<Vec<_>>().join("::");
    r.walk_path(path, fi, Lang::Rust, segs[0], &qt, segs[0], n, name, line)
}

/// Whether a name in a Rust macro argument is bound in the tokens before it: a parameter of a
/// closure (`|(a, name)| ..`) or a `let`, in its own token tree or one around it.
fn macro_local(n: Node, src: &str) -> bool {
    let name = text(n, src);
    let binds = |tokens: &[Node]| {
        let mut names = Vec::new();
        for t in tokens {
            token_idents(*t, src, &mut names);
        }
        names.contains(&name)
    };
    let mut child = n;
    while let Some(tt) = child.parent().filter(|p| p.kind() == "token_tree") {
        let mut cur = tt.walk();
        let kids: Vec<Node> = tt.children(&mut cur).collect();
        let at = kids.iter().position(|k| k.id() == child.id()).unwrap_or(kids.len());
        let mut open: Option<usize> = None;
        for (i, k) in kids.iter().enumerate().take(at) {
            match k.kind() {
                "|" => match open.take() {
                    Some(o) if binds(&kids[o + 1..i]) => return true,
                    Some(_) => {}
                    None => open = Some(i),
                },
                "let" => {
                    let bound = i + 1 + usize::from(kids.get(i + 1).is_some_and(|m| m.kind() == "mut"));
                    if bound == at || (bound < at && binds(&kids[bound..bound + 1])) {
                        return true;
                    }
                }
                // `for (a, name) in ..`: the pattern before `in`
                // the grammar may give `for` and `in` in tokens as identifiers
                _ if text(*k, src) == "for" => {
                    let end = kids[i..].iter().position(|k| text(*k, src) == "in").map_or(i + 1, |p| i + p);
                    if at < end || binds(&kids[i + 1..end]) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        // between the bars of a closure: one of its parameters
        if open.is_some() {
            return true;
        }
        child = tt;
    }
    false
}

fn token_idents<'s>(n: Node, src: &'s str, out: &mut Vec<&'s str>) {
    if n.kind() == "identifier" {
        out.push(text(n, src));
    }
    let mut cur = n.walk();
    for c in n.named_children(&mut cur) {
        token_idents(c, src, out);
    }
}

pub(super) fn cargo_package_name(toml: &str) -> Option<String> {
    let mut in_pkg = false;
    for line in toml.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_pkg = l == "[package]";
        } else if in_pkg {
            if let Some(v) = l.strip_prefix("name").map(str::trim_start).and_then(|v| v.strip_prefix('=')) {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}
