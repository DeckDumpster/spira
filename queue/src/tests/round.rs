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
    fs::read_to_string(t.qfile(name))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn round_phase(t: &T) -> String {
    let rec = kv_of(t, "round");
    assert!(
        !rec.contains_key("phase") && !rec.contains_key("phase_at"),
        "the round file carries no phase: {rec:?}"
    );
    t.lc.batch_state(&rec["batch_id"]).unwrap().0
}

fn marker(t: &T, batch: &str, ext: &str) -> PathBuf {
    t.s().run.join("rounds").join(format!("{batch}.{ext}"))
}

fn batch_of(t: &T) -> String {
    t.out()
        .lines()
        .find_map(|l| l.strip_prefix("batch="))
        .unwrap()
        .to_string()
}

fn certified_tree_exists(t: &T) -> bool {
    gate::cert::path(&t.s().run.join("verdicts"), "spira", T_ROUND).is_some_and(|p| p.exists())
}

#[test]
fn open_certify_land_is_one_round_the_whole_way() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok 3 1 fp p e 0".into()),
        ("test-b.sh".into(), "skip".into()),
    ];

    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(batch, "spira-20260929T010203Z");
    assert!(
        t.out().contains("head=merged-tb") && t.out().contains("members=sp-a:ta sp-b:tb"),
        "{}",
        t.out()
    );
    assert!(
        t.lc.has(&format!("cut {batch} sp-a:ta,sp-b:tb")),
        "spira-lc cut records the batch and moves the members"
    );
    let rec = kv_of(&t, "round");
    assert_eq!(
        (
            round_phase(&t).as_str(),
            rec["head"].as_str(),
            rec["base"].as_str()
        ),
        ("OPEN", "merged-tb", "b0")
    );
    assert!(
        marker(&t, &batch, "running").exists(),
        "round-duty sees a round in flight"
    );

    let out = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0);
    let status = t.out()[out..].to_string();
    assert!(
        status.contains("round=open")
            && status.contains(&format!("batch_id={batch}"))
            && status.contains("phase=opened"),
        "{status}"
    );
    assert!(status.contains("wall_secs=0"), "{status}");

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.contains("wall=900") && c.contains(&format!("round={batch}/spira"))),
        "the corpus runs under the configured cap and reports each red to its round: {:?}",
        t.scripts.calls.borrow()
    );
    assert!(t.lc.has(&format!("event batch {batch} CI_RUNNING 3 {{\"PassGreen\":{{\"n\":1,\"suites_s\":0,\"build_s\":0}}}}")), "{:?}", t.lc.calls.borrow());
    assert!(
        certified_tree_exists(&t),
        "the round GREEN certificate is on the head's tree"
    );
    assert_eq!(round_phase(&t), "GREEN");

    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
    assert!(
        t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"")
            && t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\"")
    );
    assert!(
        t.lc.has(&format!("land {batch} 4 merged-tb")),
        "spira-lc lands the batch"
    );
    assert!(!t.qfile("round").exists() && !marker(&t, &batch, "running").exists());
    let result = fs::read_to_string(marker(&t, &batch, "result")).unwrap();
    assert!(
        result.starts_with("landed merged-tb members=sp-a:ta,sp-b:tb"),
        "{result}"
    );
}

#[test]
fn land_with_the_batch_record_refused_prints_the_reason_and_exits_nonzero() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    t.lc.land_refused.set(Some("stale version 4"));
    assert_eq!(t.run(&["round", "land", &batch]), 1, "{}", t.err());
    let e = t.err();
    assert!(
        e.contains(&batch) && e.contains("stale version 4") && e.contains("ALARM"),
        "{e}"
    );
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("merged-tb"),
        "the release is live regardless"
    );
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("mail-operator") && c.contains(&batch)),
        "the operator is mailed, not only the log"
    );
    assert!(
        marker(&t, &batch, "unrecorded").exists(),
        "a marker is left for the later pass to reconcile"
    );
}

fn seed_suite(t: &T, name: &str) {
    let dir = PathBuf::from(&kv_of(t, "round")["worktree"]).join("spira");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), "").unwrap();
}

#[test]
fn a_red_round_eject_naming_no_suite_is_refused_and_one_naming_a_suite_records_it() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    let before = kv_of(&t, "round");
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red"]),
        2
    );
    assert!(t.err().contains("names no failing check"), "{}", t.err());
    assert_eq!(kv_of(&t, "round"), before);
    assert!(!t.lib.has("bead_reopen") && !t.lc.has("eject-member"));
    assert_eq!(
        t.run(&[
            "round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "test-b.sh"
        ]),
        0,
        "{}",
        t.err()
    );
}

#[test]
fn an_eject_forcing_a_suite_testenv_does_not_know_is_refused_naming_the_known_ones_and_changes_nothing(
) {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    let before = kv_of(&t, "round");
    assert_eq!(
        t.run(&[
            "round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "round-vm"
        ]),
        1
    );
    let e = t.err();
    assert!(e.contains("round steps: workspace-build unit-tests"), "{e}");
    assert!(
        e.contains("round-vm") && e.contains("does not know") && e.contains("test-b.sh"),
        "{e}"
    );
    assert_eq!(kv_of(&t, "round"), before);
    assert!(
        !t.lib.has("bead_reopen") && !t.lc.has("eject-member"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red",
            "--red",
            "--suites",
            "test-b.sh"
        ]),
        0,
        "positive control: {}",
        t.err()
    );
}

#[test]
fn clearing_forced_suites_removes_the_proof_and_the_sidecar_and_is_gated_on_who_asks() {
    let t = round_world();
    let run = t.s().run.clone();
    spira_config::proof::record_failing(&run, "sp-a", &["round-vm".to_string()]).unwrap();
    fs::create_dir_all(run.join("ejected")).unwrap();
    fs::write(run.join("ejected/sp-a"), "round-vm").unwrap();
    assert_eq!(t.run(&["forced-suites", "sp-a"]), 0);
    assert!(t.out().contains("round-vm"), "{}", t.out());
    assert_eq!(
        t.run(&[
            "forced-suites",
            "sp-a",
            "--clear",
            "--by=aeon",
            "--reason=x"
        ]),
        1
    );
    assert_eq!(
        spira_config::proof::failing(&run, "sp-a"),
        vec!["round-vm".to_string()],
        "a refused clear changes nothing"
    );
    assert_eq!(
        t.run(&[
            "forced-suites",
            "sp-a",
            "--clear",
            "--by=concierge",
            "--reason=round-vm is not a suite"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        spira_config::proof::failing(&run, "sp-a").is_empty() && !run.join("ejected/sp-a").exists()
    );
}

fn certified_round() -> (T, String) {
    let mut t = round_world();
    t.lib.s.home_repo = "spira".into();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    fs::create_dir_all(wt.join("target/release")).unwrap();
    testkit::write_exe(wt.join("target/release/release"), "#!/bin/sh\n");
    release_in_force(&t);
    (t, batch)
}

#[test]
fn land_rides_out_a_store_restart_after_activation_and_records_the_batch() {
    let (t, batch) = certified_round();
    t.lc.down_for.set(5);
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert!(
        t.lc.has(&format!("land {batch} 4 merged-tb")),
        "{:?}",
        t.lc.calls.borrow()
    );
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
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("merged-tb"),
        "the release is live regardless"
    );
}

#[test]
fn eject_then_land_rebuilds_the_head_without_the_member() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(
        t.out().contains("RED") && t.out().contains("red=test-b.sh"),
        "{}",
        t.out()
    );
    assert_eq!(round_phase(&t), "ATTRIBUTING");
    assert!(!certified_tree_exists(&t), "a red round certifies nothing");
    assert_eq!(
        t.run(&["round", "land", &batch]),
        1,
        "a red round does not land"
    );
    assert!(t.err().contains("is attributing, not green"), "{}", t.err());

    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red on test-b.sh",
            "--suites",
            "test-b.sh"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_reopen sp-b eject-red test-b.sh"));
    assert!(
        t.lc.has("event bead sp-b IN_DELIVERY 4 {\"Returned\":{\"reason\":\"batch-ejected\"}}")
            || t.lc.has("event bead sp-b CERTIFIED 3 \"Deliver\"")
    );
    assert!(t.lc.has(&format!(
        "eject-member {batch} sp-b ATTRIBUTING 5 red on test-b.sh"
    )));
    let rec = kv_of(&t, "round");
    assert_eq!(
        (
            rec["members"].as_str(),
            rec["head"].as_str(),
            round_phase(&t).as_str()
        ),
        ("sp-a:ta sp-c:tc", "merged-tc", "OPEN")
    );
    assert_eq!(rec["ejected"].trim(), "sp-b");
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("merge "))
            .count()
            >= 5,
        "the survivors are merged again onto the base"
    );

    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "ok".into()),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tc"));
    assert!(
        t.lc.has("event bead sp-a CERTIFIED 3 \"Deliver\"")
            && t.lc.has("event bead sp-c CERTIFIED 3 \"Deliver\"")
    );
    assert!(t.lc.has(&format!("land {batch} 9 merged-tc")));
}

#[test]
fn a_refused_lifecycle_eject_changes_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let before = kv_of(&t, "round");
    t.lc.eject_refused.set(true);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "late"]),
        1
    );
    assert!(t.err().contains("nothing changed"), "{}", t.err());
    assert_eq!(
        kv_of(&t, "round"),
        before,
        "the git head and member list stay as they were"
    );
    assert!(
        !t.lib.has("bead_reopen"),
        "the bead is not reopened for an eject the machine refused"
    );
}

#[test]
fn an_eject_whose_lifecycle_write_is_locked_exits_nonzero_naming_the_lock() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lc.event_refused.set(Some("database is locked"));
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red"]),
        1
    );
    let e = t.err();
    assert!(
        e.contains("database is locked")
            && e.contains("sp-b")
            && e.contains("did not return to REWORK"),
        "{e}"
    );
}

#[test]
fn a_successful_eject_reports_the_round_and_the_remaining_count() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out().contains(&format!(
            "ejected sp-b from round {batch}; 2 member(s) remain"
        )),
        "{}",
        t.out()
    );
}

#[test]
fn ejecting_the_last_member_closes_the_round() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-a", "--reason", "harness"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_reopen sp-a eject "));
    assert!(t
        .lc
        .has(&format!("abandon-batch {batch} OPEN 1 round emptied")));
    assert!(!t.qfile("round").exists() && !marker(&t, &batch, "running").exists());
}

#[test]
fn a_harness_fault_eject_naming_suites_is_not_charged() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "round VM lacked the config delta",
            "--suites",
            "test-b.sh",
            "--harness-fault"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lib.has("bead_reopen sp-b eject test-b.sh") && !t.lib.has("bead_reopen sp-b eject-red")
    );
}

