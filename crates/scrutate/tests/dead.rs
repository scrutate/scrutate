//! `scrutate dead` and the `dead_code` tool: definitions nothing in the repository uses, entry
//! points left out, and the ones the resolver cannot decide kept apart with the reason.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str, files: &[(&str, &str)]) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_dead_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, body) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
        }
        Repo { root }
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(BIN).arg("--root").arg(&self.root).args(args).stdin(Stdio::null()).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn json(&self, args: &[&str]) -> Value {
        let (_, out) = self.run(&[args, &["--json"]].concat());
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const APP_PY: &str = r#"import sys


def helper():
    return 1


def unused_private():
    return 2


def caller():
    return helper()


@app.route("/x")
def handler():
    return 3


def by_name():
    return 4


def dispatch(obj):
    return getattr(obj, "by_name")()


def only_in_a_comment():
    return 5


def recursive_alone(n):
    return recursive_alone(n - 1)


def main():
    # only_in_a_comment is kept for later
    caller()
    dispatch(sys)


if __name__ == "__main__":
    main()
"#;

const TEST_PY: &str = "from app import caller\n\n\ndef test_caller():\n    assert caller() == 1\n";

const WEB_JS: &str = "export function shown() { return 1 }\nfunction hidden() { return 2 }\nmodule.exports = { other }\nfunction other() {}\nconst handlers = { onSave() { return 3 } }\nregister(handlers)\n";

const LIB_RS: &str = r#"use std::fmt;

pub fn api() -> u32 {
    used_inside()
}

fn used_inside() -> u32 {
    1
}

fn private_dead() -> u32 {
    2
}

#[no_mangle]
pub extern "C" fn ffi_entry() {}

struct Shown;

impl fmt::Display for Shown {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "x")
    }
}

pub fn show() -> String {
    Shown.to_string()
}
"#;

const MAIN_GO: &str = "package main\n\nfunc init() {}\n\nfunc main() {\n\tused()\n}\n\nfunc used() {}\n\nfunc unusedGo() {}\n";

const LIB_GO: &str = "package lib\n\nfunc Exported() int {\n\treturn 1\n}\n\nfunc unexported() int {\n\treturn 2\n}\n";

fn fixture(name: &str) -> Repo {
    Repo::new(
        name,
        &[
            ("app.py", APP_PY),
            ("tests/test_app.py", TEST_PY),
            ("web.js", WEB_JS),
            ("Cargo.toml", "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n"),
            ("src/lib.rs", LIB_RS),
            ("main.go", MAIN_GO),
            ("Makefile", "build:\n\tcargo build\n"),
            ("lib/lib.go", LIB_GO),
        ],
    )
}

fn names(v: &Value, group: &str) -> Vec<String> {
    let mut out: Vec<String> = v["result"][group]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .map(|d| format!("{} {}", d["path"].as_str().unwrap(), d["qualname"].as_str().unwrap()))
        .collect();
    out.sort();
    out
}

