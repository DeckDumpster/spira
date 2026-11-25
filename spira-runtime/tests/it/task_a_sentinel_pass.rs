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
    test_ids: [ #[test_id(tmjol7w)] #[test_id(t74m7gn)] #[test_id(tzqbw3l)] #[test_id(tcws2mk)] #[test_id(t0jyubx)] ],
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
        // literal-ok: test fixture label
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

#[test_id(tpfx617)]
#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test_id(thh29x0)]
#[test]
fn t3_another_unit_while_awaiting_landing_is_ignored_and_acting_x_tick_is_refused_naming_acting() {
    assert_eq!(step(State::AwaitingLanding, Trigger::OtherUnitRan), Ok(State::AwaitingLanding));
    let refused = step(State::Acting, Trigger::Tick).expect_err("acting × tick is illegal");
    assert!(refused.to_lowercase().contains("acting"), "{refused}");
}

#[test_id(t57wkc7)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, Snapshot { plan_ready: Some(2), ..busy() });
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `tests.rs` `check2_stops_a_live_holder_before_reclaiming_and_refuses_when_it_will_not_stop`:
/// a stale lease with a live holder stops the holder; no reclaim rides the same pass.
#[test_id(t3o1goo)]
#[test]
fn t5_check2_stops_a_live_holder_before_reclaiming() {
    let (_, acts) = decide(&Carry::default(), &busy());
    assert!(acts.iter().any(|a| a.effect == STOP_HOLDER && a.key == "sp-a"), "{acts:?}");
    assert!(acts.iter().all(|a| !(a.effect == LC_EVENT && a.key.starts_with("sp-a"))), "a live holder is never reclaimed in the pass that stops it: {acts:?}");
}

/// `tests.rs` `close_landed_queue_waiters_reads_the_label_not_bd_status`: a labeled waiter whose
/// own row is LANDED is closed and released by its label and row, whatever bd says (sp-w is
/// LANDED with bd status open); a labeled WORKING one (sp-x) is not closed.
#[test_id(tihr1wi)]
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
#[test_id(tapbn54)]
#[test]
fn t5_check3b_leaves_a_waiter_with_no_lifecycle_row_alone() {
    let snap = Snapshot { beads: Some(vec![Bead { id: "sp-n".into(), title: "no row".into(), labels: vec![QUEUE_WAIT.into()], deps: vec![], status: "open".into(), ..Bead::default() }]), ..idle() };
    let (_, acts) = decide(&Carry::default(), &snap);
    assert!(acts.iter().all(|a| !a.key.starts_with("sp-n")), "a waiter with no row is not touched: {acts:?}");
}

/// The production spellings only: `WORKING` is the leased state (a `CLAIMED` row is not one),
/// and the waiting label has no default — it is the pinned `SPIRA_QUEUE_WAIT_LABEL`.
#[test_id(ts6zydl)]
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
#[test_id(t9afhkw)]
#[test]
fn t5_check7_hands_off_to_the_call() {
    let (_, acts) = decide(&Carry::default(), &Snapshot { plan_ready: Some(3), ..idle() });
    assert!(acts.iter().any(|a| a.effect == CALL_WANTED), "{acts:?}");
}

/// `pass.rs:421-426`, `lifecycle.rs:192-199`: unreadable rows make no lifecycle decision.
#[test_id(t87k3mw)]
#[test]
fn t5_unreadable_rows_decide_nothing_about_leases() {
    let (_, acts) = decide(&Carry::default(), &Snapshot { rows: None, ..busy() });
    assert!(acts.iter().all(|a| ![STOP_HOLDER, LC_EVENT, LC_FACT].contains(&a.effect.as_str())), "{acts:?}");
}

/// CHECK 7's hand-off lands on sentinel-call's own stream (`Command::EmitTo`), where its handler
/// hears it; on this task's stream nobody would.
#[test_id(tvom4fm)]
#[test]
fn t5_check7_call_wanted_is_delivered_to_the_call() {
    let r = rig(1, Snapshot { plan_ready: Some(3), ..idle() });
    kit::assert_unpoisoned(&r, STREAM);
    let to: Vec<String> = r.journaled().into_iter().filter(|e| e.kind.tag == CALL_WANTED).map(|e| e.stream.0).collect();
    assert_eq!(to, [spira_runtime::tasks::a::sentinel_call::STREAM], "call.wanted goes to sentinel-call's stream");
}

/// CHECK 6's leg starts under the properties `systemd-run` gave it (`dispatch.rs`): the pinned
/// `SPIRA_LAND_MAXSEC` as `RuntimeMaxSec`, both streams appended to `landing.log`.
#[test_id(t1ztxqn)]
#[test]
fn t5_check6_the_landing_leg_carries_its_runtime_bound_and_its_log() {
    let (_, acts) = decide(&Carry::default(), &Snapshot { landing_active: false, land_maxsec: 900, ..submitted() });
    let start = acts.iter().find(|a| a.effect == LANDING_START).expect("the landing leg starts");
    let w: Write = serde_json::from_str(&start.input).unwrap();
    let props = landing_props(&w, std::path::Path::new("run/spira"));
    assert_eq!(props.runtime_max_ms, Some(900_000));
    assert_eq!(props.output_append.as_deref(), Some(std::path::Path::new("run/spira/landing.log")));
    assert_eq!(landing_props(&Write::default(), std::path::Path::new("r")).runtime_max_ms, None, "0 is no bound");
}

fn submitted() -> Snapshot {
    let mut snap = idle();
    snap.rows.as_mut().unwrap().push(Row { id: "sp-s".into(), state: "SUBMITTED".into(), version: 1, ..Row::default() });
    snap
}

#[test_id(tzghix6)]
#[test]
fn t5_check6_an_idle_snapshot_starts_no_landing_leg_and_a_submitted_row_starts_one() {
    let idle_now = Snapshot { landing_active: false, ..idle() };
    assert!(effects(&decide(&Carry::default(), &idle_now).1, LANDING_START).is_empty());
    for state in ["SUBMITTED", "CERTIFIED", "IN_DELIVERY"] {
        let mut snap = idle_now.clone();
        snap.rows.as_mut().unwrap().push(Row { id: "sp-s".into(), state: state.into(), version: 1, ..Row::default() });
        assert_eq!(effects(&decide(&Carry::default(), &snap).1, LANDING_START).len(), 1, "{state}");
    }
}

#[test_id(t0zifc9)]
#[test]
fn t5_check6_an_idle_snapshot_does_not_escalate_for_the_age_of_the_leg() {
    let idle_stale = Snapshot { rows: idle().rows, ..stale() };
    let (_, acts) = decide(&Carry::default(), &idle_stale);
    assert!(asks(&acts).is_empty() && effects(&acts, LANDING_START).is_empty());
}

#[test_id(tx082tt)]
#[test]
fn t5_check6_unreadable_rows_still_start_the_landing_leg() {
    let snap = Snapshot { landing_active: false, rows: None, ..idle() };
    assert_eq!(effects(&decide(&Carry::default(), &snap).1, LANDING_START).len(), 1);
}

fn stale() -> Snapshot {
    Snapshot { landing_active: false, land_age_secs: Some(7_200), land_stale_secs: 1_800, land_escalate_every_secs: 3_600, land_log_tail: "gate red".into(), ..submitted() }
}

fn asks(acts: &[Act]) -> Vec<Act> {
    effects(acts, ASK)
}

#[test_id(tuy6li3)]
#[test]
fn t5_check6_a_leg_that_completed_nothing_within_the_stale_window_asks_the_concierge_once() {
    let (next, acts) = decide(&Carry::default(), &stale());
    let asked = asks(&acts);
    assert_eq!(asked.len(), 1);
    let w: Write = serde_json::from_str(&asked[0].input).unwrap();
    assert!(w.subject.starts_with("Spira is landing nothing"), "{}", w.subject);
    assert!(w.text.contains("## Question") && w.text.contains("## Default") && w.text.contains("gate red"), "{}", w.text);
    assert!(w.cause.contains("landing-pass land") && w.id.is_empty());
    assert_eq!(next.land_escalated_at_ms, Some(NOW));
}

#[test_id(tmr8f04)]
#[test]
fn t5_check6_the_land_escalation_waits_out_its_cooldown() {
    let carry = Carry { land_escalated_at_ms: Some(NOW - 3_599_000), ..Carry::default() };
    let (next, acts) = decide(&carry, &stale());
    assert!(asks(&acts).is_empty());
    assert_eq!(next.land_escalated_at_ms, carry.land_escalated_at_ms);
    let carry = Carry { land_escalated_at_ms: Some(NOW - 3_600_000), ..Carry::default() };
    assert_eq!(asks(&decide(&carry, &stale()).1).len(), 1, "at the cooldown it asks again");
}

#[test_id(t8u1ahi)]
#[test]
fn t5_check6_no_land_escalation_while_the_leg_runs_or_a_landing_is_fresh() {
    assert!(asks(&decide(&Carry::default(), &Snapshot { landing_active: true, ..stale() }).1).is_empty());
    assert!(asks(&decide(&Carry::default(), &Snapshot { land_age_secs: Some(1_800), ..stale() }).1).is_empty());
    assert!(asks(&decide(&Carry::default(), &Snapshot { land_age_secs: None, ..stale() }).1).is_empty());
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

/// An epic of the partition whose every child is over, and one still open. A work child is
/// over when its lifecycle row is terminal, never by its bd status (038cf5ca2), so each child
/// carries a LANDED row.
fn pilgrimage_world() -> Snapshot {
    let epic = Bead { kind: "epic".into(), ..bead("sp-e", &["spira"], "open") };
    let kids = [Bead { parents: vec!["sp-e".into()], ..bead("sp-c1", &[], "closed") }, Bead { parents: vec!["sp-e".into()], ..bead("sp-c2", &[], "closed") }];
    let other = Bead { kind: "epic".into(), ..bead("sp-o", &["gastown"], "open") };
    let base = idle();
    let rows = [base.rows.clone().unwrap_or_default(), vec![row("sp-c1", "LANDED"), row("sp-c2", "LANDED")]].concat();
    Snapshot { beads: Some([vec![epic, other], kids.to_vec()].concat()), rows: Some(rows), ..base }
}

/// CHECK 1 (`pilgrimage.sh check`): a complete epic of the partition is announced once and
/// closed; one outside the partition is not; a live unlanded child branch defers it; one that
/// grew again loses its marker.
#[test_id(thmdzbk)]
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
#[test_id(trtupls)]
#[test]
fn t5_check2b_stranded_work_runs_strand() {
    assert!(effects(&decide(&Carry::default(), &idle()).1, STRAND).is_empty(), "nothing stranded, no act");
    let snap = Snapshot { strand: vec!["builder\tbead\tsp-s\tWORKING with no dwarf".into()], ..idle() };
    assert_eq!(effects(&decide(&Carry::default(), &snap).1, STRAND).len(), 1);
}

/// CHECK 2c (`lifecycle.rs` check2c): rows whose holder and state disagree are detected,
/// recorded in the carry (journaled when the finding changes), and nothing is written.
#[test_id(thzwuar)]
#[test]
fn t5_check2c_inconsistent_rows_are_recorded_not_acted_on() {
    let snap = Snapshot { rows: Some(vec![row("sp-i", "WORKING"), Row { holder: Some("dwarf-9".into()), ..row("sp-l", "LANDED") }]), ..idle() };
    let (carry, acts) = decide(&Carry::default(), &snap);
    assert_eq!(carry.inconsistent.len(), 2, "{:?}", carry.inconsistent);
    assert!(acts.iter().all(|a| !a.key.starts_with("sp-i") && !a.key.starts_with("sp-l")), "{acts:?}");
}

/// CHECK 2d (`holds.rs`): an ask hold whose ask is gone is withdrawn after the grace; a held
/// share of READY over a quarter is announced; a stale stack-conflict marker is announced.
#[test_id(tqzbhd7)]
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
#[test_id(tph3z1i)]
#[test]
fn t5_check3_a_starved_plan_recomputes_blocked() {
    assert_eq!(effects(&decide(&Carry::default(), &starved_plan()).1, RECOMPUTE_BLOCKED).len(), 1);
    assert!(effects(&decide(&Carry::default(), &Snapshot { plan_ready: Some(2), ..starved_plan() }).1, RECOMPUTE_BLOCKED).is_empty());
    assert!(effects(&decide(&Carry::default(), &Snapshot { plan_ready: None, ..starved_plan() }).1, RECOMPUTE_BLOCKED).is_empty(), "unknown is not starved");
}

/// CHECK 3c (`open_children.rs`, `red_trackers.rs`): a ready parent with an open child takes
/// the open-children label; a red tracker whose fixes all landed is closed.
#[test_id(t77kv8z)]
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
#[test_id(tffxvzt)]
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
#[test_id(tap7n4v)]
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

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test_id(t7737z9)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = kit::assert_kept_as_rows(&at_work(), NAME);
    assert!(rows.iter().any(|k| k == "state"), "{rows:?}");
}

#[test_id(tl3vdas)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}

#[test_id(txh98t7)]
#[test]
fn the_landing_log_tail_of_a_big_log_with_invalid_utf8_in_its_first_line_is_its_last_30_lines() {
    let dir = testkit::TempDir::new("sentinel-land-tail");
    let mut bytes = vec![0xff, 0xfe, b'\n'];
    for i in 0..10_000 {
        bytes.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    assert!(bytes.len() > 64 * 1024);
    std::fs::write(dir.path().join("landing.log"), bytes).unwrap();
    let tail = land_log_tail(dir.path());
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 30, "{tail}");
    assert_eq!(lines[29], "line 9999");
    assert_eq!(lines[0], "line 9970");
    assert!(land_log_tail(&dir.path().join("none")).contains("no landing log"));
}

mod landing_log_rotation {
    use super::*;
    use runtime::event::EffectSpec;
    use runtime::units::{ScriptedUnits, UnitFacts, UnitHandle, UnitState};
    use std::sync::Arc;

    const MB: usize = 1024 * 1024;

    fn fixture(name: &str) -> (testkit::TempDir, spira_runtime::Effects, Arc<ScriptedUnits>) {
        let dir = testkit::TempDir::new(name);
        let units = Arc::new(ScriptedUnits::new(Arc::new(
            runtime::ports::VirtualClock::new(0),
        )));
        units.script(NAME, 1_000, runtime::units::Exit::Code(0));
        let mut e = spira_runtime::Effects::default();
        e.world.run_dir = dir.path().to_path_buf();
        e.units = Some(units.clone());
        (dir, e, units)
    }

    /// The leg starts, or stops only for want of the `landing-pass` binary, which comes after
    /// rotation: a rotation that stopped the leg would name itself.
    fn reached_the_binary_lookup(r: &Result<String, String>) -> bool {
        r.is_ok() || r.as_ref().is_err_and(|e| e.contains("landing-pass not found"))
    }

    fn start(e: &spira_runtime::Effects, cap_mb: u64, keep: u64) -> Result<String, String> {
        let w = Write {
            log_cap_mb: cap_mb,
            log_keep: keep,
            ..Write::default()
        };
        landing_start(
            e,
            &EffectSpec::keyed(
                LANDING_START,
                LANDING_UNIT,
                serde_json::to_string(&w).unwrap(),
                10_000,
            ),
        )
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut n: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("landing.log"))
            .collect();
        n.sort();
        n
    }

    #[test_id(tv0zaq3)]
    #[test]
    fn the_landing_leg_carries_the_pinned_log_cap_and_keep() {
        let (_, acts) = decide(
            &Carry::default(),
            &Snapshot {
                landing_active: false,
                land_log_cap_mb: 7,
                land_log_keep: 5,
                ..submitted()
            },
        );
        let start = acts.iter().find(|a| a.effect == LANDING_START).unwrap();
        let w: Write = serde_json::from_str(&start.input).unwrap();
        assert_eq!((w.log_cap_mb, w.log_keep), (7, 5));
    }

    #[test_id(tvekfuc)]
    #[test]
    fn a_log_at_the_cap_becomes_dot_one_and_only_keep_files_remain() {
        let (dir, e, _) = fixture("land-rotate-over");
        std::fs::write(dir.path().join("landing.log"), vec![b'n'; MB]).unwrap();
        std::fs::write(dir.path().join("landing.log.1"), "one").unwrap();
        std::fs::write(dir.path().join("landing.log.2"), "two").unwrap();
        std::fs::write(dir.path().join("landing.log.3"), "three").unwrap();
        assert!(reached_the_binary_lookup(&start(&e, 1, 3)));
        assert_eq!(names(dir.path()), ["landing.log.1", "landing.log.2"]);
        assert_eq!(
            std::fs::read(dir.path().join("landing.log.1"))
                .unwrap()
                .len(),
            MB
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("landing.log.2")).unwrap(),
            "one"
        );
    }

    #[test_id(tm92wxr)]
    #[test]
    fn a_log_under_the_cap_is_left_alone() {
        let (dir, e, _) = fixture("land-rotate-under");
        std::fs::write(dir.path().join("landing.log"), "small").unwrap();
        assert!(reached_the_binary_lookup(&start(&e, 1, 3)));
        assert_eq!(names(dir.path()), ["landing.log"]);
    }

    #[test_id(t5i3emp)]
    #[test]
    fn a_leg_in_flight_is_left_unrotated() {
        let (dir, e, units) = fixture("land-rotate-inflight");
        std::fs::write(dir.path().join("landing.log"), vec![b'n'; MB]).unwrap();
        let unit = UnitHandle::of(NAME, LANDING_KEY).unwrap();
        units.install(
            &unit.name,
            UnitFacts {
                state: UnitState::Active,
                main_pid: None,
                exit: None,
                since_ms: Some(0),
            },
        );
        assert!(start(&e, 1, 3).is_err());
        assert_eq!(names(dir.path()), ["landing.log"]);
    }

    #[test_id(t78s9y6)]
    #[test]
    fn a_rotation_that_fails_does_not_stop_the_leg() {
        let (dir, e, _) = fixture("land-rotate-fails");
        std::fs::write(dir.path().join("landing.log"), vec![b'n'; MB]).unwrap();
        let original = std::fs::metadata(dir.path()).unwrap().permissions();
        let mut ro = original.clone();
        ro.set_readonly(true);
        std::fs::set_permissions(dir.path(), ro).unwrap();
        let started = start(&e, 1, 3);
        std::fs::set_permissions(dir.path(), original).unwrap();
        assert!(reached_the_binary_lookup(&started), "{started:?}");
        assert_eq!(names(dir.path()), ["landing.log"]);
    }
}