#[test]
fn abandon_returns_the_members_and_closes_the_round() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(batch_of(&t), "hand-1");
    assert_eq!(
        t.run(&["round", "abandon", "hand-1"]),
        2,
        "a reason is required"
    );
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lc.has("abandon-batch hand-1 OPEN 1 base moved"));
    assert!(!t.qfile("round").exists() && !marker(&t, "hand-1", "running").exists());
    assert!(fs::read_to_string(marker(&t, "hand-1", "result"))
        .unwrap()
        .starts_with("abandoned: base moved"));
    assert!(!t.lc.has("land "), "an abandoned round lands nothing");
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test]
fn ending_a_round_retires_the_mender_beads_filed_for_it() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-2"
        ]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "hand-2", "mender-1-gate-x"), "sp-m9\n").unwrap();
    assert_eq!(
        t.run(&["round", "eject", "hand-2", "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_close sp-m9"), "{:?}", t.lib.calls.borrow());
    assert!(marker(&t, "hand-2", "mender-1-gate-x.retired").exists());
    fs::write(marker(&t, "hand-2", "mender-2-gate-y"), "sp-m10\n").unwrap();
    assert_eq!(
        t.run(&["round", "abandon", "hand-2", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lib.has("bead_close sp-m10"), "{:?}", t.lib.calls.borrow());
}

#[test]
fn a_mender_bead_holding_an_ask_is_not_retired_until_the_ask_is_lifted() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb", "--name", "hand-4"]), 0, "{}", t.err());
    fs::write(marker(&t, "hand-4", "mender-1-gate-x"), "sp-m13\n").unwrap();
    t.lc.rows.borrow_mut().as_mut().unwrap().push(crate::model::LcBeadRow {
        bead_id: "sp-m13".into(),
        state: "WORKING".into(),
        holds: vec!["ask".into()],
        ..Default::default()
    });
    assert_eq!(t.run(&["round", "eject", "hand-4", "sp-b", "--reason", "red"]), 0, "{}", t.err());
    assert!(!t.lib.has("bead_close sp-m13"), "{:?}", t.lib.calls.borrow());
    assert!(!marker(&t, "hand-4", "mender-1-gate-x.retired").exists());
    t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut().for_each(|r| r.holds.clear());
    assert_eq!(t.run(&["round", "abandon", "hand-4", "--reason", "base moved"]), 0, "{}", t.err());
    assert!(t.lib.has("bead_close sp-m13"), "{:?}", t.lib.calls.borrow());
    assert!(marker(&t, "hand-4", "mender-1-gate-x.retired").exists());
}

#[test]
fn a_later_round_event_retires_the_mender_beads_of_batches_already_closed() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-3"
        ]),
        0,
        "{}",
        t.err()
    );
    fs::write(marker(&t, "old-1", "mender-1-gate-x"), "sp-m11\n").unwrap();
    fs::write(marker(&t, "live-1", "mender-1-gate-y"), "sp-m12\n").unwrap();
    t.lc.batch_states
        .borrow_mut()
        .insert("old-1".into(), ("ABANDONED".into(), "2".into()));
    t.lib.close_fail.set(true);
    assert_eq!(
        t.run(&["round", "eject", "hand-3", "sp-b", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("sp-m11") && t.err().contains("refused"),
        "{}",
        t.err()
    );
    assert!(!marker(&t, "old-1", "mender-1-gate-x.retired").exists());
    t.lib.close_fail.set(false);
    assert_eq!(
        t.run(&["round", "abandon", "hand-3", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(marker(&t, "old-1", "mender-1-gate-x.retired").exists());
    assert!(!marker(&t, "live-1", "mender-1-gate-y.retired").exists());
}

#[test]
fn abandon_of_a_batch_the_lifecycle_already_abandoned_drops_the_record() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta", "--name", "hand-1"]),
        0,
        "{}",
        t.err()
    );
    t.lc.batch_states
        .borrow_mut()
        .insert("hand-1".into(), ("ABANDONED".into(), "9".into()));
    assert_eq!(t.run(&["round", "status"]), 0, "{}", t.err());
    assert!(t.out().contains("round=none"), "{}", t.out());
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "retry"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.qfile("round").exists() && !marker(&t, "hand-1", "running").exists());
    assert!(fs::read_to_string(marker(&t, "hand-1", "result"))
        .unwrap()
        .starts_with("abandoned: retry"));
}

#[test]
fn abandon_requeues_a_member_the_batch_abandon_left_in_delivery() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-a")
    {
        r.state = "IN_DELIVERY".into();
    }
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has("event bead sp-a IN_DELIVERY 7 {\"Requeued\":{\"tip\":\"ta\"}}"),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        !t.lc.has("event bead sp-b"),
        "a member not in delivery is left alone"
    );
}

#[test]
fn abandon_rereads_and_retries_a_requeue_refused_for_a_stale_version() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    for r in t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut() {
        r.state = "IN_DELIVERY".into();
    }
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-b".into(), ("IN_DELIVERY".into(), "7".into()));
    *t.lc.event_races.borrow_mut() = Some(("IN_DELIVERY".into(), "8".into()));
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|c| c == "event bead sp-a IN_DELIVERY 7 {\"Requeued\":{\"tip\":\"ta\"}}"),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c == "event bead sp-a IN_DELIVERY 8 {\"Requeued\":{\"tip\":\"ta\"}}"),
        "{calls:?}"
    );
}

#[test]
fn abandon_accepts_a_member_another_writer_moved_out_of_delivery() {
    let t = round_world();
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a:ta,sp-b:tb",
            "--name",
            "hand-1"
        ]),
        0,
        "{}",
        t.err()
    );
    for r in t.lc.rows.borrow_mut().as_mut().unwrap().iter_mut() {
        r.state = "IN_DELIVERY".into();
    }
    t.lc.bead_rows
        .borrow_mut()
        .insert("sp-a".into(), ("IN_DELIVERY".into(), "7".into()));
    *t.lc.event_races.borrow_mut() = Some(("REWORK".into(), "8".into()));
    assert_eq!(
        t.run(&["round", "abandon", "hand-1", "--reason", "base moved"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("event bead sp-a"))
            .count(),
        1
    );
    assert!(t.out().contains("sp-a: REWORK"), "{}", t.out());
}

#[test]
fn open_skips_what_is_not_certified_or_does_not_merge() {
    let t = round_world();
    t.lc_row("sp-d", "REWORK", "td", 100);
    t.git.merge_fail.borrow_mut().insert("tb".into());
    t.git
        .rebase_fail
        .borrow_mut()
        .insert("tb".into(), vec!["list.txt".into()]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-d,sp-zz"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("sp-b: conflicts with the round")
            && o.contains("sp-d: lifecycle state=REWORK")
            && o.contains("sp-zz: no lifecycle row"),
        "{o}"
    );
    assert!(o.contains("members=sp-a:ta"), "{o}");

    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("ta".into());
    t.git.rebase_fail.borrow_mut().insert("ta".into(), vec![]);
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 1);
    assert!(
        t.err().contains("nothing admissible") && !t.lc.has("cut") && !t.qfile("round").exists()
    );
}

#[test]
fn open_rebases_a_member_that_conflicts_with_the_round_onto_its_head() {
    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("tc".into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("members=sp-a:ta sp-b:tb sp-c:rebased-tc") && !o.contains("skip"),
        "{o}"
    );
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c == "rebase-onto tc merged-tb"),
        "{:?}",
        t.git.calls.borrow()
    );
    assert!(t.lc.has("cut"), "{:?}", t.lc.calls.borrow());
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("cut ") && c.ends_with("sp-c:tc") && !c.contains("rebased")),
        "the cut names the certified tip, not the rebased sha: {:?}",
        t.lc.calls.borrow()
    );
    assert!(
        !t.lc.has("event bead sp-c"),
        "a rebased member is not returned to rework"
    );
}

fn block(t: &T, id: &str, by: &[&str]) {
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == id)
    {
        r.blocked_by = by.iter().map(|b| b.to_string()).collect();
    }
}

#[test]
fn open_skips_a_member_whose_blocker_has_not_landed_and_names_it() {
    let t = round_world();
    block(&t, "sp-b", &["sp-eeg"]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("skip — sp-b: depends on sp-eeg (")
            && o.contains("members=sp-a:ta")
            && !o.contains("sp-b:tb"),
        "{o}"
    );
}

#[test]
fn open_skips_a_member_whose_blocker_is_only_a_co_member_of_the_round() {
    let t = round_world();
    block(&t, "sp-b", &["sp-a"]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("skip — sp-b: depends on sp-a (") && !o.contains("sp-b:tb"),
        "{o}"
    );
}

#[test]
fn open_admits_a_member_whose_blocker_has_landed() {
    let t = round_world();
    block(&t, "sp-b", &[]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("members=sp-a:ta sp-b:tb"), "{}", t.out());
}

#[test]
fn open_returns_a_base_conflict_to_rework_and_keeps_a_round_conflict_queued() {
    let t = round_world();
    t.git.merge_fail.borrow_mut().insert("tb".into());
    t.lib.conflict_with_base.borrow_mut().insert("tb".into());
    t.git.merge_fail.borrow_mut().insert("tc".into());
    t.git
        .rebase_fail
        .borrow_mut()
        .insert("tc".into(), vec!["c.txt".into(), "d.txt".into()]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(o.contains("sp-b: conflicts with base — returned to rework") && o.contains("sp-c: conflicts with the round, and with a rebase onto its head — paths: c.txt d.txt\n"), "{o}");
    assert!(
        t.lc.has(
            "event bead sp-b CERTIFIED 3 {\"GateRed\":{\"tip\":\"tb\",\"reason\":\"no-rebase\"}}"
        ),
        "{:?}",
        t.lc.calls.borrow()
    );
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
        assert_eq!(
            t.run(&["round", "open", "--members", "sp-a,sp-b"]),
            1,
            "ignored={ignored}: {}",
            t.err()
        );
        assert!(
            !t.out().contains("returned to rework")
                && t.out().contains("sp-b: conflicts with base ("),
            "{}",
            t.out()
        );
        assert!(
            t.err().contains("could not be returned to rework")
                && !t.lc.has("cut")
                && !t.qfile("round").exists(),
            "{}",
            t.err()
        );
    }
}

#[test]
fn open_whose_record_cannot_be_written_abandons_the_batch_it_cut() {
    let t = round_world();
    fs::create_dir_all(t.qfile("round")).unwrap();
    fs::create_dir_all(t.qfile("round.r-x")).unwrap();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b", "--name", "r-x"]),
        1,
        "{}",
        t.err()
    );
    let calls = t.lc.calls.borrow().clone();
    assert!(
        calls.iter().any(|c| c.starts_with("cut ")),
        "POSITIVE CONTROL: the cut ran first: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("abandon-batch ") && c.contains("OPEN")),
        "the cut batch is released: {calls:?}"
    );
}

#[test]
fn open_whose_cut_answers_a_deadline_after_committing_is_a_round_not_a_failure() {
    let t = round_world();
    t.lc.deadline_after_write.set(Some("cut"));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("batch="), "{}", t.out());
    assert!(!t.lc.has("requeue-orphans"));
}

#[test]
fn open_whose_cut_is_refused_sweeps_orphans() {
    let t = round_world();
    t.lc.cut_refused.set(Some("refused"));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        1,
        "{}",
        t.err()
    );
    assert!(t.lc.has("requeue-orphans"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn open_admits_a_submitted_member_its_full_suite_certifies() {
    // law-a-round-is-feature-first-then-catch-all: a round takes SUBMITTED beads directly.
    let t = round_world();
    t.lc_row("sp-s", "SUBMITTED", "ts", 100);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-s"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("sp-s:ts") && !o.contains("sp-s: lifecycle state"),
        "{o}"
    );
    assert!(t.lc.has("cut"), "the round is recorded as a batch");
}

pub(super) fn file_gate_verdict(t: &T, id: &str, tip: &str, rc: i32) {
    use landing_pass::gateq::{Done, GateQueue, Job};
    let job = Job::new("spira", &format!("spira/{id}"), id, tip, false);
    let run = landing_pass::model::GateRun::parse(
        rc,
        "gate: VERDICT=FAIL reason=branch-red suite=test-x.sh".into(),
    );
    GateQueue::new(&t.s().run)
        .complete(
            0,
            &Done {
                job,
                run,
                started_ms: 1,
                finished_ms: 2,
            },
        )
        .unwrap();
}

#[test]
fn open_skips_a_member_at_a_tip_that_already_went_red_in_a_batch() {
    let t = round_world();
    t.lc_row("sp-r", "CERTIFIED", "tsp-r", 100);
    std::fs::create_dir_all(t.qfile("red-tips").parent().unwrap()).unwrap();
    std::fs::write(t.qfile("red-tips"), "sp-r tsp-r\n").unwrap();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-r"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(o.contains("sp-r: tsp-r already went red in a batch"), "{o}");
}

#[test]
fn open_skips_a_submitted_member_whose_gate_is_fail_at_its_current_tip_and_only_that() {
    let t = round_world();
    for id in ["sp-f", "sp-old", "sp-base", "sp-nov"] {
        t.lc_row(id, "SUBMITTED", &format!("t{id}"), 100);
    }
    file_gate_verdict(&t, "sp-f", "tsp-f", 1);
    file_gate_verdict(&t, "sp-old", "an-older-tip", 1);
    file_gate_verdict(
        &t,
        "sp-base",
        "tsp-base",
        landing_pass::model::GATE_BASEFAIL,
    );
    file_gate_verdict(&t, "sp-nov", "tsp-nov", landing_pass::model::GATE_NOVERDICT);
    assert_eq!(
        t.run(&[
            "round",
            "open",
            "--members",
            "sp-a,sp-f,sp-old,sp-base,sp-nov"
        ]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("sp-f: its gate is FAIL at tsp-f (branch-red)"),
        "{o}"
    );
    let members = o.lines().find_map(|l| l.strip_prefix("members=")).unwrap();
    assert!(
        members.contains("sp-a:")
            && members.contains("sp-old:")
            && members.contains("sp-base:")
            && members.contains("sp-nov:")
            && !members.contains("sp-f:"),
        "{o}"
    );
}

#[test]
fn a_second_round_is_refused_until_the_first_is_closed() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "open", "--members", "sp-b"]), 1);
    assert!(
        t.err().contains("a round is already open") && t.err().contains("phase opened"),
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "land", "someone-else"]), 1);
    assert!(
        t.err()
            .contains("is spira-20260929T010203Z, not someone-else"),
        "{}",
        t.err()
    );
}

