use std::time::{Duration, Instant};

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use scrutate::parse::parse_source;
use scrutate::search::prune_indices;

#[path = "support/tag.rs"]
mod tag;

#[test]
fn candidate_cut_keeps_the_lowest_indices_on_tied_scores() {
    let lin = vec![1.0f32; 300];
    let mut keep = prune_indices(&lin, 200);
    keep.sort_unstable();
    assert_eq!(keep, (0..200).collect::<Vec<_>>());
    let mut lin = vec![0.5f32; 10];
    lin[7] = 2.0;
    let mut keep = prune_indices(&lin, 3);
    keep.sort_unstable();
    assert_eq!(keep, vec![0, 1, 7]);
}

/// CPU time of the calling thread, which a busy machine does not stretch as it stretches wall time;
/// wall time where that clock is not known.
fn thread_cpu() -> Duration {
    #[cfg(all(any(target_os = "macos", target_os = "linux"), target_pointer_width = "64"))]
    {
        #[repr(C)]
        struct Timespec {
            sec: i64,
            nsec: i64,
        }
        unsafe extern "C" {
            fn clock_gettime(clock: i32, ts: *mut Timespec) -> i32;
        }
        // CLOCK_THREAD_CPUTIME_ID
        let clock = if cfg!(target_os = "macos") { 16 } else { 3 };
        let mut ts = Timespec { sec: 0, nsec: 0 };
        if unsafe { clock_gettime(clock, &mut ts) } == 0 {
            return Duration::new(ts.sec as u64, ts.nsec as u32);
        }
    }
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// Parses `make(n)` and `make(4 * n)` (best of 3 runs each, so a loaded machine does not decide)
/// and fails if the larger input costs 10x or more, where linear work gives 4x and quadratic 16x.
/// The parse is the one the index runs: a chunk's text is cut later, one chunk at a time.
fn assert_parse_scales_linearly(path: &str, n: usize, make: impl Fn(usize) -> String, check: impl Fn(usize, &[scrutate::parse::Chunk])) {
    let best = |k: usize| {
        let src = make(k);
        (0..3)
            .map(|_| {
                let t = thread_cpu();
                let chunks = scrutate::parse::parse_file_with(path, &src, None).0.chunks;
                let took = thread_cpu() - t;
                check(k, &chunks);
                took
            })
            .min()
            .unwrap()
    };
    let (small, large) = (best(n), best(4 * n));
    // a floor, so that a run too short to measure does not make the ratio
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.005);
    assert!(ratio < 10.0, "{path}: {n} items took {small:?}, {} took {large:?}", 4 * n);
}

#[test]
fn deeply_nested_functions_index_in_bounded_time_and_size() {
    for (sep, n) in [("\n", 500), ("", 2500)] {
        let make = |n: usize| format!("{}{}", format!("function a(){{{sep}").repeat(n), "}".repeat(n));
        assert_parse_scales_linearly("a.js", n, make, |n, chunks| {
            assert_eq!(chunks.len(), n);
            let deepest = chunks.iter().map(|c| c.qualname.split('.').count()).max().unwrap();
            assert_eq!(deepest, 32);
            let qual_bytes: usize = chunks.iter().map(|c| c.qualname.len()).sum();
            assert!(qual_bytes < 64 * n, "{qual_bytes} bytes of qualified names");
        });
    }
}

#[test]
fn a_node_with_many_children_parses_in_linear_time() {
    assert_parse_scales_linearly("a.scm", 5000, |n| "(f ".repeat(n), |_, _| {});
}

#[test]
fn deeply_nested_lisp_and_html_parse_in_linear_time() {
    for (path, head) in [("a.lisp", "(defun f () "), ("a.scm", "(define (f) ")] {
        let make = |n: usize| format!("{head}{}{})", "(g ".repeat(n), ")".repeat(n));
        assert_parse_scales_linearly(path, 15000, make, |_, chunks| {
            assert!(chunks.iter().any(|c| c.qualname == "f" && c.calls.contains("g")), "{path}");
        });
    }
    assert_parse_scales_linearly("a.lisp", 20000, |n| "(".repeat(n) + &")".repeat(n), |_, _| {});
    assert_parse_scales_linearly("a.html", 12500, |n| "<div>".repeat(n) + &"</div>".repeat(n), |_, _| {});
}

#[test]
fn lock_files_are_not_indexed_whatever_their_extension() {
    use scrutate::parse::is_indexable;
    for name in [
        "package-lock.json",
        "web/npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "packages.lock.json",
        "Package.resolved",
        "app/.terraform.lock.hcl",
        "gradle.lockfile",
        "buildscript-gradle.lockfile",
        "bun.lockb",
        "yarn.lock",
        "go.work.sum",
    ] {
        assert!(!is_indexable(name), "{name}");
    }
    for name in ["package.json", "my-package-lock.json", "pnpm-workspace.yaml", "main.tf", "src/lock.rs"] {
        assert!(is_indexable(name), "{name}");
    }
}

#[test]
fn a_staircase_of_nested_definitions_without_a_grammar_parses_in_time_linear_in_its_size() {
    // each definition is indented one deeper, so each one's block runs to the end of the file
    let best = |m: usize| {
        let src: String = (0..m).map(|i| format!("{}sub f{i} {{\n", " ".repeat(i))).collect();
        (0..3)
            .map(|_| {
                let t = thread_cpu();
                let chunks = scrutate::parse::parse_file_with("a.pl", &src, None).0.chunks;
                let took = thread_cpu() - t;
                assert_eq!(chunks.iter().filter(|c| c.kind == "function").count(), m);
                took
            })
            .min()
            .unwrap()
    };
    // 16 times the bytes: linear work takes about 16 times as long, work over the rest of the file per definition 64 times
    let (small, large) = (best(250), best(1000));
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.005);
    assert!(ratio < 32.0, "250 definitions took {small:?}, 1000 took {large:?}");
    // the text and the body are the block's first lines up to both caps, "\n..." when cut
    let head = |s: &str| {
        let end = s.match_indices('\n').nth(scrutate::parse::MAX_CHUNK_LINES - 1).map_or(s.len(), |(i, _)| i);
        let kept = &s[..s[..end].floor_char_boundary(scrutate::parse::MAX_CHUNK_BYTES)];
        if kept.len() < s.len() { format!("{kept}\n...") } else { s.to_string() }
    };
    let cases = [(9, 4, 199), (9, 4, 200), (9, 4, 201), (9, 4, 300), (9, 3300, 9), (9, 3300, 10), (20000, 3000, 4), (20000, 3000, 5)];
    let more = [(40000, 9, 3), (9, 32766, 1), (9, 32767, 1), (9, 32768, 1), (9, 16383, 2), (9, 16384, 2), (9, 16382, 3)];
    for (first, len, n) in cases.into_iter().chain(more) {
        let head_line = format!("sub a {{{}", "#".repeat(first - 7));
        let lines: Vec<String> = std::iter::once(head_line).chain((0..n).map(|_| format!("  {}", "x".repeat(len - 2)))).collect();
        let chunks = scrutate::parse::parse_file_with("a.pl", &(lines.join("\n") + "\n"), None).0.chunks;
        let a = chunks.iter().find(|c| c.name == "a").unwrap();
        assert_eq!(a.text, head(&lines.join("\n")), "{first} {len} {n}");
        assert_eq!(a.body, head(&lines[1..].join("\n")), "{first} {len} {n}");
    }
}

#[test]
fn a_file_of_many_sibling_definitions_parses_in_linear_time() {
    for (path, line) in [
        ("w.js", "function f{i}(x) {{ return x + {i}; }}"),
        ("w.c", "int f{i}(int x) {{ return x + {i}; }}"),
        ("w.go", "func f{i}(x int) int {{ return x + {i} }}"),
    ] {
        let make = |n: usize| {
            let body: String = (0..n).map(|i| line.replace("{i}", &i.to_string()).replace("{{", "{").replace("}}", "}") + "\n").collect();
            if path.ends_with(".go") { format!("package p\n{body}") } else { body }
        };
        assert_parse_scales_linearly(path, 1000, make, |n, chunks| {
            assert_eq!(chunks.iter().filter(|c| c.kind == "function").count(), n, "{path}");
        });
    }
}

#[test]
fn the_model_signature_is_a_hash_of_the_model_file() {
    use blake2::Digest;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("models/semantic.npz");
    let bytes = std::fs::read(&path).unwrap();
    let out = blake2::Blake2b::<blake2::digest::consts::U16>::digest(&bytes);
    // the signature of a model file: "blake2b:" and the 16-byte BLAKE2b of its bytes in hex
    let want = format!("blake2b:{}", out.iter().map(|b| format!("{b:02x}")).collect::<String>());
    assert_eq!(scrutate::models::BUNDLED_SIGNATURE, want);
    assert_eq!(scrutate::models::file_signature(&bytes), want);
}

fn symbols(path: &str, src: &str) -> Vec<(String, String)> {
    let stem = path.rsplit('/').next().unwrap().split('.').next().unwrap();
    let file_chunk = |c: &scrutate::parse::Chunk| c.kind == "window" || (c.kind == "module" && c.qualname == stem);
    parse_source(path, src).into_iter().filter(|c| !file_chunk(c)).map(|c| (c.kind.to_string(), c.qualname)).collect()
}

#[test]
fn the_heuristic_parser_reads_past_modifiers_and_skips_prose() {
    let cr = "abstract class Shape\n  private def area\n    0\n  end\nend\n";
    let got = symbols("shape.cr", cr);
    assert!(got.contains(&("class".into(), "Shape".into())), "{got:?}");
    assert!(got.contains(&("function".into(), "area".into())), "{got:?}");
    // a name may start outside ASCII
    let got = symbols("ära.cr", "class Ära\n  def übersetzen\n    0\n  end\nend\n");
    assert!(got.contains(&("class".into(), "Ära".into())) && got.contains(&("function".into(), "übersetzen".into())), "{got:?}");
    if scrutate::langs::by_id("ocaml").is_none() {
        assert_eq!(symbols("m.ml", "let add x y =\n  x + y\n\nlet limit = 5\n"), vec![("function".to_string(), "add".to_string())]);
    }
    let kconfig = "config FOO\n\thelp\n\t  To compile this as a module, choose M here: the\n\t  module will be called foo.\n";
    assert_eq!(symbols("drivers/Kconfig", kconfig), vec![]);
    assert_eq!(symbols("notes.txt", "module will be called foo\nclass of service\n"), vec![]);
}

fn has(got: &[(String, String)], kind: &str, qualname: &str) -> bool {
    got.iter().any(|(k, q)| k == kind && q == qualname)
}

/// (kind, name, qualname) of every definition, without the file chunk.
fn defs(path: &str, src: &str) -> Vec<(String, String, String)> {
    parse_source(path, src).into_iter().filter(|c| !matches!(c.kind, "module" | "window")).map(|c| (c.kind.to_string(), c.name, c.qualname)).collect()
}

fn triples(v: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    v.iter().map(|(k, n, q)| (k.to_string(), n.to_string(), q.to_string())).collect()
}

#[test]
fn a_lua_function_assigned_to_a_name_or_a_table_field_is_a_definition() {
    let src = "local helper = function(x)\n  return x\nend\nM.assigned = function(x)\n  return helper(x)\nend\nM[\"bracket\"] = function(x) return x end\nM.a.b = function() end\nlocal obj = {\n  inline = function(x) return x end,\n  n = 1,\n  sub = { deep = function() end },\n}\nlocal x, y = function() end, 1\nlocal t = {}\np, M.q = function() end, function() end\n";
    let want = [
        ("function", "helper", "helper"),
        ("function", "assigned", "M.assigned"),
        ("function", "bracket", "M.bracket"),
        ("function", "b", "M.a.b"),
        ("function", "inline", "obj.inline"),
        ("function", "deep", "obj.sub.deep"),
        ("function", "x", "x"),
        ("function", "p", "p"),
        ("function", "q", "M.q"),
    ];
    assert_eq!(defs("a.lua", src), triples(&want));
    let chunks = parse_source("a.lua", src);
    let assigned = chunks.iter().find(|c| c.qualname == "M.assigned").unwrap();
    assert!(assigned.calls.contains("helper") && assigned.start == 4, "{:?} {}", assigned.calls, assigned.start);
}

#[test]
fn a_js_function_assigned_to_a_member_is_a_definition() {
    let src = "function Foo() {}\nFoo.prototype.bar = function () {\n  return helper();\n};\nFoo.staticFn = function (x) {\n  return x;\n};\nFoo.prototype.arrow = (x) => x;\n$.fn.plugin = function () {};\nexports.one = function () {};\nmodule.exports.two = () => 2;\nmodule.exports = function () {};\nexports.a.b = function () {};\nobj[\"k\"] = function () {};\nthis.x = function () {};\nFoo.count = 3;\n";
    let want = [
        ("function", "Foo", "Foo"),
        ("method", "bar", "Foo.bar"),
        ("function", "staticFn", "Foo.staticFn"),
        ("method", "arrow", "Foo.arrow"),
        ("function", "plugin", "$.fn.plugin"),
        ("function", "one", "one"),
        ("function", "two", "two"),
        ("function", "a", "a"),
        ("function", "b", "a.b"),
        ("function", "k", "obj.k"),
    ];
    for path in ["a.js", "a.ts"] {
        assert_eq!(defs(path, src), triples(&want), "{path}");
    }
    let chunks = parse_source("a.js", src);
    let bar = chunks.iter().find(|c| c.qualname == "Foo.bar").unwrap();
    assert!(bar.calls.contains("helper"), "{:?}", bar.calls);
}

#[test]
fn a_js_function_set_on_this_belongs_to_the_enclosing_function_or_class() {
    let src = "function Widget() {\n  this.render = function () {\n    return draw();\n  };\n  this.size = () => 3;\n}\nclass Panel {\n  constructor() {\n    this.open = function () {};\n  }\n  init() {\n    this.close = () => 1;\n  }\n}\nconst Box = function () {\n  this.fill = function () {};\n};\nfor (let i = 0; i < 3; i++) {\n  handlers[i] = function () {};\n}\n";
    let want = [
        ("function", "Widget", "Widget"),
        ("method", "render", "Widget.render"),
        ("method", "size", "Widget.size"),
        ("class", "Panel", "Panel"),
        ("method", "constructor", "Panel.constructor"),
        ("method", "open", "Panel.open"),
        ("method", "init", "Panel.init"),
        ("method", "close", "Panel.close"),
        ("function", "Box", "Box"),
        ("method", "fill", "Box.fill"),
    ];
    for path in ["w.js", "w.ts"] {
        assert_eq!(defs(path, src), triples(&want), "{path}");
    }
    let render = parse_source("w.js", src).into_iter().find(|c| c.qualname == "Widget.render").unwrap();
    assert!(render.calls.contains("draw") && render.start == 2, "{:?} {}", render.calls, render.start);
}

