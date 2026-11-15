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

fn shipped(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap();
    match text.find("#[cfg(test)]\nmod tests") {
        Some(i) => text[..i].to_string(),
        None => text,
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
