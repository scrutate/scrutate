//! The spans of a fuzzy search cost memory in proportion to the line, but not eight bytes per
//! byte of it: on one long line, `--json` (which prints spans) with `--fuzzy` or `--fuzzy-regex`
//! peaks at most one byte per byte of the line above the same search without fuzziness. The only
//! test in this file, so no other test's child shares the process's peak of its children.
#![cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]

use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

/// The largest peak resident size of any child this process has waited for, in bytes.
fn children_peak() -> u64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct it is given
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut u) }, 0);
    let rss = u.ru_maxrss as u64;
    if cfg!(target_os = "macos") { rss } else { rss * 1024 }
}

#[test]
fn fuzzy_spans_on_a_long_line_cost_at_most_a_byte_per_byte_of_it() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fuzzy_memory_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // one 16 MB line of words, with a call of the pattern every 120 words or so
    let mut line = String::with_capacity(16 << 20);
    let mut x = 7u64;
    while line.len() < 16 << 20 {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        if x >> 57 == 0 {
            line.push_str("kmalloc(size); ");
        } else {
            line.push_str(["alpha ", "beta_gamma ", "delta(x) ", "eps, ", "zeta_eta_theta "][(x >> 40) as usize % 5]);
        }
    }
    line.push('\n');
    let file = dir.join("big.js");
    std::fs::write(&file, &line).unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(BIN).args(args).arg(&file).env("SCRUTATE_REDACT", "off").stdout(Stdio::null()).stderr(Stdio::piped()).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    // the plainest run first: the peak of the children never falls, so each later reading is the
    // largest so far
    run(&["grep", "--json", "kmalloc"]);
    let plain = children_peak();
    let bound = plain + line.len() as u64;
    for args in [&["grep", "--json", "--fuzzy=1", "kmalloc"][..], &["grep", "--json", "--fuzzy-regex=1", "km[a-z]lloc"][..]] {
        run(args);
        let peak = children_peak();
        assert!(peak <= bound, "{args:?}: peak {} MB against {} MB for the same search without fuzziness (bound {} MB)", peak >> 20, plain >> 20, bound >> 20);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
