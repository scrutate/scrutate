//! End-to-end tests of scope-aware `refs`, `deps` and `rdeps` on PHP: namespaces and `use`,
//! typed locals, parameters and properties, `$this`, `self::`, `static::` and `parent::`.

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
        let root = std::env::temp_dir().join(format!("scrutate_resolve_php_{}_{}", name, std::process::id()));
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
        v["result"].clone()
    }
    /// (path, line) of every occurrence -> path of the definition it resolved to, or the reason
    /// it stayed unresolved.
    fn resolved(&self, name: &str) -> BTreeMap<(String, u64), String> {
        let v = self.json(&["refs", name, "--json"]);
        let mut m = BTreeMap::new();
        for d in v["definitions"].as_array().unwrap() {
            for r in d["references"].as_array().unwrap() {
                m.insert((r["path"].as_str().unwrap().to_string(), r["line"].as_u64().unwrap()), d["path"].as_str().unwrap().to_string());
            }
        }
        for u in v["unresolved"].as_array().unwrap() {
            m.insert((u["path"].as_str().unwrap().to_string(), u["line"].as_u64().unwrap()), u["reason"].as_str().unwrap_or("-").to_string());
        }
        m
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

const SHAPE: &str = "<?php\nnamespace App\\Shapes;\n\nabstract class Shape\n{\n    public function describe() { return static::label() . self::label(); }\n    public static function label() { return \"shape\"; }\n    public function area() { return 0; }\n}\n";
const CIRCLE: &str = "<?php\nnamespace App\\Shapes;\n\nclass Circle extends Shape\n{\n    public function area() { return 3; }\n    public function twice() { return $this->area() * 2; }\n}\n";
const SQUARE: &str = "<?php\nnamespace App\\Geo;\n\nuse App\\Shapes\\Shape as Base;\n\nclass Square extends Base\n{\n    public function area()\n    {\n        return parent::area() + 4;\n    }\n}\n";
const APP: &str = "<?php
namespace App;

use App\\Shapes\\{Circle, Shape};
use App\\Geo\\Square;
use function App\\Geo\\area as geoArea;
use Vendor\\Lib\\Thing;

class App
{
    private Square $square;

    public function __construct(private Circle $circle) {}

    public function run(Shape $s, $x)
    {
        $c = new Circle();
        $a = $c->area();
        $b = $this->square->area();
        $d = $this->circle->area();
        $e = $s->area();
        $f = geoArea();
        $g = area();
        $h = (new Square())->area();
        $i = $x->area();
        $j = (new Thing())->area();
        $k = $this->make()->area();
        return $c->describe();
    }

    public function make(): Square { return new Square(); }
}
";

fn shapes(tag: &str) -> Repo {
    Repo::new(
        tag,
        &[
            ("src/Shapes/Shape.php", SHAPE),
            ("src/Shapes/Circle.php", CIRCLE),
            ("src/Geo/Square.php", SQUARE),
            ("src/Geo/helpers.php", "<?php\nnamespace App\\Geo;\n\nfunction area() { return 1; }\n"),
            ("src/global.php", "<?php\nfunction area() { return 2; }\n"),
            ("src/App.php", APP),
        ],
    )
}

#[test]
fn php_method_calls_follow_the_type_of_their_receiver() {
    let r = shapes("calls");
    let (shape, circle, square, helpers, global, app) =
        ("src/Shapes/Shape.php", "src/Shapes/Circle.php", "src/Geo/Square.php", "src/Geo/helpers.php", "src/global.php", "src/App.php");
    assert_eq!(
        r.resolved("area"),
        expect(&[
            (shape, 8, shape),
            (circle, 6, circle),
            // `$this` is the class around
            (circle, 7, circle),
            (square, 8, square),
            // `parent::` is the class it extends, named through a `use` alias
            (square, 10, shape),
            (helpers, 4, helpers),
            (global, 2, global),
            // `use function`
            (app, 6, helpers),
            // a local built with `new`
            (app, 18, circle),
            // a typed property, and a property the constructor declares
            (app, 19, square),
            (app, 20, circle),
            // a typed parameter
            (app, 21, shape),
            // a call through a `use function` alias
            (app, 22, helpers),
            // an unqualified call falls back to the global function
            (app, 23, global),
            (app, 24, square),
            (app, 25, "receiver"),
            // a class from outside the repository
            (app, 26, "external"),
            // the return type a method declares
            (app, 27, square),
        ])
    );
    // `self::` and `static::` are the class around; an inherited method is the base's
    assert_eq!(r.resolved("label"), expect(&[(shape, 6, shape), (shape, 7, shape)]));
    assert_eq!(r.resolved("describe"), expect(&[(shape, 6, shape), (app, 28, shape)]));
    let (_, out) = r.run(&["refs", "area"]);
    assert!(!out.contains("not resolved in this language"), "{out}");
}

#[test]
fn php_imports_name_the_files_of_their_classes() {
    let r = shapes("imports");
    let deps: Vec<(u64, Option<String>)> = r
        .json(&["deps", "src/App.php", "--json"])
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["line"].as_u64().unwrap(), d["resolved"].as_str().map(String::from)))
        .collect();
    let file = |p: &str| Some(p.to_string());
    assert_eq!(
        deps,
        [(4, file("src/Shapes/Circle.php")), (4, file("src/Shapes/Shape.php")), (5, file("src/Geo/Square.php")), (6, file("src/Geo/helpers.php")), (7, None)]
    );
    let (_, out) = r.run(&["rdeps", "src/Geo/Square.php"]);
    assert!(out.contains("src/App.php:5"), "{out}");
    assert!(!out.contains("could not be resolved"), "{out}");
}

