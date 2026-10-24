//! `queue verdict` (DESIGN-verdict.md §2): one test per row of §2.1 and §2.2, in the
//! crate's world of fakes.

use super::*;
use crate::ops::verdict::{
    classify_fault_rerun, classify_pending, infra_red, normalize_status, parse_declared_timeout,
    parse_run_metadata, Pending,
};

fn has_call(v: &RefCell<Vec<String>>, prefix: &str) -> bool {
    v.borrow().iter().any(|c| c.starts_with(prefix))
}

fn count(v: &RefCell<Vec<String>>, prefix: &str) -> usize {
    v.borrow().iter().filter(|c| c.starts_with(prefix)).count()
}

// ------------------------------------------------------------------------ pure parts

#[test]
fn classifiers_match_the_table() {
    assert_eq!(
        classify_pending(None, None, 3600, 600),
        Pending::WaitUnknown
    );
    assert_eq!(
        classify_pending(Some(10), None, 3600, 600),
        Pending::WaitRunning
    );
    assert_eq!(
        classify_pending(Some(3600), Some(599), 3600, 600),
        Pending::WaitProgressing
    );
    assert_eq!(
        classify_pending(Some(3600), Some(600), 3600, 600),
        Pending::Cancel
    );
    assert_eq!(
        classify_pending(Some(9999), None, 3600, 600),
        Pending::Cancel
    );
    assert!(classify_fault_rerun(0, 2) && classify_fault_rerun(1, 2));
    assert!(!classify_fault_rerun(2, 2) && !classify_fault_rerun(0, 0));
    assert_eq!(normalize_status("provision_fault"), "harness_fault");
    assert_eq!(normalize_status("green"), "green");
    assert_eq!(
        parse_run_metadata(
            "started-at: 100\nlast-activity: 150\nlast-activity: 140\nlast-activity: x\n"
        ),
        (Some(100), Some(150))
    );
    assert_eq!(parse_run_metadata(""), (None, None));
    assert_eq!(
        parse_declared_timeout("timeout-sec: 600\ntimeout-sec: 5400\ntimeout-sec: x\n"),
        Some(5400)
    );
    assert_eq!(parse_declared_timeout(""), None);
}

#[test]
fn cli_takes_exactly_one_repository() {
    let p = |a: &[&str]| cli::parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(
        p(&["verdict", "spira"]),
        Ok(Cmd::Verdict {
            repo: "spira".into()
        })
    );
    assert_eq!(
        p(&["verdict"]),
        Err(cli::Usage("queue.sh verdict: repo required".into()))
    );
    assert!(p(&["verdict", "a", "b"]).is_err());
    assert!(p(&["verdict", "--all"]).is_err());
    assert!(cli::USAGE.contains("queue.sh verdict <repo>"));
}

// ------------------------------------------------------------------ queue.local publish

fn publish_record(t: &T) {
    fs::write(
        t.qfile("publish"),
        "pr=77\nhead=m3\nbase=f0\nmembers=sp-a:t1 sp-b:t2\nopened=900\nbranch=spira/publish/X\nremote=origin\nforge_branch=main\n",
    )
    .unwrap();
}

#[test]
fn publish_with_no_record_does_nothing() {
    let t = T::new(LandMode::QueueLocal);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.forge.calls.borrow().is_empty());
    assert!(t.out().is_empty() && t.err().is_empty());
}

#[test]
fn publish_record_missing_a_field_is_left_for_a_hand_look() {
    let t = T::new(LandMode::QueueLocal);
    fs::write(
        t.qfile("publish"),
        "pr=77\nhead=m3\nbranch=b\nremote=origin\n",
    )
    .unwrap();
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(
        t.err().contains(
            "verdict spira: publish record is missing a field — leaving it for a hand look:"
        ),
        "{}",
        t.err()
    );
    assert!(t.qfile("publish").exists());
    assert!(t.forge.calls.borrow().is_empty());
}

#[test]
fn publish_on_a_pr_that_is_not_open_never_moves_the_forge_target() {
    for state in ["closed", "merged"] {
        let t = T::new(LandMode::QueueLocal);
        publish_record(&t);
        *t.forge.pr_state.borrow_mut() = Some(state.into());
        t.forge.status.borrow_mut().push(Some("green\n".into()));
        assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
        assert!(!t.lib.has("push origin"), "{state}");
        assert!(!has_call(&t.forge.calls, "check-status"), "{state}");
        assert!(!t.qfile("publish").exists());
        assert!(t.landing_log().contains(&format!(
            "QUEUE PUBLISH_ABANDONED 1000 repo=spira pr=77 state={state}"
        )));
        assert!(!t.landing_log().contains("PUBLISH_GREEN"));
    }
}

#[test]
fn publish_with_unreadable_pr_state_waits_and_keeps_the_record() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    *t.forge.pr_state.borrow_mut() = Some("unknown".into());
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.out().contains("cannot read PR state (unknown) — waiting"),
        "{}",
        t.out()
    );
    assert!(t.qfile("publish").exists());
    assert!(!t.lib.has("push origin"));
    assert!(!has_call(&t.forge.calls, "check-status"));
}

#[test]
fn publish_green_that_would_not_fast_forward_keeps_the_record_and_says_so() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    t.lib.push_ok.set(false);
    t.forge.status.borrow_mut().push(Some("green\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains(
        "publish PR 77 green but origin/main would not fast-forward — something moved it"
    ));
    assert!(t
        .lib
        .has("notify spira publish PR 77 green but fast-forward refused"));
    assert!(t.qfile("publish").exists());
    assert!(!has_call(&t.forge.calls, "pr-close"));
}

#[test]
fn publish_pending_or_faulted_waits_and_changes_nothing() {
    for (answer, word) in [
        (Some("pending\n"), "pending"),
        (Some("harness_fault\n"), "harness_fault"),
        (Some("provision_fault\n"), "harness_fault"),
        (None, ""),
    ] {
        let t = T::new(LandMode::QueueLocal);
        publish_record(&t);
        t.forge.status.borrow_mut().push(answer.map(String::from));
        assert_eq!(t.run(&["verdict", "spira"]), 0);
        assert!(
            t.out()
                .contains(&format!("verdict spira: publish PR 77: {word} — waiting")),
            "{}",
            t.out()
        );
        assert!(t.qfile("publish").exists());
        assert!(!t.lib.has("push") && !has_call(&t.forge.calls, "pr-close"));
    }
}

#[test]
fn publish_red_files_one_fix_forward_and_marks_the_head() {
    // Local attribution (attribute.sh) is retired, sp-uwhx0 — the fix-forward bead now
    // names red suites and members only, no local reproduction.
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    t.forge.status.borrow_mut().push(Some(
        "red\nred-suite: test-b.sh\nred-suite: test-a.sh test-b.sh\nrun-url: http://r/9\n".into(),
    ));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    // one check-status call: the red half reads the answer the locked half judged
    assert_eq!(count(&t.forge.calls, "check-status"), 1);
    assert!(has_call(&t.forge.calls, "pr-close 77"));
    let bug = t
        .lib
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("create_bug"))
        .cloned()
        .unwrap();
    assert!(bug.starts_with("create_bug queue.sh 1 spira,plan,repo:spira publish PR 77 red for spira: test-b.sh,test-a.sh\n"), "{bug}");
    assert!(bug.contains("Publish PR 77 red for spira (http://r/9).\n\nPublished range: f0..m3\nRed suites: test-b.sh,test-a.sh\n\nMembers in this publish: sp-a,sp-b\n\n"));
    assert!(bug.ends_with("Fix forward on local/main — the next publish carries the fix. Production was never rolled back and no member bead was reopened."));
    assert!(!t.qfile("publish").exists());
    assert_eq!(
        fs::read_to_string(t.qfile("publish-red")).unwrap(),
        "head=m3\nfix_forward=sp-fix1\nsuites=test-b.sh,test-a.sh\n"
    );
    assert!(t.landing_log().contains(
        "QUEUE PUBLISH_RED 1000 repo=spira pr=77 suites=test-b.sh,test-a.sh fix_forward=sp-fix1"
    ));
    assert!(t.lib.has("notify spira publish PR 77 red"));
    assert!(t
        .out()
        .contains("verdict spira: publish PR 77 red — filed fix-forward sp-fix1"));
    // never a reopen, never judgement
    assert!(!t.lib.has("bead_reopen") && !t.lc.has("abandon-batch"));
    assert!(!has_call(&t.scripts.calls, "judgement-ci"));
}