#[test]
fn certify_never_reads_a_harness_fault_as_green_or_red() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);

    assert_eq!(
        t.run(&["round", "certify", &batch]),
        4,
        "no results at all is a fault"
    );
    assert!(
        t.err().contains("no verdicts") && round_phase(&t) == "ATTRIBUTING",
        "{}",
        t.err()
    );

    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 124,
        out: String::new(),
        err: "killed".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.err()
            .contains("hung past its guard (guard=hang-guard elapsed="),
        "{}",
        t.err()
    );
    assert!(
        !certified_tree_exists(&t)
            && !t
                .lc
                .has("event batch spira-20260929T010203Z CI_RUNNING 4 {\"PassGreen\"")
    );
}

#[test]
fn a_round_vm_exit_names_the_failure_the_vm_logged() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let err = "  ok      tmux\n  FAIL    terraform — not found on PATH and not waived\n23 ok, 1 FAIL\nround-vm: IMAGE-BUILD-FAILED: img could not be built on this VM\n";
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 3,
        out: String::new(),
        err: err.into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.err()
            .contains("round-vm exited 3: round-vm: IMAGE-BUILD-FAILED: img could not be built"),
        "{}",
        t.err()
    );
}

#[test]
fn a_pass_whose_tests_overran_the_cap_is_a_fault_even_with_every_verdict_in() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let err = "round-vm: OVER-CAP: guard=suites-cap elapsed=901s cap=900s — the suites and bare lane ran past the cap\nround-vm: UNIT-TESTS: PASS (3 test binaries)";
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 124,
        out: String::new(),
        err: err.into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    assert!(
        t.err()
            .contains("INCOMPLETE, cut off by guard=suites-cap elapsed=901s"),
        "{}",
        t.err()
    );
    assert!(!t
        .lc
        .has("event batch spira-20260929T010203Z CI_RUNNING 4 {\"PassGreen\""));
}

#[test]
fn certify_faults_on_an_unreadable_or_missing_suite_verdict() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let spira = t
        .s()
        .run
        .join("worktree")
        .join(".round-spira")
        .join("spira");
    fs::create_dir_all(&spira).unwrap();
    for s in ["test-a.sh", "test-b.sh"] {
        fs::write(spira.join(s), "").unwrap();
    }
    let ok = |s: &str| (s.to_string(), "ok".to_string());
    for (results, want) in [
        (
            vec![ok("test-a.sh"), ("test-b.sh".into(), "".into())],
            "test-b.sh (verdict \"\")",
        ),
        (
            vec![ok("test-a.sh"), ("test-b.sh".into(), "garbage".into())],
            "test-b.sh (verdict \"garbage\")",
        ),
        (vec![ok("test-a.sh")], "test-b.sh (no result)"),
    ] {
        *t.scripts.round_vm_results.borrow_mut() = results;
        assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
        assert!(
            t.err().contains(want) && t.err().contains("not judged"),
            "{}",
            t.err()
        );
        assert!(!certified_tree_exists(&t) && round_phase(&t) == "ATTRIBUTING");
    }
    *t.scripts.round_vm_results.borrow_mut() =
        vec![ok("test-a.sh"), ("test-b.sh".into(), "skip".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(certified_tree_exists(&t));
}

#[test]
fn the_blocking_only_reader_passed_what_unjudgeable_now_refuses() {
    let found = vec![
        ("test-a.sh".to_string(), String::new()),
        ("test-b.sh".to_string(), "garbage".to_string()),
    ];
    let old_reds: Vec<_> = found
        .iter()
        .filter(|(_, s)| super::super::ops::round::BLOCKING.contains(&s.as_str()))
        .collect();
    assert!(
        old_reds.is_empty(),
        "control: the old reader found nothing wrong"
    );
    assert!(super::super::ops::round::unjudgeable(Path::new("/nonexistent"), &found).is_some());
}

#[test]
fn an_attestation_of_a_corpus_that_ran_nothing_is_refused() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    for extra in [&[][..], &["--ran", "0"][..], &["--ran", "x"][..]] {
        let mut args = vec!["round", "certify", &batch, "--attest", "merged-ta"];
        args.extend_from_slice(extra);
        assert_eq!(t.run(&args), 1, "{}", t.err());
        assert!(
            t.err().contains("a vacuous pass is not a certificate"),
            "{}",
            t.err()
        );
    }
    assert!(!certified_tree_exists(&t));
}

#[test]
fn a_green_round_names_how_many_suites_it_ran() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "merged-ta",
            "--ran",
            "7"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("ran=7 wall="), "{}", t.out());
}

#[test]
fn attest_names_the_worktrees_own_head_and_nothing_else() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.git.set("refs/heads/elsewhere", "merged-ta");
    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "elsewhere",
            "--ran",
            "3"
        ]),
        1
    );
    assert!(
        t.err().contains("is not the round worktree's head"),
        "{}",
        t.err()
    );
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .all(|c| !c.starts_with("round-vm")),
        "an attestation runs no corpus"
    );

    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "merged-tb",
            "--ran",
            "3"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(certified_tree_exists(&t));
    assert!(t
        .scripts
        .calls
        .borrow()
        .iter()
        .all(|c| !c.starts_with("round-vm")));
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
    assert!(
        t.err().contains("another queue operation holds the lock"),
        "{}",
        t.err()
    );
    t.var("SPIRA_QUEUE_LOCK_HELD", "1");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
}

#[test]
fn eject_also_ejects_the_members_stacked_on_the_ejected_one() {
    let t = round_world();
    t.git
        .bases
        .borrow_mut()
        .insert(("ta".into(), "tb".into()), "own-commit-of-a".into());
    t.git
        .bases
        .borrow_mut()
        .insert(("ta".into(), "tc".into()), "b0".into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);

    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-a", "--reason", "red on lint"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out().contains("ejected sp-b from round") && t.out().contains("stacked on sp-a"),
        "{}",
        t.out()
    );
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with(&format!("eject-member {batch} sp-b "))
                && c.ends_with("stacked on sp-a")),
        "b's reason names a"
    );
    assert!(t.lib.has("bead_reopen sp-b eject "));
    let rec = kv_of(&t, "round");
    assert_eq!(
        (rec["members"].as_str(), rec["head"].as_str()),
        ("sp-c:tc", "merged-tc")
    );
    assert_eq!(rec["ejected"].trim(), "sp-a sp-b");
    assert!(
        !t.lib.has("bead_reopen sp-c eject "),
        "an independent member stays"
    );
}

fn events_of(t: &T, batch: &str) -> Vec<String> {
    let prefix = format!("event batch {batch} ");
    t.lc.calls
        .borrow()
        .iter()
        .filter_map(|c| c.strip_prefix(&prefix))
        .map(|c| c.splitn(3, ' ').nth(2).unwrap_or("").to_string())
        .collect()
}

#[test]
fn a_red_pass_the_batch_row_refuses_is_a_fault_not_a_local_red() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lc.batch_event_refused.set(Some("PassRed"));
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(
        t.err().contains("REFUSED") && t.err().contains("IllegalTransition"),
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round")["phase"], "fault");
}

#[test]
fn certify_hands_the_batch_and_repo_to_round_vm_to_record_its_boundaries() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("round-vm ") && c.contains(&format!("round={batch}/"))),
        "{:?}",
        t.scripts.calls.borrow()
    );
}

#[test]
fn a_second_certify_of_a_batch_already_being_certified_is_refused_naming_the_holder() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let g = match crate::lock::try_certify_lock(&t.s().queue_dir, "spira", &batch) {
        crate::lock::Acquire::Held(g) => g,
        _ => panic!("lock"),
    };
    let before = t.scripts.calls.borrow().len();
    assert_ne!(t.run(&["round", "certify", &batch]), 0);
    assert!(
        t.err().contains("already being certified") && t.err().contains("pid="),
        "{}",
        t.err()
    );
    assert_eq!(t.scripts.calls.borrow().len(), before, "no pass was started");
    drop(g);
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
}

#[test]
fn a_round_through_a_red_pass_an_eject_and_a_green_pass_records_both_passes() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    *t.scripts.round_vm_meta.borrow_mut() = "build_wall_s=30\n".into();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert_eq!(
        events_of(&t, &batch),
        [
            "{\"PassStarted\":{\"n\":1,\"head\":\"merged-tc\"}}",
            "{\"SuitesStarted\":{\"n\":1}}",
            "{\"Failed\":{\"suite\":\"test-b.sh\",\"n\":1}}",
            "{\"PassRed\":{\"n\":1,\"red_suites\":[\"test-b.sh\"],\"suites_s\":0,\"build_s\":30}}",
        ],
        "pass 1 is started, enters its suites and ends red, naming the suite"
    );
    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "red on test-b.sh",
            "--suites",
            "test-b.sh"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        events_of(&t, &batch)
            .last()
            .unwrap()
            .starts_with("{\"PassRebuilt\":{\"head\":"),
        "{:?}",
        events_of(&t, &batch)
    );

    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "ok".into()),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let events = events_of(&t, &batch);
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("{\"PassStarted\":") && e.contains("\"n\":2")),
        "{events:?}"
    );
    assert!(
        events
            .last()
            .unwrap()
            .starts_with("{\"PassGreen\":{\"n\":2"),
        "{events:?}"
    );
}

#[test]
fn a_pass_the_vm_could_not_finish_is_incomplete_not_red_or_green() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 124,
        out: String::new(),
        err: "killed".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    let events = events_of(&t, &batch);
    assert!(events.last().unwrap().starts_with("{\"PassIncomplete\":{\"n\":1,\"reason\":\"round-vm hung past its guard (guard=hang-guard elapsed="), "{events:?}");
    assert!(
        !events
            .iter()
            .any(|e| e.starts_with("{\"PassRed\"") || e.starts_with("{\"PassGreen\"")),
        "{events:?}"
    );
}

#[test]
fn suites_started_re_reads_and_survives_repeated_stale_versions() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    t.lc.stale_batch_events.set(3);
    assert_eq!(
        t.run(&["round", "suites-started", &batch]),
        0,
        "{}",
        t.err()
    );
    let shows =
        t.lc.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("event batch") && c.contains("SuitesStarted"))
            .count();
    assert_eq!(shows, 4, "{:?}", t.lc.calls.borrow());
    t.lc.stale_batch_events.set(100);
    assert_ne!(t.run(&["round", "suites-started", &batch]), 0);
}

#[test]
fn the_pass_verbs_record_a_hand_driven_pass_like_the_batchers() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "suites-started", &batch]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "round",
            "pass-verdict",
            &batch,
            "--verdict",
            "red",
            "--red-suites",
            "test-x.sh",
            "--suites-s",
            "90",
            "--build-s",
            "30"
        ]),
        0,
        "{}",
        t.err()
    );
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
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0);
    assert_eq!(
        t.run(&["round", "pass-start", &batch]),
        0,
        "the fake machine does not refuse; the verb must still have sent the event"
    );
    assert_eq!(
        events_of(&t, &batch)
            .iter()
            .filter(|e| e.starts_with("{\"PassStarted\""))
            .count(),
        2,
        "a strict verb never swallows a wrong-phase call"
    );
    assert_eq!(
        t.run(&["round", "pass-verdict", &batch, "--verdict", "incomplete"]),
        2,
        "an incomplete pass names why"
    );
}

