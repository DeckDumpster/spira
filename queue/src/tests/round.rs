//! `queue round` (DESIGN.md §2.2): one world of fakes, a real scratch directory for the
//! round record, the markers round-duty reads and the certificate land-local checks.

use super::*;

const T_ROUND: &str = "3333333333333333333333333333333333333333";

fn round_world() -> T {
    let t = T::new(LandMode::QueueLocal);
    t.git.set("refs/heads/local/main", "b0");
    t.git.set("refs/remotes/origin/main", "f0");
    for (id, tip) in [("sp-a", "ta"), ("sp-b", "tb"), ("sp-c", "tc")] {
        t.lc_row(id, "CERTIFIED", tip, 100);
        t.git.ancestor(tip, "merged-tc");
        t.git.ancestor(tip, "merged-tb");
    }
    for head in ["merged-ta", "merged-tb", "merged-tc"] {
        t.git.ancestor("b0", head);
        t.git.trees.borrow_mut().insert(head.into(), T_ROUND.into());
    }
    t
}

fn kv_of(t: &T, name: &str) -> BTreeMap<String, String> {
    fs::read_to_string(t.qfile(name)).unwrap_or_default().lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn marker(t: &T, batch: &str, ext: &str) -> PathBuf {
    t.s().run.join("rounds").join(format!("{batch}.{ext}"))
}

fn batch_of(t: &T) -> String {
    t.out().lines().find_map(|l| l.strip_prefix("batch=")).unwrap().to_string()
}

fn certified_tree_exists(t: &T) -> bool {
    gate::cert::path(&t.s().run.join("verdicts"), "spira", T_ROUND).is_some_and(|p| p.exists())
}

#[test]
fn open_certify_land_is_one_round_the_whole_way() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into()), ("test-b.sh".into(), "skip".into())];

    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(batch, "spira-20260929T010203Z");
    assert!(t.out().contains("head=merged-tb") && t.out().contains("members=sp-a:ta sp-b:tb"), "{}", t.out());
    assert!(t.lc.has(&format!("cut {batch} sp-a:ta,sp-b:tb")), "spira-lc cut records the batch and moves the members");
    let rec = kv_of(&t, "round");
    assert_eq!((rec["phase"].as_str(), rec["head"].as_str(), rec["base"].as_str()), ("opened", "merged-tb", "b0"));
    assert!(marker(&t, &batch, "running").exists(), "round-duty sees a round in flight");

    let out = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0);
    let status = t.out()[out..].to_string();
    assert!(status.contains("round=open") && status.contains(&format!("batch_id={batch}")) && status.contains("phase=opened"), "{status}");
    assert!(status.contains("wall_secs=0"), "{status}");

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(t.scripts.calls.borrow().iter().any(|c| c.ends_with("wall=900")), "the corpus runs under the configured cap: {:?}", t.scripts.calls.borrow());
    assert!(t.lc.has(&format!("event batch {batch} CI_RUNNING 4 {{\"PassGreen\":{{\"n\":1,\"suites_s\":0,\"build_s\":0}}}}")), "{:?}", t.lc.calls.borrow());
    assert!(certified_tree_exists(&t), "the round GREEN certificate is on the head's tree");
    assert_eq!(kv_of(&t, "round")["phase"], "green");

    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
    assert!(t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"") && t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\""));
    assert!(t.lc.has(&format!("land {batch} 4 merged-tb")), "spira-lc lands the batch");
    assert!(!t.qfile("round").exists() && !marker(&t, &batch, "running").exists());
    let result = fs::read_to_string(marker(&t, &batch, "result")).unwrap();
    assert!(result.starts_with("landed merged-tb members=sp-a:ta,sp-b:tb"), "{result}");
}

#[test]
fn land_with_the_batch_record_refused_prints_the_reason_and_exits_nonzero() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    t.lc.land_refused.set(Some("stale version 4"));
    assert_eq!(t.run(&["round", "land", &batch]), 1, "{}", t.err());
    let e = t.err();
    assert!(e.contains(&batch) && e.contains("stale version 4") && e.contains("ALARM"), "{e}");
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"), "the release is live regardless");
}

