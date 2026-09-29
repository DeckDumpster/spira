//! Test scaffolding: a private temp directory per test, and executables written safely.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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

/// Write an executable that a test will then exec, WITHOUT THIS PROCESS EVER HOLDING A WRITE
/// DESCRIPTOR ON IT.
///
/// `fs::write` then exec fails with ETXTBSY (os error 26) whenever another test thread forks
/// while the write descriptor is open: the child carries a copy until it execs, and the
/// kernel refuses to exec a file anyone has open for writing. Every child here forks for
/// real (`util::command`'s pre_exec), so the window is not theoretical —
/// `real_halt_finds_podman_and_testenv_on_its_path` read an empty `podman ps` about once in
/// four hundred runs. A child process writes the file instead, and nothing another thread
/// forks can inherit a descriptor that only ever existed in that child.
pub fn write_exe(path: &Path, body: &str) {
    let mut c = Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("sh to write an executable");
    c.stdin.take().expect("its stdin").write_all(body.as_bytes()).expect("the script body");
    assert!(c.wait().expect("the writer").success(), "could not write {}", path.display());
}