#[test]
fn dead_lists_what_nothing_uses_and_keeps_entry_points_and_undecided_uses_apart() {
    let r = fixture("fixture");
    let v = r.json(&["dead"]);
    assert_eq!(
        names(&v, "dead"),
        ["app.py recursive_alone", "app.py unused_private", "lib/lib.go unexported", "main.go unusedGo", "src/lib.rs private_dead", "web.js hidden"],
        "{v:#}"
    );
    let undecided = names(&v, "undecided");
    assert!(undecided.contains(&"app.py by_name".to_string()), "a name used only through getattr is undecided: {v:#}");
    assert!(undecided.contains(&"app.py only_in_a_comment".to_string()), "{v:#}");
    assert!(undecided.iter().any(|u| u.ends_with("onSave")), "a method of an object literal may be called by its key: {v:#}");
    for u in v["result"]["undecided"].as_array().unwrap() {
        assert!(!u["reason"].as_str().unwrap_or("").is_empty(), "every undecided one says why: {u}");
    }
    // used, entry points, tests and exports are in neither group
    for left_out in [
        "helper",
        "caller",
        "handler",
        "main",
        "dispatch",
        "test_caller",
        "shown",
        "other",
        "api",
        "used_inside",
        "ffi_entry",
        "Shown.fmt",
        "init",
        "used",
        "Exported",
        "Shown",
        "build",
    ] {
        for group in ["dead", "undecided"] {
            assert!(!names(&v, group).iter().any(|n| n.ends_with(&format!(" {left_out}"))), "{left_out} is not {group}: {v:#}");
        }
    }
    let d = &v["result"]["dead"][0];
    assert_eq!((d["path"].as_str(), d["start"].as_i64(), d["end"].as_i64(), d["kind"].as_str()), (Some("app.py"), Some(8), Some(9), Some("function")));
    assert_eq!((d["id"].as_str(), d["name"].as_str()), (Some("app.py:8-9"), Some("unused_private")));

    let (code, text) = r.run(&["dead"]);
    assert_eq!(code, 0);
    assert!(text.lines().any(|l| l == "app.py:8-9  [function] unused_private"), "{text}");
    assert!(text.lines().any(|l| l.starts_with("app.py:21-22  [function] by_name  (undecided: ")), "{text}");
    assert!(text.trim_end().ends_with("(6 dead, 3 undecided)"), "{text}");

    // PATH keeps the definitions under one directory; the uses are still looked for everywhere
    assert_eq!(names(&r.json(&["dead", "lib"]), "dead"), ["lib/lib.go unexported"]);
    assert_eq!(names(&r.json(&["dead", "src/lib.rs"]), "dead"), ["src/lib.rs private_dead"]);
    let (code, _) = r.run(&["dead", "no/such/dir"]);
    assert_eq!(code, 2);

    // the same answer the second time, from the kept answers or not
    assert_eq!(r.json(&["dead"]), v);
}

#[test]
fn a_use_added_in_any_file_takes_a_definition_out_of_dead() {
    let r = fixture("added");
    assert!(names(&r.json(&["dead"]), "dead").contains(&"web.js hidden".to_string()));
    fs::write(r.root.join("notes.txt"), "remember hidden\n").unwrap();
    let v = r.json(&["dead"]);
    assert!(!names(&v, "dead").contains(&"web.js hidden".to_string()), "a textual use in any file: {v:#}");
    assert!(names(&v, "undecided").contains(&"web.js hidden".to_string()), "{v:#}");
}

