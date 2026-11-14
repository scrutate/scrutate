//! Python bindings for the Scrutate engines (PyO3, stable ABI).
//!
//! Rules this module follows (see docs/TRACEABILITY.md, section 13):
//! * every long-running engine call runs with the GIL released (`Python::detach`), so Python
//!   threads can drive the engines in parallel;
//! * Engine A results stream through a bounded channel from the search thread and are converted
//!   to Python objects one batch at a time: a search over millions of lines never materialises
//!   a giant list unless the caller builds one;
//! * no Python runtime is needed by the core crate: this crate is an optional workspace member.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

use pyo3::exceptions::{PyFileNotFoundError, PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyType};

use scrutate::instant::{self, Files, Output, Pattern, Query, Record};
use scrutate::ops::{Answer, Op};
use scrutate::session::{Every, Session};

fn err<E: std::fmt::Display>(e: E) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// A lock, also after a panic in an earlier call, so one failed call does not make every later
/// call fail.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wait for the next message without the GIL, checking for Ctrl-C every 100 ms.
/// -> None once the sender is gone.
fn wait<T: Send>(py: Python<'_>, rx: &Mutex<mpsc::Receiver<T>>) -> PyResult<Option<T>> {
    loop {
        match py.detach(|| lock(rx).recv_timeout(Duration::from_millis(100))) {
            Ok(v) => return Ok(Some(v)),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Timeout) => py.check_signals()?,
        }
    }
}

/// One matching (or context) line from Engine A.
#[pyclass(frozen, get_all, module = "scrutate")]
#[derive(Clone)]
pub struct Match {
    pub path: String,
    pub line: u64,
    pub column: u64,
    pub text: String,
    pub spans: Vec<(usize, usize)>,
    pub context: bool,
}

#[pymethods]
impl Match {
    fn __repr__(&self) -> String {
        format!("Match({}:{}: {:?})", self.path, self.line, self.text)
    }
}

impl Match {
    /// A matching or context line; None for a note on a file.
    fn of(r: Record) -> Option<Match> {
        let (h, context) = match r {
            Record::Match(h) => (h, false),
            Record::Context(h) => (h, true),
            _ => return None,
        };
        Some(Match {
            path: h.path.to_string(),
            line: h.line,
            column: h.column,
            text: h.text,
            spans: h.spans,
            context,
        })
    }
}

/// One ranked result (a function, class, module header or text window) from Engine B.
#[pyclass(frozen, get_all, module = "scrutate")]
#[derive(Clone)]
pub struct Hit {
    pub path: String,
    pub start: i64,
    pub end: i64,
    pub name: String,
    pub qualname: String,
    pub kind: String,
    pub score: f64,
    pub text: String,
}

#[pymethods]
impl Hit {
    fn __repr__(&self) -> String {
        format!(
            "Hit({}:{}-{} [{}] {} score={:.3})",
            self.path, self.start, self.end, self.kind, self.qualname, self.score
        )
    }
}

impl From<scrutate::search::Hit> for Hit {
    fn from(h: scrutate::search::Hit) -> Hit {
        Hit {
            path: h.path,
            start: h.start,
            end: h.end,
            name: h.name,
            qualname: h.qualname,
            kind: h.kind,
            score: h.score,
            text: h.text,
        }
    }
}

fn value_err(e: anyhow::Error) -> PyErr {
    PyValueError::new_err(e.to_string())
}

/// A JSON value as the Python object `json.loads` makes of it.
fn to_py<'py>(py: Python<'py>, v: &serde_json::Value) -> PyResult<Bound<'py, PyAny>> {
    py.import("json")?.call_method1("loads", (v.to_string(),))
}

/// (path, text) of each file whose search a NUL byte stopped after a match.
type Notes = Vec<(String, String)>;

/// Streaming result of `grep`: a Python iterator over `Match` objects fed by a background search
/// thread. Dropping it (or calling `cancel()`) stops the search.
#[pyclass(module = "scrutate")]
pub struct GrepIter {
    /// behind a mutex only to satisfy `Sync` (a pyclass may be shared across Python threads);
    /// `__next__` takes `&mut self`, so the lock is never contended
    rx: Option<Mutex<mpsc::Receiver<Vec<Match>>>>,
    buf: VecDeque<Match>,
    cancel: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<anyhow::Result<(instant::Stats, Notes)>>>,
    stats: Option<instant::Stats>,
    /// files whose search a NUL byte stopped after a match: (path, what happened)
    notes: Vec<(String, String)>,
    error: Option<String>,
}

