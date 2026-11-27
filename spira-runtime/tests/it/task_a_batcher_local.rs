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
    test_ids: [ #[test_id(tygoqbm)] #[test_id(tyuwv6o)] #[test_id(twntq0u)] #[test_id(tf28eym)] #[test_id(tr5s3ph)] ],
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
    Snapshot { repos: [("spira".to_string(), repo(vec![]))].into(), now_ms: NOW, ..Default::default() }
}

/// Two sifted submissions at their tips and no open round: a cut.
fn busy() -> Snapshot {
    Snapshot { repos: [("spira".to_string(), repo(vec![member("sp-a", "t1"), member("sp-b", "t2")]))].into(), now_ms: NOW, ..Default::default() }
}

#[test_id(t1oe1vk)]
#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test_id(t20prdu)]
#[test]
fn t3_a_foreign_round_leg_in_corpus_is_refused_naming_corpus() {
    let refused = step(State::Corpus, Trigger::ForeignLeg).expect_err("a leg of another round is illegal");
    assert!(refused.contains("Corpus"), "{refused}");
}

#[test_id(t6zlevw)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// T2's other two idle shapes: a held repo and a driven open round are silent too.
#[test_id(tmvpnil)]
#[test]
fn t2_a_held_repo_and_a_driven_round_are_silent() {
    let held = Snapshot { repos: [("spira".to_string(), Repo { stopped: true, ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() };
    let driven = Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(Round { id: "r1".into(), members: vec!["sp-a".into()], head: "h".into(), worktree: "w".into(), driven: true, ..Round::default() }), ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() };
    for snap in [held, driven] {
        let mut r = rig(1, snap);
        kit::assert_unpoisoned(&r, STREAM);
        assert_silent_tick(&mut r, STREAM);
        kit::assert_unpoisoned(&r, STREAM);
    }
}

/// `screen::cut_pool`, `should_cut` (`core.rs`): a sifted pool cuts — fetch, then assemble.
#[test_id(tuqj0zf)]
#[test]
fn t5_a_sifted_pool_is_fetched_then_assembled() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    let order: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(order, [FETCH, ASSEMBLE], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Assembling);
}

/// `pace.rs:77`: a stopped repo holds and writes nothing.
#[test_id(tdxc2ti)]
#[test]
fn t5_a_stopped_repo_holds_without_a_write() {
    let held = Snapshot { repos: [("spira".to_string(), Repo { stopped: true, ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() };
    let (carry, acts) = decide(&Carry::default(), &held);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Held);
}

/// A hand-landing's cut hold stops a new cut and names its owner; once it lifts the repo cuts.
#[test_id(tzgvqvk)]
#[test]
fn t5_a_cut_held_repo_cuts_nothing_and_cuts_again_when_the_hold_lifts() {
    let held = Snapshot { repos: [("spira".to_string(), Repo { cut_hold: Some("concierge (hand-landing) until 9".into()), ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() };
    let (carry, acts) = decide(&Carry::default(), &held);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Held);
    assert!(carry.repos["spira"].skip_why.as_deref().is_some_and(|w| w.contains("concierge")), "{:?}", carry.repos["spira"].skip_why);
    let lifted = Snapshot { repos: [("spira".to_string(), Repo { cut_hold: None, ..held.repos["spira"].clone() })].into(), now_ms: NOW + 30_000, ..Default::default() };
    let (_, acts) = decide(&carry, &lifted);
    let effects: Vec<_> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(effects, [FETCH, ASSEMBLE], "{acts:?}");
}

/// `core.rs` `cut_pool`: a member not at its sifted tip stays out of the cut.
#[test_id(tnxdxrh)]
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
#[test_id(t0zi8fd)]
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

/// A round opens straight into certify: its one green pass is the certificate, so the leg's
/// `unit.ran` on this task's own stream lands the round at once and no corpus leg ever runs.
#[test_id(td9llml)]
#[test]
fn t3_a_green_certify_unit_ran_on_its_own_stream_certifies_and_lands_with_no_corpus() {
    use runtime::units::{Exit, UnitHandle, UnitRan};
    let snaps = std::sync::Mutex::new(vec![busy()]);
    let assembled = Assembled { merged: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&snaps.lock().unwrap().pop().unwrap_or_else(idle)).unwrap()))
        .double(ASSEMBLE, move |_| Ok(serde_json::to_string(&assembled).unwrap()))
        .double(ROUND_OPEN, |_| Ok(serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE, ROUND_OPEN].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let certify: Leg = serde_json::from_str(&r.requested().iter().find(|q| q.name == CERTIFY).expect("the first leg is certify").input).unwrap();
    let asked = r.requested().len();
    let ran = UnitRan { handle: UnitHandle::of(NAME, &certify.unit).unwrap(), exit: Exit::Code(0), duration_ms: 1_000 };
    r.tick(STREAM, ran.event_kind()).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after: Vec<&str> = r.requested()[asked..].iter().map(|q| q.name.as_str()).collect();
    assert!(after.contains(&LC_CERTIFY) && after.contains(&LAND), "{after:?}");
    assert!(r.requested().iter().all(|q| q.name != CORPUS), "no corpus leg ran before or after: {:?}", r.requested().iter().map(|q| &q.name).collect::<Vec<_>>());
}

/// `work.submitted` (sift's hand-off when a pass is recorded) on this task's stream asks for an
/// observe at once, without re-arming the tick.
#[test_id(trxqyr1)]
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
    let rc = RepoCarry { state, round_key: Some("k".into()), batch: Some("r9".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "w".into(), base: "b1".into(), leg: Some(leg.into()), leg_at_ms: 0, run: 1, ..RepoCarry::default() };
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
#[test_id(t3bk0p1)]
#[test]
fn t3_a_red_corpus_enters_attribution() {
    let mut r = carried(&driving(State::Corpus, "spira.c1"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c1", runtime::units::Exit::Code(1), "")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, l)| (e.as_str(), l.why.as_str())).collect::<Vec<_>>(), [(CORPUS, "attribute")], "{after:?}");
    assert_eq!(after[0].1.members.len(), 2, "every member is attributed");
    assert_eq!(after[0].1.after_pass, "w/.round-results-spira.c1", "the attribution reads the red pass's results");
}

/// Attributing × `unit.ran`: the attribution's own line decides. Survivors go on to certify
/// (alone, the owners ejected by the leg); a mender pass goes through Mending to certify; a
/// blocked round is Done, red.
#[test_id(tkuklke)]
#[test]
fn t3_the_attribution_line_decides_certify_mending_or_blocked() {
    use batcher_cut::attribute::Attribution;
    let land = Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: Some(9), mended: false, head: String::new() };
    for (line, mended) in [(land.line(), false), (Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: None, mended: true, head: String::new() }.line(), true)] {
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
#[test_id(tkye5uq)]
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

/// An attribution that hit its probe ceiling abandons the round whole under its own cause, and pauses nothing:
/// the red is kept for the next round to convict.
#[test_id(tunbz1y)]
#[test]
fn an_unconverged_attribution_abandons_the_round_with_cause_attribution_not_converging() {
    use batcher_cut::attribute::Attribution;
    let why = "unit-tests red reproduces but 8 probes named no member; suspects: m1,m2";
    let line = Attribution::Unconverged(why.into()).line();
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, "spira.a2").unwrap(), exit: runtime::units::Exit::Code(0), duration_ms: 1 };
    let out = runtime::units::RanOutput::captured(line.as_bytes(), b"");
    let (_, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran, &out, NOW).expect("its own leg");
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert!(!names.contains(&PAUSE) && !names.contains(&CERTIFY), "{names:?}");
    let abandon: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ABANDON).unwrap_or_else(|| panic!("no abandon: {names:?}")).input).unwrap();
    assert_eq!((abandon.bead.as_str(), abandon.why.as_str()), (ATTRIBUTION_NOT_CONVERGING, why));
    assert_eq!(ATTRIBUTION_NOT_CONVERGING, "attribution-not-converging");
}

/// A leg whose reruns faulted is not a red: no abandon, no pause, and the page says so.
#[test_id(t2iznh8)]
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
#[test_id(t2fwqxd)]
#[test]
fn t3_a_red_certify_ejects_the_guilty_then_abandons() {
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "verdict=red\nred-output=run/certify-r9.out\n")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
    let eject = names.iter().position(|e| *e == EJECT).unwrap_or_else(|| panic!("no eject: {names:?}"));
    assert!(names[eject..].contains(&ABANDON), "{names:?}");
    assert_eq!((after[eject].1.why.as_str(), after[eject].1.bead.as_str()), ("run/certify-r9.out", ""), "the guilty are read from the output");
}

fn attributed(mut c: Carry) -> Carry {
    c.repos.get_mut("spira").unwrap().attributed = true;
    c
}

fn confirmed(mut c: Carry) -> Carry {
    c.repos.get_mut("spira").unwrap().confirmed = true;
    attributed(c)
}

fn sole_member(mut c: Carry) -> Carry {
    c.repos.get_mut("spira").unwrap().members = vec![("sp-a".into(), "t1".into())];
    c
}

/// A red `queue round certify` of a one-member round ejects that member with the red output as
/// its evidence and writes no pause, whether or not the output names the member's files.
#[test_id(t9jl806)]
#[test]
fn sp_z43b7y_a_red_certify_of_a_one_member_round_ejects_the_member_and_writes_no_pause() {
    for out in ["verdict=red\nred-output=run/rounds/r9.results/testenv.out\n", "queue.sh round certify: round r9 is RED at abc: test-a.sh\nred=test-a.sh\n", ""] {
        let c = {
            let mut c = sole_member(driving(State::Certifying, "spira.c3"));
            c.repos.get_mut("spira").unwrap().attributed = true;
            c
        };
        let red = runtime::units::RanOutput::captured(out.as_bytes(), b"");
        let (waiting, acts) = on_ran_with(&c, &red_ran("spira.c3"), &red, NOW).expect("its own leg");
        let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
        assert!(!names.contains(&PAUSE) && !names.contains(&ABANDON), "{out:?}: the abandon waits on the eject: {names:?}");
        let eject: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == EJECT).unwrap_or_else(|| panic!("{out:?}: no eject: {names:?}")).input).unwrap();
        assert_eq!((eject.bead.as_str(), eject.head.as_str()), ("sp-a", "t1"), "{out:?}");
        if out.contains("red-output=") {
            assert!(eject.why.contains("run/rounds/r9.results/testenv.out"), "{}", eject.why);
        }
        let (next, acts) = on_outcome(&waiting, EJECT, true, "{}", &eject, NOW);
        assert!(acts.iter().all(|a| a.effect != ABANDON && a.effect != PAUSE), "{out:?}: {acts:?}");
        let rc = next.repos.get("spira").cloned().unwrap_or_default();
        assert!(rc.members.is_empty() && rc.batch.is_none(), "{out:?}: the emptied round is forgotten: {rc:?}");
    }
}

/// While the sole member's eject is in flight the dead certify leg is not judged lost: the next
/// tick starts no certify and abandons nothing.
#[test_id(tmst9ox)]
#[test]
fn sp_z1vzxd_a_tick_between_the_sole_eject_and_its_outcome_starts_nothing() {
    let mut r = carried(&attributed(sole_member(driving(State::Certifying, "spira.c3"))));
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let n = r.requested().len();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let names: Vec<String> = asked_after(&r, n).into_iter().map(|(e, _)| e).collect();
    assert!(names.iter().all(|e| ![CERTIFY, CORPUS, ABANDON, PAUSE, LAND].contains(&e.as_str())), "{names:?}");
}

/// A refused eject of the sole member abandons the round, names the refusal and keeps the red evidence.
#[test_id(t2uwbhn)]
#[test]
fn sp_z1vzxd_a_refused_eject_of_the_sole_member_abandons_the_round_naming_the_refusal() {
    let c = attributed(sole_member(driving(State::Certifying, "spira.c3")));
    let red = runtime::units::RanOutput::captured(b"red=test-a.sh\nred-output=run/rounds/r9.results/a.out\n", b"");
    let (waiting, acts) = on_ran_with(&c, &red_ran("spira.c3"), &red, NOW).expect("its own leg");
    let eject: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == EJECT).unwrap().input).unwrap();
    let (_, acts) = on_outcome(&waiting, EJECT, false, "no open round", &eject, NOW);
    let abandon: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ABANDON).unwrap_or_else(|| panic!("{acts:?}")).input).unwrap();
    assert!(abandon.why.contains("no open round") && abandon.why.contains("run/rounds/r9.results/a.out"), "{}", abandon.why);
    assert_eq!(abandon.reds, vec!["test-a.sh".to_string()]);
}

/// A multi-member abandon that ejected nothing at decision time claims no count.
#[test_id(tiktkv7)]
#[test]
fn sp_b0lhx6_an_abandon_carries_the_ejects_the_decision_made() {
    let mut r = carried(&confirmed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let after = asked_after(&r, n);
    assert_eq!(after.iter().find(|(e, _)| e == ABANDON).unwrap().1.ejected, None);
}

/// Two members and no file named: the red has no culprit to eject, so the pause stands for a person.
#[test_id(t1il1m2)]
#[test]
fn sp_z43b7y_a_red_certify_of_a_two_member_round_naming_no_member_still_pauses() {
    let mut c = driving(State::Certifying, "spira.c3");
    c.repos.get_mut("spira").unwrap().attributed = true;
    c.repos.get_mut("spira").unwrap().confirmed = true;
    let mut r = carried(&c);
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let names: Vec<String> = asked_after(&r, n).into_iter().map(|(e, _)| e).collect();
    assert!(names.contains(&PAUSE.to_string()) && !names.contains(&EJECT.to_string()), "{names:?}");
}

const THREE_REDS: &str = "queue.sh round certify: round r9 is RED at abc123: test-a.sh,test-b.sh,test-c.sh\nred=test-a.sh,test-b.sh,test-c.sh\n";

/// A red that survived its attribution is attributed once more, by suspect, before anything pauses.
#[test_id(td4ty8h)]
#[test]
fn sp_mg27kj_a_red_that_survived_its_attribution_is_attributed_once_more_by_suspect() {
    let mut r = carried(&attributed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), THREE_REDS)).unwrap();
    let after = asked_after(&r, n);
    let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
    assert!(!names.contains(&PAUSE) && !names.contains(&ABANDON), "{names:?}");
    let leg = &after.iter().find(|(e, _)| e == CORPUS).unwrap_or_else(|| panic!("no corpus leg: {after:?}")).1;
    assert!(leg.why == ATTRIBUTE && leg.after_certify && leg.confirmed, "{leg:?}");
}

fn confirmed_on(reds: &[&str]) -> Carry {
    let mut c = confirmed(driving(State::Certifying, "spira.c3"));
    c.repos.get_mut("spira").unwrap().attributed_reds = reds.iter().map(|r| r.to_string()).collect();
    c
}

