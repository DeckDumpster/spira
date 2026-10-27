//! Prerequisites whose certified tip a dependent's stacked base could not merge. The dwarf
//! that refused the claim writes the marker; `spira-claim` reads it so the dependent stays
//! blocked on that prerequisite until its tip moves.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const DIR: &str = "stack-conflict";

fn marker(run: &Path, prereq: &str) -> PathBuf {
    run.join(DIR).join(prereq)
}

pub fn record(run: &Path, prereq: &str, tip: &str) {
    if std::fs::create_dir_all(run.join(DIR)).is_ok() {
        let _ = std::fs::write(marker(run, prereq), format!("{tip}\n"));
    }
}

/// A marker blocks for one round cadence; the next attempt re-records it if the conflict stands.
pub const MAX_AGE: Duration = Duration::from_secs(45 * 60);

/// Markers older than this that still match a live prerequisite are reported by the sentinel.
pub const REPORT_AGE: Duration = Duration::from_secs(3600);

pub fn age(run: &Path, prereq: &str) -> Option<Duration> {
    let m = std::fs::metadata(marker(run, prereq)).ok()?;
    Some(SystemTime::now().duration_since(m.modified().ok()?).unwrap_or_default())
}

/// The tip a marker was recorded against, and how old it is.
pub fn recorded(run: &Path, prereq: &str) -> Option<(String, Duration)> {
    let tip = std::fs::read_to_string(marker(run, prereq)).ok()?;
    Some((tip.trim().to_string(), age(run, prereq)?))
}

/// Every prerequisite with a marker.
pub fn marked(run: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(run.join(DIR)) else { return Vec::new() };
    rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect()
}

/// True only while `tip` is the tip the conflict was recorded against and the marker is
/// younger than [`MAX_AGE`].
pub fn holds(run: &Path, prereq: &str, tip: &str) -> bool {
    recorded(run, prereq).is_some_and(|(t, a)| t == tip && a < MAX_AGE)
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

    #[test]
    fn an_old_marker_lapses_though_its_tip_never_moved() {
        let d = testkit::TempDir::new("stack-conflict-old");
        record(d.path(), "sp-a", "t1");
        let f = std::fs::File::options().write(true).open(d.path().join(DIR).join("sp-a")).unwrap();
        f.set_modified(SystemTime::now() - MAX_AGE - Duration::from_secs(60)).unwrap();
        assert!(!holds(d.path(), "sp-a", "t1"));
        assert_eq!(marked(d.path()), vec!["sp-a".to_string()]);
        record(d.path(), "sp-a", "t1");
        assert!(holds(d.path(), "sp-a", "t1"), "a re-recorded conflict blocks again");
    }
}