impl GrepIter {
    /// Join the search thread with the GIL released: in a slow file it stops only at its next check.
    fn finish(&mut self, py: Python<'_>) {
        if let Some(h) = self.handle.take() {
            match py.detach(|| h.join()) {
                Ok(Ok((st, notes))) => {
                    self.stats = Some(st);
                    self.notes = notes;
                }
                Ok(Err(e)) => self.error = Some(e.to_string()),
                Err(_) => self.error = Some("search thread panicked".into()),
            }
        }
    }
}

#[pymethods]
impl GrepIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(mut slf: PyRefMut<'_, Self>, py: Python<'_>) -> PyResult<Option<Match>> {
        if let Some(h) = slf.buf.pop_front() {
            return Ok(Some(h));
        }
        let this = &mut *slf;
        let Some(rx) = this.rx.as_ref() else {
            return Ok(None);
        };
        let batch = wait(py, rx).inspect_err(|_| {
            this.cancel.store(true, Ordering::Relaxed);
            this.rx = None;
            this.buf.clear();
        })?;
        match batch {
            Some(b) => {
                this.buf.extend(b);
                Ok(this.buf.pop_front())
            }
            None => {
                this.rx = None;
                this.finish(py);
                if let Some(e) = this.error.take() {
                    return Err(err(e));
                }
                Ok(None)
            }
        }
    }

    /// Stop the search; remaining results are discarded.
    fn cancel(&mut self, py: Python<'_>) {
        self.cancel.store(true, Ordering::Relaxed);
        self.rx = None;
        self.buf.clear();
        self.finish(py);
    }

    /// Statistics of a finished search (files searched/matched, matches, bytes, elapsed ms,
    /// cancelled), and `notes`: (path, text) of each file whose search a NUL byte stopped.
    fn stats<'py>(&mut self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        if self.rx.is_some() {
            return Ok(None); // still running
        }
        self.finish(py);
        match &self.stats {
            None => Ok(None),
            Some(st) => {
                let d = PyDict::new(py);
                d.set_item("files_searched", st.files_searched)?;
                d.set_item("files_matched", st.files_matched)?;
                d.set_item("matches", st.matches)?;
                d.set_item("bytes_searched", st.bytes_searched)?;
                d.set_item("errors", st.errors)?;
                d.set_item("elapsed_ms", st.elapsed_ms)?;
                d.set_item("cancelled", st.cancelled)?;
                d.set_item("notes", self.notes.clone())?;
                Ok(Some(d))
            }
        }
    }
}

impl Drop for GrepIter {
    /// The search thread stops at its next cancel check; joining it here would hold up a
    /// Ctrl-C for as long as one line takes to match.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.rx = None;
    }
}

/// Engine A: search `paths` for `pattern` without any index.
///
/// mode: "regex" (default), "literal", "identifier" (every casing of an identifier) or "fuzzy"
/// (within `fuzzy_edits` edits). Returns an iterator of `Match`, in the order the files are
/// searched, or with `sort=True` in the order of `rg --sort path` (the first match then waits for
/// the whole search); `sort=True` with `max_results=N` gives the first N matches in that order.
#[pyfunction]
#[pyo3(signature = (pattern, paths=vec![".".to_string()], *, mode="regex", case="sensitive", word=false, multiline=false,
                    invert=false, hidden=false, no_ignore=false, follow_links=false, globs=vec![], types=vec![],
                    max_filesize=None, max_depth=None, max_count=None, max_results=None, before=0, after=0, threads=0, fuzzy_edits=1,
                    sort=false))]
