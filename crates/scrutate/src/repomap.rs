//! The repo map: the files of the repository ranked by PageRank over the edges the index's names
//! give between files (`Names::file_edges`: calls and imports), each with its top definitions.
//! The same index gives the same bytes: the iteration count is fixed, and a tie goes to the file
//! with more edges in and out, then by path.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Result;

use crate::index::Symbol;
use crate::names::{Hub, Names};
use crate::search::Searcher;

pub const DAMPING: f64 = 0.85;
pub const ITERATIONS: usize = 50;
/// Definitions one file of the map lists.
pub const DEFINITIONS: usize = 5;

/// One file of the map: its PageRank score and its top definitions, each with its share of the
/// calls of its name.
#[derive(Clone, Debug)]
pub struct MapFile {
    pub path: String,
    pub score: f64,
    pub definitions: Vec<(Symbol, i64)>,
}

/// The files under `under` (a directory or file from the root; every file when None) that define
/// code, highest score first. A file's definitions come most called first (the calls of the
/// name split among the files that define it), then outer before inner, longer before shorter,
/// and by line; a name defined several times in the file (overloads) is listed once.
pub fn map(s: &Searcher, under: Option<&str>) -> Result<Vec<MapFile>> {
    let mut files = s.ix.files()?;
    files.sort();
    let (edges, hubs) = Names::new(s)?.file_edges(&files)?;
    let scores = pagerank(files.len(), &edges, &hubs);
    // edges in and out: of two files nobody points at, the one that points somewhere goes first
    let mut linked = vec![0.0; files.len()];
    for &(a, b, w) in &edges {
        linked[a] += w;
        linked[b] += w;
    }
    for h in &hubs {
        let share = h.from.iter().map(|&(_, w)| w).sum::<f64>() / h.to.len() as f64;
        for &(a, w) in &h.from {
            linked[a] += w;
        }
        for &t in &h.to {
            linked[t] += share;
        }
    }
    let under = under.map(|p| p.trim_start_matches("./").trim_end_matches('/')).filter(|p| !p.is_empty() && *p != ".");
    let kinds: Vec<&str> = crate::search::CODE_KINDS.iter().copied().chain(["macro"]).collect();
    let mut definers: HashMap<String, f64> = HashMap::new();
    for (name, _) in s.ix.definers(&kinds)? {
        *definers.entry(name).or_default() += 1.0;
    }
    let mut listed = Vec::new();
    for ((path, score), linked) in files.into_iter().zip(scores).zip(linked) {
        if under.is_some_and(|u| path != u && !path.starts_with(&format!("{u}/"))) {
            continue;
        }
        let defs: Vec<Symbol> = s.ix.symbols_in(&path)?.into_iter().filter(|d| kinds.contains(&d.kind.as_str())).collect();
        if !defs.is_empty() {
            listed.push((path, score, linked, defs));
        }
    }
    let names: BTreeSet<&String> = listed.iter().flat_map(|l| l.3.iter().map(|d| &d.name)).collect();
    let calls = s.ix.call_counts(&names.into_iter().collect::<Vec<_>>())?;
    let mut out = Vec::new();
    for (path, score, linked, defs) in listed {
        let share = |d: &Symbol| calls.get(&d.name).map_or(0.0, |&c| c as f64 / definers.get(&d.name).copied().unwrap_or(1.0));
        let mut definitions: Vec<(f64, Symbol)> = defs.into_iter().map(|d| (share(&d), d)).collect();
        let place = |d: &Symbol| (d.qualname.matches('.').count(), Reverse(d.end - d.start), d.start);
        definitions.sort_by(|(a, x), (b, y)| b.total_cmp(a).then_with(|| place(x).cmp(&place(y))));
        let mut seen = HashSet::new();
        definitions.retain(|(_, d)| seen.insert(d.qualname.clone()));
        let definitions = definitions.into_iter().take(DEFINITIONS).map(|(c, d)| (d, c.round() as i64)).collect();
        out.push((MapFile { path, score, definitions }, linked));
    }
    // scores closer than rounding are a tie
    let near = |x: f64| Reverse((x * 1e9).round() as i64);
    out.sort_by_cached_key(|(f, linked)| (near(f.score), near(*linked), f.path.clone()));
    Ok(out.into_iter().map(|(f, _)| f).collect())
}

