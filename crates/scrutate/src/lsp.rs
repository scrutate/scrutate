//! Language Server Protocol front end on stdio (JSON-RPC 2.0, `Content-Length` framing): definitions,
//! references, workspace symbols and document outlines answered from the Engine B index. Hand-written
//! like the MCP server: the read-only subset an index-backed server needs is small.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::index::ChunkRow;
use crate::names::Names;
use crate::ops::{self, Answer, Op};
use crate::search::Searcher;
use crate::session::{Every, Session};

const MAX_MESSAGE: usize = 256 << 20;

fn send(msg: &Value) {
    let body = msg.to_string();
    // the stdout lock keeps a message whole when the indexing thread logs at the same time
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

fn log(kind: u8, message: &str) {
    send(&json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": kind, "message": message}}));
}

/// One message body, or None at end of input.
pub fn read_message(r: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut len: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let l = line.trim_end();
        if l.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = Some(v.trim().parse().context("bad Content-Length header")?);
            }
        }
    }
    let n = len.unwrap_or(0);
    if n > MAX_MESSAGE {
        bail!("message of {n} bytes is too large");
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

// ------------------------------------------------------------------------- positions and URIs

/// Byte offset of a UTF-16 column in `line` (past the end clamps to the end).
pub fn utf16_to_byte(line: &str, units: u64) -> usize {
    let mut acc = 0u64;
    for (i, c) in line.char_indices() {
        if acc >= units {
            return i;
        }
        acc += c.len_utf16() as u64;
    }
    line.len()
}

pub fn byte_to_utf16(line: &str, byte: usize) -> u64 {
    line[..byte.min(line.len())].encode_utf16().count() as u64
}

fn is_ident(c: char) -> bool {
    crate::ident::is_continue(c)
}

/// Byte range of the identifier at (or just before) `at`.
pub fn word_at(line: &str, at: usize) -> Option<(usize, usize)> {
    let at = at.min(line.len());
    let mut a = at;
    while let Some(c) = line[..a].chars().next_back().filter(|c| is_ident(*c)) {
        a -= c.len_utf8();
    }
    let mut b = at;
    while let Some(c) = line[b..].chars().next().filter(|c| is_ident(*c)) {
        b += c.len_utf8();
    }
    (a < b).then_some((a, b))
}

/// `a..b` widened to the name the language of `path` writes there, by the rule the parser names
/// calls with: `valid?` in Ruby, `update!` in Julia, `step'` in Haskell, `is.ready` in R,
/// `~Shape` and `operator==` in C++. In Rust the name of `r#type` is `type`.
pub(crate) fn widen(line: &str, a: usize, b: usize, path: &str) -> (usize, usize) {
    let Some(lang) = crate::langs::spec_for(path).map(|s| s.id) else { return (a, b) };
    let (word, rest) = (&line[a..b], &line[b..]);
    match lang {
        // a header is often C by its extension and C++ by its content
        "c" | "cpp" if line[..a].ends_with('~') => return (a - 1, b),
        "c" | "cpp" if word == "operator" => {
            let call = ["()", "[]"].iter().find(|p| rest.starts_with(**p)).map(|p| p.len());
            let n = call.unwrap_or_else(|| rest.find(|c: char| !"+-*/%^&|~!=<>,".contains(c)).unwrap_or(rest.len()));
            return (a, b + n);
        }
        "rust" if word == "r" && rest.starts_with('#') => return word_at(line, b + 1).unwrap_or((a, b)),
        _ => {}
    }
    let mut start = a;
    while let Some(c) = line[..start].chars().next_back().filter(|c| lang == "r" && (is_ident(*c) || *c == '.')) {
        start -= c.len_utf8();
    }
    match crate::parse::name_rule(lang).find(&line[start..]) {
        Some(m) if start + m.end() >= b => (start, start + m.end()),
        _ => (a, b),
    }
}

/// Start and end byte of the run of characters around `a..b` up to a space, bracket, quote or
/// comma: `get-user`, `list->vec`, `Foo::Bar.new`.
fn run_around(line: &str, a: usize, b: usize) -> (usize, usize) {
    let stop = |c: char| c.is_whitespace() || "()[]{}\"'`,".contains(c);
    let start = line[..a].rfind(stop).map_or(0, |i| i + line[i..].chars().next().map_or(1, char::len_utf8));
    let end = line[b..].find(stop).map_or(line.len(), |i| b + i);
    (start, end)
}

/// Byte offsets of `word` in `line` where it is not part of a longer identifier.
fn find_word(line: &str, word: &str) -> Vec<usize> {
    line.match_indices(word)
        .filter(|(i, _)| !line[..*i].chars().next_back().is_some_and(is_ident) && !line[i + word.len()..].chars().next().is_some_and(is_ident))
        .map(|(i, _)| i)
        .collect()
}

/// `Point` in `Point.norm`, `Point::norm` or `p->norm` when the cursor is on `norm`.
fn qualifier(before: &str) -> Option<&str> {
    let b = before.strip_suffix('.').or_else(|| before.strip_suffix("::")).or_else(|| before.strip_suffix("->"))?;
    let (a, e) = word_at(b, b.len())?;
    Some(&b[a..e])
}

pub fn path_to_uri(p: &Path) -> String {
    uri_of(&crate::instant::slashed(p.to_string_lossy()))
}

/// The `file:` URI of a path written with `/`, as `slashed` gives it: a share
/// (`//server/share/x`) has its machine as the host.
fn uri_of(path: &str) -> String {
    let head = if path.starts_with("//") {
        "file:"
    } else if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    };
    let mut out = String::from(head);
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    path_of_uri(uri, cfg!(windows)).map(PathBuf::from)
}

/// The path a `file:` URI names, written with `/`. A host other than `localhost` is a share's
/// machine on Windows (`file://wsl.localhost/Ubuntu/home` is `//wsl.localhost/Ubuntu/home`) and
/// another machine's file elsewhere, which is no path here.
fn path_of_uri(uri: &str, windows: bool) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let (host, rest) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let rest = rest.as_bytes();
    let mut raw = Vec::with_capacity(rest.len());
    let mut i = 0;
    while i < rest.len() {
        let hex = rest.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) if rest[i] == b'%' => {
                raw.push(v);
                i += 3;
            }
            _ => {
                raw.push(rest[i]);
                i += 1;
            }
        }
    }
    let s = String::from_utf8(raw).ok()?;
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return windows.then(|| format!("//{host}{s}"));
    }
    // `/C:/x` on Windows
    Some(match s.as_bytes() {
        [b'/', d, b':', ..] if windows && d.is_ascii_alphabetic() => s[1..].to_string(),
        _ => s,
    })
}

/// Whether resolving `path` stays on the machine and share of `root` (both written with `/`).
/// On Windows a path on another machine's share is not resolved: that connects to the machine
/// and offers it the user's login.
fn same_share(path: &str, root: &str, windows: bool) -> bool {
    // `server/share` of `//server/share/x` and `//?/UNC/server/share/x`, in lower case; an
    // empty one for a device path
    fn share(p: &str) -> Option<String> {
        let rest = p.strip_prefix("//")?;
        let rest = match rest.strip_prefix("?/UNC/") {
            Some(unc) => unc,
            None if rest.starts_with("?/") && rest.as_bytes().get(3) == Some(&b':') => return None,
            None if rest.starts_with(['?', '.']) => "",
            None => rest,
        };
        Some(rest.split('/').take(2).collect::<Vec<_>>().join("/").to_lowercase())
    }
    !windows || share(path).is_none_or(|s| share(root).is_some_and(|r| r == s && !s.is_empty()))
}