#[test]
fn eject_then_land_rebuilds_the_head_without_the_member() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into()), ("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(t.out().contains("RED") && t.out().contains("red=test-b.sh"), "{}", t.out());
    assert_eq!(kv_of(&t, "round")["phase"], "red");
    assert!(!certified_tree_exists(&t), "a red round certifies nothing");
    assert_eq!(t.run(&["round", "land", &batch]), 1, "a red round does not land");
    assert!(t.err().contains("is red, not green"), "{}", t.err());

    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red on test-b.sh", "--suites", "test-b.sh"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_reopen sp-b eject-red test-b.sh"));
    assert!(t.lc.has("event bead sp-b IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected\"}}") || t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\""));
    assert!(t.lc.has(&format!("eject-member {batch} sp-b CI_RUNNING 4 red on test-b.sh")));
    let rec = kv_of(&t, "round");
    assert_eq!((rec["members"].as_str(), rec["head"].as_str(), rec["phase"].as_str()), ("sp-a:ta sp-c:tc", "merged-tc", "opened"));
    assert_eq!(rec["ejected"].trim(), "sp-b");
    assert!(t.git.calls.borrow().iter().filter(|c| c.starts_with("merge ")).count() >= 5, "the survivors are merged again onto the base");

    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into()), ("test-b.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tc"));
    assert!(t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"") && t.lc.has("event bead sp-c CERTIFIED 3 \"Deliver\""));
    assert!(t.lc.has(&format!("land {batch} 4 merged-tc")));
}

#[test]
fn a_refused_lifecycle_eject_changes_nothing() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let before = kv_of(&t, "round");
    t.lc.eject_refused.set(true);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "late"]), 1);
    assert!(t.err().contains("nothing changed"), "{}", t.err());
    assert_eq!(kv_of(&t, "round"), before, "the git head and member list stay as they were");
    assert!(!t.lib.has("bead_reopen"), "the bead is not reopened for an eject the machine refused");
}

#[test]
fn an_eject_whose_lifecycle_write_is_locked_exits_nonzero_naming_the_lock() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    t.lc.event_refused.set(Some("database is locked"));
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red"]), 1);
    let e = t.err();
    assert!(e.contains("database is locked") && e.contains("sp-b") && e.contains("did not return to REWORK"), "{e}");
}

#[test]
fn a_successful_eject_reports_the_round_and_the_remaining_count() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red"]), 0, "{}", t.err());
    assert!(t.out().contains(&format!("ejected sp-b from round {batch}; 2 member(s) remain")), "{}", t.out());
}

#[test]
fn ejecting_the_last_member_closes_the_round() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-a", "--reason", "harness"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_reopen sp-a eject "));
    assert!(t.lc.has(&format!("abandon-batch {batch} CI_RUNNING 4 round emptied")));
    assert!(!t.qfile("round").exists() && !marker(&t, &batch, "running").exists());
}

#[test]
fn a_harness_fault_eject_naming_suites_is_not_charged() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "round VM lacked the config delta", "--suites", "test-b.sh", "--harness-fault"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_reopen sp-b eject test-b.sh") && !t.lib.has("bead_reopen sp-b eject-red"));
}

#[test]
fn abandon_returns_the_members_and_closes_the_round() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb", "--name", "hand-1"]), 0, "{}", t.err());
    assert_eq!(batch_of(&t), "hand-1");
    assert_eq!(t.run(&["round", "abandon", "hand-1"]), 2, "a reason is required");
    assert_eq!(t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]), 0, "{}", t.err());
    assert!(t.lc.has("abandon-batch hand-1 CI_RUNNING 4 base moved"));
    assert!(!t.qfile("round").exists() && !marker(&t, "hand-1", "running").exists());
    assert!(fs::read_to_string(marker(&t, "hand-1", "result")).unwrap().starts_with("abandoned: base moved"));
    assert!(!t.lc.has("land "), "an abandoned round lands nothing");
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test]
fn abandon_requeues_a_member_the_batch_abandon_left_in_delivery() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb", "--name", "hand-1"]), 0, "{}", t.err());
    for r in t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut().filter(|r| r.bead_id == "sp-a") {
        r.state = "IN_DELIVERY".into();
    }
    t.lc.bead_rows.borrow_mut().insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    assert_eq!(t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]), 0, "{}", t.err());
    assert!(t.lc.has("event bead sp-a IN_DELIVERY 7 {\"Requeued\":{\"tip\":\"ta\"}}"), "{:?}", t.lc.calls.borrow());
    assert!(!t.lc.has("event bead sp-b"), "a member not in delivery is left alone");
}