/// A certify red naming a suite no attribution of the round covered is attributed afresh, so its
/// rerun can find a flip, even after the round's first and confirmed attributions were spent.
#[test_id(tz70ztm)]
#[test]
fn sp_54in0u_a_red_on_a_suite_not_yet_attributed_is_attributed_after_the_round_spent_its_two() {
    let mut r = carried(&confirmed_on(&["test-a.sh"]));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-b.sh\n")).unwrap();
    let after = asked_after(&r, n);
    let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
    assert!(!names.contains(&PAUSE) && !names.contains(&ABANDON), "{names:?}");
    let leg = &after.iter().find(|(e, _)| e == CORPUS).unwrap_or_else(|| panic!("no corpus leg: {after:?}")).1;
    assert!(leg.why == ATTRIBUTE && leg.after_certify && !leg.confirmed, "{leg:?}");
}

/// The same suite red again after its attribution is a real red: the round abandons naming it.
#[test_id(tgu9a98)]
#[test]
fn sp_54in0u_a_suite_red_again_after_its_attribution_abandons_naming_it() {
    let mut r = carried(&confirmed_on(&["test-a.sh", "test-b.sh"]));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-b.sh\n")).unwrap();
    let after = asked_after(&r, n);
    let abandon = &after.iter().find(|(e, _)| e == ABANDON).unwrap_or_else(|| panic!("no abandon: {after:?}")).1;
    assert_eq!(abandon.reds, ["test-b.sh"]);
    assert!(!after.iter().any(|(e, _)| e == CORPUS), "{after:?}");
}

#[test_id(t9z21h0)]
#[test]
fn sp_mg27kj_a_red_after_the_confirmed_attribution_pauses_and_abandons() {
    let mut r = carried(&confirmed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), THREE_REDS)).unwrap();
    let names: Vec<String> = asked_after(&r, n).into_iter().map(|(e, _)| e).collect();
    assert!(names.contains(&PAUSE.to_string()) && names.contains(&ABANDON.to_string()), "{names:?}");
}

/// Every other path that pauses blames no member, so its pause carries the reason a person acts on.
#[test_id(t789gb3)]
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
#[test_id(tqmqfru)]
#[test]
fn sp_7yu70y_a_red_certify_abandons_naming_its_verdict_and_red_suites() {
    let mut r = carried(&confirmed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    let out = "queue.sh round certify: round r9 is RED at abc123: test-a.sh,test-b.sh\nred=test-a.sh,test-b.sh\n";
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), out)).unwrap();
    let after = asked_after(&r, n);
    let abandon = after.iter().find(|(e, _)| e == ABANDON).unwrap_or_else(|| panic!("no abandon: {after:?}")).1.clone();
    assert!(abandon.why.contains("round r9 is RED at abc123"), "{}", abandon.why);
    assert!(abandon.why.contains("test-a.sh") && abandon.why.contains("test-b.sh"), "{}", abandon.why);
    assert_eq!(abandon.reds, ["test-a.sh", "test-b.sh"]);
}

#[test_id(tn06kji)]
#[test]
fn sp_7yu70y_a_red_certify_with_only_a_red_line_still_says_what_it_had() {
    let mut r = carried(&confirmed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let after = asked_after(&r, n);
    let abandon = &after.iter().find(|(e, _)| e == ABANDON).unwrap().1;
    assert!(abandon.why.contains("test-a.sh"), "{}", abandon.why);
    assert_eq!(abandon.reds, ["test-a.sh"]);
}

/// A certify red naming no suite is attributed, once per round, before it is abandoned whole
/// (sp-7yu70y.2): the second is a fault of the machinery, not a red.
#[test_id(tr8w23m)]
#[test]
fn sp_7yu70y_2_an_unnamed_certify_red_attributes_once_then_faults_without_a_red() {
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "queue.sh round certify: nothing useful\n")).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, l)| (e.as_str(), l.why.as_str())).collect::<Vec<_>>(), [(CORPUS, "attribute")], "{after:?}");
    let attribution_unit = after[0].1.unit.clone();

    let land = batcher_cut::attribute::Attribution::Land { members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], attribution_secs: None, mended: false, head: String::new() }.line();
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
#[test_id(tzg1xuk)]
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
#[test_id(tc5h8uo)]
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
#[test_id(t7jmo6g)]
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

fn held_eject_error() -> String {
    let held = batcher_cut::io::HeldEject { id: "sp-a".into(), reason: "red on x".into(), csv: "test-x.sh".into(), said: "queue round eject exited 79: a pass of round r9 is running".into() };
    format!("{}{}", batcher_cut::io::EJECT_HELD, serde_json::to_string(&held).unwrap())
}

/// An eject the queue holds for a running pass is not a refusal: no abandon, no pause, and the
/// same eject is issued again once the pass is gone, after which the survivors certify.
#[test_id(tx3gia2)]
#[test]
fn sp_3p7m5b_an_eject_held_for_a_running_pass_is_issued_again_when_the_pass_ends() {
    let c = driving(State::Certifying, "spira.e1");
    let leg = Leg { bead: "sp-a".into(), batch: "r9".into(), ..Leg::on("spira") };
    let (held, acts) = on_outcome(&c, EJECT, false, &held_eject_error(), &leg, NOW);
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert!(!names.contains(&ABANDON) && !names.contains(&PAUSE), "{names:?}");
    assert_eq!(held.repos["spira"].eject_held.as_ref().map(|h| h.eject.id.as_str()), Some("sp-a"));

    let mut snap = idle();
    snap.repos.get_mut("spira").unwrap().scoped_batches = Some(vec!["r9".into()]);
    let (still, acts) = decide(&held, &snap);
    assert!(acts.iter().all(|a| a.effect != EJECT), "a live pass scope holds the eject: {acts:?}");
    snap.repos.get_mut("spira").unwrap().scoped_batches = Some(vec![]);
    let (issued, acts) = decide(&still, &snap);
    let eject: Vec<Leg> = acts.iter().filter(|a| a.effect == EJECT).map(|a| serde_json::from_str(&a.input).unwrap()).collect();
    assert_eq!(eject.len(), 1, "{acts:?}");
    assert_eq!((eject[0].bead.as_str(), eject[0].held.as_ref().map(|h| h.csv.as_str())), ("sp-a", Some("test-x.sh")));

    let (done, acts) = on_outcome(&issued, EJECT, true, "{}", &eject[0], NOW);
    let rc = &done.repos["spira"];
    assert!(rc.eject_held.is_none() && rc.certify_held, "{rc:?}");
    assert_eq!(rc.members, vec![("sp-b".to_string(), "t2".to_string())]);
    assert!(acts.iter().all(|a| a.effect != ABANDON && a.effect != PAUSE), "{acts:?}");
}

/// The one re-issue is not retried for ever: refused again, held or not, the round ends red.
#[test_id(ti5j0z9)]
#[test]
fn sp_3p7m5b_a_held_eject_that_fails_again_after_its_re_issue_ends_the_round_red() {
    let mut c = driving(State::Certifying, "spira.e1");
    c.repos.get_mut("spira").unwrap().eject_held = Some(EjectHold { eject: Default::default(), issued: true });
    let leg = Leg { bead: "sp-a".into(), batch: "r9".into(), ..Leg::on("spira") };
    for body in [held_eject_error(), "queue round eject exited 1: refused".to_string()] {
        let (next, acts) = on_outcome(&c, EJECT, false, &body, &leg, NOW);
        assert!(acts.iter().any(|a| a.effect == ABANDON) && acts.iter().any(|a| a.effect == PAUSE), "{body}: {acts:?}");
        assert!(next.repos["spira"].eject_held.is_none());
    }
}

/// The eject the attribution leg could not issue comes back in its outcome line: it is held the same way.
#[test_id(tplg9dz)]
#[test]
fn sp_3p7m5b_an_attribution_blocked_by_a_held_eject_holds_it_instead_of_pausing() {
    let c = driving(State::Attributing, "spira.a1");
    let line = batcher_cut::attribute::Attribution::Blocked(format!("eject of sp-a from r9 held: {}", held_eject_error())).line();
    let mut r = carried(&c);
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.a1", runtime::units::Exit::Code(0), &format!("{line}\n"))).unwrap();
    let after = asked_after(&r, n);
    let names: Vec<&str> = after.iter().map(|(e, _)| e.as_str()).collect();
    assert!(!names.contains(&ABANDON) && !names.contains(&PAUSE), "{names:?}");
}

#[test_id(t0i8gwh)]
#[test]
fn sp_3p7m5b_the_pass_running_exit_is_the_one_the_queue_returns() {
    assert_eq!(batcher_cut::io::QUEUE_PASS_RUNNING_EXIT, queue::ops::round::PASS_RUNNING);
}

/// A refused eject still keeps the member's red tip out of the next cut.
#[test_id(t8wnbf5)]
#[test]
fn sp_3p7m5b_the_red_tip_is_recorded_before_the_eject_even_when_it_is_refused() {
    let dir = testkit::TempDir::new("red-tip");
    let got = spira_runtime::tasks::a::batcher_local::edge::record_then_eject(dir.path(), "spira", "sp-a", "t1", || Err("refused".into()));
    assert_eq!(got, Err("refused".into()));
    let pool = vec![batcher::core::Member { id: "sp-a".into(), tip: "t1".into(), title: String::new(), priority: None, express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new(), blocked_by: vec![], provisional: false }];
    let (kept, withheld) = batcher_cut::redtip::withhold(dir.path(), "spira", pool);
    assert!(kept.is_empty() && withheld.len() == 1, "{withheld:?}");
}

/// The pass exits green while the shared build-cache server is left in its scope: observe
/// stops the scope and certify starts on the next decide, not when the daemon dies.
#[test_id(txk9g3j)]
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
#[test_id(th9y3a1)]
#[test]
fn sp_b0lhx6_a_named_suite_red_attributes_the_certify_pass_and_the_survivors_land() {
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    let out = "queue.sh round certify: round r9 is RED at abc123: test-release-activate-config.sh\nred=test-release-activate-config.sh\n";
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), out)).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, l)| (e.as_str(), l.why.as_str())).collect::<Vec<_>>(), [(CORPUS, "attribute")], "no abandon yet: {after:?}");
    assert!(after[0].1.after_certify && after[0].1.reds.is_empty(), "the certify pass is the prior: {:?}", after[0].1);
    let unit = after[0].1.unit.clone();

    let land = batcher_cut::attribute::Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: None, mended: false, head: String::new() }.line();
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

/// The first red certify of the round, named suites or not, attributes it from the certify
/// pass's own results, for a sole member as for several: no eject, no abandon yet.
#[test_id(txiut3h)]
#[test]
fn a_first_red_certify_attributes_every_member_after_the_certify_pass() {
    for carry in [driving(State::Certifying, "spira.c3"), sole_member(driving(State::Certifying, "spira.c3"))] {
        let want = carry.repos["spira"].members.clone();
        let mut r = carried(&carry);
        let n = r.requested().len();
        r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "queue.sh round certify: round r9 is RED at abc: test-a.sh\nred=test-a.sh\n")).unwrap();
        kit::assert_unpoisoned(&r, STREAM);
        let after = asked_after(&r, n);
        assert_eq!(after.iter().map(|(e, l)| (e.as_str(), l.why.as_str())).collect::<Vec<_>>(), [(CORPUS, ATTRIBUTE)], "{after:?}");
        let leg = &after[0].1;
        assert!(leg.after_certify && leg.reds.is_empty() && leg.after_pass.is_empty(), "{leg:?}");
        assert_eq!(leg.members, want, "every member is attributed");
    }
}

/// The certify pass's red leads the pause of an attribution that ends with no outcome, and the
/// attribution's own last line follows it.
#[test_id(td7fk5v)]
#[test]
fn a_certify_red_leads_the_pause_of_an_attribution_that_ends_with_no_outcome() {
    let refusal = "results holds no deny list of the round's pass; refusing to attribute with another";
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    let certify = "queue.sh round certify: round r9 is RED at abc: spira-lint\nred-reason=spira-lint: member=sp-b r: g.rs:2: y\nred=spira-lint\n";
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), certify)).unwrap();
    let leg = asked_after(&r, n).into_iter().find(|(e, _)| e == CORPUS).expect("attribution leg").1;
    let n = r.requested().len();
    r.tick(STREAM, ended(&leg.unit, runtime::units::Exit::Code(2), &format!("batcher spira: attributing\n{refusal}\n"))).unwrap();
    let after = asked_after(&r, n);
    let pause = after.iter().find(|(e, _)| e == PAUSE).unwrap_or_else(|| panic!("no pause: {after:?}"));
    assert!(pause.1.why.starts_with("certify red in spira-lint: member=sp-b"), "{}", pause.1.why);
    assert!(pause.1.why.contains(refusal), "{}", pause.1.why);
}

/// A certify red that printed no `red-reason=` leads with its red suites, and a round whose
/// certify never went red keeps the reason it always had.
#[test_id(t6rux2a)]
#[test]
fn a_certify_red_without_a_reason_line_leads_with_its_red_suites_and_no_red_leaves_the_reason() {
    let mut r = carried(&driving(State::Certifying, "spira.c3"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "queue.sh round certify: round r9 is RED at abc: test-a.sh\nred=test-a.sh\n")).unwrap();
    let leg = asked_after(&r, n).into_iter().find(|(e, _)| e == CORPUS).expect("attribution leg").1;
    let n = r.requested().len();
    r.tick(STREAM, ended(&leg.unit, runtime::units::Exit::Code(2), "boom\n")).unwrap();
    let why = asked_after(&r, n).into_iter().find(|(e, _)| e == PAUSE).expect("pause").1.why;
    assert!(why.starts_with("certify red in test-a.sh; local corpus red — the attribution leg ended"), "{why}");

    let mut r = carried(&driving(State::Attributing, "spira.a2"));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.a2", runtime::units::Exit::Code(2), "boom\n")).unwrap();
    let why = asked_after(&r, n).into_iter().find(|(e, _)| e == PAUSE).expect("pause").1.why;
    assert!(why.starts_with("local corpus red — the attribution leg ended"), "{why}");
}

/// No row but the carried leg's own restart reaches Corpus, and a promotion that is not yet
/// attested certifies like an opened round.
#[test_id(tq75kdy)]
#[test]
fn no_path_runs_corpus_then_certify() {
    for (from, on, to) in TABLE {
        if *from != State::Corpus {
            assert!(*to != State::Corpus, "{from:?} × {on:?} reaches Corpus");
        }
    }
    assert_eq!(step(State::Opened, Trigger::OpenedWithSuites), Ok(State::Certifying));
    let mut c = driving(State::Promoting, "spira.c1");
    c.repos.get_mut("spira").unwrap().leg = None;
    let leg = Leg { repo: "spira".into(), ..Leg::default() };
    let body = serde_json::json!({"batch": "r9", "members": [["sp-a", "t1"]], "attested": false}).to_string();
    let (next, acts) = on_outcome(&c, PROMOTE, true, &body, &leg, NOW);
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [CERTIFY], "{acts:?}");
    assert_eq!(next.repos["spira"].state, State::Certifying);
}