#[test]
fn publish_red_naming_no_suite_still_files_one_bead_and_frees_the_lock() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    t.var("SPIRA_QUEUE_ACTOR", "concierge");
    t.forge.status.borrow_mut().push(Some("red\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    let bug = t
        .lib
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("create_bug"))
        .cloned()
        .unwrap();
    assert!(
        bug.starts_with(
            "create_bug concierge 1 spira,plan,repo:spira publish PR 77 red for spira\n"
        ),
        "{bug}"
    );
    assert!(bug.contains("Red suites: <none named>"));
    assert!(matches!(
        crate::lock::try_lock(&t.s().queue_dir, "spira"),
        crate::lock::Acquire::Held(_)
    ));
}

#[test]
fn publish_red_with_no_bead_filed_is_a_fault_but_still_marks_the_head() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    *t.lib.bug_id.borrow_mut() = None;
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert_eq!(
        fs::read_to_string(t.qfile("publish-red")).unwrap(),
        "head=m3\nfix_forward=<create-failed>\nsuites=test-a.sh\n"
    );
    assert!(t.out().contains("filed fix-forward <create-failed>"));
}

#[test]
fn publish_settle_skips_its_turn_when_the_queue_lock_is_held() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    let _g = t.hold_lock();
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t
        .out()
        .contains("verdict spira: another queue operation holds the lock"));
    assert!(t.forge.calls.borrow().is_empty());
}

#[test]
fn step_on_queue_local_settles_a_green_publish_before_publishing_again() {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/remotes/origin/main", "m3");
    t.git.set("refs/heads/local/main", "m3");
    publish_record(&t);
    t.forge.status.borrow_mut().push(Some("green\n".into()));
    assert_eq!(t.run(&["step", "spira"]), 0);
    let out = t.out();
    let settled = out.find("publish PR 77 green").expect("settled");
    let published = out
        .find("nothing to publish for spira")
        .expect("publish ran after");
    assert!(settled < published, "{out}");
}

// ------------------------------------------------------------------ queue.forge batch

fn batch(t: &T) {
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta sp-b:tb\nopened=500\nbranch=spira/queue/x\nowner=batcher\n");
    t.git.set("origin/main", "b0");
}

/// `batch`, with the lifecycle batch row spira-lc cut for it.
fn lc_batch(t: &T) {
    batch(t);
    let rec = record(t);
    t.open_record(&format!("{rec}batch_id=spira-1\nversion=3\n"));
}

fn record(t: &T) -> String {
    fs::read_to_string(t.qfile("open")).unwrap_or_default()
}

#[test]
fn a_malformed_batch_record_is_refused() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\n");
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("verdict spira: malformed batch record"));
    assert!(t.forge.calls.borrow().is_empty());
}

#[test]
fn a_concierge_claim_refuses_the_whole_pass() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta\nowner=concierge\n");
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.err()
            .contains("verdict spira: refused — this batch is claimed by concierge"),
        "{}",
        t.err()
    );
    assert!(t.forge.calls.borrow().is_empty());
    // the concierge itself, or the override, may settle it
    t.var("SPIRA_QUEUE_ACTOR", "concierge");
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(has_call(&t.forge.calls, "check-status 12"));
}

#[test]
fn pending_waits_while_the_run_is_young_or_progressing_and_cancels_a_stuck_one() {
    // no run yet: age from the record's opened stamp
    let t = T::new(LandMode::Queue);
    batch(&t);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.out()
            .contains("verdict spira: PR 12 pending (run age 500s)"),
        "{}",
        t.out()
    );
    // a run whose start cannot be read: never cancelled
    let t = T::new(LandMode::Queue);
    batch(&t);
    *t.forge.run.borrow_mut() = Some("77".into());
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.out()
            .contains("verdict spira: PR 12 pending (run 77 age unknown)"),
        "{}",
        t.out()
    );
    // old but active
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.verdict.ci_maxsec = 100;
    *t.forge.run.borrow_mut() = Some("77".into());
    *t.forge.meta.borrow_mut() = "started-at: 800\nlast-activity: 950\n".into();
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.out()
            .contains("verdict spira: PR 12 run 77 progressing (last activity 50s ago)"),
        "{}",
        t.out()
    );
    assert!(!has_call(&t.forge.calls, "run-cancel"));
    // past the global cap but inside the run's declared timeout: never cancelled, even idle
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.verdict.ci_maxsec = 100;
    t.lib.s.verdict.ci_idle_sec = 30;
    *t.forge.run.borrow_mut() = Some("77".into());
    *t.forge.meta.borrow_mut() = "started-at: 800\nlast-activity: 950\ntimeout-sec: 5400\n".into();
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(!has_call(&t.forge.calls, "run-cancel"));
    assert!(t.out().contains("pending (run age 200s)"), "{}", t.out());
    // past its declared timeout and idle: cancelled
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.verdict.ci_maxsec = 100;
    t.lib.s.verdict.ci_idle_sec = 30;
    *t.forge.run.borrow_mut() = Some("77".into());
    *t.forge.meta.borrow_mut() = "started-at: 800\nlast-activity: 950\ntimeout-sec: 150\n".into();
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(has_call(&t.forge.calls, "run-cancel 77"));
    // old and idle: cancelled, the record untouched for the next pass
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.verdict.ci_maxsec = 100;
    t.lib.s.verdict.ci_idle_sec = 30;
    *t.forge.run.borrow_mut() = Some("77".into());
    *t.forge.meta.borrow_mut() = "started-at: 800\nlast-activity: 950\n".into();
    let before = record(&t);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(has_call(&t.forge.calls, "run-cancel 77"));
    assert!(
        t.out().contains(
            "verdict spira: PR 12 run stuck (200s, idle 50s) — cancelled; will retry on next pass"
        ),
        "{}",
        t.out()
    );
    assert_eq!(record(&t), before);
}

#[test]
fn a_failed_status_call_is_pending_not_a_verdict() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.forge.status.borrow_mut().push(None);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("PR 12 pending"), "{}", t.out());
    assert!(!t.lc.has("abandon-batch"));
}

#[test]
fn harness_fault_reruns_within_the_budget_then_closes_and_returns_members() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.run.borrow_mut() = Some("77".into());
    t.forge.status.borrow_mut().extend([
        Some("harness_fault\n".into()),
        Some("provision_fault\n".into()),
        Some("harness_fault\n".into()),
    ]);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t
        .out()
        .contains("verdict spira: PR 12 harness fault — re-running (attempt 1/2)"));
    assert!(record(&t).ends_with("retries=1\n"));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("re-running (attempt 2/2)"));
    assert!(record(&t).ends_with("retries=2\n") && !record(&t).contains("retries=1"));
    assert_eq!(count(&t.forge.calls, "workflow-rerun 77"), 2);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t
        .out()
        .contains("verdict spira: PR 12 harness fault — retries exhausted; closing batch"));
    assert!(has_call(&t.forge.calls, "pr-close 12"));
    assert!(t.lc.has("abandon-batch spira-1 CI_RUNNING 4 batch closed"));
    assert!(!t.qfile("open").exists());
    let mail = t
        .scripts
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("mail-operator"))
        .cloned()
        .unwrap();
    assert!(mail.starts_with("mail-operator Merge queue: spira CI fault after 3 attempts\n## Note\nMerge queue batch for spira closed after 3 failed CI run attempts.\n\nPR 12 (head h1) has been closed."), "{mail}");
}

#[test]
fn a_capacity_fault_closes_once_backs_off_and_blocks_the_recut_until_it_lapses() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.forge.status.borrow_mut().push(Some(
        "provision_fault\ncapacity: VcpuLimitExceeded\n".into(),
    ));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.out()
            .contains("provision failed on capacity (VcpuLimitExceeded)"),
        "{}",
        t.out()
    );
    assert!(has_call(&t.forge.calls, "pr-close 12") && !has_call(&t.forge.calls, "workflow-rerun"));
    assert!(!t.qfile("open").exists());
    let backoff = fs::read_to_string(t.qfile("capacity-backoff")).unwrap();
    assert!(
        backoff.contains("failures=1\n") && backoff.contains("reason=VcpuLimitExceeded"),
        "{backoff}"
    );
    assert_eq!(
        count(
            &t.scripts.calls,
            "mail-operator Merge queue: spira waiting on capacity"
        ),
        1
    );
    let until: u64 = backoff
        .lines()
        .find_map(|l| l.strip_prefix("until="))
        .unwrap()
        .parse()
        .unwrap();
    t.git.set("origin/main", "b0");
    assert_eq!(t.run(&["open-batch", "--skip-pregate"]), 1);
    assert!(t.out().contains("waiting on capacity"), "{}", t.out());
    assert!(!t.lc.has("cut "));
    fs::write(
        t.qfile("capacity-backoff"),
        format!("failures=1\nuntil={}\nreason=x\n", until.min(1)),
    )
    .unwrap();
    t.run(&["open-batch", "--skip-pregate"]);
    assert!(
        t.out().matches("waiting on capacity").count() == 1,
        "lapsed backoff must not block: {}",
        t.out()
    );
}

