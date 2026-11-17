//! `scrutate hook grep`: Claude Code's `PreToolUse` hook for its Grep tool. It answers the call
//! with Engine A's matches, the lines ripgrep gives for the same arguments, and denies the call
//! with that answer as the reason, which Claude Code hands to the model in place of Grep's
//! result. Anything it cannot answer exactly (an argument it does not map, an error, a timeout)
//! prints nothing and exits 0, so Grep runs as if there were no hook. `scrutate hook stats` sums
//! the bytes Grep would have returned against the bytes sent.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::instant::{self, Case, Files, Mode, Output, Pattern, Query, Record, Report};

/// A search that takes longer lets Grep run instead.
const DEADLINE: Duration = Duration::from_secs(3);
/// Grep's own default `head_limit`.
const GREP_HEAD: usize = 250;
/// Grep shows a longer line as a note, as `rg --max-columns 500` does.
const GREP_COLUMNS: usize = 500;
/// The directories Grep leaves out (`--glob !.git` and so on).
const VCS: [&str; 6] = [".git", ".svn", ".hg", ".bzr", ".jj", ".sl"];
/// The `type` names whose files are the same in ripgrep and in scrutate's registry (a test holds
/// the list to `rg --type-list`).
pub const SAME_TYPES: [&str; 23] = [
    "asm", "cmake", "cs", "csharp", "dart", "elisp", "elm", "erlang", "fish", "gleam", "go", "julia", "kotlin", "lua", "nix", "py", "python", "racket", "rst",
    "rust", "solidity", "swift", "yaml",
];
const KEYS: [&str; 15] = ["pattern", "path", "glob", "type", "output_mode", "-B", "-A", "-C", "context", "-n", "-i", "-o", "head_limit", "offset", "multiline"];
/// The line before the result: short, as it is paid on every answer, and enough for the model
/// to read what follows as Grep's result (Claude Code says a hook denied the call).
pub const HEADER: &str = "Grep result:";

/// The answer to one Grep call.
#[derive(Debug)]
pub struct Answer {
    /// what the model gets: the header, then the result as Grep words it, bounded
    pub reason: String,
    /// the bytes of the result Grep would have returned
    pub grep_bytes: usize,
}

/// The hook: reads the call on standard input and prints the decision, or nothing. Always 0: an
/// exit code 2 would block the call.
pub fn grep_main() -> i32 {
    let answered = std::panic::catch_unwind(|| {
        let mut input = Vec::new();
        std::io::stdin().take(4 << 20).read_to_end(&mut input).ok()?;
        let input: Value = serde_json::from_slice(&input).ok()?;
        let budget =
            std::env::var("SCRUTATE_MAX_BYTES").ok().and_then(|v| v.parse::<usize>().ok()).map_or(crate::ops::AGENT_BYTES, |n| n.clamp(1_000, 1_000_000));
        let a = answer(&input, budget)?;
        Some((input.get("session_id").and_then(Value::as_str).unwrap_or("").to_string(), a))
    });
    if let Ok(Some((session, a))) = answered {
        let decision = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": a.reason}});
        let mut so = std::io::stdout().lock();
        if writeln!(so, "{decision}").and_then(|_| so.flush()).is_ok() {
            let _ = count(&session, a.grep_bytes, a.reason.len());
        }
    }
    0
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Content,
    Files,
    Count,
}

/// What a Grep call asks, as ripgrep would read it.
struct Ask {
    pattern: String,
    target: PathBuf,
    cwd: PathBuf,
    globs: Vec<String>,
    file_type: Option<String>,
    shape: Shape,
    before: usize,
    after: usize,
    breaks: bool,
    numbers: bool,
    insensitive: bool,
    multiline: bool,
    /// `None`: no limit
    head: Option<usize>,
    offset: usize,
}

