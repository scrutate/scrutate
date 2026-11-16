//! Giving the heap a long-lived front end freed back to the system after its answers.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// When memory was last given back.
static LAST: Mutex<Option<Instant>> = Mutex::new(None);

const SECOND: Duration = Duration::from_secs(1);

/// Whether an answer that ran for `ran`, `since` after memory was last given back (never: `None`),
/// gives it back: when it ran a second or more, or a second has passed since the last time.
fn should_give_back(ran: Duration, since: Option<Duration>) -> bool {
    ran >= SECOND || since.is_none_or(|s| s >= SECOND)
}

/// Called by `serve`, `lsp` and `batch` once an answer that began at `started` is out; the watch
/// loop holds no resolver store and does not call it.
pub fn give_back_memory(started: Instant) {
    let now = Instant::now();
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if !should_give_back(now - started, last.map(|l| now - l)) {
        return;
    }
    *last = Some(now);
    drop(last);
    trim();
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn trim() {
    // glibc keeps a thread's freed pages in its arena, and serve answers each call on a new thread
    unsafe {
        libc::malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim() {}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_short_answer_within_a_second_of_the_last_time_gives_nothing_back() {
        assert!(!should_give_back(10 * MS, Some(300 * MS)));
        assert!(!should_give_back(999 * MS, Some(999 * MS)));
    }

    #[test]
    fn two_short_answers_within_a_second_give_it_back_once() {
        assert!(should_give_back(10 * MS, None));
        assert!(!should_give_back(10 * MS, Some(300 * MS)));
        assert!(should_give_back(10 * MS, Some(SECOND)));
    }

    #[test]
    fn a_long_answer_always_gives_it_back() {
        assert!(should_give_back(SECOND, Some(MS)));
        assert!(should_give_back(30 * SECOND, Some(Duration::ZERO)));
    }
}