#[test]
fn a_pass_left_open_is_closed_incomplete_so_the_next_certify_starts_cleanly() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 2, 1, "suites".into()));
    t.scripts.pass_alive.set(false);
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    t.run(&["round", "certify", &batch]);
    let events = events_of(&t, &batch);
    let at = |p: &str| events.iter().position(|e| e.starts_with(p));
    assert!(
        at("{\"PassIncomplete\":{\"n\":1").is_some()
            && at("{\"PassIncomplete\"") < at("{\"PassStarted\""),
        "{events:?}"
    );
}

fn preempt_world() -> (T, String) {
    let t = round_world();
    t.git
        .anc
        .borrow_mut()
        .remove(&("tb".to_string(), "merged-tc".to_string()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 4, 1, "suites".into()));
    let spira = t
        .s()
        .run
        .join("worktree")
        .join(".round-spira")
        .join("spira");
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
    seed_suite(&t, "test-b.sh");
    let rc = t.run(&[
        "round",
        "preempt",
        &batch,
        "--eject",
        "sp-b",
        "--reason",
        "red on test-b.sh at 2/3",
        "--suites",
        "test-b.sh",
    ]);
    assert_eq!(rc, 0, "{}", t.err());
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c == "terminate 777"),
        "TERM to the run's handle, never a kill: {:?}",
        t.scripts.calls.borrow()
    );
    assert!(
        !t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("round-vm")),
        "a pass is stopped, not re-run, by the verb"
    );
    let results = marker(&t, &batch, "results");
    assert!(
        results.join("test-a.sh.result").exists() && results.join("test-b.sh.result").exists(),
        "the finished suites' results stay"
    );
    assert!(t.lc.has(&format!("event batch {batch} CI_RUNNING 4 {{\"PassPreempted\":{{\"n\":1,\"done\":2,\"total\":3,\"red_suites\":[\"test-b.sh\"]}}}}")), "{:?}", t.lc.calls.borrow());
    assert!(
        t.lc.has(&format!("eject-member {batch} sp-b "))
            && t.lib.has("bead_reopen sp-b eject-red test-b.sh")
    );
    let rec = kv_of(&t, "round");
    assert_eq!(
        (
            rec["members"].as_str(),
            rec["head"].as_str(),
            round_phase(&t).as_str()
        ),
        ("sp-a:ta sp-c:tc", "merged-tc", "OPEN")
    );
    assert!(
        t.out().contains("stopped at 2/3 suites")
            && t.out().contains("verified no ejected tip remains"),
        "{}",
        t.out()
    );
    assert!(
        t.scripts
            .calls
            .borrow()
            .iter()
            .any(|c| c == &format!("restart {batch} spira")),
        "{:?}",
        t.scripts.calls.borrow()
    );
    assert!(
        !marker(&t, &batch, "preempt").exists(),
        "the marker is spent once the pass is recorded"
    );
}

#[test]
fn preempt_refuses_without_a_running_pass_and_names_the_phase() {
    let (t, batch) = preempt_world();
    *t.lc.batch_view.borrow_mut() = Some(("RED".into(), 4, 1, "suites".into()));
    assert_eq!(
        t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "x"]),
        1
    );
    assert!(
        t.err()
            .contains(&format!("round {batch} is red, with no pass running")),
        "{}",
        t.err()
    );

    *t.lc.batch_view.borrow_mut() = Some(("CI_RUNNING".into(), 4, 1, "suites".into()));
    t.scripts.pass_alive.set(false);
    assert_eq!(
        t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "x"]),
        1
    );
    assert!(
        t.err().contains("is certifying, with no pass running"),
        "a recorded phase without a live handle is not a running pass: {}",
        t.err()
    );
    assert!(!t
        .scripts
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("terminate") || c.starts_with("restart")));
    assert!(!t.lc.has("PassPreempted") && !t.lib.has("bead_reopen"));
}

#[test]
fn preempt_does_not_restart_while_an_ejected_tip_is_still_in_the_head() {
    let (t, batch) = preempt_world();
    t.git.ancestor("tb", "merged-tc");
    assert_eq!(
        t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "red"]),
        1
    );
    assert!(
        t.err().contains("still holds the tip of sp-b")
            && t.err().contains("next pass was not started"),
        "{}",
        t.err()
    );
    assert!(!t
        .scripts
        .calls
        .borrow()
        .iter()
        .any(|c| c.starts_with("restart")));
}

#[test]
fn preempt_names_the_stuck_pass_when_it_will_not_stop() {
    let (t, batch) = preempt_world();
    t.scripts.pass_alive.set(true);
    t.scripts.calls.borrow_mut().clear();
    t.scripts.survives_term.set(true);
    assert_eq!(
        t.run(&["round", "preempt", &batch, "--eject", "sp-b", "--reason", "red"]),
        1
    );
    assert!(
        t.err().contains("did not stop") && t.err().contains("not killed"),
        "{}",
        t.err()
    );
    assert!(
        !marker(&t, &batch, "preempt").exists()
            && !t.lc.has("PassPreempted")
            && !t.lib.has("bead_reopen")
    );
}

#[test]
fn a_preempted_certify_leaves_the_record_to_the_verb() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.scripts.preempted_during_vm.set(true);
    assert_eq!(
        t.run(&["round", "certify", &batch]),
        crate::ops::round::PREEMPTED,
        "{}",
        t.err()
    );
    assert!(
        !marker(&t, &batch, "pass").exists(),
        "the handle goes with the run"
    );
    assert!(!kv_of(&t, "round").contains_key("phase"));
    assert!(
        !t.lc.has("PassIncomplete") && !t.lc.has("PassRed"),
        "{:?}",
        t.lc.calls.borrow()
    );
}

const T_STAGED: &str = "4444444444444444444444444444444444444444";
const T_MOVED: &str = "5555555555555555555555555555555555555555";

fn vm_runs(t: &T) -> usize {
    t.scripts
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("round-vm "))
        .count()
}

/// A round `r1` (sp-a) open and certified GREEN, with sp-b staged behind it and tested.
fn staged_behind_green() -> (T, String, String) {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let n = batch_of(&t);
    assert_eq!(t.run(&["round", "certify", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n.clone(), ("GREEN".into(), "5".into()));
    t.io.out.borrow_mut().clear();
    t.git.ancestor("ta", "merged-ta");
    t.git.ancestor("merged-ta", "merged-tb");
    t.git
        .trees
        .borrow_mut()
        .insert("merged-tb".into(), T_STAGED.into());
    assert_eq!(
        t.run(&[
            "round",
            "stage",
            "--members",
            "sp-b:tb",
            "--name",
            "spira-staged"
        ]),
        0,
        "{}",
        t.err()
    );
    (t, n, "spira-staged".into())
}

#[test]
fn a_staged_round_is_merged_onto_the_open_rounds_head_and_moves_no_bead() {
    let (t, n, s) = staged_behind_green();
    assert!(
        t.lc.has(&format!("stage {s} merged-tb merged-ta sp-b:tb behind {n}")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c == "merge tb spira: land sp-b"),
        "the merge is open's: land_subject, member tip"
    );
    assert!(
        t.scripts.calls.borrow().iter().any(|c| c.contains("gate")
            && c.contains("spira/round-staged/spira-staged")
            && c.contains("off")),
        "the fences run on the staged head: {:?}",
        t.scripts.calls.borrow()
    );
    assert!(
        !t.lc.has("cut ")
            || t.lc
                .calls
                .borrow()
                .iter()
                .filter(|c| c.starts_with("cut "))
                .count()
                == 1,
        "only the open round was cut"
    );
    assert!(
        !t.lc.has("event bead sp-b"),
        "a staged member is not delivered"
    );
    let rec = kv_of(&t, "round-staged");
    assert_eq!(
        (
            rec["parent"].as_str(),
            rec["head"].as_str(),
            rec["base"].as_str(),
            rec["phase"].as_str()
        ),
        (n.as_str(), "merged-tb", "merged-ta", "staged")
    );
    assert_eq!(
        t.run(&["round", "stage", "--members", "sp-c"]),
        1,
        "one staged round at a time"
    );
    assert!(t.err().contains("already waiting"), "{}", t.err());
    let _ = t.run(&["round", "status"]);
    assert!(
        t.out().contains(&format!("staged_batch={s}")),
        "{}",
        t.out()
    );
}

#[test]
fn stage_refuses_without_an_open_round_and_skips_the_open_rounds_own_members() {
    let t = round_world();
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(t.err().contains("nothing to stage behind"), "{}", t.err());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "stage", "--members", "sp-a:ta"]), 1);
    assert!(t.out().contains("sp-a: already a member of"), "{}", t.out());
    assert!(!t.qfile("round-staged").exists());
}

#[test]
fn fences_red_on_the_staged_head_stage_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    t.scripts.gate_rc.set(1);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 1);
    assert!(t.err().contains("fences are red"), "{}", t.err());
    assert!(
        !t.qfile("round-staged").exists() && !t.lc.has("stage "),
        "a red fence leaves no staged round"
    );
}

#[test]
fn a_no_verdict_fence_is_infra_and_leaves_the_stage_retryable() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    t.scripts.gate_rc.set(75);
    assert_eq!(t.run(&["round", "stage", "--members", "sp-b:tb"]), 75);
    assert!(
        t.err().contains("infra, not red") && !t.err().contains("fences are red"),
        "{}",
        t.err()
    );
    assert!(!t.qfile("round-staged").exists() && !t.lc.has("stage "));
}

#[test]
fn the_staged_round_is_tested_only_once_the_round_ahead_is_green() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "stage", "--members", "sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.run(&["round", "stage-test"]), 1);
    assert!(t.err().contains("not green"), "{}", t.err());
    assert_eq!(vm_runs(&t), 0);
}

