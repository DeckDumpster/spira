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
/// `spira/sim/tests/it/loop_ports.rs`). The assemble effect rebuilds its members from `(id, tip)` alone
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
        .double(ROUND_OPEN, |_| Ok(serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap()));
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

/// A red certify with no `red-output=` still names its suites and verdict line in the abandon
/// act (sp-7yu70y): the Concierge attributes the red from the reason, not from a guess.
#[test]
fn sp_7yu70y_a_red_certify_abandons_naming_its_verdict_and_red_suites() {
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    let out = "queue.sh round certify: round r9 is RED at abc123: test-a.sh,test-b.sh\nred=test-a.sh,test-b.sh\n";
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), out)).unwrap();
    let after = asked_after(&r, n);
    let abandon = after.iter().find(|(e, _)| e == ABANDON).unwrap_or_else(|| panic!("no abandon: {after:?}")).1.clone();
    assert!(abandon.why.contains("round r9 is RED at abc123"), "{}", abandon.why);
    assert!(abandon.why.contains("test-a.sh") && abandon.why.contains("test-b.sh"), "{}", abandon.why);
    assert_eq!(abandon.reds, ["test-a.sh", "test-b.sh"]);
}

#[test]
fn sp_7yu70y_a_red_certify_with_only_a_red_line_or_nothing_still_says_what_it_had() {
    for (out, want, reds) in [("red=test-a.sh\n", "test-a.sh", vec!["test-a.sh"]), ("", "no verdict and no suite", vec![])] {
        let mut r = carried(&driving(State::Certifying, "spira.c3"));
        let n = r.requested().len();
        r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), out)).unwrap();
        let after = asked_after(&r, n);
        let abandon = &after.iter().find(|(e, _)| e == ABANDON).unwrap().1;
        assert!(abandon.why.contains(want), "{}", abandon.why);
        assert_eq!(abandon.reds, reds);
    }
}

// --- sp-8nlvra: a held or skipping repo names why, and a red round's pause ends on a landing ---

/// `spira` with one admissible member, under the pause a red round wrote at base `b0`.
fn paused(base: &str) -> Snapshot {
    let mut s = Snapshot { repos: [("spira".to_string(), repo(vec![member("sp-a", "t1")]))].into(), now_ms: NOW };
    let r = s.repos.get_mut("spira").unwrap();
    r.base_sha = base.into();
    r.paused = Some("local corpus red — the attribution leg ended (Code(2)) with no outcome".into());
    s
}

fn paused_on(base: &str) -> Carry {
    Carry { repos: [("spira".to_string(), RepoCarry { paused_on: Some(base.into()), ..RepoCarry::default() })].into() }
}

/// The 2026-11-07 stall: a red round's pause stood a whole day while spira beads waited, and the
/// pass logged nothing. A pause seen first is held, and the hold names the pause and how it lifts.
#[test]
fn sp_8nlvra_a_paused_repo_is_held_and_names_the_pause() {
    let (carry, acts) = decide(&Carry::default(), &paused("b0"));
    assert!(acts.is_empty(), "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!(rc.state, State::Held);
    assert_eq!(rc.paused_on.as_deref(), Some("b0"), "the base the pause was seen on is carried");
    let why = rc.skip_why.clone().expect("a held repo names why");
    assert!(why.contains("paused") && why.contains("local corpus red") && why.contains("rounds-pace/spira/pause"), "{why}");
}

/// Once a round has landed past the base the pause was seen on, the red it guarded is behind the
/// repo: the pass lifts the pause and cuts the waiting member.
#[test]
fn sp_8nlvra_a_landing_past_the_paused_base_lifts_the_pause_and_cuts() {
    let (carry, acts) = decide(&paused_on("b0"), &paused("b1"));
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [PAUSE, FETCH, ASSEMBLE], "{acts:?}");
    let lift: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(lift.bead, "lift", "{lift:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.paused_on.clone(), rc.skip_why.clone()), (State::Assembling, None, None));
}

/// An operator's deliberate hold (`hold:`) is never lifted by a landing.
#[test]
fn sp_8nlvra_a_deliberate_hold_survives_a_landing() {
    let mut s = paused("b1");
    s.repos.get_mut("spira").unwrap().paused = Some("hold: publish red".into());
    let (carry, acts) = decide(&paused_on("b0"), &s);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Held);
}

