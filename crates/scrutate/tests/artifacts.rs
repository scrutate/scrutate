//! The artifact store: content seen before, under any path, in this checkout or in the main
//! checkout of a linked git work tree, is not parsed again, and the index made from it equals a
//! fresh build of the same tree, row for row.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use scrutate::index::Index;
use scrutate::models::ModelSet;
use scrutate::search::Searcher;

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("scrutate_artifacts_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(fs::canonicalize(&p).unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn py(i: usize) -> String {
    format!("import os\n\nclass Holder{i}:\n    def run_{i}(self, x):\n        return helper_{i}(x) + {i}\n\n\ndef helper_{i}(x):\n    \"\"\"Add one to x.\"\"\"\n    return os.path.join(str(x), 'v{i}')\n")
}

fn rs(i: usize) -> String {
    format!("pub fn parse_{i}(s: &str) -> usize {{\n    tokens_{i}(s).len()\n}}\n\nfn tokens_{i}(s: &str) -> Vec<&str> {{\n    s.split(' ').collect()\n}}\n")
}

/// Every file's chunks, call names and imports through the index's questions, without ids,
/// each chunk's field scores for words of every field (path words too) and its vector, and the
/// hits of a few ranked searches: what must equal between two indexes of one tree. A hit's score
/// is left out: the duplicate features tell copies apart by id, and an updated index holds other
/// ids than a fresh one, as it always did.
fn dump(ix: Index) -> String {
    let mut out = String::new();
    let mut ix = ix;
    let report = ix.check(true).unwrap();
    assert!(report.ok(), "{:?}", report.problems);
    let mut files = ix.files().unwrap();
    files.sort();
    let words = scrutate::search::QueryInfo::parse("helper join path parse tokens holder run split collect add one src lib other copy tests m0 r2 renamed");
    let keys = scrutate::search::lex_keys(&words);
    for f in &files {
        for c in ix.outline(f).unwrap() {
            let calls = ix.callees_of(c.id).unwrap();
            out += &format!(
                "{f} {} {} {} {}-{} test={} doc={} body={} tok={} calls={calls:?}\n{}\n",
                c.kind, c.name, c.qualname, c.start, c.end, c.is_test, c.ndoc, c.nbody, c.name_tok, c.text
            );
            if c.kind != "constant" {
                let lex = ix.lex().unwrap();
                let scores = lex.field_scores(&keys, &[c.id]).unwrap();
                let vec = lex.vec_of(c.id).unwrap().map(|(a, b)| a.iter().chain(&b).map(|x| format!("{x:.5}")).collect::<Vec<_>>().join(","));
                out += &format!("  scores {:?} vector {vec:?}\n", scores[0]);
            }
        }
        out += &format!("{f} imports {:?}\n", ix.imports_of(f).unwrap());
    }
    let mut s = Searcher::new(ix);
    for q in ["helper join path", "parse tokens", "Holder run", "split collect", "add one"] {
        let mut hits: Vec<String> = s.search(q, 1000, false).unwrap().iter().map(|h| format!("{q}: {} {}\n", h.path, h.start)).collect();
        hits.sort();
        out += &hits.concat();
    }
    out
}

/// Two dumps are equal; else the lines only one of them has.
#[track_caller]
fn same(a: &str, b: &str) {
    if a != b {
        let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
        let only = |x: &[&str], y: &[&str]| x.iter().filter(|l| !y.contains(l)).take(8).map(|l| l.to_string()).collect::<Vec<_>>();
        panic!("dumps differ\nonly the first: {:#?}\nonly the second: {:#?}", only(&la, &lb), only(&lb, &la));
    }
}

