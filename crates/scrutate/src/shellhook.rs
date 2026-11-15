//! `scrutate hook bash` and `scrutate hook run`: the `PreToolUse` hook of Claude Code's and
//! Codex's shell tool. When the first command of a command line is a read-only search by `rg`,
//! `grep` or `find`, the hook puts `scrutate hook run --` in front of it, byte for byte as the
//! user wrote it, and leaves the rest of the line alone: the shell still expands it and runs the
//! rest. `hook run` answers the search as the program would (`rg` through `scrutate grep`,
//! `grep` and `find` by running them), bounded for the agent and with its secrets redacted.
//! Anything else is left alone: the hook prints nothing and the command runs as written.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

/// The first command of a command line, as far as the hook reads it.
#[derive(Debug, PartialEq)]
pub struct First {
    /// where it starts and ends in the line, in bytes
    pub start: usize,
    pub end: usize,
    /// its words before the shell expands them (a glob stays as written), stderr redirections left out
    pub words: Vec<String>,
    /// a `|` or `|&` takes its output
    pub piped: bool,
}

/// The only redirection target a rewritten line may keep, written right after the `>`: on Unix
/// `NUL` is a file of the current directory.
const NULL: &[&str] = if cfg!(windows) { &["/dev/null", "NUL"] } else { &["/dev/null"] };

/// Whether the null device, as `NULL` spells it, comes next and ends its word; it is then read.
fn null_follows(cmd: &str, chars: &mut std::iter::Peekable<impl Iterator<Item = (usize, char)>>) -> bool {
    let Some(&(at, _)) = chars.peek() else { return false };
    let Some(n) = NULL.iter().find(|n| cmd[at..].starts_with(*n) && cmd[at + n.len()..].chars().next().is_none_or(ends_word)) else { return false };
    for _ in 0..n.chars().count() {
        chars.next();
    }
    true
}

/// Whether `c` ends a word for the shell.
fn ends_word(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '|' | ';' | '&')
}

/// The programs `hook run` answers.
const TOOLS: [&str; 3] = ["rg", "grep", "find"];

