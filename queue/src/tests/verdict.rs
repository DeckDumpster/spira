//! `queue verdict` (DESIGN-verdict.md §2): one test per row of §2.1 and §2.2, in the
//! crate's world of fakes.

use super::*;
use crate::ops::verdict::{classify_fault_rerun, classify_pending, normalize_status, parse_run_metadata, Pending};

fn has_call(v: &RefCell<Vec<String>>, prefix: &str) -> bool {
    v.borrow().iter().any(|c| c.starts_with(prefix))
}

fn count(v: &RefCell<Vec<String>>, prefix: &str) -> usize {
    v.borrow().iter().filter(|c| c.starts_with(prefix)).count()
}

// ------------------------------------------------------------------------ pure parts

#[test]
fn classifiers_match_the_table() {
    assert_eq!(classify_pending(None, None, 3600, 600), Pending::WaitUnknown);
    assert_eq!(classify_pending(Some(10), None, 3600, 600), Pending::WaitRunning);
    assert_eq!(classify_pending(Some(3600), Some(599), 3600, 600), Pending::WaitProgressing);
    assert_eq!(classify_pending(Some(3600), Some(600), 3600, 600), Pending::Cancel);
    assert_eq!(classify_pending(Some(9999), None, 3600, 600), Pending::Cancel);
    assert!(classify_fault_rerun(0, 2) && classify_fault_rerun(1, 2));
    assert!(!classify_fault_rerun(2, 2) && !classify_fault_rerun(0, 0));
    assert_eq!(normalize_status("provision_fault"), "harness_fault");
    assert_eq!(normalize_status("green"), "green");
    assert_eq!(parse_run_metadata("started-at: 100\nlast-activity: 150\nlast-activity: 140\nlast-activity: x\n"), (Some(100), Some(150)));
    assert_eq!(parse_run_metadata(""), (None, None));
}

#[test]
fn cli_takes_exactly_one_repository() {
    let p = |a: &[&str]| cli::parse(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(p(&["verdict", "spira"]), Ok(Cmd::Verdict { repo: "spira".into() }));
    assert_eq!(p(&["verdict"]), Err(cli::Usage("queue.sh verdict: repo required".into())));
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
    fs::write(t.qfile("publish"), "pr=77\nhead=m3\nbranch=b\nremote=origin\n").unwrap();
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("verdict spira: publish record is missing a field — leaving it for a hand look:"), "{}", t.err());
    assert!(t.qfile("publish").exists());
    assert!(t.forge.calls.borrow().is_empty());
}