#[allow(clippy::too_many_arguments)]
fn grep(
    pattern: String,
    paths: Vec<String>,
    mode: &str,
    case: &str,
    word: bool,
    multiline: bool,
    invert: bool,
    hidden: bool,
    no_ignore: bool,
    follow_links: bool,
    globs: Vec<String>,
    types: Vec<String>,
    max_filesize: Option<u64>,
    max_depth: Option<usize>,
    max_count: Option<u64>,
    max_results: Option<u64>,
    before: usize,
    after: usize,
    threads: usize,
    fuzzy_edits: u8,
    sort: bool,
) -> PyResult<GrepIter> {
    let q = Query {
        pattern: Pattern {
            text: pattern,
            mode: scrutate::ops::mode_named(mode, fuzzy_edits).map_err(value_err)?,
            case: scrutate::ops::case_named(case).map_err(value_err)?,
            word,
            multiline,
            ..Pattern::default()
        },
        invert,
        max_count,
        max_results,
        first_in_path_order: sort,
        before,
        after,
        want_spans: true,
        text_spans: true,
        ..Query::default()
    };
    // errors surface at call time, not at the first `next()`
    instant::build_matcher(&q.pattern).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let roots: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    let Some(first) = roots.first().cloned() else {
        return Err(PyValueError::new_err("no paths to search"));
    };
    let mut f = Files {
        hidden,
        no_ignore,
        follow_links,
        globs,
        types,
        max_filesize,
        max_depth,
        threads,
        ..Files::walk(roots)
    };
    // a custom type is the one of the index this search starts in, as on the command line,
    // whichever Repo of the process loaded its tables last
    if !f.types.is_empty() {
        let here = std::env::current_dir().map_err(|e| PyValueError::new_err(e.to_string()))?;
        f.customs = Some(
            scrutate::custom::for_search(&here)
                .map_err(|e| PyValueError::new_err(format!("{e:#}")))?,
        );
    }
    instant::overrides(&first, &f).map_err(|e| PyValueError::new_err(format!("{e:#}")))?;
    // an unknown type is the call's error, not the first next()'s
    instant::file_types(&f).map_err(|e| PyValueError::new_err(format!("{e:#}")))?;
    if let instant::Paths::Walk(roots) = &f.paths {
        if let Some(r) = roots.iter().find(|r| std::fs::metadata(r).is_err()) {
            return Err(PyFileNotFoundError::new_err(format!(
                "no such file or directory: {}",
                r.display()
            )));
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel::<Vec<Match>>(64); // bounded: a slow consumer throttles the search
    let c2 = Arc::clone(&cancel);
    let handle = std::thread::Builder::new()
        .name("scrutate-grep".into())
        .spawn(move || {
            let mut batch: Vec<Match> = Vec::with_capacity(256);
            let mut notes = Vec::new();
            let tx2 = tx.clone();
            let c3 = Arc::clone(&c2);
            let st = instant::search(
                &q,
                &f,
                Arc::clone(&c2),
                Output::Records(&mut |r| {
                    if let Record::Note { path, text, .. } = &r {
                        notes.push((path.to_string(), text.clone()));
                    }
                    batch.extend(Match::of(r));
                    if batch.len() >= 256 && tx2.send(std::mem::take(&mut batch)).is_err() {
                        c3.store(true, Ordering::Relaxed); // consumer gone
                    }
                }),
            );
            if !batch.is_empty() {
                let _ = tx.send(batch);
            }
            st.map(|st| (st, notes))
        })
        .map_err(err)?;
    Ok(GrepIter {
        rx: Some(Mutex::new(rx)),
        buf: VecDeque::new(),
        cancel,
        handle: Some(handle),
        stats: None,
        notes: Vec::new(),
        error: None,
    })
}

/// Engine B: a repository's persistent code-intelligence index.
#[pyclass(module = "scrutate")]
pub struct Repo {
    root: PathBuf,
    /// shared with the thread of an `index()` call, which outlives a Ctrl-C
    session: Arc<Session>,
}

fn hits_py(hits: Vec<scrutate::search::Hit>) -> Vec<Hit> {
    hits.into_iter().map(Into::into).collect()
}

/// `items` as a `Results` list whose `stopped_at` is the limit the answer stopped at when there
/// were more, as the CLI and the MCP tools report it.
fn results<'py, T: IntoPyObject<'py>>(
    py: Python<'py>,
    items: Vec<T>,
    stopped: Option<scrutate::ops::Stop>,
) -> PyResult<Bound<'py, PyAny>> {
    let stopped_at = stopped.map(|s| s.at);
    let out = py
        .import("scrutate._native")?
        .getattr("Results")?
        .call1((items,))?;
    out.setattr("stopped_at", stopped_at)?;
    Ok(out)
}

impl Repo {
    /// Run one operation with the GIL released, on an index brought up to date at most once a
    /// second, as the MCP server runs a tool call (`Session::run`). Calls from several Python
    /// threads run side by side.
    fn run(&self, py: Python<'_>, op: Op) -> PyResult<Answer> {
        py.detach(move || self.session.run(&op, &Arc::default()))
            .map_err(err)
    }
}

/// An answer of another kind than its operation gives: a bug.
fn unexpected() -> PyErr {
    PyRuntimeError::new_err("internal failure: an operation gave an answer of another kind")
}

#[pymethods]
impl Repo {
    /// Open (creating if needed) the index of the repository at `root`.
    #[new]
    fn new(py: Python<'_>, root: &str) -> PyResult<Self> {
        let root = PathBuf::from(root);
        let r2 = root.clone();
        let s = py
            .detach(move || Session::open(&r2, Every::given(None)))
            .map_err(err)?;
        Ok(Repo {
            root,
            session: Arc::new(s),
        })
    }

    #[getter]
    fn root(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    /// Bring the index up to date (incremental: only changed files are parsed). GIL released.
    /// Ctrl-C returns at once and stops the update at its next batch of files; the index stays
    /// as it was before the call.
    #[pyo3(signature = (force=false))]
    fn index<'py>(&self, py: Python<'py>, force: bool) -> PyResult<Bound<'py, PyDict>> {
        let (tx, rx) = mpsc::channel();
        let session = Arc::clone(&self.session);
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name("scrutate-index".into())
            .spawn(move || {
                let _ = tx.send(session.write(|repo| {
                    repo.update_until(force, c2)
                        .map(|up| up.code.unwrap_or_default())
                }));
            })
            .map_err(err)?;
        let st = wait(py, &Mutex::new(rx))
            .inspect_err(|_| cancel.store(true, Ordering::Relaxed))?
            .unwrap_or_else(|| Err(anyhow::anyhow!("index update panicked")))
            .map_err(err)?;
        let d = PyDict::new(py);
        d.set_item("files_total", st.files_total)?;
        d.set_item("files_changed", st.files_changed)?;
        d.set_item("files_removed", st.files_removed)?;
        d.set_item("chunks", st.chunks)?;
        d.set_item("seconds", st.seconds)?;
        d.set_item("semantic", st.semantic)?;
        Ok(d)
    }

    /// Ranked search by meaning, name or identifier. `alt` are alternative phrasings whose results
    /// are fused with the main query. GIL released.
    #[pyo3(signature = (query, k=scrutate::ops::SEARCH_K, alt=None, with_text=true))]
    fn search(
        &self,
        py: Python<'_>,
        query: &str,
        k: usize,
        alt: Option<Vec<String>>,
        with_text: bool,
    ) -> PyResult<Vec<Hit>> {
        let op = Op::Search {
            query: query.to_string(),
            alt: alt.unwrap_or_default(),
            k,
            snippets: with_text,
        };
        match self.run(py, op)? {
            Answer::Hits { hits, .. } => Ok(hits_py(hits)),
            _ => Err(unexpected()),
        }
    }

    /// Where is this function/class/method defined? (`Class.method` works.) A `Results` list:
    /// `stopped_at` is `limit` when there were more.
    #[pyo3(signature = (name, limit=scrutate::ops::SYMBOL_LIMIT))]
    fn symbol<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        limit: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        // a location (`path:start-end`) and a path spelling are read as the MCP tools read them
        let op = Op::Symbol {
            name: name.to_string(),
            limit,
        };
        match self.run(py, op)? {
            Answer::Hits { hits, stopped, .. } => results(py, hits_py(hits), stopped),
            _ => Err(unexpected()),
        }
    }

    /// Functions that call `name`. A `Results` list: `stopped_at` is `limit` when there were more,
    /// `undecided` counts the callers the resolver kept without knowing whether they call this
    /// definition or another of the same name.
    #[pyo3(signature = (name, limit=scrutate::ops::CALLERS_LIMIT))]
    fn callers<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        limit: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let op = Op::Callers {
            name: name.to_string(),
            limit,
        };
        match self.run(py, op)? {
            Answer::Callers {
                hits,
                undecided,
                stopped,
            } => {
                let out = results(py, hits_py(hits), stopped)?;
                out.setattr("undecided", undecided)?;
                Ok(out)
            }
            _ => Err(unexpected()),
        }
    }

    /// Functions `name` calls: (resolved hits, unresolved external names). The hits are a
    /// `Results` list: `stopped_at` is `limit` when there were more.
    #[pyo3(signature = (name, limit=scrutate::ops::CALLEES_LIMIT))]
    fn callees<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        limit: usize,
    ) -> PyResult<(Bound<'py, PyAny>, Vec<String>)> {
        let op = Op::Callees {
            name: name.to_string(),
            limit,
        };
        match self.run(py, op)? {
            Answer::Callees {
                resolved,
                external,
                stopped,
                ..
            } => Ok((results(py, hits_py(resolved), stopped)?, external)),
            _ => Err(unexpected()),
        }
    }

    /// Symbols of a file in source order, as dicts (kind, name, qualname, start, end).
    fn outline<'py>(&self, py: Python<'py>, path: &str) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let Answer::Outline { rows, .. } = self.run(
            py,
            Op::Outline {
                path: path.to_string(),
            },
        )?
        else {
            return Err(unexpected());
        };
        rows.iter()
            .map(|r| {
                let d = PyDict::new(py);
                d.set_item("kind", &r.kind)?;
                d.set_item("name", &r.name)?;
                d.set_item("qualname", &r.qualname)?;
                d.set_item("start", r.start)?;
                d.set_item("end", r.end)?;
                Ok(d)
            })
            .collect()
    }

    /// Modules a file imports: list of (line, module, resolved_path_or_None).
    fn dependencies(
        &self,
        py: Python<'_>,
        path: &str,
    ) -> PyResult<Vec<(i64, String, Option<String>)>> {
        let Answer::Deps { deps, .. } = self.run(
            py,
            Op::Deps {
                path: path.to_string(),
            },
        )?
        else {
            return Err(unexpected());
        };
        Ok(deps
            .into_iter()
            .map(|d| (d.line, d.module, d.resolved))
            .collect())
    }

    /// Files importing a module or repository file: list of (path, line, module as written).
    fn dependents(&self, py: Python<'_>, target: &str) -> PyResult<Vec<(String, i64, String)>> {
        match self.run(
            py,
            Op::Rdeps {
                target: target.to_string(),
            },
        )? {
            Answer::Rdeps { rows, .. } => Ok(rows),
            _ => Err(unexpected()),
        }
    }

    /// Code similar to a symbol or `path:line`: list of (hit, cosine, jaccard).
    #[pyo3(signature = (target, k=scrutate::ops::SIMILAR_K))]
    fn similar(&self, py: Python<'_>, target: &str, k: usize) -> PyResult<Vec<(Hit, f32, f32)>> {
        let op = Op::Similar {
            target: target.to_string(),
            k,
        };
        match self.run(py, op)? {
            Answer::Similar(out) => Ok(out.into_iter().map(|(h, c, j)| (h.into(), c, j)).collect()),
            _ => Err(unexpected()),
        }
    }

    /// Duplicate and near-duplicate functions, as a `Results` list of JSON-compatible dicts:
    /// `stopped_at` is `limit` when there were more.
    #[pyo3(signature = (min_tokens=scrutate::ops::DUPES_MIN_TOKENS, threshold=scrutate::ops::DUPES_THRESHOLD, limit=scrutate::ops::DUPES_LIMIT))]
    fn duplicates<'py>(
        &self,
        py: Python<'py>,
        min_tokens: usize,
        threshold: f32,
        limit: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let op = Op::Dupes {
            min_tokens,
            threshold,
            limit,
        };
        let Answer::Dupes(groups, stopped) = self.run(py, op)? else {
            return Err(unexpected());
        };
        let dicts = groups
            .iter()
            .map(|g| {
                let d = PyDict::new(py);
                d.set_item("kind", g.kind)?;
                d.set_item("similarity", g.similarity)?;
                d.set_item("tokens", g.tokens)?;
                d.set_item("members", g.members.clone())?;
                Ok(d)
            })
            .collect::<PyResult<Vec<_>>>()?;
        results(py, dicts, stopped)
    }

    /// Every use of `name`, each resolved to the definition it refers to, as the `references`
    /// tool and `scrutate refs --json` give it (a dict: `definitions` with their `references`,
    /// `unresolved`, `truncated`). `textual`: every occurrence grouped by enclosing symbol
    /// instead; `definition` (`path` or `path:line`) keeps one definition.
    #[pyo3(signature = (name, max_results=scrutate::ops::REFS_MAX, textual=false, definition=None))]
    fn references<'py>(
        &self,
        py: Python<'py>,
        name: &str,
        max_results: u64,
        textual: bool,
        definition: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let op = Op::Refs {
            name: name.to_string(),
            max_results,
            textual,
            def: definition,
        };
        let answer = self.run(py, op)?;
        to_py(py, &answer.json())
    }

    /// Any operation by its MCP tool name (`grep_code`, `find_symbol`, `references`, ...) with
    /// that tool's arguments as a dict, answered as the tool answers: a dict with `schema`,
    /// `result` (the JSON form), `stopped_at` when the answer stopped at its limit, and `text`, the
    /// text form from item `offset` on within `max_bytes` (whole without it), concise when the
    /// arguments say `response_format: concise`.
    #[pyo3(signature = (tool, args=None, *, max_bytes=None, offset=0))]
    fn call<'py>(
        &self,
        py: Python<'py>,
        tool: &str,
        args: Option<Bound<'py, PyDict>>,
        max_bytes: Option<usize>,
        offset: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        let args: serde_json::Value = match args {
            Some(a) => {
                let text: String = py.import("json")?.call_method1("dumps", (a,))?.extract()?;
                serde_json::from_str(&text).map_err(|e| PyValueError::new_err(e.to_string()))?
            }
            None => serde_json::json!({}),
        };
        let op = Op::from_tool(tool, &args).map_err(value_err)?;
        let form = scrutate::ops::Form::from_tool(&args).map_err(value_err)?;
        let answer = self.run(py, op)?;
        let mut v = answer.envelope();
        v["text"] = answer
            .text_as(max_bytes.unwrap_or(usize::MAX), offset, form)
            .into();
        to_py(py, &v)
    }

    /// Index statistics as a dict. GIL released while it waits for the engine.
    fn stats<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let v = py
            .detach(|| self.session.read(&Arc::default(), false, |s| s.ix.stats()))
            .map_err(err)?;
        let d = PyDict::new(py);
        if let Some(m) = v.as_object() {
            for (k, val) in m {
                match val {
                    serde_json::Value::Number(n) if n.is_i64() => d.set_item(k, n.as_i64())?,
                    serde_json::Value::Bool(b) => d.set_item(k, *b)?,
                    other => d.set_item(k, other.to_string())?,
                }
            }
        }
        Ok(d)
    }

    /// Lines `start..=end` of a file inside the repository (never outside it), numbered.
    /// Raises FileNotFoundError for a missing file, ValueError for a path outside the repository.
    /// GIL released while it waits for the engine.
    #[pyo3(signature = (path, start=1, end=None, max_lines=scrutate::ops::READ_LINES))]
    fn read(
        &self,
        py: Python<'_>,
        path: &str,
        start: i64,
        end: Option<i64>,
        max_lines: i64,
    ) -> PyResult<String> {
        let end = end
            .unwrap_or(i64::MAX)
            .min(start.saturating_add(max_lines.saturating_sub(1)));
        let op = Op::Read {
            path: Some(path.to_string()),
            symbol: None,
            start,
            end: Some(end),
        };
        let r = py.detach(|| self.session.run(&op, &Arc::default()));
        let e = match r {
            Ok(Answer::Read { lines, .. }) => return Ok(lines.join("\n")),
            Ok(_) => return Err(unexpected()),
            Err(e) => e.to_string(),
        };
        // the engine's message already says which: no file is looked at here
        Err(if e.starts_with("path outside repo") {
            PyValueError::new_err(e)
        } else if e.starts_with("cannot read ") {
            PyFileNotFoundError::new_err(e)
        } else {
            PyOSError::new_err(e)
        })
    }
}

