//! Section-12 reliability tests: hostile inputs, filesystem oddities, index corruption, concurrent
//! writers and incremental updates. Every test builds a throw-away repository under a temp dir.
//! Nothing here may panic; wrong answers fail assertions.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[path = "support/tag.rs"]
mod tag;

use scrutate::index::Index;
use scrutate::instant::{self, Files, Mode, Output, Pattern, Query, Record};
use scrutate::parse::parse_source;

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_rob_{}_{}", name, std::process::id()));
        tag::claim(&root);
        let _ = fs::remove_dir_all(&root);
        // a repository: no index in a directory above takes it over
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(&root).unwrap();
        Repo { root }
    }
    fn write(&self, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        p
    }
    fn index(&self) -> Index {
        // no semantic model: these tests are about robustness, not ranking
        Index::open(&self.root, scrutate::models::ModelSet::none()).expect("open index")
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn query(pattern: &str, mode: Mode) -> Query {
    Query { pattern: Pattern { text: pattern.to_string(), mode, ..Pattern::default() }, ..Query::default() }
}

fn grep(root: &Path, pattern: &str, mode: Mode) -> (Vec<instant::Line>, instant::Stats) {
    let mut hits = Vec::new();
    let mut sink = |r: Record| {
        if let Record::Match(h) = r {
            hits.push(h)
        }
    };
    let st = instant::search(&query(pattern, mode), &Files::walk(vec![root.to_path_buf()]), Arc::default(), Output::Records(&mut sink)).expect("search");
    (hits, st)
}

fn count_chunks(ix: &Index) -> i64 {
    ix.stats().unwrap()["chunks"].as_i64().unwrap()
}

// ------------------------------------------------------------------------- encodings and text shapes

#[test]
fn hostile_text_shapes_never_panic() {
    let r = Repo::new("shapes");
    // invalid UTF-8 inside a Python file (lossy-decoded, still indexed)
    r.write("bad_utf8.py", b"def ok():\n    return '\xff\xfe\xfa'\n");
    // UTF-8 BOM, CRLF, CR-only, no trailing newline, empty, whitespace-only
    r.write("bom.py", "\u{feff}def bom_func():\n    pass\n".as_bytes());
    r.write("crlf.py", b"def crlf_func():\r\n    return 1\r\n");
    r.write("cr.py", b"def cr_func():\r    return 1\r");
    r.write("nonl.py", b"def nonl_func():\n    return 1");
    r.write("empty.py", b"");
    r.write("blank.py", b"\n\n   \n\t\n");
    // UTF-16 with BOM (binary-looking because of NUL bytes: must be skipped, not crash)
    let mut u16le = vec![0xff, 0xfe];
    for u in "def utf16():\n    pass\n".encode_utf16() {
        u16le.extend_from_slice(&u.to_le_bytes());
    }
    r.write("utf16.py", &u16le);
    // multibyte characters everywhere, including right at the 4000-byte sniff boundary
    let mut m = String::from("# ");
    while m.len() < 3999 {
        m.push('н');
    }
    m.push_str("\n@interface Foo\n@end\n");
    r.write("boundary.m", m.as_bytes());
    r.write("emoji.py", "def 😀smile():\n    return '🌍'\n\nclass Ωmega:\n    def ñ(self): pass\n".as_bytes());
    // one very long line (5 MB) and many short lines
    let long = format!("x = \"{}\"\n", "a".repeat(5_000_000));
    r.write("longline.py", long.as_bytes()); // > MAX_FILE_BYTES: skipped by Engine B, searched by Engine A
    let short: String = (0..200_000).map(|i| format!("v{i} = {i}\n")).collect();
    r.write("manylines.py", short.as_bytes()); // ~2.5 MB: also skipped by Engine B
                                               // deep nesting in several grammars
    r.write("deep.py", format!("x = {}1{}\n", "[".repeat(5000), "]".repeat(5000)).as_bytes());
    r.write("deep.json", format!("{}1{}", "[".repeat(20000), "]".repeat(20000)).as_bytes());
    r.write("deep.js", format!("let x = {}1{};\n", "(".repeat(5000), ")".repeat(5000)).as_bytes());
    // NUL byte early: binary
    r.write("binary.py", b"def nope():\n\x00\x00\x00 binary\n");
    // NUL byte late (after the 4 KB sniff window): must still not break anything
    let mut late = b"def late_nul():\n    pass\n".to_vec();
    late.extend(std::iter::repeat_n(b'#', 5000));
    late.push(0);
    r.write("late_nul.py", &late);
    // extension case and no extension
    r.write("UPPER.PY", b"def upper_case_ext():\n    pass\n");
    r.write("Makefile", b"build:\n\tcargo build\n");
    r.write("noext", b"plain words here\n");
    r.write(".hidden.py", b"def hidden_dot_file():\n    pass\n");
    // long file name (255 bytes) and spaces / unicode in names
    r.write(&format!("{}.py", "n".repeat(251)), b"def long_name_file():\n    pass\n");
    r.write("dir with spaces/file ñame.py", b"def spaced():\n    pass\n");

    let mut ix = r.index();
    let st = ix.update(true).expect("update");
    assert!(st.files_total >= 20, "{st:?}");
    let s = ix.stats().unwrap();
    assert!(s["files_skipped"].as_i64().unwrap() >= 3, "utf16, binary and oversize files are skipped: {s}");

    // every well-formed definition is found
    let names = symbol_names(&ix);
    for want in ["ok", "bom_func", "crlf_func", "nonl_func", "upper_case_ext", "hidden_dot_file", "long_name_file", "spaced", "late_nul", "Ωmega"] {
        assert!(names.iter().any(|n| n == want), "missing symbol {want}; have {names:?}");
    }
    assert!(!names.iter().any(|n| n == "nope"), "binary file must not be indexed");

    // Engine A: searches the same tree without the index, including the oversize files
    let (hits, st) = grep(&r.root, "def ", Mode::Literal);
    assert!(st.errors == 0, "{st:?}");
    assert!(hits.iter().any(|h| h.path.ends_with("longline.py")) || !hits.is_empty());
    let (hits, _) = grep(&r.root, "v199999 = 199999", Mode::Literal);
    assert_eq!(hits.len(), 1, "manylines.py is searchable by Engine A");
    let (hits, _) = grep(&r.root, "aaaaaaaaaa", Mode::Literal);
    assert!(hits.iter().any(|h| h.path.ends_with("longline.py")), "5 MB line is searchable");
    let (hits, _) = grep(&r.root, "😀smile", Mode::Literal);
    assert_eq!(hits.len(), 1);
    let (hits, _) = grep(&r.root, "crlf_func", Mode::Literal);
    assert_eq!(hits.len(), 1);
    assert!(!hits[0].text.ends_with('\r'), "CR stripped from reported line text: {:?}", hits[0].text);
}

#[test]
fn parser_fuzz_smoke() {
    // random byte soup and random token soup through every tier of the parser
    let mut seed = 0x9E3779B97F4A7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let exts = ["py", "js", "ts", "go", "rs", "java", "c", "cpp", "rb", "php", "sh", "kt", "swift", "lua", "hs", "json", "yaml", "toml", "md", "zig", "ex"];
    let toks = [
        "def ",
        "fn ",
        "class ",
        "{",
        "}",
        "(",
        ")",
        "[",
        "]",
        "\n",
        "    ",
        "\"",
        "'",
        "x",
        "y1",
        "=",
        "->",
        "::",
        ".",
        "#",
        "//",
        "/*",
        "*/",
        "\u{1F600}",
        "é",
        "\t",
        "\\",
        "@",
        "import ",
        "<",
        ">",
    ];
    for i in 0..400 {
        let ext = exts[(next() % exts.len() as u64) as usize];
        let n = (next() % 400) as usize;
        let src: String = if i % 2 == 0 {
            (0..n).map(|_| toks[(next() % toks.len() as u64) as usize]).collect()
        } else {
            let bytes: Vec<u8> = (0..n).map(|_| (next() & 0xff) as u8).collect();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let chunks = parse_source(&format!("f{i}.{ext}"), &src);
        for c in &chunks {
            assert!(c.start >= 1 && c.end >= c.start, "bad range in {ext}: {c:?}");
        }
    }
}

// ------------------------------------------------------------------------- filesystem oddities

#[cfg(unix)]
#[test]
fn symlinks_ignores_and_nested_repos() {
    use std::os::unix::fs::symlink;
    let r = Repo::new("fs");
    r.write("src/a.py", b"def real_a():\n    pass\n");
    r.write("src/ignored.py", b"def should_be_ignored():\n    pass\n");
    r.write(".gitignore", b"src/ignored.py\nbuild/\n");
    r.write("build/gen.py", b"def generated():\n    pass\n");
    r.write("sub/.gitignore", b"local.py\n");
    r.write("sub/local.py", b"def nested_ignore():\n    pass\n");
    r.write("sub/kept.py", b"def nested_kept():\n    pass\n");
    // a nested repository (submodule-like: a `.git` *file*) is walked like any directory
    r.write("vendor/mod/.git", b"gitdir: ../../.git/modules/mod\n");
    r.write("vendor/mod/lib.py", b"def submodule_symbol():\n    pass\n");
    // symlink loop, dangling symlink, symlink escaping the root, symlink to a file
    symlink(&r.root, r.root.join("loop")).unwrap();
    symlink("/nonexistent/target", r.root.join("dangling.py")).unwrap();
    let outside = std::env::temp_dir().join(format!("scrutate_outside_{}", std::process::id()));
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.py"), b"def outside_secret():\n    pass\n").unwrap();
    symlink(&outside, r.root.join("escape")).unwrap();
    symlink(r.root.join("src/a.py"), r.root.join("alias.py")).unwrap();
    // .gitignore rules apply inside a git repository; a `.git` directory is all the walker checks,
    // so the test does not depend on a git binary being installed
    fs::create_dir_all(r.root.join(".git")).unwrap();

    let mut ix = r.index();
    ix.update(true).unwrap();
    let names = symbol_names(&ix);
    assert!(names.iter().any(|n| n == "real_a"));
    assert!(names.iter().any(|n| n == "nested_kept"));
    assert!(names.iter().any(|n| n == "submodule_symbol"));
    for gone in ["should_be_ignored", "generated", "nested_ignore", "outside_secret"] {
        assert!(!names.iter().any(|n| n == gone), "{gone} must not be indexed; have {names:?}");
    }
    // Engine A: same rules, symlinks not followed by default, followed on request (loop-safe)
    let (hits, st) = grep(&r.root, "def ", Mode::Literal);
    assert_eq!(st.errors, 0);
    assert!(!hits.iter().any(|h| h.path.contains("escape")));
    assert!(!hits.iter().any(|h| h.path.contains("ignored")));
    let f = Files { follow_links: true, ..Files::walk(vec![r.root.clone()]) };
    let mut n = 0;
    let mut sink = |h: Record| {
        if let Record::Match(_) = h {
            n += 1
        }
    };
    let st = instant::search(&query("outside_secret", Mode::Literal), &f, Arc::default(), Output::Records(&mut sink)).unwrap();
    assert!(!st.cancelled && n == 1, "following links reaches the escape target exactly once (loop detected): n={n}");
    let _ = fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn unreadable_files_are_reported_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc_geteuid() } == 0 {
        eprintln!("running as root: permission test skipped");
        return;
    }
    let r = Repo::new("perm");
    r.write("ok.py", b"def readable():\n    pass\n");
    let p = r.write("secret.py", b"def unreadable():\n    pass\n");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    let mut ix = r.index();
    let st = ix.update(true).unwrap();
    assert!(st.files_total >= 1);
    let (_, st) = grep(&r.root, "def ", Mode::Literal);
    assert_eq!(st.errors, 1, "the unreadable file is counted as an error");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
}

#[cfg(unix)]
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}

