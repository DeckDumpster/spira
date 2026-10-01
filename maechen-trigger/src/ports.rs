//! The boundary `real.rs` implements: `spira_home_repo`/`repo_root`/`spira_landref`
//! in-process via `spira_config::repos::Registry`; `spira_lane_admitted`/
//! `spira_open_trigger_count`/`spira_repo_lanes` in-process via `crate::lanes` (wave 4.35,
//! sp-kelr2, row V); `detect_invalid_closed` in-process via `strand::detectors` (wave 4.29,
//! sp-8ofmt, row T) — none of the old six-function `lib.sh` seam DESIGN.md's "Non-goals"
//! once named is still a seam; `bd` directly (the bash calls `bd create`, not `bdq` —
//! DESIGN.md "Decisions"); and `git log`.

use std::path::{Path, PathBuf};

pub trait World {
    fn log(&self, msg: &str);

    /// `$SPIRA_RUN/maechen.watermark` — epoch seconds, or 0 if missing/unparseable.
    fn read_watermark(&self) -> i64;
    /// `$SPIRA_RUN/maechen.lastpass` — epoch seconds, or 0 if missing/unparseable.
    fn read_lastpass(&self) -> i64;
    fn now(&self) -> i64;

    /// `spira_open_trigger_count <labels>`.
    fn open_trigger_count(&self, labels: &str) -> u64;
    /// `spira_lane_admitted <lane>`.
    fn lane_admitted(&self, lane: &str) -> bool;
    /// `spira_repo_lanes <name>` — reached by `main.rs`'s `repo-lanes` CLI door, the one
    /// lib.sh's shim calls; `groom-trigger.sh` is that door's one remaining live caller.
    fn repo_lanes(&self, name: &str) -> Result<String, String>;

    /// `spira_home_repo` (the `lib.sh` seam).
    fn home_repo(&self) -> String;
    /// `repo_root <name>` — `None` when the map has no entry or the checkout is absent.
    fn repo_root(&self, name: &str) -> Option<PathBuf>;
    /// `spira_landref <repo_path_or_name>` — `None` when it cannot be resolved.
    fn landref(&self, repo_path_or_name: &str) -> Option<String>;
    /// The raw text of `$SPIRA_REPO_MAP`, or empty if unset/unreadable.
    fn repo_map_text(&self) -> String;

    /// `git -C <repo_path> log --format=%s --after=@<since_ts> <base_ref>`.
    fn git_log_subjects(&self, repo_path: &Path, since_ts: i64, base_ref: &str) -> String;

    /// `detect_invalid_closed` (the `lib.sh` seam), raw multi-line output.
    fn detect_invalid_closed(&self) -> String;

    /// `bd -C <db> create <title> --type task --label <labels> --priority 3 --description
    /// <description>`. `Ok(())` on exit 0.
    fn create_bead(&self, title: &str, labels: &str, description: &str) -> Result<(), String>;
}
