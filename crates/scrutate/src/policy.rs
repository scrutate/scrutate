//! The thread rules of Engine A, decided from readings of the machine: its cores, the searches of
//! this user running at the same time, and how long small reads take. `System` takes the readings
//! from the operating system; a test gives its own. The scheduler takes them once per search and
//! once per look at the count, never per file.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// What the thread rules read of the machine.
pub(super) trait Machine: Sync {
    /// The threads this process may run at once.
    fn cores(&self) -> usize;
    /// The performance cores, where the system tells them apart (macOS).
    fn performance_cores(&self) -> Option<usize>;
    /// The numbers measured for this system.
    fn values(&self) -> Values {
        HERE
    }
    /// The searches of this user running now, this one included; the first call joins the count.
    /// None: nothing is counted here.
    fn running(&self) -> Option<usize>;
    /// The share a search took (`None`: no count), when `SCRUTATE_GREP_DEBUG` asks for it.
    fn show(&self, running: Option<usize>, threads: usize);
}

/// The numbers of one system, each from the measurement named beside it.
#[derive(Clone, Copy)]
pub(super) struct Values {
    /// An open and read of a small file taking this long came from the disk, not the page cache.
    pub cold_read_ns: u64,
    /// Most threads for a tree read from the disk: this many, or twice the cores when 0.
    pub cold_threads: usize,
    /// Searches of one user running at the same time split the threads.
    pub shared: bool,
}

/// macOS: about 25 us a warm read and 160 us or more a cold one (M3 Pro). Cold, the Linux tree took
/// 2.7 s on 5 threads, 1.6 s on 11, 1.31 s on 24, 1.24 s on 48, 1.36 s on 96, and 200,000 small
/// files were slower on 48 than on 22: twice the cores. The kernel serves about four file-reading
/// threads at full speed across all processes, so searches share (py-big, 4 at once: 244 ms each on
/// 5 threads, 189 ms on 1; docs/ARCHITECTURE.md has the runs).
pub(super) const MACOS: Values = Values { cold_read_ns: 75_000, cold_threads: 0, shared: true };

/// Linux: 99.9% of warm reads under 15 us and 97% of cold reads over 30 us (a VM on the same
/// machine). The Linux tree cold on 5 CPUs took 1.22 s on 5 threads, 0.76 s on 10, 0.55 s on 20,
/// 0.41 s on 40, 0.37 s on 80, 0.38 s on 160. The cores are the limit and splitting them never beat
/// each search taking them all (py-big on 5 CPUs, 4 at once: 53 ms on 5 threads each, 60 ms on 1).
pub(super) const LINUX: Values = Values { cold_read_ns: 30_000, cold_threads: 64, shared: false };

/// Windows and other systems: nothing is measured there. The read time and the cold threads are
/// the macOS numbers; there is no count of searches.
pub(super) const OTHER: Values = Values { cold_read_ns: MACOS.cold_read_ns, cold_threads: MACOS.cold_threads, shared: false };

pub(super) const HERE: Values = if cfg!(target_os = "linux") {
    LINUX
} else if cfg!(target_os = "macos") {
    MACOS
} else {
    OTHER
};

/// Most threads a search starts for a number asked for: past a few hundred, starting them costs
/// more than any search gains (`-j 4000` took 1.8 s on 11 files, `-j 100000` failed to start them).
pub const MAX_THREADS: usize = 256;

/// Cores counted at most: more only add contention on the directory walk and the output channel.
const MAX_CORES: usize = 12;

/// Slow reads before the reads can tell a cold tree from a warm one.
const COLD_READS_SEEN: u64 = 16;

/// A cold tree gets one more thread for every this many slow reads, not all its cold threads at
/// once: starting a thread costs about as much as a slow read, which a small tree never earns back
/// (231 files cold on Linux: 8.3 ms with a thread per slow read, 7.4 ms per 4, 6.0 ms per 8, 5.8 ms
/// per 16; the Linux tree takes 0.37 s with any of them).
const COLD_READS_PER_THREAD: u64 = 16;

/// A search looks at the count again once it has searched this many files, every 64 files of a
/// thread, so a small repository makes the system calls once; and once more after `COUNTED_EARLY`
/// files: searches started together each saw fewer of the others than there are.
const COUNTED_AFTER: u64 = 1000;
const COUNTED_EARLY: u64 = 4;

