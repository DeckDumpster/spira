//! A full host filesystem, as round-vm reports it and the batcher classifies it: the install
//! step could not write, which says nothing about any member's work.

use std::path::Path;

/// The abandon cause and incident kind of a round lost to a full host filesystem.
pub const CAUSE: &str = "host-disk";

/// What round-vm prints ahead of the filesystem it names; the batcher reads the rest of the line.
pub const REPORT_MARK: &str = "host-disk: filesystem ";

const FULL_MARKERS: [&str; 3] = ["Disk quota exceeded", "No space left on device", "os error 122"];

/// Whether a write failure's text says the filesystem, or the user's quota on it, is full.
pub fn is_full(text: &str) -> bool {
    FULL_MARKERS.iter().any(|m| text.contains(m))
}

/// The filesystem round-vm named, from the first report line in its stderr.
pub fn named(stderr: &str) -> Option<String> {
    stderr.lines().find_map(|l| l.split_once(REPORT_MARK).map(|(_, r)| r.trim().to_string()))
}

/// Why an install failed when the host filesystem is the reason: the filesystem round-vm named,
/// or, when only the writer's own words reached the stderr, that none was.
pub fn fault_of(stderr: &str) -> Option<String> {
    match named(stderr) {
        Some(fs) => Some(fs),
        None if is_full(stderr) => Some("filesystem not named".to_string()),
        None => None,
    }
}

/// The mount point of `path` in `/proc/mounts` text: the longest mount that is a prefix of it.
pub fn mount_of(mounts: &str, path: &Path) -> Option<String> {
    mounts
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter(|m| path.starts_with(m))
        .max_by_key(|m| m.len())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quota_or_no_space_failure_is_a_full_filesystem() {
        assert!(is_full("tar: spira-install: Cannot write: Disk quota exceeded"));
        assert!(is_full("write: No space left on device (os error 28)"));
        assert!(!is_full("tar: spira-install: Cannot open: Permission denied"));
    }

    #[test]
    fn the_longest_mount_prefix_names_the_filesystem() {
        let mounts = "tmpfs /tmp tmpfs rw 0 0\n/dev/sda1 / ext4 rw 0 0\ntmpfs /tmpx tmpfs rw 0 0\n";
        assert_eq!(mount_of(mounts, Path::new("/tmp/a/b")).as_deref(), Some("/tmp"));
        assert_eq!(mount_of(mounts, Path::new("/var/x")).as_deref(), Some("/"));
    }

    #[test]
    fn a_full_filesystem_is_a_fault_even_when_only_the_writer_spoke() {
        assert_eq!(fault_of("tar: x: Cannot write: Disk quota exceeded").as_deref(), Some("filesystem not named"));
        assert_eq!(fault_of("tar: x: Cannot open").as_deref(), None);
    }

    #[test]
    fn the_named_filesystem_is_read_from_the_report_line() {
        let err = format!("noise\nround-vm run: {REPORT_MARK}/tmp is full; largest: /tmp/k5 6 GiB\n");
        assert_eq!(named(&err).as_deref(), Some("/tmp is full; largest: /tmp/k5 6 GiB"));
        assert_eq!(named("nothing"), None);
    }
}