#[test]
fn promotion_on_an_identical_tree_reuses_the_staged_pass() {
    let (t, n, s) = staged_behind_green();
    assert_eq!(t.run(&["round", "stage-test"]), 0, "{}", t.err());
    assert_eq!(
        vm_runs(&t),
        2,
        "the open round's pass and the staged round's pass"
    );
    let rec = kv_of(&t, "round-staged");
    assert_eq!(
        (rec["phase"].as_str(), rec["tested_tree"].as_str()),
        ("green", T_STAGED)
    );

    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n.clone(), ("LANDED".into(), "9".into()));
    t.lc.batch_states
        .borrow_mut()
        .insert(s.clone(), ("CI_RUNNING".into(), "2".into()));
    t.io.out.borrow_mut().clear();
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(
        t.lc.has(&format!("promote {s} merged-tb merged-ta")),
        "base is the landed head: {:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(vm_runs(&t), 2, "promotion ran no pass of its own");
    assert!(t.out().contains("attested=1"), "{}", t.out());
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.contains(&format!("event batch {s}")) && c.contains("PassGreen")),
        "the tested pass is recorded on the promoted round: {:?}",
        t.lc.calls.borrow()
    );
    assert!(
        gate::cert::path(&t.s().run.join("verdicts"), "spira", T_STAGED)
            .is_some_and(|p| p.exists()),
        "the tested tree is certified"
    );
    let rec = kv_of(&t, "round");
    assert_eq!(
        (rec["batch_id"].as_str(), rec["members"].as_str()),
        (s.as_str(), "sp-b:tb")
    );
    assert!(!rec.contains_key("phase"));
    assert!(!t.qfile("round-staged").exists());
    t.lc.batch_states
        .borrow_mut()
        .insert(s.clone(), ("GREEN".into(), "3".into()));
    assert_eq!(t.run(&["round", "land", &s]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

#[test]
fn promotion_onto_a_moved_base_gets_a_fresh_pass() {
    let (t, n, s) = staged_behind_green();
    assert_eq!(t.run(&["round", "stage-test"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n.clone(), ("LANDED".into(), "9".into()));
    t.git.set("refs/heads/local/main", "moved");
    t.git
        .trees
        .borrow_mut()
        .insert("merged-tb".into(), T_MOVED.into());
    let merges = t
        .git
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("merge "))
        .count();
    t.io.out.borrow_mut().clear();
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("merge "))
            .count()
            > merges,
        "the members are merged again onto the moved base"
    );
    assert!(
        t.lc.has(&format!("promote {s} merged-tb moved")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(t.out().contains("attested=0"), "{}", t.out());
    assert!(
        !t.lc
            .calls
            .borrow()
            .iter()
            .any(|c| c.contains(&format!("event batch {s}")) && c.contains("PassGreen")),
        "no pass is recorded without a run"
    );
    assert!(!kv_of(&t, "round").contains_key("phase"));
    assert!(
        !gate::cert::path(&t.s().run.join("verdicts"), "spira", T_MOVED)
            .is_some_and(|p| p.exists())
    );
}

#[test]
fn a_staged_round_whose_pass_was_red_is_not_attested() {
    let (t, n, s) = staged_behind_green();
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("test-b.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(t.run(&["round", "stage-test"]), 1);
    assert_eq!(kv_of(&t, "round-staged")["phase"], "red");
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n, ("LANDED".into(), "9".into()));
    assert_eq!(t.run(&["round", "promote"]), 0, "{}", t.err());
    assert!(
        t.out().contains("attested=0") && t.lc.has(&format!("promote {s} ")),
        "{}",
        t.out()
    );
}

#[test]
fn a_red_round_discards_the_stage_behind_it() {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() =
        vec![("test-a.sh".into(), "red 1 2 fp p e 1".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let n = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round",
            "stage",
            "--members",
            "sp-b:tb",
            "--name",
            "spira-staged"
        ]),
        0,
        "{}",
        t.err()
    );
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-staged".into(), ("STAGED".into(), "0".into()));
    t.git.set("HEAD", "merged-ta");
    assert_eq!(t.run(&["round", "certify", &n]), 1);
    assert!(t.lc.has("event batch spira-staged STAGED 0 {\"Discard\":{\"reason\":\"round spira-20260929T010203Z went red: test-a.sh\"}}"), "{:?}", t.lc.calls.borrow());
    assert!(
        !t.qfile("round-staged").exists(),
        "the staged record is gone"
    );
    assert!(
        t.landing_log().contains("QUEUE ROUND-DISCARD")
            && t.landing_log().contains("batch=spira-staged")
    );
    assert!(
        !t.lc.has("event bead sp-b"),
        "discarding returns no member: none was delivered"
    );
}

#[test]
fn an_abandoned_round_discards_the_stage_behind_it() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let n = batch_of(&t);
    assert_eq!(
        t.run(&[
            "round",
            "stage",
            "--members",
            "sp-b:tb",
            "--name",
            "spira-staged"
        ]),
        0,
        "{}",
        t.err()
    );
    t.lc.batch_states
        .borrow_mut()
        .insert("spira-staged".into(), ("STAGED".into(), "0".into()));
    assert_eq!(
        t.run(&["round", "abandon", &n, "--reason", "gave up"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("event batch spira-staged STAGED 0")
                && c.contains("Discard")
                && c.contains("gave up")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(!t.qfile("round-staged").exists());
}

#[test]
fn promote_discards_when_the_round_it_followed_did_not_land() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states
        .borrow_mut()
        .insert(s.clone(), ("STAGED".into(), "0".into()));
    let _ = fs::remove_file(t.qfile("round"));
    t.lc.batch_states
        .borrow_mut()
        .insert(n, ("ABANDONED".into(), "5".into()));
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(
        t.err().contains("is ABANDONED, not LANDED") && t.err().contains("discarded"),
        "{}",
        t.err()
    );
    assert!(!t.lc.has("promote ") && !t.qfile("round-staged").exists());
}

#[test]
fn promote_waits_while_the_round_ahead_is_still_open() {
    let (t, _, _) = staged_behind_green();
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(t.err().contains("still open"), "{}", t.err());
    assert!(
        t.qfile("round-staged").exists() && !t.lc.has("promote "),
        "the stage is kept"
    );
}

#[test]
fn promote_discards_when_a_staged_member_moved() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states
        .borrow_mut()
        .insert(s, ("STAGED".into(), "0".into()));
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    t.lc.batch_states
        .borrow_mut()
        .insert(n, ("LANDED".into(), "9".into()));
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-b")
    {
        r.tip = Some("tb2".into());
    }
    assert_eq!(t.run(&["round", "promote"]), 1);
    assert!(t.err().contains("no longer admissible"), "{}", t.err());
    assert!(!t.qfile("round-staged").exists() && !t.lc.has("promote "));
}

#[test]
fn round_open_discards_a_stage_whose_round_is_gone() {
    let (t, n, s) = staged_behind_green();
    t.lc.batch_states
        .borrow_mut()
        .insert(s.clone(), ("STAGED".into(), "0".into()));
    assert_eq!(t.run(&["round", "land", &n]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.qfile("round-staged").exists());
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.starts_with(&format!("event batch {s} STAGED 0")) && c.contains("Discard")),
        "{:?}",
        t.lc.calls.borrow()
    );
}

#[test]
fn a_mender_bead_carries_the_resolved_scope_label_though_the_process_env_lacks_it() {
    let mut t = round_world();
    t.lib.s.verdict.mender_label = "mend-fixture".into();
    t.env
        .resolved
        .borrow_mut()
        .insert("SPIRA_SCOPE_LABEL".into(), "scope-fixture".into());
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );
    let calls = t.lib.calls.borrow();
    let bug = calls.iter().find(|c| c.starts_with("create_bug")).unwrap();
    assert!(bug.contains(" scope-fixture,mend-fixture,repo:"), "{bug}");
}

#[test]
fn each_red_suite_files_one_mender_lane_bead_carrying_its_output_pass_and_batch() {
    let mut t = round_world();
    t.lib.s.verdict.mender_label = "mend-fixture".into();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(results.join("test-z.sh.out"), "assertion Z failed\n").unwrap();

    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-b.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );

    let calls = t.lib.calls.borrow();
    let bugs: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bug"))
        .collect();
    assert_eq!(
        bugs.len(),
        2,
        "one bead per failure, none for a repeated report: {bugs:?}"
    );
    assert!(
        bugs[0].contains(" mend-fixture,repo:") && bugs[0].contains("test-z.sh"),
        "{}",
        bugs[0]
    );
    assert!(
        bugs[0].contains("assertion Z failed")
            && bugs[0].contains(&format!("Batch: {batch}"))
            && bugs[0].contains("Pass: 1")
            && bugs[0].contains("Deadline:"),
        "{}",
        bugs[0]
    );
    assert!(
        bugs[1].contains("test-b.sh") && bugs[1].contains("left no output"),
        "{}",
        bugs[1]
    );
}

#[test]
fn a_red_that_is_a_repo_config_refusal_files_no_mender_bead_in_any_round() {
    let mut t = round_world();
    t.lib.s.verdict.mender_label = "mend-fixture".into();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());
    let results = t.lib.s.run.join("rounds").join(format!("{batch}.results"));
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(
        results.join("ci.out"),
        ".spira/testenv.toml declares no ci_entry\n",
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(
            t.run(&["round", "failed", &batch, "--suite", "ci"]),
            0,
            "{}",
            t.err()
        );
    }
    let bugs = t
        .lib
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("create_bug"))
        .count();
    assert_eq!(bugs, 0, "a refusal no mend can fix files nothing");
}

#[test]
fn a_suite_going_red_mid_pass_is_a_failed_event_in_the_order_it_failed_and_the_pass_is_not_judged()
{
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    assert_eq!(t.run(&["round", "pass-start", &batch]), 0, "{}", t.err());

    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-z.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "test-b.sh"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        events_of(&t, &batch),
        [
            "{\"PassStarted\":{\"n\":1,\"head\":\"merged-tc\"}}",
            "{\"SuitesStarted\":{\"n\":1}}",
            "{\"Failed\":{\"suite\":\"test-z.sh\",\"n\":1}}",
            "{\"Failed\":{\"suite\":\"test-b.sh\",\"n\":1}}",
        ],
        "the suites phase opens on the first result, and no verdict is sent"
    );
    assert_eq!(
        t.run(&["round", "failed", &batch, "--suite", "bad suite"]),
        1,
        "a suite name is an identifier"
    );
}

#[test]
fn a_red_certify_with_no_batcher_files_one_mender_per_red_suite_with_the_attribution() {
    let mut t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lib.s.batcher_bin = Some(std::path::PathBuf::from("batcher"));
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: String::new(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "red".into()),
        ("test-b.sh".into(), "red".into()),
        ("test-c.sh".into(), "ok".into()),
    ];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    let calls = t.lib.calls.borrow();
    let bugs: Vec<&String> = calls
        .iter()
        .filter(|c| c.starts_with("create_bug"))
        .collect();
    assert_eq!(bugs.len(), 2, "one mender per red suite: {bugs:?}");
    assert!(
        bugs.iter().all(|b| b.contains("verdict=ambiguous")),
        "{bugs:?}"
    );
}

#[test]
fn certify_reds_a_pass_whose_unit_tests_failed_though_every_suite_is_ok() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 1,
        out: String::new(),
        err: "round-vm: UNIT-TESTS: FAIL (unit run rc=101)".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(t.out().contains("red=unit-tests"), "{}", t.out());
    assert!(
        !certified_tree_exists(&t),
        "unit tests that failed certify nothing"
    );
}

#[test]
fn certify_reds_a_failed_workspace_build_naming_the_first_compiler_error() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let err = "round-vm: building\nerror[E0063]: missing field `cpus` in initializer of `RunRequest`\n   --> testenv/src/shots.rs:224:9\nerror: could not compile `testenv`\nround-vm: the round's workspace build failed";
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 4,
        out: String::new(),
        err: err.into(),
    };
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(t.out().contains("red=workspace-build"), "{}", t.out());
    assert!(
        t.out()
            .contains("build-error=error[E0063]: missing field `cpus`")
            && t.out().contains("testenv/src/shots.rs:224"),
        "{}",
        t.out()
    );
}

#[test]
fn certify_saves_the_compiler_errors_of_a_failed_workspace_build_and_quotes_the_first() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    let err = "round-vm: building\nerror[E0425]: cannot find value `x` in this scope\n   --> spira/sim/src/x.rs:12:5\n    |\n12  |     x\n\nerror[E0063]: missing field `cpus`\n   --> testenv/src/shots.rs:224:9\n\nerror: could not compile `testenv`\nround-vm: the round's workspace build failed";
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 4, out: String::new(), err: err.into() };
    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(
        t.out().contains("red-reason=workspace-build: error[E0425] at spira/sim/src/x.rs:12:5"),
        "{}",
        t.out()
    );
    let saved = find_file_named(&t.dir, "workspace-build.out")
        .expect("workspace-build.out was written");
    assert!(saved.contains("cargo build") && saved.contains("exit: 4"), "{saved}");
    assert!(saved.contains("error[E0425]") && saved.contains("error[E0063]"), "{saved}");
}

#[test]
fn a_workspace_build_that_died_of_the_environment_is_not_the_members_red() {
    use crate::ops::round::build_environment_cause;
    assert_eq!(
        build_environment_cause("round-vm: building\nerror: failed to write: No space left on device"),
        Some("out of disk")
    );
    assert_eq!(
        build_environment_cause("error[E0425]: cannot find value\nNo space left on device"),
        None,
        "a compiler diagnostic makes it the candidate's"
    );
}

fn find_file_named(dir: &std::path::Path, name: &str) -> Option<String> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(s) = find_file_named(&p, name) {
                return Some(s);
            }
        } else if p.file_name().is_some_and(|f| f == name) {
            return std::fs::read_to_string(p).ok();
        }
    }
    None
}

#[test]
fn certify_judges_a_wall_killed_run_whose_results_and_unit_tests_are_complete() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 124,
        out: String::new(),
        err: "round-vm: UNIT-TESTS: PASS (3 test binaries)".into(),
    };
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(t.out().contains("GREEN"), "{}", t.out());
}

#[test]
fn a_green_write_that_times_out_after_applying_still_certifies_and_certify_is_idempotent() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lc.batch_event_times_out.set(Some("PassGreen"));
    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(
        certified_tree_exists(&t),
        "the certificate is written though the reply was lost"
    );

    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "merged-ta",
            "--ran",
            "3"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(certified_tree_exists(&t));
    t.run(&["round", "land", &batch]);
    assert!(
        t.err().contains("certified by round GREEN"),
        "land finds the certificate: {}",
        t.err()
    );
}

