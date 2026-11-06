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
        labels: Labels { queue_wait: QUEUE_WAIT.into(), ask: "needs-operator".into(), open_children: "spira-open-children".into(), red_tracker: "spira-red-tracker".into(), epics: vec!["spira".into()], ..Labels::default() },
        watchers: vec!["ryan".into()],
        inference_every_secs: 3_600,
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
            Bead { id: "sp-w".into(), title: "waiter".into(), labels: vec![QUEUE_WAIT.into()], deps: vec!["sp-l".into()], status: "open".into(), ..Bead::default() },
            Bead { id: "sp-x".into(), title: "working".into(), labels: vec![QUEUE_WAIT.into()], deps: vec![], status: "open".into(), ..Bead::default() },
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
    let snap = Snapshot { beads: Some(vec![Bead { id: "sp-n".into(), title: "no row".into(), labels: vec![QUEUE_WAIT.into()], deps: vec![], status: "open".into(), ..Bead::default() }]), ..idle() };
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

fn bead(id: &str, labels: &[&str], status: &str) -> Bead {
    Bead { id: id.into(), title: format!("{id} title"), labels: labels.iter().map(|l| l.to_string()).collect(), status: status.into(), ..Bead::default() }
}

fn row(id: &str, state: &str) -> Row {
    Row { id: id.into(), state: state.into(), version: 1, ..Row::default() }
}

fn effects(acts: &[Act], e: &str) -> Vec<Act> {
    acts.iter().filter(|a| a.effect == e).cloned().collect()
}

/// An epic of the partition whose every child is closed, and one still open.
fn pilgrimage_world() -> Snapshot {
    let epic = Bead { kind: "epic".into(), ..bead("sp-e", &["spira"], "open") };
    let kids = [Bead { parents: vec!["sp-e".into()], ..bead("sp-c1", &[], "closed") }, Bead { parents: vec!["sp-e".into()], ..bead("sp-c2", &[], "closed") }];
    let other = Bead { kind: "epic".into(), ..bead("sp-o", &["gastown"], "open") };
    Snapshot { beads: Some([vec![epic, other], kids.to_vec()].concat()), ..idle() }
}

/// CHECK 1 (`pilgrimage.sh check`): a complete epic of the partition is announced once and
/// closed; one outside the partition is not; a live unlanded child branch defers it; one that
/// grew again loses its marker.
#[test]
fn t5_check1_a_complete_pilgrimage_is_announced_once_and_closed() {
    let (carry, acts) = decide(&Carry::default(), &pilgrimage_world());
    let close = effects(&acts, PILGRIMAGE_CLOSE);
    assert_eq!(close.iter().map(|a| a.key.as_str()).collect::<Vec<_>>(), ["sp-e"], "{acts:?}");
    let w: Write = serde_json::from_str(&close[0].input).unwrap();
    assert_eq!((w.subject.as_str(), w.total, w.subscribers.clone()), ("PILGRIMAGE COMPLETE — sp-e: sp-e title", 2, vec!["ryan".to_string()]));
    assert_eq!(carry.announced, ["sp-e"]);
    assert!(effects(&decide(&carry, &pilgrimage_world()).1, PILGRIMAGE_CLOSE).is_empty(), "announced once");
    let deferred = Snapshot { unlanded: [("sp-e".to_string(), vec!["spira/sp-c1 in spira".to_string()])].into(), ..pilgrimage_world() };
    assert!(effects(&decide(&Carry::default(), &deferred).1, PILGRIMAGE_CLOSE).is_empty(), "a live unlanded branch defers");
    let mut grown = pilgrimage_world();
    grown.beads.as_mut().unwrap().push(Bead { parents: vec!["sp-e".into()], ..bead("sp-c3", &[], "open") });
    assert!(decide(&carry, &grown).0.announced.is_empty(), "a pilgrimage that grew again is reopened");
}

/// CHECK 2b: strand's classification names stranded work, so strand acts this pass.
#[test]
fn t5_check2b_stranded_work_runs_strand() {
    assert!(effects(&decide(&Carry::default(), &idle()).1, STRAND).is_empty(), "nothing stranded, no act");
    let snap = Snapshot { strand: vec!["builder\tbead\tsp-s\tWORKING with no dwarf".into()], ..idle() };
    assert_eq!(effects(&decide(&Carry::default(), &snap).1, STRAND).len(), 1);
}

/// CHECK 2c (`lifecycle.rs` check2c): rows whose holder and state disagree are detected,
/// recorded in the carry (journaled when the finding changes), and nothing is written.
#[test]
fn t5_check2c_inconsistent_rows_are_recorded_not_acted_on() {
    let snap = Snapshot { rows: Some(vec![row("sp-i", "WORKING"), Row { holder: Some("dwarf-9".into()), ..row("sp-l", "LANDED") }]), ..idle() };
    let (carry, acts) = decide(&Carry::default(), &snap);
    assert_eq!(carry.inconsistent.len(), 2, "{:?}", carry.inconsistent);
    assert!(acts.iter().all(|a| !a.key.starts_with("sp-i") && !a.key.starts_with("sp-l")), "{acts:?}");
}