fn symbol_kind(kind: &str) -> u8 {
    match kind {
        "module" => 2,
        "section" | "block" => 3,
        "method" => 6,
        "enum" => 10,
        "interface" | "trait" => 11,
        "function" | "macro" => 12,
        "impl" => 19,
        "key" => 20,
        "struct" => 23,
        "signal" => 24,
        "class" | "type" => 5,
        _ => 13,
    }
}

fn range(line: u64, c0: u64, end_line: u64, c1: u64) -> Value {
    json!({"start": {"line": line, "character": c0}, "end": {"line": end_line, "character": c1}})
}

// ------------------------------------------------------------------------- the server

/// File lines for one request: the editor's text for open documents, the disk otherwise.
struct Lines<'a> {
    root: &'a Path,
    /// the root as the editor named it, for the locations an answer gives
    given: &'a Path,
    docs: &'a HashMap<String, String>,
    cache: HashMap<String, Vec<String>>,
}

impl Lines<'_> {
    fn get(&mut self, rel: &str) -> &[String] {
        if !self.cache.contains_key(rel) {
            // a key that is not a path of plain names under the root (a URI outside it, `..`) is
            // an open document's text or nothing: the disk is read under the root only
            let inside = !rel.is_empty() && Path::new(rel).components().all(|c| matches!(c, std::path::Component::Normal(_)));
            let text = match self.docs.get(rel) {
                Some(t) => t.clone(),
                // a regular file that resolves under the root, links on the way included
                None if inside => crate::resolve::repo_text(self.root, rel).unwrap_or_default(),
                None => String::new(),
            };
            // an editor shows no byte order mark, and counts columns without it
            self.cache.insert(rel.to_string(), split_lines(text.strip_prefix('\u{FEFF}').unwrap_or(&text)));
        }
        &self.cache[rel]
    }

    /// (0-based line, UTF-16 start, UTF-16 end) of `name` in the first lines of a definition.
    fn name_pos(&mut self, rel: &str, start: i64, end: i64, name: &str) -> (u64, u64, u64) {
        let first = (start.max(1) - 1) as usize;
        // a Haskell or Elm definition starts at its type signature (`step :: Int -> Int`): the
        // name to go to is the one in the equation below it
        let signed = [".hs", ".lhs", ".elm", ".purs"].iter().any(|e| rel.ends_with(e));
        let lines = self.get(rel);
        let mut at = None;
        if !name.is_empty() {
            for i in first..lines.len().min(end.max(start).min(start + 5) as usize) {
                if let Some(&b) = find_word(&lines[i], name).first() {
                    let found = (i as u64, byte_to_utf16(&lines[i], b), byte_to_utf16(&lines[i], b + name.len()));
                    if !(signed && lines[i][b + name.len()..].trim_start().starts_with(':')) {
                        return found;
                    }
                    at = at.or(Some(found));
                }
            }
        }
        at.unwrap_or((first as u64, 0, 0))
    }

    fn location(&mut self, rel: &str, start: i64, end: i64, name: &str) -> Value {
        let (l, a, b) = self.name_pos(rel, start, end, name);
        json!({"uri": path_to_uri(&self.given.join(rel)), "range": range(l, a, l, b)})
    }
}

fn split_lines(text: &str) -> Vec<String> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect()
}

/// One workspace folder: the index of the repository that holds it, opened when a request or a
/// document first needs it (the first folder at `initialize`), and the documents the editor has
/// open in it.
struct Folder {
    /// the index's root, canonical
    root: PathBuf,
    /// `root` as the editor named it (a folder reached through a link keeps that name): the files
    /// of an answer are named under it, as the editor has them open
    given: PathBuf,
    /// the editor's folders this index answers for: two folders of one repository share it
    named: Vec<PathBuf>,
    /// None until opened, and when the folder is refused or its index cannot be opened
    session: Option<Arc<Session>>,
    opened: bool,
    /// why no index is built here (the server's own directory, with no folder from the editor)
    refused: Option<String>,
    /// open documents by repository-relative path
    docs: HashMap<String, String>,
    saves: Option<mpsc::Sender<()>>,
}

struct Server {
    /// `--root`, or the directory the server started in: the folder when the editor names none
    root: PathBuf,
    /// `--root` named the root: without a folder from the editor it is used as given
    root_given: bool,
    /// the workspace folders, the editor's first folder first
    folders: Vec<Folder>,
    initialized: bool,
    shutdown: bool,
    /// how long a request waits for the first build before it is answered that the index is
    /// still being built (`session::BUILD_WAIT`)
    build_wait: std::time::Duration,
    /// when a request last waited out `build_wait`: the ones read right after it were queued behind
    /// it, and are answered at once
    waited_out: std::cell::Cell<Option<std::time::Instant>>,
    /// the folders and errors already logged by a request over several folders, logged once each
    skipped: std::cell::RefCell<HashSet<(PathBuf, i64)>>,
    /// set when the editor cancels the request being answered (`$/cancelRequest`): its waits and
    /// searches stop
    cancel: Arc<AtomicBool>,
}

/// Symbols one `workspace/symbol` answer lists at most, over every folder.
const WORKSPACE_SYMBOLS: usize = 200;

type Reply = std::result::Result<Value, (i64, String)>;

impl Folder {
    /// The folder of the index at `top`, for the editor's folder `named`.
    fn new(top: PathBuf, named: PathBuf) -> Folder {
        Folder {
            root: std::fs::canonicalize(&top).unwrap_or_else(|_| top.clone()),
            given: top,
            named: vec![named],
            session: None,
            opened: false,
            refused: None,
            docs: HashMap::new(),
            saves: None,
        }
    }

    /// The root its session holds now, which a link pointed at another tree moves.
    fn follow(&mut self) {
        if let Some(s) = &self.session {
            self.root = s.root();
        }
    }

    /// Open the index and start its first build, once.
    fn open(&mut self) {
        if std::mem::replace(&mut self.opened, true) {
            return;
        }
        let existed = crate::index::exists(&self.root);
        if let Some(why) = &self.refused {
            eprintln!("{why}");
            log(1, why);
        } else {
            // by the name the editor gave: a link pointed at another tree is followed there
            match Session::open_watched(&self.given, Every::given(None)) {
                Ok(session) => {
                    // a watch that cannot start leaves requests walking the tree
                    let _ = session.watch(false);
                    // requests wait for the index instead of seeing an empty one
                    session.building();
                    self.session = Some(Arc::clone(&session));
                    let root = self.root.clone();
                    let built = std::thread::Builder::new().name("scrutate-index".into()).spawn(move || {
                        if !existed {
                            log(3, &format!("scrutate: no index in {}; building it", crate::paths::plain_path(&root).display()));
                        }
                        match session.build() {
                            Ok(st) if !existed || st.files_changed > 0 || st.files_removed > 0 => log(
                                3,
                                &format!(
                                    "scrutate: index ready ({} files, {} changed, {} chunks, {:.1}s)",
                                    st.files_total, st.files_changed, st.chunks, st.seconds
                                ),
                            ),
                            Ok(_) => {}
                            Err(e) => log(1, &format!("scrutate: indexing failed: {e:#}")),
                        }
                    });
                    if built.is_err() {
                        self.session = None;
                    }
                }
                Err(e) => log(1, &format!("scrutate: indexing failed: {e:#}")),
            }
        }
        self.saves = self.save_worker();
    }

