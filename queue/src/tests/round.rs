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

fn round_phase(t: &T) -> String {
    let rec = kv_of(t, "round");
    assert!(!rec.contains_key("phase") && !rec.contains_key("phase_at"), "the round file carries no phase: {rec:?}");
    t.lc.batch_state(&rec["batch_id"]).unwrap().0
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
    assert_eq!((round_phase(&t).as_str(), rec["head"].as_str(), rec["base"].as_str()), ("OPEN", "merged-tb", "b0"));
    assert!(marker(&t, &batch, "running").exists(), "round-duty sees a round in flight");

    let out = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0);
    let status = t.out()[out..].to_string();
    assert!(status.contains("round=open") && status.contains(&format!("batch_id={batch}")) && status.contains("phase=opened"), "{status}");
    assert!(status.contains("wall_secs=0"), "{status}");

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(t.scripts.calls.borrow().iter().any(|c| c.ends_with("wall=900")), "the corpus runs under the configured cap: {:?}", t.scripts.calls.borrow());
    assert!(t.lc.has(&format!("event batch {batch} CI_RUNNING 3 {{\"PassGreen\":{{\"n\":1,\"suites_s\":0,\"build_s\":0}}}}")), "{:?}", t.lc.calls.borrow());
    assert!(certified_tree_exists(&t), "the round GREEN certificate is on the head's tree");
    assert_eq!(round_phase(&t), "GREEN");

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

fn certified_round() -> (T, String) {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    (t, batch)
}

#[test]
fn land_rides_out_a_store_restart_after_activation_and_records_the_batch() {
    let (t, batch) = certified_round();
    t.lc.down_for.set(5);
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert!(t.lc.has(&format!("land {batch} 4 merged-tb")), "{:?}", t.lc.calls.borrow());
    assert!(!t.err().contains("ALARM"), "{}", t.err());
}

#[test]
fn land_with_the_store_down_past_the_deadline_exits_nonzero_naming_the_batch() {
    let (t, batch) = certified_round();
    t.lc.down_for.set(10_000);
    assert_eq!(t.run(&["round", "land", &batch]), 1, "{}", t.err());
    let e = t.err();
    assert!(e.contains(&batch) && e.contains("ALARM"), "{e}");
    assert!(!t.lc.has("land "), "{:?}", t.lc.calls.borrow());
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
    assert_eq!(round_phase(&t), "ATTRIBUTING");
    assert!(!certified_tree_exists(&t), "a red round certifies nothing");
    assert_eq!(t.run(&["round", "land", &batch]), 1, "a red round does not land");
    assert!(t.err().contains("is attributing, not green"), "{}", t.err());

    assert_eq!(t.run(&["round", "eject", &batch, "sp-b", "--reason", "red on test-b.sh", "--suites", "test-b.sh"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_reopen sp-b eject-red test-b.sh"));
    assert!(t.lc.has("event bead sp-b IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected\"}}") || t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\""));
    assert!(t.lc.has(&format!("eject-member {batch} sp-b ATTRIBUTING 4 red on test-b.sh")));
    let rec = kv_of(&t, "round");
    assert_eq!((rec["members"].as_str(), rec["head"].as_str(), round_phase(&t).as_str()), ("sp-a:ta sp-c:tc", "merged-tc", "OPEN"));
    assert_eq!(rec["ejected"].trim(), "sp-b");
    assert!(t.git.calls.borrow().iter().filter(|c| c.starts_with("merge ")).count() >= 5, "the survivors are merged again onto the base");

    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into()), ("test-b.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tc"));
    assert!(t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"") && t.lc.has("event bead sp-c CERTIFIED 3 \"Deliver\""));
    assert!(t.lc.has(&format!("land {batch} 8 merged-tc")));
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
    assert!(t.lc.has(&format!("abandon-batch {batch} OPEN 1 round emptied")));
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
    assert!(t.lc.has("abandon-batch hand-1 OPEN 1 base moved"));
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

fn file_gate_verdict(t: &T, id: &str, tip: &str, rc: i32) {
    use landing_pass::gateq::{Done, GateQueue, Job};
    let job = Job::new("spira", &format!("spira/{id}"), id, tip, false);
    let run = landing_pass::model::GateRun::parse(rc, "gate: VERDICT=FAIL reason=branch-red suite=test-x.sh".into());
    GateQueue::new(&t.s().run).complete(0, &Done { job, run, started_ms: 1, finished_ms: 2 }).unwrap();
}

