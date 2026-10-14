//! Unit tests: a fake `Gh`/`Proc` that answers canned JSON by argv, exactly the technique
//! `test-forge-*.sh` used with a stub `gh` script — now in-process. Covers every verb's
//! fail-closed answer and the JSON-shaped decisions forge.sh's inline Python made.

use crate::cmds::*;
use crate::ports::{Gh, GhOut, Proc};
use crate::time::epoch;
use std::cell::RefCell;
use std::path::Path;

type Canned = (Vec<String>, i32, Vec<u8>);

/// Matches on the full argv (joined by `\x1f` for exact comparison) → canned (code, stdout).
#[derive(Default)]
struct FakeGh {
    answers: RefCell<Vec<Canned>>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeGh {
    fn on(&self, args: &[&str], code: i32, stdout: &str) {
        self.answers.borrow_mut().push((args.iter().map(|s| s.to_string()).collect(), code, stdout.as_bytes().to_vec()));
    }
}

impl Gh for FakeGh {
    fn call(&self, _repo: Option<&Path>, args: &[&str]) -> GhOut {
        self.calls.borrow_mut().push(args.iter().map(|s| s.to_string()).collect());
        for (pat, code, out) in self.answers.borrow().iter() {
            if pat.iter().map(String::as_str).eq(args.iter().copied()) {
                return GhOut { code: *code, stdout: out.clone() };
            }
        }
        GhOut { code: 1, stdout: Vec::new() }
    }
    fn call_merged(&self, repo: Option<&Path>, args: &[&str], _input: Option<&[u8]>) -> GhOut {
        self.call(repo, args)
    }
}

#[derive(Default)]
struct NoProc;
impl Proc for NoProc {
    fn run(&self, _program: &str, _args: &[&str], _input: Option<&[u8]>) -> (i32, Vec<u8>) {
        (1, Vec::new())
    }
}

fn repo() -> &'static Path {
    Path::new("/tmp/repo-does-not-need-to-exist")
}

// ── check-status ─────────────────────────────────────────────────────────────────────────

#[test]
fn check_status_with_no_branch_is_pending_never_a_guess() {
    let gh = FakeGh::default();
    let out = check_status(&gh, &NoProc, repo(), "Gate", "");
    assert_eq!(out.lines, vec!["pending"]);
    assert!(gh.calls.borrow().is_empty(), "must not even ask gh without a branch to scope the run");
}

#[test]
fn check_status_no_run_is_pending() {
    let gh = FakeGh::default();
    gh.on(&["run", "list", "--branch", "spira/sp-x", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"], 0, "[]");
    let out = check_status(&gh, &NoProc, repo(), "Gate", "spira/sp-x");
    assert_eq!(out.lines, vec!["pending"]);
}

#[test]
fn check_status_green_on_success() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "spira/sp-x", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":42,"status":"completed","conclusion":"success","headSha":"abc123","url":"https://x/42"}]"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/42/jobs"], 0, r#"{"jobs":[]}"#);
    let out = check_status(&gh, &NoProc, repo(), "Gate", "spira/sp-x");
    assert_eq!(out.lines[0], "green");
    assert!(out.lines.contains(&"head-sha: abc123".to_string()));
    assert!(out.lines.contains(&"run-url: https://x/42".to_string()));
}

#[test]
fn check_status_cancelled_conclusion_is_harness_fault_not_red() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":1,"status":"completed","conclusion":"cancelled"}]"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/1/jobs"], 0, r#"{"jobs":[]}"#);
    let out = check_status(&gh, &NoProc, repo(), "Gate", "b");
    assert_eq!(out.lines[0], "harness_fault");
}

#[test]
fn check_status_provision_job_failure_overrides_red() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":7,"status":"completed","conclusion":"failure"}]"#,
    );
    gh.on(
        &["api", "repos/{owner}/{repo}/actions/runs/7/jobs"],
        0,
        r#"{"jobs":[{"name":"provision","conclusion":"failure"},{"name":"suites","conclusion":"failure"}]}"#,
    );
    let out = check_status(&gh, &NoProc, repo(), "Gate", "b");
    assert_eq!(out.lines[0], "provision_fault");
}

