//! Every question a front end can ask of the index: its arguments with their defaults and limits,
//! when the index is refreshed first, and a typed answer with one text form and one JSON form.
//! The CLI, the MCP server, `batch`, the Python bindings and the language server parse into `Op`
//! and render `Answer`; none of them holds a default, a limit or an output format of its own.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::fmt::{hit_blocks, hits_json};
use crate::index::{ChunkRow, UpdateStats};
use crate::intel::{Capture, DupGroup, RefGroup, References};
use crate::names::{Dependency, Names};

use crate::fmt::{echo, shown};
use crate::instant::{Case, Files, Mode, Pattern};
use crate::resolve::Resolved;
use crate::search::{Hit, Searcher};
use crate::text::query_words;

/// Version of Engine A's JSON Lines and of the error records.
pub const SCHEMA: u32 = 1;
/// Version of the one JSON envelope every answer leaves in (`Answer::envelope`): the CLI's
/// `--json` of an index command, a `batch` record and the MCP server's structured result.
pub const ANSWER_SCHEMA: u32 = 2;

pub const SEARCH_K: usize = 8;
/// Alternative phrasings one search fuses at most: each costs a full search, and fusion gains
/// little past a handful.
pub const ALT_QUERIES: usize = 10;
pub const SYMBOL_LIMIT: usize = 20;
pub const CALLERS_LIMIT: usize = 30;
pub const CALLEES_LIMIT: usize = 40;
/// Names of callees outside the repository that an answer lists.
pub const EXTERNAL_CALLEES: usize = 50;
pub const GREP_MAX: u64 = 100;
pub const REFS_MAX: u64 = 200;
pub const SIMILAR_K: usize = 10;
pub const DUPES_MIN_TOKENS: usize = 30;
pub const DUPES_THRESHOLD: f32 = 0.75;
pub const DUPES_LIMIT: usize = 50;
pub const TSQUERY_LIMIT: usize = 1000;
/// Calls a call path follows at most.
pub const PATH_DEPTH: usize = 10;
/// Lines one `read` returns at most.
pub const READ_LINES: i64 = 400;
/// Bytes of line text one `read_code` of a line range collects at most; a longer line is cut.
pub const READ_BYTES: usize = 1_000_000;
/// Definitions one `read` of a name returns at most.
pub const READ_DEFINITIONS: usize = 20;
/// Bytes of text an agent gets from one call unless it asks for more: about 6,000 tokens, under
/// the 10,000 at which Codex cuts a tool result and Claude Code warns.
pub const AGENT_BYTES: usize = 24_000;

/// Serialized as a command line sends it to a session; a grep never goes that way.
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Op {
    /// `snippets`: each hit carries its text (off for a caller that reads locations only).
    Search {
        query: String,
        alt: Vec<String>,
        k: usize,
        snippets: bool,
    },
    /// An Engine A search of the repository's files, its lines grouped by enclosing symbol.
    #[serde(skip)]
    Grep {
        pattern: Pattern,
        scope: Scope,
        max_results: u64,
    },
    Symbol {
        name: String,
        limit: usize,
    },
    Callers {
        name: String,
        limit: usize,
    },
    Callees {
        name: String,
        limit: usize,
    },
    /// A line range of `path`, or with `symbol` (a name or a location) every definition it names,
    /// whole; `path` then keeps the definitions in that file.
    Read {
        path: Option<String>,
        symbol: Option<String>,
        start: i64,
        end: Option<i64>,
    },
    Outline {
        path: String,
    },
    Deps {
        path: String,
    },
    Rdeps {
        target: String,
    },
    /// `textual`: every occurrence of the name grouped by enclosing symbol, instead of each one
    /// resolved to the definition it refers to. `def` (`path` or `path:line`) keeps one definition.
    Refs {
        name: String,
        max_results: u64,
        textual: bool,
        def: Option<String>,
    },
    Similar {
        target: String,
        k: usize,
    },
    Dupes {
        min_tokens: usize,
        threshold: f32,
        limit: usize,
    },
    Tsquery {
        lang: String,
        query: String,
        limit: usize,
    },
    /// The files ranked by how the others call and import them, each with its top definitions;
    /// `path` keeps the files under one directory.
    Map {
        path: Option<String>,
    },
    /// The shortest chain of calls from one definition (a name or a location) to another.
    Path {
        from: String,
        to: String,
        depth: usize,
    },
    /// Definitions nothing in the repository uses, those under `path` (a directory or file) alone.
    Dead {
        path: Option<String>,
    },
    /// `force`: read every file again, also one whose size, modification and change times are unchanged.
    Reindex {
        force: bool,
    },
    /// What the working tree (`staged`: the staged changes) changes against `base` (`HEAD` when
    /// none): the changed definitions, their callers up to `depth` steps away and the tests to run.
    Impact {
        base: Option<String>,
        staged: bool,
        depth: usize,
    },
    /// The paths of the repository ranked for a short, possibly misspelt `query` (`finder::find`).
    Files {
        query: String,
        limit: usize,
    },
}

impl Op {
    /// The operation an MCP tool call or a `batch` line names, defaults filled in and limits
    /// clamped (the caller is a program: an absurd limit is cut, not refused).
    pub fn from_tool(name: &str, args: &Value) -> Result<Op> {
        // a JSON null is an argument left out; any other value of the wrong type is an error
        let arg = |k: &str| args.get(k).filter(|v| !v.is_null());
        let wrong = |k: &str, ty: &str| anyhow!("{k} must be {ty}");
        let opt_text = |k: &str| arg(k).map(|v| v.as_str().map(str::to_string).ok_or_else(|| wrong(k, "a string"))).transpose();
        let text = |k: &str| opt_text(k)?.ok_or_else(|| anyhow!("missing required argument {k}"));
        let natural = |k: &str| arg(k).map(|v| v.as_u64().ok_or_else(|| wrong(k, "a non-negative integer"))).transpose();
        let count = |k: &str, default: u64, max: u64| natural(k).map(|n| n.unwrap_or(default).clamp(1, max));
        let line = |k: &str| arg(k).map(|v| v.as_i64().ok_or_else(|| wrong(k, "an integer (a line number)"))).transpose();
        let flag = |k: &str| arg(k).map(|v| v.as_bool().ok_or_else(|| wrong(k, "a boolean (true or false)"))).transpose().map(|b| b.unwrap_or(false));
        // read by the server, not here, but checked with the rest
        natural("max_bytes")?;
        natural("offset")?;
        Form::from_tool(args)?;
        Ok(match name {
            "search_code" => Op::Search {
                query: text("query")?,
                alt: match arg("alt_queries") {
                    None => Vec::new(),
                    Some(v) => v
                        .as_array()
                        .and_then(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>())
                        .ok_or_else(|| wrong("alt_queries", "an array of strings"))?
                        .into_iter()
                        .take(ALT_QUERIES)
                        .collect(),
                },
                k: count("k", SEARCH_K as u64, 100)? as usize,
                snippets: true,
            },
            "grep_code" => Op::Grep {
                pattern: Pattern {
                    text: text("pattern")?,
                    mode: mode_named(opt_text("mode")?.as_deref().unwrap_or("regex"), natural("max_edits")?.unwrap_or(1).min(u8::MAX as u64) as u8)?,
                    case: case_named(opt_text("case")?.as_deref().unwrap_or("sensitive"))?,
                    word: flag("word")?,
                    ..Pattern::default()
                },
                scope: Scope {
                    path: opt_text("path")?,
                    globs: match arg("globs") {
                        None => Vec::new(),
                        Some(v) => v
                            .as_array()
                            .and_then(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>())
                            .ok_or_else(|| wrong("globs", "an array of strings"))?,
                    },
                },
                max_results: count("max_results", GREP_MAX, 5000)?,
            },
            "find_symbol" => Op::Symbol { name: text("name")?, limit: count("limit", SYMBOL_LIMIT as u64, 500)? as usize },
            "callers" => Op::Callers { name: text("name")?, limit: count("limit", CALLERS_LIMIT as u64, 500)? as usize },
            "callees" => Op::Callees { name: text("name")?, limit: count("limit", CALLEES_LIMIT as u64, 500)? as usize },
            "read_code" => {
                let (path, symbol) = (opt_text("path")?, opt_text("symbol")?);
                if path.is_none() && symbol.is_none() {
                    bail!("missing path or symbol: read_code takes path (a file; start and end pick lines) or symbol (a name, Class.method or a path:start-end location)");
                }
                let (start, end) = (line("start")?.unwrap_or(1), line("end")?);
                if let Some(end) = end.filter(|e| *e < start) {
                    bail!("end ({end}) is before start ({start})");
                }
                Op::Read { path, symbol, start, end }
            }
            "outline" => Op::Outline { path: text("path")? },
            "dependencies" => Op::Deps { path: text("path")? },
            "dependents" => Op::Rdeps { target: text("target")? },
            "references" => {
                let name = text("name")?;
                if name.trim().is_empty() {
                    bail!("name is empty");
                }
                Op::Refs { name, max_results: count("max_results", REFS_MAX, 5000)?, textual: flag("textual")?, def: opt_text("def")? }
            }
            "similar_code" => Op::Similar { target: text("target")?, k: count("k", SIMILAR_K as u64, 50)? as usize },
            "duplicates" => Op::Dupes {
                min_tokens: count("min_tokens", DUPES_MIN_TOKENS as u64, 100_000)? as usize,
                threshold: arg("threshold")
                    .map(|v| v.as_f64().ok_or_else(|| wrong("threshold", "a number")))
                    .transpose()?
                    .map_or(DUPES_THRESHOLD, |t| t as f32),
                limit: count("limit", DUPES_LIMIT as u64, 500)? as usize,
            },
            "structural_search" => Op::Tsquery { lang: text("lang")?, query: text("query")?, limit: count("limit", TSQUERY_LIMIT as u64, 5000)? as usize },
            "repo_map" => Op::Map { path: opt_text("path")? },
            "call_path" => Op::Path { from: text("from")?, to: text("to")?, depth: count("depth", PATH_DEPTH as u64, 100)? as usize },
            "dead_code" => Op::Dead { path: opt_text("path")? },
            "reindex" => Op::Reindex { force: flag("force")? },
            "impact" => Op::Impact {
                base: opt_text("base")?,
                staged: flag("staged")?,
                depth: natural("depth")?.map_or(crate::impact::DEPTH, |d| d.min(crate::impact::MAX_DEPTH as u64) as usize),
            },
            "find_files" => Op::Files { query: text("query")?, limit: count("limit", crate::finder::LIMIT as u64, crate::finder::MAX_LIMIT)? as usize },
            _ => bail!("unknown tool {}", echo(name)),
        })
    }
}

impl Op {
    /// Whether the index is brought up to date before this operation: a line range of a file
    /// reads the file alone, and a reindex is the update.
    pub fn reads_index(&self) -> bool {
        !matches!(self, Op::Read { symbol: None, .. } | Op::Reindex { .. } | Op::Files { .. })
    }

    /// Repository files the arguments name (a path, or the file of a location), whose index
    /// entries an answer depends on even when it cites nothing.
    pub fn named_paths(&self) -> Vec<String> {
        let loc = |n: &str| crate::intel::location(n).map(|(p, _)| p.to_string());
        let plain = |p: &str| p.trim_start_matches("./").to_string();
        match self {
            Op::Symbol { name, .. } | Op::Callers { name, .. } | Op::Callees { name, .. } => loc(name).into_iter().collect(),
            Op::Similar { target, .. } => loc(target).into_iter().collect(),
            Op::Path { from, to, .. } => loc(from).into_iter().chain(loc(to)).collect(),
            Op::Read { path, symbol: Some(symbol), .. } => loc(symbol).into_iter().chain(path.as_deref().map(plain)).collect(),
            Op::Outline { path } | Op::Deps { path } => vec![plain(path)],
            Op::Refs { def: Some(def), .. } => vec![loc(def).unwrap_or_else(|| plain(def))],
            _ => Vec::new(),
        }
    }
}

/// The matching mode a front end names: `regex`, `literal`, `identifier`, `fuzzy` and
/// `fuzzy_regex` (within `max_edits` edits) or `pcre2`.
pub fn mode_named(name: &str, max_edits: u8) -> Result<Mode> {
    Ok(match name {
        "regex" => Mode::Regex,
        "literal" => Mode::Literal,
        "identifier" => Mode::Identifier,
        "pcre2" => Mode::Pcre2,
        "fuzzy" => Mode::Fuzzy { max_edits },
        "fuzzy_regex" => Mode::FuzzyRegex { max_edits },
        other => bail!("unknown mode {}: regex, literal, identifier, pcre2, fuzzy or fuzzy_regex", echo(other)),
    })
}

