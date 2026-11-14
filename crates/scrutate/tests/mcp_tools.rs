//! What the `serve` tools answer, called the way the server calls them: arguments as JSON, the
//! text an agent reads, and whether the call failed.

use std::fs;
use std::path::PathBuf;

use scrutate::session::{Every, Session};
use serde_json::{json, Value};

#[path = "support/conforms.rs"]
mod schema;

struct Repo {
    root: PathBuf,
    repo: scrutate::Repo,
    /// what `serve --refresh-interval 0` holds: a scan before every call
    session: Session,
}

/// What `serve --refresh-interval SECONDS` holds open on `root`.
fn session(root: &std::path::Path, seconds: f64) -> Session {
    Session::open(root, Every::given(Some(seconds))).unwrap()
}

/// A tool call as the server makes it, with the size limit it was started with.
fn call_tool(s: &Session, tool: &str, args: &Value) -> scrutate::mcp::Called {
    scrutate::mcp::call_tool(s, tool, args, &scrutate::mcp::Settings::new(None), &Default::default())
}

impl Repo {
    fn new(name: &str, files: &[(&str, &str)]) -> Repo {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scrutate_mcp_tools_{}_{}_{n}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for (rel, body) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
        }
        let root = fs::canonicalize(&root).unwrap();
        let repo = scrutate::Repo::open(&root).unwrap();
        Repo { session: session(&root, 0.0), root, repo }
    }

    /// (failed, text)
    fn call(&mut self, tool: &str, args: Value) -> (bool, String) {
        let c = call_tool(&self.session, tool, &args);
        (c.is_error, c.text)
    }

    fn ok(&mut self, tool: &str, args: Value) -> String {
        let (failed, text) = self.call(tool, args.clone());
        assert!(!failed, "{tool} {args}: {text}");
        text
    }

    fn err(&mut self, tool: &str, args: Value) -> String {
        let (failed, text) = self.call(tool, args.clone());
        assert!(failed, "{tool} {args} should fail: {text}");
        text
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The location lines of an answer (each result starts with `path:start-end`).
fn heads(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.starts_with(' ') && !l.starts_with('(')).collect()
}

fn two_dup_names() -> Repo {
    Repo::new(
        "dups",
        &[
            ("app.py", "import helper\n\ndef visible_fn():\n    return helper.dup_name()\n"),
            ("helper.py", "def dup_name():\n    return 'A_MARK'\n\ndef call_a():\n    return dup_name()\n"),
            ("other.py", "def dup_name():\n    return 'B_MARK'\n\ndef call_b():\n    return dup_name()\n"),
        ],
    )
}

#[test]
fn a_location_selects_that_one_definition() {
    let mut r = two_dup_names();
    let found = r.ok("find_symbol", json!({"name": "helper.py:1-2"}));
    assert_eq!(heads(&found), ["helper.py:1-2  [function] dup_name"], "{found}");
    assert_eq!(heads(&r.ok("find_symbol", json!({"name": "dup_name"}))).len(), 2);

    let callers = r.ok("callers", json!({"name": "helper.py:1-2"}));
    assert!(callers.contains("call_a") && callers.contains("visible_fn") && !callers.contains("call_b"), "{callers}");
    let callers = r.ok("callers", json!({"name": "./other.py:1-2"}));
    assert!(callers.contains("call_b") && !callers.contains("call_a") && !callers.contains("visible_fn"), "{callers}");
    let all = r.ok("callers", json!({"name": "dup_name"}));
    assert_eq!(heads(&all).len(), 3, "{all}");

    let callees = r.ok("callees", json!({"name": "helper.py:4-5"}));
    assert!(callees.contains("helper.py:1-2") && !callees.contains("other.py"), "{callees}");
    let callees = r.ok("callees", json!({"name": "other.py:4-5"}));
    assert!(callees.contains("other.py:1-2") && !callees.contains("helper.py"), "{callees}");
    assert!(r.err("callers", json!({"name": "helper.py:99"})).contains("no symbol at helper.py:99"));
}

#[test]
fn calls_do_not_cross_languages_or_unknown_qualifiers() {
    let mut r = Repo::new(
        "langs",
        &[
            ("check.py", "class CheckReport:\n    def ok(self):\n        return True\n\ndef py_caller(r):\n    return r.ok()\n"),
            (
                "src/lib.rs",
                "pub struct Index;\n\nimpl Index {\n    pub fn refresh(&self) {}\n}\n\npub fn rs_caller(ix: &Index) -> Option<u8> {\n    ix.refresh();\n    \"1\".parse().ok()\n}\n",
            ),
        ],
    );
    let callers = r.ok("callers", json!({"name": "ok"}));
    assert!(callers.contains("py_caller") && !callers.contains("rs_caller"), "{callers}");
    let callees = r.ok("callees", json!({"name": "rs_caller"}));
    assert!(callees.contains("refresh") && !callees.contains("CheckReport"), "{callees}");
    assert!(r.ok("callers", json!({"name": "Index.refresh"})).contains("rs_caller"));
    assert_eq!(r.ok("callers", json!({"name": "Nope.refresh"})), "(no results)");
    assert_eq!(r.ok("callees", json!({"name": "Nope.rs_caller"})), "(no results)");
}

#[test]
fn the_three_navigation_tools_match_names_alike() {
    let mut r = two_dup_names();
    let dotted_finds_nothing = r.ok("find_symbol", json!({"name": ".dup_name"})) == "(no results)";
    for (tool, name) in [("find_symbol", "dup_name"), ("callers", "dup_name"), ("callees", "call_a")] {
        let plain = r.ok(tool, json!({"name": name}));
        assert_ne!(plain, "(no results)", "{tool}");
        assert_eq!(r.ok(tool, json!({"name": name.to_uppercase()})), plain, "{tool}: case");
        assert_eq!(r.ok(tool, json!({"name": format!(".{name}")})) == "(no results)", dotted_finds_nothing, "{tool}: a leading dot");
    }
}

fn many_lines(n: usize) -> String {
    (1..=n).map(|i| format!("x_{i} = {i}\n")).collect()
}

#[test]
fn read_code_answers_the_same_for_an_outside_path_whether_or_not_it_exists() {
    let mut r = Repo::new("outside", &[("a.py", "def f():\n    return 1\n")]);
    let there = r.root.parent().unwrap().join(format!("{}_there.txt", r.root.file_name().unwrap().to_string_lossy()));
    fs::write(&there, "secret\n").unwrap();
    let name = there.file_name().unwrap().to_string_lossy().into_owned();
    // by its own parts the path leaves the repository: no file is looked at
    let a = r.err("read_code", json!({"path": format!("../{name}")}));
    let b = r.err("read_code", json!({"path": format!("../{name}.missing")}));
    assert_eq!(a, b);
    assert!(a.contains("outside"), "{a}");
    let a = r.err("read_code", json!({"path": there.to_string_lossy()}));
    let b = r.err("read_code", json!({"path": format!("{}.missing", there.to_string_lossy())}));
    assert_eq!(a, b);
    // a link that leaves the repository reads as a file that is not there
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&there, r.root.join("link.txt")).unwrap();
        let a = r.err("read_code", json!({"path": "link.txt"}));
        let b = r.err("read_code", json!({"path": "gone.txt"}));
        assert_eq!(a.replace("link.txt", "X"), b.replace("gone.txt", "X"));
        assert!(!a.contains("secret"), "{a}");
    }
    // a path that leaves and comes back, and the absolute path of a file inside, still read
    let inside = format!("../{}/a.py", r.root.file_name().unwrap().to_string_lossy());
    assert!(r.ok("read_code", json!({"path": inside})).contains("def f()"));
    assert!(r.ok("read_code", json!({"path": r.root.join("a.py").to_string_lossy()})).contains("def f()"));
    let _ = fs::remove_file(&there);
}

