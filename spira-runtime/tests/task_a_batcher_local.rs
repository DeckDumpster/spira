//! Acceptance T1–T6 for `batcher.local`, instantiated from its contract
//! (`wiki/projects/spira/designs/tasks/batcher-local.md`, Acceptance) on `TaskRig`. Red until the
//! port lands: `decide` and `step` are `unimplemented!`, and the row waits in `tasks::a::PENDING`.
//! T6 runs two virtual days (the contract asks seven, with seeded arrivals and VM reds).

use std::collections::BTreeMap;

use runtime::event::EventKind;

spira_runtime::family_a_acceptance! {
    module: batcher_local,
    registration: BatcherLocal::new(BTreeMap::new()).registration(),
    tick: EventKind::new(TICK, ""),
    first_timer_ms: CADENCE_MS,
    budget: 60,
}

const NOW: u64 = 1_793_000_000_000;

fn member(id: &str, tip: &str) -> Member {
    Member { id: id.into(), state: "SUBMITTED".into(), version: 5, tip: tip.into(), sifted_tip: Some(tip.into()), title: format!("{id} work"), priority: 2, ..Member::default() }
}

fn repo(pool: Vec<Member>) -> Repo {
    let branch_tips = pool.iter().map(|m| (m.id.clone(), m.tip.clone())).collect();
    Repo { pool, branch_tips, base_sha: "b1".into(), ..Repo::default() }
}

/// The contract's idle fixture: the pool empty and no open round.
fn idle() -> Snapshot {
    Snapshot { repos: [("spira".to_string(), repo(vec![]))].into(), now_ms: NOW }
}

/// Two sifted submissions at their tips and no open round: a cut.
fn busy() -> Snapshot {
    Snapshot { repos: [("spira".to_string(), repo(vec![member("sp-a", "t1"), member("sp-b", "t2")]))].into(), now_ms: NOW }
}

#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test]
fn t3_a_foreign_round_leg_in_corpus_is_refused_naming_corpus() {
    let refused = step(State::Corpus, Trigger::ForeignLeg).expect_err("a leg of another round is illegal");
    assert!(refused.contains("Corpus"), "{refused}");
}

#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// T2's other two idle shapes: a held repo and a driven open round are silent too.
#[test]
fn t2_a_held_repo_and_a_driven_round_are_silent() {
    let held = Snapshot { repos: [("spira".to_string(), Repo { stopped: true, ..busy().repos["spira"].clone() })].into(), now_ms: NOW };
    let driven = Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(Round { id: "r1".into(), members: vec!["sp-a".into()], head: "h".into(), worktree: "/w".into(), driven: true }), ..busy().repos["spira"].clone() })].into(), now_ms: NOW };
    for snap in [held, driven] {
        let mut r = rig(1, snap);
        kit::assert_unpoisoned(&r, STREAM);
        assert_silent_tick(&mut r, STREAM);
        kit::assert_unpoisoned(&r, STREAM);
    }
}

/// `screen::cut_pool`, `should_cut` (`core.rs`): a sifted pool cuts — fetch, then assemble.
#[test]
fn t5_a_sifted_pool_is_fetched_then_assembled() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    let order: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(order, [FETCH, ASSEMBLE], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Assembling);
}

/// `pace.rs:77`: a stopped repo holds and writes nothing.
#[test]
fn t5_a_stopped_repo_holds_without_a_write() {
    let held = Snapshot { repos: [("spira".to_string(), Repo { stopped: true, ..busy().repos["spira"].clone() })].into(), now_ms: NOW };
    let (carry, acts) = decide(&Carry::default(), &held);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Held);
}

/// `core.rs` `cut_pool`: a member not at its sifted tip stays out of the cut.
#[test]
fn t5_an_unsifted_tip_is_set_aside() {
    let mut s = busy();
    s.repos.get_mut("spira").unwrap().pool[1].sifted_tip = Some("old".into());
    let (_, acts) = decide(&Carry::default(), &s);
    let assemble = acts.iter().find(|a| a.effect == ASSEMBLE).expect("one member still cuts");
    assert!(assemble.input.contains("sp-a") && !assemble.input.contains("sp-b"), "{assemble:?}");
}

/// A leg's `unit.ran`, delivered on this task's own stream, advances the machine at once: the
/// green corpus starts the certify leg with no tick and no observe in between (the old workaround
/// read the `units` stream at the next 30-s observe).
#[test]
fn t3_a_corpus_unit_ran_on_its_own_stream_starts_certify_at_once() {
    use runtime::units::{Exit, UnitHandle, UnitRan};
    let snaps = std::sync::Mutex::new(vec![busy()]);
    let assembled = Assembled { merged: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "/w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&snaps.lock().unwrap().pop().unwrap_or_else(idle)).unwrap()))
        .double(ASSEMBLE, move |_| Ok(serde_json::to_string(&assembled).unwrap()))
        .double(ROUND_OPEN, |_| Ok(serde_json::to_string(&Opened { batch: "r9".into() }).unwrap()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE, ROUND_OPEN].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let corpus: Leg = serde_json::from_str(&r.requested().iter().find(|q| q.name == CORPUS).expect("the corpus leg started").input).unwrap();
    let asked = r.requested().len();
    let ran = UnitRan { handle: UnitHandle::of(NAME, &corpus.unit).unwrap(), exit: Exit::Code(0), duration_ms: 1_000 };
    r.tick(STREAM, ran.event_kind()).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after: Vec<&str> = r.requested()[asked..].iter().map(|q| q.name.as_str()).collect();
    assert_eq!(after, [CERTIFY], "the green corpus's unit.ran starts certify at once, with no observe between");
}

/// `work.submitted` (sift's hand-off when a pass is recorded) on this task's stream asks for an
/// observe at once, without re-arming the tick.
#[test]
fn t3_work_submitted_wakes_an_observe_at_once() {
    let mut r = rig(1, idle());
    let asked = r.requested().iter().filter(|q| q.name == OBSERVE).count();
    let timer = r.engine().next_timer_at();
    r.tick(STREAM, EventKind::new("work.submitted", r#"{"id":"sp-a","tip":"t1"}"#)).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    assert_eq!(r.requested().iter().filter(|q| q.name == OBSERVE).count(), asked + 1, "the hand-off observes now");
    assert_eq!(r.engine().next_timer_at(), timer, "the tick is not re-armed");
}
