//! A tree's answers are the same bytes whatever the history of its index: built once, in steps,
//! with files rewritten and restored, or in batches in reverse. Ties between definitions that rank
//! equal are broken by where they are, never by the order their rows were written.

#[path = "support/canon.rs"]
mod canon;
#[path = "support/tag.rs"]
mod tag;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Tree(PathBuf);

impl Tree {
    fn new(name: &str) -> Tree {
        let root = std::env::temp_dir().join(format!("scrutate_index_history_{name}_{}", std::process::id()));
        tag::claim(&root);
        let _ = fs::remove_dir_all(&root);
        // a repository: no index in a directory above takes it over
        fs::create_dir_all(root.join(".git")).unwrap();
        Tree(canon::canon(&root))
    }
    fn write(&self, rel: &str, body: &str) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }
    fn index(&self) {
        let (code, _, err) = self.cli(&["index"]);
        assert_eq!(code, 0, "{err}");
    }
    fn cli(&self, args: &[&str]) -> (i32, String, String) {
        let out =
            Command::new(BIN).args(args).current_dir(&self.0).env("SCRUTATE_ROOT", ".").env("SCRUTATE_NO_SESSION", "1").stdin(Stdio::null()).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }
    /// A tool's text as `serve` answers it, from a session that finds the index current.
    fn tool(&self, session: &scrutate::session::Session, name: &str, args: Value, edits: bool) -> String {
        let settings = scrutate::mcp::Settings::new(None);
        let settings = if edits { settings.allowing_edits() } else { settings };
        scrutate::mcp::call_tool(session, name, &args, &settings, &Default::default()).text
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// File `n` of the same-name tree: `a0`, defined alike in every file, calls a function of its own.
fn same_name_file(n: usize) -> (String, String) {
    (format!("d/f{n:02}.py"), format!("def a0():\n    return b{n:02}()\n\n\ndef b{n:02}():\n    return {n}\n"))
}

/// The answers of `a0` that depend on which of its equal-ranked definitions come first.
fn a0_answers(t: &Tree) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cli = |label: &str, args: &[&str]| {
        let (code, stdout, stderr) = t.cli(args);
        out.push((label.to_string(), format!("exit {code}\n{stdout}{stderr}")));
    };
    cli("symbol --limit 5", &["symbol", "a0", "--limit", "5"]);
    cli("symbol --limit 20", &["symbol", "a0", "--limit", "20"]);
    cli("symbol --limit 100", &["symbol", "a0", "--limit", "100"]);
    cli("read --symbol", &["read", "--symbol", "a0"]);
    cli("similar", &["similar", "a0", "-k", "5"]);
    cli("callees", &["callees", "a0", "--limit", "50"]);
    let session = scrutate::session::Session::open(&t.0, scrutate::session::Every::given(Some(3600.0))).unwrap();
    let mut tool = |label: &str, name: &str, args: Value, edits: bool| out.push((label.to_string(), t.tool(&session, name, args, edits)));
    tool("find_symbol limit 5", "find_symbol", json!({"name": "a0", "limit": 5}), false);
    tool("find_symbol limit 20", "find_symbol", json!({"name": "a0", "limit": 20}), false);
    tool("read_code symbol", "read_code", json!({"symbol": "a0"}), false);
    tool("similar_code", "similar_code", json!({"target": "a0", "k": 5}), false);
    tool("callees", "callees", json!({"name": "a0"}), false);
    tool("edit_symbol names N definitions", "edit_symbol", json!({"symbol": "a0", "new_text": "def a0():\n    return 0\n"}), true);
    drop(session);
    // what the language server's name lookup reads (symbol_rows), and the qualified-name lookup
    let ix = scrutate::index::Index::open(&t.0, scrutate::models::ModelSet::from_env()).unwrap();
    let rows = |v: Vec<scrutate::index::ChunkRow>| v.iter().map(|r| format!("{}:{}", r.path, r.start)).collect::<Vec<_>>().join(" ");
    out.push(("symbols_named 50".into(), rows(ix.symbols_named("a0", 50).unwrap())));
    let named = ix.qualnames_named("a0", "a0").unwrap();
    let paths: Vec<String> = named.iter().map(|(id, _)| ix.chunk(*id).unwrap().map_or(String::new(), |r| format!("{}:{}", r.path, r.start))).collect();
    out.push(("qualnames_named".into(), paths.join(" ")));
    out
}