#[test]
fn green_on_another_head_or_no_head_is_refused_as_a_fault() {
    for (answer, line, subject) in [
        ("green\nhead-sha: hX\n", "verdict spira: PR 12 CI head mismatch (ci=hX sealed=h1) — harness fault; PR closed, members requeued", "Merge queue: spira CI head mismatch"),
        ("green\n", "verdict spira: PR 12 green but head-sha missing — cannot verify sealed head; harness fault; PR closed, members requeued", "Merge queue: spira CI head unverifiable (missing head-sha)"),
    ] {
        let t = T::new(LandMode::Queue);
        lc_batch(&t);
        t.forge.status.borrow_mut().push(Some(answer.into()));
        assert_eq!(t.run(&["verdict", "spira"]), 0);
        assert!(t.out().contains(line), "{}", t.out());
        assert!(has_call(&t.scripts.calls, &format!("mail-operator {subject}\n")));
        assert!(t.lc.has("abandon-batch spira-1"));
        assert!(!t.lib.has("push") && !t.lc.has("land "));
        assert!(!t.qfile("open").exists());
    }
}

#[test]
fn a_refused_fast_forward_keeps_the_batch() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.push_ok.set(false);
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t
        .err()
        .contains("verdict spira: PR 12 fast-forward push failed"));
    assert!(t.qfile("open").exists());
    assert!(!t.lc.has("abandon-batch"));
}

#[test]
fn green_with_lifecycle_on_walks_the_batch_to_landed_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta\nbranch=\nbatch_id=B1\nversion=3\n");
    t.git.set("origin/main", "b0");
    t.lc.batch_states
        .borrow_mut()
        .insert("B1".into(), ("OPEN".into(), "3".into()));
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let lc = t.lc.calls.borrow().clone();
    assert!(
        lc.contains(&"event batch B1 OPEN 3 {\"CiStarted\":{\"run\":\"12\"}}".to_string()),
        "{lc:?}"
    );
    assert!(
        lc.contains(&"event batch B1 CI_RUNNING 4 \"Green\"".to_string()),
        "{lc:?}"
    );
    assert!(lc.contains(&"land B1 5 h1".to_string()), "{lc:?}");
    assert!(t.out().contains("verdict: B1 landed on spira-lc (sha=h1)"));
}

#[test]
fn green_on_a_batch_a_red_already_started_records_green_with_no_gap_and_no_second_ci_started() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta\nbranch=\nbatch_id=B1\nversion=3\n");
    t.git.set("origin/main", "b0");
    t.lc.batch_states
        .borrow_mut()
        .insert("B1".into(), ("CI_RUNNING".into(), "4".into()));
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let lc = t.lc.calls.borrow().clone();
    assert!(!lc.iter().any(|c| c.contains("CiStarted")), "{lc:?}");
    assert!(
        lc.contains(&"event batch B1 CI_RUNNING 4 \"Green\"".to_string()),
        "{lc:?}"
    );
    assert!(lc.contains(&"land B1 5 h1".to_string()), "{lc:?}");
}

#[test]
fn a_moved_base_rebuilds_and_force_pushes_when_no_member_is_in_it() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.git.set("origin/main", "b1");
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let git = t.git.calls.borrow().clone();
    assert!(
        git.contains(&"merge ta spira: land sp-a".to_string())
            && git.contains(&"merge tb spira: land sp-b".to_string()),
        "{git:?}"
    );
    assert!(t.lib.has("push origin +merged-tb:refs/heads/spira/queue/x"));
    let rec = record(&t);
    assert!(
        rec.ends_with("head=merged-tb\nretries=0\nmembers=sp-a:ta sp-b:tb\nbase=b1\n"),
        "{rec}"
    );
    assert!(t
        .out()
        .contains("verdict spira: PR 12 rebuilt on moved base (b1) — re-pushed (head merged-tb)"));
    assert!(!has_call(&t.forge.calls, "pr-close"));
    assert!(!t.lc.has("abandon-batch"));
}

#[test]
fn a_moved_base_already_carrying_a_member_closes_and_lands_that_member() {
    let t = T::new(LandMode::Queue);
    t.submitted(&["sp-a"]);
    lc_batch(&t);
    t.git.set("origin/main", "b1");
    t.git.ancestor("ta", "b1");
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(!t.git.calls.borrow().iter().any(|c| c.starts_with("merge")));
    assert!(has_call(&t.forge.calls, "pr-close 12"));
    assert!(t
        .out()
        .contains("verdict spira: PR 12 base moved (b1) — closed, members requeued"));
    assert!(
        t.lc.has("abandon-batch spira-1 CI_RUNNING 4 base moved")
            && t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"")
            && t.lib.has("bead_close sp-a b1")
    );
    assert!(t
        .out()
        .contains("verdict spira: sp-a already in moved base — LANDED"));
    assert!(!t.lc.has("event bead sp-b"));
    assert!(!t.qfile("open").exists());
}

#[test]
fn a_moved_base_that_conflicts_closes_and_requeues() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.git.set("origin/main", "b1");
    t.git.merge_fail.borrow_mut().insert("tb".into());
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t
        .out()
        .contains("verdict spira: PR 12 base moved — conflict in sp-b; closing and requeuing"));
    assert!(!t.lib.has("push"));
    assert!(t.lc.has("abandon-batch spira-1 CI_RUNNING 4 base moved"));
    assert!(!t.qfile("open").exists());
}

#[test]
fn red_summons_judgement_once_with_the_suites_and_the_run() {
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut {
        rc: 0,
        out: "filed\nid=sp-j1\n".into(),
        err: String::new(),
    };
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-b.sh\nred-suite: test-a.sh\nred-suite: test-b.sh\nrun-url: http://r/5\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let j = t
        .scripts
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("judgement-ci"))
        .cloned()
        .unwrap();
    assert!(
        j.starts_with(
            "judgement-ci batcher spira test-b.sh,test-a.sh sp-a,sp-b PR 12 — http://r/5 home="
        ),
        "{j}"
    );
    assert!(record(&t).ends_with("judgement=sp-j1\n"));
    assert!(t.out().contains("verdict spira: PR 12 red (test-b.sh,test-a.sh) — batcher-owned, summoned judgement (sp-j1)"));
    // the PR and its members are left exactly as they are
    assert!(
        !has_call(&t.forge.calls, "pr-close")
            && !t.lc.has("abandon-batch")
            && !t.lib.has("bead_reopen")
    );
    // the next pass does not summon twice
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert_eq!(count(&t.scripts.calls, "judgement-ci"), 1);
    assert!(t
        .out()
        .contains("verdict spira: PR 12 red — batcher-owned, judgement already summoned (sp-j1)"));
}

#[test]
fn a_red_batch_bars_every_member_tip_until_an_abandon_or_an_eject_attributes_it() {
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut {
        rc: 0,
        out: "filed\nid=sp-j1\n".into(),
        err: String::new(),
    };
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert_eq!(
        fs::read_to_string(t.qfile("red-tips")).unwrap(),
        "sp-a ta\nsp-b tb\n"
    );
    assert_eq!(t.run(&["abandon", "--reason", "flake"]), 0, "{}", t.err());
    assert_eq!(fs::read_to_string(t.qfile("red-tips")).unwrap(), "");
}

#[test]
fn a_red_forge_batch_summons_a_mender_per_red_suite_once() {
    let mut t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut {
        rc: 0,
        out: "id=sp-j1\n".into(),
        err: String::new(),
    };
    t.forge.status.borrow_mut().push(Some(
        "red\nred-suite: test-b.sh\nred-suite: test-a.sh\n".into(),
    ));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let events = |t: &T| {
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.contains("\"Failed\""))
            .count()
    };
    assert_eq!(events(&t), 2);
    assert!(t.lib.has("create_bug"), "{}", t.out());
    assert_eq!(
        t.lib
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("create_bug") && c.contains("mender-failure"))
            .count(),
        2
    );
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert_eq!(events(&t), 2);
}

fn red_forge_creates(t: &T) -> Vec<String> {
    t.lib
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("create_bug"))
        .cloned()
        .collect()
}

fn red_forge_run(t: &mut T) {
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut {
        rc: 0,
        out: "id=sp-j1\n".into(),
        err: String::new(),
    };
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: gate-deploy-ci-sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
}

