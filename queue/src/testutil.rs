//! Test helpers: unique scratch directories under the system temp dir.

pub fn tmpdir(tag: &str) -> testkit::TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    testkit::TempDir::new(&format!(
        "queue-test-{tag}-{}",
        N.fetch_add(1, Ordering::Relaxed)
    ))
}
