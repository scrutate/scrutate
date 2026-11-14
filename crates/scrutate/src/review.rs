//! A pull request report: what `impact` finds between a base and a head commit, and the
//! duplicates the head adds that the base did not have. It renders as the Markdown of a PR
//! comment, as JSON, and as a GitLab Code Quality report.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use anyhow::Result;
use serde_json::{json, Value};

use crate::fmt::shown;
use crate::impact::Impact;
use crate::index::Body;
use crate::intel::{DupGroup, DupKeys, Focus, KeyRows};
use crate::search::{Hit, Searcher, CODE_KINDS};

/// The first line of the comment: the GitHub Action finds its earlier comment by it.
pub const MARKER: &str = "<!-- scrutate-review -->";
/// The longest comment body GitHub takes, in characters.
pub const MAX_COMMENT: usize = 65_536;
/// Duplicate groups a report lists at most.
pub const MAX_DUPLICATES: usize = 50;

pub struct Review {
    pub impact: Impact,
    /// the head as given (`HEAD` by default)
    pub head: String,
    /// the new duplicate groups, largest first, as `dupes` sorts them
    pub duplicates: Vec<DupGroup>,
    /// more groups than `MAX_DUPLICATES` were new
    pub more_duplicates: bool,
    /// indexes into `impact.changed` of the definitions no test reaches within the walk
    pub untested: Vec<usize>,
}

/// The report of `head` (a revision; `HEAD` by default) against `base`, on the index of the
/// working tree, which should be the head checked out.
pub fn review(s: &Searcher, base: &str, head: Option<&str>, depth: usize) -> Result<Review> {
    let head = head.unwrap_or("HEAD").to_string();
    let impact = crate::impact::between(s, base, &head, depth)?;
    let (mut duplicates, untested) = (new_duplicates(s, &impact)?, untested(&impact));
    let more_duplicates = duplicates.len() > MAX_DUPLICATES;
    duplicates.truncate(MAX_DUPLICATES);
    Ok(Review { impact, head, duplicates, more_duplicates, untested })
}

/// The duplicate groups of the head that the base did not have. Only a group with a member in a
/// file the diff changed can be new; the base's groups come from the same search with the changed
/// files replaced by a parse of their base copies, around the head groups' members, since a base
/// group that holds a match for each member shares a key with one. A head group is old when one
/// base group holds a match for each of its members, each base member used once: the same
/// definition (path and qualified name), or the same body text up to whitespace.
fn new_duplicates(s: &Searcher, i: &Impact) -> Result<Vec<DupGroup>> {
    let (min, threshold) = (crate::ops::DUPES_MIN_TOKENS, crate::ops::DUPES_THRESHOLD);
    let changed: HashSet<&str> = i.head_paths.iter().chain(i.base_copies.iter().map(|c| &c.0)).map(String::as_str).collect();
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    let none = HashSet::new();
    let keys = KeyRows::load(&s.ix, min)?;
    let around_head = Focus { left_out: &none, put_in: &[], paths: &changed, keys: &[] };
    let head: Vec<DupGroup> = crate::intel::duplicates(&s.ix, &keys, Some(&around_head), threshold, usize::MAX)?
        .into_iter()
        .filter(|g| g.members.iter().any(|m| changed.contains(m.0.as_str())))
        .collect();
    if head.is_empty() {
        return Ok(head);
    }
    let parsed: Vec<Body> = i.base_copies.iter().flat_map(|(path, src)| bodies_of(path, src, min)).collect();
    let head_keys = member_keys(s, head.iter().flat_map(|g| &g.members), &[])?;
    let member: Vec<DupKeys> = head_keys.values().copied().collect();
    let put_in: Vec<&Body> = parsed.iter().collect();
    let around_base = Focus { left_out: &changed, put_in: &put_in, paths: &none, keys: &member };
    let base = crate::intel::duplicates(&s.ix, &keys, Some(&around_base), threshold, usize::MAX)?;
    let base_keys = member_keys(s, base.iter().flat_map(|g| &g.members), &parsed)?;
    let old = |g: &DupGroup| {
        base.iter().filter(|b| b.members.len() >= g.members.len()).any(|b| {
            let mut used = vec![false; b.members.len()];
            let mut left: Vec<&(String, String, i64, i64)> = Vec::new();
            for m in &g.members {
                match (0..b.members.len()).find(|&j| !used[j] && b.members[j].0 == m.0 && b.members[j].1 == m.1) {
                    Some(j) => used[j] = true,
                    None => left.push(m),
                }
            }
            left.into_iter().all(|m| {
                let found =
                    head_keys.get(m).and_then(|k| (0..b.members.len()).find(|&j| !used[j] && base_keys.get(&b.members[j]).map(|x| x.body) == Some(k.body)));
                found.map(|j| used[j] = true).is_some()
            })
        })
    };
    Ok(head.into_iter().filter(|g| !old(g)).collect())
}