/// A scoped stop names its owner's reason.
#[test]
fn sp_8nlvra_a_stopped_repo_names_the_stop() {
    let mut s = busy();
    let r = s.repos.get_mut("spira").unwrap();
    r.stopped = true;
    r.stop_why = Some("concierge: hand fix".into());
    let (carry, _) = decide(&Carry::default(), &s);
    let why = carry.repos["spira"].skip_why.clone().expect("named");
    assert!(why.contains("stopped") && why.contains("concierge: hand fix") && why.contains("world.sh unpause --repo spira --lanes rounds"), "{why}");
}

/// A feature group whose every member waits on an unlanded prerequisite must not starve the
/// catch-all: the blocked are refused first, then the round is selected from what is left.
#[test]
fn sp_8nlvra_a_blocked_feature_group_does_not_starve_the_catch_all() {
    let mut x1 = member("sp-x.1", "t1");
    x1.deps = vec!["sp-z".into()];
    let mut x2 = member("sp-x.2", "t2");
    x2.deps = vec!["sp-z".into()];
    let mut r = repo(vec![x1, x2, member("sp-c", "t3")]);
    r.blockers = vec!["sp-z".into()];
    let s = Snapshot { repos: [("spira".to_string(), r)].into(), now_ms: NOW };
    let (carry, acts) = decide(&Carry::default(), &s);
    let assemble = acts.iter().find(|a| a.effect == ASSEMBLE).unwrap_or_else(|| panic!("the catch-all member cuts: {acts:?} {carry:?}"));
    let leg: Leg = serde_json::from_str(&assemble.input).unwrap();
    assert_eq!(leg.members, [("sp-c".to_string(), "t3".to_string())]);
}

/// Members wait but none is admissible: the pass names each and why.
#[test]
fn sp_8nlvra_a_pool_with_nothing_admissible_names_each_member() {
    let mut s = busy();
    for m in &mut s.repos.get_mut("spira").unwrap().pool {
        m.sifted_tip = Some("old".into());
    }
    let (carry, acts) = decide(&Carry::default(), &s);
    assert!(acts.is_empty(), "{acts:?}");
    let why = carry.repos["spira"].skip_why.clone().expect("named");
    assert!(why.contains("sp-a: no sift pass at its tip t1") && why.contains("sp-b: no sift pass at its tip t2"), "{why}");
}

/// The log owes one line per change of a repo's named reason, and one when it clears.
#[test]
fn sp_8nlvra_a_changed_skip_reason_is_logged_once() {
    let (held, _) = decide(&Carry::default(), &paused("b0"));
    let lines = notes(&Carry::default(), &held);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("batcher.local spira: no cut — ") && lines[0].contains("paused"), "{lines:?}");
    let (again, _) = decide(&held, &paused("b0"));
    assert!(notes(&held, &again).is_empty(), "an unchanged reason is not logged again");
    let (cut, _) = decide(&again, &paused("b1"));
    let lifted = notes(&again, &cut);
    assert!(lifted.len() == 1 && lifted[0].contains("cutting"), "{lifted:?}");
}

// --- sp-822b8l: a refused open re-cuts without the members it named, never abandons ---

