//! Command-line interface: one binary, two engines.
//!
//! Exit codes follow grep: 0 = something was found, 1 = nothing found, 2 = error, 130 = cancelled.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use termcolor::{Color, ColorChoice, ColorSpec, WriteColor};

use crate::fmt::{fmt_hits, hits_json};
use crate::instant::{self, Case, Files, Mode, Output, Paths, Pattern, Query, Record, Report};
use crate::ops::Op;
use crate::session::Session;
use crate::stores::Writer;
use crate::text::query_words;

pub use crate::ops::SCHEMA;

/// Ctrl-C sets a cancellation flag (searches stop between files, exit 130). Direct libc / Win32
/// calls: the `ctrlc` crate links Foundation and libobjc into every macOS process, about 1 ms of
/// start-up on each invocation.
mod interrupt {
    #[cfg_attr(not(any(unix, windows)), allow(unused_imports))]
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg_attr(not(any(unix, windows)), allow(unused_imports))]
    use std::sync::{Arc, OnceLock};

    #[cfg(any(unix, windows))]
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    // async-signal-safe: an atomic load of the cell and an atomic store. -> whether the flag was
    // set already: a second Ctrl-C ends the process as it would any program
    #[cfg(any(unix, windows))]
    fn fire() -> bool {
        FLAG.get().is_some_and(|f| f.swap(true, Ordering::Relaxed))
    }

    #[cfg(unix)]
    pub fn install(flag: Arc<AtomicBool>) {
        extern "C" fn handler(_: i32) {
            if fire() {
                // the default action, then the signal again: the process ends as Ctrl-C ends any
                unsafe {
                    signal(SIGINT, SIG_DFL);
                    raise(SIGINT);
                }
            }
        }
        extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
            fn raise(sig: i32) -> i32;
        }
        const SIGINT: i32 = 2;
        const SIG_DFL: usize = 0;
        if FLAG.set(flag).is_ok() {
            unsafe {
                signal(SIGINT, handler as *const () as usize);
            }
        }
    }

    #[cfg(windows)]
    pub fn install(flag: Arc<AtomicBool>) {
        // 0 for a second Ctrl-C: the next handler, the system's own, ends the process
        unsafe extern "system" fn handler(_: u32) -> i32 {
            i32::from(!fire())
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        }
        if FLAG.set(flag).is_ok() {
            unsafe {
                SetConsoleCtrlHandler(Some(handler), 1);
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    pub fn install(_flag: Arc<AtomicBool>) {}
}

#[derive(Parser)]
#[command(name = "scrutate", version, about = "Code search: instant grep-class search plus an indexed, ranked code-intelligence engine", long_about = None)]
pub struct Cli {
    /// repository root for the indexed engine (default: current directory)
    #[arg(long, default_value = ".", global = true, env = "SCRUTATE_ROOT")]
    pub root: PathBuf,
    /// never print color (also when NO_COLOR is set and not empty)
    #[arg(long, global = true)]
    pub no_color: bool,
    /// size limit, in bytes, of an answer from the index; a cut answer says how many items are left
    #[arg(long, global = true, value_name = "BYTES", env = "SCRUTATE_MAX_BYTES")]
    pub max_bytes: Option<usize>,
    /// items of an answer to skip, to continue one that --max-bytes cut (not with serve: a tool call takes its own offset)
    #[arg(long, global = true, value_name = "N", default_value_t = 0)]
    pub offset: usize,
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// What `review` prints.
#[derive(Clone, Copy, ValueEnum, Debug)]
pub enum ReviewFormat {
    Markdown,
    Json,
    Gitlab,
}

#[derive(Clone, Copy, ValueEnum, Debug)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

/// The arguments of `scrutate grep`.
#[derive(clap::Args)]
#[cfg_attr(test, derive(Debug, PartialEq))]
pub struct GrepArgs {
    /// the pattern (omit it when patterns are given with -e)
    /// (not a `String`: with -e it is the first path)
    #[arg(required_unless_present_any = ["regexp", "type_list"])]
    pub pattern: Option<std::ffi::OsString>,
    /// files or directories to search, `-` for standard input (default: standard input when it is
    /// a pipe or a file, otherwise the current directory)
    pub paths: Vec<PathBuf>,
    /// a pattern to search for; repeatable, a line matching any of them matches
    #[arg(short = 'e', long = "regexp", value_name = "PATTERN", allow_hyphen_values = true)]
    pub regexp: Vec<String>,
    /// use the text index (Engine B, built by `scrutate index`) to read only the files that can
    /// match; results are those of the indexed snapshot of --root
    #[arg(long)]
    pub index: bool,
    /// with --index: bring the text index up to date first (only changed files are read), so
    /// the answer is the walk's for the tree as it is now
    #[arg(long, requires = "index")]
    pub update: bool,
    /// treat PATTERN as a literal string
    #[arg(short = 'F', long)]
    pub fixed_strings: bool,
    /// identifier search: `refreshToken` also finds refresh_token, RefreshToken, REFRESH_TOKEN
    #[arg(long)]
    pub ident: bool,
    /// approximate search: literal within N edits (a character inserted, deleted or substituted)
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "1")]
    pub fuzzy: Option<u8>,
    /// approximate regex: PATTERN within N edits (a character inserted, deleted or substituted; no
    /// anchors or word boundaries)
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "1")]
    pub fuzzy_regex: Option<u8>,
    /// PCRE2 syntax: look-around and backreferences (backtracking, stopped by PCRE2's match limit)
    #[arg(short = 'P', long)]
    pub pcre2: bool,
    /// also search inside zip and tar archives and compressed files (gz, bz2, xz, zst, lz4, br, Z)
    #[arg(short = 'z', long)]
    pub search_zip: bool,
    /// case-insensitive
    #[arg(short = 'i', long, overrides_with = "smart_case")]
    pub ignore_case: bool,
    /// case-insensitive unless the pattern has an uppercase letter (of -i and -S the later wins;
    /// SCRUTATE_GREP_SMART_CASE turns it on when neither is given)
    #[arg(short = 'S', long, overrides_with = "ignore_case")]
    pub smart_case: bool,
    /// match whole words only
    #[arg(short = 'w', long)]
    pub word: bool,
    /// let the pattern span lines
    #[arg(short = 'U', long)]
    pub multiline: bool,
    /// select non-matching lines
    #[arg(short = 'v', long)]
    pub invert_match: bool,
    /// print only the paths of files with matches
    #[arg(short = 'l', long)]
    pub files_with_matches: bool,
    /// print only the number of matching lines per file
    #[arg(short = 'c', long)]
    pub count: bool,
    /// print the file's path with each line and count (the default unless one file is the only
    /// path given); of -H and -I the later wins
    #[arg(short = 'H', long, overrides_with = "no_filename")]
    pub with_filename: bool,
    /// never print the file's path with each line and count (the default when one file is the
    /// only path given); -l and --json still print it
    #[arg(short = 'I', long, overrides_with = "with_filename")]
    pub no_filename: bool,
    /// print the line number of each line (the default); of -n and -N the later wins
    #[arg(short = 'n', long, overrides_with = "no_line_number")]
    pub line_number: bool,
    /// print no line numbers (--json still has them)
    #[arg(short = 'N', long, overrides_with = "line_number")]
    pub no_line_number: bool,
    /// lines of context after each match (wins over -C, as in ripgrep)
    #[arg(short = 'A', long)]
    pub after: Option<usize>,
    /// lines of context before each match (wins over -C, as in ripgrep)
    #[arg(short = 'B', long)]
    pub before: Option<usize>,
    /// lines of context around each match
    #[arg(short = 'C', long)]
    pub context: Option<usize>,
    /// stop after N matching lines per file
    #[arg(short = 'm', long)]
    pub max_count: Option<u64>,
    /// stop everything after N matches in total
    #[arg(long)]
    pub max_results: Option<u64>,
    /// include/exclude files by glob (prefix ! to exclude); repeatable; SCRUTATE_GREP_GLOBS takes a
    /// comma-separated list (commas inside {} belong to the glob)
    #[arg(short = 'g', long = "glob")]
    pub globs: Vec<String>,
    /// only files of this type (see --type-list); prefix ! to exclude; repeatable
    #[arg(short = 't', long = "type", env = "SCRUTATE_GREP_TYPES", value_delimiter = ',')]
    pub types: Vec<String>,
    /// list the known file types
    #[arg(long)]
    pub type_list: bool,
    /// search hidden files and directories
    #[arg(long, env = "SCRUTATE_GREP_HIDDEN", value_parser = clap::builder::FalseyValueParser::new())]
    pub hidden: bool,
    /// ignore .gitignore / .ignore rules
    #[arg(long, env = "SCRUTATE_GREP_NO_IGNORE", value_parser = clap::builder::FalseyValueParser::new())]
    pub no_ignore: bool,
    /// follow symbolic links
    #[arg(short = 'L', long, env = "SCRUTATE_GREP_FOLLOW", value_parser = clap::builder::FalseyValueParser::new())]
    pub follow: bool,
    /// skip files larger than this (e.g. 10M)
    #[arg(long, value_name = "SIZE", env = "SCRUTATE_GREP_MAX_FILESIZE")]
    pub max_filesize: Option<String>,
    /// descend at most this many directory levels
    #[arg(long, env = "SCRUTATE_GREP_MAX_DEPTH")]
    pub max_depth: Option<usize>,
    /// number of search threads, at most 256 (default: the cores, at most 12; on macOS the performance cores; more when the files come from the disk)
    #[arg(short = 'j', long, default_value_t = 0, env = "SCRUTATE_THREADS")]
    pub threads: usize,
    /// sort as `rg --sort path`: each path's results in path order, the paths in command-line
    /// order (waits for the search to finish); with --max-results, the first N in that order
    #[arg(long)]
    pub sort: bool,
    /// one JSON object per line
    #[arg(long)]
    pub json: bool,
    /// print search statistics to stderr
    #[arg(long)]
    pub stats: bool,
    /// group matches by enclosing function/class using the index (Engine B), if one exists
    #[arg(long)]
    pub symbols: bool,
}

/// `Cli` with `grep` as its only subcommand: clap builds every subcommand's arguments before it
/// parses, about 0.2 ms for all of them, and a search needs one.
#[derive(Parser)]
#[command(name = "scrutate", version, about = "Code search: instant grep-class search plus an indexed, ranked code-intelligence engine", long_about = None)]
struct GrepCli {
    /// repository root for the indexed engine (default: current directory)
    #[arg(long, default_value = ".", global = true, env = "SCRUTATE_ROOT")]
    root: PathBuf,
    /// never print color (also when NO_COLOR is set and not empty)
    #[arg(long, global = true)]
    no_color: bool,
    /// size limit, in bytes, of the answer, closing line included: whole lines in path order, then
    /// a line with how many are left (not with --json); without it the answer is whole
    #[arg(long, global = true, value_name = "BYTES", env = "SCRUTATE_GREP_MAX_BYTES")]
    max_bytes: Option<usize>,
    /// lines of the answer to skip, to continue one that --max-bytes cut
    #[arg(long, global = true, value_name = "N", default_value_t = 0)]
    offset: usize,
    #[command(subcommand)]
    cmd: GrepCmd,
}

#[derive(Subcommand)]
enum GrepCmd {
    /// Engine A: instant search of files (no index). PATTERN is a regex unless -F, --ident or --fuzzy
    // an option given twice takes its last value, as in ripgrep
    #[command(alias = "g", version, args_override_self = true)]
    Grep(GrepArgs),
}

/// The subcommand of `args`, past the options every command takes (`--root`, `--no-color`,
/// `--max-bytes`, `--offset`), so `scrutate --root x grep` is read as grep's own arguments are,
/// with grep's byte limit and grep's help.
fn subcommand(args: &[String]) -> Option<&str> {
    let mut i = 1;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--root" | "--max-bytes" | "--offset" => i += 2,
            "--no-color" => i += 1,
            a if a.starts_with("--root=") || a.starts_with("--max-bytes=") || a.starts_with("--offset=") => i += 1,
            a => return Some(a),
        }
    }
    None
}

/// The environment variables clap reads for `grep` (a test holds the list to clap's): with one
/// of them set, `plain_grep` is not used. Any other variable is read after the arguments, by
/// code both ways share.
const GREP_ENV: [&str; 9] = [
    "SCRUTATE_ROOT",
    "SCRUTATE_GREP_MAX_BYTES",
    "SCRUTATE_GREP_TYPES",
    "SCRUTATE_GREP_HIDDEN",
    "SCRUTATE_GREP_NO_IGNORE",
    "SCRUTATE_GREP_FOLLOW",
    "SCRUTATE_GREP_MAX_FILESIZE",
    "SCRUTATE_GREP_MAX_DEPTH",
    "SCRUTATE_THREADS",
];

