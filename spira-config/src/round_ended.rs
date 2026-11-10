//! The record that a round's drivers must stop: `queue round abandon` writes it before it stops
//! any leg, and every process driving the round reads it before each poll and each launch.

use std::path::{Path, PathBuf};

fn marker(run: &Path, batch: &str) -> PathBuf {
    run.join("rounds").join(format!("{batch}.abandoned"))
}

fn result(run: &Path, batch: &str) -> PathBuf {
    run.join("rounds").join(format!("{batch}.result"))
}

pub fn mark_abandoned(run: &Path, batch: &str) {
    let _ = std::fs::create_dir_all(run.join("rounds"));
    let _ = std::fs::write(marker(run, batch), "");
}

pub fn clear_abandoned(run: &Path, batch: &str) {
    let _ = std::fs::remove_file(marker(run, batch));
}

/// A round with an abandon marker, or any recorded end, has no driver left to act for it.
pub fn is_ended(run: &Path, batch: &str) -> bool {
    marker(run, batch).exists() || result(run, batch).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marked_round_is_ended_until_the_mark_is_cleared() {
        let d = testkit::TempDir::new("round-ended");
        assert!(!is_ended(d.path(), "b1"));
        mark_abandoned(d.path(), "b1");
        assert!(is_ended(d.path(), "b1"));
        assert!(!is_ended(d.path(), "b2"));
        clear_abandoned(d.path(), "b1");
        assert!(!is_ended(d.path(), "b1"));
    }

    #[test]
    fn a_round_with_a_result_line_is_ended() {
        let d = testkit::TempDir::new("round-ended-result");
        std::fs::create_dir_all(d.path().join("rounds")).unwrap();
        std::fs::write(d.path().join("rounds/b1.result"), "x abandoned y\n").unwrap();
        assert!(is_ended(d.path(), "b1"));
    }
}
