//! `scrutate files` and the MCP `find_files` tool: the paths of the repository ranked for a short,
//! possibly misspelt query, with frecency (how often and how lately a file was read) kept in a
//! small state file outside the repository.
//!
//! A path matches when the query is a substring of it, a subsequence of it (letters left out), or
//! within a few edits of a substring (a letter wrong, added, left out, or two neighbours swapped):
//! one edit per four characters of the query, at most three. Case is ignored. Ranking: the
//! basename (or the basename without its extension) equal to the query first, then matches in
//! the basename before matches in the rest of the path, then substring before subsequence before
//! fewer edits, then higher frecency, then the shorter path, then the path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const LIMIT: usize = 20;
pub const MAX_LIMIT: u64 = 1000;
/// A read counts half as much after this many days.
pub const HALF_LIFE_DAYS: f64 = 7.0;
/// Files the state keeps, over every repository; the lowest counts go first.
pub const MAX_ENTRIES: usize = 1000;
const STATE_FILE: &str = "frecency.json";

/// One path of the answer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FileHit {
    pub path: String,
    /// `exact` (the basename is the query), `basename` or `path`: where the query matched
    #[serde(rename = "match")]
    pub matched: &'static str,
    /// edits of the query the match needed (0 for a substring or a subsequence)
    pub edits: u32,
    /// the decayed count of reads; 0 for a file never read
    pub frecency: f64,
}

