//! A pull request report: what `impact` finds between a base and a head commit, findings
//! included. It renders that answer as the Markdown of a PR comment, as JSON, and as a GitLab
//! Code Quality report, and looks for nothing of its own.

use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::{Value, json};

use crate::findings::{MAX_DUPLICATES, What};
use crate::fmt::{plural, shown};
use crate::impact::Impact;
use crate::intel::DupGroup;
use crate::search::{Hit, Searcher};

/// The first line of the comment: the GitHub Action finds its earlier comment by it.
pub const MARKER: &str = "<!-- scrutate-review -->";
/// The longest comment body GitHub takes, in characters.
pub const MAX_COMMENT: usize = 65_536;

pub struct Review {
    pub impact: Impact,
    /// the head as given (`HEAD` by default)
    pub head: String,
}

/// The report of `head` (a revision; `HEAD` by default) against `base`, on the index of the
/// working tree, which should be the head checked out.
pub fn review(s: &Searcher, base: &str, head: Option<&str>, depth: usize) -> Result<Review> {
    let head = head.unwrap_or("HEAD").to_string();
    let impact = crate::impact::between(s, base, &head, depth)?;
    Ok(Review { impact, head })
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
        self.impact.is_empty() && self.impact.findings.is_empty()
    }

    /// The new duplicate groups of the findings, largest first.
    fn duplicates(&self) -> Vec<&DupGroup> {
        self.impact.findings.iter().filter_map(|f| if let What::Duplicate(g) = &f.what { Some(g) } else { None }).collect()
    }

    /// The (path, start, end, name) of each changed definition no test reaches.
    fn untested(&self) -> Vec<(&str, i64, i64, &str)> {
        let untested = self.impact.findings.iter().filter(|f| matches!(f.what, What::Untested { .. }));
        untested.map(|f| (f.path.as_str(), f.start, f.end, f.name.as_str())).collect()
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
        let unread = i.changed.iter().filter(|c| c.not_indexed.is_some()).count();
        let mut counts = vec![
            plural(i.changed.len() - unread, "changed definition", "changed definitions"),
            plural(i.callers.len(), "caller", "callers"),
            plural(i.tests.len(), "test file to run", "test files to run"),
            plural(self.duplicates().len(), "new duplicate", "new duplicates"),
        ];
        if unread > 0 {
            counts.insert(1, plural(unread, "changed file not indexed (over 1 MB)", "changed files not indexed (over 1 MB)"));
        }
        let untested = self.untested();
        if !untested.is_empty() {
            counts.push(format!("{} no test reaches", untested.len()));
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
        let untested: BTreeSet<(&str, i64, i64, &str)> = untested.into_iter().collect();
        section(
            "Changed definitions".into(),
            i.changed
                .iter()
                .map(|c| {
                    let name = if c.hit.qualname.is_empty() { &c.hit.name } else { &c.hit.qualname };
                    let reached = c.status == "deleted" || !untested.contains(&(c.hit.path.as_str(), c.hit.start, c.hit.end, name.as_str()));
                    let mark = if reached { "" } else { ", no test reaches it" };
                    let base_lines = if c.status == "deleted" && c.hit.kind != "file" { ", lines of the base" } else { "" };
                    let left = c.not_indexed.map(|why| format!("; not indexed: {why}")).unwrap_or_default();
                    format!("- {} ({}{base_lines}{mark}{left})", def(&c.hit), c.status)
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
            .duplicates()
            .into_iter()
            .map(|g| {
                let members: Vec<String> = g.members.iter().map(|m| format!("{} {}", at(&m.0, Some((m.2, m.3))), code(&m.1))).collect();
                let how = if g.kind == "exact" { "exact".to_string() } else { format!("near, similarity {:.2}", g.similarity) };
                format!("- {how}, {} tokens: {}", g.tokens, members.join(", "))
            })
            .collect();
        if i.more_findings {
            dupes.push(format!("- (more than {MAX_DUPLICATES}: `scrutate review` lists the first {MAX_DUPLICATES})"));
        }
        section("New duplicates".into(), dupes);
        section("Notes".into(), i.notes.iter().map(|(p, n)| format!("- {}: {}", at(p, None), n.text(code))).collect());
        // the stopped line explains a comment that names no untested change: the cut keeps it
        let last = if i.stopped { vec![String::new(), format!("_The walk stopped at {} callers._", crate::impact::MAX_CALLERS)] } else { Vec::new() };
        cap(lines, &last, &i.against, &self.head, i.depth)
    }

    pub fn json(&self) -> Value {
        let mut v = self.impact.json();
        v["head"] = json!(self.head);
        v["head_commit"] = json!(self.impact.head);
        v["new_duplicates"] = serde_json::to_value(self.duplicates()).unwrap_or_default();
        v["untested"] = json!(self.untested().iter().map(|(path, start, end, _)| format!("{path}:{start}-{end}")).collect::<Vec<_>>());
        v
    }

    /// GitLab's Code Quality report: one issue per finding, its check `scrutate-` and its kind, and a
    /// fingerprint of its kind and identity, so GitLab follows it while its lines move.
    pub fn gitlab(&self) -> Value {
        let found = &self.impact.findings;
        let keys: Vec<String> = found.iter().map(|f| format!("scrutate-{}\0{}", f.kind(), f.identity())).collect();
        let issue = |(i, f): (usize, &crate::findings::Finding)| {
            // overloads share a qualified name: findings that share a key add their order by line,
            // so each gets a fingerprint of its own and a lone one keeps the one it had
            let same: Vec<usize> = (0..found.len()).filter(|&j| keys[j] == keys[i]).collect();
            let key = match same.len() {
                1 => keys[i].clone(),
                _ => format!("{}\0#{}", keys[i], same.iter().filter(|&&j| (found[j].start, j) < (f.start, i)).count() + 1),
            };
            json!({"description": f.text(), "check_name": format!("scrutate-{}", f.kind()), "fingerprint": crate::index::content_hash(key.as_bytes()),
                "severity": f.severity(), "location": {"path": f.path, "lines": {"begin": f.start.max(1)}}})
        };
        Value::Array(found.iter().enumerate().map(issue).collect())
    }
}

/// The lines joined, then `last`, which is always kept: cut before the line that would pass
/// `MAX_COMMENT`, with the note that says so and names the command that prints the whole report:
/// the same comparison and depth as JSON, which is never cut.
fn cap(lines: Vec<String>, last: &[String], base: &str, head: &str, depth: usize) -> String {
    let and_head = if head == "HEAD" { String::new() } else { format!(" --head {head}") };
    let and_depth = if depth == crate::impact::DEPTH { String::new() } else { format!(" --depth {depth}") };
    let whole = format!("scrutate review --base {base}{and_head}{and_depth} --format json");
    let note = format!("\n_Cut at {MAX_COMMENT} characters: {} prints the whole report._\n", code(&whole));
    let tail: String = last.iter().map(|l| format!("{l}\n")).collect();
    let mut out = String::new();
    for l in &lines {
        if out.len() + l.len() + 1 + tail.len() + note.len() > MAX_COMMENT {
            out.push_str(&tail);
            out.push_str(&note);
            return out;
        }
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&tail);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_span_outfences_the_backticks_inside() {
        assert_eq!(code("f"), "`f`");
        assert_eq!(code("a`b"), "`` a`b ``");
    }

    #[test]
    fn a_long_report_is_cut_with_a_note_under_the_limit() {
        let lines: Vec<String> = (0..20_000).map(|n| format!("- line {n}")).collect();
        let out = cap(lines, &[], "main", "HEAD", crate::impact::DEPTH);
        assert!(out.len() <= MAX_COMMENT && out.ends_with("prints the whole report._\n"), "{}", out.len());
        assert!(out.contains("`scrutate review --base main --format json` prints"), "{}", &out[out.len() - 200..]);
        assert_eq!(cap(vec!["a".into()], &[], "main", "HEAD", 1), "a\n");
    }

    #[test]
    fn a_path_in_a_link_is_percent_encoded() {
        assert_eq!(url_path("src/a b#c.py"), "src/a%20b%23c.py");
    }
}
