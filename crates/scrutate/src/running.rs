//! How many big searches (a walk of directories, a list of many files) this user runs at once,
//! across processes, so that each takes its share of threads (`instant::policy`). The count is a
//! System V semaphore: a search adds 1 with `SEM_UNDO`, so the kernel
//! takes it back when the process dies, however it dies. Anything that fails (no System V IPC, a
//! set with that key made by another user, a limit) leaves the count unknown and the search runs
//! as it would alone. Only on macOS: on Linux the cores are the limit, and splitting them never
//! beat each search taking them all (py-big on 5 CPUs, 4 at once: 53 ms on 5 threads each, 60 ms
//! on 1, 53 ms on 2).

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

/// What this process does to the count: `HELD` searches, none while a write to stdout may block
/// (a reader that stopped reading must not slow the others) or while the process is stopped from
/// its terminal. All changed under `LOCK`, a flag and no mutex so that the signal handler can try
/// it without waiting.
static LOCK: AtomicBool = AtomicBool::new(false);
static HELD: AtomicUsize = AtomicUsize::new(0);
static WRITING: AtomicUsize = AtomicUsize::new(0);
static STOPPING: AtomicUsize = AtomicUsize::new(0);
/// what the process has in the count now
static COUNTED: AtomicUsize = AtomicUsize::new(0);
/// the set, once a search has asked for it
static SET: AtomicI32 = AtomicI32::new(-1);
/// SIGTSTP is ours to handle while a search is held
static WATCHING: AtomicBool = AtomicBool::new(false);
/// SIGTSTP came while the lock was held: its holder stops the process when it lets go
static STOP_ASKED: AtomicBool = AtomicBool::new(false);

fn lock() {
    while LOCK.swap(true, Ordering::Acquire) {
        std::hint::spin_loop();
    }
}

fn locked<T>(f: impl FnOnce() -> T) -> T {
    lock();
    let r = f();
    LOCK.store(false, Ordering::Release);
    if STOP_ASKED.swap(false, Ordering::Relaxed) {
        lock();
        stop();
    }
    r
}

/// Brings the count to what the state asks for; `false` when the system refused.
fn settle() -> bool {
    let out = WRITING.load(Ordering::Relaxed) > 0 || STOPPING.load(Ordering::Relaxed) > 0;
    let want = if out { 0 } else { HELD.load(Ordering::Relaxed) };
    let have = COUNTED.load(Ordering::Relaxed);
    if want != have {
        if !sys::add(SET.load(Ordering::Relaxed), want as i32 - have as i32) {
            return false;
        }
        COUNTED.store(want, Ordering::Relaxed);
    }
    true
}

/// Ctrl-Z while a search is counted. A stopped search must not keep threads from the others, so
/// it leaves the count, stops as it would have, and joins again when it is continued. SIGSTOP
/// cannot be caught: a search stopped that way stays counted.
extern "C" fn on_stop(_: std::ffi::c_int) {
    if LOCK.swap(true, Ordering::Acquire) {
        // the thread this interrupted may be the holder: no waiting here
        STOP_ASKED.store(true, Ordering::Relaxed);
        return;
    }
    stop();
}

/// Called with the lock held; lets it go. Only atomics and system calls: it runs in the handler.
fn stop() {
    loop {
        STOPPING.fetch_add(1, Ordering::Relaxed);
        settle();
        LOCK.store(false, Ordering::Release);
        // a stop asked for until here is this one
        STOP_ASKED.store(false, Ordering::Relaxed);
        sys::stop_self();
        lock();
        STOPPING.fetch_sub(1, Ordering::Relaxed);
        if HELD.load(Ordering::Relaxed) > 0 && WATCHING.load(Ordering::Relaxed) {
            sys::watch_stop(on_stop);
        }
        settle();
        LOCK.store(false, Ordering::Release);
        if !STOP_ASKED.swap(false, Ordering::Relaxed) {
            return;
        }
        lock();
    }
}

/// One big search, counted while this lives.
pub struct Counted(());

impl Counted {
    pub fn start() -> Option<Counted> {
        let id = sys::id()?;
        locked(|| {
            // a set made again after someone removed it: what was counted went with the old one
            if SET.swap(id, Ordering::Relaxed) != id {
                COUNTED.store(0, Ordering::Relaxed);
            }
            HELD.fetch_add(1, Ordering::Relaxed);
            if !settle() {
                HELD.fetch_sub(1, Ordering::Relaxed);
                return None;
            }
            if HELD.load(Ordering::Relaxed) == 1 {
                WATCHING.store(sys::watch_stop(on_stop), Ordering::Relaxed);
            }
            Some(Counted(()))
        })
    }