#[test]
fn open_skips_a_submitted_member_whose_gate_is_fail_at_its_current_tip_and_only_that() {
    let t = round_world();
    for id in ["sp-f", "sp-old", "sp-base", "sp-nov"] {
        t.lc_row(id, "SUBMITTED", &format!("t{id}"), 100);
    }
    file_gate_verdict(&t, "sp-f", "tsp-f", 1);
    file_gate_verdict(&t, "sp-old", "an-older-tip", 1);
    file_gate_verdict(&t, "sp-base", "tsp-base", landing_pass::model::GATE_BASEFAIL);
    file_gate_verdict(&t, "sp-nov", "tsp-nov", landing_pass::model::GATE_NOVERDICT);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a,sp-f,sp-old,sp-base,sp-nov"]), 0, "{}", t.err());
    let o = t.out();
    assert!(o.contains("sp-f: its gate is FAIL at tsp-f (branch-red)"), "{o}");
    let members = o.lines().find_map(|l| l.strip_prefix("members=")).unwrap();
    assert!(members.contains("sp-a:") && members.contains("sp-old:") && members.contains("sp-base:") && members.contains("sp-nov:") && !members.contains("sp-f:"), "{o}");
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
    assert!(t.err().contains("no verdicts") && round_phase(&t) == "ATTRIBUTING", "{}", t.err());

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
        assert!(!certified_tree_exists(&t) && round_phase(&t) == "ATTRIBUTING");
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
fn a_red_pass_the_batch_row_refuses_is_a_fault_not_a_local_red() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    t.lc.batch_event_refused.set(Some("PassRed"));
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(t.err().contains("REFUSED") && t.err().contains("IllegalTransition"), "{}", t.err());
    assert_eq!(kv_of(&t, "round")["phase"], "fault");
}

#[test]
fn certify_hands_the_batch_and_repo_to_round_vm_to_record_its_boundaries() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(t.scripts.calls.borrow().iter().any(|c| c.starts_with("round-vm ") && c.contains(&format!("round={batch}/"))), "{:?}", t.scripts.calls.borrow());
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

fn preempt_world() -> (T, String) {
    let t = round_world();
    t.git.anc.borrow_mut().remove(&("tb".to_string(), "merged-tc".to_string()));
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 4, 1, "suites".into()));
    let spira = t.s().run.join("worktree").join(".round-spira").join("spira");
    fs::create_dir_all(&spira).unwrap();
    for s in ["test-a.sh", "test-b.sh", "test-c.sh"] {
        fs::write(spira.join(s), "").unwrap();
    }
    let results = marker(&t, &batch, "results");
    fs::create_dir_all(&results).unwrap();
    fs::write(results.join("test-a.sh.result"), "ok\n").unwrap();
    fs::write(results.join("test-b.sh.result"), "red 1 2 fp p e 1\n").unwrap();
    fs::write(marker(&t, &batch, "pass"), "777").unwrap();
    t.scripts.pass_alive.set(true);
    (t, batch)
}

#[test]
fn preempting_a_running_pass_salvages_ejects_rebuilds_and_restarts() {
    let (t, batch) = preempt_world();
    let rc = t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "red on test-b.sh at 2/3", "--suites", "test-b.sh"]);
    assert_eq!(rc, 0, "{}", t.err());
    assert!(t.scripts.calls.borrow().iter().any(|c| c == "terminate 777"), "TERM to the run's handle, never a kill: {:?}", t.scripts.calls.borrow());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("round-vm")), "a pass is stopped, not re-run, by the verb");
    let results = marker(&t, &batch, "results");
    assert!(results.join("test-a.sh.result").exists() && results.join("test-b.sh.result").exists(), "the finished suites' results stay");
    assert!(t.lc.has(&format!("event batch {batch} CI_RUNNING 4 {{\"PassPreempted\":{{\"n\":1,\"done\":2,\"total\":3,\"red_suites\":[\"test-b.sh\"]}}}}")), "{:?}", t.lc.calls.borrow());
    assert!(t.lc.has(&format!("eject-member {batch} sp-b ")) && t.lib.has("bead_reopen sp-b eject-red test-b.sh"));
    let rec = kv_of(&t, "round");
    assert_eq!((rec["members"].as_str(), rec["head"].as_str(), round_phase(&t).as_str()), ("sp-a:ta sp-c:tc", "merged-tc", "OPEN"));
    assert!(t.out().contains("stopped at 2/3 suites") && t.out().contains("verified no ejected tip remains"), "{}", t.out());
    assert!(t.scripts.calls.borrow().iter().any(|c| c == &format!("restart {batch} spira")), "{:?}", t.scripts.calls.borrow());
    assert!(!marker(&t, &batch, "preempt").exists(), "the marker is spent once the pass is recorded");
}

