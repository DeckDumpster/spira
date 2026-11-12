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
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// T2's other two idle shapes: a held repo and a driven open round are silent too.
#[test]
fn t2_a_held_repo_and_a_driven_round_are_silent() {
    let held = Snapshot { repos: [("spira".to_string(), Repo { stopped: true, ..busy().repos["spira"].clone() })].into(), now_ms: NOW };
    let driven = Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(Round { id: "r1".into(), members: vec!["sp-a".into()], head: "h".into(), worktree: "/w".into(), driven: true, ..Round::default() }), ..busy().repos["spira"].clone() })].into(), now_ms: NOW };
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
    assert_eq!(after[0].1.after_pass, "/w/.round-results-spira.c1", "the attribution reads the red pass's results");
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
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
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
    let after = asked_after(&r, n);
    let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
    assert!(!names.contains(&ABANDON) && !names.contains(&EJECT) && !names.contains(&WITHDRAW), "an attribution with no outcome judged nothing, so the round stays open: {names:?}");
    let pause = after.iter().find(|(e, _)| e == PAUSE).unwrap_or_else(|| panic!("no pause: {after:?}"));
    assert!(pause.1.why.contains("batcher: spira-lc unreachable"), "the leg's own failure text is the pause's reason: {}", pause.1.why);
}

/// A round whose VM could not be acquired never ran: the attribution leg says so as a fault, and
/// the repo cuts again at once — no pause for an operator to lift (sp-lgffas).
#[test]
fn sp_lgffas_a_fault_in_the_attribution_leg_re_cuts_on_the_next_tick_without_a_pause() {
    use batcher_cut::attribute::Attribution;
    let fault = Attribution::Fault("the attribution did not finish: round-vm: harness fault — round-vm run: acquire failed".into()).line();
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, "spira.a2").unwrap(), exit: runtime::units::Exit::Code(0), duration_ms: 1 };
    let out = runtime::units::RanOutput::captured(fault.as_bytes(), b"");
    let (carry, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran, &out, NOW).expect("its own leg");
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert!(names.contains(&ABANDON) && !names.contains(&PAUSE) && !names.contains(&CERTIFY), "{names:?}");
    assert_eq!(carry.repos["spira"].pace.infra, 0, "a fault with no VM does not count toward a stop");

    let (_, acts) = decide(&carry, &busy());
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [FETCH, ASSEMBLE], "cut again on the next tick");
}

/// A leg whose reruns faulted is not a red: no abandon, no pause, and the page says so.
#[test]
fn sp_xslgtq_a_faulted_attribution_leaves_the_round_open_and_pages_attribution_faulted() {
    use batcher_cut::attribute::Attribution;
    let why = "attribution faulted: its reruns of test-sim-scenarios.sh could not run";
    let line = Attribution::Faulted(why.into()).line();
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, "spira.a2").unwrap(), exit: runtime::units::Exit::Code(0), duration_ms: 1 };
    let out = runtime::units::RanOutput::captured(line.as_bytes(), b"");
    let (_, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran, &out, NOW).expect("its own leg");
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert!(!names.contains(&ABANDON) && !names.contains(&PAUSE) && !names.contains(&CERTIFY), "{names:?}");
    let page = acts.iter().find(|a| a.effect == INBOX_APPEND).unwrap_or_else(|| panic!("no page: {names:?}"));
    assert!(page.input.contains("ATTRIBUTION FAULTED") && page.input.contains("test-sim-scenarios.sh") && !page.input.contains("unattributed"), "{}", page.input);
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

fn attributed(mut c: Carry) -> Carry {
    c.repos.get_mut("spira").unwrap().attributed = true;
    c
}

fn sole_member(mut c: Carry) -> Carry {
    c.repos.get_mut("spira").unwrap().members = vec![("sp-a".into(), "t1".into())];
    c
}

/// A red `queue round certify` of a one-member round ejects that member with the red output as
/// its evidence and writes no pause, whether or not the output names the member's files.
#[test]
fn sp_z43b7y_a_red_certify_of_a_one_member_round_ejects_the_member_and_writes_no_pause() {
    for out in ["verdict=red\nred-output=/run/rounds/r9.results/testenv.out\n", "queue.sh round certify: round r9 is RED at abc: test-a.sh\nred=test-a.sh\n", ""] {
        let mut r = carried(&{
            let mut c = sole_member(driving(State::Certifying, "spira.c3"));
            c.repos.get_mut("spira").unwrap().attributed = true;
            c
        });
        let n = r.requested().len();
        r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), out)).unwrap();
        kit::assert_unpoisoned(&r, STREAM);
        let after = asked_after(&r, n);
        let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
        assert!(!names.contains(&PAUSE), "{out:?}: {names:?}");
        let eject = after.iter().find(|(e, _)| e == EJECT).unwrap_or_else(|| panic!("{out:?}: no eject: {names:?}")).1.clone();
        assert_eq!((eject.bead.as_str(), eject.head.as_str()), ("sp-a", "t1"), "{out:?}");
        if out.contains("red-output=") {
            assert!(eject.why.contains("/run/rounds/r9.results/testenv.out"), "{}", eject.why);
        }
        assert!(names.contains(&ABANDON), "{names:?}");
    }
}

