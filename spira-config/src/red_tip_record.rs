//! The batcher's record of the tip a red was attributed to: `<run>/batcher-red-tips/<repo>/<id>`,
//! first word the tip. The cut and the doors that withdraw a red name it through here.

use std::fs;
use std::path::{Path, PathBuf};

pub fn path(run: &Path, repo: &str, id: &str) -> PathBuf {
    run.join("batcher-red-tips").join(repo).join(id)
}

/// Removes `id`'s record when it names `tip` or an abbreviation of it either way round, as the queue's own red row is matched.
pub fn forget(run: &Path, repo: &str, id: &str, tip: &str) -> std::io::Result<()> {
    let p = path(run, repo, id);
    let Ok(text) = fs::read_to_string(&p) else {
        return Ok(());
    };
    match text.split_whitespace().next() {
        Some(t) if !tip.is_empty() && (t.starts_with(tip) || tip.starts_with(t)) => {
            fs::remove_file(&p)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forget_removes_only_a_record_naming_that_tip() {
        let run = testkit::TempDir::new("red-tip-record");
        let p = path(run.path(), "r", "sp-a");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "t1 filed\n").unwrap();
        forget(run.path(), "r", "sp-a", "t9").unwrap();
        assert!(p.exists(), "another tip's record stays");
        forget(run.path(), "r", "sp-a", "").unwrap();
        assert!(p.exists(), "an empty tip names nothing");
        fs::write(&p, "abcdef1 filed\n").unwrap();
        forget(run.path(), "r", "sp-a", "abcd").unwrap();
        assert!(!p.exists(), "an abbreviated tip clears the full record");
        fs::write(&p, "abcd\n").unwrap();
        forget(run.path(), "r", "sp-a", "abcdef1").unwrap();
        assert!(!p.exists());
        forget(run.path(), "r", "sp-a", "abcd").unwrap();
    }
}