#[test]
fn a_retry_after_a_timed_out_green_completes_the_missing_certificate() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("GREEN".into(), 1, 1, String::new()));
    let ran = t.scripts.calls.borrow().len();
    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "merged-ta",
            "--ran",
            "3"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(certified_tree_exists(&t));
    assert_eq!(
        t.scripts.calls.borrow().len(),
        ran,
        "a GREEN round is finished, never re-tested: {:?}",
        t.scripts.calls.borrow()
    );
}

#[test]
fn a_green_the_lifecycle_refuses_keeps_the_certificate_the_pass_earned() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.lc.batch_event_refused.set(Some("PassGreen"));
    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(
        certified_tree_exists(&t),
        "the full corpus passed on this tree; a refused reply must not lose that"
    );
    assert!(
        t.err().contains("certificate") && t.err().contains("kept"),
        "{}",
        t.err()
    );
}

#[test]
fn a_green_batch_at_another_head_is_still_refused_as_changed() {
    let t = round_world();
    *t.lc.batch_view.borrow_mut() = Some(("OPEN".into(), 0, 0, String::new()));
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    *t.lc.batch_view.borrow_mut() = Some(("GREEN".into(), 1, 1, String::new()));
    t.git.set("refs/heads/elsewhere", "merged-ta");
    assert_eq!(
        t.run(&[
            "round",
            "certify",
            &batch,
            "--attest",
            "elsewhere",
            "--ran",
            "3"
        ]),
        1
    );
    assert!(!certified_tree_exists(&t));
}

fn guards_file(t: &T) -> String {
    let p = t.dir.path().join("statute-guards");
    std::fs::write(
        &p,
        "law-nonrebase | refuse | rebase by hand\nlaw-touch | show | allocator\n",
    )
    .unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn a_round_eject_reason_contradicting_a_statute_is_refused_and_ejects_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let g = guards_file(&t);
    t.env
        .vars
        .borrow_mut()
        .insert("SPIRA_STATUTE_GUARDS".into(), g);
    let before = kv_of(&t, "round");
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "needs a rebase by hand"
        ]),
        1
    );
    assert!(
        t.err().contains("law-nonrebase") && t.err().contains("BEAD_STATUTE_CONSIDERED"),
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round"), before);
    assert!(!t.lib.has("bead_reopen"));
}

#[test]
fn a_round_eject_with_an_override_reason_proceeds_and_a_touch_is_shown() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let g = guards_file(&t);
    t.env
        .vars
        .borrow_mut()
        .insert("SPIRA_STATUTE_GUARDS".into(), g);
    t.env
        .vars
        .borrow_mut()
        .insert("BEAD_STATUTE_CONSIDERED".into(), "not that rebase".into());
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-b",
            "--reason",
            "rebase by hand"
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "round",
            "eject",
            &batch,
            "sp-a",
            "--reason",
            "the allocator is red"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.err().contains("law-touch"), "{}", t.err());
}

#[test]
fn an_unreadable_statute_registry_refuses_a_round_eject() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    t.env
        .vars
        .borrow_mut()
        .insert("SPIRA_STATUTE_GUARDS".into(), "/nonexistent/guards".into());
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-a", "--reason", "red"]),
        1
    );
    assert!(
        t.err().contains("cannot read the statute registry"),
        "{}",
        t.err()
    );
}

const T_FIX: &str = "4444444444444444444444444444444444444444";

fn rework_world(holds: &[&str]) -> T {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .retain(|r| r.bead_id != "sp-a");
    t.lc_row("sp-a", "REWORK", "ta", 100);
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-a")
    {
        r.holds = holds.iter().map(|h| h.to_string()).collect();
    }
    t.git.set("refs/heads/spira/sp-a", T_FIX);
    t.git.ancestor(T_FIX, "merged-tb");
    t
}

fn ev_tip() -> String {
    format!("--evidence-tip={T_FIX}")
}

#[test]
fn a_rework_bead_held_manual_is_retipped_and_admitted_to_a_round() {
    let t = rework_world(&["manual"]);
    let tip_arg = format!("--tip={T_FIX}");
    t.run(&["round", "open", "--members", &format!("sp-a:{T_FIX}")]);
    assert!(
        t.err().contains("nothing admissible") && !t.qfile("round").exists(),
        "a REWORK row is not admitted: {}",
        t.err()
    );

    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has(&format!(
            "retip sp-a {T_FIX} concierge [hand-built fix] [npm test 1/0] {T_FIX}"
        )),
        "{:?}",
        t.lc.calls.borrow()
    );

    assert_eq!(
        t.run(&["round", "open", "--members", &format!("sp-a:{T_FIX}")]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains(&format!("sp-a:{T_FIX}")), "{}", t.out());
}

#[test]
fn resubmit_moves_the_local_branch_to_the_new_tip() {
    let t = rework_world(&["manual"]);
    t.git.set(
        "refs/heads/spira/sp-a",
        "9999999999999999999999999999999999999999",
    );
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.git.get("refs/heads/spira/sp-a").as_deref(), Some(T_FIX));
}

#[test]
fn resubmit_leaves_a_branch_it_already_moved_alone_when_branch_f_would_refuse() {
    let t = rework_world(&["manual"]);
    t.git.branch_set_fails.set(true);
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(t.git.get("refs/heads/spira/sp-a").as_deref(), Some(T_FIX));
}

#[test]
fn resubmit_fetches_an_absent_tip_and_refuses_when_the_fetch_brings_nothing() {
    let t = rework_world(&["manual"]);
    let absent = "6".repeat(40);
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &format!("--tip={absent}"),
            "--by=concierge",
            "--reason=r",
            "--evidence=npm test 1/0",
            &format!("--evidence-tip={absent}")
        ]),
        1
    );
    assert!(
        t.git
            .calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("fetch ") && c.ends_with("spira/sp-a")),
        "{:?}",
        t.git.calls.borrow()
    );
    assert_eq!(
        t.git.get("refs/heads/spira/sp-a").as_deref(),
        Some(T_FIX),
        "the branch is not moved"
    );
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn an_unclaimed_ready_bead_is_resubmitted_and_its_branch_set_to_the_tip() {
    let t = rework_world(&[]);
    t.git.set("refs/heads/spira/sp-a", "stale");
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-a")
    {
        r.state = "READY".into();
    }
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built P0",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has(&format!(
            "retip sp-a {T_FIX} concierge [hand-built P0] [npm test 1/0] {T_FIX}"
        )),
        "{:?}",
        t.lc.calls.borrow()
    );
    let state =
        t.lc.rows
            .borrow()
            .as_ref()
            .unwrap()
            .iter()
            .find(|r| r.bead_id == "sp-a")
            .map(|r| (r.state.clone(), r.tip.clone()))
            .unwrap();
    assert_eq!(state, ("SUBMITTED".to_string(), Some(T_FIX.to_string())));
    assert_eq!(
        t.git
            .rev_parse(Path::new("/"), "refs/heads/spira/sp-a")
            .as_deref(),
        Some(T_FIX)
    );
}

#[test]
fn a_bead_a_live_aeon_holds_is_not_resubmitted_and_the_refusal_names_the_holder() {
    let t = rework_world(&[]);
    for r in
        t.lc.rows
            .borrow_mut()
            .as_mut()
            .unwrap()
            .iter_mut()
            .filter(|r| r.bead_id == "sp-a")
    {
        r.state = "WORKING".into();
        r.holder = Some("aeon-7".into());
    }
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built P0",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert!(t.err().contains("aeon-7"), "{}", t.err());
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn a_hand_mended_tip_that_fails_its_lint_is_not_retipped() {
    let t = rework_world(&["manual"]);
    t.scripts
        .lint_red
        .borrow_mut()
        .insert(T_FIX.into(), "src/lib.rs:3:1".into());
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert!(t.err().contains("src/lib.rs:3:1"), "{}", t.err());
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn a_bead_held_by_no_one_is_not_retipped() {
    let t = rework_world(&[]);
    let tip_arg = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &tip_arg,
            "--by=concierge",
            "--reason=hand-built fix",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert!(t.err().contains("held by no one"), "{}", t.err());
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn resubmit_refuses_a_stranger_a_short_tip_an_unknown_commit_and_a_non_rework_row() {
    let t = rework_world(&["manual"]);
    let ok = format!("--tip={T_FIX}");
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &ok,
            "--by=aeon-x",
            "--reason=r",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            "--tip=abc",
            "--by=concierge",
            "--reason=r",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-a",
            &format!("--tip={}", "5".repeat(40)),
            "--by=concierge",
            "--reason=r",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert_eq!(
        t.run(&[
            "resubmit",
            "sp-b",
            &ok,
            "--by=concierge",
            "--reason=r",
            "--evidence=npm test 1/0",
            &ev_tip()
        ]),
        1
    );
    assert_eq!(
        t.run(&["resubmit", "sp-a", &ok, "--by=concierge"]),
        2,
        "a reason is required"
    );
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

#[test]
fn a_resubmit_without_evidence_or_with_an_evidence_tip_that_is_not_the_tip_is_refused() {
    let t = rework_world(&["manual"]);
    let tip_arg = format!("--tip={T_FIX}");
    let base = [
        "resubmit",
        "sp-a",
        tip_arg.as_str(),
        "--by=concierge",
        "--reason=hand-built fix",
    ];
    assert_eq!(t.run(&base), 2);
    assert!(t.err().contains("--evidence is required"), "{}", t.err());
    let other = format!("--evidence-tip={}", "7".repeat(40));
    assert_eq!(
        t.run(&[&base[..], &["--evidence=npm test 1/0", other.as_str()]].concat()),
        1
    );
    assert!(
        t.err().contains("--evidence-tip must equal --tip"),
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[&base[..], &["--evidence=npm test 1/0"]].concat()),
        1,
        "no evidence tip at all"
    );
    assert!(!t.lc.has("retip"), "{:?}", t.lc.calls.borrow());
}

fn concurrent_world() -> T {
    let mut t = round_world();
    t.lib.s.rounds_max = 2;
    *t.scripts.round_vm.borrow_mut() = RunOut::default();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    t
}

#[test]
fn a_second_round_with_disjoint_members_opens_beside_the_first() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-2"]),
        0,
        "{}",
        t.err()
    );
    assert!(kv_of(&t, "round")["batch_id"] == "r-1" && kv_of(&t, "round.r-2")["batch_id"] == "r-2");
    assert_ne!(
        kv_of(&t, "round")["worktree"],
        kv_of(&t, "round.r-2")["worktree"],
        "each round has its own worktree"
    );
    let out = t.io.out.borrow().len();
    assert_eq!(t.run(&["round", "status"]), 0);
    let status = t.out()[out..].to_string();
    assert!(
        status.contains("rounds=2") && status.contains("open_batches=r-1 r-2"),
        "{status}"
    );
    assert!(
        status.contains("batch_id=r-1")
            && status.contains("round2_batch_id=r-2")
            && status.contains("round2_members=sp-b:tb"),
        "{status}"
    );
}

#[test]
fn a_second_round_naming_a_member_of_an_open_round_is_refused_naming_it() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b,sp-a", "--name", "r-2"]),
        1
    );
    assert!(
        t.err()
            .contains("sp-a is already a member of open round r-1"),
        "{}",
        t.err()
    );
    assert!(!t.qfile("round.r-2").exists());
}

#[test]
fn the_open_past_the_limit_is_refused() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-2"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-c", "--name", "r-3"]),
        1
    );
    assert!(
        t.err()
            .contains("2 rounds are already open for spira (limit 2)"),
        "{}",
        t.err()
    );
}

#[test]
fn a_round_is_addressed_by_its_batch_id_and_the_oldest_record_follows_the_landed() {
    let t = concurrent_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a", "--name", "r-1"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-2"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "abandon", "r-2", "--reason", "x"]),
        0,
        "{}",
        t.err()
    );
    assert!(!t.qfile("round.r-2").exists() && kv_of(&t, "round")["batch_id"] == "r-1");
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b", "--name", "r-3"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "abandon", "r-1", "--reason", "x"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(
        kv_of(&t, "round")["batch_id"],
        "r-3",
        "the surviving round takes over the round file"
    );
    assert!(!t.qfile("round.r-3").exists());
}

fn tree_of(t: &T, head: &str) {
    t.git.ancestor(head, head);
    t.git.trees.borrow_mut().insert(head.into(), T_ROUND.into());
}