#[test]
fn preempt_refuses_without_a_running_pass_and_names_the_phase() {
    let (t, batch) = preempt_world();
    *t.lc.batch_view.borrow_mut() = Some(("RED".into(), 4, 1, "suites".into()));
    assert_eq!(t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "x"]), 1);
    assert!(t.err().contains(&format!("round {batch} is red, with no pass running")), "{}", t.err());

    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 4, 1, "suites".into()));
    t.scripts.pass_alive.set(false);
    assert_eq!(t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "x"]), 1);
    assert!(t.err().contains("is certifying, with no pass running"), "a recorded phase without a live handle is not a running pass: {}", t.err());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("terminate") || c.starts_with("restart")));
    assert!(!t.lc.has("PassPreempted") && !t.lib.has("bead_reopen"));
}

#[test]
fn preempt_does_not_restart_while_an_ejected_tip_is_still_in_the_head() {
    let (t, batch) = preempt_world();
    t.git.ancestor("tb", "merged-tc");
    assert_eq!(t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "red"]), 1);
    assert!(t.err().contains("still holds the tip of sp-b") && t.err().contains("next pass was not started"), "{}", t.err());
    assert!(!t.scripts.calls.borrow().iter().any(|c| c.starts_with("restart")));
}

#[test]
fn preempt_names_the_stuck_pass_when_it_will_not_stop() {
    let (t, batch) = preempt_world();
    t.scripts.pass_alive.set(true);
    t.scripts.calls.borrow_mut().clear();
    t.scripts.survives_term.set(true);
    assert_eq!(t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "red"]), 1);
    assert!(t.err().contains("did not stop") && t.err().contains("not killed"), "{}", t.err());
    assert!(!marker(&t, &batch, "preempt").exists() && !t.lc.has("PassPreempted") && !t.lib.has("bead_reopen"));
}

#[test]
fn a_preempted_certify_leaves_the_record_to_the_verb() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    t.scripts.preempted_during_vm.set(true);
    assert_eq!(t.run(&["round", "certify", &batch]), crate::ops::round::PREEMPTED, "{}", t.err());
    assert!(!marker(&t, &batch, "pass").exists(), "the handle goes with the run");
    assert!(!kv_of(&t, "round").contains_key("phase"));
    assert!(!t.lc.has("PassIncomplete") && !t.lc.has("PassRed"), "{:?}", t.lc.calls.borrow());
}

const T_STAGED: &str = "4444444444444444444444444444444444444444";
const T_MOVED: &str = "5555555555555555555555555555555555555555";

fn vm_runs(t: &T) -> usize {
    t.scripts.calls.borrow().iter().filter(|c| c.starts_with("round-vm ")).count()
}

/// A round `r1` (sp-a) open and certified GREEN, with sp-b staged behind it and tested.
fn staged_behind_green() -> (T, String, String) {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let n = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &n]), 0, "{}", t.err());
    t.lc.batch_states.borrow_mut().insert(n.clone(), ("GREEN".into(), "5".into()));
    t.io.out.borrow_mut().clear();
    t.git.ancestor("ta", "merged-ta");
    t.git.ancestor("merged-ta", "merged-tb");
    t.git.trees.borrow_mut().insert("merged-tb".into(), T_STAGED.into());
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb", "--name", "spira-staged"]), 0, "{}", t.err());
    (t, n, "spira-staged".into())
}