/// The keys of these group members: one of `parsed` from its text, any other from the index.
fn member_keys<'a>(
    s: &Searcher,
    members: impl Iterator<Item = &'a (String, String, i64, i64)>,
    parsed: &[Body],
) -> Result<HashMap<(String, String, i64, i64), DupKeys>> {
    let (mut out, mut seen, mut ids) = (HashMap::new(), HashSet::new(), Vec::new());
    let mut at: HashMap<i64, (String, String, i64, i64)> = HashMap::new();
    for m in members {
        if !seen.insert(m) {
            continue;
        }
        match parsed.iter().find(|b| (&b.1, &b.2, b.3, b.4) == (&m.0, &m.1, m.2, m.3)) {
            Some(b) => {
                out.insert(m.clone(), DupKeys::of(&b.5));
            }
            None => {
                if let Some(id) = s.ix.code_chunk_at(&m.0, &m.1, m.2, m.3)? {
                    ids.push(id);
                    at.insert(id, m.clone());
                }
            }
        }
    }
    for (id, k) in s.ix.dup_keys_of(&ids)? {
        if let Some(m) = at.remove(&id) {
            out.insert(m, k);
        }
    }
    Ok(out)
}

/// The definitions of a base copy as the index would hold them for `dupes`.
fn bodies_of(path: &str, src: &[u8], min_tokens: usize) -> Vec<Body> {
    if src.len() as u64 > crate::index::MAX_FILE_BYTES || !crate::parse::is_indexable(path) {
        return Vec::new();
    }
    crate::parse::parse_file(path, &String::from_utf8_lossy(src))
        .chunks
        .into_iter()
        .filter(|c| CODE_KINDS.contains(&c.kind) && crate::text::tokenize2(&c.body, Some(crate::text::BODY_CAP)).0.len() >= min_tokens)
        .map(|c| (0, path.to_string(), c.qualname, c.start, c.end, c.text))
        .collect()
}