/// Reviewer 4's four histories of one tree of 40 files, each defining `a0` alike: built once; in
/// three steps; built once, then every third file rewritten and restored; in four batches in
/// reverse. Every answer about `a0` is the same bytes for all four.
#[test]
fn same_name_definitions_answer_alike_whatever_the_index_history() {
    let files: Vec<(String, String)> = (0..40).map(same_name_file).collect();
    let user = ("use.py", "from d.f00 import a0\n\n\ndef caller():\n    return a0()\n");
    let built = |name: &str, steps: &[(usize, usize)], rewrite: bool| {
        let t = Tree::new(name);
        t.write(user.0, user.1);
        for step in steps {
            for (rel, body) in &files[step.0..step.1] {
                t.write(rel, body);
            }
            t.index();
        }
        if rewrite {
            for (rel, body) in files.iter().step_by(3) {
                t.write(rel, &format!("{body}\n"));
            }
            t.index();
            for (rel, body) in files.iter().step_by(3) {
                t.write(rel, body);
            }
            t.index();
        }
        t
    };
    let trees = [
        ("built once", built("once", &[(0, 40)], false)),
        ("in three steps", built("steps", &[(0, 14), (14, 27), (27, 40)], false)),
        ("every third file rewritten", built("rewritten", &[(0, 40)], true)),
        ("in reverse batches", built("reverse", &[(30, 40), (20, 30), (10, 20), (0, 10)], false)),
    ];
    let answers: Vec<(&str, Vec<(String, String)>)> = trees.iter().map(|(how, t)| (*how, a0_answers(t))).collect();
    let mut wrong = Vec::new();
    for (how, a) in &answers[1..] {
        for ((label, want), (_, got)) in answers[0].1.iter().zip(a) {
            if want != got {
                wrong.push(format!("{label}, {how}:\n--- built once\n{want}\n--- {how}\n{got}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{} answers depend on the index's history:\n{}", wrong.len(), wrong.join("\n"));
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

const NAMES: [&str; 4] = ["alpha", "beta", "gamma", "delta"];

/// A generated file: same-name functions, classes and methods across files and languages, each
/// body calling another name of the pool, many of them alike so that rankings tie.
fn generated_file(rng: &mut Rng, i: usize) -> (String, String) {
    let js = i.is_multiple_of(3);
    let mut body = String::new();
    for _ in 0..1 + rng.below(3) {
        let (name, callee) = (NAMES[rng.below(4)], NAMES[rng.below(4)]);
        let class = rng.below(3) == 0;
        body += &match (js, class) {
            (false, false) => format!("def {name}():\n    return {callee}()\n\n\n"),
            (false, true) => format!("class Box{}:\n    def {name}(self):\n        return {callee}()\n\n\n", rng.below(2)),
            (true, false) => format!("function {name}() {{\n  return {callee}();\n}}\n\n"),
            (true, true) => format!("class Box{} {{\n  {name}() {{\n    return {callee}();\n  }}\n}}\n\n", rng.below(2)),
        };
    }
    (format!("{}/m{i:02}.{}", ["src", "lib", "app"][i % 3], if js { "js" } else { "py" }), body)
}

/// Every read answer of the generated tree, each with limits below its number of ties.
fn read_answers(t: &Tree, files: &[(String, String)]) -> Vec<(String, String)> {
    let mut asks: Vec<Vec<String>> = Vec::new();
    for name in NAMES {
        for args in [
            vec!["symbol", name, "--limit", "2"],
            vec!["symbol", name, "--limit", "50"],
            vec!["refs", name, "--max-results", "3"],
            vec!["refs", name],
            vec!["callers", name, "--limit", "2"],
            vec!["callers", name],
            vec!["callees", name, "--limit", "2"],
            vec!["callees", name],
            vec!["read", "--symbol", name],
            vec!["similar", name, "-k", "3"],
            vec!["search", name, "-k", "3"],
        ] {
            asks.push(args.into_iter().map(String::from).collect());
        }
    }
    asks.push(vec!["search".into(), "alpha beta".into(), "-k".into(), "4".into()]);
    asks.push(vec!["map".into()]);
    asks.push(vec!["dead".into()]);
    asks.push(vec!["dupes".into(), "--min-tokens".into(), "1".into()]);
    for (rel, _) in files.iter().take(6) {
        asks.push(vec!["outline".into(), rel.clone()]);
    }
    asks.iter()
        .map(|a| {
            let (code, out, err) = t.cli(&a.iter().map(String::as_str).collect::<Vec<_>>());
            (a.join(" "), format!("exit {code}\n{out}{err}"))
        })
        .collect()
}

/// The property behind the same-name case: for one generated tree, an index built once and one
/// built in random steps (files added in batches, rewritten with a newline more and back, removed
/// and added again, each step an update, so the segments are merged on the way) give every read
/// answer as the same bytes.
#[test]
fn every_read_answer_is_the_same_whatever_the_index_history() {
    let mut wrong = Vec::new();
    for seed in 0..3u64 {
        let mut rng = Rng(0x1d_e4_b1_57 + seed * 0x9e37_79b9);
        let files: Vec<(String, String)> = (0..30).map(|i| generated_file(&mut rng, i)).collect();
        let once = Tree::new(&format!("prop_once_{seed}"));
        for (rel, body) in &files {
            once.write(rel, body);
        }
        once.index();
        let steps = Tree::new(&format!("prop_steps_{seed}"));
        let mut order: Vec<usize> = (0..files.len()).collect();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i + 1));
        }
        for batch in order.chunks(1 + rng.below(8)) {
            for &i in batch {
                steps.write(&files[i].0, &files[i].1);
            }
            steps.index();
            let (a, b) = (rng.below(files.len()), rng.below(files.len()));
            if steps.0.join(&files[a].0).exists() {
                steps.write(&files[a].0, &format!("{}\n", files[a].1));
                steps.index();
                steps.write(&files[a].0, &files[a].1);
            }
            if steps.0.join(&files[b].0).exists() {
                fs::remove_file(steps.0.join(&files[b].0)).unwrap();
                steps.index();
                steps.write(&files[b].0, &files[b].1);
            }
            steps.index();
        }
        for ((ask, want), (_, got)) in read_answers(&once, &files).iter().zip(read_answers(&steps, &files)) {
            if *want != got {
                wrong.push(format!("seed {seed}, {ask}:\n--- built once\n{want}\n--- in steps\n{got}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{} answers depend on the index's history:\n{}", wrong.len(), wrong.join("\n"));
}

/// The shape of the kind-bound tests in limit_filters.rs, scaled down and turned round: the one
/// function `A0` sits in the path that sorts first and is indexed in the same run as 101 classes
/// `A0`, which rank before it. `symbol A0 --kind function` finds it, and every history of the
/// tree answers alike, so the property rests on neither insertion order nor a path's spelling.
#[test]
fn a_kind_finds_the_one_function_among_classes_of_its_name_whatever_the_history() {
    let classes: Vec<(String, String)> = (0..101).map(|i| (format!("m/c{i:03}.py"), format!("class A0:\n    n = {i}\n"))).collect();
    let function = ("a/first.py", "def A0():\n    return 1\n");
    let mut answers = Vec::new();
    for (name, reverse) in [("kind_once", false), ("kind_reverse", true)] {
        let t = Tree::new(name);
        if reverse {
            for (rel, body) in classes.iter().rev() {
                t.write(rel, body);
                if rel.ends_with("0.py") {
                    t.index();
                }
            }
            t.write(function.0, function.1);
        } else {
            t.write(function.0, function.1);
            for (rel, body) in &classes {
                t.write(rel, body);
            }
        }
        t.index();
        let asks: [&[&str]; 3] =
            [&["symbol", "A0", "--kind", "function"], &["symbol", "A0", "--kind", "function", "--limit", "1"], &["symbol", "A0", "--limit", "3"]];
        let got: Vec<String> = asks.iter().map(|a| t.cli(a).1).collect();
        assert!(got[0].contains("a/first.py") && got[1].contains("a/first.py"), "{name}: {got:?}");
        answers.push(got);
    }
    assert_eq!(answers[0], answers[1], "the answers depend on the index's history");
}

/// Reviewer 3's tie tree scaled down: 300 byte-identical definitions tie in every source. Each
/// source gives its own bound of candidates, not every tie, ordered by place; the pool and the
/// answers of `search` and `similar` are the same for two histories of the tree.
#[test]
fn a_cut_among_many_ties_keeps_its_size_and_its_order() {
    let n = 300;
    let body = "def shared_helper(value):\n    # the same helper in every copy\n    return value + 1\n";
    let rel = |i: usize| format!("vendor/v{i:03}/copy{i:03}.py");
    let mut seen = Vec::new();
    for (name, reverse) in [("ties_once", false), ("ties_steps", true)] {
        let t = Tree::new(name);
        let order: Vec<usize> = if reverse { (n / 2..n).chain(0..n / 2).collect() } else { (0..n).collect() };
        for (k, &i) in order.iter().enumerate() {
            t.write(&rel(i), body);
            if reverse && (k + 1) % (n / 2) == 0 {
                t.index();
            }
        }
        if reverse {
            for i in (0..n).step_by(3) {
                t.write(&rel(i), &format!("{body}\n"));
            }
            t.index();
            for i in (0..n).step_by(3) {
                t.write(&rel(i), body);
            }
        }
        t.index();
        let ix = scrutate::index::Index::open(&t.0, scrutate::models::ModelSet::from_env()).unwrap();
        let mut s = scrutate::search::Searcher::new(ix);
        let c = s.candidates("shared_helper value", false).unwrap();
        let bounds = [usize::MAX, scrutate::search::LEX_LIMIT, scrutate::search::SEM_LIMIT];
        for (list, bound) in c.found.iter().zip(bounds) {
            assert!(list.len() <= bound, "{name}: a source gave {} candidates past its bound {bound}", list.len());
        }
        assert!(c.found[1].len() == scrutate::search::LEX_LIMIT, "{name}: the lexical pool is {}", c.found[1].len());
        let place = |id: &i64| s.ix.chunk(*id).unwrap().map(|r| format!("{}:{}", r.path, r.start)).unwrap_or_default();
        let pools: Vec<Vec<String>> = c.found.iter().map(|l| l.iter().map(place).collect()).collect();
        let search = t.cli(&["search", "shared_helper", "-k", "8"]).1;
        let similar = t.cli(&["similar", &format!("{}:1", rel(0)), "-k", "8"]).1;
        seen.push((pools, search, similar));
    }
    assert_eq!(seen[0].0, seen[1].0, "the pools depend on the history");
    assert_eq!(seen[0].1, seen[1].1, "search depends on the history");
    assert_eq!(seen[0].2, seen[1].2, "similar depends on the history");
}
