//! What a change does that a reviewer would want named: the findings of `impact`. Every front end
//! and `review` render this one list; none of them looks for findings of its own.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;

use crate::fmt::plural;
use crate::impact::Impact;
use crate::index::Body;
use crate::intel::{DupGroup, DupKeys, Focus, KeyRows};
use crate::search::{Hit, Searcher, CODE_KINDS};

/// One finding, at the definition the change touched.
pub struct Finding {
    pub path: String,
    pub start: i64,
    pub end: i64,
    /// the qualified name of the definition there
    pub name: String,
    pub what: What,
}

pub enum What {
    /// a duplicate group the base did not have; the finding is at its member the change touched
    Duplicate(DupGroup),
    /// no test file reaches the definition within `depth` steps of the callers the walk found
    Untested { status: &'static str, depth: usize },
}

impl Finding {
    /// A stable name: a new kind gets a new name, an existing one is never renamed.
    pub fn kind(&self) -> &'static str {
        match self.what {
            What::Duplicate(_) => "duplicate",
            What::Untested { .. } => "untested-change",
        }
    }

    /// A level of GitLab's Code Quality scale: info, minor, major, critical, blocker.
    pub fn severity(&self) -> &'static str {
        match self.what {
            What::Duplicate(_) => "minor",
            What::Untested { .. } => "info",
        }
    }

    /// One sentence, the finding's description in every rendering.
    pub fn text(&self) -> String {
        match &self.what {
            What::Duplicate(g) => {
                let how = if g.kind == "exact" { "an exact duplicate".to_string() } else { format!("a near duplicate (similarity {:.2})", g.similarity) };
                let others: Vec<String> = self.related().iter().map(|(path, name, start, end)| format!("{name} at {path}:{start}-{end}")).collect();
                format!("{} is {how} of {}", self.name, others.join(", "))
            }
            What::Untested { status, depth } => {
                format!("{} changed ({status}) and no test file reaches it within {} of callers", self.name, plural(*depth, "step", "steps"))
            }
        }
    }

    /// What stays the same while the finding's lines move: paths and names, never line numbers.
    pub fn identity(&self) -> String {
        match &self.what {
            What::Duplicate(g) => {
                let mut members: Vec<String> = g.members.iter().map(|(path, name, ..)| format!("{path}\0{name}")).collect();
                members.sort();
                members.join("\0")
            }
            What::Untested { .. } => format!("{}\0{}", self.path, self.name),
        }
    }

    /// The other places the finding names, as (path, qualified name, start, end).
    pub fn related(&self) -> Vec<(String, String, i64, i64)> {
        match &self.what {
            What::Duplicate(g) => g
                .members
                .iter()
                .filter(|m| (m.0.as_str(), m.1.as_str(), m.2, m.3) != (self.path.as_str(), self.name.as_str(), self.start, self.end))
                .cloned()
                .collect(),
            What::Untested { .. } => Vec::new(),
        }
    }
}

/// Duplicate groups a list holds at most.
pub const MAX_DUPLICATES: usize = 50;

/// The findings of a change: its new duplicate groups (largest first, at most `MAX_DUPLICATES`,
/// and whether more were cut), then the changed definitions no test reaches, in `changed` order.
pub(crate) fn find(s: &Searcher, i: &Impact) -> Result<(Vec<Finding>, bool)> {
    let mut groups = new_duplicates(s, i)?;
    let more = groups.len() > MAX_DUPLICATES;
    groups.truncate(MAX_DUPLICATES);
    let head: HashSet<&str> = i.head_paths.iter().map(String::as_str).collect();
    let mut out: Vec<Finding> = groups
        .into_iter()
        .map(|g| {
            // the member a changed definition overlaps, else one in a changed file
            let edited = |m: &&(String, String, i64, i64)| {
                i.changed.iter().any(|c| c.status != "deleted" && c.hit.path == m.0 && c.hit.start <= m.3 && m.2 <= c.hit.end)
            };
            let at = g.members.iter().find(edited).or_else(|| g.members.iter().find(|m| head.contains(m.0.as_str()))).unwrap_or(&g.members[0]).clone();
            Finding { path: at.0, name: at.1, start: at.2, end: at.3, what: What::Duplicate(g) }
        })
        .collect();
    out.extend(untested(i).into_iter().map(|k| {
        let h = &i.changed[k].hit;
        let name = if h.qualname.is_empty() { &h.name } else { &h.qualname };
        Finding { path: h.path.clone(), start: h.start, end: h.end, name: name.clone(), what: What::Untested { status: i.changed[k].status, depth: i.depth } }
    }));
    Ok((out, more))
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

/// The changed code definitions (`CODE_KINDS`, added or edited, outside test files) from which no
/// test file is reached through the calls the walk found.
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
            c.status != "deleted" && c.hit.id != 0 && CODE_KINDS.contains(&c.hit.kind.as_str()) && !reaches_test(&c.hit)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The search `new_duplicates` replaced, over every definition's text: the oracle it must agree with.
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
    fn a_finding_names_its_kind_severity_sentence_identity_and_the_places_it_points_to() {
        let group = DupGroup {
            kind: "near",
            similarity: 0.9,
            members: vec![("lib/dups.py".into(), "unique".into(), 1, 12), ("lib/fresh.py".into(), "copy_of_unique".into(), 3, 14)],
            tokens: 41,
        };
        let dup = Finding { path: "lib/fresh.py".into(), start: 3, end: 14, name: "copy_of_unique".into(), what: What::Duplicate(group) };
        assert_eq!((dup.kind(), dup.severity()), ("duplicate", "minor"));
        assert_eq!(dup.text(), "copy_of_unique is a near duplicate (similarity 0.90) of unique at lib/dups.py:1-12");
        assert_eq!(dup.identity(), "lib/dups.py\0unique\0lib/fresh.py\0copy_of_unique");
        assert_eq!(dup.related(), [("lib/dups.py".to_string(), "unique".to_string(), 1, 12)]);

        let untested = Finding { path: "lib/dups.py".into(), start: 13, end: 20, name: "lonely".into(), what: What::Untested { status: "added", depth: 1 } };
        assert_eq!((untested.kind(), untested.severity()), ("untested-change", "info"));
        assert_eq!(untested.text(), "lonely changed (added) and no test file reaches it within 1 step of callers");
        assert_eq!(untested.identity(), "lib/dups.py\0lonely");
        assert!(untested.related().is_empty());
    }
}