/// The sole-member abandon follows its eject, so it may say one was made; a multi-member abandon
/// that ejected nothing at decision time claims no count.
#[test]
fn sp_b0lhx6_an_abandon_carries_the_ejects_the_decision_made() {
    let mut r = carried(&attributed(sole_member(driving(State::Certifying, "spira.c3"))));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let after = asked_after(&r, n);
    assert_eq!(after.iter().find(|(e, _)| e == ABANDON).unwrap().1.ejected, Some(1));
    let mut r = carried(&attributed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let after = asked_after(&r, n);
    assert_eq!(after.iter().find(|(e, _)| e == ABANDON).unwrap().1.ejected, None);
}

/// Two members and no file named: the red has no culprit to eject, so the pause stands for a person.
#[test]
fn sp_z43b7y_a_red_certify_of_a_two_member_round_naming_no_member_still_pauses() {
    let mut c = driving(State::Certifying, "spira.c3");
    c.repos.get_mut("spira").unwrap().attributed = true;
    let mut r = carried(&c);
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let names: Vec<String> = asked_after(&r, n).into_iter().map(|(e, _)| e).collect();
    assert!(names.contains(&PAUSE.to_string()) && !names.contains(&EJECT.to_string()), "{names:?}");
}

/// Every other path that pauses blames no member, so its pause carries the reason a person acts on.
#[test]
fn sp_z43b7y_a_pause_without_a_culprit_carries_the_reason_a_person_acts_on() {
    let mut r = carried(&sole_member(driving(State::Attributing, "spira.a2")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.a2", runtime::units::Exit::Code(1), "batcher: spira-lc unreachable")).unwrap();
    let after = asked_after(&r, n);
    let pause = after.iter().find(|(e, _)| e == PAUSE).unwrap_or_else(|| panic!("no pause: {after:?}"));
    assert!(pause.1.why.contains("attribution leg ended"), "{}", pause.1.why);
    assert!(!after.iter().any(|(e, _)| e == EJECT), "no culprit is named: {after:?}");
}

/// A red certify with no `red-output=` still names its suites and verdict line in the abandon
/// act (sp-7yu70y): the Concierge attributes the red from the reason, not from a guess.
#[test]
fn sp_7yu70y_a_red_certify_abandons_naming_its_verdict_and_red_suites() {
    let mut r = carried(&attributed(driving(State::Certifying, "spira.c3")));
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
fn sp_7yu70y_a_red_certify_with_only_a_red_line_still_says_what_it_had() {
    let mut r = carried(&attributed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let after = asked_after(&r, n);
    let abandon = &after.iter().find(|(e, _)| e == ABANDON).unwrap().1;
    assert!(abandon.why.contains("test-a.sh"), "{}", abandon.why);
    assert_eq!(abandon.reds, ["test-a.sh"]);
}

/// A certify red naming no suite is attributed, once per round, before it is abandoned whole
/// (sp-7yu70y.2): the second is a fault of the machinery, not a red.
#[test]
fn sp_7yu70y_2_an_unnamed_certify_red_attributes_once_then_faults_without_a_red() {
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "queue.sh round certify: nothing useful\n")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, l)| (e.as_str(), l.why.as_str())).collect::<Vec<_>>(), [(CORPUS, "attribute")], "{after:?}");
    let attribution_unit = after[0].1.unit.clone();

    let land = batcher_cut::attribute::Attribution::Land { members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], attribution_secs: None, mended: false }.line();
    let n = r.requested().len();
    r.tick(STREAM, ended(&attribution_unit, runtime::units::Exit::Code(0), &format!("{land}\n"))).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(), [CERTIFY], "{after:?}");
    let certify_unit = after[0].1.unit.clone();

    let n = r.requested().len();
    r.tick(STREAM, ended(&certify_unit, runtime::units::Exit::Code(1), "queue.sh round certify: refused, nothing ran\n")).unwrap();
    let after = asked_after(&r, n);
    let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
    assert!(!names.contains(&ABANDON) && !names.contains(&PAUSE) && !names.contains(&EJECT), "a certify that judged nothing is no red: {names:?}");
}

/// A certify that exits non-zero naming no verdict and no suite, after the attribution, is a
/// fault of the machinery: its full output is kept and the reason carries its last line and the
/// file, the round stays open, and no pause is written.
#[test]
fn sp_byw57t_a_certify_that_judged_nothing_keeps_its_output_and_names_it_in_the_reason() {
    let mut c = driving(State::Certifying, "spira.c3");
    c.repos.get_mut("spira").unwrap().attributed = true;
    let mut r = carried(&c);
    let n = r.requested().len();
    let out = "queue.sh round certify: first line\nqueue.sh round certify: round r9 is already being certified (pid=7) — refused\n";
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), out)).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
    assert!(!names.contains(&ABANDON) && !names.contains(&PAUSE), "the round stays open and unpaused: {names:?}");
    let kept = &after.iter().find(|(e, _)| e == KEEP_CERTIFY_OUTPUT).unwrap_or_else(|| panic!("output not kept: {after:?}")).1;
    assert_eq!(kept.key, "run/rounds/r9.certify-1.out");
    assert_eq!(kept.why, out, "the whole output is kept");
    let reason = certify_fault_reason(runtime::units::Exit::Code(1), out, &kept.key);
    assert!(reason.contains("already being certified") && reason.contains("run/rounds/r9.certify-1.out"), "{reason}");
}