/// A fresh index of a copy of `root`, dumped.
fn fresh(root: &Path, models: Arc<ModelSet>) -> String {
    let copy = PathBuf::from(format!("{}_fresh", root.display()));
    let _ = fs::remove_dir_all(&copy);
    copy_tree(root, &copy);
    let _ = fs::remove_dir_all(copy.join(".scrutate"));
    let _ = fs::remove_dir_all(copy.join(".git"));
    let mut ix = Index::open(&copy, models).unwrap();
    ix.update(false).unwrap();
    let d = dump(ix);
    fs::remove_dir_all(&copy).unwrap();
    d
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn open(root: &Path, models: &Arc<ModelSet>) -> Index {
    Index::open(root, Arc::clone(models)).unwrap()
}

#[test]
fn renamed_moved_and_copied_files_are_not_parsed_again_and_the_index_equals_a_fresh_build() {
    for models in [ModelSet::none(), ModelSet::from_env()] {
        let t = Tmp::new(if models.semantic_enabled() { "rename_model" } else { "rename" });
        for i in 0..6 {
            put(&t.0, &format!("src/m{i}.py"), &py(i));
            put(&t.0, &format!("lib/r{i}.rs"), &rs(i));
        }
        let mut ix = open(&t.0, &models);
        ix.update(false).unwrap();
        assert_eq!(ix.parsed, 12);
        // moved to another directory, renamed within one, copied, two contents swapped, and moved
        // into a test directory, which changes the flag; a file under another name is parsed, since
        // the module chunk and the windows take their name from it
        fs::create_dir_all(t.0.join("other")).unwrap();
        fs::rename(t.0.join("src/m0.py"), t.0.join("other/m0.py")).unwrap();
        fs::rename(t.0.join("src/m1.py"), t.0.join("src/renamed.py")).unwrap();
        put(&t.0, "copy/m2.py", &py(2));
        put(&t.0, "lib/r0.rs", &rs(1));
        put(&t.0, "lib/r1.rs", &rs(0));
        fs::create_dir_all(t.0.join("tests")).unwrap();
        fs::rename(t.0.join("lib/r2.rs"), t.0.join("tests/r2.rs")).unwrap();
        ix.update(false).unwrap();
        assert_eq!(ix.parsed, 15, "only the files under another name are parsed: renamed.py and the swapped r0.rs, r1.rs");
        same(&dump(ix), &fresh(&t.0, Arc::clone(&models)));
    }
}

#[test]
fn a_branch_switch_and_back_parses_only_what_the_store_never_held() {
    let t = Tmp::new("switch");
    let models = ModelSet::none();
    for i in 0..8 {
        put(&t.0, &format!("m{i}.py"), &py(i));
    }
    let mut ix = open(&t.0, &models);
    ix.update(false).unwrap();
    let main = dump(open(&t.0, &models));
    // the other branch: three files edited, one deleted, one added
    for i in 0..3 {
        put(&t.0, &format!("m{i}.py"), &py(100 + i));
    }
    fs::remove_file(t.0.join("m7.py")).unwrap();
    put(&t.0, "new.py", &py(200));
    ix.update(false).unwrap();
    assert_eq!(ix.parsed, 8 + 4);
    let branch = dump(open(&t.0, &models));
    same(&branch, &fresh(&t.0, Arc::clone(&models)));
    // back: the old contents come from the store
    for i in 0..3 {
        put(&t.0, &format!("m{i}.py"), &py(i));
    }
    put(&t.0, "m7.py", &py(7));
    fs::remove_file(t.0.join("new.py")).unwrap();
    ix.update(false).unwrap();
    assert_eq!(ix.parsed, 12, "the switch back parses nothing");
    same(&dump(open(&t.0, &models)), &main);
    // and over again
    for i in 0..3 {
        put(&t.0, &format!("m{i}.py"), &py(100 + i));
    }
    fs::remove_file(t.0.join("m7.py")).unwrap();
    put(&t.0, "new.py", &py(200));
    ix.update(false).unwrap();
    assert_eq!(ix.parsed, 12);
    same(&dump(ix), &branch);
}

#[test]
fn the_artifacts_no_file_names_are_bounded_and_the_named_ones_are_never_collected() {
    let t = Tmp::new("bounded");
    let models = ModelSet::none();
    for i in 0..10 {
        put(&t.0, &format!("m{i}.py"), &py(i));
    }
    let mut ix = open(&t.0, &models);
    ix.kept_min = 1;
    ix.update(false).unwrap();
    for round in 1..30 {
        for i in 0..10 {
            put(&t.0, &format!("m{i}.py"), &py(1000 * round + i));
        }
        ix.update(false).unwrap();
        let (kept, size) = ix.store_bytes().unwrap();
        assert!(kept <= size / 4, "round {round}: {kept} bytes kept of a database of {size}");
    }
    let (named, kept) = ix.store_counts().unwrap();
    assert_eq!(named, 10);
    assert!(kept > 0 && kept < 290, "{kept} kept");
    // the most recently dropped contents are the ones kept
    for i in 0..10 {
        put(&t.0, &format!("m{i}.py"), &py(1000 * 28 + i));
    }
    let before = ix.parsed;
    ix.update(false).unwrap();
    assert_eq!(ix.parsed, before, "the last contents dropped are still kept");
    same(&dump(ix), &fresh(&t.0, Arc::clone(&models)));
}

/// A main checkout at `root/main` and a linked work tree of it at `root/wt`, as `git worktree add`
/// leaves them, written by hand (the program reads git's files and runs no git).
fn linked(root: &Path) -> (PathBuf, PathBuf) {
    let (main, wt) = (root.join("main"), root.join("wt"));
    put(&main, ".git/HEAD", "ref: refs/heads/main\n");
    put(&main, ".git/worktrees/wt/HEAD", "ref: refs/heads/wt\n");
    put(&main, ".git/worktrees/wt/commondir", "../..\n");
    put(&main, ".git/worktrees/wt/gitdir", &format!("{}\n", wt.join(".git").display()));
    put(&wt, ".git", &format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()));
    for i in 0..6 {
        for d in [&main, &wt] {
            put(d, &format!("src/m{i}.py"), &py(i));
            put(d, &format!("lib/r{i}.rs"), &rs(i));
        }
    }
    (main, wt)
}