/// `scrutate grep ARGS` of the plain shapes (`[-FiwnlcU...] [-e PATTERN]... [-g GLOB]... [-j N]
/// [PATTERN] [PATH]...`, flags anywhere, each at most once), read without clap: clap's parse of
/// even these is 1.3 M instructions and 4 page-ins, more than a small search. `None` for anything
/// else, which clap then parses (and reports when it is wrong); `plain_grep_is_clap` holds the
/// two to the same answer. Returns `--no-color` and the arguments.
fn plain_grep(args: &[String]) -> Option<(bool, GrepArgs)> {
    const BOOLS: [(char, &str); 24] = [
        ('F', "fixed-strings"),
        ('\0', "ident"),
        ('P', "pcre2"),
        ('z', "search-zip"),
        ('i', "ignore-case"),
        ('S', "smart-case"),
        ('w', "word"),
        ('U', "multiline"),
        ('v', "invert-match"),
        ('l', "files-with-matches"),
        ('c', "count"),
        ('H', "with-filename"),
        ('I', "no-filename"),
        ('n', "line-number"),
        ('N', "no-line-number"),
        ('L', "follow"),
        ('\0', "hidden"),
        ('\0', "no-ignore"),
        ('\0', "index"),
        ('\0', "sort"),
        ('\0', "json"),
        ('\0', "stats"),
        ('\0', "symbols"),
        ('\0', "no-color"),
    ];
    const VALUES: [(char, &str); 3] = [('e', "regexp"), ('g', "glob"), ('j', "threads")];
    let mut g = GrepArgs {
        pattern: None,
        paths: Vec::new(),
        regexp: Vec::new(),
        index: false,
        update: false,
        fixed_strings: false,
        ident: false,
        fuzzy: None,
        fuzzy_regex: None,
        pcre2: false,
        search_zip: false,
        ignore_case: false,
        smart_case: false,
        word: false,
        multiline: false,
        invert_match: false,
        files_with_matches: false,
        count: false,
        with_filename: false,
        no_filename: false,
        line_number: false,
        no_line_number: false,
        after: None,
        before: None,
        context: None,
        max_count: None,
        max_results: None,
        globs: Vec::new(),
        types: Vec::new(),
        type_list: false,
        hidden: false,
        no_ignore: false,
        follow: false,
        max_filesize: None,
        max_depth: None,
        threads: 0,
        sort: false,
        json: false,
        stats: false,
        symbols: false,
    };
    let mut no_color = false;
    let mut seen = 0u32;
    let mut threads_seen = false;
    let mut positional: Vec<&String> = Vec::new();
    // what clap takes for a flag's value given as the next argument
    fn value_of(v: Option<&String>) -> Option<&String> {
        v.filter(|v| !v.starts_with('-'))
    }
    let mut set = |i: usize, g: &mut GrepArgs| -> Option<()> {
        if seen & 1 << i != 0 {
            return None;
        }
        seen |= 1 << i;
        // of -i and -S, -H and -I, -n and -N the later wins
        match i {
            0 => g.fixed_strings = true,
            1 => g.ident = true,
            2 => g.pcre2 = true,
            3 => g.search_zip = true,
            4 => (g.ignore_case, g.smart_case) = (true, false),
            5 => (g.smart_case, g.ignore_case) = (true, false),
            6 => g.word = true,
            7 => g.multiline = true,
            8 => g.invert_match = true,
            9 => g.files_with_matches = true,
            10 => g.count = true,
            11 => (g.with_filename, g.no_filename) = (true, false),
            12 => (g.no_filename, g.with_filename) = (true, false),
            13 => (g.line_number, g.no_line_number) = (true, false),
            14 => (g.no_line_number, g.line_number) = (true, false),
            15 => g.follow = true,
            16 => g.hidden = true,
            17 => g.no_ignore = true,
            18 => g.index = true,
            19 => g.sort = true,
            20 => g.json = true,
            21 => g.stats = true,
            22 => g.symbols = true,
            _ => no_color = true,
        }
        Some(())
    };
    let mut value = |i: usize, v: &String, g: &mut GrepArgs| -> Option<()> {
        match i {
            0 => g.regexp.push(v.clone()),
            1 => g.globs.push(v.clone()),
            _ if threads_seen || v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) => return None,
            _ => {
                threads_seen = true;
                g.threads = v.parse().ok()?;
            }
        }
        Some(())
    };
    if !matches!(args.get(1).map(String::as_str), Some("grep" | "g")) || args.iter().any(|a| a.contains('\u{FFFD}')) {
        return None;
    }
    let mut rest = args[2..].iter();
    while let Some(a) = rest.next() {
        if let Some(name) = a.strip_prefix("--") {
            if let Some(i) = BOOLS.iter().position(|f| f.1 == name) {
                set(i, &mut g)?;
            } else {
                let i = VALUES.iter().position(|f| f.1 == name)?;
                value(i, value_of(rest.next())?, &mut g)?;
            }
        } else if let Some(shorts) = a.strip_prefix('-').filter(|s| !s.is_empty()) {
            for (at, c) in shorts.char_indices() {
                if let Some(i) = BOOLS.iter().position(|f| f.0 == c) {
                    set(i, &mut g)?;
                    continue;
                }
                let i = VALUES.iter().position(|f| f.0 == c)?;
                let attached = &shorts[at + 1..];
                if attached.is_empty() {
                    value(i, value_of(rest.next())?, &mut g)?;
                } else if !attached.starts_with(['=', '-']) {
                    value(i, &attached.to_string(), &mut g)?;
                } else {
                    return None;
                }
                break;
            }
        } else {
            positional.push(a);
        }
    }
    // clap refuses an empty path
    if positional.iter().skip(1).any(|p| p.is_empty()) {
        return None;
    }
    let mut positional = positional.into_iter().cloned();
    g.pattern = positional.next().map(Into::into);
    g.paths = positional.map(PathBuf::from).collect();
    (g.pattern.is_some() || !g.regexp.is_empty()).then_some((no_color, g))
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Engine A: instant search of files (no index). PATTERN is a regex unless -F, --ident or --fuzzy
    // an option given twice takes its last value, as in ripgrep
    #[command(alias = "g", version, args_override_self = true)]
    Grep(GrepArgs),
    /// Engine B: build or update the index (code intelligence and the text index for `grep --index`)
    Index {
        #[arg(long)]
        force: bool,
        /// only the text index (trigram postings for `grep --index`)
        #[arg(long, conflicts_with = "no_text")]
        text: bool,
        /// skip the text index
        #[arg(long)]
        no_text: bool,
    },
    /// Engine B: ranked search by meaning, name or identifier
    #[command(alias = "s")]
    Search {
        query: String,
        /// alternative phrasing (repeatable); results of all phrasings are fused
        #[arg(short = 'a', long = "alt")]
        alt: Vec<String>,
        /// also search this repository (repeatable); its results start with the path given here
        #[arg(long = "also", value_name = "ROOT")]
        also: Vec<PathBuf>,
        #[arg(short, default_value_t = crate::ops::SEARCH_K)]
        k: usize,
        /// show why each result matched
        #[arg(long)]
        why: bool,
        #[arg(long)]
        json: bool,
    },
    /// Unified: pick the engine from the query (regex-looking -> grep, otherwise ranked search; falls back to identifier grep without an index)
    Find {
        query: String,
        #[arg(short, default_value_t = crate::ops::SEARCH_K)]
        k: usize,
        #[arg(long)]
        json: bool,
    },
    /// where is this function/class/method defined?
    Symbol {
        name: String,
        /// list at most this many definitions
        #[arg(long, default_value_t = crate::ops::SYMBOL_LIMIT)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// which functions call this one?
    Callers {
        name: String,
        /// list at most this many callers
        #[arg(long, default_value_t = crate::ops::CALLERS_LIMIT)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// which functions does this one call?
    Callees {
        name: String,
        /// list at most this many callees
        #[arg(long, default_value_t = crate::ops::CALLEES_LIMIT)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// the shortest chain of calls from one function to another (names, `Class.method` or `path:line`)
    #[command(name = "path")]
    CallPath {
        from: String,
        to: String,
        /// follow at most this many calls
        #[arg(long, default_value_t = crate::ops::PATH_DEPTH)]
        depth: usize,
        #[arg(long)]
        json: bool,
    },
    /// the files ranked by how the others call and import them, each with its top definitions
    Map {
        /// keep the files under this directory
        path: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// modules/files a file imports (Engine B)
    Deps {
        file: String,
        #[arg(long)]
        json: bool,
    },
    /// files that import a module or file (reverse dependencies)
    Rdeps {
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// lines of a file, or with --symbol whole definitions (as the MCP read_code tool)
    Read {
        /// a file of the repository; with --symbol, keep the definitions in this file
        path: Option<String>,
        /// a name, `Class.method` or a location (`path:start-end`) whose definitions to read
        #[arg(long)]
        symbol: Option<String>,
        /// first line
        #[arg(long, allow_negative_numbers = true)]
        start: Option<i64>,
        /// last line [default: 400 lines from start]
        #[arg(long, allow_negative_numbers = true)]
        end: Option<i64>,
        #[arg(long)]
        json: bool,
    },
    /// symbols of a file in source order
    Outline {
        file: String,
        #[arg(long)]
        json: bool,
    },
    /// every occurrence of a name, each resolved to the definition it refers to (or unresolved)
    Refs {
        name: String,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = crate::ops::REFS_MAX)]
        max_results: u64,
        /// only the references of the definition of NAME in PATH (at LINE, when several)
        #[arg(long = "def", value_name = "PATH[:LINE]")]
        def: Option<String>,
        /// no resolution: definitions, callers and every whole-word occurrence grouped by enclosing symbol
        #[arg(long, conflicts_with = "def")]
        textual: bool,
    },
    /// code similar to a symbol or location (`name`, `Class.method` or `path:line`)
    Similar {
        target: String,
        #[arg(short, default_value_t = crate::ops::SIMILAR_K)]
        k: usize,
        #[arg(long)]
        json: bool,
    },
    /// duplicate and near-duplicate functions across the repository
    Dupes {
        /// ignore functions with fewer body tokens than this
        #[arg(long, default_value_t = crate::ops::DUPES_MIN_TOKENS)]
        min_tokens: usize,
        /// near-duplicate threshold (Jaccard similarity of token 3-grams)
        #[arg(long, default_value_t = crate::ops::DUPES_THRESHOLD)]
        threshold: f32,
        #[arg(long, default_value_t = crate::ops::DUPES_LIMIT)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// what a change touches: the definitions a git diff changes, their callers and the tests to run
    Impact {
        /// compare with this revision; `main...` compares with the merge base of main and HEAD
        #[arg(long, value_name = "REV")]
        base: Option<String>,
        /// the staged changes, not the working tree
        #[arg(long)]
        staged: bool,
        /// follow callers this many steps (at most 10)
        #[arg(long, default_value_t = crate::impact::DEPTH as u64, value_parser = clap::value_parser!(u64).range(0..=crate::impact::MAX_DEPTH as u64))]
        depth: u64,
        #[arg(long)]
        json: bool,
    },
    /// a pull request report: impact between two commits and the duplicates the head adds
    Review {
        /// the commit the pull request starts from; `main...` is the merge base of main and the head
        #[arg(long, value_name = "REV")]
        base: String,
        /// the commit of the pull request; check it out first, the index is the working tree's
        #[arg(long, value_name = "REV", default_value = "HEAD")]
        head: String,
        #[arg(long, value_enum, default_value_t = ReviewFormat::Markdown)]
        format: ReviewFormat,
        /// follow callers this many steps (at most 10)
        #[arg(long, default_value_t = crate::impact::DEPTH as u64, value_parser = clap::value_parser!(u64).range(0..=crate::impact::MAX_DEPTH as u64))]
        depth: u64,
        /// link each location in the Markdown to URL/PATH#Lstart-Lend
        #[arg(long, value_name = "URL")]
        link: Option<String>,
    },
    /// paths of the repository ranked for a short query that may be misspelt (`srch.rs` finds `search.rs`), files read lately first among equals
    Files {
        query: String,
        #[arg(long, default_value_t = crate::finder::LIMIT as u64, value_parser = clap::value_parser!(u64).range(1..=crate::finder::MAX_LIMIT))]
        limit: u64,
        #[arg(long)]
        json: bool,
    },
    /// definitions nothing in the repository uses (entry points left out), and the undecided ones with why
    Dead {
        /// keep the definitions under this directory or file
        path: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// structural search with a tree-sitter query, e.g. `scrutate tsquery python '(function_definition name: (identifier) @n)'`
    Tsquery {
        lang: String,
        query: String,
        #[arg(default_value = ".")]
        paths: Vec<PathBuf>,
        #[arg(long, default_value_t = crate::ops::TSQUERY_LIMIT)]
        limit: usize,
        #[arg(long)]
        json: bool,
        /// parse every file again instead of reusing the results cached in `--root`/.scrutate/tsquery
        #[arg(long)]
        no_cache: bool,
        /// replace each match with TEMPLATE, where `$NAME` or `${NAME}` is capture @NAME and `$$` a dollar; prints a diff
        #[arg(long, value_name = "TEMPLATE", conflicts_with_all = ["json", "limit"])]
        rewrite: Option<String>,
        /// with --rewrite: change the files instead of printing the diff
        #[arg(long, requires = "rewrite")]
        write: bool,
    },
    /// index statistics
    Stats,
    /// keep the index (and the text index, if there is one) up to date as files change
    Watch {
        /// repository to watch (default: --root)
        path: Option<PathBuf>,
    },
    /// run the MCP server on stdio
    Serve {
        /// at most one directory scan per this many seconds [default: up to 1, less when a scan is cheap]
        #[arg(long, env = "SCRUTATE_REFRESH_INTERVAL")]
        refresh_interval: Option<f64>,
        /// also update the index from filesystem events, in the background
        #[arg(long)]
        watch: bool,
        /// also send each answer as JSON (`structuredContent`, with an `outputSchema` per tool), for
        /// clients whose scripts read tool results; a client that shows its model the JSON in
        /// place of the text loses the text's snippets
        #[arg(long, env = "SCRUTATE_MCP_STRUCTURED", value_parser = clap::builder::FalseyValueParser::new())]
        structured: bool,
        /// send short tool descriptions and no argument descriptions in the tool list, for fewer
        /// tokens in every session; the tools, their arguments and defaults stay the same
        #[arg(long, env = "SCRUTATE_MCP_SHORT_TOOLS", value_parser = clap::builder::FalseyValueParser::new())]
        short_tools: bool,
    },
    /// run a Language Server Protocol server on stdio (definitions, references, symbols from the index)
    Lsp,
    /// verify the index: database integrity, segment consistency, and (with --files) that every
    /// indexed file still has the content that was indexed
    Check {
        #[arg(long)]
        files: bool,
        #[arg(long)]
        json: bool,
    },
    /// print the configuration file in effect and the variables it sets
    Config {
        /// the root an index command run here uses and the configuration file, as JSON (for editors)
        #[arg(long)]
        json: bool,
    },
    /// list the languages this build parses structurally (--json: the capability registry)
    Languages {
        #[arg(long)]
        json: bool,
    },
    /// print shell completions
    Completions { shell: Shell },
    /// print the parse tree of a file (debugging grammar rules)
    #[command(hide = true)]
    Sexp { file: PathBuf },
    /// run many ranked queries from a JSON-lines file ({"query": ..., "k": 10, "alt": [...]})
    Batch {
        file: PathBuf,
        /// include every candidate with its score and features (`x`, in the order of `feature_names`)
        #[arg(long)]
        features: bool,
        #[arg(long)]
        timing: bool,
    },
    /// print the chunks of files with the tokens of each view, and the terms of queries, as the
    /// index and the vector model see them, from a JSON-lines file ({"file" or "text", "path"},
    /// {"query"}, {"split"} or {"stopwords": true}): the input of training and of the benchmarks
    #[command(hide = true)]
    Views { file: PathBuf },
    /// add the scrutate MCP server to an agent's configuration (or with --uninstall take it out)
    Setup {
        agent: crate::setup::Agent,
        /// the agent's file in the repository of the current directory, not the user's
        #[arg(long)]
        project: bool,
        /// remove exactly what setup added
        #[arg(long)]
        uninstall: bool,
        /// print each file it would change and the change; write nothing
        #[arg(long)]
        dry_run: bool,
        /// also the hook that answers Claude Code's Grep with scrutate's search
        #[arg(long)]
        hook: bool,
        /// also a short block in the agent's instruction file (CLAUDE.md, AGENTS.md, GEMINI.md)
        #[arg(long)]
        instructions: bool,
    },
    /// Claude Code's Grep hook (`hook grep`) and its counter of bytes (`hook stats`)
    Hook {
        #[command(subcommand)]
        cmd: HookCmd,
    },
}

#[derive(Subcommand)]
pub enum HookCmd {
    /// read a PreToolUse call of Grep on standard input and answer it, or print nothing so Grep runs
    Grep,
    /// the bytes Grep would have returned against the bytes the hook sent, per session and in all
    Stats {
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('K') | Some('k') => (&s[..s.len() - 1], 1024u64),
        Some('M') | Some('m') => (&s[..s.len() - 1], 1024 * 1024),
        Some('G') | Some('g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.parse::<u64>().ok().and_then(|n| n.checked_mul(mult)).with_context(|| format!("bad size {s:?}"))
}

/// The extensions a file of language `id` may have that another language shares: its text decides.
fn by_content(id: &str) -> Vec<&'static str> {
    crate::langs::BY_CONTENT.iter().filter(|(_, ids)| ids.contains(&id)).map(|(e, _)| *e).collect()
}

fn open_fresh(root: &Path) -> Result<crate::Repo> {
    let (s, st) = crate::open_updated(root, false)?;
    said_indexed(&st);
    Ok(s)
}

fn said_indexed(st: &crate::index::UpdateStats) {
    if st.files_changed > 0 || st.files_removed > 0 {
        eprintln!("[scrutate] indexed {} changed files ({} chunks, {:.1}s)", st.files_changed, st.chunks, st.seconds);
    }
}

fn color_choice(no_color: bool) -> ColorChoice {
    if no_color || !std::io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        ColorChoice::Never
    } else {
        ColorChoice::Auto
    }
}

/// True when the query looks like a regular expression rather than words or an identifier.
pub fn looks_like_regex(q: &str) -> bool {
    q.chars().any(|c| matches!(c, '*' | '+' | '?' | '[' | ']' | '(' | ')' | '|' | '^' | '$' | '\\' | '{' | '}'))
}

/// A comma-separated list of globs, split at the commas outside `{}`.
fn split_globs(list: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0usize);
    for c in list.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.retain(|g| !g.is_empty());
    out
}

const CONFIG_FILE: &str = ".scrutate.toml";

/// The directory whose configuration file applies: the nearest one holding the file, from `cwd`
/// up to the repository top (a directory holding `.git` or an index); `cwd` when there is none.
fn nearest_config(cwd: &Path) -> PathBuf {
    for dir in cwd.ancestors() {
        if dir.join(CONFIG_FILE).is_file() {
            return dir.to_path_buf();
        }
        if dir.join(".git").exists() || dir.join(crate::files::INDEX_DIR).is_dir() {
            break;
        }
    }
    cwd.to_path_buf()
}

/// The names of the directories from `top` down to `dir`; empty when `dir` is not below `top`.
fn names_below(dir: &Path, top: &Path) -> Vec<String> {
    dir.strip_prefix(top).map(|r| r.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect()).unwrap_or_default()
}

/// The directory whose index a command run in `cwd` uses, when that is not `cwd`. The index of a
/// repository is at its top (the first directory upward that holds `.git`: a directory, or a
/// file in a submodule or worktree): a first build lands there, and the top's index wins over
/// one an earlier run left in a subdirectory. Outside a repository: the nearest index above.
pub(crate) fn index_above(cwd: &Path) -> Option<PathBuf> {
    let has_index = |d: &Path| d.join(crate::files::INDEX_DIR).is_dir();
    let mut nearest = None;
    for dir in cwd.ancestors() {
        if nearest.is_none() && has_index(dir) {
            nearest = Some(dir);
        }
        if dir.join(".git").exists() {
            // a home directory kept in git is no project: nothing is built there from below
            let build_here = nearest.is_none() && !is_home(dir);
            if has_index(dir) || build_here {
                nearest = Some(dir);
            }
            break;
        }
    }
    nearest.filter(|d| *d != cwd).map(Path::to_path_buf)
}

/// `dir` is the home directory, the two compared with links resolved.
fn is_home(dir: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).is_some_and(|h| canonical(Path::new(&h)) == canonical(dir))
}

/// Why no first index is built in `dir`: it is the home directory or a filesystem root, so the
/// index would read everything below it. None when `dir` is neither, or already has an index.
pub(crate) fn no_first_build(dir: &Path) -> Option<String> {
    let wide = dir.parent().is_none() || is_home(dir);
    (wide && !dir.join(crate::files::INDEX_DIR).is_dir()).then(|| {
        format!(
            "[scrutate] {} is the home directory or a filesystem root and has no index: pass --root DIR for a project, or run `scrutate index` here to index everything below it",
            dir.display().to_string().trim_start_matches(r"\\?\")
        )
    })
}

/// Where a command runs in the repository: its path arguments are the shell's, relative to the
/// current directory, and the index names files from its root.
struct Here {
    cwd: PathBuf,
    /// the root as given, and in its canonical form
    root: PathBuf,
    canonical_root: PathBuf,
    /// the current directory is the root or below it
    inside: bool,
    /// the names of the directories from the root down to the current one; empty at the root or outside it
    below: Vec<String>,
}

/// What a path argument names from the current directory.
enum Named {
    /// nothing there: the argument is read as given (a path from the root, or a module name)
    Given,
    /// a file or directory of the root, by its path from the root
    Root(String),
    /// a file or directory outside the root, canonical
    Outside(PathBuf),
}

impl Here {
    fn new(cwd: Option<PathBuf>, root: &Path) -> Here {
        let cwd = cwd.unwrap_or_default();
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        // both in one form: a Windows temp directory may come as an 8.3 short name
        let canonical_cwd = std::fs::canonicalize(&cwd).unwrap_or_else(|_| cwd.clone());
        let below = names_below(&canonical_cwd, &canonical_root);
        Here { inside: canonical_cwd.starts_with(&canonical_root), cwd, root: root.to_path_buf(), canonical_root, below }
    }

    /// For a command that never asks: not inside the root, and nothing below it.
    fn unused() -> Here {
        Here { cwd: PathBuf::new(), root: PathBuf::new(), canonical_root: PathBuf::new(), inside: false, below: Vec::new() }
    }

    /// The current directory from the root, when it is below it.
    fn below(&self) -> Option<String> {
        (!self.below.is_empty()).then(|| self.below.join("/"))
    }

    fn named(&self, arg: &str) -> Named {
        let p = Path::new(arg);
        if arg.is_empty() || !self.cwd.join(p).exists() {
            return Named::Given;
        }
        // spelled from here, so a symbolic link inside the repository keeps its name
        let from_root = if self.inside && !p.is_absolute() { PathBuf::from(self.below.join("/")).join(p) } else { self.cwd.join(p) };
        if let Some(rel) = crate::ops::repo_rel(&self.canonical_root, &from_root) {
            return Named::Root(rel);
        }
        match std::fs::canonicalize(self.cwd.join(p)) {
            Ok(abs) => Named::Outside(abs),
            Err(_) => Named::Given,
        }
    }

    /// `arg` from the root when it names a file or directory of the root from here; otherwise as
    /// given (a path from the root, as answers print it, or a module name).
    fn path(&self, arg: &str) -> String {
        match self.named(arg) {
            // the root itself (`..` from a directory below it)
            Named::Root(r) if r.is_empty() => ".".to_string(),
            Named::Root(r) => r,
            _ => arg.to_string(),
        }
    }

    /// A location (`path:line`) with its path read as `path` reads one; a name as given.
    fn location(&self, arg: &str) -> String {
        match arg.rsplit_once(':') {
            Some((path, line)) if crate::intel::location(arg).is_some() => format!("{}:{line}", self.path(path)),
            _ => arg.to_string(),
        }
    }

    /// A path to walk: as given when it exists from here, else the same path from the root when
    /// that exists (as answers print it).
    fn shell(&self, p: &Path) -> PathBuf {
        if p.is_absolute() || self.cwd.join(p).exists() {
            return p.to_path_buf();
        }
        let up: PathBuf = if self.inside { self.below.iter().map(|_| "..").collect() } else { self.root.clone() };
        if self.cwd.join(&up).join(p).exists() {
            up.join(p)
        } else {
            p.to_path_buf()
        }
    }
}

/// The file an index command's argument names: a path, or the path of a location.
fn named_file(cmd: &Cmd) -> Option<&str> {
    fn loc(s: &str) -> Option<&str> {
        crate::intel::location(s).map(|(p, _)| p)
    }
    match cmd {
        Cmd::Symbol { name, .. } | Cmd::Callers { name, .. } | Cmd::Callees { name, .. } => loc(name),
        Cmd::Similar { target, .. } => loc(target),
        Cmd::CallPath { from, to, .. } => loc(from).or(loc(to)),
        Cmd::Deps { file, .. } | Cmd::Outline { file, .. } => Some(file),
        Cmd::Rdeps { target, .. } => Some(target),
        Cmd::Refs { def: Some(d), .. } => loc(d).or(Some(d)),
        Cmd::Read { path: Some(p), .. } => Some(p),
        Cmd::Read { symbol: Some(s), .. } => loc(s),
        _ => None,
    }
}

pub fn run() -> Result<i32> {
    // configuration file -> environment -> clap (command line still wins)
    let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    // before the configuration file, which can stop a command: the hook must never fail
    if args.len() == 3 && args[1] == "hook" && args[2] == "grep" {
        return Ok(crate::hook::grep_main());
    }
    let cwd = std::env::current_dir().ok();
    let root_given = std::env::var_os("SCRUTATE_ROOT").is_some() || args.iter().any(|a| a == "--root" || a.starts_with("--root="));
    let cfg_root = match &cwd {
        Some(cwd) if !root_given => nearest_config(cwd),
        _ => crate::config::root_from_args(&args),
    };
    let cfg_path = crate::config::locate(&cfg_root);
    let local = cfg_path.as_deref() == Some(cfg_root.join(CONFIG_FILE).as_path());
    // the globs of a .scrutate.toml match from its directory, so they do not go through the
    // environment, where -g's meaning (from each searched path) would apply
    let mut cfg_globs = Vec::new();
    let mut cfg_excludes = false;
    // a bad file stops the commands that read it, after clap has had --help and --version
    let cfg_error = cfg_path.as_deref().and_then(|p| {
        crate::config::load(p)
            .map(|mut cfg| {
                cfg_excludes = !cfg.index.exclude_dirs.is_empty();
                if local {
                    cfg_globs = std::mem::take(&mut cfg.grep.globs);
                    crate::config::confine(&mut cfg);
                }
                crate::config::apply_as_env(&cfg)
            })
            .err()
    });
    // as with every other setting, the environment and the command line replace the file's globs
    let cfg_globs = if std::env::var_os("SCRUTATE_GREP_GLOBS").is_none() { cfg_globs } else { Vec::new() };
    let mut cli = if matches!(subcommand(&args), Some("grep" | "g")) {
        // a setting from a file, or from a variable clap reads for grep, is clap's to read
        let settings = cfg_path.is_some() || GREP_ENV.iter().any(|name| std::env::var_os(name).is_some());
        let plain = (!settings && matches!(args.get(1).map(String::as_str), Some("grep" | "g"))).then(|| plain_grep(&args)).flatten();
        let (root, no_color, max_bytes, offset, grep) = match plain {
            Some((no_color, grep)) => (PathBuf::from("."), no_color, None, 0, grep),
            None => {
                let GrepCli { root, no_color, max_bytes, offset, cmd: GrepCmd::Grep(grep) } = GrepCli::parse();
                if grep.regexp.is_empty() && grep.pattern.as_ref().is_some_and(|p| p.to_str().is_none()) {
                    // clap's own error for a `String` argument
                    let mut cmd = GrepCli::command();
                    cmd.build();
                    let grep = cmd.find_subcommand_mut("grep").expect("the grep subcommand");
                    grep.error(clap::error::ErrorKind::InvalidUtf8, "invalid UTF-8 was detected in one or more arguments").exit();
                }
                (root, no_color, max_bytes, offset, grep)
            }
        };
        Cli { root, no_color, max_bytes, offset, cmd: Cmd::Grep(grep) }
    } else {
        Cli::parse()
    };
    if let Some(e) = cfg_error {
        if !matches!(cli.cmd, Cmd::Config { .. } | Cmd::Completions { .. }) {
            return Err(e);
        }
        diagnostic("warning", &format!("{e:#}"));
    }
    let uses_index = match &cli.cmd {
        Cmd::Grep(g) => g.index || g.symbols,
        // the language server applies the rule to the editor's folder
        Cmd::Config { .. }
        | Cmd::Completions { .. }
        | Cmd::Languages { .. }
        | Cmd::Sexp { .. }
        | Cmd::Watch { path: Some(_) }
        | Cmd::Lsp
        | Cmd::Setup { .. }
        | Cmd::Hook { .. } => false,
        _ => true,
    };
    let mut found_index = None;
    if uses_index && !root_given {
        if let Some(top) = cwd.as_deref().and_then(index_above) {
            cli.root = top.clone();
            found_index = Some(top);
        }
    }
    // a search without the index never looks at the root: two canonical paths, 0.3 M instructions
    let mut here = match &cli.cmd {
        Cmd::Grep(g) if !g.index => Here::unused(),
        _ => Here::new(cwd.clone(), &cli.root),
    };
    // a file of another indexed repository (a path `search --also` printed) is answered from its index
    let mut outside = None;
    if let Some(arg) = named_file(&cli.cmd) {
        if let Named::Outside(abs) = here.named(arg) {
            match abs.ancestors().find(|d| d.join(crate::files::INDEX_DIR).is_dir()) {
                Some(top) => {
                    cli.root = top.to_path_buf();
                    found_index = Some(top.to_path_buf());
                    here = Here::new(cwd.clone(), &cli.root);
                }
                None => outside = Some(format!("{} is outside the repository", crate::fmt::echo(arg))),
            }
        }
    }
    // `index` there is the user's stated wish; any other first build there is not
    if uses_index && !root_given && found_index.is_none() && !matches!(cli.cmd, Cmd::Index { .. }) {
        if let Some(why) = cwd.as_deref().and_then(no_first_build) {
            eprintln!("{why}");
            return Ok(2);
        }
    }
    if let Some(top) = found_index {
        eprintln!("[scrutate] using the index in {}", top.display().to_string().trim_start_matches(r"\\?\"));
    }
    // every process that updates an index must leave out the same directories, so only the root's own file counts
    if cfg_excludes && uses_index && std::env::var_os("SCRUTATE_INDEX_EXCLUDE_DIRS").is_none() {
        let own = cli.root.join(CONFIG_FILE);
        let same = |a: &Path, b: &Path| std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b);
        if let Some(p) = cfg_path.as_deref().filter(|p| !same(p, &own)) {
            diagnostic(
                "warning",
                &format!("exclude_dirs in {} is ignored: the index reads it from {} (or SCRUTATE_INDEX_EXCLUDE_DIRS)", p.display(), own.display()),
            );
        }
    }
    // a bad language table stops the command, as a bad configuration file does
    let wants_langs = match &cli.cmd {
        Cmd::Grep(g) => g.index || g.symbols || !g.types.is_empty() || g.type_list,
        Cmd::Config { .. } | Cmd::Completions { .. } | Cmd::Setup { .. } | Cmd::Hook { .. } => false,
        _ => true,
    };
    if wants_langs {
        let root = match (uses_index || root_given, &cwd) {
            (false, Some(cwd)) => index_above(cwd).unwrap_or_else(|| cwd.clone()),
            _ => cli.root.clone(),
        };
        crate::custom::load(&root)?;
    }
    let no_color = cli.no_color;
    let page = (cli.max_bytes.unwrap_or(usize::MAX), cli.offset);
    match cli.cmd {
        Cmd::Grep(GrepArgs {
            pattern,
            mut paths,
            regexp,
            index,
            update,
            fixed_strings,
            pcre2,
            fuzzy_regex,
            search_zip,
            ident,
            fuzzy,
            ignore_case,
            smart_case,
            word,
            multiline,
            invert_match,
            files_with_matches,
            count,
            with_filename,
            no_filename,
            line_number: _,
            no_line_number,
            after,
            before,
            context,
            max_count,
            max_results,
            globs,
            types,
            type_list,
            hidden,
            no_ignore,
            follow,
            max_filesize,
            max_depth,
            threads,
            sort,
            json,
            stats,
            symbols,
        }) => {
            // read here, not by clap: with an environment value clap rejects -i as conflicting
            let smart_case = smart_case
                || !ignore_case
                    && std::env::var("SCRUTATE_GREP_SMART_CASE")
                        .is_ok_and(|v| !matches!(v.to_ascii_lowercase().as_str(), "" | "0" | "n" | "no" | "f" | "false" | "off"));
            let scoped_globs = if globs.is_empty() && !cfg_globs.is_empty() { Some(Arc::new(instant::ScopedGlobs::new(&cfg_root, &cfg_globs)?)) } else { None };
            let globs = if globs.is_empty() { std::env::var("SCRUTATE_GREP_GLOBS").map(|v| split_globs(&v)).unwrap_or_default() } else { globs };
            if type_list {
                let mut so = std::io::stdout().lock();
                for (name, globs) in instant::type_list() {
                    match writeln!(so, "{name}: {}", globs.join(", ")) {
                        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => break,
                        r => r?,
                    }
                }
                return Ok(0);
            }
            if pcre2 && (ident || fuzzy.is_some()) {
                anyhow::bail!("-P/--pcre2 cannot be combined with --ident or --fuzzy");
            }
            if fuzzy_regex.is_some() && (pcre2 || ident || fuzzy.is_some() || fixed_strings) {
                anyhow::bail!("--fuzzy-regex cannot be combined with -P, --ident, --fuzzy or -F (use --fuzzy for a literal)");
            }
            let mode = match (fixed_strings, ident, fuzzy) {
                _ if fuzzy_regex.is_some() => Mode::FuzzyRegex { max_edits: fuzzy_regex.unwrap_or(1) },
                (_, _, Some(n)) => Mode::Fuzzy { max_edits: n },
                (_, true, _) => Mode::Identifier,
                _ if pcre2 => Mode::Pcre2,
                (true, _, _) => Mode::Literal,
                _ => Mode::Regex,
            };
            let (pattern, mode) = if regexp.is_empty() {
                instant::join_patterns(&[pattern.and_then(|p| p.into_string().ok()).unwrap_or_default()], mode, fixed_strings)?
            } else {
                // with -e, the first positional argument is a path
                if let Some(p) = pattern {
                    paths.insert(0, PathBuf::from(p));
                }
                instant::join_patterns(&regexp, mode, fixed_strings)?
            };
            // as ripgrep: no path, and a pipe or a file on standard input, searches standard input
            if paths.is_empty() && !index && grep_cli::is_readable_stdin() {
                paths.push(PathBuf::from("-"));
            }
            let case = if ignore_case {
                Case::Insensitive
            } else if smart_case {
                Case::Smart
            } else {
                Case::Sensitive
            };
            let q = Query {
                pattern: Pattern { text: pattern, mode, case, word, multiline, ..Pattern::default() },
                invert: invert_match,
                // -c with -l prints the count of the one match -l stops at
                report: if files_with_matches {
                    Report::Files
                } else if count {
                    Report::Counts
                } else {
                    Report::Lines
                },
                max_count,
                max_results,
                first_in_path_order: sort,
                before: before.or(context).unwrap_or(0),
                after: after.or(context).unwrap_or(0),
                want_bytes: stats || json,
                want_spans: json || color_choice(no_color) != ColorChoice::Never,
                text_spans: true,
                raw_member_names: json,
                // as ripgrep: one path that is not a directory is searched without its name; with
                // -z an archive is a directory of its members
                with_filename: with_filename
                    || (!no_filename
                        && !(paths.len() == 1
                            && !paths[0].is_dir()
                            && !(search_zip && crate::archive::Kind::of(&paths[0].to_string_lossy()).is_some_and(|k| k.has_members())))),
                line_numbers: !no_line_number,
            };
            let mut f = Files {
                hidden,
                no_ignore,
                follow_links: follow,
                globs,
                scoped_globs,
                types,
                max_filesize: max_filesize.as_deref().map(parse_size).transpose()?,
                max_depth,
                threads,
                search_zip,
                paths_as_given: !paths.is_empty(),
                small_tree: std::env::var("SCRUTATE_SMALL_TREE").ok().and_then(|v| v.parse().ok()),
                cwd: cwd.clone(),
                ..Files::default()
            };
            if let Some(n) = std::env::var("SCRUTATE_SPLIT_FILE_BYTES").ok().and_then(|v| v.parse().ok()) {
                f.split_file_bytes = n;
            }
            if index {
                f.check_indexable()?;
                if !paths.is_empty() {
                    anyhow::bail!("--index searches the indexed repository (--root); narrow it with -g/-t instead of paths");
                }
            }
            let roots: Vec<PathBuf> = if paths.is_empty() { vec![PathBuf::from(".")] } else { paths };
            f.paths = Paths::Walk(roots);
            let under = here.below();
            // a bounded answer is the same on every call, so --offset neither repeats nor skips a line
            let page = (!json && (cli.max_bytes.is_some() || cli.offset > 0)).then_some((cli.max_bytes.unwrap_or(usize::MAX), cli.offset));
            let (q, sort) = if page.is_some() { (Query { first_in_path_order: true, ..q }, true) } else { (q, sort) };
            grep_command(
                q,
                f,
                GrepOutput { files_with_matches, count, sort, json, stats, symbols, root: cli.root, no_color, indexed: index, update, under, page },
            )
        }
        Cmd::Index { force, text, no_text } => {
            let w = match (text, no_text) {
                (true, _) => Writer::IndexText,
                (_, true) => Writer::IndexCode,
                _ => Writer::Index,
            };
            let up = crate::stores::update(&cli.root, w, force)?;
            if let Some(st) = up.code {
                println!("{} files, {} changed, {} chunks, semantic={}, {:.1}s", st.files_total, st.files_changed, st.chunks, st.semantic, st.seconds);
            }
            if let Some(t) = up.text {
                println!(
                    "text index: {} files, {} changed, {} removed, {} segment(s), {:.1} MB, {:.1}s",
                    t.files,
                    t.files_changed,
                    t.files_removed,
                    t.segments,
                    t.bytes as f64 / 1e6,
                    t.seconds
                );
            }
            Ok(0)
        }
        Cmd::Stats => {
            let s = open_fresh(&cli.root)?;
            let mut v = s.ix.stats()?;
            if let Some(t) = crate::textindex::TextIndex::open(&s.ix.root)? {
                v["text_index"] =
                    serde_json::json!({"files": t.files(), "segments": t.segments(), "bytes": crate::textindex::TextIndex::disk_bytes(&s.ix.root)});
            }
            println!("{}", serde_json::to_string(&v)?);
            Ok(0)
        }
        Cmd::Search { query, alt, also, k, why, json } => {
            let mut qs = vec![query.clone()];
            qs.extend(alt.iter().cloned());
            // each repository once, whatever the spelling; the first is the current one
            let mut roots: Vec<(PathBuf, PathBuf)> = Vec::new();
            for root in std::iter::once(&cli.root).chain(&also) {
                let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
                if !roots.iter().any(|(_, c)| *c == canonical) {
                    roots.push((root.clone(), canonical));
                }
            }
            if roots.len() == 1 {
                let op = Op::Search { query, alt, k, snippets: true };
                return run_request(&cli.root, crate::session::Request { op, json, max_bytes: page.0, offset: page.1, outside: None, why });
            }
            // (hit, its repository) with the path a reader opens from here: another repository's
            // results carry its path as given
            let mut hits: Vec<(crate::search::Hit, usize)> = Vec::new();
            for (i, (root, _)) in roots.iter().enumerate() {
                let op = Op::Search { query: query.clone(), alt: alt.clone(), k, snippets: true };
                let crate::ops::Answer::Hits { hits: part, .. } = open_fresh(root)?.run_as_is(&op)? else { unreachable!("a search answers hits") };
                hits.extend(part.into_iter().map(|h| (h, i)));
            }
            let shown = |(h, i): &(crate::search::Hit, usize)| {
                if *i == 0 {
                    h.path.clone()
                } else {
                    format!("{}/{}", roots[*i].0.to_string_lossy().trim_end_matches('/'), h.path)
                }
            };
            if roots.len() > 1 {
                // the ranker's scores are comparable across repositories (same model, same features)
                hits.sort_by(|a, b| b.0.score.partial_cmp(&a.0.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| shown(a).cmp(&shown(b))));
                hits.truncate(k);
            }
            let other = hits.iter().find(|(_, i)| *i > 0).map(|x| format!("{}:{}-{}", shown(x), x.0.start, x.0.end));
            if json && roots.len() > 1 {
                let plain: Vec<crate::search::Hit> = hits.iter().map(|(h, _)| h.clone()).collect();
                let mut v = hits_json(&plain);
                for (rec, (_, i)) in v.as_array_mut().into_iter().flatten().zip(&hits) {
                    rec["root"] = serde_json::json!(roots[*i].1.to_string_lossy());
                }
                println!("{}", serde_json::to_string_pretty(&crate::ops::envelope(v, None))?);
            } else {
                let hits: Vec<crate::search::Hit> = hits.iter().map(|x| crate::search::Hit { path: shown(x), ..x.0.clone() }).collect();
                if json || hits.is_empty() {
                    print_hits(&hits, &qs, why, json)?;
                } else {
                    let words: Vec<String> = qs.iter().flat_map(|q| query_words(q)).collect();
                    println!("{}", crate::ops::paged(crate::fmt::hit_blocks(&hits, &words, true, why), "results", text_room(page.0), page.1));
                }
            }
            if let (Some(other), false) = (other, json) {
                eprintln!("[scrutate] a location of another repository is answered from its own index as printed (scrutate symbol {other})");
            }
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        Cmd::Check { files, json } => check_command(&cli.root, files, json),
        Cmd::Config { json: true } => {
            let root = match (root_given, &cwd) {
                (false, Some(cwd)) => index_above(cwd).unwrap_or_else(|| cwd.clone()),
                _ => std::path::absolute(&cli.root).unwrap_or(cli.root),
            };
            println!("{}", serde_json::json!({"root": root.to_string_lossy(), "configuration_file": cfg_path.map(|p| p.to_string_lossy().into_owned())}));
            Ok(0)
        }
        Cmd::Config { json: false } => {
            match &cfg_path {
                Some(p) => {
                    println!("configuration file: {}", p.display());
                    println!("{}", std::fs::read_to_string(p)?.trim_end());
                    if !cfg_globs.is_empty() {
                        println!("\ngrep globs, matched from {}: {}", cfg_root.display().to_string().trim_start_matches(r"\\?\"), cfg_globs.join(", "));
                    }
                }
                None if root_given => println!(
                    "no configuration file (looked for $SCRUTATE_CONFIG, {}/{CONFIG_FILE}, $XDG_CONFIG_HOME/scrutate/config.toml)",
                    cfg_root.display()
                ),
                None => println!(
                    "no configuration file (looked for $SCRUTATE_CONFIG, {CONFIG_FILE} here and in each directory above up to the repository top, $XDG_CONFIG_HOME/scrutate/config.toml)"
                ),
            }
            println!("\nenvironment in effect:");
            let mut vars: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("SCRUTATE_")).collect();
            vars.sort();
            for (k, v) in vars {
                println!("  {k}={v}");
            }
            let root = match (root_given, &cwd) {
                (false, Some(cwd)) => index_above(cwd).unwrap_or_else(|| cwd.clone()),
                _ => cli.root.clone(),
            };
            let excluded = crate::files::config_excludes(&root);
            if std::env::var_os("SCRUTATE_INDEX_EXCLUDE_DIRS").is_none() && !excluded.is_empty() {
                println!("\nindex exclude_dirs, from {}: {}", root.join(CONFIG_FILE).display(), excluded.join(", "));
            }
            Ok(0)
        }
        Cmd::Find { query, k, json } => find_command(&cli.root, &query, k, json, no_color),
        Cmd::Symbol { name, limit, json } => run_op(&cli.root, Op::Symbol { name: here.location(&name), limit }, json, page, outside.as_deref()),
        Cmd::Callers { name, limit, json } => run_op(&cli.root, Op::Callers { name: here.location(&name), limit }, json, page, outside.as_deref()),
        Cmd::Callees { name, limit, json } => run_op(&cli.root, Op::Callees { name: here.location(&name), limit }, json, page, outside.as_deref()),
        Cmd::CallPath { from, to, depth, json } => {
            run_op(&cli.root, Op::Path { from: here.location(&from), to: here.location(&to), depth }, json, page, outside.as_deref())
        }
        Cmd::Map { path, json } => run_op(&cli.root, Op::Map { path: path.map(|p| here.path(&p)) }, json, page, outside.as_deref()),
        Cmd::Deps { file, json } => run_op(&cli.root, Op::Deps { path: here.path(&file) }, json, page, outside.as_deref()),
        Cmd::Rdeps { target, json } => run_op(&cli.root, Op::Rdeps { target: here.path(&target) }, json, page, outside.as_deref()),
        Cmd::Outline { file, json } => run_op(&cli.root, Op::Outline { path: here.path(&file) }, json, page, outside.as_deref()),
        Cmd::Refs { name, json, max_results, def, textual } => {
            let def = def.map(|d| if crate::intel::location(&d).is_some() { here.location(&d) } else { here.path(&d) });
            run_op(&cli.root, Op::Refs { name, max_results, textual, def }, json, page, outside.as_deref())
        }
        Cmd::Similar { target, k, json } => run_op(&cli.root, Op::Similar { target: here.location(&target), k }, json, page, outside.as_deref()),
        Cmd::Read { path, symbol, start, end, json } => {
            // the read_code tool's arguments, so the operation checks them as it checks a call
            let args = serde_json::json!({"path": path.map(|p| here.path(&p)), "symbol": symbol.map(|s| here.location(&s)), "start": start, "end": end});
            run_op(&cli.root, Op::from_tool("read_code", &args)?, json, page, outside.as_deref())
        }
        Cmd::Dupes { min_tokens, threshold, limit, json } => run_op(&cli.root, Op::Dupes { min_tokens, threshold, limit }, json, page, outside.as_deref()),
        Cmd::Impact { base, staged, depth, json } => run_op(&cli.root, Op::Impact { base, staged, depth: depth as usize }, json, page, None),
        Cmd::Review { base, head, format, depth, link } => {
            let r = open_fresh(&cli.root)?.recovering(|s| crate::review::review(s, &base, Some(&head), depth as usize))?;
            match format {
                ReviewFormat::Markdown => print!("{}", r.markdown(link.as_deref())),
                ReviewFormat::Json => {
                    let stopped = r.impact.stopped.then_some(crate::ops::Stop { at: crate::impact::MAX_CALLERS, how: "pass a smaller depth" });
                    println!("{}", serde_json::to_string_pretty(&crate::ops::envelope(r.json(), stopped))?)
                }
                ReviewFormat::Gitlab => println!("{}", serde_json::to_string_pretty(&r.gitlab())?),
            }
            Ok(i32::from(r.is_empty()))
        }
        Cmd::Files { query, limit, json } => {
            crate::files::root_dir(&cli.root)?;
            run_op(&cli.root, Op::Files { query, limit: limit as usize }, json, page, None)
        }
        Cmd::Tsquery { lang, query, paths, rewrite: Some(template), write, .. } => {
            crate::files::root_dir(&cli.root)?;
            if cli.offset != 0 {
                anyhow::bail!("--offset does nothing with --rewrite: the diff is printed whole");
            }
            let paths: Vec<PathBuf> = paths.iter().map(|p| here.shell(p)).collect();
            let mut trouble = false;
            for p in &paths {
                if let Err(e) = std::fs::metadata(p) {
                    diagnostic("warning", &format!("{}: {e}", p.display()));
                    trouble = true;
                }
            }
            let plan = crate::rewrite::plan(&lang, &query, &template, &paths, &cli.root)?;
            plan.skipped.iter().for_each(|s| diagnostic("warning", s));
            let mut left = plan.left.clone();
            let mut w = std::io::BufWriter::new(std::io::stdout().lock());
            if write {
                for (f, why) in plan.files.iter().zip(plan.write()) {
                    match why {
                        Some(why) => left.push(why),
                        None => writeln!(w, "{}: {} replaced", f.shown, f.replaced)?,
                    }
                }
            } else if let Err(e) = w.write_all(plan.diff().as_bytes()) {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(e.into());
                }
            }
            let _ = w.flush();
            left.iter().for_each(|s| diagnostic("warning", s));
            trouble |= !left.is_empty();
            Ok(if trouble {
                2
            } else if plan.files.is_empty() {
                1
            } else {
                0
            })
        }
        Cmd::Dead { path, json } => run_op(&cli.root, Op::Dead { path: path.map(|p| here.path(&p)) }, json, page, outside.as_deref()),
        Cmd::Tsquery { lang, query, paths, limit, json, no_cache, .. } => {
            crate::files::root_dir(&cli.root)?;
            // streamed: one JSON object or one text line per capture, in path order
            let stdout = std::io::stdout();
            let mut w = std::io::BufWriter::new(stdout.lock());
            let mut err: Option<std::io::Error> = None;
            let cache_root = (!no_cache).then_some(cli.root.as_path());
            let paths: Vec<PathBuf> = paths.iter().map(|p| here.shell(p)).collect();
            // as grep: a path that cannot be read is named, the others are still searched, exit 2
            let mut missing = false;
            for p in &paths {
                if let Err(e) = std::fs::metadata(p) {
                    diagnostic("warning", &format!("{}: {e}", p.display()));
                    missing = true;
                }
            }
            // with a size limit or an offset the lines are collected and paged as other answers are
            let paging = !json && page != (usize::MAX, 0);
            let mut lines: Vec<String> = Vec::new();
            // one more than the limit, to tell whether it cut the answer
            let mut seen = 0usize;
            let (count, cancel) = (crate::index::FileCount::default(), std::sync::atomic::AtomicBool::new(false));
            let watch = crate::intel::Watch { count: &count, cancel: &cancel };
            let n = crate::intel::tsquery_each(&lang, &query, &paths, true, limit.saturating_add(1), cache_root, watch, &mut |c| {
                seen += 1;
                if err.is_some() || seen > limit {
                    return;
                }
                if paging {
                    lines.push(crate::ops::capture_line(&c));
                    return;
                }
                let r = if json {
                    serde_json::to_writer(&mut w, &c)
                        .map_err(|e| std::io::Error::new(e.io_error_kind().unwrap_or(std::io::ErrorKind::Other), e))
                        .and_then(|_| w.write_all(b"\n"))
                } else {
                    writeln!(w, "{}", crate::ops::capture_line(&c))
                };
                if let Err(e) = r {
                    err = Some(e);
                }
            })?;
            if paging && !lines.is_empty() {
                err = writeln!(w, "{}", crate::ops::paged(lines, "matches", text_room(page.0), page.1)).err();
            }
            if n > limit && err.is_none() {
                let note = crate::ops::stop_line(Some(crate::ops::Stop { at: limit, how: crate::ops::RAISE_LIMIT }), "matches");
                if json {
                    eprintln!("{note}");
                } else {
                    err = writeln!(w, "{note}").err();
                }
            }
            if let Some(e) = err {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(e.into());
                }
            }
            let _ = w.flush();
            Ok(if missing {
                2
            } else if n == 0 {
                1
            } else {
                0
            })
        }
        Cmd::Serve { refresh_interval, watch, structured, short_tools } => {
            if cfg!(target_os = "wasi") {
                anyhow::bail!("serve is not in the wasi build: it needs threads");
            }
            if cli.offset != 0 {
                anyhow::bail!("--offset does nothing with serve: a tool call takes its own offset");
            }
            let s = Arc::new(Session::open(&cli.root, crate::session::Every::given(refresh_interval))?);
            // command line calls of this user may use the session until the server exits; before
            // any thread starts, as a long socket path is named from its directory
            let _listening = crate::session::listen(&s, printed);
            if watch {
                let s = Arc::clone(&s);
                std::thread::Builder::new().name("scrutate-watch".into()).spawn(move || {
                    static NEVER: AtomicBool = AtomicBool::new(false);
                    let root = s.root().to_path_buf();
                    // the session's writer lock orders this with its other writes; the index's own
                    // locks order it with other processes
                    let r = crate::watch::run(
                        &root,
                        &NEVER,
                        || eprintln!("[scrutate] watching {}", root.display()),
                        |changes| s.write(|repo| repo.update_stores(Writer::Watch, changes, false)).map(|_| ()),
                    );
                    if let Err(e) = r {
                        eprintln!("[scrutate] watching stopped: {e:#}");
                    }
                })?;
            }
            crate::mcp::serve(s, crate::mcp::Settings::new(cli.max_bytes), crate::VERSION, structured, short_tools)?;
            Ok(0)
        }
        Cmd::Lsp => crate::lsp::serve(&cli.root, root_given),
        Cmd::Watch { path } => watch_command(path.as_deref().unwrap_or(&cli.root)),
        Cmd::Languages { json: true } => {
            // the verified capability registry: what each grammar's rules extract
            let v: Vec<serde_json::Value> = crate::langs::LANGS
                .iter()
                .map(|spec| {
                    let kinds = crate::langs::definition_kinds(spec);
                    serde_json::json!({
                        "id": spec.id, "extensions": spec.exts, "extensions_by_content": by_content(spec.id), "filenames": spec.files,
                        "definitions": !spec.defs.is_empty(), "definition_kinds": kinds,
                        "call_edges": !spec.calls.is_empty(), "imports": !spec.imports.is_empty(),
                        "docstrings": spec.docstring, "tier": "grammar"
                    })
                })
                .chain(crate::custom::current().langs.iter().map(|l| {
                    serde_json::json!({
                        "id": l.id, "extensions": l.exts, "extensions_by_content": [], "filenames": [],
                        "definitions": !l.defs.is_empty(), "definition_kinds": l.kinds(),
                        "call_edges": false, "imports": false, "docstrings": false, "tier": "custom", "source": l.source
                    })
                }))
                .collect();
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(0)
        }
        Cmd::Languages { json: false } => {
            let mut so = std::io::stdout().lock();
            for spec in crate::langs::LANGS.iter() {
                let labelled = |label: &str, v: &[&str]| (!v.is_empty()).then(|| format!("{label}{}", v.join(" ")));
                let parts = [labelled("", spec.exts), labelled("by content: ", &by_content(spec.id)), labelled("files: ", spec.files)];
                match writeln!(so, "{:<16} {}", spec.id, parts.into_iter().flatten().collect::<Vec<_>>().join("  ")) {
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(0),
                    r => r?,
                }
            }
            for l in crate::custom::current().langs.iter() {
                match writeln!(so, "{:<16} {}  custom: {}", l.id, l.exts.join(" "), l.source) {
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => break,
                    r => r?,
                }
            }
            Ok(0)
        }
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            let sh = match shell {
                Shell::Bash => clap_complete::Shell::Bash,
                Shell::Zsh => clap_complete::Shell::Zsh,
                Shell::Fish => clap_complete::Shell::Fish,
                Shell::Powershell => clap_complete::Shell::PowerShell,
                Shell::Elvish => clap_complete::Shell::Elvish,
            };
            clap_complete::generate(sh, &mut cmd, name, &mut std::io::stdout());
            Ok(0)
        }
        Cmd::Sexp { file } => {
            let source = std::fs::read_to_string(&file)?;
            let rel = file.to_string_lossy().into_owned();
            match crate::parse::spec_for(&rel, &source) {
                Some(spec) => {
                    println!("grammar: {}", spec.id);
                    println!("{}", crate::parse::sexp(spec, &source));
                }
                None => println!("no grammar for {rel} (heuristic symbols)"),
            }
            for c in crate::parse::parse_source(&rel, &source) {
                println!(
                    "CHUNK {:<9} {:<30} L{}-{} doc={:?} calls={:?}",
                    c.kind,
                    c.qualname,
                    c.start,
                    c.end,
                    c.doc.lines().next().unwrap_or(""),
                    c.calls.iter().take(6).collect::<Vec<_>>()
                );
            }
            Ok(0)
        }
        Cmd::Batch { file, features, timing } => batch_command(&cli.root, &file, features, timing),
        Cmd::Views { file } => views_command(&file),
        Cmd::Setup { agent, project, uninstall, dry_run, hook, instructions } => {
            crate::setup::run(agent, crate::setup::Options { project, uninstall, dry_run, hook, instructions })
        }
        Cmd::Hook { cmd: HookCmd::Grep } => Ok(crate::hook::grep_main()),
        Cmd::Hook { cmd: HookCmd::Stats { json } } => crate::hook::stats(json),
    }
}

/// One line per request: `{"file", "path"}` reads `file` and parses it as `path` (its language
/// and module come from `path`), `{"text", "path"}` parses `text` so; `{"query"}` gives the
/// query's terms, `{"split"}` a name's subwords and `{"stopwords": true}` the query stop list.
fn views_command(file: &Path) -> Result<i32> {
    use crate::text::{query_terms, raw_subwords, tokenize2, BODY_CAP, QUERY_STOP};
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in std::io::BufReader::new(std::fs::File::open(file)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: serde_json::Value = serde_json::from_str(&line)?;
        let s = |k: &str| req.get(k).and_then(|v| v.as_str());
        if let Some(q) = s("query") {
            writeln!(out, "{}", serde_json::json!({"query": q, "terms": query_terms(q)}))?;
            continue;
        }
        if let Some(name) = s("split") {
            writeln!(out, "{}", serde_json::json!({"split": name, "subwords": raw_subwords(name)}))?;
            continue;
        }
        if req.get("stopwords").is_some() {
            let mut stop: Vec<&str> = QUERY_STOP.iter().copied().collect();
            stop.sort_unstable();
            writeln!(out, "{}", serde_json::json!({"stopwords": stop}))?;
            continue;
        }
        let Some(rel) = s("path").filter(|_| s("file").is_some() || s("text").is_some()) else {
            anyhow::bail!("a request needs \"query\", \"split\", \"stopwords\", or \"file\" or \"text\" with \"path\": {line}")
        };
        let text = match (s("text"), s("file")) {
            (Some(text), _) => Ok(text.to_string()),
            (None, Some(src)) => std::fs::read(src).map_err(anyhow::Error::from).and_then(|raw| String::from_utf8(raw).map_err(anyhow::Error::from)),
            (None, None) => unreachable!(),
        };
        let lang = text.as_ref().ok().and_then(|t| crate::parse::spec_for(rel, t)).map(|spec| spec.id);
        let parsed =
            text.and_then(|text| std::panic::catch_unwind(|| crate::parse::parse_source(rel, &text)).map_err(|_| anyhow::anyhow!("the parser panicked")));
        let rec = match parsed {
            Ok(chunks) => {
                let chunks: Vec<serde_json::Value> = chunks
                    .iter()
                    .map(|c| {
                        let qual = c.qualname.rfind('.').map_or("", |i| &c.qualname[..i]);
                        serde_json::json!({
                            "kind": c.kind, "name": c.name, "qualname": c.qualname, "start": c.start, "doc": c.doc,
                            "views": {
                                "name": tokenize2(&c.name, None).0,
                                "qual": tokenize2(qual, None).0,
                                "sig": tokenize2(&c.sig, Some(80)).0,
                                "doc": tokenize2(&c.doc, Some(200)).0,
                                "body": tokenize2(&c.body, Some(BODY_CAP)).0,
                            }
                        })
                    })
                    .collect();
                serde_json::json!({"path": rel, "lang": lang, "test": crate::text::is_test_path(rel), "chunks": chunks})
            }
            Err(e) => serde_json::json!({"path": rel, "error": format!("{e:#}")}),
        };
        writeln!(out, "{rec}")?;
    }
    Ok(0)
}

/// Run one index operation and print its answer; exit code 1 when nothing was found. `outside`:
/// why an argument names nothing in this index, said with an empty answer or an error.
/// What a paged text may take, so that it and the newline the command line ends it with fit the limit.
fn text_room(max_bytes: usize) -> usize {
    if max_bytes == usize::MAX {
        max_bytes
    } else {
        max_bytes.saturating_sub(1)
    }
}

fn run_op(root: &Path, op: Op, json: bool, (max_bytes, offset): (usize, usize), outside: Option<&str>) -> Result<i32> {
    run_request(root, crate::session::Request { op, json, max_bytes, offset, outside: outside.map(str::to_string), why: false })
}

fn run_request(root: &Path, request: crate::session::Request) -> Result<i32> {
    let outside = request.outside.as_deref();
    // a session `serve` runs on this root does the same work with the index already open
    // git runs in the environment of the process that asked
    let session = crate::session::wanted() && !matches!(request.op, Op::Impact { .. } | Op::Files { .. });
    let asked = match session.then(|| std::fs::canonicalize(root).ok()).flatten() {
        Some(r) => crate::session::ask(&r, &request)?,
        None => None,
    };
    let p = match asked {
        Some((p, st)) => {
            said_indexed(&st);
            p
        }
        None => {
            let op = &request.op;
            let run = || match op {
                Op::Files { query, limit } => crate::ops::files(root, query, *limit),
                _ => (if op.reads_index() { open_fresh(root)? } else { crate::Repo::open(root)? }).run_as_is(op),
            };
            let answer = match (run(), outside) {
                (Err(e), Some(why)) => anyhow::bail!("{e:#} ({why})"),
                (answer, _) => answer?,
            };
            printed(answer, &request)?
        }
    };
    print!("{}", p.out);
    eprint!("{}", p.err);
    Ok(p.code)
}

/// An answer as `run_op` prints it: standard output, standard error and the exit code (1 when
/// nothing was found). A session prints with this too.
fn printed(answer: crate::ops::Answer, r: &crate::session::Request) -> Result<crate::session::Printed> {
    use crate::ops::Answer;
    use std::fmt::Write as _;
    let mut p = crate::session::Printed::default();
    // these print nothing on standard output when empty, so a pipeline sees no lines
    let quiet =
        matches!(answer, Answer::Outline { .. } | Answer::Deps { .. } | Answer::Rdeps { .. } | Answer::Similar(_) | Answer::Dupes(..) | Answer::Files(..));
    let why = r.outside.as_deref().filter(|_| answer.is_empty());
    if r.json {
        writeln!(p.out, "{}", serde_json::to_string_pretty(&answer.envelope())?)?;
        // standard output keeps the JSON's shape, so the stop line goes beside it
        let note = answer.stop_note();
        if !note.is_empty() {
            writeln!(p.err, "{note}")?;
        }
    } else if quiet && answer.is_empty() {
        writeln!(p.err, "{}{}", answer.text(), why.map(|w| format!(" ({w})")).unwrap_or_default())?;
        p.code = 1;
        return Ok(p);
    } else if let (false, Op::Search { query, alt, .. }, Answer::Hits { hits, .. }) = (answer.is_empty(), &r.op, &answer) {
        // a ranked search shows `--why` and lights up the words of every phrasing as given
        let words: Vec<String> = std::iter::once(query).chain(alt).flat_map(|q| query_words(q)).collect();
        writeln!(p.out, "{}", crate::ops::paged(crate::fmt::hit_blocks(hits, &words, true, r.why), "results", text_room(r.max_bytes), r.offset))?;
    } else {
        let text = answer.text_within(text_room(r.max_bytes), r.offset);
        write!(p.out, "{text}{}", if text.ends_with('\n') { "" } else { "\n" })?;
    }
    if let Some(w) = why {
        writeln!(p.err, "[scrutate] {w}")?;
    }
    p.code = if answer.is_empty() { 1 } else { 0 };
    Ok(p)
}

/// `watch`: update the indexes once the watch is in place, then after every burst of file changes
/// until Ctrl-C.
fn watch_command(root: &Path) -> Result<i32> {
    let root = std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    let stop = Arc::new(AtomicBool::new(false));
    interrupt::install(Arc::clone(&stop));
    // Ctrl-C also ends a wait for another process's write lock
    crate::index::stop_waits_on(Arc::clone(&stop));
    let stopped = || stop.load(Ordering::Relaxed);
    let session = match Session::open(&root, crate::session::Every::given(None)) {
        Err(_) if stopped() => return Ok(130),
        s => s?,
    };
    let update = |changes: &crate::stores::Changes| -> Result<()> {
        // Ctrl-C stops an update at its next batch of files
        let up = session.write(|repo| {
            repo.set_cancel(Arc::clone(&stop));
            let up = repo.update_stores(Writer::Watch, changes, false);
            repo.set_cancel(Arc::default());
            up
        })?;
        let (st, text) = (up.code.unwrap_or_default(), up.text);
        let text_changed = text.as_ref().map(|t| t.files_changed + t.files_removed).unwrap_or(0);
        if st.files_changed + st.files_removed + text_changed > 0 {
            eprintln!(
                "[scrutate] {} changed, {} removed ({} chunks){}",
                st.files_changed,
                st.files_removed,
                st.chunks,
                text.map(|t| format!("; text index: {} changed, {} removed", t.files_changed, t.files_removed)).unwrap_or_default()
            );
        }
        Ok(())
    };
    let r = crate::watch::run(&root, &stop, || eprintln!("[scrutate] watching {}", root.display()), update);
    if stopped() {
        return Ok(130);
    }
    r.map(|()| 0)
}

/// `check`: integrity of the database and the segments; with `--files`, re-hash every indexed file.
fn check_command(root: &Path, files: bool, json: bool) -> Result<i32> {
    let mut s = crate::open(root)?;
    let report = s.ix.check(files)?;
    // a constant or a field has no postings, so no segment row: the counts below leave it out
    let constants = s.ix.constants();
    if json {
        let mut v = serde_json::to_value(&report)?;
        v["constants"] = serde_json::json!(constants);
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("database: {}", if report.database_ok { "ok" } else { "CORRUPT" });
        println!("segments: {} checked, {}", report.segments, if report.segments_ok { "ok" } else { "DAMAGED" });
        println!(
            "chunks: {} ranked in the database, {} alive in segments{}, {constants} constants and fields (no postings)",
            report.chunks_db,
            report.chunks_alive,
            if report.chunks_db == report.chunks_alive { "" } else { "  MISMATCH" }
        );
        if files {
            println!("files: {} indexed, {} changed since indexing, {} missing", report.files_indexed, report.files_changed, report.files_missing);
        }
        for p in &report.problems {
            println!("problem: {p}");
        }
        println!("{}", if report.ok() { "OK" } else { "PROBLEMS FOUND (run `scrutate index` to repair; `scrutate index --force` rebuilds from scratch)" });
    }
    Ok(if report.ok() { 0 } else { 1 })
}

fn print_hits(hits: &[crate::search::Hit], qs: &[String], why: bool, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&crate::ops::envelope(hits_json(hits), None))?);
    } else {
        let words: Vec<String> = qs.iter().flat_map(|q| query_words(q)).collect();
        println!("{}", fmt_hits(hits, &words, true, why));
    }
    Ok(())
}