/// Engine version string.
#[pyfunction]
fn version() -> &'static str {
    scrutate::VERSION
}

/// Thread-safety review for free-threaded Python (`gil_used = false`):
/// * `Match` and `Hit` are frozen (immutable after construction);
/// * `Repo` holds a `Session`, which runs calls from several threads side by side, each on a
///   reader of its own, and its writes one at a time;
/// * `GrepIter` methods take `&mut self`: PyO3's borrow checker rejects concurrent mutable access
///   with an exception instead of racing; the channel receiver is additionally behind a `Mutex`;
/// * the module holds no other global mutable state; the crate's custom language tables are
///   read per use: each `Repo` call and index update under its own root's, and `grep(types=)`
///   under those of the index the current directory is in, as on the command line.
#[pymodule(gil_used = false)]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Match>()?;
    m.add_class::<Hit>()?;
    m.add_class::<GrepIter>()?;
    m.add_class::<Repo>()?;
    m.add_function(wrap_pyfunction!(grep, m)?)?;
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add("__version__", scrutate::VERSION)?;
    // a plain list subclass, so a result works wherever a list did
    let py = m.py();
    let ns = PyDict::new(py);
    ns.set_item("__module__", "scrutate._native")?;
    ns.set_item("__doc__", "A list of results; `stopped_at` is the limit the answer stopped at when there were more, else None.")?;
    ns.set_item("stopped_at", py.None())?;
    ns.set_item("undecided", py.None())?;
    m.add(
        "Results",
        py.get_type::<PyType>()
            .call1(("Results", (py.get_type::<PyList>(),), ns))?,
    )?;
    Ok(())
}