/// A pass scope that outlives the wall does not hold certify for good.
#[test]
fn sp_byw57t_a_pass_scope_that_outlives_the_wall_releases_certify() {
    let mut c = driving(State::Certifying, "");
    let rc = c.repos.get_mut("spira").unwrap();
    rc.leg = None;
    rc.certify_held = true;
    rc.leg_at_ms = 0;
    let mut snap = idle();
    snap.repos.get_mut("spira").unwrap().scoped_batches = Some(vec!["r9".into()]);
    let (_, acts) = decide(&c, &snap);
    assert!(acts.iter().any(|a| a.effect == CERTIFY), "{acts:?}");
}

/// Certify refuses a round whose pass scope is still finishing: the attribution's survivors wait
/// for the scope to end, then certify starts.
#[test]
fn sp_byw57t_certify_waits_for_the_attribution_passs_scope_to_end() {
    let mut c = driving(State::Certifying, "");
    {
        let rc = c.repos.get_mut("spira").unwrap();
        rc.leg = None;
        rc.certify_held = true;
        rc.leg_at_ms = NOW;
    }
    let mut snap = idle();
    snap.repos.get_mut("spira").unwrap().scoped_batches = Some(vec!["r9".into()]);
    let (held, acts) = decide(&c, &snap);
    assert!(acts.iter().all(|a| a.effect != CERTIFY), "a live pass scope holds certify: {acts:?}");
    snap.repos.get_mut("spira").unwrap().scoped_batches = Some(vec![]);
    let (_, acts) = decide(&held, &snap);
    assert!(acts.iter().any(|a| a.effect == CERTIFY), "certify starts once the scope has ended: {acts:?}");
}

/// The pass exits green while the shared build-cache server is left in its scope: observe
/// stops the scope and certify starts on the next decide, not when the daemon dies.
#[test]
fn sp_b2azes_a_daemon_left_in_the_pass_scope_does_not_hold_certify() {
    use runtime::units::{ScriptedUnits, UnitFacts, UnitProcess, UnitState, Units};
    let units = ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0)));
    let scope = format!("{}.scope", runtime::units::round_scope_unit("spira", "r9", "7-1"));
    units.install(&scope, UnitFacts { state: UnitState::Active, main_pid: None, exit: None, since_ms: Some(0) });
    units.hold_processes(&scope, vec![UnitProcess { pid: 880229, comm: "sccache".into() }]);
    let mut c = driving(State::Certifying, "");
    {
        let rc = c.repos.get_mut("spira").unwrap();
        rc.leg = None;
        rc.certify_held = true;
        rc.leg_at_ms = NOW;
    }
    let mut snap = idle();
    snap.repos.get_mut("spira").unwrap().scoped_batches = spira_runtime::tasks::a::batcher_local::edge::scoped_batches_in(&units, "spira", &c);
    assert_eq!(snap.repos["spira"].scoped_batches, Some(vec![]));
    assert!(!units.list("*").unwrap().iter().any(|d| d.name == scope && matches!(d.state, UnitState::Active)), "the scope was stopped");
    let (_, acts) = decide(&c, &snap);
    assert!(acts.iter().any(|a| a.effect == CERTIFY), "certify starts within one tick: {acts:?}");
}

/// A certify red naming a suite in a round of several members is attributed on that suite,
/// and the survivors are certified again and land (sp-b0lhx6).
#[test]
fn sp_b0lhx6_a_named_suite_red_attributes_that_suite_and_the_survivors_land() {
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    let out = "queue.sh round certify: round r9 is RED at abc123: test-release-activate-config.sh\nred=test-release-activate-config.sh\nred-output=/run/rounds/r9.results/testenv.out\n";
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), out)).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, l)| (e.as_str(), l.why.as_str())).collect::<Vec<_>>(), [(CORPUS, "attribute")], "no abandon yet: {after:?}");
    assert_eq!(after[0].1.reds, ["test-release-activate-config.sh"], "only the named suite is attributed");
    let unit = after[0].1.unit.clone();

    let land = batcher_cut::attribute::Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: None, mended: false }.line();
    let n = r.requested().len();
    r.tick(STREAM, ended(&unit, runtime::units::Exit::Code(0), &format!("{land}\n"))).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(), [CERTIFY], "{after:?}");
    let certify_unit = after[0].1.unit.clone();

    let n = r.requested().len();
    r.tick(STREAM, ended(&certify_unit, runtime::units::Exit::Code(0), "verdict=green\n")).unwrap();
    let names: Vec<String> = asked_after(&r, n).into_iter().map(|(e, _)| e).collect();
    assert!(names.contains(&LAND.to_string()) && !names.contains(&ABANDON.to_string()), "{names:?}");
}