/// Searches of at most this many files, with no directory to walk, are not counted, and a walk of
/// roots without directories takes this many files on one walking thread.
pub(super) const SMALL_SEARCH: usize = 256;

/// The thread rules for one search, from the machine's readings and the threads asked for.
#[derive(Clone, Copy)]
pub(super) struct Policy {
    /// `Files::threads`, at most `MAX_THREADS`; 0 when none were asked for
    asked: usize,
    cores: usize,
    threads: usize,
    values: Values,
}

impl Policy {
    pub(super) fn read<M: Machine + ?Sized>(m: &M, asked: usize) -> Policy {
        // one thread, as if asked for: wasi starts no other
        #[cfg(target_os = "wasi")]
        let asked = {
            let _ = asked;
            1
        };
        let asked = asked.min(MAX_THREADS);
        if asked != 0 {
            // every rule keeps a number asked for: the cores are not asked of the system (system calls)
            return Policy { asked, cores: asked, threads: asked, values: m.values() };
        }
        let cores = m.cores().clamp(1, MAX_CORES);
        // on macOS the performance cores: threads opening and reading files at once mostly wait on
        // each other in the kernel there (the Linux tree: 977 ms on 11 threads, 843 ms on 6, 850 ms
        // on 4; LLVM: 1649 ms on 11, 1377 ms on 4)
        let threads = m.performance_cores().map_or(cores, |p| p.clamp(2, cores.max(2)).min(cores));
        Policy { asked, cores, threads, values: m.values() }
    }

    /// Threads searching many files: the number asked for, or the cores (the performance cores
    /// where there are such).
    pub(super) fn threads(&self) -> usize {
        if self.asked != 0 { self.asked } else { self.threads }
    }

    /// Threads for one large file searched as ranges: every core, since that work is matching
    /// rather than opening files.
    pub(super) fn range_threads(&self) -> usize {
        if self.asked != 0 { self.asked } else { self.cores }
    }

    /// Threads searching the files a walking thread finds in roots without directories.
    pub(super) fn walked_threads(&self) -> usize {
        self.threads().min(3)
    }

    /// Whether a search joins the count of searches running (`Machine::running`): a number asked
    /// for is kept, and a small search is over before the count would matter.
    pub(super) fn counted(&self, small: bool) -> bool {
        self.asked == 0 && !small
    }

    /// Whether a search may grow past the threads it starts with: a number asked for is kept.
    pub(super) fn grows(&self) -> bool {
        self.asked == 0
    }

    /// The threads of one search beside `running` others and itself: the default threads split
    /// evenly, at least one each (an unknown count is one search).
    pub(super) fn share(&self, running: Option<usize>) -> usize {
        let alone = self.threads();
        running.map_or(alone, |k| (alone / k.max(1)).max(1))
    }

    /// Whether a search looks at the count again after one of its threads searched `files` files
    /// and the search `searched`.
    pub(super) fn look_again(&self, files: u32, searched: u64) -> bool {
        files % 64 == 16 && searched >= COUNTED_AFTER
    }

    /// Whether the one look after the first few files is due.
    pub(super) fn look_early(&self, searched: u64) -> bool {
        searched >= COUNTED_EARLY
    }

    /// A small read took this long: it came from the disk.
    #[inline]
    pub(super) fn slow(&self, ns: u64) -> bool {
        ns >= self.values.cold_read_ns
    }

    /// Most threads for a tree read from the disk: those reads wait on the device, so more of them
    /// than cores are worth having.
    pub(super) fn cold_threads(&self) -> usize {
        match self.values.cold_threads {
            0 => 2 * self.cores,
            n => n,
        }
    }

    /// Most threads a search with no number asked for may grow to.
    pub(super) fn most(&self) -> usize {
        self.cold_threads().max(self.threads())
    }