    /// The big searches running now, this one included.
    pub fn running(&self) -> Option<usize> {
        sys::value(SET.load(Ordering::Relaxed)).map(|n| n.max(1))
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        locked(|| {
            if HELD.fetch_sub(1, Ordering::Relaxed) == 1 && WATCHING.swap(false, Ordering::Relaxed) {
                sys::unwatch_stop(on_stop);
            }
            settle();
        });
    }
}

/// Stdout that takes this process's searches out of the count while a write to it may block.
pub struct GivesWay(pub std::io::Stdout);

impl GivesWay {
    fn around<T>(&mut self, f: impl FnOnce(&mut std::io::Stdout) -> T) -> T {
        if HELD.load(Ordering::Relaxed) == 0 || !sys::stdout_may_block() {
            return f(&mut self.0);
        }
        locked(|| {
            WRITING.fetch_add(1, Ordering::Relaxed);
            settle();
        });
        let r = f(&mut self.0);
        locked(|| {
            WRITING.fetch_sub(1, Ordering::Relaxed);
            settle();
        });
        r
    }
}

impl Write for GivesWay {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.around(|o| o.write(buf))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.around(|o| o.flush())
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::c_int;

    pub type Id = c_int;

    // as `struct sembuf` in <sys/sem.h>
    #[repr(C)]
    struct Sembuf {
        num: u16,
        op: i16,
        flg: i16,
    }

    // as `struct sigaction` in <sys/signal.h>
    #[repr(C)]
    struct Sigaction {
        handler: usize,
        mask: u32,
        flags: c_int,
    }

    extern "C" {
        fn semget(key: c_int, nsems: c_int, flags: c_int) -> c_int;
        fn semop(id: c_int, ops: *mut Sembuf, n: usize) -> c_int;
        fn semctl(id: c_int, num: c_int, cmd: c_int, ...) -> c_int;
        fn getuid() -> u32;
        fn sigaction(sig: c_int, act: *const Sigaction, old: *mut Sigaction) -> c_int;
        fn raise(sig: c_int) -> c_int;
        fn pthread_sigmask(how: c_int, set: *const u32, old: *mut u32) -> c_int;
    }

    const IPC_CREAT: c_int = 0o1000;
    const IPC_STAT: c_int = 2;
    const IPC_NOWAIT: i16 = 0o4000;
    const SEM_UNDO: i16 = 0o10000;
    const GETVAL: c_int = 5;
    /// Byte offset of `sem_perm.cuid` in `struct semid_ds`
    const CUID_AT: usize = 8;
    const SIGTSTP: c_int = 18;
    const SIG_DFL: usize = 0;
    const SIG_UNBLOCK: c_int = 2;
    const SA_RESTART: c_int = 2;
    const DEFAULT: Sigaction = Sigaction { handler: SIG_DFL, mask: 0, flags: 0 };