struct GrepOutput {
    files_with_matches: bool,
    count: bool,
    sort: bool,
    json: bool,
    stats: bool,
    symbols: bool,
    root: PathBuf,
    no_color: bool,
    /// answer from the text index of `root` instead of walking `roots`
    indexed: bool,
    /// refresh that index first
    update: bool,
    /// with `indexed`: only the files under this directory of the root (the current directory),
    /// named from it, as the walk names them
    under: Option<String>,
    /// the text answer within a budget of bytes, from this line on
    page: Option<(usize, usize)>,
}

/// Where grep's text goes: standard output, or the lines of an answer paged within a budget.
enum Out {
    Stream(termcolor::BufferedStandardStream),
    Paged(Paged),
}

/// Lines kept up to what a budget can show; the ones after them are counted.
struct Paged {
    max_bytes: usize,
    offset: usize,
    items: Vec<String>,
    held: usize,
    rest: usize,
    line: Vec<u8>,
}

impl Paged {
    fn end_line(&mut self) {
        let line = String::from_utf8_lossy(&self.line).into_owned();
        self.line.clear();
        if self.items.len() + self.rest < self.offset {
            // skipped: only its place counts
            self.items.push(String::new());
        } else if self.rest == 0 && (self.held == 0 || self.held + line.len() < self.max_bytes) {
            self.held += line.len() + 1;
            self.items.push(line);
        } else {
            self.rest += 1;
        }
    }

