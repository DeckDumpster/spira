//! Prerequisites whose certified tip a dependent's stacked base could not merge. The aeon
//! that refused the claim writes the marker; `spira-claim` reads it so the dependent stays
//! blocked on that prerequisite until its tip moves.

use std::path::{Path, PathBuf};

const DIR: &str = "stack-conflict";

fn marker(run: &Path, prereq: &str) -> PathBuf {
    run.join(DIR).join(prereq)
}

pub fn record(run: &Path, prereq: &str, tip: &str) {
    if std::fs::create_dir_all(run.join(DIR)).is_ok() {
        let _ = std::fs::write(marker(run, prereq), format!("{tip}\n"));
    }
}

/// True only while `tip` is the tip the conflict was recorded against.
pub fn holds(run: &Path, prereq: &str, tip: &str) -> bool {
    std::fs::read_to_string(marker(run, prereq)).is_ok_and(|t| t.trim() == tip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_holds_for_its_tip_and_lapses_when_the_tip_moves() {
        let d = testkit::TempDir::new("stack-conflict");
        assert!(!holds(d.path(), "sp-a", "t1"), "no marker, no conflict");
        record(d.path(), "sp-a", "t1");
        assert!(holds(d.path(), "sp-a", "t1"));
        assert!(!holds(d.path(), "sp-a", "t2"), "a rebased prerequisite is free again");
    }
}