/// The answer to `input` (Claude Code's hook input), or `None` when Grep must run.
pub fn answer(input: &Value, budget: usize) -> Option<Answer> {
    // a path in an index directory is refused, not declined: Grep would then search it itself
    if let (Some(cwd), Some(path)) = (input.get("cwd").and_then(Value::as_str), input.pointer("/tool_input/path").and_then(Value::as_str)) {
        let cwd = Path::new(cwd);
        let index = crate::files::IndexFiles::near(cwd);
        if index.holds(&cwd.join(path)) {
            let reason = crate::files::refused(path, &cwd.join(path), &index, "searched");
            return Some(Answer { grep_bytes: 0, reason });
        }
    }
    let ask = read_ask(input)?;
    let project = std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from).unwrap_or_else(|| ask.cwd.clone());
    // Grep asks before it reads outside the project, and Claude Code's read rules filter it: the
    // hook answers neither case
    if !ask.target.starts_with(&project) || rules_limit_reads(&project) {
        return None;
    }
    let mut lines = search(&ask)?;
    let (first, end, _) = window(lines.len(), ask.head, ask.offset);
    let mut keyed = 0;
    if crate::secrets::on(true) {
        crate::secrets::know_key_lines_at(&ask.target);
        keyed = key_lines(&mut lines[first..end]);
        // a path that holds a secret is one too
        let mut prefixes: Vec<String> = lines.iter_mut().map(|i| std::mem::take(&mut i.prefix)).collect();
        keyed += crate::secrets::Redactor::default().redact_shown(&mut prefixes, first, budget).iter().sum::<usize>();
        lines.iter_mut().zip(prefixes).for_each(|(i, p)| i.prefix = p);
    }
    let mut texts: Vec<String> = lines.iter_mut().map(|i| std::mem::take(&mut i.text)).collect();
    let note = redacted(&mut texts, first, budget, keyed);
    lines.iter_mut().zip(texts).for_each(|(i, t)| i.text = t);
    let mut answer = render(&ask, lines, budget);
    if let Some(note) = note {
        answer.reason.push('\n');
        answer.reason.push_str(&note);
    }
    Some(answer)
}

/// The lines a hook shows an agent with their secrets redacted, unless SCRUTATE_REDACT=off: from
/// `first`, up to `room` bytes (`secrets::Redactor::redact_shown`). -> the closing line that says
/// how many were redacted, `more` made before this among them, when any were. Every hook writes
/// its lines through this.
pub(crate) fn redacted(lines: &mut [String], first: usize, room: usize, more: usize) -> Option<String> {
    if !crate::secrets::on(true) {
        return None;
    }
    let n: usize = crate::secrets::Redactor::default().redact_shown(lines, first, room).iter().sum();
    crate::secrets::note(n + more)
}

/// The lines of a private key's body redacted as their whole file reads them, armour shown or not
/// (`-A 1` after the armour, a match on the last line alone). -> how many redactions.
fn key_lines(items: &mut [Item]) -> usize {
    let mut keys = crate::secrets::KeyFiles::default();
    items
        .iter_mut()
        .map(|i| {
            let Some((path, n)) = &i.at else { return 0 };
            if !crate::secrets::could_be_key_line(&i.text) {
                return 0;
            }
            let Some((line, k)) = keys.line(path, *n, || crate::secrets::open_regular(path)) else { return 0 };
            i.text = line.to_string();
            k
        })
        .sum()
}

