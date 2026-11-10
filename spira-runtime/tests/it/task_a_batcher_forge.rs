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

/// A corpus leg's `unit.ran`, on this task's own stream, moves the repo at once; another
/// task's run or a leg not carried changes nothing.
#[test]
fn t3_a_corpus_unit_ran_moves_the_machine_at_once() {
    use runtime::units::{Exit, UnitHandle, UnitRan};
    let rc = RepoCarry { state: State::Corpus, leg: Some("spira.c1".into()), ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let ran = |task: &str, key: &str| UnitRan { handle: UnitHandle::of(task, key).unwrap(), exit: Exit::Code(0), duration_ms: 5 };
    let (next, _) = on_ran(&carry, &ran(NAME, "spira.c1"), 1).expect("its own leg");
    assert_ne!(next.repos.get("spira").map(|r| r.state), Some(State::Corpus), "the green corpus moved on");
    assert!(on_ran(&carry, &ran("batcher.local", "spira.c1"), 1).is_none());
    assert!(on_ran(&carry, &ran(NAME, "spira.other"), 1).is_none());
}

/// Contract: Corpus × `unit.ran` red → Attributing, the owners withdrawn by the attribution leg
/// (`batcher attribute` with no open round) and the rest re-run on the survivors.
#[test]
fn t3_a_red_corpus_enters_attribution() {
    use runtime::units::{Exit, UnitHandle, UnitRan};
    let rc = RepoCarry { state: State::Corpus, leg: Some("spira.c1".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], run: 1, ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let (next, acts) = on_ran(&carry, &UnitRan { handle: UnitHandle::of(NAME, "spira.c1").unwrap(), exit: Exit::Code(1), duration_ms: 5 }, 1).expect("its own leg");
    assert_eq!(next.repos["spira"].state, State::Attributing);
    let legs: Vec<(String, spira_runtime::tasks::a::batcher_local::Leg)> = acts.iter().map(|a| (a.effect.clone(), serde_json::from_str(&a.input).unwrap())).collect();
    assert_eq!(legs.iter().map(|(e, l)| (e.as_str(), l.why.as_str())).collect::<Vec<_>>(), [(CORPUS, "attribute")], "{legs:?}");
}

/// Attributing × `unit.ran`: survivors open the batch PR; a blocked round is Idle, verdict red.
#[test]
fn t3_the_attribution_line_opens_or_blocks() {
    use batcher_cut::attribute::Attribution;
    use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};
    let rc = RepoCarry { state: State::Attributing, leg: Some("spira.a2".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], run: 2, ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let ran = UnitRan { handle: UnitHandle::of(NAME, "spira.a2").unwrap(), exit: Exit::Code(0), duration_ms: 5 };
    let land = Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: None, mended: false };
    let (next, acts) = on_ran_with(&carry, &ran, &RanOutput::captured(land.line().as_bytes(), b""), 1).expect("its own leg");
    assert_eq!(next.repos["spira"].state, State::Opening);
    let open: spira_runtime::tasks::a::batcher_local::Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == BATCH_OPEN).expect("the batch opens").input).unwrap();
    assert_eq!(open.members, [("sp-b".to_string(), "t2".to_string())], "the survivors open the PR");
    let (next, acts) = on_ran_with(&carry, &ran, &RanOutput::captured(Attribution::Blocked("unattributed".into()).line().as_bytes(), b""), 1).expect("its own leg");
    assert!(next.repos.get("spira").is_none_or(|r| r.state == State::Idle));
    assert!(acts.iter().any(|a| a.effect == VERDICT_WRITE && a.input.contains("red")), "{acts:?}");
}

fn attributing_with(n: usize) -> Carry {
    let members = (0..n).map(|i| (format!("sp-{i}"), format!("t{i}"))).collect();
    let rc = RepoCarry { state: State::Attributing, leg: Some("spira.a2".into()), leg_at_ms: NOW, members, run: 2, ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn lost_after(n: usize, after_ms: u64) -> bool {
    use spira_runtime::tasks::a::batcher_local::LEG_GRACE_MS;
    let snapshot = Snapshot { enabled: true, repos: [("spira".to_string(), repo(vec![]))].into(), now_ms: NOW + after_ms + LEG_GRACE_MS };
    let (next, _) = decide(&attributing_with(n), &snapshot);
    next.repos.get("spira").is_none_or(|r| r.state != State::Attributing)
}

#[test]
fn an_attribution_leg_is_given_one_wall_for_each_corpus_run_it_may_make() {
    let wall = spira_runtime::tasks::a::batcher_local::Policy::default().wall_ms;
    assert!(!lost_after(3, wall * 5 - 1), "three members allow five runs");
    assert!(lost_after(3, wall * 5 + 1));
    assert!(!lost_after(1, wall * 2), "past one wall, as the audit's 2099s iterations would be");
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

/// A leg that ended while the runtime was down left no `unit.ran`: the tick reads it through
/// `unit.facts`.
#[test]
fn a_tick_reads_every_leg_in_flight_through_unit_facts() {
    use runtime::units::{Exit, UnitFacts, UnitState, UNIT_FACTS};
    let facts = serde_json::to_string(&UnitFacts { state: UnitState::Inactive, main_pid: None, exit: Some(Exit::Code(0)), since_ms: Some(1) }).unwrap();
    for state in [State::Corpus, State::Attributing] {
        let rc = RepoCarry { state, leg: Some("spira.r9.leg.1".into()), ..RepoCarry::default() };
        let carry = Carry { repos: [("spira".to_string(), rc)].into() };
        let facts = facts.clone();
        let mut r = runtime::taskrig::TaskRig::new(1, BatcherForge::new(BTreeMap::new()).registration()).unwrap().double(UNIT_FACTS, move |_| Ok(facts.clone()));
        r.tick(STREAM, EventKind::new(STATE, &serde_json::to_string(&carry).unwrap())).unwrap();
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
        assert!(r.requested().iter().any(|q| q.name == UNIT_FACTS), "{state:?}: {:?}", r.requested());
    }
}
