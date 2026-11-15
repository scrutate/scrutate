//! Whether ripgrep is on PATH, for every test that runs it or compares with it. CI installs it and
//! sets SCRUTATE_TEST_NEEDS_RG (tools/ci_ripgrep.sh), so there a missing rg fails the test instead
//! of skipping it.

use std::process::{Command, Stdio};

pub fn have_rg() -> bool {
    let ok = Command::new("rg").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    assert!(ok || std::env::var_os("SCRUTATE_TEST_NEEDS_RG").is_none(), "rg is not installed, and SCRUTATE_TEST_NEEDS_RG says it must be");
    if !ok {
        eprintln!("skipped: rg is not installed");
    }
    ok
}
