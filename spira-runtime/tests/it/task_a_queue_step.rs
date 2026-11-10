//! Acceptance T1–T6 for `queue.step`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/queue-step.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide` and `step` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T4 checks the 16 effects; the contract's "18 names" counts `cut_wanted` and
//! `publish_wanted`, which its own table makes Emits with no runner. T6 runs two virtual days
//! (the contract asks seven, over a local and a forge repo).

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: queue_step,
    registration: QueueStep::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
}

fn local() -> Repo {
    Repo { mode: "queue.local".into(), lc_ok: true, base_sha: "b1".into(), ..Repo::default() }
}

fn batch() -> OpenBatch {
    OpenBatch { id: "batch-7".into(), version: 3, pr: Some(88), branch: "spira/batch-7".into(), sealed_head: "h7".into(), members: vec!["sp-a".into()], batcher_owned: true, owner: "batcher".into() }
}

fn with(repo: Repo) -> Snapshot {
    Snapshot { repos: [("spira".to_string(), repo)].into(), world_running: true }
}

/// The contract's idle fixture: one `queue.local` repo, no batch, no publish record, check unchanged.
fn idle() -> Snapshot {
    with(local())
}

/// An open batch whose check is green at the sealed head, base unmoved.
fn busy() -> Snapshot {
    with(Repo { open_batch: Some(batch()), pr_state: PrState::Open, check: Check::Green, sealed_base: Some("b1".into()), ..local() })
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_landing_x_unit_ran_is_refused_naming_landing() {
    let refused = step(State::Landing, Trigger::UnitRan).expect_err("landing × unit.ran is illegal");
    assert!(refused.contains("Landing"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `tests.rs` step cases: green at the sealed head on an unmoved base fast-forwards.
#[test]
fn t5_green_pushes_the_sealed_head() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    assert_eq!(effects(&acts), [PUSH], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Pushing);
}

/// Red on a batcher-owned batch goes to judgement, a unit leg.
#[test]
fn t5_red_goes_to_judgement() {
    let red = with(Repo { check: Check::Red, ..busy().repos["spira"].clone() });
    let (carry, acts) = decide(&Carry::default(), &red);
    assert!(effects(&acts).contains(&JUDGEMENT), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Judging);
}

/// `classify_fault_rerun`: a harness fault reruns once.
#[test]
fn t5_a_harness_fault_reruns() {
    let fault = with(Repo { check: Check::HarnessFault, ..busy().repos["spira"].clone() });
    let (_, acts) = decide(&Carry::default(), &fault);
    assert!(effects(&acts).contains(&RERUN) || effects(&acts).contains(&NOTIFY), "{acts:?}");
}

/// `ops/reconcile.rs:22`: a closed or merged PR is reconciled.
#[test]
fn t5_a_closed_pr_is_reconciled() {
    let closed = with(Repo { pr_state: PrState::Closed, check: Check::None, ..busy().repos["spira"].clone() });
    let (_, acts) = decide(&Carry::default(), &closed);
    assert!(effects(&acts).contains(&RECONCILE), "{acts:?}");
}

/// The contract's States table sends `Observing × no batch, no publish rec` to `Cutting` with a
/// `cut_wanted` Emit, while its T2 makes that same snapshot silent. Silence wins (the soak
/// lesson): the idle repo settles once and then asks for nothing, so a cut is wanted only on a change.
#[test]
fn t5_an_idle_repo_settles_once_then_wants_nothing() {
    let (carry, _) = decide(&Carry::default(), &idle());
    assert_eq!(decide(&carry, &idle()).1, Vec::<Act>::new());
}

/// `Waiting` with an unchanged check records nothing.
#[test]
fn t5_an_unchanged_pending_check_records_once() {
    let pending = with(Repo { check: Check::Pending, ..busy().repos["spira"].clone() });
    let (carry, first) = decide(&Carry::default(), &pending);
    assert!(effects(&first).contains(&RECORD), "{first:?}");
    let (_, again) = decide(&carry, &pending);
    assert!(!effects(&again).contains(&RECORD), "{again:?}");
}

/// The judgement leg's `unit.ran`, on this task's own stream, moves the judging repo at once: a
/// green one is recorded and wants the cut on the base it was judged on; another task's run, or
/// a leg this repo is not judging, changes nothing.
#[test]
fn t3_the_judgement_unit_ran_moves_the_machine_at_once() {
    use runtime::units::{Exit, UnitHandle, UnitRan};
    let rc = RepoCarry { state: State::Judging, leg: Some("spira.j1".into()), base: "b2".into(), mode: "queue.local".into(), batch: Some("b-1:3".into()), ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let ran = |task: &str, key: &str, exit| UnitRan { handle: UnitHandle::of(task, key).unwrap(), exit, duration_ms: 5 };
    let (next, acts) = on_ran(&carry, &ran(NAME, "spira.j1", Exit::Code(0)), 1).expect("its own leg");
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert!(names.contains(&RECORD) && names.contains(&CUT_WANTED) && names.contains(&PUBLISH_WANTED), "{names:?}");
    assert!(acts.iter().any(|a| a.effect == PUBLISH_WANTED && a.key.ends_with("/b2")), "the cut is on the judged base: {acts:?}");
    assert_ne!(next.repos.get("spira").map(|r| r.state), Some(State::Judging));
    let (_, acts) = on_ran(&carry, &ran(NAME, "spira.j1", Exit::Code(1)), 1).expect("its own leg");
    assert!(acts.iter().any(|a| a.effect == NOTIFY), "a failed judgement notifies: {acts:?}");
    assert!(on_ran(&carry, &ran("batcher.local", "spira.j1", Exit::Code(0)), 1).is_none(), "another task's run");
    assert!(on_ran(&carry, &ran(NAME, "spira.other", Exit::Code(0)), 1).is_none(), "a leg not being judged");
}

#[test]
fn the_carry_stays_under_the_journal_column_for_the_longest_repo_names_and_the_rest_wait() {
    let one = busy().repos["spira"].clone();
    let name = |i: usize| format!("{i:04}-{}", "r".repeat(250));
    let snap = Snapshot { repos: (0..600).map(|i| (name(i), one.clone())).collect(), world_running: true };
    let (carry, _) = decide(&Carry::default(), &snap);
    let bytes = serde_json::to_string(&carry).unwrap().len();
    assert!(bytes < 65_535 && !carry.repos.is_empty() && carry.repos.len() < 600, "{} repos, {bytes} bytes", carry.repos.len());
    let (again, _) = decide(&carry, &snap);
    assert!(carry.repos.keys().all(|k| again.repos.contains_key(k)), "a carried repo keeps its entry");
    assert!(serde_json::to_string(&again).unwrap().len() < 65_535);
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

/// The judgement leg ended while the runtime was down, so its `unit.ran` was never heard: the
/// next tick reads the unit through `unit.facts` and the machine moves on what it says.
#[test]
fn t3_a_judgement_whose_unit_ran_was_lost_is_read_through_unit_facts() {
    use runtime::units::{Exit, UnitFacts, UnitState, UNIT_FACTS};
    let rc = RepoCarry { state: State::Judging, leg: Some("spira.j1".into()), base: "b2".into(), mode: "queue.local".into(), batch: Some("b-1:3".into()), ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let facts = serde_json::to_string(&UnitFacts { state: UnitState::Inactive, main_pid: None, exit: Some(Exit::Code(0)), since_ms: Some(1) }).unwrap();
    let mut r = TaskRig::new(1, QueueStep::new(BTreeMap::new()).registration()).unwrap().double(UNIT_FACTS, move |_| Ok(facts.clone()));
    r.tick(STREAM, EventKind::new(STATE, &serde_json::to_string(&carry).unwrap())).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let asked: Vec<&str> = r.requested().iter().map(|q| q.name.as_str()).collect();
    assert!(asked.contains(&UNIT_FACTS), "the tick reads the in-flight leg: {asked:?}");
    assert!(asked.contains(&RECORD), "the exit read from facts judged the leg: {asked:?}");
}
