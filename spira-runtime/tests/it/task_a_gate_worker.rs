//! Acceptance T1–T6 for `gate.worker`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/gate-worker.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide`, `step`, `pick`, `check_tip` and `classify` are `unimplemented!`, and the
//! row waits in `tasks::a::PENDING`. The N slots are N entries of one carry on one stream: a
//! `Command::Timer` arms only its own event's stream. T6 runs two virtual days (the contract asks
//! three, with gate legs finished by scripted `unit.ran`); its idle budget is "0 per idle hour".

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: gate_worker,
    registration: GateWorker::new(2, BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
}

fn job(repo: &str, branch: &str, fix: bool, priority: u8, enqueued_ms: u64) -> Job {
    Job { repo: repo.into(), branch: branch.into(), tip: format!("{branch}-tip"), bead: format!("sp-{branch}"), fix, priority, enqueued_ms }
}

/// The contract's idle fixture: an empty inbox.
fn idle() -> Snapshot {
    Snapshot { free_slots: 2, ..Snapshot::default() }
}

/// One job queued and admission free.
fn busy() -> Snapshot {
    Snapshot { inbox: vec![job("spira", "a", false, 2, 10)], ..idle() }
}

#[test]
fn t1_the_slots_are_certify_par() {
    let mut r = TaskRig::new(1, GateWorker::new(3, BTreeMap::new()).registration()).unwrap().double(OBSERVE, kit::cycling(vec![busy()], 1));
    for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let carry: Carry = r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).expect("a claim records the slots");
    assert_eq!(carry.slots.len(), 3);
}

#[test]
fn t2_full_admission_is_silent_too() {
    let mut r = rig(1, Snapshot { free_slots: 0, ..busy() });
    kit::assert_unpoisoned(&r, STREAM);
    assert_silent_tick(&mut r, STREAM);
    kit::assert_unpoisoned(&r, STREAM);
}

