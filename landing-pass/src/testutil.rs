//! Test scaffolding: a private temp directory per test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

/// A test's private directory, REMOVED WHEN IT DROPS. These tests run on the host at every
/// gate (gate_mode = unit), and a directory left behind per test per run exhausted /tmp's
/// inodes on 2026-09-29 (84,964 `landing-pass-test-*` directories). Derefs to the path, so a
/// caller reads it as the `PathBuf` it used to be.
/// testkit::TempDir underneath (sp-qgfdi); `path` is its path, for callers that want a
/// `&PathBuf`.
pub struct TmpDir {
    _dir: testkit::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for TmpDir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

impl AsRef<Path> for TmpDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

pub fn tmpdir(tag: &str) -> TmpDir {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let d = testkit::TempDir::new(&format!("landing-pass-test-{tag}-{n}"));
    TmpDir { path: d.to_path_buf(), _dir: d }
}

/// Serialises tests that mutate process-global state (an env var such as `SPIRA_REPO_MAP`,
/// read by `real::repo_registry` — sp-o88bx, "wave 4.12") against each other. Production has
/// one thread; `cargo test` runs these on several, so without this two tests racing on the
/// same env var is a test-harness flake, not a real defect. Re-entrant per thread, same
/// shape as `queue::testutil::serial`.
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