/// The named-suite attribution runs once; a second named red abandons and says what it had.
#[test]
fn sp_b0lhx6_a_named_suite_red_after_attribution_abandons_without_claiming_ejects() {
    let mut r = carried(&attributed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let after = asked_after(&r, n);
    assert!(after.iter().any(|(e, _)| e == ABANDON), "{after:?}");
    assert!(!after.iter().any(|(e, _)| e == CORPUS), "{after:?}");
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

/// A round that landed between the pause being written and the batcher first seeing it still
/// lifts it: the base is recorded when the pause is written, not when it is first seen.
#[test]
fn sp_56d3l3_a_pause_remembers_the_base_its_red_round_was_cut_on_so_a_landing_lifts_it() {
    let mut carry = driving(State::Attributing, "spira.a2");
    let (after, acts) = on_ran_with(&carry, &ran_of("spira.a2"), &runtime::units::RanOutput::captured(batcher_cut::attribute::Attribution::Blocked("unattributed red test-x.sh".into()).line().as_bytes(), b""), NOW).expect("its own leg");
    assert!(acts.iter().any(|a| a.effect == PAUSE), "a blocked attribution pauses: {acts:?}");
    assert_eq!(after.repos["spira"].paused_on.as_deref(), Some("b1"));
    carry = after;
    let (carry, acts) = decide(&carry, &paused("b2"));
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [PAUSE, FETCH, ASSEMBLE], "{acts:?}");
    assert_eq!(carry.repos["spira"].paused_on, None);
}

fn ran_of(leg: &str) -> runtime::units::UnitRan {
    runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, leg).unwrap(), exit: runtime::units::Exit::Code(0), duration_ms: 1 }
}

/// A round the attribution emptied by ejecting every member holds no red: it is abandoned and
/// writes no pause.
#[test]
fn sp_56d3l3_a_round_emptied_by_its_own_ejects_is_abandoned_without_a_pause() {
    use batcher_cut::attribute::Attribution;
    for outcome in [Attribution::Emptied("unit-tests red ejected every member".into()), Attribution::Land { members: vec![], attribution_secs: None, mended: false }] {
        let out = runtime::units::RanOutput::captured(outcome.line().as_bytes(), b"");
        let (carry, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran_of("spira.a2"), &out, NOW).expect("its own leg");
        let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
        assert!(names.contains(&ABANDON) && !names.contains(&PAUSE), "{outcome:?}: {names:?}");
        assert_eq!(carry.repos.get("spira").and_then(|r| r.paused_on.clone()), None);
    }
}

/// The pause file is written by an effect a moment after the verdict, so a tick can see the repo
/// unpaused with the verdict already carried: the base must survive that tick, or the pause is
/// first read after the next landing and records the landed head, which nothing can move past.
#[test]
fn sp_esyv8o_a_pause_written_at_one_head_is_lifted_by_a_landing_though_a_tick_saw_no_file_first() {
    let blocked = runtime::units::RanOutput::captured(batcher_cut::attribute::Attribution::Blocked("unattributed red test-x.sh".into()).line().as_bytes(), b"");
    let (carry, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran_of("spira.a2"), &blocked, NOW).expect("its own leg");
    assert!(acts.iter().any(|a| a.effect == PAUSE), "{acts:?}");

    let mut unwritten = paused("b1");
    unwritten.repos.get_mut("spira").unwrap().paused = None;
    let (carry, acts) = decide(&carry, &unwritten);
    assert!(acts.iter().any(|a| a.effect == ASSEMBLE), "the file is not there yet, so the repo cuts: {acts:?}");
    let landed = Carry { repos: [("spira".to_string(), RepoCarry { paused_on: carry.repos["spira"].paused_on.clone(), ..RepoCarry::default() })].into() };
    assert_eq!(landed.repos["spira"].paused_on.as_deref(), Some("b1"), "the head the pause was written at is kept");

    let (carry, acts) = decide(&landed, &paused("b2"));
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [PAUSE, FETCH, ASSEMBLE], "the landing lifts the pause and the waiting work is cut: {acts:?}");
    assert_eq!(carry.repos["spira"].paused_on, None);
}

/// A pause file that is gone after the repo moved on is no pause of ours: the next one is a new pause.
#[test]
fn sp_esyv8o_a_vanished_pause_forgets_its_base_once_the_repo_has_moved_past_it() {
    let mut gone = paused("b2");
    gone.repos.get_mut("spira").unwrap().paused = None;
    let (carry, _) = decide(&paused_on("b1"), &gone);
    assert_eq!(carry.repos["spira"].paused_on, None);
}

/// An attribution leg stopped by a signal says nothing about the round: the Concierge's abandon
/// stops it with SIGTERM, and a pause written for that holds the repo against nothing red.
#[test]
fn sp_esyv8o_an_attribution_leg_stopped_by_a_signal_writes_no_pause() {
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, "spira.a2").unwrap(), exit: runtime::units::Exit::Signal(15), duration_ms: 1 };
    let (carry, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran, &runtime::units::RanOutput::default(), NOW).expect("its own leg");
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert!(!names.contains(&PAUSE) && !names.contains(&ABANDON), "{names:?}");
    assert_eq!(carry.repos.get("spira").and_then(|r| r.paused_on.clone()), None);
    assert!(carry.repos.get("spira").is_none_or(|r| r.state == State::Idle), "the round is let go");
}

/// Only the stop an abandon sends is let go: a leg killed or crashed is a red with no outcome.
#[test]
fn sp_esyv8o_an_attribution_leg_killed_by_another_signal_still_pauses() {
    for sig in [9, 11] {
        let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, "spira.a2").unwrap(), exit: runtime::units::Exit::Signal(sig), duration_ms: 1 };
        let (_, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran, &runtime::units::RanOutput::default(), NOW).expect("its own leg");
        assert!(acts.iter().any(|a| a.effect == PAUSE), "signal {sig}: {acts:?}");
    }
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
    assert!(why.contains("stopped") && why.contains("concierge: hand fix") && why.contains("queue round start spira"), "{why}");
}