#[test]
fn check_status_provision_fault_names_a_capacity_marker_from_the_provision_log() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":7,"status":"completed","conclusion":"failure"}]"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/7/jobs"], 0, r#"{"jobs":[{"id":41,"name":"provision","conclusion":"failure"}]}"#);
    gh.on(&["api", "repos/{owner}/{repo}/actions/jobs/41/logs"], 0, "2026-10-14T01:00:00.000Z Error: VcpuLimitExceeded: current vCPU limit of 64\n");
    let out = check_status(&gh, &NoProc, repo(), "Gate", "b");
    assert_eq!(out.lines[0], "provision_fault");
    assert!(out.lines.contains(&"capacity: VcpuLimitExceeded".to_string()), "{:?}", out.lines);
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":7,"status":"completed","conclusion":"failure"}]"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/7/jobs"], 0, r#"{"jobs":[{"id":41,"name":"provision","conclusion":"failure"}]}"#);
    gh.on(&["api", "repos/{owner}/{repo}/actions/jobs/41/logs"], 0, "Error: ssh timed out\n");
    let out = check_status(&gh, &NoProc, repo(), "Gate", "b");
    assert!(!out.lines.iter().any(|l| l.starts_with("capacity: ")), "{:?}", out.lines);
}

#[test]
fn check_status_red_with_only_a_non_suite_step_names_the_step_and_its_log_tail() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":7,"status":"completed","conclusion":"failure"}]"#,
    );
    gh.on(
        &["api", "repos/{owner}/{repo}/actions/runs/7/jobs"],
        0,
        r#"{"jobs":[{"id":99,"name":"build","conclusion":"success"},{"id":55,"name":"stage","conclusion":"failure","steps":[{"name":"Checkout","conclusion":"success"},{"name":"Stage the build as a release","conclusion":"failure"}]}]}"#,
    );
    let mut log = String::new();
    for i in 0..30 {
        log.push_str(&format!("2026-10-10T01:02:03.456Z line {i}\n"));
    }
    log.push_str("2026-10-10T01:02:04.000Z release: cannot create /x: Permission denied (os error 13)\n2026-10-10T01:02:04.100Z ##[error]Process completed with exit code 1.\n2026-10-10T01:02:05.000Z Post job cleanup.\n");
    gh.on(&["api", "repos/{owner}/{repo}/actions/jobs/55/logs"], 0, &log);
    let out = check_status(&gh, &NoProc, repo(), "Gate", "b");
    assert_eq!(out.lines[0], "red");
    assert!(out.lines.contains(&"run-id: 7".to_string()), "{:?}", out.lines);
    assert!(out.lines.contains(&"failed-step: stage / Stage the build as a release".to_string()), "{:?}", out.lines);
    let tail: Vec<&String> = out.lines.iter().filter(|l| l.starts_with("step-log: ")).collect();
    assert_eq!(tail.len(), 20);
    assert_eq!(tail[18], "step-log: release: cannot create /x: Permission denied (os error 13)");
    assert_eq!(tail[19], "step-log: ##[error]Process completed with exit code 1.");
}

#[test]
fn check_status_suites_job_cancelled_is_harness_fault_not_a_verdict() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":7,"status":"completed","conclusion":"failure"}]"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/7/jobs"], 0, r#"{"jobs":[{"name":"suites","conclusion":"cancelled"}]}"#);
    let out = check_status(&gh, &NoProc, repo(), "Gate", "b");
    assert_eq!(out.lines[0], "harness_fault");
}

#[test]
fn check_status_build_failure_extracts_deduped_capped_error_lines() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":7,"status":"completed","conclusion":"failure"}]"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/7/jobs"], 0, r#"{"jobs":[{"name":"build","id":99,"conclusion":"failure"}]}"#);
    let log = "2026-09-29T00:00:00.000Z error[E0433]: failed to resolve\n\
               2026-09-29T00:00:01.000Z error[E0433]: failed to resolve\n\
               2026-09-29T00:00:02.000Z note: this is not an error line\n\
               2026-09-29T00:00:03.000Z error: could not compile `x`\n";
    gh.on(&["api", "repos/{owner}/{repo}/actions/jobs/99/logs"], 0, log);
    let out = check_status(&gh, &NoProc, repo(), "Gate", "b");
    let errs: Vec<&String> = out.lines.iter().filter(|l| l.starts_with("build-error:")).collect();
    assert_eq!(errs.len(), 2, "the repeated E0433 line dedupes to one and the note line is excluded, got {errs:?}");
}