/// The case rule a front end names: `sensitive`, `insensitive` or `smart`.
pub fn case_named(name: &str) -> Result<Case> {
    Ok(match name {
        "sensitive" => Case::Sensitive,
        "insensitive" => Case::Insensitive,
        "smart" => Case::Smart,
        other => bail!("unknown case {}: sensitive, insensitive or smart", echo(other)),
    })
}

/// Which files of the repository a grep reads: the ones a search of the repository covers
/// (`files::repository_files`), under `path` (a file or directory, from the root) and kept by
/// `globs` (as `grep -g`: `!` excludes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scope {
    pub path: Option<String>,
    pub globs: Vec<String>,
}

impl Scope {
    fn files(&self, ix: &crate::index::Index) -> Result<Files> {
        let root = ix.root.as_path();
        let mut f = crate::files::repository_files(root);
        // the repository's own exclusions come last, where a later glob wins, so no glob of the caller brings them back
        let skip = std::mem::take(&mut f.globs);
        f.globs = self.globs.iter().cloned().chain(skip).collect();
        let path = self.path.as_deref().map(|p| p.trim_start_matches("./")).filter(|p| !p.is_empty() && *p != ".");
        if let Some(p) = path {
            let p = respelled(ix, p);
            let p = unescaped(root, &p);
            if !inside(root, &p) {
                bail!("path outside repo: give a path relative to the repository root, or an absolute one under it");
            }
            // spelled from the root, so the answer names files as without a detour (`../repo/pkg`)
            let p = repo_rel(root, std::path::Path::new(&*p)).map_or(p, std::borrow::Cow::Owned);
            // a link that leaves the repository reads as a missing path, as read_code answers it
            match std::fs::canonicalize(root.join(&*p)) {
                Ok(c) if c.starts_with(root) => f.paths = crate::instant::Paths::Walk(vec![root.join(&*p)]),
                _ => bail!("no such file or directory in the repository: {}", echo(&p)),
            }
            f.paths_as_given = true;
        }
        Ok(f)
    }
}

/// Whether the tool leaves everything as it was (`reindex` rewrites the index, a cache).
pub fn read_only(tool: &str) -> bool {
    tool != "reindex"
}

/// The JSON Schema of what `Answer::structured` returns for one tool: the answer's JSON form in
/// `result`, whole or cut to one page in the same shape, or `null` there for a call that failed.
pub fn output_schema(tool: &str) -> Value {
    let ty = |t: &str| json!({"type": t});
    let obj = |properties: Value, required: &[&str]| json!({"type": "object", "properties": properties, "required": required});
    let list = |items: Value| json!({"type": "array", "items": items});
    // a symbol: where it is, and its name under the key this answer uses
    let span = |name: &str| {
        let mut properties = json!({"id": ty("string"), "path": ty("string"), "start": ty("integer"), "end": ty("integer"), "kind": ty("string")});
        properties[name] = ty("string");
        obj(properties, &["path", "start", "end", "kind", name])
    };
    let result = match tool {
        "search_code" | "find_symbol" | "callers" => list(span("name")),
        "callees" => obj(json!({"resolved": list(span("name")), "external": list(ty("string"))}), &["resolved", "external"]),
        "grep_code" => {
            let line = obj(json!({"line": ty("integer"), "text": ty("string")}), &["line", "text"]);
            let group = obj(
                json!({"symbol": {"anyOf": [span("name"), ty("null")]}, "path": ty("string"), "lines": list(line), "note": ty("string")}),
                &["symbol", "path", "lines"],
            );
            obj(json!({"groups": list(group), "stopped_at": {"type": ["integer", "null"]}}), &["groups", "stopped_at"])
        }
        "read_code" => obj(
            json!({"text": ty("string"), "start": ty("integer"), "end": ty("integer"), "file_lines": {"type": ["integer", "null"]}, "definitions": list(span("qualname"))}),
            &[],
        ),
        "outline" => list(obj(
            json!({"kind": ty("string"), "name": ty("string"), "qualname": ty("string"), "start": ty("integer"), "end": ty("integer")}),
            &["kind", "name", "qualname", "start", "end"],
        )),
        "dependencies" => list(obj(json!({"line": ty("integer"), "module": ty("string"), "resolved": {"type": ["string", "null"]}}), &["line", "module"])),
        "dependents" => list(obj(json!({"path": ty("string"), "line": ty("integer"), "module": ty("string")}), &["path", "line", "module"])),
        "references" => {
            let note = obj(json!({"path": ty("string"), "note": ty("string")}), &["path", "note"]);
            obj(
                json!({"name": ty("string"), "definitions": list(span("qualname")), "truncated": ty("boolean"), "notes": list(note)}),
                &["name", "definitions", "truncated"],
            )
        }
        "similar_code" => {
            let mut hit = span("qualname");
            hit["properties"]["cosine"] = ty("number");
            hit["properties"]["jaccard"] = ty("number");
            list(hit)
        }
        "duplicates" => list(obj(
            json!({"kind": ty("string"), "similarity": ty("number"), "tokens": ty("integer"), "members": ty("array")}),
            &["kind", "similarity", "tokens", "members"],
        )),
        "impact" => {
            let item = |more: &[(&str, Value)]| {
                let mut v = span("qualname");
                for (k, t) in more {
                    v["properties"][*k] = t.clone();
                }
                v["properties"]["start"] = json!({"type": ["integer", "null"]});
                v["properties"]["end"] = json!({"type": ["integer", "null"]});
                v
            };
            let test = obj(json!({"path": ty("string"), "distance": ty("integer"), "via": {"anyOf": [item(&[]), ty("null")]}}), &["path", "distance", "via"]);
            let note = obj(json!({"path": ty("string"), "note": ty("string")}), &["path", "note"]);
            obj(
                json!({
                    "against": ty("string"), "base": ty("string"), "staged": ty("boolean"), "depth": ty("integer"),
                    "changed": list(item(&[("status", json!({"type": "string", "enum": ["added", "edited", "deleted"]}))])),
                    "callers": list(item(&[("distance", ty("integer")), ("via", ty("string")), ("undecided", ty("boolean"))])),
                    "tests": list(test), "notes": list(note), "stopped": ty("boolean"),
                }),
                &["against", "base", "staged", "depth", "changed", "callers", "tests", "notes", "stopped"],
            )
        }
        "find_files" => list(obj(
            json!({"path": ty("string"), "match": {"type": "string", "enum": ["exact", "basename", "path"]}, "edits": ty("integer"), "frecency": ty("number")}),
            &["path", "match", "edits", "frecency"],
        )),
        "structural_search" => list(obj(
            json!({"path": ty("string"), "line": ty("integer"), "column": ty("integer"), "capture": ty("string"), "kind": ty("string"), "text": ty("string")}),
            &["path", "line", "column", "capture", "kind", "text"],
        )),
        "repo_map" => {
            let mut def = span("qualname");
            def["properties"]["calls"] = ty("integer");
            list(obj(
                json!({"path": ty("string"), "rank": ty("integer"), "score": ty("number"), "definitions": list(def)}),
                &["path", "rank", "score", "definitions"],
            ))
        }
        "call_path" => {
            let step = obj(
                json!({"path": ty("string"), "line": ty("integer"), "definition": span("name"), "tied": ty("boolean")}),
                &["path", "line", "definition", "tied"],
            );
            obj(
                json!({"from": span("name"), "to": span("name"), "found": ty("boolean"), "depth": ty("integer"), "stopped": ty("boolean"), "steps": list(step)}),
                &["from", "to", "found", "depth", "stopped", "steps"],
            )
        }
        "dead_code" => {
            let mut why = span("qualname");
            why["properties"]["reason"] = ty("string");
            obj(json!({"dead": list(span("qualname")), "undecided": list(why)}), &["dead", "undecided"])
        }
        _ => {
            obj(json!({"files_changed": ty("integer"), "files_removed": ty("integer"), "chunks": ty("integer")}), &["files_changed", "files_removed", "chunks"])
        }
    };
    let stopped = json!({"type": "integer", "description": "present when the answer stopped at this limit and more exist: the text's last line says which argument raises it"});
    let complete = json!({"type": "boolean", "description": "false when the answer is larger than max_bytes: result then holds the items of this page (null when the call failed)"});
    let page = |d: &str| json!({"type": "integer", "minimum": 0, "description": d});
    let next = json!({"type": ["integer", "null"], "minimum": 0, "description": "the offset to call again with for the next page; null on the last"});
    obj(
        json!({"schema": ty("integer"), "complete": complete, "result": {"anyOf": [result, ty("null")]}, "text": ty("string"), "stopped_at": stopped,
            "offset": page("the first item of this page"), "next_offset": next, "total": page("the items of the whole answer")}),
        &["schema", "complete", "result"],
    )
}

/// An answer's JSON form in the envelope every front end prints.
pub fn envelope(mut result: Value, stopped: Option<Stop>) -> Value {
    mark_vendored(&mut result);
    let mut v = json!({"schema": ANSWER_SCHEMA, "result": result});
    if let Some(stop) = stopped {
        v["stopped_at"] = json!(stop.at);
    }
    v
}

/// Every item of an answer with a `path` of vendored code (`text::is_vendored_path`) gains
/// `"vendored": true`: the one place, so no answer can leave it out.
fn mark_vendored(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if m.get("path").and_then(Value::as_str).is_some_and(crate::text::is_vendored_path) {
                m.insert("vendored".into(), Value::Bool(true));
            }
            m.values_mut().for_each(mark_vendored);
        }
        Value::Array(a) => a.iter_mut().for_each(mark_vendored),
        _ => {}
    }
}

/// Where an answer stopped because it reached its limit, and what to pass to see more.
#[derive(Clone, Copy, Debug)]
pub struct Stop {
    pub at: usize,
    pub how: &'static str,
}

pub const RAISE_LIMIT: &str = "raise limit to see more";

/// `items` cut to `limit`, and where they stopped when there were more (ask for `limit + 1`).
fn cut<T>(mut items: Vec<T>, limit: usize, how: &'static str) -> (Vec<T>, Option<Stop>) {
    if items.len() <= limit {
        return (items, None);
    }
    items.truncate(limit);
    (items, Some(Stop { at: limit, how }))
}

pub enum Answer {
    /// `stopped`: the limit the answer stopped at, when there were more.
    Hits {
        hits: Vec<Hit>,
        words: Vec<String>,
        snippet: bool,
        stopped: Option<Stop>,
    },
    /// `undecided`: callers kept although the resolver could not tell whether they call this
    /// definition or another of the same name.
    Callers {
        hits: Vec<Hit>,
        undecided: usize,
        stopped: Option<Stop>,
    },
    /// `from`: the files of the definitions whose calls these are.
    Callees {
        resolved: Vec<Hit>,
        external: Vec<String>,
        from: Vec<String>,
        stopped: Option<Stop>,
    },
    /// `stopped_at`: the limit the search stopped at, when it did.
    Grep {
        groups: Vec<RefGroup>,
        stopped_at: Option<u64>,
    },
    /// Numbered lines of `path` from line `start` on, and how many lines the file has (None
    /// for a large file read only up to the lines asked for: more lines follow).
    /// `defs`: the definitions the index has in the file, which the concise form lists.
    Read {
        path: String,
        lines: Vec<String>,
        start: i64,
        total: Option<i64>,
        defs: Vec<crate::index::Symbol>,
    },
    /// Whole definitions, each with its numbered lines and the definitions inside it.
    Definitions(Vec<(ChunkRow, String, Vec<crate::index::Symbol>)>, Option<Stop>),
    Outline {
        path: String,
        rows: Vec<ChunkRow>,
    },
    Deps {
        path: String,
        deps: Vec<Dependency>,
    },
    /// `unresolved`: imports that resolved to no file and may name `target`.
    Rdeps {
        target: String,
        rows: Vec<(String, i64, String)>,
        unresolved: usize,
    },
    Refs {
        name: String,
        refs: References,
        max_results: u64,
    },
    /// `only`: the one definition asked for with `def`.
    Resolved {
        name: String,
        refs: Resolved,
        only: Option<i64>,
        max_results: u64,
    },
    Similar(Vec<(Hit, f32, f32)>),
    Dupes(Vec<DupGroup>, Option<Stop>),
    Captures(Vec<Capture>, Option<Stop>),
    Map(Vec<crate::repomap::MapFile>),
    Path(Box<crate::names::CallPath>),
    Dead(crate::dead::Dead),
    Reindexed(UpdateStats),
    Impact(crate::impact::Impact),
    Files(Vec<crate::finder::FileHit>, Option<Stop>),
}

/// The `files` answer, which reads no index: a front end runs it without opening one.
pub fn files(root: &std::path::Path, query: &str, limit: usize) -> Result<Answer> {
    let (hits, more) = crate::finder::find(root, query, limit)?;
    Ok(Answer::Files(hits, more.then_some(Stop { at: limit, how: RAISE_LIMIT })))
}