#[test]
fn a_path_that_leaves_the_root_by_its_parts_is_refused_without_a_look() {
    let mut r = Repo::new("lexical", &[("a.py", "def f():\n    return 1\n")]);
    // the same file through a spelling that is not under the root as the index knows it
    #[cfg(unix)]
    {
        let alias = r.root.with_file_name(format!("{}_alias", r.root.file_name().unwrap().to_string_lossy()));
        let _ = fs::remove_file(&alias);
        std::os::unix::fs::symlink(&r.root, &alias).unwrap();
        let path = alias.join("a.py");
        assert!(path.is_file());
        let text = r.err("read_code", json!({"path": path.to_string_lossy()}));
        assert!(text.contains("outside"), "{text}");
        // a location or an outline of it names no file of the repository either
        let (_, text) = r.call("outline", json!({"path": path.to_string_lossy()}));
        assert!(text.contains("no symbols"), "{text}");
        let _ = fs::remove_file(&alias);
    }
    // on Windows the root is the verbatim spelling: the plain one names the same file
    #[cfg(windows)]
    {
        let root = r.root.to_string_lossy().into_owned();
        let plain = root.strip_prefix(r"\\?\").unwrap_or(&root);
        assert!(r.ok("read_code", json!({"path": format!(r"{plain}\a.py")})).contains("def f()"));
        assert!(r.ok("read_code", json!({"path": format!("{}/a.py", plain.replace('\\', "/"))})).contains("def f()"));
        assert!(r.ok("outline", json!({"path": format!(r"{plain}\a.py")})).contains(" f"));
    }
    // an absolute path elsewhere is not the repository's file of that name
    let (_, text) = r.call("outline", json!({"path": "/a.py"}));
    assert!(text.contains("no symbols"), "{text}");
    let (_, text) = r.call("find_symbol", json!({"name": "/a.py:1"}));
    assert!(!text.contains("a.py:1-2"), "{text}");
    assert!(r.err("read_code", json!({"path": "/a.py"})).contains("outside"));
    assert!(r.ok("read_code", json!({"path": "./a.py"})).contains("def f()"));
    let text = r.ok("outline", json!({"path": "./a.py"}));
    assert!(text.contains("function") && text.trim_end().ends_with(" f"), "{text}");
}

#[test]
fn callees_says_how_many_outside_names_it_left_out() {
    let calls: String = (10..70).map(|i| format!("    ext_{i}()\n")).collect();
    let mut r = Repo::new("externals", &[("a.py", &format!("def big():\n{calls}"))]);
    let text = r.ok("callees", json!({"name": "big"}));
    assert!(text.contains("ext_59") && !text.contains("ext_60,") && !text.contains("ext_69"), "{text}");
    assert!(text.contains("(and 10 more)"), "{text}");
    let few = Repo::new("externals_few", &[("a.py", "def small():\n    ext_1()\n    ext_2()\n")]).ok("callees", json!({"name": "small"}));
    assert!(few.contains("ext_2") && !few.contains("more)"), "{few}");
}

#[test]
fn read_code_of_a_symbol_reads_as_read_code_of_its_path_does() {
    // the index takes a file as text by its first 4 KB, read_code by its first 8 KB
    let body = format!("def f():\n    return 1\n\n# {}\nx = \"\0\"\n", "pad ".repeat(1250));
    let mut r = Repo::new("late_nul", &[("a.py", body.as_str()), ("b.py", "def g():\n    return 2\n")]);
    assert_eq!(heads(&r.ok("find_symbol", json!({"name": "f"}))), ["a.py:1-2  [function] f"]);
    let by_path = r.err("read_code", json!({"path": "a.py"}));
    assert!(by_path.contains("not a text file"), "{by_path}");
    assert_eq!(r.err("read_code", json!({"symbol": "f"})), by_path);
    assert!(r.ok("read_code", json!({"symbol": "g"})).contains("return 2"));
}

#[test]
fn read_code_says_where_it_stopped() {
    let mut r = Repo::new("read", &[("many.py", &many_lines(9000)), ("empty.py", ""), ("three.py", "a = 1\nb = 2\nc = 3\n")]);
    let text = r.ok("read_code", json!({"path": "many.py", "max_bytes": 100_000}));
    let last = text.lines().last().unwrap();
    assert_eq!(text.lines().count(), 401, "{last}");
    assert!(last.contains("lines 1-400 of 9000") && last.contains("start=401"), "{last}");

    let text = r.ok("read_code", json!({"path": "many.py", "end": 9000, "max_bytes": 1_000_000}));
    assert!(text.lines().last().unwrap().starts_with(" 9000  x_9000"), "an explicit end is honoured");

    let text = r.ok("read_code", json!({"path": "many.py", "end": 9000, "max_bytes": 1000}));
    let last = text.lines().last().unwrap();
    assert!(text.len() <= 1000 && last.contains("of 9000") && last.contains("start="), "{last}");

    let text = r.ok("read_code", json!({"path": "many.py", "start": 2, "end": 3}));
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(text.lines().last().unwrap().contains("lines 2-3 of 9000"), "{text}");
    assert_eq!(r.ok("read_code", json!({"path": "three.py"})).lines().count(), 3, "the whole file needs no note");

    assert_eq!(r.ok("read_code", json!({"path": "empty.py"})), "empty.py is empty");
    assert_eq!(r.ok("read_code", json!({"path": "three.py", "start": 10})), "three.py has 3 lines: start=10 is past the end");
    assert!(r.err("read_code", json!({"path": "three.py", "start": 3, "end": 2})).contains("end (2) is before start (3)"));
}

#[test]
fn read_code_of_a_huge_file_reads_only_what_the_answer_needs() {
    let mut r = Repo::new("read_huge", &[("a.py", "a = 1\n")]);
    // 3 GB and 1 GB sparse files: text at the start, zeros after it, no disk used
    let big = r.root.join("big.log");
    fs::write(&big, many_lines(2000)).unwrap();
    fs::OpenOptions::new().write(true).open(&big).unwrap().set_len(3 << 30).unwrap();
    let wide = r.root.join("wide.txt");
    fs::write(&wide, "w".repeat(20_000)).unwrap();
    fs::OpenOptions::new().write(true).open(&wide).unwrap().set_len(1 << 30).unwrap();
    let zeros = r.root.join("zeros.bin");
    fs::File::create(&zeros).unwrap().set_len(1 << 30).unwrap();

    let t = std::time::Instant::now();
    let text = r.ok("read_code", json!({"path": "big.log", "start": 1500, "end": 1502}));
    assert_eq!(text.lines().count(), 4, "{text}");
    assert!(text.starts_with(" 1500  x_1500 = 1500"), "{text}");
    assert!(text.lines().last().unwrap().contains("more lines follow") && text.contains("start=1503"), "{text}");
    let json = r.repo.run(&scrutate::ops::Op::from_tool("read_code", &json!({"path": "big.log", "end": 2})).unwrap(), 0.0).unwrap().json();
    assert_eq!(json["file_lines"], Value::Null);

    // one line of a gigabyte is cut at the byte budget
    let text = r.ok("read_code", json!({"path": "wide.txt", "max_bytes": 1_000_000}));
    assert!(text.len() <= 1_000_000 && text.starts_with("    1  www"), "{}", &text[text.len().saturating_sub(300)..]);
    assert!(text.contains("more lines follow") || text.contains("cut"), "{}", &text[text.len().saturating_sub(300)..]);

    let msg = r.err("read_code", json!({"path": "zeros.bin"}));
    assert!(msg.contains("not a text file"), "{msg}");
    assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
}

#[test]
fn arguments_of_the_wrong_type_are_errors() {
    let mut r = two_dup_names();
    for (tool, args, name, ty) in [
        ("search_code", json!({"query": "q", "k": "5"}), "k", "integer"),
        ("read_code", json!({"path": "app.py", "start": "2"}), "start", "integer"),
        ("read_code", json!({"path": "app.py", "start": 9223372036854775808u64}), "start", "integer"),
        ("references", json!({"name": "dup_name", "textual": "yes"}), "textual", "boolean"),
        ("find_symbol", json!({"name": "dup_name", "max_bytes": "5000"}), "max_bytes", "integer"),
        ("find_symbol", json!({"name": "dup_name", "offset": -1}), "offset", "integer"),
        ("find_symbol", json!({"name": 7}), "name", "string"),
        ("duplicates", json!({"threshold": "high"}), "threshold", "number"),
    ] {
        let e = r.err(tool, args.clone());
        assert!(e.contains(name) && e.contains(ty), "{tool} {args}: {e}");
    }
    assert_ne!(r.ok("find_symbol", json!({"name": "dup_name", "limit": null})), "", "null is the same as leaving it out");
}

#[test]
fn every_tool_that_takes_a_path_accepts_the_spellings_read_code_does() {
    let mut r = Repo::new("paths", &[("pkg/a.py", "import pkg.b\n\ndef fa():\n    return 1\n"), ("pkg/b.py", "def fb():\n    return 2\n")]);
    for (tool, key, path) in [("outline", "path", "pkg/a.py"), ("dependencies", "path", "pkg/a.py"), ("dependents", "target", "pkg/b.py")] {
        let plain = r.ok(tool, json!({key: path}));
        for spelling in [format!("./{path}"), format!("pkg/../{path}"), format!("pkg/./{}", &path[4..])] {
            assert_eq!(r.ok(tool, json!({key: spelling})), plain, "{tool} {spelling}");
        }
    }
    assert!(r.ok("outline", json!({"path": "pkg/a.py"})).contains("fa"));
}

#[test]
fn a_cut_answer_fits_max_bytes_whole() {
    let files: Vec<(String, String)> = (0..300).map(|i| (format!("m{i}.py"), format!("def widget_{i}():\n    return 'widget {i}'\n"))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let mut r = Repo::new("fits", &refs);
    for max in [1000, 1500, 4000] {
        for (tool, args) in
            [("grep_code", json!({"pattern": "widget"})), ("search_code", json!({"query": "widget", "k": 100})), ("references", json!({"name": "widget_1"}))]
        {
            let mut args = args;
            args["max_bytes"] = json!(max);
            let text = r.ok(tool, args);
            assert!(text.len() <= max, "{tool} max_bytes={max}: {} bytes", text.len());
        }
    }
}

#[test]
fn references_say_what_max_results_cut_and_when_nothing_matched() {
    let mut r = two_dup_names();
    let text = r.ok("references", json!({"name": "dup_name", "max_results": 1}));
    assert!(text.contains("max_results=1") && !text.contains("truncated"), "{text}");
    assert_eq!(r.ok("references", json!({"name": "nothing_has_this_name"})), "(no results)");
    assert!(r.err("references", json!({"name": ""})).contains("name is empty"));
}

#[test]
fn errors_echo_a_short_prefix_of_a_long_argument() {
    let mut r = two_dup_names();
    let long = "z".repeat(50_000);
    for (tool, args) in [
        ("similar_code", json!({"target": long})),
        ("read_code", json!({"path": long})),
        ("read_code", json!({"symbol": long})),
        ("callers", json!({"name": format!("{long}:3")})),
        ("structural_search", json!({"lang": long, "query": "(identifier) @x"})),
    ] {
        let (_, text) = r.call(tool, args);
        assert!(text.len() < 400 && text.contains("characters in all"), "{tool}: {} bytes", text.len());
    }
    for (tool, key) in [("outline", "path"), ("dependencies", "path"), ("dependents", "target")] {
        let (_, text) = r.call(tool, json!({key: long}));
        assert!(text.len() < 400, "{tool}: {} bytes", text.len());
    }
}

// control characters and a colon are not allowed in a Windows file name
#[cfg(unix)]
#[test]
fn control_characters_in_a_file_name_are_shown_escaped() {
    let mut r = Repo::new(
        "evil",
        &[("evil\nfake.py:1-2  [function] injected\n.py", "def evil_fn():\n    return 1\n"), ("esc\x1b[31m.py", "def esc_fn():\n    return 2\n")],
    );
    let text = r.ok("find_symbol", json!({"name": "evil_fn"}));
    assert_eq!(heads(&text), [r"evil\nfake.py:1-2  [function] injected\n.py:1-2  [function] evil_fn"], "{text}");
    let text = r.ok("grep_code", json!({"pattern": "return 2"}));
    assert!(text.contains(r"esc\x1b[31m.py") && !text.contains('\x1b'), "{text}");
}

#[test]
fn structural_search_says_what_is_wrong_with_a_query() {
    let mut r = two_dup_names();
    assert!(r.err("structural_search", json!({"lang": "python", "query": ""})).contains("empty"));
    assert!(r.err("structural_search", json!({"lang": "python", "query": "(identifier)"})).contains("capture"));
    let e = r.err("structural_search", json!({"lang": "python", "query": "((identifier) @x (#nope? @x \"a\"))"}));
    assert!(e.contains("#nope?"), "{e}");
    assert!(!r.ok("structural_search", json!({"lang": "python", "query": "((identifier) @x (#eq? @x \"dup_name\"))"})).contains("no matches"));
}

#[test]
fn a_capped_grep_or_references_answer_is_the_first_matches_in_path_order() {
    let names: Vec<String> = (0..300).map(|i| format!("src/f{i:03}.py")).collect();
    let files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "def needle_fn():\n    return needle_fn\n")).collect();
    let mut r = Repo::new("ordered_cap", &files);
    let grep = r.ok("grep_code", json!({"pattern": "return needle", "max_results": 7}));
    for i in 0..7 {
        assert!(grep.contains(&format!("src/f{i:03}.py")), "{grep}");
    }
    assert!(!grep.contains("src/f007.py") && !grep.contains("src/f299.py"), "{grep}");
    // the same lines on every call, so a second page neither repeats nor skips one
    for (tool, args) in [
        ("grep_code", json!({"pattern": "return needle", "max_results": 7})),
        ("references", json!({"name": "needle_fn", "max_results": 7, "textual": true})),
        ("references", json!({"name": "needle_fn", "max_results": 7, "def": "src/f000.py"})),
    ] {
        let first = r.ok(tool, args.clone());
        for _ in 0..4 {
            assert_eq!(r.ok(tool, args.clone()), first, "{tool} {args}");
        }
    }
}