/// A WORKING builder on `sp-b` stacked at `tip`, over a prerequisite `sp-a` at `a_tip`.
fn stacked(a_state: &str, a_tip: &str, red: bool, b_state: &str, stacked_at: &str) -> Snapshot {
    let a = Row {
        id: "sp-a".into(),
        state: a_state.into(),
        version: 2,
        tip: Some(a_tip.into()),
        known_red: red,
        ..Row::default()
    };
    let b = Row {
        id: "sp-b".into(),
        state: b_state.into(),
        version: 4,
        holder: Some("dwarf-2".into()),
        lease_until_ms: Some(NOW + 600_000),
        stack: vec!["sp-a".into()],
        stack_tips: [("sp-a".to_string(), stacked_at.to_string())].into(),
        ..Row::default()
    };
    Snapshot {
        rows: Some(vec![a, b]),
        holder_alive: [("sp-b".to_string(), true)].into(),
        ..idle()
    }
}

#[test_id(t8knsfh)]
#[test]
fn a_working_dependent_on_a_red_tip_gets_one_stop_doomed() {
    let (_, acts) = decide(
        &Carry::default(),
        &stacked("CERTIFIED", "t1", true, "WORKING", "t1"),
    );
    let stops = effects(&acts, STOP_DOOMED);
    assert_eq!(stops.len(), 1, "{acts:?}");
    assert_eq!(stops[0].key, "sp-b:sp-a:t1");
    assert!(
        stops[0]
            .input
            .contains("stopped: stacked on sp-a at t1, which a round or gate found red"),
        "{}",
        stops[0].input
    );
}

