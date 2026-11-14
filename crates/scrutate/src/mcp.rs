//! MCP server over stdio (JSON-RPC 2.0, newline-delimited), exposing the search tools to Claude Code
//! and other MCP clients. No SDK: the protocol surface a tools-only server needs is small, and a
//! dependency-free implementation is one less thing to break over the years.
//!
//! Two eras of the protocol are served on the same pipe. A client that opens with `initialize`
//! gets the handshake revisions. A request that names its protocol version in `_meta` is served
//! by revision 2026-07-28, which has no handshake: every result then carries `resultType`, and
//! lists say how long they may be cached.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};

use crate::index::FileCount;
use crate::ops;
use crate::session::{Panicked, Session, BUILD_WAIT};

/// Revisions that open with the `initialize` handshake.
const HANDSHAKE_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
/// The revision without a handshake.
const STATELESS_VERSION: &str = "2026-07-28";
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";
/// How long a client may keep a list, in milliseconds: the tools change only with the binary.
const LIST_TTL_MS: u64 = 3_600_000;

/// What a client shows its model before any tool is loaded; the first sentences stand alone.
const INSTRUCTIONS: &str = "scrutate searches the code of this repository from a local index. \
Start with search_code for a question in words or a name, or repo_map for the important files of a repository you do not know; \
use grep_code for an exact pattern, find_symbol for a definition, find_files for a file by a part of its name, \
references for every use of a name, callers and callees to follow calls, call_path for how one function reaches another, \
outline and read_code to read a file in pieces or one whole definition, and impact for what a git diff changes, who calls it \
and which tests to run. \
Every tool only reads, except reindex, which refreshes the index (a cache under .scrutate/). \
Results are capped; a tool says when it stopped early and which argument raises the limit. \
Each result begins with its location (path:start-end). Pass that location wherever a tool takes a symbol name \
to mean that one definition: read_code symbol, find_symbol, callers, callees, call_path, similar_code, references def. \
A result of kind window is a stretch of lines outside every definition: callees and similar_code take its location, \
and read_code reads it by path and lines; find_symbol, callers and references def answer that no symbol is there. \
response_format concise gives the locations and names without code: there a line that ends with / names the directory \
of the paths below it written without one, and a location written :start-end is in the file of the one before it.";

/// The tools, each with the hints clients use to decide whether a call needs approval and, with
/// `structured`, the schema of the JSON its results carry. `short`: the short descriptions and
/// no argument descriptions, for clients that send the list to their model in every session;
/// names, argument types, defaults and what is required stay the same.
pub fn tools(structured: bool, max_bytes: usize, short: bool) -> Value {
    let mut tools = tool_table();
    for tool in tools.as_array_mut().into_iter().flatten() {
        let name = tool["name"].as_str().unwrap_or("").to_string();
        let read_only = ops::read_only(&name);
        let brief = tool.as_object_mut().and_then(|t| t.remove("short")).unwrap_or(Value::Null);
        tool["annotations"] = if short && read_only {
            // the other two hints mean something only for a tool that writes
            json!({"readOnlyHint": true, "openWorldHint": false})
        } else {
            json!({"readOnlyHint": read_only, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false})
        };
        if structured {
            tool["outputSchema"] = ops::output_schema(&name);
        }
        let properties = &mut tool["inputSchema"]["properties"];
        if short {
            // the arguments every tool shares are named once, in the instructions (`instructions`)
            tool["description"] = brief;
            for schema in ["inputSchema", "outputSchema"] {
                tool.get_mut(schema).into_iter().for_each(drop_descriptions);
            }
            continue;
        }
        if read_only {
            properties["max_bytes"] = json!({"type": "integer", "default": max_bytes, "description": "size limit of the answer"});
            properties["offset"] =
                json!({"type": "integer", "default": 0, "description": "items to skip, to continue an answer that said more were not shown"});
        }
        properties["response_format"] = json!({"type": "string", "enum": ["detailed", "concise"], "default": "detailed",
            "description": "concise: locations and names, no code"});
    }
    tools
}

/// What the server tells a client at the start; with the short tool list, the arguments every
/// tool shares, which its schemas then leave out (the server checks them as before).
fn instructions(short: bool, max_bytes: usize) -> String {
    if !short {
        return INSTRUCTIONS.to_string();
    }
    format!(
        "{INSTRUCTIONS} Every tool also takes response_format (\"detailed\", the default, or \"concise\"), and every tool but \
reindex takes max_bytes (an integer, default {max_bytes}) and offset (an integer, default 0)."
    )
}