#[test]
fn open_skips_what_is_not_certified_or_does_not_merge() {
    let t = round_world();
    t.lc_row("sp-d", "REWORK", "td", 100);
    t.git.merge_fail.borrow_mut().insert("tb".into());
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b,sp-d,sp-zz"]), 0, "{}", t.err());
    let o = t.out();
    assert!(o.contains("sp-b: conflicts with the round") && o.contains("sp-d: lifecycle state=REWORK") && o.contains("sp-zz: no lifecycle row"), "{o}");
    assert!(o.contains("members=sp-a:ta"), "{o}");

    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("ta".into());
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 1);
    assert!(t.err().contains("nothing admissible") && !t.lc.has("cut") && !t.qfile("round").exists());
}

fn block(t: &T, id: &str, by: &[&str]) {
    for r in t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut().filter(|r| r.bead_id == id) {
        r.blocked_by = by.iter().map(|b| b.to_string()).collect();
    }
}

#[test]
fn open_refuses_a_member_whose_blocker_has_not_landed_and_names_it() {
    let t = round_world();
    block(&t, "sp-b", &["sp-eeg"]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    let o = t.out();
    assert!(o.contains("sp-b: blocked by sp-eeg") && o.contains("members=sp-a:ta") && !o.contains("sp-b:tb"), "{o}");
}

#[test]
fn open_admits_a_member_whose_blocker_is_merged_ahead_of_it_in_the_round() {
    let t = round_world();
    block(&t, "sp-b", &["sp-a"]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    assert!(t.out().contains("members=sp-a:ta sp-b:tb"), "{}", t.out());
}

#[test]
fn open_refuses_a_member_listed_ahead_of_its_blocker() {
    let t = round_world();
    block(&t, "sp-a", &["sp-b"]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    assert!(t.out().contains("sp-a: blocked by sp-b") && t.out().contains("members=sp-b:tb"), "{}", t.out());
}

#[test]
fn open_admits_a_member_whose_blocker_has_landed() {
    let t = round_world();
    block(&t, "sp-b", &[]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 0, "{}", t.err());
    assert!(t.out().contains("members=sp-a:ta sp-b:tb"), "{}", t.out());
}

#[test]
fn open_returns_a_base_conflict_to_rework_and_keeps_a_round_conflict_queued() {
    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("tb".into());
    t.lib.conflict_with_base.borrow_mut().insert("tb".into());
    t.git.merge_fail.borrow_mut().insert("tc".into());
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]), 0, "{}", t.err());
    let o = t.out();
    assert!(o.contains("sp-b: conflicts with base — returned to rework") && o.contains("sp-c: conflicts with the round\n"), "{o}");
    assert!(t.lc.has("event bead sp-b CERTIFIED 3 {\"GateRed\":{\"tip\":\"tb\",\"reason\":\"no-rebase\"}}"), "{:?}", t.lc.calls.borrow());
    assert!(!t.lc.has("event bead sp-c"), "{:?}", t.lc.calls.borrow());
    assert_eq!(t.lc.bead_state("sp-b").unwrap().0, "REWORK");
}

#[test]
fn open_fails_and_does_not_claim_a_return_the_lifecycle_refused_or_ignored() {
    for ignored in [false, true] {
        let t = round_world();
        t.git.merge_fail.borrow_mut().insert("tb".into());
        t.lib.conflict_with_base.borrow_mut().insert("tb".into());
        if ignored {
            t.lc.event_ignored.set(true);
        } else {
            t.lc.event_refused.set(Some("refused: IllegalTransition"));
        }
        assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-b"]), 1, "ignored={ignored}: {}", t.err());
        assert!(!t.out().contains("returned to rework") && t.out().contains("sp-b: conflicts with base ("), "{}", t.out());
        assert!(t.err().contains("could not be returned to rework") && !t.lc.has("cut") && !t.qfile("round").exists(), "{}", t.err());
    }
}

#[test]
fn open_admits_a_submitted_member_its_full_suite_certifies() {
    // law-a-round-is-feature-first-then-catch-all: a round takes SUBMITTED beads directly.
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-s"]), 0, "{}", t.err());
    let o = t.out();
    assert!(o.contains("sp-s:ts") && !o.contains("sp-s: lifecycle state"), "{o}");
    assert!(t.lc.has("cut"), "the round is recorded as a batch");
}

