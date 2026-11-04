//! Acceptance T1–T6 for `sentinel-pass`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/sentinel-pass.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide` and `step` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T6 runs two virtual days (the contract asks seven, with scripted `spira-landing` `unit.ran`).

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: sentinel_pass,
    registration: SentinelPass::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: BOOT_DELAY_MS,
    budget: 60,
}

const NOW: u64 = 1_793_000_000_000;

/// The contract's idle fixture: no open plan beads, no lease past grace, landing in flight.
fn idle() -> Snapshot {
    Snapshot {
        beads: Some(vec![]),
        rows: Some(vec![Row { id: "sp-a".into(), state: "CLAIMED".into(), version: 3, holder: Some("dwarf-1".into()), lease_until_ms: Some(NOW + 600_000), ..Row::default() }]),
        plan_ready: Some(0),
        holder_alive: [("sp-a".to_string(), true)].into(),
        landing_active: true,
        world_running: true,
        now_ms: NOW,
        ..Snapshot::default()
    }
}

/// A lease long past grace whose holder is still alive.
fn busy() -> Snapshot {
    Snapshot { rows: Some(vec![Row { id: "sp-a".into(), state: "CLAIMED".into(), version: 3, holder: Some("dwarf-1".into()), lease_until_ms: Some(NOW - 3_600_000), ..Row::default() }]), ..idle() }
}

#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_another_unit_while_awaiting_landing_is_ignored_and_acting_x_tick_is_refused_naming_acting() {
    assert_eq!(step(State::AwaitingLanding, Trigger::OtherUnitRan), Ok(State::AwaitingLanding));
    let refused = step(State::Acting, Trigger::Tick).expect_err("acting × tick is illegal");
    assert!(refused.to_lowercase().contains("acting"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, Snapshot { plan_ready: Some(2), ..busy() });
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `tests.rs` `check2_stops_a_live_holder_before_reclaiming_and_refuses_when_it_will_not_stop`:
/// a stale lease with a live holder stops the holder; no reclaim rides the same pass.
#[test]
fn t5_check2_stops_a_live_holder_before_reclaiming() {
    let (_, acts) = decide(&Carry::default(), &busy());
    assert!(acts.iter().any(|a| a.effect == STOP_HOLDER && a.key == "sp-a"), "{acts:?}");
    assert!(acts.iter().all(|a| !(a.effect == LC_EVENT && a.key.starts_with("sp-a"))), "a live holder is never reclaimed in the pass that stops it: {acts:?}");
}

/// `tests.rs` `close_landed_queue_waiters_reads_the_label_not_bd_status`: a queue waiter whose
/// wait landed is released by its label, not by bd status.
#[test]
fn t5_check3b_releases_a_landed_queue_waiter_by_its_label() {
    let snap = Snapshot {
        beads: Some(vec![
            Bead { id: "sp-w".into(), title: "waiter".into(), labels: vec!["queue-wait".into()], deps: vec!["sp-l".into()], status: "open".into() },
            Bead { id: "sp-l".into(), title: "landed".into(), labels: vec![], deps: vec![], status: "open".into() },
        ]),
        rows: Some(vec![Row { id: "sp-l".into(), state: "DELIVERED".into(), version: 9, ..Row::default() }]),
        ..idle()
    };
    let (_, acts) = decide(&Carry::default(), &snap);
    assert!(acts.iter().any(|a| [CONTENT_LABEL, CONTENT_CLOSE, LC_UNHOLD].contains(&a.effect.as_str()) && a.key.starts_with("sp-w")), "{acts:?}");
}

/// CHECK 7 (`pass.rs:520`): plan beads ready hand off to the call as `call.wanted`, never a fork.
#[test]
fn t5_check7_hands_off_to_the_call() {
    let (_, acts) = decide(&Carry::default(), &Snapshot { plan_ready: Some(3), ..idle() });
    assert!(acts.iter().any(|a| a.effect == CALL_WANTED), "{acts:?}");
}

/// `pass.rs:421-426`, `lifecycle.rs:192-199`: unreadable rows make no lifecycle decision.
#[test]
fn t5_unreadable_rows_decide_nothing_about_leases() {
    let (_, acts) = decide(&Carry::default(), &Snapshot { rows: None, ..busy() });
    assert!(acts.iter().all(|a| ![STOP_HOLDER, LC_EVENT, LC_FACT].contains(&a.effect.as_str())), "{acts:?}");
}