/// PageRank of `n` nodes over weighted `edges` (from, to, weight) and `hubs`, each the edges from
/// every node of `from` to every node of `to` with the node's weight split evenly among `to`: a
/// node without edges out shares its score among all.
fn pagerank(n: usize, edges: &[(usize, usize, f64)], hubs: &[Hub]) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let mut out_weight = vec![0.0; n];
    for &(a, _, w) in edges {
        out_weight[a] += w;
    }
    for &(a, w) in hubs.iter().flat_map(|h| &h.from) {
        out_weight[a] += w;
    }
    let mut score = vec![1.0 / n as f64; n];
    for _ in 0..ITERATIONS {
        let dangling: f64 = (0..n).filter(|&i| out_weight[i] == 0.0).map(|i| score[i]).sum();
        let mut next = vec![(1.0 - DAMPING) / n as f64 + DAMPING * dangling / n as f64; n];
        for &(a, b, w) in edges {
            next[b] += DAMPING * score[a] * w / out_weight[a];
        }
        for h in hubs {
            let each = DAMPING * h.from.iter().map(|&(a, w)| score[a] * w / out_weight[a]).sum::<f64>() / h.to.len() as f64;
            for &t in &h.to {
                next[t] += each;
            }
        }
        score = next;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` files defining a method `get` and `n` files calling `d.get(1)`, indexed.
    fn common_name_tree(name: &str, n: usize) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_repomap_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("defs")).unwrap();
        std::fs::create_dir_all(root.join("calls")).unwrap();
        for i in 0..n {
            std::fs::write(root.join(format!("defs/d{i}.py")), format!("class D{i}:\n    def get(self, x):\n        return x + {i}\n")).unwrap();
            std::fs::write(root.join(format!("calls/c{i}.py")), format!("def use{i}(d):\n    return d.get({i})\n")).unwrap();
        }
        root
    }

    #[test]
    fn a_name_many_files_define_costs_its_callers_and_definers_not_their_product() {
        let root = common_name_tree("product", 60);
        let (mut repo, _) = crate::open_updated(&root, true).unwrap();
        let (n, (edges, hubs)) = repo
            .recovering(|s| {
                let mut files = s.ix.files()?;
                files.sort();
                Ok((files.len(), Names::new(s)?.file_edges(&files)?))
            })
            .unwrap();
        // 60 callers times 60 definers would be 3,600 edges
        assert!(edges.len() < 1000, "{} edges", edges.len());
        assert_eq!(hubs.iter().map(|h| (h.from.len(), h.to.len())).collect::<Vec<_>>(), [(60, 60)]);
        assert_close(&pagerank(n, &edges, &hubs), &pagerank(n, &[edges.clone(), expanded(&hubs)].concat(), &[]));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The edges a hub stands for.
    fn expanded(hubs: &[Hub]) -> Vec<(usize, usize, f64)> {
        hubs.iter().flat_map(|h| h.from.iter().flat_map(move |&(a, w)| h.to.iter().map(move |&t| (a, t, w / h.to.len() as f64)))).collect()
    }

    fn assert_close(a: &[f64], b: &[f64]) {
        assert_eq!(a.len(), b.len());
        assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-12), "{a:?}\n{b:?}");
        assert!((a.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{a:?}");
    }

    #[test]
    fn a_hub_ranks_as_the_edges_it_stands_for() {
        // a few graphs mixing edges, hubs that share nodes with them, and a node with no edge out
        let mut seed = 7u64;
        let mut next = |m: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % m
        };
        for _ in 0..20 {
            let n = 3 + next(30);
            let edges: Vec<_> = (0..next(60)).map(|_| (next(n), next(n), 1.0 + next(4) as f64)).collect();
            let hubs: Vec<Hub> = (0..1 + next(3))
                .map(|_| Hub { from: (0..1 + next(n)).map(|_| (next(n), 1.0 + next(3) as f64)).collect(), to: (0..1 + next(n)).map(|_| next(n)).collect() })
                .collect();
            assert_close(&pagerank(n, &edges, &hubs), &pagerank(n, &[edges.clone(), expanded(&hubs)].concat(), &[]));
        }
    }

    #[test]
    fn a_node_others_point_at_ranks_above_one_nobody_does() {
        // 1..=3 point at 0; 4 stands alone
        let s = pagerank(5, &[(1, 0, 1.0), (2, 0, 1.0), (3, 0, 2.0)], &[]);
        assert!(s[0] > s[1] && s[1] == s[4], "{s:?}");
        assert!((s.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{s:?}");
    }
}