/// The named-suite attribution runs once; a second named red abandons and says what it had.
#[test_id(t5zidgw)]
#[test]
fn sp_b0lhx6_a_named_suite_red_after_attribution_abandons_without_claiming_ejects() {
    let mut r = carried(&confirmed(driving(State::Certifying, "spira.c3")));
    let n = r.requested().len();
    r.tick(STREAM, ended("spira.c3", runtime::units::Exit::Code(1), "red=test-a.sh\n")).unwrap();
    let after = asked_after(&r, n);
    assert!(after.iter().any(|(e, _)| e == ABANDON), "{after:?}");
    assert!(!after.iter().any(|(e, _)| e == CORPUS), "{after:?}");
}

// --- sp-8nlvra: a held or skipping repo names why, and a red round's pause ends on a landing ---

/// `spira` with one admissible member, under the pause a red round wrote at base `b0`.
fn paused(base: &str) -> Snapshot {
    let mut s = Snapshot { repos: [("spira".to_string(), repo(vec![member("sp-a", "t1")]))].into(), now_ms: NOW, ..Default::default() };
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
#[test_id(t5g8kw5)]
#[test]
fn sp_8nlvra_a_paused_repo_is_held_and_names_the_pause() {
    let (carry, acts) = decide(&Carry::default(), &paused("b0"));
    assert!(acts.is_empty(), "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!(rc.state, State::Held);
    assert_eq!(rc.paused_on.as_deref(), Some("b0"), "the base the pause was seen on is carried");
    let why = rc.skip_why.clone().expect("a held repo names why");
    assert!(why.contains("paused") && why.contains("local corpus red") && why.contains("batcher pause-lift --repo spira"), "{why}");
}

fn paused_with_undriven_round() -> Snapshot {
    let mut s = paused("b0");
    let r = s.repos.get_mut("spira").unwrap();
    r.open_round = Some(Round { id: "r9".into(), members: vec!["sp-a".into()], head: "h".into(), worktree: "w".into(), phase: "opened".into(), age_secs: 60, ..Round::default() });
    s
}

#[test_id(t9xxmh8)]
#[test]
fn sp_s9qcq8_a_paused_repo_drives_an_open_round_it_does_not_drive_and_cuts_no_new_one() {
    let (carry, acts) = decide(&Carry::default(), &paused_with_undriven_round());
    let names = effects(&acts);
    assert!(names.contains(&REDRIVE), "{acts:?}");
    assert!(!names.contains(&FETCH) && !names.contains(&ASSEMBLE), "{acts:?}");
    let why = carry.repos["spira"].skip_why.clone().expect("the pause is still named");
    assert!(why.contains("paused") && why.contains("local corpus red"), "{why}");
}

#[test_id(taj4suw)]
#[test]
fn sp_s9qcq8_a_stopped_repo_still_holds_an_open_round() {
    let mut snap = paused_with_undriven_round();
    snap.repos.get_mut("spira").unwrap().stopped = true;
    let (_, acts) = decide(&Carry::default(), &snap);
    assert!(acts.is_empty(), "{acts:?}");
}

fn pause_on(mut snap: Snapshot) -> Snapshot {
    snap.repos.get_mut("spira").unwrap().paused = paused("b0").repos["spira"].paused.clone();
    snap
}

#[test_id(tws3kye)]
#[test]
fn sp_s9qcq8_a_paused_repo_lands_an_open_green_round_and_attributes_an_open_red_one() {
    let (carry, snap) = green_orphan();
    let (carry, acts) = decide(&carry, &pause_on(snap));
    assert_eq!(effects(&acts), [LC_CERTIFY, LAND], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Landing);
    let (carry, snap) = red_orphan();
    let (carry, acts) = decide(&carry, &pause_on(snap));
    assert_eq!(effects(&acts), [CORPUS], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Attributing);
}

#[test_id(t7uozw4)]
#[test]
fn sp_s9qcq8_a_paused_repo_with_no_open_round_is_held_and_one_that_drives_it_is_not() {
    let (carry, _) = decide(&Carry::default(), &paused("b0"));
    assert_eq!(carry.repos["spira"].state, State::Held);
    let (carry, _) = decide(&carry, &paused_with_undriven_round());
    assert_ne!(carry.repos["spira"].state, State::Held);
}

/// Once a round has landed past the base the pause was seen on, the red it guarded is behind the
/// repo: the pass lifts the pause and cuts the waiting member.
#[test_id(tnjmrvg)]
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
#[test_id(taz46qq)]
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

/// A red pause no landing lifts (nothing lands while it holds) is lifted by the task itself once
/// it has outlived `SPIRA_BATCHER_PAUSE_LIFT_SECS`, and the repo cuts in the same pass.
fn expiring(held_secs: u64, lifts: u32) -> (Carry, Snapshot) {
    let mut snap = paused("b0");
    snap.repos.get_mut("spira").unwrap().paused_since = Some(NOW / 1000 - held_secs);
    let mut carry = paused_on("b0");
    carry.repos.get_mut("spira").unwrap().pace.pause_lifts = lifts;
    (carry, snap)
}

fn pinned_lift(secs: u64) -> Policy {
    Policy { pause_lift_s: secs, ..Policy::default() }
}

#[test_id(txu95fh)]
#[test]
fn sp_qmd5g4_a_red_pause_past_its_bound_is_lifted_and_cuts_with_no_landing() {
    let (carry, snap) = expiring(500, 0);
    let (carry, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(names, [PAUSE, INBOX_APPEND, FETCH, ASSEMBLE], "{acts:?}");
    let lift: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(lift.bead, "lift", "{lift:?}");
    assert!(lift.why.contains("500s") && lift.why.contains("400s"), "{}", lift.why);
    assert_eq!(carry.repos["spira"].pace.pause_lifts, 1);
}

#[test_id(tqh5g74)]
#[test]
fn sp_qmd5g4_a_pause_exactly_at_its_bound_is_lifted() {
    let (carry, snap) = expiring(400, 0);
    let (_, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    assert_eq!(acts[0].effect, PAUSE, "{acts:?}");
}

#[test_id(t2mvltp)]
#[test]
fn sp_qmd5g4_the_lift_bound_is_read_from_its_key() {
    let pinned: BTreeMap<String, String> = [("SPIRA_BATCHER_PAUSE_LIFT_SECS".to_string(), "77".to_string())].into();
    assert_eq!(Policy::pinned(&pinned).pause_lift_s, 77);
    assert_eq!(Policy::pinned(&BTreeMap::new()).pause_lift_s, 900);
}

#[test_id(tlb70fi)]
#[test]
fn sp_qmd5g4_a_red_pause_inside_its_bound_stays_held() {
    let (carry, snap) = expiring(300, 0);
    let (carry, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Held);
}

#[test_id(tgrixnx)]
#[test]
fn sp_qmd5g4_a_second_red_after_a_lift_stays_paused_however_long_it_stands() {
    let (carry, snap) = expiring(100_000, 1);
    let (carry, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    assert_eq!(effects(&acts), [INBOX_APPEND, PAUSE_ASK], "it pages and asks, and cuts nothing: {acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Held);
}

/// A red pause held past its bound with its lift spent: the page and the ask, once per pause.
fn unattended(since: u64) -> (Carry, Snapshot) {
    let (carry, mut snap) = expiring(0, batcher_cut::pace::PAUSE_LIFTS_MAX);
    let r = snap.repos.get_mut("spira").unwrap();
    r.paused_since = Some(since);
    r.pause_evidence = Some("unit-tests failed: it::batcher_x::the_failing_test; thread 'it::batcher_x::the_failing_test' panicked at src/x.rs:1:1".into());
    snap.now_ms = (since + 400 + 1) * 1000;
    (carry, snap)
}

#[test_id(tj209i3)]
#[test]
fn sp_plukb4_a_pause_held_past_its_bound_with_its_lift_spent_pages_and_asks_once_per_pause() {
    let t = NOW / 1000;
    let (carry, snap) = unattended(t);
    let (carry, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    assert_eq!(effects(&acts), [INBOX_APPEND, PAUSE_ASK], "{acts:?}");
    assert!(acts[0].input.contains("PAUSE UNATTENDED spira:"), "{}", acts[0].input);
    for want in ["held 401s", "1 certified waiting", "the_failing_test", "panicked at"] {
        assert!(acts[0].input.contains(want), "{want}: {}", acts[0].input);
    }
    assert_eq!(acts[1].key, format!("spira:pause:{t}"));
    assert!(acts[1].input.contains("MOOT-WHEN: batcher pause-show --repo spira"), "{}", acts[1].input);
    assert!(acts[1].input.contains(&t.to_string()), "the predicate names this pause's since: {}", acts[1].input);
    let (carry, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    assert!(acts.is_empty(), "the same pause is not paged again: {acts:?}");
    let (_, snap2) = unattended(t + 5000);
    let (_, acts) = decide_with(&pinned_lift(400), &carry, &snap2);
    assert_eq!(effects(&acts), [INBOX_APPEND, PAUSE_ASK], "a new pause pages afresh: {acts:?}");
}

#[test_id(tz5hrwe)]
#[test]
fn sp_plukb4_an_unattended_pause_with_no_work_waiting_or_a_hold_pages_nothing() {
    let (carry, mut snap) = unattended(NOW / 1000);
    snap.repos.get_mut("spira").unwrap().pool.clear();
    let (_, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    assert!(acts.is_empty(), "{acts:?}");
    let (carry, mut snap) = unattended(NOW / 1000);
    snap.repos.get_mut("spira").unwrap().paused = Some("hold: operator maintenance".into());
    let (_, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    assert!(acts.is_empty(), "{acts:?}");
}

#[test_id(th8qq1o)]
#[test]
fn sp_qmd5g4_an_operators_hold_is_never_lifted_by_the_bound() {
    let (carry, mut snap) = expiring(100_000, 0);
    snap.repos.get_mut("spira").unwrap().paused = Some("hold: operator maintenance".into());
    let (carry, acts) = decide_with(&pinned_lift(400), &carry, &snap);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Held);
}

fn red_ran(leg: &str) -> runtime::units::UnitRan {
    runtime::units::UnitRan { exit: runtime::units::Exit::Code(1), ..ran_of(leg) }
}

fn ran_of(leg: &str) -> runtime::units::UnitRan {
    runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, leg).unwrap(), exit: runtime::units::Exit::Code(0), duration_ms: 1 }
}

/// A round the attribution emptied by ejecting every member holds no red: it is abandoned and
/// writes no pause.
#[test_id(t4ygo78)]
#[test]
fn sp_56d3l3_a_round_emptied_by_its_own_ejects_is_abandoned_without_a_pause() {
    use batcher_cut::attribute::Attribution;
    for outcome in [Attribution::Emptied("unit-tests red ejected every member".into()), Attribution::Land { members: vec![], attribution_secs: None, mended: false, head: String::new() }] {
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
#[test_id(tpls93r)]
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
#[test_id(tinpn77)]
#[test]
fn sp_esyv8o_a_vanished_pause_forgets_its_base_once_the_repo_has_moved_past_it() {
    let mut gone = paused("b2");
    gone.repos.get_mut("spira").unwrap().paused = None;
    let (carry, _) = decide(&paused_on("b1"), &gone);
    assert_eq!(carry.repos["spira"].paused_on, None);
}

/// An attribution leg stopped by a signal says nothing about the round: the Concierge's abandon
/// stops it with SIGTERM, and a pause written for that holds the repo against nothing red.
#[test_id(t11jdam)]
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
#[test_id(tzo1sn5)]
#[test]
fn sp_esyv8o_an_attribution_leg_killed_by_another_signal_still_pauses() {
    for sig in [9, 11] {
        let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, "spira.a2").unwrap(), exit: runtime::units::Exit::Signal(sig), duration_ms: 1 };
        let (_, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran, &runtime::units::RanOutput::default(), NOW).expect("its own leg");
        assert!(acts.iter().any(|a| a.effect == PAUSE), "signal {sig}: {acts:?}");
    }
}

/// An operator's deliberate hold (`hold:`) is never lifted by a landing.
#[test_id(ti5fr15)]
#[test]
fn sp_8nlvra_a_deliberate_hold_survives_a_landing() {
    let mut s = paused("b1");
    s.repos.get_mut("spira").unwrap().paused = Some("hold: publish red".into());
    let (carry, acts) = decide(&paused_on("b0"), &s);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Held);
}

/// A scoped stop names its owner's reason.
#[test_id(tg2pec4)]
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
#[test_id(t4sfher)]
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
    let (_, later) = decide(&carry, &Snapshot { repos: [("spira".to_string(), Repo { paused: None, ..s.repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() });
    assert!(later.is_empty(), "{later:?}");
}

/// A feature group whose every member waits on an unlanded prerequisite must not starve the
/// catch-all: the blocked are refused first, then the round is selected from what is left.
#[test_id(trhglwk)]
#[test]
fn sp_8nlvra_a_blocked_feature_group_does_not_starve_the_catch_all() {
    let mut x1 = member("sp-x.1", "t1");
    x1.deps = vec!["sp-z".into()];
    let mut x2 = member("sp-x.2", "t2");
    x2.deps = vec!["sp-z".into()];
    let mut r = repo(vec![x1, x2, member("sp-c", "t3")]);
    r.blockers = vec!["sp-z".into()];
    let s = Snapshot { repos: [("spira".to_string(), r)].into(), now_ms: NOW, ..Default::default() };
    let (carry, acts) = decide(&Carry::default(), &s);
    let assemble = acts.iter().find(|a| a.effect == ASSEMBLE).unwrap_or_else(|| panic!("the catch-all member cuts: {acts:?} {carry:?}"));
    let leg: Leg = serde_json::from_str(&assemble.input).unwrap();
    assert_eq!(leg.members, [("sp-c".to_string(), "t3".to_string())]);
}

/// Members wait but none is admissible: the pass names each and why.
#[test_id(tf4r9vt)]
#[test]
fn sp_8nlvra_a_pool_with_nothing_admissible_names_each_member() {
    let mut s = busy();
    for m in &mut s.repos.get_mut("spira").unwrap().pool {
        m.sifted_tip = Some("old".into());
    }
    let (carry, acts) = decide(&Carry::default(), &s);
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [NOTE_LEFT_OUT], "{acts:?}");
    let why = carry.repos["spira"].skip_why.clone().expect("named");
    assert!(why.contains("sp-a: no sift pass at its tip t1") && why.contains("sp-b: no sift pass at its tip t2"), "{why}");
}

/// The log owes one line per change of a repo's named reason, and one when it clears.
#[test_id(tefzpbj)]
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
    let rc = RepoCarry { state: State::Opened, round_key: Some("k".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into()), ("sp-c".into(), "t3".into())], wt: "w".into(), base: "b1".into(), ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn open_leg() -> Leg {
    Leg { why: "suites".into(), ..Leg::on("spira") }
}

/// 2026-11-07 05:26Z: four cuts abandoned members-differ because the open would not admit
/// sp-1mspuc. The open now names it (typed), and the batcher opens the others at once.
#[test_id(ttcgwbr)]
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
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [CERTIFY], "{acts:?}");
    assert_eq!(carry.repos["spira"].members.len(), 2);
}

/// The refused member stays out of later cuts at the same tip and base, named; a moved base
/// lets it be tried again.
#[test_id(touyvxo)]
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

// --- sp-4pne6q: an image-absent refusal builds its tag once and readmits when the store holds it ---

const BUILD: &str = "batcher.local.image_build";

/// `rc` read from `v`, so a field the release under test lacks is ignored rather than refused.
fn rc_of(v: serde_json::Value) -> RepoCarry {
    serde_json::from_value(v).unwrap()
}

fn refusing_image(carry: &Carry, refused: &[(&str, &str)]) -> (Carry, Vec<Act>) {
    let body = serde_json::json!({
        "refused": refused.iter().map(|(id, _)| (id, "image-absent", "needs an image")).collect::<Vec<_>>(),
        "images": refused.iter().copied().collect::<BTreeMap<_, _>>(),
    });
    on_outcome(carry, ROUND_OPEN, true, &body.to_string(), &open_leg(), NOW)
}

fn builds(acts: &[Act]) -> Vec<Leg> {
    acts.iter().filter(|a| a.effect == BUILD).map(|a| serde_json::from_str(&a.input).unwrap()).collect()
}

fn image_wants(carry: &Carry) -> serde_json::Value {
    serde_json::to_value(&carry.repos["spira"]).unwrap()["images"].clone()
}

fn observing(images_present: &[&str]) -> Snapshot {
    let mut v = serde_json::to_value(busy()).unwrap();
    v["repos"]["spira"]["images_present"] = serde_json::json!(images_present);
    serde_json::from_value(v).unwrap()
}

fn assembled_ids(acts: &[Act]) -> Vec<String> {
    let leg: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ASSEMBLE).expect("a cut").input).unwrap();
    leg.members.into_iter().map(|(id, _)| id).collect()
}

fn awaiting_image(failed: bool) -> Carry {
    let rc = rc_of(serde_json::json!({
        "state": "Idle", "round_key": null,
        "refused": {"sp-b": ["t2", "b1", "image-absent"]},
        "images": {"tagz": {"tip": "t2", "members": ["sp-b"], "unit": null, "failed": failed}},
    }));
    Carry { repos: [("spira".to_string(), rc)].into() }
}

#[test_id(t98kfsc)]
#[test]
fn sp_4pne6q_an_image_absent_member_is_cut_once_the_store_holds_its_tag_at_the_same_tip_and_base() {
    for failed in [false, true] {
        let (waiting, acts) = decide(&awaiting_image(failed), &observing(&["other"]));
        assert_eq!(assembled_ids(&acts), ["sp-a"], "stays out while the tag is absent");
        assert!(waiting.repos["spira"].refused.contains_key("sp-b"));
        let (ready, acts) = decide(&awaiting_image(failed), &observing(&["tagz"]));
        assert_eq!(assembled_ids(&acts), ["sp-a", "sp-b"], "failed={failed}: a hand build readmits too");
        assert!(ready.repos["spira"].refused.is_empty());
        assert!(image_wants(&ready).as_object().is_none_or(|m| m.is_empty()), "{:?}", image_wants(&ready));
    }
}

#[test_id(tgdrrw3)]
#[test]
fn sp_4pne6q_a_want_whose_members_tips_moved_is_pruned() {
    let mut snap = observing(&[]);
    snap.repos.get_mut("spira").unwrap().pool[1] = member("sp-b", "t9");
    let (next, acts) = decide(&awaiting_image(true), &snap);
    assert_eq!(assembled_ids(&acts), ["sp-a", "sp-b"]);
    assert!(image_wants(&next).as_object().is_none_or(|m| m.is_empty()), "{:?}", image_wants(&next));
}

#[test_id(ttb73yy)]
#[test]
fn sp_4pne6q_one_image_build_starts_per_tag_however_many_members_need_it() {
    let (carry, acts) = refusing_image(&opening(), &[("sp-b", "tagz"), ("sp-c", "tagz")]);
    let first = builds(&acts);
    assert_eq!(first.len(), 1, "{acts:?}");
    assert_eq!(acts.iter().find(|a| a.effect == BUILD).unwrap().key, "spira:image:tagz");
    assert!(acts.iter().any(|a| a.effect == ROUND_OPEN), "the others' round does not wait: {acts:?}");
    assert_eq!(image_wants(&carry)["tagz"]["members"], serde_json::json!(["sp-b", "sp-c"]));

    let again = |carry: &Carry, tag: &str| {
        let mut next = carry.clone();
        let rc = next.repos.get_mut("spira").unwrap();
        (rc.state, rc.members) = (State::Opened, vec![("sp-a".into(), "t1".into()), ("sp-d".into(), "t4".into())]);
        refusing_image(&next, &[("sp-d", tag)])
    };
    let (joined, acts) = again(&carry, "tagz");
    assert!(builds(&acts).is_empty(), "a want that is building starts nothing: {acts:?}");
    assert_eq!(image_wants(&joined)["tagz"]["members"], serde_json::json!(["sp-b", "sp-c", "sp-d"]));
    let (_, acts) = again(&carry, "tagy");
    assert_eq!(builds(&acts).len(), 1, "another tag is another build: {acts:?}");

    let mut failed = serde_json::to_value(&carry).unwrap();
    failed["repos"]["spira"]["images"]["tagz"]["failed"] = true.into();
    let (_, acts) = again(&serde_json::from_value(failed).unwrap(), "tagz");
    assert!(builds(&acts).is_empty(), "a failed tag is not built again: {acts:?}");
}

fn building() -> (Carry, String) {
    let (carry, acts) = refusing_image(&opening(), &[("sp-b", "tagz"), ("sp-c", "tagz")]);
    let unit = builds(&acts).first().expect("a build").unit.clone();
    (carry, unit)
}

fn image_ran(carry: &Carry, unit: &str, code: i32, said: &str) -> (Carry, Vec<Act>) {
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, unit).unwrap(), exit: runtime::units::Exit::Code(code), duration_ms: 1 };
    let out = runtime::units::RanOutput { stdout_tail: Some(said.into()), stderr_tail: None };
    on_ran_with(carry, &ran, &out, NOW).expect("an image build's run is this task's")
}

#[test_id(tb7g8z3)]
#[test]
fn sp_4pne6q_a_failed_image_build_marks_the_want_failed_and_pages_the_hand_build_once() {
    let (carry, unit) = building();
    assert_eq!(unit, "spira.image-tagz");
    let (next, acts) = image_ran(&carry, &unit, 3, "STEP 4/9\nerror: no such package\n");
    assert_eq!(image_wants(&next)["tagz"]["failed"], true);
    let lines: Vec<&Act> = acts.iter().filter(|a| a.effect == INBOX_APPEND).collect();
    assert_eq!(lines.len(), 1, "{acts:?}");
    for want in ["tagz", "sp-b", "sp-c", "exit 3", "error: no such package", spira_config::recipe::HAND_BUILD] {
        assert!(lines[0].input.contains(want), "{want}: {}", lines[0].input);
    }
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [IMAGE_CLEAN, INBOX_APPEND], "nothing is started");
    let (_, again) = image_ran(&next, &unit, 3, "");
    assert!(!again.iter().any(|a| a.effect == INBOX_APPEND || a.effect == BUILD), "a second delivery pages nothing: {again:?}");
}