#[test]
fn grep_code_searches_the_hidden_files_the_index_holds_but_not_git_internals() {
    let mut r = Repo::new(
        "hidden",
        &[
            ("src/a.py", "def needle_fn():\n    return 1\n"),
            (".github/ci.yml", "name: needle_ci\n"),
            (".git/logs/HEAD", "needle commit\n"),
            ("node_modules/pkg/index.js", "function needle_dep() {}\n"),
        ],
    );
    let found = r.ok("grep_code", json!({"pattern": "needle"}));
    assert!(found.contains("src/a.py") && found.contains(".github/ci.yml"), "{found}");
    assert!(!found.contains(".git/") && !found.contains("node_modules"), "{found}");
    assert!(r.ok("search_code", json!({"query": "needle_ci"})).contains(".github/ci.yml"));
}

#[test]
fn grep_code_takes_a_mode_a_case_and_a_scope() {
    let mut r = Repo::new(
        "grep_modes",
        &[
            ("src/a.py", "def refresh_token():\n    return 1\n"),
            ("src/deep/b.py", "REFRESH_TOKEN = 2\n"),
            ("docs/c.md", "refreshToken in the docs\n"),
            (".github/ci.yml", "refresh_token: x\n"),
            ("node_modules/d.js", "refresh_token()\n"),
        ],
    );
    let files = |text: &str| {
        let mut f: Vec<String> = text.lines().filter_map(|l| l.trim_start().split([':', ' ']).next().filter(|p| p.contains('.'))).map(String::from).collect();
        f.sort();
        f.dedup();
        f
    };
    // every casing of the name, in the files the index holds
    let all = r.ok("grep_code", json!({"pattern": "refreshToken", "mode": "identifier"}));
    assert_eq!(files(&all), [".github/ci.yml", "docs/c.md", "src/a.py", "src/deep/b.py"], "{all}");
    let scoped = r.ok("grep_code", json!({"pattern": "refreshToken", "mode": "identifier", "path": "src"}));
    assert_eq!(files(&scoped), ["src/a.py", "src/deep/b.py"], "{scoped}");
    assert!(scoped.contains("src/a.py:1-2  [function] refresh_token"), "the scope keeps the paths from the root: {scoped}");
    let one = r.ok("grep_code", json!({"pattern": "refreshToken", "mode": "identifier", "path": "./src/deep/b.py"}));
    assert_eq!(files(&one), ["src/deep/b.py"], "{one}");
    let globbed = r.ok("grep_code", json!({"pattern": "refreshToken", "mode": "identifier", "globs": ["*.py", "!b.py"]}));
    assert_eq!(files(&globbed), ["src/a.py"], "{globbed}");
    // a glob of the caller does not bring back a directory the index skips
    let skipped = r.ok("grep_code", json!({"pattern": "refresh_token", "globs": ["*.js"]}));
    assert!(skipped.starts_with("0 matching lines"), "{skipped}");
    let any_case = r.ok("grep_code", json!({"pattern": "REFRESH_token", "mode": "literal", "case": "insensitive"}));
    assert_eq!(files(&any_case), [".github/ci.yml", "src/a.py", "src/deep/b.py"], "{any_case}");
    let fuzzy = r.ok("grep_code", json!({"pattern": "refresh_tokne", "mode": "fuzzy", "max_edits": 2, "path": "src/a.py"}));
    assert_eq!(files(&fuzzy), ["src/a.py"], "{fuzzy}");
    assert!(r.ok("grep_code", json!({"pattern": "token", "word": true, "path": "docs"})).starts_with("0 matching lines"));
    assert!(r.err("grep_code", json!({"pattern": "x", "path": "../elsewhere"})).contains("path outside repo: give a path relative to the repository root"));
    assert!(r.err("grep_code", json!({"pattern": "x", "path": "src/none"})).contains("no such file or directory in the repository"));
    assert!(r.err("grep_code", json!({"pattern": "x", "mode": "glob"})).contains("unknown mode glob"));
    assert!(r.err("grep_code", json!({"pattern": "x", "globs": "*.py"})).contains("globs must be an array of strings"));
}