#[test_id(tgtac4f)]
#[test]
fn no_stop_doomed_for_a_blocker_not_red_a_moved_tip_or_a_dependent_not_working() {
    for (snap, why) in [
        (
            stacked("CERTIFIED", "t1", false, "WORKING", "t1"),
            "blocker not red",
        ),
        (
            stacked("CERTIFIED", "t2", true, "WORKING", "t1"),
            "tip moved",
        ),
        (
            stacked("CERTIFIED", "t1", true, "READY", "t1"),
            "dependent not working",
        ),
    ] {
        let (_, acts) = decide(&Carry::default(), &snap);
        assert!(effects(&acts, STOP_DOOMED).is_empty(), "{why}: {acts:?}");
    }
}

#[test_id(tc0fofv)]
#[test]
fn unreadable_rows_decide_no_stop_doomed() {
    let (_, acts) = decide(
        &Carry::default(),
        &Snapshot {
            rows: None,
            ..stacked("CERTIFIED", "t1", true, "WORKING", "t1")
        },
    );
    assert!(effects(&acts, STOP_DOOMED).is_empty(), "{acts:?}");
}

mod stop_doomed_effect {
    use super::*;
    use runtime::event::EffectSpec;
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    fn fixture(shown: Arc<Mutex<serde_json::Value>>) -> (testkit::TempDir, spira_runtime::Effects) {
        let dir = testkit::TempDir::new("sentinel-stop-doomed");
        let sock = dir.path().join("lc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            runtime::socket::answer(&listener, &|argv| Ok(shown.lock().unwrap()[argv[1].as_str()].to_string()))
        });
        let run = dir.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let far = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3_600;
        std::fs::write(run.join("dwarf-1-sp-b.pid"), "1\n").unwrap();
        std::fs::write(run.join("dwarf-1-sp-b.lease"), format!("{far}\n")).unwrap();
        let mut e = spira_runtime::Effects::default();
        e.world.run_dir = run;
        e.world.lc_socket = sock;
        (dir, e)
    }

    fn row(state: &str, tip: &str) -> serde_json::Value {
        serde_json::json!({
            "sp-b": { "bead": { "state": state, "stack": { "sp-a": tip } } },
            "sp-a": { "bead": { "state": "CERTIFIED", "tip": "t1", "known_red": 1 } },
        })
    }

    fn stop(e: &spira_runtime::Effects, why: &str) -> Result<String, String> {
        let w = Write {
            id: "sp-b".into(),
            label: "sp-a".into(),
            cause: "t1".into(),
            text: why.into(),
            ..Write::default()
        };
        stop_doomed(
            e,
            &EffectSpec::keyed(
                STOP_DOOMED,
                "sp-b:sp-a:t1",
                serde_json::to_string(&w).unwrap(),
                5_000,
            ),
        )
    }

    #[test_id(tsd2uwx)]
    #[test]
    fn a_stop_is_written_once_and_a_row_or_a_prerequisite_that_moved_is_refused() {
        let shown = Arc::new(Mutex::new(row("WORKING", "t1")));
        let (_dir, e) = fixture(shown.clone());
        let file = dwarf::stop::request_file(&e.world.run_dir, "sp-b");
        stop(
            &e,
            "stopped: stacked on sp-a at t1, which a round or gate found red",
        )
        .unwrap();
        assert_eq!(
            dwarf::stop::reason(&e.world.run_dir, "sp-b"),
            "stopped: stacked on sp-a at t1, which a round or gate found red"
        );
        let first = std::fs::metadata(&file).unwrap().modified().unwrap();
        stop(&e, "a different reason").unwrap();
        assert_eq!(
            dwarf::stop::reason(&e.world.run_dir, "sp-b"),
            "stopped: stacked on sp-a at t1, which a round or gate found red",
            "a pending request is left alone"
        );
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), first);

        dwarf::stop::clear(&e.world.run_dir, "sp-b");
        let mut recovered = row("WORKING", "t1");
        recovered["sp-a"]["bead"]["known_red"] = serde_json::json!(0);
        let mut retipped = row("WORKING", "t1");
        retipped["sp-a"]["bead"]["tip"] = serde_json::json!("t2");
        for moved in [row("READY", "t1"), row("WORKING", "t2"), recovered, retipped] {
            *shown.lock().unwrap() = moved;
            assert!(stop(&e, "x").is_err());
            assert!(!dwarf::stop::requested(&e.world.run_dir, "sp-b"));
        }
    }
}