#[test_id(tciy8zr)]
#[test]
fn sp_4pne6q_a_build_that_succeeds_only_removes_its_checkout() {
    let (carry, unit) = building();
    let (next, acts) = image_ran(&carry, &unit, 0, "");
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [IMAGE_CLEAN], "{acts:?}");
    assert_eq!(image_wants(&next)["tagz"]["failed"], false);
    assert!(next.repos["spira"].refused.contains_key("sp-b"), "the member waits for the store to hold the tag");
    let (_, acts) = image_ran(&Carry::default(), &unit, 0, "");
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [IMAGE_CLEAN], "a want already readmitted still loses its checkout");
}

#[test_id(trs5e7x)]
#[test]
fn sp_4pne6q_a_build_that_could_not_start_is_failed_and_one_lost_at_a_restart_is_asked_again() {
    let (carry, _) = building();
    let (_, acts) = refusing_image(&opening(), &[("sp-b", "tagz")]);
    let leg = builds(&acts).remove(0);
    let (failed, acts) = on_outcome(&carry, BUILD, false, "no unit manager configured", &leg, NOW);
    assert_eq!(image_wants(&failed)["tagz"]["failed"], true);
    assert!(acts.iter().any(|a| a.effect == INBOX_APPEND && a.input.contains("no unit manager")), "{acts:?}");
    let (lost, acts) = on_outcome(&carry, BUILD, false, runtime::engine::LOST_AT_RESTART, &leg, NOW);
    assert_eq!(builds(&acts), [leg], "the effect is idempotent over a running unit");
    assert_eq!(image_wants(&lost)["tagz"]["failed"], false);
}

#[test_id(tupb5j6)]
#[test]
fn sp_4pne6q_a_build_past_the_wall_with_no_result_is_stopped_and_failed() {
    let (carry, _) = building();
    let wall = Policy::default().wall_ms + LEG_GRACE_MS;
    let at = |ms: u64| Snapshot { now_ms: NOW + ms, ..observing(&[]) };
    let (_, acts) = decide(&carry_idle(&carry), &at(wall));
    assert!(!acts.iter().any(|a| a.effect == runtime::units::UNIT_STOP), "{acts:?}");
    let (next, acts) = decide(&carry_idle(&carry), &at(wall + 1));
    assert!(acts.iter().any(|a| a.effect == runtime::units::UNIT_STOP), "{acts:?}");
    assert!(acts.iter().any(|a| a.effect == IMAGE_CLEAN), "{acts:?}");
    assert!(acts.iter().any(|a| a.effect == INBOX_APPEND && a.input.contains("tagz")), "{acts:?}");
    assert_eq!(image_wants(&next)["tagz"]["failed"], true);
}

/// `carry` with its round gone and only what the repo shares left, its members' refusals kept.
fn carry_idle(carry: &Carry) -> Carry {
    let rc = &carry.repos["spira"];
    let idle = RepoCarry { refused: rc.refused.clone(), images: rc.images.clone(), ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), idle)].into() }
}

#[test_id(tj53t42)]
#[test]
fn sp_4pne6q_only_a_wanted_tag_is_looked_up_in_the_store() {
    let none = images_held(&RepoCarry::default(), &|tag| panic!("an idle observe looked up {tag}"));
    assert!(none.is_empty());
    let (carry, _) = building();
    assert_eq!(images_held(&carry.repos["spira"], &|tag| tag == "tagz"), ["tagz"]);
}

/// A refusal the pool read already shows (held, state) is not remembered: once the pool lists the
/// member again at the same tip and base it is cut; one the pool cannot see stays out.
#[test_id(trov4c8)]
#[test]
fn sp_7der7t_a_member_refused_held_is_cut_once_the_pool_shows_it_unheld() {
    for (kind, cut) in [("held", true), ("state", true), ("no-row", true), ("integration", false), ("text-only", false), ("batch-red", false)] {
        let refused = Opened { refused: vec![("sp-b".into(), kind.into(), "sp-b: refused".into())], ..Opened::default() };
        let mut carry = opening();
        carry.repos.get_mut("spira").unwrap().members = vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())];
        let (carry, acts) = on_outcome(&carry, ROUND_OPEN, true, &serde_json::to_string(&refused).unwrap(), &open_leg(), NOW);
        let reopen: Leg = serde_json::from_str(&acts[0].input).unwrap();
        assert_eq!(reopen.members, [("sp-a".to_string(), "t1".to_string())], "{kind}: the re-open is without it");
        let mut world = busy();
        world.repos.get_mut("spira").unwrap().base_sha = "b1".into();
        let idle = Carry { repos: [("spira".to_string(), RepoCarry { refused: carry.repos["spira"].refused.clone(), ..RepoCarry::default() })].into() };
        let (_, acts) = decide(&idle, &world);
        let leg: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ASSEMBLE).expect("a leg").input).unwrap();
        assert_eq!(leg.members.iter().any(|(id, _)| id == "sp-b"), cut, "{kind}: {leg:?}");
    }
}

/// 2026-11-21: sp-7wx6ql was left out of two cuts and nobody was told why.
#[test_id(t8knuvh)]
#[test]
fn sp_7der7t_a_cut_names_each_member_it_left_out() {
    let mut unsifted = member("sp-c", "t3");
    unsifted.sifted_tip = None;
    let mut r = repo(vec![member("sp-a", "t1"), member("sp-b", "t2"), unsifted]);
    r.left_out = vec![batcher_cut::io::LeftOut { id: "sp-d".into(), tip: "t4".into(), cause: batcher_cut::io::Cause::Held(vec!["manual".into()]) }];
    let snap = Snapshot { repos: [("spira".to_string(), r)].into(), now_ms: NOW, ..Default::default() };
    let rc = RepoCarry { refused: [("sp-b".to_string(), ("t2".to_string(), "b1".to_string(), "integration".to_string()))].into(), ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let (next, acts) = decide(&carry, &snap);
    assert!(acts.iter().any(|a| a.effect == ASSEMBLE), "sp-a is cut: {acts:?}");
    let notes: Vec<&Act> = acts.iter().filter(|a| a.effect == NOTE_LEFT_OUT).collect();
    assert_eq!(notes.len(), 1, "{acts:?}");
    let leg: Leg = serde_json::from_str(&notes[0].input).unwrap();
    let left: Vec<batcher_cut::io::LeftOut> = serde_json::from_str(&leg.why).unwrap();
    let mut got: Vec<(&str, String)> = left.iter().map(|l| (l.id.as_str(), l.tag())).collect();
    got.sort();
    assert_eq!(got, [("sp-b", "refused-integration".to_string()), ("sp-c", "no-sift".to_string()), ("sp-d", "held-manual".to_string())]);
    let (_, again) = decide(&next, &snap);
    assert!(!again.iter().any(|a| a.effect == NOTE_LEFT_OUT), "an unchanged list is not noted again: {again:?}");
}

/// A feature round leaves out what is not part of it; a full round leaves out its overflow; a
/// dependency on an unlanded bead leaves the dependant out.
#[test_id(tho8n0s)]
#[test]
fn sp_fssqg1_the_selection_names_the_feature_full_and_blocked_members_it_drops() {
    let mut blocked = member("sp-e", "t5");
    blocked.deps = vec!["sp-gone".into()];
    let mut r = repo(vec![member("sp-f.1", "t1"), member("sp-f.2", "t2"), member("sp-f.3", "t3"), member("sp-x", "t4"), blocked]);
    r.blockers = vec!["sp-gone".into()];
    let (selected, left) = spira_runtime::tasks::a::batcher_local::select_split(&r, 2);
    assert!(selected.is_ok());
    let mut got: Vec<(String, String)> = left.iter().map(|l| (l.id.clone(), l.tag())).collect();
    got.sort();
    assert_eq!(got, [("sp-e".to_string(), "blocked".to_string()), ("sp-f.3".to_string(), "round-full".to_string()), ("sp-x".to_string(), "outside-feature".to_string())]);
}

/// A list that empties clears the records once; an idle empty list pushes nothing.
#[test_id(tizyw2v)]
#[test]
fn sp_fssqg1_a_list_that_empties_is_pushed_once_to_clear_its_records() {
    let rc = RepoCarry { left_noted: vec![("sp-d".into(), "held-manual".into(), "t4".into())], ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let (next, acts) = decide(&carry, &busy());
    assert_eq!(acts.iter().filter(|a| a.effect == NOTE_LEFT_OUT).count(), 1, "{acts:?}");
    let (_, again) = decide(&next, &busy());
    assert!(!again.iter().any(|a| a.effect == NOTE_LEFT_OUT), "{again:?}");
}

/// A note that failed leaves the list unnamed, so the next decide pushes it again.
#[test_id(tp7byb6)]
#[test]
fn sp_fssqg1_a_failed_left_out_note_is_pushed_again() {
    let mut r = repo(vec![]);
    r.left_out = vec![batcher_cut::io::LeftOut { id: "sp-d".into(), tip: "t4".into(), cause: batcher_cut::io::Cause::Held(vec!["manual".into()]) }];
    let snap = Snapshot { repos: [("spira".to_string(), r)].into(), now_ms: NOW, ..Default::default() };
    let (named, acts) = decide(&Carry::default(), &snap);
    let leg: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == NOTE_LEFT_OUT).expect("first push").input).unwrap();
    let (failed, _) = on_outcome(&named, NOTE_LEFT_OUT, false, "bead down", &leg, NOW);
    let (_, again) = decide(&failed, &snap);
    assert!(again.iter().any(|a| a.effect == NOTE_LEFT_OUT), "{again:?}");
}