    /// Saves are indexed by the session's writer, so requests keep reading meanwhile (WAL).
    fn save_worker(&self) -> Option<mpsc::Sender<()>> {
        let (tx, rx) = mpsc::channel::<()>();
        let session = Arc::clone(self.session.as_ref()?);
        std::thread::Builder::new()
            .name("scrutate-save".into())
            .spawn(move || {
                let mut said_newer = false;
                while rx.recv().is_ok() {
                    while rx.try_recv().is_ok() {} // one update covers every save queued meanwhile
                    match session.write(|repo| repo.update(false)).err() {
                        // an index a newer scrutate built is not updated: said once, not per save
                        Some(e) if e.downcast_ref::<crate::index::Newer>().is_some() => {
                            if !std::mem::replace(&mut said_newer, true) {
                                eprintln!("scrutate: {e:#}");
                            }
                        }
                        Some(e) => eprintln!("scrutate: index update failed: {e:#}"),
                        None => {}
                    }
                }
            })
            .ok()?;
        Some(tx)
    }

    fn key(&self, uri: &str) -> String {
        self.rel(uri).unwrap_or_else(|| uri.to_string())
    }

    fn rel(&self, uri: &str) -> Option<String> {
        let p = uri_to_path(uri)?;
        let slashed = |p: &Path| p.to_string_lossy().replace('\\', "/");
        if !same_share(&slashed(&p), &slashed(&self.root), cfg!(windows)) {
            return None;
        }
        crate::ops::repo_rel(&self.root, &p)
    }

    fn lines(&self) -> Lines<'_> {
        self.lines_at(&self.given)
    }

    fn lines_at<'a>(&'a self, given: &'a Path) -> Lines<'a> {
        Lines { root: &self.root, given, docs: &self.docs, cache: HashMap::new() }
    }

    /// The folder as the editor spells it in the request's document: one repository opened under
    /// two spellings (a folder through a link, a file named another way) is answered in the
    /// document's. The first folder's spelling when the document's path does not end in its
    /// path in the repository.
    fn given_for(&self, p: &Value) -> PathBuf {
        let spelled = p["textDocument"]["uri"].as_str().and_then(|u| {
            let (path, rel) = (uri_to_path(u)?, self.rel(u)?);
            let top = path.ancestors().nth(Path::new(&rel).components().count())?;
            (top.join(&rel) == path).then(|| top.to_path_buf())
        });
        spelled.unwrap_or_else(|| self.given.clone())
    }

    /// The line under the cursor and the byte range of the name there: the identifier, widened
    /// to the name the file's language writes (`valid?`, `step'`, `is.ready`) when the index has it.
    fn word_in_line(&self, s: &Searcher, p: &Value) -> Result<Option<(String, usize, usize)>> {
        let Some(rel) = p["textDocument"]["uri"].as_str().map(|u| self.key(u)) else { return Ok(None) };
        let (Some(line), Some(ch)) = (p["position"]["line"].as_u64(), p["position"]["character"].as_u64()) else { return Ok(None) };
        let Some(text) = self.lines().get(&rel).get(line as usize).cloned() else { return Ok(None) };
        let Some((a, b)) = word_at(&text, utf16_to_byte(&text, ch)) else { return Ok(None) };
        let (wa, wb) = widen(&text, a, b, &rel);
        // `x-y` is a subtraction in most languages: only a name the index knows is taken
        if (wa, wb) != (a, b) && !s.find_symbol(&text[wa..wb], 1)?.is_empty() {
            return Ok(Some((text, wa, wb)));
        }
        Ok(Some((text, a, b)))
    }

    /// The name under the cursor and the text just before it.
    fn ident_at(&self, s: &Searcher, p: &Value) -> Result<Option<(String, String)>> {
        Ok(self.word_in_line(s, p)?.map(|(text, a, b)| (text[a..b].to_string(), text[..a].to_string())))
    }

    /// The definition the name under the cursor refers to, as references and definition both take it.
    fn resolved_at(&self, names: &mut Names, p: &Value, ident: &str, before: &str) -> Option<i64> {
        let rel = p["textDocument"]["uri"].as_str().and_then(|u| self.rel(u))?;
        let line = p["position"]["line"].as_u64().unwrap_or(0);
        // the resolver's column counts the byte order mark on the first line; the editor's does not
        let bom = if line == 0 && names.starts_with_bom(&rel) { '\u{FEFF}'.len_utf8() } else { 0 };
        names.at(&rel, line + 1, before.len() + bom, ident).ok().map(|(id, _)| id)
    }

    fn definition(&self, s: &mut Searcher, p: &Value) -> Result<Value> {
        let Some((text, a, b)) = self.word_in_line(s, p)? else { return Ok(Value::Null) };
        let given = self.given_for(p);
        let (ident, before) = (&text[a..b], &text[..a]);
        if let Some(d) = self.resolved_at(&mut Names::new(s)?, p, ident, before).map(|id| s.ix.chunk(id)).transpose()?.flatten() {
            return Ok(json!([self.lines_at(&given).location(&d.path, d.start, d.end, &d.name)]));
        }
        let here = p["textDocument"]["uri"].as_str().and_then(|u| self.rel(u));
        // the run up to the word (`my-trim`, `Foo::Bar` in `Foo::Bar.new`), then the whole run as
        // one name (`get-user` from `get`) unless the word is the owner of what follows
        let (start, end) = run_around(&text, a, b);
        let mut hits = if &text[start..b] != ident { definitions(s, &text[start..b])? } else { Vec::new() };
        let owner = [".", "::", "#"].iter().any(|sep| text[b..].starts_with(sep));
        if hits.is_empty() && end > b && !owner {
            hits = s.symbol_rows(&text[start..end], 50)?;
        }
        if hits.is_empty() {
            let qualified = match qualifier(before) {
                Some(q) => definitions(s, &format!("{q}.{ident}"))?,
                None => Vec::new(),
            };
            hits = if qualified.is_empty() { definitions(s, ident)? } else { qualified };
        }
        hits.sort_by_key(|h| Some(&h.path) != here.as_ref()); // stable: index order within each group
        let mut lines = self.lines_at(&given);
        Ok(Value::Array(hits.iter().map(|h| lines.location(&h.path, h.start, h.end, &h.name)).collect()))
    }

    fn references(&self, s: &mut Searcher, p: &Value) -> Result<Value> {
        let Some((ident, before)) = self.ident_at(s, p)? else { return Ok(Value::Null) };
        let given = self.given_for(p);
        let include_decl = p["context"]["includeDeclaration"].as_bool().unwrap_or(true);
        // the definition under the cursor; an unresolved cursor falls back to every textual occurrence
        let at = self.resolved_at(&mut Names::new(s)?, p, &ident, &before);
        let refs = |s: &mut Searcher, textual: bool| ops::run(s, &Op::Refs { name: ident.clone(), max_results: REFERENCES, textual, def: None });
        if let Some(def_id) = at {
            let Answer::Resolved { refs: r, .. } = refs(s, false)? else { unreachable!("resolved references") };
            let mut lines = self.lines_at(&given);
            let mut out = Vec::new();
            let mut decl = None;
            if let (true, Some(d)) = (include_decl, r.definitions.iter().find(|d| d.id == def_id)) {
                let (l, a, b) = lines.name_pos(&d.path, d.start, d.end, &d.name);
                decl = Some((d.path.clone(), l, a));
                out.push(json!({"uri": path_to_uri(&given.join(&d.path)), "range": range(l, a, l, b)}));
            }
            for o in r.of(def_id) {
                let (l, a) = (o.line.saturating_sub(1), byte_to_utf16(&o.text, o.at));
                // the declaration is listed once; a definition's other line (Haskell's signature) is its own
                if decl.as_ref().is_some_and(|(p, dl, da)| *p == o.path && (*dl, *da) == (l, a))
                    || !include_decl && matches!(o.resolution, Ok((_, "definition")))
                {
                    continue;
                }
                // an alias (`h` for `helper`) spans what the line writes, not the name asked for
                let written = o.text.get(o.at..).unwrap_or("");
                let word = |c: &char| c.is_alphanumeric() || *c == '_' || *c == '$';
                // the name as asked for, whole: `helper2` starts with `helper` but is another word
                let whole = written.strip_prefix(ident.as_str()).is_some_and(|rest| !rest.chars().next().is_some_and(|c| word(&c)));
                let width = if whole { ident.encode_utf16().count() } else { written.chars().take_while(word).map(char::len_utf16).sum() } as u64;
                out.push(json!({"uri": path_to_uri(&given.join(&o.path)), "range": range(l, a, l, a + width)}));
            }
            return Ok(Value::Array(out));
        }
        let Answer::Refs { refs: r, .. } = refs(s, true)? else { unreachable!("textual references") };
        let mut lines = self.lines_at(&given);
        let mut seen: HashSet<(String, u64, u64)> = HashSet::new();
        let mut out = Vec::new();
        for d in &r.definitions {
            let (l, a, b) = lines.name_pos(&d.path, d.start, d.end, &d.name);
            seen.insert((d.path.clone(), l, a));
            if include_decl {
                out.push(json!({"uri": path_to_uri(&given.join(&d.path)), "range": range(l, a, l, b)}));
            }
        }
        let width = ident.encode_utf16().count() as u64;
        // `valid` in `valid?` is another name; `x` in `x-y` is not, as the index has no `x-y`
        let mut known: HashMap<String, bool> = HashMap::new();
        for g in &r.groups {
            for h in &g.lines {
                let text = h.text.trim_end_matches(['\n', '\r']);
                for off in find_word(text, &ident) {
                    let (wa, wb) = widen(text, off, off + ident.len(), &g.path);
                    if (wa, wb) != (off, off + ident.len()) {
                        let wider = &text[wa..wb];
                        if !known.contains_key(wider) {
                            known.insert(wider.to_string(), !s.find_symbol(wider, 1)?.is_empty());
                        }
                        if known[wider] {
                            continue;
                        }
                    }
                    let (l, a) = (h.line.saturating_sub(1), byte_to_utf16(text, off));
                    if seen.insert((g.path.clone(), l, a)) {
                        out.push(json!({"uri": path_to_uri(&given.join(&g.path)), "range": range(l, a, l, a + width)}));
                    }
                }
            }
        }
        Ok(Value::Array(out))
    }

    fn document_symbols(&self, s: &mut Searcher, p: &Value) -> Result<Value> {
        let Some(rel) = p["textDocument"]["uri"].as_str().and_then(|u| self.rel(u)) else { return Ok(Value::Null) };
        let Answer::Outline { rows, .. } = ops::run(s, &Op::Outline { path: rel.clone() })? else { unreachable!("an outline") };
        let mut lines = self.lines();
        // rows come by start line, enclosing symbols first: a stack of open symbols nests them
        let mut roots: Vec<Value> = Vec::new();
        let mut stack: Vec<(&ChunkRow, Value, Vec<Value>)> = Vec::new();
        let pop = |stack: &mut Vec<(&ChunkRow, Value, Vec<Value>)>, roots: &mut Vec<Value>| {
            let (_, mut v, children) = stack.pop().unwrap();
            if !children.is_empty() {
                v["children"] = Value::Array(children);
            }
            match stack.last_mut() {
                Some(parent) => parent.2.push(v),
                None => roots.push(v),
            }
        };
        // lines an earlier definition starts or ends on
        let mut taken: std::collections::HashSet<i64> = std::collections::HashSet::new();
        for r in &rows {
            // one that starts on the first or last line of the open one, and is not named inside
            // it, is its sibling (`function b() {} function c() {}`, `} function c() {`)
            let sibling = |top: &ChunkRow| (top.start == r.start || top.end == r.start) && !named_inside(&r.qualname, &top.qualname);
            while stack.last().is_some_and(|top| top.0.end < r.end || sibling(top.0)) {
                pop(&mut stack, &mut roots);
            }
            let mut v = document_symbol(&mut lines, &rel, r);
            // the index keeps lines, not columns: a definition on a line another one starts or
            // ends on starts at its name
            if taken.contains(&r.start) {
                v["range"]["start"] = v["selectionRange"]["start"].clone();
            }
            taken.extend([r.start, r.end]);
            stack.push((r, v, Vec::new()));
        }
        while !stack.is_empty() {
            pop(&mut stack, &mut roots);
        }
        Ok(Value::Array(roots))
    }

    fn workspace_symbols(&self, s: &mut Searcher, p: &Value) -> Result<Value> {
        let rows = crate::symmatch::ranked(&s.ix, p["query"].as_str().unwrap_or(""), 200)?;
        let mut lines = self.lines();
        Ok(Value::Array(
            rows.iter()
                .map(|(r, _)| {
                    let container = r.qualname.rsplit_once('.').map(|(c, _)| c).unwrap_or("");
                    json!({"name": r.name, "kind": symbol_kind(&r.kind), "containerName": container,
                           "location": lines.location(&r.path, r.start, r.end, &r.name)})
                })
                .collect(),
        ))
    }
}