fn reserved(owner: &str, gone: bool) -> Snapshot {
    let row = Row { id: "sp-a".into(), state: "READY".into(), version: 5, takeover_owner: Some(owner.into()), ..Row::default() };
    Snapshot { rows: Some(vec![row]), session_gone: [(owner.to_string(), gone)].into(), ..idle() }
}

#[test_id(tz4p8vd)]
#[test]
fn t5_check2_ends_a_takeover_whose_owner_session_is_gone_with_a_note_and_keeps_a_live_owners() {
    let (_, acts) = decide(&Carry::default(), &reserved("direct@1.2", true));
    let end = acts.iter().find(|a| a.effect == LC_EVENT && a.key.starts_with("sp-a")).unwrap_or_else(|| panic!("{acts:?}"));
    assert!(end.input.contains("TakeoverEnd") && end.input.contains("direct@1.2"), "{end:?}");
    assert!(acts.iter().any(|a| a.effect == CONTENT_NOTE && a.input.contains("direct@1.2") && a.input.contains("gone")), "{acts:?}");
    let (_, acts) = decide(&Carry::default(), &reserved("direct@1.2", false));
    assert!(acts.iter().all(|a| !(a.effect == LC_EVENT && a.key.starts_with("sp-a"))), "a live owner's takeover stays: {acts:?}");
}