fn open_and_certify(t: &T, member: &str, name: &str) {
    assert_eq!(
        t.run(&["round", "open", "--members", member, "--name", name]),
        0,
        "{}",
        t.err()
    );
}

fn two_green_stacked_rounds(t: &T) {
    open_and_certify(t, "sp-a", "r-1");
    assert_eq!(t.run(&["round", "certify", "r-1"]), 0, "{}", t.err());
    t.git.heads_name_their_base.set(true);
    open_and_certify(t, "sp-b", "r-2");
    tree_of(t, "merged-tb@merged-ta");
    assert_eq!(t.run(&["round", "certify", "r-2"]), 0, "{}", t.err());
    t.git.ancestor("ta", "merged-ta");
    t.git.ancestor("tb", "merged-tb@merged-ta");
}

#[test]
fn a_later_round_is_cut_on_the_earlier_rounds_head_and_lands_after_it_with_one_certify() {
    let t = concurrent_world();
    two_green_stacked_rounds(&t);
    let second = kv_of(&t, "round.r-2");
    assert_eq!(
        (second["base"].as_str(), second["parent"].as_str()),
        ("merged-ta", "r-1"),
        "the second round is stacked on the first round's head"
    );
    let runs = vm_runs(&t);
    assert_eq!(t.run(&["round", "land", "r-2"]), 1);
    assert!(
        t.err()
            .contains("stacked on round r-1, which has not landed"),
        "{}",
        t.err()
    );
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("b0"),
        "the stacked round lands only behind its parent"
    );
    assert_eq!(t.run(&["round", "land", "r-1"]), 0, "{}", t.err());
    assert_eq!(t.run(&["round", "land", "r-2"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb@merged-ta"));
    assert_eq!(
        vm_runs(&t),
        runs,
        "landing the second round re-certified nothing"
    );
    assert!(!t.landing_log().contains("QUEUE ROUND-REMERGE"));
}

#[test]
fn a_stacked_round_whose_parent_was_abandoned_is_rebuilt_without_it_and_certified_again() {
    let t = concurrent_world();
    two_green_stacked_rounds(&t);
    assert_eq!(
        t.run(&["round", "abandon", "r-1", "--reason", "red"]),
        0,
        "{}",
        t.err()
    );
    tree_of(&t, "merged-tb");
    let runs = vm_runs(&t);
    assert_eq!(t.run(&["round", "land", "r-2"]), 1);
    assert!(t.err().contains("it is not landed"), "{}", t.err());
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("b0"),
        "nothing landed on the parent's tree"
    );
    let rec = kv_of(&t, "round");
    assert_eq!(
        (
            rec["batch_id"].as_str(),
            rec["base"].as_str(),
            rec["head"].as_str()
        ),
        ("r-2", "b0", "merged-tb")
    );
    assert!(!rec.contains_key("parent"));
    assert_eq!(round_phase(&t), "REBUILDING");
    t.git.ancestor("tb", "merged-tb");
    assert_eq!(t.run(&["round", "certify", "r-2"]), 0, "{}", t.err());
    assert_eq!(vm_runs(&t), runs + 1);
    assert_eq!(t.run(&["round", "land", "r-2"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

#[test]
fn a_stacked_round_is_recut_on_the_head_its_parent_was_rebuilt_to() {
    let t = concurrent_world();
    two_green_stacked_rounds(&t);
    let mut parent = kv_of(&t, "round");
    parent.insert("head".into(), "rebuilt-ta".into());
    let mut kv = crate::records::Kv::default();
    for (k, v) in &parent {
        kv.push(k, v);
    }
    crate::records::write_atomic(&t.qfile("round"), &kv.render()).unwrap();
    tree_of(&t, "merged-tb@rebuilt-ta");
    assert_eq!(t.run(&["round", "land", "r-2"]), 1);
    let rec = kv_of(&t, "round.r-2");
    assert_eq!(
        (
            rec["base"].as_str(),
            rec["head"].as_str(),
            rec["parent"].as_str()
        ),
        ("rebuilt-ta", "merged-tb@rebuilt-ta", "r-1")
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}

#[test]
fn a_member_that_conflicts_in_meaning_with_an_earlier_round_is_certified_on_that_rounds_tree() {
    let t = concurrent_world();
    open_and_certify(&t, "sp-a", "r-1");
    assert_eq!(t.run(&["round", "certify", "r-1"]), 0, "{}", t.err());
    t.git.heads_name_their_base.set(true);
    open_and_certify(&t, "sp-b", "r-2");
    assert_eq!(
        t.git.cut_on.borrow().as_str(),
        "merged-ta",
        "the second member is merged into the first round's tree, where a duplicate definition fails the build"
    );
    t.git.ancestor("merged-tb@merged-ta", "merged-tb@merged-ta");
    t.git.trees.borrow_mut().insert(
        "merged-tb@merged-ta".into(),
        "4444444444444444444444444444444444444444".into(),
    );
    *t.scripts.round_vm_results.borrow_mut() = vec![
        ("test-a.sh".into(), "ok".into()),
        ("test-b.sh".into(), "red 1 2 fp p e 1".into()),
    ];
    t.git.ancestor("ta", "merged-ta");
    assert_eq!(t.run(&["round", "land", "r-1"]), 0, "{}", t.err());
    *t.lc.batch_view.borrow_mut() = None;
    assert_eq!(t.run(&["round", "certify", "r-2"]), 1, "{}", t.out());
    assert_eq!(t.run(&["round", "land", "r-2"]), 1);
    assert_eq!(
        t.landed_ref().as_deref(),
        Some("merged-ta"),
        "the conflicting round never lands"
    );
}

#[test]
fn a_member_whose_tip_is_a_recorded_ejected_red_tip_is_not_offered_to_the_cut() {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r.bead_id == "sp-b")
        .unwrap()
        .ejected_red_tip = Some("tb".into());
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out().contains("sp-b: tb is a recorded ejected-red tip"),
        "{}",
        t.out()
    );
    assert!(t.out().contains("members=sp-a:ta sp-c:tc"), "{}", t.out());
}

#[test]
fn a_member_spira_lc_refuses_at_the_cut_is_dropped_and_the_round_opens_with_the_rest() {
    let t = round_world();
    t.lc.cut_refuses_member.set(Some("sp-b"));
    t.lc.no_batch_row.set(true);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb,sp-c:tc"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.out()
            .contains("sp-b: spira-lc refused it at the cut — dropped from the round"),
        "{}",
        t.out()
    );
    assert!(t.out().contains("members=sp-a:ta sp-c:tc"), "{}", t.out());
}

#[test]
fn rework_returns_an_unbatched_submitted_bead_and_refuses_a_round_member() {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .retain(|r| r.bead_id != "sp-c");
    t.lc_row("sp-c", "SUBMITTED", "tc", 100);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );

    assert_eq!(
        t.run(&["rework", "sp-a", "--by=concierge", "--reason=red"]),
        1
    );
    assert!(
        t.err().contains("member of the open round spira-"),
        "the batch is named: {}",
        t.err()
    );
    assert!(!t.lc.has("\"GateRed\""), "{:?}", t.lc.calls.borrow());

    assert_eq!(t.run(&["rework", "sp-c", "--by=aeon-x", "--reason=red"]), 1);
    assert_eq!(
        t.run(&["rework", "sp-c", "--by=concierge"]),
        2,
        "a reason is required"
    );
    assert!(!t.lc.has("\"GateRed\""), "{:?}", t.lc.calls.borrow());

    assert_eq!(
        t.run(&[
            "rework",
            "sp-c",
            "--by=concierge",
            "--reason=red on test-install-aeons"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(t.lc.has("event bead sp-c "), "{:?}", t.lc.calls.borrow());
    assert!(
        t.lc.calls
            .borrow()
            .iter()
            .any(|c| c.contains("\"tip\":\"tc\"")),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert_eq!(t.lc.bead_state("sp-c").unwrap().0, "REWORK");
}

fn proof_run(t: &T) -> std::path::PathBuf {
    t.s().run.clone()
}

#[test]
fn a_resubmit_is_refused_naming_an_ejected_suite_that_does_not_pass_at_the_new_tip_and_accepted_when_it_does(
) {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["test-x.sh".to_string()])
        .unwrap();
    t.scripts.suite_red.borrow_mut().push("test-x.sh".into());
    let tip_arg = format!("--tip={T_FIX}");
    let args = [
        "resubmit",
        "sp-a",
        &tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=npm test 1/0",
        &ev_tip(),
    ];
    assert_eq!(t.run(&args), 1);
    assert!(
        t.err().contains("test-x.sh failed at") && t.err().contains("refused"),
        "{}",
        t.err()
    );
    assert!(
        !t.lc.calls.borrow().iter().any(|c| c.starts_with("retip")),
        "a refused resubmit must not move the bead"
    );

    t.scripts.suite_red.borrow_mut().clear();
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert!(spira_config::proof::proven(
        &proof_run(&t),
        "sp-a",
        T_FIX,
        "test-x.sh"
    ));
}

#[test]
fn an_eject_red_on_unit_tests_records_the_step_as_the_requirement() {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = batch_of(&t);
    seed_suite(&t, "test-b.sh");
    assert_eq!(
        t.run(&["round", "eject", &batch, "sp-b", "--reason", "red", "--red", "--suites", "unit-tests"]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lib.has("bead_reopen sp-b eject-red unit-tests"),
        "{:?} {:?}",
        t.lib.calls.borrow(),
        t.lc.calls.borrow()
    );
}

#[test]
fn a_round_only_step_is_proven_only_by_a_recorded_pass_at_the_tip() {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["workspace-build".to_string()])
        .unwrap();
    let tip_arg = format!("--tip={T_FIX}");
    let args = [
        "resubmit",
        "sp-a",
        &tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=cargo build 1/0",
        &ev_tip(),
    ];
    assert_eq!(t.run(&args), 1);
    assert!(
        t.err().contains("workspace-build has no green round recorded"),
        "{}",
        t.err()
    );
    assert!(
        !t.scripts.calls.borrow().iter().any(|c| c.starts_with("suite_run workspace-build")),
        "a round-only step is never run as a suite"
    );
    spira_config::proof::record_run(&proof_run(&t), "sp-a", T_FIX, "workspace-build", 0, "ok").unwrap();
    assert_eq!(t.run(&args), 0, "{}", t.err());
}

#[test]
fn a_bead_ejected_on_unit_tests_resubmits_after_a_green_runner_run_and_not_after_a_red_one() {
    let t = rework_world(&["manual"]);
    spira_config::proof::record_failing(&proof_run(&t), "sp-a", &["unit-tests".to_string()])
        .unwrap();
    let tip_arg = format!("--tip={T_FIX}");
    let args = [
        "resubmit",
        "sp-a",
        &tip_arg,
        "--by=concierge",
        "--reason=hand-built fix",
        "--evidence=cargo test 1/0",
        &ev_tip(),
    ];
    t.scripts.suite_red.borrow_mut().push("unit-tests".to_string());
    assert_eq!(t.run(&args), 1);
    assert!(t.scripts.calls.borrow().iter().any(|c| c.starts_with("suite_run unit-tests")));
    t.scripts.suite_red.borrow_mut().clear();
    assert_eq!(t.run(&args), 0, "{}", t.err());
    assert!(!t.err().contains("--clear"), "{}", t.err());
}

#[test]
fn a_hand_eject_for_red_must_name_the_failing_suite() {
    let t = rework_world(&[]);
    assert_eq!(t.run(&["eject", "sp-a", "--red", "--reason", "red"]), 2);
    assert!(t.err().contains("--suites"), "{}", t.err());
}

#[test]
fn absolve_refuses_without_evidence_and_with_it_readmits_a_red_tip() {
    let t = round_world();
    t.lc.rows
        .borrow_mut()
        .as_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r.bead_id == "sp-b")
        .unwrap()
        .ejected_red_tip = Some("tb".into());
    fs::write(t.qfile("red-tips"), "sp-b tb\n").unwrap();
    assert_eq!(
        t.run(&["absolve", "sp-b", "--by=concierge", "--reason=infra red"]),
        2,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "absolve",
            "sp-b",
            "--by=concierge",
            "--reason=infra red",
            "--evidence=sccache startup timeout",
            "--evidence-tip=other"
        ]),
        1,
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&[
            "absolve",
            "sp-b",
            "--by=aeon",
            "--reason=infra red",
            "--evidence=e",
            "--evidence-tip=tb"
        ]),
        1,
        "{}",
        t.err()
    );
    assert!(!t.lc.has("absolve"), "{:?}", t.lc.calls.borrow());
    assert_eq!(t.run(&["round", "open", "--members", "sp-b:tb"]), 1);
    assert!(
        format!("{}{}", t.out(), t.err()).contains("sp-b: tb is a recorded ejected-red tip"),
        "positive control: {}{}",
        t.out(),
        t.err()
    );

    assert_eq!(
        t.run(&[
            "absolve",
            "sp-b",
            "--by=concierge",
            "--reason=infra red",
            "--evidence=sccache startup timeout, no suite ran",
            "--evidence-tip=tb"
        ]),
        0,
        "{}",
        t.err()
    );
    assert!(
        t.lc.has(
            "absolve sp-b tb concierge [infra red] [sccache startup timeout, no suite ran] tb"
        ),
        "{:?}",
        t.lc.calls.borrow()
    );
    assert!(fs::read_to_string(t.qfile("red-tips")).unwrap().is_empty());
    assert_eq!(
        t.run(&[
            "absolve",
            "sp-b",
            "--by=concierge",
            "--reason=r",
            "--evidence=e",
            "--evidence-tip=tb"
        ]),
        1,
        "nothing left to absolve: {}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    assert!(t.out().contains("members=sp-b:tb"), "{}", t.out());
}

fn declare_own_testenv(t: &T) {
    let wt = PathBuf::from(&kv_of(t, "round")["worktree"]);
    fs::create_dir_all(wt.join(".spira")).unwrap();
    fs::write(
        wt.join(".spira").join("testenv.toml"),
        "ci_entry = \"deploy/ci.sh all\"\n",
    )
    .unwrap();
}

#[test]
fn a_repo_that_declares_its_own_testenv_certifies_and_lands_from_its_verdict() {
    let t = round_world();
    *t.scripts.round_vm.borrow_mut() = RunOut {
        rc: 2,
        out: String::new(),
        err: "round-vm must not run".into(),
    };
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let calls = t.scripts.calls.borrow().clone();
    assert!(
        calls.iter().any(|c| {
            c.starts_with("repo-round-vm ")
                && c.contains("entry=deploy/ci.sh all round=")
                && c.ends_with("/spira wall=900")
        }),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("round-vm")),
        "{calls:?}"
    );
    assert!(certified_tree_exists(&t));
    assert_eq!(round_phase(&t), "GREEN");

    assert_eq!(t.run(&["round", "land", &batch]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("merged-tb"));
}

#[test]
fn a_worktree_moved_by_stabilization_certifies_only_when_the_batcher_names_its_head() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);
    t.git.set("HEAD", "stabilized");
    t.git
        .trees
        .borrow_mut()
        .insert("stabilized".into(), T_ROUND.into());
    t.git.ancestor("b0", "stabilized");

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(
        t.err().contains("is not at the round's head"),
        "{}",
        t.err()
    );
    assert_eq!(
        t.run(&["round", "certify", &batch, "--rehead", "merged-tb"]),
        1
    );
    assert!(
        t.err()
            .contains("--rehead merged-tb is not the round worktree's head"),
        "{}",
        t.err()
    );
    assert!(!certified_tree_exists(&t));

    assert_eq!(
        t.run(&["round", "certify", &batch, "--rehead", "stabilized"]),
        0,
        "{}",
        t.err()
    );
    assert_eq!(kv_of(&t, "round")["head"], "stabilized");
    assert_eq!(round_phase(&t), "GREEN");
}