    fn text(mut self) -> String {
        if !self.line.is_empty() {
            self.end_line();
        }
        crate::ops::paged_counted(self.items, self.rest, "lines", self.max_bytes, self.offset)
    }
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::Stream(w) => w.write(buf),
            Out::Paged(p) => {
                for &b in buf {
                    if b == b'\n' {
                        p.end_line();
                    } else {
                        p.line.push(b);
                    }
                }
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::Stream(w) => w.flush(),
            Out::Paged(_) => Ok(()),
        }
    }
}

impl WriteColor for Out {
    fn supports_color(&self) -> bool {
        matches!(self, Out::Stream(w) if w.supports_color())
    }

    fn set_color(&mut self, spec: &ColorSpec) -> std::io::Result<()> {
        match self {
            Out::Stream(w) => w.set_color(spec),
            Out::Paged(_) => Ok(()),
        }
    }

    fn reset(&mut self) -> std::io::Result<()> {
        match self {
            Out::Stream(w) => w.reset(),
            Out::Paged(_) => Ok(()),
        }
    }
}

/// The canonical root and the indexed files that can match `q` (after an incremental refresh of
/// the text index when `update`); with `under`, that directory of the root and the files in it.
fn indexed_candidates(root: &Path, q: &Query, update: bool, under: Option<&str>) -> Result<(PathBuf, Vec<String>)> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if let Some(dir) = under {
        // the walk of the root never enters a hidden or ignored directory, so its index cannot answer for one
        let mut at = root.clone();
        for step in dir.split('/') {
            let next = at.join(step);
            if !crate::textindex::walk_entries(&at, Some(1)).0.contains(&next) {
                anyhow::bail!("the current directory is not in the index (hidden or ignored); search without --index");
            }
            at = next;
        }
    }
    if update {
        crate::stores::update(&root, Writer::GrepUpdate, false)?;
    }
    let ti = crate::textindex::TextIndex::open(&root)?
        .ok_or_else(|| anyhow::anyhow!("no text index in {}; build it with `scrutate index --text` (or add --update)", root.display()))?;
    let mut files = ti.candidates(&crate::textindex::query_for(q)?, q.pattern.multiline)?;
    if let Some(dir) = under {
        files = files.into_iter().filter_map(|f| f.strip_prefix(dir).and_then(|f| f.strip_prefix('/')).map(String::from)).collect();
        return Ok((root.join(dir), files));
    }
    Ok((root, files))
}