    /// Threads `reads` small reads, `slow` of them slow, call for on top of `threads`.
    pub(super) fn cold_extra(&self, reads: u64, slow: u64) -> usize {
        // three quarters slow: a few reads delayed by a busy machine do not make a warm tree look cold
        if self.asked == 0 && reads >= COLD_READS_SEEN && slow * 4 >= reads * 3 {
            let most = self.cold_threads().saturating_sub(self.threads);
            ((slow / COLD_READS_PER_THREAD) as usize).min(most)
        } else {
            0
        }
    }
}

/// The operating system's readings, for one search: it holds that search's place in the count.
#[derive(Default)]
pub(super) struct System {
    counted: OnceLock<Option<crate::running::Counted>>,
}

impl Machine for System {
    fn cores(&self) -> usize {
        cores().0
    }

    fn performance_cores(&self) -> Option<usize> {
        cores().1
    }

    fn running(&self) -> Option<usize> {
        if !self.values().shared {
            return None;
        }
        self.counted.get_or_init(crate::running::Counted::start).as_ref().and_then(|n| n.running())
    }

    fn show(&self, running: Option<usize>, threads: usize) {
        static DEBUG: OnceLock<bool> = OnceLock::new();
        if !*DEBUG.get_or_init(|| crate::config::env_on("SCRUTATE_GREP_DEBUG")) {
            return;
        }
        match running {
            Some(k) => eprintln!("[scrutate] searches running: {k}, threads: {threads}"),
            None => eprintln!("[scrutate] searches running: not counted, threads: {threads}"),
        }
    }
}

/// The share a search shows, so `Machine::show` hears of each change once.
#[derive(Default)]
pub(super) struct Shown(AtomicUsize);

impl Shown {
    pub(super) fn show(&self, m: &dyn Machine, running: Option<usize>, threads: usize) {
        let key = running.map_or(usize::MAX, |k| k << 16 | threads);
        if self.0.swap(key, Ordering::Relaxed) != key {
            m.show(running, threads);
        }
    }
}

/// The cores and the performance cores, asked of the system once: system calls on some platforms.
fn cores() -> (usize, Option<usize>) {
    static CORES: OnceLock<(usize, Option<usize>)> = OnceLock::new();
    *CORES.get_or_init(|| (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2), performance_cores()))
}

#[cfg(target_os = "macos")]
fn performance_cores() -> Option<usize> {
    unsafe extern "C" {
        fn sysctlbyname(name: *const std::ffi::c_char, oldp: *mut std::ffi::c_void, oldlenp: *mut usize, newp: *mut std::ffi::c_void, newlen: usize) -> i32;
    }
    let mut v: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    // SAFETY: a NUL-terminated name, an i32 to write and its size, no new value
    let rc = unsafe { sysctlbyname(c"hw.perflevel0.physicalcpu".as_ptr(), (&mut v as *mut i32).cast(), &mut len, std::ptr::null_mut(), 0) };
    (rc == 0 && len == std::mem::size_of::<i32>() && v > 0).then_some(v as usize)
}