impl Server {
    /// Add the editor's folder `folder`: the index at the repository top above it, as the CLI
    /// finds it (`ruled`), or the folder itself. A folder of a repository already here shares its
    /// index. -> its place in `folders`.
    fn add_folder(&mut self, folder: PathBuf, ruled: bool, may_refuse: bool) -> usize {
        let folder = std::path::absolute(&folder).unwrap_or(folder);
        let top = ruled.then(|| crate::cli::index_above(&folder)).flatten();
        if let Some(top) = &top {
            eprintln!("[scrutate] using the index in {}", crate::paths::plain_path(top).display());
        }
        // a folder from the editor is the user's choice; the server's own directory is not
        let refused = (may_refuse && top.is_none()).then(|| crate::cli::no_first_build(&folder)).flatten();
        let top = top.unwrap_or_else(|| folder.clone());
        let root = std::fs::canonicalize(&top).unwrap_or_else(|_| top.clone());
        if let Some(i) = self.folders.iter().position(|f| f.root == root) {
            self.folders[i].named.push(folder);
            return i;
        }
        let mut f = Folder::new(top, folder);
        f.refused = refused;
        self.folders.push(f);
        self.folders.len() - 1
    }

    fn initialize(&mut self, p: &Value) -> Value {
        // every folder the editor sends is answered from the index of its own repository
        let mut named: Vec<PathBuf> = p["workspaceFolders"].as_array().into_iter().flatten().filter_map(|f| f["uri"].as_str().and_then(uri_to_path)).collect();
        if named.is_empty() {
            named.extend(p["rootUri"].as_str().and_then(uri_to_path).or_else(|| p["rootPath"].as_str().map(PathBuf::from)));
        }
        let from_editor = !named.is_empty();
        let ruled = from_editor || !self.root_given;
        if from_editor {
            for f in named {
                self.add_folder(f, ruled, false);
            }
        } else {
            self.add_folder(self.root.clone(), ruled, !self.root_given);
        }
        // the first folder is opened now, so its index is built before the first request; the
        // others when first needed
        if let Some(f) = self.folders.first_mut() {
            f.open();
        }
        json!({
            "capabilities": {
                "positionEncoding": "utf-16",
                "textDocumentSync": {"openClose": true, "change": 1, "save": {"includeText": false}},
                "definitionProvider": true,
                "referencesProvider": true,
                "workspaceSymbolProvider": true,
                "documentSymbolProvider": true,
                "workspace": {"workspaceFolders": {"supported": true, "changeNotifications": true}},
            },
            "serverInfo": {"name": "scrutate", "version": crate::VERSION},
        })
    }