/// `f.paths` holds the roots as given; with `--index` the search reads the indexed candidates.
fn grep_command(q: Query, mut f: Files, out: GrepOutput) -> Result<i32> {
    let cancel = Arc::new(AtomicBool::new(false));
    interrupt::install(Arc::clone(&cancel));
    // Ctrl-C also ends `--update`'s wait for another process's lock
    crate::index::stop_waits_on(Arc::clone(&cancel));
    let roots = match &f.paths {
        Paths::Walk(roots) => roots.clone(),
        Paths::List { .. } | Paths::Memory(_) => Vec::new(),
    };
    let colour = color_choice(out.no_color) != ColorChoice::Never;
    let plain = !out.json && !colour && !out.sort && !out.symbols && q.report == Report::Lines && out.page.is_none();
    let sink = plain.then(|| Arc::new(instant::PlainSink::new(Box::new(crate::running::GivesWay(std::io::stdout())))));
    if out.indexed {
        let (root, files) = match indexed_candidates(&out.root, &q, out.update, out.under.as_deref()) {
            Err(_) if cancel.load(Ordering::Relaxed) => return Ok(130),
            r => r?,
        };
        f.paths = Paths::List { root, files };
    }
    if let Some(sink) = sink {
        return grep_plain(&roots, &q, &f, out, cancel, sink);
    }
    // buffered: one write per 64 KB instead of several per matching line
    let mut w = match out.page {
        Some((max_bytes, offset)) => Out::Paged(Paged { max_bytes: text_room(max_bytes), offset, items: Vec::new(), held: 0, rest: 0, line: Vec::new() }),
        None => Out::Stream(termcolor::BufferedStandardStream::stdout(color_choice(out.no_color))),
    };
    let mut collected: Vec<Record> = Vec::new();
    let mut pipe_closed = false; // the reader went away (`| head`): stop quietly, like ripgrep (exit 0)
    let mut counts: Vec<Record> = Vec::new();
    let defer = out.sort || out.count || out.files_with_matches || out.symbols;
    let mut found = false;
    let mut linebuf: Vec<u8> = Vec::with_capacity(512);
    let groups = q.before > 0 || q.after > 0;
    let mut last: Option<(Arc<str>, u64, u64)> = None;
    let mut emit = |r: &Record, w: &mut Out| -> Result<()> {
        let (h, context) = match r {
            Record::Match(h) => (h, false),
            Record::Context(h) => (h, true),
            Record::Note { path, path_bytes, text } => {
                if out.json {
                    let rec = serde_json::json!({"type": "binary", "schema": SCHEMA, "path": path, "text": text});
                    writeln!(w, "{}", with_path_bytes(rec, path_bytes.as_deref()))?;
                } else if q.with_filename {
                    w.write_all(&grep_path(path, path_bytes.as_deref(), colour))?;
                    writeln!(w, ": {text}")?;
                } else {
                    writeln!(w, "{text}")?;
                }
                return Ok(());
            }
            Record::Count { .. } | Record::File { .. } => return Ok(()),
        };
        if out.json {
            let rec = serde_json::json!({"type": if context { "context" } else { "match" }, "schema": SCHEMA, "path": h.path, "line": h.line,
                "column": h.column, "text": h.text, "spans": h.spans.iter().map(|(a, b)| serde_json::json!({"start": a, "end": b})).collect::<Vec<_>>()});
            writeln!(w, "{}", with_path_bytes(rec, h.path_bytes.as_deref()))?;
            return Ok(());
        }
        if groups {
            // `--` between lines that are not adjacent in one file, as the plain path's searcher does;
            // a line at or before the previous one starts another search of the same file
            if last.as_ref().is_some_and(|(p, s, l)| **p != *h.path || h.line > l + 1 || h.line <= *s) {
                w.write_all(b"--\n")?;
            }
            last = Some((Arc::clone(&h.path), h.line, h.line + h.text.matches('\n').count() as u64));
        }
        if !colour {
            // plain output: one assembled line, one write (the common piped case)
            linebuf.clear();
            instant::format_plain(
                &mut linebuf,
                if q.with_filename { h.path_bytes.as_deref().unwrap_or(h.path.as_bytes()) } else { b"" },
                if q.line_numbers { h.line } else { 0 },
                h.text.as_bytes(),
                context,
            );
            w.write_all(&linebuf)?;
            return Ok(());
        }
        write_colour(w, h, context, q.with_filename, q.line_numbers)?;
        Ok(())
    };
    let stats = {
        let mut sink = |r: Record| {
            match r {
                Record::Context(_) => {}
                Record::Count { .. } | Record::File { .. } => {
                    found = true;
                    counts.push(r);
                    return;
                }
                Record::Match(_) | Record::Note { .. } => found = true,
            }
            if defer {
                collected.push(r);
            } else if let Err(e) = emit(&r, &mut w) {
                if broken_pipe(&e) {
                    pipe_closed = true;
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        };
        instant::search(&q, &f, Arc::clone(&cancel), Output::Records(&mut sink))?
    };
    warn_failures(&stats);
    if pipe_closed {
        return Ok(if found { 0 } else { 1 });
    }
    // a record per file searched: a file two paths reach is counted and listed twice, as ripgrep does
    // with --sort the search hands them over in ripgrep's order
    if !out.sort {
        counts.sort_by(|a, b| a.path().cmp(b.path()));
    }
    // a reader that went away (`| head`) ends the output quietly, as on the line path
    let written = (|| -> Result<()> {
        if out.count {
            for r in &counts {
                let (p, b) = (r.path(), r.path_bytes());
                let c = if let Record::Count { count, .. } = r { *count } else { 1 };
                if out.json {
                    writeln!(w, "{}", with_path_bytes(serde_json::json!({"type": "count", "schema": SCHEMA, "path": p, "count": c}), b))?;
                } else if q.with_filename {
                    w.write_all(&grep_path(p, b, colour))?;
                    writeln!(w, ":{c}")?;
                } else {
                    writeln!(w, "{c}")?;
                }
            }
        } else if out.files_with_matches {
            for r in &counts {
                let (p, b) = (r.path(), r.path_bytes());
                if out.json {
                    writeln!(w, "{}", with_path_bytes(serde_json::json!({"type": "file", "schema": SCHEMA, "path": p}), b))?;
                } else {
                    w.write_all(&grep_path(p, b, colour))?;
                    writeln!(w)?;
                }
            }
        } else if out.symbols {
            grouped_by_symbol(&out.root, &mut collected, &mut w, out.json)?;
        } else if defer {
            for r in &collected {
                emit(r, &mut w)?;
            }
        }
        if out.stats || out.json {
            // -l stops each file at its first match: the matches were not counted
            let matches = (q.report != Report::Files).then_some(stats.matches);
            let rec = serde_json::json!({"type": "summary", "schema": SCHEMA, "files_searched": stats.files_searched, "files_matched": stats.files_matched,
                "matches": matches, "bytes_searched": stats.bytes_searched, "errors": stats.errors, "elapsed_ms": stats.elapsed_ms, "cancelled": stats.cancelled});
            if out.json {
                writeln!(w, "{rec}")?;
            } else {
                eprintln!(
                    "{}{} of {} files{} ({:.1} MB) in {:.1} ms{}{}",
                    matches.map(|n| format!("{n} matches in ")).unwrap_or_default(),
                    stats.files_matched,
                    stats.files_searched,
                    if matches.is_none() { " match" } else { "" },
                    stats.bytes_searched as f64 / 1e6,
                    stats.elapsed_ms,
                    if stats.errors > 0 { format!(", {} unreadable", stats.errors) } else { String::new() },
                    if stats.cancelled { ", stopped early" } else { "" }
                );
            }
        }
        Ok(())
    })();
    if let Err(e) = written {
        if broken_pipe(&e) {
            return Ok(if found { 0 } else { 1 });
        }
        return Err(e);
    }
    let flushed = match w {
        Out::Paged(p) => {
            let text = p.text();
            let mut so = std::io::stdout().lock();
            if text.is_empty() {
                Ok(())
            } else {
                writeln!(so, "{text}").and_then(|_| so.flush())
            }
        }
        Out::Stream(mut w) => w.flush(),
    };
    if let Err(e) = flushed {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(e.into());
        }
    }
    if stats.cancelled && !q.max_results.map(|m| stats.matches >= m).unwrap_or(false) {
        return Ok(130);
    }
    Ok(if missing_root(&roots, &stats) {
        2
    } else if found {
        0
    } else {
        1
    })
}

/// A path as `grep` writes it: on a terminal for a person, control characters as escapes; else
/// for a program that reads paths, the file's own bytes.
fn grep_path<'p>(path: &'p str, bytes: Option<&'p [u8]>, terminal: bool) -> std::borrow::Cow<'p, [u8]> {
    use std::borrow::Cow;
    match (terminal, crate::fmt::shown(path)) {
        (true, Cow::Owned(s)) => Cow::Owned(s.into_bytes()),
        (true, Cow::Borrowed(s)) => Cow::Borrowed(s.as_bytes()),
        (false, _) => Cow::Borrowed(bytes.unwrap_or(path.as_bytes())),
    }
}

/// A JSON record of `grep` that names a file whose path is not UTF-8: its bytes beside the text.
fn with_path_bytes(mut rec: serde_json::Value, bytes: Option<&[u8]>) -> serde_json::Value {
    if let Some(b) = bytes {
        rec["path_bytes"] = b.into();
    }
    rec
}

/// One line in colour: its path and line number when asked for, and its matches highlighted. Each
/// line of a multi-line match gets its own, as in plain output, and a highlight stops at its end.
fn write_colour(w: &mut impl WriteColor, h: &instant::Line, context: bool, path: bool, number: bool) -> std::io::Result<()> {
    let sep = if context { "-" } else { ":" };
    let text = h.text.as_str();
    let mut start = 0;
    for (k, part) in text.split('\n').enumerate() {
        let end = start + part.len();
        if path {
            w.set_color(ColorSpec::new().set_fg(Some(Color::Magenta)))?;
            write!(w, "{}", crate::fmt::shown(&h.path))?;
            w.reset()?;
            write!(w, "{sep}")?;
        }
        if number {
            w.set_color(ColorSpec::new().set_fg(Some(Color::Green)))?;
            write!(w, "{}", h.line + k as u64)?;
            w.reset()?;
            write!(w, "{sep}")?;
        }
        // the spans lie on character boundaries of the text, but an empty one, which colours nothing
        let mut pos = start;
        for &(a, b) in &h.spans {
            let (x, y) = (a.max(start), b.min(end));
            if x < pos || x > y || (x == y && a < b) || !text.is_char_boundary(x) || !text.is_char_boundary(y) {
                continue;
            }
            write!(w, "{}", &text[pos..x])?;
            w.set_color(ColorSpec::new().set_fg(Some(Color::Red)).set_bold(true))?;
            write!(w, "{}", &text[x..y])?;
            w.reset()?;
            pos = y;
        }
        writeln!(w, "{}", &text[pos..end])?;
        start = end + 1;
    }
    Ok(())
}

fn broken_pipe(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
}

/// A path given to search does not exist or cannot be opened (grep's exit 2); files the walk
/// cannot read do not count.
fn missing_root(roots: &[PathBuf], stats: &instant::Stats) -> bool {
    stats.errors > 0 && roots.iter().any(|r| r.as_os_str() != "-" && std::fs::metadata(r).is_err())
}

/// The first files a search could not read, or not to the end (PCRE2's match limit, I/O errors).
fn warn_failures(stats: &instant::Stats) {
    for f in &stats.failures {
        diagnostic("warning", f);
    }
    let more = stats.errors.saturating_sub(stats.failures.len() as u64);
    if more > 0 {
        diagnostic("warning", &format!("{more} more files could not be searched"));
    }
}

/// Plain unsorted output, written by the search threads (see `instant::PlainSink`).
fn grep_plain(roots: &[PathBuf], q: &Query, f: &Files, out: GrepOutput, cancel: Arc<AtomicBool>, sink: Arc<instant::PlainSink>) -> Result<i32> {
    let stats = instant::search(q, f, Arc::clone(&cancel), Output::Plain(Arc::clone(&sink)))?;
    warn_failures(&stats);
    let found = stats.matches > 0;
    if sink.broken.load(Ordering::Relaxed) {
        if let Some(e) = sink.failure() {
            return Err(anyhow::Error::new(e).context("write the output"));
        }
        return Ok(if found { 0 } else { 1 }); // the reader went away (`| head`): stop quietly
    }
    if let Err(e) = sink.flush() {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(e.into());
        }
    }
    if out.stats {
        eprintln!(
            "{} matches in {} of {} files ({:.1} MB) in {:.1} ms{}{}",
            stats.matches,
            stats.files_matched,
            stats.files_searched,
            stats.bytes_searched as f64 / 1e6,
            stats.elapsed_ms,
            if stats.errors > 0 { format!(", {} unreadable", stats.errors) } else { String::new() },
            if stats.cancelled { ", stopped early" } else { "" }
        );
    }
    if stats.cancelled && !q.max_results.map(|m| stats.matches >= m).unwrap_or(false) {
        return Ok(130);
    }
    Ok(if missing_root(roots, &stats) {
        2
    } else if found {
        0
    } else {
        1
    })
}