/// CHECK 2d (`holds.rs`): an ask hold whose ask is gone is withdrawn after the grace; a held
/// share of READY over a quarter is announced; a stale stack-conflict marker is announced.
#[test]
fn t5_check2d_stale_ask_holds_starvation_and_stack_conflicts() {
    let held = |id: &str| Row { holds: vec!["ask".into()], updated_at_ms: Some(NOW - 20_000_000), ..row(id, "READY") };
    let snap = Snapshot { beads: Some(vec![bead("sp-h", &[], "open")]), rows: Some(vec![held("sp-h")]), reclaim_grace_secs: 10_800, ..idle() };
    let acts = decide(&Carry::default(), &snap).1;
    assert!(acts.iter().any(|a| a.effect == LC_EVENT && a.key.starts_with("sp-h") && a.input.contains("AskWithdrawn")), "{acts:?}");
    let starved = Snapshot { rows: Some((0..4).map(|i| held(&format!("sp-{i}"))).collect()), reclaim_grace_secs: 999_999_999, ..idle() };
    let acts = decide(&Carry::default(), &starved).1;
    assert!(effects(&acts, INCIDENT_EVENT).iter().any(|a| a.input.contains("queue.starved") && a.input.contains("4 of 4 READY")), "{acts:?}");
    let mut p = row("sp-p", "IN_DELIVERY");
    p.tip = Some("t1".into());
    let conflict = Snapshot { beads: Some(vec![bead("sp-p", &[], "open"), Bead { deps: vec!["sp-p".into()], ..bead("sp-d", &[], "open") }]), rows: Some(vec![p, row("sp-d", "REWORK")]), conflict_markers: [("sp-p".to_string(), ("t1".to_string(), 7_200))].into(), ..idle() };
    let acts = decide(&Carry::default(), &conflict).1;
    assert!(effects(&acts, INCIDENT_EVENT).iter().any(|a| a.input.contains("sp-d") && a.input.contains("stack-conflict")), "{acts:?}");
}

/// A starved plan: open plan work, none ready, none running.
fn starved_plan() -> Snapshot {
    Snapshot { beads: Some(vec![bead("sp-p1", &["plan"], "open")]), rows: Some(vec![row("sp-p1", "READY")]), plan_ready: Some(0), ..idle() }
}

/// CHECK 3: a starved plan recomputes the stale blocked flags; a moving one does not.
#[test]
fn t5_check3_a_starved_plan_recomputes_blocked() {
    assert_eq!(effects(&decide(&Carry::default(), &starved_plan()).1, RECOMPUTE_BLOCKED).len(), 1);
    assert!(effects(&decide(&Carry::default(), &Snapshot { plan_ready: Some(2), ..starved_plan() }).1, RECOMPUTE_BLOCKED).is_empty());
    assert!(effects(&decide(&Carry::default(), &Snapshot { plan_ready: None, ..starved_plan() }).1, RECOMPUTE_BLOCKED).is_empty(), "unknown is not starved");
}

/// CHECK 3c (`open_children.rs`, `red_trackers.rs`): a ready parent with an open child takes
/// the open-children label; a red tracker whose fixes all landed is closed.
#[test]
fn t5_check3c_open_children_and_landed_red_trackers() {
    let snap = Snapshot {
        beads: Some(vec![bead("sp-par", &[], "open"), Bead { parents: vec!["sp-par".into()], ..bead("sp-kid", &[], "open") }, Bead { deps: vec!["sp-fix".into()], ..bead("sp-rt", &["spira-red-tracker"], "open") }, bead("sp-fix", &[], "closed")]),
        rows: Some(vec![row("sp-par", "READY"), row("sp-kid", "WORKING"), row("sp-rt", "READY"), row("sp-fix", "LANDED")]),
        holder_alive: [("sp-kid".to_string(), true)].into(),
        ..idle()
    };
    let acts = decide(&Carry::default(), &snap).1;
    assert!(effects(&acts, CONTENT_LABEL).iter().any(|a| a.key == "sp-par:spira-open-children:add"), "{acts:?}");
    let close = effects(&acts, LC_CLOSE);
    assert!(close.iter().any(|a| a.key == "sp-rt" && a.input.contains("sp-fix")), "{acts:?}");
}

/// CHECK 8 (`judge.rs`): a starved plan where nothing moved is judged once per cooldown, with
/// the open plan beads it is shown; inside the cooldown it is not.
#[test]
fn t5_check8_a_starved_plan_is_judged_once_per_cooldown() {
    // CHECK 3's recompute rides the same pass; it is not progress until it frees a bead.
    let snap = starved_plan();
    let (carry, acts) = decide(&Carry::default(), &snap);
    let reflect = effects(&acts, REFLECT);
    assert_eq!(reflect.len(), 1, "{acts:?}");
    assert!(reflect[0].input.contains("sp-p1"));
    assert_eq!(carry.inference_last_ms, Some(NOW));
    assert!(effects(&decide(&carry, &snap).1, REFLECT).is_empty(), "inside the cooldown");
    let later = Snapshot { now_ms: NOW + 3_600_001, ..snap };
    assert_eq!(effects(&decide(&carry, &later).1, REFLECT).len(), 1, "after it");
}

/// T3: a pilgrimage close that failed drops its marker, so the next pass retries it.
#[test]
fn t3_a_failed_pilgrimage_close_is_retried() {
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, |_| Ok(serde_json::to_string(&pilgrimage_world()).unwrap())).double(PILGRIMAGE_CLOSE, |_| Err("a subscriber could not be reached".into()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, PILGRIMAGE_CLOSE].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    assert_eq!(r.requested().iter().filter(|q| q.name == PILGRIMAGE_CLOSE).count(), 2, "retried on the next pass");
}