/// The paths of the files a search of `root` covers that match `query`, best first, at most
/// `limit`, and whether more matched. A query that is exactly one of those paths counts as a
/// read of that file: a caller says which result it chose by asking for it by its path.
pub fn find(root: &Path, query: &str, limit: usize) -> Result<(Vec<FileHit>, bool)> {
    let typed = query.trim().replace('\\', "/");
    let typed = typed.trim_start_matches("./");
    let q: Vec<char> = typed.to_lowercase().chars().collect();
    if q.is_empty() {
        bail!("files needs a query: part of a file name or path");
    }
    let key = root_key(root);
    let state = state_file().map(|f| State::load(&f)).unwrap_or_default();
    let now = now();
    let mut hits: Vec<(Rank, FileHit)> = walk(root)
        .into_iter()
        .filter_map(|path| {
            let (tier, matched, quality) = rank_of(&path, &q)?;
            let frecency = state.frecency(&key, &path, now);
            let edits = quality.saturating_sub(1);
            Some(((tier, quality, std::cmp::Reverse(Score(frecency)), path.len(), path.clone()), FileHit { path, matched, edits, frecency: round(frecency) }))
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some((_, h)) = hits.iter().find(|(_, h)| h.path == typed) {
        visited(root, &[h.path.as_str()]);
    }
    let more = hits.len() > limit;
    Ok((hits.into_iter().take(limit).map(|(_, h)| h).collect(), more))
}

/// Count one read of each of `paths` (relative to `root`). Never fails: a state file that does
/// not parse is started again, one that cannot be written is left as it was.
pub fn visited(root: &Path, paths: &[&str]) {
    // the calls of one MCP server run at once; another process's write may still be lost
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Some(file) = state_file() else { return };
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = State::load(&file);
    let key = root_key(root);
    let now = now();
    for p in paths {
        state.visit(&key, p.replace('\\', "/").trim_start_matches("./"), now);
    }
    state.bound(now);
    let _ = state.save(&file);
}

/// `$XDG_STATE_HOME/scrutate/frecency.json`, else under `~/.local/state/scrutate/` (on Windows
/// `%LOCALAPPDATA%\scrutate\`). None when no such directory is named.
pub fn state_file() -> Option<PathBuf> {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| p.is_absolute());
    let base = var("XDG_STATE_HOME").or_else(|| if cfg!(windows) { var("LOCALAPPDATA") } else { var("HOME").map(|h| h.join(".local").join("state")) })?;
    Some(base.join("scrutate").join(STATE_FILE))
}

/// tier (exact basename, basename, path), quality, frecency, length, path
type Rank = (u8, u32, std::cmp::Reverse<Score>, usize, String);

/// A frecency that sorts; it is never NaN.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Score(f64);
impl Eq for Score {}
impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Score {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

fn round(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// (tier, `match` name, quality) of a path the query matches.
fn rank_of(path: &str, q: &[char]) -> Option<(u8, &'static str, u32)> {
    let lower: Vec<char> = path.to_lowercase().chars().collect();
    let base = lower.iter().rposition(|&c| c == '/').map_or(&lower[..], |i| &lower[i + 1..]);
    let stem = base.iter().rposition(|&c| c == '.').filter(|&i| i > 0).map_or(base, |i| &base[..i]);
    if base == q || stem == q {
        return Some((0, "exact", 0));
    }
    if !q.contains(&'/') {
        if let Some(e) = quality(base, q) {
            return Some((1, "basename", e));
        }
    }
    quality(&lower, q).map(|e| (2, "path", e))
}

/// 0 for a substring, 1 for a subsequence, 1 + edits for a substring within the edits allowed.
fn quality(text: &[char], q: &[char]) -> Option<u32> {
    if text.windows(q.len()).any(|w| w == q) {
        return Some(0);
    }
    let mut it = text.iter();
    if q.iter().all(|c| it.any(|t| t == c)) {
        return Some(1);
    }
    let k = (q.len() / 4).min(3);
    let e = edits(text, q);
    (k > 0 && e <= k).then_some(1 + e as u32)
}

/// The fewest edits (insertion, deletion, substitution, a swap of neighbours) that turn `q` into
/// some substring of `text`: Sellers' algorithm with the optimal string alignment distance.
fn edits(text: &[char], q: &[char]) -> usize {
    let m = q.len();
    let mut before: Vec<usize> = (0..=m).collect();
    let mut prev = before.clone();
    let mut best = m;
    for j in 0..text.len() {
        let mut cur = vec![0; m + 1];
        for i in 1..=m {
            cur[i] = (prev[i - 1] + usize::from(q[i - 1] != text[j])).min(prev[i] + 1).min(cur[i - 1] + 1);
            if i > 1 && j > 0 && q[i - 1] == text[j - 1] && q[i - 2] == text[j] {
                cur[i] = cur[i].min(before[i - 2] + 1);
            }
        }
        best = best.min(cur[m]);
        before = std::mem::replace(&mut prev, cur);
    }
    best
}

/// Relative '/' paths of the files a search of the repository covers, unordered.
fn walk(root: &Path) -> Vec<String> {
    let found = std::sync::Mutex::new(Vec::new());
    let Ok(w) = crate::instant::build_walker(&[root.to_path_buf()], &crate::files::repository_files(root)) else { return Vec::new() };
    w.run(|| {
        Box::new(|e| {
            if let Some(e) = e.ok().filter(|e| e.file_type().is_some_and(|t| t.is_file())) {
                if let Ok(rel) = e.path().strip_prefix(root) {
                    found.lock().unwrap().push(crate::files::slashed(rel));
                }
            }
            ignore::WalkState::Continue
        })
    });
    found.into_inner().unwrap()
}

fn root_key(root: &Path) -> String {
    crate::paths::key(root)
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct Entry {
    /// the count at `at`, before decay
    score: f64,
    /// seconds since the Unix epoch
    at: f64,
}

impl Entry {
    fn now(&self, now: f64) -> f64 {
        self.score * 0.5f64.powf((now - self.at).max(0.0) / (HALF_LIFE_DAYS * 86_400.0))
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct State {
    version: u32,
    /// repository root -> path -> entry
    roots: BTreeMap<String, BTreeMap<String, Entry>>,
}

impl State {
    /// The state in `file`; an empty one when it is missing or does not parse.
    fn load(file: &Path) -> State {
        let mut s: State = std::fs::read(file).ok().and_then(|b| serde_json::from_slice::<State>(&b).ok()).filter(|s| s.version == 1).unwrap_or_default();
        s.plain_keys(cfg!(windows));
        s
    }

    /// Roots kept in an older form (on Windows the verbatim `\\?\C:\x` that `canonicalize` gives)
    /// under the form `paths::key` writes now; a file kept under both keeps the later visit.
    fn plain_keys(&mut self, windows: bool) {
        for (root, files) in std::mem::take(&mut self.roots) {
            let to = self.roots.entry(crate::paths::plain_on(&root, windows).into_owned()).or_default();
            for (path, e) in files {
                let kept = to.entry(path).or_insert(e);
                if e.at > kept.at {
                    *kept = e;
                }
            }
        }
    }

    fn frecency(&self, root: &str, path: &str, now: f64) -> f64 {
        self.roots.get(root).and_then(|r| r.get(path)).map_or(0.0, |e| e.now(now))
    }

    fn visit(&mut self, root: &str, path: &str, now: f64) {
        let e = self.roots.entry(root.to_string()).or_default().entry(path.to_string()).or_default();
        *e = Entry { score: e.now(now) + 1.0, at: now };
    }

    /// At most `MAX_ENTRIES` entries: the lowest decayed counts go.
    fn bound(&mut self, now: f64) {
        let mut all: Vec<(f64, String, String)> =
            self.roots.iter().flat_map(|(r, files)| files.iter().map(move |(p, e)| (e.now(now), r.clone(), p.clone()))).collect();
        if all.len() <= MAX_ENTRIES {
            return;
        }
        all.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| (&a.1, &a.2).cmp(&(&b.1, &b.2))));
        let over = all.len() - MAX_ENTRIES;
        for (_, r, p) in all.into_iter().take(over) {
            if let Some(files) = self.roots.get_mut(&r) {
                files.remove(&p);
            }
        }
        self.roots.retain(|_, files| !files.is_empty());
    }

    /// Written whole to a file beside `file`, then renamed over it.
    fn save(&mut self, file: &Path) -> Result<()> {
        self.version = 1;
        let dir = file.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        #[cfg(not(target_os = "wasi"))]
        let tmp = dir.join(format!("{STATE_FILE}.{}.tmp", std::process::id()));
        // wasi has no process id
        #[cfg(target_os = "wasi")]
        let tmp = dir.join(format!("{STATE_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        if let Err(e) = std::fs::rename(&tmp, file) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn a_root_kept_in_its_verbatim_form_is_read_under_its_plain_one() {
        let e = |score: f64, at: f64| Entry { score, at };
        let state = || {
            let mut s = State::default();
            s.roots.insert(r"\\?\C:\repo".into(), [("a.py".to_string(), e(5.0, 10.0)), ("b.py".to_string(), e(1.0, 30.0))].into());
            s.roots.insert(r"C:\repo".into(), [("a.py".to_string(), e(2.0, 20.0))].into());
            s
        };
        let mut s = state();
        s.plain_keys(true);
        assert_eq!(s.roots.keys().collect::<Vec<_>>(), [r"C:\repo"]);
        let files = &s.roots[r"C:\repo"];
        assert_eq!((files["a.py"].score, files["a.py"].at), (2.0, 20.0), "the later visit is kept");
        assert_eq!((files["b.py"].score, files["b.py"].at), (1.0, 30.0));
        let mut elsewhere = state();
        elsewhere.plain_keys(false);
        assert_eq!(elsewhere.roots.len(), 2, "only Windows has the verbatim form");
    }

    #[test]
    fn a_typo_a_dropped_letter_and_a_swap_match_within_their_edits() {
        let q = |t: &str, q: &str| quality(&chars(t), &chars(q));
        assert_eq!(q("search.rs", "search"), Some(0));
        assert_eq!(q("search.rs", "srch.rs"), Some(1));
        assert_eq!(q("search.rs", "saerch.rs"), Some(2), "a swap is one edit");
        assert_eq!(q("search.rs", "seerch.rs"), Some(2));
        assert_eq!(q("search.rs", "searchx.rs"), Some(2));
        assert_eq!(q("search.rs", "zzzzzz.rs"), None);
        assert_eq!(q("abc", "abd"), None, "three letters allow no edit");
    }

    #[test]
    fn a_read_decays_by_half_each_half_life_and_the_state_stays_bounded() {
        let mut s = State::default();
        s.visit("r", "a", 0.0);
        s.visit("r", "a", 0.0);
        let week = HALF_LIFE_DAYS * 86_400.0;
        assert!((s.frecency("r", "a", week) - 1.0).abs() < 1e-9);
        s.visit("r", "a", week);
        assert!((s.frecency("r", "a", week) - 2.0).abs() < 1e-9);
        for i in 0..MAX_ENTRIES + 5 {
            s.visit("q", &format!("f{i}"), 2.0 * week);
        }
        s.bound(2.0 * week);
        assert_eq!(s.roots.values().map(BTreeMap::len).sum::<usize>(), MAX_ENTRIES);
        assert!(s.frecency("r", "a", 2.0 * week) > 0.0, "the most read file stays");
    }

    #[test]
    fn a_state_file_that_does_not_parse_is_read_as_empty_and_replaced() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp").join(format!("frecency_unit_{}", std::process::id()));
        let file = dir.join(STATE_FILE);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, b"{not json").unwrap();
        let mut s = State::load(&file);
        assert!(s.roots.is_empty());
        s.visit("r", "a", 1.0);
        s.save(&file).unwrap();
        assert!((State::load(&file).frecency("r", "a", 1.0) - 1.0).abs() < 1e-9);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temporary file is left");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