#[test]
fn a_default_export_is_named_by_its_function_or_else_by_the_file() {
    type Case = (&'static str, &'static str, &'static [(&'static str, &'static str, &'static str)]);
    let cases: &[Case] = &[
        ("lib/app.js", "module.exports = function () {\n  return 1;\n};\n", &[("function", "app", "app")]),
        ("lib/named.js", "module.exports = function createApp() {};\n", &[("function", "createApp", "createApp")]),
        ("lib/arrow.ts", "module.exports = () => 5;\n", &[("function", "arrow", "arrow")]),
        ("lib/widget/index.js", "export default function () {}\n", &[("function", "widget", "widget")]),
        ("index.ts", "export default () => 1;\n", &[("function", "index", "index")]),
        ("lib/cls.ts", "export default class {\n  m() {}\n}\n", &[("class", "cls", "cls"), ("method", "m", "cls.m")]),
        ("lib/k.js", "module.exports = class {\n  m() {}\n};\n", &[("class", "k", "k"), ("method", "m", "k.m")]),
        ("lib/named2.ts", "export default function make() {}\n", &[("function", "make", "make")]),
        (
            "lib/obj.js",
            "module.exports = {\n  run() {},\n  stop: function () {},\n  sub: { deep() {} },\n};\n",
            &[("method", "run", "run"), ("method", "stop", "stop"), ("method", "deep", "sub.deep")],
        ),
    ];
    for (path, src, want) in cases {
        assert_eq!(defs(path, src), triples(want), "{path}");
    }
}

#[test]
fn a_julia_method_of_another_modules_function_is_named_by_the_function() {
    let src = "module Geo\nfunction Base.show(io::IO, p::Point)\n    print(io, p)\nend\nBase.length(p::Point) = 1\nfunction Base.:+(a::Point, b::Point)\n    a\nend\nfunction plain(x)\n    x\nend\nBase.:(==)(a::Point, b::Point) = true\nfunction Base.:(==)(a::Line, b::Line)\n    false\nend\nend\n";
    let want = [
        ("module", "Geo", "Geo"),
        ("function", "show", "Geo.Base.show"),
        ("function", "length", "Geo.Base.length"),
        ("function", "+", "Geo.Base.+"),
        ("function", "plain", "Geo.plain"),
        ("function", "==", "Geo.Base.=="),
        ("function", "==", "Geo.Base.=="),
    ];
    let got: Vec<_> = parse_source("geo.jl", src).into_iter().filter(|c| c.kind != "window").map(|c| (c.kind.to_string(), c.name, c.qualname)).collect();
    assert_eq!(got, triples(&want));
}

#[test]
fn cpp_call_operators_haskell_operators_and_r6_methods_are_named_definitions() {
    let cpp = "struct F {\n  int operator()(int x) { return x; }\n  bool operator==(const F& o) const;\n  void operator()() const;\n};\nint F::operator()(int x) { return x; }\n";
    let want = [
        ("struct", "F", "F"),
        ("method", "operator()", "F.operator()"),
        ("method", "operator==", "F.operator=="),
        ("method", "operator()", "F.operator()"),
        ("function", "operator()", "F.operator()"),
    ];
    assert_eq!(defs("f.cpp", cpp), triples(&want));
    let hs = "module M where\na <+> b = a + b\n(<+>) a b = a\n(|>) = flip ($)\nx `op` y = x\nplain x = x\n";
    let want = [("function", "<+>", "<+>"), ("function", "|>", "|>"), ("function", "op", "op"), ("function", "plain", "plain")];
    assert_eq!(defs("m.hs", hs), triples(&want));
    let r = "Counter <- R6Class(\"Counter\",\n  public = list(\n    n = 0,\n    add = function(n) {\n      self$n <- self$n + n\n    }\n  ),\n  private = list(h = function() 1)\n)\nx <- list(f = function() 1)\ng <- function() 1\nStack <- R6::R6Class(\"Stack\", public = list(push = function(x) x))\n";
    let want = [("method", "add", "Counter.add"), ("method", "h", "Counter.h"), ("function", "g", "g"), ("method", "push", "Stack.push")];
    assert_eq!(defs("c.r", r), triples(&want));
}

#[test]
fn rust_macros_and_trait_methods_without_a_body_are_symbols() {
    let got = symbols(
        "a.rs",
        "macro_rules! square {\n    ($x:expr) => { $x * $x };\n}\n\ntrait Runner {\n    fn run(&self) -> u32;\n    fn name(&self) -> String { String::new() }\n}\n",
    );
    assert!(has(&got, "macro", "square") && has(&got, "function", "Runner.run") && has(&got, "function", "Runner.name"), "{got:?}");
}

#[test]
fn go_methods_are_qualified_by_their_receiver_type() {
    let got = symbols("a.go", "package p\n\nfunc (s *Store) Get(key string) (int, bool) {\n\treturn 0, false\n}\n\nfunc (b Box[T]) Put(v T) {}\n");
    assert!(has(&got, "method", "Store.Get") && has(&got, "method", "Box.Put"), "{got:?}");
    let c = parse_source("a.go", "package p\n\nfunc (s *Store) Get() {}\n").into_iter().find(|c| c.qualname == "Store.Get").unwrap();
    assert_eq!(c.name, "Get");
}

#[test]
fn cpp_methods_defined_outside_the_class_are_named_by_the_method() {
    let chunks = parse_source("v.cpp", "void Widget::draw() {}\nvoid ns::Widget::paint() {}\n");
    let draw = chunks.iter().find(|c| c.name == "draw").expect("draw");
    assert_eq!(draw.qualname, "Widget.draw");
    assert!(chunks.iter().any(|c| c.name == "paint" && c.qualname == "ns.Widget.paint"));
}

#[test]
fn cpp_methods_declared_in_a_class_body_are_symbols() {
    let src = "class Shape {\npublic:\n  virtual double area() const = 0;\n  void resize(int w);\n  int x;\n  int (*fp)(int);\n  friend void swap(Shape&, Shape&);\n};\nvoid free_decl(int);\n";
    let got = symbols("s.hpp", src);
    assert_eq!(
        got,
        vec![
            ("class".into(), "Shape".into()),
            ("method".into(), "Shape.area".into()),
            ("method".into(), "Shape.resize".into()),
            ("function".into(), "free_decl".into())
        ]
    );
}

#[test]
fn c_and_cpp_function_like_macros_are_symbols() {
    let src = "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n#define LIMIT 5\n";
    for path in ["m.h", "m.hpp"] {
        let got = symbols(path, src);
        assert_eq!(got, vec![("macro".to_string(), "MAX".to_string()), ("constant".to_string(), "LIMIT".to_string())], "{path}");
    }
}

#[test]
fn js_class_field_functions_and_commonjs_exports_are_symbols() {
    let got = symbols(
        "a.js",
        "class Store {\n  handle = (e) => { this.x = e; };\n  count = 3;\n}\nmodule.exports.legacy = function legacy() {};\nexports.other = () => {};\n",
    );
    assert!(has(&got, "method", "Store.handle") && has(&got, "function", "legacy") && has(&got, "function", "other"), "{got:?}");
    assert!(!got.iter().any(|(_, q)| q.ends_with("count")), "{got:?}");
    let got = symbols("a.ts", "class Store {\n  handle = (e: E) => { };\n}\n");
    assert!(has(&got, "method", "Store.handle"), "{got:?}");
}

#[test]
fn ts_abstract_methods_declared_functions_and_namespaces_are_symbols() {
    let got = symbols(
        "a.ts",
        "abstract class Repo<T> {\n  abstract find(id: Id): T | undefined;\n}\ndeclare function external(x: number): void;\nexport namespace Util {\n  export function clamp(x: number): number { return x; }\n}\n",
    );
    assert!(
        has(&got, "method", "Repo.find") && has(&got, "function", "external") && has(&got, "module", "Util") && has(&got, "function", "Util.clamp"),
        "{got:?}"
    );
}

fn tree(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("scrutate_parse_index_{name}_{}", std::process::id()));
    tag::claim(&root);
    let _ = std::fs::remove_dir_all(&root);
    for (rel, body) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    canon(root)
}

#[test]
fn inside_git_the_ignore_rules_decide_for_build_dist_target_and_venv() {
    let files = [
        ("build/maker.py", "def tracked_builder():\n    pass\n"),
        ("target/gen.rs", "fn g() {}\n"),
        ("dist/out.py", "def x():\n    pass\n"),
        ("node_modules/m/i.js", "function m() {}\n"),
        ("src/a.py", "def a():\n    pass\n"),
        (".gitignore", "dist/\n"),
    ];
    let repo = tree("repo", &files);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    assert_eq!(scrutate::files::list_files(&repo), vec![".gitignore", "build/maker.py", "src/a.py", "target/gen.rs"]);
    let mut m = scrutate::files::Membership::new(&repo);
    assert!(m.may_contain(&repo.join("build/new.py")));
    assert!(!m.may_contain(&repo.join("dist/new.py")));
    let plain = tree("plain", &files);
    assert_eq!(scrutate::files::list_files(&plain), vec![".gitignore", "src/a.py"]);
    let mut m = scrutate::files::Membership::new(&plain);
    assert!(!m.may_contain(&plain.join("build/new.py")));
}

#[test]
fn calls_inside_rust_macro_arguments_are_calls() {
    let src = r#"fn f() {
    println!("{}", a::b(x), y.m(z));
    assert_eq!(vec![g(1)], if k(2) { 1 } else { 0 });
    let _ = json!({"tools": tools(structured)});
    assert!(matches!(first(s), Some(Pat(_)) | Whole(_)));
    quote! { fn declared(x: u8) {} #[derive(Debug)] struct S(u8); };
}
"#;
    let chunks = parse_source("m.rs", src);
    let f = chunks.iter().find(|c| c.name == "f").unwrap();
    let calls: Vec<&str> = f.calls.iter().map(|s| s.as_str()).collect();
    assert_eq!(calls, ["assert", "assert_eq", "b", "first", "g", "json", "k", "m", "matches", "println", "quote", "tools", "vec"]);
}

#[test]
fn chunk_text_has_a_byte_cap_on_a_one_line_file_of_nested_functions() {
    use scrutate::parse::MAX_CHUNK_BYTES;
    let n = 8000;
    let src = format!("{}{}", "function a(){".repeat(n), "}".repeat(n));
    let chunks = parse_source("a.js", &src);
    assert_eq!(chunks.len(), n);
    for c in &chunks {
        assert!(c.text.len() <= MAX_CHUNK_BYTES + 4 && c.body.len() <= MAX_CHUNK_BYTES + 4, "{} bytes", c.text.len());
    }
    // the file's budget keeps the text linear in the file: the outer definitions whole, the rest
    // their first line, cut
    use scrutate::parse::{CUT_CHUNK_BYTES, FILE_TEXT_FLOOR, FILE_TEXT_PER_BYTE};
    let total: usize = chunks.iter().map(|c| c.text.len() + c.body.len()).sum();
    let bound = FILE_TEXT_FLOOR + FILE_TEXT_PER_BYTE * src.len() + 2 * MAX_CHUNK_BYTES + 8 + n * 2 * (CUT_CHUNK_BYTES + 4);
    assert!(total <= bound, "{total} bytes of chunk text, over {bound}");
    assert!(chunks[0].text.len() > MAX_CHUNK_BYTES);
    let last = &chunks[n / 2];
    assert!(last.text.starts_with("function a(){") && last.text.ends_with("\n...") && last.text.len() <= CUT_CHUNK_BYTES + 4, "{}", last.text.len());
    // cut at a character, never inside one
    let wide = format!("function w() {{ \"{}\" }}", "\u{e9}".repeat(MAX_CHUNK_BYTES));
    let w = parse_source("w.js", &wide).into_iter().find(|c| c.name == "w").unwrap();
    assert!(w.text.ends_with("\u{e9}\n...") && w.text.len() <= MAX_CHUNK_BYTES + 4);
}

#[test]
fn cpp_constructors_destructors_member_templates_and_conversions_are_methods() {
    let src = "class Shape {\npublic:\n  Shape();\n  explicit Shape(int w);\n  virtual ~Shape();\n  template <class U> void g(U);\n  operator bool() const;\n  Shape& operator=(const Shape&);\n  Shape* next();\n  int x;\n  void (*fp)(int);\n};\nShape make();\n";
    let got: Vec<(String, String)> = parse_source("s.hpp", src).iter().map(|c| (c.kind.to_string(), c.qualname.clone())).collect();
    let want: Vec<(String, String)> = [
        ("class", "Shape"),
        ("method", "Shape.Shape"),
        ("method", "Shape.Shape"),
        ("method", "Shape.~Shape"),
        ("method", "Shape.g"),
        ("method", "Shape.operator bool"),
        ("method", "Shape.operator="),
        ("method", "Shape.next"),
        ("function", "make"),
    ]
    .iter()
    .map(|(k, q)| (k.to_string(), q.to_string()))
    .collect();
    assert_eq!(got, want);
}

#[test]
fn a_chunk_ends_on_the_last_line_that_holds_its_text() {
    let src = "#include <x.h>\n#define MAX(a, b) ((a) > (b) ? (a) : (b))\ntypedef int T;\n#define LONG(a) \\\n  ((a) + 1)\nint f(void) { return 0; }\n";
    let got: Vec<(String, i64, i64)> = parse_source("m.c", src).iter().map(|c| (c.name.clone(), c.start, c.end)).collect();
    assert_eq!(got[1..], [("MAX".into(), 2, 2), ("T".into(), 3, 3), ("LONG".into(), 4, 5), ("f".into(), 6, 6)]);
    // a Rust line comment ends with its line break: one a blank line away is not documentation
    let rs = parse_source("d.rs", "/// stray note\n\nfn f() {}\n/// Doc.\nfn g() {}\n");
    let doc = |n: &str| rs.iter().find(|c| c.name == n).unwrap().doc.clone();
    assert_eq!((doc("f"), doc("g")), (String::new(), "Doc.".to_string()));
}