#[test]
fn the_dead_code_tool_gives_the_text_of_the_command() {
    let r = fixture("mcp");
    let (_, cli) = r.run(&["dead", "lib"]);
    let mut child =
        Command::new(BIN).arg("--root").arg(&r.root).arg("serve").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for m in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"dead_code","arguments":{"path":"lib"}}}"#,
    ] {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let by_id = |id: i64| lines.iter().find(|v| v["id"] == id).unwrap_or_else(|| panic!("no response {id}"));
    let tool = by_id(2)["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "dead_code").cloned().expect("dead_code is listed");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert!(tool["inputSchema"]["properties"]["path"].is_object());
    assert_eq!(by_id(3)["result"]["content"][0]["text"].as_str().unwrap().trim_end(), cli.trim_end());
}

#[test]
fn ruby_methods_the_runtime_calls_are_entry_points() {
    let rb = "class Proxy\n  def initialize(s)\n    @s = s\n  end\n\n  def method_missing(name, *args)\n    @s.send(name, *args)\n  end\n\n  def respond_to_missing?(name, all = false)\n    true\n  end\n\n  def <=>(other)\n    0\n  end\n\n  def coerce(n)\n    [n, 1]\n  end\n\n  def to_str\n    @s\n  end\n\n  def self.inherited(k)\n    super\n  end\n\n  def really_unused\n    1\n  end\nend\n\nputs Proxy.new(\"abc\").upcase\n";
    let r = Repo::new("ruby_hooks", &[("p.rb", rb)]);
    let v = r.json(&["dead"]);
    assert_eq!(names(&v, "dead"), ["p.rb Proxy.really_unused"], "{v:#}");
}

/// The names `callers NAME` prints, as `path qualname`.
fn callers(r: &Repo, name: &str) -> Vec<String> {
    let (_, out) = r.run(&["callers", name]);
    out.lines().filter_map(|l| Some(format!("{} {}", l.split(':').next()?, l.split("] ").nth(1)?))).collect()
}

#[test]
fn a_language_that_ignores_case_uses_a_name_in_any_case() {
    let pas = "program P;\n\nprocedure DoWork;\nbegin\nend;\n\nprocedure Run;\nbegin\n  dowork();\nend;\n\nbegin\n  Run();\nend.\n";
    let r = Repo::new("pascal_case", &[("p.pas", pas)]);
    assert!(!names(&r.json(&["dead"]), "dead").contains(&"p.pas DoWork".to_string()));
    assert_eq!(callers(&r, "DoWork"), ["p.pas Run"]);

    let adb = "procedure Main is\n   procedure Do_It is\n   begin\n      null;\n   end Do_It;\nbegin\n   do_it;\nend Main;\n";
    let r = Repo::new("ada_case", &[("a.adb", adb)]);
    assert_eq!(names(&r.json(&["dead"]), "dead"), Vec::<String>::new());
    assert_eq!(callers(&r, "Do_It"), ["a.adb Main"]);

    let php = "<?php\nfunction formatPrice($p) { return $p; }\n\nfunction show() { return FormatPrice(3); }\n\nshow();\n";
    let r = Repo::new("php_case", &[("f.php", php), ("g.py", "def formatprice():\n    return 1\n")]);
    assert_eq!(names(&r.json(&["dead"]), "dead"), ["g.py formatprice"], "a case-sensitive language keeps its case");
    assert_eq!(callers(&r, "formatPrice"), ["f.php show"]);

    let r = Repo::new("py_case", &[("h.py", "def Helper():\n    return 1\n\n\ndef run():\n    return helper()\n")]);
    assert_eq!(names(&r.json(&["dead"]), "dead"), ["h.py Helper", "h.py run"], "Python names keep their case");
}

#[cfg(feature = "extra-langs")]
#[test]
fn fortran_uses_a_name_in_any_case() {
    let r = Repo::new(
        "fortran_case",
        &[
            ("m.f90", "subroutine Compute_Total(x)\n  real :: x\nend subroutine Compute_Total\n"),
            ("main.f90", "subroutine driver()\n  real :: y\n  call compute_total(y)\nend subroutine driver\n"),
        ],
    );
    assert!(!names(&r.json(&["dead"]), "dead").contains(&"m.f90 Compute_Total".to_string()));
    assert_eq!(callers(&r, "Compute_Total"), ["main.f90 driver"]);
}

#[test]
fn callees_in_a_language_that_ignores_case_reach_a_name_in_any_case() {
    let callees = |r: &Repo, name: &str| {
        let (_, out) = r.run(&["callees", name]);
        out.lines().filter_map(|l| Some(format!("{} {}", l.split(':').next()?, l.split("] ").nth(1)?))).collect::<Vec<_>>()
    };
    let pas = "program P;\n\nprocedure DoWork;\nbegin\nend;\n\nprocedure Run;\nbegin\n  dowork();\nend;\n\nbegin\n  Run();\nend.\n";
    let r = Repo::new("pascal_callees", &[("p.pas", pas)]);
    assert_eq!(callees(&r, "Run"), ["p.pas DoWork"]);
    let php = "<?php\nfunction formatPrice($p) { return $p; }\n\nfunction show() { return FormatPrice(3); }\n";
    let r = Repo::new("php_callees", &[("f.php", php), ("g.py", "def formatprice():\n    return 1\n\n\ndef run():\n    return FormatPrice()\n")]);
    assert_eq!(callees(&r, "show"), ["f.php formatPrice"]);
    let (_, out) = r.run(&["callees", "run"]);
    assert!(out.contains("external: FormatPrice"), "Python keeps case: {out}");
}

#[test]
fn refs_in_a_language_that_ignores_case_finds_a_name_in_any_case() {
    // (path:line, the file it resolves to or "-") of every occurrence `refs NAME --json` lists
    let refs = |r: &Repo, name: &str| {
        let v = r.json(&["refs", name])["result"].clone();
        let mut out = Vec::new();
        for d in v["definitions"].as_array().unwrap_or_else(|| panic!("{v}")) {
            for o in d["references"].as_array().unwrap() {
                out.push(format!("{}:{} {}", o["path"].as_str().unwrap(), o["line"], d["path"].as_str().unwrap()));
            }
        }
        for o in v["unresolved"].as_array().unwrap() {
            out.push(format!("{}:{} -", o["path"].as_str().unwrap(), o["line"]));
        }
        out.sort();
        out
    };
    let pas = "program P;\n\nprocedure DoWork;\nbegin\nend;\n\nprocedure Run;\nbegin\n  dowork();\nend;\n\nbegin\n  Run();\nend.\n";
    let r = Repo::new("pascal_refs", &[("p.pas", pas)]);
    assert!(refs(&r, "DoWork").iter().any(|o| o.starts_with("p.pas:9 ")), "{:?}", refs(&r, "DoWork"));
    let php = "<?php\nfunction formatPrice($p) { return $p; }\n\nfunction show() { return FormatPrice(3); }\n";
    let r = Repo::new("php_refs", &[("f.php", php), ("g.py", "def run():\n    return formatprice()\n")]);
    let got = refs(&r, "formatPrice");
    assert!(got.contains(&"f.php:4 f.php".to_string()), "{got:?}");
    assert!(!got.iter().any(|o| o.starts_with("g.py")), "Python keeps case: {got:?}");
}

#[test]
fn a_pascal_call_without_parentheses_is_a_call() {
    let pas = "program P;\n\nprocedure DoWork;\nbegin\nend;\n\nprocedure Tidy;\nbegin\nend;\n\nprocedure Run;\nvar x: integer;\nbegin\n  dowork;\n  x := 1;\n  if x > 0 then Obj.Tidy;\nend;\n\nbegin\n  Run;\nend.\n";
    let r = Repo::new("pascal_bare_call", &[("p.pas", pas)]);
    assert_eq!(callers(&r, "DoWork"), ["p.pas Run"]);
    assert_eq!(callers(&r, "Tidy"), ["p.pas Run"]);
    let (_, out) = r.run(&["callees", "Run"]);
    assert!(out.contains("DoWork") && out.contains("Tidy") && !out.contains("external: x"), "{out}");
    assert!(names(&r.json(&["dead"]), "dead").is_empty(), "{:?}", r.json(&["dead"]));
}

#[test]
fn a_pascal_unit_in_a_pp_file_ignores_case_too() {
    let pp = "unit U;\n\ninterface\n\nprocedure DoWork;\nprocedure Run;\n\nimplementation\n\nprocedure DoWork;\nbegin\nend;\n\nprocedure Run;\nbegin\n  dowork;\n  dowork();\nend;\n\nend.\n";
    let r = Repo::new("pascal_pp_case", &[("u.pp", pp)]);
    assert_eq!(callers(&r, "DoWork"), ["u.pp Run"]);
    assert!(!names(&r.json(&["dead"]), "dead").iter().any(|n| n.ends_with("DoWork")), "{:?}", r.json(&["dead"]));
}

/// An Objective-C header (`.h`, which C and C++ share) is not read by the resolver's C rules: a
/// use in a C file among 16 header lines that name the same function is still asked about.
#[test]
fn an_objective_c_header_is_read_in_its_own_language() {
    let mut files: Vec<(String, String)> = (0..16)
        .map(|i| {
            (
                format!("A{i:02}.h"),
                format!("#import <Foundation/Foundation.h>\n@interface S{i:02} : NSObject\n@end\nstatic inline int w{i:02}(void) {{ return helper(); }}\n"),
            )
        })
        .collect();
    files.push(("main.c".into(), "int helper(void);\nint main(void) { return helper(); }\n".into()));
    files.push(("util.c".into(), "int helper(void) { return 1; }\n".into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let r = Repo::new("objc_header", &refs);
    let v = r.json(&["dead"]);
    for group in ["dead", "undecided"] {
        assert!(!names(&v, group).contains(&"util.c helper".to_string()), "util.c helper is called from main.c: {v:#}");
    }
}

/// A Free Pascal unit starts with compiler directives (`{$mode objfpc}{$H+}`, the Lazarus default):
/// their `$` is no Puppet variable, and the unit is parsed as Pascal.
#[test]
fn a_free_pascal_unit_with_directives_in_a_pp_file_is_pascal() {
    let pp = "unit unit1;\n\n{$mode objfpc}{$H+}\n\ninterface\n\nfunction Add(a, b: Integer): Integer;\n\nimplementation\n\nfunction Add(a, b: Integer): Integer;\nbegin\n  Result := a + b;\nend;\n\nend.\n";
    let r = Repo::new("fpc_unit", &[("unit1.pp", pp), ("site.pp", "class site {\n  $port = 80\n}\n")]);
    let (code, out) = r.run(&["outline", "unit1.pp"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.lines().any(|l| l.split_whitespace().eq(["11-14", "function", "Add"])), "{out}");
    let (_, out) = r.run(&["outline", "site.pp"]);
    assert!(out.contains("site") && !out.contains("function"), "Puppet stays Puppet: {out}");
}

/// A C++ header with a Doxygen `@class` in a comment is C++: only a line that starts with an
/// Objective-C keyword makes a `.h` Objective-C.
#[test]
fn a_doxygen_class_tag_in_a_comment_leaves_a_cpp_header_cpp() {
    let h = "namespace ui {\n/** @class Widget\n *  Draws itself. */\ntemplate <typename T> class Widget {\n  void draw();\n};\n}\n";
    let objc = "#import <Foundation/Foundation.h>\n@interface Shim : NSObject\n- (int)count;\n@end\n";
    let r = Repo::new("doxygen_class", &[("widget.h", h), ("shim.h", objc)]);
    let (code, out) = r.run(&["outline", "widget.h"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("draw"), "parsed as C++: {out}");
    let (_, out) = r.run(&["outline", "shim.h"]);
    assert!(out.contains("Shim"), "Objective-C stays Objective-C: {out}");
}

/// A definition named with its owner (Pascal's `procedure TFoo.Tidy`) is used where its own name
/// is (`o.Tidy`), and a Ruby writer (`def label=`) where it is assigned (`v.label = "x"`).
#[test]
fn a_method_named_with_its_owner_or_as_a_writer_is_used_where_it_is_called_or_assigned() {
    let pas = "unit u;\n\ninterface\n\ntype\n  TFoo = class\n    procedure Tidy;\n    procedure Unused;\n  end;\n\nprocedure DoWork(o: TFoo);\n\nimplementation\n\nprocedure TFoo.Tidy;\nbegin\nend;\n\nprocedure TFoo.Unused;\nbegin\nend;\n\nprocedure DoWork(o: TFoo);\nbegin\n  o.Tidy;\nend;\n\nend.\n";
    // `@size` read in `show` is no use of `size=`, and `send(:color=, 1)` is one of `color=`
    let rb = "class Box\n  def label=(v)\n    @label = v\n  end\n\n  def size=(v)\n    @size = v\n  end\n\n  def color=(v)\n    @color = v\n  end\n\n  def show\n    puts @size\n  end\nend\n\nb = Box.new\nb.label = \"x\"\nb.send(:color=, 1)\nb.show\n";
    let r = Repo::new("owner_and_writer", &[("u.pas", pas), ("a.rb", rb)]);
    let v = r.json(&["dead"]);
    let dead = names(&v, "dead");
    assert!(!dead.iter().any(|d| d.ends_with("Tidy") || d.ends_with("label=") || d.ends_with("color=")), "{v:#}");
    // the declaration in the class and the body name each other, and nothing else names them
    assert!(dead.contains(&"a.rb Box.size=".to_string()) && dead.contains(&"u.pas TFoo.Unused".to_string()), "{v:#}");
}