#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_gating_x_gate_queued_is_refused_naming_gating() {
    let refused = step(State::Gating, Trigger::GateQueued).expect_err("gating × gate.queued is illegal");
    assert!(refused.to_lowercase().contains("gating"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `gateq.rs:242-254`: fix branches first, then the repo with the fewest running, then priority, then age.
#[test]
fn t5_pick_takes_the_fix_then_the_quiet_repo_then_priority_then_age() {
    let snap = Snapshot {
        inbox: vec![job("spira", "old", false, 2, 1), job("spira", "fix", true, 3, 9), job("deck", "p1", false, 1, 5), job("deck", "p2", false, 2, 2)],
        running_by_repo: [("spira".to_string(), 1)].into(),
        free_slots: 2,
        ..Snapshot::default()
    };
    assert_eq!(pick(&snap, 0).map(|j| j.branch), Some("fix".into()));
    let rest = Snapshot { inbox: snap.inbox.iter().filter(|j| !j.fix).cloned().collect(), ..snap };
    assert_eq!(pick(&rest, 0).map(|j| j.branch), Some("p1".into()), "deck runs nothing, and P1 beats P2");
}

/// `lib.rs:309-322`: a gone or moved branch is dropped.
#[test]
fn t5_a_gone_or_moved_branch_is_discarded() {
    let j = job("spira", "a", false, 2, 10);
    assert_eq!(check_tip(&j, None), TipCheck::Gone);
    assert_eq!(check_tip(&j, Some("other")), TipCheck::Moved);
    assert_eq!(check_tip(&j, Some("a-tip")), TipCheck::Proceed);
}

/// `lib.rs:92-101`: only the gate's own status is a verdict.
#[test]
fn t5_classify_keeps_the_gates_verdict_and_makes_the_rest_no_verdict() {
    assert_eq!(classify(0, "gate: VERDICT=PASS\n").verdict, "PASS");
    assert_eq!(classify(1, "gate: VERDICT=FAIL\n").verdict, "FAIL");
    for (rc, why) in [(126, "gate-not-found"), (127, "gate-not-found"), (-1, "gate-did-not-start"), (137, "gate-killed")] {
        let run = classify(rc, "");
        assert_eq!((run.verdict.as_str(), run.reason.as_deref()), (NO_VERDICT, Some(why)), "rc {rc}");
    }
}

/// A claimed job whose gate ran files its verdict: retain a non-PASS output, then complete.
#[test]
fn t5_a_claim_is_asked_for_the_picked_job() {
    let (carry, acts) = decide(&Carry { slots: vec![Slot::default(), Slot::default()] }, &busy());
    assert_eq!(acts.iter().filter(|a| a.effect == CLAIM).map(|a| a.key.as_str()).collect::<Vec<_>>(), ["spira.a.a-tip"]);
    assert_eq!(carry.slots.iter().filter(|s| s.state == State::Claimed).count(), 1);
}

/// Its own legs' `unit.ran` (routed to its stream by unit name) wakes it: the slot's status is
/// read at once, not at the next tick.
#[test]
fn t3_its_own_leg_unit_ran_is_heard() {
    let reg = registration();
    assert!(reg.handler.interests().contains(&runtime::units::UNIT_RAN), "gate.worker listens for its legs' ends");
}

/// A transient unit inherits no environment: the gate leg must carry the release on PATH or
/// `gate.sh` is not found and every bead gets NO_VERDICT.
#[test]
fn a_gate_leg_carries_the_release_on_path() {
    let base_env = spira_runtime::tasks::a::batcher_local::edge::leg_env(vec![], "/rel", "/home/x", "/rel/conf", "/usr/bin");
    let snap = Snapshot { base_env, ..idle() };
    let slot = Slot { state: State::Warming, job: Some(job("spira", "a", false, 2, 10)), ..Slot::default() };
    let failed_warm = runtime::units::UnitSpec { task: NAME.into(), key: "0/warm/spira/a-tip".into(), argv: vec![], env: vec![], cwd: None };
    let (_, acts) = on_outcome(&Carry { slots: vec![slot] }, UNIT_START, &serde_json::to_string(&failed_warm).unwrap(), false, "", &snap);
    let leg = acts.iter().find(|a| a.effect == UNIT_START).expect("a failed warm leg moves on to the gate leg");
    let spec: runtime::units::UnitSpec = serde_json::from_str(&leg.input).unwrap();
    let get = |k: &str| spec.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert!(get("PATH").unwrap_or_default().starts_with("/rel/bin:/rel/spira:"), "PATH {:?}", get("PATH"));
    assert_eq!(get("SPIRA_RELEASE").as_deref(), Some("/rel"));
    assert_eq!(get("SPIRA_TOML").as_deref(), Some("/rel/conf"));
    assert!(get("SPIRA_GATE_BEAD").is_some(), "the gate's own variables stay");
}

#[test]
fn a_warm_leg_carries_the_release_on_path_and_a_leg_variable_wins() {
    let base_env = spira_runtime::tasks::a::batcher_local::edge::leg_env(vec![], "/rel", "/home/x", "/rel/conf", "/usr/bin");
    let snap = Snapshot { base_env, ..idle() };
    let j = job("spira", "a", false, 2, 10);
    let slot = Slot { state: State::Claimed, job: Some(j.clone()), ..Slot::default() };
    let input = serde_json::to_string(&SlotJob { slot: 0, job: j.clone() }).unwrap();
    let (_, acts) = on_outcome(&Carry { slots: vec![slot] }, TIP, &input, true, &j.tip, &snap);
    let spec: runtime::units::UnitSpec = serde_json::from_str(&acts.iter().find(|a| a.effect == UNIT_START).expect("tip answered starts the warm leg").input).unwrap();
    assert!(spec.env.iter().any(|(k, v)| k == "PATH" && v.starts_with("/rel/bin:/rel/spira:")), "{:?}", spec.env);
    let kept = spira_runtime::tasks::a::batcher_local::edge::with_defaults(vec![("PATH".into(), "/mine".into())], &snap.base_env);
    assert_eq!(kept.iter().filter(|(k, _)| k == "PATH").map(|(_, v)| v.as_str()).collect::<Vec<_>>(), ["/mine"]);
}

fn held(state: State, leg_started: bool, since_ms: Option<u64>) -> Slot {
    Slot { state, job: Some(job("spira", "a", false, 2, 10)), started_ms: Some(0), leg_started, stall: since_ms.map(|s| Stall { state, leg_started, since_ms: s }) }
}

fn at(now_ms: u64) -> Snapshot {
    Snapshot { now_ms, ..idle() }
}

#[test]
fn a_slot_that_never_leaves_claimed_is_recovered_and_the_worker_gates_the_next_job() {
    let stuck = Carry { slots: vec![held(State::Claimed, false, Some(1_000)), Slot::default()] };
    let (next, acts) = decide(&stuck, &at(1_000 + STUCK_MS + 1));
    assert!(acts.iter().any(|a| a.effect == RECOVER && a.input == "0"), "the stuck slot is recovered: {acts:?}");
    assert_eq!(next.slots[0], Slot::default());
    let (_, acts) = decide(&next, &Snapshot { inbox: vec![job("spira", "b", false, 2, 20)], now_ms: 1_000 + STUCK_MS + 2, ..idle() });
    assert!(acts.iter().any(|a| a.effect == CLAIM), "the freed slot gates the next SUBMITTED bead: {acts:?}");
}

#[test]
fn a_stuck_slot_is_first_seen_then_recovered_not_recovered_on_sight() {
    let carry = Carry { slots: vec![held(State::Filing, false, None)] };
    let (next, acts) = decide(&carry, &at(500_000));
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(next.slots[0].stall.as_ref().map(|s| s.since_ms), Some(500_000));
    let (_, acts) = decide(&next, &at(500_000 + STUCK_MS - 1));
    assert!(acts.is_empty(), "inside the bound: {acts:?}");
}

#[test]
fn a_gate_leg_that_runs_long_is_never_recovered() {
    let carry = Carry { slots: vec![held(State::Gating, true, None)] };
    let (next, acts) = decide(&carry, &at(10 * STUCK_MS));
    assert!(acts.iter().all(|a| a.effect != RECOVER), "{acts:?}");
    assert_eq!(next.slots[0].stall, None);
}

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    kit::assert_kept_as_rows(&at_work(), NAME);
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}