/// Every named member refused: no round, no abandon, and the repo is idle with the members out.
#[test_id(tyrbnu4)]
#[test]
fn sp_822b8l_an_open_refusing_every_member_abandons_nothing() {
    let all = Opened { refused: ["sp-a", "sp-b", "sp-c"].iter().map(|id| (id.to_string(), "gate-red".to_string(), format!("{id}: its gate is FAIL"))).collect(), ..Opened::default() };
    let (carry, acts) = on_outcome(&opening(), ROUND_OPEN, true, &serde_json::to_string(&all).unwrap(), &open_leg(), NOW);
    assert!(!acts.iter().any(|a| a.effect == ABANDON || a.effect == PAUSE), "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.refused.len()), (State::Idle, 3));
}

/// Fails the open `n` times, each from a fresh `Opened` state carrying the previous pace.
fn fail_opens(bodies: &[&str]) -> (Carry, Vec<Vec<String>>) {
    let mut carry = opening();
    let mut all = Vec::new();
    for body in bodies {
        let (next, acts) = on_outcome(&carry, ROUND_OPEN, false, body, &open_leg(), NOW);
        all.push(pages(&acts).into_iter().map(|a| a.input.clone()).collect());
        let mut again = opening();
        again.repos.get_mut("spira").unwrap().pace = next.repos["spira"].pace.clone();
        carry = again;
    }
    (carry, all)
}

#[test_id(tl09qdg)]
#[test]
fn sp_64x7v2_one_failed_open_does_not_page_and_a_later_open_keeps_the_count_at_zero() {
    let (carry, all) = fail_opens(&[""]);
    assert!(all[0].is_empty(), "{all:?}");
    let ok = serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap();
    let (carry, _) = on_outcome(&carry, ROUND_OPEN, true, &ok, &open_leg(), NOW);
    assert_eq!(carry.repos["spira"].pace.open_failed, 0);
}

#[test_id(tgdxksi)]
#[test]
fn sp_64x7v2_three_failed_opens_in_a_row_page_once_with_the_last_body() {
    let lock = "spira: another operation holds the lock";
    let (_, all) = fail_opens(&["a", "b", lock]);
    assert!(all[0].is_empty() && all[1].is_empty(), "{all:?}");
    assert_eq!(all[2].len(), 1, "{all:?}");
    assert!(all[2][0].contains(lock), "{all:?}");
}

#[test_id(t8l169s)]
#[test]
fn sp_64x7v2_a_timed_out_open_page_says_it_timed_out_and_never_shows_empty_parens() {
    let (_, all) = fail_opens(&["", "", ""]);
    assert!(all[2][0].contains("did not answer within 30s") && !all[2][0].contains("()"), "{all:?}");
}

// --- sp-xmqd8k: a refused landing of a green round leaves the round open ---

/// A landing refused for one member's change is not a red round: the round stays open with all
/// its members, the land output is kept, no pause is written, and the page carries the refusing
/// line and the verbs that settle it.
#[test_id(tvs8x8p)]
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
#[test_id(tync6c5)]
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
    assert_eq!(after, [LC_CERTIFY, LAND, SUPERSEDE_REDS], "green again: it lands");

    let (_, acts) = on_outcome(&Carry { repos: [("spira".to_string(), RepoCarry { state: State::Landing, ..carry.repos["spira"].clone() })].into() }, LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &land, NOW);
    assert!(!acts.iter().any(|a| a.effect == ABANDON), "{acts:?}");
}

/// The activation restart re-drives LAND on a round that already landed; the queue answers it
/// ok, and that lands the round — no abandon, no red pause.
#[test_id(tj6fhhj)]
#[test]
fn a_land_re_driven_after_it_landed_is_landed_not_red() {
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let (carry, acts) = on_outcome(&landing(), LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &land, NOW);
    assert!(!acts.iter().any(|a| a.effect == ABANDON), "{acts:?}");
    assert!(!carry.repos.contains_key("spira"), "the round is finished, not held red: {:?}", carry.repos);
}

fn supersede_legs(acts: &[Act]) -> Vec<Leg> {
    acts.iter().filter(|a| a.effect == SUPERSEDE_REDS).map(|a| serde_json::from_str(&a.input).unwrap()).collect()
}

#[test_id(te1d7ox)]
#[test]
fn a_landed_round_asks_for_the_repos_local_reds_to_be_closed() {
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let (_, acts) = on_outcome(&landing(), LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &land, NOW);
    let legs = supersede_legs(&acts);
    assert_eq!(legs.len(), 1, "{acts:?}");
    assert_eq!(legs[0].repo, "spira");
}

#[test_id(ta8pjsf)]
#[test]
fn a_landed_round_hands_its_members_to_the_registry_clearing() {
    let land = Leg { batch: "r9".into(), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], ..Leg::on("spira") };
    let (_, acts) = on_outcome(&landing(), LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &land, NOW);
    assert_eq!(supersede_legs(&acts)[0].members, land.members, "{acts:?}");
}

#[test_id(thi6bra)]
#[test]
fn a_staged_landing_promotes_and_asks_for_the_local_reds_to_be_closed() {
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let staged = serde_json::to_string(&Landed { staged: Some("r10".into()), ..Landed::default() }).unwrap();
    let (_, acts) = on_outcome(&landing(), LAND, true, &staged, &land, NOW);
    assert!(acts.iter().any(|a| a.effect == PROMOTE), "{acts:?}");
    assert_eq!(supersede_legs(&acts).len(), 1, "{acts:?}");
}

#[test_id(t31xibs)]
#[test]
fn a_base_moved_or_refused_land_closes_no_local_reds() {
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let moved = serde_json::to_string(&Landed { base_moved: Some("b2".into()), ..Landed::default() }).unwrap();
    let (_, acts) = on_outcome(&landing(), LAND, true, &moved, &land, NOW);
    assert!(supersede_legs(&acts).is_empty(), "{acts:?}");
    let (_, acts) = on_outcome(&landing(), LAND, false, "refused", &land, NOW);
    assert!(supersede_legs(&acts).is_empty(), "{acts:?}");
}

/// At most once per base change: base-moved again on the same base is a red, abandoned.
#[test_id(tv0k615)]
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
#[test_id(teuhum2)]
#[test]
fn a_leg_unit_carries_the_release_path_home_and_config_spec() {
    use spira_runtime::tasks::a::batcher_local::edge::leg_env;
    let env = leg_env(vec![("ROUND_BATCH".into(), "b1".into())], "r", "h", "c/cfg", "x/bin:usr/bin");
    let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(get("SPIRA_TOML").as_deref(), Some("c/cfg"));
    assert_eq!(get("SPIRA_RELEASE").as_deref(), Some("r"));
    assert_eq!(get("HOME").as_deref(), Some("h"));
    assert!(get("PATH").is_some_and(|p| p.starts_with("r/bin:")));
    assert_eq!(get("ROUND_BATCH").as_deref(), Some("b1"), "the leg's own variables are kept");
    let kept = leg_env(vec![("SPIRA_TOML".into(), "own.toml".into())], "r", "h", "c/cfg", "x/bin");
    assert_eq!(kept.iter().filter(|(n, _)| n == "SPIRA_TOML").count(), 1);
    assert_eq!(kept.iter().find(|(n, _)| n == "SPIRA_TOML").map(|(_, v)| v.as_str()), Some("own.toml"));
}

#[test_id(tlkw2p1)]
#[test]
fn a_leg_unit_path_keeps_the_release_first_then_the_runtime_path_so_aws_is_found() {
    use spira_runtime::tasks::a::batcher_local::edge::leg_env;
    let path_of = |runtime: &str| leg_env(vec![], "r", "h", "c/cfg", runtime).into_iter().find(|(n, _)| n == "PATH").map(|(_, v)| v).unwrap();
    assert_eq!(path_of("opt/tools/bin:usr/bin"), "r/bin:r/spira:opt/tools/bin:usr/bin");
    assert_eq!(path_of(""), format!("r/bin:r/spira:{}", system_paths::SYSTEM_PATH), "no runtime PATH falls back to the system dirs");
}

#[test_id(t35obfe)]
#[test]
fn the_unit_environment_built_for_a_leg_ends_in_the_runtimes_own_path() {
    use spira_runtime::tasks::a::batcher_local::edge::base_env;
    let own = std::env::var("PATH").expect("test process has a PATH");
    let env = base_env(vec![], "c/cfg");
    let path = env.into_iter().find(|(n, _)| n == "PATH").map(|(_, v)| v).unwrap();
    assert!(path.ends_with(&format!(":{own}")), "{path} should end with the runtime PATH {own}");
}

/// A round whose land outcome never came back holds its state; the repo must not sit idle behind it.
#[test_id(takksm3)]
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
#[test_id(ttnhjt0)]
#[test]
fn sp_2gcs4h_a_certifying_round_is_never_forgotten_for_waiting() {
    let (seen, _) = decide(&driving(State::Certifying, "u"), &Snapshot { now_ms: NOW, ..idle() });
    let (next, _) = decide(&seen, &Snapshot { now_ms: NOW + 10 * OUTCOME_WAIT_MS, ..idle() });
    assert_eq!(next.repos["spira"].waiting, None);
}

#[test_id(tqkyohr)]
#[test]
fn sp_2gcs4h_the_outcome_wait_outlasts_every_effect_a_waiting_state_issues() {
    let longest = ACTS.iter().filter(|(e, _)| [ASSEMBLE, ROUND_OPEN, LAND, LC_CERTIFY, PROMOTE, FETCH].contains(e)).map(|(_, d)| *d).max().unwrap();
    assert!(OUTCOME_WAIT_MS > longest, "{OUTCOME_WAIT_MS} vs {longest}");
}

#[test_id(tz1pcj7)]
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

#[test_id(tnb7zcz)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    kit::assert_kept_as_rows(&at_work(), NAME);
}

#[test_id(t7z40ls)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}

/// The round key lists every member as `id:tip`, so it outgrows the store's 255-char key column
/// with the round's size; a green certify of such a round must still reach `lc_certify`.
#[test_id(tooq6r6)]
#[test]
fn sp_lkm2fv_a_green_certify_of_a_wide_round_is_requested_not_refused_by_the_store() {
    use runtime::units::{Exit, UnitHandle, UnitRan};
    let members: Vec<Member> = (0..9).map(|i| Member { state: "CERTIFIED".into(), ..member(&format!("sp-wide{i:02}"), &format!("{:040x}", i + 1)) }).collect();
    let world = Snapshot { repos: [("spira".to_string(), repo(members.clone()))].into(), now_ms: NOW, ..Default::default() };
    let snaps = std::sync::Mutex::new(vec![world]);
    let assembled = Assembled { merged: members.iter().map(|m| (m.id.clone(), m.tip.clone())).collect(), wt: "w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
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
    let ran = |leg: &Leg| UnitRan { handle: UnitHandle::of(NAME, &leg.unit).unwrap(), exit: Exit::Code(0), duration_ms: 1_000 };
    let certify = leg_of(&r, CERTIFY);
    r.tick(STREAM, ran(&certify).event_kind()).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    assert!(r.requested().iter().any(|q| q.name == LC_CERTIFY), "the green certify reaches lc_certify");
}

// --- sp-pi2ojm: an opened round is driven on the next tick, not after its outcome's 20-minute wait ---

fn opened_unadvanced() -> Carry {
    let rc = RepoCarry { state: State::Opened, round_key: Some("k".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "w".into(), base: "b1".into(), waiting: Some(Waiting { state: State::Opened, since_ms: NOW - 40_000 }), ..RepoCarry::default() };
    Carry { repos: [("spira".to_string(), rc)].into() }
}

fn queue_holds(id: &str, members: &[&str]) -> Snapshot {
    let round = Round { id: id.into(), members: members.iter().map(|m| m.to_string()).collect(), head: "h1".into(), worktree: "w".into(), ..Round::default() };
    Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round), ..repo(vec![]) })].into(), now_ms: NOW, ..Default::default() }
}

#[test_id(t253iek)]
#[test]
fn sp_pi2ojm_the_tick_after_round_open_launches_the_certify_leg_when_the_outcome_never_arrived() {
    let (carry, acts) = decide(&opened_unadvanced(), &queue_holds("r9", &["sp-a", "sp-b"]));
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [CERTIFY], "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref()), (State::Certifying, Some("r9")));
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!(leg.batch, "r9");
}

#[test_id(t0369a4)]
#[test]
fn sp_pi2ojm_an_opened_round_the_queue_does_not_hold_yet_is_left_waiting() {
    let (carry, acts) = decide(&opened_unadvanced(), &queue_holds("other", &["sp-x"]));
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opened);
}

#[test_id(tstdd1e)]
#[test]
fn sp_64x7v2_adopting_a_late_opened_round_resets_the_failed_open_count() {
    let mut carry = opened_unadvanced();
    carry.repos.get_mut("spira").unwrap().pace.open_failed = 2;
    let (carry, _) = decide(&carry, &queue_holds("r9", &["sp-a", "sp-b"]));
    assert_eq!(carry.repos["spira"].pace.open_failed, 0);
}

#[test_id(tmkg1ez)]
#[test]
fn sp_pi2ojm_the_late_round_open_outcome_after_the_adoption_launches_no_second_leg() {
    let (carry, _) = decide(&opened_unadvanced(), &queue_holds("r9", &["sp-a", "sp-b"]));
    let (_, acts) = on_outcome(&carry, ROUND_OPEN, true, &serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap(), &Leg { why: "suites".into(), ..Leg::on("spira") }, NOW);
    assert!(acts.is_empty(), "{acts:?}");
}