/// Engine B enrichment of Engine A results: group matching lines by the enclosing symbol.
fn grouped_by_symbol(root: &Path, records: &mut Vec<Record>, w: &mut impl Write, json: bool) -> Result<()> {
    let s = open_fresh(root)?;
    let mut lines: Vec<Record> = std::mem::take(records).into_iter().filter(|r| matches!(r, Record::Match(_) | Record::Note { .. })).collect();
    // the index names a file by its path under the root, whatever path the search was given
    let mut named: std::collections::HashMap<std::sync::Arc<str>, std::sync::Arc<str>> = std::collections::HashMap::new();
    for r in &mut lines {
        let under = |p: &str| std::fs::canonicalize(p).ok().and_then(|c| c.strip_prefix(&s.ix.root).ok().map(|r| instant::slashed(r.to_string_lossy())));
        let path = match r {
            Record::Match(h) => &mut h.path,
            Record::Note { path, .. } => path,
            _ => continue,
        };
        *path = named.entry(path.clone()).or_insert_with_key(|p| under(p).map_or_else(|| p.clone(), Into::into)).clone();
    }
    lines.sort_by(|a, b| a.path().cmp(b.path()).then(a.order().cmp(&b.order())));
    for g in crate::intel::group_by_symbol(&s.ix, lines)? {
        // the text form is for a person or an agent: names escaped, as the index commands print them
        let shown = |s: &str| if json { std::borrow::Cow::Borrowed(s) } else { crate::fmt::shown(s) }.into_owned();
        let name = g
            .symbol
            .as_ref()
            .map(|r| format!("{}:{}-{}  [{}] {}", shown(&r.path), r.start, r.end, r.kind, shown(if r.qualname.is_empty() { &r.name } else { &r.qualname })));
        if json {
            let mut rec = serde_json::json!({"type": "symbol_group", "schema": SCHEMA, "symbol": name, "path": g.path,
            "matches": g.lines.iter().map(|h| serde_json::json!({"line": h.line, "text": h.text})).collect::<Vec<_>>()});
            if let Some(note) = &g.note {
                rec["note"] = note.as_str().into();
            }
            writeln!(w, "{rec}")?;
        } else {
            writeln!(w, "{}", name.unwrap_or_else(|| format!("{}  (no symbol)", shown(&g.path))))?;
            for h in &g.lines {
                writeln!(w, "    {}: {}", h.line, h.text.trim_end())?;
            }
            if let Some(note) = &g.note {
                writeln!(w, "    note: {note}")?;
            }
        }
    }
    Ok(())
}