#[test]
fn pr_merge_commit_prints_the_sha_and_fails_closed_when_there_is_none() {
    let gh = FakeGh::default();
    gh.on(&["pr", "view", "5", "--json", "mergeCommit", "-q", ".mergeCommit.oid"], 0, "abc123\n");
    assert_eq!(pr_merge_commit(&gh, repo(), "5").lines, vec!["abc123"]);
    let none = FakeGh::default();
    let out = pr_merge_commit(&none, repo(), "5");
    assert!(out.code != 0 && out.lines.is_empty());
    none.on(&["pr", "view", "5", "--json", "mergeCommit", "-q", ".mergeCommit.oid"], 0, "null\n");
    assert!(pr_merge_commit(&none, repo(), "5").lines.is_empty());
}

// ── pr-mergeability / pr-state ───────────────────────────────────────────────────────────

#[test]
fn pr_mergeability_maps_conflicting_and_dirty_to_dirty() {
    let gh = FakeGh::default();
    gh.on(&["pr", "view", "5", "--json", "mergeable,mergeStateStatus"], 0, r#"{"mergeable":"CONFLICTING","mergeStateStatus":"CLEAN"}"#);
    assert_eq!(pr_mergeability(&gh, repo(), "5").lines, vec!["DIRTY"]);
}

#[test]
fn pr_mergeability_unreadable_is_unknown_not_a_guess() {
    let gh = FakeGh::default();
    assert_eq!(pr_mergeability(&gh, repo(), "5").lines, vec!["UNKNOWN"]);
}

#[test]
fn pr_state_maps_every_github_state_and_falls_closed_to_unknown() {
    let gh = FakeGh::default();
    gh.on(&["pr", "view", "spira/sp-a", "--json", "state", "-q", ".state"], 0, "MERGED\n");
    assert_eq!(pr_state(&gh, repo(), "spira/sp-a").lines, vec!["merged"]);
    let gh2 = FakeGh::default();
    assert_eq!(pr_state(&gh2, repo(), "spira/sp-b").lines, vec!["unknown"]);
}

#[test]
fn pr_red_names_the_failing_job_and_its_fail_lines_and_is_silent_otherwise() {
    let gh = FakeGh::default();
    gh.on(
        &["pr", "view", "spira/sp-a", "--json", "headRefOid,statusCheckRollup"],
        0,
        r#"{"headRefOid":"abc123","statusCheckRollup":[
            {"name":"lint","conclusion":"SUCCESS","detailsUrl":"https://x/actions/runs/1/job/10"},
            {"name":"suites","conclusion":"FAILURE","detailsUrl":"https://x/actions/runs/1/job/77"}]}"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/jobs/77/logs"], 0, "2026-10-10T00:00:01.0Z ok 1 a\n2026-10-10T00:00:02.0Z not ok 2 b\n");
    assert_eq!(
        pr_red(&gh, repo(), "spira/sp-a").lines,
        vec!["head abc123", "job suites", "fail-line: 2026-10-10T00:00:02.0Z not ok 2 b"]
    );
    let green = FakeGh::default();
    green.on(&["pr", "view", "spira/sp-b", "--json", "headRefOid,statusCheckRollup"], 0, r#"{"headRefOid":"d","statusCheckRollup":[{"name":"x","conclusion":"SUCCESS"}]}"#);
    assert!(pr_red(&green, repo(), "spira/sp-b").lines.is_empty());
    assert!(pr_red(&FakeGh::default(), repo(), "spira/sp-c").lines.is_empty());
}

// ── runs-active: fail-closed '?', never 0 ───────────────────────────────────────────────

#[test]
fn runs_active_prints_question_mark_when_the_api_call_fails() {
    let gh = FakeGh::default();
    assert_eq!(runs_active(&gh, repo()).lines, vec!["?"]);
}

#[test]
fn runs_active_counts_only_pull_request_runs_belonging_to_an_open_pr() {
    let gh = FakeGh::default();
    gh.on(
        &["api", "repos/{owner}/{repo}/actions/runs?per_page=100"],
        0,
        r#"{"workflow_runs":[
            {"event":"pull_request","status":"queued","pull_requests":[{"number":1}]},
            {"event":"pull_request","status":"in_progress","pull_requests":[{"number":2}]},
            {"event":"push","status":"queued","pull_requests":[]},
            {"event":"pull_request","status":"completed","pull_requests":[{"number":1}]}
        ]}"#,
    );
    gh.on(&["pr", "list", "--state", "open", "--json", "number"], 0, r#"[{"number":1}]"#);
    // Run for PR #2 is pull_request+queued-shaped but PR #2 is not open — excluded. The push
    // event is excluded outright. The completed run is excluded by status.
    assert_eq!(runs_active(&gh, repo()).lines, vec!["1"]);
}