fn read_ask(input: &Value) -> Option<Ask> {
    if input.get("hook_event_name")?.as_str()? != "PreToolUse" || input.get("tool_name")?.as_str()? != "Grep" {
        return None;
    }
    let t = input.get("tool_input")?.as_object()?;
    if t.keys().any(|k| !KEYS.contains(&k.as_str())) {
        return None;
    }
    let num = |k: &str| -> Option<Option<usize>> {
        match t.get(k) {
            None | Some(Value::Null) => Some(None),
            Some(v) => v.as_u64().and_then(|n| usize::try_from(n).ok()).map(Some),
        }
    };
    let flag = |k: &str, default: bool| -> Option<bool> {
        match t.get(k) {
            None | Some(Value::Null) => Some(default),
            Some(v) => v.as_bool(),
        }
    };
    let text = |k: &str| -> Option<Option<&str>> {
        match t.get(k) {
            None | Some(Value::Null) => Some(None),
            Some(v) => v.as_str().map(Some),
        }
    };
    let pattern = text("pattern")?.filter(|p| !p.is_empty())?.to_string();
    let shape = match text("output_mode")?.unwrap_or("files_with_matches") {
        "content" => Shape::Content,
        "files_with_matches" => Shape::Files,
        "count" => Shape::Count,
        _ => return None,
    };
    let (context, c, b, a) = (num("context")?, num("-C")?, num("-B")?, num("-A")?);
    if shape == Shape::Content && flag("-o", false)? {
        return None;
    }
    let (before, after) = match context.or(c) {
        Some(n) => (n, n),
        None => (b.unwrap_or(0), a.unwrap_or(0)),
    };
    let cwd = PathBuf::from(input.get("cwd")?.as_str()?);
    if !cwd.is_absolute() {
        return None;
    }
    let target = match text("path")? {
        Some(p) if p.starts_with('~') => return None,
        Some(p) => lexical(&cwd.join(p))?,
        None => lexical(&cwd)?,
    };
    let target = PathBuf::from(real_path(&std::fs::canonicalize(&target).ok()?.to_string_lossy(), target.to_str()?, cfg!(windows))?);
    let mut globs: Vec<String> = VCS.iter().map(|d| format!("!{d}")).collect();
    for g in split_glob(text("glob")?.unwrap_or("")) {
        // a glob with a slash matches from ripgrep's working directory, which the hook cannot see
        let bare = g.strip_prefix('!').unwrap_or(&g);
        if bare.strip_prefix("**/").unwrap_or(bare).contains('/') || (cfg!(windows) && g.contains('\\')) {
            return None;
        }
        globs.push(g);
    }
    let file_type = match text("type")?.filter(|t| !t.is_empty()) {
        Some(t) if SAME_TYPES.contains(&t) => Some(t.to_string()),
        Some(_) => return None,
        None => None,
    };
    Some(Ask {
        pattern,
        target,
        cwd,
        globs,
        file_type,
        shape,
        before: if shape == Shape::Content { before } else { 0 },
        after: if shape == Shape::Content { after } else { 0 },
        breaks: [context, c, b, a].iter().any(|n| n.is_some_and(|n| n > 0)),
        numbers: flag("-n", true)?,
        insensitive: flag("-i", false)?,
        multiline: flag("multiline", false)?,
        head: match num("head_limit")? {
            None => Some(GREP_HEAD),
            Some(0) => None,
            Some(n) => Some(n),
        },
        offset: num("offset")?.unwrap_or(0),
    })
}

/// `p` with `.` and `..` resolved by name, as Node's `path.resolve` does.
fn lexical(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out.is_absolute().then_some(out)
}

/// The path Grep searches for `p`, from `real`, what `canonicalize` gave: Claude Code takes the
/// real path, and on Windows that may differ from `p` in the case of its letters only. Anything
/// else is a link on the way, which makes ripgrep's paths differ from the walk's, or a path not on
/// a drive letter, which Claude Code searches by another way (`fr(e)&&!Qd(e)||Ga(e)`).
fn real_path(real: &str, p: &str, windows: bool) -> Option<String> {
    if !windows {
        return (real == p).then(|| p.to_string());
    }
    let real = crate::paths::plain_on(real, windows);
    let b = real.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && &b[1..3] == b":\\";
    (drive && real.to_lowercase() == p.to_lowercase()).then(|| real.to_string())
}

/// What follows `base` in `p` when `p` is `base` or under it, as Node's `path.relative` finds it:
/// without case on Windows.
fn under<'a>(p: &'a str, base: &str, windows: bool) -> Option<&'a str> {
    let sep = if windows { '\\' } else { '/' };
    let base = base.trim_end_matches(sep);
    let head = p.get(..base.len())?;
    if head != base && !(windows && head.to_lowercase() == base.to_lowercase()) {
        return None;
    }
    let rest = &p[base.len()..];
    if rest.is_empty() { Some(rest) } else { rest.strip_prefix(sep) }
}

/// A path of Engine A's (`/` between its parts) as Grep prints it: `rat` of Claude Code, the
/// path from the session's directory when under it, else as ripgrep gave it, with `\` on Windows.
fn shown(path: &str, cwd: &str, windows: bool) -> String {
    let (p, cwd) = if windows { (path.replace('/', "\\"), cwd.replace('/', "\\")) } else { (path.to_string(), cwd.to_string()) };
    match under(&p, &cwd, windows) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => p,
    }
}

/// Grep's `glob` argument as it splits it: at whitespace, and at commas outside a word with braces.
fn split_glob(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for w in s.split_whitespace() {
        if w.contains('{') && w.contains('}') {
            out.push(w.to_string());
        } else {
            out.extend(w.split(',').filter(|g| !g.is_empty()).map(String::from));
        }
    }
    out
}