#[test]
fn a_linked_work_tree_starts_from_its_main_checkouts_index_and_equals_a_fresh_build() {
    for models in [ModelSet::none(), ModelSet::from_env()] {
        let t = Tmp::new(if models.semantic_enabled() { "linked_model" } else { "linked" });
        let (main, wt) = linked(&t.0);
        put(&wt, "src/only_here.py", &py(50));
        put(&wt, "lib/r0.rs", &rs(60));
        let mut m = open(&main, &models);
        m.update(false).unwrap();
        let mut w = open(&wt, &models);
        w.update(false).unwrap();
        assert_eq!(w.parsed, 2, "only the files that differ from the main checkout");
        same(&dump(w), &fresh(&wt, Arc::clone(&models)));
        // content the main checkout indexed later is found in its store, not parsed here
        put(&main, "src/later.py", &py(70));
        m.update(false).unwrap();
        put(&wt, "other/later.py", &py(70));
        let mut w = open(&wt, &models);
        w.update(false).unwrap();
        assert_eq!(w.parsed, 0);
        same(&dump(w), &fresh(&wt, Arc::clone(&models)));
        // nothing of the main checkout was written: its index is as its own update left it
        same(&dump(open(&main, &models)), &fresh(&main, Arc::clone(&models)));
    }
}

#[test]
fn a_segment_the_main_checkout_removed_while_it_was_copied_is_not_taken_into_a_work_tree() {
    let models = ModelSet::none();
    let t = Tmp::new("half_removed");
    let (main, wt) = linked(&t.0);
    let mut m = open(&main, &models);
    m.update(false).unwrap();
    drop(m);
    // what a copy sees of a segment the main checkout is removing after its commit: part of it
    let seg = fs::read_dir(main.join(".scrutate/seg")).unwrap().flatten().map(|e| e.path()).find(|p| p.join("ids.npy").exists()).unwrap();
    fs::remove_file(seg.join("ids.npy")).unwrap();
    let mut w = open(&wt, &models);
    w.update(false).unwrap();
    same(&dump(w), &fresh(&wt, Arc::clone(&models)));
    let name = seg.file_name().unwrap();
    let copied = wt.join(".scrutate/seg").join(name);
    assert!(!copied.exists() || copied.join("ids.npy").exists(), "a half segment was kept");
}