#[test]
fn grep_code_shows_a_binary_file_note_as_a_note() {
    let filler: String = (0..3000).map(|i| format!("x = {i}  # filler line to push the NUL past the first read\n")).collect();
    let body = format!("def needle_xyz():\n    return 1\n\n{filler}\0\nneedle_xyz()\n");
    let mut r = Repo::new("nul", &[("nul_probe.py", body.as_str())]);
    let text = r.ok("grep_code", json!({"pattern": "needle_xyz"}));
    assert!(text.contains("def needle_xyz") && text.contains("stopped searching binary file"), "{text}");
    assert!(!text.contains(&u64::MAX.to_string()), "{text}");
    let op = scrutate::ops::Op::from_tool("grep_code", &json!({"pattern": "needle_xyz"})).unwrap();
    let v = r.repo.run(&op, 0.0).unwrap().json();
    assert!(v.to_string().contains("stopped searching binary file") && !v.to_string().contains(&u64::MAX.to_string()), "{v}");
}

#[test]
fn resolved_references_say_when_a_file_s_search_stopped_at_a_nul_byte() {
    let filler: String = (0..3000).map(|i| format!("x = {i}  # filler line to push the NUL past the first read\n")).collect();
    let body = format!("def needle_xyz():\n    return 1\n\n{filler}\0\nneedle_xyz()\n");
    let mut r =
        Repo::new("nul_refs", &[("nul_probe.py", body.as_str()), ("use.py", "from nul_probe import needle_xyz\n\n\ndef go():\n    return needle_xyz()\n")]);
    let text = r.ok("references", json!({"name": "needle_xyz"}));
    assert!(text.contains("use.py:5") && text.contains("note: nul_probe.py: ") && text.contains("stopped searching binary file"), "{text}");
    let op = scrutate::ops::Op::from_tool("references", &json!({"name": "needle_xyz"})).unwrap();
    let v = r.repo.run(&op, 0.0).unwrap().json();
    assert_eq!(v["notes"][0]["path"], "nul_probe.py", "{v}");
    assert!(v["notes"][0]["note"].as_str().unwrap().contains("stopped searching binary file"), "{v}");
    // a name with no note has no such key
    let op = scrutate::ops::Op::from_tool("references", &json!({"name": "go"})).unwrap();
    assert!(r.repo.run(&op, 0.0).unwrap().json().get("notes").is_none());
}