#[test]
fn a_repo_that_declares_its_own_testenv_goes_red_and_does_not_land_on_a_failing_suite() {
    let t = round_world();
    *t.scripts.repo_testenv.borrow_mut() = RunOut {
        rc: 1,
        out: "VERDICT RED ran=4 red=1".into(),
        err: String::new(),
    };
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);

    assert_eq!(t.run(&["round", "certify", &batch]), 1);
    assert!(t.out().contains("red=testenv"), "{}", t.out());
    assert!(!certified_tree_exists(&t), "a red round certifies nothing");
    assert_eq!(t.run(&["round", "land", &batch]), 1);
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing landed");
}

#[test]
fn a_testenv_fault_on_an_own_testenv_repo_is_not_judged() {
    let t = round_world();
    *t.scripts.repo_testenv.borrow_mut() = RunOut {
        rc: 2,
        out: "VERDICT FAULT rc=2".into(),
        err: "tier lint: podman: command not found".into(),
    };
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    declare_own_testenv(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(
        t.err().contains("CI entry exited 2")
            && t.err().contains("not judged")
            && t.err().contains("tier lint: podman: command not found"),
        "{}",
        t.err()
    );
}

#[test]
fn an_own_testenv_repo_with_no_ci_entry_is_not_judged_and_runs_nothing() {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    let wt = PathBuf::from(&kv_of(&t, "round")["worktree"]);
    fs::create_dir_all(wt.join(".spira")).unwrap();
    fs::write(wt.join(".spira").join("testenv.toml"), "").unwrap();
    assert_eq!(t.run(&["round", "certify", &batch]), 4, "{}", t.err());
    assert!(t.err().contains("declares no ci_entry"), "{}", t.err());
    let calls = t.scripts.calls.borrow().clone();
    assert!(!calls.iter().any(|c| c.contains("round-vm")), "{calls:?}");
}

const STALE_ACQUIRE: &str = "round-vm run: acquire failed: EC2 launch failed (placement ec2-always)\nclone refused: the newest round AMI ami-1 carries recipe-sha aa but this release's recipe is bb; run the round-ami-build workflow\n";

fn stale_ami() -> RunOut {
    RunOut {
        rc: 2,
        out: String::new(),
        err: STALE_ACQUIRE.into(),
    }
}

fn stale_world() -> (T, String) {
    let t = round_world();
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    (t, batch)
}

#[test]
fn a_stale_round_image_waits_for_the_build_in_progress_then_proceeds() {
    let (t, batch) = stale_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![stale_ami(), stale_ami(), RunOut::default()];
    *t.forge.ami_runs.borrow_mut() = vec![
        Some(vec!["381189175".into()]),
        Some(vec!["381189175".into()]),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    assert!(
        t.out().contains("waiting for round AMI 381189175"),
        "{}",
        t.out()
    );
    let calls = t.forge.calls.borrow().join("\n");
    assert!(!calls.contains("ami-dispatch"), "{calls}");
    assert_eq!(calls.matches("ami-runs").count(), 2, "{calls}");
}

#[test]
fn a_stale_round_image_with_no_build_running_triggers_exactly_one() {
    let (t, batch) = stale_world();
    *t.scripts.round_vm_queue.borrow_mut() = vec![stale_ami(), stale_ami(), RunOut::default()];
    *t.forge.ami_runs.borrow_mut() = vec![Some(vec![]), Some(vec!["7".into()])];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let calls = t.forge.calls.borrow().join("\n");
    assert_eq!(calls.matches("ami-dispatch").count(), 1, "{calls}");
}

#[test]
fn a_round_image_that_stays_stale_reports_the_acquire_error_itself() {
    let (t, batch) = stale_world();
    *t.scripts.round_vm.borrow_mut() = stale_ami();

    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    let err = t.err();
    assert!(
        err.contains("round AMI is stale")
            && err.contains("clone refused")
            && err.contains("round-ami-build workflow"),
        "{err}"
    );
    assert!(!err.contains("left no verdicts"), "{err}");
}

fn recipe_changing_world() -> (T, String) {
    let mut t = round_world();
    t.lib.s.round_spill = "ec2-always".into();
    *t.scripts.recipe_release.borrow_mut() = Some("release-recipe".into());
    *t.scripts.recipe_tree.borrow_mut() = Some("candidate-recipe-sha".into());
    *t.scripts.round_vm_results.borrow_mut() = vec![("test-a.sh".into(), "ok 3 1 fp p e 0".into())];
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a"]),
        0,
        "{}",
        t.err()
    );
    let batch = batch_of(&t);
    (t, batch)
}

#[test]
fn a_recipe_changing_round_builds_its_own_image_and_certifies_on_it() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Missing, AmiLookup::Found("ami-cand".into())];
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::NotStarted),
        Some(AmiBuild::Running("41".into())),
        Some(AmiBuild::Success("41".into())),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let branch = format!("spira-round-candidate/{batch}-candidate-re");
    let forge = t.forge.calls.borrow().join("\n");
    assert_eq!(
        forge.matches(&format!("ami-dispatch {branch}")).count(),
        1,
        "{forge}"
    );
    assert!(
        t.out()
            .contains("waiting for the candidate round image build 41"),
        "{}",
        t.out()
    );
    let git = t.lib.calls.borrow().join("\n");
    assert!(git.contains(&format!(":refs/heads/{branch}")), "{git}");
    let vm = t.scripts.calls.borrow().join("\n");
    assert!(vm.contains("ami=ami-cand"), "{vm}");
}

#[test]
fn a_recipe_already_imaged_dispatches_no_build() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.ami_for.borrow_mut() = vec![AmiLookup::Found("ami-have".into())];

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let forge = t.forge.calls.borrow().join("\n");
    assert!(!forge.contains("ami-dispatch"), "{forge}");
    assert!(t.scripts.calls.borrow().join("\n").contains("ami=ami-have"));
}

#[test]
fn a_recipe_change_whose_image_fails_to_build_reds_the_round_naming_the_step() {
    let (t, batch) = recipe_changing_world();
    *t.forge.ami_results.borrow_mut() = vec![
        Some(AmiBuild::NotStarted),
        Some(AmiBuild::Failure(
            "41".into(),
            vec!["build / Build, image and tag".into()],
        )),
    ];

    assert_eq!(t.run(&["round", "certify", &batch]), 1, "{}", t.err());
    assert!(t.out().contains("red=workspace-build"), "{}", t.out());
    assert!(
        t.out()
            .contains("build-error=round image build 41 failed at build / Build, image and tag"),
        "{}",
        t.out()
    );
    let vm = t.scripts.calls.borrow().join("\n");
    assert!(!vm.contains("round-vm "), "{vm}");
}

#[test]
fn a_round_whose_recipe_is_the_releases_never_touches_the_image_build() {
    let (t, batch) = recipe_changing_world();
    *t.scripts.recipe_tree.borrow_mut() = Some("release-recipe".into());

    assert_eq!(t.run(&["round", "certify", &batch]), 0, "{}", t.err());
    let forge = t.forge.calls.borrow().join("\n");
    assert!(!forge.contains("ami-"), "{forge}");
    assert!(!t.scripts.calls.borrow().join("\n").contains("ami="));
}

#[test]
fn a_candidate_image_build_that_outlasts_its_wait_leaves_the_round_unjudged() {
    let (t, batch) = recipe_changing_world();
    *t.forge.ami_results.borrow_mut() = vec![Some(AmiBuild::Running("41".into()))];

    assert_eq!(
        t.run(&["round", "certify", &batch]),
        crate::ops::round::FAULT,
        "{}",
        t.err()
    );
    assert!(
        t.err().contains("did not finish within 5400s"),
        "{}",
        t.err()
    );
}

#[test]
fn open_rounds_a_recipe_changing_member_apart_from_the_ordinary_ones() {
    let t = round_world();
    t.git.stat("b0", "tb", &[("round-vm/ami-setup.sh", 1, 1)]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b,sp-c"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("skip — sp-b: solo: sp-b changes the round recipe")
            && o.contains("members=sp-a:ta sp-c:tc")
            && !o.contains("sp-b:tb"),
        "{o}"
    );
}

#[test]
fn open_admits_a_lone_recipe_changing_member_by_itself() {
    let t = round_world();
    t.git.stat("b0", "ta", &[("flake.nix", 1, 1)]);
    t.git.stat("b0", "tb", &[("flake.lock", 1, 1)]);
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a,sp-b"]),
        0,
        "{}",
        t.err()
    );
    let o = t.out();
    assert!(
        o.contains("members=sp-a:ta")
            && !o.contains("sp-b:tb")
            && o.contains("sp-b: solo: sp-b changes the round recipe"),
        "{o}"
    );
}