#[test]
fn a_one_member_red_forge_batch_files_no_mend() {
    let mut t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta\nopened=500\nbranch=spira/queue/x\nowner=batcher\nbatch_id=spira-1\nversion=3\n");
    t.git.set("origin/main", "b0");
    red_forge_run(&mut t);
    assert!(
        red_forge_creates(&t).is_empty(),
        "{:?}",
        red_forge_creates(&t)
    );
}

#[test]
fn a_red_forge_batch_with_a_member_in_rework_files_no_mend() {
    let mut t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("REWORK".into(), "3".into()));
    red_forge_run(&mut t);
    assert!(
        red_forge_creates(&t).is_empty(),
        "{:?}",
        red_forge_creates(&t)
    );
}

#[test]
fn a_multi_member_red_forge_batch_files_one_mend_carrying_the_repo_label() {
    let mut t = T::new(LandMode::Queue);
    lc_batch(&t);
    red_forge_run(&mut t);
    let made = red_forge_creates(&t);
    assert_eq!(made.len(), 1, "{made:?}");
    assert!(made[0].contains(",repo:spira "), "{made:?}");
}

#[test]
fn a_red_forge_batch_starts_its_pass_before_the_failed_event_because_its_row_is_open() {
    let mut t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 3, 0, String::new()));
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut {
        rc: 0,
        out: "id=sp-j1\n".into(),
        err: String::new(),
    };
    t.forge.status.borrow_mut().push(Some(
        "red\nred-suite: test-a.sh\nred-suite: test-b.sh\n".into(),
    ));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let calls = t.lc.calls.borrow().clone();
    let at = |needle: &str| {
        calls
            .iter()
            .position(|c| c.starts_with("event batch") && c.contains(needle))
    };
    assert!(
        at("\"PassStarted\"").is_some() && at("\"PassStarted\"") < at("\"Failed\""),
        "{calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.contains("\"PassStarted\""))
            .count(),
        1,
        "the second red finds the pass already running"
    );
    assert!(!t.err().contains("no mender for"), "{}", t.err());
}

#[test]
fn a_hand_cut_batch_red_goes_to_judgement_too() {
    // D1: verdict's own split/eject attribution is retired; owner is not consulted.
    let mut t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta\nowner=operator\n");
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut {
        rc: 0,
        out: "id=sp-j2\n".into(),
        err: String::new(),
    };
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(has_call(
        &t.scripts.calls,
        "judgement-ci batcher spira test-a.sh sp-a PR 12 home="
    ));
    assert!(
        t.out()
            .contains("verdict spira: PR 12 red (test-a.sh) — summoned judgement (sp-j2)"),
        "{}",
        t.out()
    );
}

#[test]
fn a_setup_red_is_rerun_once_then_abandoned_with_evidence_never_an_endless_defer() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.run.borrow_mut() = Some("77".into());
    let red = "red\nfailed-step: gate / Set up runner\nstep-log: no space left\n";
    let tick = |t: &T| {
        t.forge.status.borrow_mut().push(Some(red.into()));
        assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    };
    let count = |t: &T, what: &str| {
        t.forge
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with(what))
            .count()
    };
    let mails = |t: &T| {
        t.scripts
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("mail-operator"))
            .count()
    };
    tick(&t);
    assert_eq!(count(&t, "workflow-rerun 77"), 1);
    assert_eq!(mails(&t), 0);
    *t.forge.meta.borrow_mut() = "run-status: in_progress\n".into();
    tick(&t);
    assert_eq!(count(&t, "workflow-rerun 77"), 1, "{}", t.out());
    assert!(
        t.out().contains("setup red, re-run in flight"),
        "{}",
        t.out()
    );
    assert_eq!(mails(&t), 0, "a rerun inside the grace is still in flight");
    t.clock
        .now
        .set(t.clock.now.get() + t.lib.s.verdict.ci_idle_sec + 1);
    tick(&t);
    assert_eq!(mails(&t), 1);
    let calls = t.scripts.calls.borrow().join("\n");
    assert!(
        calls.contains("Failed step: gate / Set up runner") && calls.contains("no space left"),
        "{calls}"
    );
    assert!(
        t.lc.calls.borrow().iter().any(|c| c
            .starts_with("abandon-batch spira-1 CI_RUNNING 4 setup red")
            && c.contains("no space left")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        has_call(&t.forge.calls, "pr-close 12"),
        "{:?}",
        t.forge.calls.borrow()
    );
    assert!(
        !t.qfile("open").exists(),
        "the record goes so the next batch can be cut"
    );
    assert_eq!(count(&t, "workflow-rerun 77"), 1);
}

