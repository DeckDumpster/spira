//! Test helpers: unique scratch directories under the system temp dir.


pub fn tmpdir(tag: &str) -> testkit::TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    testkit::TempDir::new(&format!("queue-test-{tag}-{}", N.fetch_add(1, Ordering::Relaxed)))
}

/// Serialises the tests that hold a queue lock with the tests that fork a child. flock
/// belongs to the open file description, and a child forked by another test thread holds
/// a copy of our lock descriptor until its exec closes it (CLOEXEC) — long enough for a
/// test's second run to find its own, already-dropped lock still held. Production has one
/// thread; this is a test-harness race only. Re-entrant per thread (a test may hold several
/// worlds at once, shadowed variables live to the end of scope).
pub struct Serial;

static M: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    static HELD: std::cell::RefCell<(u32, Option<std::sync::MutexGuard<'static, ()>>)> = const { std::cell::RefCell::new((0, None)) };
}

pub fn serial() -> Serial {
    HELD.with(|h| {
        let mut h = h.borrow_mut();
        if h.0 == 0 {
            h.1 = Some(M.lock().unwrap_or_else(|e| e.into_inner()));
        }
        h.0 += 1;
    });
    Serial
}

impl Drop for Serial {
    fn drop(&mut self) {
        HELD.with(|h| {
            let mut h = h.borrow_mut();
            h.0 -= 1;
            if h.0 == 0 {
                h.1 = None;
            }
        });
    }
}