/// `schema` without the `description` of any property.
fn drop_descriptions(schema: &mut Value) {
    match schema {
        Value::Object(map) => {
            if map.get("description").is_some_and(Value::is_string) {
                map.remove("description");
            }
            map.values_mut().for_each(drop_descriptions);
        }
        Value::Array(items) => items.iter_mut().for_each(drop_descriptions),
        _ => {}
    }
}

fn tool_table() -> Value {
    json!([
        {"name": "search_code", "short": "Search code by meaning or name.",
         "description": "Search the codebase by meaning or by name. Returns the best matching functions/classes with file:line and a snippet. `query` may be natural language (\"where do we refresh expired tokens\") or identifiers (`ClientSession.call_tool`). To improve recall put synonyms and likely identifier names in `alt_queries` (e.g. [\"renew credentials\", \"refresh_token\"], at most 10); results of all phrasings are fused. Vendored code (vendor/, third_party/, node_modules/, Pods/, minified files) ranks below the project's own and is marked \"vendored\": true.",
         "inputSchema": {"type": "object", "properties": {
             "query": {"type": "string"},
             "alt_queries": {"type": ["array", "null"], "items": {"type": "string"}, "default": null},
             "k": {"type": "integer", "default": ops::SEARCH_K}}, "required": ["query"]}},
        {"name": "grep_code", "short": "Exact or regex search, lines grouped by symbol.", "description": "Exact search of the files the index holds: hidden ones such as .github/ included; ignored files, .git/ and dependency or cache directories such as node_modules/ not. Matching lines are grouped by the enclosing function/class. `mode` identifier finds every casing of a name (refreshToken, refresh_token, REFRESH_TOKEN); fuzzy allows `max_edits` typos; `path` keeps one file or directory and `globs` keep files by name (`!` excludes).",
         "inputSchema": {"type": "object", "properties": {
             "pattern": {"type": "string"},
             "mode": {"type": "string", "enum": ["regex", "literal", "identifier", "fuzzy", "fuzzy_regex", "pcre2"], "default": "regex"},
             "case": {"type": "string", "enum": ["sensitive", "insensitive", "smart"], "default": "sensitive"},
             "word": {"type": "boolean", "default": false, "description": "whole words only"},
             "max_edits": {"type": "integer", "default": 1, "description": "edits a fuzzy match may need"},
             "path": {"type": ["string", "null"], "default": null, "description": "a file or directory of the repository to search under"},
             "globs": {"type": ["array", "null"], "items": {"type": "string"}, "default": null, "description": "keep the files these globs match, as grep -g (`!` excludes)"},
             "max_results": {"type": "integer", "default": ops::GREP_MAX}}, "required": ["pattern"]}},
        {"name": "find_symbol", "short": "Where a name is defined.", "description": "Where is this function/class/method defined? (`Class.method` works, as do `Class::method`, `Class#method` and a location from another answer).",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::SYMBOL_LIMIT}}, "required": ["name"]}},
        {"name": "callers", "short": "Who calls a function.", "description": "Which functions call this function?",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::CALLERS_LIMIT}}, "required": ["name"]}},
        {"name": "callees", "short": "What a function calls.", "description": "Which functions does this function call?",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::CALLEES_LIMIT}}, "required": ["name"]}},
        {"name": "call_path", "short": "The call chain from one function to another.", "description": "How does `from` reach `to`? The shortest chain of calls between two functions (names, `Class.method` or locations), each step a call site and the function it calls; a name with several definitions lists them to pick one by location.",
         "inputSchema": {"type": "object", "properties": {"from": {"type": "string"}, "to": {"type": "string"}, "depth": {"type": "integer", "default": ops::PATH_DEPTH, "description": "calls to follow at most"}}, "required": ["from", "to"]}},
        {"name": "repo_map", "short": "The most used files and their top definitions.", "description": "The important files of the repository first: files ranked by how much the others call and import them (PageRank), each with its most called definitions. A first call in a repository you do not know.",
         "inputSchema": {"type": "object", "properties": {"path": {"type": ["string", "null"], "default": null, "description": "keep the files under this directory"}}}},
        {"name": "read_code", "short": "Read lines of a file or a whole definition.", "description": "Read a line range of a file (`path`, `start`, `end`), or one whole definition: `symbol` is a name, `Class.method`, or a location from another answer (`src/a.py:12-40`). A name with several definitions returns each; `path` then keeps the ones in that file.",
         "inputSchema": {"type": "object", "properties": {
             "path": {"type": ["string", "null"], "default": null}, "symbol": {"type": ["string", "null"], "default": null},
             "start": {"type": "integer", "default": 1}, "end": {"type": ["integer", "null"], "default": null}}}},
        {"name": "outline", "short": "Symbols of a file.", "description": "Symbols of a file in source order (kind, name, line range).",
         "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}},
        {"name": "dependencies", "short": "Imports of a file.", "description": "Modules a file imports, resolved to repository files where possible.",
         "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}},
        {"name": "dependents", "short": "Files that import a module or file.", "description": "Files that import a module or repository file (reverse dependencies).",
         "inputSchema": {"type": "object", "properties": {"target": {"type": "string", "description": "module name as imported, or a repository file path"}}, "required": ["target"]}},
        {"name": "references", "short": "Every use of a name.", "description": "Every use of a name, each resolved to the definition it refers to (same-named definitions in other modules are kept apart). `def` (`path` or `path:line`) keeps one definition; `textual` lists every occurrence of the name grouped by the enclosing function/class instead.",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "max_results": {"type": "integer", "default": ops::REFS_MAX},
             "def": {"type": ["string", "null"], "default": null}, "textual": {"type": "boolean", "default": false}}, "required": ["name"]}},
        {"name": "similar_code", "short": "Functions like this one.", "description": "Functions similar to a given symbol or location (`name`, `Class.method` or `path:line`): find duplicated logic or existing implementations.",
         "inputSchema": {"type": "object", "properties": {"target": {"type": "string"}, "k": {"type": "integer", "default": ops::SIMILAR_K}}, "required": ["target"]}},
        {"name": "duplicates", "short": "Duplicated functions.", "description": "Duplicate and near-duplicate functions across the repository.",
         "inputSchema": {"type": "object", "properties": {"min_tokens": {"type": "integer", "default": ops::DUPES_MIN_TOKENS}, "threshold": {"type": "number", "default": ops::DUPES_THRESHOLD}, "limit": {"type": "integer", "default": ops::DUPES_LIMIT}}}},
        {"name": "structural_search", "short": "A tree-sitter query over one language.", "description": "Run a tree-sitter query (S-expression pattern with @captures) over all files of one language, e.g. lang=python query=\"(class_definition name: (identifier) @cls)\".",
         "inputSchema": {"type": "object", "properties": {"lang": {"type": "string"}, "query": {"type": "string"}, "limit": {"type": "integer", "default": ops::TSQUERY_LIMIT}}, "required": ["lang", "query"]}},
        {"name": "impact", "short": "Definitions, callers and tests a git diff touches.", "description": "What does my change touch? From the git diff (the working tree and untracked files against HEAD; `base` a revision, `main...` for the merge base with main; `staged` the staged changes) to the changed definitions, their callers with distances up to `depth` steps, and the test files to run.",
         "inputSchema": {"type": "object", "properties": {
             "base": {"type": ["string", "null"], "default": null, "description": "revision to compare with (default HEAD)"},
             "staged": {"type": "boolean", "default": false},
             "depth": {"type": "integer", "default": crate::impact::DEPTH, "description": "steps of callers to follow (at most 10)"}}}},
        {"name": "find_files", "short": "Find a file by part of its name.", "description": "Find a file by part of its name or path, misspelt or with letters left out (`srch.rs` finds search.rs): an exact basename first, then basename matches, then path matches; among equals the files read lately through read_code first. A query that is exactly one path counts as choosing that file. Needs no index.",
         "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}, "limit": {"type": "integer", "default": crate::finder::LIMIT}}, "required": ["query"]}},
        {"name": "dead_code", "short": "Definitions nothing uses.", "description": "Functions, methods, classes and types nothing in the repository uses: no line outside one names it. Entry points (main, tests, exports, public items of a library, decorated or attribute-registered ones) are left out. Ones whose name some line holds that the resolver ties to nothing (a string, a comment, a use it cannot place) are listed as undecided, with why. `path` keeps the definitions under one directory or file.",
         "inputSchema": {"type": "object", "properties": {"path": {"type": ["string", "null"], "default": null, "description": "a directory or file of the repository"}}}},
        {"name": "reindex", "short": "Rescan changed files.", "description": "Re-scan for changed files (searches already do this automatically). `force` reads every file again, also one whose size, modification time and change time did not change.",
         "inputSchema": {"type": "object", "properties": {"force": {"type": "boolean", "default": false}}}}
    ])
}