#[test]
fn a_single_member_batch_red_at_the_same_step_after_the_rerun_bars_the_members_tip() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    let rec = record(&t).replace("members=sp-a:ta sp-b:tb", "members=sp-b:tb");
    t.open_record(&format!("{rec}batch_id=spira-1\nversion=3\n"));
    *t.forge.run.borrow_mut() = Some("77".into());
    let red = "red\nfailed-step: gate / Lint\nstep-log: ##[error]I001 Organize imports\n";
    t.forge.status.borrow_mut().push(Some(red.into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(!t.lc.has("eject-member"), "{:?}", t.lc.calls.borrow());
    t.clock
        .now
        .set(t.clock.now.get() + t.lib.s.verdict.ci_idle_sec + 1);
    t.forge.status.borrow_mut().push(Some(red.into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(
        t.lc.has("eject-member spira-1 sp-b"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(
        fs::read_to_string(t.qfile("red-tips")).unwrap(),
        "sp-b tb\n"
    );
    assert!(!t.qfile("open").exists());
}

#[test]
fn an_image_build_red_with_an_infra_signature_never_bars_the_single_members_tip() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    let rec = record(&t).replace("members=sp-a:ta sp-b:tb", "members=sp-b:tb");
    t.open_record(&format!("{rec}batch_id=spira-1\nversion=3\n"));
    t.git.stat("b0", "tb", &[("deploy/image-lib.sh", 3, 1)]);
    *t.forge.run.borrow_mut() = Some("77".into());
    let red = "red\nfailed-step: gate / Build image\nstep-log: deploy/image-lib.sh:68: sccache: error: Timed out waiting for server startup\n";
    t.forge.status.borrow_mut().push(Some(red.into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(
        t.forge
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("workflow-rerun 77")),
        "the infra red is re-run first: {:?}",
        t.forge.calls.borrow()
    );
    t.clock
        .now
        .set(t.clock.now.get() + t.lib.s.verdict.ci_idle_sec + 1);
    t.forge.status.borrow_mut().push(Some(red.into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(!t.lc.has("eject-member"), "{:?}", t.lc.calls.borrow());
    assert!(
        !t.lc
            .calls
            .borrow()
            .iter()
            .any(|c| c.contains("batch-ejected")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("abandon-batch spira-1")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        !t.qfile("red-tips").exists()
            || fs::read_to_string(t.qfile("red-tips")).unwrap().is_empty()
    );
    assert!(
        !t.qfile("open").exists(),
        "the record goes so the members are re-cut"
    );
}

#[test]
fn infra_red_reads_signatures_and_runner_provisioning_and_nothing_else() {
    assert!(infra_red("red\nfailed-step: build / Build image\nstep-log: sccache: error: Timed out waiting for server startup\n"));
    assert!(infra_red("red\nfailed-step: gate / Set up runner\n"));
    assert!(infra_red(
        "red\nfailed-step: x\nstep-log: dial tcp: TLS handshake timeout\n"
    ));
    assert!(!infra_red(
        "red\nfailed-step: gate / Lint\nstep-log: ##[error]I001 Organize imports\n"
    ));
    assert!(!infra_red(
        "red\nfailed-step: build / Build image\nstep-log: error[E0308]: mismatched types\n"
    ));
}

#[test]
fn a_rerun_that_never_started_is_not_called_in_flight() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.run.borrow_mut() = Some("77".into());
    *t.forge.meta.borrow_mut() = "run-status: completed\n".into();
    let red = "red\nfailed-step: gate / Set up runner\nstep-log: no space left\n";
    for _ in 0..2 {
        t.forge.status.borrow_mut().push(Some(red.into()));
        assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    }
    assert!(!t.out().contains("re-run in flight"), "{}", t.out());
    assert!(
        has_call(&t.forge.calls, "pr-close 12"),
        "{:?}",
        t.forge.calls.borrow()
    );
}

#[test]
fn a_lint_red_naming_a_members_file_ejects_that_member_and_bars_its_tip() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.git.stat("b0", "ta", &[("mtg_collector/other.py", 1, 0)]);
    t.git
        .stat("b0", "tb", &[("mtg_collector/cli/data_cmd.py", 3, 1)]);
    *t.forge.run.borrow_mut() = Some("77".into());
    let red = "red\nfailed-step: gate / Lint\nstep-log: mtg_collector/cli/data_cmd.py:3:1: I001 [*] Import block is un-sorted or un-formatted\nstep-log: Found 1 error.\n";
    t.forge.status.borrow_mut().push(Some(red.into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(
        t.out()
            .contains("sp-b ejected: mtg_collector/cli/data_cmd.py:3:1: I001"),
        "{}",
        t.out()
    );
    assert!(
        t.lc.has("eject-member spira-1 sp-b"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        !t.lc.has("eject-member spira-1 sp-a"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(t.lib.has("cause_event sp-b eject-red"));
    assert!(!t
        .forge
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("workflow-rerun")));
    assert!(has_call(&t.forge.calls, "pr-close 12"));
    assert!(!t.qfile("open").exists());
    assert_eq!(
        fs::read_to_string(t.qfile("red-tips")).unwrap(),
        "sp-b tb\n"
    );
    let calls = t.lib.calls.borrow();
    let bugs: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bug"))
        .collect();
    assert_eq!(bugs.len(), 1, "{bugs:?}");
    assert!(
        bugs[0].contains("mender-failure")
            && bugs[0].contains("gate-lint")
            && bugs[0].contains("sp-b")
            && bugs[0].contains("I001")
            && !bugs[0].contains("sp-a"),
        "{}",
        bugs[0]
    );
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.contains("Failed") && c.contains("gate-lint")),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test]
fn ejecting_the_member_a_red_names_retires_the_mender_bead_it_filed() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.lib.bug_id.borrow_mut() = Some("sp-m1".into());
    t.git.stat("b0", "ta", &[("mtg_collector/other.py", 1, 0)]);
    t.git
        .stat("b0", "tb", &[("mtg_collector/cli/data_cmd.py", 3, 1)]);
    *t.forge.run.borrow_mut() = Some("77".into());
    t.forge.status.borrow_mut().push(Some(
        "red\nfailed-step: gate / Lint\nstep-log: mtg_collector/cli/data_cmd.py:3:1: I001\n".into(),
    ));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lib.has("create_bug"), "{}", t.out());
    assert!(t.lib.has("bead_close sp-m1"), "{:?}", t.lib.calls.borrow());
    assert!(t.out().contains("mender bead sp-m1 retired"), "{}", t.out());
}

#[test]
fn a_lint_red_naming_no_members_file_is_still_a_setup_red() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.git.stat("b0", "ta", &[("a.py", 1, 0)]);
    t.git.stat("b0", "tb", &[("b.py", 1, 0)]);
    *t.forge.run.borrow_mut() = Some("77".into());
    t.forge.status.borrow_mut().push(Some(
        "red\nfailed-step: gate / Lint\nstep-log: elsewhere.py:3:1: I001 x\n".into(),
    ));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(!t.lc.has("eject-member"), "{:?}", t.lc.calls.borrow());
    assert!(t.out().contains("no suite annotations"), "{}", t.out());
}

#[test]
fn a_pytest_failure_with_no_annotations_is_a_test_red_not_a_setup_red() {
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut {
        rc: 0,
        out: "id=sp-j3\n".into(),
        err: String::new(),
    };
    *t.forge.run.borrow_mut() = Some("77".into());
    t.forge.status.borrow_mut().push(Some("red\nfailed-step: gate / Run tests\nstep-log: FAILED tests/test_progress_file.py::test_x - AssertionError\nstep-log: ===== 1 failed, 9 passed in 2s =====\nstep-log: CI FAILED during step: unit\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(
        has_call(
            &t.scripts.calls,
            "judgement-ci batcher spira tests/test_progress_file.py "
        ),
        "{:?}",
        t.scripts.calls
    );
    assert!(!t
        .forge
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("workflow-rerun")));
    assert!(
        t.out().contains("summoned judgement (sp-j3)"),
        "{}",
        t.out()
    );
}

#[test]
fn a_provision_failure_log_stays_setup_red_and_an_unparseable_one_still_raises_an_incident() {
    for log in ["step-log: no space left on device\n", ""] {
        let mut t = T::new(LandMode::Queue);
        batch(&t);
        t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
        *t.forge.run.borrow_mut() = Some("77".into());
        t.forge.status.borrow_mut().push(Some(format!(
            "red\nfailed-step: gate / Set up runner\n{log}"
        )));
        assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
        assert!(t.out().contains("no suite annotations"), "{}", t.out());
        assert!(!has_call(&t.scripts.calls, "judgement-ci"));
        t.clock
            .now
            .set(t.clock.now.get() + t.lib.s.verdict.ci_idle_sec + 1);
        t.forge.status.borrow_mut().push(Some(format!(
            "red\nfailed-step: gate / Set up runner\n{log}"
        )));
        assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
        assert!(t
            .scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("mail-operator")));
    }
}

#[test]
fn red_without_suites_or_without_a_batcher_is_not_judged() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.forge.status.borrow_mut().push(Some("red\n".into()));
    *t.forge.run.borrow_mut() = Some("77".into());
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("verdict spira: PR 12 red — batcher-owned, no suite annotations (step: unknown); re-running run 77 once"), "{}", t.out());
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.batcher_off = true;
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains(
        "batcher-owned but the batcher is off (SPIRA_BATCHER_ENABLE=0); cannot summon judgement"
    ));
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut {
        rc: 1,
        out: "batcher: no repo\n".into(),
        err: String::new(),
    };
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    let before = record(&t);
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t
        .err()
        .contains("batcher-owned, judgement-ci failed: batcher: no repo"));
    assert_eq!(
        record(&t).replace("pass_recorded=h1/0/red\npasses=1\n", ""),
        before
    );
}

#[test]
fn an_unknown_status_is_a_fault() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.forge.status.borrow_mut().push(Some("weird\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t
        .err()
        .contains("verdict spira: PR 12 unknown check status: weird"));
    assert!(t.qfile("open").exists());
}

#[test]
fn a_held_lock_is_waited_for_then_counted_toward_starvation() {
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.verdict.lock_wait = 3;
    t.lib.s.verdict.lock_starve_max = 2;
    let g = t.hold_lock();
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.out()
            .ends_with("verdict spira: another queue operation holds the lock\n"),
        "{}",
        t.out()
    );
    assert_eq!(t.clock.now(), 1_003, "waited lock_wait seconds");
    assert_eq!(fs::read_to_string(t.qfile("lock-skips")).unwrap(), "1\n");
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.out().ends_with(
            "verdict spira: queue lock starvation — skipped 2 consecutive ticks waiting for lock\n"
        ),
        "{}",
        t.out()
    );
    // the signal fires once per gridlock, not on every tick past the threshold
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(
        t.out()
            .ends_with("verdict spira: another queue operation holds the lock\n"),
        "{}",
        t.out()
    );
    assert_eq!(fs::read_to_string(t.qfile("lock-skips")).unwrap(), "3\n");
    drop(g);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(!t.qfile("lock-skips").exists());
    assert!(has_call(&t.forge.calls, "check-status 12"));
}

#[test]
fn an_express_takeover_stash_is_named_not_silently_skipped() {
    let t = T::new(LandMode::Queue);
    fs::write(t.qfile("attributing-41"), "pr=41\n").unwrap();
    fs::write(t.qfile("attributing-41.lock"), "").unwrap();
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    let err = t.err();
    assert!(
        err.contains("attributing-41 is an express-takeover stash — its attribution is retired"),
        "{err}"
    );
    assert!(!err.contains("attributing-41.lock"));
}

#[test]
fn other_modes_have_nothing_to_settle() {
    for m in [LandMode::Push, LandMode::Pr, LandMode::Hold] {
        let t = T::new(m);
        batch(&t);
        assert_eq!(t.run(&["verdict", "spira"]), 0);
        assert!(t.forge.calls.borrow().is_empty());
    }
}

#[test]
fn verdict_with_lifecycle_on_and_spira_lc_unreachable_refuses_before_reading_anything() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    *t.lc.rows.borrow_mut() = Err("dolt down".into());
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t
        .err()
        .contains("queue.sh verdict: spira-lc is unreachable (dolt down)"));
    assert!(t.forge.calls.borrow().is_empty());
}

#[test]
fn a_repo_the_map_does_not_carry_is_refused() {
    let t = T::new(LandMode::Queue);
    t.lib.r.borrow_mut().path = None;
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t
        .err()
        .contains("verdict spira: not a registered repository (no checkout path)"));
}

#[test]
fn no_open_batch_never_calls_the_forge() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.forge.calls.borrow().is_empty());
    assert!(t.out().is_empty() && t.err().is_empty());
}

