//! A runtime task's pulse: its observe stamps a file under `$SPIRA_RUN` each time it answers,
//! and whoever asks whether the task is alive reads the file's age.

use std::path::{Path, PathBuf};

pub const DIR: &str = "task-ticked";

pub fn path(run_dir: &Path, task: &str) -> PathBuf {
    run_dir.join(DIR).join(task)
}

pub fn stamp(run_dir: &Path, task: &str) {
    let file = path(run_dir, task);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&file, "");
}

/// Seconds since the task last stamped; `None` when it never has.
pub fn quiet_secs(run_dir: &Path, task: &str) -> Option<u64> {
    std::fs::metadata(path(run_dir, task)).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs())
}

/// The task an effect name is the observe probe of: `batcher.local.observe` is `batcher.local`'s.
pub fn task_of_probe(effect: &str) -> Option<&str> {
    effect.strip_suffix(".observe").filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_observe_probe_names_a_task() {
        assert_eq!(task_of_probe("batcher.local.observe"), Some("batcher.local"));
        assert_eq!(task_of_probe("batcher.local.land"), None);
        assert_eq!(task_of_probe(".observe"), None);
    }

    #[test]
    fn a_stamp_is_fresh_and_a_task_that_never_stamped_has_no_age() {
        let run = testkit::TempDir::new("ticked");
        assert_eq!(quiet_secs(&run, "batcher.local"), None);
        stamp(&run, "batcher.local");
        assert!(quiet_secs(&run, "batcher.local").is_some_and(|s| s < 5));
    }
}
