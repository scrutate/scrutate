//! Every mutable static of the crate is on a list with why it may be one process-wide (or
//! per-thread) value. A static whose value depends on the repository an answer is for is wrong
//! when one process serves two (`--also`, two Python `Repo`s, MCP): it must be keyed by root.

use std::collections::BTreeSet;
use std::path::Path;

/// (file under src/, static, why it is safe as one value for the whole process or thread)
const ALLOWED: &[(&str, &str, &str)] = &[
    ("answers.rs", "READS", "per thread, the files a resolver read while one Names is open"),
    ("cli.rs", "FLAG", "the process's Ctrl-C flag"),
    ("cli.rs", "NEVER", "a stop flag that is never set, for the watch thread of serve"),
    ("custom.rs", "CURRENT", "the last loaded root's custom languages; an update runs under custom::with for its own root"),
    ("custom.rs", "SCOPED", "per thread, the custom languages of the root custom::with runs for"),
    ("finder.rs", "ONE", "serializes this process's writes of the shared recent-files list"),
    ("index.rs", "COMP", "per thread, a reused deflate state"),
    ("index.rs", "PARSE", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "TREESITTER", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "TOKENIZE", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "ENCODE", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "PACK", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "INSERT", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "SEGMENT", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "MODEL", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "READ", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "LOOKUP", "SCRUTATE_PROFILE timing totals for the process"),
    ("index.rs", "SAID", "a warning said once per process"),
    ("index.rs", "BEFORE_CHUNKS", "per thread, a test hook"),
    ("index.rs", "AFTER_LANGUAGES", "per thread, a test hook"),
    ("index.rs", "OPENED", "whether the process opened any index, for the panic message"),
    ("index.rs", "STOP", "the process's stop flag, which ends every wait for a lock of any root"),
    ("instant.rs", "JOINED", "per thread, a reused path buffer"),
    ("instant.rs", "HELD", "per thread, the batches of the file being searched"),
    ("instant.rs", "ONE_LINE", "per thread, search statistics for tests"),
    ("instant.rs", "NO_ONE_LINE", "per thread, search statistics for tests"),
    ("instant.rs", "MOST_QUEUED", "per thread, search statistics for tests"),
    ("lazy_bind.rs", "SYSTEM", "the handle of the system library, the same for the process"),
    ("parse.rs", "PARSERS", "per thread, a reused tree-sitter parser per language"),
    ("running.rs", "LOCK", "the process's signal state"),
    ("running.rs", "HELD", "the process's signal state"),
    ("running.rs", "WRITING", "the process's signal state"),
    ("running.rs", "STOPPING", "the process's signal state"),
    ("running.rs", "COUNTED", "the process's count of running searches"),
    ("running.rs", "SET", "the process's count of running searches"),
    ("running.rs", "WATCHING", "the process's signal state"),
    ("running.rs", "STOP_ASKED", "the process's signal state"),
    ("text.rs", "STEM_CACHE", "per thread, stems of words, the same for every root"),
    ("text.rs", "IDENT_CACHE", "per thread, splits of identifiers, the same for every root"),
    ("text.rs", "OWN", "keyed by canonical root"),
    ("textindex.rs", "TABLE", "per thread, a reused trigram mask table"),
];

const MUTABLE: &[&str] = &["Mutex", "RwLock", "Cell<", "Cell::", "RefCell", "Atomic", "OnceCell", "UnsafeCell"];

/// (file, name) of each static whose type can change after it is first set.
fn mutable_statics(src: &Path) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut dirs = vec![src.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).unwrap().map(Result::unwrap) {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
                continue;
            }
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let file = p.strip_prefix(src).unwrap().to_string_lossy().replace('\\', "/");
            let mut rest = text.as_str();
            while let Some(at) = rest.find("static ") {
                let before = rest[..at].chars().next_back();
                rest = &rest[at + "static ".len()..];
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\'' || c == '&') {
                    continue;
                }
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                let Some(ty) = rest[name.len()..].trim_start().strip_prefix(':') else { continue };
                let ty = &ty[..ty.find(['=', ';']).unwrap_or(ty.len())];
                if MUTABLE.iter().any(|m| ty.contains(m)) {
                    out.insert((file.clone(), name));
                }
            }
        }
    }
    out
}

#[test]
fn every_mutable_static_is_listed_with_why_it_may_be_one_value() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = mutable_statics(&src);
    let listed: BTreeSet<(String, String)> = ALLOWED.iter().map(|(f, n, _)| (f.to_string(), n.to_string())).collect();
    let new: Vec<_> = found.difference(&listed).collect();
    assert!(new.is_empty(), "mutable statics not on the list (key one by root, or list it with why): {new:?}");
    let gone: Vec<_> = listed.difference(&found).collect();
    assert!(gone.is_empty(), "listed statics that are no longer there: {gone:?}");
    assert!(found.len() > 30, "the scan found too few to be reading the sources: {}", found.len());
}