#[test]
fn a_staged_round_is_merged_onto_the_open_rounds_head_and_moves_no_bead() {
    let (t, n, s) = staged_behind_green();
    assert!(t.lc.has(&format!("stage {s} merged-tb merged-ta sp-b:tb behind {n}")), "{:?}", t.lc.calls.borrow());
    assert!(t.git.calls.borrow().iter().any(|c| c == "merge tb spira: land sp-b"), "the merge is open's: land_subject, member tip");
    assert!(t.scripts.calls.borrow().iter().any(|c| c.contains("gate") && c.contains("spira/round-staged/spira-staged") && c.contains("off")), "the fences run on the staged head: {:?}", t.scripts.calls.borrow());
    assert!(!t.lc.has("cut ") || t.lc.calls.borrow().iter().filter(|c| c.starts_with("cut ")).count() == 1, "only the open round was cut");
    assert!(!t.lc.has("event bead sp-b"), "a staged member is not delivered");
    let rec = kv_of(&t, "round-staged");
    assert_eq!((rec["parent"].as_str(), rec["head"].as_str(), rec["base"].as_str(), rec["phase"].as_str()), (n.as_str(), "merged-tb", "merged-ta", "staged"));
    assert_eq!(t.run(&["round", "stage", "--members", "sp-c"]), 1, "one staged round at a time");
    assert!(t.err().contains("already waiting"), "{}", t.err());
    let _ = t.run(&["round", "status"]);
    assert!(t.out().contains(&format!("staged_batch={s}")), "{}", t.out());
}

#[test]
fn stage_refuses_without_an_open_round_and_skips_the_open_rounds_own_members() {
    let t = round_world();
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(t.err().contains("nothing to stage behind"), "{}", t.err());
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "stage", "--members", "sp-a:ta"]), 1);
    assert!(t.out().contains("sp-a: already a member of"), "{}", t.out());
    assert!(!t.qfile("round-staged").exists());
}

#[test]
fn fences_red_on_the_staged_head_stage_nothing() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    t.scripts.gate_rc.set(1);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(t.err().contains("fences are red"), "{}", t.err());
    assert!(!t.qfile("round-staged").exists() && !t.lc.has("stage "), "a red fence leaves no staged round");
}

#[test]
fn the_staged_round_is_tested_only_once_the_round_ahead_is_green() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "stage-test"]), 1);
    assert!(t.err().contains("not green"), "{}", t.err());
    assert_eq!(vm_runs(&t), 0);
}

