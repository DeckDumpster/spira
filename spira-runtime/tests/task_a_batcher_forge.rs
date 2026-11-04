//! Acceptance T1–T6 for `batcher.forge`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/batcher-forge.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide` and `step` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T6 runs two virtual days (the contract asks seven, with a sim `queue.step`).

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: batcher_forge,
    registration: BatcherForge::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
    budget: 60,
}

const NOW: u64 = 1_793_000_000_000;

fn certified(id: &str, tip: &str) -> Member {
    Member { id: id.into(), state: "CERTIFIED".into(), version: 7, tip: tip.into(), sifted_tip: Some(tip.into()), title: format!("{id} work"), priority: 2, ..Member::default() }
}

fn repo(pool: Vec<Member>) -> Repo {
    let branch_tips = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    Repo { pool, branch_tips, base_sha: "b1".into(), base_confirmed: true, ..Repo::default() }
}

/// The contract's idle fixture: one queue repo, an empty pool, no open batch.
fn idle() -> Snapshot {
    Snapshot { enabled: true, repos: [("deckdumpster".to_string(), repo(vec![]))].into(), now_ms: NOW }
}

/// Two certified beads waiting and no open batch: a cut.
fn busy() -> Snapshot {
    Snapshot { repos: [("deckdumpster".to_string(), repo(vec![certified("dd-a", "t1"), certified("dd-b", "t2")]))].into(), ..idle() }
}

#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_batch_open_in_open_is_refused_naming_open() {
    let refused = step(State::Open, Trigger::BatchOpen).expect_err("a second batch_open is illegal");
    assert!(refused.contains("Open"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// T2's second shape: disabled, with a waiting set already paged, is silent.
#[test]
fn t2_disabled_with_an_unchanged_waiting_set_is_silent() {
    let mut r = rig(1, Snapshot { enabled: false, ..busy() });
    kit::assert_unpoisoned(&r, STREAM);
    assert_silent_tick(&mut r, STREAM);
    kit::assert_unpoisoned(&r, STREAM);
}

/// `main.rs:1235-1254` (restored, see the contract): disabled pages BATCHES HELD once per change of the waiting set.
#[test]
fn t5_disabled_pages_the_held_set_once() {
    let (carry, acts) = decide(&Carry::default(), &Snapshot { enabled: false, ..busy() });
    assert_eq!(acts.iter().filter(|a| a.effect == INBOX_APPEND).count(), 1, "{acts:?}");
    assert_eq!(carry.repos["deckdumpster"].state, State::Disabled);
    assert!(decide(&carry, &Snapshot { enabled: false, ..busy() }).1.is_empty(), "the same waiting set is not paged twice");
}

/// `main.rs:285-288`: an open batch means no cut; its settlement is handed to queue.step.
#[test]
fn t5_an_open_batch_is_handed_to_queue_step_not_cut() {
    let mut s = busy();
    s.repos.get_mut("deckdumpster").unwrap().open_batch = Some(Batch { pr: 12, head: "h1".into(), members: vec!["dd-a".into()] });
    let (_, acts) = decide(&Carry::default(), &s);
    assert!(acts.iter().any(|a| a.effect == SETTLE_WANTED), "{acts:?}");
    assert!(acts.iter().all(|a| a.effect != ASSEMBLE && a.effect != BATCH_OPEN), "{acts:?}");
}

/// The cut: certified members are fetched then assembled.
#[test]
fn t5_a_certified_pool_is_fetched_then_assembled() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    let order: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(order, [FETCH, ASSEMBLE], "{acts:?}");
    assert_eq!(carry.repos["deckdumpster"].state, State::Assembling);
}