/// What a tool call returns: text for the model, the same answer as JSON for a client that
/// reads structured results, and whether it failed.
pub struct Called {
    pub text: String,
    pub structured: Value,
    pub is_error: bool,
}

impl Called {
    /// A sentence in place of an answer.
    fn note(text: String, is_error: bool) -> Called {
        let structured = json!({"schema": ops::ANSWER_SCHEMA, "complete": false, "result": null, "text": text});
        Called { text, structured, is_error }
    }
}

/// Run one tool; errors become text for the model.
pub fn call_tool(session: &Session, name: &str, args: &Value, settings: &Settings, cancel: &Arc<AtomicBool>) -> Called {
    call_counted(session, name, args, settings, cancel, &Arc::default())
}

/// `call_tool`, counting the files the call reads in `count` as it goes.
fn call_counted(session: &Session, name: &str, args: &Value, settings: &Settings, cancel: &Arc<AtomicBool>, count: &Arc<FileCount>) -> Called {
    let max_bytes = args.get("max_bytes").and_then(Value::as_u64).map_or(settings.max_bytes, |n| clamp_bytes(n as usize));
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    // a wrong value is refused with the other arguments
    let form = ops::Form::from_tool(args).unwrap_or_default();
    match ops::Op::from_tool(name, args).and_then(|op| session.run_counted(&op, cancel, count)) {
        Ok(answer) => {
            let (text, shown, total) = answer.text_page(max_bytes, offset, form);
            let mut structured = answer.structured(text.len(), max_bytes, offset, shown, total);
            if structured["result"].is_null() {
                // a client that shows only the structured form still gets the part that fits
                structured["text"] = json!(text);
            }
            Called { text, structured, is_error: false }
        }
        Err(e) if e.is::<crate::index::BeingBuilt>() => Called::note(STILL_BUILDING.to_string(), false),
        Err(e) if e.is::<Panicked>() => {
            let text = if session.build_failed() {
                "error: the index could not be built: internal failure while indexing (a bug: please report it, with the server's stderr); `scrutate index --force` in the repository may clear it, and the server keeps running".to_string()
            } else {
                format!("error: internal failure while running {name} (a bug: please report it); the server keeps running")
            };
            Called::note(text, true)
        }
        Err(e) => Called::note(format!("error: {e}"), true),
    }
}