#[test]
fn runs_active_a_run_with_no_linked_pr_counts_as_active() {
    let gh = FakeGh::default();
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs?per_page=100"], 0, r#"{"workflow_runs":[{"event":"pull_request","status":"queued","pull_requests":[]}]}"#);
    gh.on(&["pr", "list", "--state", "open", "--json", "number"], 0, "[]");
    assert_eq!(runs_active(&gh, repo()).lines, vec!["1"]);
}

// ── branch-protection-status: fail closed to unprotected ────────────────────────────────

#[test]
fn branch_protection_status_unreadable_is_unprotected_not_a_guess() {
    let gh = FakeGh::default();
    assert_eq!(branch_protection_status(&gh, repo(), "main").lines, vec!["unprotected"]);
}

#[test]
fn branch_protection_status_reads_the_protected_field() {
    let gh = FakeGh::default();
    gh.on(&["api", "repos/{owner}/{repo}/branches/main"], 0, r#"{"protected":true}"#);
    assert_eq!(branch_protection_status(&gh, repo(), "main").lines, vec!["protected"]);
}

// ── pr-list-queue / pr-list-open (new) ───────────────────────────────────────────────────

#[test]
fn pr_list_queue_filters_to_the_queue_prefix() {
    let gh = FakeGh::default();
    gh.on(
        &["pr", "list", "--state", "open", "--json", "number,headRefName"],
        0,
        r#"[{"number":1,"headRefName":"spira/queue/abc"},{"number":2,"headRefName":"spira/sp-x"}]"#,
    );
    assert_eq!(pr_list_queue(&gh, repo()).lines, vec!["1"]);
}

#[test]
fn pr_list_open_lists_every_open_pr_with_its_branch() {
    let gh = FakeGh::default();
    gh.on(
        &["pr", "list", "--state", "open", "--json", "number,headRefName"],
        0,
        r#"[{"number":1,"headRefName":"spira/queue/abc"},{"number":2,"headRefName":"spira/sp-x"}]"#,
    );
    assert_eq!(pr_list_open(&gh, repo()).lines, vec!["1 spira/queue/abc", "2 spira/sp-x"]);
}

// ── pr-automerge (new) ────────────────────────────────────────────────────────────────────

const ARMED: &str = r#"{"autoMergeRequest":{"mergeMethod":"MERGE"},"state":"OPEN"}"#;
const UNARMED: &str = r#"{"autoMergeRequest":null,"state":"OPEN"}"#;

#[test]
fn pr_automerge_arms_squash_and_reports_gh_s_exit() {
    let gh = FakeGh::default();
    gh.on(&["pr", "merge", "--auto", "--squash", "spira/sp-a"], 0, "");
    gh.on(&["pr", "view", "spira/sp-a", "--json", "autoMergeRequest,state"], 0, ARMED);
    assert_eq!(pr_automerge(&gh, repo(), "spira/sp-a", "").code, 0);
    let gh2 = FakeGh::default();
    assert_eq!(pr_automerge(&gh2, repo(), "spira/sp-a", "").code, 1);
    let gh3 = FakeGh::default();
    gh3.on(&["pr", "merge", "--auto", "--merge", "7"], 0, "");
    gh3.on(&["pr", "view", "7", "--json", "autoMergeRequest,state"], 0, ARMED);
    assert_eq!(pr_automerge(&gh3, repo(), "7", "merge").code, 0);
    assert_eq!(pr_automerge(&gh3, repo(), "7", "bogus").code, 1);
}

#[test]
fn pr_automerge_is_not_armed_until_the_pr_shows_the_request() {
    let gh = FakeGh::default();
    gh.on(&["pr", "merge", "--auto", "--merge", "7"], 0, "");
    gh.on(&["pr", "view", "7", "--json", "autoMergeRequest,state"], 0, UNARMED);
    assert_eq!(pr_automerge(&gh, repo(), "7", "merge").code, 1);
    let gh2 = FakeGh::default();
    gh2.on(&["pr", "merge", "--auto", "--merge", "7"], 0, "");
    gh2.on(&["pr", "view", "7", "--json", "autoMergeRequest,state"], 0, r#"{"autoMergeRequest":null,"state":"MERGED"}"#);
    assert_eq!(pr_automerge(&gh2, repo(), "7", "merge").code, 0);
}

#[test]
fn pr_arm_queue_arms_only_unarmed_batch_prs() {
    let gh = FakeGh::default();
    gh.on(
        &["pr", "list", "--state", "open", "--json", "number,headRefName,autoMergeRequest"],
        0,
        r#"[{"number":1,"headRefName":"spira/queue/a","autoMergeRequest":null},
            {"number":2,"headRefName":"spira/queue/b","autoMergeRequest":{"mergeMethod":"MERGE"}},
            {"number":3,"headRefName":"spira/sp-x","autoMergeRequest":null}]"#,
    );
    gh.on(&["pr", "merge", "--auto", "--merge", "1"], 0, "");
    gh.on(&["pr", "view", "1", "--json", "autoMergeRequest,state"], 0, ARMED);
    let o = pr_arm_queue(&gh, repo(), "merge");
    assert_eq!((o.code, o.lines), (0, vec!["armed 1".to_string()]));
    let none = FakeGh::default();
    none.on(&["pr", "list", "--state", "open", "--json", "number,headRefName,autoMergeRequest"], 0, r#"[{"number":1,"headRefName":"spira/queue/a","autoMergeRequest":null}]"#);
    let o = pr_arm_queue(&none, repo(), "merge");
    assert_eq!((o.code, o.lines), (1, vec!["failed 1".to_string()]));
}

// ── run-metadata: both last-activity lines, per forge.sh's own double-print ─────────────

#[test]
fn run_metadata_prints_started_at_then_both_last_activity_lines() {
    let gh = FakeGh::default();
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/9"], 0, r#"{"run_started_at":"2026-09-29T00:00:00Z","updated_at":"2026-09-29T00:05:00Z"}"#);
    gh.on(
        &["api", "repos/{owner}/{repo}/actions/runs/9/jobs"],
        0,
        r#"{"jobs":[{"started_at":"2026-09-29T00:01:00Z","completed_at":"2026-09-29T00:10:00Z","steps":[]}]}"#,
    );
    let out = run_metadata(&gh, repo(), "9");
    let la: Vec<&String> = out.lines.iter().filter(|l| l.starts_with("last-activity:")).collect();
    assert_eq!(la.len(), 2, "the run-level and jobs-level last-activity both print; a reader takes the last one: {out:?}", out = out.lines);
    assert!(out.lines[0].starts_with("started-at:"));
}

#[test]
fn run_metadata_reports_the_runs_own_status() {
    let gh = FakeGh::default();
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/9"], 0, r#"{"status":"queued","run_started_at":"2026-09-29T00:00:00Z"}"#);
    let out = run_metadata(&gh, repo(), "9");
    assert!(out.lines.contains(&"run-status: queued".to_string()), "{:?}", out.lines);
}

#[test]
fn run_metadata_names_the_job_and_step_in_progress() {
    let gh = FakeGh::default();
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/9"], 0, r#"{"run_started_at":"2026-09-29T00:00:00Z"}"#);
    gh.on(
        &["api", "repos/{owner}/{repo}/actions/runs/9/jobs"],
        0,
        r#"{"jobs":[{"name":"lint","status":"completed","completed_at":"2026-09-29T00:02:00Z","steps":[]},{"name":"test","status":"completed","completed_at":"2026-09-29T00:03:00Z","steps":[]},{"name":"unit","status":"in_progress","started_at":"2026-09-29T00:01:00Z","steps":[{"name":"checkout","status":"completed"},{"name":"pytest","status":"in_progress"}]}]}"#,
    );
    let out = run_metadata(&gh, repo(), "9");
    assert!(out.lines.contains(&"running-step: unit / pytest".to_string()), "{:?}", out.lines);
}

#[test]
fn run_metadata_reads_declared_timeout_and_live_job_log_activity() {
    let gh = FakeGh::default();
    gh.on(
        &["api", "repos/{owner}/{repo}/actions/runs/9"],
        0,
        r#"{"run_started_at":"2026-09-29T00:00:00Z","updated_at":"2026-09-29T00:05:00Z","path":".github/workflows/gate.yml@refs/heads/b","head_sha":"abc"}"#,
    );
    gh.on(
        &["api", "-H", "Accept: application/vnd.github.raw", "repos/{owner}/{repo}/contents/.github/workflows/gate.yml?ref=abc"],
        0,
        "jobs:\n  a:\n    timeout-minutes: 30\n  b:\n    timeout-minutes: 90 # long\n",
    );
    gh.on(
        &["api", "repos/{owner}/{repo}/actions/runs/9/jobs"],
        0,
        r#"{"jobs":[{"id":5,"status":"in_progress","started_at":"2026-09-29T00:01:00Z","steps":[]}]}"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/jobs/5/logs"], 0, "2026-09-29T00:02:00.1234567Z a\n2026-09-29T00:40:00.7654321Z b\n\n");
    let out = run_metadata(&gh, repo(), "9").lines;
    assert!(out.contains(&"timeout-sec: 5400".to_string()), "{out:?}");
    assert_eq!(out.last().unwrap(), &format!("last-activity: {}", epoch("2026-09-29T00:40:00Z")), "{out:?}");
}

// ── batch-ci-status ───────────────────────────────────────────────────────────────────────

#[test]
fn batch_ci_status_no_run_prints_nothing() {
    let gh = FakeGh::default();
    gh.on(&["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,conclusion,status,updatedAt,headSha,url", "--limit", "1"], 0, "[]");
    assert!(batch_ci_status(&gh, &NoProc, repo(), "Gate", "b").lines.is_empty());
}

#[test]
fn batch_ci_status_reports_queued_since_from_jobs() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,conclusion,status,updatedAt,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":3}]"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/3/jobs"], 0, r#"{"jobs":[{"status":"queued","created_at":"2026-09-29T00:00:00Z"}]}"#);
    let out = batch_ci_status(&gh, &NoProc, repo(), "Gate", "b");
    assert!(out.lines.contains(&"run-id: 3".to_string()));
    assert!(out.lines.iter().any(|l| l.starts_with("queued-since:")));
}

// ── dispatch requires a ref ──────────────────────────────────────────────────────────────

#[test]
fn dispatch_refuses_without_a_ref() {
    let gh = FakeGh::default();
    let out = dispatch(&gh, repo(), "Gate", "", "some-suite");
    assert_eq!(out.code, 1);
}

// ── force-cancel names the VM it orphans ─────────────────────────────────────────────────

#[test]
fn force_cancel_posts_the_force_cancel_endpoint_after_reading_the_jobs() {
    let gh = FakeGh::default();
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/9/jobs"], 0, r#"{"jobs":[{"labels":["self-hosted","runner-a"]},{"labels":["runner-a"]}]}"#);
    gh.on(&["api", "--method", "POST", "repos/{owner}/{repo}/actions/runs/9/force-cancel"], 0, "");
    let out = force_cancel(&gh, repo(), "9");
    assert_eq!(out.code, 0);
    let calls = gh.calls.borrow();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].last().unwrap().ends_with("/jobs"), "jobs are read before the cancel: {calls:?}");
    assert!(calls[1].last().unwrap().ends_with("/force-cancel"));
}