/// Unified strategy: regex-looking queries go to Engine A; everything else to the ranked engine when
/// an index exists or the repository is small enough to index now; otherwise an identifier grep.
fn find_command(root: &Path, query: &str, k: usize, json: bool, no_color: bool) -> Result<i32> {
    let has_index = crate::index::exists(root);
    let small_repo = || crate::files::list_files(root).len() <= 3000;
    // a question mark ends a question, it does not make the words a regex
    let query = if query.contains(' ') { query.trim_end().strip_suffix('?').unwrap_or(query) } else { query };
    if looks_like_regex(query) {
        let q = Query {
            pattern: Pattern { text: query.to_string(), mode: Mode::Regex, case: Case::Smart, ..Pattern::default() },
            first_in_path_order: true,
            ..Query::default()
        };
        let r = grep_command(
            q,
            Files::walk(vec![root.to_path_buf()]),
            GrepOutput {
                files_with_matches: false,
                count: false,
                sort: true,
                json,
                stats: false,
                symbols: false,
                root: root.to_path_buf(),
                no_color,
                indexed: false,
                update: false,
                under: None,
                page: None,
            },
        );
        // nothing grepped, or not a valid regex: words with punctuation, for the ranked search
        match r {
            Ok(1) if has_index && !json => {}
            Err(_) if has_index => {}
            r => return r,
        }
    }
    if has_index || small_repo() {
        // find pages nothing: its answer is whole
        let op = Op::Search { query: query.to_string(), alt: Vec::new(), k, snippets: true };
        return run_request(root, crate::session::Request { op, json, max_bytes: usize::MAX, offset: 0, outside: None, why: false });
    }
    eprintln!("[scrutate] no index for this large repository: identifier search (run `scrutate index` for ranked results)");
    let q = Query {
        pattern: Pattern { text: query.to_string(), mode: Mode::Identifier, case: Case::Smart, ..Pattern::default() },
        first_in_path_order: true,
        ..Query::default()
    };
    grep_command(
        q,
        Files::walk(vec![root.to_path_buf()]),
        GrepOutput {
            files_with_matches: false,
            count: false,
            sort: true,
            json,
            stats: false,
            symbols: false,
            root: root.to_path_buf(),
            no_color,
            indexed: false,
            update: false,
            under: None,
            page: None,
        },
    )
}