fn opening() -> Carry {
    let rc = RepoCarry { state: State::Opened, round_key: Some("k".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into()), ("sp-c".into(), "t3".into())], wt: "/w".into(), base: "b1".into(), ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn open_leg() -> Leg {
    Leg { why: "suites".into(), ..Leg::on("spira") }
}

/// 2026-11-07 05:26Z: four cuts abandoned members-differ because the open would not admit
/// sp-1mspuc. The open now names it (typed), and the batcher opens the others at once.
#[test]
fn sp_822b8l_a_pool_with_one_inadmissible_member_still_opens_a_round_of_the_others() {
    let refused = Opened { refused: vec![("sp-b".into(), "integration".into(), "sp-b: carried by integration sp-int — lands with it, not admitted".into())], ..Opened::default() };
    let (carry, acts) = on_outcome(&opening(), ROUND_OPEN, true, &serde_json::to_string(&refused).unwrap(), &open_leg(), NOW);
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [ROUND_OPEN], "re-opened without the refused member, nothing abandoned: {acts:?}");
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(leg.members, [("sp-a".to_string(), "t1".to_string()), ("sp-c".to_string(), "t3".to_string())]);
    assert_eq!(leg.why, "suites", "the re-open keeps the corpus leg's mark");
    let rc = &carry.repos["spira"];
    assert_eq!(rc.state, State::Opened);
    assert_eq!(rc.refused.get("sp-b"), Some(&("t2".to_string(), "b1".to_string(), "integration".to_string())));
    let (carry, acts) = on_outcome(&carry, ROUND_OPEN, true, &serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap(), &leg, NOW);
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [CORPUS], "{acts:?}");
    assert_eq!(carry.repos["spira"].members.len(), 2);
}

/// The refused member stays out of later cuts at the same tip and base, named; a moved base
/// lets it be tried again.
#[test]
fn sp_822b8l_a_refused_member_stays_out_until_its_tip_or_the_base_moves() {
    let rc = RepoCarry { refused: [("sp-b".to_string(), ("t2".to_string(), "b1".to_string(), "integration".to_string()))].into(), ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let (next, acts) = decide(&carry, &busy());
    let assemble = acts.iter().find(|a| a.effect == ASSEMBLE).expect("sp-a still cuts");
    let leg: Leg = serde_json::from_str(&assemble.input).unwrap();
    assert_eq!(leg.members, [("sp-a".to_string(), "t1".to_string())]);
    assert!(next.repos["spira"].refused.contains_key("sp-b"));
    let mut moved = busy();
    moved.repos.get_mut("spira").unwrap().base_sha = "b2".into();
    let (_, acts) = decide(&carry, &moved);
    let leg: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ASSEMBLE).unwrap().input).unwrap();
    assert_eq!(leg.members.len(), 2, "a moved base tries it again: {leg:?}");
}