#[test]
fn publish_green_fast_forwards_the_forge_and_retires_the_record() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    t.forge.status.borrow_mut().push(Some("green\nrun-url: http://r/1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert_eq!(t.forge.calls.borrow()[0], "check-status 77 spira/publish/X");
    assert!(t.lib.has("push origin m3:refs/heads/main"));
    assert!(has_call(&t.forge.calls, "pr-close 77"));
    assert!(!t.qfile("publish").exists());
    assert!(t.landing_log().contains("QUEUE PUBLISH_GREEN 1000 repo=spira pr=77 head=m3"));
    assert!(t.lib.has("notify spira publish PR 77 merged"));
    assert!(t.out().contains("verdict spira: publish PR 77 green — origin/main fast-forwarded to m3"));
    t.assert_lc_untouched();
}

#[test]
fn publish_green_that_would_not_fast_forward_keeps_the_record_and_says_so() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    t.lib.push_ok.set(false);
    t.forge.status.borrow_mut().push(Some("green\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("publish PR 77 green but origin/main would not fast-forward — something moved it"));
    assert!(t.lib.has("notify spira publish PR 77 green but fast-forward refused"));
    assert!(t.qfile("publish").exists());
    assert!(!has_call(&t.forge.calls, "pr-close"));
}

#[test]
fn publish_pending_or_faulted_waits_and_changes_nothing() {
    for (answer, word) in [(Some("pending\n"), "pending"), (Some("harness_fault\n"), "harness_fault"), (Some("provision_fault\n"), "harness_fault"), (None, "")] {
        let t = T::new(LandMode::QueueLocal);
        publish_record(&t);
        t.forge.status.borrow_mut().push(answer.map(String::from));
        assert_eq!(t.run(&["verdict", "spira"]), 0);
        assert!(t.out().contains(&format!("verdict spira: publish PR 77: {word} — waiting")), "{}", t.out());
        assert!(t.qfile("publish").exists());
        assert!(!t.lib.has("push") && !has_call(&t.forge.calls, "pr-close"));
    }
}

#[test]
fn publish_red_attributes_files_one_fix_forward_and_marks_the_head() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-b.sh\nred-suite: test-a.sh test-b.sh\nrun-url: http://r/9\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    // one check-status call: the red half reads the answer the locked half judged
    assert_eq!(count(&t.forge.calls, "check-status"), 1);
    assert!(has_call(&t.scripts.calls, "attribute spira/publish/X f0 test-b.sh,test-a.sh sp-a,sp-b"), "{:?}", t.scripts.calls.borrow());
    assert!(has_call(&t.forge.calls, "pr-close 77"));
    let bug = t.lib.calls.borrow().iter().find(|c| c.starts_with("create_bug")).cloned().unwrap();
    assert!(bug.starts_with("create_bug queue.sh 1 spira,plan,repo:spira publish PR 77 red for spira: test-b.sh,test-a.sh\n"), "{bug}");
    assert!(bug.contains("Publish PR 77 red for spira (http://r/9).\n\nPublished range: f0..m3\nRed suites: test-b.sh,test-a.sh\n\nMembers in this publish: sp-a,sp-b\n\n"));
    assert!(bug.contains("Local attribution (attribute.sh):\nATTR suite-a owner=sp-a method=single\nEJECT sp-a suite-a\n\n"));
    assert!(bug.ends_with("Fix forward on local/main — the next publish carries the fix. Production was never rolled back and no member bead was reopened."));
    assert!(!t.qfile("publish").exists());
    assert_eq!(fs::read_to_string(t.qfile("publish-red")).unwrap(), "head=m3\nfix_forward=sp-fix1\n");
    assert!(t.landing_log().contains("QUEUE PUBLISH_RED 1000 repo=spira pr=77 suites=test-b.sh,test-a.sh fix_forward=sp-fix1"));
    assert!(t.lib.has("notify spira publish PR 77 red"));
    assert!(t.out().contains("verdict spira: publish PR 77 red — filed fix-forward sp-fix1"));
    // never a reopen, never a landstate change, never judgement
    assert!(!t.lib.has("bead_reopen") && !t.lib.has("land_mark"));
    assert!(!has_call(&t.scripts.calls, "judgement-ci"));
}

#[test]
fn publish_red_naming_no_suite_files_one_bead_without_attribution_and_frees_the_lock() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    t.var("SPIRA_QUEUE_ACTOR", "concierge");
    t.forge.status.borrow_mut().push(Some("red\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    // no suites named: no attribution, still one bead
    assert!(!has_call(&t.scripts.calls, "attribute"));
    let bug = t.lib.calls.borrow().iter().find(|c| c.starts_with("create_bug")).cloned().unwrap();
    assert!(bug.starts_with("create_bug concierge 1 spira,plan,repo:spira publish PR 77 red for spira\n"), "{bug}");
    assert!(bug.contains("Red suites: <none named>"));
    assert!(matches!(crate::lock::try_lock(&t.s().queue_dir, "spira"), crate::lock::Acquire::Held(_)));
}

#[test]
fn publish_red_with_no_bead_filed_is_a_fault_but_still_marks_the_head() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    *t.lib.bug_id.borrow_mut() = None;
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert_eq!(fs::read_to_string(t.qfile("publish-red")).unwrap(), "head=m3\nfix_forward=<create-failed>\n");
    assert!(t.out().contains("filed fix-forward <create-failed>"));
}

#[test]
fn publish_settle_skips_its_turn_when_the_queue_lock_is_held() {
    let t = T::new(LandMode::QueueLocal);
    publish_record(&t);
    let _g = t.hold_lock();
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("verdict spira: another queue operation holds the lock"));
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
    let published = out.find("nothing to publish for spira").expect("publish ran after");
    assert!(settled < published, "{out}");
}

// ------------------------------------------------------------------ queue.forge batch

