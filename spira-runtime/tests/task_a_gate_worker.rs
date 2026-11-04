//! Acceptance T1–T6 for `gate.worker`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/gate-worker.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide`, `step`, `pick`, `check_tip` and `classify` are `unimplemented!`, and the
//! row waits in `tasks::a::STAGED`. The N slots are N entries of one carry on one stream: a
//! `Command::Timer` arms only its own event's stream. T6 runs two virtual days (the contract asks
//! three, with gate legs finished by scripted `unit.ran`); its idle budget is "0 per idle hour".

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: gate_worker,
    registration: GateWorker::new(2, BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
    budget: 0,
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
