//! Everything census.sh's orchestration logic needs from the world, as a trait (DESIGN.md
//! §4). The SQL itself (`_census_events_sql`, `_census_class_fold_map`, the retry-aware
//! `census_events_run_sql`/`census_handwritten_run_sql`/`census_deliberate_run_sql`) and
//! the six `census/*.py` pipeline scripts are UNCHANGED, still lib.sh and still Python
//! respectively — this crate moves only the bash glue around them into Rust (DESIGN.md §2).

use std::path::Path;

pub trait World {
    fn env(&self, k: &str) -> Option<String>;

    // ---- lib.sh seam: the retry-aware SQL runners and the fold map, never re-derived ----
    /// `census_events_run_sql [since_epoch_s]` -> Ok(tabular output) or Err(its stderr) when
    /// the substrate stayed unreachable after lib.sh's own 3 retries.
    fn census_events_run_sql(&self, since: Option<i64>) -> Result<String, String>;
    fn census_handwritten_run_sql(&self) -> String;
    fn census_deliberate_run_sql(&self, since: Option<i64>) -> String;
    /// `_census_class_fold_map` -> "<alias> <canonical>" lines.
    fn census_class_fold_map(&self) -> String;
    /// `_census_deliberate_reopen_causes`'s cause NAMES alone (the admission-exemption
    /// flag that function also carries is row I's business, `bead_reopen`'s, not this
    /// query's) — `spira-claim deliberate-causes` is the one declared list now (sp-3wfcb,
    /// row I); empty on any failure to reach it (fails toward an empty IN-list, matching
    /// every other census SQL field).
    fn deliberate_cause_names(&self) -> Vec<String>;
    /// `repo_root` with no argument — the default repository census's own closed-remedy
    /// check reads git branches from.
    fn repo_root(&self) -> Option<String>;
    /// `landed <id>` -> its exit code (0 landed, 1 not landed, 2 unknown).
    fn landed(&self, id: &str) -> i32;

    // ---- the census/*.py pipeline, unchanged, run as subprocesses exactly as bash ran them ----
    /// `count.py < tabular>` -> Ok("<beads> <events> <class>" lines) or Err when the
    /// process itself could not run.
    fn count_py(&self, tabular: &str) -> Result<String, String>;
    /// `merge.py <all-time-file> <since-wm-file>`.
    fn merge_py(&self, all_time: &str, since_wm: &str) -> String;
    /// `covers.py <fold-map-file> < bdq-json>`.
    fn covers_py(&self, bdq_json: &str, fold_map: &str) -> String;
    /// `covers_closed.py <fold-map-file> < bdq-json>`.
    fn covers_closed_py(&self, bdq_json: &str, fold_map: &str) -> String;
    fn handwritten_py(&self, tabular: &str) -> String;
    fn deliberate_py(&self, tabular: &str) -> String;

    // ---- bd, direct (a `list` read: bdq's guards apply only to create/reopen/update/close) ----
    fn bd_list_json(&self, status: &str, label: &str) -> String;

    // ---- git ----
    fn git_branch_exists_matching(&self, repo: &str, pattern: &str) -> bool;

    // ---- the clock-skew guard ----
    /// `${SPIRA_NOW:-$(date -u +%s)}`.
    fn host_utc_epoch(&self) -> i64;
    /// The third line (`sed -n '3p'`) of `bd -C $SPIRA_DB sql "SELECT
    /// DATE_FORMAT(UTC_TIMESTAMP(), ...) AS utc_fn"` -> Ok(that row) or Err(combined
    /// output) when the query itself fails.
    fn bd_sql_utc_now_row(&self) -> Result<String, String>;
    /// `date -u -d "<s>" +%s` -> the epoch, or None when `s` does not parse.
    fn parse_utc_to_epoch(&self, s: &str) -> Option<i64>;
    /// `date -u -d "@<epoch>" '+%Y-%m-%d %H:%M:%S'`.
    fn format_epoch_utc(&self, epoch: i64) -> String;

    // ---- filesystem ----
    fn read_to_string(&self, p: &Path) -> Option<String>;

    fn out(&self, s: &str);
    fn err(&self, s: &str);
}