/// The answer to a call that came before the first index of the repository is ready.
const STILL_BUILDING: &str =
    "scrutate is still building its index of this repository (first use, or many files changed). Call the tool again in a little while.";

/// Where messages go: stdout in the server, a list in its tests.
type Wire<'a> = dyn Fn(Value) + Sync + 'a;

/// One line on stdout: a message leaves whole whichever thread writes it.
fn to_stdout(msg: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// `stateless`: the request came under the revision whose results all say what kind they are.
fn respond(send: &Wire, id: &Value, stateless: bool, mut result: Value) {
    if stateless {
        result["resultType"] = json!("complete");
    }
    send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn respond_err(send: &Wire, id: &Value, code: i64, message: &str, data: Option<Value>) {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    send(json!({"jsonrpc": "2.0", "id": id, "error": error}));
}

/// Tool calls in flight, by request id, each with the flag `notifications/cancelled` sets.
#[derive(Default)]
struct Calls(Mutex<HashMap<String, Arc<AtomicBool>>>);

impl Calls {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<AtomicBool>>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn begin(&self, id: &Value) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.map().insert(id.to_string(), Arc::clone(&cancelled));
        cancelled
    }
    fn cancel(&self, id: &Value) {
        if let Some(cancelled) = self.map().get(&id.to_string()) {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

struct Call {
    id: Value,
    name: String,
    args: Value,
    stateless: bool,
    /// send the answer as JSON too (`serve --structured`)
    structured: bool,
    /// the client's token for progress reports; null when it did not ask for any
    progress: Value,
    cancelled: Arc<AtomicBool>,
}

/// A tool call waits up to `BUILD_WAIT` for an index that is still being built before it answers
/// that it is. A client that sent a progress token waits for the index and gets the real answer,
/// with the files read so far against the files to read in `notifications/progress`, while the
/// index is built and while the call runs, at most once per `PROGRESS_EVERY`.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);
/// How often a running call's count is looked at.
const PROGRESS_LOOK: Duration = Duration::from_millis(50);
/// Calls beyond this many in flight run on the thread that reads requests.
const MAX_CALLS: usize = 32;

/// The progress of one call: a count that rises with every report, as the protocol asks.
struct Reporter<'a> {
    send: &'a Wire<'a>,
    token: Value,
    sent: usize,
    /// what an earlier count had reached: a later one adds to it
    base: usize,
    last: Option<Instant>,
}

impl Reporter<'_> {
    fn report(&mut self, (done, total): (usize, usize), message: &str) {
        let progress = self.base + done;
        if self.token.is_null() || progress <= self.sent || self.last.is_some_and(|t| t.elapsed() < PROGRESS_EVERY) {
            return;
        }
        let mut params = json!({"progressToken": self.token, "progress": progress, "message": message});
        if total > 0 {
            params["total"] = json!(self.base + total.max(done));
        }
        (self.send)(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}));
        self.sent = progress;
        self.last = Some(Instant::now());
    }
}

