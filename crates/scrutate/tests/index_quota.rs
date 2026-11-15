//! The index quota (`SCRUTATE_INDEX_MAX_FILES`, `SCRUTATE_INDEX_MAX_BYTES`): an update that would
//! pass it fails and leaves the last committed index as it was. One test, since it sets the
//! process environment.

use std::fs;

use scrutate::index::Index;
use scrutate::models::ModelSet;
use scrutate::textindex;

fn set(k: &str, v: &str) {
    // SAFETY: the only test of this binary, so no other thread reads the environment
    unsafe { std::env::set_var(k, v) };
}

#[test]
fn an_update_past_the_quota_fails_and_keeps_the_last_index() {
    let root = std::env::temp_dir().join(format!("scrutate_quota_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    // a repository: no index in a directory above takes it over
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(&root).unwrap();
    let body = "def f():\n    return 1\n";
    for f in ["a.py", "b.py"] {
        fs::write(root.join(f), body).unwrap();
    }
    let mut ix = Index::open(&root, ModelSet::none()).unwrap();
    let files = |ix: &Index| ix.stats().unwrap()["files"].as_i64().unwrap();
    set("SCRUTATE_INDEX_MAX_FILES", "2");
    ix.update(false).unwrap();
    textindex::update(&root, false, false).unwrap();

    // a walk that finds a third file
    fs::write(root.join("c.py"), body).unwrap();
    let e = format!("{:#}", ix.update(false).unwrap_err());
    assert!(e.contains("code index would hold 3 files") && e.contains("index.max_files = 2") && e.contains("SCRUTATE_INDEX_MAX_FILES"), "{e}");
    assert_eq!(files(&ix), 2);
    let e = format!("{:#}", textindex::update(&root, false, false).unwrap_err());
    assert!(e.contains("text index would hold 3 files"), "{e}");

    // a named update counts the files it does not name too
    let e = format!("{:#}", ix.update_paths(&["c.py".into()], true).unwrap_err());
    assert!(e.contains("would hold 3 files"), "{e}");
    assert!(textindex::update_paths(&root, &["c.py".into()]).is_err());
    fs::remove_file(root.join("a.py")).unwrap();
    assert_eq!(ix.update_paths(&["a.py".into(), "c.py".into()], true).unwrap().files_changed, 1, "one out, one in");
    assert_eq!(textindex::update_paths(&root, &["a.py".into(), "c.py".into()]).unwrap().files, 2);
    assert_eq!(files(&ix), 2);

    // the failed updates wrote nothing: with room, a walk finds a.py new
    fs::write(root.join("a.py"), body).unwrap();
    set("SCRUTATE_INDEX_MAX_FILES", "");
    assert_eq!(ix.update(false).unwrap().files_changed, 1);
    assert_eq!(textindex::update(&root, false, false).unwrap().files_changed, 1);

    // bytes: the files' sizes, where a file over the code index's size limit adds nothing there
    let total = (3 * body.len()).to_string();
    set("SCRUTATE_INDEX_MAX_BYTES", &total);
    fs::write(root.join("big.txt"), vec![b'x'; 1_000_001]).unwrap();
    ix.update(false).unwrap();
    let e = format!("{:#}", textindex::update(&root, false, false).unwrap_err());
    assert!(e.contains(&format!("text index would hold {} bytes of files", 3 * body.len() + 1_000_001)) && e.contains("index.max_bytes"), "{e}");
    fs::remove_file(root.join("big.txt")).unwrap();
    fs::write(root.join("b.py"), format!("{body}#\n")).unwrap();
    let e = format!("{:#}", ix.update(false).unwrap_err());
    assert!(e.contains(&format!("would hold {} bytes of files, more than index.max_bytes = {total}", 3 * body.len() + 2)), "{e}");
    set("SCRUTATE_INDEX_MAX_BYTES", "1K");
    ix.update(false).unwrap();
    set("SCRUTATE_INDEX_MAX_BYTES", "lots");
    assert!(format!("{:#}", ix.update(false).unwrap_err()).contains("SCRUTATE_INDEX_MAX_BYTES"));
    drop(ix);
    let _ = fs::remove_dir_all(&root);
}