#[test_id(tsj9742)]
#[test]
fn sp_pi2ojm_an_opened_round_in_another_tree_is_not_ours_to_adopt() {
    let mut snap = queue_holds("r9", &["sp-a", "sp-b"]);
    snap.repos.get_mut("spira").unwrap().open_round.as_mut().unwrap().worktree = "elsewhere".into();
    let (carry, acts) = decide(&opened_unadvanced(), &snap);
    assert!(acts.is_empty(), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opened);
}

#[test_id(tmll06v)]
#[test]
fn sp_pi2ojm_an_adopted_round_without_suites_certifies_with_or_without_its_own_testenv() {
    let mut own = opened_unadvanced();
    let rc = own.repos.get_mut("spira").unwrap();
    (rc.suites, rc.own_testenv) = (Some(false), true);
    let (carry, acts) = decide(&own, &queue_holds("r9", &["sp-a", "sp-b"]));
    assert_eq!((carry.repos["spira"].state, acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>()), (State::Certifying, vec![CERTIFY]));
    let mut bare = opened_unadvanced();
    bare.repos.get_mut("spira").unwrap().suites = Some(false);
    let (carry, acts) = decide(&bare, &queue_holds("r9", &["sp-a", "sp-b"]));
    assert_eq!((carry.repos["spira"].state, acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>()), (State::Certifying, vec![CERTIFY]));
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

#[test_id(tr04h4c)]
#[test]
fn a_failed_assemble_names_its_error_in_skip_why_from_the_first_tick() {
    let (carry, acts) = failed_assemble(&Carry::default(), LEFTOVER);
    let why = carry.repos["spira"].skip_why.as_deref().expect("the failure is carried");
    assert!(why.contains(LEFTOVER), "{why}");
    assert!(pages(&acts).is_empty(), "one failure is not yet a page");
    let (carry, _) = decide(&carry, &busy());
    assert!(carry.repos["spira"].skip_why.as_deref().unwrap_or("").contains(LEFTOVER), "the next cut does not wipe it before it runs");
}

#[test_id(tsdmrs9)]
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

#[test_id(tlyp33r)]
#[test]
fn a_different_assemble_failure_restarts_the_count() {
    let (c1, _) = failed_assemble(&Carry::default(), LEFTOVER);
    let (c2, _) = failed_assemble(&c1, LEFTOVER);
    let (c3, a3) = failed_assemble(&c2, "git worktree add exited 128: something else");
    assert!(pages(&a3).is_empty(), "{a3:?}");
    assert!(c3.repos["spira"].skip_why.as_deref().unwrap().contains("something else"));
}

#[test_id(te34ers)]
#[test]
fn an_assemble_that_succeeds_after_failures_clears_the_failure_and_its_skip_why() {
    let (failed, _) = failed_assemble(&Carry::default(), LEFTOVER);
    let (carry, acts) = decide(&failed, &busy());
    let assemble = acts.iter().find(|a| a.effect == ASSEMBLE).unwrap();
    let leg: Leg = serde_json::from_str(&assemble.input).unwrap();
    let ok = Assembled { merged: vec![("sp-a".into(), "t1".into())], wt: "w".into(), base: "b1".into(), ..Assembled::default() };
    let (carry, _) = on_outcome(&carry, ASSEMBLE, true, &serde_json::to_string(&ok).unwrap(), &leg, NOW);
    assert_eq!(carry.repos["spira"].assemble_failed, None);
    assert_eq!(carry.repos["spira"].skip_why, None);
}

#[test_id(t7fpelf)]
#[test]
fn an_assemble_lost_at_restart_is_requested_again_on_the_next_tick() {
    let (carry, _) = failed_assemble(&Carry::default(), "lost at restart");
    let (carry, acts) = decide(&carry, &busy());
    assert_eq!(carry.repos["spira"].skip_why, None, "a lost assemble is not a failure to report");
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [FETCH, ASSEMBLE], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Assembling);
}

#[test_id(ta89pw0)]
#[test]
fn a_lost_assemble_is_not_counted_as_an_assemble_failure() {
    let (carry, acts) = failed_assemble(&Carry::default(), "lost at restart");
    assert!(carry.repos.get("spira").is_none_or(|rc| rc.skip_why.is_none() && rc.assemble_failed.is_none() && rc.state == State::Idle), "{carry:?}");
    assert!(acts.is_empty(), "{acts:?}");
}

#[test_id(tg3szgl)]
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

#[test_id(tfcylgy)]
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
#[test_id(tifz9it)]
#[test]
fn a_round_is_cut_within_a_minute_of_a_restart_that_interrupted_the_assemble() {
    let assembled = Assembled { merged: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
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

/// A rig halted before its first tick, left to observe a cuttable pool for three cadences.
fn halted_cut() -> TaskRig {
    let assembled = Assembled { merged: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
    let mut r = TaskRig::new(1, registration())
        .unwrap()
        .double(OBSERVE, |_| Ok(serde_json::to_string(&busy()).unwrap()))
        .double(ASSEMBLE, move |_| Ok(serde_json::to_string(&assembled).unwrap()))
        .double(ROUND_OPEN, |_| Ok(serde_json::to_string(&Opened { batch: "r9".into(), ..Opened::default() }).unwrap()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE, ROUND_OPEN].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.deliver(runtime::event::HALT_STREAM, EventKind::new(runtime::event::SCHEDULER_HALTED, "test")).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS * 3).unwrap();
    r
}

fn held_assembles(r: &TaskRig) -> usize {
    r.journaled().iter().filter(|e| e.kind.tag == runtime::event::EFFECT_HELD).filter(|e| serde_json::from_str::<serde_json::Value>(&e.kind.body).unwrap()["spec"]["name"] == ASSEMBLE).count()
}

fn last_state(r: &TaskRig) -> State {
    let carry: Carry = r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).expect("a recorded carry");
    carry.repos["spira"].state
}

/// Resumes, then requires the one released ASSEMBLE and the round opened from its outcome.
fn resume_and_follow_the_assemble(r: &mut TaskRig) {
    r.deliver(runtime::event::HALT_STREAM, EventKind::new(runtime::event::SCHEDULER_RESUMED, "")).unwrap();
    let asked = |name: &str| r.requested().iter().filter(|q| q.name == name).count();
    assert_eq!((asked(ASSEMBLE), asked(ROUND_OPEN)), (1, 1));
    assert_eq!(last_state(r), State::Certifying, "the round opened from the released assemble moves on to its suites");
}

#[test_id(tyk4jmb)]
#[test]
fn an_assemble_held_by_a_halt_is_waited_for_and_released_once() {
    let mut r = halted_cut();
    kit::assert_unpoisoned(&r, STREAM);
    assert_eq!((held_assembles(&r), last_state(&r)), (1, State::Assembling));
    resume_and_follow_the_assemble(&mut r);
}

#[test_id(tcyez23)]
#[test]
fn an_assemble_held_by_a_halt_is_still_waited_for_after_a_restart() {
    let mut r = halted_cut();
    r.reopen(registration()).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS * 2).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    assert_eq!((held_assembles(&r), last_state(&r)), (1, State::Assembling));
    resume_and_follow_the_assemble(&mut r);
}

/// The host accepts a resume before it completes an outcome, so an observe answer can be queued
/// behind the resume and ahead of what the resume releases. The pool grows so a re-cut has a new
/// key, which the engine does not refuse.
#[test_id(t6lo0g1)]
#[test]
fn an_observe_answered_just_behind_the_resume_still_sees_the_released_assemble() {
    use runtime::engine::{Engine, Executor, Outcome};
    let (journal, clock) = (runtime::journal::MemJournal::new(), runtime::ports::VirtualClock::new(0));
    let mut engine = Engine::open(journal.clone(), std::sync::Arc::new(clock), vec![registration()], 1).unwrap();
    engine.submit("halt", runtime::event::HALT_STREAM, EventKind::new(runtime::event::SCHEDULER_HALTED, "test")).unwrap();
    let probe = |engine: &mut Engine| {
        engine.start(STREAM, EventKind::new(TICK, ""));
        engine.run_until_idle().unwrap();
        engine.pending_effects().into_iter().find(|p| p.spec.name == OBSERVE).expect("an observe probe").key
    };
    let world = serde_json::to_string(&busy()).unwrap();
    let first = probe(&mut engine);
    engine.complete(&first, Outcome::Succeeded(world.clone())).unwrap();
    engine.run_until_idle().unwrap();
    let second = probe(&mut engine);
    engine.submit("resume", runtime::event::HALT_STREAM, EventKind::new(runtime::event::SCHEDULER_RESUMED, "")).unwrap();
    let arrived = Snapshot { repos: [("spira".to_string(), repo(vec![member("sp-a", "t1"), member("sp-b", "t2"), member("sp-c", "t3")]))].into(), ..busy() };
    engine.complete(&second, Outcome::Succeeded(serde_json::to_string(&arrived).unwrap())).unwrap();
    engine.run_until_idle().unwrap();
    let assembles = journal.log().iter().filter(|e| e.kind.tag == runtime::event::EFFECT_REQUESTED && e.kind.body.contains(&format!("\"name\":\"{ASSEMBLE}\""))).count();
    assert_eq!(assembles, 1);
}

/// The engine's own lost-at-restart outcome for an open round open is asked for again.
#[test_id(tb8x00y)]
#[test]
fn a_round_open_interrupted_by_a_restart_is_asked_again() {
    let assembled = Assembled { merged: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], wt: "w".into(), base: "b1".into(), suites: true, ..Assembled::default() };
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
#[test_id(tee4jb1)]
#[test]
fn a_round_open_lost_at_restart_is_asked_again() {
    let mut carry = Carry::default();
    carry.repos.insert("spira".into(), RepoCarry { state: State::Opened, round_key: Some("k".into()), members: vec![("sp-a".into(), "t1".into())], wt: "w".into(), base: "b1".into(), suites: Some(true), ..RepoCarry::default() });
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
#[test_id(t6y14w0)]
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

// --- sp-5u1oru: a round the batcher opened is always driven ---

fn open_with(phase: &str, members: &[&str], worktree: &str) -> Snapshot {
    let round = Round { id: "r9".into(), members: members.iter().map(|m| m.to_string()).collect(), head: "h".into(), worktree: worktree.into(), phase: phase.into(), ..Round::default() };
    Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round), ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() }
}

fn opened_unanswered() -> Carry {
    let mut c = driving(State::Opened, "");
    let rc = c.repos.get_mut("spira").unwrap();
    rc.batch = None;
    rc.leg = None;
    c
}

fn effects(acts: &[Act]) -> Vec<&str> {
    acts.iter().map(|a| a.effect.as_str()).collect()
}

#[test_id(tmb90f0)]
#[test]
fn sp_5u1oru_an_open_round_whose_open_answer_is_lost_is_adopted_at_the_next_tick() {
    let (carry, acts) = decide(&opened_unanswered(), &open_with("opened", &["sp-a", "sp-b"], "w"));
    assert!(effects(&acts).contains(&CERTIFY), "the first pass starts on this tick: {acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref()), (State::Certifying, Some("r9")));
}

#[test_id(ty7njr9)]
#[test]
fn sp_5u1oru_an_open_round_of_other_members_is_not_adopted() {
    let (carry, acts) = decide(&opened_unanswered(), &open_with("opened", &["sp-x"], "w"));
    assert!(!effects(&acts).contains(&CORPUS), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Opened);
}

#[test_id(tijr8os)]
#[test]
fn sp_5u1oru_a_round_opened_seconds_ago_is_left_to_its_opener() {
    let (_, acts) = decide(&Carry::default(), &open_with("opened", &["sp-a"], "w"));
    assert!(!effects(&acts).contains(&REDRIVE), "{acts:?}");
}

#[test_id(tmgvejt)]
#[test]
fn sp_5u1oru_an_open_round_with_no_pass_and_no_driver_is_redriven_at_once_and_only_once() {
    let mut snap = open_with("opened", &["sp-a"], "w");
    snap.repos.get_mut("spira").unwrap().open_round.as_mut().unwrap().age_secs = 60;
    let (carry, acts) = decide(&Carry::default(), &snap);
    assert!(effects(&acts).contains(&REDRIVE), "no orphan wait for a round that never began: {acts:?}");
    let (_, again) = decide(&carry, &snap);
    assert!(!effects(&again).contains(&REDRIVE) && !effects(&again).contains(&ABANDON), "the second tick waits: {again:?}");
}

// --- sp-4odl0h: an undriven round whose last pass ended red is attributed, not abandoned ---

fn red_orphan() -> (Carry, Snapshot) {
    let mut snap = open_with("attributing", &["sp-a", "sp-b"], "w");
    let round = snap.repos.get_mut("spira").unwrap().open_round.as_mut().unwrap();
    round.tips = vec!["t1".into(), "t2".into()];
    round.red = true;
    round.age_secs = 1_400;
    (Carry::default(), snap)
}

#[test_id(tvbx7t4)]
#[test]
fn sp_4odl0h_an_undriven_round_whose_last_pass_ended_red_is_attributed_not_redriven_or_abandoned() {
    let (carry, snap) = red_orphan();
    let (carry, acts) = decide(&carry, &snap);
    assert_eq!(effects(&acts), [CORPUS], "{acts:?}");
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    assert_eq!((leg.why.as_str(), leg.after_pass.as_str(), leg.batch.as_str()), (ATTRIBUTE, "run/rounds/r9.results", "r9"), "{leg:?}");
    assert_eq!(leg.members.len(), 2, "every member is attributed");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref()), (State::Attributing, Some("r9")));
    let (_, again) = decide(&carry, &snap);
    assert!(!effects(&again).contains(&REDRIVE) && !effects(&again).contains(&ABANDON), "{again:?}");
}

#[test_id(t4rmls8)]
#[test]
fn sp_4odl0h_an_adopted_red_rounds_attribution_naming_one_member_certifies_the_survivor() {
    let (carry, snap) = red_orphan();
    let (carry, acts) = decide(&carry, &snap);
    let leg: Leg = serde_json::from_str(&acts[0].input).unwrap();
    let survivor = batcher_cut::attribute::Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: None, mended: false, head: String::new() };
    let mut r = carried(&carry);
    let n = r.requested().len();
    r.tick(STREAM, ended(&leg.unit, runtime::units::Exit::Code(0), &format!("{}\n", survivor.line()))).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let after = asked_after(&r, n);
    assert_eq!(after.iter().map(|(e, _)| e.as_str()).collect::<Vec<_>>(), [CERTIFY], "{after:?}");
    assert_eq!(after[0].1.members, [("sp-b".to_string(), "t2".to_string())]);
}

#[test_id(tqve466)]
#[test]
fn sp_4odl0h_an_undriven_round_with_no_red_pass_is_still_redriven() {
    let (carry, mut snap) = red_orphan();
    let round = snap.repos.get_mut("spira").unwrap().open_round.as_mut().unwrap();
    round.red = false;
    round.phase = "opened".into();
    let (_, acts) = decide(&carry, &snap);
    assert!(effects(&acts).contains(&REDRIVE) && !effects(&acts).contains(&CORPUS), "{acts:?}");
}

// --- sp-k7ltss: a green round no pass drives is landed, not abandoned ---