/// The changed definitions (added or edited, outside test files) from which no test file is
/// reached through the calls the walk found.
fn untested(i: &Impact) -> Vec<usize> {
    let mut up: HashMap<&str, Vec<&Hit>> = HashMap::new();
    let by_id: HashMap<i64, &Hit> = i.changed.iter().map(|c| &c.hit).chain(i.callers.iter().map(|c| &c.hit)).map(|h| (h.id, h)).collect();
    for (id, target) in &i.edges {
        if let Some(h) = by_id.get(id) {
            up.entry(target.as_str()).or_default().push(h);
        }
    }
    let reaches_test = |start: &Hit| {
        let mut seen = HashSet::from([start.loc()]);
        let mut queue = VecDeque::from([start]);
        while let Some(h) = queue.pop_front() {
            if crate::text::is_test_path(&h.path) {
                return true;
            }
            for c in up.get(h.loc().as_str()).into_iter().flatten() {
                if seen.insert(c.loc()) {
                    queue.push_back(c);
                }
            }
        }
        false
    };
    (0..i.changed.len())
        .filter(|&k| {
            let c = &i.changed[k];
            c.status != "deleted" && c.hit.id != 0 && !matches!(c.hit.kind.as_str(), "window" | "module" | "file") && !reaches_test(&c.hit)
        })
        .collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `s` as a Markdown code span, fenced with more backticks than it holds in a row.
fn code(s: &str) -> String {
    let s = shown(s);
    let run = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(run + 1);
    let pad = if run > 0 { " " } else { "" };
    format!("{fence}{pad}{s}{pad}{fence}")
}

/// A path percent-encoded for a URL, `/` kept.
fn url_path(p: &str) -> String {
    p.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Review {
    pub fn is_empty(&self) -> bool {
        self.impact.is_empty() && self.duplicates.is_empty()
    }

    /// The comment body: the marker, a line of counts, then each section that has items. With
    /// `link` (a URL that ends where a path follows, such as `https://github.com/o/r/blob/SHA`),
    /// each location links to its lines. Cut at `MAX_COMMENT` bytes with a line that says so.
    pub fn markdown(&self, link: Option<&str>) -> String {
        let i = &self.impact;
        let at = |path: &str, lines: Option<(i64, i64)>| {
            let text = match lines {
                Some((a, b)) => format!("{path}:{a}-{b}"),
                None => path.to_string(),
            };
            match link {
                Some(base) => {
                    let anchor = lines.map(|(a, b)| format!("#L{a}-L{b}")).unwrap_or_default();
                    format!("[{}]({}/{}{anchor})", code(&text), base.trim_end_matches('/'), url_path(path))
                }
                None => code(&text),
            }
        };
        let def = |h: &Hit| {
            if h.kind == "file" {
                at(&h.path, None)
            } else {
                format!("{} {} {}", at(&h.path, Some((h.start, h.end))), h.kind, code(if h.qualname.is_empty() { &h.name } else { &h.qualname }))
            }
        };
        let short = |c: &str| c.get(..12).unwrap_or(c).to_string();
        // a revision given as its commit id is named once, by its short form
        let named = |rev: &str, commit: &str| if commit.starts_with(rev) { code(&short(commit)) } else { format!("{} ({})", code(rev), short(commit)) };
        let mut counts = vec![
            plural(i.changed.len(), "changed definition", "changed definitions"),
            plural(i.callers.len(), "caller", "callers"),
            plural(i.tests.len(), "test file to run", "test files to run"),
            plural(self.duplicates.len(), "new duplicate", "new duplicates"),
        ];
        if !self.untested.is_empty() {
            counts.push(format!("{} no test reaches", self.untested.len()));
        }
        let head_commit = i.head.as_deref().unwrap_or_default();
        let mut lines = vec![
            MARKER.to_string(),
            format!("**scrutate review** of {} against {}", named(&self.head, head_commit), named(&i.against, &i.base)),
            String::new(),
            if self.is_empty() { "No changed definitions and no new duplicates.".to_string() } else { format!("{}.", counts.join(", ")) },
        ];
        let mut section = |title: String, items: Vec<String>| {
            if !items.is_empty() {
                lines.push(String::new());
                lines.push(format!("#### {title}"));
                lines.push(String::new());
                lines.extend(items);
            }
        };
        let untested: BTreeSet<usize> = self.untested.iter().copied().collect();
        section(
            "Changed definitions".into(),
            i.changed
                .iter()
                .enumerate()
                .map(|(k, c)| {
                    let mark = if untested.contains(&k) { ", no test reaches it" } else { "" };
                    let base_lines = if c.status == "deleted" && c.hit.kind != "file" { ", lines of the base" } else { "" };
                    format!("- {} ({}{base_lines}{mark})", def(&c.hit), c.status)
                })
                .collect(),
        );
        section(
            format!("Callers, up to {} away", plural(i.depth, "step", "steps")),
            i.callers
                .iter()
                .map(|c| {
                    let mark = if c.calls_deleted {
                        ", calls a deleted definition"
                    } else if c.undecided {
                        ", undecided"
                    } else {
                        ""
                    };
                    format!("- {} (distance {}{mark})", def(&c.hit), c.distance)
                })
                .collect(),
        );
        section(
            "Tests to run".into(),
            i.tests
                .iter()
                .map(|t| match &t.via {
                    Some(h) => format!(
                        "- {} (distance {}, through {})",
                        at(&t.path, None),
                        t.distance,
                        code(if h.qualname.is_empty() { &h.name } else { &h.qualname })
                    ),
                    None => format!("- {} (changed)", at(&t.path, None)),
                })
                .collect(),
        );
        let mut dupes: Vec<String> = self
            .duplicates
            .iter()
            .map(|g| {
                let members: Vec<String> = g.members.iter().map(|m| format!("{} {}", at(&m.0, Some((m.2, m.3))), code(&m.1))).collect();
                let how = if g.kind == "exact" { "exact".to_string() } else { format!("near, similarity {:.2}", g.similarity) };
                format!("- {how}, {} tokens: {}", g.tokens, members.join(", "))
            })
            .collect();
        if self.more_duplicates {
            dupes.push(format!("- (more than {MAX_DUPLICATES}: `scrutate review` lists the first {MAX_DUPLICATES})"));
        }
        section("New duplicates".into(), dupes);
        section("Notes".into(), i.notes.iter().map(|(p, n)| format!("- {}: {n}", at(p, None))).collect());
        if i.stopped {
            lines.push(String::new());
            lines.push(format!("_The walk stopped at {} callers._", crate::impact::MAX_CALLERS));
        }
        cap(lines, &i.against)
    }

    pub fn json(&self) -> Value {
        let mut v = self.impact.json();
        v["head"] = json!(self.head);
        v["head_commit"] = json!(self.impact.head);
        v["new_duplicates"] = serde_json::to_value(&self.duplicates).unwrap_or_default();
        v["untested"] = json!(self.untested.iter().map(|&k| self.impact.changed[k].hit.loc()).collect::<Vec<_>>());
        v
    }

    /// GitLab's Code Quality report: a list of issues, one per new duplicate group at the member
    /// the diff changed (`minor`), and one per changed definition no test reaches (`info`).
    pub fn gitlab(&self) -> Value {
        let i = &self.impact;
        let changed: HashSet<&str> = i.head_paths.iter().map(String::as_str).collect();
        let issue = |check: &str, description: String, severity: &str, path: &str, line: i64, identity: &str| {
            let fingerprint = crate::index::content_hash(format!("{check}\0{identity}").as_bytes());
            json!({"description": description, "check_name": check, "fingerprint": fingerprint, "severity": severity,
                "location": {"path": path, "lines": {"begin": line.max(1)}}})
        };
        let mut out = Vec::new();
        for g in &self.duplicates {
            let edited = |m: &&(String, String, i64, i64)| {
                i.changed.iter().any(|c| c.status != "deleted" && c.hit.path == m.0 && c.hit.start <= m.3 && m.2 <= c.hit.end)
            };
            let at = g.members.iter().find(edited).or_else(|| g.members.iter().find(|m| changed.contains(m.0.as_str()))).unwrap_or(&g.members[0]);
            let others: Vec<String> = g.members.iter().filter(|m| *m != at).map(|m| format!("{} at {}:{}-{}", m.1, m.0, m.2, m.3)).collect();
            let how = if g.kind == "exact" { "an exact duplicate".to_string() } else { format!("a near duplicate (similarity {:.2})", g.similarity) };
            let mut identity: Vec<String> = g.members.iter().map(|m| format!("{}\0{}", m.0, m.1)).collect();
            identity.sort();
            out.push(issue("scrutate-duplicate", format!("{} is {how} of {}", at.1, others.join(", ")), "minor", &at.0, at.2, &identity.join("\0")));
        }
        for &k in &self.untested {
            let h = &i.changed[k].hit;
            let name = if h.qualname.is_empty() { &h.name } else { &h.qualname };
            let why = format!("{name} changed ({}) and no test file reaches it within {} of callers", i.changed[k].status, plural(i.depth, "step", "steps"));
            out.push(issue("scrutate-untested-change", why, "info", &h.path, h.start, &format!("{}\0{name}", h.path)));
        }
        Value::Array(out)
    }
}

/// The lines joined, cut before the line that would pass `MAX_COMMENT` with the note that says so.
fn cap(lines: Vec<String>, base: &str) -> String {
    let note = format!("\n_Cut at {MAX_COMMENT} characters: `scrutate review --base {}` prints the whole report._\n", shown(base));
    let mut out = String::new();
    for l in &lines {
        if out.len() + l.len() + 1 + note.len() > MAX_COMMENT {
            out.push_str(&note);
            return out;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The search `new_duplicates` replaced, over every definition's text: the oracle it must agree with.
    /// The duplicate groups of the head that the base did not have. Only a group with a member in a
    /// file the diff changed can be new; the base's groups come from the same search over the head's
    /// definitions with the changed files replaced by a parse of their base copies. A head group is
    /// old when one base group holds a match for each of its members, each base member used once:
    /// the same definition (path and qualified name), or the same body text up to whitespace.
    fn new_duplicates_oracle(s: &Searcher, i: &Impact) -> Result<Vec<DupGroup>> {
        let (min, threshold) = (crate::ops::DUPES_MIN_TOKENS, crate::ops::DUPES_THRESHOLD);
        let changed: HashSet<&str> = i.head_paths.iter().chain(i.base_copies.iter().map(|c| &c.0)).map(String::as_str).collect();
        if changed.is_empty() {
            return Ok(Vec::new());
        }
        let rows = s.ix.bodies(CODE_KINDS, min)?;
        let head: Vec<DupGroup> = crate::intel::dupes_in(&rows.iter().collect::<Vec<_>>(), min, threshold, usize::MAX)
            .into_iter()
            .filter(|g| g.members.iter().any(|m| changed.contains(m.0.as_str())))
            .collect();
        if head.is_empty() {
            return Ok(head);
        }
        let parsed: Vec<Body> = i.base_copies.iter().flat_map(|(path, src)| bodies_of(path, src, min)).collect();
        let base_rows: Vec<&Body> = rows.iter().filter(|r| !changed.contains(r.1.as_str())).chain(&parsed).collect();
        let base = crate::intel::dupes_in(&base_rows, min, threshold, usize::MAX);
        // a changed file has a definition at the same place on both sides: one map per side
        let keys = |rs: &[&Body]| -> HashMap<(String, String, i64, i64), u64> {
            rs.iter().map(|r| ((r.1.clone(), r.2.clone(), r.3, r.4), crate::intel::body_key(&r.5))).collect()
        };
        let (head_keys, base_keys) = (keys(&rows.iter().collect::<Vec<_>>()), keys(&base_rows));
        let old = |g: &DupGroup| {
            base.iter().filter(|b| b.members.len() >= g.members.len()).any(|b| {
                let mut used = vec![false; b.members.len()];
                let mut left: Vec<&(String, String, i64, i64)> = Vec::new();
                for m in &g.members {
                    match (0..b.members.len()).find(|&j| !used[j] && b.members[j].0 == m.0 && b.members[j].1 == m.1) {
                        Some(j) => used[j] = true,
                        None => left.push(m),
                    }
                }
                left.into_iter().all(|m| {
                    let found = head_keys.get(m).and_then(|k| (0..b.members.len()).find(|&j| !used[j] && base_keys.get(&b.members[j]) == Some(k)));
                    found.map(|j| used[j] = true).is_some()
                })
            })
        };
        Ok(head.into_iter().filter(|g| !old(g)).collect())
    }

    use crate::intel::duplicates_tests::{files, tree, Spec};
    use proptest::prelude::*;

    /// A base tree and a head made from it: some definitions changed, maybe a new file.
    fn histories() -> impl Strategy<Value = (Spec, Spec)> {
        (files(), prop::collection::vec((any::<prop::sample::Index>(), 0..4usize, 0u8..6), 0..4), files()).prop_map(|(base, edits, extra)| {
            let mut head = base.clone();
            for (at, seed, how) in edits {
                let places: Vec<(usize, usize)> = head.iter().enumerate().flat_map(|(f, d)| (0..d.len()).map(move |k| (f, k))).collect();
                let (f, k) = places[at.index(places.len())];
                head[f][k] = (seed, how, head[f][k].2);
            }
            head.push(extra[0].clone());
            (base, head)
        })
    }

    fn git(root: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(root)
            .envs([("GIT_CONFIG_GLOBAL", "/dev/null"), ("GIT_CONFIG_NOSYSTEM", "1")])
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(root: &std::path::Path, spec: &Spec) -> String {
        for e in std::fs::read_dir(root).unwrap().flatten() {
            if e.path().extension().is_some_and(|x| x == "py") {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
        for (name, text) in tree(spec) {
            std::fs::write(root.join(name), text).unwrap();
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "--allow-empty", "-m", "x"]);
        git(root, &["rev-parse", "HEAD"])
    }

    #[test]
    fn a_third_copy_is_new_and_the_pair_the_base_had_is_not() {
        let root = std::env::temp_dir().join(format!("scrutate_review_third_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        let base: Spec = vec![vec![(0, 0, false), (0, 0, false), (1, 0, false), (1, 0, false)]];
        let base_sha = commit(&root, &base);
        // seed 1's pair is left as it was; seed 0 gets a third copy in a new file
        let mut head = base.clone();
        head.push(vec![(0, 0, false)]);
        let head_sha = commit(&root, &head);
        let mut ix = crate::index::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let s = Searcher::new(ix);
        let i = crate::impact::between(&s, &base_sha, &head_sha, 1).unwrap();
        let found = new_duplicates(&s, &i).unwrap();
        assert_eq!(found.iter().map(|g| g.members.len()).collect::<Vec<_>>(), [3], "{found:?}");
        assert_eq!(found, new_duplicates_oracle(&s, &i).unwrap());
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 32, failure_persistence: None, ..ProptestConfig::default() })]
        #[test]
        fn the_new_duplicates_are_those_the_old_search_found((base, head) in histories()) {
            let root = std::env::temp_dir().join(format!("scrutate_review_dups_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            git(&root, &["init", "-q"]);
            let base_sha = commit(&root, &base);
            let head_sha = commit(&root, &head);
            let mut ix = crate::index::Index::open(&root, crate::models::ModelSet::none()).unwrap();
            ix.update(false).unwrap();
            let s = Searcher::new(ix);
            let i = crate::impact::between(&s, &base_sha, &head_sha, 1).unwrap();
            prop_assert_eq!(new_duplicates(&s, &i).unwrap(), new_duplicates_oracle(&s, &i).unwrap());
            drop(s);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn a_code_span_outfences_the_backticks_inside() {
        assert_eq!(code("f"), "`f`");
        assert_eq!(code("a`b"), "`` a`b ``");
    }

    #[test]
    fn a_long_report_is_cut_with_a_note_under_the_limit() {
        let lines: Vec<String> = (0..20_000).map(|n| format!("- line {n}")).collect();
        let out = cap(lines, "main");
        assert!(out.len() <= MAX_COMMENT && out.ends_with("prints the whole report._\n"), "{}", out.len());
        assert_eq!(cap(vec!["a".into()], "main"), "a\n");
    }

    #[test]
    fn a_path_in_a_link_is_percent_encoded() {
        assert_eq!(url_path("src/a b#c.py"), "src/a%20b%23c.py");
    }
}