/// Every named member refused: no round, no abandon, and the repo is idle with the members out.
#[test]
fn sp_822b8l_an_open_refusing_every_member_abandons_nothing() {
    let all = Opened { refused: ["sp-a", "sp-b", "sp-c"].iter().map(|id| (id.to_string(), "gate-red".to_string(), format!("{id}: its gate is FAIL"))).collect(), ..Opened::default() };
    let (carry, acts) = on_outcome(&opening(), ROUND_OPEN, true, &serde_json::to_string(&all).unwrap(), &open_leg(), NOW);
    assert!(!acts.iter().any(|a| a.effect == ABANDON || a.effect == PAUSE), "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.refused.len()), (State::Idle, 3));
}

// --- sp-tjrjca: a green round refused base-moved is certified again, never abandoned ---

fn landing() -> Carry {
    let mut c = driving(State::Landing, "spira.c1");
    c.repos.get_mut("spira").unwrap().leg = None;
    c
}

/// 2026-11-07 07:24Z: spira-20261107T070615Z certified GREEN stacked on a parent that ended
/// without landing; land answered base-moved (rebuilt on the new base) and the batcher abandoned
/// it. Now it certifies the rebuilt round and lands it.
#[test]
fn sp_tjrjca_a_green_round_refused_base_moved_is_certified_again_and_landed() {
    let moved = serde_json::to_string(&Landed { base_moved: Some("b2".into()), ..Landed::default() }).unwrap();
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let (carry, acts) = on_outcome(&landing(), LAND, true, &moved, &land, NOW);
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [CERTIFY], "re-certified, not abandoned: {acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.recertified_on.as_deref()), (State::Certifying, Some("b2")));
    let unit = serde_json::from_str::<Leg>(&acts[0].input).unwrap().unit;
    assert_ne!(unit, "spira.c1", "a fresh certify leg");

    let mut r = carried(&carry);
    let n = r.requested().len();
    r.tick(STREAM, ended(&unit, runtime::units::Exit::Code(0), "verdict=green\n")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after: Vec<String> = asked_after(&r, n).into_iter().map(|(e, _)| e).collect();
    assert_eq!(after, [LC_CERTIFY, LAND], "green again: it lands");

    let (_, acts) = on_outcome(&Carry { repos: [("spira".to_string(), RepoCarry { state: State::Landing, ..carry.repos["spira"].clone() })].into() }, LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &land, NOW);
    assert!(!acts.iter().any(|a| a.effect == ABANDON), "{acts:?}");
}

/// At most once per base change: base-moved again on the same base is a red, abandoned.
#[test]
fn sp_tjrjca_base_moved_twice_on_one_base_is_red() {
    let mut c = landing();
    c.repos.get_mut("spira").unwrap().recertified_on = Some("b2".into());
    let moved = serde_json::to_string(&Landed { base_moved: Some("b2".into()), ..Landed::default() }).unwrap();
    let (_, acts) = on_outcome(&c, LAND, true, &moved, &Leg { batch: "r9".into(), ..Leg::on("spira") }, NOW);
    assert!(acts.iter().any(|a| a.effect == ABANDON) && !acts.iter().any(|a| a.effect == CERTIFY), "{acts:?}");
}

/// A leg unit gets the base environment a dwarf gets (sp-8nlvra follow-up): without
/// `SPIRA_TOML` the corpus and attribution legs' round-vm refused at once (exit 2) and
/// the batcher abandoned its first live round, spira-20261108T032319Z, after 43 s.
#[test]
fn a_leg_unit_carries_the_release_path_home_and_config_spec() {
    use spira_runtime::tasks::a::batcher_local::edge::leg_env;
    let env = leg_env(vec![("ROUND_BATCH".into(), "b1".into())], "/r", "/h", "/c/cfg", "/x/bin:/usr/bin");
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(get("SPIRA_TOML").as_deref(), Some("/c/cfg"));
    assert_eq!(get("SPIRA_RELEASE").as_deref(), Some("/r"));
    assert_eq!(get("HOME").as_deref(), Some("/h"));
    assert!(get("PATH").is_some_and(|p| p.starts_with("/r/bin:")));
    assert_eq!(get("ROUND_BATCH").as_deref(), Some("b1"), "the leg's own variables are kept");
    let kept = leg_env(vec![("SPIRA_TOML".into(), "/own.toml".into())], "/r", "/h", "/c/cfg", "/x/bin");
    assert_eq!(kept.iter().filter(|(n, _)| n == "SPIRA_TOML").count(), 1);
    assert_eq!(kept.iter().find(|(n, _)| n == "SPIRA_TOML").map(|(_, v)| v.as_str()), Some("/own.toml"));
}

#[test]
fn a_leg_unit_path_keeps_the_release_first_then_the_runtime_path_so_aws_is_found() {
    use spira_runtime::tasks::a::batcher_local::edge::leg_env;
    let path_of = |runtime: &str| leg_env(vec![], "/r", "/h", "/c/cfg", runtime).into_iter().find(|(n, _)| n == "PATH").map(|(_, v)| v).unwrap();
    assert_eq!(path_of("/home/u/.local/lib/spira-env/bin:/usr/bin"), "/r/bin:/r/spira:/home/u/.local/lib/spira-env/bin:/usr/bin");
    assert_eq!(path_of(""), "/r/bin:/r/spira:/usr/local/bin:/usr/bin:/bin", "no runtime PATH falls back to the system dirs");
}