fn batch(t: &T) {
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta sp-b:tb\nopened=500\nbranch=spira/queue/x\nowner=batcher\n");
    t.git.set("origin/main", "b0");
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
    assert!(t.err().contains("verdict spira: refused — this batch is claimed by concierge"), "{}", t.err());
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
    assert!(t.out().contains("verdict spira: PR 12 pending (run age 500s)"), "{}", t.out());
    // a run whose start cannot be read: never cancelled
    let t = T::new(LandMode::Queue);
    batch(&t);
    *t.forge.run.borrow_mut() = Some("77".into());
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("verdict spira: PR 12 pending (run 77 age unknown)"), "{}", t.out());
    // old but active
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.verdict.ci_maxsec = 100;
    *t.forge.run.borrow_mut() = Some("77".into());
    *t.forge.meta.borrow_mut() = "started-at: 800\nlast-activity: 950\n".into();
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("verdict spira: PR 12 run 77 progressing (last activity 50s ago)"), "{}", t.out());
    assert!(!has_call(&t.forge.calls, "run-cancel"));
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
    assert!(t.out().contains("verdict spira: PR 12 run stuck (200s, idle 50s) — cancelled; will retry on next pass"), "{}", t.out());
    assert_eq!(record(&t), before);
}

#[test]
fn a_failed_status_call_is_pending_not_a_verdict() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.forge.status.borrow_mut().push(None);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("PR 12 pending"), "{}", t.out());
    assert!(!t.lib.has("land_mark"));
}

#[test]
fn harness_fault_reruns_within_the_budget_then_closes_and_returns_members() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    *t.forge.run.borrow_mut() = Some("77".into());
    t.forge.status.borrow_mut().extend([Some("harness_fault\n".into()), Some("provision_fault\n".into()), Some("harness_fault\n".into())]);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("verdict spira: PR 12 harness fault — re-running (attempt 1/2)"));
    assert!(record(&t).ends_with("retries=1\n"));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("re-running (attempt 2/2)"));
    assert!(record(&t).ends_with("retries=2\n") && !record(&t).contains("retries=1"));
    assert_eq!(count(&t.forge.calls, "workflow-rerun 77"), 2);
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("verdict spira: PR 12 harness fault — retries exhausted; closing batch"));
    assert!(has_call(&t.forge.calls, "pr-close 12"));
    assert!(t.lib.has("land_mark sp-a CERTIFIED ta") && t.lib.has("land_mark sp-b CERTIFIED tb"));
    assert!(!t.qfile("open").exists());
    let mail = t.scripts.calls.borrow().iter().find(|c| c.starts_with("mail-operator")).cloned().unwrap();
    assert!(mail.starts_with("mail-operator Merge queue: spira CI fault after 3 attempts\n## Note\nMerge queue batch for spira closed after 3 failed CI run attempts.\n\nPR 12 (head h1) has been closed."), "{mail}");
}

#[test]
fn green_on_another_head_or_no_head_is_refused_as_a_fault() {
    for (answer, line, subject) in [
        ("green\nhead-sha: hX\n", "verdict spira: PR 12 CI head mismatch (ci=hX sealed=h1) — harness fault; PR closed, members requeued", "Merge queue: spira CI head mismatch"),
        ("green\n", "verdict spira: PR 12 green but head-sha missing — cannot verify sealed head; harness fault; PR closed, members requeued", "Merge queue: spira CI head unverifiable (missing head-sha)"),
    ] {
        let t = T::new(LandMode::Queue);
        batch(&t);
        t.forge.status.borrow_mut().push(Some(answer.into()));
        assert_eq!(t.run(&["verdict", "spira"]), 0);
        assert!(t.out().contains(line), "{}", t.out());
        assert!(has_call(&t.scripts.calls, &format!("mail-operator {subject}\n")));
        assert!(t.lib.has("land_mark sp-a CERTIFIED ta"));
        assert!(!t.lib.has("push") && !t.lib.has("land_mark sp-a LANDED"));
        assert!(!t.qfile("open").exists());
    }
}

