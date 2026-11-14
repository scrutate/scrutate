"""tools/check_doc_shift.py: a new Rust item inserted between a doc comment and its item is caught."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "tools"))
import check_doc_shift as C  # noqa: E402

# hunks of the commits that did it, as `git diff -U5` printed them (CI checks out one commit only)
SHIFT_38734ad8 = "diff --git a/crates/scrutate/src/lex.rs b/crates/scrutate/src/lex.rs\n--- a/crates/scrutate/src/lex.rs\n+++ b/crates/scrutate/src/lex.rs\n@@ -575,16 +576,23 @@ impl Rng {\n \n /// Nearest centroid of each row of `x` (unit rows, unit centroids): the centroid with the largest\n /// `dot_seq64`, the lowest cluster number on a tie. A float32 product in any summation order is\n /// within k * 2^-24 of the true dot product, so the winner always scores within `margin` of the\n /// float32 maximum; only rows with more than one centroid that close are decided by `dot_seq64`.\n-fn nearest(x: &[f32], k: usize, cent: &[f32], nk: usize) -> Vec<i32> {\n+/// Whether the operation that writes a segment was asked to stop: clustering a large segment's\n+/// vectors takes minutes in a debug build, and a stop is seen within one block of rows.\n+pub type Stop<'a> = &'a (dyn Fn() -> bool + Sync);\n+\n+fn nearest(stop: Stop, x: &[f32], k: usize, cent: &[f32], nk: usize) -> Vec<i32> {\n     let margin = 4.0 * k as f32 * f32::EPSILON;\n     let n = x.len() / k;\n     let block = 2048;\n     let mut out = vec![0i32; n];\n     out.par_chunks_mut(block).enumerate().for_each(|(bi, chunk)| {\n+        if stop() {\n+            return;\n+        }\n         let r0 = bi * block;\n         let rows = chunk.len();\n         let mut c = vec![0f32; rows * nk];\n         unsafe {\n             matrixmultiply::sgemm(rows, k, nk, 1.0, x[r0 * k..].as_ptr(), k as isize, 1, cent.as_ptr(), 1, k as isize, 0.0, c.as_mut_ptr(), nk as isize, 1);\n"
SHIFT_62fbfc25 = 'diff --git a/crates/scrutate/src/index.rs b/crates/scrutate/src/index.rs\n--- a/crates/scrutate/src/index.rs\n+++ b/crates/scrutate/src/index.rs\n@@ -2518,10 +2518,17 @@ impl Index {\n         let mut st = self.db.prepare_cached(&format!("SELECT EXISTS(SELECT 1 FROM chunks WHERE name_key=?1 AND name=?2 AND kind IN ({}))", quoted(kinds)))?;\n         Ok(st.query_row(params![name_key(name), name], |r| r.get(0))?)\n     }\n \n     /// The definitions of these kinds named exactly `name`, those outside tests first.\n+    /// `named_of_kinds` with the name in any ASCII case.\n+    pub fn named_of_kinds_any_case(&self, name: &str, kinds: &[&str]) -> Result<Vec<ChunkRow>> {\n+        let mut st = self.db.prepare_cached(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE name_key=?1 AND kind IN ({}) ORDER BY is_test", quoted(kinds)))?;\n+        let rows: Vec<ChunkRow> = st.query_map(params![name_key(name)], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;\n+        Ok(rows.into_iter().filter(|r| r.name.eq_ignore_ascii_case(name)).collect())\n+    }\n+\n     pub fn named_of_kinds(&self, name: &str, kinds: &[&str]) -> Result<Vec<ChunkRow>> {\n         let mut st = self\n             .db\n             .prepare_cached(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE name_key=?1 AND name=?2 AND kind IN ({}) ORDER BY is_test", quoted(kinds)))?;\n         let rows = st.query_map(params![name_key(name), name], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;\n'
SHIFT_ee75c6d3 = 'diff --git a/crates/scrutate/src/ops.rs b/crates/scrutate/src/ops.rs\n--- a/crates/scrutate/src/ops.rs\n+++ b/crates/scrutate/src/ops.rs\n@@ -695,29 +695,48 @@ pub fn run(s: &mut Searcher, op: &Op) -> Result<Answer> {\n                 crate::intel::Watch { count: &s.ix.count, cancel: &s.cancel },\n             )?;\n             let (caps, stopped) = cut(caps, *limit, RAISE_LIMIT);\n             Answer::Captures(caps, stopped)\n         }\n-        Op::Map { path } => Answer::Map(crate::repomap::map(s, path.as_deref().map(|p| repo_path(s, p)).as_deref())?),\n+        Op::Map { path } => {\n+            let path = path.as_deref().map(|p| repo_path(s, p)).filter(|p| !p.is_empty() && p != ".");\n+            existing(s, path.as_deref())?;\n+            Answer::Map(crate::repomap::map(s, path.as_deref())?)\n+        }\n         Op::Path { from, to, depth } => Answer::Path(Box::new(Names::new(s)?.path(from, to, *depth)?)),\n         Op::Dead { path } => {\n             let path = path.as_deref().map(|p| repo_path(s, p)).filter(|p| !p.is_empty() && p != ".");\n-            if let Some(p) = &path {\n-                if !inside(&s.ix.root, p) || !s.ix.root.join(p).exists() {\n-                    bail!("no such file or directory in the repository: {}", echo(p));\n-                }\n-            }\n+            existing(s, path.as_deref())?;\n             Answer::Dead(Names::new(s)?.dead(path.as_deref())?)\n         }\n         Op::Reindex { .. } => bail!("reindex keeps every store current: run it through Repo"),\n         Op::Impact { base, staged, depth } => Answer::Impact(crate::impact::impact(s, base.as_deref(), *staged, *depth)?),\n         Op::Files { query, limit } => files(&s.ix.root, query, *limit)?,\n     })\n }\n \n /// Whether `root` joined with `path` stays under `root` by their parts alone (`.` dropped and\n /// `..` taking a part away), with no file looked at.\n+/// `p` with the escapes an answer writes read back (`a\\tb` as a tab), when that names a file or\n+/// directory and prints as `p` again; else `p` as written (a directory named `x\\ty`).\n+fn unescaped<\'p>(root: &std::path::Path, p: &\'p str) -> std::borrow::Cow<\'p, str> {\n+    let back = crate::fmt::unshown(p);\n+    if back != p && crate::fmt::shown(&back) == p && root.join(&*back).exists() {\n+        back\n+    } else {\n+        std::borrow::Cow::Borrowed(p)\n+    }\n+}\n+\n+/// An error unless `path` (from the root, or none) is a file or directory of the repository.\n+fn existing(s: &Searcher, path: Option<&str>) -> Result<()> {\n+    match path {\n+        Some(p) if !inside(&s.ix.root, p) || !s.ix.root.join(p).exists() => bail!("no such file or directory in the repository: {}", echo(p)),\n+        _ => Ok(()),\n+    }\n+}\n+\n fn inside(root: &std::path::Path, path: &str) -> bool {\n     let mut out = std::path::PathBuf::new();\n     for c in root.join(path).components() {\n         match c {\n             std::path::Component::CurDir => {}\n'
SHIFT_2df0ee70 = 'diff --git a/crates/scrutate/src/resolve/mod.rs b/crates/scrutate/src/resolve/mod.rs\n--- a/crates/scrutate/src/resolve/mod.rs\n+++ b/crates/scrutate/src/resolve/mod.rs\n@@ -796,10 +801,17 @@ impl<\'a> Resolver<\'a> {\n         self.cache.insert(path.to_string(), Rc::clone(&fi));\n         fi\n     }\n \n     /// Drops the loaded copy of `path`; a later use loads it again.\n+    /// The names `path` imports under a local name of its own, as (local, imported name):\n+    /// `from util import helper as h` is ("h", "helper").\n+    pub(crate) fn aliases(&mut self, path: &str) -> Vec<(String, String)> {\n+        let fi = self.file(path);\n+        fi.bindings.iter().filter(|b| !b.module.is_empty()).filter_map(|b| b.item.as_ref().filter(|i| **i != b.local).map(|i| (b.local.clone(), i.clone()))).collect()\n+    }\n+\n     pub fn forget(&mut self, path: &str) {\n         self.cache.remove(path);\n         // node ids name nodes of trees still loaded\n         self.found.clear();\n         self.tys.clear();\n'
RENAME_ebeba91e = 'diff --git a/crates/scrutate/src/mcp.rs b/crates/scrutate/src/mcp.rs\n--- a/crates/scrutate/src/mcp.rs\n+++ b/crates/scrutate/src/mcp.rs\n@@ -275,37 +282,80 @@ struct Call {\n     progress: Value,\n     cancelled: Arc<AtomicBool>,\n }\n \n /// A tool call waits up to `BUILD_WAIT` for an index that is still being built before it answers\n-/// that it is. A client that sent a progress token hears every `PROGRESS_SECONDS` that the build\n-/// goes on, and gets the real answer.\n-const PROGRESS_SECONDS: u64 = 2;\n+/// that it is. A client that sent a progress token waits for the index and gets the real answer,\n+/// with the files read so far against the files to read in `notifications/progress`, while the\n+/// index is built and while the call runs, at most once per `PROGRESS_EVERY`.\n+const PROGRESS_EVERY: Duration = Duration::from_millis(500);\n+/// How often a running call\'s count is looked at.\n+const PROGRESS_LOOK: Duration = Duration::from_millis(50);\n /// Calls beyond this many in flight run on the thread that reads requests.\n const MAX_CALLS: usize = 32;\n \n+/// The progress of one call: a count that rises with every report, as the protocol asks.\n+struct Reporter<\'a> {\n+    send: &\'a Wire<\'a>,\n+    token: Value,\n+    sent: usize,\n+    /// what an earlier count had reached: a later one adds to it\n+    base: usize,\n+    last: Option<Instant>,\n+}\n+\n+impl Reporter<\'_> {\n+    fn report(&mut self, (done, total): (usize, usize), message: &str) {\n+        let progress = self.base + done;\n+        if self.token.is_null() || progress <= self.sent || self.last.is_some_and(|t| t.elapsed() < PROGRESS_EVERY) {\n+            return;\n+        }\n+        let mut params = json!({"progressToken": self.token, "progress": progress, "message": message});\n+        if total > 0 {\n+            params["total"] = json!(self.base + total.max(done));\n+        }\n+        (self.send)(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}));\n+        self.sent = progress;\n+        self.last = Some(Instant::now());\n+    }\n+}\n+\n /// One tool call from its first wait to its answer. A cancelled call sends nothing more: it\n-/// stops waiting for the index or for its turn, and an answer already being computed is dropped.\n+/// stops waiting for the index or for its turn, an index update or a structural search it runs\n+/// stops, and an answer already being computed is dropped.\n fn run_call(send: &Wire, session: &Session, calls: &Calls, settings: &Settings, call: Call) {\n     let Call { id, name, args, stateless, structured, progress, cancelled } = call;\n-    let mut reported = 0;\n     let gone = || cancelled.load(Ordering::Relaxed);\n     let limit = progress.is_null().then_some(BUILD_WAIT);\n-    let built = session.wait_built(&cancelled, limit, |waited| {\n-        let seconds = waited.as_secs();\n-        if !progress.is_null() && seconds >= reported + PROGRESS_SECONDS {\n-            reported = seconds;\n-            let params = json!({"progressToken": progress, "progress": seconds, "message": "building the index of this repository"});\n-            send(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}));\n-        }\n-    });\n+    let mut reporter = Reporter { send, token: progress, sent: 0, base: 0, last: None };\n+    let built = session.wait_built(&cancelled, limit, |_| reporter.report(session.build_count(), "building the index of this repository"));\n     let called = if gone() {\n         None\n     } else if !built {\n         Some(Called::note(STILL_BUILDING.to_string(), false))\n     } else {\n-        Some(call_tool(session, &name, &args, settings, &cancelled))\n+        reporter.base = reporter.sent;\n+        let count = Arc::new(FileCount::default());\n+        let finished = AtomicBool::new(false);\n+        let message = format!("{name}: files read");\n+        Some(std::thread::scope(|s| {\n+            let ticker = (!reporter.token.is_null()).then(|| {\n+                s.spawn(|| loop {\n+                    std::thread::park_timeout(PROGRESS_LOOK);\n+                    if finished.load(Ordering::Acquire) || gone() {\n+                        break;\n+                    }\n+                    reporter.report(count.get(), &message);\n+                })\n+            });\n+            let c = call_counted(session, &name, &args, settings, &cancelled, &count);\n+            finished.store(true, Ordering::Release);\n+            if let Some(t) = &ticker {\n+                t.thread().unpark();\n+            }\n+            c\n+        }))\n     };\n     calls.map().remove(&id.to_string());\n     if let Some(c) = called.filter(|_| !gone()) {\n         let mut result = json!({"content": [{"type": "text", "text": c.text}], "isError": c.is_error});\n         if structured {\n'


def test_each_real_case_is_named_by_file_and_line():
    assert [b.split(': ')[0] for b in C.shifted(SHIFT_38734ad8)] == ['crates/scrutate/src/lex.rs:583']
    assert [b.split(': ')[0] for b in C.shifted(SHIFT_62fbfc25)] == ['crates/scrutate/src/index.rs:2524']
    assert [b.split(': ')[0] for b in C.shifted(SHIFT_ee75c6d3)] == ['crates/scrutate/src/ops.rs:721']
    assert [b.split(': ')[0] for b in C.shifted(SHIFT_2df0ee70)] == ['crates/scrutate/src/resolve/mod.rs:808']


def doc_diff(*lines):
    return "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1,4 +1,8 @@\n" + "\n".join(lines) + "\n"


def test_a_clean_insertion_an_edit_a_doc_line_and_a_rename_pass():
    above = doc_diff("+fn added() {}", "+", " /// What `kept` does.", " fn kept() {}")
    edit = doc_diff(" /// What `kept` does.", "-fn kept() {}", "+fn kept(x: u8) {}")
    more_doc = doc_diff(" /// What `kept` does.", "+/// And a second line.", " fn kept() {}")
    for d in (above, edit, more_doc, RENAME_ebeba91e):
        assert C.shifted(d) == [], d


def test_an_insertion_after_a_doc_and_its_attribute_or_with_its_own_doc_is_caught():
    attr = doc_diff(" /// What `kept` does.", " #[inline]", "+pub(crate) const fn added() -> u8 { 1 }", " fn kept() {}")
    own_doc = doc_diff(" /// What `kept` does.", "+/// What `added` does.", "+impl<T> Added for T {}", " fn kept() {}")
    back_below = doc_diff(" /// What `kept` does.", "-fn kept() {}", "+struct Added;", "+fn kept(x: u8) {}")
    assert C.shifted(attr) == ["x.rs:3: pub(crate) const fn added() -> u8 { 1 }"]
    assert C.shifted(own_doc) == ["x.rs:3: impl<T> Added for T {}"]
    assert C.shifted(back_below) == ["x.rs:2: struct Added;"]


def test_a_file_that_is_not_rust_is_left_alone():
    assert C.shifted(doc_diff(" /// a", "+fn b() {}").replace("x.rs", "x.py")) == []


def test_an_edited_documented_item_with_a_new_item_after_it_passes():
    # the documented `A` edited in place, then `B` added after a blank line: A keeps its doc
    edit_then_add = doc_diff(" /// The limit.", "-const A: u8 = 1;", "+const A: u8 = 2;", " ", "+const B: u8 = 3;", " fn main() {}")
    assert C.shifted(edit_then_add) == []


def test_an_attribute_over_several_lines_does_not_hide_the_doc_above_it():
    d = doc_diff(" /// What `kept` does.", " #[cfg(any(", "     unix,", "     windows,", " ))]", "+fn added() {}", " fn kept() {}")
    assert C.shifted(d) == ["x.rs:6: fn added() {}"]


def test_a_root_commit_is_read_without_a_parent(tmp_path, monkeypatch):
    import subprocess
    git = lambda *a: subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a], cwd=tmp_path, check=True, capture_output=True)
    git("init", "-q")
    (tmp_path / "x.rs").write_text("/// The limit.\nconst A: u8 = 1;\n")
    git("add", "x.rs")
    git("commit", "-qm", "root")
    monkeypatch.chdir(tmp_path)
    assert C.main(["HEAD"]) == 0


def test_a_merge_is_read_against_its_first_parent(tmp_path, monkeypatch):
    # a pull request's merge ref: what the branch brings in is what is checked
    import subprocess
    git = lambda *a: subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a], cwd=tmp_path, check=True, capture_output=True)
    git("init", "-q", "-b", "main")
    (tmp_path / "x.rs").write_text("/// The limit.\nconst A: u8 = 1;\n")
    git("add", "x.rs")
    git("commit", "-qm", "base")
    git("checkout", "-qb", "topic")
    (tmp_path / "x.rs").write_text("/// The limit.\nconst NEW: u8 = 9;\nconst A: u8 = 1;\n")
    git("commit", "-qam", "shifted")
    git("checkout", "-q", "main")
    (tmp_path / "y.rs").write_text("fn other() {}\n")
    git("add", "y.rs")
    git("commit", "-qm", "main moves on")
    git("merge", "-q", "--no-ff", "-m", "merge", "topic")
    monkeypatch.chdir(tmp_path)
    assert C.main(["HEAD"]) == 1


def test_each_commit_of_a_push_is_checked_not_only_the_last(tmp_path, monkeypatch):
    import subprocess
    git = lambda *a: subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a], cwd=tmp_path, check=True, capture_output=True, text=True)
    git("init", "-q")
    (tmp_path / "x.rs").write_text("/// The limit.\nconst A: u8 = 1;\n")
    git("add", "x.rs")
    git("commit", "-qm", "before the push")
    before = git("rev-parse", "HEAD").stdout.strip()
    (tmp_path / "x.rs").write_text("/// The limit.\nconst NEW: u8 = 9;\nconst A: u8 = 1;\n")
    git("commit", "-qam", "first pushed commit: shifts the doc")
    (tmp_path / "y.rs").write_text("fn other() {}\n")
    git("add", "y.rs")
    git("commit", "-qm", "second pushed commit: clean")
    monkeypatch.chdir(tmp_path)
    assert C.main(["HEAD"]) == 0, "the last commit alone is clean"
    assert C.main(["--each", f"{before}..HEAD"]) == 1
    assert C.main(["--each", "HEAD^..HEAD"]) == 0