#[test]
fn promotion_on_an_identical_tree_reuses_the_staged_pass() {
    let (t, n, s) = staged_behind_green();
    assert_eq!(t.run(&["round", "stage-test"]), 0, "{}", t.err());
    assert_eq!(vm_runs(&t), 2, "the open round's pass and the staged round's pass");
    let rec = kv_of(&t, "round-staged");
    assert_eq!((rec["phase"].as_str(), rec["tested_tree"].as_str()), ("green", T_STAGED));

    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states.borrow_mut().insert(n.clone(), ("LANDED".into(), "9".into()));
    t.lc.batch_states.borrow_mut().insert(s.clone(), ("CI_RUNNING".into(), "2".into()));
    t.io.out.borrow_mut().clear();
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(t.lc.has(&format!("promote {s} merged-tb merged-ta")), "base is the landed head: {:?}", t.lc.calls.borrow());
    assert_eq!(vm_runs(&t), 2, "promotion ran no pass of its own");
    assert!(t.out().contains("attested=1"), "{}", t.out());
    assert!(t.lc.calls.borrow().iter().any(|c| c.contains(&format!("event batch {s}")) && c.contains("PassGreen")), "the tested pass is recorded on the promoted round: {:?}", t.lc.calls.borrow());
    assert!(gate::cert::path(&t.s().run.join("verdicts"), "spira", T_STAGED).is_some_and(|p| p.exists()), "the tested tree is certified");
    let rec = kv_of(&t, "round");
    assert_eq!((rec["batch_id"].as_str(), rec["members"].as_str()), (s.as_str(), "sp-b:tb"));
    assert!(!rec.contains_key("phase"));
    assert!(!t.qfile("round-staged").exists());
    t.lc.batch_states.borrow_mut().insert(s.clone(), ("GREEN".into(), "3".into()));
    assert_eq!(t.run(&["round", "land", &s]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

#[test]
fn promotion_onto_a_moved_base_gets_a_fresh_pass() {
    let (t, n, s) = staged_behind_green();
    assert_eq!(t.run(&["round", "stage-test"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states.borrow_mut().insert(n.clone(), ("LANDED".into(), "9".into()));
    t.git.set("refs/heads/local/main", "moved");
    t.git.trees.borrow_mut().insert("merged-tb".into(), T_MOVED.into());
    let merges = t.git.calls.borrow().iter().filter(|c| c.starts_with("merge ")).count();
    t.io.out.borrow_mut().clear();
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(t.git.calls.borrow().iter().filter(|c| c.starts_with("merge ")).count() > merges, "the members are merged again onto the moved base");
    assert!(t.lc.has(&format!("promote {s} merged-tb moved")), "{:?}", t.lc.calls.borrow());
    assert!(t.out().contains("attested=0"), "{}", t.out());
    assert!(!t.lc.calls.borrow().iter().any(|c| c.contains(&format!("event batch {s}")) && c.contains("PassGreen")), "no pass is recorded without a run");
    assert!(!kv_of(&t, "round").contains_key("phase"));
    assert!(!gate::cert::path(&t.s().run.join("verdicts"), "spira", T_MOVED).is_some_and(|p| p.exists()));
}

#[test]
fn a_staged_round_whose_pass_was_red_is_not_attested() {
    let (t, n, s) = staged_behind_green();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(t.run(&["round", "stage-test"]), 1);
    assert_eq!(kv_of(&t, "round-staged")["phase"], "red");
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states.borrow_mut().insert(n, ("LANDED".into(), "9".into()));
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(t.out().contains("attested=0") && t.lc.has(&format!("promote {s} ")), "{}", t.out());
}

#[test]
fn a_red_round_discards_the_stage_behind_it() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let n = batch_of(&t);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb", "--name", "spira-staged"]), 0, "{}", t.err());
    t.lc.batch_states.borrow_mut().insert("spira-staged".into(), ("STAGED".into(), "0".into()));
    t.git.set("HEAD", "merged-ta");
    assert_eq!(t.run(&["round", "certify", &n]), 1);
    assert!(t.lc.has("event batch spira-staged STAGED 0 {\"Discard\":{\"reason\":\"round spira-20260929T010203Z went red: test-a.sh\"}}"), "{:?}", t.lc.calls.borrow());
    assert!(!t.qfile("round-staged").exists(), "the staged record is gone");
    assert!(t.landing_log().contains("QUEUE ROUND-DISCARD") && t.landing_log().contains("batch=spira-staged"));
    assert!(!t.lc.has("event bead sp-b"), "discarding returns no member: none was delivered");
}

#[test]
fn an_abandoned_round_discards_the_stage_behind_it() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let n = batch_of(&t);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb", "--name", "spira-staged"]), 0, "{}", t.err());
    t.lc.batch_states.borrow_mut().insert("spira-staged".into(), ("STAGED".into(), "0".into()));
    assert_eq!(t.run(&["round", "abandon", &n, "--reason", "gave up"]), 0, "{}", t.err());
    assert!(t.lc.calls.borrow().iter().any(|c| c.starts_with("event batch spira-staged STAGED 0") && c.contains("Discard") && c.contains("gave up")), "{:?}", t.lc.calls.borrow());
    assert!(!t.qfile("round-staged").exists());
}

#[test]
fn promote_discards_when_the_round_it_followed_did_not_land() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states.borrow_mut().insert(s.clone(), ("STAGED".into(), "0".into()));
    let _ = fs::remove_file(t.qfile("round"));
    t.lc.batch_states.borrow_mut().insert(n, ("ABANDONED".into(), "5".into()));
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(t.err().contains("is ABANDONED, not LANDED") && t.err().contains("discarded"), "{}", t.err());
    assert!(!t.lc.has("promote ") && !t.qfile("round-staged").exists());
}

#[test]
fn promote_waits_while_the_round_ahead_is_still_open() {
    let (t, _, _) = staged_behind_green();
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(t.err().contains("still open"), "{}", t.err());
    assert!(t.qfile("round-staged").exists() && !t.lc.has("promote "), "the stage is kept");
}

#[test]
fn promote_discards_when_a_staged_member_moved() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states.borrow_mut().insert(s, ("STAGED".into(), "0".into()));
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states.borrow_mut().insert(n, ("LANDED".into(), "9".into()));
    for r in t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut().filter(|r| r.bead_id == "sp-b") {
        r.tip = Some("tb2".into());
    }
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(t.err().contains("no longer admissible"), "{}", t.err());
    assert!(!t.qfile("round-staged").exists() && !t.lc.has("promote "));
}

#[test]
fn round_open_discards_a_stage_whose_round_is_gone() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states.borrow_mut().insert(s.clone(), ("STAGED".into(), "0".into()));
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "open", "--members", "sp-c:tc"]), 0, "{}", t.err());
    assert!(!t.qfile("round-staged").exists());
    assert!(t.lc.calls.borrow().iter().any(|c| c.starts_with(&format!("event batch {s} STAGED 0")) && c.contains("Discard")), "{:?}", t.lc.calls.borrow());
}