#[test]
fn a_publish_red_bead_carries_the_failing_suite_lines() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    *t.forge.run.borrow_mut() = Some("555".into());
    *t.forge.fails.borrow_mut() =
        "fail-line: test-a.sh: gate: test-a.sh FAILED\nfail-line: test-a.sh: not ok 3 - widget\n"
            .into();
    t.forge.status.borrow_mut().push(Some(
        "red\nred-suite: test-a.sh\nrun-url: http://r/9\n".into(),
    ));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(has_call(&t.forge.calls, "fail-lines 555 test-a.sh"));
    assert!(t.lib.has("create_bug"));
    let bug = t
        .lib
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("create_bug"))
        .cloned()
        .unwrap();
    assert!(bug.contains("Failing lines:\nfail-line: test-a.sh: gate: test-a.sh FAILED\nfail-line: test-a.sh: not ok 3 - widget"), "{bug}");
}

#[test]
fn a_publish_red_from_a_non_suite_step_carries_the_step_and_log_tail_and_files_once_per_run() {
    let t = T::new(LandMode::QueueLocal);
    let red = "red\nrun-id: 38071344778\nfailed-step: stage / Stage the build as a release\nstep-log: release: cannot create /x: Permission denied (os error 13)\nstep-log: ##[error]Process completed with exit code 1.\n";
    publish_record(&t);
    t.forge.status.borrow_mut().push(Some(red.into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let bug = t
        .lib
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("create_bug"))
        .cloned()
        .unwrap();
    assert!(
        bug.contains("Failed steps:\n- stage / Stage the build as a release\n"),
        "{bug}"
    );
    assert!(bug.contains("Log tail of the failed step:\nrelease: cannot create /x: Permission denied (os error 13)\n##[error]Process completed with exit code 1."), "{bug}");
    publish_record(&t);
    t.forge.status.borrow_mut().push(Some(red.into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert_eq!(
        t.lib
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("create_bug"))
            .count(),
        1
    );
    assert!(
        t.out()
            .contains("run 38071344778 already has tracker sp-fix1"),
        "{}",
        t.out()
    );
}

#[test]
fn a_publish_red_bead_carries_the_red_tracker_label_only_when_one_is_configured() {
    for (label, want) in [("tracker-x", true), ("", false)] {
        let mut t = T::new(LandMode::QueueLocal);
        t.lib.s.verdict.red_tracker_label = label.into();
        publish_record(&t);
        t.forge
            .status
            .borrow_mut()
            .push(Some("red\nred-suite: test-a.sh\n".into()));
        assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
        let bug = t
            .lib
            .calls
            .borrow()
            .iter()
            .find(|c| c.starts_with("create_bug"))
            .cloned()
            .unwrap();
        assert_eq!(bug.contains(",tracker-x"), want, "{bug}");
    }
}

#[test]
fn a_second_publish_red_with_the_same_failing_set_amends_the_first_with_its_lines() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    fs::write(
        t.qfile("publish-red"),
        "head=m2\nfix_forward=sp-old\nsuites=test-a.sh\n",
    )
    .unwrap();
    *t.forge.run.borrow_mut() = Some("556".into());
    *t.forge.fails.borrow_mut() = "fail-line: test-a.sh: not ok 3 - widget\n".into();
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lib.has("amend_bug queue.sh sp-old"));
    let note = t
        .lib
        .calls
        .borrow()
        .iter()
        .find(|c| c.starts_with("amend_bug"))
        .cloned()
        .unwrap();
    assert!(note.contains("not ok 3 - widget"), "{note}");
    assert!(!t.lib.has("create_bug"));
}

#[test]
fn a_repeat_publish_red_for_the_same_suites_amends_the_open_bead_rather_than_filing() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    fs::write(
        t.qfile("publish-red"),
        "head=m2\nfix_forward=sp-old\nsuites=test-b.sh,test-a.sh\n",
    )
    .unwrap();
    t.forge.status.borrow_mut().push(Some(
        "red\nred-suite: test-b.sh\nred-suite: test-a.sh test-b.sh\nrun-url: http://r/9\n".into(),
    ));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lib.has("amend_bug queue.sh sp-old"));
    assert!(!t.lib.has("create_bug"));
    assert!(fs::read_to_string(t.qfile("publish-red"))
        .unwrap()
        .starts_with("head=m3\nfix_forward=sp-old\n"));
    assert!(t.out().contains("amended open fix-forward sp-old"));
}

#[test]
fn a_repeat_publish_red_files_anew_when_the_prior_bead_is_closed_or_the_suites_differ() {
    for (prior, ok) in [
        ("sp-old\nsuites=test-b.sh,test-a.sh", false),
        ("sp-old\nsuites=test-z.sh", true),
    ] {
        let t = T::new(LandMode::QueueLocal);
        publish_record(&t);
        t.lib.amend_ok.set(ok);
        fs::write(
            t.qfile("publish-red"),
            format!("head=m2\nfix_forward={prior}\n"),
        )
        .unwrap();
        t.forge.status.borrow_mut().push(Some(
            "red\nred-suite: test-b.sh\nred-suite: test-a.sh test-b.sh\n".into(),
        ));
        assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
        assert!(t.lib.has("create_bug"), "{prior}");
    }
}

#[test]
fn publish_green_fast_forwards_the_forge_and_retires_the_record() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nrun-url: http://r/1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert_eq!(t.forge.calls.borrow()[1], "check-status 77 spira/publish/X");
    assert!(t.lib.has("push origin m3:refs/heads/main"));
    assert!(has_call(&t.forge.calls, "pr-close 77"));
    assert!(!t.qfile("publish").exists());
    assert!(t
        .landing_log()
        .contains("QUEUE PUBLISH_GREEN 1000 repo=spira pr=77 head=m3"));
    assert!(!t.lib.has("notify spira publish PR 77 merged"));
    assert!(t
        .out()
        .contains("verdict spira: publish PR 77 green — origin/main fast-forwarded to m3"));
}

#[test]
fn green_on_an_unmoved_base_fast_forwards_lands_every_member_and_cleans_up() {
    let t = T::new(LandMode::Queue);
    t.submitted(&["sp-a", "sp-b"]);
    batch(&t);
    t.git.set("refs/heads/spira/queue/x", "h1");
    t.git.set("refs/heads/spira/queue/old", "o1");
    t.git.ancestor("spira/queue/old", "origin/main");
    *t.git.branches.borrow_mut() = vec![
        ("spira/queue/old".into(), "o1".into()),
        ("spira/queue/new".into(), "n1".into()),
    ];
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: h1\nflaky: test-f.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lib.has("push origin h1:main"));
    assert!(t
        .out()
        .contains("verdict spira: PR 12 landed by fast-forward (h1)"));
    for id in ["sp-a", "sp-b"] {
        assert!(t.lib.has(&format!("gh_closeout {id} h1")));
        assert!(t.lib.has(&format!("bead_close {id} h1")));
    }
    assert!(!t.lib.has("notify spira PR 12 merged (fast-forward)"));
    assert!(has_call(&t.scripts.calls, "observe-flake test-f.sh h1"));
    assert!(!t.qfile("open").exists());
    assert!(t.lib.has("push origin :refs/heads/spira/queue/x"));
    assert!(t
        .git
        .calls
        .borrow()
        .contains(&"branch -D sanctioned spira/queue/x".to_string()));
    assert!(t
        .out()
        .contains("verdict spira: deleted batch branch spira/queue/x"));
    assert!(t
        .out()
        .contains("verdict spira: reaped stale queue ref spira/queue/old"));
    assert!(!t
        .git
        .calls
        .borrow()
        .iter()
        .any(|c| c.ends_with("spira/queue/new")));
}

#[test]
fn each_pass_leaves_the_forge_state_the_pane_reads() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("open".into());
    *t.forge.run.borrow_mut() = Some("77".into());
    *t.forge.meta.borrow_mut() =
        "started-at: 800\nlast-activity: 950\nrunning-step: unit / pytest\n".into();
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    let state = fs::read_to_string(t.qfile("forge-state")).unwrap();
    for want in [
        "batch_id=spira-1\n",
        "pr=12\n",
        "pr_state=open\n",
        "ci=pending\n",
        "run=77\n",
        "run_started=800\n",
        "last_activity=950\n",
        "step=unit / pytest\n",
    ] {
        assert!(state.contains(want), "{want:?} missing from {state}");
    }
}

// ------------------------------------------------------------------- forge batch reconciler

fn archived(t: &T) -> bool {
    fs::read_dir(t.qfile(""))
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("closed-pr"))
        })
        .unwrap_or(false)
}

