//! The boundary `real.rs` implements against `bd`, `gh`, `git`, `bead.sh`, `tsd-ingest.sh`
//! and the `lib.sh` repository-map seam (DESIGN.md "Non-goals" — none of those five are
//! ported).

use std::path::{Path, PathBuf};

pub trait World {
    /// Every repository name in the map, in `lib.sh`'s own order.
    fn repo_names(&self) -> Vec<String>;
    /// `repo_land <name>` — `pr`, `push` or `hold`.
    fn repo_land(&self, name: &str) -> String;
    /// `repo_root <name>` — `None` when the map has no entry or the checkout is absent.
    fn repo_root(&self, name: &str) -> Option<PathBuf>;
    /// `spira_home_repo`.
    fn home_repo(&self) -> String;

    fn bd_gate_list_json(&self) -> String;
    /// `(cd <repo>; bd gate discover --branch <branch>)`, best-effort — its exit code and
    /// output are never read, same as the bash's `|| true`.
    fn bd_gate_discover(&self, repo: &Path, branch: &str);
    /// `bd gate check --type=gh:run`, combined output.
    fn bd_gate_check(&self) -> String;
    fn bd_show_json(&self, id: &str) -> String;
    fn bd_gate_resolve(&self, id: &str);
    /// `spira_event <kind> <subject> <summary> <detail>` (the `lib.sh` seam).
    fn spira_event(&self, kind: &str, subject: &str, summary: &str, detail: &str);

    fn bd_list_json(&self) -> String;
    fn bd_priority(&self, id: &str, p: i64);
    fn bd_note(&self, id: &str, text: &str);
    /// `bead.sh file <title> --for builder --repo <repo> -p <priority> --body-file -`, `body`
    /// on stdin.
    fn file_bead(&self, title: &str, repo: &str, priority: i64, body: &str);

    /// `SPIRA_FLAKY_GH_REPO`, only when non-empty and `gh` is on `PATH` — legs 5-7 are a
    /// no-op otherwise, exactly as the bash's own guard read.
    fn flaky_repo(&self) -> Option<String>;
    /// `gh run list --repo <repo> --status completed --limit <limit> --json databaseId --jq
    /// '.[].databaseId'`.
    fn gh_recent_run_ids(&self, repo: &str, limit: u32) -> Vec<String>;
    /// `gh api repos/<repo>/actions/runs/<run>/jobs`, raw JSON.
    fn gh_jobs_json(&self, repo: &str, run_id: &str) -> String;
    /// `gh api repos/<repo>/check-runs/<job>/annotations`, raw JSON.
    fn gh_annotations_json(&self, repo: &str, job_id: &str) -> String;
    /// `gh run list --repo <repo> --branch main --status success --limit 1 --json headSha
    /// --jq '.[0].headSha'`.
    fn gh_last_green_main_sha(&self, repo: &str) -> Option<String>;
    /// `gh run list --repo <repo> --branch main --status failure --limit <limit> --json
    /// databaseId,headSha --jq '.[] | [(.databaseId|tostring), .headSha] | @tsv'`.
    fn gh_failed_main_runs(&self, repo: &str, limit: u32) -> Vec<(String, String)>;
    /// `gh run view <run> --repo <repo> --log-failed | grep 'FAIL' | sed 's/^[^\t]*\t[^\t]*\t//'
    /// | head -10`.
    fn gh_fail_lines(&self, repo: &str, run_id: &str) -> String;

    /// `git -C <repo_root> log --oneline <from>..<to>`.
    fn git_log_range(&self, repo_root: &Path, from: &str, to: &str) -> String;

    /// `tsd-ingest.sh <repo> <run-id>` from `<home>`.
    fn tsd_ingest(&self, home: &Path, repo: &str, run_id: &str);

    fn print(&self, s: &str);
}