#[test]
fn the_comment_right_above_a_definition_is_its_documentation() {
    // (file, source, qualname, first line of the chunk, documentation)
    let cases: &[(&str, &str, &str, i64, &str)] = &[
        ("a.py", "import os\n# Adds one.\ndef f(x):\n    return x + 1\n", "f", 3, "Adds one."),
        ("a.py", "# Adds one.\n# Twice.\ndef f(x):\n    \"\"\"Returns x.\"\"\"\n    return x\n", "f", 3, "Adds one.\nTwice.\nReturns x."),
        ("a.py", "x = 1\n# A box.\nclass Box:\n    # Its size.\n    SIZE = 3\n", "Box", 3, "A box."),
        ("a.py", "x = 1\n# A box.\nclass Box:\n    # Its size.\n    SIZE = 3\n", "Box.SIZE", 5, "Its size."),
        ("a.py", "x = 1\nclass Box:  # not about SIZE\n    SIZE = 3\n", "Box.SIZE", 3, ""),
        ("a.py", "x = 1\n# The cap.\nLIMIT = 3\n", "LIMIT", 3, "The cap."),
        ("a.py", "x = 1\n# Cached.\n@cache\ndef f():\n    pass\n", "f", 3, "Cached."),
        ("a.py", "x = 1\n# stray note\n\ndef f():\n    pass\n", "f", 4, ""),
        ("a.py", "#!/usr/bin/env python\ndef f():\n    pass\n", "f", 2, ""),
        ("BUILD.bazel", "x = 1\n# Makes a rule.\ndef rule_fn(name):\n    pass\n", "rule_fn", 3, "Makes a rule."),
        ("a.hs", "module A where\n\n-- Adds one.\nadd :: Int -> Int\nadd x = x + 1\n", "add", 4, "Adds one."),
        ("a.hs", "module A where\n\nadd :: Int -> Int\nadd x = x + 1\n", "add", 3, ""),
        ("A.elm", "module A exposing (..)\n\n-- Adds one.\nadd : Int -> Int\nadd x =\n    x + 1\n", "add", 4, "Adds one."),
        ("a.fish", "set -g x 1\n# Says hi.\nfunction greet\n    echo hi\nend\n", "greet", 3, "Says hi."),
        ("a.lisp", "(defvar *x* 1)\n;; Adds one.\n(defun add (x)\n  (+ x 1))\n", "add", 3, "Adds one."),
        ("a.s", ".text\n# Returns.\nf:\n    ret\n", "f", 3, "Returns."),
        ("a.jsonnet", "local x = 1;\n// Adds one.\nlocal add(a) = a + 1;\nadd(x)\n", "add", 3, "Adds one."),
        ("a.jsonnet", "// About x.\nlocal x = 1;\nlocal add(a) = a + 1;\nadd(x)\n", "add", 3, ""),
        // a function assigned to a member, to `this` or to `module.exports` takes the comment above its statement
        ("a.js", "function Foo() {}\n/** Spins the rotor. */\nFoo.prototype.spin = function () {\n  return 2;\n};\n", "Foo.spin", 3, "Spins the rotor."),
        ("a.js", "function Foo() {\n  // Paints it.\n  this.paint = function () {\n    return 1;\n  };\n}\n", "Foo.paint", 3, "Paints it."),
        ("a.js", "function Foo() {\n  // Paints it.\n  this[\"paint\"] = function () {\n    return 1;\n  };\n}\n", "Foo.paint", 3, "Paints it."),
        ("a.ts", "class Foo {\n  constructor() {\n    this['paint'] = () => 1;\n  }\n}\n", "Foo.paint", 3, ""),
        ("lib/app.js", "const x = 1;\n/** Builds the app. */\nmodule.exports = function createApp() {\n  return x;\n};\n", "createApp", 3, "Builds the app."),
        ("a.ts", "class Foo {}\n/** Makes one. */\nFoo.make = (): Foo => new Foo();\n", "Foo.make", 3, "Makes one."),
        ("m.lua", "local M = {}\n--- Opens the door.\nM.open = function(x)\n  return x\nend\n", "M.open", 3, "Opens the door."),
        // attributes, decorators and annotations between the comment and the definition: the
        // definition starts at the attribute lines right above it
        ("a.rs", "/// A target.\n#[derive(Debug, Clone)]\npub struct Target {\n    x: i32,\n}\n", "Target", 2, "A target."),
        ("a.rs", "/// Two.\n#[derive(Debug)]\n#[repr(u8)]\npub enum E {\n    A,\n}\n", "E", 2, "Two."),
        ("a.rs", "pub struct T;\nimpl T {\n    /// Fast.\n    #[inline]\n    pub fn go(&self) {}\n}\n", "T.go", 4, "Fast."),
        ("a.rs", "/// The cap.\n#[allow(dead_code)]\nconst CAP: usize = 3;\n", "CAP", 3, "The cap."),
        ("a.rs", "/// stray\n\n#[derive(Debug)]\nstruct S;\n", "S", 3, ""),
        ("a.rs", "/// stray\n#[derive(Debug)]\n\nstruct S;\n", "S", 4, ""),
        ("a.ts", "class A {\n  /** Gets x. */\n  @Get('/x')\n  target() {\n    return 1;\n  }\n}\n", "A.target", 3, "Gets x."),
        ("a.dart", "class A {\n  /// Plain.\n  int f() => 1;\n  /// Overrides.\n  @override\n  int g() => 1;\n}\n", "A.f", 3, "Plain."),
        ("a.dart", "class A {\n  /// Plain.\n  int f() => 1;\n  /// Overrides.\n  @override\n  int g() => 1;\n}\n", "A.g", 5, "Overrides."),
        ("a.ex", "defmodule M do\n  # Adds one.\n  @spec f(integer()) :: integer()\n  def f(x), do: x + 1\nend\n", "M.f", 3, "Adds one."),
        ("a.ex", "defmodule M do\n  # The timeout.\n  @timeout 5000\n  def f, do: @timeout\nend\n", "M.f", 3, ""),
        ("a.erl", "-module(a).\n\n%% Adds one.\n-spec f(integer()) -> integer().\nf(X) -> X + 1.\n", "f", 5, "Adds one."),
        // the language's own documentation form
        (
            "a.ex",
            "defmodule M do\n  @moduledoc \"\"\"\n  The module.\n  \"\"\"\n  @doc \"\"\"\n  Adds one.\n  \"\"\"\n  @spec f(integer()) :: integer()\n  def f(x), do: x + 1\nend\n",
            "M.f",
            8,
            "Adds one.",
        ),
        (
            "a.ex",
            "defmodule M do\n  @moduledoc \"\"\"\n  The module.\n  \"\"\"\n  @doc ~S\"\"\"\n  Adds one.\n  \"\"\"\n  def f(x), do: x + 1\nend\n",
            "M",
            1,
            "The module.",
        ),
        ("a.ex", "defmodule M do\n  @doc ~S\"Adds one.\"\n  def f(x), do: x + 1\nend\n", "M.f", 2, "Adds one."),
        ("a.hs", "module A where\n\n-- | Adds one.\nadd :: Int -> Int\nadd x = x + 1\n", "add", 4, "Adds one."),
        ("a.hs", "module A where\n\n{-| Adds one.\n-}\nadd :: Int -> Int\nadd x = x + 1\n", "add", 5, "Adds one."),
        ("a.jl", "x = 1\n\"\"\"Adds one.\"\"\"\nfunction add(x)\n    x + 1\nend\n", "add", 3, "Adds one."),
        ("a.ps1", "<#\n.SYNOPSIS\nAdds one.\n#>\nfunction Add-One {\n    1\n}\n", "Add-One", 5, ".SYNOPSIS\nAdds one."),
        ("a.ps1", "function Add-One {\n    <#\n    .SYNOPSIS\n    Adds one.\n    #>\n    param($x)\n    $x + 1\n}\n", "Add-One", 1, ".SYNOPSIS\nAdds one."),
        ("a.ps1", "function Add-One { <# Adds one. #> 1 }\n", "Add-One", 1, "Adds one."),
        ("a.ps1", "$x = 1\n<# Adds one. #>\nfunction Add-One { 1 }\n", "Add-One", 3, "Adds one."),
        ("a.ex", "defmodule M do\n  @doc false\n  # Internal.\n  def f(x), do: x\nend\n", "M.f", 4, ""),
        ("a.ex", "defmodule M do\n  @doc false\n  def f(x), do: x\nend\n", "M.f", 2, ""),
        ("a.erl", "-module(a).\n\n-doc \"Adds one.\".\n-spec f(integer()) -> integer().\nf(X) -> X + 1.\n", "f", 5, "Adds one."),
        ("a.erl", "-module(a).\n\n-doc \"\"\"\nAdds one.\n\"\"\".\nf(X) -> X + 1.\n", "f", 6, "Adds one."),
        (
            "a.adb",
            "procedure A is\n   --  Adds one.\n   function F return Integer is\n   begin\n      return 1;\n   end F;\nbegin\n   null;\nend A;\n",
            "A.F",
            3,
            "Adds one.",
        ),
        ("a.nix", "{\n  # Adds one.\n  add = x: x + 1;\n}\n", "add", 3, "Adds one."),
        ("a.tf", "# The web server.\nresource \"aws_instance\" \"web\" {\n  ami = \"x\"\n}\n", "resource aws_instance web", 2, "The web server."),
        ("a.yml", "# The target.\ntarget:\n  key: 1\n", "target", 2, "The target."),
        ("a.rb", "class K\n  # First.\n  def f; end\nend\n", "K.f", 3, "First."),
        // constants
        ("a.go", "package p\n\n// LockSeconds is the lock time.\nconst LockSeconds = 30\n", "LockSeconds", 4, "LockSeconds is the lock time."),
        ("a.go", "package p\n\n// Server holds the listeners.\ntype Server struct {\n\tn int\n}\n", "Server", 4, "Server holds the listeners."),
        ("a.go", "package p\n\n// Mode says how it runs.\ntype Mode int\n", "Mode", 4, "Mode says how it runs."),
        ("a.go", "package p\n\n// Celsius is a unit.\ntype Celsius = float64\n", "Celsius", 4, "Celsius is a unit."),
        ("a.go", "package p\n\nfunc f() {\n\t// M maps names.\n\ttype M = map[string]int\n\t_ = M{}\n}\n", "f.M", 5, "M maps names."),
        ("a.go", "package p\n\nfunc g() { // not about N\n\ttype N int\n}\n", "g.N", 4, ""),
        ("a.go", "package p\n\n// The kinds.\ntype (\n\t// Pair keeps two.\n\tPair struct{ a, b int }\n\tBare int\n)\n", "Pair", 6, "Pair keeps two."),
        ("a.go", "package p\n\n// The kinds.\ntype (\n\t// Pair keeps two.\n\tPair struct{ a, b int }\n\tBare int\n)\n", "Bare", 7, ""),
        ("a.go", "package p\n\nconst (\n\t// A doc.\n\tA = 1\n\tB = 2\n)\n", "A", 5, "A doc."),
        ("a.go", "package p\n\nconst (\n\t// A doc.\n\tA = 1\n\tB = 2\n)\n", "B", 6, ""),
        ("a.rb", "# Lock time.\nLOCK_SECONDS = 30\n", "LOCK_SECONDS", 2, "Lock time."),
        ("a.rb", "module M\n  class K\n    # The name.\n    Name = \"x\".freeze\n  end\nend\n", "M.K.Name", 4, "The name."),
    ];
    let mut wrong = Vec::new();
    for &(path, src, qualname, start, doc) in cases {
        let chunks = parse_source(path, src);
        let got = chunks.iter().find(|c| c.qualname == qualname).map(|c| (c.start, c.doc.as_str()));
        if got != Some((start, doc)) {
            wrong.push(format!("{path} {qualname}: {got:?}, want {:?}", (start, doc)));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn an_escaped_comma_is_part_of_an_excluded_directory_name() {
    assert_eq!(scrutate::files::split_names(r"gen\,dir, vendor ,,a\b"), vec!["gen,dir", "vendor", r"a\b"]);
}

fn find<'a>(chunks: &'a [scrutate::parse::Chunk], qualname: &str) -> &'a scrutate::parse::Chunk {
    chunks.iter().find(|c| c.qualname == qualname).unwrap_or_else(|| panic!("no {qualname} in {:?}", chunks.iter().map(|c| &c.qualname).collect::<Vec<_>>()))
}

fn windows(path: &str, src: &str) -> Vec<(String, i64, i64)> {
    parse_source(path, src).into_iter().filter(|c| c.kind == "window").map(|c| (c.qualname, c.start, c.end)).collect()
}

#[test]
fn lines_no_chunk_keeps_are_windows_named_after_the_definition_that_holds_them() {
    // past the 200 lines a function keeps, and past the 40 a class keeps
    let mut py = String::from("def long(x):\n");
    py += &"    x = x + 1\n".repeat(260);
    py += "\n\nclass K:\n";
    py += &"    a = 1\n".repeat(45);
    py += "    def m(self):\n        return 1\n    b = 2\n";
    let w = windows("a.py", &py);
    assert_eq!(w, [("long".into(), 201, 250), ("long".into(), 241, 261), ("K".into(), 304, 309), ("K".into(), 312, 312)]);
    // top-level code after a definition is the module's; a lone closer makes no window
    let js = "function helper() {\n  return 1;\n}\n\napp.get('/x', (req, res) => {\n  res.send(helper());\n});\n";
    assert_eq!(windows("server.js", js), [("server".into(), 5, 7)]);
    let rs = "fn a() {\n    if x {\n        y();\n    }\n}\n\nfn b() {}\n";
    assert!(windows("a.rs", rs).is_empty());
    // a line only in a constant has no postings, so it is in a window too
    assert_eq!(windows("a.go", "package p\n\nfunc f() {}\n\nconst Max = 3\n"), [("a".into(), 5, 5)]);
}

#[test]
fn a_line_of_separator_control_characters_is_not_blank() {
    // U+001C to U+001F are white space to Python's strip and not to `trim`
    let py = "def f():\n    \"\"\"Doc.\"\"\"\n    return 1\n\u{1c}\nx = 1\n\u{1d}\u{1e}\u{1f}\n\n";
    assert_eq!(windows("a.py", py), [("a".into(), 4, 6)]);
    // a no-break space and a line separator are white space to both
    assert_eq!(windows("a.py", "def f():\n    return 1\n\u{a0}\nx = 1\n\u{2028}\n"), [("a".into(), 4, 4)]);
}

#[test]
fn a_definitions_documentation_is_in_no_window() {
    // each file: a first definition, then a documented one whose comment has lines without a comment mark
    let cases: &[(&str, &str)] = &[
        ("a.c", "int a(void) { return 1; }\n\n/* Return the total.\n   Second line.  */\nint total(void) { return 2; }\n"),
        ("a.js", "function a() {}\n\n/**\n Returns the total.\n */\nfunction total() {}\n"),
        ("a.go", "package p\n\nfunc a() {}\n\n/*\nTotal returns it.\n*/\nfunc Total() {}\n"),
        ("a.rs", "fn a() {}\n\n/*\nReturns the total.\n*/\nfn total() {}\n"),
        ("a.rs", "fn a() {}\n\n/*\nA target.\n*/\n#[derive(Debug)]\nstruct T;\n"),
        ("a.php", "<?php\nfunction a() {}\n\n/*\nReturns the total.\n*/\nfunction total() {}\n"),
        ("Main.elm", "module Main exposing (..)\n\n\nb : Int\nb =\n    1\n\n\n{-| Adds one.\nmore text\n-}\nadd : Int -> Int\nadd x =\n    x + 1\n"),
        ("A.hs", "module A where\n\nb :: Int\nb = 1\n\n{- | Adds one.\nmore text\n-}\nadd :: Int -> Int\nadd x = x + 1\n"),
        ("a.ps1", "function A { 1 }\n\n<#\n.SYNOPSIS\nAdds one.\n#>\nfunction Add-One { 1 }\n"),
        ("a.lua", "function a() end\n\n--[[\nAdds one.\n]]\nfunction add(x) return x end\n"),
        ("a.rb", "def a; end\n\n=begin\nAdds one.\n=end\ndef add(x); x; end\n"),
        ("a.jl", "function a() end\n\n#=\nAdds one.\n=#\nfunction add(x) x end\n"),
        ("a.jl", "function a() end\n\n\"\"\"\nAdds one.\n\"\"\"\nfunction add(x) x end\n"),
        ("a.vim", "function! A()\nendfunction\n\n\" Adds one.\nfunction! Add(x)\n  return a:x\nendfunction\n"),
        ("a.ex", "defmodule M do\n  def a, do: 1\n\n  @doc \"\"\"\n  Adds one.\n  \"\"\"\n  def f(x), do: x\nend\n"),
        ("a.py", "def a():\n    pass\n\n# Cached.\n@cache\ndef f():\n    pass\n"),
    ];
    let mut wrong = Vec::new();
    for &(path, src) in cases {
        let w = windows(path, src);
        let last = parse_source(path, src).into_iter().filter(|c| !matches!(c.kind, "module" | "window")).max_by_key(|c| c.start);
        if !w.is_empty() || last.is_none_or(|c| c.doc.is_empty()) {
            wrong.push(format!("{path}: {w:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    // a blank line between: the comment is no documentation, so a window holds it
    assert_eq!(windows("a.c", "int a(void) { return 1; }\n\n/* stray\n   note */\n\nint total(void) { return 2; }\n"), [("a".into(), 3, 4)]);
    // an attribute between the documentation and a definition that does not start at it stays searchable
    assert_eq!(windows("a.erl", "-module(a).\n\ng() -> 1.\n\n%% Adds one.\n-spec f(integer()) -> integer().\nf(X) -> X + 1.\n"), [("a".into(), 6, 6)]);
}

#[test]
fn without_a_grammar_the_comment_above_a_definition_is_its_documentation() {
    // (file, source, where `first` ends, a word of the documentation of `second`); the keyword tier
    // reads these unless the build has their grammar, which must agree
    let cases: &[(&str, &str, i64, &str)] = &[
        ("a.ml", "let first x = x + 1\n\n(** Computes the total. *)\nlet second y = first y\n", 1, "Computes"),
        ("a.ml", "let first x = x + 1\n\n(*\n   Computes the total.\n*)\nlet second y = first y\n", 1, "Computes"),
        (
            "a.f90",
            "subroutine first()\nend subroutine first\n\n! Computes the total.\nsubroutine second()\n  call first()\nend subroutine second\n",
            2,
            "Computes",
        ),
        ("a.f", "      subroutine first()\n      end\nC     Computes the total.\n      subroutine second()\n      end\n", 2, "Computes"),
        ("a.purs", "data First = First\n\n{- | Computes the total.\n   Over two lines. -}\ndata Second = Second\n", 1, "Computes"),
        ("a.sql", "create table first (x int);\n\n/* Computes the total.\n   Over two lines. */\ncreate table second (y int);\n", 1, "Computes"),
    ];
    let mut wrong = Vec::new();
    for &(path, src, end, word) in cases {
        // the Fortran grammar reads free form only: it finds no subroutine in fixed form
        if path == "a.f" && cfg!(feature = "extra-langs") {
            continue;
        }
        let chunks = parse_source(path, src);
        let first = chunks.iter().find(|c| c.name.eq_ignore_ascii_case("first")).map(|c| c.end);
        let second = chunks.iter().find(|c| c.name.eq_ignore_ascii_case("second")).map(|c| c.doc.as_str());
        if first != Some(end) || !second.is_some_and(|d| d.contains(word)) {
            wrong.push(format!("{path}: first ends at {first:?}, second's doc {second:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    // a mark of another language starts code here: these lines stay in the chunk above
    for (path, src) in [
        ("a.nim", "proc first() =\n  discard\n\"text\".echo\nproc second() =\n  discard\n"),
        ("a.nim", "proc first() =\n  discard\n(*p).x = 1\nproc second() =\n  discard\n"),
        ("a.cr", "def first\n  1\n!done\ndef second\n  2\nend\n"),
    ] {
        assert_eq!(parse_source(path, src).iter().find(|c| c.name == "first").map(|c| c.end), Some(3), "{path}: {src}");
    }
}

/// (kind, start, end, calls) of the module chunk and the windows that hold calls.
fn outside_calls(path: &str, src: &str) -> Vec<(String, i64, i64, Vec<String>)> {
    parse_source(path, src)
        .into_iter()
        .filter(|c| matches!(c.kind, "module" | "window") && !c.calls.is_empty())
        .map(|c| (c.kind.to_string(), c.start, c.end, c.calls.into_iter().collect()))
        .collect()
}

fn owned(v: &[(&str, i64, i64, &[&str])]) -> Vec<(String, i64, i64, Vec<String>)> {
    v.iter().map(|(k, a, b, c)| (k.to_string(), *a, *b, c.iter().map(|s| s.to_string()).collect())).collect()
}

#[test]
fn a_call_outside_every_definition_belongs_to_the_chunk_that_holds_its_line() {
    let py = "# job.py\ndef helper(x):\n    return x + 1\n\nif __name__ == \"__main__\":\n    total = helper(2)\n";
    assert_eq!(outside_calls("job.py", py), owned(&[("window", 5, 6, &["helper"])]));
    let js = "function onReady() {}\nfunction main() {\n  return 1;\n}\napp.listen(port, onReady);\nmain().catch(console.error);\n";
    assert_eq!(outside_calls("server.js", js), owned(&[("window", 5, 6, &["catch", "listen", "main"])]));
    let sh = "main() {\n  echo hi\n}\n\nmain \"$@\"\n";
    let got = outside_calls("run.sh", sh);
    assert!(got.len() == 1 && got[0].0 == "window" && got[0].3.contains(&"main".to_string()), "{got:?}");
    // a call before the first definition is the module chunk's
    let header = "setup();\n\nfunction setup() {\n  return 1;\n}\n";
    assert_eq!(outside_calls("app.js", header), owned(&[("module", 1, 2, &["setup"])]));
    // where two windows overlap, the first takes the call; a call inside a definition stays there
    let mut long = String::from("function f() {\n  return 1;\n}\n");
    long += &"x = 1;\n".repeat(44);
    long += "f();\n";
    long += &"y = 2;\n".repeat(20);
    let got = outside_calls("long.js", &long);
    assert_eq!(got, owned(&[("window", 4, 53, &["f"])]));
    assert!(find(&parse_source("long.js", &long), "f").calls.is_empty());
}

#[test]
fn many_top_level_calls_parse_in_linear_time() {
    assert_parse_scales_linearly(
        "a.py",
        25_000,
        |n| "f(1)\n".repeat(n),
        |n, chunks| {
            let windows = chunks.iter().filter(|c| c.kind == "window").count();
            assert!(windows >= n / 40 - 2, "{windows} windows for {n} lines");
            assert!(chunks.iter().filter(|c| c.kind == "window").all(|c| c.calls.contains("f")));
        },
    );
}

#[test]
fn chains_of_calls_parse_in_linear_time() {
    let shapes: [(&str, &str, &str, &str, &str); 9] = [
        ("a.js", "", "a", "()", ";\n"),
        ("a.js", "", "x", ".a()", ";\n"),
        ("a.py", "", "a", "()", "\n"),
        ("a.py", "", "x", ".a()", "\n"),
        ("a.rs", "fn f() { ", "a", "()", "; }\n"),
        ("a.rs", "fn f() { ", "x", ".a()", "; }\n"),
        ("a.cpp", "void f() { ", "a", "()", "; }\n"),
        ("a.cpp", "void f() { ", "x", ".a()", "; }\n"),
        ("a.php", "<?php\n", "\\A\\b", "()", ";\n"),
    ];
    for (path, open, head, link, close) in shapes {
        let make = move |n: usize| format!("{open}{head}{}{close}", link.repeat(n));
        assert_parse_scales_linearly(path, 4000, make, move |_, chunks| {
            let name = if link == "()" { head.rsplit('\\').next().unwrap() } else { "a" };
            assert!(chunks.iter().any(|c| c.calls.contains(name)), "{path} {head}{link}");
        });
    }
    // only the first call of `a.operator()()()` calls an operator: the others are no names as long as the chain
    let make = |n: usize| format!("void f() {{ a.operator{}; }}\n", "()".repeat(n));
    assert_parse_scales_linearly("a.cpp", 4000, make, |_, chunks| {
        let names: Vec<&String> = chunks.iter().flat_map(|c| &c.calls).collect();
        assert!(names.contains(&&"operator()".to_string()) && names.iter().all(|n| n.len() <= 16), "{names:?}");
    });
    let make = |n: usize| format!("x := {}{}\n", "$(call f,".repeat(n), ")".repeat(n));
    assert_parse_scales_linearly("Makefile", 16000, make, |_, chunks| assert!(chunks.iter().any(|c| c.calls.contains("f"))));
}

#[test]
fn nested_php_includes_and_c_includes_parse_in_linear_time() {
    for word in ["include", "require_once"] {
        let make = move |n: usize| format!("<?php\n{}'x.php';\n", format!("{word} ").repeat(n));
        assert_parse_scales_linearly("a.php", 4000, make, |_, _| {});
        let imports = scrutate::parse::parse_file("a.php", &make(3)).imports;
        assert!(!imports.is_empty() && imports.iter().all(|(line, m)| *line == 2 && m == "x.php"), "{word}: {imports:?}");
    }
    let make = |n: usize| format!("{}#include \"x.h\"\n{}", "#if A\n".repeat(n), "#endif\n".repeat(n));
    assert_parse_scales_linearly("a.c", 4000, make, |_, _| {});
    assert_eq!(scrutate::parse::parse_file("a.c", &make(3)).imports, vec![(4, "x.h".to_string())]);
}

#[test]
fn declarations_in_deeply_nested_preprocessor_blocks_parse_in_linear_time() {
    for (path, decl) in [("a.c", "const int X = 1;"), ("a.cpp", "int f();")] {
        let make = move |n: usize| format!("{}{}", format!("#if A\n{decl}\n").repeat(n), "#endif\n".repeat(n));
        assert_parse_scales_linearly(path, 1500, make, move |n, chunks| {
            let name = if path == "a.c" { "X" } else { "f" };
            assert_eq!(chunks.iter().filter(|c| c.name == name).count(), n, "{path}");
        });
    }
}

#[test]
fn windows_of_very_long_lines_keep_every_line() {
    // 60 lines of 2,000 bytes: 50 of them are 100 KB, three times what a chunk's text keeps
    let line = "x".repeat(1_990);
    let src: String = (0..60).map(|i| format!("# word{i:04} {line}\n")).collect();
    let chunks = parse_source("data.py", &src);
    for i in 0..60 {
        let word = format!("word{i:04} ");
        assert!(chunks.iter().any(|c| c.text.contains(&word)), "line {} is in no chunk's text", i + 1);
    }
    // the module chunk keeps the first 16 lines, the windows the rest, none of them cut
    let windows: Vec<_> = chunks.iter().filter(|c| c.kind == "window").collect();
    assert!(windows.iter().all(|w| !w.text.ends_with("\n...")), "no window is cut");
    assert_eq!(windows.iter().map(|w| (w.start, w.end)).collect::<Vec<_>>(), [(17, 32), (33, 48), (49, 60)]);
}

#[test]
fn a_definition_starts_at_the_attribute_lines_right_above_it() {
    let rs = "/// Shows one.\n#[get(\"/users/show\")]\n#[inline]\npub fn show() {}\n\n#[cfg(test)]\n\nmod tests {}\n";
    let chunks = parse_source("a.rs", rs);
    let show = find(&chunks, "show");
    assert_eq!((show.start, show.doc.as_str()), (2, "Shows one."));
    assert!(show.text.starts_with("#[get(\"/users/show\")]\n#[inline]\npub fn show"), "{:?}", show.text);
    assert_eq!(find(&chunks, "tests").start, 8, "a blank line keeps an attribute apart");
    let ts = "class A {\n  @HostListener('window:resize')\n  onResize() {\n    return 1;\n  }\n}\n";
    let chunks = parse_source("a.ts", ts);
    let m = find(&chunks, "A.onResize");
    assert_eq!(m.start, 2);
    assert!(m.text.starts_with("@HostListener"), "{:?}", m.text);
    // where a line that starts with @ is no attribute, the definition starts on its own line
    let chunks = parse_source("Makefile", "build:\n\t@echo building\ntest: build\n\t@cargo test\n");
    assert_eq!((find(&chunks, "build").start, find(&chunks, "test").start), (1, 3));
    let chunks = parse_source("a.rb", "class A\n  @count = 0\n  def incr\n    1\n  end\nend\n");
    assert_eq!(find(&chunks, "A.incr").start, 3);
    let chunks = parse_source("a.m", "@interface Greeter : NSObject\n- (void)greet;\n@end\n@implementation Greeter\n- (void)greet {}\n@end\n");
    assert!(
        chunks.iter().filter(|c| c.name == "greet").all(|c| c.start == 2 || c.start == 5),
        "{:?}",
        chunks.iter().map(|c| (c.qualname.as_str(), c.start)).collect::<Vec<_>>()
    );
}

#[test]
fn a_generic_owner_is_named_by_its_bare_type() {
    let rs = "pub struct Stack<T> { items: Vec<T> }\nimpl<T> Stack<T> {\n    pub fn push(&mut self, x: T) {}\n}\n\
              impl<'a, T: Clone> Wrapper<'a, T> {\n    pub fn get(&self) -> T { todo!() }\n}\n\
              impl<T: Clone> Clone for Wrapper<'_, T> {\n    fn clone(&self) -> Self { todo!() }\n}\n\
              impl std::fmt::Display for Wrapper<'_, u8> {\n    fn fmt(&self) {}\n}\nimpl crate::Thing {\n    pub fn run(&self) {}\n}\n";
    let chunks = parse_source("more.rs", rs);
    for (q, name) in [("Stack.push", "push"), ("Wrapper.get", "get"), ("Wrapper.clone", "clone"), ("Wrapper.fmt", "fmt"), ("Thing.run", "run")] {
        assert_eq!(find(&chunks, q).name, name);
    }
    let impls: Vec<&str> = chunks.iter().filter(|c| c.kind == "impl").map(|c| c.name.as_str()).collect();
    assert_eq!(impls, ["Stack", "Wrapper", "Wrapper", "Wrapper", "Thing"]);
    let cpp = parse_source("box.cpp", "template <class T>\nvoid Box<T>::put(T x) {}\nvoid ns::Map<K, std::vector<V>>::get() {}\n");
    assert_eq!(find(&cpp, "Box.put").name, "put");
    assert_eq!(find(&cpp, "ns.Map.get").name, "get");
}

#[test]
fn a_dotted_definition_name_is_its_last_part_under_its_owner() {
    let lua =
        parse_source("mod.lua", "local M = {}\n\nfunction M.helper(x)\n  return x\nend\n\nfunction M.sub.deep(x)\n  return x\nend\n\nfunction M:meth()\nend\n");
    for (q, name) in [("M.helper", "helper"), ("M.sub.deep", "deep"), ("M.meth", "meth")] {
        assert_eq!(find(&lua, q).name, name);
    }
    let caller = parse_source("use.lua", "local function go()\n  return M.helper(1)\nend\n");
    assert!(find(&caller, "go").calls.contains("helper"));
    let ex =
        parse_source("mix.ex", "defmodule MyApp.Accounts.User do\n  def changeset(user), do: user\nend\n\ndefprotocol MyApp.Sizeable do\n  def size(x)\nend\n");
    assert_eq!(find(&ex, "MyApp.Accounts.User").name, "User");
    assert_eq!(find(&ex, "MyApp.Accounts.User.changeset").name, "changeset");
    assert_eq!(find(&ex, "MyApp.Sizeable").name, "Sizeable");
    // a dot is part of an R name
    let r = parse_source("stats.R", "read.table2 <- function(x) x\nprint.myclass <- function(x) x\n");
    assert_eq!(find(&r, "read.table2").name, "read.table2");
    assert_eq!(find(&r, "print.myclass").name, "print.myclass");
}

fn constants(path: &str, src: &str) -> Vec<(String, String, String)> {
    parse_source(path, src).into_iter().filter(|c| c.kind == "constant").map(|c| (c.name, c.qualname, c.text)).collect()
}

fn named(got: &[(String, String, String)]) -> Vec<&str> {
    got.iter().map(|(_, q, _)| q.as_str()).collect()
}

#[test]
fn constants_are_symbols_in_every_language_the_reference_has() {
    let rs = constants(
        "c.rs",
        "/// The cap.\npub const LIMIT: usize = 3;\npub static NAME: &str = \"x\";\nimpl<T> S<T> {\n    const N: u8 = 1;\n    fn f() {\n        const IN: u8 = 2;\n    }\n}\nconst _: () = ();\n",
    );
    assert_eq!(named(&rs), ["LIMIT", "NAME", "S.N"]);
    assert_eq!(rs[0].2, "pub const LIMIT: usize = 3;");
    let go = constants("c.go", "package p\n\nconst A = 1\nconst (\n\tB = 2\n\tC\n)\nvar X, Y int\nvar (\n\tZ = 3\n)\nvar _ = 4\nfunc f() { const L = 1 }\n");
    assert_eq!(named(&go), ["A", "B", "C", "X", "Y", "Z"]);
    assert_eq!((go[0].2.as_str(), go[1].2.as_str()), ("const A = 1", "B = 2"));
    let ts_src = "export const MAX = 3;\nconst f = () => 1, k = 2;\nlet notc = 1;\nconst { a, b } = o;\nenum Color { Red, Green = 2 }\nfunction g() { const inner = 1; }\n";
    let ts = constants("c.ts", ts_src);
    assert_eq!(named(&ts), ["MAX", "k", "Color.Red", "Color.Green"]);
    assert_eq!(ts[0].2, "export const MAX = 3;");
    assert_eq!(named(&constants("c.js", "export const MAX = 3;\nconst f = function () {};\nvar v = 1;\n")), ["MAX"]);
    let java = constants("C.java", "class C {\n  public static final int MAX = 3;\n  static int n;\n  final int m = 1;\n}\n");
    assert_eq!(named(&java), ["C.MAX"]);
    let py = constants("c.py", "LIMIT = 3\n_PRIVATE_CAP: int = 4\nlower = 5\nA, B = 1, 2\nclass K:\n    INNER = 1\ndef f():\n    LOCAL = 2\n");
    assert_eq!(named(&py), ["LIMIT", "_PRIVATE_CAP", "A", "B", "K.INNER"]);
    let c = constants("c.h", "#define LIMIT 5\n#define GUARD_H\n#define MAX(a, b) ((a) > (b) ? (a) : (b))\nenum Color { RED, GREEN = 2 };\n");
    assert_eq!(named(&c), ["LIMIT", "Color.RED", "Color.GREEN"]);
    assert_eq!(named(&constants("c.cpp", "#define LIMIT 5\nenum class Mode { On, Off };\n")), ["LIMIT", "Mode.On", "Mode.Off"]);
}

#[test]
fn constants_come_last_and_do_not_move_the_file_header() {
    let src = "use std::fmt;\n\npub const LIMIT: usize = 3;\n\nfn f() -> usize {\n    LIMIT\n}\n";
    let chunks = parse_source("m.rs", src);
    let kinds: Vec<&str> = chunks.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, ["module", "function", "constant"]);
    assert_eq!((chunks[0].start, chunks[0].end), (1, 4));
}

#[test]
fn constants_are_found_by_name_but_never_ranked() {
    let root = tree(
        "constants",
        &[(
            "src/lib.rs",
            "/// How many retries.\npub const RETRY_LIMIT: usize = 3;\n\n/// Retries a request up to the limit.\npub fn retry_request() -> usize {\n    RETRY_LIMIT\n}\n",
        )],
    );
    let mut repo = scrutate::Repo::open(&root).unwrap();
    repo.update(false).unwrap();
    let hits = repo.find_symbol("RETRY_LIMIT", 5).unwrap();
    assert_eq!(hits.iter().map(|h| (h.kind.as_str(), h.qualname.as_str())).collect::<Vec<_>>(), [("constant", "RETRY_LIMIT")]);
    let outline = repo.ix.outline("src/lib.rs").unwrap();
    assert!(outline.iter().any(|r| r.kind == "constant" && r.name == "RETRY_LIMIT"));
    for q in ["RETRY_LIMIT", "retry limit", "how many retries"] {
        let cands = repo.candidates(q, true).unwrap();
        assert!(cands.rows.iter().all(|r| r.kind != "constant"), "{q}: {:?}", cands.rows.iter().map(|r| &r.qualname).collect::<Vec<_>>());
    }
    let lexed = |repo: &mut scrutate::Repo| -> i64 { repo.ix.check(false).unwrap().chunks_db };
    let n = lexed(&mut repo);
    assert_eq!(repo.ix.lex().unwrap().n_alive as i64, n);
    let report = repo.ix.check(false).unwrap();
    assert!(report.ok(), "{report:?}");
    std::fs::write(root.join("src/lib.rs"), "pub const RETRY_LIMIT: usize = 4;\npub fn retry_request() {}\n").unwrap();
    repo.update(false).unwrap();
    let n = lexed(&mut repo);
    assert_eq!(repo.ix.lex().unwrap().n_alive as i64, n);
    let report = repo.ix.check(false).unwrap();
    assert!(report.ok(), "{report:?}");
}

const BURST_RS: &str = "/// Keeps the files of one burst.\npub struct BurstFiles {\n    n: usize,\n}\n\nimpl BurstFiles {\n    /// Collects the files of a burst.\n    pub fn collect(&self) -> usize {\n        self.n\n    }\n}\n\n/// The files of one burst.\npub fn burst_files() -> usize {\n    0\n}\n";

#[test]
fn a_name_as_typed_is_a_feature_and_an_impl_is_its_own_hit() {
    let root = tree("case_impl", &[("src/lib.rs", BURST_RS)]);
    let mut repo = scrutate::Repo::open(&root).unwrap();
    repo.update(false).unwrap();
    let f = |n: &str| scrutate::search::FEATURES.iter().position(|x| *x == n).unwrap();
    let get = |c: &scrutate::search::Candidates, kind: &str, name: &str, feat: &str| {
        let i = c.rows.iter().position(|r| r.kind == kind && r.name == name).unwrap_or_else(|| panic!("{kind} {name}"));
        c.x[i][f(feat)]
    };
    let c = repo.candidates("BurstFiles", true).unwrap();
    assert_eq!(get(&c, "struct", "BurstFiles", "exact_case"), 1.0);
    assert_eq!(get(&c, "function", "burst_files", "exact_case"), 0.0);
    // typed in a style neither is written in: both are as near as it gets
    let c = repo.candidates("burstFiles", true).unwrap();
    assert_eq!((get(&c, "struct", "BurstFiles", "exact_case"), get(&c, "function", "burst_files", "exact_case")), (1.0, 1.0));
    // the struct and its impl block share a qualified name and are still two hits
    let hits = repo.search("BurstFiles", 10, false).unwrap();
    let kinds: Vec<&str> = hits.iter().filter(|h| h.name == "BurstFiles").map(|h| h.kind.as_str()).collect();
    assert!(kinds.contains(&"struct") && kinds.contains(&"impl"), "{kinds:?}");
    // the same name in another Unicode form is the name as typed
    let root = tree("case_nfc", &[("a.py", "def caf\u{e9}():\n    return 1\n\n\ndef cafe\u{301}_x():\n    return 2\n")]);
    let mut repo = scrutate::Repo::open(&root).unwrap();
    repo.update(false).unwrap();
    let c = repo.candidates("cafe\u{301}", true).unwrap();
    assert_eq!(get(&c, "function", "caf\u{e9}", "exact_case"), 1.0);
    assert_eq!(get(&c, "function", "cafe\u{301}_x", "exact_case"), 0.0);
}

#[test]
fn a_name_typed_with_its_punctuation_is_the_name_as_typed() {
    let root = tree(
        "case_punct",
        &[
            ("a.rb", "class Rec\n  def valid\n    @valid\n  end\n\n  def valid?\n    true\n  end\nend\n"),
            ("a.ps1", "function Greeting {\n    \"hi\"\n}\n\nfunction Get-Greeting {\n    \"hi\"\n}\n\nfunction Set-Greeting {\n    \"ho\"\n}\n"),
            ("a.scm", "(define (vector x) x)\n\n(define (list->vector x) x)\n"),
            ("a.js", "function http() {\n  return 1;\n}\n\nfunction $http() {\n  return 2;\n}\n"),
        ],
    );
    let mut repo = scrutate::Repo::open(&root).unwrap();
    repo.update(false).unwrap();
    let f = |n: &str| scrutate::search::FEATURES.iter().position(|x| *x == n).unwrap();
    // (exact_name, exact_case) of the candidate called `name`
    let mut get = |q: &str, name: &str| {
        let c = repo.candidates(q, true).unwrap();
        let i = c.rows.iter().position(|r| r.name == name).unwrap_or_else(|| panic!("{q}: no candidate {name}"));
        (c.x[i][f("exact_name")], c.x[i][f("exact_case")])
    };
    for (q, typed, plain) in
        [("valid?", "valid?", "valid"), ("Get-Greeting", "Get-Greeting", "Greeting"), ("list->vector", "list->vector", "vector"), ("$http", "$http", "http")]
    {
        assert_eq!(get(q, typed), (1.0, 1.0), "{q}: {typed}");
        assert_eq!(get(q, plain).1, 0.0, "{q}: {plain}");
    }
    // the last part of a dotted name, and the plain names as before
    assert_eq!(get("Rec.valid?", "valid?").1, 1.0);
    assert_eq!(get("Rec.valid?", "valid").1, 0.0);
    assert_eq!((get("valid", "valid").1, get("valid", "valid?").1), (1.0, 0.0));
    assert_eq!((get("Greeting", "Greeting").1, get("Greeting", "Get-Greeting").1), (1.0, 0.0));
    // in a sentence too
    assert_eq!((get("is the record valid? or not", "valid?").1, get("is the record valid? or not", "valid").1), (1.0, 0.0));
    // and the name typed whole is the first result, whatever the ranker makes of its words
    for q in ["valid?", "Get-Greeting", "list->vector", "$http"] {
        assert_eq!(repo.search(q, 3, false).unwrap()[0].name, q);
    }
    for (q, first) in [("valid", "valid"), ("Greeting", "Greeting"), ("vector", "vector"), ("http", "http")] {
        assert_eq!(repo.search(q, 3, false).unwrap()[0].name, first, "{q}");
    }
}

#[test]
fn swift_protocol_requirements_kotlin_type_aliases_and_elixir_guards_are_symbols() {
    let swift = symbols("c.swift", "protocol Payable {\n    func pay(amount: Int)\n}\n");
    assert!(has(&swift, "function", "Payable.pay"), "{swift:?}");
    let kt = symbols("b.kt", "package shop\n\ntypealias Handler = (Int) -> Unit\n");
    assert!(has(&kt, "type", "Handler"), "{kt:?}");
    let ex = symbols("f.ex", "defmodule Shop.Invoice do\n  defguard is_big(x) when x > 100\n  def f(x) when is_integer(x), do: x\nend\n");
    assert!(has(&ex, "function", "Shop.Invoice.is_big") && has(&ex, "function", "Shop.Invoice.f"), "{ex:?}");
}

#[test]
fn a_turbofish_call_is_a_call_and_a_struct_body_holds_methods() {
    let rs = parse_source("t.rs", "fn f() {\n    g::<u8>(1);\n    x.collect::<Vec<_>>();\n}\n");
    let calls: Vec<&str> = find(&rs, "f").calls.iter().map(|s| s.as_str()).collect();
    assert_eq!(calls, ["collect", "g"]);
    let got = symbols("p.hpp", "struct P {\n  ~P() = default;\n  P() {}\n};\n");
    assert_eq!(got, vec![("struct".into(), "P".into()), ("method".into(), "P.~P".into()), ("method".into(), "P.P".into())]);
}

#[test]
fn every_name_of_a_declaration_is_a_constant_with_the_declaration_as_its_text() {
    let go = constants("m.go", "package p\n\nvar X, Y int\nconst A, B = 1, 2\nconst (\n\tC, D = 3, 4\n\tH = 7\n)\nvar (\n\tE, F string\n)\nvar _, G = 5, 6\n");
    assert_eq!(named(&go), ["X", "Y", "A", "B", "C", "D", "H", "E", "F", "G"]);
    let texts: Vec<&str> = go.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(
        texts,
        [
            "var X, Y int",
            "var X, Y int",
            "const A, B = 1, 2",
            "const A, B = 1, 2",
            "C, D = 3, 4",
            "C, D = 3, 4",
            "H = 7",
            "E, F string",
            "E, F string",
            "var _, G = 5, 6"
        ]
    );
    let java = constants("M.java", "class M {\n  static final int A = 1, B = 2;\n  static int n, m;\n}\n");
    assert_eq!(named(&java), ["M.A", "M.B"]);
    assert!(java.iter().all(|(_, _, t)| t == "static final int A = 1, B = 2;"), "{java:?}");
    let js = constants("m.js", "const a = 1, b = 2;\nexport const c = 3, d = () => 4;\n");
    assert_eq!(named(&js), ["a", "b", "c"]);
    assert_eq!(js[1].2, "const a = 1, b = 2;");
}

fn calls_of(path: &str, src: &str, qualname: &str) -> Vec<String> {
    find(&parse_source(path, src), qualname).calls.iter().cloned().collect()
}

#[test]
fn a_method_call_records_the_method_not_the_receiver() {
    let cases = [
        ("m.lua", "function f()\n  obj:meth(1)\nend\n", "f"),
        ("m.lua", "function f()\n  obj.meth(1)\nend\n", "f"),
        ("m.php", "<?php\nfunction f() { $obj->meth(); }\n", "f"),
        ("m.php", "<?php\nfunction f() { Obj::meth(); }\n", "f"),
        ("m.kt", "fun f() { obj.meth(1) }\n", "f"),
        ("m.swift", "func f() { obj.meth(1) }\n", "f"),
        ("m.cs", "class C { void F() { obj.meth(1); } }\n", "C.F"),
        ("m.rb", "def f\n  obj.meth(1)\nend\n", "f"),
        ("m.java", "class C { void f() { obj.meth(1); } }\n", "C.f"),
        ("m.go", "package p\nfunc f() { obj.meth(1) }\n", "f"),
        ("m.py", "def f():\n    obj.meth(1)\n", "f"),
        ("m.js", "function f() { obj.meth(1); }\n", "f"),
        ("m.rs", "fn f() { obj.meth(1); }\n", "f"),
        ("m.cpp", "void f() { obj->meth(1); }\n", "f"),
        ("m.scala", "object O { def f() = obj.meth(1) }\n", "O.f"),
        ("m.mm", "void f(void) { [obj meth]; [obj meth:1 with:2]; }\n", "f"),
        ("m.zig", "fn f() void { obj.meth(1); }\n", "f"),
        ("m.jl", "function f()\n  obj.meth(1)\nend\n", "f"),
        ("m.dart", "void f() {\n  obj.meth(1);\n}\n", "f"),
        ("m.dart", "void f() {\n  obj..meth(1);\n}\n", "f"),
        ("m.dart", "void f() {\n  obj?.meth(1);\n}\n", "f"),
        ("m.groovy", "def f() {\n  obj.meth(1)\n}\n", "f"),
        ("m.groovy", "def f() {\n  obj.meth 1\n}\n", "f"),
        ("m.ex", "defmodule M do\n  def f(x), do: Mod.meth(x)\nend\n", "M.f"),
        ("m.ex", "defmodule M do\n  def f(x), do: Mod.Sub.meth(x)\nend\n", "M.f"),
        ("m.ex", "defmodule M do\n  def f(x), do: x |> Mod.meth()\nend\n", "M.f"),
        ("m.php", "<?php\nfunction f() { $obj?->meth(); }\n", "f"),
        ("m.r", "f <- function() {\n  obj$meth(1)\n}\n", "f"),
        ("m.ps1", "function f {\n  $obj.meth(1)\n}\n", "f"),
        ("m.scala", "object O { def f() = obj meth 1 }\n", "O.f"),
        ("m.adb", "procedure F is\nbegin\n  Obj.meth(1);\nend F;\n", "F"),
        ("m.pas", "program P;\nprocedure F;\nbegin\n  Obj.meth(1);\nend;\nbegin end.\n", "F"),
    ];
    let wrong: Vec<String> = cases
        .iter()
        .map(|&(path, src, q)| (path, calls_of(path, src, q)))
        .filter(|(_, got)| got != &["meth"])
        .map(|(p, got)| format!("{p}: {got:?}"))
        .collect();
    assert!(wrong.is_empty(), "{wrong:?}");
}

fn wrong_calls(cases: &[(&str, &str, &str, &[&str])]) -> Vec<String> {
    cases
        .iter()
        .map(|&(path, src, q, want)| (path, calls_of(path, src, q), want))
        .filter(|(_, got, want)| got != want)
        .map(|(p, got, want)| format!("{p}: got {got:?}, want {want:?}"))
        .collect()
}

#[test]
fn an_explicit_cpp_destructor_or_operator_call_records_that_name() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("m.cpp", "void f(S& a) { a.~S(); }\n", "f", &["~S"]),
        ("m.cpp", "void f(S* p) { p->~S(); }\n", "f", &["~S"]),
        ("m.cpp", "void f(S* p) { p->Base::~Base(); }\n", "f", &["~Base"]),
        ("m.cpp", "void f(S& a, S& b) { a.operator==(b); }\n", "f", &["operator=="]),
        ("m.cpp", "void f(S* p) { p->operator()(3); }\n", "f", &["operator()"]),
        ("m.cpp", "void f(S& a) { a.operator[](1); }\n", "f", &["operator[]"]),
        ("m.cpp", "void f(S& b) { S::operator+(b); }\n", "f", &["operator+"]),
        // the bitwise operator before a call is no destructor
        ("m.cpp", "int f() { return ~bits(); }\n", "f", &["bits"]),
    ];
    let wrong = wrong_calls(cases);
    assert!(wrong.is_empty(), "{wrong:?}");
}

#[test]
fn a_plain_call_and_a_constructor_call_record_the_called_name() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("m.py", "def f():\n    g(1)\n    Point(1, 2)\n", "f", &["Point", "g"]),
        ("m.js", "function f() { g(1); new Point(1, 2); }\n", "f", &["Point", "g"]),
        ("m.ts", "function f() { g(1); new Point(1, 2); }\n", "f", &["Point", "g"]),
        ("m.tsx", "function f() { g(1); new Point(1, 2); }\n", "f", &["Point", "g"]),
        ("m.go", "package p\nfunc f() { g(1) }\n", "f", &["g"]),
        ("m.rs", "fn f() { g(1); Point::new(1, 2); m!(x); }\n", "f", &["g", "m", "new"]),
        ("m.java", "class C { void f() { g(1); new Point(1, 2); } }\n", "C.f", &["Point", "g"]),
        ("m.c", "void f(void) { g(1); }\n", "f", &["g"]),
        ("m.cpp", "void f() { g(1); new Point(1, 2); Size(3, 4); }\n", "f", &["Point", "Size", "g"]),
        ("m.glsl", "void f() { g(1); vec3 v = vec3(1.0); }\n", "f", &["g", "vec3"]),
        ("m.mm", "void f(void) { g(1); [[Point alloc] init]; }\n", "f", &["alloc", "g", "init"]),
        ("m.cs", "class C { void F() { g(1); new Point(1, 2); } }\n", "C.F", &["Point", "g"]),
        ("m.rb", "def f\n  g(1)\n  Point.new(1, 2)\n  bare 1\nend\n", "f", &["bare", "g", "new"]),
        ("m.php", "<?php\nfunction f() { g(1); new Point(1, 2); }\n", "f", &["Point", "g"]),
        ("m.sh", "f() {\n  g 1\n}\n", "f", &["g"]),
        ("m.fish", "function f\n  g 1\nend\n", "f", &["g"]),
        ("m.ps1", "function f {\n  g 1\n  [Point]::new(1, 2)\n}\n", "f", &["g", "new"]),
        ("m.kt", "fun f() { g(1); Point(1, 2) }\n", "f", &["Point", "g"]),
        ("m.swift", "func f() { g(1); Point(x: 1) }\n", "f", &["Point", "g"]),
        ("m.scala", "object O { def f() = { g(1); new Point(1, 2) } }\n", "O.f", &["Point", "g"]),
        ("m.groovy", "def f() {\n  g(1)\n  new Point(1, 2)\n  println 'x'\n}\n", "f", &["Point", "g", "println"]),
        ("m.lua", "function f()\n  g(1)\nend\n", "f", &["g"]),
        ("m.ex", "defmodule M do\n  def f(x) do\n    g(x)\n    x |> h()\n    bare x\n  end\nend\n", "M.f", &["bare", "g", "h"]),
        ("m.erl", "-module(m).\nf() -> g(1).\n", "f", &["g"]),
        ("m.hs", "f x = g x\n", "f", &["g"]),
        ("m.elm", "f x =\n  g x\n", "f", &["g"]),
        ("m.zig", "fn f() void { g(1); }\n", "f", &["g"]),
        ("m.jl", "function f()\n  g(1)\n  Point(1, 2)\nend\n", "f", &["Point", "g"]),
        ("m.dart", "void f() {\n  g(1);\n  Point(1, 2);\n  new Size(1, 2);\n  const Box(1);\n  Cls.make();\n}\n", "f", &["Box", "Point", "Size", "g", "make"]),
        ("m.dart", "class C {\n  void m() {\n    g(1);\n  }\n}\n", "C.m", &["g"]),
        ("m.r", "f <- function() {\n  g(1)\n}\n", "f", &["g"]),
        ("m.nix", "{ f = x: g x; }\n", "f", &["g"]),
        ("m.tf", "locals {\n  v = g(1)\n}\n", "locals", &["g"]),
        ("m.sol", "contract C { function f() public { g(1); new Point(1, 2); } }\n", "C.f", &["Point", "g"]),
        ("m.pas", "program P;\nprocedure F;\nbegin\n  G(1);\nend;\nbegin end.\n", "F", &["G"]),
        ("m.adb", "procedure F is\n  X : Integer;\nbegin\n  G(1);\n  X := H(2);\nend F;\n", "F", &["G", "H"]),
        ("m.m", "function f()\n  g(1);\nend\n", "f", &["g"]),
        ("m.el", "(defun f (x)\n  (g x))\n", "f", &["g"]),
        ("m.scm", "(define (f x)\n  (g x))\n", "f", &["g"]),
        ("m.rkt", "#lang racket\n(define (f x)\n  (g x))\n", "f", &["g"]),
        ("m.lisp", "(defun f (x)\n  (g x))\n", "f", &["g"]),
        ("m.vim", "function! F()\n  call G(1)\nendfunction\n", "F", &["G"]),
        ("m.gleam", "pub fn f() {\n  g(1)\n  Point(1, 2)\n}\n", "f", &["Point", "g"]),
        ("m.bzl", "def f():\n    g(1)\n", "f", &["g"]),
        ("CMakeLists.txt", "function(f)\n  g(1)\nendfunction()\n", "f", &["g"]),
        ("m.bicep", "func f(x int) int => g(x)\n", "f", &["g"]),
        ("m.pp", "class foo {\n  $x = g(1)\n}\n", "foo", &["g"]),
    ];
    let wrong = wrong_calls(cases);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[cfg(feature = "extra-langs")]
#[test]
fn calls_are_recorded_in_the_optional_grammars() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("m.cu", "void f() { g(1); obj.meth(1); k<<<1, 2>>>(3); }\n", "f", &["g", "k", "meth"]),
        ("m.ml", "let f x =\n  g x;\n  Mod.meth x\n", "f", &["g", "meth"]),
        ("m.ml", "let step' x = x\nlet f x = step' x\n", "step'", &[]),
        ("m.ml", "let step' x = x\nlet f x = step' x\n", "f", &["step'"]),
        ("m.fs", "let step' x = x\nlet f x =\n  step' x\n", "f", &["step'"]),
        ("m.fs", "let f x =\n  g x\n  obj.meth(1)\n  Point(1, 2)\n", "f", &["Point", "g", "meth"]),
        ("m.f90", "subroutine f()\n  call g(1)\n  x = h(2)\nend subroutine f\n", "f", &["g", "h"]),
        ("m.d", "void f() { g(1); obj.meth(1); new Point(1, 2); }\n", "f", &["Point", "g", "meth"]),
        ("m.v", "module m;\n  function int f(int x);\n    return g(x);\n  endfunction\nendmodule\n", "m.f", &["g"]),
        ("m.odin", "package p\nf :: proc() {\n  g(1); obj.meth(1)\n}\n", "f", &["g", "meth"]),
        ("m.ll", "define i32 @f() {\n  %1 = call i32 @g(i32 1)\n  ret i32 %1\n}\n", "f", &["g"]),
        ("m.qml", "Item {\n  function f() { g(1); obj.meth(1); }\n}\n", "Item.f", &["g", "meth"]),
        ("m.tla", "---- MODULE m ----\nF(x) == G(x) + 1\n====\n", "m.F", &["G"]),
        ("m.ahk", "f() {\n  g(1)\n  obj.meth(1)\n}\n", "f", &["g", "meth"]),
        ("m.sv", "module m;\n  task t;\n    int y;\n    h(1);\n    h2(1, 2);\n    y = 1;\n  endtask\nendmodule\n", "m.t", &["h", "h2"]),
        ("m.v", "module m;\n  always @(posedge clk) begin\n    h(2);\n  end\n  adder u1 (.a(a), .b(b));\nendmodule\n", "m", &["adder", "h"]),
        ("m.vhd", VHDL, "pkg.f", &["g"]),
        ("m.vhd", VHDL, "pkg.p", &["q"]),
        ("m.vhd", VHDL, "rtl.h", &["f"]),
        ("m.vhd", VHDL, "rtl", &["h", "p"]),
        ("m.sv", "module m;\n  task t;\n    h;\n  endtask\n  initial begin\n    int y;\n    h3;\n    h4(1);\n  end\nendmodule\n", "m.t", &["h"]),
        ("m.sv", "module m;\n  task t;\n    h;\n  endtask\n  initial begin\n    int y;\n    h3;\n    h4(1);\n  end\nendmodule\n", "m", &["h3", "h4"]),
        ("i.vhd", VHDL_INSTANCES, "rtl", &["adder", "adder2", "adder3", "h"]),
    ];
    let wrong = wrong_calls(cases);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

// an instance records its entity or component; `a(1)` of the signal `a` is no call
#[cfg(feature = "extra-langs")]
const VHDL_INSTANCES: &str = "architecture rtl of top is\n  signal a : std_logic_vector(7 downto 0);\n  signal s : integer;\n\
  function h(x : integer) return integer is\n  begin\n    return x;\n  end function;\nbegin\n\
  u1: entity work.adder port map (x => a(1), y => s);\n  u2: adder2 port map (x => A(2));\n\
  u3: component adder3 port map (x => s);\n  process\n  begin\n    s <= h(2) + a(3);\n  end process;\nend architecture;\n";

#[cfg(feature = "extra-langs")]
const VHDL: &str = "package pkg is\n  function f(x : integer) return integer;\nend package;\n\npackage body pkg is\n\
  function f(x : integer) return integer is\n  begin\n    return g(x) + 1;\n  end function;\n\
  procedure p(x : integer) is\n  begin\n    q(x);\n  end procedure;\nend package body;\n\n\
architecture rtl of top is\n  signal s : std_logic_vector(7 downto 0);\n\
  function h(x : integer) return integer is\n  begin\n    return f(x);\n  end function;\n\
begin\n  process\n  begin\n    p(1);\n    s <= h(2);\n  end process;\nend architecture;\n";

#[cfg(feature = "extra-langs")]
#[test]
fn vhdl_subprograms_with_a_body_are_functions_of_their_package_or_architecture() {
    let got = symbols("m.vhd", VHDL);
    let want = [("module", "pkg"), ("module", "pkg"), ("function", "pkg.f"), ("function", "pkg.p"), ("impl", "rtl"), ("function", "rtl.h")];
    assert_eq!(got, want.map(|(k, q)| (k.to_string(), q.to_string())));
}

#[test]
fn calls_are_recorded_in_make_jsonnet_and_assembly() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("Makefile", "define g\n\techo $(1)\nendef\n\nall:\n\t$(call g,1)\n\t@echo $(shell date) $(subst a,b,c)\n", "all", &["g"]),
        ("Makefile", "define outer\n\t$(call g,2) ${call  h-x ,3}\nendef\n", "outer", &["g", "h-x"]),
        ("m.jsonnet", "local f(x) = g(x) + obj.method(x);\nf(1)\n", "f", &["g", "method"]),
        ("m.jsonnet", "{\n  run(y): std.length(h(y)),\n  n: 1,\n}\n", "run", &["h", "length"]),
        (
            "m.s",
            "_start:\n    call helper\n    jmp done\n    bl other\n    jal third\n    b fourth\n    j fifth\nhelper:\n    ret\n",
            "_start",
            &["helper", "other", "third"],
        ),
        ("m.s", "f:\n    callq printf@PLT\n.L2:\n    call g\n    # doc of h\nh:\n    ret\n", "f", &["g", "printf"]),
        ("m.s", "f:\n    callq printf@PLT\n.L2:\n    call g\n    # doc of h\nh:\n    ret\n", ".L2", &["g"]),
    ];
    let wrong = wrong_calls(cases);
    assert!(wrong.is_empty(), "{wrong:#?}");
    let js = symbols("m.jsonnet", "local f(x) = x;\nlocal v = 1;\nlocal o = { m(y): y, n: 2 };\no\n");
    assert_eq!(js, [("function", "f"), ("key", "v"), ("key", "o"), ("method", "o.m")].map(|(k, q)| (k.to_string(), q.to_string())));
}

#[test]
fn an_assembly_label_holds_its_instructions_up_to_the_next_label_or_section() {
    let asm = parse_source("m.s", ".text\nf:\n    mov $1, %eax\n.L2:\n    call g\n    # doc of h\nh:\n    ret\n    .section .rodata\nmsg: .ascii \"x\"\n");
    let span = |q: &str| (find(&asm, q).start, find(&asm, q).end);
    assert_eq!([span("f"), span(".L2"), span("h")], [(2, 5), (4, 5), (7, 8)]);
    assert!(find(&asm, "f").text.contains("call g"), "{}", find(&asm, "f").text);
}

#[test]
fn a_definition_head_and_the_definition_macros_are_not_calls() {
    let ex = parse_source(
        "m.ex",
        "defmodule M do\n  use GenServer\n  import Enum\n  alias Foo.Bar\n  require Logger\n  def f(x), do: g(x)\n  defp p(x) when x > 0, do: f(x)\n  defguard is_big(x) when x > 100\nend\n",
    );
    assert!(find(&ex, "M").calls.is_empty(), "{:?}", find(&ex, "M").calls);
    assert_eq!(find(&ex, "M.f").calls.iter().collect::<Vec<_>>(), ["g"]);
    assert_eq!(find(&ex, "M.p").calls.iter().collect::<Vec<_>>(), ["f"]);
    assert!(find(&ex, "M.is_big").calls.is_empty());
    assert_eq!(calls_of("m.scm", "(define (f n)\n  (f (- n 1)))\n", "f"), ["f"]);
    assert_eq!(calls_of("m.lisp", "(defmethod m ((x point) y)\n  (h x))\n", "m"), ["h"]);
}

#[test]
fn a_call_records_a_lisp_name_whole_and_a_private_method_with_its_mark() {
    assert_eq!(calls_of("m.scm", "(define (use x)\n  (list->vec (my-string->list x)))\n", "use"), ["list->vec", "my-string->list"]);
    assert_eq!(calls_of("m.el", "(defun use (x)\n  (my-pkg--trim! x))\n", "use"), ["my-pkg--trim!"]);
    let js = parse_source("c.js", "class Cart {\n  #secret() {\n    return 1;\n  }\n  run() {\n    return this.#secret();\n  }\n}\n");
    assert_eq!(find(&js, "Cart.run").calls.iter().collect::<Vec<_>>(), ["#secret"]);
    // a C arrow is still a member access
    assert_eq!(calls_of("m.c", "void use(struct s *p) {\n  p->run(1);\n}\n", "use"), ["run"]);
}

#[test]
fn a_call_records_the_name_its_definition_has() {
    // file, source, definitions as named in the index, caller, its calls
    type Case = (&'static str, &'static str, &'static [&'static str], &'static str, &'static [&'static str]);
    let cases: &[Case] = &[
        (
            "m.rb",
            "class Rec\n  def valid?\n    true\n  end\n  def save!\n  end\n  def run\n    valid?\n    self.valid?\n    save!\n    o.valid?()\n  end\nend\n",
            &["valid?", "save!"],
            "Rec.run",
            &["save!", "valid?"],
        ),
        (
            "m.ex",
            "defmodule M do\n  def ok?(x), do: x\n  def run!(x), do: x\n  def go(x) do\n    ok?(x)\n    M.run!(x)\n  end\nend\n",
            &["ok?", "run!"],
            "M.go",
            &["ok?", "run!"],
        ),
        (
            "m.jl",
            "function update!(x)\n  x\nend\nmodule Tools\nfunction helper!(x)\n  x\nend\nend\nfunction go(x)\n  update!(x)\n  Tools.helper!(x)\nend\n",
            &["update!", "helper!"],
            "go",
            &["helper!", "update!"],
        ),
        ("m.r", "is.ready <- function(x) {\n  x\n}\ngo <- function(x) {\n  is.ready(x)\n  base::is.ready(x)\n}\n", &["is.ready"], "go", &["is.ready"]),
        ("n.r", "ärger.neu <- function(x) {\n  x\n}\ngo <- function(x) {\n  ärger.neu(x)\n}\n", &["ärger.neu"], "go", &["ärger.neu"]),
        (
            "m.sh",
            "ns::helper() {\n  echo hi\n}\nlib.load() {\n  :\n}\ngo() {\n  ns::helper\n  lib.load\n}\n",
            &["ns::helper", "lib.load"],
            "go",
            &["lib.load", "ns::helper"],
        ),
        ("m.rs", "const r#A: i32 = 1;\nfn r#type() -> i32 { 1 }\nfn go() -> i32 { r#type() + S::r#match() }\n", &["A", "type"], "go", &["match", "type"]),
        (
            "m.php",
            "<?php\nfunction helper() { return 1; }\nfunction go() {\n  Util\\helper();\n  \\App\\Util\\helper();\n  new \\App\\Box();\n}\n",
            &["helper"],
            "go",
            &["Box", "helper"],
        ),
        ("m.hs", "step x = x\nstep' x = x\ngo x = step' (M.step'' x)\n", &["step", "step'"], "go", &["step'", "step''"]),
    ];
    let mut wrong = Vec::new();
    for &(path, src, defs, caller, want) in cases {
        let chunks = parse_source(path, src);
        let names: Vec<&str> = chunks.iter().map(|c| c.name.as_str()).collect();
        if let Some(d) = defs.iter().find(|d| !names.contains(d)) {
            wrong.push(format!("{path}: no definition named {d} in {names:?}"));
        }
        let got = calls_of(path, src, caller);
        if got != want {
            wrong.push(format!("{path}: got {got:?}, want {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_header_is_read_in_the_language_it_is_written_in() {
    let quals = |src: &str| parse_source("shape.h", src).iter().map(|c| format!("{} {}", c.kind, c.qualname)).collect::<Vec<_>>();
    let cpp = quals("// a licence line\n#pragma once\nnamespace geo {\nclass Shape {\npublic:\n  void draw();\n};\nShape make();\n}\n");
    for want in ["class geo.Shape", "method geo.Shape.draw", "function geo.make"] {
        assert!(cpp.iter().any(|q| q == want), "{want} not in {cpp:?}");
    }
    let templ = quals("#ifndef T_H\n#define T_H\ntemplate <typename T>\nT tmax(T a, T b);\n#endif\n");
    assert!(templ.iter().any(|q| q == "function tmax"), "{templ:?}");
    // a prototype inside an include guard is at file scope; one inside a function body is not
    let guarded =
        quals("#ifndef A_H\n#define A_H\nint add(int a, int b);\nstatic int use(void) {\n#ifdef X\n  int inner(int);\n#endif\n  return 0;\n}\n#endif\n");
    assert!(guarded.iter().any(|q| q == "function add") && !guarded.iter().any(|q| q.ends_with("inner")), "{guarded:?}");
    let objc = quals("#import <Foundation/Foundation.h>\n@interface Greeter : NSObject\n- (void)greet;\n@end\n");
    assert!(objc.iter().any(|q| q.ends_with("Greeter")), "{objc:?}");
    // plain C stays C: `class` and `namespace` are ordinary names there
    assert_eq!(scrutate::parse::spec_for("a.h", "struct s { int class; };\nint add(int a, int b);\n/* namespace of the driver */\n").map(|s| s.id), Some("c"));
    assert_eq!(scrutate::parse::spec_for("a.h", "namespace geo { int f(); }\n").map(|s| s.id), Some("cpp"));
}

#[test]
fn an_objc_multi_part_selector_call_is_a_call_of_its_definition() {
    let chunks = parse_source("m.mm", "@implementation Foo\n- (void)a:(int)x b:(int)y {\n}\n- (void)run {\n  [obj a:1 b:2];\n}\n@end\n");
    assert_eq!(find(&chunks, "Foo.a").name, "a");
    assert_eq!(find(&chunks, "Foo.run").calls.iter().collect::<Vec<_>>(), ["a"]);
}

#[test]
fn the_clauses_of_a_haskell_function_are_one_definition() {
    let hs = parse_source("M.hs", "module M where\n\ndouble :: Int -> Int\ndouble 0 = 0\ndouble x = helper x\n\nquad :: Int -> Int\nquad x = double x\n");
    let fns: Vec<(&str, i64, i64)> = hs.iter().filter(|c| c.kind == "function").map(|c| (c.name.as_str(), c.start, c.end)).collect();
    assert_eq!(fns, [("double", 3, 5), ("quad", 7, 8)]);
    assert_eq!(find(&hs, "double").calls.iter().collect::<Vec<_>>(), ["helper"]);
    // guards are one clause; a `where` helper stays a definition of its own inside the clause it
    // belongs to, and the next clause still joins the function
    let hs = parse_source(
        "G.hs",
        "module G where\n\nsign :: Int -> Int\nsign x\n  | x > 0 = 1\n  | otherwise = 0\n\nscale 0 = 0\n  where k = 2\nscale n = n * k2\n  where k2 = 3\n",
    );
    let fns: Vec<(&str, i64, i64)> =
        hs.iter().filter(|c| c.kind == "function" && matches!(c.name.as_str(), "sign" | "scale")).map(|c| (c.name.as_str(), c.start, c.end)).collect();
    assert_eq!(fns, [("sign", 3, 6), ("scale", 8, 11)]);
    // the same name in two instances, or in another module, is another definition
    let hs = parse_source("S.hs", "module S where\n\ninstance Show A where\n  show _ = \"a\"\n\ninstance Show B where\n  show _ = \"b\"\n");
    let shows: Vec<(i64, i64)> = hs.iter().filter(|c| c.name == "show").map(|c| (c.start, c.end)).collect();
    assert_eq!(shows, [(4, 4), (7, 7)]);
    let other = parse_source("N.hs", "module N where\n\ndouble :: Int -> Int\ndouble x = x\n");
    assert_eq!(other.iter().filter(|c| c.name == "double").map(|c| (c.start, c.end)).collect::<Vec<_>>(), [(3, 4)]);
}

#[test]
fn dart_and_pascal_definitions_hold_their_bodies() {
    let dart = parse_source("m.dart", "void f() {\n  g(1);\n}\n");
    assert_eq!((find(&dart, "f").start, find(&dart, "f").end), (1, 3));
    let pas = parse_source("m.pas", "program P;\nprocedure F;\nbegin\n  G(1);\nend;\nbegin end.\n");
    assert_eq!((find(&pas, "F").start, find(&pas, "F").end), (2, 5));
}

const OBJECT_LITERALS: &str = "const api = { get() { return 1; }, post: function () { return 2; }, put: () => 3, gen: function* () {}, \"q\": () => 5 };
export const store = { count: 0, increment() { return this.total(); }, total() { return this.count; }, sub: { m() {} }, later: async () => go() };
export default { methods: { save() { return 1; }, run() { return this.save(); } } };
module.exports.tool = { build() { return 4; } };
exports.kit = { fix() {} };
f({ cb() {} });
";

#[test]
fn methods_of_an_object_literal_are_owned_by_what_holds_the_literal() {
    for path in ["o.js", "o.ts"] {
        let chunks = parse_source(path, OBJECT_LITERALS);
        let methods: Vec<(&str, &str)> = chunks.iter().filter(|c| c.kind == "method").map(|c| (c.name.as_str(), c.qualname.as_str())).collect();
        assert_eq!(
            methods,
            [
                ("get", "api.get"),
                ("post", "api.post"),
                ("put", "api.put"),
                ("gen", "api.gen"),
                ("q", "api.q"),
                ("increment", "store.increment"),
                ("total", "store.total"),
                ("m", "store.sub.m"),
                ("later", "store.later"),
                ("save", "methods.save"),
                ("run", "methods.run"),
                ("build", "tool.build"),
                ("fix", "kit.fix"),
                ("cb", "cb"),
            ],
            "{path}"
        );
        assert_eq!(find(&chunks, "store.later").calls.iter().collect::<Vec<_>>(), ["go"], "{path}");
        assert_eq!(find(&chunks, "store.increment").calls.iter().collect::<Vec<_>>(), ["total"], "{path}");
    }
}

#[test]
fn c_and_cpp_function_prototypes_are_function_symbols() {
    let c = symbols(
        "p.h",
        "int add(int a, int b);\nextern int x;\nint y;\nint (*fp)(int);\ntypedef int (*cb)(int);\nstatic char *name(void);\nvoid g(void) { foo(1); int h(int); }\n",
    );
    let want = |v: &[(&str, &str)]| v.iter().map(|&(k, q)| (k.to_string(), q.to_string())).collect::<Vec<_>>();
    assert_eq!(c, want(&[("function", "add"), ("type", "cb"), ("function", "name"), ("function", "g")]));
    let cpp = parse_source(
        "p.hpp",
        "namespace ns {\n  Shape make();\n  int x;\n}\nextern \"C\" {\n  int cfun(int);\n}\nextern \"C\" int cone(int);\nWidget w();\nusing F = int(int);\nclass K { void m(); };\nint& rf();\nint (*fp)(int);\ntemplate <typename T> T tmax(T a, T b);\nvoid g() { foo(1); int h(int); }\n",
    );
    let got: Vec<(&str, &str)> = cpp.iter().filter(|c| c.kind != "window").map(|c| (c.kind, c.qualname.as_str())).collect();
    assert_eq!(
        got,
        [
            ("module", "ns"),
            ("function", "ns.make"),
            ("function", "cfun"),
            ("function", "cone"),
            ("function", "w"),
            ("type", "F"),
            ("class", "K"),
            ("method", "K.m"),
            ("function", "rf"),
            ("function", "tmax"),
            ("function", "g"),
        ]
    );
    assert_eq!(find(&cpp, "ns.make").text, "Shape make();");
    assert_eq!(find(&cpp, "tmax").text, "template <typename T> T tmax(T a, T b);");
    assert!(find(&cpp, "ns.make").calls.is_empty());
    assert_eq!(find(&cpp, "g").calls.iter().collect::<Vec<_>>(), ["foo"]);
}

const TS_WRAPPED_LITERALS: &str = "export const api = { get() {} } as const;
const store = { inc() {}, sub: ({ deep() {} }) as Sub } satisfies Store;
const box = ({ open() {} });
let tool = ({ run() {} } as const) satisfies Tool;
";

#[test]
fn a_typescript_literal_wrapped_in_as_satisfies_or_parentheses_keeps_its_owner() {
    for path in ["w.ts", "w.tsx"] {
        let chunks = parse_source(path, TS_WRAPPED_LITERALS);
        let methods: Vec<&str> = chunks.iter().filter(|c| c.kind == "method").map(|c| c.qualname.as_str()).collect();
        assert_eq!(methods, ["api.get", "store.inc", "store.sub.deep", "box.open", "tool.run"], "{path}");
    }
}

#[test]
fn c_and_cpp_const_globals_are_constants() {
    let c = constants(
        "k.c",
        "const int MAX_LEN = 80;\nstatic const char *NAME = \"x\";\nextern const int EXT;\nconst int A = 1, B = 2;\nint counter = 0;\n\
         int f(const int p) { const int local = 1; return local; }\nstruct s { const int m; };\n",
    );
    assert_eq!(named(&c), ["MAX_LEN", "NAME", "A", "B"]);
    assert_eq!((c[0].2.as_str(), c[1].2.as_str(), c[3].2.as_str()), ("const int MAX_LEN = 80;", "static const char *NAME = \"x\";", "const int A = 1, B = 2;"));
    let cpp = constants(
        "k.cpp",
        "constexpr int kLimit = 8;\nnamespace cfg { inline constexpr double kPi = 3.14; }\nextern \"C\" { const int kC = 1; }\n\
         class K { public: static const int kSize = 2; };\nvoid g() { constexpr int in = 1; }\n",
    );
    assert_eq!(named(&cpp), ["kLimit", "cfg.kPi", "kC", "K.kSize"]);
}

#[test]
fn python_class_constants_and_tuple_targets_are_constants() {
    let py =
        constants("k.py", "A, B = 1, 2\n(C, d) = 3, 4\nclass Config:\n    MAX = 3\n    name = 'x'\n    X, Y = 1, 2\n    def f(self):\n        INNER = 1\n");
    assert_eq!(named(&py), ["A", "B", "C", "Config.MAX", "Config.X", "Config.Y"]);
    let texts: Vec<&str> = py.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(texts, ["A, B = 1, 2", "A, B = 1, 2", "(C, d) = 3, 4", "MAX = 3", "X, Y = 1, 2", "X, Y = 1, 2"]);
}

#[test]
fn python_list_and_parenthesised_targets_are_constants() {
    let py = constants("k.py", "[A, B] = 1, 2\n(C, D) = 3, 4\nclass K:\n    [X, y] = 5, 6\n");
    assert_eq!(named(&py), ["A", "B", "C", "D", "K.X"]);
    assert_eq!(py[0].2, "[A, B] = 1, 2");
}

#[test]
fn cpp_class_body_constants_are_constants_of_their_class() {
    let src = "struct Config {\n  static const int kSize = 2;\n  static constexpr double kPi = 3.14;\n  inline static const std::string kName = \"x\";\n\
               static const int kLater;\n  const int notStatic = 1;\n  static int counter = 0;\n  static constexpr int kA = 1, kB = 2;\n\
               static constexpr const char *kPath = \"/\";\n  void f();\n};\nconst int Config::kLater = 5;\n\
               class K {\npublic:\n  static const int kMax = 9;\n};\n";
    let cpp = constants("c.cpp", src);
    assert_eq!(named(&cpp), ["Config.kSize", "Config.kPi", "Config.kName", "Config.kA", "Config.kB", "Config.kPath", "Config.kLater", "K.kMax"]);
    assert_eq!(cpp[0], ("kSize".into(), "Config.kSize".into(), "static const int kSize = 2;".into()));
    assert_eq!(cpp[6], ("kLater".into(), "Config.kLater".into(), "const int Config::kLater = 5;".into()));
}

#[test]
fn glsl_and_objc_file_scope_const_globals_are_constants() {
    assert_eq!(named(&constants("s.glsl", "const float PI = 3.14159;\nuniform float time;\nvoid main() {\n  const int local = 1;\n}\n")), ["PI"]);
    let objc = "#import <Foundation/Foundation.h>\nstatic const int kMax = 3;\nNSString *const kName = @\"x\";\nNSString *mutableName = @\"y\";\n\
                @implementation G\n- (void)f {\n  const int local = 1;\n}\n@end\n";
    for path in ["g.m", "g.mm"] {
        assert_eq!(named(&constants(path, objc)), ["kMax", "kName"], "{path}");
    }
}

#[test]
fn an_objc_property_is_a_method_of_its_class_or_protocol() {
    let src = "@interface Greeter : NSObject\n@property (nonatomic) int count;\n@property int bare;\n@end\n\
               @protocol Speaker\n@property (readonly) int volume;\n@end\n";
    let got = symbols("g.h", src);
    let want = [("class", "Greeter"), ("method", "Greeter.count"), ("method", "Greeter.bare"), ("interface", "Speaker"), ("method", "Speaker.volume")];
    assert_eq!(got, want.map(|(k, q)| (k.to_string(), q.to_string())));
    assert_eq!(find(&parse_source("g.h", src), "Greeter.count").text, "@property (nonatomic) int count;");
    let root = tree("objc_prop", &[("g.h", src), ("g.m", "#import \"g.h\"\n@implementation Greeter\n- (int)count {\n  return _count;\n}\n@end\n")]);
    let mut repo = scrutate::Repo::open(&root).unwrap();
    repo.update(false).unwrap();
    let hits = repo.find_symbol("Greeter.count", 5).unwrap();
    assert_eq!(hits.iter().map(|h| h.loc()).collect::<Vec<_>>(), ["g.m:3-5", "g.h:2-2"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn static_readonly_class_members_are_constants() {
    let src = "export default class K {\n  static readonly SIZE = 2;\n  static n = 1;\n  readonly r = 3;\n  private static readonly p: number = 4;\n  static readonly f = () => 1;\n}\n";
    for path in ["k.ts", "k.tsx"] {
        let ts = constants(path, src);
        assert_eq!(named(&ts), ["K.SIZE", "K.p"], "{path}");
        assert_eq!(ts[0].2, "static readonly SIZE = 2", "{path}");
    }
    let js = constants("k.js", "class K {\n  static MAX = 3;\n  static n = 1;\n  static #P = 2;\n  LIMIT = 4;\n}\n");
    assert_eq!(named(&js), ["K.MAX"]);
}

const OBJC_HEADER: &str = "#import <Foundation/Foundation.h>\n@interface Greeter : NSObject\n@property (nonatomic) int count;\n- (void)greet;\n\
                           + (instancetype)make:(int)n with:(int)m;\n@end\n@protocol Speaker\n- (void)speak:(int)x;\n@end\n";

#[test]
fn objc_method_declarations_are_methods_of_their_class_or_protocol() {
    for path in ["g.h", "g.m"] {
        let got = symbols(path, OBJC_HEADER);
        let want = [
            ("class", "Greeter"),
            ("method", "Greeter.count"),
            ("method", "Greeter.greet"),
            ("method", "Greeter.make"),
            ("interface", "Speaker"),
            ("method", "Speaker.speak"),
        ];
        assert_eq!(got, want.map(|(k, q)| (k.to_string(), q.to_string())), "{path}");
        assert_eq!(find(&parse_source(path, OBJC_HEADER), "Greeter.make").text, "+ (instancetype)make:(int)n with:(int)m;", "{path}");
    }
    let root =
        tree("objc_decl", &[("g.h", OBJC_HEADER), ("g.m", "#import \"g.h\"\n@implementation Greeter\n- (void)greet {\n  NSLog(@\"hello there\");\n}\n@end\n")]);
    let mut repo = scrutate::Repo::open(&root).unwrap();
    repo.update(false).unwrap();
    let hits = repo.find_symbol("Greeter.greet", 5).unwrap();
    assert_eq!(hits.iter().map(|h| h.loc()).collect::<Vec<_>>(), ["g.m:3-5", "g.h:4-4"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_function_pointer_typedef_is_named_by_its_name() {
    let c = symbols("t.c", "typedef int (*cb)(int);\ntypedef void (*handler_t)(int, void *);\nstruct ops { int (*fp)(int); };\n");
    assert_eq!(c, [("type", "cb"), ("type", "handler_t"), ("struct", "ops")].map(|(k, q)| (k.to_string(), q.to_string())));
    let cpp = symbols("t.cpp", "using cb = int (*)(int);\ntypedef int (*cb2)(int);\nstruct S { int (*fp)(int); };\n");
    assert_eq!(cpp, [("type", "cb"), ("type", "cb2"), ("struct", "S")].map(|(k, q)| (k.to_string(), q.to_string())));
}

fn wrong_imports(cases: &[(&str, &str, &[&str])]) -> Vec<String> {
    cases
        .iter()
        .map(|&(path, src, want)| (path, scrutate::parse::parse_file(path, src).imports.into_iter().map(|(_, m)| m).collect::<Vec<_>>(), want))
        .filter(|(_, got, want)| got != want)
        .map(|(p, got, want)| format!("{p}: got {got:?}, want {want:?}"))
        .collect()
}

#[test]
fn every_language_records_its_imports_as_written() {
    let cases: &[(&str, &str, &[&str])] = &[
        ("m.py", "import os.path\nfrom a.b import c as d\nfrom . import z\n", &["os.path", "a.b", "."]),
        (
            "m.js",
            "import x from \"./a\";\nimport \"./side\";\nimport { y as z } from \"./b\";\nconst c = require(\"./c\");\nconst d = await import(\"./d\");\nconst e = require(name);\nconst f = require(`./f${n}`);\nconst g = require(`./g`);\n",
            &["./a", "./side", "./b", "./c", "./d", "./g"],
        ),
        (
            "m.js",
            "export const url = \"https://example.com/x\";\nexport function greet() {\n  return \"hello world\";\n}\nexport default { mode: \"fast\" };\nexport function f() { const s = \"./h.js\"; }\nexport { a } from \"./x\";\nexport * from \"./y\";\nexport * as ns from \"./z\";\n",
            &["./x", "./y", "./z"],
        ),
        (
            "m.ts",
            "import type { T } from \"./t\";\nimport x = require(\"./r\");\nexport type { U } from \"./u\";\nexport function render(title: string): string {\n  return title;\n}\nexport const label: string = \"ok\";\nexport type Id = string;\n",
            &["./t", "./r", "./u"],
        ),
        ("m.tsx", "import React from \"react\";\nexport { B } from \"./b\";\nexport const C = () => <div className=\"c\" />;\n", &["react", "./b"]),
        ("m.go", "package main\nimport (\n  \"fmt\"\n  m \"github.com/x/y\"\n)\n", &["fmt", "github.com/x/y"]),
        ("m.rs", "use std::fmt;\nuse crate::lex::{a, b};\nextern crate serde;\n", &["std::fmt", "crate::lex"]),
        ("A.java", "import x.y.Z;\nimport static x.y.W.m;\nimport x.q.*;\nclass A { }\n", &["x.y.Z", "x.y.W.m", "x.q.*"]),
        ("m.c", "#include <stdio.h>\n#include \"x.h\"\nint a;\n", &["stdio.h", "x.h"]),
        ("m.cpp", "#include <vector>\n#include \"y.hpp\"\nint a;\n", &["vector", "y.hpp"]),
        ("m.glsl", "#include \"common.glsl\"\nvoid main() {}\n", &["common.glsl"]),
        (
            "m.m",
            "#import <Foundation/Foundation.h>\n#include \"x.h\"\n@import UIKit;\n@import Foo.Bar;\n",
            &["Foundation/Foundation.h", "x.h", "UIKit", "Foo.Bar"],
        ),
        (
            "m.cs",
            "using System;\nusing System.Collections.Generic;\nusing IO = System.IO;\nusing static System.Math;\n",
            &["System", "System.Collections.Generic", "System.IO", "System.Math"],
        ),
        (
            "m.rb",
            "require \"json\"\nrequire_relative \"lib/util\"\nload \"lib/util.rb\"\nYAML.load(\"--- a: 1\")\nRails.cache.load(\"user_key\")\nSettings.load(\"lib/util\")\nrequire \"x#{y}\"\n",
            &["json", "lib/util", "lib/util.rb"],
        ),
        (
            "m.php",
            "<?php\nrequire \"lib.php\";\nrequire_once 'inc/db.php';\ninclude \"inc/view.php\";\ninclude_once 'inc/x.php';\nuse Foo\\Bar;\nuse function Foo\\bar;\nuse Foo\\{A, B};\nuse Foo\\Baz as Q;\n",
            &["lib.php", "inc/db.php", "inc/view.php", "inc/x.php", "Foo\\Bar", "Foo\\bar", "Foo\\A", "Foo\\B", "Foo\\Baz"],
        ),
        ("m.sh", "source ./lib.sh\n. \"./env.sh\"\necho hi\n", &["./lib.sh", "./env.sh"]),
        (
            "m.kt",
            "package p\nimport kotlin.math.max\nimport com.acme.Util as U\nimport com.acme.io.*\n",
            &["kotlin.math.max", "com.acme.Util", "com.acme.io.*"],
        ),
        ("m.swift", "import Foundation\nimport UIKit\n", &["Foundation", "UIKit"]),
        ("m.scala", "import scala.collection.mutable\nimport a.b.{C, D}\n", &["scala.collection.mutable", "a.b"]),
        ("m.groovy", "import groovy.json.JsonSlurper\nimport static java.lang.Math.max\n", &["groovy.json.JsonSlurper", "java.lang.Math.max"]),
        (
            "m.lua",
            "local a = require \"mod.a\"\nlocal b = require(\"mod.b\")\nlocal c = require[[mod.c]]\nlocal d = require [=[mod.d]=]\nlocal e = require(name)\n",
            &["mod.a", "mod.b", "mod.c", "mod.d"],
        ),
        ("m.ex", "defmodule M do\n  alias Foo.Bar\n  import Baz\n  use GenServer\nend\n", &["Foo.Bar", "Baz", "GenServer"]),
        ("m.hs", "import qualified Data.Map as M\nimport Data.List (sort)\n", &["Data.Map", "Data.List"]),
        ("m.elm", "module M exposing (..)\nimport Html exposing (div)\nimport Json.Decode as D\n", &["Html", "Json.Decode"]),
        ("m.zig", "const std = @import(\"std\");\nconst u = @import(\"util.zig\");\n", &["std", "util.zig"]),
        (
            "m.jl",
            "using LinearAlgebra, Statistics\nimport Base: show, print\nimport JSON\ninclude(\"util.jl\")\nusing A.B\nimport C.D as E\nusing ..Parent: f\nBase.include(\"no.jl\")\ninclude(\"$x.jl\")\n",
            &["LinearAlgebra", "Statistics", "Base", "JSON", "util.jl", "A.B", "C.D", "..Parent"],
        ),
        (
            "m.dart",
            "import 'package:flutter/material.dart';\nimport 'util.dart' as u;\nexport 'src/a.dart';\n",
            &["package:flutter/material.dart", "util.dart", "src/a.dart"],
        ),
        (
            "m.R",
            "library(dplyr)\nlibrary(\"ggplot2\")\nrequire(stats)\nrequireNamespace(\"jsonlite\")\nsource(\"util.R\")\n",
            &["dplyr", "ggplot2", "stats", "jsonlite", "util.R"],
        ),
        ("m.nix", "let a = import ./a.nix; b = import <nixpkgs> {}; in a\n", &["./a.nix", "nixpkgs"]),
        ("m.gleam", "import gleam/io\nimport gleam/list.{map}\n", &["gleam/io", "gleam/list"]),
    ];
    let wrong = wrong_imports(cases);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[cfg(feature = "extra-langs")]
#[test]
fn imports_are_recorded_in_the_optional_grammars() {
    let cases: &[(&str, &str, &[&str])] = &[
        ("m.cu", "#include <cuda_runtime.h>\n#include \"k.cuh\"\n__global__ void k() {}\n", &["cuda_runtime.h", "k.cuh"]),
        ("m.qml", "import QtQuick 2.15\nimport \"./js/util.js\" as U\nItem {}\n", &["QtQuick", "./js/util.js"]),
    ];
    let wrong = wrong_imports(cases);
    assert!(wrong.is_empty(), "{wrong:#?}");
}
