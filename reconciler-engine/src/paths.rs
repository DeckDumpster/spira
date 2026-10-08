//! Where the reconciler family keeps its files under the run directory — one definition, so
//! the passes that write them and the detectors that read them cannot disagree.

use std::path::{Path, PathBuf};

pub fn state(run: &Path) -> PathBuf {
    run.join("reconciler-state.json")
}

pub fn status_log(run: &Path) -> PathBuf {
    run.join("tsd").join("reconciler-status.jsonl")
}

pub fn pass_stamp(run: &Path) -> PathBuf {
    run.join("reconciler-pass.stamp")
}

pub fn remedy_log(run: &Path) -> PathBuf {
    run.join("tsd").join("reconciler-remedy.jsonl")
}

pub fn effect_state(run: &Path) -> PathBuf {
    run.join("reconciler-effects.json")
}

pub fn alerted(run: &Path) -> PathBuf {
    run.join("reconciler-alerted.json")
}

pub fn reminders(run: &Path) -> PathBuf {
    run.join("reconciler-reminders.json")
}

pub fn main_health_tree(run: &Path) -> PathBuf {
    run.join("main-health-tree")
}

pub fn main_health_log(run: &Path, class: &str) -> PathBuf {
    run.join(format!("main-health-{class}.log"))
}

pub fn flow_state(run: &Path) -> PathBuf {
    run.join("reconciler-flow-state.json")
}

pub fn flow_alerted(run: &Path) -> PathBuf {
    run.join("reconciler-flow-alerted.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_log_lives_under_tsd() {
        assert_eq!(status_log(Path::new("/r")), Path::new("/r/tsd/reconciler-status.jsonl"));
    }
}