#[test]
fn force_cancel_still_cancels_when_the_jobs_are_unreadable_and_refuses_without_a_run() {
    let gh = FakeGh::default();
    gh.on(&["api", "--method", "POST", "repos/{owner}/{repo}/actions/runs/9/force-cancel"], 0, "");
    assert_eq!(force_cancel(&gh, repo(), "9").code, 0);
    let gh = FakeGh::default();
    assert_eq!(force_cancel(&gh, repo(), "").code, 1);
    assert!(gh.calls.borrow().is_empty());
}

// ── workflow-rerun ───────────────────────────────────────────────────────────────────────

fn rerun_gh(conclusion: &str, jobs: &str) -> FakeGh {
    let gh = FakeGh::default();
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/9"], 0, &format!(r#"{{"status":"completed","conclusion":"{conclusion}","head_sha":"abc","path":".github/workflows/gate.yml"}}"#));
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/9/jobs"], 0, jobs);
    gh.on(&["run", "view", "9", "--json", "status", "-q", ".status"], 0, "completed");
    gh.on(&["run", "rerun", "9"], 0, "");
    gh
}

fn rerun_called(gh: &FakeGh) -> bool {
    gh.calls.borrow().iter().any(|c| c.first().map(String::as_str) == Some("run") && c.get(1).map(String::as_str) == Some("rerun"))
}

