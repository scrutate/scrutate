//! The resolver's store of parsed files stays near its cap in memory, not only in its weight.

#[cfg(target_os = "linux")]
fn rss() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: usize = status.lines().find_map(|l| l.strip_prefix("VmRSS:")).unwrap().trim().trim_end_matches(" kB").trim().parse().unwrap();
    kb << 10
}

/// A store filled past its cap with this crate's own Rust sources and a C file holds about its
/// cap in memory: what it weighs is what it takes, within a quarter.
#[cfg(target_os = "linux")]
#[test]
fn a_full_store_of_parsed_files_takes_about_its_cap_in_memory() {
    use std::path::Path;
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("parsed_memory_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    let mut paths = Vec::new();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for dir in [src.clone(), src.join("resolve")] {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".rs") {
                let rel = format!("src/{}{name}", if dir == src { "" } else { "resolve_" });
                std::fs::copy(e.path(), root.join(&rel)).unwrap();
                paths.push(rel);
            }
        }
    }
    let sample = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/sample.c")).unwrap();
    for i in 0..40 {
        let rel = format!("c{i}.c");
        std::fs::write(root.join(&rel), sample.repeat(200)).unwrap();
        paths.push(rel);
    }
    let lang = std::env::var("SCRUTATE_PROBE_LANG").unwrap_or_default();
    paths.retain(|p| lang.is_empty() || p.ends_with(&lang));
    let (repo, _) = scrutate::open_updated(&root, false).unwrap();
    let s = repo.into_searcher();
    let cap = 96 << 20;
    let before = rss();
    let probe = scrutate::resolve::ParsedProbe::fill(&s.ix, &paths, cap).unwrap();
    let grown = rss().saturating_sub(before);
    let (weight, text, nodes) = probe.held();
    println!(
        "held weight {weight}, text {text}, nodes {nodes}; RSS grew {grown}: {:.1} bytes per node beyond the text",
        (grown as f64 - text as f64) / nodes as f64
    );
    assert!(weight <= cap && weight > cap / 2, "the store is not near full: {weight} of {cap}");
    assert!(grown <= weight + weight / 4, "a store of weight {weight} took {grown} bytes");
    drop(probe);
    let _ = std::fs::remove_dir_all(&root);
}