    /// The set, made when it is not there: one semaphore, read and written by this user only.
    /// Asked for by every big search (two system calls), so a process that lives long finds a
    /// set made again after its first one was removed.
    pub fn id() -> Option<Id> {
        // SAFETY: no arguments
        let uid = unsafe { getuid() };
        // 0 is IPC_PRIVATE, a new set on every call; SCRUTATE_COUNT_KEY keeps a count apart from
        // the one this user's other searches join (the tests of this count use it)
        let key = std::env::var("SCRUTATE_COUNT_KEY").ok().and_then(|k| k.parse::<c_int>().ok()).unwrap_or((0x5343_5254 ^ uid) as c_int);
        if key == 0 {
            return None;
        }
        // SAFETY: plain integers
        let id = unsafe { semget(key, 1, IPC_CREAT | 0o600) };
        if id < 0 {
            return None;
        }
        // a set another user made with this key and opened to others is not ours to count in
        let mut ds = [0u64; 64];
        // SAFETY: larger than `struct semid_ds` (72 bytes); IPC_STAT takes a pointer to it as
        // the fourth argument
        if unsafe { semctl(id, 0, IPC_STAT, ds.as_mut_ptr()) } != 0 {
            return None;
        }
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ds.as_ptr().cast(), 16) };
        let cuid = u32::from_ne_bytes(bytes[CUID_AT..CUID_AT + 4].try_into().ok()?);
        (cuid == uid).then_some(id)
    }

    #[cfg(test)]
    pub fn remove(id: Id) {
        const IPC_RMID: c_int = 0;
        // SAFETY: IPC_RMID takes no fourth argument
        unsafe { semctl(id, 0, IPC_RMID) };
    }

    /// Adds `n` to the count, undone by the kernel when the process ends; never waits.
    pub fn add(id: Id, n: i32) -> bool {
        let mut op = Sembuf { num: 0, op: n as i16, flg: SEM_UNDO | IPC_NOWAIT };
        // SAFETY: one operation, in a struct laid out as `struct sembuf`
        unsafe { semop(id, &mut op, 1) == 0 }
    }

    pub fn value(id: Id) -> Option<usize> {
        // SAFETY: GETVAL takes no fourth argument
        let v = unsafe { semctl(id, 0, GETVAL) };
        (v >= 0).then_some(v as usize)
    }

    /// Takes SIGTSTP when nothing else handles it; `false` leaves the signal as it was.
    pub fn watch_stop(handler: extern "C" fn(c_int)) -> bool {
        // system calls the signal interrupts start again, as they do without a handler
        let ours = Sigaction { handler: handler as usize, mask: 0, flags: SA_RESTART };
        let mut old = DEFAULT;
        // SAFETY: both point to structs laid out as `struct sigaction`
        if unsafe { sigaction(SIGTSTP, &ours, &mut old) } != 0 {
            return false;
        }
        if old.handler == SIG_DFL || old.handler == ours.handler {
            return true;
        }
        // ignored, or the program this runs in has its own handler
        // SAFETY: what `sigaction` filled in
        unsafe { sigaction(SIGTSTP, &old, std::ptr::null_mut()) };
        false
    }

    /// Gives SIGTSTP back, unless the program has set a handler of its own since.
    pub fn unwatch_stop(handler: extern "C" fn(c_int)) {
        let mut now = DEFAULT;
        // SAFETY: structs laid out as `struct sigaction`
        unsafe {
            if sigaction(SIGTSTP, std::ptr::null(), &mut now) == 0 && now.handler == handler as usize {
                sigaction(SIGTSTP, &DEFAULT, std::ptr::null_mut());
            }
        }
    }

    /// Stops the process as SIGTSTP does when nothing handles it; returns once it is continued.
    pub fn stop_self() {
        let set: u32 = 1 << (SIGTSTP - 1);
        // SAFETY: pointers to a constant and a local; sent to this thread with the signal open,
        // so the stop comes before `raise` returns
        unsafe {
            sigaction(SIGTSTP, &DEFAULT, std::ptr::null_mut());
            pthread_sigmask(SIG_UNBLOCK, &set, std::ptr::null_mut());
            raise(SIGTSTP);
        }
    }

    /// Stdout is a pipe, a socket or a terminal, where a write waits for the reader; a file never
    /// makes it wait.
    pub fn stdout_may_block() -> bool {
        static MAY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *MAY.get_or_init(|| {
            use std::io::IsTerminal;
            use std::os::fd::FromRawFd;
            use std::os::unix::fs::FileTypeExt;
            // SAFETY: borrowed for one `fstat`, never closed
            let out = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
            out.metadata().is_ok_and(|m| m.file_type().is_fifo() || m.file_type().is_socket()) || std::io::stdout().is_terminal()
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    pub type Id = i32;
    pub fn id() -> Option<Id> {
        None
    }
    pub fn add(_: Id, _: i32) -> bool {
        false
    }
    pub fn value(_: Id) -> Option<usize> {
        None
    }
    pub fn stdout_may_block() -> bool {
        false
    }
    pub fn watch_stop(_: extern "C" fn(std::ffi::c_int)) -> bool {
        false
    }
    pub fn unwatch_stop(_: extern "C" fn(std::ffi::c_int)) {}
    pub fn stop_self() {}
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn a_set_removed_under_a_running_process_is_made_again() {
        let a = Counted::start().expect("System V semaphores");
        assert!(a.running().is_some());
        sys::remove(SET.load(Ordering::Relaxed));
        assert!(a.running().is_none(), "the set is gone");
        let b = Counted::start().expect("a new set");
        let n = b.running().expect("the new set answers");
        assert!(n >= 2, "both searches of this process are in the new set, {n} counted");
        drop(b);
        assert!(a.running().is_some_and(|n| n >= 1));
    }
}