/// An owned stop outlives the landing that lifts a red pause: the pause goes, the stop holds, and
/// nothing is cut.
#[test]
fn sp_banzvc_the_landing_that_lifts_a_red_pause_leaves_an_owned_stop_in_force() {
    let mut s = paused("b1");
    let r = s.repos.get_mut("spira").unwrap();
    r.stopped = true;
    r.stop_why = Some("concierge: fixes unlanded (until: bead:sp-x=LANDED)".into());
    let (carry, acts) = decide(&paused_on("b0"), &s);
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [PAUSE], "the pause lifts and nothing is cut: {acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!(rc.state, State::Held);
    let why = rc.skip_why.clone().expect("named");
    assert!(why.contains("concierge") && why.contains("bead:sp-x=LANDED") && why.contains("queue round start spira"), "{why}");
    let (_, later) = decide(&carry, &Snapshot { repos: [("spira".to_string(), Repo { paused: None, ..s.repos["spira"].clone() })].into(), now_ms: NOW });
    assert!(later.is_empty(), "{later:?}");
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

// --- sp-xmqd8k: a refused landing of a green round leaves the round open ---

/// A landing refused for one member's change is not a red round: the round stays open with all
/// its members, the land output is kept, no pause is written, and the page carries the refusing
/// line and the verbs that settle it.
#[test]
fn sp_xmqd8k_a_refused_landing_leaves_the_green_round_open_and_pages_the_refusing_line() {
    let refusal = "queue.sh land-local: release c1ad5ddc failed to build or verify\nrelease: spira.task_modes names watch.perf_happy=observe, which this release does not register";
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let (carry, acts) = on_outcome(&landing(), LAND, false, refusal, &land, NOW);
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert!(!names.contains(&ABANDON) && !names.contains(&PAUSE) && !names.contains(&EJECT), "the round stays open and unpaused: {names:?}");
    let kept = &acts.iter().find(|a| a.effect == KEEP_CERTIFY_OUTPUT).unwrap_or_else(|| panic!("output not kept: {acts:?}")).input;
    let kept: Leg = serde_json::from_str(kept).unwrap();
    assert_eq!((kept.key.as_str(), kept.why.as_str()), ("run/rounds/r9.land-1.out", refusal));
    let page = &acts.iter().find(|a| a.effect == INBOX_APPEND).unwrap_or_else(|| panic!("no page: {acts:?}")).input;
    assert!(page.contains("which this release does not register"), "the refusing line: {page}");
    assert!(page.contains("queue round eject r9 <member>") && page.contains("certify") && page.contains("land"), "the next verbs: {page}");
    let rc = carry.repos.get("spira").cloned().unwrap_or_default();
    assert_eq!(rc.state, State::Idle);
    assert!(rc.paused_on.is_none(), "no pause");
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

/// The activation restart re-drives LAND on a round that already landed; the queue answers it
/// ok, and that lands the round — no abandon, no red pause.
#[test]
fn a_land_re_driven_after_it_landed_is_landed_not_red() {
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let (carry, acts) = on_outcome(&landing(), LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &land, NOW);
    assert!(!acts.iter().any(|a| a.effect == ABANDON), "{acts:?}");
    assert!(!carry.repos.contains_key("spira"), "the round is finished, not held red: {:?}", carry.repos);
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
    assert_eq!(path_of("/opt/tools/bin:/usr/bin"), "/r/bin:/r/spira:/opt/tools/bin:/usr/bin");
    assert_eq!(path_of(""), "/r/bin:/r/spira:/usr/local/bin:/usr/bin:/bin", "no runtime PATH falls back to the system dirs");
}

#[test]
fn the_unit_environment_built_for_a_leg_ends_in_the_runtimes_own_path() {
    use spira_runtime::tasks::a::batcher_local::edge::base_env;
    let own = std::env::var("PATH").expect("test process has a PATH");
    let env = base_env(vec![], "/c/cfg");
    let path = env.into_iter().find(|(n, _)| n == "PATH").map(|(_, v)| v).unwrap();
    assert!(path.ends_with(&format!(":{own}")), "{path} should end with the runtime PATH {own}");
}

/// A round whose land outcome never came back holds its state; the repo must not sit idle behind it.
#[test]
fn sp_2gcs4h_a_round_that_lost_its_land_outcome_is_forgotten_and_the_waiting_member_is_cut() {
    let mut stuck = driving(State::Landing, "");
    stuck.repos.get_mut("spira").unwrap().leg = None;
    let landed_world = |now_ms| Snapshot { now_ms, ..busy() };
    let (seen, acts) = decide(&stuck, &landed_world(NOW));
    assert!(acts.is_empty(), "first seen, inside the bound: {acts:?}");
    assert_eq!(seen.repos["spira"].state, State::Landing);
    let (_, acts) = decide(&seen, &landed_world(NOW + OUTCOME_WAIT_MS - 1));
    assert!(acts.is_empty(), "still inside the bound: {acts:?}");
    let (next, acts) = decide(&seen, &landed_world(NOW + OUTCOME_WAIT_MS + 1));
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [FETCH, ASSEMBLE], "the next tick cuts: {acts:?}");
    assert_eq!(next.repos["spira"].state, State::Assembling);
}

/// A leg that runs for hours is not an outcome wait.
#[test]
fn sp_2gcs4h_a_certifying_round_is_never_forgotten_for_waiting() {
    let (seen, _) = decide(&driving(State::Certifying, "u"), &Snapshot { now_ms: NOW, ..idle() });
    let (next, _) = decide(&seen, &Snapshot { now_ms: NOW + 10 * OUTCOME_WAIT_MS, ..idle() });
    assert_eq!(next.repos["spira"].waiting, None);
}

#[test]
fn sp_2gcs4h_the_outcome_wait_outlasts_every_effect_a_waiting_state_issues() {
    let longest = ACTS.iter().filter(|(e, _)| [ASSEMBLE, ROUND_OPEN, LAND, LC_CERTIFY, PROMOTE, FETCH].contains(e)).map(|(_, d)| *d).max().unwrap();
    assert!(OUTCOME_WAIT_MS > longest, "{OUTCOME_WAIT_MS} vs {longest}");
}

#[test]
fn sp_2gcs4h_after_a_landing_with_a_certified_bead_and_no_round_open_the_next_tick_cuts() {
    let landing = driving(State::Landing, "");
    let leg = Leg { batch: "r9".into(), key: "r9".into(), ..Leg::on("spira") };
    let (landed, _) = on_outcome(&landing, LAND, true, "{}", &leg, NOW);
    let (_, acts) = decide(&landed, &Snapshot { now_ms: NOW + CADENCE_MS, ..busy() });
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [FETCH, ASSEMBLE], "{acts:?}");
}

