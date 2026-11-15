//! The text index across updates that go wrong or come often: a manifest cut short, a liveness
//! file left by a failed update, and many small updates beside one large segment.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/tag.rs"]
mod tag;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("scrutate_text_index_{}_{}", name, std::process::id()));
        tag::claim(&p);
        let _ = fs::remove_dir_all(&p);
        // a repository: no index in a directory above takes it over
        fs::create_dir_all(p.join(".git")).unwrap();
        fs::create_dir_all(&p).unwrap();
        Tmp(canon(&p))
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// (exit code, stdout, stderr)
fn run(root: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN).arg("--root").arg(root).args(args).stdin(Stdio::null()).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn segments(root: &Path) -> Vec<String> {
    let m: serde_json::Value = serde_json::from_slice(&fs::read(root.join(".scrutate/text/manifest.json")).unwrap()).unwrap();
    m["segments"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect()
}

/// The lines `grep --index -c` prints, sorted.
fn counts(root: &Path, pattern: &str) -> Vec<String> {
    let (code, out, err) = run(root, &["grep", "--index", "-c", pattern]);
    assert!(code == 0 || code == 1, "{err}");
    let mut lines: Vec<String> = out.lines().map(str::to_string).collect();
    lines.sort();
    lines
}

#[test]
fn a_manifest_cut_short_is_rebuilt_by_the_next_update() {
    let t = Tmp::new("manifest");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    let manifest = t.0.join(".scrutate/text/manifest.json");
    for cut in ["", "{\"format\":"] {
        fs::write(&manifest, cut).unwrap();
        // a search cannot write the index: it says what to run
        let (code, _, err) = run(&t.0, &["grep", "--index", "alpha"]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("index --text"), "{err}");
        let (code, _, err) = run(&t.0, &["index", "--text"]);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("damaged") && err.contains("rebuilding"), "{err}");
        assert_eq!(counts(&t.0, "alpha"), ["a.py:1"]);
    }
}

#[test]
fn a_liveness_file_left_by_a_failed_update_is_written_again() {
    let t = Tmp::new("live");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return alpha()\n").unwrap();
    // enough other documents that one dead one starts no merge
    for i in 0..10 {
        fs::write(t.0.join(format!("other{i}.txt")), format!("nothing {i}\n")).unwrap();
    }
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    let seg = t.0.join(".scrutate/text").join(&segments(&t.0)[0]);
    // what an update that stopped before its manifest leaves: the next generation's liveness
    // file, with every document of the segment still live
    fs::copy(seg.join("live-00000001.bin"), seg.join("live-00000002.bin")).unwrap();
    fs::write(t.0.join("a.py"), "def alpha():\n    return 22\n").unwrap();
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    assert_eq!(segments(&t.0).len(), 2);
    assert_eq!(counts(&t.0, "alpha"), ["a.py:1", "b.py:1"]);
    let (_, out, _) = run(&t.0, &["grep", "--index", "return"]);
    let mut lines: Vec<&str> = out.lines().collect();
    lines.sort();
    assert_eq!(lines, ["a.py:2:    return 22", "b.py:2:    return alpha()"]);
}

#[test]
fn small_updates_do_not_write_the_large_segment_again() {
    let t = Tmp::new("merge");
    for i in 0..60 {
        fs::write(t.0.join(format!("f{i:02}.py")), format!("def fn_{i}():\n    return {i}\n")).unwrap();
    }
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    let first = segments(&t.0);
    assert_eq!(first.len(), 1);
    for round in 0..9 {
        fs::write(t.0.join(format!("f{round:02}.py")), format!("def fn_{round}():\n    return {round} + {round}\n# round {round}\n")).unwrap();
        assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
        let now = segments(&t.0);
        assert!(now.contains(&first[0]), "round {round}: the segment of the first build is gone: {now:?}");
        assert!(now.len() <= 5, "round {round}: {now:?}");
    }
    // every file is found once, the changed ones in their new text
    assert_eq!(counts(&t.0, "def fn_").len(), 60);
    assert_eq!(counts(&t.0, "# round").len(), 9);
    // when most documents of the large segment are dead, everything is merged into one
    for i in 0..40 {
        fs::write(t.0.join(format!("f{i:02}.py")), format!("def fn_{i}():\n    return 0\n# again {i}\n")).unwrap();
    }
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    let now = segments(&t.0);
    assert!(!now.contains(&first[0]) && now.len() == 1, "{now:?}");
    assert_eq!(counts(&t.0, "def fn_").len(), 60);
    assert_eq!(counts(&t.0, "# again").len(), 40);
}

/// Overwrite the element `back` places before the end of a little-endian `.npy` array of 4- or
/// 8-byte elements.
fn patch_npy(path: &Path, back: usize, value: i64) {
    let mut b = fs::read(path).unwrap();
    let len_size = if b[6] == 1 { 2 } else { 4 };
    let hlen = b[8..8 + len_size].iter().rev().fold(0usize, |a, &x| a * 256 + x as usize);
    let data = 8 + len_size + hlen;
    let header = String::from_utf8_lossy(&b[..data]).into_owned();
    let w = if header.contains("<i8") || header.contains("<u8") { 8 } else { 4 };
    let i = (b.len() - data) / w - back;
    b[data + i * w..data + (i + 1) * w].copy_from_slice(&value.to_le_bytes()[..w]);
    fs::write(path, b).unwrap();
}