fn batch_command(root: &Path, file: &Path, features: bool, timing: bool) -> Result<i32> {
    let mut s = crate::Repo::open(root)?;
    if let Some(st) = s.refresh(0.0)?.filter(|st| st.files_changed > 0 || st.files_removed > 0) {
        eprintln!("[scrutate] indexed {} changed files ({} chunks, {:.1}s)", st.files_changed, st.chunks, st.seconds);
    }
    let f = std::fs::File::open(file)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for (n, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let why = e.to_string();
                let why = why.rsplit_once(" at line ").map_or(why.as_str(), |(w, _)| w);
                writeln!(out, "{}", serde_json::json!({"error": format!("line {}, column {}: not JSON ({why})", n + 1, e.column())}))?;
                continue;
            }
        };
        if let Some(tool) = req.get("op").and_then(|v| v.as_str()) {
            // any operation, named and given arguments as an MCP tool call is
            let rec = match Op::from_tool(tool, &req).and_then(|op| s.run_as_is(&op)) {
                Ok(answer) => {
                    let mut rec = answer.envelope();
                    rec["op"] = serde_json::json!(tool);
                    rec
                }
                Err(e) => serde_json::json!({"op": tool, "error": e.to_string()}),
            };
            writeln!(out, "{rec}")?;
            continue;
        }
        let query = req.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let k = req.get("k").and_then(|v| v.as_u64()).unwrap_or(crate::ops::SEARCH_K as u64) as usize;
        let alt: Vec<String> =
            req.get("alt").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let t = Instant::now();
        let mut rec = serde_json::Map::new();
        rec.insert("query".into(), serde_json::Value::String(query.clone()));
        if features {
            let c = s.recovering(|s| s.candidates(&query, true))?;
            let scores = s.score(&c.x)?;
            let hits = s.hits(&c, &scores, k, false)?;
            rec.insert("hits".into(), hits_json(&hits));
            let cands: Vec<serde_json::Value> = c
                .rows
                .iter()
                .zip(&c.x)
                .zip(&scores)
                .map(|((r, x), sc)| serde_json::json!({"id": r.id, "path": r.path, "name": r.name, "qualname": r.qualname, "kind": r.kind, "score": sc, "x": x.to_vec()}))
                .collect();
            // once per query, so a candidate's `x` stays a plain array in this order
            rec.insert("feature_names".into(), serde_json::json!(crate::search::FEATURES.as_slice()));
            rec.insert("candidates".into(), serde_json::Value::Array(cands));
            rec.insert("terms".into(), serde_json::json!(c.q.terms));
            rec.insert("lits".into(), serde_json::json!(c.q.lits));
            for source in crate::search::Source::ALL {
                rec.insert(format!("{}_ids", source.name()), serde_json::json!(c.ids(source)));
            }
        } else {
            let op = Op::Search { query: query.clone(), alt, k, snippets: false };
            let crate::ops::Answer::Hits { hits, .. } = s.run_as_is(&op)? else { unreachable!("a search answers hits") };
            rec.insert("hits".into(), hits_json(&hits));
        }
        if timing {
            rec.insert("ms".into(), serde_json::json!(t.elapsed().as_secs_f64() * 1000.0));
        }
        writeln!(out, "{}", serde_json::Value::Object(rec))?;
    }
    Ok(0)
}

/// Errors go to stderr as one line of text, or as one JSON record (`{"type": "error", ...}`) when
/// the command was given `--json` or `SCRUTATE_DIAGNOSTICS=json` is set, so callers that parse
/// the output can parse the failure too.
fn diagnostic(kind: &str, message: &str) {
    let json = std::env::var("SCRUTATE_DIAGNOSTICS").map(|v| v == "json").unwrap_or(false) || std::env::args_os().any(|a| a == "--json");
    if json {
        eprintln!("{}", serde_json::json!({"type": kind, "schema": SCHEMA, "message": message}));
    } else {
        eprintln!("scrutate: {}: {message}", kind.replace('_', " "));
    }
}

/// The one line a panic prints. Only a panic in a process that opened an index advises rebuilding it.
fn panic_text(msg: &str, location: &str, index_opened: bool) -> String {
    if msg.contains("failed to spawn thread") {
        format!("{msg} ({location}). The system refused another thread: lower -j or SCRUTATE_THREADS, or raise the process limit.")
    } else if index_opened {
        format!("{msg} ({location}). If this persists run `scrutate index --force`; please report it with RUST_BACKTRACE=1.")
    } else {
        format!("{msg} ({location}). Please report it with RUST_BACKTRACE=1.")
    }
}

pub fn main_exit() {
    // a panic is a bug, but the user still gets a one-line diagnosis and a stable exit code
    // (2 = error) instead of a backtrace dump; RUST_BACKTRACE=1 shows the details
    std::panic::set_hook(Box::new(|info| {
        let msg = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_default();
        let opened = crate::index::OPENED.load(std::sync::atomic::Ordering::Relaxed);
        diagnostic("internal_error", &panic_text(&msg, &info.location().map(|l| l.to_string()).unwrap_or_default(), opened));
    }));
    let code = match std::panic::catch_unwind(run) {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            diagnostic("error", &format!("{e:#}"));
            2
        }
        Err(_) => 2,
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use proptest::prelude::*;

    /// `scrutate grep` in colour on a terminal, as `grep_command` writes it.
    fn coloured(dir: &std::path::Path, pattern: &str, multiline: bool) -> String {
        use crate::instant::{Files, Output, Pattern, Query, Record};
        let q = Query {
            pattern: Pattern { text: pattern.into(), multiline, ..Pattern::default() },
            want_spans: true,
            text_spans: true,
            first_in_path_order: true,
            ..Query::default()
        };
        let mut records = Vec::new();
        crate::instant::search(&q, &Files::walk(vec![dir.to_path_buf()]), std::sync::Arc::default(), Output::Records(&mut |r| records.push(r))).unwrap();
        let mut w = termcolor::Buffer::ansi();
        for r in &records {
            if let Record::Match(h) = r {
                super::write_colour(&mut w, h, false, true, true).unwrap();
            }
        }
        String::from_utf8(w.into_inner()).unwrap()
    }

    /// The text written in the colour of matches.
    fn highlighted(out: &str) -> Vec<&str> {
        out.split("\x1b[31m").skip(1).map(|s| &s[..s.find("\x1b[0m").unwrap()]).collect()
    }

    #[test]
    fn colour_highlights_the_matches_in_lines_with_invalid_utf8() {
        let root = std::env::temp_dir().join(format!("scrutate_cli_colour_inv_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"x\x80\xffabc def abc\nab\xffcd\n").unwrap();
        let out = coloured(&root, r"abc|b(?-u:\xFF)c", false);
        assert_eq!(highlighted(&out), ["abc", "abc", "b\u{FFFD}c"], "{out:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn colour_gives_each_line_of_a_multi_line_match_its_path_and_number() {
        let root = std::env::temp_dir().join(format!("scrutate_cli_colour_ml_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("m.txt"), "one\nfoo\nbar baz\nend\n").unwrap();
        let out = coloured(&root, r"foo\nbar|baz", true);
        let plain = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap().replace_all(&out, "");
        assert_eq!(plain, "m.txt:2:foo\nm.txt:3:bar baz\n", "{out:?}");
        // a highlight that crosses a line break is cut at it
        assert_eq!(highlighted(&out), ["foo", "bar", "baz"], "{out:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn grep_names_a_file_exactly_for_a_program_and_escaped_on_a_terminal() {
        let raw = b"x\xff\n.c";
        assert_eq!(&*super::grep_path("x\u{FFFD}\n.c", Some(raw), false), raw);
        assert_eq!(&*super::grep_path("x\u{FFFD}\n.c", Some(raw), true), "x\u{FFFD}\\n.c".as_bytes());
        assert_eq!(&*super::grep_path("a\nb\x1b.c", None, false), b"a\nb\x1b.c");
        assert_eq!(&*super::grep_path("a\nb\x1b.c", None, true), br"a\nb\x1b.c");
        let rec = super::with_path_bytes(serde_json::json!({"path": "x\u{FFFD}\n.c"}), Some(raw));
        assert_eq!(rec["path_bytes"], serde_json::json!([120, 255, 10, 46, 99]));
        assert!(super::with_path_bytes(serde_json::json!({"path": "a.c"}), None).get("path_bytes").is_none());
        // colour output is for a person: a line break in a name cannot start a line of its own
        let h = crate::instant::Line { path: "x\nfake.txt:1:needle".into(), path_bytes: None, line: 3, column: 1, text: "needle".into(), spans: vec![(0, 6)] };
        let mut w = termcolor::Buffer::ansi();
        super::write_colour(&mut w, &h, false, true, true).unwrap();
        let out = String::from_utf8(w.into_inner()).unwrap();
        let plain = regex::Regex::new("\x1b\\[[0-9;]*m").unwrap().replace_all(&out, "").into_owned();
        assert_eq!(plain, "x\\nfake.txt:1:needle:3:needle\n");
    }

    #[test]
    fn a_watch_whose_update_panics_goes_on_and_runs_the_next_update() {
        let root = std::env::temp_dir().join(format!("scrutate_cli_watch_panic_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "0\n").unwrap();
        let stop = std::sync::atomic::AtomicBool::new(false);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let session = crate::session::Session::open(&root, crate::session::Every::given(None)).unwrap();
        let r = std::thread::scope(|sc| {
            let watch = sc.spawn(|| {
                crate::watch::run(
                    &root,
                    &stop,
                    || {},
                    |_| {
                        session.write(|_| {
                            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                            if n == 2 {
                                panic!("an update that panics");
                            }
                            if n > 2 {
                                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            Ok(())
                        })
                    },
                )
            });
            let t0 = std::time::Instant::now();
            for i in 1.. {
                if watch.is_finished() || t0.elapsed().as_secs() > 30 {
                    break;
                }
                std::fs::write(root.join("a.txt"), format!("{i}\n")).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            watch.join()
        });
        let _ = std::fs::remove_dir_all(&root);
        assert!(r.is_ok_and(|r| r.is_ok()), "the watch stopped at the panic");
        assert!(calls.into_inner() > 2, "no update after the panic");
    }

    /// What clap makes of `scrutate ARGS`, as `plain_grep` answers.
    fn clap_grep(args: &[String]) -> Option<(bool, super::GrepArgs)> {
        let super::GrepCli { root, no_color, cmd: super::GrepCmd::Grep(g), .. } = super::GrepCli::try_parse_from(args).ok()?;
        assert_eq!(root, std::path::PathBuf::from("."));
        Some((no_color, g))
    }

    // inside the shapes, near them (values that start with `-` or `=`, flags given twice) and outside
    const TOKENS: &str = "-F|-i|-S|-w|-U|-v|-l|-c|-H|-I|-n|-N|-L|-P|-z|-Fi|-nF|-iw|-lc|-HI|--fixed-strings|--ignore-case|--smart-case|--word|--count|--line-number|--no-line-number|--with-filename|--no-filename|--hidden|--no-ignore|--ident|--index|--sort|--json|--stats|--no-color|--follow|-e|-e|-ex|-Fex|-e=x|-e-x|--regexp|-g|-g|-g*.rs|--glob|-j|-j1|-j4|-Fj2|-j=2|--threads|--regexp=x|--glob=x|--threads=2|hello|a.txt|src|x y||-|--|-x|-1|01|+1|*.c|!*.h|=x|--fuzzy|--update|-A|-C|2|-h|--help|-V|--root|--max-count|-t|rs|--type-list|é|-é";

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20000))]
        #[test]
        fn plain_grep_is_clap(sub in prop::sample::select(vec!["grep", "g"]), picks in prop::collection::vec(0..TOKENS.split('|').count(), 0..7)) {
            if std::env::vars_os().any(|(k, _)| k.as_encoded_bytes().starts_with(b"SCRUTATE_")) {
                return Ok(()); // clap would read them, and plain_grep is not asked then
            }
            let args: Vec<String> = ["scrutate", sub].into_iter().chain(picks.iter().map(|&i| TOKENS.split('|').nth(i).unwrap())).map(String::from).collect();
            if let Some(plain) = super::plain_grep(&args) {
                prop_assert_eq!(Some(plain), clap_grep(&args), "{:?}", args);
            }
        }

        #[test]
        fn plain_grep_is_clap_inside_its_shapes(
            flags in prop::sample::subsequence(vec!["-F", "-i", "-S", "-w", "-U", "-v", "-l", "-c", "-H", "-I", "-n", "-N", "--hidden", "--no-ignore", "--ident", "--no-color", "--json", "-j2"], 0..6).prop_shuffle(),
            values in prop::collection::vec(prop::sample::select(vec!["-e", "-g", "--regexp", "--glob", "", "pos"]), 0..5),
            words in prop::collection::vec("[a-z.*!/é]{0,4}", 5),
        ) {
            let mut args: Vec<String> = vec!["scrutate".into(), "grep".into()];
            for (i, v) in values.iter().enumerate() {
                match *v {
                    "pos" | "" => args.push(words[i].clone()),
                    flag => args.extend([flag.to_string(), words[i].clone()]),
                }
            }
            for (i, f) in flags.iter().enumerate() {
                args.insert(2 + (i * 3) % (args.len() - 1), f.to_string());
            }
            let plain = super::plain_grep(&args);
            let clap = clap_grep(&args);
            if plain.is_some() {
                prop_assert_eq!(plain, clap, "{:?}", args);
            } else {
                // declined only where clap fails, a value is empty, or a pattern given with -e
                // starts with a dash (clap takes that one)
                let dashed = args.windows(2).any(|w| matches!(w[0].as_str(), "-e" | "--regexp") && w[1].starts_with('-'));
                prop_assert!(clap.is_none() || dashed || args.iter().any(|a| a.is_empty()), "{:?}", args);
            }
        }
    }

    #[test]
    fn plain_grep_takes_the_common_shapes() {
        for line in [
            "grep -F -e zqxjkvbw_never_present_symbol",
            "grep -j1 -F -i -w -e a -e b",
            "g -n -U pattern src a.txt",
            "grep --no-ignore --hidden -g !.git -g !.scrutate -e x",
            "grep -Fl x -",
            "grep --ident refreshToken",
            "grep -g *.c -g *.h -e x",
        ] {
            let args: Vec<String> = std::iter::once("scrutate").chain(line.split(' ')).map(String::from).collect();
            assert!(super::plain_grep(&args).is_some(), "{line}");
        }
        let args = |s: &str| -> Vec<String> { std::iter::once("scrutate").chain(s.split(' ')).map(String::from).collect() };
        for line in ["grep -F -F x", "grep --help", "grep -A 2 x", "grep -e", "grep -- -x", "index x", "grep -j=1 x", "grep x --root ."] {
            assert!(super::plain_grep(&args(line)).is_none(), "{line}");
        }
    }

    #[test]
    fn every_variable_clap_reads_for_grep_sends_the_arguments_to_clap() {
        use clap::CommandFactory;
        let cmd = super::GrepCli::command();
        let mut read: Vec<String> = Vec::new();
        for c in std::iter::once(&cmd).chain(cmd.get_subcommands()) {
            read.extend(c.get_arguments().filter_map(|a| a.get_env()).map(|e| e.to_string_lossy().into_owned()));
        }
        read.sort();
        read.dedup();
        let mut listed: Vec<String> = super::GREP_ENV.iter().map(|s| s.to_string()).collect();
        listed.sort();
        assert_eq!(read, listed);
    }

    #[test]
    fn sizes_that_overflow_are_refused() {
        assert_eq!(super::parse_size("10M").unwrap(), 10 << 20);
        assert_eq!(super::parse_size("17179869183G").unwrap(), 17179869183 << 30);
        assert!(super::parse_size("17179869184G").is_err());
        assert!(super::parse_size("x").is_err());
    }

    #[test]
    fn a_failed_thread_spawn_does_not_blame_the_index() {
        let t = super::panic_text("failed to spawn thread: Os { code: 35 }", "x.rs:1:1", true);
        assert!(!t.contains("index") && t.contains("-j"), "{t}");
        assert!(super::panic_text("index out of bounds", "x.rs:1:1", true).contains("index --force"));
    }

    #[test]
    fn a_panic_in_a_process_that_opened_no_index_does_not_blame_the_index() {
        let t = super::panic_text("index out of bounds", "x.rs:1:1", false);
        assert!(!t.contains("index --force") && t.contains("RUST_BACKTRACE=1"), "{t}");
    }
}