/// The first command of `cmd`: up to the first unquoted `|`, `|&`, `||`, `&&`, `;`, `&` or line
/// end. `None` when that command holds what the hook does not read: `$`, a backquote, `(`, `)`,
/// `{`, `}`, `<`, a comment, an assignment before the program, or any redirection but
/// `2>/dev/null`, `2>&1`, `>/dev/null` and `&>/dev/null` (`NUL` for `/dev/null` too), which are
/// left out of the words: any other target is a file the line would write.
pub fn first_command(cmd: &str) -> Option<First> {
    let start = cmd.len() - cmd.trim_start().len();
    let (mut words, mut word, mut open) = (Vec::new(), String::new(), false);
    let mut chars = cmd[start..].char_indices().map(|(i, c)| (i + start, c)).peekable();
    let push = |words: &mut Vec<String>, word: &mut String, open: &mut bool| {
        if std::mem::take(open) {
            words.push(std::mem::take(word));
        }
    };
    let (mut end, mut piped) = (cmd.len(), false);
    while let Some((i, c)) = chars.next() {
        match c {
            ' ' | '\t' => push(&mut words, &mut word, &mut open),
            '\'' => {
                open = true;
                loop {
                    match chars.next()?.1 {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                open = true;
                loop {
                    match chars.next()?.1 {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => match chars.next()?.1 {
                            c @ ('"' | '\\') => word.push(c),
                            '$' | '`' | '\n' => return None,
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                open = true;
                match chars.next()?.1 {
                    '\n' | '\r' => return None,
                    c => word.push(c),
                }
            }
            // bash reads `\r` as part of a word: what follows it would apply to `hook run` unread
            '\r' => return None,
            '|' | ';' | '\n' => {
                piped = c == '|' && chars.peek().is_none_or(|&(_, n)| n != '|');
                end = i;
                break;
            }
            '&' => match chars.peek().map(|&(_, n)| n) {
                // only `&>/dev/null`, written so
                Some('>') => {
                    chars.next();
                    push(&mut words, &mut word, &mut open);
                    if !null_follows(cmd, &mut chars) {
                        return None;
                    }
                }
                _ => {
                    end = i;
                    break;
                }
            },
            '>' => {
                // `2>/dev/null`, `2>&1`, `>/dev/null`, written so: Codex is told to allow the line
                let fd = if open { std::mem::take(&mut word) } else { String::new() };
                open = false;
                if !matches!(fd.as_str(), "" | "1" | "2") {
                    // `foo>x`: the word, then the redirection
                    words.push(fd.clone());
                }
                if fd == "2" && chars.peek().is_some_and(|&(_, n)| n == '&') {
                    chars.next();
                    // only `2>&1`, whole
                    if chars.next().map(|(_, n)| n) != Some('1') || chars.peek().is_some_and(|&(_, n)| !ends_word(n)) {
                        return None;
                    }
                } else if !null_follows(cmd, &mut chars) {
                    return None;
                }
            }
            '$' | '`' | '(' | ')' | '{' | '}' | '<' => return None,
            '#' if !open => return None,
            c => {
                open = true;
                word.push(c);
            }
        }
    }
    push(&mut words, &mut word, &mut open);

    // `VAR=x rg`, `/usr/bin/rg`: not the programs the hook answers
    let program = words.first()?;
    if program.contains(['=', '/', '\\']) {
        return None;
    }
    Some(First { start, end, words, piped })
}

/// Whether `rest`, what follows the first command, is nothing or one pipe into `head` or `tail`
/// with plain words: all Codex may be told to allow.
fn only_head_or_tail(rest: &str) -> bool {
    let rest = rest.trim();
    if rest.is_empty() {
        return true;
    }
    let Some(filter) = rest.strip_prefix('|').filter(|r| !r.starts_with(['|', '&'])) else { return false };
    // the same reading as the first command's, and nothing after it
    let Some(f) = first_command(filter) else { return false };
    f.end == filter.len() && filter_lines(&f.words).is_some()
}

/// The lines `head ARGS` or `tail ARGS` keeps: no argument, `-n N`, `-nN`, `-N`, `--lines=N`.
fn filter_lines(args: &[String]) -> Option<usize> {
    let n = |s: &str| s.parse::<usize>().ok();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (tool, rest) = args.split_first()?;
    if !matches!(*tool, "head" | "tail") {
        return None;
    }
    match rest {
        [] => Some(10),
        ["-n", v] => n(v),
        [a] if a.starts_with("--lines=") => n(&a["--lines=".len()..]),
        [a] if a.starts_with("-n") => n(&a[2..]),
        [a] if a.starts_with('-') => n(&a[1..]),
        _ => None,
    }
}

/// Whether `hook run` may run `argv`: a bare `rg`, `grep` or `find` (no path to another, no
/// assignment) that only reads. The rewrite and `hook run` both ask this: Codex is told to allow
/// what the hook rewrites, and a user may allow every `hook run` command without a prompt.
pub fn allowed(argv: &[String]) -> bool {
    // on Windows the first `find` on PATH may be System32's, another program altogether
    argv.first().is_some_and(|p| TOOLS.contains(&p.as_str()) && !(cfg!(windows) && p == "find")) && read_only(argv)
}

/// ripgrep's flags that run a program: a preprocessor, the decompressors of `-z`, a hostname
/// command.
const RG_RUNS: [&str; 5] = ["--pre", "--pre-glob", "--search-zip", "--hostname-bin", "-z"];

/// `scrutate grep`'s flags that `rg` lacks: `rg` would refuse them, and `--index` and `--update`
/// write the text index.
const NOT_RG: [&str; 14] = [
    "--after",
    "--before",
    "--fuzzy",
    "--fuzzy-regex",
    "--ident",
    "--index",
    "--max-bytes",
    "--max-results",
    "--no-color",
    "--offset",
    "--root",
    "--symbols",
    "--update",
    "--word",
];

/// Whether a long flag takes a value: none, one (after `=` or as the next word), or one only
/// after `=` (GNU grep's `--color`).
#[derive(Clone, Copy, PartialEq)]
enum Takes {
    No,
    Value,
    Joined,
}

/// The flags a program may be given here: every other flag is refused, so one this table does not
/// know (a new release's, or a rare one) costs only the rewrite.
struct Flags {
    /// short flags without a value, which may stand together (`-rn`)
    plain: &'static str,
    /// short flags with a value: the rest of their cluster (`-tpy`), or the next word
    valued: &'static str,
    long: &'static [(&'static str, Takes)],
}

/// ripgrep 15's flags that only choose what is read and how it is printed. Not here: the ones that
/// run a program (`RG_RUNS`) or that `rg` lacks (`NOT_RG`).
const RG_FLAGS: Flags = Flags {
    plain: "sFivxUPSwLauclbnNoqHI0.",
    valued: "efEmjgdtTABCMr",
    long: &[
        ("--regexp", Takes::Value),
        ("--file", Takes::Value),
        ("--encoding", Takes::Value),
        ("--engine", Takes::Value),
        ("--max-count", Takes::Value),
        ("--threads", Takes::Value),
        ("--glob", Takes::Value),
        ("--iglob", Takes::Value),
        ("--ignore-file", Takes::Value),
        ("--max-depth", Takes::Value),
        ("--max-filesize", Takes::Value),
        ("--type", Takes::Value),
        ("--type-not", Takes::Value),
        ("--after-context", Takes::Value),
        ("--before-context", Takes::Value),
        ("--context", Takes::Value),
        ("--color", Takes::Value),
        ("--colors", Takes::Value),
        ("--context-separator", Takes::Value),
        ("--field-context-separator", Takes::Value),
        ("--field-match-separator", Takes::Value),
        ("--max-columns", Takes::Value),
        ("--path-separator", Takes::Value),
        ("--replace", Takes::Value),
        ("--sort", Takes::Value),
        ("--sortr", Takes::Value),
        ("--dfa-size-limit", Takes::Value),
        ("--regex-size-limit", Takes::Value),
        ("--case-sensitive", Takes::No),
        ("--crlf", Takes::No),
        ("--fixed-strings", Takes::No),
        ("--ignore-case", Takes::No),
        ("--invert-match", Takes::No),
        ("--line-regexp", Takes::No),
        ("--multiline", Takes::No),
        ("--multiline-dotall", Takes::No),
        ("--no-unicode", Takes::No),
        ("--null-data", Takes::No),
        ("--pcre2", Takes::No),
        ("--smart-case", Takes::No),
        ("--word-regexp", Takes::No),
        ("--binary", Takes::No),
        ("--follow", Takes::No),
        ("--glob-case-insensitive", Takes::No),
        ("--hidden", Takes::No),
        ("--no-ignore", Takes::No),
        ("--no-ignore-dot", Takes::No),
        ("--no-ignore-exclude", Takes::No),
        ("--no-ignore-files", Takes::No),
        ("--no-ignore-global", Takes::No),
        ("--no-ignore-parent", Takes::No),
        ("--no-ignore-vcs", Takes::No),
        ("--no-require-git", Takes::No),
        ("--one-file-system", Takes::No),
        ("--text", Takes::No),
        ("--unrestricted", Takes::No),
        ("--count", Takes::No),
        ("--count-matches", Takes::No),
        ("--files", Takes::No),
        ("--files-with-matches", Takes::No),
        ("--files-without-match", Takes::No),
        ("--json", Takes::No),
        ("--byte-offset", Takes::No),
        ("--column", Takes::No),
        ("--heading", Takes::No),
        ("--no-heading", Takes::No),
        ("--include-zero", Takes::No),
        ("--line-number", Takes::No),
        ("--no-line-number", Takes::No),
        ("--null", Takes::No),
        ("--only-matching", Takes::No),
        ("--passthru", Takes::No),
        ("--quiet", Takes::No),
        ("--stats", Takes::No),
        ("--trim", Takes::No),
        ("--vimgrep", Takes::No),
        ("--with-filename", Takes::No),
        ("--no-filename", Takes::No),
        ("--no-messages", Takes::No),
        ("--no-config", Takes::No),
        ("--max-columns-preview", Takes::No),
        ("--type-list", Takes::No),
    ],
};

/// GNU and BSD grep's flags that only choose what is read and how it is printed (grep has none
/// that runs a program or writes a file, and the table holds it to that).
const GREP_FLAGS: Flags = Flags {
    plain: "EFGPiywxzvcLloqsbHhnTZUaIrR0123456789",
    valued: "efmABCdD",
    long: &[
        ("--regexp", Takes::Value),
        ("--file", Takes::Value),
        ("--max-count", Takes::Value),
        ("--after-context", Takes::Value),
        ("--before-context", Takes::Value),
        ("--context", Takes::Value),
        ("--include", Takes::Value),
        ("--exclude", Takes::Value),
        ("--exclude-dir", Takes::Value),
        ("--exclude-from", Takes::Value),
        ("--binary-files", Takes::Value),
        ("--directories", Takes::Value),
        ("--devices", Takes::Value),
        ("--label", Takes::Value),
        ("--group-separator", Takes::Value),
        ("--color", Takes::Joined),
        ("--colour", Takes::Joined),
        ("--extended-regexp", Takes::No),
        ("--fixed-strings", Takes::No),
        ("--basic-regexp", Takes::No),
        ("--perl-regexp", Takes::No),
        ("--ignore-case", Takes::No),
        ("--no-ignore-case", Takes::No),
        ("--word-regexp", Takes::No),
        ("--line-regexp", Takes::No),
        ("--null-data", Takes::No),
        ("--invert-match", Takes::No),
        ("--count", Takes::No),
        ("--files-with-matches", Takes::No),
        ("--files-without-match", Takes::No),
        ("--only-matching", Takes::No),
        ("--quiet", Takes::No),
        ("--silent", Takes::No),
        ("--no-messages", Takes::No),
        ("--byte-offset", Takes::No),
        ("--with-filename", Takes::No),
        ("--no-filename", Takes::No),
        ("--line-number", Takes::No),
        ("--initial-tab", Takes::No),
        ("--null", Takes::No),
        ("--text", Takes::No),
        ("--binary", Takes::No),
        ("--recursive", Takes::No),
        ("--dereference-recursive", Takes::No),
        ("--line-buffered", Takes::No),
        ("--no-group-separator", Takes::No),
    ],
};

/// Whether `a` names a flag of `RG_RUNS` or `NOT_RG`, or is a cluster of short flags with `z` in
/// it (`-iz`). `NOT_RG` too, since `scrutate grep`'s parser may take as a flag what rg takes as a value.
fn rg_runs(a: &str) -> bool {
    let flag = a.split_once('=').map_or(a, |(f, _)| f);
    RG_RUNS.contains(&flag) || NOT_RG.contains(&flag) || a.starts_with('-') && !a.starts_with("--") && a[1..].contains('z')
}

/// Whether every word of `args` is a flag of `flags`, the value of one, or a positional word, read
/// as the program reads them: a flag's value is skipped whatever it looks like, so only a `--` in
/// a flag's place ends the flags.
fn only_known(flags: &Flags, args: &[String]) -> bool {
    let mut words = args.iter();
    while let Some(a) = words.next() {
        if a == "--" {
            return true;
        }
        if a.starts_with("--") {
            let (name, joined) = a.split_once('=').map_or((a.as_str(), false), |(n, _)| (n, true));
            match flags.long.iter().find(|(n, _)| *n == name).map(|(_, t)| *t) {
                None => return false,
                Some(Takes::No) if joined => return false,
                Some(Takes::Value) if !joined && words.next().is_none() => return false,
                _ => {}
            }
        } else if a.len() > 1 && a.starts_with('-') {
            for (i, c) in a[1..].char_indices() {
                if flags.valued.contains(c) {
                    if i + c.len_utf8() == a.len() - 1 && words.next().is_none() {
                        return false;
                    }
                    break;
                }
                if !flags.plain.contains(c) {
                    return false;
                }
            }
        }
    }
    true
}

/// find's expressions that only read and print; any other (`-exec`, `-execdir`, `-ok`, `-okdir`,
/// `-delete`, `-fprint`, `-fprint0`, `-fprintf`, `-fls`, ...) leaves the command alone.
const FIND_READS: [&str; 46] = [
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-wholename",
    "-iwholename",
    "-regex",
    "-iregex",
    "-regextype",
    "-type",
    "-xtype",
    "-size",
    "-empty",
    "-newer",
    "-mtime",
    "-mmin",
    "-atime",
    "-amin",
    "-ctime",
    "-cmin",
    "-used",
    "-perm",
    "-user",
    "-group",
    "-uid",
    "-gid",
    "-nouser",
    "-nogroup",
    "-links",
    "-inum",
    "-samefile",
    "-readable",
    "-writable",
    "-executable",
    "-maxdepth",
    "-mindepth",
    "-depth",
    "-prune",
    "-print",
    "-print0",
    "-true",
    "-false",
    "-not",
    "-and",
    "-or",
    "-xdev",
];
/// The expressions of `FIND_READS` and `FIND_MORE` that take a value.
const FIND_VALUED: [&str; 31] = [
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-wholename",
    "-iwholename",
    "-regex",
    "-iregex",
    "-regextype",
    "-type",
    "-xtype",
    "-size",
    "-newer",
    "-mtime",
    "-mmin",
    "-atime",
    "-amin",
    "-ctime",
    "-cmin",
    "-used",
    "-perm",
    "-user",
    "-group",
    "-uid",
    "-gid",
    "-links",
    "-inum",
    "-samefile",
    "-maxdepth",
    "-mindepth",
    "-printf",
];
/// find's options before the paths, and more expressions that only read.
const FIND_MORE: [&str; 12] = ["-a", "-o", "-mount", "-noleaf", "-daystart", "-follow", "-quit", "-ls", "-printf", "-L", "-H", "-P"];

/// Whether `argv` only reads files and writes to standard output.
fn read_only(argv: &[String]) -> bool {
    match argv[0].as_str() {
        // a flag that runs a program is refused wherever it sits, also as a value or after `--`;
        // then every word must be one the table knows
        "rg" => !argv[1..].iter().any(|a| rg_runs(a)) && only_known(&RG_FLAGS, &argv[1..]),
        "grep" => only_known(&GREP_FLAGS, &argv[1..]),
        "find" => {
            let mut words = argv[1..].iter();
            while let Some(a) = words.next() {
                if !a.starts_with('-') {
                    continue;
                }
                if !FIND_READS.contains(&a.as_str()) && !FIND_MORE.contains(&a.as_str()) {
                    return false;
                }
                // its value may start with `-` (`-size -10k`, `-mtime -1`)
                if FIND_VALUED.contains(&a.as_str()) && words.next().is_none() {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

/// `s` as one POSIX sh word, whatever it holds.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `cmd` with `exe`'s `hook run` put in front of its first command, the rest byte for byte.
pub fn rewritten(exe: &str, session: &str, cmd: &str, first: &First) -> String {
    let mut prefix = format!("{} hook run", quote(exe));
    if !session.is_empty() {
        prefix.push_str(&format!(" --session {}", quote(session)));
    }
    if first.piped {
        prefix.push_str(" --piped");
    }
    format!("{}{prefix} -- {}", &cmd[..first.start], &cmd[first.start..])
}

/// Whether the user turned the Bash hook off: SCRUTATE_HOOK_BASH=off.
fn turned_off() -> bool {
    std::env::var("SCRUTATE_HOOK_BASH").is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no"))
}

/// The decision for one `PreToolUse` call of the shell tool, or `None` to let it run as written.
pub fn decide(input: &Value, exe: &str) -> Option<Value> {
    if turned_off() || input.get("hook_event_name")?.as_str()? != "PreToolUse" || input.get("tool_name")?.as_str()? != "Bash" {
        return None;
    }
    // rg reads its own configuration file there, which `scrutate grep` does not
    if std::env::var_os("RIPGREP_CONFIG_PATH").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let tool_input = input.get("tool_input")?.as_object()?;
    let cmd = tool_input.get("command")?.as_str()?;
    // bash reads `\r` and the other control characters as part of a word, which the reader would
    // not: what follows one would apply to `hook run` unread
    if cmd.chars().any(|c| c.is_control() && c != '\t' && c != '\n') {
        return None;
    }
    let first = first_command(cmd)?;
    // the words as written; `hook run` asks again of the words the shell expands
    if !allowed(&first.words) {
        return None;
    }
    let session = input.get("session_id").and_then(Value::as_str).unwrap_or("");
    let command = rewritten(exe, session, cmd, &first);
    // Codex sends a turn; it takes only `command` and needs `allow` with it, which approves the
    // whole line: only a search, with at most `| head` or `| tail` after it
    if input.get("turn_id").is_some() {
        if !only_head_or_tail(&cmd[first.end..]) {
            return None;
        }
        return Some(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": {"command": command}}}));
    }
    // Claude Code replaces the whole input: the other fields go back as they came
    let mut updated = tool_input.clone();
    updated.insert("command".into(), json!(command));
    Some(json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": updated}}))
}

/// `scrutate hook bash`: reads the call on standard input and prints the rewrite, or nothing.
/// Always 0, so a failure lets the command run as written.
pub fn bash_main() -> i32 {
    let decided = std::panic::catch_unwind(|| {
        let mut input = Vec::new();
        std::io::stdin().take(4 << 20).read_to_end(&mut input).ok()?;
        let input: Value = serde_json::from_slice(&input).ok()?;
        let exe = std::env::current_exe().ok()?;
        decide(&input, exe.to_str()?)
    });
    if let Ok(Some(d)) = decided {
        let mut so = std::io::stdout().lock();
        let _ = writeln!(so, "{d}").and_then(|_| so.flush());
    }
    0
}

/// The command that answers `argv`. `rg` goes to `exe grep`, with no configuration of
/// scrutate's own in play, when `scrutate grep` takes its arguments: the output `rg` itself gives;
/// else, and for every `grep` and `find`, the program itself runs, so its own lines, messages and
/// code come back. None when the program is on no allowed directory of PATH.
fn command(exe: &std::path::Path, argv: &[String]) -> Option<Command> {
    let mut args = vec!["-N".to_string()];
    args.extend(argv[1..].iter().cloned());
    let mut c = if argv[0] == "rg" && crate::cli::grep_takes(&args) {
        let mut c = Command::new(exe);
        // ripgrep writing to a pipe shows no line numbers unless asked; a later `-n` still wins
        c.arg("grep").args(&args);
        for (k, _) in std::env::vars_os() {
            if k.as_encoded_bytes().starts_with(b"SCRUTATE_") {
                c.env_remove(k);
            }
        }
        c.env("SCRUTATE_CONFIG", if cfg!(windows) { "NUL" } else { "/dev/null" });
        c
    } else {
        let mut c = Command::new(on_path(&argv[0])?);
        c.args(&argv[1..]);
        c
    };
    // rg's configuration file may add any flag, `--pre` among them; a fixed read-only tool needs
    // no preloaded library
    c.env_remove("RIPGREP_CONFIG_PATH");
    for (k, _) in std::env::vars_os() {
        let k8 = k.as_encoded_bytes();
        if matches!(k8, b"LD_PRELOAD" | b"LD_LIBRARY_PATH" | b"LD_AUDIT") || k8.starts_with(b"DYLD_") {
            c.env_remove(k);
        }
    }
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    Some(c)
}

/// On Unix: the child dies when `hook run` does, however it dies (the agent's timeout, SIGKILL).
/// Before the program starts, its process forks a watcher that waits for the end of a pipe only
/// `hook run` holds open, then kills the program if it is still running. -> the pipe's ends:
/// the caller drops the read end once the program has started and keeps the write end until the
/// program is done.
#[cfg(unix)]
fn die_with_us(c: &mut Command) -> Option<(std::fs::File, std::fs::File)> {
    use std::os::fd::FromRawFd;
    use std::os::unix::process::CommandExt;
    extern "C" {
        fn pipe(fds: *mut i32) -> i32;
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        fn fork() -> i32;
        fn read(fd: i32, buf: *mut std::ffi::c_void, n: usize) -> isize;
        fn close(fd: i32) -> i32;
        fn getpid() -> i32;
        fn getppid() -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
        fn _exit(code: i32) -> !;
        fn getrlimit(resource: i32, rlim: *mut [u64; 2]) -> i32;
        #[cfg(target_os = "linux")]
        fn syscall(n: i64, ...) -> i64;
    }
    const F_SETFD: i32 = 2;
    const FD_CLOEXEC: i32 = 1;
    const SIGKILL: i32 = 9;
    let mut fds = [0i32; 2];
    // SAFETY: plain system calls on file descriptors this function made
    unsafe {
        if pipe(fds.as_mut_ptr()) != 0 {
            return None;
        }
        // neither end reaches the program itself: only the watcher holds the read end
        fcntl(fds[0], F_SETFD, FD_CLOEXEC);
        fcntl(fds[1], F_SETFD, FD_CLOEXEC);
    }
    let (r, w) = (fds[0], fds[1]);
    // the highest descriptor number open files can have here, read before the fork
    const RLIMIT_NOFILE: i32 = if cfg!(target_os = "linux") { 7 } else { 8 };
    let mut lim = [0u64; 2];
    // SAFETY: getrlimit fills the two words of an rlimit
    let bound = if unsafe { getrlimit(RLIMIT_NOFILE, &mut lim) } == 0 { lim[0].min(1 << 20) as i32 } else { 1 << 16 };
    // fork inside pre_exec is safe only while hook run has one thread at the spawn: glibc's fork
    // takes the malloc and atfork locks, which a second thread could hold
    #[cfg(target_os = "linux")]
    debug_assert_eq!(std::fs::read_dir("/proc/self/task").map(|d| d.count()).unwrap_or(1), 1, "hook run spawns with one thread");
    // SAFETY: between fork and exec the closure calls only async-signal-safe functions
    unsafe {
        c.pre_exec(move || {
            let program = getpid();
            match fork() {
                0 => {
                    // no file but the pipe's read end, not even the program's output: its reader
                    // must see that end (the spawn's own pipes are still open here, no exec closes them)
                    #[cfg(target_os = "linux")]
                    {
                        const SYS_CLOSE_RANGE: i64 = 436;
                        let low = syscall(SYS_CLOSE_RANGE, 0u32, (r - 1) as u32, 0u32);
                        let high = syscall(SYS_CLOSE_RANGE, (r + 1) as u32, u32::MAX, 0u32);
                        if low != 0 || high != 0 {
                            for fd in (0..bound.max(r + 1)).filter(|&fd| fd != r) {
                                close(fd);
                            }
                        }
                    }
                    #[cfg(not(target_os = "linux"))]
                    for fd in (0..bound.max(r + 1)).filter(|&fd| fd != r) {
                        close(fd);
                    }
                    let mut b = 0u8;
                    while read(r, (&raw mut b).cast(), 1) > 0 {}
                    // still the program's child: the program runs on
                    if getppid() == program {
                        kill(program, SIGKILL);
                    }
                    _exit(0)
                }
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            }
        });
        Some((std::fs::File::from_raw_fd(r), std::fs::File::from_raw_fd(w)))
    }
}

/// On Windows: the child is in a job that ends with `hook run`'s last handle to it.
#[cfg(windows)]
fn die_with_us_windows(child: &std::process::Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    // SAFETY: a job this function makes, given the child's own handle; the job's handle is kept
    // to the end of the process, which closes it
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const std::ffi::c_void, size) != 0 {
            AssignProcessToJobObject(job, child.as_raw_handle() as _);
        }
    }
}

/// The first `name` in a directory of PATH given as an absolute path that is not inside the
/// current directory or its git work tree: never the current directory, a relative entry, a
/// repository's own `bin` (a `.envrc` can put one on PATH) or the directory of scrutate itself,
/// which a plain `Command::new` searches on Windows.
fn on_path(name: &str) -> Option<std::path::PathBuf> {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let here = std::env::current_dir().ok().and_then(|d| std::fs::canonicalize(d).ok());
    let top = here.as_ref().and_then(|h| h.ancestors().find(|a| a.join(".git").exists()).map(std::path::Path::to_path_buf));
    let inside = |d: &std::path::Path| {
        let d = std::fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf());
        here.as_ref().is_some_and(|h| d.starts_with(h)) || top.as_ref().is_some_and(|t| d.starts_with(t))
    };
    std::env::split_paths(&std::env::var_os("PATH")?).filter(|d| d.is_absolute() && !inside(d)).map(|d| d.join(&file)).find(|p| p.is_file())
}

/// The most bytes of one line read at a time.
const PIECE: usize = 1 << 20;

/// Streams the child's standard output to ours, line by line, with secrets redacted. With a
/// `room`, it stops at that many bytes, kills the child and says so. -> (the exit code, the bytes
/// the child wrote that were read, the bytes sent)
fn stream(mut child: std::process::Child, room: Option<usize>) -> (i32, usize, usize) {
    use std::io::BufRead;
    let out = child.stdout.take().expect("a piped stdout");
    let mut reader = std::io::BufReader::new(out);
    let mut so = std::io::stdout().lock();
    let redact = crate::secrets::on(true);
    if redact {
        if let Ok(cwd) = std::env::current_dir() {
            crate::secrets::know_key_lines_at(&cwd);
        }
    }
    let mut r = crate::secrets::Redactor::default();
    // a line of a key's body grep shows without its armour, read back from its file
    let mut keys = crate::secrets::KeyFiles::default();
    let (mut read, mut sent, mut cut) = (0usize, 0usize, false);
    let mut line = Vec::new();
    loop {
        line.clear();
        // a line longer than this (a minified file, a device with no line ends) comes in pieces,
        // each redacted, so memory stays bounded
        match (&mut reader).take(PIECE as u64).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
        let shown: Vec<u8> = if !redact {
            line.clone()
        } else {
            // a line that is not UTF-8 (one stray byte, a piece cut inside a character) is read
            // lossily; it goes out as written unless that finds a secret, then lossily, redacted
            let (text, exact) = match std::str::from_utf8(&line) {
                Ok(t) => (std::borrow::Cow::Borrowed(t), true),
                Err(_) => (String::from_utf8_lossy(&line), false),
            };
            let mut t = text.to_string();
            let (prefix, rest) = crate::secrets::split_line(&t);
            if crate::secrets::could_be_key_line(rest) {
                if let Some((path, n)) = crate::secrets::grep_place(prefix) {
                    if let Some((key, k)) = keys.line(path, n, || std::fs::File::open(path).ok()) {
                        let end = if rest.ends_with('\n') { "\n" } else { "" };
                        t = format!("{prefix}{key}{end}");
                        *r.counts.entry("private-key").or_default() += k;
                    }
                }
            }
            let mut one = [t];
            let marks: usize = r.redact_shown(&mut one, 0, usize::MAX).iter().sum();
            let [t] = one;
            if exact || marks > 0 || t != text {
                t.into_bytes()
            } else {
                line.clone()
            }
        };
        if room.is_some_and(|room| sent + shown.len() > room) {
            cut = true;
            break;
        }
        if so.write_all(&shown).is_err() {
            break;
        }
        sent += shown.len();
    }
    if cut {
        let _ = child.kill();
        let note = "[scrutate] output cut here: narrow the search, or pass a path\n";
        let _ = so.write_all(note.as_bytes());
        sent += note.len();
    }
    if let Some(note) = r.note() {
        let _ = writeln!(so, "{note}");
        sent += note.len() + 1;
    }
    let _ = so.flush();
    let status = child.wait().ok().and_then(|s| s.code());
    // a cut search had matches, as the program would have said
    (if cut { 0 } else { status.unwrap_or(2) }, read, sent)
}

/// `scrutate hook run [--session ID] [--piped] -- PROGRAM ARGS...`: the search a rewritten
/// command asks, answered as the program would answer it. Its output streams through, secrets
/// redacted; when no pipe takes it, it stops at the agent's byte budget (a pipe's next command
/// reads it whole). There is no timer of its own: the agent's command timeout governs, as it
/// would without the hook.
pub fn run_main(args: &[String]) -> i32 {
    let (mut session, mut piped, mut i) = (String::new(), false, 0);
    while i < args.len() && args[i] != "--" {
        match (args[i].as_str(), args.get(i + 1)) {
            ("--session", Some(v)) => {
                session = v.clone();
                i += 2;
            }
            ("--piped", _) => {
                piped = true;
                i += 1;
            }
            _ => {
                eprintln!("scrutate hook run: usage: scrutate hook run [--session ID] [--piped] -- PROGRAM ARGS...");
                return 2;
            }
        }
    }
    let argv = &args[(i + 1).min(args.len())..];
    if !allowed(argv) {
        eprintln!("scrutate hook run: only a search by {} that runs no program and writes no file is answered", TOOLS.join(", "));
        return 2;
    }
    let Ok(exe) = std::env::current_exe() else { return 2 };
    let Some(mut c) = command(&exe, argv) else {
        // as sh says it, with its code
        eprintln!("{}: command not found", argv[0]);
        return 127;
    };
    #[cfg(unix)]
    let mut pipe = die_with_us(&mut c);
    let child = match c.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("{}: {e}", argv[0]);
            return 126;
        }
    };
    // the watcher has its read end now; ours goes, the write end stays until the program is done
    #[cfg(unix)]
    let _alive = pipe.take().map(|(r, w)| {
        drop(r);
        w
    });
    #[cfg(windows)]
    die_with_us_windows(&child);
    let budget = std::env::var("SCRUTATE_MAX_BYTES").ok().and_then(|v| v.parse::<usize>().ok()).map_or(crate::ops::AGENT_BYTES, |n| n.clamp(1_000, 1_000_000));
    let (code, read, sent) = stream(child, (!piped).then_some(budget));
    let _ = crate::hook::count_bash(&session, read, sent);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn words(cmd: &str) -> Option<Vec<String>> {
        first_command(cmd).map(|f| f.words)
    }

    #[test]
    fn the_first_command_ends_at_the_first_operator_and_keeps_its_words() {
        assert_eq!(words("rg -n foo src"), Some(w(&["rg", "-n", "foo", "src"])));
        assert_eq!(words("rg 'a b' \"c d\" e\\ f"), Some(w(&["rg", "a b", "c d", "e f"])));
        assert_eq!(words("rg 'a|b' x; ls").map(|w| w.len()), Some(3));
        assert_eq!(words("grep foo *.rs"), Some(w(&["grep", "foo", "*.rs"])));
        assert_eq!(words("rg foo 2>/dev/null"), Some(w(&["rg", "foo"])));
        assert_eq!(words("rg foo 2>/dev/null src"), Some(w(&["rg", "foo", "src"])));
        // `NUL` is the null device on Windows only; elsewhere it is a file of the current directory
        for cmd in ["rg foo 2>NUL", "rg foo >NUL", "rg foo &>NUL"] {
            assert_eq!(words(cmd), cfg!(windows).then(|| w(&["rg", "foo"])), "{cmd:?}");
        }
        assert_eq!(words("rg foo 2>&1 | cut -c1"), Some(w(&["rg", "foo"])));
        assert_eq!(words("rg foo >/dev/null"), Some(w(&["rg", "foo"])));
        assert_eq!(words("rg foo &>/dev/null && echo yes"), Some(w(&["rg", "foo"])));

        for (cmd, end, piped) in [
            ("rg foo | head", 7, true),
            ("rg foo |& cat", 7, true),
            ("rg foo || true", 7, false),
            ("rg foo && ls", 7, false),
            ("rg foo; ls", 6, false),
            ("rg foo & ls", 7, false),
            ("rg foo\nls", 6, false),
            ("  rg foo", 8, false),
        ] {
            let f = first_command(cmd).unwrap_or_else(|| panic!("{cmd:?}"));
            assert_eq!((f.end, f.piped), (end, piped), "{cmd:?}");
        }
        for cmd in [
            "rg foo > out",
            "rg foo >> out",
            "rg foo >| out",
            "rg foo &> out",
            "rg foo &>> /dev/null",
            "rg foo 1> out",
            "rg foo 2>file",
            "rg foo 2> err.txt src",
            "rg foo 2>>file",
            "rg foo 2>>/dev/null",
            "rg foo 2>~/.bashrc",
            "rg foo >|/dev/null",
            "rg foo 1>file",
            "rg foo 2>&2",
            "rg foo 2>&1x",
            "rg foo 3>file",
            "rg foo>out",
            // only the null device as written, right after the `>`
            "rg foo 2> /dev/null",
            "rg foo 2>/dev//null",
            "rg foo 2>/dev/./null",
            "rg foo 2>\"/dev/null\"",
            "rg foo 2>'/dev/null'",
            "rg foo 2>/dev/nullx",
            "rg foo 2>/dev/tcp/127.0.0.1/9",
            "rg foo 2>&10",
            "rg foo &> /dev/null",
            // bash reads `\r` as part of a word
            "rg foo\r> victim",
            "rg foo\r2>victim",
            "rg foo\r 2> victim",
            "rg foo\r| tee victim",
            "rg foo < in",
            "rg foo <<EOF",
            "rg $PAT",
            "rg \"$PAT\"",
            "rg `x`",
            "rg $(x)",
            "rg {a,b}",
            "(rg foo)",
            "X=1 rg foo",
            "/usr/bin/rg foo",
            "rg foo # c",
            "rg 'open",
            "rg foo 2>",
            "",
        ] {
            assert_eq!(first_command(cmd), None, "{cmd:?}");
        }
    }

    #[test]
    fn only_a_read_only_search_is_allowed() {
        // on Windows the first `find` on PATH may be System32's, so find is never allowed there
        for find in [&["find", ".", "-name", "*.py", "-type", "f"][..], &["find", ".", "-size", "-10k", "-mtime", "-1"]] {
            assert_eq!(allowed(&w(find)), !cfg!(windows), "{find:?}");
        }
        // a flag's value is skipped as rg skips it, whatever it looks like
        for ok in [
            &["rg", "foo"][..],
            &["grep", "-rn", "foo", "src"],
            &["rg", "-e", "--", "x"],
            &["rg", "-g", "*.rs", "--", "-v"],
            &["rg", "-ge", "foo"],
            &["rg", "--glob=--", "foo"],
            &["rg", "-n", "--", "foo", "src"],
            // the tables: clusters, a value in its cluster, grep's -NUM, and --color with no value
            &["rg", "-tpy", "-uu", "-.", "--hidden", "foo"],
            &["rg", "--type=py", "--max-count", "3", "foo"],
            &["grep", "-A3", "-rn", "foo", "src"],
            &["grep", "-5", "foo", "a.txt"],
            &["grep", "--color", "foo", "a.txt"],
            &["grep", "--color=auto", "--include=*.py", "-rn", "foo", "."],
        ] {
            assert!(allowed(&w(ok)), "{ok:?}");
        }
        for no in [
            &["find", ".", "-delete"][..],
            &["find", ".", "-exec", "rm", "{}", "+"],
            &["find", ".", "-execdir", "ls", "+"],
            &["find", ".", "-ok", "rm"],
            &["find", ".", "-okdir", "rm"],
            &["find", ".", "-fprint", "o"],
            &["find", ".", "-fprint0", "o"],
            &["find", ".", "-fprintf", "o", "x"],
            &["find", ".", "-fls", "o"],
            &["find", ".", "-newerXY", "x"],
            &["rg", "--pre", "./p", "x"],
            &["rg", "--pre=./p", "x"],
            &["rg", "--pre-glob", "*.pdf", "x"],
            &["rg", "-z", "x"],
            &["rg", "--search-zip", "x"],
            &["rg", "-iz", "x"],
            &["rg", "--hostname-bin", "./h", "x"],
            &["rg", "--index", "x"],
            &["rg", "--root", "/", "x"],
            // a `--` that is a flag's value ends nothing: the next word is a flag
            &["rg", "-g", "--", "--pre=./p", "hello", "f.txt"],
            &["rg", "-e", "--", "--pre=./p", "hello", "f.txt"],
            &["rg", "--regexp", "--", "--pre=./p", "hello", "f.txt"],
            &["rg", "-e", "--", "-z", "hello"],
            &["rg", "--glob", "--", "--pre-glob=x", "hello"],
            &["rg", "-g", "--", "--index", "x"],
            // and a flag that runs a program is refused wherever it sits, after a real `--` too
            &["rg", "-i", "--", "-z"],
            &["rg", "--", "--pre=./p", "x"],
            &["rg", "-e", "--hostname-bin=./h", "x"],
            // a flag without its value
            &["rg", "x", "-e"],
            &["rg", "x", "--glob"],
            // a flag the table does not know, today's or a future release's
            &["rg", "--exec-later", "x", "pat"],
            &["rg", "-K", "pat"],
            &["grep", "--no-such-flag", "foo"],
            &["grep", "-p", "foo"],
            &["grep", "--count=3", "foo"],
            &["cat", "x"],
            &["/bin/rg", "x"],
        ] {
            assert!(!allowed(&w(no)), "{no:?}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_quoted_word_reads_back_as_itself_in_sh() {
        for s in ["plain", "it's", "a \"b\"", "$HOME", "`x`", "a\nb", "-e", "--", "", "\\", "'", "*"] {
            let q = quote(s);
            let out = Command::new("sh").arg("-c").arg(format!("printf %s {q}")).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), s, "{q}");
        }
    }

    #[test]
    fn claude_gets_the_line_with_a_prefix_and_codex_only_a_search_with_head_or_tail() {
        let claude = |cmd: &str| {
            json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "session_id": "s1", "cwd": "/r",
            "tool_input": {"command": cmd, "description": "find foo", "timeout": 1000}})
        };
        let d = decide(&claude("grep -rn foo src | cut -d: -f1 && echo ok"), "/bin/scrutate").unwrap();
        let out = &d["hookSpecificOutput"];
        assert!(out.get("permissionDecision").is_none(), "{d}");
        assert_eq!(out["updatedInput"]["description"], "find foo");
        assert_eq!(out["updatedInput"]["timeout"], 1000);
        assert_eq!(out["updatedInput"]["command"], "'/bin/scrutate' hook run --session 's1' --piped -- grep -rn foo src | cut -d: -f1 && echo ok");
        let codex =
            |cmd: &str| json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "turn_id": "t", "session_id": "s2", "tool_input": {"command": cmd}});
        let d = decide(&codex("rg -n x | head -5"), "/bin/scrutate").unwrap();
        assert_eq!(d["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(d["hookSpecificOutput"]["updatedInput"], json!({"command": "'/bin/scrutate' hook run --session 's2' --piped -- rg -n x | head -5"}));
        assert!(decide(&codex("rg x"), "/b").is_some());
        assert!(decide(&codex("rg x | tail -n 3"), "/b").is_some());
        for cmd in ["rg x | cut -c1", "rg x && rm -rf y", "rg x; ls", "rg x | head | sh", "rg x | head > f"] {
            assert!(decide(&codex(cmd), "/b").is_none(), "{cmd:?}");
        }
        assert!(decide(&claude("ls -la"), "/b").is_none());
        for cmd in ["rg foo\r> victim", "rg foo\r2>victim", "rg foo\r 2> victim", "rg foo\r| tee victim", "rg foo\u{7}x", "rg foo\u{1b}[0m"] {
            assert!(decide(&claude(cmd), "/b").is_none(), "{cmd:?}");
            assert!(decide(&codex(cmd), "/b").is_none(), "{cmd:?}");
        }
        for cmd in ["rg foo 2> victim", "rg foo 2> f | head -5"] {
            assert!(decide(&claude(cmd), "/b").is_none(), "{cmd:?}");
            assert!(decide(&codex(cmd), "/b").is_none(), "{cmd:?}");
        }
        // what follows the first command is Claude Code's to approve by its own rules, never Codex's allow
        assert!(decide(&codex("rg foo | head -5 2> f"), "/b").is_none());
        assert!(decide(&claude("find . -delete"), "/b").is_none());
        let grep_tool = json!({"hook_event_name": "PreToolUse", "tool_name": "Grep", "tool_input": {"command": "rg foo"}});
        assert!(decide(&grep_tool, "/b").is_none());
    }
}