    /// The folder a document is in: the deepest one that holds it. None for a document in no
    /// folder, which is not answered.
    fn folder_of(&self, uri: &str) -> Option<usize> {
        self.folders.iter().enumerate().filter(|(_, f)| f.rel(uri).is_some()).max_by_key(|(_, f)| f.root.as_os_str().len()).map(|(i, _)| i)
    }

    /// `workspace/didChangeWorkspaceFolders`: a removed folder's index is closed when no other
    /// folder of the editor's uses it, and an added one is answered from its own.
    fn change_folders(&mut self, event: &Value) {
        let paths = |key: &str| -> Vec<PathBuf> {
            event[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| f["uri"].as_str().and_then(uri_to_path))
                .map(|p| std::path::absolute(&p).unwrap_or(p))
                .collect()
        };
        for gone in paths("removed") {
            for f in &mut self.folders {
                f.named.retain(|n| *n != gone);
            }
        }
        self.folders.retain(|f| !f.named.is_empty());
        for added in paths("added") {
            self.add_folder(added, true, false);
        }
    }

    fn notify(&mut self, method: &str, p: &Value) {
        self.folders.iter_mut().for_each(Folder::follow);
        if method == "workspace/didChangeWorkspaceFolders" {
            self.change_folders(&p["event"]);
            return;
        }
        let uri = p["textDocument"]["uri"].as_str().unwrap_or("");
        // a document in no folder is not answered, so its text is not kept
        let Some(i) = self.folder_of(uri) else { return };
        let f = &mut self.folders[i];
        let key = f.key(uri);
        match method {
            "textDocument/didOpen" => {
                let text = p["textDocument"]["text"].as_str().unwrap_or("").to_string();
                f.docs.insert(key, text);
            }
            "textDocument/didChange" => {
                // full sync: the last change holds the whole text
                if let Some(text) = p["contentChanges"].as_array().and_then(|c| c.last()).and_then(|c| c["text"].as_str()) {
                    f.docs.insert(key, text.to_string());
                }
            }
            "textDocument/didClose" => {
                f.docs.remove(&key);
            }
            "textDocument/didSave" => {
                if let Some(tx) = &f.saves {
                    let _ = tx.send(());
                }
            }
            _ => {}
        }
    }