fn at_work() -> TaskRig {
    let mut r = rig(1, busy());
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
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

/// The round key lists every member as `id:tip`, so it outgrows the store's 255-char key column
/// with the round's size; a green certify of such a round must still reach `lc_certify`.
#[test]
fn sp_lkm2fv_a_green_certify_of_a_wide_round_is_requested_not_refused_by_the_store() {
    use runtime::units::{Exit, UnitHandle, UnitRan};
    let members: Vec<Member> = (0..9).map(|i| Member { state: "CERTIFIED".into(), ..member(&format!("sp-wide{i:02}"), &format!("{:040x}", i + 1)) }).collect();
    let world = Snapshot { repos: [("spira".to_string(), repo(members.clone()))].into(), now_ms: NOW };
    let snaps = std::sync::Mutex::new(vec![world]);
    let assembled = Assembled { merged: members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(), wt: "/w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&snaps.lock().unwrap().pop().unwrap_or_else(idle)).unwrap()))
        .double(ASSEMBLE, move |_| Ok(serde_json::to_string(&assembled).unwrap()))
        .double(ROUND_OPEN, |_| Ok(serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE, ROUND_OPEN].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert!(r.requested().iter().any(|q| q.name == ROUND_OPEN), "certified beads and no open round: the first tick opens a round");
    let leg_of = |r: &TaskRig, effect: &str| -> Leg { serde_json::from_str(&r.requested().iter().rev().find(|q| q.name == effect).unwrap_or_else(|| panic!("{effect} was never asked")).input).unwrap() };
    let corpus = leg_of(&r, CORPUS);
    let ran = |leg: &Leg| UnitRan { handle: UnitHandle::of(NAME, &leg.unit).unwrap(), exit: Exit::Code(0), duration_ms: 1_000 };
    r.tick(STREAM, ran(&corpus).event_kind()).unwrap();
    let certify = leg_of(&r, CERTIFY);
    r.tick(STREAM, ran(&certify).event_kind()).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    assert!(r.requested().iter().any(|q| q.name == LC_CERTIFY), "the green certify reaches lc_certify");
}

// --- sp-pi2ojm: an opened round is driven on the next tick, not after its outcome's 20-minute wait ---

fn opened_unadvanced() -> Carry {
    let rc = RepoCarry { state: State::Opened, round_key: Some("k".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "/w".into(), base: "b1".into(), waiting: Some(Waiting { state: State::Opened, since_ms: NOW - 40_000 }), ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn queue_holds(id: &str, members: &[&str]) -> Snapshot {
    let round = Round { id: id.into(), members: members.iter().map(|m| m.to_string()).collect(), head: "h1".into(), worktree: "/w".into(), ..Round::default() };
    Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round), ..repo(vec![]) })].into(), now_ms: NOW }
}

#[test]
fn sp_pi2ojm_the_tick_after_round_open_launches_the_corpus_leg_when_the_outcome_never_arrived() {
    let (carry, acts) = decide(&opened_unadvanced(), &queue_holds("r9", &["sp-a", "sp-b"]));
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [CORPUS], "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref()), (State::Corpus, Some("r9")));
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(leg.batch, "r9");
}

#[test]
fn sp_pi2ojm_an_opened_round_the_queue_does_not_hold_yet_is_left_waiting() {
    let (carry, acts) = decide(&opened_unadvanced(), &queue_holds("other", &["sp-x"]));
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opened);
}

#[test]
fn sp_pi2ojm_the_late_round_open_outcome_after_the_adoption_launches_no_second_leg() {
    let (carry, _) = decide(&opened_unadvanced(), &queue_holds("r9", &["sp-a", "sp-b"]));
    let (_, acts) = on_outcome(&carry, ROUND_OPEN, true, &serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap(), &Leg { why: "suites".into(), ..Leg::on("spira") }, NOW);
    assert!(acts.is_empty(), "{acts:?}");
}