// ------------------------------------------------------------------------- incremental updates

#[test]
fn incremental_updates_track_edits_renames_and_deletes() {
    let r = Repo::new("incr");
    r.write("a.py", b"def alpha():\n    pass\n");
    r.write("b.py", b"def beta():\n    pass\n");
    let mut ix = r.index();
    ix.update(true).unwrap();
    let n0 = count_chunks(&ix);
    // no change: nothing re-indexed
    let st = ix.update(false).unwrap();
    assert_eq!(st.files_changed, 0);
    // touch without content change: nothing re-indexed (content hash)
    let t = filetime_now_plus(5);
    set_mtime(&r.root.join("a.py"), t);
    let st = ix.update(false).unwrap();
    assert_eq!(st.files_changed, 0, "mtime-only change must not re-index");
    // edit: one file re-indexed, old chunks gone
    r.write("a.py", b"def alpha2():\n    pass\n\ndef alpha3():\n    pass\n");
    set_mtime(&r.root.join("a.py"), filetime_now_plus(10));
    let st = ix.update(false).unwrap();
    assert_eq!(st.files_changed, 1);
    let names = symbol_names(&ix);
    assert!(names.contains(&"alpha2".to_string()) && names.contains(&"alpha3".to_string()) && !names.contains(&"alpha".to_string()), "{names:?}");
    // rename: old path removed, new path added
    fs::rename(r.root.join("b.py"), r.root.join("c.py")).unwrap();
    let st = ix.update(false).unwrap();
    assert_eq!(st.files_removed, 1);
    assert_eq!(st.files_changed, 1);
    let paths = ix.files().unwrap();
    assert!(paths.contains(&"c.py".to_string()) && !paths.contains(&"b.py".to_string()), "{paths:?}");
    // delete: chunks removed
    fs::remove_file(r.root.join("c.py")).unwrap();
    let st = ix.update(false).unwrap();
    assert_eq!(st.files_removed, 1);
    let paths = ix.files().unwrap();
    assert_eq!(paths, vec!["a.py".to_string()]);
    assert!(count_chunks(&ix) <= n0);
    // the live segment statistics agree with the database after all that
    let lex_n = ix.lex().unwrap().n_alive;
    assert_eq!(lex_n as i64, count_chunks(&ix));
}

