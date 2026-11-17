//! A file of one long line under the split size is held once, not in a stream buffer grown to
//! three times the line, and `--json` writes a line without another copy of it.
#![cfg(unix)]

use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

#[repr(C)]
struct Rusage {
    utime: [i64; 2],
    stime: [i64; 2],
    maxrss: i64,
    rest: [i64; 13],
}

unsafe extern "C" {
    fn wait4(pid: i32, status: *mut i32, options: i32, usage: *mut Rusage) -> i32;
}

/// `scrutate grep ARGS FILE`'s peak resident size in bytes and how many bytes it printed.
#[allow(clippy::zombie_processes)] // wait4 reaps it
fn peak(args: &[&str], file: &std::path::Path, out: &std::path::Path) -> (u64, u64) {
    let child = Command::new(BIN)
        .arg("grep")
        .args(args)
        .arg(file)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(out).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (mut status, mut u) = (0, Rusage { utime: [0; 2], stime: [0; 2], maxrss: 0, rest: [0; 13] });
    // SAFETY: wait4 reaps this child and writes its struct rusage, whose first fields are two
    // timevals and ru_maxrss
    assert_eq!(unsafe { wait4(child.id() as i32, &mut status, 0, &mut u) }, child.id() as i32);
    assert_eq!(status, 0, "{args:?}");
    // macOS counts bytes, Linux KiB
    let rss = if cfg!(target_os = "macos") { u.maxrss as u64 } else { u.maxrss as u64 * 1024 };
    (rss, std::fs::metadata(out).unwrap().len())
}

#[test]
fn a_long_line_is_held_once() {
    let dir = std::env::temp_dir().join(format!("scrutate_one_line_memory_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out");
    let size: usize = 32 << 20;
    let mut line: Vec<u8> = b"alpha beta gamma delta ".iter().copied().cycle().take(size - 20).collect();
    line.extend_from_slice(b"NEEDLE_xyz_resume(\n");
    std::fs::write(dir.join("one.txt"), &line).unwrap();
    // a short first line: the stream reader takes the long one
    std::fs::write(dir.join("two.txt"), [b"short\n".as_slice(), &line].concat()).unwrap();
    std::fs::write(dir.join("small.txt"), b"tiny NEEDLE_xyz_resume(\n").unwrap();
    let floor = peak(&["-e", r"NEEDLE_\w+\("], &dir.join("small.txt"), &out).0;
    let over = |args: &[&str], file: &str| {
        let (rss, printed) = peak(args, &dir.join(file), &out);
        assert!(printed >= size as u64, "{args:?} {file}: {printed} bytes");
        rss.saturating_sub(floor) as f64 / size as f64
    };
    let regex = [&["-e", r"NEEDLE_\w+\("][..], &["--json", "-e", r"NEEDLE_\w+\("], &["--json", "-F", "NEEDLE_xyz"]];
    // one line: the file once, and for --json the record's copy of the line
    for (args, most) in regex.iter().zip([1.3, 2.3, 2.3]) {
        let x = over(args, "one.txt");
        assert!(x < most, "{args:?} one.txt: {x:.2} times the line over the floor (at most {most})");
    }
    // the stream's buffer holds the line either way; --json adds the record's copy and no other
    let plain = over(regex[0], "two.txt");
    for args in &regex[1..] {
        let x = over(args, "two.txt") - plain;
        assert!(x < 1.4, "{args:?} two.txt: --json holds {x:.2} times the line more than plain output");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
