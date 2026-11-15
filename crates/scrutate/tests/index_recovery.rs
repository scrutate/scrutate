//! The index survives what a stopped writer leaves behind, and refuses roots and index
//! directories it must not write into.

#[path = "support/canon.rs"]
mod canon;
use canon::canon;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "support/tag.rs"]
mod tag;

use scrutate::index::Index;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("scrutate_recovery_{}_{}", name, std::process::id()));
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

fn run(root: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN).arg("--root").arg(root).args(args).stdin(Stdio::null()).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn leftover_segment_with_the_next_name_does_not_block_an_update() {
    let t = Tmp::new("orphan");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    // what a writer stopped before COMMIT leaves: the counter rolled back, the directories stayed
    let next = ix.meta("seg_counter").unwrap().parse::<i64>().unwrap() + 1;
    let seg = t.0.join(".scrutate/seg");
    for name in [format!("s{next:06}"), format!("s{next:06}.tmp")] {
        fs::create_dir_all(seg.join(&name)).unwrap();
        fs::write(seg.join(&name).join("ids.npy"), b"partial").unwrap();
    }
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let st = ix.update(false).unwrap();
    assert_eq!(st.files_changed, 1);
    drop(ix);
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    fs::write(t.0.join("c.py"), "def gamma():\n    return 3\n").unwrap();
    assert_eq!(ix.update(false).unwrap().files_changed, 1);
    assert!(ix.check(false).unwrap().problems.is_empty());
}