fn symbol_names(ix: &Index) -> Vec<String> {
    ix.files().unwrap().iter().flat_map(|p| ix.outline(p).unwrap()).map(|r| r.name).collect()
}

fn filetime_now_plus(secs: u64) -> std::time::SystemTime {
    std::time::SystemTime::now() + std::time::Duration::from_secs(secs)
}

fn set_mtime(p: &Path, t: std::time::SystemTime) {
    let f = fs::OpenOptions::new().write(true).open(p).unwrap();
    f.set_modified(t).unwrap();
}

// ------------------------------------------------------------------------- corruption and recovery

#[test]
fn corrupted_index_is_detected_and_rebuilt() {
    let r = Repo::new("corrupt");
    for i in 0..30 {
        r.write(&format!("m{i}.py"), format!("def func_{i}():\n    return {i}\n").as_bytes());
    }
    {
        let mut ix = r.index();
        ix.update(true).unwrap();
        assert_eq!(symbol_names(&ix).len(), 30);
    }
    let dir = r.root.join(".scrutate");
    // 1. a segment file truncated to half
    let seg = fs::read_dir(dir.join("seg")).unwrap().next().unwrap().unwrap().path();
    let ids = seg.join("ids.npy");
    let bytes = fs::read(&ids).unwrap();
    fs::write(&ids, &bytes[..bytes.len() / 2]).unwrap();
    {
        let names = recover_and_names(&r.root);
        assert_eq!(names.len(), 30, "rebuilt after truncated segment");
    }
    // 2. a segment directory deleted entirely
    fs::remove_dir_all(&seg).ok();
    {
        let names = recover_and_names(&r.root);
        assert_eq!(names.len(), 30, "rebuilt after missing segment");
    }
    // 3. garbage written over the database header
    let db = dir.join("index.db");
    let mut bytes = fs::read(&db).unwrap();
    for b in bytes.iter_mut().take(200) {
        *b = 0x55;
    }
    fs::write(&db, &bytes).unwrap();
    let _ = fs::remove_file(dir.join("index.db-wal"));
    let _ = fs::remove_file(dir.join("index.db-shm"));
    {
        let names = recover_and_names(&r.root);
        assert_eq!(names.len(), 30, "rebuilt after corrupted database header");
    }
    // 4. database replaced by a non-database file
    fs::write(&db, b"this is not a database at all").unwrap();
    let _ = fs::remove_file(dir.join("index.db-wal"));
    {
        let names = recover_and_names(&r.root);
        assert_eq!(names.len(), 30, "rebuilt after bogus database file");
    }
    // 5. a corrupted vector/posting file with a valid header but garbage content
    let seg = fs::read_dir(dir.join("seg")).unwrap().next().unwrap().unwrap().path();
    let ptr0 = seg.join("ptr0.npy");
    let mut bytes = fs::read(&ptr0).unwrap();
    let n = bytes.len();
    for b in bytes.iter_mut().skip(n.saturating_sub(64)) {
        *b = 0xff;
    }
    fs::write(&ptr0, &bytes).unwrap();
    {
        let names = recover_and_names(&r.root);
        assert_eq!(names.len(), 30, "rebuilt or survived garbage postings");
    }
}