#[test]
fn a_second_round_is_refused_until_the_first_is_closed() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "open", "--members", "sp-b"]), 1);
    assert!(t.err().contains("a round is already open") && t.err().contains("phase opened"), "{}", t.err());
    assert_eq!(t.run(&["round", "land", "someone-else"]), 1);
    assert!(t.err().contains("is spira-20260929T010203Z, not someone-else"), "{}", t.err());
}

#[test]
fn certify_never_reads_a_harness_fault_as_green_or_red() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 4, "no results at all is a fault");
    assert!(t.err().contains("no verdicts") && kv_of(&t, "round")["phase"] == "fault", "{}", t.err());

    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 124, out: String::new(), err: "killed".into() };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(t.err().contains("exceeded the 900s wall"), "{}", t.err());
    assert!(!certified_tree_exists(&t) && !t.lc.has("event batch spira-20260929T010203Z CI_RUNNING 4 {\"PassGreen\""));
}

#[test]
fn certify_faults_on_an_unreadable_or_missing_suite_verdict() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let spira = t.s().run.join("worktree").join(".round-spira").join("spira");
    fs::create_dir_all(&spira).unwrap();
    for s in ["test-a.sh", "test-b.sh"] {
        fs::write(spira.join(s), "").unwrap();
    }
    let ok = |s: &str| (s.to_string(), "ok".to_string());
    for (results, want) in [
        (vec![ok("test-a.sh"), ("test-b.sh".into(), "".into())], "test-b.sh (verdict \"\")"),
        (vec![ok("test-a.sh"), ("test-b.sh".into(), "garbage".into())], "test-b.sh (verdict \"garbage\")"),
        (vec![ok("test-a.sh")], "test-b.sh (no result)"),
    ] {
        *t.scripts.round_vm_results.borrow_mut() = results;
        assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
        assert!(t.err().contains(want) && t.err().contains("not judged"), "{}", t.err());
        assert!(!certified_tree_exists(&t) && kv_of(&t, "round")["phase"] == "fault");
    }
    *t.scripts.round_vm_results.borrow_mut() = vec![ok("test-a.sh"), ("test-b.sh".into(), "skip".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(certified_tree_exists(&t));
}

#[test]
fn the_blocking_only_reader_passed_what_unjudgeable_now_refuses() {
    let found = vec![("test-a.sh".to_string(), String::new()), ("test-b.sh".to_string(), "garbage".to_string())];
    let old_reds: Vec<_> = found.iter().filter(|(_, s)| super::super::ops::round::BLOCKING.contains(&s.as_str())).collect();
    assert!(old_reds.is_empty(), "control: the old reader found nothing wrong");
    assert!(super::super::ops::round::unjudgeable(Path::new("/nonexistent"), &found).is_some());
}

#[test]
fn attest_names_the_worktrees_own_head_and_nothing_else() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    t.git.set("refs/heads/elsewhere", "merged-ta");
    assert_eq!(t.run(&["round", "certify", &batch, "--attest", "elsewhere"]), 1);
    assert!(t.err().contains("is not the round worktree's head"), "{}", t.err());
    assert!(t.scripts.calls.borrow().iter().all(|c| !c.starts_with("round-vm")), "an attestation runs no corpus");

    assert_eq!(t.run(&["round", "certify", &batch, "--attest", "merged-tb"]), 0, "{}", t.err());
    assert!(certified_tree_exists(&t));
    assert!(t.scripts.calls.borrow().iter().all(|c| !c.starts_with("round-vm")));
}

#[test]
fn round_verbs_belong_to_queue_local() {
    let t = T::new(LandMode::Queue);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 1);
    assert!(t.err().contains("not in queue.local mode"), "{}", t.err());
    let t = round_world();
    assert_eq!(t.run(&["round", "status"]), 0);
    assert!(t.out().contains("round=none"), "{}", t.out());
}

#[test]
fn a_caller_holding_the_lock_runs_the_verbs_under_it() {
    let t = round_world();
    let _held = t.hold_lock();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 1);
    assert!(t.err().contains("another queue operation holds the lock"), "{}", t.err());
    t.var("SPIRA_QUEUE_LOCK_HELD", "1");
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
}