/// Whether a settings file of Claude Code has a `deny` or `ask` rule for reads, which Grep's
/// answer would have to follow. A settings file that does not parse counts as one.
fn rules_limit_reads(project: &Path) -> bool {
    let home = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(|h| PathBuf::from(h).join(".claude")));
    let mut files = vec![project.join(".claude").join("settings.json"), project.join(".claude").join("settings.local.json")];
    files.extend(home.map(|h| h.join("settings.json")));
    let managed = PathBuf::from(if cfg!(target_os = "macos") {
        "/Library/Application Support/ClaudeCode/managed-settings.json"
    } else if cfg!(windows) {
        r"C:\Program Files\ClaudeCode\managed-settings.json"
    } else {
        "/etc/claude-code/managed-settings.json"
    });
    files.extend(drop_ins(&managed));
    files.push(managed);
    // policies the hook does not read (a macOS profile, Windows registry policies) may hold read
    // rules: where one exists, Grep runs and applies it itself
    if managed_policy_elsewhere() {
        return true;
    }
    files.iter().any(|f| match std::fs::read(f) {
        Err(_) => false,
        Ok(b) => match serde_json::from_slice::<Value>(&b) {
            Err(_) => true,
            Ok(v) => ["deny", "ask"].iter().any(|k| {
                v.pointer(&format!("/permissions/{k}"))
                    .and_then(Value::as_array)
                    .is_some_and(|rules| rules.iter().any(|r| r.as_str().is_none_or(|r| r.starts_with("Read") || r.starts_with("Grep"))))
            }),
        },
    })
}

/// Every `*.json` of the `managed-settings.d` directory beside the managed settings file, which
/// Claude Code reads as managed settings too, in name order.
fn drop_ins(managed: &Path) -> Vec<PathBuf> {
    let Some(Ok(rd)) = managed.parent().map(|d| std::fs::read_dir(d.join("managed-settings.d"))) else { return Vec::new() };
    let mut drops: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    drops.sort();
    drops
}

/// Whether Claude Code has managed settings in a place the hook does not read: a configuration
/// profile's preferences on macOS, the policy keys of the registry on Windows.
fn managed_policy_elsewhere() -> bool {
    #[cfg(target_os = "macos")]
    {
        let user = std::env::var("USER").unwrap_or_default();
        let plist = "com.anthropic.claudecode.plist";
        let dir = Path::new("/Library/Managed Preferences");
        dir.join(plist).exists() || (!user.is_empty() && dir.join(&user).join(plist).exists())
    }
    #[cfg(windows)]
    {
        windows_policy::exists()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        false
    }
}

#[cfg(windows)]
mod windows_policy {
    use std::ffi::c_void;

    const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
    const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
    const KEY_READ: u32 = 0x2_0019;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegOpenKeyExW(key: isize, sub: *const u16, options: u32, sam: u32, out: *mut *mut c_void) -> i32;
        fn RegCloseKey(key: *mut c_void) -> i32;
    }

    /// Whether `SOFTWARE\Policies\ClaudeCode` exists under HKLM or HKCU.
    pub fn exists() -> bool {
        let sub: Vec<u16> = r"SOFTWARE\Policies\ClaudeCode".encode_utf16().chain([0]).collect();
        [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER].iter().any(|&root| {
            let mut out = std::ptr::null_mut();
            // SAFETY: a nul-terminated UTF-16 string and a valid out pointer; the key is closed
            let found = unsafe { RegOpenKeyExW(root, sub.as_ptr(), 0, KEY_READ, &mut out) } == 0;
            if found {
                unsafe { RegCloseKey(out) };
            }
            found
        })
    }
}

/// One line of Grep's result list, before the paging.
struct Item {
    /// what Grep writes before the line: its path and number (`a.py:3:`, `a.py-4-`)
    prefix: String,
    text: String,
    /// the file and line number of a line of a file
    at: Option<(Arc<str>, u64)>,
    /// the matching lines of a `count` line
    count: u64,
}