#[test]
fn a_file_written_just_before_a_call_is_searched_inside_the_refresh_interval() {
    let mut files: Vec<(String, String)> = (0..40).map(|i| (format!("m{i:03}.py"), format!("def fn_{i:03}(x):\n    return x + {i}\n"))).collect();
    files.push(("use.py".into(), "from m001 import fn_001\n".into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let r = Repo::new("fresh_grep", &refs);
    scrutate::stores::update(&r.root, scrutate::stores::Writer::Index, false).unwrap();
    assert!(scrutate::textindex::exists(&r.root));
    // a server that looks for changes once a minute, after its first look
    let s = session(&r.root, 60.0);
    let ok = |tool: &str, args: Value| {
        let c = call_tool(&s, tool, &args);
        assert!(!c.is_error, "{}", c.text);
        c.text
    };
    ok("grep_code", json!({"pattern": "zz_written_late"}));
    fs::write(r.root.join("new_file.py"), "x = 'zz_written_late'\n").unwrap();
    fs::write(r.root.join("m010.py"), "def fn_010(x):\n    return x + 10  # zz_written_late\n").unwrap();
    fs::write(r.root.join("use.py"), "from m001 import fn_001\n\n\ndef later():\n    return fn_001(2)\n").unwrap();
    let grep = ok("grep_code", json!({"pattern": "zz_written_late"}));
    assert!(grep.contains("new_file.py") && grep.contains("m010.py"), "{grep}");
    let textual = ok("references", json!({"name": "zz_written_late", "textual": true}));
    assert!(textual.contains("new_file.py") && textual.contains("m010.py"), "{textual}");
    let resolved = ok("references", json!({"name": "fn_001"}));
    assert!(resolved.contains("use.py:5"), "{resolved}");
}

#[test]
fn resolved_references_answer_alike_with_and_without_the_text_index() {
    let mut files: Vec<(String, String)> = (0..60).map(|i| (format!("pkg/m{i:02}.py"), format!("def fn_{i}(x):\n    return x + {i}\n"))).collect();
    files.push(("pkg/rare.py".into(), "def needle_rare():\n    return 7\n".into()));
    files.push(("pkg/use.py".into(), "from pkg.rare import needle_rare\n\n\ndef go():\n    return needle_rare()\n".into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let mut r = Repo::new("refs_text_index", &refs);
    let op = scrutate::ops::Op::from_tool("references", &json!({"name": "needle_rare"})).unwrap();
    // with a text index its trigrams pick the files to read; the answer must not show it
    scrutate::stores::update(&r.root, scrutate::stores::Writer::Index, false).unwrap();
    assert!(scrutate::textindex::exists(&r.root));
    let with = r.repo.run(&op, 0.0).unwrap().json();
    assert!(with.to_string().contains("pkg/use.py"), "{with}");
    fs::remove_dir_all(scrutate::textindex::dir_of(&r.root)).unwrap();
    let without = r.repo.run(&op, 0.0).unwrap().json();
    assert_eq!(with, without);
}

#[test]
fn grep_code_and_textual_references_search_the_same_files() {
    let mut r = Repo::new(
        "hidden_refs",
        &[
            (".hidden/h.py", "def needle_hidden():\n    return 1\n"),
            ("use.py", "from x import needle_hidden\n\n\ndef go():\n    return needle_hidden()\n"),
            ("node_modules/pkg/index.js", "needle_hidden()\n"),
        ],
    );
    let paths = |v: Value, key: &str| -> Vec<String> {
        let mut p: Vec<String> = v[key].as_array().unwrap().iter().map(|g| g["path"].as_str().unwrap().to_string()).collect();
        p.sort();
        p.dedup();
        p
    };
    let grep = scrutate::ops::Op::from_tool("grep_code", &json!({"pattern": "needle_hidden"})).unwrap();
    let refs = scrutate::ops::Op::from_tool("references", &json!({"name": "needle_hidden", "textual": true})).unwrap();
    let g = paths(r.repo.run(&grep, 0.0).unwrap().json(), "groups");
    let t = paths(r.repo.run(&refs, 0.0).unwrap().json(), "references");
    assert_eq!(g, [".hidden/h.py", "use.py"]);
    assert_eq!(t, g);
}

#[test]
fn a_cancelled_call_searches_no_file() {
    let mut r = Repo::new("cancelled", &[("a.py", "def needle_fn():\n    return needle_fn\n")]);
    for (tool, args, key) in
        [("grep_code", json!({"pattern": "needle"}), "groups"), ("references", json!({"name": "needle_fn", "textual": true}), "references")]
    {
        let op = scrutate::ops::Op::from_tool(tool, &args).unwrap();
        assert_eq!(r.repo.run(&op, 0.0).unwrap().json()[key][0]["path"], "a.py", "{tool}");
        r.repo.set_cancel(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)));
        let v = r.repo.run_as_is(&op).map(|a| a.json()).unwrap_or_default();
        r.repo.set_cancel(std::sync::Arc::default());
        assert!(v[key].as_array().is_none_or(Vec::is_empty), "{tool}: {v}");
    }
}

#[test]
fn callees_match_the_name_as_the_call_writes_it() {
    let src = "pub struct Report;\nimpl Report {\n    pub fn ok(&self) -> bool {\n        true\n    }\n}\npub fn returns_ok() -> Result<i32, ()> {\n    Ok(1)\n}\npub fn some(x: i32) -> i32 {\n    x\n}\npub fn makes_some() -> Option<i32> {\n    Some(1)\n}\npub fn asks(r: &Report) -> bool {\n    r.ok()\n}\n";
    let mut r = Repo::new("callee_case", &[("src/lib.rs", src)]);
    assert!(!r.ok("callees", json!({"name": "returns_ok"})).contains("Report.ok"));
    assert!(!r.ok("callees", json!({"name": "makes_some"})).contains("] some"));
    assert!(r.ok("callees", json!({"name": "asks"})).contains("Report.ok"));
}

#[test]
fn read_code_takes_the_smallest_start_as_line_one() {
    let mut r = Repo::new("read_min", &[("m.py", "a = 1\nb = 2\n")]);
    let text = r.ok("read_code", json!({"path": "m.py", "start": i64::MIN}));
    assert!(text.contains("a = 1") && !text.contains("past the end"), "{text}");
}

#[test]
fn type_colon_colon_method_names_one_definition_like_type_dot_method() {
    let mut r = Repo::new(
        "scoped",
        &[
            (
                "src/config.rs",
                "pub struct Config;\n\nimpl Config {\n    pub fn new() -> Config {\n        Config::helper();\n        Config\n    }\n    fn helper() {}\n}\n\npub struct Other;\n\nimpl Other {\n    pub fn new() -> Other {\n        Other\n    }\n}\n\npub fn make() -> Config {\n    Config::new()\n}\n",
            ),
            ("widget.cpp", "class Widget { public: void draw(); };\n\nvoid Widget::draw() {\n    int x = 1;\n}\n"),
            ("store.go", "package store\n\ntype Store struct{}\n\nfunc (s *Store) Get() int {\n\treturn 1\n}\n"),
            ("store.rb", "class Shelf\n  def get\n    1\n  end\nend\n"),
        ],
    );
    // a colon is not allowed in a Windows file name
    if cfg!(unix) {
        fs::write(r.root.join("a::b.py"), "def in_odd_file():\n    return 1\n").unwrap();
    }
    for (dotted, scoped) in [("Config.new", "Config::new"), ("Widget.draw", "Widget::draw"), ("Store.Get", "Store::Get"), ("Shelf.get", "Shelf#get")] {
        let want = r.ok("find_symbol", json!({"name": dotted}));
        assert_ne!(want, "(no results)", "{dotted}");
        assert_eq!(r.ok("find_symbol", json!({"name": scoped})), want, "{scoped}");
    }
    assert_eq!(heads(&r.ok("find_symbol", json!({"name": "Config::new"}))), ["src/config.rs:4-7  [function] Config.new"]);
    assert_eq!(r.ok("find_symbol", json!({"name": "Widget->draw"})), r.ok("find_symbol", json!({"name": "Widget.draw"})));
    assert_eq!(r.ok("find_symbol", json!({"name": "::make"})), r.ok("find_symbol", json!({"name": "make"})));
    for (tool, key) in [("read_code", "symbol"), ("callers", "name"), ("callees", "name"), ("similar_code", "target")] {
        assert_eq!(r.call(tool, json!({key: "Config::new"})), r.call(tool, json!({key: "Config.new"})), "{tool}");
    }
    assert!(r.ok("callers", json!({"name": "Config::new"})).contains("make"));
    if cfg!(unix) {
        assert!(r.ok("read_code", json!({"symbol": "a::b.py:1-2"})).contains("in_odd_file"));
        assert!(r.ok("find_symbol", json!({"name": "a::b.py:1"})).contains("a::b.py:1-2"));
    }
}

#[test]
fn a_call_through_an_imported_module_is_not_a_call_of_a_local_namesake() {
    let mut r = Repo::new(
        "module_calls",
        &[
            ("cls.py", "class Alpha:\n    def run(self):\n        return 1\n\n\ndef drive(a: Alpha):\n    return a.run()\n"),
            ("other.py", "import subprocess\nimport os.path\n\n\ndef shell_out():\n    return subprocess.run(['ls'])\n\n\ndef join_it():\n    return os.path.join('a', 'b')\n"),
            ("paths.py", "def join(a, b):\n    return a + b\n\n\ndef glue():\n    return join('a', 'b')\n"),
        ],
    );
    let callers = r.ok("callers", json!({"name": "Alpha.run"}));
    assert!(!callers.contains("shell_out"), "{callers}");
    assert!(!r.ok("callees", json!({"name": "shell_out"})).contains("Alpha.run"));
    let join = r.ok("callers", json!({"name": "join"}));
    assert!(join.contains("glue") && !join.contains("join_it"), "{join}");
    let callees = r.ok("callees", json!({"name": "join_it"}));
    assert!(!callees.contains("paths.py") && callees.contains("join"), "{callees}");
}

// a line break or a tab is not allowed in a Windows file name
#[cfg(unix)]
#[test]
fn a_location_printed_with_escapes_can_be_passed_back() {
    let mut r = Repo::new(
        "escaped_names",
        &[("evil\nname.py", "def newline_name_fn():\n    return helper_one()\n"), ("dir/tab\tname.py", "def tab_name_fn():\n    return 2\n")],
    );
    for (name, shown) in [("newline_name_fn", "evil\\nname.py"), ("tab_name_fn", "dir/tab\\tname.py")] {
        let found = r.ok("find_symbol", json!({"name": name}));
        let location = found.split_whitespace().next().unwrap().to_string();
        assert!(location.starts_with(shown), "{found}");
        assert!(r.ok("read_code", json!({"symbol": location})).contains(name), "{location}");
        assert!(!r.ok("callees", json!({"name": location})).contains("no symbol"), "{location}");
        assert!(r.ok("outline", json!({"path": shown})).contains(name), "{shown}");
        assert!(r.ok("read_code", json!({"path": shown})).contains(name), "{shown}");
        // a two-line function has no vector to compare, which is a different answer from "not found"
        assert!(!r.call("similar_code", json!({"target": location})).1.contains("no symbol or location"), "{location}");
    }
}

#[test]
fn an_operator_name_keeps_its_arrow() {
    let src = "struct Handle {\n    int* p;\n    int* operator->() {\n        return p;\n    }\n    int& operator*() {\n        return *p;\n    }\n};\n";
    let mut r = Repo::new("operators", &[("h.cpp", src)]);
    assert!(r.ok("find_symbol", json!({"name": "operator->"})).contains("h.cpp:3-5"));
    assert!(r.ok("find_symbol", json!({"name": "Handle::operator->"})).contains("h.cpp:3-5"));
    assert!(r.ok("find_symbol", json!({"name": "Handle->operator*"})).contains("h.cpp:6-8"));
}

#[test]
fn an_answer_inside_the_refresh_interval_shows_the_files_it_cites_as_they_are_now() {
    let geo = "def make_point():\n    return 1\n\n\ndef caller_one():\n    return make_point()\n";
    let mut r = Repo::new("fresh", &[("geo.py", geo), ("other.py", "def unrelated():\n    return 2\n")]);
    // one scan at the first call, none after it for an hour
    let s = session(&r.root, 3600.0);
    let call = |_: &mut Repo, tool: &str, args: Value| {
        let c = call_tool(&s, tool, &args);
        assert!(!c.is_error, "{tool} {args}: {}", c.text);
        c.text
    };
    assert!(call(&mut r, "find_symbol", json!({"name": "make_point"})).starts_with("geo.py:1-2"));
    assert!(call(&mut r, "callers", json!({"name": "make_point"})).contains("caller_one"));
    assert!(call(&mut r, "find_symbol", json!({"name": "unrelated"})).starts_with("other.py:1-2"));

    fs::write(r.root.join("geo.py"), "# one\n# two\n# three\ndef make_point():\n    return 1\n\n\ndef caller_one():\n    return 0\n").unwrap();
    fs::remove_file(r.root.join("other.py")).unwrap();

    let read = call(&mut r, "read_code", json!({"symbol": "make_point"}));
    assert!(read.starts_with("geo.py:4-5") && read.contains("def make_point") && !read.contains("# three"), "{read}");
    assert!(call(&mut r, "find_symbol", json!({"name": "make_point"})).starts_with("geo.py:4-5"));
    assert!(call(&mut r, "search_code", json!({"query": "make_point"})).starts_with("geo.py:4-5"));
    assert_eq!(call(&mut r, "callers", json!({"name": "make_point"})), "(no results)");
    assert_eq!(call(&mut r, "find_symbol", json!({"name": "unrelated"})), "(no results)");
}

/// The functions named in the location lines of an answer.
fn names(text: &str) -> Vec<&str> {
    heads(text).iter().filter_map(|l| l.rsplit(' ').next()).collect()
}

#[test]
fn a_method_call_on_a_value_of_a_type_from_outside_calls_no_namesake_of_the_repository() {
    let rs = "pub struct Buf {\n    data: Vec<u8>,\n}\n\nimpl Buf {\n    pub fn len(&self) -> usize {\n        self.data.len()\n    }\n}\n\npub fn len() -> usize {\n    0\n}\n\nfn vec_len(v: &Vec<u8>) -> usize {\n    v.len()\n}\n\nfn str_len(s: &str) -> usize {\n    s.len()\n}\n\nfn buf_len(b: &Buf) -> usize {\n    b.len()\n}\n\nfn free_len() -> usize {\n    len()\n}\n\nfn unknown_len(x: impl Sized) -> usize {\n    x.len()\n}\n";
    let py = "class Box:\n    def size(self):\n        return 1\n\n\ndef size():\n    return 0\n\n\ndef list_size(items: list):\n    return items.size()\n\n\ndef str_size(s: str):\n    return s.size()\n\n\ndef box_size(b: Box):\n    return b.size()\n\n\ndef free_size():\n    return size()\n";
    let ts = "export class Tally {\n  count(): number { return 1; }\n}\n\nexport function count(): number { return 0; }\n\nexport function arrayCount(a: Array<number>): number {\n  return a.count();\n}\n\nexport function tallyCount(t: Tally): number {\n  return t.count();\n}\n\nexport function freeCount(): number {\n  return count();\n}\n\nexport function anyCount(x) {\n  return x.count();\n}\n";
    let mut r = Repo::new("foreign_types", &[("src/lib.rs", rs), ("box.py", py), ("tally.ts", ts)]);
    assert_eq!(names(&r.ok("callers", json!({"name": "Buf.len"}))), ["buf_len", "unknown_len"]);
    assert_eq!(names(&r.ok("callers", json!({"name": "src/lib.rs:11-13"}))), ["free_len"]);
    for f in ["vec_len", "str_len"] {
        let callees = r.ok("callees", json!({"name": f}));
        assert!(callees.starts_with("(no results)") && callees.contains("external: len"), "{f}: {callees}");
    }
    assert_eq!(names(&r.ok("callers", json!({"name": "Box.size"}))), ["box_size"]);
    assert_eq!(names(&r.ok("callers", json!({"name": "box.py:6-7"}))), ["free_size"]);
    assert_eq!(names(&r.ok("callers", json!({"name": "Tally.count"}))), ["tallyCount", "anyCount"]);
    assert_eq!(names(&r.ok("callers", json!({"name": "tally.ts:5-5"}))), ["freeCount"]);
}

#[test]
fn a_tool_that_stops_at_its_limit_says_so_and_names_the_argument() {
    let mut src = String::from("def target():\n    return 1\n\n\n");
    for i in 0..60 {
        src.push_str(&format!("def caller_{i}():\n    return target()\n\n\n"));
    }
    src.push_str("def hub():\n");
    for i in 0..80 {
        src.push_str(&format!("    callee_{i}()\n"));
    }
    src.push_str("\n\n");
    for i in 0..80 {
        src.push_str(&format!("def callee_{i}():\n    return {i}\n\n\n"));
    }
    let mut files = vec![("main.py".to_string(), src)];
    for i in 0..40 {
        files.push((format!("same/m{i}.py"), "def same_name():\n    return 1\n".to_string()));
    }
    let files: Vec<(&str, &str)> = files.iter().map(|(p, s)| (p.as_str(), s.as_str())).collect();
    let mut r = Repo::new("limits", &files);
    let last = |t: &str| t.lines().last().unwrap_or("").to_string();
    for (tool, args, shown, raise) in [
        ("callers", json!({"name": "target"}), 30, "limit"),
        ("callees", json!({"name": "hub"}), 40, "limit"),
        ("find_symbol", json!({"name": "same_name"}), 20, "limit"),
        ("read_code", json!({"symbol": "same_name"}), 20, "path"),
        ("structural_search", json!({"lang": "python", "query": "(function_definition name: (identifier) @n)", "limit": 5}), 5, "limit"),
    ] {
        let text = r.ok(tool, args.clone());
        let end = last(&text);
        assert!(end.starts_with(&format!("(stopped at {shown} ")) && end.contains(raise), "{tool}: {end}");
        let c = call_tool(&r.session, tool, &args);
        assert_eq!(c.structured["stopped_at"], shown, "{tool}");
    }
    let whole = r.ok("callers", json!({"name": "target", "limit": 100}));
    assert!(!whole.contains("stopped at") && heads(&whole).len() == 60, "{whole}");
}

#[cfg(unix)]
#[test]
fn a_name_cannot_forge_a_result_line() {
    let evil = "evil\nfake.py:1-2  [function] injected\n.py";
    let mut r = Repo::new("forged_name", &[(evil, "import os\n\n\ndef real_fn():\n    return os.sep\n")]);
    let shown_path = "evil\\nfake.py:1-2  [function] injected\\n.py";
    for (tool, args) in [
        ("outline", json!({"path": shown_path})),
        ("search_code", json!({"query": "evil injected real_fn"})),
        ("find_symbol", json!({"name": "evil"})),
        ("read_code", json!({"symbol": "real_fn"})),
        ("grep_code", json!({"pattern": "import os"})),
    ] {
        let text = r.ok(tool, args);
        assert!(!text.lines().any(|l| l.starts_with("fake.py") || l.starts_with(".py")), "{tool}: {text}");
    }
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_file_name_is_shown_apart_from_an_escape() {
    let mut r = Repo::new("backslash", &[("real\\nname.py", "def slash_fn():\n    return 1\n"), ("real\nname.py", "def newline_fn():\n    return 2\n")]);
    let slash = r.ok("find_symbol", json!({"name": "slash_fn"}));
    let newline = r.ok("find_symbol", json!({"name": "newline_fn"}));
    assert!(slash.starts_with("real\\\\nname.py:1-2"), "{slash}");
    assert!(newline.starts_with("real\\nname.py:1-2"), "{newline}");
    for (found, name) in [(slash, "slash_fn"), (newline, "newline_fn")] {
        let location = found.split_whitespace().next().unwrap().to_string();
        assert!(r.ok("read_code", json!({"symbol": location})).contains(name), "{location}");
        let path = location.rsplit_once(':').unwrap().0.to_string();
        assert!(r.ok("outline", json!({"path": path})).contains(name), "{path}");
    }
    // a path as the file system spells it works too, when no file prints as that text
    let mut r = Repo::new("backslash_alone", &[("real\\nname.py", "def slash_fn():\n    return 1\n")]);
    assert!(r.ok("outline", json!({"path": "real\\nname.py"})).contains("slash_fn"));
    assert!(r.ok("read_code", json!({"path": "real\\nname.py"})).contains("slash_fn"));
}

#[test]
fn similar_code_says_a_short_body_is_too_short_to_compare() {
    let mut r = Repo::new("short_body", &[("a.py", "def one():\n    return 1\n")]);
    let (_, text) = r.call("similar_code", json!({"target": "one"}));
    assert!(text.contains("too short to compare") && !text.contains("empty body"), "{text}");
}

#[test]
fn the_repository_s_own_exclude_dirs_applies_whatever_directory_the_server_runs_in() {
    let mut r = Repo::new(
        "exclude_dirs",
        &[
            (".scrutate.toml", "[index]\nexclude_dirs = [\"gen\", \"*\", \"/src\"]\n"),
            ("gen/g.py", "def g_fn():\n    return 1\n"),
            ("src/s.py", "def s_fn():\n    return 2\n"),
        ],
    );
    assert_eq!(r.ok("find_symbol", json!({"name": "g_fn"})), "(no results)");
    assert!(r.ok("find_symbol", json!({"name": "s_fn"})).starts_with("src/s.py:1-2"));
    let grep = r.ok("grep_code", json!({"pattern": "_fn"}));
    assert!(grep.contains("src/s.py") && !grep.contains("gen/g.py"), "{grep}");
}

#[test]
fn an_exclude_dirs_name_with_a_backslash_excludes_nothing() {
    let mut files = vec![
        (".scrutate.toml", "[index]\nexclude_dirs = [\"a\\\\b\", \"x\\\\\"]\n"),
        ("ab/b.py", "def ab_fn():\n    return 1\n"),
        ("src/s.py", "def s_fn():\n    return 2\n"),
    ];
    if cfg!(unix) {
        files.push(("a\\b/c.py", "def back_fn():\n    return 3\n"));
    }
    let mut r = Repo::new("exclude_backslash", &files);
    let grep = r.ok("grep_code", json!({"pattern": "_fn"}));
    assert!(grep.contains("ab/b.py") && grep.contains("src/s.py"), "{grep}");
    assert!(r.ok("find_symbol", json!({"name": "ab_fn"})).starts_with("ab/b.py:1-2"));
    if cfg!(unix) {
        assert!(grep.contains("b/c.py"), "{grep}");
        let found = r.ok("find_symbol", json!({"name": "back_fn"}));
        assert!(found.contains("b/c.py:1-2"), "{found}");
    }
}

#[test]
fn similar_code_reads_a_location_as_the_other_tools_read_it() {
    let body = "def area(w, h):\n    scaled = compute_scale(w) * adjust_height(h)\n    return finalize_area(scaled, w, h)\n";
    let mut r = Repo::new("similar_spellings", &[("src/geo.py", body), ("src/geo_copy.py", body)]);
    let want = r.ok("similar_code", json!({"target": "src/geo.py:1"}));
    assert!(want.contains("src/geo_copy.py:1-3"), "{want}");
    let abs = format!("{}:1", r.root.join("src/geo.py").to_string_lossy().replace('\\', "/"));
    let mut spellings = vec!["src/../src/geo.py:1".to_string(), "./src/geo.py:1".to_string(), "src//geo.py:1".to_string(), abs];
    // another case of the name reaches the file only where the file system ignores case
    if r.root.join("SRC/GEO.py").exists() {
        spellings.push("SRC/GEO.py:1".to_string());
    }
    for target in spellings {
        assert_eq!(r.ok("similar_code", json!({"target": target})), want, "{target}");
        assert!(!r.call("find_symbol", json!({"name": target})).0, "{target}");
    }
    // a path that leaves the root by its parts and comes back is read from the root, as the
    // other tools read it
    let name = r.root.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(r.ok("similar_code", json!({"target": format!("../{name}/src/geo.py:1")})), want);
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_unix_file_name_stays_in_the_paths_answers_print() {
    let mut r = Repo::new("backslash", &[("dir\\x.py", "def slash_fn():\n    return 1\n")]);
    r.ok("reindex", json!({}));
    let text = r.ok("structural_search", json!({"lang": "python", "query": "((identifier) @x (#eq? @x \"slash_fn\"))"}));
    assert!(text.contains(r"dir\\x.py:1") && !text.contains("dir/x.py"), "{text}");
    // another case of the name reaches the file only where the file system ignores case
    if r.root.join("DIR\\x.py").exists() {
        let text = r.ok("outline", json!({"path": "DIR\\x.py"}));
        assert!(text.contains("slash_fn"), "{text}");
    }
}

#[test]
fn duplicates_say_where_limit_stopped_them() {
    // three groups of three identical functions, bodies of 6, 5 and 3 tokens
    let bodies = ["return alpha + beta + gamma + delta + omega + sigma", "return alpha + beta + gamma + delta + omega", "return alpha + beta + gamma"];
    let files: Vec<(String, String)> =
        (0..9).map(|n| (format!("g{}_{}.py", n / 3, n % 3), format!("def f{n}(alpha, beta, gamma, delta, omega, sigma):\n    {}\n", bodies[n / 3]))).collect();
    let files: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let mut r = Repo::new("dupes_limit", &files);
    let all = r.ok("duplicates", json!({"min_tokens": 1}));
    assert_eq!(all.lines().filter(|l| l.starts_with("exact")).count(), 3, "{all}");
    assert!(!all.contains("stopped at"), "{all}");
    let one = r.ok("duplicates", json!({"min_tokens": 1, "limit": 1}));
    assert_eq!(one.lines().filter(|l| l.starts_with("exact")).count(), 1, "{one}");
    assert_eq!(one.lines().last(), Some("(stopped at 1 groups: raise limit to see more)"), "{one}");
    let c = call_tool(&r.session, "duplicates", &json!({"min_tokens": 1, "limit": 1}));
    assert_eq!(c.structured["stopped_at"], 1, "{}", c.structured);
}

#[cfg(unix)]
#[test]
fn a_path_through_a_linked_folder_reads_nothing_outside_the_repository() {
    use std::os::unix::fs::symlink;
    let leak = "def leaked_fn():\n    TOKEN = 'outside_root_mark'\n    return TOKEN\n";
    let r = Repo::new("linkdir", &[("src/a.py", "def inside_fn():\n    return 1\n"), ("sub/b.py", "def sub_fn():\n    return 2\n")]);
    let outside = r.root.with_file_name(format!("{}_outside", r.root.file_name().unwrap().to_string_lossy()));
    let _ = fs::remove_dir_all(&outside);
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.py"), leak).unwrap();
    fs::write(outside.join("b.py"), leak).unwrap();
    symlink(&outside, r.root.join("link")).unwrap();
    symlink(r.root.join("src"), r.root.join("inlink")).unwrap();
    // one scan at the first call, so later calls go through the files their arguments name
    let s = session(&r.root, 3600.0);
    let call = |tool: &str, args: Value| call_tool(&s, tool, &args);
    assert!(call("find_symbol", json!({"name": "sub_fn"})).text.starts_with("sub/b.py:1-2"));
    let calls = [
        ("find_symbol", json!({"name": "link/secret.py:1"})),
        ("outline", json!({"path": "link/secret.py"})),
        ("callees", json!({"name": "link/secret.py:2"})),
        ("similar_code", json!({"target": "link/secret.py:1"})),
        ("dependencies", json!({"path": "link/secret.py"})),
        ("references", json!({"name": "TOKEN", "def": "link/secret.py:1"})),
        ("read_code", json!({"path": "link/secret.py"})),
        ("find_symbol", json!({"name": "leaked_fn"})),
    ];
    for (tool, args) in &calls {
        let text = call(tool, args.clone()).text;
        assert!(!text.contains("leaked_fn") && !text.contains("outside_root_mark"), "{tool} {args}: {text}");
    }
    // an indexed folder replaced by a link: its files are gone, not read through the link
    fs::remove_dir_all(r.root.join("sub")).unwrap();
    symlink(&outside, r.root.join("sub")).unwrap();
    for (tool, args) in
        [("find_symbol", json!({"name": "sub/b.py:1"})), ("outline", json!({"path": "sub/b.py"})), ("find_symbol", json!({"name": "leaked_fn"}))]
    {
        let text = call(tool, args.clone()).text;
        assert!(!text.contains("leaked_fn"), "{tool} {args}: {text}");
    }
    // a link that stays inside the repository answers as before
    let inner = call("outline", json!({"path": "inlink/a.py"}));
    assert!(!inner.is_error && inner.text.contains("inside_fn"), "{}", inner.text);
    // and a new file named through it is indexed once, under the path a scan lists
    fs::write(r.root.join("src/fresh.py"), "def fresh_fn():\n    return 3\n").unwrap();
    let fresh = call("outline", json!({"path": "inlink/fresh.py"}));
    assert!(!fresh.is_error && fresh.text.contains("fresh_fn"), "{}", fresh.text);
    let found = call("find_symbol", json!({"name": "fresh_fn"})).text;
    assert!(found.starts_with("src/fresh.py:1-2") && !found.contains("inlink/"), "{found}");
    let _ = fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn a_root_reached_through_a_link_indexes_a_new_file_an_argument_names() {
    let r = Repo::new("linkroot", &[("a.py", "def first_fn():\n    return 1\n")]);
    let via = r.root.with_file_name(format!("{}_via", r.root.file_name().unwrap().to_string_lossy()));
    let _ = fs::remove_file(&via);
    std::os::unix::fs::symlink(&r.root, &via).unwrap();
    let s = session(&via, 3600.0);
    assert!(call_tool(&s, "find_symbol", &json!({"name": "first_fn"})).text.starts_with("a.py:1-2"));
    fs::write(r.root.join("b.py"), "def second_fn():\n    return 2\n").unwrap();
    let c = call_tool(&s, "outline", &json!({"path": "b.py"}));
    assert!(!c.is_error && c.text.contains("second_fn"), "{}", c.text);
    drop(s);
    let _ = fs::remove_file(&via);
}

/// The items of a structured result, flat: a definition or group a page boundary cuts has its
/// head on each page it is on, which `flat` counts once.
fn items(tool: &str, textual: bool, r: &Value) -> Vec<Value> {
    let list = |v: &Value, tag: &str| v.as_array().unwrap().iter().map(|x| json!([tag, x])).collect::<Vec<_>>();
    let without = |v: &Value, keys: &[&str]| {
        let mut v = v.clone();
        keys.iter().for_each(|k| drop(v.as_object_mut().unwrap().remove(*k)));
        v
    };
    match tool {
        "grep_code" => list(&r["groups"], "group"),
        "dead_code" => [list(&r["dead"], "dead"), list(&r["undecided"], "undecided")].concat(),
        "read_code" => r["text"].as_str().unwrap().lines().map(|l| json!(l)).collect(),
        "references" if textual => {
            let mut out = [list(&r["definitions"], "def"), list(&r["callers"], "caller")].concat();
            for g in r["references"].as_array().unwrap() {
                out.push(json!(["head", without(g, &["lines", "note"])]));
                out.extend(list(&g["lines"], "line"));
                out.extend(g.get("note").map(|n| json!(["note", n])));
            }
            out
        }
        "references" => {
            let mut out = Vec::new();
            for d in r["definitions"].as_array().unwrap() {
                out.push(json!(["head", without(d, &["references"])]));
                out.extend(list(&d["references"], "ref"));
            }
            out.extend(list(&r["unresolved"], "unresolved"));
            out.extend(r.get("notes").map(|n| list(n, "note")).unwrap_or_default());
            out
        }
        _ => list(r, "item"),
    }
}

/// `items` of every page in order, each head once.
fn flat(pages: Vec<Vec<Value>>) -> Vec<Value> {
    let mut heads = std::collections::HashSet::new();
    pages.into_iter().flatten().filter(|x| x[0] != "head" || heads.insert(x.to_string())).collect()
}

#[test]
fn a_large_answer_pages_its_structured_items_to_the_end_in_both_forms() {
    let mut files: Vec<(String, String)> = vec![("util.py".into(), "def shared_helper(x):\n    return x\n".into())];
    for i in 0..60 {
        let body = format!(
            "from util import shared_helper\n\n\ndef user_{i}():\n    return shared_helper({i})\n\n\ndef other_{i}():\n    return shared_helper(-{i})\n"
        );
        files.push((format!("pkg/a{i}.py"), body));
    }
    files.push(("big.py".into(), (0..120).map(|i| format!("def lone_{i}():\n    return {i}\n\n\n")).collect()));
    let files: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let r = Repo::new("pages", &files);
    let cases = [
        ("grep_code", json!({"pattern": "shared_helper", "max_results": 1000})),
        ("references", json!({"name": "shared_helper", "max_results": 1000})),
        ("references", json!({"name": "shared_helper", "max_results": 1000, "textual": true})),
        ("callers", json!({"name": "shared_helper", "limit": 500})),
        ("outline", json!({"path": "big.py"})),
        ("read_code", json!({"path": "big.py"})),
        ("dead_code", json!({})),
    ];
    for (tool, args) in cases {
        let textual = args["textual"] == true;
        let schema = scrutate::ops::output_schema(tool);
        let at = |offset: u64, max_bytes: u64, form: &str| {
            let mut a = args.clone();
            a["offset"] = json!(offset);
            a["max_bytes"] = json!(max_bytes);
            a["response_format"] = json!(form);
            let c = call_tool(&r.session, tool, &a);
            assert!(!c.is_error, "{tool} {a}: {}", c.text);
            schema::conforms(&schema, &c.structured).unwrap_or_else(|e| panic!("{tool}: {e}"));
            c
        };
        let whole = at(0, 1_000_000, "detailed").structured;
        assert_eq!(whole["complete"], true, "{tool}");
        let (mut offset, mut pages, mut total) = (0, Vec::new(), None);
        loop {
            let page = at(offset, 1_000, "detailed");
            let s = &page.structured;
            assert_eq!((&s["complete"], &s["offset"]), (&json!(false), &json!(offset)), "{tool}: {s}");
            assert_eq!(*total.get_or_insert(s["total"].clone()), s["total"], "{tool}");
            assert_eq!(at(offset, 1_000, "concise").structured, *s, "{tool}: a concise page holds the same items");
            pages.push(items(tool, textual, &s["result"]));
            let Some(next) = s["next_offset"].as_u64() else { break };
            assert!(next > offset, "{tool}");
            if tool != "read_code" {
                assert!(page.text.contains(&format!("offset={next}")), "{tool}: {}", page.text);
            }
            offset = next;
        }
        assert!(pages.len() > 2, "{tool}: {} pages", pages.len());
        assert_eq!(flat(pages), items(tool, textual, &whole["result"]), "{tool}");
    }
}

#[test]
fn repo_map_and_dead_code_take_an_absolute_directory_and_refuse_a_missing_one() {
    let mut r = Repo::new("abs_dir", &[("src/a.py", "def used():\n    return 1\n\n\ndef unused():\n    return used()\n"), ("top.py", "X = 1\n")]);
    let abs = r.root.join("src").to_string_lossy().into_owned();
    for tool in ["repo_map", "dead_code"] {
        let plain = r.ok(tool, json!({"path": "src"}));
        assert!(plain.contains("a.py"), "{tool}: {plain}");
        assert_eq!(r.ok(tool, json!({"path": abs})), plain, "{tool}");
        assert!(r.err(tool, json!({"path": "nope"})).contains("no such file or directory"), "{tool}");
    }
}

#[cfg(unix)]
#[test]
fn grep_code_takes_a_path_whose_name_holds_a_backslash() {
    let mut r = Repo::new("backslash_dir", &[("x\\ty/f.txt", "needle\n"), ("other.txt", "needle\n")]);
    let text = r.ok("grep_code", json!({"pattern": "needle", "path": "x\\ty"}));
    assert!(text.contains("f.txt") && !text.contains("other.txt"), "{text}");
}

/// A server started on a root spelled through a symbolic link (`--root <dir>/link`, or macOS's
/// `/tmp` for `/private/tmp`) takes paths spelled through it.
#[cfg(unix)]
#[test]
fn paths_through_the_root_as_the_server_was_given_it_name_its_files() {
    let base = std::env::temp_dir().join(format!("scrutate_mcp_tools_linkroot_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("real/src")).unwrap();
    fs::write(base.join("real/src/a.py"), "def used():\n    return 1\n\n\ndef unused():\n    return used()\n").unwrap();
    let link = base.join("link");
    std::os::unix::fs::symlink(base.join("real"), &link).unwrap();
    let s = session(&link, 0.0);
    let text = |tool: &str, args: Value| {
        let c = call_tool(&s, tool, &args);
        assert!(!c.is_error, "{tool} {args}: {}", c.text);
        c.text
    };
    let through = link.join("src").to_string_lossy().into_owned();
    for tool in ["repo_map", "dead_code"] {
        assert_eq!(text(tool, json!({"path": through})), text(tool, json!({"path": "src"})), "{tool}");
    }
    assert_eq!(text("read_code", json!({"path": format!("{through}/a.py")})), text("read_code", json!({"path": "src/a.py"})));
    assert_eq!(text("grep_code", json!({"pattern": "used", "path": through})), text("grep_code", json!({"pattern": "used", "path": "src"})));
    drop(s);
    let _ = fs::remove_dir_all(&base);
}

/// A concise page that would pass max_bytes is the detailed page, which says how to go on: no
/// item is cut and none is lost between pages.
#[test]
fn a_concise_page_too_long_for_max_bytes_still_says_how_to_go_on() {
    let files: Vec<(String, String)> = (0..400).map(|i| (format!("d{i}/a.py"), format!("x{i} = 1\n"))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let mut r = Repo::new("concise_page", &refs);
    let text = r.ok("find_files", json!({"query": "a.py", "limit": 400, "max_bytes": 1000, "response_format": "concise"}));
    assert!(text.len() <= 1000, "{}", text.len());
    assert!(!text.contains("this item is cut"), "{text}");
    assert!(text.contains("call again with offset="), "{text}");
}

/// A tool's path that leaves the repository and comes back into it names the repository's
/// directory, as the same path without the detour does.
#[test]
fn a_tool_path_that_leaves_the_repository_and_comes_back_names_its_directory() {
    let mut r = Repo::new("out_and_back", &[("pkg/a.py", "def used():\n    return 1\n\ndef dead_one():\n    return used()\n")]);
    let name = r.root.file_name().unwrap().to_string_lossy().into_owned();
    let detour = format!("../{name}/pkg");
    for tool in ["dead_code", "repo_map"] {
        let plain = r.ok(tool, json!({"path": "pkg"}));
        assert_eq!(r.ok(tool, json!({"path": detour})), plain, "{tool}");
    }
    let plain = r.ok("grep_code", json!({"pattern": "return", "path": "pkg"}));
    assert_eq!(r.ok("grep_code", json!({"pattern": "return", "path": detour})), plain);
}