#[test]
fn eject_also_ejects_the_members_stacked_on_the_ejected_one() {
    let t = round_world();
    t.git.bases.borrow_mut().insert(("ta".into(), "tb".into()), "own-commit-of-a".into());
    t.git.bases.borrow_mut().insert(("ta".into(), "tc".into()), "b0".into());
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "eject", &batch, "sp-a", "--reason", "red on lint"]), 0, "{}", t.err());
    assert!(t.out().contains("ejected sp-b from round") && t.out().contains("stacked on sp-a"), "{}", t.out());
    assert!(t.lc.calls.borrow().iter().any(|c| c.starts_with(&format!("eject-member {batch} sp-b ")) && c.ends_with("stacked on sp-a")), "b's reason names a");
    assert!(t.lib.has("bead_reopen sp-b eject "));
    let rec = kv_of(&t, "round");
    assert_eq!((rec["members"].as_str(), rec["head"].as_str()), ("sp-c:tc", "merged-tc"));
    assert_eq!(rec["ejected"].trim(), "sp-a sp-b");
    assert!(!t.lib.has("bead_reopen sp-c eject "), "an independent member stays");
}

fn events_of(t: &T, batch: &str) -> Vec<String> {
    let prefix = format!("event batch {batch} ");
    t.lc.calls.borrow().iter().filter_map(|c| c.strip_prefix(&prefix)).map(|c| c.splitn(3, ' ').nth(2).unwrap_or("").to_string()).collect()
}

#[test]
fn a_round_through_a_red_pass_an_eject_and_a_green_pass_records_both_passes() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into()), ("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    *t.scripts.round_vm_meta.borrow_mut() = "build_wall_s=30\n".into();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert_eq!(
        events_of(&t, &batch),
        [
            "{\"PassStarted\":{\"n\":1,\"head\":\"merged-tc\"}}",
            "{\"SuitesStarted\":{\"n\":1}}",
            "{\"PassRed\":{\"n\":1,\"red_suites\":[\"test-b.sh\"],\"suites_s\":0,\"build_s\":30}}",
        ],
        "pass 1 is started, enters its suites and ends red, naming the suite"
    );
    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red on test-b.sh", "--suites", "test-b.sh"]), 0, "{}", t.err());
    assert!(events_of(&t, &batch).last().unwrap().starts_with("{\"PassRebuilt\":{\"head\":"), "{:?}", events_of(&t, &batch));

    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into()), ("test-b.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let events = events_of(&t, &batch);
    assert!(events.iter().any(|e| e.starts_with("{\"PassStarted\":") && e.contains("\"n\":2")), "{events:?}");
    assert!(events.last().unwrap().starts_with("{\"PassGreen\":{\"n\":2"), "{events:?}");
}

#[test]
fn a_pass_the_vm_could_not_finish_is_incomplete_not_red_or_green() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 124, out: String::new(), err: "killed".into() };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    let events = events_of(&t, &batch);
    assert!(events.last().unwrap().starts_with("{\"PassIncomplete\":{\"n\":1,\"reason\":\"round-vm exceeded the 900s wall"), "{events:?}");
    assert!(!events.iter().any(|e| e.starts_with("{\"PassRed\"") || e.starts_with("{\"PassGreen\"")), "{events:?}");
}

#[test]
fn the_pass_verbs_record_a_hand_driven_pass_like_the_batchers() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "suites-started", &batch]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "pass-verdict", &batch, "--verdict", "red", "--red-suites", "test-x.sh", "--suites-s", "90", "--build-s", "30"]), 0, "{}", t.err());
    assert_eq!(
        events_of(&t, &batch),
        [
            "{\"PassStarted\":{\"n\":1,\"head\":\"merged-ta\"}}",
            "{\"SuitesStarted\":{\"n\":1}}",
            "{\"PassRed\":{\"n\":1,\"red_suites\":[\"test-x.sh\"],\"suites_s\":90,\"build_s\":30}}",
        ]
    );
}

#[test]
fn a_pass_verb_out_of_phase_is_sent_so_the_machine_can_refuse_it_by_name() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "the fake machine does not refuse; the verb must still have sent the event");
    assert_eq!(events_of(&t, &batch).iter().filter(|e| e.starts_with("{\"PassStarted\"")).count(), 2, "a strict verb never swallows a wrong-phase call");
    assert_eq!(t.run(&["round", "pass-verdict", &batch, "--verdict", "incomplete"]), 2, "an incomplete pass names why");
}