#[test]
fn a_merged_pr_lands_the_batch_with_the_merge_commit_and_a_second_pass_is_a_no_op() {
    let t = T::new(LandMode::Queue);
    t.submitted(&["sp-a", "sp-b"]);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    *t.forge.merge_commit.borrow_mut() = Some("m1".into());
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-1".into(), ("GREEN".into(), "5".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lc.has("land spira-1 5 m1"), "{:?}", t.lc.calls.borrow());
    for id in ["sp-a", "sp-b"] {
        assert!(t.lib.has(&format!("bead_close {id} m1")));
    }
    assert!(!t.qfile("open").exists());
    assert!(
        !has_call(&t.forge.calls, "check-status"),
        "a merged PR's CI is not read"
    );
    let writes = |t: &T| {
        (
            t.lc.calls
                .borrow()
                .iter()
                .filter(|c| {
                    c.starts_with("land ")
                        || c.starts_with("abandon-batch")
                        || c.starts_with("event")
                })
                .count(),
            t.lib.calls.borrow().len(),
        )
    };
    let before = writes(&t);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert_eq!(writes(&t), before);
}

#[test]
fn a_merged_pr_on_a_batch_still_open_on_the_lifecycle_walks_it_to_landed() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    *t.forge.merge_commit.borrow_mut() = Some("m1".into());
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-1".into(), ("OPEN".into(), "3".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let lc = t.lc.calls.borrow().clone();
    assert!(
        lc.contains(&"event batch spira-1 OPEN 3 {\"CiStarted\":{\"run\":\"12\"}}".to_string()),
        "{lc:?}"
    );
    assert!(
        lc.contains(&"event batch spira-1 CI_RUNNING 4 \"Green\"".to_string()),
        "{lc:?}"
    );
    assert!(lc.contains(&"land spira-1 5 m1".to_string()), "{lc:?}");
}

#[test]
fn a_merged_pr_whose_batch_the_lifecycle_already_landed_only_retires_the_record() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    *t.forge.merge_commit.borrow_mut() = Some("m1".into());
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-1".into(), ("LANDED".into(), "6".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(!t.lc.has("land "));
    assert!(!t.qfile("open").exists());
}

#[test]
fn a_merged_pr_whose_batch_was_abandoned_lands_its_members_from_the_merge() {
    let t = T::new(LandMode::Queue);
    t.submitted(&["sp-a", "sp-b"]);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    *t.forge.merge_commit.borrow_mut() = Some("m1".into());
    t.git.ancestor("h1", "origin/main");
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-1".into(), ("ABANDONED".into(), "6".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let lc = t.lc.calls.borrow().clone();
    for id in ["sp-a", "sp-b"] {
        let want = format!(
            "event bead {id} CERTIFIED 3 {{\"ContentOnBase\":{{\"proof\":\"ancestry:m1\"}}}}"
        );
        assert!(lc.contains(&want), "{lc:?}");
        assert!(t.lib.has(&format!("bead_close {id} m1")));
    }
    assert!(!t.lc.has("land "));
    assert!(!t.qfile("open").exists());
}

#[test]
fn a_merged_pr_on_an_abandoned_batch_with_a_member_not_on_the_base_keeps_the_record() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    *t.forge.merge_commit.borrow_mut() = Some("m1".into());
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-1".into(), ("ABANDONED".into(), "6".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("members not landed"), "{}", t.err());
    assert!(!t
        .lc
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("event bead")));
    assert!(t.qfile("open").exists());
}

#[test]
fn a_merged_pr_falls_back_to_the_sealed_head_when_it_is_on_the_base() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    t.git.ancestor("h1", "origin/main");
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-1".into(), ("GREEN".into(), "5".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lc.has("land spira-1 5 h1"));
}

#[test]
fn a_land_the_lifecycle_refuses_keeps_the_record_and_alerts_once() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    *t.forge.merge_commit.borrow_mut() = Some("m1".into());
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-1".into(), ("GREEN".into(), "5".into()));
    t.lc.land_refused.set(Some("stale version"));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.qfile("open").exists());
    assert_eq!(
        t.lib
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("notify spira forge batch cannot converge"))
            .count(),
        1
    );
    t.lc.land_refused.set(None);
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(!t.qfile("open").exists());
}

#[test]
fn a_closed_unmerged_pr_abandons_the_batch_and_a_second_pass_is_a_no_op() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("closed".into());
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(
        t.lc.has("abandon-batch spira-1"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(!t.lc.has("land "));
    assert!(
        !has_call(&t.forge.calls, "pr-close"),
        "an already closed PR is not closed again"
    );
    assert!(!t.qfile("open").exists());
    assert!(archived(&t));
    let writes = |t: &T| {
        (
            t.lc.calls
                .borrow()
                .iter()
                .filter(|c| {
                    c.starts_with("land ")
                        || c.starts_with("abandon-batch")
                        || c.starts_with("event")
                })
                .count(),
            t.lib.calls.borrow().len(),
        )
    };
    let before = writes(&t);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert_eq!(writes(&t), before);
}

#[test]
fn a_closed_unmerged_pr_retires_the_mender_beads_filed_for_its_red() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    let rounds = t.s().run.join("rounds");
    fs::create_dir_all(&rounds).unwrap();
    fs::write(rounds.join("spira-1.mender-1-gate-lint"), "sp-m1\n").unwrap();
    fs::write(rounds.join("other-9.mender-1-gate-lint"), "sp-m2\n").unwrap();
    *t.forge.pr_state.borrow_mut() = Some("closed".into());
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_close sp-m1"), "{:?}", t.lib.calls.borrow());
    assert!(!t.lib.has("bead_close sp-m2"), "{:?}", t.lib.calls.borrow());
    assert!(rounds.join("spira-1.mender-1-gate-lint.retired").exists());
}

#[test]
fn a_mender_bead_of_a_slash_named_batch_is_retired_when_its_pr_closes() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    let rec = record(&t);
    t.open_record(&format!("{rec}batch_id=spira/queue/1\nversion=3\n"));
    let rounds = t.s().run.join("rounds");
    fs::create_dir_all(&rounds).unwrap();
    fs::write(
        rounds.join("spira%2Fqueue%2F1.mender-1-gate-lint"),
        "sp-m1\n",
    )
    .unwrap();
    *t.forge.pr_state.borrow_mut() = Some("closed".into());
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_close sp-m1"), "{:?}", t.lib.calls.borrow());
    assert!(rounds
        .join("spira%2Fqueue%2F1.mender-1-gate-lint.retired")
        .exists());
}

#[test]
fn a_batch_with_no_members_is_abandoned_and_its_open_pr_closed() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=\nopened=500\nbranch=spira/queue/x\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lc.has("abandon-batch spira-1"));
    assert!(has_call(&t.forge.calls, "pr-close 12"));
    assert!(t.lib.has("cancel_runs spira/queue/x"));
    assert!(!t.qfile("open").exists());
    assert!(archived(&t));
}

#[test]
fn a_batch_with_no_pr_is_left_young_and_abandoned_past_the_grace() {
    let t = T::new(LandMode::Queue);
    t.open_record("head=h1\nbase=b0\nmembers=sp-a:ta\nopened=500\nbatch_id=spira-1\nversion=3\n");
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("malformed batch record"));
    assert!(!t.lc.has("abandon-batch"));
    t.clock.now.set(500 + t.s().verdict.ci_maxsec + 1);
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lc.has("abandon-batch spira-1"));
    assert!(!t.qfile("open").exists());
}

#[test]
fn a_refused_land_in_the_verdict_keeps_the_record_for_the_reconciler() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: h1\n".into()));
    t.lc.land_refused.set(Some("stale version"));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.qfile("open").exists());
}

#[test]
fn a_queue_pr_that_is_not_the_recorded_batch_is_closed_and_its_runs_cancelled() {
    let t = T::new(LandMode::Queue);
    t.open_record("pr=501\nhead=h1\nbase=b0\nmembers=sp-a:ta\nopened=500\nbranch=spira/queue/B\nbatch_id=spira-1\nversion=3\n");
    *t.forge.queue_prs.borrow_mut() = vec![
        ("500".into(), "spira/queue/A".into()),
        ("501".into(), "spira/queue/B".into()),
    ];
    t.run(&["verdict", "spira"]);
    assert!(
        has_call(&t.forge.calls, "pr-close 500"),
        "{:?}",
        t.forge.calls.borrow()
    );
    assert!(!has_call(&t.forge.calls, "pr-close 501"));
    assert!(t.lib.has("cancel_runs spira/queue/A"));
    assert!(!t.lib.has("cancel_runs spira/queue/B"));
}