#[test]
fn green_on_an_unmoved_base_fast_forwards_lands_every_member_and_cleans_up() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.git.set("refs/heads/spira/queue/x", "h1");
    t.git.set("refs/heads/spira/queue/old", "o1");
    t.git.ancestor("spira/queue/old", "origin/main");
    *t.git.branches.borrow_mut() = vec![("spira/queue/old".into(), "o1".into()), ("spira/queue/new".into(), "n1".into())];
    t.forge.status.borrow_mut().push(Some("green\nhead-sha: h1\nflaky: test-f.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(t.lib.has("push origin h1:main"));
    assert!(t.out().contains("verdict spira: PR 12 landed by fast-forward (h1)"));
    for (id, tip) in [("sp-a", "ta"), ("sp-b", "tb")] {
        assert!(t.lib.has(&format!("land_mark {id} LANDED {tip} ")), "{:?}", t.lib.calls.borrow());
        assert!(t.lib.has(&format!("gh_closeout {id} h1")));
        assert!(t.lib.has(&format!("close_on_land {id} h1")));
    }
    assert!(t.lib.has("notify spira PR 12 merged (fast-forward)"));
    assert!(has_call(&t.scripts.calls, "observe-flake test-f.sh h1"));
    assert!(!t.qfile("open").exists());
    assert!(t.lib.has("push origin :refs/heads/spira/queue/x"));
    assert!(t.git.calls.borrow().contains(&"branch -D sanctioned spira/queue/x".to_string()));
    assert!(t.out().contains("verdict spira: deleted batch branch spira/queue/x"));
    // a stale queue ref already on the base is reaped; one that is not stays
    assert!(t.out().contains("verdict spira: reaped stale queue ref spira/queue/old"));
    assert!(!t.git.calls.borrow().iter().any(|c| c.ends_with("spira/queue/new")));
    t.assert_lc_untouched();
}

#[test]
fn a_refused_fast_forward_keeps_the_batch() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.push_ok.set(false);
    t.forge.status.borrow_mut().push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("verdict spira: PR 12 fast-forward push failed"));
    assert!(t.qfile("open").exists());
    assert!(!t.lib.has("land_mark"));
}

#[test]
fn green_with_lifecycle_on_walks_the_batch_to_landed_on_spira_lc() {
    let t = T::new(LandMode::Queue);
    t.lifecycle_on();
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta\nbranch=\nbatch_id=B1\nversion=3\n");
    t.git.set("origin/main", "b0");
    t.forge.status.borrow_mut().push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let lc = t.lc.calls.borrow().clone();
    assert!(lc.contains(&"event batch B1 OPEN 3 {\"CiStarted\":{\"run\":\"12\"}}".to_string()), "{lc:?}");
    assert!(lc.contains(&"event batch B1 CI_RUNNING 4 \"Green\"".to_string()), "{lc:?}");
    assert!(lc.contains(&"land B1 5 h1".to_string()), "{lc:?}");
    assert!(t.out().contains("verdict: B1 landed on spira-lc (sha=h1)"));
}

#[test]
fn a_moved_base_rebuilds_and_force_pushes_when_no_member_is_in_it() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.git.set("origin/main", "b1");
    t.forge.status.borrow_mut().push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let git = t.git.calls.borrow().clone();
    assert!(git.contains(&"merge ta spira: land sp-a".to_string()) && git.contains(&"merge tb spira: land sp-b".to_string()), "{git:?}");
    assert!(t.lib.has("push origin +merged-tb:refs/heads/spira/queue/x"));
    let rec = record(&t);
    assert!(rec.ends_with("head=merged-tb\nretries=0\nmembers=sp-a:ta sp-b:tb\nbase=b1\n"), "{rec}");
    assert!(t.out().contains("verdict spira: PR 12 rebuilt on moved base (b1) — re-pushed (head merged-tb)"));
    assert!(!has_call(&t.forge.calls, "pr-close"));
    assert!(!t.lib.has("land_mark"));
}

#[test]
fn a_moved_base_already_carrying_a_member_closes_and_lands_that_member() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.git.set("origin/main", "b1");
    t.git.ancestor("ta", "b1");
    t.forge.status.borrow_mut().push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(!t.git.calls.borrow().iter().any(|c| c.starts_with("merge")));
    assert!(has_call(&t.forge.calls, "pr-close 12"));
    assert!(t.out().contains("verdict spira: PR 12 base moved (b1) — closed, members requeued"));
    assert!(t.lib.has("land_mark sp-a LANDED ta already-in-base") && t.lib.has("close_on_land sp-a b1"));
    assert!(t.out().contains("verdict spira: sp-a already in moved base — LANDED"));
    assert!(t.lib.has("land_mark sp-b CERTIFIED tb"));
    assert!(!t.qfile("open").exists());
}