#[cfg(not(target_os = "macos"))]
fn performance_cores() -> Option<usize> {
    None
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Readings a test gives: the cores, the performance cores, the system's numbers and the count
    /// of searches running. It keeps every share shown.
    pub(in super::super) struct Given {
        pub cores: usize,
        pub performance: Option<usize>,
        pub values: &'static Values,
        pub running: Mutex<Option<usize>>,
        pub shown: Mutex<Vec<(Option<usize>, usize)>>,
    }

    impl Given {
        pub(in super::super) fn new(cores: usize, performance: Option<usize>, values: &'static Values, running: Option<usize>) -> Given {
            Given { cores, performance, values, running: Mutex::new(running), shown: Mutex::new(Vec::new()) }
        }
    }

    impl Machine for Given {
        fn cores(&self) -> usize {
            self.cores
        }
        fn performance_cores(&self) -> Option<usize> {
            self.performance
        }
        fn values(&self) -> Values {
            *self.values
        }
        fn running(&self) -> Option<usize> {
            self.running.lock().unwrap().filter(|_| self.values.shared)
        }
        fn show(&self, running: Option<usize>, threads: usize) {
            self.shown.lock().unwrap().push((running, threads));
        }
    }

    fn policy(cores: usize, performance: Option<usize>, values: &'static Values, asked: usize) -> Policy {
        Policy::read(&Given::new(cores, performance, values, None), asked)
    }

    #[test]
    fn the_default_threads_are_the_performance_cores_or_the_cores_at_most_twelve() {
        assert_eq!(policy(11, Some(5), &MACOS, 0).threads(), 5, "an M3 Pro");
        assert_eq!(policy(11, Some(5), &MACOS, 0).range_threads(), 11, "a large file takes every core");
        assert_eq!(policy(8, Some(1), &MACOS, 0).threads(), 2, "at least two performance cores");
        assert_eq!(policy(1, Some(1), &MACOS, 0).threads(), 1, "never more than the cores");
        assert_eq!(policy(64, None, &LINUX, 0).threads(), 12);
        assert_eq!(policy(64, None, &LINUX, 0).range_threads(), 12);
        assert_eq!(policy(4, None, &OTHER, 0).threads(), 4);
        assert_eq!(policy(11, Some(5), &MACOS, 0).walked_threads(), 3);
        assert_eq!(policy(1, None, &LINUX, 0).walked_threads(), 1);
    }

    #[test]
    fn a_number_asked_for_is_kept_up_to_the_most() {
        let p = policy(11, Some(5), &MACOS, 7);
        assert_eq!((p.threads(), p.range_threads(), p.walked_threads()), (7, 7, 3));
        assert_eq!(policy(4, None, &LINUX, 100_000).threads(), MAX_THREADS);
        assert!(!p.counted(false), "a number asked for is not counted");
        assert_eq!(p.share(Some(4)), 7 / 4);
        assert_eq!(p.cold_extra(100, 100), 0, "-j is kept on a cold tree");
    }

    #[test]
    fn searches_running_together_split_the_threads() {
        let p = policy(12, Some(5), &MACOS, 0);
        let shares: Vec<usize> = [None, Some(0), Some(1), Some(2), Some(3), Some(4), Some(9)].into_iter().map(|k| p.share(k)).collect();
        assert_eq!(shares, [5, 5, 5, 2, 1, 1, 1], "an unknown count is one search");
        assert_eq!(policy(12, None, &OTHER, 0).share(Some(4)), 3);
    }

    #[test]
    fn only_big_searches_join_the_count_and_only_macos_counts() {
        assert!(policy(11, Some(5), &MACOS, 0).counted(false));
        assert!(!policy(11, Some(5), &MACOS, 0).counted(true), "a small search");
        assert!(!policy(11, Some(5), &MACOS, 4).grows(), "a number asked for");
        const { assert!(MACOS.shared && !LINUX.shared && !OTHER.shared, "Linux and Windows count nothing") };
        assert_eq!(Given::new(5, None, &LINUX, Some(3)).running(), None);
    }

    #[test]
    fn the_count_is_looked_at_early_once_then_every_64_files_past_a_thousand() {
        let p = policy(11, Some(5), &MACOS, 0);
        assert!(!p.look_early(3) && p.look_early(4));
        assert!(!p.look_again(4, 4));
        assert!(!p.look_again(16, 999));
        assert!(p.look_again(16, 1000));
        assert!(p.look_again(80, 5000));
        assert!(!p.look_again(81, 5000));
    }

    #[test]
    fn a_cold_tree_earns_threads_by_its_slow_reads() {
        for (values, cores, performance, most) in [(&MACOS, 11, Some(5), 22 - 5), (&LINUX, 5, None, 64 - 5), (&OTHER, 4, None, 8 - 4)] {
            let p = policy(cores, performance, values, 0);
            assert_eq!(p.cold_extra(15, 15), 0, "too few reads to tell");
            assert_eq!(p.cold_extra(1000, 700), 0, "mostly fast reads are a warm tree");
            assert_eq!(p.cold_extra(16, 16), 1);
            assert_eq!(p.cold_extra(100, 80), 5.min(most));
            assert_eq!(p.cold_extra(1_000_000, 1_000_000), most);
            assert_eq!(p.most(), p.threads() + most);
        }
        let mac = policy(11, Some(5), &MACOS, 0);
        assert!(!mac.slow(74_999) && mac.slow(75_000));
        let linux = policy(5, None, &LINUX, 0);
        assert!(!linux.slow(29_999) && linux.slow(30_000));
    }
}
