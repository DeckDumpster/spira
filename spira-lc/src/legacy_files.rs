//! Reads the queue's open-batch record (one file per repository) for the migration classifier.

use std::path::Path;

/// A currently open batch for one repository: its `members=` line (space-separated bead
/// ids), plus `head`/`base`/`opened` for the batch row the classifier writes.
pub struct OpenBatch {
    pub members: Vec<String>,
    pub head: Option<String>,
    pub base: Option<String>,
    pub opened_at: Option<i64>,
}

/// Reads `<queue_dir>/<repo>/open`, the same file `batch.sh` and `queue.sh open-batch` both
/// write. `None` means there is no open batch for this repository right now.
pub fn read_open_batch(queue_dir: &Path, repo: &str) -> Option<OpenBatch> {
    let text = std::fs::read_to_string(queue_dir.join(repo).join("open")).ok()?;
    let mut members = Vec::new();
    let mut head = None;
    let mut base = None;
    let mut opened_at = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("members=") {
            members = rest.split_whitespace().map(str::to_string).collect();
        } else if let Some(rest) = line.strip_prefix("head=") {
            head = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("base=") {
            base = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("opened=") {
            opened_at = rest.trim().parse().ok();
        }
    }
    Some(OpenBatch { members, head, base, opened_at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(tag: &str) -> testkit::TempDir {
        testkit::TempDir::new(&format!("spira-lc-legacy-files-test-{tag}"))
    }

    #[test]
    fn reads_an_open_batchs_members_head_base_and_opened_at() {
        let dir = scratch("batch");
        fs::create_dir_all(dir.join("service")).unwrap();
        fs::write(
            dir.join("service").join("open"),
            "pr=42\nhead=deadbeef\nbase=cafef00d\nmembers=sp-1 sp-2 sp-3\nopened=1700000000\nbranch=queue/service/batch-1\n",
        )
        .unwrap();
        let batch = read_open_batch(&dir, "service").unwrap();
        assert_eq!(batch.members, vec!["sp-1", "sp-2", "sp-3"]);
        assert_eq!(batch.head.as_deref(), Some("deadbeef"));
        assert_eq!(batch.base.as_deref(), Some("cafef00d"));
        assert_eq!(batch.opened_at, Some(1700000000));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_repository_with_no_open_batch_is_none() {
        let dir = scratch("no-batch");
        assert!(read_open_batch(&dir, "service").is_none());
        fs::remove_dir_all(&dir).ok();
    }
}