/// Engine A's answer as Grep's lines, in path order (by modification time, newest first, for
/// `files_with_matches`, as Grep sorts them); `None` when it cannot stand for ripgrep's.
fn search(ask: &Ask) -> Option<Vec<Item>> {
    // no thread to keep the deadline on wasi: Grep answers there
    if cfg!(target_os = "wasi") {
        return None;
    }
    let q = Query {
        pattern: Pattern {
            text: if ask.multiline { format!("(?s){}", ask.pattern) } else { ask.pattern.clone() },
            mode: Mode::Regex,
            case: if ask.insensitive { Case::Insensitive } else { Case::Sensitive },
            multiline: ask.multiline,
            ..Pattern::default()
        },
        report: match ask.shape {
            Shape::Content => Report::Lines,
            Shape::Files => Report::Files,
            Shape::Count => Report::Counts,
        },
        before: ask.before,
        after: ask.after,
        first_in_path_order: true,
        ..Query::default()
    };
    let f = Files {
        hidden: true,
        globs: ask.globs.clone(),
        types: ask.file_type.iter().cloned().collect(),
        paths_as_given: true,
        map_named: true,
        ..Files::walk(vec![ask.target.clone()])
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let timer = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(DEADLINE);
        timer.store(true, Ordering::Relaxed);
    });
    let mut records = Vec::new();
    let mut odd = false;
    let stats = instant::search(
        &q,
        &f,
        cancel,
        Output::Records(&mut |r| {
            odd |= r.path_bytes().is_some() || matches!(r, Record::Note { .. });
            records.push(r);
        }),
    )
    .ok()?;
    if odd || stats.cancelled || stats.errors > 0 || records.is_empty() {
        return None;
    }
    let dir = ask.target.is_dir();
    let cwd = ask.cwd.to_string_lossy();
    let shown = |p: &str| shown(p, &cwd, cfg!(windows));
    let mut items = Vec::new();
    match ask.shape {
        Shape::Content => {
            let mut last: Option<(std::sync::Arc<str>, u64)> = None;
            for r in &records {
                let (l, sep) = match r {
                    Record::Match(l) => (l, ':'),
                    Record::Context(l) => (l, '-'),
                    _ => return None,
                };
                if ask.breaks
                    && let Some((p, n)) = &last
                    && (*p != l.path || l.line > n + 1)
                {
                    items.push(Item { prefix: String::new(), text: "--".into(), count: 0, at: None });
                }
                // a multi-line match is one record; Grep shows each of its lines with its number
                let parts: Vec<&str> = l.text.strip_suffix('\n').unwrap_or(&l.text).split('\n').collect();
                last = Some((l.path.clone(), l.line + parts.len() as u64 - 1));
                for (k, part) in parts.iter().enumerate() {
                    let part = part.trim_end_matches('\r');
                    let text = if part.encode_utf16().count() > GREP_COLUMNS {
                        if sep == ':' { "[Omitted long matching line]" } else { "[Omitted long context line]" }
                    } else {
                        part
                    };
                    let mut prefix = if dir { format!("{}{sep}", shown(&l.path)) } else { String::new() };
                    if ask.numbers {
                        prefix.push_str(&format!("{}{sep}", l.line + k as u64));
                    }
                    items.push(Item { prefix, text: text.to_string(), count: 0, at: Some((l.path.clone(), l.line + k as u64)) });
                }
            }
        }
        Shape::Count => {
            for r in &records {
                let Record::Count { path, count, .. } = r else { return None };
                if *count > 0 {
                    items.push(Item { prefix: String::new(), text: format!("{}:{count}", shown(path)), count: *count, at: None });
                }
            }
        }
        Shape::Files => {
            let mut files: Vec<(f64, String)> = Vec::new();
            for r in &records {
                let Record::File { path, .. } = r else { return None };
                let ms = std::fs::metadata(&**path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
                files.push((ms, shown(path)));
            }
            files.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            items.extend(files.into_iter().map(|(_, text)| Item { prefix: String::new(), text, count: 0, at: None }));
        }
    }
    Some(items)
}

/// The window Grep shows of `len` items: from `offset`, `head` of them; and the limit it names
/// when there are more.
fn window(len: usize, head: Option<usize>, offset: usize) -> (usize, usize, Option<usize>) {
    let start = offset.min(len);
    match head {
        None => (start, len, None),
        Some(h) => (start, (offset.saturating_add(h)).min(len), (len.saturating_sub(offset) > h).then_some(h)),
    }
}