/// Run one operation on the index as it is; `Repo::run` brings the index up to date first. Every
/// read of it comes from one commit: a writer's commit meanwhile shows in the next operation.
pub fn run(s: &mut Searcher, op: &Op) -> Result<Answer> {
    in_snapshot(s, |s| run_in(s, op))
}

/// `f` on `s` with every read of the code index from one commit (`Index::begin_read`).
pub fn in_snapshot<T>(s: &mut Searcher, f: impl FnOnce(&mut Searcher) -> Result<T>) -> Result<T> {
    let snap = Snapshot::begin(s)?;
    f(&mut *snap.0)
}

/// A read snapshot of the code index while it lives, ended on every way out (an error or a panic
/// too); none when a transaction is open already (an update).
struct Snapshot<'s>(&'s mut Searcher, bool);

impl<'s> Snapshot<'s> {
    fn begin(s: &'s mut Searcher) -> Result<Snapshot<'s>> {
        let own = s.ix.begin_read()?;
        Ok(Snapshot(s, own))
    }
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        if self.1 {
            self.0.ix.end_read();
        }
    }
}

fn run_in(s: &mut Searcher, op: &Op) -> Result<Answer> {
    Ok(match op {
        Op::Search { query, alt, k, snippets } => {
            let mut qs = vec![query.clone()];
            qs.extend(alt.iter().filter(|a| !a.is_empty() && *a != query).cloned());
            let hits = if qs.len() == 1 { s.search(query, *k, *snippets)? } else { s.search_many(&qs, *k, *snippets)? };
            Answer::Hits { hits, words: qs.iter().flat_map(|q| query_words(q)).collect(), snippet: true, stopped: None }
        }
        Op::Grep { pattern, scope, max_results } => {
            let q = crate::instant::Query {
                pattern: pattern.clone(),
                max_results: Some(*max_results),
                // a capped answer is the same on every call, so paging does not repeat or skip lines
                first_in_path_order: true,
                ..Default::default()
            };
            let files = scope.files(&s.ix)?;
            let (groups, stopped) = crate::intel::grouped_grep(s, &q, &files)?;
            Answer::Grep { groups, stopped_at: stopped.then_some(*max_results) }
        }
        Op::Symbol { name, limit } => {
            let (hits, stopped) = cut(targets(s, name, limit.saturating_add(1), false)?, *limit, RAISE_LIMIT);
            Answer::Hits { hits, words: Vec::new(), snippet: true, stopped }
        }
        Op::Callers { name, limit } => {
            let (hits, mut undecided) = Names::new(s)?.callers(name, limit.saturating_add(1))?;
            let (hits, stopped) = cut(hits, *limit, RAISE_LIMIT);
            // the undecided ones come last, so the one cut off was one of them
            if stopped.is_some() {
                undecided = undecided.saturating_sub(1);
            }
            Answer::Callers { hits, undecided, stopped }
        }
        Op::Callees { name, limit } => {
            let (resolved, external, from) = Names::new(s)?.callees(name, limit.saturating_add(1))?;
            let (resolved, stopped) = cut(resolved, *limit, RAISE_LIMIT);
            Answer::Callees { resolved, external, from, stopped }
        }
        Op::Read { path, symbol: Some(symbol), .. } => {
            let mut rows = match crate::intel::location(symbol) {
                Some((path, line)) => s.ix.chunk_at(&repo_path(s, path), line)?.into_iter().collect(),
                None => {
                    let hits = s.find_symbol(symbol, READ_DEFINITIONS + 1)?;
                    hits.iter().filter_map(|h| s.ix.chunk(h.id).transpose()).collect::<Result<Vec<_>>>()?
                }
            };
            if let Some(path) = path {
                let path = repo_path(s, path);
                rows.retain(|r| r.path == path);
            }
            if rows.is_empty() {
                bail!("no definition matches {}", echo(symbol));
            }
            let read = |r: ChunkRow| -> Result<(ChunkRow, String, Vec<crate::index::Symbol>)> {
                let mut text = read_range(s, &r.path, r.start, r.end.min(r.start.saturating_add(READ_LINES - 1)))?.0.join("\n");
                if r.end - r.start + 1 > READ_LINES {
                    text.push_str(&format!("\n(the definition goes on to line {}: read_code path={} start={})", r.end, r.path, r.start + READ_LINES));
                }
                let mut inner = s.ix.symbols_in(&r.path).unwrap_or_default();
                inner.retain(|m| m.id != r.id && r.start <= m.start && m.end <= r.end);
                Ok((r, text, inner))
            };
            let (rows, stopped) = cut(rows, READ_DEFINITIONS, "pass path to keep the ones of one file, or a location (path:start-end) for one");
            let read = rows.into_iter().map(read).collect::<Result<Vec<_>>>()?;
            crate::finder::visited(
                &s.ix.root,
                &read.iter().map(|(r, ..)| r.path.as_str()).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>(),
            );
            Answer::Definitions(read, stopped)
        }
        Op::Read { path, start, end, .. } => {
            let path = path.as_deref().unwrap_or("");
            // a path copied from an answer has its control characters written as escapes
            let read_back = crate::fmt::unshown(path);
            // a path that leaves the repository by its own parts is not looked at
            let exists = |p: &str| inside(&s.ix.root, p) && s.ix.root.join(p).exists();
            let printed = read_back != path && crate::fmt::shown(&read_back) == path && exists(&read_back);
            let path = if printed || (read_back != path && !exists(path)) { read_back.as_ref() } else { path };
            let start = (*start).max(1);
            let (lines, total) = read_range(s, path, start, end.unwrap_or(start.saturating_add(READ_LINES - 1)))?;
            let path = path.trim_start_matches("./").to_string();
            crate::finder::visited(&s.ix.root, &[&path]);
            let last = start + lines.len() as i64 - 1;
            let mut defs = s.ix.symbols_in(&path).unwrap_or_default();
            defs.retain(|d| start <= d.start && d.start <= last);
            Answer::Read { path, lines, start, total, defs }
        }
        Op::Outline { path } => {
            let path = repo_path(s, path);
            Answer::Outline { rows: s.ix.outline(&path)?, path }
        }
        Op::Deps { path } => {
            let path = repo_path(s, path);
            Answer::Deps { deps: Names::new(s)?.imports(&path)?, path }
        }
        Op::Rdeps { target } => {
            let target = repo_path(s, target);
            let (rows, unresolved) = Names::new(s)?.importers(&target)?;
            Answer::Rdeps { rows, target, unresolved }
        }
        Op::Refs { name, max_results, textual: true, .. } => {
            let name = &name.trim().to_string();
            Answer::Refs { name: name.clone(), refs: crate::intel::references(s, name, *max_results)?, max_results: *max_results }
        }
        Op::Refs { name, max_results, def, .. } => {
            let name = &name.trim().to_string();
            let refs = Names::new(s)?.references(name, *max_results)?;
            let only = match def {
                Some(spec) => Some(definition_at(s, &refs, name, spec)?),
                None => None,
            };
            Answer::Resolved { name: name.clone(), refs, only, max_results: *max_results }
        }
        Op::Similar { target, k } => Answer::Similar(crate::intel::similar(s, target, *k)?),
        Op::Dupes { min_tokens, threshold, limit } => {
            let (groups, stopped) = cut(crate::intel::dupes(&s.ix, *min_tokens, *threshold, limit.saturating_add(1))?, *limit, RAISE_LIMIT);
            Answer::Dupes(groups, stopped)
        }
        Op::Tsquery { lang, query, limit } => {
            let caps = crate::intel::tsquery(
                lang,
                query,
                std::slice::from_ref(&s.ix.root),
                limit.saturating_add(1),
                Some(&s.ix.root),
                crate::intel::Watch { count: &s.ix.count, cancel: &s.cancel },
            )?;
            let (caps, stopped) = cut(caps, *limit, RAISE_LIMIT);
            Answer::Captures(caps, stopped)
        }
        Op::Map { path } => {
            let path = path.as_deref().map(|p| repo_path(s, p)).filter(|p| !p.is_empty() && p != ".");
            existing(s, path.as_deref())?;
            Answer::Map(crate::repomap::map(s, path.as_deref())?)
        }
        Op::Path { from, to, depth } => Answer::Path(Box::new(Names::new(s)?.path(from, to, *depth)?)),
        Op::Dead { path } => {
            let path = path.as_deref().map(|p| repo_path(s, p)).filter(|p| !p.is_empty() && p != ".");
            existing(s, path.as_deref())?;
            Answer::Dead(Names::new(s)?.dead(path.as_deref())?)
        }
        Op::Reindex { .. } => bail!("reindex keeps every store current: run it through Repo"),
        Op::Impact { base, staged, depth } => Answer::Impact(crate::impact::impact(s, base.as_deref(), *staged, *depth)?),
        Op::Files { query, limit } => files(&s.ix.root, query, *limit)?,
    })
}

/// `p` with the escapes an answer writes read back (`a\tb` as a tab), when that names a file or
/// directory and prints as `p` again; else `p` as written (a directory named `x\ty`).
fn unescaped<'p>(root: &std::path::Path, p: &'p str) -> std::borrow::Cow<'p, str> {
    let back = crate::fmt::unshown(p);
    if back != p && crate::fmt::shown(&back) == p && root.join(&*back).exists() {
        back
    } else {
        std::borrow::Cow::Borrowed(p)
    }
}

/// `path` with the root as the index was given it (`<dir>/link`, macOS's `/tmp/x`) spelled as the
/// index's own root; a look at its text only, as for every path outside.
fn respelled<'p>(ix: &crate::index::Index, path: &'p str) -> std::borrow::Cow<'p, str> {
    let p = std::path::Path::new(path);
    if ix.given == ix.root || !p.is_absolute() {
        return std::borrow::Cow::Borrowed(path);
    }
    match p.strip_prefix(&ix.given) {
        Ok(rest) => std::borrow::Cow::Owned(ix.root.join(rest).to_string_lossy().into_owned()),
        Err(_) => std::borrow::Cow::Borrowed(path),
    }
}

/// An error unless `path` (from the root, or none) is a file or directory of the repository.
fn existing(s: &Searcher, path: Option<&str>) -> Result<()> {
    match path {
        Some(p) if !inside(&s.ix.root, p) || !s.ix.root.join(p).exists() => bail!("no such file or directory in the repository: {}", echo(p)),
        _ => Ok(()),
    }
}

/// Whether `root` joined with `path` stays under `root` by their parts alone (`.` dropped and
/// `..` taking a part away), with no file looked at.
fn inside(root: &std::path::Path, path: &str) -> bool {
    let mut out = std::path::PathBuf::new();
    for c in root.join(path).components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    if cfg!(windows) {
        under(&out.to_string_lossy(), &root.to_string_lossy())
    } else {
        out.starts_with(root)
    }
}

/// On Windows one folder has several spellings (`\\?\C:\x` from `canonicalize`, `C:\x`, `c:/X`)
/// and the prefix parts of two paths compare by kind, so the two are compared as text.
fn under(path: &str, root: &str) -> bool {
    fn plain(p: &str) -> String {
        let s = p.replace('\\', "/");
        let s = match s.strip_prefix("//?/") {
            Some(rest) if rest.get(..4).is_some_and(|h| h.eq_ignore_ascii_case("UNC/")) => format!("//{}", &rest[4..]),
            Some(rest) => rest.to_string(),
            None => s,
        };
        s.to_lowercase()
    }
    let (path, root) = (plain(path), plain(root));
    path.strip_prefix(root.trim_end_matches('/')).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Lines `start..=end` of a repository file, numbered, read only as far as they go and at most
/// `READ_BYTES` of them; and how many lines the file has, when the read reached its end or the
/// file is small enough to count them.
pub fn read_range(s: &Searcher, path: &str, start: i64, end: i64) -> Result<(Vec<String>, Option<i64>)> {
    use std::io::BufRead;
    let shown = echo(path);
    let path = &*respelled(&s.ix, path);
    // the answer for a path outside the repository must not tell whether a file is there: one
    // that leaves by its own parts is outside, with nothing looked at (on Windows a share on
    // another machine would be connected to, and offered the user's login), and a link that
    // leaves reads as a missing file
    if !inside(&s.ix.root, path) {
        bail!("path outside repo: give a path relative to the repository root, or an absolute one under it");
    }
    let canon = match std::fs::canonicalize(s.ix.root.join(path)) {
        Ok(c) if c.starts_with(&s.ix.root) => c,
        _ => bail!("cannot read {shown}"),
    };
    if !canon.is_file() {
        bail!("{shown} is not a regular file");
    }
    let file = std::fs::File::open(&canon).map_err(|_| anyhow!("cannot read {shown}"))?;
    let small = file.metadata()?.len() <= crate::index::MAX_FILE_BYTES;
    let mut r = std::io::BufReader::with_capacity(64 * 1024, file);
    // a NUL byte in the first 8 KB, the test grep tools use
    if r.fill_buf().map(|b| b[..b.len().min(8192)].contains(&0))? {
        bail!("{shown} is not a text file (it holds a NUL byte): read_code shows text only");
    }
    // one line into `line`, up to its first `keep` bytes, where the read stops when `keep` is
    // not 0 -> (anything read, the line goes on past what was read)
    let mut next_line = |line: &mut Vec<u8>, keep: usize| -> std::io::Result<(bool, bool)> {
        let mut any = false;
        loop {
            let buf = r.fill_buf()?;
            if buf.is_empty() {
                return Ok((any, false));
            }
            any = true;
            let (body, done) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (i, i + 1),
                None => (buf.len(), buf.len()),
            };
            let room = keep.saturating_sub(line.len());
            if keep > 0 && body > room {
                line.extend_from_slice(&buf[..room]);
                r.consume(room);
                return Ok((any, true));
            }
            line.extend_from_slice(&buf[..body.min(room)]);
            let newline = done > body;
            r.consume(done);
            if newline {
                return Ok((any, false));
            }
        }
    };
    let (mut out, mut used, mut n, mut line, mut mid_line) = (Vec::new(), 0usize, 0i64, Vec::new(), false);
    loop {
        if n >= end || (n + 1 >= start && used >= READ_BYTES) {
            break;
        }
        let wanted = n + 1 >= start;
        line.clear();
        let (any, cut) = next_line(&mut line, if wanted { READ_BYTES - used } else { 0 })?;
        if !any {
            return Ok((out, Some(n)));
        }
        n += 1;
        if wanted {
            if !cut && line.last() == Some(&b'\r') {
                line.pop();
            }
            used += line.len().max(1);
            let mut text = format!("{n:>5}  {}", String::from_utf8_lossy(&line));
            if cut {
                text.push_str(&format!("  (line cut at {} bytes)", line.len()));
                mid_line = true;
            }
            out.push(text);
        }
    }
    if !small {
        return Ok((out, (!mid_line && r.fill_buf()?.is_empty()).then_some(n)));
    }
    if mid_line {
        next_line(&mut line, 0)?;
    }
    loop {
        line.clear();
        if !next_line(&mut line, 0)?.0 {
            return Ok((out, Some(n)));
        }
        n += 1;
    }
}

