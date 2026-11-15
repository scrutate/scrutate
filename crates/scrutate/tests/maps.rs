//! scrutate maps only files of its own that are immutable by construction, and the files named to a
//! one-shot `scrutate grep` or the Grep hook, which ripgrep maps the same way (a process that ends
//! with the search). Every place the crate maps a file into memory is on a list with why the file
//! may be mapped: a process that stays up (`serve`, MCP, a session) dies by SIGBUS when a mapped
//! file is cut under it, so a file the user controls is read there instead.

use std::collections::BTreeSet;
use std::path::Path;

/// (file under src/, function, why its file may be mapped)
const ALLOWED: &[(&str, &str, &str)] = &[
    ("npy.rs", "map", "the one call of Mmap::map; each caller below says why its file may be mapped"),
    ("npy.rs", "open", "Npy::open, for the index's segment arrays only (its callers are listed)"),
    (
        "lex.rs",
        "open",
        "a lexical segment's .npy arrays: written in a new directory, renamed into place, never rewritten; removal or replacement by rename is safe for a mapping; only an in-place truncation is not, and scrutate never does one",
    ),
    ("textindex.rs", "open", "a text index segment's .npy arrays, written and renamed as a lexical segment's"),
    ("textindex.rs", "map_file", "a text index segment's post.bin and paths.bin, written and renamed as its arrays"),
    (
        "source.rs",
        "make_searcher",
        "a file named to a one-shot grep (Files::map_named, set by the CLI and the Grep hook only), as ripgrep maps it; a resident process reads it whole",
    ),
];

/// What maps a file: the memmap2 calls, and the crate's functions that map what they are given.
/// (token, the one file it counts in, or "" for every file)
const MAPS: &[(&str, &str)] = &[
    ("Mmap::map", ""),
    ("MmapOptions", ""),
    ("MmapMut", ""),
    ("MmapRaw", ""),
    ("MmapChoice", ""),
    ("npy::map(", ""),
    ("Npy::open(", ""),
    ("map_file(", "textindex.rs"),
    ("= map(&", "npy.rs"),
];

/// (file, function) of each call that maps a file, outside the sources' test modules.
fn map_sites(src: &Path) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut dirs = vec![src.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).unwrap().map(Result::unwrap) {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let file = p.strip_prefix(src).unwrap().to_string_lossy().replace('\\', "/");
                out.extend(in_source(&file, &std::fs::read_to_string(&p).unwrap()).into_iter().map(|f| (file.clone(), f)));
            }
        }
    }
    out
}

/// The functions of `text` (the source of `file`) that call something in `MAPS`.
fn in_source(file: &str, text: &str) -> Vec<String> {
    let text = text.split("\n#[cfg(test)]\nmod tests").next().unwrap_or(text);
    let mut out = Vec::new();
    for (token, only) in MAPS {
        if !only.is_empty() && *only != file {
            continue;
        }
        for (at, _) in text.match_indices(token) {
            let before = &text[..at];
            if before.ends_with("fn ") {
                continue; // the definition, not a call
            }
            let Some(f) = before.rfind("fn ") else { continue };
            let name: String = before[f + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

#[test]
fn the_scan_finds_every_call_that_maps_a_file() {
    let src = "fn a(f: &File) {\n    unsafe { memmap2::Mmap::map(f) };\n}\nfn b(p: &Path) {\n    let x = Npy::open(p);\n}\nfn map_file(p: &Path) {}\nfn c() {\n    crate::npy::map(&f, p);\n}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        Npy::open(p);\n    }\n}\n";
    assert_eq!(in_source("x.rs", src), ["a", "c", "b"]);
    assert_eq!(in_source("textindex.rs", "fn map_file(p: &Path) {}\nfn open() {\n    map_file(p);\n}\n"), ["open"]);
}

#[test]
fn every_file_map_is_listed_with_why_it_may_be_mapped() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = map_sites(&src);
    let listed: BTreeSet<(String, String)> = ALLOWED.iter().map(|(f, n, _)| (f.to_string(), n.to_string())).collect();
    let new: Vec<_> = found.difference(&listed).collect();
    assert!(new.is_empty(), "files mapped where it is not listed (read a file the user controls, or list it with why): {new:?}");
    let gone: Vec<_> = listed.difference(&found).collect();
    assert!(gone.is_empty(), "listed maps that are no longer there: {gone:?}");
}
