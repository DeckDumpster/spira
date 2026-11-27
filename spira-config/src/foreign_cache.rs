//! A foreign repository's warm cargo cache on the host, `$SPIRA_RUN/foreign-cache/<repo>`:
//! one cache, built in by testenv's gate and by a host round, under one lock. testenv holds it
//! shared while it reads the target as an overlay's lower layer; whoever writes the target
//! holds it exclusive.

use std::fs::{File, OpenOptions};
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
    let _ = File::open(dir.join("target")).and_then(|f| f.set_modified(SystemTime::now()));
    Ok(())
}

/// One non-blocking try at the cache's lock; `op` is `LOCK_SH` or `LOCK_EX`. Dropping the
/// file, or the holder dying, frees it.
pub fn try_flock(dir: &Path, op: i32) -> Option<File> {
    let f = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("lock")).ok()?;
    // SAFETY: flock on a descriptor we own; released when the File is dropped.
    (unsafe { libc::flock(f.as_raw_fd(), op | libc::LOCK_NB) } == 0).then_some(f)
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