#[test]
fn with_no_recorded_batch_every_open_queue_pr_is_stray() {
    let t = T::new(LandMode::Queue);
    *t.forge.queue_prs.borrow_mut() = vec![("500".into(), "spira/queue/A".into())];
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(has_call(&t.forge.calls, "pr-close 500"));
}

#[test]
fn a_record_naming_no_branch_closes_nothing() {
    let t = T::new(LandMode::Queue);
    t.open_record(
        "pr=501\nhead=h1\nbase=b0\nmembers=sp-a:ta\nopened=500\nbatch_id=spira-1\nversion=3\n",
    );
    *t.forge.queue_prs.borrow_mut() = vec![("500".into(), "spira/queue/A".into())];
    t.run(&["verdict", "spira"]);
    assert!(!has_call(&t.forge.calls, "pr-close 500"));
}

fn pass_rows(t: &T) -> Vec<serde_json::Value> {
    let text =
        fs::read_to_string(tsd::family_path(&t.s().run, tsd::pass::FAMILY)).unwrap_or_default();
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn a_judged_forge_run_leaves_one_cert_pass_row_and_a_reread_leaves_no_second() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.lib.push_ok.set(false);
    for _ in 0..2 {
        t.forge
            .status
            .borrow_mut()
            .push(Some("green\nhead-sha: h1\n".into()));
        assert_eq!(t.run(&["verdict", "spira"]), 1);
    }
    let rows = pass_rows(&t);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        (
            rows[0]["repo"].as_str(),
            rows[0]["batch"].as_str(),
            rows[0]["pass"].as_str(),
            rows[0]["cause"].as_str()
        ),
        (Some("spira"), Some("spira-1"), Some("1"), Some("green"))
    );
    assert_eq!(rows[0]["members"], serde_json::json!(["sp-a", "sp-b"]));
}

#[test]
fn a_pending_run_leaves_no_row_and_a_harness_fault_or_mismatched_green_leaves_an_infra_row() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.forge.status.borrow_mut().push(Some("pending".into()));
    t.run(&["verdict", "spira"]);
    assert!(pass_rows(&t).is_empty());

    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    t.forge
        .status
        .borrow_mut()
        .push(Some("green\nhead-sha: hX\n".into()));
    t.run(&["verdict", "spira"]);
    let rows = pass_rows(&t);
    assert_eq!(
        (
            rows.len(),
            rows[0]["cause"].as_str(),
            rows[0]["verdict"].as_str()
        ),
        (1, Some("infra"), Some("incomplete"))
    );
}

#[test]
fn a_red_batch_writes_the_failing_lines_into_every_members_bead_once_per_run() {
    let t = T::new(LandMode::Queue);
    lc_batch(&t);
    *t.forge.run.borrow_mut() = Some("77".into());
    let red = "red\nrun-id: 77\nrun-url: https://example.test/runs/77\nfailed-step: gate / deploy/ci.sh\nstep-log: x\nfail-evidence: FAIL tests/deploy/run.sh:1328: expected 3 units, got 2\nfail-evidence: # fail 1\n";
    let tick = |t: &T| {
        t.forge.status.borrow_mut().push(Some(red.into()));
        t.run(&["verdict", "spira"]);
    };
    tick(&t);
    tick(&t);
    let calls = t.lib.calls.borrow();
    let notes: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("comment ") && c.contains("FAIL tests/deploy/run.sh:1328"))
        .collect();
    assert!(
        notes.iter().any(|c| c.starts_with("comment sp-a "))
            && notes.iter().any(|c| c.starts_with("comment sp-b ")),
        "{calls:?}"
    );
    assert!(
        notes.iter().all(|c| c.contains("run 77")
            && c.contains("Failed step: gate / deploy/ci.sh")
            && c.contains("# fail 1")),
        "{notes:?}"
    );
    assert_eq!(
        notes
            .iter()
            .filter(|c| c.starts_with("comment sp-a "))
            .count(),
        1,
        "one note per run, not per pass: {notes:?}"
    );
}

fn red_publish(t: &T, pr: u32) {
    fs::write(
        t.qfile("publish"),
        format!("pr={pr}\nhead=m3\nbase=f0\nmembers=sp-a:t1 sp-b:t2\nopened=900\nbranch=spira/publish/X\nremote=origin\nforge_branch=main\n"),
    )
    .unwrap();
    t.forge
        .status
        .borrow_mut()
        .push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
}

fn operator_mails(t: &T) -> Vec<String> {
    t.scripts
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("mail-operator"))
        .cloned()
        .collect()
}

#[test]
fn a_red_publish_tracker_is_filed_at_the_publish_red_priority_not_the_incident_one() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.verdict.incident_priority = "3".into();
    t.lib.s.publish_red_priority = "1".into();
    red_publish(&t, 77);
    assert!(
        t.lib.has("create_bug queue.sh 1 "),
        "{:?}",
        t.lib.calls.borrow()
    );
}

#[test]
fn three_consecutive_red_publishes_escalate_once_and_a_fourth_does_not() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.publish_red_escalate_after = 3;
    for pr in [77, 78] {
        red_publish(&t, pr);
        fs::remove_file(t.qfile("publish-red")).unwrap();
    }
    assert!(operator_mails(&t).is_empty());
    red_publish(&t, 79);
    fs::remove_file(t.qfile("publish-red")).unwrap();
    let mails = operator_mails(&t);
    assert_eq!(mails.len(), 1, "{mails:?}");
    assert!(
        mails[0].contains("spira: 3 consecutive publishes red"),
        "{}",
        mails[0]
    );
    assert!(
        mails[0].contains("Publish PRs 77, 78, 79 for spira"),
        "{}",
        mails[0]
    );
    assert!(
        mails[0].contains("Open tracker beads: sp-fix1"),
        "{}",
        mails[0]
    );
    assert!(
        mails[0].contains("Landed beads waiting to publish: "),
        "{}",
        mails[0]
    );
    red_publish(&t, 80);
    assert_eq!(operator_mails(&t).len(), 1);
}

#[test]
fn a_green_publish_resets_the_red_run() {
    let mut t = T::new(LandMode::QueueLocal);
    t.lib.s.publish_red_escalate_after = 2;
    red_publish(&t, 77);
    fs::remove_file(t.qfile("publish-red")).unwrap();
    fs::write(
        t.qfile("publish"),
        "pr=78\nhead=m3\nbase=f0\nmembers=sp-a:t1\nopened=900\nbranch=b\nremote=origin\nforge_branch=main\n",
    )
    .unwrap();
    t.forge.status.borrow_mut().push(Some("green\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(!t.qfile("publish-red-streak").exists());
    red_publish(&t, 79);
    assert!(operator_mails(&t).is_empty());
}

#[test]
fn a_merged_forge_batch_left_in_queue_local_lands_and_clears_its_records() {
    let t = T::new(LandMode::QueueLocal);
    t.submitted(&["sp-a", "sp-b"]);
    lc_batch(&t);
    fs::write(t.qfile("prepared"), "head=h\n").unwrap();
    fs::write(t.qfile("forge-state"), "ci=green\n").unwrap();
    *t.forge.pr_state.borrow_mut() = Some("merged".into());
    *t.forge.merge_commit.borrow_mut() = Some("m1".into());
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-1".into(), ("GREEN".into(), "5".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lc.has("land spira-1 5 m1"), "{:?}", t.lc.calls.borrow());
    for f in ["open", "prepared", "forge-state"] {
        assert!(!t.qfile(f).exists(), "{f} survived the settle");
    }
}

#[test]
fn a_live_forge_batch_left_in_queue_local_is_not_driven_and_keeps_its_records() {
    let t = T::new(LandMode::QueueLocal);
    t.submitted(&["sp-a", "sp-b"]);
    lc_batch(&t);
    fs::write(t.qfile("prepared"), "head=h\n").unwrap();
    *t.forge.pr_state.borrow_mut() = Some("open".into());
    t.run(&["verdict", "spira"]);
    assert!(!has_call(&t.forge.calls, "check-status"));
    assert!(t.qfile("open").exists() && t.qfile("prepared").exists());
}

#[test]
fn mode_switch_refuses_on_an_unsettled_batch() {
    let t = T::new(LandMode::QueueLocal);
    t.submitted(&["sp-a", "sp-b"]);
    lc_batch(&t);
    *t.forge.pr_state.borrow_mut() = Some("open".into());
    assert_eq!(t.run(&["mode-switch", "spira", "queue.forge"]), 1);
    assert!(t.qfile("open").exists());
    assert_eq!(t.run(&["mode-switch", "spira", "queue.local"]), 1);
    assert!(t.err().contains("already queue.local"));
}