/// One tool call from its first wait to its answer. A cancelled call sends nothing more: it
/// stops waiting for the index or for its turn, an index update or a structural search it runs
/// stops, and an answer already being computed is dropped.
fn run_call(send: &Wire, session: &Session, calls: &Calls, settings: &Settings, call: Call) {
    let Call { id, name, args, stateless, structured, progress, cancelled } = call;
    let gone = || cancelled.load(Ordering::Relaxed);
    let limit = progress.is_null().then_some(BUILD_WAIT);
    let mut reporter = Reporter { send, token: progress, sent: 0, base: 0, last: None };
    let built = session.wait_built(&cancelled, limit, |_| reporter.report(session.build_count(), "building the index of this repository"));
    let called = if gone() {
        None
    } else if !built {
        Some(Called::note(STILL_BUILDING.to_string(), false))
    } else {
        reporter.base = reporter.sent;
        let count = Arc::new(FileCount::default());
        let finished = AtomicBool::new(false);
        let message = format!("{name}: files read");
        Some(std::thread::scope(|s| {
            let ticker = (!reporter.token.is_null()).then(|| {
                s.spawn(|| loop {
                    std::thread::park_timeout(PROGRESS_LOOK);
                    if finished.load(Ordering::Acquire) || gone() {
                        break;
                    }
                    reporter.report(count.get(), &message);
                })
            });
            let c = call_counted(session, &name, &args, settings, &cancelled, &count);
            finished.store(true, Ordering::Release);
            if let Some(t) = &ticker {
                t.thread().unpark();
            }
            c
        }))
    };
    calls.map().remove(&id.to_string());
    if let Some(c) = called.filter(|_| !gone()) {
        let mut result = json!({"content": [{"type": "text", "text": c.text}], "isError": c.is_error});
        if structured {
            result["structuredContent"] = c.structured;
        }
        respond(send, &id, stateless, result);
    }
}

/// The longest message read, as the language server's: a line without its end past it is dropped,
/// not held in memory.
const MAX_MESSAGE: usize = 256 << 20;

enum Line {
    Read,
    TooLong,
    End,
}

