//! End-to-end tests of scope-aware `refs`: per language, a small repository with two same-named
//! definitions in different modules, and every occurrence checked against the one it must land on.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str, files: &[(&str, &str)]) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_resolve_e2e_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, body) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
        }
        let r = Repo { root };
        assert_eq!(r.run(&["index", "--no-text"]).0, 0);
        r
    }
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(BIN).args(args).current_dir(&self.root).stdin(Stdio::null()).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }
    fn json(&self, args: &[&str]) -> serde_json::Value {
        let (code, out) = self.run(args);
        assert_eq!(code, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        // an answer leaves in its envelope; `check` prints a report of its own
        if v["schema"] == 2 {
            v["result"].clone()
        } else {
            v
        }
    }
    /// (path, line) of every occurrence -> path of the definition it resolved to ("-" = unresolved).
    fn resolved(&self, name: &str) -> BTreeMap<(String, u64), String> {
        let v = self.json(&["refs", name, "--json"]);
        let mut m = BTreeMap::new();
        for d in v["definitions"].as_array().unwrap() {
            for r in d["references"].as_array().unwrap() {
                m.insert((r["path"].as_str().unwrap().to_string(), r["line"].as_u64().unwrap()), d["path"].as_str().unwrap().to_string());
            }
        }
        for u in v["unresolved"].as_array().unwrap() {
            m.insert((u["path"].as_str().unwrap().to_string(), u["line"].as_u64().unwrap()), "-".to_string());
        }
        m
    }
    /// `path:line` of the occurrences `refs NAME --def DEF` ties to that definition, sorted.
    fn refs_of(&self, name: &str, def: &str) -> Vec<String> {
        let v = self.json(&["refs", name, "--def", def, "--json"]);
        let refs = v["definitions"][0]["references"].as_array().unwrap_or_else(|| panic!("{v}"));
        let mut out: Vec<String> = refs.iter().map(|o| format!("{}:{}", o["path"].as_str().unwrap(), o["line"])).collect();
        out.sort();
        out
    }
    /// `line reason` of each unresolved occurrence of `name` in `file`.
    fn reasons(&self, name: &str, file: &str) -> Vec<String> {
        let v = self.json(&["refs", name, "--json"]);
        let rows = v["unresolved"].as_array().unwrap().iter().filter(|o| o["path"] == file);
        rows.map(|o| format!("{} {}", o["line"], o["reason"].as_str().unwrap())).collect()
    }
    /// The qualified names of a `callers` answer, sorted.
    fn callers(&self, target: &str) -> Vec<String> {
        let v = self.json(&["callers", target, "--json"]);
        let mut out: Vec<String> = v.as_array().unwrap().iter().map(|h| h["name"].as_str().unwrap_or("").to_string()).collect();
        out.sort();
        out
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn expect(want: &[(&str, u64, &str)]) -> BTreeMap<(String, u64), String> {
    want.iter().map(|(p, l, d)| ((p.to_string(), *l), d.to_string())).collect()
}

#[test]
fn python_two_areas() {
    let r = Repo::new(
        "py",
        &[
            ("shapes/circle.py", "def area(r):\n    return 3 * r * r\n"),
            ("shapes/square.py", "def area(s):\n    return s * s\n"),
            ("shapes/__init__.py", "from .circle import area\n"),
            (
                "app.py",
                "from shapes import square\nfrom shapes import area\nimport shapes.circle as c\n\n\ndef main(area_of=None):\n    a = area(1)\n    b = square.area(2)\n    d = c.area(3)\n    return a + b + d\n\n\n# area in a comment\n",
            ),
        ],
    );
    let (c, s) = ("shapes/circle.py", "shapes/square.py");
    assert_eq!(
        r.resolved("area"),
        expect(&[
            (c, 1, c),
            (s, 1, s),
            ("shapes/__init__.py", 1, c),
            ("app.py", 2, c),
            ("app.py", 7, c),
            ("app.py", 8, s),
            ("app.py", 9, c),
            ("app.py", 13, "-")
        ])
    );

    let v = r.json(&["refs", "area", "--def", "shapes/square.py", "--json"]);
    let defs = v["definitions"].as_array().unwrap();
    assert_eq!(defs.len(), 1, "{v}");
    let lines: Vec<u64> = defs[0]["references"].as_array().unwrap().iter().map(|o| o["line"].as_u64().unwrap()).collect();
    assert_eq!(lines, [8, 1], "app.py:8 and the definition itself: {v}");
    let (_, text) = r.run(&["refs", "area", "--def", "shapes/square.py:1"]);
    assert!(text.starts_with("shapes/square.py:1-2  [function] area  (2 references)"), "{text}");
    assert_eq!(r.run(&["refs", "area", "--def", "app.py"]).0, 2, "no definition there is an error");

    let v = r.json(&["refs", "area", "--textual", "--json"]);
    assert!(v["callers"].is_array() && v["references"].is_array(), "the old shape stays reachable: {v}");
}

#[test]
fn javascript_and_typescript_two_areas() {
    let r = Repo::new(
        "js",
        &[
            ("lib/a.js", "export function area() { return 1; }\n"),
            ("lib/b.js", "export function area() { return 2; }\n"),
            ("main.js", "import {area} from './lib/a.js';\nimport * as b from './lib/b';\narea();\nb.area();\n"),
            ("geo/point.ts", "export class Shape {\n  area() { return 1; }\n  twice() { return this.area() * 2; }\n}\n"),
            ("geo/box.ts", "export function area() { return 2; }\n"),
            ("geo/index.ts", "export { area } from './box';\n"),
            ("use.ts", "import { Shape } from './geo/point';\nimport { area } from './geo';\nconst s = new Shape();\narea();\ns.area();\n"),
        ],
    );
    let (a, b, p, x) = ("lib/a.js", "lib/b.js", "geo/point.ts", "geo/box.ts");
    assert_eq!(
        r.resolved("area"),
        expect(&[
            (a, 1, a),
            (b, 1, b),
            ("main.js", 1, a),
            ("main.js", 3, a),
            ("main.js", 4, b),
            (p, 2, p),
            (p, 3, p),
            (x, 1, x),
            ("geo/index.ts", 1, x),
            ("use.ts", 2, x),
            ("use.ts", 4, x),
            ("use.ts", 5, p), // `s` is built with `new Shape()`
        ])
    );
}

#[test]
fn rust_two_areas() {
    let r = Repo::new(
        "rs",
        &[
            ("Cargo.toml", "[package]\nname = \"shapes\"\nversion = \"0.1.0\"\n"),
            ("src/lib.rs", "pub mod app;\npub mod circle;\npub mod square;\npub use circle::area;\n"),
            ("src/circle.rs", "pub fn area() -> u32 {\n    1\n}\n"),
            ("src/square.rs", "pub fn area() -> u32 {\n    2\n}\n\npub fn twice() -> u32 {\n    area() * 2\n}\n"),
            (
                "src/app.rs",
                "use crate::area;\nuse crate::square;\n\npub fn run() -> u32 {\n    area()\n        + square::area()\n        + crate::circle::area()\n}\n",
            ),
        ],
    );
    let (c, s) = ("src/circle.rs", "src/square.rs");
    assert_eq!(
        r.resolved("area"),
        expect(&[
            (c, 1, c),
            (s, 1, s),
            (s, 6, s),
            ("src/lib.rs", 4, c),
            ("src/app.rs", 1, c),
            ("src/app.rs", 5, c),
            ("src/app.rs", 6, s),
            ("src/app.rs", 7, c)
        ])
    );
}

#[test]
fn rust_a_path_in_a_macro_argument_is_resolved_as_a_path() {
    let r = Repo::new(
        "rs_macro_path",
        &[
            ("Cargo.toml", "[package]\nname = \"mp\"\nversion = \"0.1.0\"\n"),
            ("src/util.rs", "pub fn work() -> i32 {\n    1\n}\n"),
            ("src/other.rs", "pub fn work() -> i32 {\n    2\n}\n"),
            (
                "src/main.rs",
                "mod util;\nmod other;\nuse other::work;\n\nfn caller() {\n    println!(\"{}\", util::work());\n    let v = vec![crate::util::work()];\n    mymac!(util::work);\n    work();\n}\n",
            ),
        ],
    );
    let (u, o, m) = ("src/util.rs", "src/other.rs", "src/main.rs");
    assert_eq!(r.resolved("work"), expect(&[(u, 1, u), (o, 1, o), (m, 3, o), (m, 6, u), (m, 7, u), (m, 8, u), (m, 9, o)]));
}

#[test]
fn javascript_a_shorthand_property_is_a_use_of_the_name() {
    let r = Repo::new(
        "js_shorthand",
        &[
            ("lib.js", "export function helper() {\n  return 1;\n}\n"),
            ("main.js", "import { helper } from './lib.js';\nconst table = { helper };\nconst other = { helper: 2 };\ntable.helper();\n"),
        ],
    );
    assert_eq!(r.refs_of("helper", "lib.js:1"), ["lib.js:1", "main.js:1", "main.js:2"]);
}

#[test]
fn javascript_the_name_of_a_function_expression_is_its_own_inside_it() {
    let r = Repo::new("js_fn_expr", &[("s.js", "function load() {\n  return 1;\n}\nvar h = function load() {\n  return load();\n};\nload();\n")]);
    assert_eq!(r.refs_of("load", "s.js:1"), ["s.js:1", "s.js:7"]);
}

#[test]
fn javascript_the_name_of_a_generator_or_class_expression_is_its_own_inside_it() {
    let r = Repo::new(
        "js_gen_class_expr",
        &[(
            "s.js",
            "function load() {\n  return 1;\n}\nvar g = function* load() {\n  yield load();\n};\nclass Box {}\nvar C = class Box {\n  m() {\n    return new Box();\n  }\n};\nload();\nnew Box();\nvar p = function* (load) {\n  yield load;\n};\n",
        )],
    );
    assert_eq!(r.refs_of("load", "s.js:1"), ["s.js:1", "s.js:13"]);
    assert_eq!(r.refs_of("Box", "s.js:7"), ["s.js:14", "s.js:7"]);
}

#[test]
fn javascript_refs_list_a_use_under_an_alias_in_a_file_that_never_spells_the_name() {
    let r = Repo::new(
        "js_alias_refs",
        &[
            ("lib/m.js", "function helper() {\n  return 1;\n}\nmodule.exports = helper;\n"),
            ("lib/e.js", "export default function shout() {\n  return 2;\n}\n"),
            ("lib/main.js", "const h = require(\"./m\");\nimport s from \"./e\";\n\nfunction start() {\n  return h() + s();\n}\n"),
        ],
    );
    assert_eq!(r.refs_of("helper", "lib/m.js:1"), ["lib/m.js:1", "lib/m.js:4", "lib/main.js:5"]);
    assert_eq!(r.refs_of("shout", "lib/e.js:1"), ["lib/e.js:1", "lib/main.js:5"]);
}

#[test]
fn rust_a_use_of_an_inline_module_item_is_that_item() {
    let r = Repo::new(
        "rs_inline_mod",
        &[
            ("Cargo.toml", "[package]\nname = \"im\"\nversion = \"0.1.0\"\n"),
            (
                "src/main.rs",
                "mod util {\n    pub fn work() -> i32 {\n        1\n    }\n}\nmod other {\n    pub fn work() -> i32 {\n        2\n    }\n}\nuse other::work;\n\nfn caller() -> i32 {\n    work() + util::work()\n}\n",
            ),
        ],
    );
    let m = "src/main.rs";
    assert_eq!(r.refs_of("work", "src/main.rs:7"), [format!("{m}:11"), format!("{m}:14"), format!("{m}:7")]);
    assert_eq!(r.refs_of("work", "src/main.rs:2"), [format!("{m}:14"), format!("{m}:2")]);
    assert!(r.reasons("work", m).is_empty());
}

#[test]
fn rust_a_trait_default_method_is_found_on_the_implementing_type() {
    let r = Repo::new(
        "rs_trait_default",
        &[
            ("Cargo.toml", "[package]\nname = \"td\"\nversion = \"0.1.0\"\n"),
            ("src/lib.rs", "pub trait Greet {\n    fn hello(&self) -> u8 {\n        1\n    }\n}\npub struct S;\nimpl Greet for S {}\n\npub fn run(s: S) -> u8 {\n    s.hello()\n        + S.hello()\n}\n"),
        ],
    );
    assert_eq!(r.refs_of("hello", "src/lib.rs:2"), ["src/lib.rs:10", "src/lib.rs:11", "src/lib.rs:2"]);
}

#[test]
fn go_a_method_promoted_through_an_embedded_struct_is_found() {
    let r = Repo::new(
        "go_promoted",
        &[
            ("go.mod", "module ex\n\ngo 1.22\n"),
            (
                "main.go",
                "package main\n\ntype Base struct{}\n\nfunc (b Base) Hello() int {\n\treturn 1\n}\n\ntype Wrap struct {\n\t*Base\n}\n\ntype Outer struct {\n\tWrap\n}\n\nfunc main() {\n\tv := Wrap{}\n\tv.Hello()\n\tv.Base.Hello()\n\to := Outer{}\n\to.Hello()\n}\n",
            ),
        ],
    );
    assert_eq!(r.refs_of("Hello", "main.go:5"), ["main.go:19", "main.go:20", "main.go:22", "main.go:5"]);
}

#[test]
fn javascript_a_member_of_require_a_dynamic_import_and_ts_import_require_bind_names() {
    let r = Repo::new(
        "js_require_forms",
        &[
            ("lib/n.js", "export function x() {\n  return 1;\n}\nexport function other() {\n  return 2;\n}\n"),
            ("lib/lazy.js", "export function later() {\n  return 3;\n}\n"),
            (
                "main.js",
                "const x = require('./lib/n').x;\nasync function go() {\n  const { later } = await import('./lib/lazy');\n  return x() + later();\n}\n",
            ),
            ("app.ts", "import m = require('./lib/n');\nm.other();\n"),
        ],
    );
    assert_eq!(r.refs_of("x", "lib/n.js:1"), ["lib/n.js:1", "main.js:1", "main.js:1", "main.js:4"]);
    assert_eq!(r.refs_of("later", "lib/lazy.js:1"), ["lib/lazy.js:1", "main.js:3", "main.js:4"]);
    assert_eq!(r.refs_of("other", "lib/n.js:4"), ["app.ts:2", "lib/n.js:4"]);
}

#[test]
fn go_a_method_promoted_from_another_package_s_type_is_that_type_s() {
    let r = Repo::new(
        "go_promoted_pkg",
        &[
            ("go.mod", "module ex\n\ngo 1.22\n"),
            ("base/base.go", "package base\n\ntype Base struct{}\n\nfunc (b Base) Hello() int {\n\treturn 1\n}\n"),
            (
                "main.go",
                "package main\n\nimport \"ex/base\"\n\ntype Base struct{}\n\nfunc (b Base) Hello() int {\n\treturn 2\n}\n\ntype Ext struct {\n\tbase.Base\n}\n\nfunc run(e Ext) int {\n\treturn e.Hello()\n}\n",
            ),
        ],
    );
    assert_eq!(r.refs_of("Hello", "base/base.go:5"), ["base/base.go:5", "main.go:16"]);
    assert_eq!(r.refs_of("Hello", "main.go:7"), ["main.go:7"]);
}

#[test]
fn rust_a_trait_default_method_is_found_on_a_type_in_a_module() {
    let r = Repo::new(
        "rs_trait_default_mod",
        &[
            ("Cargo.toml", "[package]\nname = \"tdm\"\nversion = \"0.1.0\"\n"),
            (
                "src/lib.rs",
                "pub trait Greet {\n    fn hello(&self) -> u8 {\n        1\n    }\n}\npub mod inner {\n    pub struct T;\n    pub struct U;\n    impl super::Greet for U {}\n}\nimpl Greet for inner::T {}\n\npub fn run(t: inner::T, u: inner::U) -> u8 {\n    t.hello()\n        + u.hello()\n}\n",
            ),
        ],
    );
    assert_eq!(r.refs_of("hello", "src/lib.rs:2"), ["src/lib.rs:14", "src/lib.rs:15", "src/lib.rs:2"]);
}

#[test]
fn refs_through_an_alias_keep_only_uses_of_the_aliased_definition() {
    let r = Repo::new(
        "js_alias_shadow",
        &[
            ("u.js", "export function jsfn() { return 1; }\n"),
            (
                "m.js",
                "import { jsfn as g } from \"./u\";\nexport function caller() { return g(); }\nexport function shadowed() { const g = () => 2; return g(); }\n",
            ),
        ],
    );
    let v = r.json(&["refs", "jsfn", "--json"]);
    let groups: Vec<String> = v["definitions"].as_array().unwrap().iter().map(|d| format!("{}:{}", d["path"].as_str().unwrap(), d["start"])).collect();
    assert_eq!(groups, ["u.js:1"], "{v}");
    assert_eq!(r.refs_of("jsfn", "u.js:1"), ["m.js:1", "m.js:2", "u.js:1"]);
}

#[test]
fn python_a_class_attribute_hides_an_import_in_the_class_body_only() {
    let r = Repo::new(
        "py_class_attr",
        &[
            ("util.py", "def helper():\n    return 1\n"),
            (
                "k.py",
                "from util import helper as h\n\nclass K:\n    h = 5\n    j = h + 1\n\n    def m(self):\n        return h()\n\ndef use():\n    return h()\n",
            ),
        ],
    );
    assert_eq!(r.refs_of("helper", "util.py:1"), ["k.py:1", "k.py:11", "k.py:8", "util.py:1"]);
    assert_eq!(r.reasons("h", "k.py"), ["4 local variable", "5 local variable"]);
}

#[test]
fn python_class_body_bindings_follow_the_order_and_scopes_python_runs_them_in() {
    let r = Repo::new("py_class_scopes", &[("util.py", "def helper():\n    return 1\n"), ("k.py", "from util import helper as h\n\nclass A:\n    if True:\n        h = 5\n    a = h\n\nclass B:\n    h, x = 1, 2\n    b = h\n\nclass C:\n    def h(self):\n        return 1\n    c = h\n\nclass D:\n    h = h\n\nclass E:\n    h = 5\n    e = [h for _ in range(2)]\n\nclass F:\n    for h in range(2):\n        pass\n    f = h\n\nclass G:\n    h: int = 5\n    g = h\n")]);
    // the import is read by `h = h`'s right side (line 18) and inside a comprehension (line 22);
    // every other `h` is bound in its class body first: in an `if`, a tuple, a `for`, an annotation
    assert_eq!(r.refs_of("helper", "util.py:1"), ["k.py:1", "k.py:18", "k.py:22", "util.py:1"]);
}

#[test]
fn go_two_areas() {
    let r = Repo::new(
        "go",
        &[
            ("go.mod", "module example.com/shapes\n\ngo 1.22\n"),
            ("circle/area.go", "package circle\n\nfunc Area() int { return 1 }\n"),
            ("square/area.go", "package square\n\nfunc Area() int { return 2 }\n"),
            ("square/twice.go", "package square\n\nfunc Twice() int { return Area() * 2 }\n"),
            ("main.go", "package main\n\nimport (\n\t\"example.com/shapes/circle\"\n\tsq \"example.com/shapes/square\"\n)\n\nfunc main() {\n\tprintln(circle.Area())\n\tprintln(sq.Area())\n}\n"),
        ],
    );
    let (c, s) = ("circle/area.go", "square/area.go");
    assert_eq!(r.resolved("Area"), expect(&[(c, 3, c), (s, 3, s), ("square/twice.go", 3, s), ("main.go", 9, c), ("main.go", 10, s)]));
}

#[test]
fn javascript_var_belongs_to_the_function_and_let_to_the_block() {
    let use_js = "import { helper } from './lib';\nfunction viaVar(flag) {\n  if (flag) {\n    var helper = other();\n  }\n  return helper();\n}\nfunction viaLet(flag) {\n  if (flag) {\n    let helper = other();\n  }\n  return helper();\n}\nfunction viaConst(flag) {\n  {\n    const helper = 3;\n  }\n  for (var i = 0; i < 1; i++) {\n    var other = helper();\n  }\n  return helper();\n}\n";
    let r = Repo::new("js_var", &[("lib.js", "export function helper() { return 1; }\n"), ("use.js", use_js)]);
    let h = "lib.js";
    assert_eq!(
        r.resolved("helper"),
        expect(&[
            (h, 1, h),
            ("use.js", 1, h),
            ("use.js", 4, "-"),
            ("use.js", 6, "-"),
            ("use.js", 10, "-"),
            ("use.js", 12, h),
            ("use.js", 16, "-"),
            ("use.js", 19, h),
            ("use.js", 21, h)
        ])
    );
}

#[test]
fn c_names_resolve_through_includes_to_the_definition_a_declaration_stands_for() {
    let main = "#include \"util/util.h\"\n#include <stdio.h>\n\nstatic int add_twice(int x) {\n    int add = x;\n    return add + add;\n}\n\nint main(void) {\n    printf(\"%d\", add(1, 2));\n    return add_twice(FAST);\n}\n";
    let r = Repo::new(
        "c",
        &[
            ("util/util.h", "#ifndef UTIL_H\n#define UTIL_H\nint add(int a, int b);\nenum mode { FAST, SLOW };\n#endif\n"),
            ("util/util.c", "#include \"util.h\"\n\nint add(int a, int b) { return a + b; }\n"),
            ("other/add.c", "static int add(int a) { return a; }\nint use(void) { return add(3); }\n"),
            ("src/main.c", main),
        ],
    );
    let (u, o, m) = ("util/util.c", "other/add.c", "src/main.c");
    // the header's declaration and a call through the include both stand for the definition in
    // the file named like the header; a static function of another file is that file's own
    assert_eq!(r.resolved("add"), expect(&[("util/util.h", 3, u), (u, 3, u), (m, 5, "-"), (m, 6, "-"), (m, 10, u), (o, 1, o), (o, 2, o)]));
    assert_eq!(r.reasons("add", m), ["5 local variable", "6 local variable", "6 local variable"]);
    // an enumerator is in the scope of its enum
    assert_eq!(r.resolved("FAST"), expect(&[("util/util.h", 4, "util/util.h"), (m, 11, "util/util.h")]));
    assert_eq!(r.reasons("printf", m), ["10 external"]);
    assert_eq!(r.callers("util/util.c:3"), ["main"]);
    let deps = r.json(&["deps", m, "--json"]);
    let resolved: Vec<&str> = deps.as_array().unwrap().iter().map(|d| d["resolved"].as_str().unwrap_or("-")).collect();
    assert_eq!(resolved, ["util/util.h", "-"], "{deps}");
}

#[test]
fn cpp_names_resolve_through_namespaces_classes_and_bases() {
    let hpp = "#pragma once\nnamespace geo {\nclass Shape {\npublic:\n    virtual double area() const;\n    double twice() const { return area() * 2; }\n};\ndouble dist(double a, double b);\n}\n";
    let cpp = "#include \"shape.hpp\"\nnamespace geo {\ndouble Shape::area() const { return 1; }\ndouble dist(double a, double b) { return a - b; }\n}\n";
    let main = "#include \"geo/shape.hpp\"\n#include <vector>\n\nclass Square : public geo::Shape {\npublic:\n    double area() const { return 4; }\n    double both() const { return area() + twice(); }\n};\n\nint main() {\n    geo::Shape s;\n    std::vector<int> v;\n    return geo::dist(s.area(), 1) + v.size();\n}\n";
    let r = Repo::new("cpp", &[("geo/shape.hpp", hpp), ("geo/shape.cpp", cpp), ("app/main.cpp", main)]);
    let (h, c, m) = ("geo/shape.hpp", "geo/shape.cpp", "app/main.cpp");
    assert_eq!(r.resolved("dist"), expect(&[(h, 8, c), (c, 4, c), (m, 13, c)]));
    // a member of the class around, the out-of-class definition a declaration stands for, and a
    // call on a value, whose type is not followed
    assert_eq!(r.resolved("area"), expect(&[(h, 5, c), (h, 6, c), (c, 3, c), (m, 6, m), (m, 7, m), (m, 13, "-")]));
    assert_eq!(r.reasons("area", m), ["13 receiver"]);
    // a member of a base class
    assert_eq!(r.resolved("twice"), expect(&[(h, 6, h), (m, 7, h)]));
    assert_eq!(r.reasons("vector", m), ["2 comment or string", "12 external"]);
    assert_eq!(r.callers(&format!("{c}:3")), ["geo.Shape.twice", "main"]);
}

#[test]
fn csharp_names_resolve_through_namespaces_usings_and_typed_values() {
    let shape = "namespace Acme.Models;\n\npublic interface IShape { double Area(); }\n\npublic partial class Circle : IShape\n{\n    private double r;\n    public Circle(double r) { this.r = r; }\n    public double Area() => 3 * r * r;\n    public static Circle Unit() => new Circle(1);\n}\n";
    let extra = "namespace Acme.Models\n{\n    public partial class Circle\n    {\n        public Report Summary() { return new Report(Area()); }\n    }\n\n    public class Report\n    {\n        public Report(double v) { }\n        public bool Ok() { return true; }\n    }\n}\n";
    let program = "using System;\nusing Acme.Models;\nusing M = Acme.Models;\n\nnamespace Acme.App\n{\n    public class Program\n    {\n        static bool Run(IShape s)\n        {\n            var c = new Circle(2);\n            Console.WriteLine(c.Area());\n            M.Circle u = Circle.Unit();\n            return s.Area() > 0 && u.Summary().Ok();\n        }\n    }\n}\n";
    let r = Repo::new("csharp", &[("Models/Shape.cs", shape), ("Models/Circle.Extra.cs", extra), ("App/Program.cs", program)]);
    let (s, e, p) = ("Models/Shape.cs", "Models/Circle.Extra.cs", "App/Program.cs");
    // a variable built with `new`, a parameter typed by an interface, the other part of a partial class
    assert_eq!(r.refs_of("Area", &format!("{s}:9")), [format!("{p}:12"), format!("{e}:5"), format!("{s}:9")]);
    assert_eq!(r.refs_of("Area", &format!("{s}:3")), [format!("{p}:14"), format!("{s}:3")]);
    // a static call, a chain typed by declared return types, a type through an alias
    assert_eq!(r.refs_of("Unit", &format!("{s}:10")), [format!("{p}:13"), format!("{s}:10")]);
    assert_eq!(r.refs_of("Summary", &format!("{e}:5")), [format!("{p}:14"), format!("{e}:5")]);
    assert_eq!(r.refs_of("Ok", &format!("{e}:11")), [format!("{p}:14"), format!("{e}:11")]);
    assert_eq!(r.reasons("Console", p), ["12 external"]);
    assert_eq!(r.reasons("r", s), ["7 field or key", "8 local variable", "8 field or key", "8 local variable", "9 field or key", "9 field or key"]);
    // a call through the interface may run the implementation: it comes undecided
    assert_eq!(r.callers(&format!("{s}:9")), ["Acme.App.Program.Run", "Acme.Models.Circle.Summary"]);
    let deps = r.json(&["deps", p, "--json"]);
    let mut resolved: Vec<&str> = deps.as_array().unwrap().iter().filter_map(|d| d["resolved"].as_str()).collect();
    resolved.sort();
    resolved.dedup();
    assert_eq!(resolved, [e, s], "{deps}");
}

/// The resolver reads the C, C++ and C# snapshot samples: each call and use lands on its definition.
#[test]
fn the_c_cpp_and_csharp_samples_resolve() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
    let read = |n: &str| fs::read_to_string(dir.join(n)).unwrap();
    let (c, cpp, cs) = (read("sample.c"), read("sample.cpp"), read("sample.cs"));
    let r = Repo::new("samples", &[("sample.c", &c), ("sample.cpp", &cpp), ("sample.cs", &cs)]);
    assert_eq!(r.resolved("add"), expect(&[("sample.c", 4, "sample.c"), ("sample.c", 8, "sample.c"), ("sample.c", 16, "sample.c")]));
    assert_eq!(r.refs_of("add", "sample.c:8"), ["sample.c:16", "sample.c:4", "sample.c:8"]);
    assert_eq!(r.refs_of("dist", "sample.cpp:16"), ["sample.cpp:16", "sample.cpp:18"]);
    assert_eq!(r.refs_of("Point", "sample.cpp:14"), ["sample.cpp:14", "sample.cpp:15", "sample.cpp:16", "sample.cpp:16", "sample.cpp:18"]);
    assert_eq!(r.refs_of("Format", "sample.cs:8"), ["sample.cs:7", "sample.cs:8"]);
    assert_eq!(r.refs_of("Name", "sample.cs:5"), ["sample.cs:5", "sample.cs:6", "sample.cs:7"]);
    for name in ["add", "dist", "Point", "Format", "Name", "Greeter", "RED"] {
        let v = r.json(&["refs", name, "--json"]);
        assert_eq!(v["unresolved"], serde_json::json!([]), "{name}: {v}");
    }
}

#[test]
fn a_std_method_on_a_call_result_is_external_and_a_macro_argument_is_undecided() {
    let r = Repo::new(
        "std_chain",
        &[
            ("Cargo.toml", "[package]\nname = \"rep\"\nversion = \"0.1.0\"\n"),
            ("src/lib.rs", "pub mod report;\npub mod meta;\npub mod user;\n"),
            ("src/report.rs", "pub struct Report;\n\nimpl Report {\n    pub fn ok(&self) -> bool {\n        true\n    }\n}\n\npub fn text() -> u8 {\n    1\n}\n\npub struct Other;\n\nimpl Other {\n    pub fn ok(&self) -> bool {\n        false\n    }\n}\n"),
            ("src/meta.rs", "pub fn meta(file: &std::fs::File) -> bool {\n    file.metadata().ok().is_some()\n}\n"),
            (
                "src/user.rs",
                "use crate::report::{text, Report};\n\npub fn user(v: Vec<Report>) -> bool {\n    v.first().unwrap().ok()\n}\n\npub fn args() {\n    mymac!(text, 3);\n    println!(\"{}\", text);\n}\n\npub fn maps(k: u8) -> bool {\n    let mut m = std::collections::HashMap::new();\n    m.insert(k, Report);\n    m.get(&k).unwrap().ok()\n}\n",
            ),
        ],
    );
    let v = r.json(&["callers", "Report.ok", "--json"]);
    let names: Vec<&str> = v.as_array().unwrap().iter().map(|h| h["name"].as_str().unwrap()).collect();
    // the map's values are not written, so its call of `ok` is undecided, not external
    assert_eq!(names, ["user", "maps"], "{v}");
    let (_, text) = r.run(&["callers", "Report.ok"]);
    assert!(text.contains("(1 of these call a definition of that name the resolver could not identify"), "{text}");
    let v = r.json(&["refs", "text", "--json"]);
    let why: Vec<(u64, &str)> = v["unresolved"].as_array().unwrap().iter().map(|u| (u["line"].as_u64().unwrap(), u["reason"].as_str().unwrap())).collect();
    assert_eq!(why, [(8, "macro argument")], "{v}");
    assert_eq!(r.resolved("text").get(&("src/user.rs".to_string(), 9)).map(String::as_str), Some("src/report.rs"));
}

#[test]
fn javascript_this_in_a_method_under_a_quoted_key_is_the_object_literal() {
    let r = Repo::new(
        "js_quoted_key",
        &[("a.js", "const api = {\n  \"get\": function () {\n    return this.put();\n  },\n  \"del\"() {\n    return this.put();\n  },\n  put() {\n    return 1;\n  },\n};\n")],
    );
    assert_eq!(r.refs_of("put", "a.js:8"), ["a.js:3", "a.js:6", "a.js:8"]);
}