#[test]
fn missing_root_is_an_error_and_nothing_is_created() {
    let t = Tmp::new("missing");
    let missing = t.0.join("typo-root");
    for args in [&["index", "--no-text"][..], &["index", "--text"], &["serve"], &["search", "alpha"]] {
        let (code, err) = run(&missing, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(!missing.exists(), "{args:?} created the root");
    }
    let file = t.0.join("plain.txt");
    fs::write(&file, "x").unwrap();
    let (code, err) = run(&file, &["index", "--no-text"]);
    assert_eq!(code, 2, "{err}");
    assert!(Index::open(&missing, scrutate::models::ModelSet::none()).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_index_dir_is_refused() {
    let t = Tmp::new("symlink");
    let repo = t.0.join("repo");
    let outside = t.0.join("outside");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(outside.join("seg")).unwrap();
    fs::write(outside.join("index.db"), b"keep me").unwrap();
    fs::write(repo.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    std::os::unix::fs::symlink(&outside, repo.join(".scrutate")).unwrap();
    for args in [&["index", "--no-text"][..], &["index", "--text"], &["serve"]] {
        let (code, err) = run(&repo, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains(".scrutate"), "{err}");
    }
    scrutate::index::wipe_index(&repo);
    let mut left: Vec<String> = fs::read_dir(&outside).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["index.db", "seg"]);
    assert_eq!(fs::read(outside.join("index.db")).unwrap(), b"keep me");

    // a symlink one level down is refused too
    fs::remove_file(repo.join(".scrutate")).unwrap();
    fs::create_dir_all(repo.join(".scrutate")).unwrap();
    std::os::unix::fs::symlink(outside.join("seg"), repo.join(".scrutate/seg")).unwrap();
    let (code, err) = run(&repo, &["index", "--no-text"]);
    assert_eq!(code, 2, "{err}");
    assert_eq!(fs::read_dir(outside.join("seg")).unwrap().count(), 0);
}

#[test]
fn an_edit_that_keeps_size_and_mtime_is_seen() {
    let t = Tmp::new("samestat");
    let f = t.0.join("a.py");
    fs::write(&f, "def alpha():\n    return 1\n").unwrap();
    let mtime = fs::metadata(&f).unwrap().modified().unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    fs::write(&f, "def omega():\n    return 1\n").unwrap();
    fs::File::options().write(true).open(&f).unwrap().set_modified(mtime).unwrap();
    // the write moved the change time, which no one can set back
    assert_eq!(ix.update(false).unwrap().files_changed, 1);
    assert_eq!(ix.update(true).unwrap().files_changed, 1);
    assert_eq!(ix.named("omega").unwrap().len(), 1);
}

/// Index `root` with one file, damage it, then require two later starts to index a new file,
/// answer from it and pass `check`.
fn survives(name: &str, damage: impl FnOnce(&Path, &Index)) {
    let t = Tmp::new(name);
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::from_env()).unwrap();
    ix.update(false).unwrap();
    damage(&t.0, &ix);
    drop(ix);
    for (i, f) in ["b.py", "c.py"].iter().enumerate() {
        fs::write(t.0.join(f), format!("def fresh_{i}():\n    return {i}\n")).unwrap();
        let mut repo = scrutate::Repo::open(&t.0).unwrap_or_else(|e| panic!("{name}: start {i}: {e:#}"));
        repo.update(false).unwrap_or_else(|e| panic!("{name}: start {i}: {e:#}"));
        assert_eq!(repo.find_symbol(&format!("fresh_{i}"), 5).unwrap().len(), 1, "{name}: start {i}");
        assert_eq!(repo.find_symbol("alpha", 5).unwrap().len(), 1, "{name}: start {i}");
        let report = repo.ix.check(false).unwrap();
        assert!(report.ok(), "{name}: {report:?}");
    }
}

fn seg_dir(root: &Path) -> PathBuf {
    root.join(".scrutate/seg")
}

fn remove_db(root: &Path) {
    for n in ["index.db", "index.db-wal", "index.db-shm"] {
        let _ = fs::remove_file(root.join(".scrutate").join(n));
    }
}

#[test]
fn empty_database_file_beside_segments() {
    survives("empty_db", |r, _| {
        remove_db(r);
        fs::write(r.join(".scrutate/index.db"), b"").unwrap();
    });
}

#[test]
fn deleted_database_beside_young_segments() {
    survives("deleted_db", |r, _| remove_db(r));
}

#[test]
fn segment_counter_below_a_live_segment() {
    survives("counter", |_, ix| {
        ix.damage_for_tests("UPDATE meta SET value='0' WHERE key='seg_counter'").unwrap();
    });
}

#[test]
fn dropped_meta_table() {
    survives("no_meta", |_, ix| ix.damage_for_tests("DROP TABLE meta").unwrap());
}

#[test]
fn fresh_unlisted_segment_with_the_next_name() {
    survives("unlisted", |r, ix| {
        let from = seg_dir(r).join(&ix.segments()[0]);
        let to = seg_dir(r).join("s000002");
        fs::create_dir_all(&to).unwrap();
        for e in fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    });
}

#[test]
fn segments_entry_that_is_not_json() {
    survives("bad_json", |_, ix| {
        ix.damage_for_tests("UPDATE meta SET value='{{{' WHERE key='segments'").unwrap();
    });
}

// Windows does not let a file that is open be removed or replaced
#[cfg(unix)]
#[test]
fn database_replaced_by_a_directory() {
    survives("db_dir", |r, _| {
        remove_db(r);
        fs::create_dir_all(r.join(".scrutate/index.db")).unwrap();
    });
}

#[test]
fn segment_directory_replaced_by_a_file() {
    survives("seg_file", |r, _| {
        fs::remove_dir_all(seg_dir(r)).unwrap();
        fs::write(seg_dir(r), b"x").unwrap();
    });
}

#[test]
fn damaged_chunk_text_is_rebuilt_on_the_next_call() {
    let t = Tmp::new("blob");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    repo.update(false).unwrap();
    repo.ix.damage_for_tests("UPDATE chunks SET text=x'00112233'").unwrap();
    let answer = repo.run(&op("search_code", serde_json::json!({"query": "alpha"})), 0.0).unwrap();
    assert!(answer.text_within(100_000, 0).contains("alpha"));
}

#[test]
fn damaged_chunk_text_is_rebuilt_inside_an_explicit_refresh_interval() {
    let t = Tmp::new("blob_interval");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let session = scrutate::session::Session::open(&t.0, scrutate::session::Every::given(Some(3600.0))).unwrap();
    let settings = scrutate::mcp::Settings::new(None);
    let args = serde_json::json!({"query": "alpha"});
    assert!(scrutate::mcp::call_tool(&session, "search_code", &args, &settings, &Default::default()).text.contains("alpha"));
    scrutate::Repo::open(&t.0).unwrap().ix.damage_for_tests("UPDATE chunks SET text=x'00112233'").unwrap();
    let c = scrutate::mcp::call_tool(&session, "search_code", &args, &settings, &Default::default());
    assert!(!c.is_error && c.text.contains("alpha"), "{}", c.text);
}

fn op(tool: &str, args: serde_json::Value) -> scrutate::ops::Op {
    scrutate::ops::Op::from_tool(tool, &args).unwrap()
}

fn text(repo: &mut scrutate::Repo, tool: &str, args: serde_json::Value) -> String {
    repo.run(&op(tool, args), 0.0).unwrap_or_else(|e| panic!("{tool}: {e:#}")).text_within(100_000, 0)
}

#[test]
fn segment_without_vectors_is_rebuilt() {
    survives("no_vec", |r, ix| fs::remove_file(seg_dir(r).join(&ix.segments()[0]).join("vec.npy")).unwrap());
}

// Windows does not let a directory with open files be removed
#[cfg(unix)]
#[test]
fn index_dir_deleted_under_an_open_repo_is_rebuilt_whole() {
    let t = Tmp::new("dir_gone");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    repo.update(false).unwrap();
    fs::remove_dir_all(t.0.join(".scrutate")).unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    assert!(text(&mut repo, "find_symbol", serde_json::json!({"name": "beta"})).contains("b.py"));
    for f in ["index.db", ".gitignore", "seg"] {
        assert!(t.0.join(".scrutate").join(f).exists(), "{f}");
    }
}

#[test]
fn a_failed_compaction_after_an_update_keeps_the_update_and_its_statistics() {
    let t = indexed_tree("compact_fails");
    let counter: u32 = meta_of(&t.0, "seg_counter").unwrap().parse().unwrap();
    // the update writes the next segment; the merge after it would write the one after that
    fs::write(seg_dir(&t.0).join(format!("s{:06}", counter + 2)), "not a directory").unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let out = Command::new(BIN).arg("--root").arg(&t.0).args(["index", "--no-text"]).stdin(Stdio::null()).output().unwrap();
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    assert!(stdout.contains("1 changed"), "{stdout}{stderr}");
    assert!(stderr.contains("compaction failed"), "{stderr}");
    assert_eq!(files_of(&t.0), ["a.py", "b.py"]);
    let (code, err) = run(&t.0, &["symbol", "beta"]);
    assert_eq!(code, 0, "{err}");
}

fn files_of(root: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open(root.join(".scrutate/index.db")).unwrap();
    let mut q = db.prepare("SELECT path FROM files ORDER BY path").unwrap();
    q.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

// a database file another process has open cannot be deleted on Windows
#[cfg(unix)]
#[test]
fn a_database_another_process_rebuilt_is_reopened_and_not_written_after_it_was_deleted() {
    let t = Tmp::new("replaced");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut long = scrutate::Repo::open(&t.0).unwrap();
    long.update(false).unwrap();
    let mut reader = scrutate::Repo::open(&t.0).unwrap();
    assert!(text(&mut reader, "search_code", serde_json::json!({"query": "alpha"})).contains("a.py"));
    // what another process's rebuild after damage does
    scrutate::index::wipe_index(&t.0);
    scrutate::Repo::open(&t.0).unwrap().update(true).unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    assert!(text(&mut long, "find_symbol", serde_json::json!({"name": "beta"})).contains("b.py"));
    assert_eq!(files_of(&t.0), ["a.py", "b.py"], "the update went into the database on disk");
    assert!(long.ix.segments().iter().all(|s| seg_dir(&t.0).join(s).is_dir()), "{:?}", long.ix.segments());
    // a reader whose segments were deleted reads the new ones, without a rebuild
    let ver = meta_of(&t.0, "data_version");
    assert!(text(&mut reader, "search_code", serde_json::json!({"query": "beta"})).contains("b.py"));
    assert!(reader.update(false).is_ok());
    assert_eq!(meta_of(&t.0, "data_version"), ver, "nothing was rebuilt");
}

#[cfg(unix)]
#[test]
fn a_process_on_an_index_rebuilt_by_force_answers_from_the_new_one() {
    let t = indexed_tree("forced_under");
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    let op = |name: &str| scrutate::ops::Op::Symbol { name: name.into(), limit: 5, exact: true };
    assert!(repo.run(&op("alpha"), 0.0).is_ok());
    set_meta_of(&t.0, "schema_version", &(scrutate::index::SCHEMA_VERSION + 1).to_string());
    assert!(repo.run(&op("alpha"), 0.0).err().is_some_and(|e| e.is::<scrutate::index::Newer>()));
    assert_eq!(run(&t.0, &["index", "--force", "--no-text"]).0, 0);
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let r = repo.run(&op("beta"), 0.0).map(|a| a.text_within(100_000, 0));
    assert!(r.as_ref().is_ok_and(|s| s.contains("b.py")), "{r:?}");
    assert_eq!(files_of(&t.0), ["a.py", "b.py"]);
}

#[test]
fn a_held_write_lock_does_not_block_readers() {
    let t = Tmp::new("locked");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    scrutate::Repo::open(&t.0).unwrap().update(false).unwrap();
    let other = rusqlite::Connection::open(t.0.join(".scrutate/index.db")).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let t0 = std::time::Instant::now();
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    assert!(text(&mut repo, "find_symbol", serde_json::json!({"name": "alpha"})).contains("a.py"));
    let (s, _) = scrutate::open_updated(&t.0, false).unwrap();
    assert_eq!(s.find_symbol("alpha", 5).unwrap().len(), 1);
    assert!(t0.elapsed().as_secs() < 5, "{:?}", t0.elapsed());

    // a writer waits for the lock, then indexes
    let root = t.0.clone();
    let writer = std::thread::spawn(move || scrutate::Repo::open(&root).unwrap().update(false).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!writer.is_finished());
    other.execute_batch("ROLLBACK").unwrap();
    assert_eq!(writer.join().unwrap().files_changed, 1);
    assert_eq!(repo.find_symbol("beta", 5).unwrap().len(), 1);
}

#[test]
fn a_read_while_another_process_builds_the_first_index_says_so() {
    let t = Tmp::new("first_build");
    fs::write(t.0.join("a.py"), "def only_here():\n    return 1\n").unwrap();
    drop(scrutate::Repo::open(&t.0).unwrap());
    let other = rusqlite::Connection::open(t.0.join(".scrutate/index.db")).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();

    let (code, err) = run(&t.0, &["symbol", "only_here"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("being built by another process"), "{err}");

    let session = scrutate::session::Session::open(&t.0, scrutate::session::Every::given(Some(0.0))).unwrap();
    let c =
        scrutate::mcp::call_tool(&session, "find_symbol", &serde_json::json!({"name": "only_here"}), &scrutate::mcp::Settings::new(None), &Default::default());
    assert!(!c.is_error && c.text.contains("still building its index"), "{}", c.text);

    other.execute_batch("ROLLBACK").unwrap();
    let (code, err) = run(&t.0, &["symbol", "only_here"]);
    assert_eq!(code, 0, "{err}");
}

#[cfg(unix)]
fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

/// Root ignores permissions: tests that need a read-only directory skip under it.
#[cfg(unix)]
fn permissions_hold(dir: &Path) -> bool {
    let probe = dir.join(".probe");
    let held = fs::write(&probe, b"").is_err();
    let _ = fs::remove_file(&probe);
    held
}

#[cfg(unix)]
#[test]
fn read_only_index_dir_under_an_open_repo_answers_from_the_index() {
    let t = Tmp::new("ro_dir");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    repo.update(false).unwrap();
    let dirs = [t.0.join(".scrutate"), seg_dir(&t.0)];
    for d in &dirs {
        chmod(d, 0o555);
    }
    let held = permissions_hold(&seg_dir(&t.0));
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let r = repo.run(&op("grep_code", serde_json::json!({"pattern": "def alpha"})), 0.0);
    let r2 = repo.run(&op("find_symbol", serde_json::json!({"name": "alpha"})), 0.0);
    for d in &dirs {
        chmod(d, 0o755);
    }
    if !held {
        return;
    }
    assert!(r.unwrap().text_within(100_000, 0).contains("a.py"));
    assert!(r2.unwrap().text_within(100_000, 0).contains("a.py"));
}

#[cfg(unix)]
#[test]
fn read_only_root_with_a_complete_index_answers() {
    let t = Tmp::new("ro_root");
    let repo = t.0.join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    assert_eq!(run(&repo, &["index", "--no-text"]).0, 0);
    let dirs = [seg_dir(&repo), repo.join(".scrutate"), repo.clone()];
    for d in &dirs {
        chmod(d, 0o555);
    }
    let db = repo.join(".scrutate/index.db");
    chmod(&db, 0o444);
    let held = permissions_hold(&repo);
    let out = Command::new(BIN).arg("--root").arg(&repo).args(["symbol", "alpha"]).stdin(Stdio::null()).output().unwrap();
    chmod(&db, 0o644);
    for d in dirs.iter().rev() {
        chmod(d, 0o755);
    }
    if !held {
        return;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && stdout.contains("a.py"), "{stdout} {}", String::from_utf8_lossy(&out.stderr));
}

#[cfg(unix)]
#[test]
fn unreadable_file_loses_its_chunks() {
    let t = Tmp::new("unreadable");
    let f = t.0.join("auth.py");
    fs::write(&f, "def refresh_token():\n    return 1\n").unwrap();
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    repo.update(false).unwrap();
    // a chmod alone keeps size and mtime, so only a forced update reads the file again
    fs::write(&f, "def refresh_token():\n    return 22\n").unwrap();
    chmod(&f, 0o000);
    let held = fs::read(&f).is_err();
    let r = repo.update(false);
    let n = repo.find_symbol("refresh_token", 5).map(|h| h.len());
    chmod(&f, 0o644);
    if !held {
        return;
    }
    r.unwrap();
    assert_eq!(n.unwrap(), 0);
    repo.update(false).unwrap();
    assert_eq!(repo.find_symbol("refresh_token", 5).unwrap().len(), 1);
    chmod(&f, 0o000);
    let r = repo.update(true);
    let n = repo.find_symbol("refresh_token", 5).map(|h| h.len());
    chmod(&f, 0o644);
    r.unwrap();
    assert_eq!(n.unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn a_chmod_alone_drops_a_file_and_brings_it_back_without_a_forced_update() {
    let t = Tmp::new("chmod_only");
    let f = t.0.join("a.py");
    fs::write(&f, "def alpha():\n    return 1\n").unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    let alphas = |ix: &Index| ix.named("alpha").unwrap().len();
    chmod(&f, 0o000);
    let held = fs::read(&f).is_err();
    let r = ix.update(false);
    let n = alphas(&ix);
    chmod(&f, 0o644);
    if !held {
        return;
    }
    r.unwrap();
    assert_eq!(n, 0);
    ix.update(false).unwrap();
    assert_eq!(alphas(&ix), 1);
    // a change of ctime alone costs a read and a hash, no parse
    chmod(&f, 0o600);
    assert_eq!(ix.update(false).unwrap().files_changed, 0);
    assert_eq!(alphas(&ix), 1);
}

#[test]
fn an_emptied_chunks_table_is_damage_and_is_rebuilt() {
    let t = Tmp::new("emptied");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let damage = |sql: &str| rusqlite::Connection::open(t.0.join(".scrutate/index.db")).unwrap().execute_batch(sql).unwrap();
    // under an open repository
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    repo.update(false).unwrap();
    damage("DELETE FROM refs; DELETE FROM chunks;");
    assert!(text(&mut repo, "find_symbol", serde_json::json!({"name": "alpha"})).contains("a.py"));
    drop(repo);
    // at the next start
    damage("DELETE FROM refs; DELETE FROM chunks;");
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    assert!(text(&mut repo, "find_symbol", serde_json::json!({"name": "alpha"})).contains("a.py"));
    drop(repo);
    // some rows gone: found when the statistics are read
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    scrutate::Repo::open(&t.0).unwrap().update(false).unwrap();
    damage("DELETE FROM chunks WHERE name='beta'");
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    assert!(text(&mut repo, "search_code", serde_json::json!({"query": "beta"})).contains("b.py"));
}

#[test]
fn an_index_whose_files_table_has_no_ctime_column_is_read_and_upgraded() {
    let t = Tmp::new("no_ctime");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap().update(false).unwrap();
    // the table as it was before the ctime column, with a row written that way
    let con = rusqlite::Connection::open(t.0.join(".scrutate/index.db")).unwrap();
    con.execute_batch(
        "CREATE TABLE f2(path TEXT PRIMARY KEY, size INTEGER, mtime_ns INTEGER, hash TEXT);
         INSERT INTO f2 SELECT path, size, mtime_ns, hash FROM files; DROP TABLE files; ALTER TABLE f2 RENAME TO files;",
    )
    .unwrap();
    drop(con);
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    assert!(text(&mut repo, "find_symbol", serde_json::json!({"name": "alpha"})).contains("a.py"));
    // the first update reads the file again (no ctime on record), parses nothing, and records one
    assert_eq!(repo.update(false).unwrap().files_changed, 0);
    // the stored format, read through a connection of its own
    let con = rusqlite::Connection::open(t.0.join(".scrutate/index.db")).unwrap();
    let ctime: Option<i64> = con.query_row("SELECT ctime_ns FROM files WHERE path='a.py'", [], |r| r.get(0)).unwrap();
    assert!(ctime.is_some());
    con.execute("INSERT OR REPLACE INTO files(path,size,mtime_ns,hash) VALUES('b.py',1,1,'x')", []).unwrap();
}

#[test]
fn an_index_of_files_without_chunks_is_not_taken_for_damage() {
    let t = Tmp::new("no_chunks");
    fs::write(t.0.join("empty.py"), "").unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    let before = ix.meta("data_version");
    drop(ix);
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    ix.lex().unwrap();
    assert_eq!(ix.meta("data_version"), before);
}

#[cfg(unix)]
#[test]
fn symlinks_inside_the_index_dir_are_refused() {
    let t = Tmp::new("planted");
    let repo = t.0.join("repo");
    let outside = t.0.join("outside");
    fs::create_dir_all(repo.join(".scrutate/text")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(repo.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    fs::write(outside.join("victim.txt"), b"mine").unwrap();
    std::os::unix::fs::symlink(outside.join("victim.txt"), repo.join(".scrutate/text/manifest.json.tmp")).unwrap();
    for args in [&["index", "--no-text"][..], &["index", "--text"], &["index"]] {
        let (code, err) = run(&repo, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains("manifest.json.tmp"), "{err}");
    }
    assert_eq!(fs::read(outside.join("victim.txt")).unwrap(), b"mine");

    fs::remove_dir_all(repo.join(".scrutate")).unwrap();
    fs::create_dir_all(repo.join(".scrutate")).unwrap();
    std::os::unix::fs::symlink(outside.join("created_by_scrutate"), repo.join(".scrutate/.gitignore")).unwrap();
    let (code, err) = run(&repo, &["index"]);
    assert_eq!(code, 2, "{err}");
    assert!(!outside.join("created_by_scrutate").exists());
}

#[test]
fn write_new_replaces_a_planted_link_instead_of_following_it() {
    let t = Tmp::new("write_new");
    fs::write(t.0.join("victim"), b"mine").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(t.0.join("victim"), t.0.join("link")).unwrap();
    scrutate::index::write_new(&t.0.join("link"), b"new").unwrap();
    assert_eq!(fs::read(t.0.join("victim")).unwrap(), b"mine");
    assert_eq!(fs::read(t.0.join("link")).unwrap(), b"new");
}

#[test]
fn a_transaction_left_open_by_a_failed_update_is_rolled_back_by_the_next() {
    let t = Tmp::new("left_open");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    // what a panic in the middle of an update leaves on the connection
    ix.damage_for_tests("BEGIN IMMEDIATE; DELETE FROM chunks;").unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    assert_eq!(ix.update(false).unwrap().files_changed, 1);
    assert_eq!(ix.named("alpha").unwrap().len() + ix.named("beta").unwrap().len(), 2);
    assert!(ix.check(false).unwrap().ok());
}

#[test]
fn a_cancelled_update_leaves_the_index_as_it_was() {
    let t = Tmp::new("cancel");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    ix.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let e = ix.update(false).unwrap_err();
    assert_eq!(e.to_string(), "cancelled");
    assert_eq!(ix.files().unwrap(), ["a.py"], "the update was rolled back");
    assert!(ix.check(false).unwrap().ok());
    ix.cancel.store(false, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(ix.update(false).unwrap().files_changed, 1);
}

#[test]
fn an_index_built_with_older_parsing_rules_is_read_again_once() {
    let t = Tmp::new("content_rev");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    assert_eq!(ix.meta("content_rev").as_deref(), Some(scrutate::index::CONTENT_REVISION.to_string().as_str()));
    // what an older build left: its own symbol names and an older revision
    ix.damage_for_tests("UPDATE chunks SET name='old_alpha' WHERE name='alpha'; UPDATE meta SET value='0' WHERE key='content_rev';").unwrap();
    drop(ix);
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    let st = ix.update(false).unwrap();
    assert_eq!(st.files_changed, 2);
    assert_eq!(ix.named("alpha").unwrap().len(), 1);
    assert_eq!(ix.meta("content_rev").as_deref(), Some(scrutate::index::CONTENT_REVISION.to_string().as_str()));
    assert_eq!(ix.update(false).unwrap().files_changed, 0);
    drop(ix);
    assert_eq!(Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap().update(false).unwrap().files_changed, 0);
}

fn meta_of(root: &Path, key: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    let db = rusqlite::Connection::open(root.join(".scrutate/index.db")).unwrap();
    db.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional().unwrap()
}

fn set_meta_of(root: &Path, key: &str, value: &str) {
    let db = rusqlite::Connection::open(root.join(".scrutate/index.db")).unwrap();
    db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)", [key, value]).unwrap();
}

fn indexed_tree(name: &str) -> Tmp {
    let t = Tmp::new(name);
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    assert_eq!(run(&t.0, &["index"]).0, 0);
    t
}

#[test]
fn an_index_of_a_newer_content_revision_is_read_as_it_is_and_not_updated() {
    let t = indexed_tree("newer_rev");
    let newer = (scrutate::index::CONTENT_REVISION + 1).to_string();
    set_meta_of(&t.0, "content_rev", &newer);
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let (code, err) = run(&t.0, &["search", "alpha"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err.matches("built by a newer scrutate").count(), 1, "{err}");
    assert!(err.contains(&format!("content revision {newer}")) && err.contains("index --force"), "{err}");
    assert_eq!(meta_of(&t.0, "content_rev").as_deref(), Some(newer.as_str()));
    let db = rusqlite::Connection::open(t.0.join(".scrutate/index.db")).unwrap();
    let files: i64 = db.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
    assert_eq!(files, 1, "a newer index is not updated");
    drop(db);
    let (code, err) = run(&t.0, &["check"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("built by a newer scrutate"), "{err}");
    let (code, err) = run(&t.0, &["index", "--no-text"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("built by a newer scrutate"), "{err}");
    assert_eq!(meta_of(&t.0, "content_rev").as_deref(), Some(newer.as_str()));
    assert_eq!(run(&t.0, &["index", "--force"]).0, 0);
    assert_eq!(meta_of(&t.0, "content_rev"), Some(scrutate::index::CONTENT_REVISION.to_string()));
}

#[test]
fn an_index_of_a_newer_schema_or_model_is_refused_and_left_as_it_is() {
    for (key, value) in [("schema_version", scrutate::index::SCHEMA_VERSION + 1), ("model_rev", scrutate::index::MODEL_REVISION + 1)] {
        let t = indexed_tree(key);
        set_meta_of(&t.0, key, &value.to_string());
        let db = t.0.join(".scrutate/index.db");
        let before = fs::read(&db).unwrap();
        let (code, err) = run(&t.0, &["search", "alpha"]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("built by a newer scrutate") && err.contains("cannot read it"), "{err}");
        assert!(!err.contains("rebuilding"), "{err}");
        assert!(fs::read(&db).unwrap() == before, "{key}: the database changed");
        assert_eq!(run(&t.0, &["check"]).0, 2);
        assert_eq!(run(&t.0, &["index", "--no-text"]).0, 2);
        assert!(fs::read(&db).unwrap() == before, "{key}: the database changed");
        let (code, err) = run(&t.0, &["index", "--force", "--no-text"]);
        assert_eq!(code, 0, "{err}");
        assert_eq!(meta_of(&t.0, "schema_version"), Some(scrutate::index::SCHEMA_VERSION.to_string()));
        assert_eq!(meta_of(&t.0, "model_rev"), Some(scrutate::index::MODEL_REVISION.to_string()));
    }
}

#[test]
fn an_index_without_a_model_revision_is_not_rebuilt() {
    let t = indexed_tree("no_model_rev");
    let db = rusqlite::Connection::open(t.0.join(".scrutate/index.db")).unwrap();
    db.execute("DELETE FROM meta WHERE key='model_rev'", []).unwrap();
    drop(db);
    let (code, err) = run(&t.0, &["search", "alpha"]);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("rebuilding") && !err.contains("newer"), "{err}");
    assert_eq!(meta_of(&t.0, "model_rev"), None, "a reader does not write it");
}

#[test]
fn a_running_process_stops_updating_when_a_newer_scrutate_takes_over_the_index() {
    let t = Tmp::new("newer_live");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut ix = Index::open(&t.0, scrutate::models::ModelSet::none()).unwrap();
    ix.update(false).unwrap();
    let rev = (scrutate::index::CONTENT_REVISION + 1).to_string();
    ix.set_meta("content_rev", &rev).unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    assert!(ix.refresh(0.0).unwrap().is_none());
    assert!(ix.update(false).unwrap_err().is::<scrutate::index::Newer>());
    assert!(ix.update_paths(&["b.py".into()], false).is_err());
    assert_eq!(ix.meta("content_rev"), Some(rev));
    ix.set_meta("schema_version", &(scrutate::index::SCHEMA_VERSION + 1).to_string()).unwrap();
    assert!(ix.refresh(0.0).unwrap_err().is::<scrutate::index::Newer>());
    drop(ix);
    assert!(Index::open(&t.0, scrutate::models::ModelSet::none()).err().is_some_and(|e| e.is::<scrutate::index::Newer>()));
    // a long-lived process answers every operation with the error, without touching the index
    let t = indexed_tree("newer_repo");
    let mut repo = scrutate::Repo::open(&t.0).unwrap();
    let op = scrutate::ops::Op::Symbol { name: "alpha".into(), limit: 5, exact: true };
    assert!(repo.run(&op, 0.0).is_ok());
    repo.ix.set_meta("schema_version", &(scrutate::index::SCHEMA_VERSION + 1).to_string()).unwrap();
    for _ in 0..2 {
        assert!(repo.run(&op, 0.0).err().is_some_and(|e| e.is::<scrutate::index::Newer>()));
        assert!(repo.run_as_is(&op).err().is_some_and(|e| e.is::<scrutate::index::Newer>()));
        assert!(repo.update(true).unwrap_err().is::<scrutate::index::Newer>());
    }
    assert_eq!(repo.ix.meta("schema_version"), Some((scrutate::index::SCHEMA_VERSION + 1).to_string()));
}

#[test]
fn a_text_index_of_a_newer_format_is_refused_and_left_as_it_is() {
    let t = Tmp::new("newer_text");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    assert_eq!(run(&t.0, &["index", "--text"]).0, 0);
    let manifest = t.0.join(".scrutate/text/manifest.json");
    let text = fs::read_to_string(&manifest).unwrap();
    let ours = format!("\"format\":{}", scrutate::textindex::FORMAT);
    let newer = scrutate::textindex::FORMAT + 1;
    assert!(text.contains(&ours), "{text}");
    fs::write(&manifest, text.replace(&ours, &format!("\"format\":{newer}"))).unwrap();
    let before = fs::read(&manifest).unwrap();
    let (code, err) = run(&t.0, &["index", "--text"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("built by a newer scrutate") && err.contains(&format!("format {newer}")), "{err}");
    assert!(fs::read(&manifest).unwrap() == before);
    let (code, err) = run(&t.0, &["grep", "--index", "alpha"]);
    assert_eq!(code, 2, "{err}");
    assert!(fs::read(&manifest).unwrap() == before);
    assert_eq!(run(&t.0, &["index", "--text", "--force"]).0, 0);
    assert!(fs::read_to_string(&manifest).unwrap().contains(&ours));
}

/// Overwrite element `i` of a little-endian `.npy` array of 4- or 8-byte elements.
fn patch_npy(path: &Path, i: usize, value: i64) {
    let mut b = fs::read(path).unwrap();
    let len_size = if b[6] == 1 { 2 } else { 4 };
    let hlen = b[8..8 + len_size].iter().rev().fold(0usize, |a, &x| a * 256 + x as usize);
    let data = 8 + len_size + hlen;
    let header = String::from_utf8_lossy(&b[..data]).into_owned();
    let w = if header.contains("<i8") || header.contains("<u8") { 8 } else { 4 };
    b[data + i * w..data + (i + 1) * w].copy_from_slice(&value.to_le_bytes()[..w]);
    fs::write(path, b).unwrap();
}

#[test]
fn a_segment_row_out_of_range_is_rebuilt_by_the_merge_of_the_next_update() {
    let t = Tmp::new("bad_row");
    fs::write(t.0.join("a.py"), "def alpha():\n    return beta()\n").unwrap();
    fs::write(t.0.join("b.py"), "def beta():\n    return 1\n").unwrap();
    assert_eq!(run(&t.0, &["index"]).0, 0);
    let segs: Vec<serde_json::Value> = serde_json::from_str(&meta_of(&t.0, "segments").unwrap()).unwrap();
    let seg = seg_dir(&t.0).join(segs[0].as_str().unwrap());
    patch_npy(&seg.join("doc0.npy"), 0, 99999);
    // the new file's segment and the damaged one are merged
    fs::write(t.0.join("c.py"), "def gamma():\n    return 3\n").unwrap();
    let (code, err) = run(&t.0, &["index"]);
    assert!(!err.contains("internal error"), "{err}");
    assert_eq!(code, 0, "{err}");
    let (code, err) = run(&t.0, &["symbol", "gamma"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(files_of(&t.0), ["a.py", "b.py", "c.py"]);
}

/// An index that holds no files and cannot be filled (a rebuild cut short after its clear
/// committed, then a directory nobody may write) answers with an error, not with "nothing found".
#[cfg(unix)]
#[test]
fn an_empty_index_that_cannot_be_filled_is_an_error_not_an_empty_answer() {
    use std::os::unix::fs::PermissionsExt;
    let t = Tmp::new("empty_readonly");
    assert_eq!(run(&t.0, &["index", "--no-text"]).0, 0);
    fs::write(t.0.join("a.py"), "def write_segment():\n    return 1\n").unwrap();
    let mode = |m: u32| {
        for p in [t.0.join(".scrutate"), t.0.clone()] {
            fs::set_permissions(&p, fs::Permissions::from_mode(m)).unwrap();
        }
        for e in fs::read_dir(t.0.join(".scrutate")).unwrap().flatten() {
            let _ = fs::set_permissions(e.path(), fs::Permissions::from_mode(if m == 0o555 { 0o444 } else { 0o644 }));
        }
    };
    mode(0o555);
    let out = Command::new(BIN).arg("--root").arg(&t.0).args(["symbol", "write_segment"]).stdin(Stdio::null()).output().unwrap();
    mode(0o755);
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(2), "stdout {stdout:?} stderr {stderr:?}");
    assert!(!stdout.contains("(no results)") && stderr.contains("holds no files"), "stdout {stdout:?} stderr {stderr:?}");
}
