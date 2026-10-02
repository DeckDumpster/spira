//! The shared reservation ledger for the scratch tmpfs: gate build trees and testenv slots
//! reserve an estimated peak before they build and release it after, so concurrent consumers
//! cannot each pass a free-space check and then jointly overflow it.
//!
//! A reservation is a file in the ledger directory, locked with `flock` by its holder for as
//! long as the holder lives: a crashed holder's lock dies with it, and its file is then
//! garbage that the next reserver removes. Reservations are counted whole (never reduced by
//! what the holder has already written), which over-counts and so errs toward refusing.

use std::fs;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const LEDGER_DIR: &str = "spira-scratch-ledger";

/// The ledger for a scratch root: beside it, so every consumer of one filesystem shares one.
pub fn ledger_for(root: &Path) -> PathBuf {
    root.parent().unwrap_or(Path::new("/")).join(LEDGER_DIR)
}

/// Text a build prints when the scratch filesystem ran out: infrastructure, never a red.
pub fn is_exhaustion(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["no space left on device", "disk quota exceeded"]
        .iter()
        .any(|m| lower.contains(m))
}

#[derive(Debug)]
pub struct Guard {
    _file: fs::File,
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn try_flock(f: &fs::File) -> bool {
    // SAFETY: flock on a descriptor we own.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

/// MiB held by live reservations; removes the files of dead holders.
pub fn reserved_mib(dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    let mut sum = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("r-") {
            continue;
        }
        let Ok(f) = fs::OpenOptions::new().read(true).open(e.path()) else { continue };
        if try_flock(&f) {
            let _ = fs::remove_file(e.path());
            continue;
        }
        sum += fs::read_to_string(e.path()).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    }
    sum
}

/// Reserve `mib` on the scratch filesystem for the life of the returned guard. Err (the
/// refusal) when `free_mib` minus every live reservation leaves less than `mib` plus
/// `floor_mib`, or when the filesystem cannot be probed.
pub fn reserve(
    dir: &Path,
    owner: &str,
    mib: u64,
    floor_mib: u64,
    free_mib: &dyn Fn() -> Option<u64>,
) -> Result<Guard, String> {
    fs::create_dir_all(dir).map_err(|e| format!("scratch: cannot create {}: {e}", dir.display()))?;
    let gate = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".lock"))
        .map_err(|e| format!("scratch: cannot open the ledger lock in {}: {e}", dir.display()))?;
    // SAFETY: a blocking flock on a descriptor we own; it serialises reservers only.
    if unsafe { libc::flock(gate.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err("scratch: cannot lock the reservation ledger".into());
    }
    let free = free_mib().ok_or("scratch: cannot probe the scratch filesystem's free space")?;
    let held = reserved_mib(dir);
    let need = mib.saturating_add(floor_mib).saturating_add(held);
    if free < need {
        return Err(format!(
            "scratch: {owner} needs {mib} MiB (+{floor_mib} floor) but only {free} MiB is free with {held} MiB already reserved by live consumers"
        ));
    }
    let tag: String = owner.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let path = dir.join(format!("r-{}-{nanos}-{tag}", std::process::id()));
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(&path)
        .map_err(|e| format!("scratch: cannot record the reservation {}: {e}", path.display()))?;
    if !try_flock(&f) {
        let _ = fs::remove_file(&path);
        return Err("scratch: cannot hold the reservation".into());
    }
    let _ = writeln!(f, "{mib}");
    Ok(Guard { _file: f, path })
}

/// True when a live process has `p` (or anything under it) as its cwd or an open file.
pub fn held_by_process(p: &Path) -> bool {
    let Ok(procs) = fs::read_dir("/proc") else { return true };
    for pr in procs.flatten() {
        let n = pr.file_name().to_string_lossy().into_owned();
        if !n.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let mut links = vec![pr.path().join("cwd")];
        if let Ok(fds) = fs::read_dir(pr.path().join("fd")) {
            links.extend(fds.flatten().map(|f| f.path()));
        }
        if links.iter().filter_map(|l| fs::read_link(l).ok()).any(|t| t.starts_with(p)) {
            return true;
        }
    }
    false
}

/// Remove entries of `root` whose name starts with one of `prefixes`, whose mtime is older
/// than `min_age`, and that no live process holds. Returns the names removed.
pub fn sweep(root: &Path, prefixes: &[&str], min_age: Duration, held: &dyn Fn(&Path) -> bool) -> Vec<String> {
    let mut gone = Vec::new();
    let Ok(rd) = fs::read_dir(root) else { return gone };
    let now = SystemTime::now();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !prefixes.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let Ok(md) = fs::symlink_metadata(e.path()) else { continue };
        let old = md.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|a| a >= min_age);
        if !old || held(&e.path()) {
            continue;
        }
        let r = if md.is_dir() { fs::remove_dir_all(e.path()) } else { fs::remove_file(e.path()) };
        if r.is_ok() {
            gone.push(name);
        }
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("scratch-{tag}-{}-{}", std::process::id(), line!()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn classifies_exhaustion_and_nothing_else() {
        assert!(is_exhaustion("rustc-LLVM ERROR: Disk quota exceeded"));
        assert!(is_exhaustion("ld: write error: No space left on device"));
        assert!(!is_exhaustion("error[E0308]: mismatched types"));
    }

    #[test]
    fn concurrent_reservations_cannot_jointly_overflow() {
        let d = tmp("joint");
        let free = || Some(1000u64);
        let a = reserve(&d, "a", 600, 100, &free).unwrap();
        // each alone fits in 1000; together they do not
        let b = reserve(&d, "b", 600, 100, &free);
        assert!(b.is_err(), "{b:?}", b = b.as_ref().map(|_| ()));
        assert_eq!(reserved_mib(&d), 600);
        drop(a);
        assert_eq!(reserved_mib(&d), 0);
        assert!(reserve(&d, "b", 600, 100, &free).is_ok());
    }

    #[test]
    fn roots_on_one_filesystem_share_a_ledger() {
        assert_eq!(ledger_for(Path::new("/tmp/spira-gate-target-ab")), ledger_for(Path::new("/tmp/spira-testenv-cd")));
    }

    #[test]
    fn an_unprobeable_filesystem_refuses() {
        let d = tmp("probe");
        assert!(reserve(&d, "a", 1, 0, &|| None).is_err());
    }

    #[test]
    fn a_dead_holders_file_is_not_counted() {
        let d = tmp("dead");
        fs::write(d.join("r-1-1-ghost"), "900\n").unwrap();
        assert_eq!(reserved_mib(&d), 0);
        assert!(!d.join("r-1-1-ghost").exists());
    }

    #[test]
    fn sweep_takes_old_unheld_and_spares_young_and_held() {
        let d = tmp("sweep");
        for n in ["spira-old", "spira-held", "other-old"] {
            fs::create_dir_all(d.join(n)).unwrap();
        }
        let held = |p: &Path| p.ends_with("spira-held");
        assert!(sweep(&d, &["spira-"], Duration::from_secs(3600), &held).is_empty(), "young entries are spared");
        let gone = sweep(&d, &["spira-"], Duration::ZERO, &held);
        assert_eq!(gone, vec!["spira-old".to_string()]);
        assert!(d.join("spira-held").exists() && d.join("other-old").exists());
    }

    #[test]
    fn a_process_holding_a_directory_as_cwd_is_seen() {
        let cwd = std::env::current_dir().unwrap();
        assert!(held_by_process(&cwd));
        assert!(!held_by_process(Path::new("/nonexistent-scratch-probe")));
    }
}