/// One line for a structural match: where, the capture, and its text (the first line of the
/// node, with control characters written as escapes).
pub fn capture_line(c: &Capture) -> String {
    format!("{}:{}:{}  @{} ({})  {}", shown(&c.path), c.line, c.column, c.capture, c.kind, shown(&c.text))
}

/// `path` from the root of `root` with `.`, `..` and empty parts gone: a path from the root, or
/// an absolute one under it (as written, else once its links are resolved). None when it leaves
/// the root, by its parts or by being elsewhere.
pub fn repo_rel(root: &std::path::Path, path: &std::path::Path) -> Option<String> {
    use std::path::Component;
    let lexical = |p: &std::path::Path| {
        let mut parts: Vec<String> = Vec::new();
        for c in p.components() {
            match c {
                Component::Normal(n) => parts.push(n.to_string_lossy().into_owned()),
                Component::ParentDir => {
                    parts.pop()?;
                }
                _ => {}
            }
        }
        Some(parts.join("/"))
    };
    if !path.is_absolute() {
        // one that leaves the root and comes back by its parts (`../repo/pkg`) is resolved from
        // the root, as `inside` reads it, with no file looked at
        return lexical(path).or_else(|| inside(root, &path.to_string_lossy()).then(|| repo_rel(root, &root.join(path))).flatten());
    }
    if inside(root, &path.to_string_lossy()) {
        // `inside` read the `..` parts already; what is left starts with the root's own parts
        let rest = lexical(path)?;
        let top = lexical(root)?;
        if let Some(r) = rest.strip_prefix(&top).filter(|r| r.is_empty() || r.starts_with('/')) {
            return Some(r.trim_start_matches('/').to_string());
        }
    }
    let real = std::fs::canonicalize(path).ok()?;
    let top = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    real.strip_prefix(&top).ok().map(|r| crate::instant::slashed(r.to_string_lossy()))
}

/// `path` as the index spells it, when it names a repository file in a spelling `read_code`
/// accepts (`./a.py`, `pkg/../a.py`, `<root>/a.py`, another case on a case-insensitive file
/// system); else as given.
pub fn repo_path(s: &Searcher, given: &str) -> String {
    let path = &*respelled(&s.ix, given);
    // a path that leaves the repository names no file of it (`/a.py` is not `a.py`), and is not looked at
    let Some(plain) = inside(&s.ix.root, path).then(|| repo_rel(&s.ix.root, std::path::Path::new(path))).flatten() else {
        return given.to_string();
    };
    let indexed = |p: &str| s.ix.has_file(p);
    // a path copied from an answer has its control characters and backslashes written as
    // escapes; the text an answer prints for a file names that file, whatever else exists
    let read_back = crate::fmt::unshown(&plain);
    let escaped = read_back != plain;
    if escaped && crate::fmt::shown(&read_back) == plain && indexed(&read_back) {
        return read_back.into_owned();
    }
    if path.is_empty() || indexed(&plain) {
        return plain;
    }
    if escaped && indexed(&read_back) {
        return read_back.into_owned();
    }
    // another spelling of a file of the repository
    std::fs::canonicalize(s.ix.root.join(path))
        .ok()
        .filter(|p| p.is_file())
        .and_then(|p| p.strip_prefix(&s.ix.root).ok().map(|r| crate::instant::slashed(r.to_string_lossy())))
        .filter(|p| indexed(p))
        .unwrap_or(plain)
}

/// The definitions `name` stands for: the one at a location (the `path:start-end` an earlier
/// answer printed), else every one `find_symbol` finds. With `windows`, a line outside every
/// definition stands for the first window that holds it, which holds that line's calls.
pub fn targets(s: &Searcher, name: &str, limit: usize, windows: bool) -> Result<Vec<Hit>> {
    let Some((path, line)) = crate::intel::location(name) else { return s.find_symbol(name, limit) };
    let path = repo_path(s, path);
    let mut at = s.ix.chunk_at(&path, line)?;
    if windows && at.as_ref().is_some_and(|c| c.kind == "window") {
        at = s.ix.window_at(&path, line)?;
    }
    match at {
        Some(c) if windows || c.kind != "window" => Ok(vec![Hit::from_row(&c, true)]),
        // a search result can be a window: say what the location is, and which commands take it
        Some(_) => bail!("no symbol at {}: these lines are outside every definition (callees and similar take such a location)", echo(name)),
        None => bail!("no symbol at {}", echo(name)),
    }
}

/// The definition of `name` that `spec` (a path or a location) points at: the innermost one
/// around the line, or the first in the file.
fn definition_at(s: &Searcher, refs: &Resolved, name: &str, spec: &str) -> Result<i64> {
    let (path, line) = match crate::intel::location(spec) {
        Some((path, line)) => (repo_path(s, path), Some(line)),
        None => (repo_path(s, spec), None),
    };
    refs.definitions
        .iter()
        .filter(|d| d.path == path && line.is_none_or(|l| d.start <= l && l <= d.end))
        .min_by_key(|d| (line.is_some().then_some(d.end - d.start), d.start))
        .map(|d| d.id)
        .ok_or_else(|| anyhow!("no definition of {} in {}", echo(name), echo(spec)))
}

/// The closing line of an answer that stopped at its limit.
pub fn stop_line(stopped: Option<Stop>, noun: &str) -> String {
    stopped.map(|s| format!("(stopped at {} {noun}: {})", s.at, s.how)).unwrap_or_default()
}

/// Two closing lines, either of them possibly empty.
fn joined(a: String, b: String) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b,
        (_, true) => a,
        _ => format!("{a}\n{b}"),
    }
}

/// The closing line of a `references` answer whose occurrence search stopped at `max_results`.
fn stopped_at(truncated: bool, max_results: u64) -> String {
    if truncated {
        format!("(the search stopped at max_results={max_results} occurrences, so these counts cover only those: raise max_results to see all)")
    } else {
        String::new()
    }
}

/// A group's JSON with its note on the file, when it has one.
fn with_note(mut v: Value, g: &RefGroup) -> Value {
    if let Some(note) = &g.note {
        v["note"] = note.as_str().into();
    }
    v
}

/// A definition as one line: `path:start-end  [kind] name`.
fn hit_line(h: &Hit) -> String {
    hit_blocks(std::slice::from_ref(h), &[], false, false).remove(0)
}

fn symbol_name(c: &ChunkRow) -> &str {
    if c.qualname.is_empty() {
        &c.name
    } else {
        &c.qualname
    }
}

fn sym_name(s: &crate::index::Symbol) -> &str {
    if s.qualname.is_empty() {
        &s.name
    } else {
        &s.qualname
    }
}

fn hit_name(h: &Hit) -> &str {
    if h.qualname.is_empty() {
        &h.name
    } else {
        &h.qualname
    }
}

/// A definition's own name, without the names it is inside (`Answer.text` is `text`), as text shows it.
fn named(qualname: &str) -> std::borrow::Cow<'_, str> {
    shown(qualname.rsplit('.').next().filter(|n| !n.is_empty()).unwrap_or(qualname))
}

/// The code of a numbered line of `read_range`, without its number and indentation.
fn code_of(line: &str) -> &str {
    line.trim_start().split_once("  ").map_or("", |(_, code)| code).trim()
}

/// A group of matching lines in concise form: where its symbol is and the numbers of the lines.
fn group_brief(g: &RefGroup, indent: &str) -> Vec<Brief> {
    let lines = g.lines.iter().map(|h| h.line.to_string()).collect::<Vec<_>>().join(" ");
    let mut out = vec![match &g.symbol {
        Some(c) => Brief::text(indent).then_at(&c.path, c.start, c.end).then(format!(" {}: {lines}", named(symbol_name(c)))),
        None => Brief::text(indent).then_path(&g.path).then(format!(": {lines}")),
    }];
    out.extend(g.note.as_ref().map(|note| Brief::text(format!("{indent}  note: {note}"))));
    out
}

/// The concise lines of `intel::references_render`, one entry per line of it.
fn refs_brief(r: &References) -> Vec<Vec<Brief>> {
    let mut out = Vec::new();
    let mut section = |title: &str, defs: Vec<Brief>| {
        if !defs.is_empty() {
            out.push(vec![Brief::text(title)]);
            out.extend(defs.into_iter().map(|d| vec![d]));
        }
    };
    let at = |path: &str, start: i64, end: i64, name: &str| Brief::text("  ").then_at(path, start, end).then(format!(" {}", named(name)));
    section("definitions:", r.definitions.iter().map(|d| at(&d.path, d.start, d.end, &d.qualname)).collect());
    section("callers:", r.callers.iter().map(|h| at(&h.path, h.start, h.end, &h.qualname)).collect());
    out.push(vec![Brief::text(format!("references ({} lines in {} symbols):", r.total_lines(), r.groups.len()))]);
    for g in &r.groups {
        let mut lines = group_brief(g, "  ").into_iter();
        out.push(lines.next().into_iter().collect());
        out.extend(g.lines.iter().map(|_| Vec::new()));
        out.extend(lines.map(|note| vec![note]));
    }
    out
}

/// The concise lines of `resolve::resolved_text`, one entry per line of it.
fn resolved_brief(r: &Resolved, only: Option<i64>) -> Vec<Vec<Brief>> {
    let occurrence = |o: &crate::resolve::Occurrence| vec![Brief::text("  ").then_line(&o.path, format!(":{}:{}", o.line, o.column))];
    let mut out = Vec::new();
    for d in r.definitions.iter().filter(|d| only.is_none_or(|id| d.id == id)) {
        let occ: Vec<_> = r.of(d.id).collect();
        out.push(vec![Brief::at(&d.path, d.start, d.end, &format!("{} ({} references)", named(&d.qualname), occ.len()))]);
        out.extend(occ.into_iter().map(occurrence));
    }
    let unresolved: Vec<_> = r.unresolved().collect();
    match only {
        Some(id) => {
            let elsewhere = r.occurrences.len() - unresolved.len() - r.of(id).count();
            out.push(vec![Brief::text(format!("({elsewhere} other occurrences resolve to other definitions, {} are unresolved)", unresolved.len()))]);
        }
        None => {
            out.push(vec![Brief::text(format!("unresolved ({}):", unresolved.len()))]);
            out.extend(unresolved.into_iter().map(occurrence));
        }
    }
    out.extend(r.notes.iter().map(|(path, note)| vec![Brief::text("note: ").then_path(path).then(format!(": {note}"))]));
    out
}