#[test]
fn a_posting_pointer_past_the_data_is_rebuilt_by_the_merge_of_the_next_update() {
    let t = Tmp::new("bad_ptr");
    fs::write(t.0.join("a.txt"), "hello world\n").unwrap();
    fs::write(t.0.join("b.txt"), "other words\n").unwrap();
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    let seg = t.0.join(".scrutate/text").join(&segments(&t.0)[0]);
    let post = fs::metadata(seg.join("post.bin")).unwrap().len();
    // the start of the last posting list past the end of the data
    patch_npy(&seg.join("ptr.npy"), 2, post as i64 + 1000);
    // one of three documents dead: the update merges the segments
    fs::write(t.0.join("a.txt"), "hello again\n").unwrap();
    let (code, _, err) = run(&t.0, &["index", "--text"]);
    assert!(!err.contains("internal error"), "{err}");
    assert_eq!(code, 0, "{err}");
    assert_eq!(counts(&t.0, "hello"), ["a.txt:1"]);
    assert_eq!(counts(&t.0, "words"), ["b.txt:1"]);
}

// the write moves the change time, which no one can set back
#[test]
fn an_edit_that_keeps_size_and_mtime_is_seen() {
    let t = Tmp::new("samestat");
    let f = t.0.join("a.txt");
    fs::write(&f, "hello world\n").unwrap();
    let mtime = fs::metadata(&f).unwrap().modified().unwrap();
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    fs::write(&f, "jello world\n").unwrap();
    fs::File::options().write(true).open(&f).unwrap().set_modified(mtime).unwrap();
    let (code, out, err) = run(&t.0, &["grep", "--index", "--update", "jello"]);
    assert_eq!((code, out.as_str()), (0, "a.txt:1:jello world\n"), "{err}");
}

fn format(root: &Path) -> u64 {
    let m: serde_json::Value = serde_json::from_slice(&fs::read(root.join(".scrutate/text/manifest.json")).unwrap()).unwrap();
    m["format"].as_u64().unwrap()
}

fn set_format(root: &Path, format: u64) {
    let p = root.join(".scrutate/text/manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    m["format"] = format.into();
    fs::write(&p, serde_json::to_vec(&m).unwrap()).unwrap();
}

#[test]
fn an_index_of_the_previous_format_is_rebuilt_by_the_next_update_and_a_newer_one_is_refused() {
    let t = Tmp::new("format");
    fs::write(t.0.join("a.txt"), "hello world\n").unwrap();
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    let now = format(&t.0);
    set_format(&t.0, now - 1);
    // a search cannot write the index: it says what to run
    let (code, _, err) = run(&t.0, &["grep", "--index", "hello"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("index --text"), "{err}");
    let (code, out, err) = run(&t.0, &["index", "--text"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("1 changed"), "{out}{err}");
    assert_eq!(counts(&t.0, "hello"), ["a.txt:1"]);
    set_format(&t.0, now - 1);
    let (code, out, err) = run(&t.0, &["grep", "--index", "--update", "hello"]);
    assert_eq!((code, out.as_str()), (0, "a.txt:1:hello world\n"), "{err}");
    set_format(&t.0, now + 1);
    for args in [&["grep", "--index", "hello"][..], &["index", "--text"]] {
        let (code, _, err) = run(&t.0, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains("newer scrutate"), "{args:?}: {err}");
    }
}

/// `batch` with the lines of `ops`, as an MCP client asks; the query file lies outside the tree.
fn batch(root: &Path, name: &str, ops: &str) -> (i32, String, String) {
    let q = std::env::temp_dir().join(format!("scrutate_text_index_q_{name}_{}.jsonl", std::process::id()));
    fs::write(&q, ops).unwrap();
    let r = run(root, &["batch", q.to_str().unwrap()]);
    let _ = fs::remove_file(&q);
    r
}

#[test]
fn a_reindex_brings_the_text_index_up_to_date_too() {
    let t = Tmp::new("reindex");
    fs::write(t.0.join("a.py"), "def one():\n    return 1\n").unwrap();
    assert_eq!(run(&t.0, &["index"]).0, 0);
    fs::write(t.0.join("b.py"), "zzqtoken = 1\n").unwrap();
    let (code, out, err) = batch(&t.0, "reindex", "{\"op\": \"reindex\"}\n");
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = run(&t.0, &["grep", "--index", "-n", "zzqtoken"]);
    assert_eq!((code, out.as_str()), (0, "b.py:1:zzqtoken = 1\n"), "{err}");
}

#[test]
fn a_reindex_keeps_no_text_index_where_there_is_none_and_rebuilds_a_damaged_one() {
    let t = Tmp::new("reindex_keep");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    assert_eq!(run(&t.0, &["index", "--no-text"]).0, 0);
    assert_eq!(batch(&t.0, "keep_none", "{\"op\": \"reindex\"}\n").0, 0);
    assert!(!t.0.join(".scrutate/text").exists(), "a reindex made a text index");
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    fs::write(t.0.join(".scrutate/text/manifest.json"), "").unwrap();
    let (code, _, err) = batch(&t.0, "keep_damaged", "{\"op\": \"reindex\"}\n");
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("text index damaged") && err.contains("rebuilding"), "{err}");
    assert_eq!(counts(&t.0, "alpha"), ["a.py:1"]);
}
