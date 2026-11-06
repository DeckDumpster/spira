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

/// `core.rs` `order_key`, as `batcher rounds` merged a round (`topo_order` of the full members):
/// express first, then priority, so of two siblings that change one file the higher-priority one
/// merges and the other conflicts (sim scenario `conflicting-siblings`, ported to
/// `spira/sim/tests/loop_ports.rs`). The assemble effect rebuilds its members from `(id, tip)` alone
/// (`core_members`: no priority, no stack), so the merge order is the leg's own order.
#[test]
fn t5_the_assemble_leg_lists_its_members_in_merge_order_priority_first() {
    let mut s = busy();
    s.repos.get_mut("spira").unwrap().pool[1].priority = 0;
    let (_, acts) = decide(&Carry::default(), &s);
    let assemble = acts.iter().find(|a| a.effect == ASSEMBLE).expect("a cut");
    let leg: Leg = serde_json::from_str(&assemble.input).expect("an assemble leg");
    let ids: Vec<&str> = leg.members.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["sp-b", "sp-a"], "the priority-0 member merges first: {leg:?}");
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

/// A rig whose carry is `carry` (recorded as the task's own state event), every effect
/// succeeding with `{}` and the observe answering the idle world.
fn carried(carry: &Carry) -> TaskRig {
    let mut r = TaskRig::new(1, registration()).unwrap().double(OBSERVE, |_| Ok(serde_json::to_string(&idle()).unwrap()));
    for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(STATE, &serde_json::to_string(carry).unwrap())).unwrap();
    r
}

/// The repo `spira` in `state`, driving round `r9` on `/w` from base `b1`, its leg `leg`.
fn driving(state: State, leg: &str) -> Carry {
    let rc = RepoCarry { state, round_key: Some("k".into()), batch: Some("r9".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "/w".into(), base: "b1".into(), leg: Some(leg.into()), leg_at_ms: 0, run: 1, ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

/// `leg` of this task ended with `exit`, its stdout `out`.
fn ended(leg: &str, exit: runtime::units::Exit, out: &str) -> EventKind {
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, leg).unwrap(), exit, duration_ms: 1 };
    ran.event_kind_with(&runtime::units::RanOutput::captured(out.as_bytes(), b""))
}

/// The acts requested after `from`, as (effect, leg).
fn asked_after(r: &TaskRig, from: usize) -> Vec<(String, Leg)> {
    r.requested()[from..].iter().filter(|q| q.journaled).map(|q| (q.name.clone(), serde_json::from_str(&q.input).unwrap_or_default())).collect()
}

/// Contract: Corpus × `unit.ran` red → Attributing. The red corpus starts the attribution leg
/// (`batcher attribute`, `drive::attribute_round`) on the round, not a blind re-run.
#[test]
fn t3_a_red_corpus_enters_attribution() {
    let mut r = carried(&driving(State::Corpus, "spira.c1"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c1", runtime::units::Exit::Code(1), "")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, l)| (e.as_str(), l.why.as_str())).collect::<Vec<_>>(), [(CORPUS, "attribute")], "{after:?}");
    assert_eq!(after[0].1.members.len(), 2, "every member is attributed");
}

/// Attributing × `unit.ran`: the attribution's own line decides. Survivors go on to certify
/// (alone, the owners ejected by the leg); a mender pass goes through Mending to certify; a
/// blocked round is Done, red.
#[test]
fn t3_the_attribution_line_decides_certify_mending_or_blocked() {
    use batcher_cut::attribute::Attribution;
    let land = Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: Some(9), mended: false };
    for (line, mended) in [(land.line(), false), (Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: None, mended: true }.line(), true)] {
        let mut r = carried(&driving(State::Attributing, "spira.a2"));
        let n = r.requested().len();
        r.tick(STREAM, ended("spira.a2", runtime::units::Exit::Code(0), &format!("batcher spira: attributing\n{line}\n"))).unwrap();
        kit::assert_unpoisoned(&r, STREAM);
        let after = asked_after(&r, n);
        assert_eq!(after.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(), [CERTIFY], "mended={mended}: {after:?}");
        assert_eq!(after[0].1.members, [("sp-b".to_string(), "t2".to_string())], "the survivors certify");
    }
    let mut r = carried(&driving(State::Attributing, "spira.a2"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.a2", runtime::units::Exit::Code(0), &Attribution::Blocked("unattributed red test-x.sh".into()).line())).unwrap();
    let after: Vec<String> = asked_after(&r, n).into_iter().map(|(e, _)| e).collect();
    assert!(after.contains(&ABANDON.to_string()) && !after.contains(&CERTIFY.to_string()), "blocked is red: {after:?}");
    let mut r = carried(&driving(State::Attributing, "spira.a2"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.a2", runtime::units::Exit::Code(1), "batcher: spira-lc unreachable")).unwrap();
    let after: Vec<String> = asked_after(&r, n).into_iter().map(|(e, _)| e).collect();
    assert!(after.contains(&ABANDON.to_string()), "an attribution with no outcome blocks the round: {after:?}");
}

/// Certifying × `unit.ran` red: the guilty are ejected from the round by the certify output
/// `queue round certify` names (`red-output=`, `io::implicated`), then the round is abandoned.
#[test]
fn t3_a_red_certify_ejects_the_guilty_then_abandons() {
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "verdict=red\nred-output=/run/certify-r9.out\n")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
    let eject = names.iter().position(|e| *e == EJECT).unwrap_or_else(|| panic!("no eject: {names:?}"));
    assert!(names[eject..].contains(&ABANDON), "{names:?}");
    assert_eq!((after[eject].1.why.as_str(), after[eject].1.bead.as_str()), ("/run/certify-r9.out", ""), "the guilty are read from the output");
}