/// Open the repository as a front end does and read the segments (which validates them): damage
/// either meets is rebuilt from the sources.
fn recover_and_names(root: &Path) -> Vec<String> {
    let mut repo = scrutate::Repo::open(root).unwrap();
    repo.update(false).unwrap();
    let n = repo.recovering(|s| Ok(s.ix.lex()?.n_alive)).expect("store opens after recovery");
    assert!(n > 0);
    symbol_names(&repo.ix)
}

// ------------------------------------------------------------------------- concurrency

#[test]
fn concurrent_indexers_and_readers_do_not_corrupt() {
    let r = Repo::new("concurrent");
    for i in 0..40 {
        r.write(&format!("pkg/m{i}.py"), format!("def func_{i}(x):\n    return x + {i}\n").as_bytes());
    }
    let root = r.root.clone();
    let writers: Vec<_> = (0..3)
        .map(|w| {
            let root = root.clone();
            std::thread::spawn(move || {
                let mut ix = Index::open(&root, scrutate::models::ModelSet::none()).unwrap();
                for round in 0..3 {
                    fs::write(root.join(format!("pkg/w{w}.py")), format!("def writer_{w}_{round}():\n    pass\n")).unwrap();
                    ix.update(false).unwrap();
                }
            })
        })
        .collect();
    let readers: Vec<_> = (0..2)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || {
                let mut ix = Index::open(&root, scrutate::models::ModelSet::none()).unwrap();
                for _ in 0..20 {
                    let _ = ix.refresh(0.0).unwrap();
                    let n = ix.lex().unwrap().n_alive as i64;
                    assert!(n >= 0);
                }
            })
        })
        .collect();
    for t in writers.into_iter().chain(readers) {
        t.join().expect("thread");
    }
    let mut ix = r.index();
    ix.update(false).unwrap();
    let names = symbol_names(&ix);
    for w in 0..3 {
        assert!(names.contains(&format!("writer_{w}_2")), "final content of writer {w}: {names:?}");
        assert!(!names.contains(&format!("writer_{w}_0")), "stale chunk survived: {names:?}");
    }
    assert_eq!(ix.lex().unwrap().n_alive as i64, count_chunks(&ix));
    assert!(ix.check(false).unwrap().database_ok);
}