#[test]
fn sp_pi2ojm_an_opened_round_in_another_tree_is_not_ours_to_adopt() {
    let mut snap = queue_holds("r9", &["sp-a", "sp-b"]);
    snap.repos.get_mut("spira").unwrap().open_round.as_mut().unwrap().worktree = "/elsewhere".into();
    let (carry, acts) = decide(&opened_unadvanced(), &snap);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opened);
}

#[test]
fn sp_pi2ojm_an_adopted_round_without_suites_certifies_in_its_own_testenv_or_is_blocked() {
    let mut own = opened_unadvanced();
    let rc = own.repos.get_mut("spira").unwrap();
    (rc.suites, rc.own_testenv) = (Some(false), true);
    let (carry, acts) = decide(&own, &queue_holds("r9", &["sp-a", "sp-b"]));
    assert_eq!((carry.repos["spira"].state, acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>()), (State::Certifying, vec![CERTIFY]));
    let mut bare = opened_unadvanced();
    bare.repos.get_mut("spira").unwrap().suites = Some(false);
    let (_, acts) = decide(&bare, &queue_holds("r9", &["sp-a", "sp-b"]));
    assert!(acts.iter().all(|a| a.effect != CORPUS && a.effect != CERTIFY), "{acts:?}");
}

const LEFTOVER: &str = "git worktree add exited 128: fatal: '/run/worktree/.batcher-spira' already exists";

/// One tick of a cut whose assemble fails with `err`: the carry after, and what the outcome asked.
fn failed_assemble(carry: &Carry, err: &str) -> (Carry, Vec<Act>) {
    let (carry, acts) = decide(carry, &busy());
    let assemble = acts.iter().find(|a| a.effect == ASSEMBLE).expect("a cut asks for an assemble");
    let leg: Leg = serde_json::from_str(&assemble.input).unwrap();
    on_outcome(&carry, ASSEMBLE, false, err, &leg, NOW)
}

fn pages(acts: &[Act]) -> Vec<&Act> {
    acts.iter().filter(|a| a.effect == INBOX_APPEND).collect()
}

#[test]
fn a_failed_assemble_names_its_error_in_skip_why_from_the_first_tick() {
    let (carry, acts) = failed_assemble(&Carry::default(), LEFTOVER);
    let why = carry.repos["spira"].skip_why.as_deref().expect("the failure is carried");
    assert!(why.contains(LEFTOVER), "{why}");
    assert!(pages(&acts).is_empty(), "one failure is not yet a page");
    let (carry, _) = decide(&carry, &busy());
    assert!(carry.repos["spira"].skip_why.as_deref().unwrap_or("").contains(LEFTOVER), "the next cut does not wipe it before it runs");
}

#[test]
fn the_same_assemble_failure_three_ticks_running_pages_once_with_the_error() {
    let (c1, a1) = failed_assemble(&Carry::default(), LEFTOVER);
    let (c2, a2) = failed_assemble(&c1, LEFTOVER);
    assert!(pages(&a1).is_empty() && pages(&a2).is_empty());
    let (c3, a3) = failed_assemble(&c2, LEFTOVER);
    let paged = pages(&a3);
    assert_eq!(paged.len(), 1, "{a3:?}");
    assert!(paged[0].input.contains(LEFTOVER), "{}", paged[0].input);
    let (_, a4) = failed_assemble(&c3, LEFTOVER);
    assert!(pages(&a4).is_empty(), "a fourth tick does not page again");
}

#[test]
fn a_different_assemble_failure_restarts_the_count() {
    let (c1, _) = failed_assemble(&Carry::default(), LEFTOVER);
    let (c2, _) = failed_assemble(&c1, LEFTOVER);
    let (c3, a3) = failed_assemble(&c2, "git worktree add exited 128: something else");
    assert!(pages(&a3).is_empty(), "{a3:?}");
    assert!(c3.repos["spira"].skip_why.as_deref().unwrap().contains("something else"));
}

#[test]
fn an_assemble_that_succeeds_after_failures_clears_the_failure_and_its_skip_why() {
    let (failed, _) = failed_assemble(&Carry::default(), LEFTOVER);
    let (carry, acts) = decide(&failed, &busy());
    let assemble = acts.iter().find(|a| a.effect == ASSEMBLE).unwrap();
    let leg: Leg = serde_json::from_str(&assemble.input).unwrap();
    let ok = Assembled { merged: vec![("sp-a".into(), "t1".into())], wt: "/w".into(), base: "b1".into(), ..Assembled::default() };
    let (carry, _) = on_outcome(&carry, ASSEMBLE, true, &serde_json::to_string(&ok).unwrap(), &leg, NOW);
    assert_eq!(carry.repos["spira"].assemble_failed, None);
    assert_eq!(carry.repos["spira"].skip_why, None);
}

#[test]
fn an_assemble_lost_at_restart_is_requested_again_on_the_next_tick() {
    let (carry, _) = failed_assemble(&Carry::default(), "lost at restart");
    let (carry, acts) = decide(&carry, &busy());
    assert_eq!(carry.repos["spira"].skip_why, None, "a lost assemble is not a failure to report");
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [FETCH, ASSEMBLE], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Assembling);
}

