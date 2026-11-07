//! Scratch-space housekeeping shared by the gate and testenv: recognising a build that ran
//! out of room, and sweeping stale fixture scratch no live process holds. Nothing here
//! refuses work for want of room; a filesystem that fills is cut at the consumer that filled it.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Text a build prints when the scratch filesystem ran out: infrastructure, never a red.
pub fn is_exhaustion(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["no space left on device", "disk quota exceeded"]
        .iter()
        .any(|m| lower.contains(m))
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

/// A fixture may leave a tree with the write bit off, which `remove_dir_all` cannot empty.
fn make_writable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(md) = fs::symlink_metadata(dir) else { return };
    if !md.is_dir() {
        return;
    }
    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(md.permissions().mode() | 0o700));
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            make_writable(&e.path());
        }
    }
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
        let r = if md.is_dir() {
            make_writable(&e.path());
            fs::remove_dir_all(e.path())
        } else {
            fs::remove_file(e.path())
        };
        if r.is_ok() {
            gone.push(name);
        }
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("scratch-{tag}"))
    }

    #[test]
    fn classifies_exhaustion_and_nothing_else() {
        assert!(is_exhaustion("rustc-LLVM ERROR: Disk quota exceeded"));
        assert!(is_exhaustion("ld: write error: No space left on device"));
        assert!(!is_exhaustion("error[E0308]: mismatched types"));
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
    fn sweep_removes_a_tree_whose_write_bit_is_off() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("ro");
        fs::create_dir_all(d.join("spira-ro/bin")).unwrap();
        fs::write(d.join("spira-ro/bin/tool"), "x").unwrap();
        fs::set_permissions(d.join("spira-ro/bin"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(d.join("spira-ro"), fs::Permissions::from_mode(0o555)).unwrap();
        let gone = sweep(&d, &["spira-"], Duration::ZERO, &|_| false);
        assert_eq!(gone, vec!["spira-ro".to_string()]);
    }

    #[test]
    fn a_process_holding_a_directory_as_cwd_is_seen() {
        let cwd = std::env::current_dir().unwrap();
        assert!(held_by_process(&cwd));
        assert!(!held_by_process(Path::new("/nonexistent-scratch-probe")));
    }
}