#[test]
fn the_accept_fixture_resolves_a_method_through_a_typed_local() {
    let r = Repo::new(
        "accept",
        &[
            ("app/Models/User.php", "<?php\nnamespace App\\Models;\n\nclass User\n{\n    public function name() { return \"u\"; }\n}\n"),
            ("app/Http/Controller.php", "<?php\nnamespace App\\Http;\n\nuse App\\Models\\User;\n\nclass Controller\n{\n    public function show() { $u = new User(); return $u->name(); }\n}\n"),
        ],
    );
    assert_eq!(r.resolved("name"), expect(&[("app/Http/Controller.php", 8, "app/Models/User.php"), ("app/Models/User.php", 6, "app/Models/User.php")]));
}

#[test]
fn php_class_alias_namespace_and_method_names_match_in_any_case() {
    let str_php = "<?php\nnamespace App\\Util;\n\nclass Str\n{\n    public static function slug($s) { return $s; }\n}\n";
    let app = "<?php\nnamespace App;\n\nuse App\\Util\\Str as S;\n\nfunction run() {\n    s::SLUG(\"a\");\n    \\app\\util\\str::slug(\"b\");\n    $x = new s();\n    $x->Slug(\"c\");\n}\n";
    let r = Repo::new("any_case", &[("src/Util/Str.php", str_php), ("app.php", app)]);
    let s = "src/Util/Str.php";
    assert_eq!(r.resolved("slug"), expect(&[(s, 6, s), ("app.php", 7, s), ("app.php", 8, s), ("app.php", 10, s)]));
}

#[test]
fn a_variable_a_closure_captures_with_use_has_the_type_it_has_outside() {
    let a = "<?php\nclass User\n{\n    public function save() { return 1; }\n}\nfunction run(User $u) {\n    $f = function () use ($u) {\n        return $u->save();\n    };\n    $g = fn() => $u->save();\n}\n";
    let r = Repo::new("closure_use", &[("a.php", a)]);
    assert_eq!(r.resolved("save"), expect(&[("a.php", 4, "a.php"), ("a.php", 8, "a.php"), ("a.php", 10, "a.php")]));
}

#[test]
fn this_in_a_trait_calling_a_method_the_trait_lacks_is_a_receiver() {
    let a = "<?php\ntrait Greets\n{\n    public function hello() { return $this->word(); }\n}\nclass Person\n{\n    use Greets;\n    public function word() { return 'hi'; }\n}\n";
    let r = Repo::new("trait_this", &[("a.php", a)]);
    assert_eq!(r.resolved("word"), expect(&[("a.php", 4, "receiver"), ("a.php", 9, "a.php")]));
}

#[test]
fn a_call_on_a_static_property_goes_through_the_property_type() {
    let a = "<?php\nclass User\n{\n    public static User $current;\n    public function save() { return 1; }\n}\nfunction run() {\n    User::$current->save();\n}\n";
    let r = Repo::new("static_prop", &[("a.php", a)]);
    assert_eq!(r.resolved("save"), expect(&[("a.php", 5, "a.php"), ("a.php", 8, "a.php")]));
    assert_eq!(r.resolved("current"), expect(&[("a.php", 4, "field or key"), ("a.php", 8, "field or key")]));
}

#[test]
fn self_static_and_parent_match_in_any_case() {
    let a = "<?php\nclass Base\n{\n    public static function make() { return 1; }\n}\nclass Kid extends Base\n{\n    public static function build() { return 2; }\n    public static function run()\n    {\n        return SELF::build() + Static::build() + PARENT::make();\n    }\n}\n";
    let r = Repo::new("keyword_case", &[("a.php", a)]);
    assert_eq!(r.resolved("build"), expect(&[("a.php", 8, "a.php"), ("a.php", 11, "a.php")]));
    assert_eq!(r.resolved("make"), expect(&[("a.php", 4, "a.php"), ("a.php", 11, "a.php")]));
}

#[test]
fn a_variable_a_closure_captures_by_reference_has_the_type_it_has_outside() {
    let a = "<?php\nclass User\n{\n    public function save() { return 1; }\n}\nfunction run(User $u) {\n    $f = function () use (&$u) {\n        return $u->save();\n    };\n}\n";
    let r = Repo::new("closure_use_ref", &[("a.php", a)]);
    assert_eq!(r.resolved("save"), expect(&[("a.php", 4, "a.php"), ("a.php", 8, "a.php")]));
}

#[test]
fn refs_find_a_use_alias_written_in_another_case() {
    let app = "<?php\nnamespace App;\nuse function Lib\\phpfn as pf;\nuse Lib\\Tool as T;\nfunction upper() { return PF(); }\nfunction cls() { return t::make(); }\nfunction cls2() { return T::make(); }\n";
    let lib = "<?php\nnamespace Lib;\nfunction phpfn() { return 1; }\nclass Tool\n{\n    public static function make() { return 1; }\n}\n";
    let r = Repo::new("alias_case_refs", &[("app.php", app), ("lib.php", lib)]);
    assert_eq!(r.resolved("phpfn"), expect(&[("app.php", 3, "lib.php"), ("app.php", 5, "lib.php"), ("lib.php", 3, "lib.php")]));
    assert_eq!(r.resolved("Tool"), expect(&[("app.php", 4, "lib.php"), ("app.php", 6, "lib.php"), ("app.php", 7, "lib.php"), ("lib.php", 4, "lib.php")]));
}