#[test]
fn workflow_rerun_of_a_decided_run_is_refused_with_the_unchanged_inputs() {
    for concl in ["success", "failure"] {
        let gh = rerun_gh(concl, r#"{"jobs":[{"name":"suites","conclusion":"failure"}]}"#);
        assert_eq!(workflow_rerun(&gh, repo(), "9").code, 3, "{concl}");
        assert!(!rerun_called(&gh), "{concl}: must not reach gh run rerun");
        let why = rerun_refusal(&gh, repo(), "9").unwrap();
        assert!(why.contains("abc") && why.contains("gate.yml") && why.contains("no input changed"), "{why}");
    }
}

#[test]
fn workflow_rerun_still_serves_a_fault_and_an_unreadable_run() {
    let faulted = rerun_gh("failure", r#"{"jobs":[{"name":"suites","conclusion":"cancelled"}]}"#);
    assert_eq!(workflow_rerun(&faulted, repo(), "9").code, 0);
    assert!(rerun_called(&faulted));
    let cancelled = rerun_gh("cancelled", r#"{"jobs":[]}"#);
    assert_eq!(workflow_rerun(&cancelled, repo(), "9").code, 0);
    let unreadable = FakeGh::default();
    unreadable.on(&["run", "rerun", "9"], 0, "");
    assert_eq!(workflow_rerun(&unreadable, repo(), "9").code, 0);
    assert!(rerun_called(&unreadable));
}

// ── stranded-runners ─────────────────────────────────────────────────────────────────────

const NOW: u64 = 1_790_640_000 + 3600;
const RUNS: &[&str] = &["api", "repos/{owner}/{repo}/actions/runs?per_page=50"];
const RUNNERS: &[&str] = &["api", "repos/{owner}/{repo}/actions/runners?per_page=100"];
const JOBS: &[&str] = &["api", "repos/{owner}/{repo}/actions/runs/9001/jobs"];

fn stranded_fixture(age: u64, labels: &str, runners: &str) -> FakeGh {
    let gh = FakeGh::default();
    gh.on(RUNS, 0, r#"{"workflow_runs":[{"id":9001,"status":"in_progress"}]}"#);
    let created = crate::time::epoch("2026-09-29T00:00:00Z") + 3600 - age;
    let ts = {
        let d = created - 1_790_640_000;
        format!("2026-09-29T{:02}:{:02}:{:02}Z", d / 3600, d % 3600 / 60, d % 60)
    };
    gh.on(JOBS, 0, &format!(r#"{{"jobs":[{{"name":"suites","status":"queued","created_at":"{ts}","labels":{labels}}}]}}"#));
    gh.on(RUNNERS, 0, runners);
    gh
}

#[test]
fn stranded_unregistered_runner_fires_without_waiting_for_the_clock() {
    let gh = stranded_fixture(360, r#"["self-hosted","spira-run-9001"]"#, r#"{"runners":[]}"#);
    let out = stranded_runners(&gh, repo(), NOW, 300);
    assert_eq!(out.lines, vec![r#"STRANDED run=9001 job="suites" label=spira-run-9001 queued=360s runner=none vmid=?"#]);
}

#[test]
fn stranded_offline_runner_names_runner_and_vmid() {
    let gh = stranded_fixture(
        360,
        r#"["self-hosted","spira-run-9001"]"#,
        r#"{"runners":[{"name":"eph-133742","status":"offline","labels":[{"name":"self-hosted"},{"name":"spira-run-9001"}]}]}"#,
    );
    let out = stranded_runners(&gh, repo(), NOW, 300);
    assert_eq!(out.lines.len(), 1);
    assert!(out.lines[0].contains("runner=eph-133742 vmid=133742"), "{:?}", out.lines);
}

#[test]
fn stranded_is_silent_for_online_young_or_hosted_jobs() {
    let online = r#"{"runners":[{"name":"eph-1337","status":"online","labels":[{"name":"spira-run-9001"}]}]}"#;
    let g = stranded_fixture(360, r#"["self-hosted","spira-run-9001"]"#, online);
    assert!(stranded_runners(&g, repo(), NOW, 300).lines.is_empty());
    let g = stranded_fixture(30, r#"["self-hosted","spira-run-9001"]"#, r#"{"runners":[]}"#);
    assert!(stranded_runners(&g, repo(), NOW, 300).lines.is_empty());
    let g = stranded_fixture(360, r#"["ubuntu-latest"]"#, r#"{"runners":[]}"#);
    assert!(stranded_runners(&g, repo(), NOW, 300).lines.is_empty());
}

#[test]
fn stranded_unreadable_runs_is_silent_not_a_guess() {
    assert!(stranded_runners(&FakeGh::default(), repo(), NOW, 300).lines.is_empty());
}

// ── per-repo gate workflow ───────────────────────────────────────────────────────────────

#[test]
fn check_status_reads_the_named_workflow_so_a_ci_repo_goes_green() {
    let gh = FakeGh::default();
    gh.on(
        &["run", "list", "--branch", "b", "--workflow", "CI", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"],
        0,
        r#"[{"databaseId":7,"status":"completed","conclusion":"success"}]"#,
    );
    gh.on(&["api", "repos/{owner}/{repo}/actions/runs/7/jobs"], 0, r#"{"jobs":[]}"#);
    assert_eq!(check_status(&gh, &NoProc, repo(), "CI", "b").lines[0], "green");
    assert_eq!(check_status(&gh, &NoProc, repo(), "Gate", "b").lines[0], "pending");
}

#[test]
fn every_run_list_verb_filters_on_the_workflow_it_is_given() {
    let gh = FakeGh::default();
    runs_for_branch(&gh, repo(), "CI", "b");
    runs_queue_branches(&gh, repo(), "CI");
    batch_ci_status(&gh, &NoProc, repo(), "CI", "b");
    dispatch(&gh, repo(), "CI", "main", "s");
    let calls = gh.calls.borrow();
    assert!(calls.len() >= 4);
    for c in calls.iter() {
        assert!(!c.iter().any(|a| a == "Gate"), "{c:?}");
        assert!(c.iter().any(|a| a == "CI"), "{c:?}");
    }
}

#[test]
fn workflow_for_picks_the_repos_pair_else_the_default() {
    let t = "deck=CI pokedumpster=CI2";
    assert_eq!(workflow_for("Gate", t, Some("deck")), "CI");
    assert_eq!(workflow_for("Gate", t, Some("pokedumpster")), "CI2");
    assert_eq!(workflow_for("Gate", t, Some("deckdumpster")), "Gate");
    assert_eq!(workflow_for("Gate", t, None), "Gate");
    assert_eq!(workflow_for("Gate", "", Some("deck")), "Gate");
    assert_eq!(workflow_for("Gate", "deck=", Some("deck")), "Gate");
}

// ── pr-create: one open PR per installation per repo ──────────────────────────────────

const LIST_MINE: &[&str] = &["pr", "list", "--state", "open", "--author", "@me", "--json", "number,headRefName"];
const CREATE: &[&str] = &["pr", "create", "--head", "spira/queue/b", "--base", "main", "--title", "t", "--body-file", "-"];

fn creatable(open_json: &str) -> FakeGh {
    let gh = FakeGh::default();
    gh.on(LIST_MINE, 0, open_json);
    gh.on(CREATE, 0, "");
    gh.on(&["pr", "view", "spira/queue/b", "--json", "number", "-q", ".number"], 0, "9\n");
    gh
}

fn created(gh: &FakeGh) -> bool {
    gh.calls.borrow().iter().any(|c| c.iter().map(String::as_str).eq(CREATE.iter().copied()))
}

#[test]
fn pr_create_refuses_a_second_open_installation_pr_and_opens_nothing() {
    let gh = creatable(r#"[{"number":22,"headRefName":"spira/queue/a"}]"#);
    let out = pr_create(&gh, repo(), "spira/queue/b", "main", "t", b"", "");
    assert_eq!(out.code, 1);
    assert_eq!(out.lines, vec![String::new()]);
    assert!(!created(&gh));
}

#[test]
fn pr_create_with_the_override_opens_the_second_pr() {
    let gh = creatable(r#"[{"number":22,"headRefName":"spira/queue/a"}]"#);
    let out = pr_create(&gh, repo(), "spira/queue/b", "main", "t", b"", "expunge fix");
    assert_eq!((out.code, out.lines), (0, vec!["9".to_string()]));
    assert!(created(&gh));
}

#[test]
fn pr_create_opens_when_the_only_open_prs_are_another_authors() {
    let gh = creatable("[]");
    assert_eq!(pr_create(&gh, repo(), "spira/queue/b", "main", "t", b"", "").lines, vec!["9".to_string()]);
    assert!(created(&gh));
}

#[test]
fn pr_create_refuses_when_the_listing_cannot_be_read() {
    let gh = FakeGh::default();
    gh.on(CREATE, 0, "");
    assert_eq!(pr_create(&gh, repo(), "spira/queue/b", "main", "t", b"", "").code, 1);
    assert!(!created(&gh));
}
