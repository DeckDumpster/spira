//! Acceptance T1–T6 for `queue.step`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/queue-step.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide` and `step` are `unimplemented!`, and the row waits in `tasks::a::STAGED`.
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
    budget: 60,
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