/// The concise lines of `Impact::lines`, one entry per line of it.
fn impact_brief(i: &crate::impact::Impact) -> Vec<Vec<Brief>> {
    let symbol = |h: &Hit, mark: &str| {
        if h.kind == "file" {
            Brief::path(&h.path).then(format!(" [file]{mark}"))
        } else {
            Brief::at(&h.path, h.start, h.end, &format!("{}{mark}", named(hit_name(h))))
        }
    };
    let mut out = vec![vec![Brief::text(format!("changed: {}", i.changed.len()))]];
    out.extend(i.changed.iter().map(|c| vec![symbol(&c.hit, &format!(" ({})", c.status))]));
    out.push(vec![Brief::text(format!("callers: {} (depth {})", i.callers.len(), i.depth))]);
    out.extend(i.callers.iter().map(|c| {
        let mark = if c.calls_deleted {
            " (calls deleted)"
        } else if c.undecided {
            "?"
        } else {
            ""
        };
        vec![Brief::text(format!("{} ", c.distance)).and(symbol(&c.hit, mark))]
    }));
    out.push(vec![Brief::text(format!("tests: {}", i.tests.len()))]);
    out.extend(i.tests.iter().map(|t| {
        vec![match &t.via {
            // the definition that reaches the change names the test file when it is in it
            Some(h) if h.path == t.path && h.kind != "file" => symbol(h, &format!(" (test, {})", t.distance)),
            Some(h) => Brief::path(&t.path).then(format!(" ({} via ", t.distance)).and(symbol(h, ")")),
            None => Brief::path(&t.path).then(" (changed)"),
        }]
    }));
    if !i.notes.is_empty() {
        out.push(vec![Brief::text(format!("notes: {}", i.notes.len()))]);
        out.extend(i.notes.iter().map(|(p, n)| vec![Brief::path(p).then(format!(" {n}"))]));
    }
    out
}

