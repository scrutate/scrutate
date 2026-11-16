//! The store of parsed files weighs each grammar's trees by what they take, in the grammar where a
//! node takes the most: what it weighs is still what it takes, within a quarter.

#[cfg(target_os = "linux")]
fn rss() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: usize = status.lines().find_map(|l| l.strip_prefix("VmRSS:")).unwrap().trim().trim_end_matches(" kB").trim().parse().unwrap();
    kb << 10
}

/// Copies `files` into a fresh repository at `root` and fills a store of weight `cap` with them:
/// (weight held, bytes RSS grew, text bytes, tree nodes).
#[cfg(target_os = "linux")]
fn fill(root: &std::path::Path, files: &[(String, String)], cap: usize) -> (usize, usize, usize, usize) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root).unwrap();
    for (rel, body) in files {
        std::fs::write(root.join(rel), body).unwrap();
    }
    let paths: Vec<String> = files.iter().map(|(rel, _)| rel.clone()).collect();
    let (repo, _) = scrutate::open_updated(root, false).unwrap();
    let s = repo.into_searcher();
    let before = rss();
    let probe = scrutate::resolve::ParsedProbe::fill(&s.ix, &paths, cap).unwrap();
    let grown = rss().saturating_sub(before);
    let (weight, text, nodes) = probe.held();
    drop(probe);
    drop(s);
    let _ = std::fs::remove_dir_all(root);
    (weight, grown, text, nodes)
}

/// C# methods made of arithmetic over calls, fields and indexing take more memory a node than
/// any other code measured.
#[cfg(target_os = "linux")]
fn expression_heavy_csharp(file: usize) -> String {
    let mut src = format!("namespace Gen.N{file}\n{{\n    public class C{file}\n    {{\n");
    for m in 0..30 {
        src.push_str(&format!("        public int Method{m}(int a0, string b0, object c0)\n        {{\n"));
        for k in 0..12 {
            src.push_str(&format!(
                "            var v{k} = a{}.Call{}(b{}, c[{k}]) + d.Field{} * (e{} - {k}) ?? f{}.Prop;\n",
                k % 7,
                k % 5,
                k % 3,
                k % 9,
                k % 4,
                k % 6
            ));
        }
        src.push_str(&format!("            return v{};\n        }}\n\n", m % 12));
    }
    src.push_str("    }\n}\n");
    src
}

#[cfg(target_os = "linux")]
#[test]
fn a_full_store_of_the_heaviest_csharp_takes_about_its_cap_in_memory() {
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("parsed_memory_cs_{}", std::process::id()));
    let files: Vec<(String, String)> = (0..60).map(|i| (format!("f{i}.cs"), expression_heavy_csharp(i))).collect();
    let cap = 64 << 20;
    let (weight, grown, text, nodes) = fill(&root, &files, cap);
    println!(
        "held weight {weight}, text {text}, nodes {nodes}; RSS grew {grown}: {:.1} bytes a node beyond the text",
        (grown as f64 - text as f64) / nodes as f64
    );
    assert!(weight <= cap && weight > cap / 2, "the store is not near full: {weight} of {cap}");
    assert!(grown <= weight + weight / 4, "a store of weight {weight} took {grown} bytes");
}

/// Measures the memory a node of a grammar's trees takes, for the figures `Parsed::weight` uses:
/// `PROBE_DIR=<a tree> PROBE_EXT=cs cargo test --release -p scrutate --test parsed_memory_by_grammar
/// -- --ignored --nocapture`. Each file under 1 MB with that extension is parsed and kept.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "a measurement"]
fn bytes_a_node_of_a_grammar() {
    let from = std::path::PathBuf::from(std::env::var("PROBE_DIR").expect("PROBE_DIR"));
    let ext = std::env::var("PROBE_EXT").expect("PROBE_EXT");
    let mut files = Vec::new();
    let mut dirs = vec![from];
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == ext.as_str()) && e.metadata().is_ok_and(|m| m.len() < 1 << 20) {
                if let Ok(body) = std::fs::read_to_string(&p) {
                    files.push((format!("f{}.{ext}", files.len()), body));
                }
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("parsed_memory_probe_{}", std::process::id()));
    let (weight, grown, text, nodes) = fill(&root, &files, 4 << 30);
    println!(
        "{ext}: {} files, text {text}, nodes {nodes}, weight {weight}, RSS grew {grown}: {:.1} bytes a node beyond the text, {:.3} of the weight",
        files.len(),
        (grown as f64 - text as f64) / nodes as f64,
        grown as f64 / weight as f64
    );
}