// ------------------------------------------------------------------------- Engine A resource limits

#[test]
fn regex_limits_and_errors_are_clean() {
    let r = Repo::new("regex");
    r.write("a.txt", format!("{}\n", "a".repeat(10_000)).as_bytes());
    // pathological patterns are linear here (no backtracking); must finish quickly
    let t = std::time::Instant::now();
    let (hits, st) = grep(&r.root, "(a*)*b", Mode::Regex);
    assert!(hits.is_empty() && st.errors == 0 && t.elapsed().as_secs() < 5);
    let (hits, _) = grep(&r.root, "(a|aa)+$", Mode::Regex);
    assert_eq!(hits.len(), 1);
    // an oversized regex is rejected with an error, not a crash or a hang
    let f = Files::walk(vec![r.root.clone()]);
    let mut sink = |_h: Record| {};
    let res = instant::search(&query("(a{1000}){1000}", Mode::Regex), &f, Arc::default(), Output::Records(&mut sink));
    assert!(res.is_err(), "regex over the size limit must be an error");
    // invalid regex: error
    let res = instant::search(&query("(unclosed", Mode::Regex), &f, Arc::default(), Output::Records(&mut sink));
    assert!(res.is_err());
    // fuzzy limits
    assert!(instant::FuzzyMatcher::new("", 1, false).is_err());
    assert!(instant::FuzzyMatcher::new("ab", 2, false).is_err());
    assert!(instant::FuzzyMatcher::new(&"x".repeat(instant::FUZZY_MAX + 1), 1, false).is_err());
    assert!(instant::FuzzyMatcher::new(&"x".repeat(instant::FUZZY_MAX), 1, false).is_ok());
    // cancellation flag stops a search
    let big: String = (0..300_000).map(|i| format!("line {i} needle\n")).collect();
    r.write("big.txt", big.as_bytes());
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = Arc::clone(&cancel);
    let mut n = 0u64;
    let mut sink = |_h: Record| {
        n += 1;
        if n == 10 {
            c2.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    };
    let st = instant::search(&query("needle", Mode::Literal), &f, cancel, Output::Records(&mut sink)).unwrap();
    assert!(st.cancelled);
    assert!(n < 300_000, "stopped early after cancellation: {n}");
    // a search that stops at max_results stops itself: the caller's flag, shared with other work
    // of one call, stays as the caller left it
    let cancel = Arc::new(AtomicBool::new(false));
    for first_in_path_order in [false, true] {
        let q = Query { max_results: Some(5), first_in_path_order, ..query("needle", Mode::Literal) };
        let st = instant::search(&q, &f, Arc::clone(&cancel), Output::Records(&mut |_| {})).unwrap();
        assert!(st.cancelled && st.matches == 5, "{st:?}");
        assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed), "first_in_path_order={first_in_path_order}");
    }
}