fn green_orphan() -> (Carry, Snapshot) {
    let mut snap = open_with("green", &["sp-a", "sp-b"], "w");
    let round = snap.repos.get_mut("spira").unwrap().open_round.as_mut().unwrap();
    round.tips = vec!["t1".into(), "t2".into()];
    round.age_secs = 1_400;
    let pace = Pace { orphan: vec!["r9".into()], orphan_since_ms: NOW - 700_000, redriven: "r9".into(), ..Pace::default() };
    let carry = Carry { repos: [("spira".to_string(), RepoCarry { pace, ..RepoCarry::default() })].into() };
    (carry, snap)
}

#[test_id(t3ayw3i)]
#[test]
fn sp_k7ltss_a_green_round_with_no_driver_is_landed_not_abandoned() {
    let (carry, snap) = green_orphan();
    let (carry, acts) = decide(&carry, &snap);
    assert_eq!(effects(&acts), [LC_CERTIFY, LAND], "{acts:?}");
    let rc = &carry.repos["spira"];
    assert_eq!((rc.state, rc.batch.as_deref(), rc.members.clone()), (State::Landing, Some("r9"), vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())]));
    assert_eq!((rc.wt.as_str(), rc.head.as_str()), ("w", "h"));
    assert_eq!((rc.pace.orphan.len(), rc.pace.redriven.as_str()), (0, ""), "the orphan clock is cleared");
}

#[test_id(tkn030g)]
#[test]
fn sp_k7ltss_a_green_round_whose_tips_are_unknown_is_not_adopted() {
    let (carry, mut snap) = green_orphan();
    snap.repos.get_mut("spira").unwrap().open_round.as_mut().unwrap().tips.clear();
    let (carry, acts) = decide(&carry, &snap);
    assert!(!effects(&acts).contains(&LAND) && !effects(&acts).contains(&ABANDON), "{acts:?}");
    assert_ne!(carry.repos["spira"].state, State::Landing);
}

#[test_id(tvxr8fk)]
#[test]
fn sp_k7ltss_a_green_round_above_another_open_round_waits() {
    let (_, mut snap) = green_orphan();
    let repo = snap.repos.get_mut("spira").unwrap();
    let green = Round { id: "r10".into(), ..repo.open_round.take().unwrap() };
    repo.open_round = Some(Round { id: "r9".into(), members: vec!["sp-a".into(), "sp-b".into()], head: "h0".into(), worktree: "w".into(), driven: true, phase: "certifying".into(), ..Round::default() });
    repo.open_above = vec![green];
    let mut carry = driving(State::Certifying, "spira.c3");
    carry.repos.get_mut("spira").unwrap().pace = Pace { orphan: vec!["r10".into()], orphan_since_ms: NOW - 700_000, redriven: "r10".into(), ..Pace::default() };
    let max_two = Policy { max_open: 2, ..Policy::default() };
    let (carry, acts) = decide_with(&max_two, &carry, &snap);
    assert!(!effects(&acts).contains(&LAND) && !effects(&acts).contains(&ABANDON), "{acts:?}");
    assert_ne!(carry.repos["spira"].batch.as_deref(), Some("r10"), "the round above is not adopted");
}

#[test_id(t4l4rwi)]
#[test]
fn sp_3uxw41_a_member_blocked_on_an_unlanded_dep_records_nothing_admissible() {
    let blocked = Member { deps: vec!["sp-base".into()], ..member("sp-a", "t1") };
    let s = Snapshot { repos: [("spira".to_string(), Repo { blockers: vec!["sp-base".into()], ..repo(vec![blocked]) })].into(), now_ms: NOW, ..Default::default() };
    let (carry, acts) = decide(&Carry::default(), &s);
    assert!(!effects(&acts).contains(&ASSEMBLE), "{acts:?}");
    assert_eq!(carry.repos["spira"].admissible, Vec::<String>::new());
}

#[test_id(tih6lgk)]
#[test]
fn a_member_refused_for_an_unlanded_dep_is_carried_with_that_dep_and_one_whose_dep_is_not_a_blocker_is_not() {
    let blocked = Member { deps: vec!["sp-base".into()], ..member("sp-a", "t1") };
    let landed = Member { deps: vec!["sp-done".into()], ..member("sp-b", "t2") };
    let s = Snapshot { repos: [("spira".to_string(), Repo { blockers: vec!["sp-base".into()], ..repo(vec![blocked, landed]) })].into(), now_ms: NOW, ..Default::default() };
    let (carry, _) = decide(&Carry::default(), &s);
    let json = serde_json::to_value(&carry).unwrap();
    assert_eq!(json["repos"]["spira"]["blocked"]["sp-a"], "sp-base");
    assert!(json["repos"]["spira"]["blocked"].get("sp-b").is_none(), "{json}");
}

#[test_id(tu84j54)]
#[test]
fn a_pass_with_an_empty_pool_clears_the_carried_dependency_refusals() {
    let blocked = Member { deps: vec!["sp-base".into()], ..member("sp-a", "t1") };
    let s = Snapshot { repos: [("spira".to_string(), Repo { blockers: vec!["sp-base".into()], ..repo(vec![blocked]) })].into(), now_ms: NOW, ..Default::default() };
    let (carry, _) = decide(&Carry::default(), &s);
    assert!(!carry.repos["spira"].blocked.is_empty());
    let empty = Snapshot { repos: [("spira".to_string(), repo(vec![]))].into(), now_ms: NOW, ..Default::default() };
    let (carry, _) = decide(&carry, &empty);
    assert!(carry.repos.get("spira").is_none_or(|rc| rc.blocked.is_empty()), "{:?}", carry.repos);
}

#[test_id(txk164f)]
#[test]
fn sp_3uxw41_an_unblocked_sifted_member_with_no_round_open_is_admissible() {
    let s = Snapshot { repos: [("spira".to_string(), repo(vec![member("sp-a", "t1")]))].into(), now_ms: NOW, ..Default::default() };
    let (carry, _) = decide(&Carry::default(), &s);
    assert_eq!(carry.repos["spira"].admissible, ["sp-a"]);
}

#[test_id(t871cno)]
#[test]
fn sp_3uxw41_a_repo_with_its_rounds_at_max_open_records_nothing_admissible() {
    let mut s = Snapshot { repos: [("spira".to_string(), repo(vec![member("sp-c", "t3")]))].into(), now_ms: NOW, ..Default::default() };
    s.repos.get_mut("spira").unwrap().open_round = Some(Round { id: "r9".into(), members: vec!["sp-a".into()], head: "h0".into(), worktree: "w".into(), driven: true, phase: "certifying".into(), ..Round::default() });
    let mut before = driving(State::Certifying, "spira.c3");
    before.repos.get_mut("spira").unwrap().admissible = vec!["sp-c".into()];
    let (carry, _) = decide(&before, &s);
    assert_eq!(carry.repos["spira"].admissible, Vec::<String>::new(), "a stale answer is cleared");
}

// --- sp-29matj: a round whose pass ended with no verdict and no pass running is ended ---

fn driverless_carry() -> Carry {
    let mut carry = driving(State::Attributing, "");
    let rc = carry.repos.get_mut("spira").unwrap();
    rc.leg = None;
    rc.leg_head = "old".into();
    rc.head = "old".into();
    rc.leg_at_ms = NOW - LEG_START_GRACE_MS - 1;
    carry
}

fn driverless_snap(driven: bool) -> Snapshot {
    let round = Round { id: "r9".into(), members: vec!["sp-a".into(), "sp-b".into()], head: "new".into(), worktree: "w".into(), phase: "attributing".into(), driven, ..Round::default() };
    Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round), live_legs: Some(vec![]), ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() }
}

#[test_id(tny9wnn)]
#[test]
fn sp_29matj_a_driverless_round_with_no_verdict_is_abandoned_as_a_harness_fault() {
    let snap = driverless_snap(false);
    let (seen, acts) = decide(&driverless_carry(), &snap);
    assert!(!effects(&acts).contains(&ABANDON), "the pass the queue has just started is given the grace: {acts:?}");
    let later = Snapshot { now_ms: NOW + LEG_START_GRACE_MS + 1, ..snap };
    let (next, acts) = decide(&seen, &later);
    let abandon: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ABANDON).unwrap_or_else(|| panic!("no abandon: {acts:?}")).input).unwrap();
    assert!(abandon.why.contains("new") && abandon.why.contains("no pass is running"), "{}", abandon.why);
    assert!(abandon.reds.is_empty());
    assert_eq!(abandon.bead, NO_DRIVER, "a pass with no verdict is a harness fault, filed as no-driver and not as a red abandon");
    let rc = next.repos.get("spira").cloned().unwrap_or_default();
    assert_eq!((rc.state, rc.batch.as_deref(), rc.pace.infra), (State::Idle, None, 1), "no member is charged and the pace counts one infra");
}

fn red_snap(id: &str, members: &[&str], suites: &[&str], now_ms: u64) -> Snapshot {
    let round = Round { id: id.into(), members: members.iter().map(|m| m.to_string()).collect(), head: "new".into(), worktree: "w".into(), phase: "attributing".into(), red: true, red_suites: suites.iter().map(|s| s.to_string()).collect(), ..Round::default() };
    Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round), live_legs: Some(vec![]), ..busy().repos["spira"].clone() })].into(), now_ms, ..Default::default() }
}

/// Plays a leg-less red round to the point where it used to be abandoned.
fn play_red_round(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let (seen, _) = decide(carry, snap);
    decide(&seen, &Snapshot { now_ms: snap.now_ms + LEG_START_GRACE_MS + 1, ..snap.clone() })
}

fn assert_attributes_kept_results(acts: &[Act], round: &str) {
    assert!(!acts.iter().any(|a| a.effect == ABANDON), "a recorded red is a verdict, not a missing one: {acts:?}");
    let leg: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == CORPUS).unwrap_or_else(|| panic!("no attribution: {acts:?}")).input).unwrap();
    assert_eq!((leg.why.as_str(), leg.after_pass.as_str()), (ATTRIBUTE, kept_results_file(round).as_str()));
}

#[test_id(t3nlx6c)]
#[test]
fn sp_we0r0p_a_recorded_red_on_a_multi_member_round_is_attributed_not_abandoned() {
    let snap = red_snap("r9", &["sp-2ykfhe", "sp-4wxapo"], &["test-repo-check-killed.sh"], NOW);
    let (next, acts) = play_red_round(&driverless_carry(), &snap);
    assert_attributes_kept_results(&acts, "r9");
    let rc = &next.repos["spira"];
    assert_eq!((rc.state, rc.pace.infra), (State::Attributing, 0));
    let leg: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == CORPUS).unwrap().input).unwrap();
    assert_eq!(leg.reds, ["test-repo-check-killed.sh"]);
}

#[test_id(twvav1e)]
#[test]
fn sp_we0r0p_a_recorded_red_on_a_one_member_round_is_attributed_not_abandoned() {
    let snap = red_snap("r9", &["sp-yrxbnt"], &["unit-tests"], NOW);
    let (next, acts) = play_red_round(&driverless_carry(), &snap);
    assert_attributes_kept_results(&acts, "r9");
    let rc = &next.repos["spira"];
    assert_eq!((rc.state, rc.pace.infra), (State::Attributing, 0));
}

#[test_id(t2g43ym)]
#[test]
fn sp_we0r0p_two_recorded_red_rounds_in_a_row_do_not_stop_the_lane_as_having_no_verdicts() {
    let mut carry = driverless_carry();
    for id in ["r8", "r9"] {
        let snap = red_snap(id, &["sp-2ykfhe", "sp-4wxapo"], &["test-repo-check-killed.sh"], NOW);
        let (next, acts) = play_red_round(&carry, &snap);
        assert!(!acts.iter().any(|a| a.effect == INBOX_APPEND && a.input.contains("ROUNDS STOPPED")), "{id}: {acts:?}");
        assert!(!acts.iter().any(|a| a.effect == PAUSE && serde_json::from_str::<Leg>(&a.input).is_ok_and(|l| l.bead == "stop")), "{id}: {acts:?}");
        carry = driverless_carry();
        carry.repos.get_mut("spira").unwrap().pace = next.repos["spira"].pace.clone();
    }
    assert_eq!(carry.repos["spira"].pace.infra, 0);
}

#[test_id(th4k1fc)]
#[test]
fn sp_we0r0p_a_head_that_is_red_and_legless_again_after_its_attribution_ends_the_round_red() {
    let snap = red_snap("r9", &["sp-2ykfhe", "sp-4wxapo"], &["test-repo-check-killed.sh"], NOW);
    let (attributing, _) = play_red_round(&driverless_carry(), &snap);
    let mut again = attributing.clone();
    let rc = again.repos.get_mut("spira").unwrap();
    rc.leg = None;
    rc.leg_at_ms = NOW - LEG_START_GRACE_MS - 1;
    let (next, acts) = decide(&again, &snap);
    let abandon: Leg = serde_json::from_str(&acts.iter().find(|a| a.effect == ABANDON).unwrap_or_else(|| panic!("no abandon: {acts:?}")).input).unwrap();
    assert!(abandon.why.contains("test-repo-check-killed.sh"), "{}", abandon.why);
    assert!(acts.iter().any(|a| a.effect == PAUSE), "{acts:?}");
    let page = acts.iter().find(|a| a.effect == INBOX_APPEND).unwrap_or_else(|| panic!("no page: {acts:?}"));
    assert!(page.input.contains("ROUND RED") && page.input.contains("test-repo-check-killed.sh"), "{}", page.input);
    assert_eq!(next.repos["spira"].pace.infra, 0);
}

#[test_id(tfgc2mg)]
#[test]
fn sp_29matj_a_head_move_is_discarded_once_not_every_tick() {
    let mut carry = driverless_carry();
    carry.repos.get_mut("spira").unwrap().leg_at_ms = NOW;
    let snap = driverless_snap(false);
    let (once, _) = decide(&carry, &snap);
    assert_eq!(once.repos["spira"].discarded_head, "new", "the new head is the one handled");
    let (twice, acts) = decide(&once, &snap);
    assert_eq!(twice, once);
    assert!(acts.is_empty(), "{acts:?}");
}

#[test_id(tfwlbr0)]
#[test]
fn sp_29matj_a_live_pass_or_a_pass_just_started_is_left_alone() {
    let mut seen = driverless_carry();
    seen.repos.get_mut("spira").unwrap().discarded_head = "new".into();
    let (_, acts) = decide(&seen, &driverless_snap(true));
    assert!(!effects(&acts).contains(&ABANDON), "driven: {acts:?}");
    let (_, acts) = decide(&seen, &driverless_snap(false));
    assert!(effects(&acts).contains(&ABANDON), "the same setup undriven is abandoned: {acts:?}");
    let mut fresh = driverless_carry();
    fresh.repos.get_mut("spira").unwrap().leg_at_ms = NOW - 1_000;
    let (_, acts) = decide(&fresh, &driverless_snap(false));
    assert!(!effects(&acts).contains(&ABANDON), "inside the grace: {acts:?}");
}