/// Grep's words for its paging: `limit: 250, offset: 5`.
fn pagination(limit: Option<usize>, offset: usize) -> String {
    let mut parts = Vec::new();
    if let Some(l) = limit {
        parts.push(format!("limit: {l}"));
    }
    if offset > 0 {
        parts.push(format!("offset: {offset}"));
    }
    parts.join(", ")
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Grep's result for these items, and the bounded answer the model gets.
fn render(ask: &Ask, items: Vec<Item>, budget: usize) -> Answer {
    let (start, end, limit) = window(items.len(), ask.head, ask.offset);
    let pages = pagination(limit, ask.offset);
    let lines: Vec<String> = items[start..end].iter().map(|i| format!("{}{}", i.prefix, i.text)).collect();
    let page: Vec<&str> = lines.iter().map(String::as_str).collect();
    let (pre, empty, post, noun) = match ask.shape {
        Shape::Content => {
            let empty = if ask.offset > 0 && !items.is_empty() { "No entries at this offset" } else { "No matches found" };
            (String::new(), empty.to_string(), if pages.is_empty() { String::new() } else { format!("\n[Showing results with pagination = {pages}]") }, "lines")
        }
        Shape::Count => {
            let total: u64 = items.iter().map(|i| i.count).sum();
            let empty = if total > 0 { "No entries at this offset" } else { "No matches found" };
            let with = if pages.is_empty() { String::new() } else { format!(" with pagination = {pages}") };
            (
                String::new(),
                empty.to_string(),
                format!("\nFound {} across {}.{with}", plural(total, "total occurrence", "total occurrences"), plural(items.len() as u64, "file", "files")),
                "files",
            )
        }
        Shape::Files => {
            let n = page.len() as u64;
            let empty = if ask.offset > 0 && !items.is_empty() {
                format!("No entries at this offset. [Showing results with pagination = {pages}]")
            } else {
                "No files found".into()
            };
            (
                format!("Found {}{}\n", plural(n, "file", "files"), if pages.is_empty() { String::new() } else { format!(" {pages}") }),
                empty,
                String::new(),
                "files",
            )
        }
    };
    let grep = if page.is_empty() {
        if ask.shape == Shape::Count { format!("{empty}{post}") } else { empty.clone() }
    } else {
        format!("{pre}{}{post}", page.join("\n"))
    };
    let reason = if page.is_empty() {
        format!("{HEADER}\n{grep}")
    } else {
        let room = budget.saturating_sub(HEADER.len() + 1 + pre.len() + post.len());
        format!("{HEADER}\n{pre}{}{post}", bounded(&page, start, end, room, noun))
    };
    Answer { reason, grep_bytes: grep.len() }
}

/// The lines that fit `room` bytes, whole, then a line naming the `offset` that continues them.
fn bounded(page: &[&str], start: usize, end: usize, room: usize, noun: &str) -> String {
    let more = |next: usize| format!("\n({} more {noun} not shown: call Grep again with offset={next}, or narrow the request)", end - next);
    // the widest closing line: its numbers have the most digits
    let room_for_more = more(start).len().max(more(end).len());
    let mut out = String::new();
    for (i, l) in page.iter().enumerate() {
        let tail = if i + 1 == page.len() { 0 } else { room_for_more };
        if i > 0 && out.len() + 1 + l.len() + tail > room {
            out.push_str(&more(start + i));
            return out;
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(l);
    }
    out
}

/// The file the counter appends to.
pub fn counter_path() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = if cfg!(windows) {
        var("LOCALAPPDATA")?.join("scrutate")
    } else {
        var("XDG_STATE_HOME").filter(|p| p.is_absolute()).unwrap_or(var("HOME")?.join(".local").join("state")).join("scrutate")
    };
    Some(dir.join("hook-grep.jsonl"))
}

/// One line per answered call: one write, so lines of hooks running at once do not mix.
fn count(session: &str, grep_bytes: usize, sent_bytes: usize) -> Result<()> {
    counted(json!({"session": session, "grep_bytes": grep_bytes, "sent_bytes": sent_bytes}))
}

/// Counts one shell search `hook run` answered: the bytes the program gave, the bytes sent; a
/// search `cut` at the byte bound gave only those read before it was ended.
pub(crate) fn count_bash(session: &str, full_bytes: usize, sent_bytes: usize, cut: bool) -> Result<()> {
    let mut rec = json!({"session": session, "grep_bytes": full_bytes, "sent_bytes": sent_bytes, "via": "bash"});
    if cut {
        rec["cut"] = json!(true);
    }
    counted(rec)
}

fn counted(mut rec: Value) -> Result<()> {
    let path = counter_path().context("no state directory")?;
    std::fs::create_dir_all(path.parent().expect("a file in a directory"))?;
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    rec["time"] = json!(time);
    let line = format!("{rec}\n");
    std::fs::OpenOptions::new().create(true).append(true).open(&path)?.write_all(line.as_bytes())?;
    Ok(())
}

#[derive(Default)]
struct Tally {
    calls: u64,
    grep: u64,
    sent: u64,
    saved: i64,
    /// shell searches ended at the byte bound: their output was not read whole
    cut: u64,
}

impl Tally {
    fn add(&mut self, grep: u64, sent: u64, cut: bool) {
        self.calls += 1;
        self.grep += grep;
        self.sent += sent;
        let saved = grep as i64 - sent as i64;
        // a cut search's whole output is more than was read, so its saving is at least 0
        self.saved += if cut { saved.max(0) } else { saved };
        self.cut += u64::from(cut);
    }

    fn json(&self) -> Value {
        json!({"calls": self.calls, "grep_bytes": self.grep, "sent_bytes": self.sent, "saved_bytes": self.saved, "cut_calls": self.cut})
    }

    fn text(&self) -> String {
        format!("{} calls, Grep would have returned {} bytes, scrutate sent {} ({} saved)", self.calls, self.grep, self.sent, self.saved)
    }
}

/// `scrutate hook stats`: the counter per session, in the order the sessions began, and in all.
pub fn stats(json_out: bool) -> Result<i32> {
    let path = counter_path().context("no state directory: set HOME or XDG_STATE_HOME")?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let mut sessions: Vec<(String, Tally)> = Vec::new();
    let mut all = Tally::default();
    // the shell searches `hook run` answered, apart: their bytes are the program's own output
    let mut bash = Tally::default();
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let (Some(g), Some(s)) = (v.get("grep_bytes").and_then(Value::as_u64), v.get("sent_bytes").and_then(Value::as_u64)) else { continue };
        let id = v.get("session").and_then(Value::as_str).unwrap_or("").to_string();
        let i = sessions.iter().position(|(k, _)| *k == id).unwrap_or_else(|| {
            sessions.push((id, Tally::default()));
            sessions.len() - 1
        });
        let shell = v.get("via").and_then(Value::as_str) == Some("bash");
        let cut = v.get("cut").and_then(Value::as_bool) == Some(true);
        for t in [Some(&mut sessions[i].1), Some(&mut all), shell.then_some(&mut bash)].into_iter().flatten() {
            t.add(g, s, cut);
        }
    }
    let mut so = std::io::stdout().lock();
    if json_out {
        let list: Vec<Value> = sessions
            .iter()
            .map(|(id, t)| {
                json!({"session": id})
                    .as_object()
                    .cloned()
                    .map(|mut o| {
                        o.extend(t.json().as_object().cloned().unwrap_or_default());
                        Value::Object(o)
                    })
                    .unwrap_or_default()
            })
            .collect();
        writeln!(so, "{}", json!({"counter": path.to_string_lossy(), "sessions": list, "total": all.json(), "shell": bash.json()}))?;
    } else if all.calls == 0 {
        eprintln!("no Grep call was answered yet (counter: {})", path.display());
    } else {
        for (id, t) in &sessions {
            writeln!(so, "session {}: {}", if id.is_empty() { "(none)" } else { id }, t.text())?;
        }
        writeln!(so, "all sessions: {}", all.text())?;
        if bash.calls > 0 {
            writeln!(so, "of which shell searches (hook run): {}", bash.text())?;
        }
        if all.cut > 0 {
            writeln!(so, "of which cut at the byte bound: {} (their whole output was not read, so their saving is at least what is shown)", all.cut)?;
        }
    }
    Ok(if all.calls == 0 { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grep_splits_its_glob_argument_at_spaces_and_commas() {
        assert_eq!(split_glob("*.rs, *.toml"), ["*.rs", "*.toml"]);
        assert_eq!(split_glob("*.{ts,tsx} !x"), ["*.{ts,tsx}", "!x"]);
        assert_eq!(split_glob(""), Vec::<String>::new());
    }

    #[test]
    fn the_window_and_its_words_are_greps() {
        assert_eq!(window(300, Some(250), 0), (0, 250, Some(250)));
        assert_eq!(window(300, Some(250), 100), (100, 300, None));
        assert_eq!(window(10, None, 3), (3, 10, None));
        assert_eq!(window(2, Some(250), 5), (2, 2, None));
        assert_eq!(pagination(Some(250), 5), "limit: 250, offset: 5");
    }

    #[test]
    fn a_bounded_page_keeps_whole_lines_and_names_the_offset() {
        let page = ["aaaa", "bbbb", "cccc"];
        assert_eq!(bounded(&page, 0, 3, 100, "lines"), "aaaa\nbbbb\ncccc");
        let more = "\n(2 more lines not shown: call Grep again with offset=11, or narrow the request)";
        let room = 4 + more.len() + 2;
        assert_eq!(bounded(&page, 10, 13, room, "lines"), format!("aaaa{more}"));
    }

    #[test]
    fn paths_are_shown_as_claude_codes_path_relative_gives_them() {
        assert_eq!(shown("/r/src/a.rs", "/r", false), "src/a.rs");
        assert_eq!(shown("/r2/a.rs", "/r", false), "/r2/a.rs");
        assert_eq!(shown("/R/a.rs", "/r", false), "/R/a.rs");
        assert_eq!(shown("/a.rs", "/", false), "a.rs");
        // Windows: `\` between the parts, and the session's directory matched without case
        assert_eq!(shown("C:/Users/X/repo/src/a.rs", r"C:\Users\X\repo", true), r"src\a.rs");
        assert_eq!(shown("C:/Users/X/repo/src/a.rs", r"c:\users\x\REPO", true), r"src\a.rs");
        assert_eq!(shown("C:/Users/X/repo/src/a.rs", "C:/Users/X/repo/", true), r"src\a.rs");
        assert_eq!(shown("C:/Users/X/repo2/a.rs", r"C:\Users\X\repo", true), r"C:\Users\X\repo2\a.rs");
        assert_eq!(shown("D:/repo/a.rs", r"C:\repo", true), r"D:\repo\a.rs");
        assert_eq!(shown("C:/a.rs", r"C:\", true), "a.rs");
    }

    #[test]
    fn the_searched_path_is_the_real_one_and_differs_only_in_case_on_windows() {
        assert_eq!(real_path("/r/a", "/r/a", false).as_deref(), Some("/r/a"));
        assert_eq!(real_path("/r/A", "/r/a", false), None);
        assert_eq!(real_path(r"\\?\C:\Users\X\repo", r"c:\users\x\repo", true).as_deref(), Some(r"C:\Users\X\repo"));
        assert_eq!(real_path(r"C:\Users\X\repo", r"C:\Users\X\repo", true).as_deref(), Some(r"C:\Users\X\repo"));
        assert_eq!(real_path(r"\\?\C:\elsewhere", r"C:\Users\X\link", true), None);
        assert_eq!(real_path(r"\\?\UNC\server\share\x", r"\\server\share\x", true), None);
        // a verbatim path Claude Code never sends, as a cwd: Grep runs
        assert_eq!(real_path(r"\\?\C:\x", r"\\?\C:\x", true), None);
    }

    #[test]
    fn the_drop_in_directory_beside_the_managed_settings_is_read_in_name_order() {
        let dir = std::env::temp_dir().join(format!("scrutate_drop_ins_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("managed-settings.d")).unwrap();
        for f in ["b.json", "a.json", "notes.txt"] {
            std::fs::write(dir.join("managed-settings.d").join(f), "{}").unwrap();
        }
        let got = drop_ins(&dir.join("managed-settings.json"));
        assert_eq!(got, [dir.join("managed-settings.d/a.json"), dir.join("managed-settings.d/b.json")]);
        assert!(drop_ins(&dir.join("elsewhere/managed-settings.json")).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lexical_paths_resolve_dots_by_name() {
        assert_eq!(lexical(Path::new("/a/./b/../c")), Some(PathBuf::from("/a/c")));
        assert_eq!(lexical(Path::new("x")), None);
    }
}
