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

/// The site of a map outside every function: a static's initialiser, say.
const OUTSIDE: &str = "(outside a fn)";
/// The site of a memmap2 import or type alias in a file `ALLOWED` does not list: through it, `Mmap`
/// may go by any name.
const IMPORT: &str = "use memmap2";

/// The sites of `text` (the source of `file`) that call something in `MAPS`: the function each call
/// sits in by brace depth, `OUTSIDE`, or `IMPORT`. Comments, literals and the test module are left out.
fn in_source(file: &str, text: &str) -> Vec<String> {
    let text = without_tests(&code_only(text));
    let mut out = Vec::new();
    let mut add = |name: String| {
        if !out.contains(&name) {
            out.push(name);
        }
    };
    let word = |at: usize| at == 0 || !text.as_bytes()[at - 1].is_ascii_alphanumeric() && text.as_bytes()[at - 1] != b'_';
    // `use` and `type` declarations, keyword to `;`: one that names memmap2 can rename `Mmap`
    let decl =
        |kw: &'static str| text.match_indices(kw).filter(move |&(at, _)| word(at)).map(|(at, _)| (at, text[at..].find(';').map_or(text.len(), |e| at + e)));
    let uses: Vec<(usize, usize)> = decl("use ").collect();
    let types: Vec<(usize, usize)> = decl("type ").collect();
    let listed = ALLOWED.iter().any(|(f, _, _)| *f == file);
    if !listed && uses.iter().chain(&types).any(|&(a, z)| text[a..z].contains("memmap2")) {
        add(IMPORT.to_string());
    }
    for (token, only) in MAPS {
        if !only.is_empty() && *only != file {
            continue;
        }
        for (at, _) in text.match_indices(token) {
            // the definition, or an import, maps nothing
            if text[..at].ends_with("fn ") || uses.iter().any(|&(a, z)| (a..z).contains(&at)) {
                continue;
            }
            add(enclosing_fn(&text[..at]).unwrap_or_else(|| OUTSIDE.to_string()));
        }
    }
    out
}

/// The function whose body is open at the end of `before`, by brace depth.
fn enclosing_fn(before: &str) -> Option<String> {
    let mut open: Vec<Option<String>> = Vec::new();
    let mut named: Option<String> = None;
    let b = before.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'{' => open.push(named.take().or_else(|| open.last().cloned().flatten())),
            b'}' => {
                open.pop();
            }
            b';' => named = None,
            b'f' if before[i..].starts_with("fn ") && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) => {
                let name: String = before[i + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !name.is_empty() {
                    named = Some(name);
                }
            }
            _ => {}
        }
    }
    open.pop().flatten()
}

/// `text` with every `#[cfg(test)] mod tests { ... }` blanked, braces and all; what follows stays.
fn without_tests(text: &str) -> String {
    let mut out = text.to_string();
    let mut from = 0;
    while let Some(at) = out[from..].find("#[cfg(test)]").map(|a| a + from) {
        from = at + 1;
        let rest = out[at + 12..].trim_start();
        let Some(body) = rest.strip_prefix("mod tests").map(str::trim_start).filter(|r| r.starts_with('{')) else { continue };
        let open = out.len() - body.len();
        let mut depth = 0;
        let close = out[open..].char_indices().find_map(|(i, c)| {
            depth += match c {
                '{' => 1,
                '}' => -1,
                _ => 0,
            };
            (depth == 0).then_some(open + i + 1)
        });
        let close = close.unwrap_or(out.len());
        let blanked: String = out[at..close].chars().map(|c| if c == '\n' { c } else { ' ' }).collect();
        out.replace_range(at..close, &blanked);
    }
    out
}