fn chunk_json(c: &ChunkRow) -> Value {
    json!({"id": format!("{}:{}-{}", c.path, c.start, c.end), "path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "qualname": c.qualname})
}

/// How an answer's text is written. `Concise` keeps every location and name and drops what an
/// agent can ask for again: code, snippets, scores, kinds and explanations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Form {
    #[default]
    Detailed,
    Concise,
}

impl Form {
    /// The `response_format` argument of a tool call; left out or null is `detailed`.
    pub fn from_tool(args: &Value) -> Result<Form> {
        match args.get("response_format").filter(|v| !v.is_null()) {
            None => Ok(Form::Detailed),
            Some(v) => match v.as_str() {
                Some("detailed") => Ok(Form::Detailed),
                Some("concise") => Ok(Form::Concise),
                _ => bail!("response_format must be \"detailed\" or \"concise\""),
            },
        }
    }
}

/// One line of a concise answer: text, paths and locations. A line that ends with `/` names a
/// directory, and a path without `/` after it is in that directory (`./` is the root); a
/// location without a path (`:start-end`, `:line`, `:line:column`) is in the file of the location
/// before it. A directory
/// line comes before a line whose first path is in another directory; another path of that line
/// in another directory is written whole, and a file at the root as `./name`.
#[derive(Default)]
struct Brief(Vec<Piece>);

enum Piece {
    Text(String),
    Path(String),
    /// a path and what follows it: `:start-end`, `:line` or `:line:column`
    At(String, String),
}

/// The directory of the last directory line and the file of the last location of a page.
#[derive(Default)]
struct Written {
    dir: String,
    file: String,
}

impl Brief {
    fn text(text: impl Into<String>) -> Brief {
        Brief(vec![Piece::Text(text.into())])
    }
    fn path(path: &str) -> Brief {
        Brief::default().then_path(path)
    }
    fn then(mut self, text: impl Into<String>) -> Brief {
        self.0.push(Piece::Text(text.into()));
        self
    }
    fn and(mut self, more: Brief) -> Brief {
        self.0.extend(more.0);
        self
    }
    fn then_path(mut self, path: &str) -> Brief {
        self.0.push(Piece::Path(shown(path).into_owned()));
        self
    }
    fn then_at(self, path: &str, start: i64, end: i64) -> Brief {
        self.then_line(path, format!(":{start}-{end}"))
    }
    fn then_line(mut self, path: &str, place: String) -> Brief {
        self.0.push(Piece::At(shown(path).into_owned(), place));
        self
    }
    /// `path:start-end rest`
    fn at(path: &str, start: i64, end: i64, rest: &str) -> Brief {
        Brief::default().then_at(path, start, end).then(format!(" {rest}"))
    }
    /// The line after `out`, below a directory line when its first path is not in the last one.
    fn write(&self, page: &mut Written, out: &mut String) {
        let first = self.0.iter().find_map(|p| match p {
            Piece::Path(path) => Some(path),
            Piece::At(path, ..) if *path != page.file => Some(path),
            _ => None,
        });
        if let Some((d, _)) = first.map(|f| split_dir(f)).filter(|(d, _)| *d != page.dir) {
            push_line(out, &if d.is_empty() { "./".to_string() } else { format!("{d}/") });
            page.dir = d.to_string();
        }
        let mut line = String::new();
        let path = |line: &mut String, page: &Written, path: &str| match split_dir(path) {
            (d, name) if d == page.dir => line.push_str(name),
            ("", _) => line.push_str(&format!("./{path}")),
            _ => line.push_str(path),
        };
        for piece in &self.0 {
            match piece {
                Piece::Text(text) => line.push_str(text),
                Piece::Path(p) => path(&mut line, page, p),
                Piece::At(p, place) => {
                    if *p != page.file {
                        path(&mut line, page, p);
                        page.file = p.clone();
                    }
                    line.push_str(place);
                }
            }
        }
        push_line(out, &line);
    }
}

/// A path's directory (empty at the root) and file name.
fn split_dir(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// An answer as text: a first line, the items that can be counted and cut, and a last line.
#[derive(Default)]
struct Parts {
    head: String,
    items: Vec<String>,
    tail: String,
    noun: &'static str,
    /// items after `items` that were counted and not kept
    rest: usize,
    /// the items are lines of a file: its path, the number of the first, and the file's length
    /// (None: unknown, more lines follow)
    file: Option<(String, i64, Option<i64>)>,
    /// the concise form: the lines of each item (none hides it), and a last line in place of
    /// `tail`; without it the concise text is the detailed one
    brief: Option<Vec<Vec<Brief>>>,
    brief_tail: Option<String>,
}

const CUT_ITEM: &str = "(this item is cut: pass a larger max_bytes to see all of it)";

/// `items` (one result each, possibly of several lines) as `Answer::text_within` pages them.
pub fn paged(items: Vec<String>, noun: &'static str, max_bytes: usize, offset: usize) -> String {
    paged_counted(items, 0, noun, max_bytes, offset)
}

/// `paged` for a caller that kept the items up to what can be shown and counted the `rest`.
pub fn paged_counted(items: Vec<String>, rest: usize, noun: &'static str, max_bytes: usize, offset: usize) -> String {
    Parts { items, rest, noun, ..Parts::default() }.within(max_bytes, offset, Form::Detailed).0
}

impl Parts {
    /// The page from item `offset` on that fits `max_bytes`, and how many items it shows. A
    /// concise page holds the items the detailed one holds, so an offset means the same items in
    /// both forms.
    fn within(&self, max_bytes: usize, offset: usize, form: Form) -> (String, usize) {
        let mut room = max_bytes;
        loop {
            let (out, shown, cut) = self.render(max_bytes, room, offset);
            if out.len() <= max_bytes || room == 0 || (shown <= 1 && !cut) {
                // the concise text of the same items when it fits, else the detailed page, which
                // says how to go on
                let text = match &self.brief {
                    Some(brief) if form == Form::Concise => Some(self.render_brief(brief, offset, shown)).filter(|b| b.len() <= max_bytes).unwrap_or(out),
                    _ => out,
                };
                return (text, shown);
            }
            room = room.saturating_sub(out.len() - max_bytes);
        }
    }

    fn render_brief(&self, brief: &[Vec<Brief>], offset: usize, count: usize) -> String {
        let mut out = self.head.clone();
        let mut page = Written::default();
        let lines: Vec<&Brief> = brief.iter().skip(offset).take(count).flatten().collect();
        let any = !lines.is_empty();
        for line in lines {
            line.write(&mut page, &mut out);
        }
        if let (false, Some((_, first, _))) = (any, &self.file) {
            let last = first + (offset + count) as i64 - 1;
            push_line(&mut out, &format!("(no definition starts in lines {}-{last}: response_format detailed shows them)", first + offset as i64));
        }
        self.close(&mut out, offset, count, self.brief_tail.as_deref().unwrap_or(&self.tail));
        out
    }

    /// The closing lines of a page of `count` items from `offset`: how to see more, or the last line.
    fn close(&self, out: &mut String, offset: usize, count: usize, tail: &str) {
        let all = self.items.len() + self.rest;
        let next = offset.min(all) + count;
        if let Some((path, first, total)) = &self.file {
            let last = first + next as i64 - 1;
            if count > 0 && total.is_none_or(|t| last < t) {
                let path = shown(path);
                let of = match total {
                    Some(t) => format!("of {t} in {path}"),
                    None => format!("of {path}, more lines follow"),
                };
                push_line(out, &format!("(lines {}-{last} {of}: read_code path={path} start={} for more)", first + offset as i64, last + 1));
            }
        } else if next < all {
            push_line(out, &format!("({} more {} not shown: call again with offset={next}, or narrow the request)", all - next, self.noun));
        }
        if next >= all {
            if offset > 0 && count == 0 {
                push_line(out, &format!("(no more {}: {all} in all)", self.noun));
            }
            push_line(out, tail);
        }
    }

    /// The text with the items from `offset` on that fit `room` bytes (at least one; one longer
    /// than `max_bytes` is cut to fit `room`), how many were shown, and whether one was cut.
    fn render(&self, max_bytes: usize, room: usize, offset: usize) -> (String, usize, bool) {
        let mut out = self.head.clone();
        let push = push_line;
        let (mut count, mut cut) = (0, false);
        for item in self.items.iter().skip(offset) {
            if count > 0 && out.len() + item.len() + 1 > room {
                break;
            }
            let before = out.len();
            push(&mut out, item);
            count += 1;
            if out.len() > max_bytes {
                let mut at = room.saturating_sub(CUT_ITEM.len() + 1).max(before);
                while !out.is_char_boundary(at) {
                    at -= 1;
                }
                out.truncate(at);
                push(&mut out, CUT_ITEM);
                cut = true;
                break;
            }
        }
        self.close(&mut out, offset, count, &self.tail);
        (out, count, cut)
    }
}

/// The items of an answer's text in order: `take(n)` takes the next `n` and gives the ones among
/// them that are on `page`, counted from the first of them.
struct Walk {
    at: usize,
    page: std::ops::Range<usize>,
}

impl Walk {
    fn take(&mut self, n: usize) -> std::ops::Range<usize> {
        let first = self.at;
        self.at += n;
        let lo = self.page.start.clamp(first, self.at) - first;
        lo..(self.page.end.clamp(first, self.at) - first).max(lo)
    }
}

/// `v`, an array, cut to the elements in `r`.
fn keep(v: &mut Value, r: std::ops::Range<usize>) {
    if let Value::Array(a) = v {
        a.truncate(r.end);
        a.drain(..r.start.min(a.len()));
    }
}

/// `part` on a line of its own after `out`; an empty part adds nothing.
fn push_line(out: &mut String, part: &str) {
    if !out.is_empty() && !part.is_empty() {
        out.push('\n');
    }
    out.push_str(part);
}

impl Answer {
    /// The repository files whose indexed content or positions this answer shows. A line range
    /// and a structural search read the files themselves and cite none.
    pub fn cited(&self) -> Vec<&str> {
        fn rows(rows: &[ChunkRow]) -> Vec<&str> {
            rows.iter().map(|r| r.path.as_str()).collect()
        }
        match self {
            Answer::Hits { hits, .. } | Answer::Callers { hits, .. } => hits.iter().map(|h| h.path.as_str()).collect(),
            Answer::Callees { resolved, from, .. } => resolved.iter().map(|h| h.path.as_str()).chain(from.iter().map(String::as_str)).collect(),
            Answer::Grep { groups, .. } => groups.iter().map(|g| g.path.as_str()).collect(),
            Answer::Definitions(defs, _) => defs.iter().map(|(c, ..)| c.path.as_str()).collect(),
            Answer::Outline { path, .. } | Answer::Deps { path, .. } => vec![path.as_str()],
            Answer::Rdeps { rows, .. } => rows.iter().map(|r| r.0.as_str()).collect(),
            Answer::Refs { refs, .. } => rows(&refs.definitions).into_iter().chain(refs.groups.iter().map(|g| g.path.as_str())).collect(),
            Answer::Resolved { refs, .. } => rows(&refs.definitions).into_iter().chain(refs.occurrences.iter().map(|o| o.path.as_str())).collect(),
            Answer::Similar(v) => v.iter().map(|h| h.0.path.as_str()).collect(),
            Answer::Dupes(groups, _) => groups.iter().flat_map(|g| g.members.iter().map(|m| m.0.as_str())).collect(),
            Answer::Impact(i) => i.cited(),
            Answer::Map(files) => files.iter().map(|f| f.path.as_str()).collect(),
            Answer::Path(p) => [&p.from, &p.to].into_iter().chain(p.steps.iter().map(|s| &s.to)).map(|h| h.path.as_str()).collect(),
            Answer::Dead(d) => d.dead.iter().chain(d.undecided.iter().map(|u| &u.0)).map(|s| s.path.as_str()).collect(),
            Answer::Read { .. } | Answer::Captures(..) | Answer::Reindexed(_) | Answer::Files(..) => Vec::new(),
        }
    }

    /// Nothing was found (the CLI's exit code 1).
    pub fn is_empty(&self) -> bool {
        match self {
            Answer::Hits { hits, .. } | Answer::Callers { hits, .. } => hits.is_empty(),
            Answer::Callees { resolved, external, .. } => resolved.is_empty() && external.is_empty(),
            Answer::Grep { groups, .. } => groups.is_empty(),
            Answer::Read { lines, .. } => lines.is_empty(),
            Answer::Definitions(defs, _) => defs.is_empty(),
            Answer::Outline { rows, .. } => rows.is_empty(),
            Answer::Deps { deps, .. } => deps.is_empty(),
            Answer::Rdeps { rows, .. } => rows.is_empty(),
            Answer::Refs { refs, .. } => refs.is_empty(),
            Answer::Resolved { refs, only, .. } => match only {
                Some(_) => false,
                None => refs.definitions.is_empty() && refs.occurrences.is_empty() && refs.notes.is_empty(),
            },
            Answer::Similar(v) => v.is_empty(),
            Answer::Dupes(v, _) => v.is_empty(),
            Answer::Captures(v, _) => v.is_empty(),
            Answer::Map(files) => files.is_empty(),
            Answer::Path(p) => !p.found,
            Answer::Dead(d) => d.dead.is_empty() && d.undecided.is_empty(),
            Answer::Reindexed(_) => false,
            Answer::Impact(i) => i.is_empty(),
            Answer::Files(v, _) => v.is_empty(),
        }
    }

    /// The answer as text: one item per line or block, and a sentence when there is nothing.
    pub fn text(&self) -> String {
        self.text_within(usize::MAX, 0)
    }

    /// The answer as text of `max_bytes` at most: whole items from item `offset` on, then a line
    /// saying how many are left and how to ask for them. A harness cuts a tool result that is
    /// too long for it (Codex at 10,000 tokens, from the middle); cutting here keeps whole items
    /// and tells the agent what to do next. Only a budget smaller than one item and that line
    /// together is exceeded.
    pub fn text_within(&self, max_bytes: usize, offset: usize) -> String {
        self.text_as(max_bytes, offset, Form::Detailed)
    }

    /// `text_within` in either form.
    pub fn text_as(&self, max_bytes: usize, offset: usize, form: Form) -> String {
        self.text_page(max_bytes, offset, form).0
    }

    /// `text_as`, with how many items the page shows and how many the answer has.
    pub fn text_page(&self, max_bytes: usize, offset: usize, form: Form) -> (String, usize, usize) {
        let mut parts = self.parts();
        let briefs = parts.brief.as_ref().map(Vec::len);
        // a concise form that does not line up with the items would page other items
        debug_assert!(briefs.is_none_or(|n| n == parts.items.len()), "the concise form has another number of items");
        if briefs.is_some_and(|n| n != parts.items.len()) {
            parts.brief = None;
        }
        let (text, shown) = parts.within(max_bytes, offset, form);
        (text, shown, parts.items.len() + parts.rest)
    }

    fn parts(&self) -> Parts {
        let lines = |text: String, noun: &'static str| Parts { items: text.lines().map(str::to_string).collect(), noun, ..Parts::default() };
        let nothing = |sentence: String| Parts { head: sentence, ..Parts::default() };
        let hits_brief = |hits: &[Hit]| Some(hits.iter().map(|h| vec![Brief::at(&h.path, h.start, h.end, &named(hit_name(h)))]).collect());
        match self {
            Answer::Hits { hits, .. } if hits.is_empty() => nothing("(no results)".to_string()),
            Answer::Hits { hits, words, snippet, stopped } => Parts {
                items: hit_blocks(hits, words, *snippet, false),
                tail: stop_line(*stopped, "results"),
                noun: "results",
                brief: hits_brief(hits),
                ..Parts::default()
            },
            Answer::Callers { hits, .. } if hits.is_empty() => nothing("(no results)".to_string()),
            Answer::Callers { hits, undecided, stopped } => Parts {
                brief: hits_brief(hits),
                items: hit_blocks(hits, &[], false, false),
                tail: joined(
                    match *undecided {
                        0 => String::new(),
                        n if n == hits.len() => {
                            "(the resolver could not tell whether any of these calls this definition or another of the same name)".to_string()
                        }
                        n => format!("({n} of these call a definition of that name the resolver could not identify: it may be another one)"),
                    },
                    stop_line(*stopped, "results"),
                ),
                noun: "results",
                ..Parts::default()
            },
            Answer::Callees { resolved, external, stopped, .. } => Parts {
                head: if resolved.is_empty() { "(no results)".to_string() } else { String::new() },
                brief: hits_brief(resolved),
                brief_tail: Some(joined(
                    match external.len() {
                        0 => String::new(),
                        n => format!("({n} external {}: response_format detailed names them)", if n == 1 { "callee" } else { "callees" }),
                    },
                    stop_line(*stopped, "callees"),
                )),
                items: hit_blocks(resolved, &[], false, false),
                tail: joined(
                    if external.is_empty() {
                        String::new()
                    } else {
                        let more = external.len().saturating_sub(EXTERNAL_CALLEES);
                        let listed = external.iter().take(EXTERNAL_CALLEES).map(|e| shown(e)).collect::<Vec<_>>().join(", ");
                        format!("external: {listed}{}", if more > 0 { format!(" (and {more} more)") } else { String::new() })
                    },
                    stop_line(*stopped, "callees"),
                ),
                noun: "callees",
                ..Parts::default()
            },
            Answer::Grep { groups, stopped_at } => {
                let lines: usize = groups.iter().map(|g| g.lines.len()).sum();
                let stopped = stopped_at.map(|max| format!(" (stopped at {max} matching lines: narrow the pattern or raise max_results)")).unwrap_or_default();
                Parts {
                    head: format!("{lines} matching lines in {} symbols{stopped}", groups.len()),
                    items: groups.iter().map(crate::intel::group_text).collect(),
                    noun: "symbols",
                    brief: Some(groups.iter().map(|g| group_brief(g, "")).collect()),
                    ..Parts::default()
                }
            }
            Answer::Read { path, total: Some(0), .. } => nothing(format!("{} is empty", shown(path))),
            Answer::Read { path, lines, start, total: Some(total), .. } if lines.is_empty() => {
                nothing(format!("{} has {total} lines: start={start} is past the end", shown(path)))
            }
            Answer::Read { path, lines, start, total: None, .. } if lines.is_empty() => nothing(format!("{}: no lines from start={start}", shown(path))),
            Answer::Read { path, lines, start, total, defs } => Parts {
                items: lines.clone(),
                noun: "lines",
                file: Some((path.clone(), *start, *total)),
                brief: Some(
                    (*start..)
                        .zip(lines)
                        .map(|(n, line)| {
                            defs.iter().find(|d| d.start == n).map(|d| Brief::text(format!("{}-{}  {}", d.start, d.end, code_of(line)))).into_iter().collect()
                        })
                        .collect(),
                ),
                ..Parts::default()
            },
            Answer::Definitions(defs, stopped) => Parts {
                items: defs
                    .iter()
                    .map(|(c, text, _)| format!("{}:{}-{}  [{}] {}\n{text}", shown(&c.path), c.start, c.end, c.kind, shown(symbol_name(c))))
                    .collect(),
                tail: stop_line(*stopped, "definitions"),
                noun: "definitions",
                brief: Some(
                    defs.iter()
                        .map(|(c, text, members)| {
                            let line = |n: i64| text.lines().nth((n - c.start) as usize).filter(|_| n >= c.start).map(code_of);
                            let mut out = vec![Brief::at(&c.path, c.start, c.end, &named(symbol_name(c)))];
                            out.extend(line(c.start).map(|l| Brief::text(format!("  {l}"))));
                            out.extend(members.iter().map(|m| {
                                let code = line(m.start).map_or_else(|| named(sym_name(m)).into_owned(), str::to_string);
                                Brief::text(format!("  {}-{}  {code}", m.start, m.end))
                            }));
                            out
                        })
                        .collect(),
                ),
                ..Parts::default()
            },
            Answer::Outline { path, rows } if rows.is_empty() => nothing(format!("no symbols in {} (not indexed or no definitions)", echo(path))),
            Answer::Outline { rows, .. } => Parts {
                items: rows
                    .iter()
                    .map(|r| format!("{:>5}-{:<5} {}{:<9} {}", r.start, r.end, "  ".repeat(r.qualname.matches('.').count()), r.kind, shown(&r.qualname)))
                    .collect(),
                noun: "symbols",
                brief: Some(
                    rows.iter()
                        .map(|r| {
                            let depth = r.qualname.matches('.').count();
                            let name = r.qualname.rsplit('.').next().filter(|n| !n.is_empty()).unwrap_or(&r.name);
                            vec![Brief::text(format!("{}{} {}", " ".repeat(depth), r.start, shown(name)))]
                        })
                        .collect(),
                ),
                ..Parts::default()
            },
            Answer::Deps { path, deps } if deps.is_empty() => nothing(format!("no imports recorded for {}", echo(path))),
            Answer::Deps { path, deps } => Parts {
                items: deps
                    .iter()
                    .map(|d| {
                        format!(
                            "{}:{}  {}{}",
                            shown(path),
                            d.line,
                            shown(&d.module),
                            d.resolved.as_ref().map(|r| format!("  -> {}", shown(r))).unwrap_or_default()
                        )
                    })
                    .collect(),
                noun: "imports",
                brief: {
                    let mut seen = std::collections::HashSet::new();
                    Some(
                        deps.iter()
                            .map(|d| match &d.resolved {
                                Some(r) if seen.insert(r.as_str()) => vec![Brief::path(r)],
                                None if seen.insert(d.module.as_str()) => vec![Brief::text(shown(&d.module))],
                                _ => Vec::new(),
                            })
                            .collect(),
                    )
                },
                ..Parts::default()
            },
            Answer::Rdeps { target, rows, unresolved: 0 } if rows.is_empty() => nothing(format!("nothing imports {}", echo(target))),
            Answer::Rdeps { target, rows, unresolved } if rows.is_empty() => nothing(format!(
                "no import resolves to {}, but {unresolved} {} could not be resolved and may name it",
                echo(target),
                if *unresolved == 1 { "import" } else { "imports" }
            )),
            Answer::Rdeps { rows, unresolved, .. } => Parts {
                items: rows.iter().map(|(p, l, m)| format!("{}:{l}  {}", shown(p), shown(m))).collect(),
                tail: match unresolved {
                    0 => String::new(),
                    n => format!("({n} more {} could not be resolved and may name it)", if *n == 1 { "import" } else { "imports" }),
                },
                noun: "importers",
                brief: {
                    let mut seen = std::collections::HashSet::new();
                    Some(rows.iter().map(|(p, ..)| if seen.insert(p.as_str()) { vec![Brief::path(p)] } else { Vec::new() }).collect())
                },
                ..Parts::default()
            },
            Answer::Refs { refs, max_results, .. } => {
                if refs.is_empty() {
                    nothing("(no results)".to_string())
                } else {
                    Parts {
                        tail: stopped_at(refs.truncated, *max_results),
                        brief: Some(refs_brief(refs)),
                        ..lines(crate::intel::references_render(refs), "lines")
                    }
                }
            }
            Answer::Resolved { .. } if self.is_empty() => nothing("(no results)".to_string()),
            Answer::Resolved { refs, only, max_results, .. } => Parts {
                tail: stopped_at(refs.truncated, *max_results),
                brief: Some(resolved_brief(refs, *only)),
                ..lines(crate::resolve::resolved_text(refs, *only), "lines")
            },
            Answer::Similar(v) if v.is_empty() => nothing("(no results)".to_string()),
            Answer::Similar(v) => Parts {
                items: v
                    .iter()
                    .map(|(h, cos, jac)| format!("{:.3} cos  {:.2} jaccard  {}  [{}] {}", cos, jac, crate::fmt::loc(h), h.kind, shown(&h.qualname)))
                    .collect(),
                noun: "results",
                brief: Some(v.iter().map(|(h, ..)| vec![Brief::at(&h.path, h.start, h.end, &named(hit_name(h)))]).collect()),
                ..Parts::default()
            },
            Answer::Dupes(groups, _) if groups.is_empty() => nothing("no duplicates found".to_string()),
            Answer::Dupes(groups, stopped) => Parts {
                items: groups
                    .iter()
                    .map(|g| {
                        let mut out = format!("{} ({} tokens, similarity {:.2})", g.kind, g.tokens, g.similarity);
                        for (p, q, a, b) in &g.members {
                            out.push_str(&format!("\n    {}:{a}-{b}  {}", shown(p), shown(q)));
                        }
                        out
                    })
                    .collect(),
                tail: stop_line(*stopped, "groups"),
                noun: "groups",
                brief: Some(
                    groups
                        .iter()
                        .map(|g| {
                            // one name for members that all have it
                            let one = g.members.windows(2).all(|w| w[0].1 == w[1].1);
                            let mut line = Brief::text(format!("{:.2}", g.similarity));
                            for (p, q, a, b) in &g.members {
                                line = line.then(" ").then_at(p, *a, *b);
                                if !one {
                                    line = line.then(format!(" {}", named(q)));
                                }
                            }
                            if let Some((_, q, ..)) = g.members.first().filter(|_| one) {
                                line = line.then(format!(" {}", named(q)));
                            }
                            vec![line]
                        })
                        .collect(),
                ),
                ..Parts::default()
            },
            Answer::Captures(caps, _) if caps.is_empty() => nothing("no matches".to_string()),
            Answer::Captures(caps, stopped) => Parts {
                tail: stop_line(*stopped, "matches"),
                items: caps.iter().map(capture_line).collect(),
                noun: "matches",
                brief: Some(
                    caps.iter()
                        .map(|c| {
                            vec![Brief::default().then_line(&c.path, format!(":{}:{}", c.line, c.column)).then(format!(" @{} {}", c.capture, shown(&c.text)))]
                        })
                        .collect(),
                ),
                ..Parts::default()
            },
            Answer::Map(files) if files.is_empty() => nothing("(no file with a definition)".to_string()),
            Answer::Map(files) => Parts {
                items: files
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let mut out = format!("{}  (rank {}, score {:.4})", shown(&f.path), i + 1, f.score);
                        for (d, calls) in &f.definitions {
                            let calls = match calls {
                                0 => String::new(),
                                1 => "  (1 call)".to_string(),
                                n => format!("  ({n} calls)"),
                            };
                            let name = if d.qualname.is_empty() { &d.name } else { &d.qualname };
                            out.push_str(&format!("\n  {}-{}  [{}] {}{calls}", d.start, d.end, d.kind, shown(name)));
                        }
                        out
                    })
                    .collect(),
                noun: "files",
                brief: Some(
                    files
                        .iter()
                        .map(|f| {
                            let mut out = vec![Brief::path(&f.path)];
                            out.extend(f.definitions.iter().map(|(d, _)| Brief::text(format!("  {}-{} {}", d.start, d.end, named(sym_name(d))))));
                            out
                        })
                        .collect(),
                ),
                ..Parts::default()
            },
            Answer::Path(p) if !p.found => {
                let ends = format!("from {} to {}", hit_line(&p.from), hit_line(&p.to));
                nothing(if p.stopped {
                    format!("(no call path {ends} within {} {}: raise depth to search further)", p.depth, if p.depth == 1 { "call" } else { "calls" })
                } else {
                    format!("(no call path {ends})")
                })
            }
            Answer::Path(p) => Parts {
                items: std::iter::once(hit_line(&p.from))
                    .chain(p.steps.iter().map(|s| {
                        let guess = if s.tied { "" } else { "  (by name: the resolver did not tie this call)" };
                        format!("  {}:{}  calls {}{guess}", shown(&s.path), s.line, hit_line(&s.to))
                    }))
                    .collect(),
                noun: "steps",
                brief: Some(
                    std::iter::once(vec![Brief::at(&p.from.path, p.from.start, p.from.end, &named(hit_name(&p.from)))])
                        .chain(p.steps.iter().map(|s| {
                            let to = &s.to;
                            let guess = if s.tied { "" } else { " (by name)" };
                            vec![Brief::text("  ")
                                .then_line(&s.path, format!(":{}", s.line))
                                .then(" -> ")
                                .then_at(&to.path, to.start, to.end)
                                .then(format!(" {}{guess}", named(hit_name(to))))]
                        }))
                        .collect(),
                ),
                ..Parts::default()
            },
            Answer::Dead(_) if self.is_empty() => nothing("(no dead code found)".to_string()),
            Answer::Dead(d) => {
                let line = |s: &crate::index::Symbol| format!("{}:{}-{}  [{}] {}", shown(&s.path), s.start, s.end, s.kind, shown(sym_name(s)));
                let brief = |s: &crate::index::Symbol, mark: &str| vec![Brief::at(&s.path, s.start, s.end, &format!("{}{mark}", named(sym_name(s))))];
                Parts {
                    items: d.dead.iter().map(line).chain(d.undecided.iter().map(|(s, why)| format!("{}  (undecided: {why})", line(s)))).collect(),
                    tail: format!("({} dead, {} undecided)", d.dead.len(), d.undecided.len()),
                    noun: "definitions",
                    brief: Some(d.dead.iter().map(|s| brief(s, "")).chain(d.undecided.iter().map(|(s, _)| brief(s, "?"))).collect()),
                    ..Parts::default()
                }
            }
            Answer::Reindexed(st) => nothing(format!("{} files changed, {} removed, {} chunks", st.files_changed, st.files_removed, st.chunks)),
            Answer::Impact(i) if i.is_empty() => nothing(format!("no changes: {}", i.head())),
            Answer::Impact(i) => {
                Parts { head: i.head(), items: i.lines(), tail: i.stop_line(), noun: "lines", brief: Some(impact_brief(i)), ..Parts::default() }
            }
            Answer::Files(v, _) if v.is_empty() => nothing("no file matches".to_string()),
            Answer::Files(v, stopped) => Parts {
                items: v.iter().map(|h| shown(&h.path).into_owned()).collect(),
                tail: stop_line(*stopped, "files"),
                noun: "files",
                // in rank order: a directory line each time the directory changes
                brief: Some(v.iter().map(|h| vec![Brief::path(&h.path)]).collect()),
                ..Parts::default()
            },
        }
    }

    /// The answer for a client that reads structured results, sent beside a text page of
    /// `text_len` bytes that shows `shown` of `total` items from item `offset`: the whole JSON
    /// form when both together fit one and a half times `max_bytes`, else the JSON of the items
    /// that page shows, with where it starts, where the next one starts and how many there are.
    pub fn structured(&self, text_len: usize, max_bytes: usize, offset: usize, shown: usize, total: usize) -> Value {
        let mut v = self.envelope();
        if offset == 0 && text_len + v["result"].to_string().len() <= max_bytes.saturating_add(max_bytes / 2) {
            v["complete"] = json!(true);
            return v;
        }
        let from = offset.min(total);
        let to = (from + shown).min(total);
        v["complete"] = json!(false);
        v["result"] = envelope(self.page_json(from..to, total), None)["result"].take();
        v["offset"] = json!(from);
        v["next_offset"] = if to < total { json!(to) } else { Value::Null };
        v["total"] = json!(total);
        v
    }

    /// The JSON form cut to the text items in `page`, in the shape of the whole one: each list
    /// keeps the elements whose lines are on the page, in the order the text has them.
    fn page_json(&self, page: std::ops::Range<usize>, total: usize) -> Value {
        let mut v = self.json();
        if total == 0 {
            return v;
        }
        let mut w = Walk { at: 0, page: page.clone() };
        let len = |v: &Value| v.as_array().map_or(0, Vec::len);
        let list = |w: &mut Walk, v: &mut Value| {
            let r = w.take(len(v));
            keep(v, r);
        };
        match self {
            Answer::Callees { .. } => {
                list(&mut w, &mut v["resolved"]);
                // the text names the external callees after the last item
                if page.end < total {
                    v["external"] = json!([]);
                }
            }
            Answer::Grep { .. } => list(&mut w, &mut v["groups"]),
            Answer::Definitions(..) => list(&mut w, &mut v["definitions"]),
            Answer::Dead(_) => {
                list(&mut w, &mut v["dead"]);
                list(&mut w, &mut v["undecided"]);
            }
            Answer::Path(_) => {
                w.take(1);
                list(&mut w, &mut v["steps"]);
            }
            Answer::Read { lines, start, total: file_lines, .. } => {
                let r = w.take(lines.len());
                v = json!({"text": lines[r.clone()].join("\n"), "start": start + r.start as i64, "end": start + r.end as i64 - 1, "file_lines": file_lines});
            }
            Answer::Impact(_) => {
                for key in ["changed", "callers", "tests", "notes"] {
                    if key != "notes" || len(&v[key]) > 0 {
                        w.take(1);
                    }
                    list(&mut w, &mut v[key]);
                }
            }
            Answer::Resolved { only, .. } => {
                if let Some(defs) = v["definitions"].as_array_mut() {
                    defs.retain_mut(|d| {
                        let head = w.take(1);
                        let r = w.take(len(&d["references"]));
                        let on = !head.is_empty() || !r.is_empty();
                        keep(&mut d["references"], r);
                        on
                    });
                }
                w.take(1);
                if only.is_none() {
                    list(&mut w, &mut v["unresolved"]);
                }
                if v.get("notes").is_some() {
                    list(&mut w, &mut v["notes"]);
                    if len(&v["notes"]) == 0 {
                        v.as_object_mut().map(|o| o.remove("notes"));
                    }
                }
            }
            Answer::Refs { .. } => {
                for key in ["definitions", "callers"] {
                    if len(&v[key]) > 0 {
                        w.take(1);
                        list(&mut w, &mut v[key]);
                    }
                }
                w.take(1);
                if let Some(groups) = v["references"].as_array_mut() {
                    // a group is its symbol's line, a line per match and its note's line
                    groups.retain_mut(|g| {
                        let n = len(&g["lines"]);
                        let note = g.get("note").is_some();
                        let r = w.take(1 + n + usize::from(note));
                        let lo = r.start.max(1) - 1;
                        keep(&mut g["lines"], lo..(r.end.min(n + 1).saturating_sub(1)).max(lo));
                        if note && !r.contains(&(n + 1)) {
                            g.as_object_mut().map(|o| o.remove("note"));
                        }
                        !r.is_empty()
                    });
                }
            }
            Answer::Reindexed(_) => {}
            _ => list(&mut w, &mut v),
        }
        debug_assert_eq!(w.at, total, "the JSON form does not line up with the text's items");
        v
    }

    /// The one JSON form every front end gives: `{"schema", "result"}`, and `stopped_at` when
    /// the answer stopped at its limit with more left.
    pub fn envelope(&self) -> Value {
        envelope(self.json(), self.stopped())
    }

    /// The limit the answer stopped at, when there were more.
    pub fn stopped(&self) -> Option<Stop> {
        match self {
            Answer::Hits { stopped, .. } | Answer::Callers { stopped, .. } | Answer::Callees { stopped, .. } => *stopped,
            Answer::Definitions(_, stopped) | Answer::Captures(_, stopped) | Answer::Dupes(_, stopped) | Answer::Files(_, stopped) => *stopped,
            Answer::Impact(i) if i.stopped => Some(Stop { at: crate::impact::MAX_CALLERS, how: "pass a smaller depth" }),
            _ => None,
        }
    }

    /// The line the text ends with when the answer stopped at its limit, else empty.
    pub fn stop_note(&self) -> String {
        match self {
            Answer::Refs { refs, max_results, .. } => stopped_at(refs.truncated, *max_results),
            Answer::Resolved { refs, max_results, .. } => stopped_at(refs.truncated, *max_results),
            Answer::Impact(i) => i.stop_line(),
            _ => self.stopped().map(|s| stop_line(Some(s), self.parts().noun)).unwrap_or_default(),
        }
    }

    /// The answer as JSON, the shape `--json` prints.
    pub fn json(&self) -> Value {
        match self {
            Answer::Hits { hits, .. } | Answer::Callers { hits, .. } => hits_json(hits),
            Answer::Callees { resolved, external, .. } => json!({"resolved": hits_json(resolved), "external": external}),
            Answer::Grep { groups, stopped_at } => json!({
                "groups": groups.iter().map(|g| with_note(json!({
                    "symbol": g.symbol.as_ref().map(|c| json!({"path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "name": symbol_name(c)})),
                    "path": g.path,
                    "lines": g.lines.iter().map(|h| json!({"line": h.line, "text": h.text})).collect::<Vec<_>>(),
                }), g)).collect::<Vec<_>>(),
                "stopped_at": stopped_at,
            }),
            Answer::Read { lines, start, total, .. } => {
                json!({"text": lines.join("\n"), "start": start, "end": start + lines.len() as i64 - 1, "file_lines": total})
            }
            Answer::Definitions(defs, _) => json!({
                "definitions": defs.iter().map(|(c, text, _)| {
                    let mut v = chunk_json(c);
                    v["text"] = json!(text);
                    v
                }).collect::<Vec<_>>(),
            }),
            Answer::Outline { rows, .. } => {
                rows.iter().map(|r| json!({"kind": r.kind, "name": r.name, "qualname": r.qualname, "start": r.start, "end": r.end})).collect()
            }
            Answer::Deps { deps, .. } => serde_json::to_value(deps).unwrap_or_default(),
            Answer::Rdeps { rows, .. } => rows.iter().map(|(p, l, m)| json!({"path": p, "line": l, "module": m})).collect(),
            Answer::Refs { name, refs, .. } => json!({
                "name": name,
                "definitions": refs.definitions.iter().map(chunk_json).collect::<Vec<_>>(),
                "callers": hits_json(&refs.callers),
                "references": refs.groups.iter().map(|g| with_note(json!({
                    "symbol": g.symbol.as_ref().map(chunk_json),
                    "path": g.path,
                    "lines": g.lines.iter().map(|h| json!({"line": h.line, "column": h.column, "text": h.text})).collect::<Vec<_>>(),
                }), g)).collect::<Vec<_>>(),
                "truncated": refs.truncated,
            }),
            Answer::Resolved { name, refs, only, .. } => {
                // a use on a long line carries the part of the line around it, and says so
                let line_of = |o: &crate::resolve::Occurrence, mut v: Value| {
                    let (text, cut) = o.shown_line();
                    v["text"] = json!(if cut { text.as_ref() } else { &*o.text });
                    if cut {
                        v["cut"] = json!(true);
                    }
                    v
                };
                let occurrence =
                    |o: &crate::resolve::Occurrence| line_of(o, json!({"path": o.path, "line": o.line, "column": o.column, "via": o.resolution.ok().map(|x| x.1)}));
                let mut v = json!({
                    "name": name,
                    "definitions": refs.definitions.iter().filter(|d| only.is_none_or(|id| d.id == id)).map(|c| json!({
                        "path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "qualname": c.qualname,
                        "references": refs.of(c.id).map(occurrence).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "truncated": refs.truncated,
                });
                if only.is_none() {
                    v["unresolved"] = refs
                        .unresolved()
                        .map(|o| line_of(o, json!({"path": o.path, "line": o.line, "column": o.column, "reason": o.resolution.err()})))
                        .collect();
                }
                if !refs.notes.is_empty() {
                    v["notes"] = refs.notes.iter().map(|(path, note)| json!({"path": path, "note": note})).collect();
                }
                v
            }
            Answer::Similar(v) => v
                .iter()
                .map(|(h, cos, jac)| json!({"path": h.path, "start": h.start, "end": h.end, "kind": h.kind, "qualname": h.qualname, "cosine": cos, "jaccard": jac}))
                .collect(),
            Answer::Dupes(groups, _) => serde_json::to_value(groups).unwrap_or_default(),
            Answer::Captures(caps, _) => serde_json::to_value(caps).unwrap_or_default(),
            Answer::Map(files) => files
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let defs: Vec<Value> = f
                        .definitions
                        .iter()
                        .map(|(d, calls)| {
                            json!({"id": format!("{}:{}-{}", d.path, d.start, d.end), "path": d.path, "start": d.start, "end": d.end, "kind": d.kind, "qualname": d.qualname, "calls": calls})
                        })
                        .collect();
                    json!({"path": f.path, "rank": i + 1, "score": f.score, "definitions": defs})
                })
                .collect(),
            Answer::Path(p) => {
                let hit = |h: &Hit| hits_json(std::slice::from_ref(h))[0].clone();
                json!({
                    "from": hit(&p.from), "to": hit(&p.to), "found": p.found, "depth": p.depth, "stopped": p.stopped,
                    "steps": p.steps.iter().map(|s| json!({"path": s.path, "line": s.line, "definition": hit(&s.to), "tied": s.tied})).collect::<Vec<_>>(),
                })
            }
            Answer::Dead(d) => {
                let one = |s: &crate::index::Symbol| {
                    json!({"id": format!("{}:{}-{}", s.path, s.start, s.end), "path": s.path, "start": s.start, "end": s.end, "kind": s.kind, "name": s.name, "qualname": sym_name(s)})
                };
                json!({
                    "dead": d.dead.iter().map(one).collect::<Vec<_>>(),
                    "undecided": d.undecided.iter().map(|(s, why)| {
                        let mut v = one(s);
                        v["reason"] = json!(why);
                        v
                    }).collect::<Vec<_>>(),
                })
            }
            Answer::Reindexed(st) => json!({"files_changed": st.files_changed, "files_removed": st.files_removed, "chunks": st.chunks}),
            Answer::Impact(i) => i.json(),
            Answer::Files(v, _) => serde_json::to_value(v).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_windows_a_plain_path_is_under_the_root_canonicalize_gave() {
        let root = r"\\?\C:\proj";
        for (path, is_under) in [
            (r"C:\proj\a.py", true),
            ("c:/PROJ/sub/a.py", true),
            (r"\\?\C:\proj\a.py", true),
            (r"C:\proj", true),
            (r"C:\proj2\a.py", false),
            (r"C:\a.py", false),
            (r"D:\proj\a.py", false),
            (r"\\server\share\proj\a.py", false),
        ] {
            assert_eq!(under(path, root), is_under, "{path}");
        }
        let share = r"\\?\UNC\server\share\repo";
        assert!(under(r"\\server\share\repo\a.py", share));
        assert!(under(r"\\?\UNC\server\share\repo\a.py", share));
        assert!(!under(r"\\other\share\repo\a.py", share));
        // a repository at the root of a drive
        assert!(under(r"C:\a.py", r"\\?\C:\"));
    }

    #[test]
    fn a_tool_call_without_optional_arguments_gets_the_defaults() {
        let op = |tool: &str, args: Value| Op::from_tool(tool, &args).unwrap();
        assert!(matches!(op("search_code", json!({"query": "q"})), Op::Search { k: SEARCH_K, .. }));
        assert!(
            matches!(op("grep_code", json!({"pattern": "p"})), Op::Grep { max_results: GREP_MAX, ref pattern, ref scope } if pattern.mode == Mode::Regex && pattern.case == Case::Sensitive && *scope == Scope::default())
        );
        let grep = op("grep_code", json!({"pattern": "p", "mode": "fuzzy", "max_edits": 2, "case": "smart", "word": true, "path": "src", "globs": ["*.rs"]}));
        assert!(
            matches!(grep, Op::Grep { ref pattern, ref scope, .. } if pattern.mode == Mode::Fuzzy { max_edits: 2 } && pattern.case == Case::Smart && pattern.word && scope.path.as_deref() == Some("src") && scope.globs == ["*.rs"])
        );
        assert_eq!(
            Op::from_tool("grep_code", &json!({"pattern": "p", "mode": "glob"})).err().unwrap().to_string(),
            "unknown mode glob: regex, literal, identifier, pcre2, fuzzy or fuzzy_regex"
        );
        assert!(matches!(op("find_symbol", json!({"name": "n"})), Op::Symbol { limit: SYMBOL_LIMIT, .. }));
        assert!(matches!(op("callers", json!({"name": "n"})), Op::Callers { limit: CALLERS_LIMIT, .. }));
        assert!(matches!(op("callees", json!({"name": "n"})), Op::Callees { limit: CALLEES_LIMIT, .. }));
        assert!(matches!(op("read_code", json!({"path": "p"})), Op::Read { start: 1, end: None, symbol: None, .. }));
        assert!(matches!(op("read_code", json!({"symbol": "Point.norm"})), Op::Read { path: None, symbol: Some(_), .. }));
        assert_eq!(
            Op::from_tool("read_code", &json!({})).err().unwrap().to_string(),
            "missing path or symbol: read_code takes path (a file; start and end pick lines) or symbol (a name, Class.method or a path:start-end location)"
        );
        assert!(matches!(op("references", json!({"name": "n"})), Op::Refs { max_results: REFS_MAX, textual: false, def: None, .. }));
        assert!(matches!(op("similar_code", json!({"target": "t"})), Op::Similar { k: SIMILAR_K, .. }));
        assert!(matches!(op("duplicates", json!({})), Op::Dupes { min_tokens: DUPES_MIN_TOKENS, limit: DUPES_LIMIT, .. }));
        assert!(matches!(op("structural_search", json!({"lang": "rust", "query": "(x)"})), Op::Tsquery { limit: TSQUERY_LIMIT, .. }));
        assert!(matches!(op("reindex", json!({})), Op::Reindex { force: false }));
        assert!(matches!(op("impact", json!({})), Op::Impact { base: None, staged: false, depth: crate::impact::DEPTH }));
        assert!(matches!(op("find_files", json!({"query": "x"})), Op::Files { limit: crate::finder::LIMIT, .. }));
        assert!(matches!(op("find_files", json!({"query": "x", "limit": 99999})), Op::Files { limit: 1000, .. }));
        assert!(matches!(
            op("impact", json!({"base": "main...", "staged": true, "depth": 99})),
            Op::Impact { staged: true, depth: crate::impact::MAX_DEPTH, .. }
        ));
        assert!(matches!(op("repo_map", json!({})), Op::Map { path: None }));
        assert!(matches!(op("call_path", json!({"from": "a", "to": "b"})), Op::Path { depth: PATH_DEPTH, .. }));
        assert_eq!(Op::from_tool("call_path", &json!({"from": "a"})).err().unwrap().to_string(), "missing required argument to");
    }

    #[test]
    fn limits_are_clamped_and_bad_calls_are_errors() {
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": 100_000})).unwrap(), Op::Search { k: 100, .. }));
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": 0})).unwrap(), Op::Search { k: 1, .. }));
        assert_eq!(Op::from_tool("search_code", &json!({"query": "q", "k": -3})).err().unwrap().to_string(), "k must be a non-negative integer");
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": null})).unwrap(), Op::Search { k: SEARCH_K, .. }));
        assert!(matches!(Op::from_tool("references", &json!({"name": "n", "max_results": 1_000_000})).unwrap(), Op::Refs { max_results: 5000, .. }));
        assert_eq!(Op::from_tool("callers", &json!({})).err().unwrap().to_string(), "missing required argument name");
        assert_eq!(Op::from_tool("no_such_tool", &json!({})).err().unwrap().to_string(), "unknown tool no_such_tool");
        assert!(read_only("search_code") && read_only("read_code") && !read_only("reindex"));
    }

    #[test]
    fn a_long_answer_is_cut_at_whole_items_and_says_how_to_continue() {
        let hit = |i: usize| Hit { path: format!("src/f{i}.rs"), start: 1, end: 9, name: format!("f{i}"), kind: "function".into(), ..Hit::default() };
        let answer = Answer::Hits { hits: (0..5).map(hit).collect(), words: vec![], snippet: false, stopped: None };
        let all = answer.text();
        assert_eq!(all.lines().count(), 5);
        assert_eq!(answer.text_within(usize::MAX, 0), all);
        // room for two items of 30 bytes each and the closing line, not three
        let cut = answer.text_within(140, 0);
        assert!(cut.len() <= 140);
        assert_eq!(
            cut.lines().collect::<Vec<_>>(),
            ["src/f0.rs:1-9  [function] f0", "src/f1.rs:1-9  [function] f1", "(3 more results not shown: call again with offset=2, or narrow the request)",]
        );
        let rest = answer.text_within(usize::MAX, 2);
        assert_eq!(rest.lines().count(), 3);
        assert!(rest.starts_with("src/f2.rs"));
        assert_eq!(answer.text_within(140, 5), "(no more results: 5 in all)");
        // an item larger than the budget keeps its start
        let one = Answer::Read { path: "a.txt".into(), lines: vec!["x".repeat(500)], start: 1, total: Some(1), defs: vec![] };
        let cut = one.text_within(100, 0);
        assert!(cut.len() <= 100 && cut.starts_with(&"x".repeat(30)) && cut.ends_with(CUT_ITEM), "{cut}");
    }

    #[test]
    fn an_empty_answer_says_so_in_words() {
        assert_eq!(Answer::Outline { path: "a.py".into(), rows: vec![] }.text(), "no symbols in a.py (not indexed or no definitions)");
        assert_eq!(Answer::Rdeps { target: "m".into(), rows: vec![], unresolved: 0 }.text(), "nothing imports m");
        assert_eq!(Answer::Hits { hits: vec![], words: vec![], snippet: true, stopped: None }.text(), "(no results)");
        assert!(Answer::Dupes(vec![], None).is_empty() && Answer::Captures(vec![], None).is_empty());
        assert_eq!(
            Answer::Rdeps { target: "m".into(), rows: vec![("a.py".into(), 3, "m".into())], unresolved: 0 }.json(),
            json!([{"path": "a.py", "line": 3, "module": "m"}])
        );
    }
}
