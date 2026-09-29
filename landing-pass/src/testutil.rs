//! Test scaffolding: a private temp directory per test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

/// A test's private directory, REMOVED WHEN IT DROPS. These tests run on the host at every
/// gate (gate_mode = unit), and a directory left behind per test per run exhausted /tmp's
/// inodes on 2026-09-29 (84,964 `landing-pass-test-*` directories). Derefs to the path, so a
/// caller reads it as the `PathBuf` it used to be.
pub struct TmpDir(PathBuf);

impl std::ops::Deref for TmpDir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for TmpDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn tmpdir(tag: &str) -> TmpDir {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("landing-pass-test-{}-{tag}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    TmpDir(d)
}