/// One line of `r` into `buf`, at most `max` bytes before its line end; a longer one is read to
/// its end a piece at a time and dropped.
fn read_line(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> std::io::Result<Line> {
    use std::io::Read;
    buf.clear();
    if r.by_ref().take(max as u64 + 1).read_until(b'\n', buf)? == 0 {
        return Ok(Line::End);
    }
    if buf.len() <= max || buf.ends_with(b"\n") {
        return Ok(Line::Read);
    }
    loop {
        buf.clear();
        if r.by_ref().take(1 << 16).read_until(b'\n', buf)? == 0 || buf.ends_with(b"\n") {
            buf.clear();
            return Ok(Line::TooLong);
        }
    }
}

/// Serve until stdin closes. The first indexing runs in the background so a client is answered
/// at once; tool calls that arrive meanwhile wait for it (`BUILD_WAIT`). Each call runs on a
/// thread of its own, so the reader still sees a `notifications/cancelled` for it.
///
/// `structured`: every tool declares an `outputSchema` and every result carries the answer as
/// JSON beside its text. Off unless asked for: Claude Code (2.1.322) shows its model that JSON in
/// place of the text, which has the snippets and says how to page. `short`: `tools/list` sends
/// the short form of `tools`.
pub fn serve(session: Arc<Session>, settings: Settings, version: &str, structured: bool, short: bool) -> Result<()> {
    session.building();
    {
        let session = Arc::clone(&session);
        std::thread::Builder::new().name("scrutate-index".into()).spawn(move || match session.build() {
            Err(e) if e.is::<Panicked>() => eprintln!("[scrutate] indexing failed: internal failure (a bug: please report it); each tool call tries again"),
            Err(e) => eprintln!("[scrutate] indexing failed: {e:#}"),
            Ok(_) => {}
        })?;
    }
    let calls = Calls::default();
    let server = Server { session: &session, calls: &calls, settings: &settings, version, structured, short };
    let (server, send) = (&server, &to_stdout as &Wire);
    std::thread::scope(|scope| -> Result<()> {
        let mut stdin = std::io::stdin().lock();
        let mut buf = Vec::new();
        loop {
            match read_line(&mut stdin, &mut buf, MAX_MESSAGE)? {
                Line::End => break,
                Line::TooLong => {
                    respond_err(send, &Value::Null, -32700, "parse error: a message is at most 256 MiB", None);
                    continue;
                }
                Line::Read => {}
            }
            let Ok(line) = std::str::from_utf8(&buf) else {
                respond_err(send, &Value::Null, -32700, "parse error: not UTF-8", None);
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(Value::Array(batch)) if !batch.is_empty() => {
                    scope.spawn(move || {
                        let replies = Mutex::new(Vec::new());
                        // progress notifications go out at once; the batch's answers go out together
                        let sink = |m: Value| {
                            if m.get("method").is_some() {
                                send(m)
                            } else {
                                replies.lock().unwrap_or_else(|p| p.into_inner()).push(m)
                            }
                        };
                        std::thread::scope(|inner| batch.into_iter().for_each(|m| server.handle(inner, &sink, m)));
                        let replies = replies.into_inner().unwrap_or_else(|p| p.into_inner());
                        if !replies.is_empty() {
                            send(Value::Array(replies));
                        }
                    });
                }
                Ok(msg) => server.handle(scope, send, msg),
                Err(_) => respond_err(send, &Value::Null, -32700, "parse error", None),
            }
        }
        Ok(())
    })?;
    session.hold_writes();
    Ok(())
}

/// The answer size `serve` was started with.
pub struct Settings {
    /// answer size of a call that gives no `max_bytes`
    max_bytes: usize,
}

impl Settings {
    pub fn new(max_bytes: Option<usize>) -> Settings {
        Settings { max_bytes: max_bytes.map_or(ops::AGENT_BYTES, clamp_bytes) }
    }
}

fn clamp_bytes(n: usize) -> usize {
    n.clamp(1_000, 1_000_000)
}

/// What every message is handled with.
struct Server<'a> {
    session: &'a Session,
    calls: &'a Calls,
    settings: &'a Settings,
    version: &'a str,
    structured: bool,
    short: bool,
}