#[test]
fn a_link_that_is_not_confirmed_both_ways_or_a_linked_index_directory_is_not_followed() {
    let models = ModelSet::none();
    let t = Tmp::new("unconfirmed");
    let (main, wt) = linked(&t.0);
    let mut m = open(&main, &models);
    m.update(false).unwrap();
    let parsed = |wt: &Path| {
        let _ = fs::remove_dir_all(wt.join(".scrutate"));
        let mut w = open(wt, &models);
        w.update(false).unwrap();
        w.parsed
    };
    assert_eq!(parsed(&wt), 0);
    // git's record names another work tree
    put(&main, ".git/worktrees/wt/gitdir", &format!("{}\n", t.0.join("elsewhere/.git").display()));
    assert_eq!(parsed(&wt), 12);
    put(&main, ".git/worktrees/wt/gitdir", &format!("{}\n", wt.join(".git").display()));
    // a .git file that names a directory outside any worktrees directory
    put(&wt, ".git", &format!("gitdir: {}\n", main.join(".git").display()));
    assert_eq!(parsed(&wt), 12);
    put(&wt, ".git", &format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()));
    // commondir names another repository
    put(&main, ".git/worktrees/wt/commondir", "../../../wt\n");
    assert_eq!(parsed(&wt), 12);
    put(&main, ".git/worktrees/wt/commondir", "../..\n");
    assert_eq!(parsed(&wt), 0);
    #[cfg(unix)]
    {
        // the main checkout's index directory holds a symbolic link: refused as for this one
        std::os::unix::fs::symlink(t.0.join("elsewhere"), main.join(".scrutate/link")).unwrap();
        assert_eq!(parsed(&wt), 12);
        fs::remove_file(main.join(".scrutate/link")).unwrap();
        // or is one
        fs::rename(main.join(".scrutate"), t.0.join("real_index")).unwrap();
        std::os::unix::fs::symlink(t.0.join("real_index"), main.join(".scrutate")).unwrap();
        assert_eq!(parsed(&wt), 12);
    }
    // a plain copy of the tree is no work tree: built as always
    let copy = t.0.join("copy");
    copy_tree(&wt, &copy);
    fs::remove_file(copy.join(".git")).unwrap();
    assert_eq!(parsed(&copy), 12);
}

#[test]
fn a_linked_work_tree_reads_no_half_written_artifact_of_its_main_checkout() {
    let models = ModelSet::none();
    let t = Tmp::new("half");
    let (main, wt) = linked(&t.0);
    let mut m = open(&main, &models);
    m.update(false).unwrap();
    // a writer of the main checkout in the middle of an update: every artifact and row changed, uncommitted
    let writer = rusqlite::Connection::open(main.join(".scrutate/index.db")).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE; UPDATE artifacts SET body = x'00'; UPDATE chunks SET text = x'00'; DELETE FROM files;").unwrap();
    let mut w = open(&wt, &models);
    w.update(false).unwrap();
    assert_eq!(w.parsed, 0);
    same(&dump(w), &fresh(&wt, Arc::clone(&models)));
    // content new to the work tree is looked up in the main checkout's last commit too
    put(&main, "src/later.py", &py(70));
    put(&wt, "src/later.py", &py(70));
    let mut w = open(&wt, &models);
    w.update(false).unwrap();
    assert_eq!(w.parsed, 1, "the main checkout's uncommitted state is not read");
    writer.execute_batch("ROLLBACK").unwrap();
    m.update(false).unwrap();
    put(&wt, "src/later2.py", &py(70));
    let mut w = open(&wt, &models);
    w.update(false).unwrap();
    assert_eq!(w.parsed, 1, "the main checkout committed it, but this store holds it already");
    same(&dump(w), &fresh(&wt, Arc::clone(&models)));
}

#[test]
fn a_build_in_the_main_checkout_beside_lookups_in_a_linked_work_tree() {
    let models = ModelSet::none();
    let t = Tmp::new("beside");
    let (main, wt) = linked(&t.0);
    let mut m = open(&main, &models);
    m.update(false).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let builder = {
        let (main, stop, models) = (main.clone(), Arc::clone(&stop), Arc::clone(&models));
        std::thread::spawn(move || {
            let mut m = Index::open(&main, models).unwrap();
            let mut n = 0;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                n += 1;
                for i in 0..6 {
                    put(&main, &format!("src/m{i}.py"), &py(i + 10 * (n % 3)));
                }
                m.update(false).unwrap();
            }
        })
    };
    let want = fresh(&wt, Arc::clone(&models));
    for _ in 0..8 {
        let _ = fs::remove_dir_all(wt.join(".scrutate"));
        let mut w = open(&wt, &models);
        w.update(false).unwrap();
        same(&dump(w), &want);
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    builder.join().unwrap();
}

