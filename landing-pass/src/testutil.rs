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
