//! A foreign repository's warm cargo cache on the host, `$SPIRA_RUN/foreign-cache/<repo>`:
//! one cache, built in by testenv's gate and by a host round, under one lock. testenv holds it
//! shared while it reads the target as an overlay's lower layer; whoever writes the target
//! holds it exclusive. A writer that has to wait holds a second lock, its intent, which
//! readers arriving later wait behind.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Where a container sees the cache. Cargo's fingerprints hold these paths, so every builder
/// mounts at the same ones.
pub const CACHE_TARGET: &str = system_paths::guest::VAR_CACHE_SPIRA_FOREIGN_TARGET;
pub const CACHE_CARGO: &str = system_paths::guest::VAR_CACHE_SPIRA_FOREIGN_CARGO;

/// The character set a repository or share name keeps in a path.
pub fn sanitise(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '_' }).collect()
}

pub fn root(run: &Path) -> PathBuf {
    run.join("foreign-cache")
}

pub fn dir(root: &Path, repo: &str) -> PathBuf {
    root.join(sanitise(repo))
}

/// Makes the cache's directories (podman refuses a missing mount source) and marks the target
/// used, which keeps it from the idle reaper.
pub fn prepare(dir: &Path) -> std::io::Result<()> {
    for sub in ["target", "runs", "cargo"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    touch(&dir.join("target"));
    Ok(())
}

/// Marks `path` used now, for a reaper that judges idleness by mtime.
pub fn touch(path: &Path) {
    let _ = File::open(path).and_then(|f| f.set_modified(SystemTime::now()));
}

/// One non-blocking try at the cache's lock; `op` is `LOCK_SH` or `LOCK_EX`. Dropping the
/// file, or the holder dying, frees it.
pub fn try_flock(dir: &Path, op: i32) -> Option<File> {
    flock_file(&dir.join("lock"), op)
}

fn flock_file(path: &Path, op: i32) -> Option<File> {
    let f = OpenOptions::new().create(true).truncate(false).write(true).open(path).ok()?;
    // SAFETY: flock on a descriptor we own; released when the File is dropped.
    (unsafe { libc::flock(f.as_raw_fd(), op | libc::LOCK_NB) } == 0).then_some(f)
}

/// A writer's claim to the exclusive lock next, held while it waits for the readers to finish.
pub struct Intent(File);

impl Intent {
    pub fn take(dir: &Path, who: &str) -> Option<Intent> {
        let intent = Intent(flock_file(&dir.join("intent"), libc::LOCK_EX)?);
        let _ = intent.0.set_len(0).and_then(|()| (&intent.0).write_all(who.as_bytes()));
        Some(intent)
    }
}

impl Drop for Intent {
    fn drop(&mut self) {
        let _ = self.0.set_len(0);
        // SAFETY: flock on a descriptor we own. Explicit, because a child forked meanwhile
        // holds the descriptor, and so the lock, until its exec.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// The writer waiting for the exclusive lock, as it recorded itself. A probe holds the intent
/// shared for an instant, so an [`Intent::take`] can fail with no writer there: retry it.
pub fn waiting_writer(dir: &Path) -> Option<String> {
    let f = File::open(dir.join("intent")).ok()?;
    // SAFETY: flock on a descriptor we own; released when the File is dropped.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        return None;
    }
    Some(std::fs::read_to_string(dir.join("intent")).unwrap_or_default().trim().to_string())
}

/// Who holds the exclusive lock, as it recorded itself; empty when nobody did.
pub fn holder(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("holder")).unwrap_or_default().trim().to_string()
}

/// The exclusive lock, naming its holder to whoever waits on it.
pub struct Exclusive {
    _lock: File,
    holder: PathBuf,
}

impl Exclusive {
    pub fn try_take(dir: &Path, who: &str) -> Option<Exclusive> {
        let _lock = try_flock(dir, libc::LOCK_EX)?;
        let holder = dir.join("holder");
        let _ = std::fs::write(&holder, who);
        Some(Exclusive { _lock, holder })
    }
}

impl Drop for Exclusive {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.holder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spira_test_id::test_id;

    #[test_id(tfumfns)]
    #[test]
    fn a_waiting_writer_is_named_only_while_its_intent_is_held() {
        let dir = testkit::TempDir::new("foreign-cache-intent");
        assert_eq!(waiting_writer(dir.path()), None);
        let held = Intent::take(dir.path(), "round-vm x").expect("free");
        assert_eq!(waiting_writer(dir.path()).as_deref(), Some("round-vm x"));
        assert!(Intent::take(dir.path(), "another").is_none(), "one writer waits at a time");
        assert_eq!(waiting_writer(dir.path()).as_deref(), Some("round-vm x"), "a refused take leaves the name");
        drop(held);
        assert_eq!(waiting_writer(dir.path()), None);
    }
}