#[test]
fn damage_to_the_store_is_found_and_the_index_built_again() {
    // the model set `stores::update` opens with
    let models = ModelSet::from_env();
    for (name, damage) in [
        ("truncated", "UPDATE artifacts SET body = substr(body, 1, 12)"),
        ("garbage", "UPDATE artifacts SET body = x'789c0300000000'"),
        ("missing", "DELETE FROM artifacts"),
        ("texts", "UPDATE artifacts SET kept = x'00' WHERE kept IS NOT NULL"),
    ] {
        let t = Tmp::new(&format!("damage_{name}"));
        for i in 0..4 {
            put(&t.0, &format!("m{i}.py"), &py(i));
        }
        scrutate::stores::update(&t.0, scrutate::stores::Writer::IndexCode, false).unwrap();
        // an edit leaves the old content kept in the store
        put(&t.0, "m3.py", &py(30));
        scrutate::stores::update(&t.0, scrutate::stores::Writer::IndexCode, false).unwrap();
        open(&t.0, &models).damage_for_tests(damage).unwrap();
        let mut ix = open(&t.0, &models);
        let report = ix.check(false).unwrap();
        assert!(!report.ok(), "{name}: check finds it");
        // what reads the store meets the damage: a rename, and the old content back
        fs::rename(t.0.join("m0.py"), t.0.join("moved.py")).unwrap();
        put(&t.0, "m3.py", &py(3));
        let up = scrutate::stores::update(&t.0, scrutate::stores::Writer::IndexCode, false);
        assert!(up.is_ok(), "{name}: {up:?}");
        eprintln!("{name}");
        same(&dump(open(&t.0, &models)), &fresh(&t.0, Arc::clone(&models)));
    }
}

/// An index of six Python and six Rust files built without a vector model.
fn built_without_a_model(name: &str) -> Tmp {
    let t = Tmp::new(name);
    for i in 0..6 {
        put(&t.0, &format!("src/m{i}.py"), &py(i));
        put(&t.0, &format!("lib/r{i}.rs"), &rs(i));
    }
    open(&t.0, &ModelSet::none()).update(false).unwrap();
    t
}

#[cfg(unix)]
#[test]
fn a_change_to_a_vector_model_makes_the_vectors_from_the_store_and_reads_no_source_file() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let models = ModelSet::from_env();
    assert!(models.semantic_enabled(), "the bundled model");
    let t = built_without_a_model("vectors_from_store");
    let ix = open(&t.0, &models);
    let files = ix.files().unwrap();
    // every source file unreadable, with the stat the index records, so only a read would see it
    for f in &files {
        let p = t.0.join(f);
        fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
        let md = fs::metadata(&p).unwrap();
        ix.damage_for_tests(&format!("UPDATE files SET ctime_ns={} WHERE path='{f}'", md.ctime() * 1_000_000_000 + md.ctime_nsec())).unwrap();
    }
    if fs::read(t.0.join(&files[0])).is_ok() {
        eprintln!("skipped: this user reads a file of mode 000");
        return;
    }
    let mut ix = open(&t.0, &models);
    let st = ix.update(false).unwrap();
    assert_eq!((ix.parsed, st.files_changed, st.files_removed), (0, 0, 0));
    for f in &files {
        fs::set_permissions(t.0.join(f), fs::Permissions::from_mode(0o644)).unwrap();
    }
    same(&dump(ix), &fresh(&t.0, Arc::clone(&models)));
}

#[test]
fn a_file_that_names_no_artifact_is_read_again_at_a_change_of_vector_model() {
    let models = ModelSet::from_env();
    let t = built_without_a_model("no_artifact");
    // as an index written before the store held artifacts
    let ix = open(&t.0, &models);
    ix.damage_for_tests("DELETE FROM artifacts WHERE key=(SELECT art FROM files WHERE path='src/m2.py'); UPDATE files SET art=NULL WHERE path='src/m2.py'")
        .unwrap();
    let mut ix = open(&t.0, &models);
    ix.update(false).unwrap();
    assert_eq!(ix.parsed, 1);
    same(&dump(ix), &fresh(&t.0, Arc::clone(&models)));
}