#[test_id(to4z6jw)]
#[test]
fn a_green_foreign_round_whose_bare_round_recert_unit_is_active_is_not_landed_by_the_tick() {
    use runtime::units::{ScriptedUnits, UnitFacts, UnitState};
    let units = ScriptedUnits::new(std::sync::Arc::new(runtime::ports::VirtualClock::new(0)));
    units.install("round-recert-b1.service", UnitFacts { state: UnitState::Active, main_pid: Some(7), exit: None, since_ms: Some(1) });
    let driven = edge::recert_driven(Some(&units), "b1");
    let open = Round { id: "b1".into(), members: vec!["m1".into()], tips: vec!["t1".into()], head: "h1".into(), phase: "green".into(), driven, ..Round::default() };
    let repo = Repo { open_round: Some(open), base_sha: "base".into(), ..Repo::default() };
    let snap = Snapshot { repos: [("spira".to_string(), repo)].into(), now_ms: NOW, ..Default::default() };
    assert!(!decide(&Carry::default(), &snap).1.iter().any(|a| a.effect == LAND));
}

#[test_id(tl1drgz)]
#[test]
fn an_attribution_whose_record_lost_a_carried_member_is_stopped_and_certifies_the_survivors_once() {
    let mut carry = driving(State::Attributing, "spira.a2");
    let rc = carry.repos.get_mut("spira").unwrap();
    rc.leg_head = "h0".into();
    rc.head = "h0".into();
    rc.leg_at_ms = NOW;
    let round = Round { id: "r9".into(), members: vec!["sp-a".into()], tips: vec!["t1".into()], head: "h1".into(), worktree: "w".into(), phase: "attributing".into(), driven: true, ..Round::default() };
    let snap = Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round), live_legs: Some(vec!["spira.a2".into()]), ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() };
    let (next, acts) = decide(&carry, &snap);
    let stops = |acts: &[Act]| acts.iter().filter(|a| a.effect == runtime::units::UNIT_STOP).count();
    assert_eq!(stops(&acts), 1, "{acts:?}");
    let rc = &next.repos["spira"];
    assert_eq!((rc.state, rc.head.as_str()), (State::Certifying, "h1"));
    assert_eq!(rc.members, [("sp-a".to_string(), "t1".to_string())]);
    let (_, again) = decide(&next, &snap);
    assert_eq!(stops(&again), 0, "one stop per eject: {again:?}");
}

/// The abandon of a round stops its leg with SIGTERM: whichever leg it was, the round is not
/// judged — no attribution, no abandon, no pause — and the repo cuts again on the next tick.
#[test_id(t6sbdmr)]
#[test]
fn a_leg_stopped_by_its_rounds_abandon_ends_the_round_unjudged_and_the_repo_cuts_again() {
    for (state, leg) in [(State::Certifying, "spira.c3"), (State::Corpus, "spira.c1")] {
        let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, leg).unwrap(), exit: runtime::units::Exit::Signal(15), duration_ms: 1 };
        let (carry, acts) = on_ran_with(&driving(state, leg), &ran, &runtime::units::RanOutput::default(), NOW).expect("its own leg");
        let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
        for no in [CORPUS, ABANDON, PAUSE, EJECT] {
            assert!(!names.contains(&no), "{state:?}: {no} asked: {names:?}");
        }
        let (_, acts) = decide(&carry, &busy());
        assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [FETCH, ASSEMBLE], "{state:?}: cut again on the next tick");
    }
}

/// An attribution that saw the abandon marker says so; that is no red.
#[test_id(tpiivts)]
#[test]
fn an_attribution_line_saying_abandoned_ends_the_round_unjudged_and_the_repo_cuts_again() {
    use batcher_cut::attribute::Attribution;
    let ran = runtime::units::UnitRan { handle: runtime::units::UnitHandle::of(NAME, "spira.a2").unwrap(), exit: runtime::units::Exit::Code(0), duration_ms: 1 };
    let out = runtime::units::RanOutput { stdout_tail: Some(Attribution::Abandoned.line()), ..Default::default() };
    let (carry, acts) = on_ran_with(&driving(State::Attributing, "spira.a2"), &ran, &out, NOW).expect("its own leg");
    let names: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    for no in [CORPUS, ABANDON, PAUSE, EJECT] {
        assert!(!names.contains(&no), "{no} asked: {names:?}");
    }
    let (_, acts) = decide(&carry, &busy());
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [FETCH, ASSEMBLE]);
}

#[test_id(tqpr2ti)]
#[test]
fn an_attribution_whose_member_was_ejected_by_the_concierge_is_stopped_and_certifies_the_survivors_once() {
    let mut carry = driving(State::Attributing, "spira.a2");
    let rc = carry.repos.get_mut("spira").unwrap();
    rc.leg_head = "h0".into();
    rc.head = "h0".into();
    rc.leg_at_ms = NOW;
    let round = Round { id: "r9".into(), members: vec!["sp-a".into()], tips: vec!["t1".into()], head: "h1".into(), worktree: "w".into(), phase: "attributing".into(), driven: true, ejected: vec!["sp-b".into()], ejected_by: vec!["concierge".into()], ..Round::default() };
    let snap = Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round), live_legs: Some(vec!["spira.a2".into()]), ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() };
    let (next, acts) = decide(&carry, &snap);
    let stops = |acts: &[Act]| acts.iter().filter(|a| a.effect == runtime::units::UNIT_STOP).count();
    assert_eq!(stops(&acts), 1, "{acts:?}");
    assert_eq!((next.repos["spira"].state, next.repos["spira"].head.as_str()), (State::Certifying, "h1"));
    let (_, again) = decide(&next, &snap);
    assert_eq!(stops(&again), 0, "one stop per eject: {again:?}");
}

#[test_id(tigznqq)]
#[test]
fn an_attribution_whose_member_the_batcher_itself_ejected_runs_on_and_certifies_its_rebuilt_head() {
    use batcher_cut::attribute::Attribution;
    let mut carry = driving(State::Attributing, "spira.a2");
    let rc = carry.repos.get_mut("spira").unwrap();
    rc.leg_head = "h0".into();
    rc.head = "h0".into();
    rc.leg_at_ms = NOW;
    let round = Round { id: "r9".into(), members: vec!["sp-a".into()], tips: vec!["t1".into()], head: "h0".into(), worktree: "w".into(), phase: "attributing".into(), driven: true, ejected: vec!["sp-b".into()], ejected_by: vec!["batcher".into()], ..Round::default() };
    let snap = Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round), live_legs: Some(vec!["spira.a2".into()]), ..busy().repos["spira"].clone() })].into(), now_ms: NOW, ..Default::default() };
    let (next, acts) = decide(&carry, &snap);
    assert!(!acts.iter().any(|a| a.effect == runtime::units::UNIT_STOP), "{acts:?}");
    assert_eq!(next.repos["spira"].state, State::Attributing);
    assert_eq!(next.repos["spira"].members.len(), 2, "both members stay carried");

    let mut r = carried(&next);
    let n = r.requested().len();
    let land = Attribution::Land { members: vec![("sp-a".into(), "t1".into())], attribution_secs: None, mended: false, head: "h2".into() }.line();
    r.tick(STREAM, ended("spira.a2", runtime::units::Exit::Code(0), &format!("{land}\n"))).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let after = asked_after(&r, n);
    let certify = after.iter().find(|(e, _)| e == CERTIFY).unwrap_or_else(|| panic!("no certify: {after:?}"));
    assert_eq!(certify.1.head, "h2");
    let argv = certify_argv("q", &certify.1);
    assert!(argv.windows(2).any(|w| w == ["--rehead", "h2"]), "{argv:?}");
}

#[test_id(t55v5k5)]
#[test]
fn an_attribution_that_ejects_one_of_three_members_keeps_its_leg_and_certifies_the_two_left() {
    use batcher_cut::attribute::Attribution;
    let mut carry = driving(State::Attributing, "spira.a2");
    let rc = carry.repos.get_mut("spira").unwrap();
    rc.members = vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into()), ("sp-c".into(), "t3".into())];
    rc.leg_head = "h0".into();
    rc.head = "h0".into();
    rc.leg_at_ms = NOW;
    let round = Round { id: "r9".into(), members: vec!["sp-a".into(), "sp-c".into()], tips: vec!["t1".into(), "t3".into()], head: "h1".into(), worktree: "w".into(), phase: "attributing".into(), driven: false, ejected: vec!["sp-b".into()], ejected_by: vec!["batcher".into()], ..Round::default() };
    let at = |now_ms| Snapshot { repos: [("spira".to_string(), Repo { open_round: Some(round.clone()), live_legs: Some(vec!["spira.a2".into()]), ..busy().repos["spira"].clone() })].into(), now_ms, ..Default::default() };
    let (next, acts) = decide(&carry, &at(NOW));
    let (later, more) = decide(&next, &at(NOW + LEG_START_GRACE_MS + 15_000));
    for a in acts.iter().chain(more.iter()) {
        assert!(a.effect != ABANDON && a.effect != runtime::units::UNIT_STOP, "{a:?}");
    }
    let rc = &later.repos["spira"];
    assert_eq!((rc.state, rc.leg.as_deref()), (State::Attributing, Some("spira.a2")));

    let mut r = carried(&later);
    let n = r.requested().len();
    let land = Attribution::Land { members: vec![("sp-a".into(), "t1".into()), ("sp-c".into(), "t3".into())], attribution_secs: None, mended: false, head: "h1".into() }.line();
    r.tick(STREAM, ended("spira.a2", runtime::units::Exit::Code(0), &format!("{land}\n"))).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let after = asked_after(&r, n);
    let certify = after.iter().find(|(e, _)| e == CERTIFY).unwrap_or_else(|| panic!("no certify: {after:?}"));
    assert_eq!(certify.1.head, "h1");
}

fn wakes(acts: &[Act]) -> Vec<&Act> {
    acts.iter().filter(|a| a.effect == spira_runtime::tasks::a::queue_step::PUBLISH_WANTED).collect()
}

#[test_id(t7ftcp7)]
#[test]
fn a_landed_round_wakes_publish_and_a_base_moved_or_refused_land_does_not() {
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let (old, acts) = on_outcome(&landing(), LAND, true, &serde_json::to_string(&Landed::default()).unwrap(), &land, NOW);
    assert_eq!(wakes(&acts).len(), 1, "{acts:?}");
    let input: serde_json::Value = serde_json::from_str(&wakes(&acts)[0].input).unwrap();
    assert_eq!(input, serde_json::json!({"repo": "spira", "mode": "queue.local"}));
    let cmds = kit::commands(&SHAPE, ACTS, EMITS, &old, &old, acts);
    let to: Vec<String> = cmds.iter().filter_map(|c| match c { runtime::event::Command::EmitTo { stream, kind } if kind.tag == spira_runtime::tasks::a::queue_step::PUBLISH_WANTED => Some(stream.0.clone()), _ => None }).collect();
    assert_eq!(to, [spira_runtime::tasks::a::publish::STREAM]);
    let moved = serde_json::to_string(&Landed { base_moved: Some("b2".into()), ..Landed::default() }).unwrap();
    assert!(wakes(&on_outcome(&landing(), LAND, true, &moved, &land, NOW).1).is_empty());
    assert!(wakes(&on_outcome(&landing(), LAND, false, "refused", &land, NOW).1).is_empty());
}

#[test_id(tlk77d5)]
#[test]
fn a_refused_landing_page_lands_again_before_a_conditional_eject() {
    let refusal = "queue.sh land-local: release c1ad5ddc failed to build or verify\nrelease: pre-activate: FAIL deps: bd: probe error: timed out after 10 seconds; refused";
    let land = Leg { batch: "r9".into(), ..Leg::on("spira") };
    let (_, acts) = on_outcome(&landing(), LAND, false, refusal, &land, NOW);
    let page = &acts.iter().find(|a| a.effect == INBOX_APPEND).unwrap_or_else(|| panic!("no page: {acts:?}")).input;
    let at = |s: &str| page.find(s).unwrap_or_else(|| panic!("{s:?} missing: {page}"));
    assert!(at("queue round land r9") < at("only if the refusing line names a member's change"), "{page}");
    assert!(at("only if the refusing line names a member's change") < at("queue round eject r9 <member>"), "{page}");
}

#[test_id(t70zf3w)]
#[test]
fn sp_4pne6q_a_moved_base_neither_forgets_a_failed_tag_nor_builds_it_again() {
    let (carry, unit) = building();
    let (failed, _) = image_ran(&carry, &unit, 1, "");
    let mut moved = observing(&[]);
    moved.repos.get_mut("spira").unwrap().base_sha = "b2".into();
    let (next, acts) = decide(&carry_idle(&failed), &moved);
    assert!(next.repos["spira"].refused.is_empty() && assembled_ids(&acts).contains(&"sp-b".to_string()), "a moved base tries the member again");
    assert_eq!(image_wants(&next)["tagz"]["failed"], true);
    let mut reopened = next.clone();
    let rc = reopened.repos.get_mut("spira").unwrap();
    (rc.state, rc.members) = (State::Opened, vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())]);
    let (_, acts) = refusing_image(&reopened, &[("sp-b", "tagz")]);
    assert!(builds(&acts).is_empty(), "{acts:?}");
}

#[test_id(t6pz8nw)]
#[test]
fn sp_4pne6q_a_build_whose_result_was_never_read_still_loses_its_checkout_on_readmit() {
    let (carry, _) = building();
    let (_, acts) = decide(&carry_idle(&carry), &observing(&["tagz"]));
    assert!(acts.iter().any(|a| a.effect == IMAGE_CLEAN), "{acts:?}");
}

#[test_id(ttpsql1)]
#[test]
fn sp_4pne6q_a_tick_reads_a_building_image_through_unit_facts() {
    use runtime::units::{UnitFacts, UnitState, UNIT_FACTS};
    let facts = serde_json::to_string(&UnitFacts { state: UnitState::Failed, main_pid: None, exit: Some(runtime::units::Exit::Code(2)), since_ms: Some(1) }).unwrap();
    let (carry, _) = building();
    let mut r = TaskRig::new(1, registration()).unwrap().double(UNIT_FACTS, move |_| Ok(facts.clone()));
    r.tick(STREAM, EventKind::new(STATE, &serde_json::to_string(&carry_idle(&carry)).unwrap())).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    let asked: Vec<&str> = r.requested().iter().map(|q| q.name.as_str()).collect();
    assert!(asked.contains(&UNIT_FACTS) && asked.contains(&INBOX_APPEND) && asked.contains(&IMAGE_CLEAN), "{asked:?}");
}

#[test_id(t52r8ul)]
#[test]
fn sp_4pne6q_a_refused_open_record_reaches_the_machine_with_its_tag() {
    use spira_runtime::tasks::a::batcher_local::edge::opened_refusing;
    let record = "refused=sp-b kind=image-absent image=tagz why=sp-b: build it\nrefused=sp-c kind=held why=sp-c: held\n";
    let opened = opened_refusing(batcher_cut::io::parse_refused(record));
    let (_, acts) = on_outcome(&opening(), ROUND_OPEN, true, &serde_json::to_string(&opened).unwrap(), &open_leg(), NOW);
    assert_eq!(builds(&acts).iter().map(|l| l.image.as_str()).collect::<Vec<_>>(), ["tagz"]);
}
