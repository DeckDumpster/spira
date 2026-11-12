//! `queue hand-land`: the one way to land out of turn, and `land-local`'s refusal to be another.

use super::round::round_world;
use super::*;

const TIP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const T_ROUND: &str = "3333333333333333333333333333333333333333";

/// A round `hand-1` of sp-a and sp-b is open on b0, and sp-z's tip descends from b0 with a
/// round-GREEN tree.
fn world() -> T {
    let t = round_world();
    assert_eq!(
        t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb", "--name", "hand-1"]),
        0,
        "{}",
        t.err()
    );
    t.git.ancestor("b0", TIP);
    t.git.trees.borrow_mut().insert(TIP.into(), T1.into());
    t.certify("spira", T1, gate::cert::Source::Round, "-");
    t.lc_row("sp-z", "SUBMITTED", TIP, 100);
    t
}

fn hand_land(t: &T, extra: &[&str]) -> i32 {
    let mut argv = vec![
        "hand-land",
        "sp-z",
        "--tip",
        TIP,
        "--by",
        "concierge",
        "--reason",
        "the fix cannot wait",
        "--evidence",
        "cargo test -p queue ok",
        "--evidence-tip",
        TIP,
    ];
    argv.extend_from_slice(extra);
    t.run(&argv)
}

fn state(t: &T, id: &str) -> String {
    t.lc.bead_row(id).map(|r| r.state).unwrap_or_default()
}

#[test]
fn land_local_under_an_open_round_exits_3_naming_the_round_and_the_verb() {
    let t = world();
    assert_eq!(t.run(&["land-local", "--head", TIP, "--members", &format!("sp-z:{TIP}")]), 3);
    let e = t.err();
    assert!(
        e.contains("a round is open on b0 (hand-1, phase") && e.contains("members sp-a,sp-b") && e.contains("queue hand-land"),
        "{e}"
    );
    assert_eq!(t.landed_ref().as_deref(), Some("b0"), "nothing moved");
}