#[test]
fn damage_to_the_store_met_at_a_change_of_vector_model_builds_the_index_again() {
    // the model set `stores::update` opens with
    let models = ModelSet::from_env();
    for (name, damage) in [
        ("garbage", "UPDATE artifacts SET body = x'789c0300000000' WHERE key=(SELECT art FROM files WHERE path='lib/r3.rs')"),
        ("missing", "DELETE FROM artifacts WHERE key=(SELECT art FROM files WHERE path='lib/r3.rs')"),
        ("other", "UPDATE artifacts SET body=(SELECT body FROM artifacts WHERE key=(SELECT art FROM files WHERE path='src/m0.py')) WHERE key=(SELECT art FROM files WHERE path='lib/r3.rs')"),
    ] {
        let t = built_without_a_model(&format!("switch_damage_{name}"));
        open(&t.0, &models).damage_for_tests(damage).unwrap();
        let up = scrutate::stores::update(&t.0, scrutate::stores::Writer::IndexCode, false);
        assert!(up.is_ok(), "{name}: {up:?}");
        same(&dump(open(&t.0, &models)), &fresh(&t.0, Arc::clone(&models)));
    }
}

#[test]
fn the_store_collected_while_a_reader_holds_it_leaves_the_reader_its_snapshot() {
    let models = ModelSet::none();
    let t = Tmp::new("collected");
    for i in 0..6 {
        put(&t.0, &format!("m{i}.py"), &py(i));
    }
    let mut ix = open(&t.0, &models);
    ix.kept_min = 1;
    ix.update(false).unwrap();
    for round in 1..30 {
        for i in 0..6 {
            put(&t.0, &format!("m{i}.py"), &py(100 * round + i));
        }
        ix.update(false).unwrap();
    }
    let db = t.0.join(".scrutate/index.db");
    let kept = |c: &rusqlite::Connection| -> Vec<Vec<u8>> {
        c.prepare("SELECT key FROM artifacts WHERE kept IS NOT NULL ORDER BY key")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let reader = {
        let db = db.clone();
        std::thread::spawn(move || {
            let reader = rusqlite::Connection::open(&db).unwrap();
            reader.execute_batch("BEGIN").unwrap();
            let held = kept(&reader);
            held_tx.send(held.clone()).unwrap();
            // until the writer's collection commits
            let probe = rusqlite::Connection::open(&db).unwrap();
            let t0 = std::time::Instant::now();
            while kept(&probe).iter().all(|k| held.contains(k)) && kept(&probe).len() == held.len() {
                assert!(t0.elapsed().as_secs() < 60, "no collection");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(kept(&reader), held, "the reader's snapshot");
            let bodies: Vec<Vec<u8>> = reader
                .prepare("SELECT body FROM artifacts WHERE kept IS NOT NULL")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(bodies.iter().all(|b| scrutate::artifacts::Artifact::decode(b).is_ok()));
            reader.execute_batch("COMMIT").unwrap();
            held
        })
    };
    let held = held_rx.recv().unwrap();
    assert!(!held.is_empty());
    for i in 0..6 {
        put(&t.0, &format!("m{i}.py"), &py(1000 + i));
    }
    ix.update(false).unwrap();
    reader.join().unwrap();
    let now = kept(&rusqlite::Connection::open(&db).unwrap());
    assert!(held.iter().any(|k| !now.contains(k)), "the update collected some of what the reader held");
    same(&dump(ix), &fresh(&t.0, Arc::clone(&models)));
}

#[test]
fn first_opens_of_a_new_work_tree_at_once_leave_an_index_that_checks() {
    let models = ModelSet::none();
    let t = Tmp::new("seed_race");
    let (main, wt) = linked(&t.0);
    open(&main, &models).update(false).unwrap();
    for round in 0..8 {
        let _ = fs::remove_dir_all(wt.join(".scrutate"));
        let gate = Arc::new(std::sync::Barrier::new(3));
        let opens: Vec<_> = (0..3)
            .map(|_| {
                let (wt, models, gate) = (wt.clone(), Arc::clone(&models), Arc::clone(&gate));
                std::thread::spawn(move || {
                    gate.wait();
                    drop(Index::open(&wt, models).unwrap());
                })
            })
            .collect();
        for o in opens {
            o.join().unwrap();
        }
        let report = open(&wt, &models).check(true).unwrap();
        assert!(report.ok(), "round {round}: {:?}", report.problems);
    }
}