#[test]
fn a_lost_assemble_is_not_counted_as_an_assemble_failure() {
    let (carry, acts) = failed_assemble(&Carry::default(), "lost at restart");
    assert!(carry.repos.get("spira").is_none_or(|rc| rc.skip_why.is_none() && rc.assemble_failed.is_none() && rc.state == State::Idle), "{carry:?}");
    assert!(acts.is_empty(), "{acts:?}");
}

#[test]
fn a_restart_with_assembling_carried_and_no_assemble_in_flight_requests_it_on_the_next_tick() {
    let (carry, _) = decide(&Carry::default(), &busy());
    let mut quiet = busy();
    quiet.now_ms = NOW + CADENCE_MS;
    quiet.repos.get_mut("spira").unwrap().in_flight = Some(vec![]);
    let (carry, acts) = decide(&carry, &quiet);
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [FETCH, ASSEMBLE], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Assembling);
}

#[test]
fn an_assemble_in_flight_is_left_alone() {
    let (carry, _) = decide(&Carry::default(), &busy());
    let mut flying = busy();
    flying.now_ms = NOW + CADENCE_MS;
    flying.repos.get_mut("spira").unwrap().in_flight = Some(vec![ASSEMBLE.into()]);
    let (_, acts) = decide(&carry, &flying);
    assert!(acts.is_empty(), "{acts:?}");
}

/// A scheduler restart while the assemble is open ends it lost; a round is cut within a minute.
#[test]
fn a_round_is_cut_within_a_minute_of_a_restart_that_interrupted_the_assemble() {
    let assembled = Assembled { merged: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "/w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .hanging(ASSEMBLE)
        .double(OBSERVE, |_| Ok(serde_json::to_string(&busy()).unwrap()))
        .double(ASSEMBLE, move |_| Ok(serde_json::to_string(&assembled).unwrap()))
        .double(ROUND_OPEN, |_| Ok(serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE, ROUND_OPEN].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS * 3).unwrap();
    let asked = |r: &TaskRig, name: &str| r.requested().iter().filter(|q| q.name == name).count();
    assert_eq!((asked(&r, ASSEMBLE), asked(&r, ROUND_OPEN)), (1, 0), "an open assemble is waited for, not asked again");
    r.reopen(registration()).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(60_000).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    assert_eq!((asked(&r, ASSEMBLE), asked(&r, ROUND_OPEN)), (2, 1), "{:?}", r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>());
}

/// The engine's own lost-at-restart outcome for an open round open is asked for again.
#[test]
fn a_round_open_interrupted_by_a_restart_is_asked_again() {
    let assembled = Assembled { merged: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "/w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .hanging(ROUND_OPEN)
        .double(OBSERVE, |_| Ok(serde_json::to_string(&busy()).unwrap()))
        .double(ASSEMBLE, move |_| Ok(serde_json::to_string(&assembled).unwrap()))
        .double(ROUND_OPEN, |_| Ok(serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE, ROUND_OPEN].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let opens = |r: &TaskRig| r.requested().iter().filter(|q| q.name == ROUND_OPEN).map(|q| (q.key.clone(), q.input.clone())).collect::<Vec<_>>();
    assert_eq!(opens(&r).len(), 1, "an open round open is waited for, not asked again: {:?}", opens(&r));
    r.reopen(registration()).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(60_000).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let asked = opens(&r);
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert!(asked[1].1.contains("spira"), "the repeat names the repo: {asked:?}");
}

/// A round open that ends lost is neither a failed open nor a reason to wait out the outcome timer.
#[test]
fn a_round_open_lost_at_restart_is_asked_again() {
    let mut carry = Carry::default();
    carry.repos.insert("spira".into(), RepoCarry { state: State::Opened, round_key: Some("k".into()), members: vec![("sp-a".into(), "t1".into())], wt: "/w".into(), base: "b1".into(), suites: Some(true), ..RepoCarry::default() });
    let mut world = Snapshot { now_ms: NOW, ..idle() };
    world.repos.get_mut("spira").unwrap().in_flight = Some(vec![]);
    let (carry, acts) = decide(&carry, &world);
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [ROUND_OPEN], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opened);
    let leg = Leg { repo: "spira".into(), ..Leg::default() };
    let (carry, acts) = on_outcome(&carry, ROUND_OPEN, false, "lost at restart", &leg, NOW);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opened, "not an open failure");
}

/// Certify writes its refusals to stderr: they are kept with the stdout, on their own line.
#[test]
fn sp_byw57t_a_certify_legs_stderr_is_kept_with_its_stdout() {
    let mut c = driving(State::Certifying, "spira.c3");
    c.repos.get_mut("spira").unwrap().attributed = true;
    let out = runtime::units::RanOutput::captured(b"starting", b"queue round certify: refused, the round worktree is not at the head");
    let ran = runtime::units::UnitRan { exit: runtime::units::Exit::Code(1), ..ran_of("spira.c3") };
    let (_, acts) = on_ran_with(&c, &ran, &out, NOW).expect("its own leg");
    let kept = acts.iter().find(|a| a.effect == KEEP_CERTIFY_OUTPUT).unwrap_or_else(|| panic!("output not kept: {acts:?}"));
    let why = serde_json::from_str::<Leg>(&kept.input).unwrap().why;
    assert_eq!(why, "starting\nqueue round certify: refused, the round worktree is not at the head");
}
