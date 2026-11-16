//! Every parser tier (tree-sitter grammars, keyword heuristics, line windows) on arbitrary text
//! under every registered file extension: no panic, and chunk line ranges inside the file.
#![no_main]
use libfuzzer_sys::fuzz_target;

const EXTS: &[&str] = &[
    "py",
    "rs",
    "go",
    "js",
    "ts",
    "tsx",
    "c",
    "cpp",
    "java",
    "rb",
    "php",
    "sh",
    "kt",
    "swift",
    "scala",
    "lua",
    "hs",
    "ex",
    "zig",
    "jl",
    "dart",
    "r",
    "erl",
    "nix",
    "groovy",
    "m",
    "elm",
    "ps1",
    "tf",
    "sol",
    "html",
    "css",
    "json",
    "yaml",
    "toml",
    "md",
    "cmake",
    "mk",
    "gleam",
    "pas",
    "adb",
    "el",
    "scm",
    "rkt",
    "lisp",
    "vim",
    "glsl",
    "bzl",
    "fish",
    "graphql",
    "prisma",
    "pp",
    "bicep",
    "s",
    "jsonnet",
    "xml",
    "ini",
    "rst",
    "txt",
    "unknownext",
];

fuzz_target!(|data: &[u8]| {
    let Some((&e, rest)) = data.split_first() else {
        return;
    };
    let source = String::from_utf8_lossy(rest);
    let path = format!("f.{}", EXTS[e as usize % EXTS.len()]);
    let parsed = scrutate::parse::parse_file(&path, &source);
    let lines = source.lines().count().max(1) as i64 + 1;
    for c in &parsed.chunks {
        assert!(
            c.start >= 1 && c.start <= c.end && c.end <= lines,
            "{path}: chunk {}..{} of {lines} lines",
            c.start,
            c.end
        );
    }
});
