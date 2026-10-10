//! Unit tests: a fake `Gh`/`Proc` that answers canned JSON by argv, exactly the technique
//! `test-forge-*.sh` used with a stub `gh` script — now in-process. Covers every verb's
//! fail-closed answer and the JSON-shaped decisions forge.sh's inline Python made.

use crate::cmds::*;
use crate::ports::{Gh, GhOut, Proc};
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
    let out = check_status(&gh, &NoProc, repo(), "");
    assert_eq!(out.lines, vec!["pending"]);
    assert!(gh.calls.borrow().is_empty(), "must not even ask gh without a branch to scope the run");
}

#[test]
fn check_status_no_run_is_pending() {
    let gh = FakeGh::default();
    gh.on(&["run", "list", "--branch", "spira/sp-x", "--workflow", "Gate", "--json", "databaseId,status,conclusion,headSha,url", "--limit", "1"], 0, "[]");
    let out = check_status(&gh, &NoProc, repo(), "spira/sp-x");
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
    let out = check_status(&gh, &NoProc, repo(), "spira/sp-x");
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
    let out = check_status(&gh, &NoProc, repo(), "b");
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
    let out = check_status(&gh, &NoProc, repo(), "b");
    assert_eq!(out.lines[0], "provision_fault");
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
    let out = check_status(&gh, &NoProc, repo(), "b");
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
    let out = check_status(&gh, &NoProc, repo(), "b");
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
    let out = check_status(&gh, &NoProc, repo(), "b");
    let errs: Vec<&String> = out.lines.iter().filter(|l| l.starts_with("build-error:")).collect();
    assert_eq!(errs.len(), 2, "the repeated E0433 line dedupes to one and the note line is excluded, got {errs:?}");
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

#[test]
fn pr_automerge_arms_squash_and_reports_gh_s_exit() {
    let gh = FakeGh::default();
    gh.on(&["pr", "merge", "--auto", "--squash", "spira/sp-a"], 0, "");
    assert_eq!(pr_automerge(&gh, repo(), "spira/sp-a").code, 0);
    let gh2 = FakeGh::default();
    assert_eq!(pr_automerge(&gh2, repo(), "spira/sp-a").code, 1);
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

// ── batch-ci-status ───────────────────────────────────────────────────────────────────────

#[test]
fn batch_ci_status_no_run_prints_nothing() {
    let gh = FakeGh::default();
    gh.on(&["run", "list", "--branch", "b", "--workflow", "Gate", "--json", "databaseId,conclusion,status,updatedAt,headSha,url", "--limit", "1"], 0, "[]");
    assert!(batch_ci_status(&gh, &NoProc, repo(), "b").lines.is_empty());
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
    let out = batch_ci_status(&gh, &NoProc, repo(), "b");
    assert!(out.lines.contains(&"run-id: 3".to_string()));
    assert!(out.lines.iter().any(|l| l.starts_with("queued-since:")));
}

// ── dispatch requires a ref ──────────────────────────────────────────────────────────────

#[test]
fn dispatch_refuses_without_a_ref() {
    let gh = FakeGh::default();
    let out = dispatch(&gh, repo(), "", "some-suite");
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