#[test]
fn land_local_with_no_round_open_still_lands() {
    let t = round_world();
    t.git.ancestor("b0", TIP);
    t.git.trees.borrow_mut().insert(TIP.into(), T1.into());
    t.certify("spira", T1, gate::cert::Source::Round, "-");
    assert_eq!(t.run(&["land-local", "--head", TIP, "--members", &format!("sp-z:{TIP}")]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some(TIP));
}

#[test]
fn a_dry_run_prints_the_round_its_green_passes_and_the_cost_and_changes_nothing() {
    let t = world();
    t.certify("spira", T_ROUND, gate::cert::Source::Round, "-");
    assert_eq!(hand_land(&t, &["--dry-run"]), 0, "{}", t.err());
    let o = t.out();
    assert!(o.contains("round hand-1 phase") && o.contains("members sp-a,sp-b") && o.contains("1 green pass(es)"), "{o}");
    assert!(
        o.contains("round hand-1 will be voided: 2 member(s) re-queued, 1 green pass(es) discarded"),
        "{o}"
    );
    assert!(o.contains(&format!("tip {TIP} contains local/main")), "{o}");
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert!(t.qfile("round").exists() && !t.qfile("cut-hold").exists());
    assert!(!t.landing_log().contains("HAND-LAND"));
}

#[test]
fn a_dry_run_with_wait_says_the_landing_waits_and_with_no_round_says_none_is_open() {
    let t = world();
    assert_eq!(hand_land(&t, &["--dry-run", "--round", "wait"]), 0);
    assert!(t.out().contains("landing waits for round hand-1"), "{}", t.out());
    let none = round_world();
    none.git.ancestor("b0", TIP);
    assert_eq!(hand_land(&none, &["--dry-run"]), 0);
    assert!(none.out().contains("no round open"), "{}", none.out());
}

#[test]
fn a_dry_run_names_the_config_keys_the_tip_adds_and_retires() {
    let t = world();
    t.git.stat("b0", TIP, &[("spira/conf.d/SPIRA_NEW", 3, 0), ("spira/conf.d/SPIRA_OLD", 0, 3), ("src/a.rs", 1, 1)]);
    t.git.files.borrow_mut().insert(format!("{TIP}:spira/conf.d/SPIRA_NEW"), "x".into());
    t.git.files.borrow_mut().insert("b0:spira/conf.d/SPIRA_OLD".into(), "x".into());
    assert_eq!(hand_land(&t, &["--dry-run"]), 0, "{}", t.err());
    assert!(t.out().contains("adds: SPIRA_NEW; retires: SPIRA_OLD"), "{}", t.out());
}

#[test]
fn a_hand_landing_needs_a_round_policy() {
    let t = world();
    assert_eq!(hand_land(&t, &[]), 2);
    assert!(t.err().contains("--round wait|subvert is required"), "{}", t.err());
    assert_eq!(hand_land(&t, &["--round", "later"]), 2);
}

#[test]
fn subvert_voids_the_round_with_the_cause_lands_and_hands_the_members_back() {
    let t = world();
    assert_eq!(hand_land(&t, &["--round", "subvert"]), 0, "{}", t.err());
    let log = t.landing_log();
    let abandon = log.lines().find(|l| l.contains("ROUND-ABANDON")).unwrap_or_default();
    assert!(abandon.contains("cause=hand-landing by=concierge bead=sp-z"), "{log}");
    assert!(log.contains("HAND-LAND") && log.contains("round=subvert") && log.contains("voided=hand-1"), "{log}");
    assert_eq!(t.landed_ref().as_deref(), Some(TIP));
    assert!(!t.qfile("round").exists(), "the round is closed");
    for id in ["sp-a", "sp-b"] {
        assert_eq!(state(&t, id), "CERTIFIED", "{id} is back as it was");
    }
    assert!(t.out().contains("member sp-a: ") && t.out().contains(" in the round, returned as CERTIFIED"), "{}", t.out());
    assert!(!t.qfile("cut-hold").exists(), "the cut hold is lifted when the landing is done");
}

#[test]
fn subvert_takes_a_leased_round_under_its_holders_token() {
    let t = world();
    t.lc.leases.borrow_mut().insert(
        "spira/hand-1".into(),
        Ok(Some(RoundLease { holder: Some("batcher".into()), token: "7-1".into(), ttl_left_secs: 60 })),
    );
    assert_eq!(hand_land(&t, &["--round", "subvert"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some(TIP));
}

#[test]
fn wait_holds_the_cut_until_the_round_ends_then_lands_in_that_gap() {
    let t = world();
    let hold = t.qfile("cut-hold");
    let seen = std::rc::Rc::new(Cell::new(0u32));
    let ended = std::rc::Rc::new(Cell::new(false));
    let (s, e, h) = (seen.clone(), ended.clone(), hold.clone());
    let lc: *const FLc = &t.lc;
    *t.clock.on_sleep.borrow_mut() = Some(Box::new(move || {
        assert!(h.exists(), "the cut is held for every moment of the wait");
        s.set(s.get() + 1);
        if s.get() == 3 {
            // SAFETY: the world outlives the verb that sleeps.
            unsafe { &*lc }.batch_states.borrow_mut().insert("hand-1".into(), ("LANDED".into(), "9".into()));
            e.set(true);
        }
    }));
    assert_eq!(hand_land(&t, &["--round", "wait"]), 0, "{}", t.err());
    assert!(ended.get() && seen.get() == 3, "it waited for the round and no longer");
    assert_eq!(t.landed_ref().as_deref(), Some(TIP));
    assert!(!hold.exists(), "released after the landing");
    assert!(t.out().contains("waiting for round hand-1"), "{}", t.out());
    assert!(!t.lc.has("abandon-batch"), "wait voids nothing");
}

#[test]
fn wait_gives_up_at_the_rounds_bound_lands_nothing_and_lifts_the_hold() {
    let t = world();
    assert_eq!(hand_land(&t, &["--round", "wait"]), 1);
    assert!(t.err().contains("round hand-1 is still open after"), "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
    assert!(!t.qfile("cut-hold").exists());
}

#[test]
fn a_cut_already_held_by_another_hand_landing_refuses() {
    let t = world();
    let other = spira_config::cut_hold::CutHold {
        owner: "hand-land sp-q".into(),
        reason: "r".into(),
        pid: std::process::id(),
        since: 1_000,
        until: 99_999,
    };
    spira_config::cut_hold::place(&t.s().queue_dir, "spira", &other).unwrap();
    assert_eq!(hand_land(&t, &["--round", "subvert"]), 1);
    assert!(t.err().contains("hand-land sp-q"), "{}", t.err());
    assert!(t.qfile("round").exists(), "the round was left alone");
}

#[test]
fn a_tip_that_does_not_contain_the_landing_ref_is_refused_before_anything_is_touched() {
    let t = world();
    t.git.set("refs/heads/local/main", "b1");
    assert_eq!(hand_land(&t, &["--round", "subvert"]), 1);
    assert!(t.err().contains("does not contain local/main"), "{}", t.err());
    assert!(t.qfile("round").exists() && !t.lc.has("abandon-batch"));
}

fn harness(t: &mut T) -> testkit::TempDir {
    t.lib.s.home_repo = "spira".into();
    release_in_force(t);
    t.git.set("HEAD", TIP);
    let bins = testkit::TempDir::new("queue-hand-land-bins");
    fs::create_dir_all(bins.path().join("target/release")).unwrap();
    testkit::write_exe(bins.path().join("target/release/release"), "#!/bin/sh\n");
    bins
}

#[test]
fn a_new_release_is_read_through_after_it_is_activated() {
    let mut t = world();
    let bins = harness(&mut t);
    let wt = bins.path().display().to_string();
    assert_eq!(hand_land(&t, &["--round", "subvert", "--worktree", &wt]), 0, "{}", t.err());
    assert!(release_argv(&t).iter().any(|a| a.starts_with("activate ")));
    assert!(t.lc.has("fresh-read show sp-z") && t.lc.has("fresh-read content show sp-z"));
    assert!(t.landing_log().contains("verify=ok"), "{}", t.landing_log());
}

#[test]
fn a_release_whose_content_read_is_refused_is_rolled_back_and_the_read_is_named() {
    let mut t = world();
    let bins = harness(&mut t);
    *t.lc.fresh_refused.borrow_mut() = Some(("content show", "content door: SPIRA_NEW is not declared".into()));
    let wt = bins.path().display().to_string();
    assert_eq!(hand_land(&t, &["--round", "subvert", "--worktree", &wt]), 3);
    let e = t.err();
    assert!(e.contains("content read") && e.contains("content door: SPIRA_NEW is not declared"), "{e}");
    assert!(e.contains("local/main stays at") && e.contains("queue rollback-local spira"), "{e}");
    let activations = release_argv(&t).iter().filter(|a| a.starts_with("activate ")).count();
    assert_eq!(activations, 2, "the new release, then the one it replaced: {:?}", release_argv(&t));
    assert!(release_argv(&t).iter().any(|a| a.starts_with("activate r1 ")), "{:?}", release_argv(&t));
    assert_eq!(t.landed_ref().as_deref(), Some(TIP), "the landing ref stays moved");
    assert!(t.landing_log().contains("verify=failed"), "{}", t.landing_log());
}

const MISMATCH: &str = "round-vm run: refusing binaries: tree sha 3033 does not match expected cdc7 — nothing installed";

fn round_vm_refuses_its_tree(t: &T) {
    *t.scripts.round_vm.borrow_mut() = RunOut { rc: 2, out: String::new(), err: MISMATCH.into() };
}

fn open_round_for_certify() -> (T, String) {
    let t = round_world();
    assert_eq!(t.run(&["round", "open", "--members", "sp-a:ta,sp-b:tb"]), 0, "{}", t.err());
    let batch = t.lc.batch_states.borrow().keys().next().cloned().unwrap_or_else(|| "hand".into());
    let batch = fs::read_to_string(t.qfile("round"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("batch_id=").map(String::from))
        .unwrap_or(batch);
    (t, batch)
}

#[test]
fn a_pass_refused_for_its_tree_after_the_base_moved_says_so_and_rebuilds_instead_of_faulting() {
    let (t, batch) = open_round_for_certify();
    t.git.set("refs/heads/local/main", "b1");
    t.git.ancestor("b0", "b1");
    t.git.bases.borrow_mut().insert(("b1".into(), "merged-tb".into()), "b0".into());
    t.git.log.borrow_mut().push(RangeCommit { sha: "b1b1b1b1b1b1".into(), parents: vec!["b0".into()], subject: "sp-ixhu8a: landed by hand".into() });
    round_vm_refuses_its_tree(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), crate::ops::LAND_BASE_MOVED);
    let e = t.err();
    assert!(e.contains("base moved from b0 to b1 by b1b1b1b sp-ixhu8a: landed by hand"), "{e}");
    assert!(!e.contains("round-vm exited 2"), "not a harness fault: {e}");
    assert!(e.contains("re-merged"), "the round was rebuilt on the new base: {e}");
    assert!(t.landing_log().contains("QUEUE ROUND-REMERGE"), "{}", t.landing_log());
}

#[test]
fn a_pass_refused_for_its_tree_with_the_base_unmoved_stays_a_round_fault() {
    let (t, batch) = open_round_for_certify();
    round_vm_refuses_its_tree(&t);
    assert_eq!(t.run(&["round", "certify", &batch]), 4);
    let e = t.err();
    assert!(!e.contains("base moved from"), "{e}");
    assert!(!t.landing_log().contains("QUEUE ROUND-REMERGE"));
}

#[test]
fn wait_refuses_a_tip_that_the_round_it_waited_for_landed_past() {
    let t = world();
    let lc: *const FLc = &t.lc;
    let git: *const FGit = &t.git;
    *t.clock.on_sleep.borrow_mut() = Some(Box::new(move || {
        // SAFETY: the world outlives the verb that sleeps.
        unsafe { &*lc }.batch_states.borrow_mut().insert("hand-1".into(), ("LANDED".into(), "9".into()));
        unsafe { &*git }.set("refs/heads/local/main", "b9");
    }));
    assert_eq!(hand_land(&t, &["--round", "wait"]), 1);
    let e = t.err();
    assert!(e.contains("moved to b9 while this waited") && e.contains("rebase the tip"), "{e}");
    assert_eq!(t.landed_ref().as_deref(), Some("b9"), "this landed nothing");
    assert!(!t.qfile("cut-hold").exists());
    assert!(t.landing_log().contains("verify=refused: base moved"), "{}", t.landing_log());
}

fn uncertified_world() -> T {
    let t = world();
    t.git.trees.borrow_mut().insert(TIP.into(), "9999999999999999999999999999999999999999".into());
    t.git.ancestor("b0", "b0");
    t
}

#[test]
fn subvert_lands_an_uncertified_tip_on_the_verbs_own_evidence_with_no_environment() {
    let t = uncertified_world();
    assert_eq!(hand_land(&t, &["--round", "subvert"]), 0, "{}", t.err());
    assert_eq!(t.landed_ref().as_deref(), Some(TIP));
    assert!(t.landing_log().contains("QUEUE UNGATED") && t.landing_log().contains("by=concierge"), "{}", t.landing_log());
    assert!(t.out().contains("verify: "), "{}", t.out());
}

#[test]
fn a_landing_that_cannot_be_admitted_exits_before_the_round_is_touched() {
    let t = uncertified_world();
    let rc = t.run(&[
        "hand-land", "sp-z", "--tip", TIP, "--by", "nobody", "--reason", "r", "--evidence", "e",
        "--evidence-tip", TIP, "--round", "subvert",
    ]);
    assert_eq!(rc, 1, "{}", t.err());
    assert!(t.err().contains("cannot be landed ungated"), "{}", t.err());
    assert!(t.qfile("round").exists(), "the round was left alone");
    assert!(!t.lc.has("abandon-batch"));
    assert_eq!(t.landed_ref().as_deref(), Some("b0"));
}