impl Server<'_> {
    /// Answer one message, alone or a member of a batch, through `send`.
    fn handle<'s, 'e>(&'e self, scope: &'s std::thread::Scope<'s, 'e>, send: &'e Wire<'e>, msg: Value) {
        if !msg.is_object() {
            return respond_err(send, &Value::Null, -32600, "invalid request: not a JSON object", None);
        }
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // a client's response to a request is not answered
            if msg.get("method").is_none() && (msg.get("result").is_some() || msg.get("error").is_some()) {
                return;
            }
            return respond_err(send, &id, -32600, "invalid request: no method", None);
        };
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        if msg.get("id").is_none() {
            if method == "notifications/cancelled" {
                self.calls.cancel(&params["requestId"]);
            }
            return;
        }
        let asked = params.get("_meta").and_then(|m| m.get(VERSION_KEY)).and_then(Value::as_str);
        if let Some(other) = asked.filter(|v| *v != STATELESS_VERSION) {
            return respond_err(send, &id, -32022, "Unsupported protocol version", Some(json!({"supported": [STATELESS_VERSION], "requested": other})));
        }
        let stateless = asked.is_some();
        // a list a client may cache, under the revision that asks every list to say so
        let list = |mut result: Value| {
            if stateless {
                result["ttlMs"] = json!(LIST_TTL_MS);
                result["cacheScope"] = json!("public");
            }
            respond(send, &id, stateless, result);
        };
        match method {
            "server/discover" if stateless => list(json!({
                "supportedVersions": [STATELESS_VERSION],
                "capabilities": {"tools": {}},
                "instructions": instructions(self.short, self.settings.max_bytes),
                "_meta": {SERVER_INFO_KEY: {"name": "scrutate", "version": self.version}},
            })),
            "server/discover" => respond_err(send, &id, -32602, &format!("_meta has no {VERSION_KEY}"), None),
            "initialize" => {
                let latest = HANDSHAKE_VERSIONS[HANDSHAKE_VERSIONS.len() - 1];
                let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(latest);
                let ver = if HANDSHAKE_VERSIONS.contains(&asked) { asked } else { latest };
                respond(
                    send,
                    &id,
                    false,
                    json!({
                        "protocolVersion": ver,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "scrutate", "version": self.version},
                        "instructions": instructions(self.short, self.settings.max_bytes),
                    }),
                );
            }
            "ping" => respond(send, &id, stateless, json!({})),
            "tools/list" => list(json!({"tools": tools(self.structured, self.settings.max_bytes, self.short)})),
            "resources/list" => list(json!({"resources": []})),
            "resources/templates/list" => list(json!({"resourceTemplates": []})),
            "prompts/list" => list(json!({"prompts": []})),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return respond_err(send, &id, -32602, "tools/call has no params.name", None);
                };
                if !tool_table().as_array().into_iter().flatten().any(|t| t["name"] == name) {
                    return respond_err(send, &id, -32602, &format!("unknown tool: {name}"), None);
                }
                let call = Call {
                    name: name.to_string(),
                    args: params.get("arguments").cloned().unwrap_or(json!({})),
                    stateless,
                    structured: self.structured,
                    progress: params.get("_meta").and_then(|m| m.get("progressToken")).cloned().unwrap_or(Value::Null),
                    cancelled: self.calls.begin(&id),
                    id,
                };
                let (session, calls, settings) = (self.session, self.calls, self.settings);
                let run = move || run_call(send, session, calls, settings, call);
                if calls.map().len() > MAX_CALLS {
                    run();
                } else {
                    scope.spawn(run);
                }
            }
            _ => respond_err(send, &id, -32601, &format!("method not found: {method}"), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_past_the_limit_is_dropped_whole_and_the_next_one_read() {
        let mut r = std::io::Cursor::new(b"{\"a\":1}\n0123456789abcdef0123\n{\"b\":2}\n".to_vec());
        let mut buf = Vec::new();
        assert!(matches!(read_line(&mut r, &mut buf, 16).unwrap(), Line::Read) && buf == b"{\"a\":1}\n");
        assert!(matches!(read_line(&mut r, &mut buf, 16).unwrap(), Line::TooLong));
        assert!(matches!(read_line(&mut r, &mut buf, 16).unwrap(), Line::Read) && buf == b"{\"b\":2}\n");
        assert!(matches!(read_line(&mut r, &mut buf, 16).unwrap(), Line::End));
    }
    use crate::session::Every;
    use std::time::Duration;

    /// A repository with one function, its session with the first build done, and the messages a
    /// call sends.
    fn fixture(name: &str) -> (std::path::PathBuf, Session, Mutex<Vec<Value>>) {
        let root = std::env::temp_dir().join(format!("scrutate_mcp_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
        session.build().unwrap();
        (root, session, Mutex::new(Vec::new()))
    }

    fn call(calls: &Calls, id: i64, progress: Value) -> Call {
        let id = json!(id);
        Call { name: "find_symbol".into(), args: json!({"name": "make_point"}), stateless: true, structured: true, progress, cancelled: calls.begin(&id), id }
    }

    #[test]
    fn a_cancelled_call_stops_the_reindex_it_runs() {
        let (root, session, _) = fixture("cancel_reindex");
        let settings = Settings::new(None);
        // what `notifications/cancelled` sets for the call in flight
        let called = call_tool(&session, "reindex", &json!({"force": true}), &settings, &Arc::new(AtomicBool::new(true)));
        assert!(called.is_error && called.text.contains("cancelled"), "{}", called.text);
        let called = call_tool(&session, "find_symbol", &json!({"name": "make_point"}), &settings, &Arc::default());
        assert!(called.text.contains("src/geo.py"), "the index is as it was: {}", called.text);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_call_that_panics_is_a_tool_error_and_only_a_call_that_updates_clears_the_failed_first_build() {
        let (root, session, sent) = fixture("build_failed_reset");
        // the state a first build that panicked leaves
        session.build_failing_for_test();
        assert!(session.build_failed());
        let calls = Calls::default();
        let send = |m: Value| sent.lock().unwrap().push(m);
        let settings = Settings::new(None);
        // neither a call with a bad argument nor a read of a file by its path looks at the index
        let read = Call { name: "read_code".into(), args: json!({"path": "src/geo.py"}), ..call(&calls, 5, Value::Null) };
        let bad = Call { args: json!({"name": 7}), ..call(&calls, 6, Value::Null) };
        for c in [read, bad] {
            run_call(&send, &session, &calls, &settings, c);
            assert!(session.build_failed());
        }
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 2\n").unwrap();
        run_call(&send, &session, &calls, &settings, call(&calls, 7, Value::Null));
        let sent = sent.lock().unwrap();
        assert_eq!((&sent[0]["result"]["isError"], &sent[1]["result"]["isError"], &sent[2]["result"]["isError"]), (&json!(false), &json!(true), &json!(false)));
        assert!(!session.build_failed());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_call_with_a_progress_token_reports_files_while_the_index_is_built_and_then_answers() {
        let (root, first, sent) = fixture("progress");
        drop(first);
        for i in 0..700 {
            std::fs::write(root.join(format!("src/m{i}.py")), format!("def f{i}():\n    return {i}\n")).unwrap();
        }
        let session = Session::open(&root, Every::given(Some(0.0))).unwrap();
        let calls = Calls::default();
        let send = |m: Value| sent.lock().unwrap().push(m);
        session.building();
        std::thread::scope(|s| {
            let job = s.spawn(|| run_call(&send, &session, &calls, &Settings::new(None), call(&calls, 7, json!("tok"))));
            std::thread::sleep(Duration::from_millis(100));
            assert!(!job.is_finished(), "the call waits for the index");
            session.build().unwrap();
        });
        let sent = sent.lock().unwrap().clone();
        let (last, progress) = sent.split_last().unwrap();
        assert!(!progress.is_empty(), "{sent:?}");
        let mut before = 0;
        for p in progress {
            assert_eq!(p["method"], "notifications/progress");
            assert_eq!(p["params"]["progressToken"], "tok");
            let (done, total) = (p["params"]["progress"].as_u64().unwrap(), p["params"]["total"].as_u64().unwrap());
            assert!(done > before && done <= total && total >= 700, "{p}");
            before = done;
        }
        assert_eq!(last["id"], 7);
        assert_eq!(last["result"]["resultType"], "complete");
        assert!(last["result"]["content"][0]["text"].as_str().unwrap().starts_with("src/geo.py:1-2"), "{last}");
        assert_eq!(last["result"]["structuredContent"]["result"][0]["id"], "src/geo.py:1-2");
        assert!(calls.map().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn progress_rises_with_every_report_and_comes_at_most_once_per_interval() {
        let sent = Mutex::new(Vec::new());
        let send = |m: Value| sent.lock().unwrap().push(m["params"].clone());
        let mut r = Reporter { send: &send, token: json!(3), sent: 0, base: 0, last: None };
        r.report((0, 0), "m");
        r.report((5, 0), "m");
        r.report((9, 20), "m");
        r.last = r.last.map(|t| t - PROGRESS_EVERY);
        r.report((5, 20), "m");
        r.report((12, 20), "m");
        // a second count goes on from where the first one was reported
        r.base = r.sent;
        r.last = r.last.map(|t| t - PROGRESS_EVERY);
        r.report((4, 10), "m");
        let sent = sent.into_inner().unwrap();
        assert_eq!(
            sent,
            [
                json!({"progressToken": 3, "progress": 5, "message": "m"}),
                json!({"progressToken": 3, "progress": 12, "total": 20, "message": "m"}),
                json!({"progressToken": 3, "progress": 16, "total": 22, "message": "m"})
            ]
        );
        let mut silent = Reporter { send: &|_| panic!("no token, no report"), token: Value::Null, sent: 0, base: 0, last: None };
        silent.report((5, 9), "m");
    }

    #[test]
    fn a_cancelled_call_sends_nothing() {
        let (root, session, sent) = fixture("cancel");
        let calls = Calls::default();
        let send = |m: Value| sent.lock().unwrap().push(m);
        session.building();
        std::thread::scope(|s| {
            // waiting for the index: the notice ends the wait
            let waiting = call(&calls, 1, Value::Null);
            let job = s.spawn(|| run_call(&send, &session, &calls, &Settings::new(None), waiting));
            calls.cancel(&json!(1));
            job.join().unwrap();
            calls.cancel(&json!(99)); // a notice for a request that is not there is ignored
        });
        session.build().unwrap();
        // cancelled before its answer: the answer is dropped
        let done = call(&calls, 2, Value::Null);
        calls.cancel(&json!(2));
        run_call(&send, &session, &calls, &Settings::new(None), done);
        assert_eq!(*sent.lock().unwrap(), Vec::<Value>::new());
        assert!(calls.map().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
