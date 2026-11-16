//! Memory an answer frees on a thread of its own goes back to the system once the answer is out.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn rss() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: usize = status.lines().find_map(|l| l.strip_prefix("VmRSS:")).unwrap().trim().trim_end_matches(" kB").trim().parse().unwrap();
    kb << 10
}

/// Answers on threads of their own, each freeing 64 MB of small blocks and keeping one, as a
/// `serve` call does with the trees it drops and the one it keeps: once memory is given back after
/// each, the process holds about what it held before them.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn memory_answers_freed_on_their_own_threads_goes_back() {
    use std::time::{Duration, Instant};
    let before = rss();
    let mut kept = Vec::new();
    for round in 0..8 {
        let answers: Vec<_> = (0..4)
            .map(|t| {
                std::thread::spawn(move || {
                    let blocks: Vec<Vec<u8>> = (0..16_000).map(|i| vec![(round + t + i) as u8; 4000]).collect();
                    let keep = vec![1u8; 4000];
                    drop(blocks);
                    keep
                })
            })
            .collect();
        kept.extend(answers.into_iter().map(|a| a.join().unwrap()));
        scrutate::memory::give_back_memory(Instant::now() - Duration::from_secs(2));
    }
    let after = rss();
    println!("RSS before {before}, after {after}, {} blocks kept", kept.len());
    assert!(after < before + (32 << 20), "the answers' freed memory stayed: {before} before, {after} after");
}
