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
    // The row's 2-min cadence: the row has no first-boot delay, so the boot tick arms the pass
    // one cadence later (the old timer's OnBootSec=180 s is not modelled).
    first_timer_ms: CADENCE_MS,
    budget: 60,
}

const NOW: u64 = 1_793_000_000_000;

/// The contract's idle fixture: no open plan beads, no lease past grace, landing in flight.
fn idle() -> Snapshot {
    Snapshot {
        beads: Some(vec![]),
        rows: Some(vec![Row { id: "sp-a".into(), state: "WORKING".into(), version: 3, holder: Some("dwarf-1".into()), lease_until_ms: Some(NOW + 600_000), ..Row::default() }]),
        plan_ready: Some(0),
        holder_alive: [("sp-a".to_string(), true)].into(),
        landing_active: true,
        world_running: true,
        now_ms: NOW,
        labels: Labels { queue_wait: QUEUE_WAIT.into() },
        ..Snapshot::default()
    }
}

/// `SPIRA_QUEUE_WAIT_LABEL`'s production value (`spira/conf.d/SPIRA_QUEUE_WAIT_LABEL`).
const QUEUE_WAIT: &str = "spira-queue-waiting";

/// A lease long past grace whose holder is still alive.
fn busy() -> Snapshot {
    Snapshot { rows: Some(vec![Row { id: "sp-a".into(), state: "WORKING".into(), version: 3, holder: Some("dwarf-1".into()), lease_until_ms: Some(NOW - 3_600_000), ..Row::default() }]), ..idle() }
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

/// `tests.rs` `close_landed_queue_waiters_reads_the_label_not_bd_status`: a labeled waiter whose
/// own row is LANDED is closed and released by its label and row, whatever bd says (sp-w is
/// LANDED with bd status open); a labeled WORKING one (sp-x) is not closed.
#[test]
fn t5_check3b_releases_a_landed_queue_waiter_by_its_label() {
    let snap = Snapshot {
        beads: Some(vec![
            Bead { id: "sp-w".into(), title: "waiter".into(), labels: vec![QUEUE_WAIT.into()], deps: vec!["sp-l".into()], status: "open".into() },
            Bead { id: "sp-x".into(), title: "working".into(), labels: vec![QUEUE_WAIT.into()], deps: vec![], status: "open".into() },
        ]),
        rows: Some(vec![Row { id: "sp-w".into(), state: "LANDED".into(), version: 9, ..Row::default() }, Row { id: "sp-x".into(), state: "WORKING".into(), version: 2, holder: Some("dwarf-2".into()), lease_until_ms: Some(NOW + 600_000), ..Row::default() }]),
        holder_alive: [("sp-x".to_string(), true)].into(),
        ..idle()
    };
    let (_, acts) = decide(&Carry::default(), &snap);
    for e in [CONTENT_LABEL, CONTENT_CLOSE, LC_UNHOLD] {
        assert!(acts.iter().any(|a| a.effect == e && a.key.starts_with("sp-w")), "{e} for sp-w: {acts:?}");
    }
    assert!(acts.iter().all(|a| a.effect != CONTENT_CLOSE || !a.key.starts_with("sp-x")), "{acts:?}");
}

/// `waiters.rs` `labeled_set_is_claimable_by_the_lifecycle_row`: the waiter set is the labeled
/// beads whose row is claimable; a labeled bead with no lifecycle row is left alone, whatever
/// its content status.
#[test]
fn t5_check3b_leaves_a_waiter_with_no_lifecycle_row_alone() {
    let snap = Snapshot { beads: Some(vec![Bead { id: "sp-n".into(), title: "no row".into(), labels: vec![QUEUE_WAIT.into()], deps: vec![], status: "open".into() }]), ..idle() };
    let (_, acts) = decide(&Carry::default(), &snap);
    assert!(acts.iter().all(|a| !a.key.starts_with("sp-n")), "a waiter with no row is not touched: {acts:?}");
}

/// The production spellings only: `WORKING` is the leased state (a `CLAIMED` row is not one),
/// and the waiting label has no default — it is the pinned `SPIRA_QUEUE_WAIT_LABEL`.
#[test]
fn t5_the_checks_read_the_production_spellings_only() {
    assert_eq!(Labels::default().queue_wait, "", "no fixture default for the label");
    let claimed = Snapshot { rows: Some(vec![Row { state: "CLAIMED".into(), ..busy().rows.unwrap().remove(0) }]), ..busy() };
    let (_, acts) = decide(&Carry::default(), &claimed);
    assert!(acts.iter().all(|a| a.effect != STOP_HOLDER), "CLAIMED is not an alias of WORKING: {acts:?}");
    let (_, acts) = decide(&Carry::default(), &busy());
    assert!(acts.iter().any(|a| a.effect == STOP_HOLDER), "WORKING is the leased state: {acts:?}");
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

/// CHECK 7's hand-off lands on sentinel-call's own stream (`Command::EmitTo`), where its handler
/// hears it; on this task's stream nobody would.
#[test]
fn t5_check7_call_wanted_is_delivered_to_the_call() {
    let r = rig(1, Snapshot { plan_ready: Some(3), ..idle() });
    kit::assert_unpoisoned(&r, STREAM);
    let to: Vec<String> = r.journaled().into_iter().filter(|e| e.kind.tag == CALL_WANTED).map(|e| e.stream.0).collect();
    assert_eq!(to, [spira_runtime::tasks::a::sentinel_call::STREAM], "call.wanted goes to sentinel-call's stream");
}

/// CHECK 6's leg starts under the properties `systemd-run` gave it (`dispatch.rs`): the pinned
/// `SPIRA_LAND_MAXSEC` as `RuntimeMaxSec`, both streams appended to `landing.log`.
#[test]
fn t5_check6_the_landing_leg_carries_its_runtime_bound_and_its_log() {
    let (_, acts) = decide(&Carry::default(), &Snapshot { landing_active: false, land_maxsec: 900, ..idle() });
    let start = acts.iter().find(|a| a.effect == LANDING_START).expect("the landing leg starts");
    let w: Write = serde_json::from_str(&start.input).unwrap();
    let props = landing_props(&w, std::path::Path::new("/run/spira"));
    assert_eq!(props.runtime_max_ms, Some(900_000));
    assert_eq!(props.output_append.as_deref(), Some(std::path::Path::new("/run/spira/landing.log")));
    assert_eq!(landing_props(&Write::default(), std::path::Path::new("/r")).runtime_max_ms, None, "0 is no bound");
}
