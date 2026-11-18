//! Every place the shipped code starts `git`. git.rs's `Git::raw` is the one: `Git::command` calls
//! it with the read-only contract (only commands that read, every filter driver emptied, no fetch,
//! no optional locks, no file system monitor), and `Git::no_filters` calls it to read the filter
//! drivers. A new `Command::new("git")` or a new call of `raw` fails here: route it through
//! `Git::command` instead. A file's test module (from `#[cfg(test)]` over `mod tests`) is left out.

use std::fs;
use std::path::Path;

/// (file, what it holds) for every `Command::new("git")` outside test code.
const SPAWNS: [(&str, &str); 1] = [("git.rs", "fn raw")];

/// The callers of `Git::raw`.
const RAW_CALLS: [&str; 2] = ["fn command", "fn no_filters"];

/// The file without its test modules: a `#[cfg(...)]` naming `test` over a `mod` line with its
/// body (`#[cfg(test)]`, `#[cfg(all(test, windows))]`), at any depth, cut to its closing brace.
fn shipped(path: &Path) -> String {
    let mut text = fs::read_to_string(path).unwrap();
    'cut: loop {
        let mut at = 0;
        for line in text.split_inclusive('\n') {
            let rest = &text[at + line.len()..];
            let (attr, next) = (line.trim_start(), rest.lines().next().unwrap_or("").trim_start());
            // `mod x;` names a file the walk reads on its own
            if attr.starts_with("#[cfg(") && attr.contains("test") && (next.starts_with("mod ") || next.starts_with("pub mod ")) && next.contains('{') {
                let open = at + line.len() + rest.find('{').unwrap();
                let mut depth = 0;
                let close = text[open..]
                    .char_indices()
                    .find(|&(_, c)| {
                        depth += match c {
                            '{' => 1,
                            '}' => -1,
                            _ => 0,
                        };
                        depth == 0
                    })
                    .map_or(text.len(), |(i, _)| open + i + 1);
                text.replace_range(at..close, "");
                continue 'cut;
            }
            at += line.len();
        }
        return text;
    }
}

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// The name of the `fn` whose body holds byte `at` of `text`.
fn enclosing_fn(text: &str, at: usize) -> String {
    let start = text[..at].rfind("fn ").expect("a fn before the call");
    let name: String = text[start + 3..].chars().take_while(|c| !c.is_whitespace() && !"(<".contains(*c)).collect();
    format!("fn {name}")
}

#[test]
fn git_starts_in_one_place() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for dir in ["scrutate/src", "scrutate-py/src"] {
        sources(&crates.join(dir), &mut files);
    }
    let mut spawns = Vec::new();
    for f in &files {
        let text = shipped(f);
        for (at, _) in text.match_indices("Command::new(\"git\")") {
            spawns.push((f.file_name().unwrap().to_string_lossy().into_owned(), enclosing_fn(&text, at)));
        }
    }
    let want: Vec<(String, String)> = SPAWNS.iter().map(|(f, n)| (f.to_string(), n.to_string())).collect();
    assert_eq!(spawns, want, "a git spawn outside Git::raw: route it through Git::command");

    let git = shipped(&crates.join("scrutate/src/git.rs"));
    let mut callers: Vec<String> = git.match_indices(".raw(").map(|(at, _)| enclosing_fn(&git, at)).collect();
    callers.sort();
    assert_eq!(callers, RAW_CALLS, "Git::raw skips the read-only contract: call Git::command");
}

/// (file, what it holds) for every process the shipped code starts: a `Command::new` of any
/// program, or grep-cli's decompression reader, which runs a decompressor. docs/SECURITY.md ("No
/// code execution") lists each; a new one fails here until it is listed there too.
const ANY_SPAWNS: [(&str, &str); 5] =
    [("archive.rs", "fn external"), ("git.rs", "fn raw"), ("setup.rs", "fn apply"), ("shellhook.rs", "fn command"), ("shellhook.rs", "fn command")];

#[test]
fn every_process_the_shipped_code_starts_is_one_security_md_lists() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for dir in ["scrutate/src", "scrutate-py/src"] {
        sources(&crates.join(dir), &mut files);
    }
    files.sort();
    let mut spawns = Vec::new();
    for f in &files {
        let text = shipped(f);
        for needle in ["Command::new(", "DecompressionReaderBuilder"] {
            for (at, _) in text.match_indices(needle) {
                spawns.push((f.file_name().unwrap().to_string_lossy().into_owned(), enclosing_fn(&text, at)));
            }
        }
    }
    spawns.sort();
    let want: Vec<(String, String)> = ANY_SPAWNS.iter().map(|(f, n)| (f.to_string(), n.to_string())).collect();
    assert_eq!(spawns, want, "a new process spawn: list it in docs/SECURITY.md and here, or remove it");
}