/// `text` with its comments and the insides of its string and character literals turned to spaces,
/// line breaks kept, so a token in a doc comment or a string is no call.
fn code_only(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, a: usize, z: usize| out[a..z.min(b.len())].iter_mut().filter(|c| **c != b'\n').for_each(|c| *c = b' ');
    let ident = |i: usize| i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
    let mut i = 0;
    while i < b.len() {
        let rest = &text[i..];
        if rest.starts_with("//") {
            let z = rest.find('\n').map_or(b.len(), |e| i + e);
            blank(&mut out, i, z);
            i = z;
        } else if rest.starts_with("/*") {
            let (mut depth, mut j) = (0, i);
            while j < b.len() {
                if text[j..].starts_with("/*") {
                    depth += 1;
                    j += 2;
                } else if text[j..].starts_with("*/") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            blank(&mut out, i, j);
            i = j;
        } else if (rest.starts_with("r#") || rest.starts_with("r\"")) && (!ident(i) || b[i - 1] == b'b' && !ident(i - 1)) {
            let hashes = rest[1..].bytes().take_while(|&c| c == b'#').count();
            if rest.as_bytes().get(1 + hashes) != Some(&b'"') {
                i += 1;
                continue;
            }
            let end = format!("\"{}", "#".repeat(hashes));
            let open = i + 2 + hashes;
            let z = text[open..].find(&end).map_or(b.len(), |e| open + e);
            blank(&mut out, open, z);
            i = z + end.len();
        } else if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            blank(&mut out, i + 1, j);
            i = j + 1;
        } else if b[i] == b'\'' {
            // a character literal ('x', '\n', '\u{1F600}'); a lifetime ('a) has no closing quote
            let z = if rest[1..].starts_with('\\') {
                rest[2..].find('\'').map(|e| i + 2 + e)
            } else {
                rest[1..].chars().next().map(|c| i + 1 + c.len_utf8()).filter(|&e| b.get(e) == Some(&b'\''))
            };
            match z {
                Some(z) => {
                    blank(&mut out, i + 1, z);
                    i = z + 1;
                }
                None => i += 1,
            }
        } else {
            i += 1;
        }
    }
    String::from_utf8(out).expect("only whole characters are blanked")
}

#[test]
fn the_scan_finds_every_call_that_maps_a_file() {
    let src = "fn a(f: &File) {\n    unsafe { memmap2::Mmap::map(f) };\n}\nfn b(p: &Path) {\n    let x = Npy::open(p);\n}\nfn map_file(p: &Path) {}\nfn c() {\n    crate::npy::map(&f, p);\n}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        Npy::open(p);\n    }\n}\n";
    assert_eq!(in_source("x.rs", src), ["a", "c", "b"]);
    assert_eq!(in_source("textindex.rs", "fn map_file(p: &Path) {}\nfn open() {\n    map_file(p);\n}\n"), ["open"]);
}

#[test]
fn a_token_in_a_comment_or_a_string_is_no_call() {
    // the shape that made up_to_the_nul look like a map: a doc comment naming the token
    let src = "fn make_searcher() {\n    unsafe { grep_searcher::MmapChoice::auto() };\n}\nfn up_to_the_nul() {}\n/// maps (`MmapChoice::auto`) here\nconst MAPS: bool = true;\nfn s() {\n    let t = \"Mmap::map\";\n    let r = r#\"Npy::open(\"#;\n}\n";
    assert_eq!(in_source("x.rs", src), ["make_searcher"]);
}

#[test]
fn a_map_outside_every_function_is_a_site_of_its_own() {
    let src = "static M: LazyLock<Mmap> = LazyLock::new(|| unsafe { memmap2::Mmap::map(&f) }.unwrap());\nfn a() {}\n";
    assert_eq!(in_source("x.rs", src), [OUTSIDE]);
}

#[test]
fn a_memmap2_import_outside_the_listed_files_is_a_site() {
    let src = "use memmap2::Mmap as Mapped;\nfn a(f: &File) {\n    unsafe { Mapped::map(f) };\n}\n";
    assert_eq!(in_source("x.rs", src), [IMPORT]);
    assert!(in_source("npy.rs", src).is_empty());
}

#[test]
fn a_raw_byte_string_a_word_with_use_in_it_and_a_type_alias_are_read_as_rust_reads_them() {
    let src = "fn a() {\n    let p = br\"C:\\\";\n    Npy::open(p);\n}\nfn b() {\n    let q = br#\"x\"y\"#;\n    Npy::open(q);\n}\nfn c() {\n    let reuse = Npy::open(p);\n}\n";
    assert_eq!(in_source("x.rs", src), ["a", "b", "c"]);
    let alias = "type M = memmap2::Mmap;\nfn d(f: &File) {\n    unsafe { M::map(f) };\n}\n";
    assert_eq!(in_source("x.rs", alias), [IMPORT]);
    assert!(in_source("npy.rs", alias).is_empty());
}

#[test]
fn a_function_after_the_test_module_is_scanned() {
    let src = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() {\n        Npy::open(p);\n    }\n}\nfn late(p: &Path) {\n    Npy::open(p);\n}\n";
    assert_eq!(in_source("x.rs", src), ["late"]);
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
