//! The executable's lazy-binding stubs (`src/lazy_bind.rs`), linked into this test binary so its
//! own calls to the listed system functions go through them.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

#[path = "../src/lazy_bind.rs"]
mod lazy_bind;

use std::ffi::{CStr, c_char, c_int, c_void};

unsafe extern "C" {
    fn snprintf(buf: *mut c_char, n: usize, fmt: *const c_char, ...) -> c_int;
    fn qsort(base: *mut c_void, n: usize, width: usize, cmp: extern "C" fn(*const c_void, *const c_void) -> c_int);
    fn powf(x: f32, y: f32) -> f32;
    fn getpid() -> c_int;
    fn fork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn _exit(code: c_int) -> !;
}

#[test]
fn a_variadic_call_keeps_its_stack_arguments() {
    let mut buf = [0 as c_char; 128];
    let n = unsafe {
        snprintf(
            buf.as_mut_ptr(),
            buf.len(),
            c"%d %s %.2f %ld %d %d %d %d %d %.1f".as_ptr(),
            7 as c_int,
            c"x".as_ptr(),
            2.5f64,
            1i64 << 40,
            1 as c_int,
            2 as c_int,
            3 as c_int,
            4 as c_int,
            5 as c_int,
            9.5f64,
        )
    };
    let text = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
    assert_eq!(text, "7 x 2.50 1099511627776 1 2 3 4 5 9.5");
    assert_eq!(n as usize, text.len());
}

#[test]
fn float_and_integer_registers_reach_the_function() {
    assert_eq!(unsafe { powf(2.0, 10.0) }, 1024.0);
    extern "C" fn by_value(a: *const c_void, b: *const c_void) -> c_int {
        let (a, b) = unsafe { (*a.cast::<u32>(), *b.cast::<u32>()) };
        a.cmp(&b) as c_int
    }
    let mut v = [5u32, 3, 9, 1, 7];
    unsafe { qsort(v.as_mut_ptr().cast(), v.len(), 4, by_value) };
    assert_eq!(v, [1, 3, 5, 7, 9]);
}

#[test]
fn a_panic_unwinds_and_runs_destructors() {
    struct Flag<'a>(&'a std::sync::atomic::AtomicBool);
    impl Drop for Flag<'_> {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let dropped = std::sync::atomic::AtomicBool::new(false);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _flag = Flag(&dropped);
        panic!("unwind through the stubs");
    }));
    assert!(r.is_err());
    assert!(dropped.load(std::sync::atomic::Ordering::Relaxed));
    // the backtrace walks the stack and asks dyld for the loaded images
    assert!(!std::backtrace::Backtrace::force_capture().to_string().is_empty());
}

#[test]
fn a_return_by_directory_descriptor_reaches_the_system() {
    use std::os::unix::io::AsRawFd;
    unsafe extern "C" {
        fn fchdir(fd: c_int) -> c_int;
    }
    let here = std::fs::File::open(".").unwrap();
    assert_eq!(unsafe { fchdir(here.as_raw_fd()) }, 0);
}

#[test]
fn fork_binds_every_stub_first() {
    assert!(lazy_bind::unbound() > 50, "{} stubs unbound: are the stubs linked in?", lazy_bind::unbound());
    let pid = unsafe { fork() };
    if pid == 0 {
        // only what is safe after a fork in a threaded process
        let ok = lazy_bind::unbound() == 0 && unsafe { getpid() } > 0;
        unsafe { _exit(if ok { 7 } else { 1 }) }
    }
    assert!(pid > 0);
    let mut status = 0;
    assert_eq!(unsafe { waitpid(pid, &mut status, 0) }, pid);
    assert_eq!((status >> 8) & 0xff, 7, "wait status {status:#x}");
    assert_eq!(lazy_bind::unbound(), 0);
}
