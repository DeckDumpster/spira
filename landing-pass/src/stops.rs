//! The `land` lane: a repo whose lane is stopped lands nothing, and the pass says so.

use crate::model::RepoRow;
use crate::report::Reporter;
use spira_config::scoped_stop::{skip_line, stop_for};
use std::path::Path;

pub const LANE: &str = "land";

/// `repos` less every repo whose `land` lane is stopped, each skip logged with who lifts it.
pub fn landable(run: &Path, repos: Vec<RepoRow>, out: &Reporter) -> Vec<RepoRow> {
    repos
        .into_iter()
        .filter(|r| match stop_for(run, &r.name, LANE) {
            Some(s) => {
                out.log(&skip_line("landing-pass", LANE, &s));
                false
            }
            None => true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LandMode;

    fn row(name: &str) -> RepoRow {
        RepoRow { name: name.into(), path: "/r".into(), mode: LandMode::QueueLocal, landref: None, base_fq: None, base_remote: None, base_branch: "main".into(), forge_ref: None }
    }

    #[test]
    fn a_repo_whose_land_lane_is_stopped_is_skipped_and_named() {
        let d = testkit::TempDir::new("landing-pass-land-lane");
        spira_config::scoped_stop::pause_repo(d.path(), "other", &["land".to_string()], "cutover-mode", "cutover", "world stop", 1).unwrap();
        spira_config::scoped_stop::pause_repo(d.path(), "spira", &["rounds".to_string()], "o", "r", "u", 1).unwrap();
        let out = Reporter::capture(None);
        let left: Vec<String> = landable(d.path(), vec![row("spira"), row("other")], &out).into_iter().map(|r| r.name).collect();
        assert_eq!(left, ["spira"], "another lane's stop lands as before");
        let lines = out.lines();
        assert!(lines.iter().any(|l| l.contains("land lane of other") && l.contains("world stop")), "{lines:?}");
    }
}