    fn request(&mut self, method: &str, p: &Value) -> Reply {
        self.folders.iter_mut().for_each(Folder::follow);
        if method == "initialize" {
            if self.initialized {
                return Err((-32600, "already initialized".into()));
            }
            self.initialized = true;
            return Ok(self.initialize(p));
        }
        if !self.initialized {
            return Err((-32002, "server not initialized".into()));
        }
        if self.shutdown {
            return Err((-32600, "server is shutting down".into()));
        }
        if method == "shutdown" {
            self.shutdown = true;
            return Ok(Value::Null);
        }
        let asked: Vec<usize> = match method {
            "workspace/symbol" => (0..self.folders.len()).collect(),
            "textDocument/definition" | "textDocument/references" | "textDocument/documentSymbol" => {
                match p["textDocument"]["uri"].as_str().and_then(|u| self.folder_of(u)) {
                    Some(i) => vec![i],
                    None => return Ok(Value::Null),
                }
            }
            _ => return Err((-32601, format!("method not found: {method}"))),
        };
        for &i in &asked {
            self.folders[i].open();
        }
        // over several folders, one that cannot answer (no index, its first build still running)
        // leaves the others' answers, and the request fails only when none answered. A first build
        // is waited for only when no folder could answer without waiting.
        let several = asked.len() > 1;
        let (mut parts, mut answered, mut failed, mut building) = (Vec::new(), false, None, Vec::new());
        for pass in 0..2 {
            let these = match pass {
                0 => asked.clone(),
                _ if several && !answered => std::mem::take(&mut building),
                _ => break,
            };
            for i in these {
                let f = &self.folders[i];
                let answer = self.run(method, f, several && pass == 0, &mut |s| match method {
                    "textDocument/definition" => f.definition(s, p),
                    "textDocument/references" => f.references(s, p),
                    "textDocument/documentSymbol" => f.document_symbols(s, p),
                    _ => f.workspace_symbols(s, p),
                });
                match answer {
                    Ok(a) if method != "workspace/symbol" => return Ok(a),
                    Ok(a) => {
                        answered = true;
                        parts.push((i, a));
                    }
                    Err(e) if several && pass == 0 && e.0 == -32801 => building.push(i),
                    Err(e) if several => {
                        self.left_out(f, &e);
                        // "still being built" asks the editor to ask again: it wins over the others
                        if failed.as_ref().is_none_or(|(code, _)| *code != -32801 && e.0 == -32801) {
                            failed = Some(e);
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        // the folders set aside in their first build when another folder answered are left out too
        for &i in &building {
            self.left_out(&self.folders[i], &(-32801, "the index is still being built".to_string()));
        }
        if let (false, Some(e)) = (answered, failed) {
            return Err(e);
        }
        // a file is listed by the deepest folder that answered and holds it, as definition goes to
        // the deepest folder: an outer folder's index holds a nested folder's files too
        let mut out: Vec<Value> = Vec::new();
        for (i, a) in &parts {
            let theirs = |e: &&Value| {
                let Some(uri) = e["location"]["uri"].as_str() else { return true };
                let holders = parts.iter().map(|(j, _)| *j).filter(|&j| self.folders[j].rel(uri).is_some());
                holders.max_by_key(|&j| self.folders[j].root.as_os_str().len()).is_none_or(|o| o == *i)
            };
            out.extend(a.as_array().into_iter().flatten().filter(theirs).cloned());
        }
        out.truncate(WORKSPACE_SYMBOLS);
        Ok(Value::Array(out))
    }

    /// Log, once per folder and error, that `folder` is left out of an answer over several folders.
    fn left_out(&self, folder: &Folder, e: &(i64, String)) {
        if self.skipped.borrow_mut().insert((folder.root.clone(), e.0)) {
            log(2, &format!("scrutate: {} is left out of this answer: {}", folder.given.display(), e.1));
        }
    }

    /// `f` on a reader of `folder`'s index for a `method` request, once its first build is done;
    /// `no_wait` does not wait for that build.
    fn run(&self, method: &str, folder: &Folder, no_wait: bool, f: &mut dyn FnMut(&mut Searcher) -> Result<Value>) -> Reply {
        // read within a second of a request that waited out the build: queued behind it
        let queued = self.waited_out.get().is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1));
        let build_wait = if queued || no_wait { std::time::Duration::ZERO } else { self.build_wait };
        let session = folder.session.as_ref().ok_or((-32603, "the index is not available (see the log)".to_string()))?;
        let cancel = Arc::clone(&self.cancel);
        // the one thread that answers requests must not wait out a long first build: a change and
        // shutdown come in behind it; ContentModified asks the editor to ask again
        if !session.wait_built(&cancel, Some(build_wait), |_| {}) {
            // a request that did not wait has waited out nothing: the next one may still wait
            if !no_wait {
                self.waited_out.set(Some(std::time::Instant::now()));
            }
            return Err((-32801, "the index is still being built".to_string()));
        }
        // a damaged index is built again and the request answered from the new one
        let (v, pending) = session
            .read_pending(&cancel, |s| {
                // a newer scrutate took the index over since it was opened: nothing here can read it
                if let Some(newer) = s.ix.newer().filter(|n| !n.readable) {
                    return Err(newer.into());
                }
                crate::ops::in_snapshot(s, &mut *f)
            })
            .map_err(|e| (-32603, format!("{e:#}")))?;
        if let Some(note) = pending.note() {
            log(2, &format!("scrutate: {method}: {note}"));
        }
        Ok(v)
    }
}

/// Occurrences one references request reads at most.
const REFERENCES: u64 = 5000;

/// The definitions of `name` as the `symbol` operation lists them, up to 50; none for words that
/// only look like a location (`a:1`).
fn definitions(s: &mut Searcher, name: &str) -> Result<Vec<crate::search::Hit>> {
    if crate::intel::location(name).is_some() {
        return Ok(Vec::new());
    }
    let Answer::Hits { hits, .. } = ops::run(s, &Op::Symbol { name: name.to_string(), limit: 50, exact: true })? else { unreachable!("symbol answers hits") };
    Ok(hits)
}

/// Whether the qualified name `inner` is a name inside `outer` (`b.c` inside `b`).
fn named_inside(inner: &str, outer: &str) -> bool {
    inner.strip_prefix(outer).is_some_and(|rest| rest.starts_with('.'))
}

fn document_symbol(lines: &mut Lines, rel: &str, r: &ChunkRow) -> Value {
    let (l, a, b) = lines.name_pos(rel, r.start, r.end, &r.name);
    let last = (r.end.max(r.start).max(1) - 1) as usize;
    let end_col = lines.get(rel).get(last).map(|t| t.encode_utf16().count() as u64).unwrap_or(0);
    let name = [&r.name, &r.qualname, &r.kind].into_iter().find(|n| !n.is_empty()).cloned().unwrap_or_else(|| "?".into());
    json!({
        "name": name,
        "detail": r.kind,
        "kind": symbol_kind(&r.kind),
        "range": range((r.start.max(1) - 1) as u64, 0, last as u64, end_col),
        "selectionRange": range(l, a, l, b),
    })
}

/// Serve until `exit` or end of input. -> the process exit code (0 only after `shutdown`).
pub fn serve(root: &Path, root_given: bool) -> Result<i32> {
    if cfg!(target_os = "wasi") {
        bail!("lsp is not in the wasi build: it needs threads");
    }
    let mut srv = Server {
        root: root.to_path_buf(),
        root_given,
        folders: Vec::new(),
        initialized: false,
        shutdown: false,
        build_wait: crate::session::BUILD_WAIT,
        waited_out: Default::default(),
        skipped: Default::default(),
        cancel: Default::default(),
    };
    // the editor's messages are read on a thread of their own, so a `$/cancelRequest` reaches the
    // request it cancels while that request runs; one cancelled before it ran is answered at once
    type Running = Arc<std::sync::Mutex<(Option<Value>, Arc<AtomicBool>, HashSet<String>)>>;
    let running: Running = Default::default();
    let (tx, rx) = mpsc::channel::<std::result::Result<Vec<u8>, String>>();
    {
        let running = Arc::clone(&running);
        std::thread::Builder::new().name("scrutate-lsp-read".into()).spawn(move || {
            let mut input = std::io::stdin().lock();
            loop {
                let body = match read_message(&mut input) {
                    Ok(Some(body)) => body,
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(Err(format!("{e:#}")));
                        break;
                    }
                };
                if let Some(m) = serde_json::from_slice::<Value>(&body).ok().filter(|m| m["method"] == "$/cancelRequest") {
                    let id = &m["params"]["id"];
                    let mut r = running.lock().unwrap_or_else(|p| p.into_inner());
                    if r.0.as_ref() == Some(id) {
                        r.1.store(true, std::sync::atomic::Ordering::Relaxed);
                    } else {
                        r.2.insert(id.to_string());
                    }
                    continue;
                }
                if tx.send(Ok(body)).is_err() {
                    break;
                }
            }
        })?;
    }
    while let Ok(body) = rx.recv() {
        let body = body.map_err(|e| anyhow::anyhow!(e))?;
        let msg: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => {
                send(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}));
                continue;
            }
        };
        let method = msg["method"].as_str().unwrap_or("");
        let params = &msg["params"];
        let Some(id) = msg.get("id") else {
            if method == "exit" {
                break;
            }
            srv.notify(method, params);
            continue;
        };
        if method.is_empty() {
            continue; // a response: this server sends no requests
        }
        let cancelled = || (-32800, "request cancelled".to_string());
        let flag = Arc::new(AtomicBool::new(false));
        {
            let mut r = running.lock().unwrap_or_else(|p| p.into_inner());
            if r.2.remove(&id.to_string()) {
                drop(r);
                send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": cancelled().0, "message": cancelled().1}}));
                continue;
            }
            *r = (Some(id.clone()), Arc::clone(&flag), std::mem::take(&mut r.2));
        }
        srv.cancel = Arc::clone(&flag);
        // a bug in one request must not take the editor's server down
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| srv.request(method, params)))
            .unwrap_or_else(|_| Err((-32603, format!("internal failure while answering {method} (a bug: please report it)"))));
        running.lock().unwrap_or_else(|p| p.into_inner()).0 = None;
        let reply = if flag.load(std::sync::atomic::Ordering::Relaxed) { Err(cancelled()) } else { reply };
        send(&match reply {
            Ok(mut result) => {
                // what an editor's agent reads has its secrets redacted unless SCRUTATE_REDACT=off
                if crate::secrets::on(true) {
                    crate::secrets::know_key_lines_at(&srv.root);
                    crate::secrets::Redactor::default().redact_json(&mut result);
                }
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        });
    }
    Ok(if srv.shutdown { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_during_the_first_build_is_answered_once_the_wait_runs_out() {
        let root = std::env::temp_dir().join(format!("scrutate_lsp_building_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let session = Arc::new(Session::open(&root, crate::session::Every::given(None)).unwrap());
        // a first build is running and does not end
        session.building();
        let mut srv = Server {
            root: root.clone(),
            root_given: true,
            folders: vec![Folder { session: Some(session), opened: true, ..Folder::new(root.clone(), root.clone()) }],
            initialized: true,
            shutdown: false,
            build_wait: std::time::Duration::from_millis(300),
            waited_out: Default::default(),
            skipped: Default::default(),
            cancel: Default::default(),
        };
        // a request that waits the build out never returns; 100 times the wait leaves a loaded
        // machine room to answer one that stops at the wait
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let r = srv.request("workspace/symbol", &serde_json::json!({"query": "alpha"}));
            let _ = tx.send((r, srv));
        });
        let (r, mut srv) = rx.recv_timeout(std::time::Duration::from_millis(300) * 100).expect("the request waited for the build to end");
        assert_eq!(r.map_err(|e| e.0), Err(-32801));
        // shutdown is answered at once whatever the build does
        assert!(srv.request("shutdown", &Value::Null).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Over several folders, a folder in its first build that is left out because another folder
    /// answered is logged once too, and when no folder answers, "still being built" (ask again)
    /// wins over a folder that has no index.
    #[test]
    fn a_folder_left_out_in_its_first_build_is_logged_and_asks_again_when_none_answered() {
        let base = std::env::temp_dir().join(format!("scrutate_lsp_left_out_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let folder = |name: &str, session: Option<Arc<Session>>| Folder { session, opened: true, ..Folder::new(base.join(name), base.join(name)) };
        for name in ["x", "y"] {
            std::fs::create_dir_all(base.join(name)).unwrap();
            std::fs::write(base.join(name).join("a.py"), "def alpha():\n    return 1\n").unwrap();
        }
        let x = Arc::new(Session::open(&base.join("x"), crate::session::Every::given(None)).unwrap());
        x.build().unwrap();
        let y = Arc::new(Session::open(&base.join("y"), crate::session::Every::given(None)).unwrap());
        // a first build is running and does not end
        y.building();
        let server = |folders: Vec<Folder>| Server {
            root: base.clone(),
            root_given: true,
            folders,
            initialized: true,
            shutdown: false,
            build_wait: std::time::Duration::from_millis(300),
            waited_out: Default::default(),
            skipped: Default::default(),
            cancel: Default::default(),
        };
        let mut srv = server(vec![folder("x", Some(Arc::clone(&x))), folder("y", Some(Arc::clone(&y)))]);
        let r = srv.request("workspace/symbol", &serde_json::json!({"query": "alpha"})).unwrap();
        assert_eq!(r.as_array().map(Vec::len), Some(1), "{r}");
        assert!(srv.skipped.borrow().contains(&(srv.folders[1].root.clone(), -32801)), "the building folder's skip is logged");
        let mut srv = server(vec![folder("y", Some(y)), folder("gone", None)]);
        let r = srv.request("workspace/symbol", &serde_json::json!({"query": "alpha"}));
        assert_eq!(r.map_err(|e| e.0), Err(-32801), "the editor is asked to ask again");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A repository opened as its own folder inside another folder is in both folders' indexes;
    /// workspace/symbol lists each of its symbols once, from the deepest folder, as definition
    /// answers from it; while that folder's first build runs, from the deepest folder that answered.
    #[test]
    fn workspace_symbol_lists_a_nested_folder_once() {
        let base = std::env::temp_dir().join(format!("scrutate_lsp_nested_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // `building`: how many of the deepest folders are in their first build. The deepest
        // folder's symbol is the exact match `shared`, so an outer folder ranks it first: an outer
        // folder that kept it would list it ahead of its own
        let run = |levels: &[&str], building: usize| {
            let mut dir = base.clone();
            let mut folders = Vec::new();
            for (n, name) in levels.iter().enumerate() {
                dir = dir.join(name);
                std::fs::create_dir_all(&dir).unwrap();
                let def = if n == levels.len() - 1 { "shared".to_string() } else { format!("shared_{name}") };
                std::fs::write(dir.join(format!("{name}.py")), format!("def {def}():\n    return 1\n")).unwrap();
            }
            let mut dir = base.clone();
            for (n, name) in levels.iter().enumerate() {
                dir = dir.join(name);
                let s = Arc::new(Session::open(&dir, crate::session::Every::given(None)).unwrap());
                if n >= levels.len() - building {
                    s.building();
                } else {
                    s.build().unwrap();
                }
                folders.push(Folder { session: Some(s), opened: true, ..Folder::new(dir.clone(), dir.clone()) });
            }
            let mut srv = Server {
                root: base.clone(),
                root_given: true,
                folders,
                initialized: true,
                shutdown: false,
                build_wait: std::time::Duration::from_millis(300),
                waited_out: Default::default(),
                skipped: Default::default(),
                cancel: Default::default(),
            };
            let r = srv.request("workspace/symbol", &serde_json::json!({"query": "shared"})).unwrap();
            // in answer order: each folder's after the one before
            r.as_array().unwrap().iter().map(|e| e["location"]["uri"].as_str().unwrap().rsplit('/').next().unwrap().to_string()).collect::<Vec<_>>()
        };
        let sorted = |mut v: Vec<String>| {
            v.sort();
            v
        };
        // each folder lists its own file, the outer one first
        assert_eq!(run(&["outer", "inner"], 0), ["outer.py", "inner.py"]);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(run(&["a", "b", "c"], 0), ["a.py", "b.py", "c.py"]);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(sorted(run(&["outer", "inner"], 1)), ["inner.py", "outer.py"]);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(sorted(run(&["a", "b", "c"], 1)), ["a.py", "b.py", "c.py"]);
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(sorted(run(&["a", "b", "c", "d"], 2)), ["a.py", "b.py", "c.py", "d.py"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// One repository opened under two spellings, as two folders (one through a link) or as a
    /// folder and a file named through a link, answers definition and references in the spelling
    /// of the request's document; workspace/symbol, about no document, in the first folder's.
    #[cfg(unix)]
    #[test]
    fn a_repository_under_two_spellings_answers_in_the_documents() {
        let base = std::env::temp_dir().join(format!("scrutate_lsp_spellings_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (real, code) = (base.join("real/proj"), base.join("code/proj"));
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("code")).unwrap();
        std::fs::write(real.join("lib.py"), "def helper():\n    return 1\n").unwrap();
        std::fs::write(real.join("app.py"), "from lib import helper\nhelper()\n").unwrap();
        let session = Arc::new(Session::open(&real, crate::session::Every::given(None)).unwrap());
        session.build().unwrap();
        let server = |spellings: &[&Path]| {
            let mut srv = Server {
                root: base.clone(),
                root_given: true,
                folders: Vec::new(),
                initialized: true,
                shutdown: false,
                build_wait: std::time::Duration::from_millis(300),
                waited_out: Default::default(),
                skipped: Default::default(),
                cancel: Default::default(),
            };
            for f in spellings {
                srv.add_folder(f.to_path_buf(), false, false);
            }
            assert_eq!(srv.folders.len(), 1, "one repository, one index");
            srv.folders[0].session = Some(Arc::clone(&session));
            srv.folders[0].opened = true;
            srv
        };
        let at = |dir: &Path| serde_json::json!({"textDocument": {"uri": path_to_uri(&dir.join("app.py"))}, "position": {"line": 1, "character": 0}, "context": {"includeDeclaration": true}});
        let uris = |v: Value| -> Vec<String> {
            v.as_array().unwrap().iter().map(|e| e["uri"].as_str().or(e["location"]["uri"].as_str()).unwrap().to_string()).collect()
        };
        let under = |dir: &Path, uris: &[String]| !uris.is_empty() && uris.iter().all(|u| u.starts_with(&format!("{}/", path_to_uri(dir))));
        for spellings in [&[real.as_path(), code.as_path()][..], &[real.as_path()][..]] {
            let mut srv = server(spellings);
            for dir in [&code, &real] {
                let d = uris(srv.request("textDocument/definition", &at(dir)).unwrap());
                assert!(under(dir, &d), "{spellings:?} definition from {dir:?}: {d:?}");
                let r = uris(srv.request("textDocument/references", &at(dir)).unwrap());
                assert!(under(dir, &r), "{spellings:?} references from {dir:?}: {r:?}");
            }
            let w = uris(srv.request("workspace/symbol", &serde_json::json!({"query": "helper"})).unwrap());
            assert!(under(&real, &w), "{w:?}");
        }
        drop(session);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Requests queued behind one that waited out the first build are answered at once, so a
    /// shutdown behind them is not kept waiting a build wait per request; one that comes later
    /// waits again.
    #[test]
    fn requests_queued_behind_a_build_wait_do_not_wait_again() {
        let root = std::env::temp_dir().join(format!("scrutate_lsp_queued_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let session = Arc::new(Session::open(&root, crate::session::Every::given(None)).unwrap());
        session.building();
        let mut srv = Server {
            root: root.clone(),
            root_given: true,
            folders: vec![Folder { session: Some(session), opened: true, ..Folder::new(root.clone(), root.clone()) }],
            initialized: true,
            shutdown: false,
            build_wait: std::time::Duration::from_secs(2),
            waited_out: Default::default(),
            skipped: Default::default(),
            cancel: Default::default(),
        };
        assert_eq!(srv.request("workspace/symbol", &serde_json::json!({"query": "alpha"})).map_err(|e| e.0), Err(-32801));
        // the three queued behind it wait nothing; one that waited again would take a whole
        // build_wait on its own
        let t0 = std::time::Instant::now();
        for _ in 0..3 {
            assert_eq!(srv.request("workspace/symbol", &serde_json::json!({"query": "alpha"})).map_err(|e| e.0), Err(-32801));
        }
        assert!(srv.request("shutdown", &Value::Null).is_ok());
        let took = t0.elapsed();
        assert!(took < srv.build_wait, "{took:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn lines_are_read_from_the_disk_only_under_the_root() {
        let base = std::env::temp_dir().join(format!("scrutate_lsp_lines_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(root.join("src/a.py"), "inside = 1\n").unwrap();
        std::fs::write(base.join("outside/secret.py"), "TOKEN = 'outside'\n").unwrap();
        std::os::unix::fs::symlink(base.join("outside"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("src"), root.join("inlink")).unwrap();
        let root = root.canonicalize().unwrap();
        let docs = HashMap::from([("../open.py".to_string(), "from the editor\n".to_string())]);
        let mut lines = Lines { root: &root, given: &root, docs: &docs, cache: HashMap::new() };
        assert_eq!(lines.get("src/a.py"), ["inside = 1", ""]);
        assert_eq!(lines.get("inlink/a.py"), ["inside = 1", ""]);
        // a link that leaves the root, a path that climbs out of it and an absolute one read nothing
        let outside = base.join("outside/secret.py");
        for rel in ["link/secret.py", "../outside/secret.py", outside.to_str().unwrap(), ""] {
            assert_eq!(lines.get(rel), [""], "{rel}");
        }
        // an open document is the editor's text whatever its key
        assert_eq!(lines.get("../open.py"), ["from the editor", ""]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn utf16_columns() {
        let line = "s = \"é🙂\"; p = make_point()";
        let b = line.find("make_point").unwrap();
        let u = byte_to_utf16(line, b);
        assert_eq!(u, line[..b].chars().map(|c| c.len_utf16() as u64).sum::<u64>());
        assert_eq!(utf16_to_byte(line, u), b);
        assert_eq!(word_at(line, utf16_to_byte(line, u + 3)), Some((b, b + "make_point".len())));
        assert_eq!(word_at(line, b + "make_point".len()), Some((b, b + "make_point".len())), "cursor just after the word");
        assert_eq!(utf16_to_byte(line, 999), line.len());
    }

    #[test]
    fn words_qualifiers_and_uris() {
        assert_eq!(find_word("make_point(make_points) make_point", "make_point"), vec![0, 24]);
        assert_eq!(qualifier("    Point."), Some("Point"));
        assert_eq!(qualifier("a::Point::"), Some("Point"));
        assert_eq!(qualifier("x = "), None);
        let p = if cfg!(windows) { PathBuf::from(r"C:\a b\é.rs") } else { PathBuf::from("/a b/é.rs") };
        let uri = path_to_uri(&p);
        assert!(uri.contains("a%20b/%C3%A9.rs"), "{uri}");
        assert_eq!(uri_to_path(&uri).unwrap(), PathBuf::from(p.to_string_lossy().replace('\\', "/")));
    }

    #[test]
    fn a_uri_names_a_share_on_windows_and_no_file_elsewhere() {
        // a workspace in WSL or on a share: the machine is the URI's host
        assert_eq!(path_of_uri("file://wsl.localhost/Ubuntu/home/u/proj", true).as_deref(), Some("//wsl.localhost/Ubuntu/home/u/proj"));
        assert_eq!(path_of_uri("file:////server/share/a%20b.py", true).as_deref(), Some("//server/share/a b.py"));
        assert_eq!(path_of_uri("file:///C:/a%20b/x.rs", true).as_deref(), Some("C:/a b/x.rs"));
        assert_eq!(path_of_uri("file://localhost/C:/x.rs", true).as_deref(), Some("C:/x.rs"));
        assert_eq!(path_of_uri("file://LOCALHOST/home/u/x", false).as_deref(), Some("/home/u/x"));
        assert_eq!(path_of_uri("file:///home/u/a%20b", false).as_deref(), Some("/home/u/a b"));
        assert_eq!(path_of_uri("file://server/share/x", false), None, "another machine's file is no path here");
        assert_eq!(path_of_uri("https://x/y", true), None);
        // and back: what `canonicalize` gives for a share is written with its host
        // a verbatim share as `slashed` prints it on Windows
        assert_eq!(uri_of(&crate::paths::plain_on(r"\\?\UNC\server\share\repo/a b.py", true).replace('\\', "/")), "file://server/share/repo/a%20b.py");
        assert_eq!(uri_of("//server/share/a.py"), "file://server/share/a.py");
        assert_eq!(uri_of("C:/x/a.py"), "file:///C:/x/a.py");
        assert_eq!(uri_of("/home/u/a.py"), "file:///home/u/a.py");
        for uri in ["file://server/share/repo/a%20b.py", "file:///C:/x/a.py"] {
            assert_eq!(uri_of(&path_of_uri(uri, true).unwrap()), uri);
        }
        // a share on another machine than the root's is not resolved: that would connect to it
        assert!(same_share("//server/share/repo/a.py", "//?/UNC/Server/Share/repo", true));
        assert!(!same_share("//attacker/share/a.py", "//?/UNC/server/share/repo", true));
        assert!(!same_share("//attacker/share/a.py", "C:/repo", true));
        assert!(!same_share("//./pipe/x", "C:/repo", true));
        assert!(same_share("D:/other/a.py", "C:/repo", true));
        assert!(same_share("//anything/at/all", "/repo", false));
    }

    #[test]
    fn a_damaged_index_is_replaced_under_the_readers() {
        let root = std::env::temp_dir().join(format!("scrutate_lsp_damaged_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let session = Session::open(&root, Every::given(Some(3600.0))).unwrap();
        session.build().unwrap();
        let never = Arc::new(AtomicBool::new(false));
        assert_eq!(session.read(&never, false, |s| s.find_symbol("make_point", 5)).unwrap().len(), 1);
        let db = root.join(".scrutate/index.db");
        std::fs::write(&db, vec![0xA5u8; std::fs::metadata(&db).unwrap().len() as usize]).unwrap();
        let hits = session.read(&never, false, |s| s.find_symbol("make_point", 5)).unwrap();
        assert_eq!(hits.len(), 1, "the rebuilt index answers");
        drop(session);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn framing() {
        let raw = b"Content-Length: 2\r\nContent-Type: x\r\n\r\n{}Content-Length: 3\r\n\r\n[1]";
        let mut r = std::io::BufReader::new(&raw[..]);
        assert_eq!(read_message(&mut r).unwrap().unwrap(), b"{}");
        assert_eq!(read_message(&mut r).unwrap().unwrap(), b"[1]");
        assert!(read_message(&mut r).unwrap().is_none());
    }
}