#[test]
fn a_moved_base_that_conflicts_closes_and_requeues() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.git.set("origin/main", "b1");
    t.git.merge_fail.borrow_mut().insert("tb".into());
    t.forge.status.borrow_mut().push(Some("green\nhead-sha: h1\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("verdict spira: PR 12 base moved — conflict in sp-b; closing and requeuing"));
    assert!(!t.lib.has("push"));
    assert!(t.lib.has("land_mark sp-a CERTIFIED ta") && t.lib.has("land_mark sp-b CERTIFIED tb"));
    assert!(!t.qfile("open").exists());
}

#[test]
fn red_summons_judgement_once_with_the_suites_and_the_run() {
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut { rc: 0, out: "filed\nid=sp-j1\n".into(), err: String::new() };
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-b.sh\nred-suite: test-a.sh\nred-suite: test-b.sh\nrun-url: http://r/5\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    let j = t.scripts.calls.borrow().iter().find(|c| c.starts_with("judgement-ci")).cloned().unwrap();
    assert!(j.starts_with("judgement-ci batcher spira test-b.sh,test-a.sh sp-a,sp-b PR 12 — http://r/5 home="), "{j}");
    assert!(record(&t).ends_with("judgement=sp-j1\n"));
    assert!(t.out().contains("verdict spira: PR 12 red (test-b.sh,test-a.sh) — batcher-owned, summoned judgement (sp-j1)"));
    // the PR and its members are left exactly as they are
    assert!(!has_call(&t.forge.calls, "pr-close") && !t.lib.has("land_mark") && !t.lib.has("bead_reopen"));
    // the next pass does not summon twice
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert_eq!(count(&t.scripts.calls, "judgement-ci"), 1);
    assert!(t.out().contains("verdict spira: PR 12 red — batcher-owned, judgement already summoned (sp-j1)"));
}

#[test]
fn a_hand_cut_batch_red_goes_to_judgement_too() {
    // D1: verdict's own split/eject attribution is retired; owner is not consulted.
    let mut t = T::new(LandMode::Queue);
    t.open_record("pr=12\nhead=h1\nbase=b0\nmembers=sp-a:ta\nowner=operator\n");
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut { rc: 0, out: "id=sp-j2\n".into(), err: String::new() };
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0, "{}", t.err());
    assert!(has_call(&t.scripts.calls, "judgement-ci batcher spira test-a.sh sp-a PR 12 home="));
    assert!(t.out().contains("verdict spira: PR 12 red (test-a.sh) — summoned judgement (sp-j2)"), "{}", t.out());
}

#[test]
fn red_without_suites_or_without_a_batcher_is_not_judged() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.forge.status.borrow_mut().push(Some("red\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().contains("verdict spira: PR 12 red — batcher-owned, no suite annotations; leaving for the batcher to re-check"));
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.batcher_off = true;
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-a.sh\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("batcher-owned but the batcher is off (SPIRA_BATCHER_ENABLE=0); cannot summon judgement"));
    let mut t = T::new(LandMode::Queue);
    batch(&t);
    t.lib.s.batcher_bin = Some(PathBuf::from("batcher"));
    *t.scripts.judgement.borrow_mut() = RunOut { rc: 1, out: "batcher: no repo\n".into(), err: String::new() };
    t.forge.status.borrow_mut().push(Some("red\nred-suite: test-a.sh\n".into()));
    let before = record(&t);
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("batcher-owned, judgement-ci failed: batcher: no repo"));
    assert_eq!(record(&t), before);
}

#[test]
fn an_unknown_status_is_a_fault() {
    let t = T::new(LandMode::Queue);
    batch(&t);
    t.forge.status.borrow_mut().push(Some("weird\n".into()));
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("verdict spira: PR 12 unknown check status: weird"));
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
    assert!(t.out().ends_with("verdict spira: another queue operation holds the lock\n"), "{}", t.out());
    assert_eq!(t.clock.now(), 1_003, "waited lock_wait seconds");
    assert_eq!(fs::read_to_string(t.qfile("lock-skips")).unwrap(), "1\n");
    assert_eq!(t.run(&["verdict", "spira"]), 0);
    assert!(t.out().ends_with("verdict spira: queue lock starvation — skipped 2 consecutive ticks waiting for lock\n"), "{}", t.out());
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
    assert!(err.contains("attributing-41 is an express-takeover stash — its attribution is retired"), "{err}");
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
    t.lifecycle_on();
    *t.lc.in_delivery.borrow_mut() = Err("dolt down".into());
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("queue.sh verdict: lifecycle_enforce is on and spira-lc is unreachable (dolt down)"));
    assert!(t.forge.calls.borrow().is_empty());
}

#[test]
fn a_repo_the_map_does_not_carry_is_refused() {
    let t = T::new(LandMode::Queue);
    t.lib.r.borrow_mut().path = None;
    assert_eq!(t.run(&["verdict", "spira"]), 1);
    assert!(t.err().contains("verdict spira: no repo-map entry"));
}
