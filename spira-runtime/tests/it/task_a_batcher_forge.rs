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
    test_ids: [ #[test_id(tbv77vy)] #[test_id(tpi9jdd)] #[test_id(t4tirwv)] #[test_id(tkuivja)] #[test_id(tyha3rb)] ],
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

#[test_id(t65wlk6)]
#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test_id(t474m94)]
#[test]
fn t3_batch_open_in_open_is_refused_naming_open() {
    let refused = step(State::Open, Trigger::BatchOpen).expect_err("a second batch_open is illegal");
    assert!(refused.contains("Open"), "{refused}");
}

#[test_id(t2ag42t)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    // The first held tick notes its round, changing the carry after that probe was asked.
    r.advance(2 * CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// T2's second shape: disabled, with a waiting set already paged, is silent.
#[test_id(t2iahyc)]
#[test]
fn t2_disabled_with_an_unchanged_waiting_set_is_silent() {
    let mut r = rig(1, Snapshot { enabled: false, ..busy() });
    kit::assert_unpoisoned(&r, STREAM);
    assert_silent_tick(&mut r, STREAM);
    kit::assert_unpoisoned(&r, STREAM);
}

/// `main.rs:1235-1254` (restored, see the contract): disabled pages BATCHES HELD once per change of the waiting set.
#[test_id(tp89hj8)]
#[test]
fn t5_disabled_pages_the_held_set_once() {
    let (carry, acts) = decide(&Carry::default(), &Snapshot { enabled: false, ..busy() });
    assert_eq!(acts.iter().filter(|a| a.effect == INBOX_APPEND).count(), 1, "{acts:?}");
    assert_eq!(carry.repos["deckdumpster"].state, State::Disabled);
    assert!(decide(&carry, &Snapshot { enabled: false, ..busy() }).1.is_empty(), "the same waiting set is not paged twice");
}

/// `main.rs:285-288`: an open batch means no cut; its settlement is handed to queue.step.
#[test_id(t4acu7r)]
#[test]
fn t5_an_open_batch_is_handed_to_queue_step_not_cut() {
    let mut s = busy();
    s.repos.get_mut("deckdumpster").unwrap().open_batch = Some(Batch { pr: 12, head: "h1".into(), members: vec!["dd-a".into()] });
    let (_, acts) = decide(&Carry::default(), &s);
    assert!(acts.iter().any(|a| a.effect == SETTLE_WANTED), "{acts:?}");
    assert!(acts.iter().all(|a| a.effect != ASSEMBLE && a.effect != BATCH_OPEN), "{acts:?}");
}

/// The cut: certified members are fetched then assembled.
#[test_id(t9nr9sy)]
#[test]
fn t5_a_certified_pool_is_fetched_then_assembled() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    let order: Vec<&str> = acts.iter().map(|a| a.effect.as_str()).collect();
    assert_eq!(order, [FETCH, ASSEMBLE], "{acts:?}");
    assert_eq!(carry.repos["deckdumpster"].state, State::Assembling);
}

/// A member refused for an unlanded prerequisite is carried with it, whether or not the pass cuts.
#[test_id(tpcg6xx)]
#[test]
fn a_member_refused_for_an_unlanded_dep_is_carried_with_that_dep() {
    let blocked = Member { deps: vec!["dd-base".into()], ..certified("dd-a", "t1") };
    let landed = Member { deps: vec!["dd-done".into()], ..certified("dd-b", "t2") };
    let s = Snapshot { repos: [("deckdumpster".to_string(), Repo { blockers: vec!["dd-base".into()], ..repo(vec![blocked, landed]) })].into(), ..idle() };
    let (carry, _) = decide(&Carry::default(), &s);
    let json = serde_json::to_value(&carry).unwrap();
    assert_eq!(json["repos"]["deckdumpster"]["blocked"]["dd-a"], "dd-base");
    assert!(json["repos"]["deckdumpster"]["blocked"].get("dd-b").is_none(), "{json}");
}

/// A corpus leg's `unit.ran`, on this task's own stream, moves the repo at once; another
/// task's run or a leg not carried changes nothing.
#[test_id(tbubtni)]
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
#[test_id(tnokg37)]
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
#[test_id(two2jt9)]
#[test]
fn t3_the_attribution_line_opens_or_blocks() {
    use batcher_cut::attribute::Attribution;
    use runtime::units::{Exit, RanOutput, UnitHandle, UnitRan};
    let rc = RepoCarry { state: State::Attributing, leg: Some("spira.a2".into()), members: vec![("sp-a".into(), "t1".into()), ("sp-b".into(), "t2".into())], run: 2, ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let ran = UnitRan { handle: UnitHandle::of(NAME, "spira.a2").unwrap(), exit: Exit::Code(0), duration_ms: 5 };
    let land = Attribution::Land { members: vec![("sp-b".into(), "t2".into())], attribution_secs: None, mended: false, head: String::new() };
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

#[test_id(t8kozdb)]
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

#[test_id(twqkpmk)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    kit::assert_kept_as_rows(&at_work(), NAME);
}

#[test_id(teq1iip)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}

/// A leg that ended while the runtime was down left no `unit.ran`: the tick reads it through
/// `unit.facts`.
#[test_id(tuwcxpf)]
#[test]
fn a_tick_reads_every_leg_in_flight_through_unit_facts() {
    use runtime::units::{Exit, UnitFacts, UnitState, UNIT_FACTS};
    let facts = serde_json::to_string(&UnitFacts { state: UnitState::Inactive, main_pid: None, exit: Some(Exit::Code(0)), since_ms: Some(1) }).unwrap();
    for state in [State::Corpus, State::Attributing] {
        let rc = RepoCarry { state, leg: Some("spira.r9.leg.1".into()), ..RepoCarry::default() };
        let carry = Carry { repos: [("spira".to_string(), rc)].into() };
        let facts = facts.clone();
        let mut r = TaskRig::new(1, BatcherForge::new(BTreeMap::new()).registration()).unwrap().double(UNIT_FACTS, move |_| Ok(facts.clone()));
        r.tick(STREAM, EventKind::new(STATE, &serde_json::to_string(&carry).unwrap())).unwrap();
        r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
        let asked: Vec<&str> = r.requested().iter().map(|q| q.name.as_str()).collect();
        assert!(asked.contains(&UNIT_FACTS), "{state:?}: {asked:?}");
        assert!(asked.iter().any(|n| *n != UNIT_FACTS && *n != OBSERVE), "{state:?}: the exit read from facts moved the machine: {asked:?}");
    }
}

fn read(id: &str, provisional: bool) -> batcher::core::Member {
    batcher::core::Member { id: id.into(), tip: "t1".into(), title: format!("{id} work"), priority: Some(2), express: false, base_fix: false, certified_at: 5, stack: BTreeMap::new(), blocked_by: vec![], provisional }
}

/// A snapshot of one repo as the probe builds it from the pool read.
fn observed(certified: Vec<batcher::core::Member>, left_out: Vec<batcher_cut::io::LeftOut>) -> Snapshot {
    Snapshot { repos: [("deckdumpster".to_string(), observed_repo(certified, left_out, None, "b1".into()))].into(), ..idle() }
}

#[test_id(toqrq04)]
#[test]
fn the_probe_keeps_a_provisional_member_submitted_and_a_certified_one_certified() {
    let states: Vec<(String, String)> = observed_repo(vec![read("dd-a", true), read("dd-b", false)], vec![], None, "b1".into()).pool.into_iter().map(|m| (m.id, m.state)).collect();
    assert_eq!(states, [("dd-a".to_string(), "SUBMITTED".to_string()), ("dd-b".to_string(), "CERTIFIED".to_string())]);
}

#[test_id(tr2iwn8)]
#[test]
fn a_lone_marked_submitted_member_is_cut() {
    let (_, acts) = decide(&Carry::default(), &observed(vec![read("dd-a", true)], vec![]));
    assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [FETCH, ASSEMBLE], "{acts:?}");
    let leg: spira_runtime::tasks::a::batcher_local::Leg = serde_json::from_str(&acts[1].input).unwrap();
    assert_eq!(leg.members, [("dd-a".to_string(), "t1".to_string())]);
}

fn uncertified(id: &str) -> batcher_cut::io::LeftOut {
    batcher_cut::io::LeftOut { id: id.into(), tip: "t1".into(), cause: batcher_cut::io::Cause::Uncertified }
}

fn named(acts: &[Act]) -> Vec<Vec<batcher_cut::io::LeftOut>> {
    acts.iter().filter(|a| a.effect == NOTE_LEFT_OUT).map(|a| serde_json::from_str(&serde_json::from_str::<spira_runtime::tasks::a::batcher_local::Leg>(&a.input).unwrap().why).unwrap()).collect()
}

#[test_id(thcg3kg)]
#[test]
fn a_row_the_cut_leaves_out_is_noted_once_per_change_of_the_set() {
    let snap = observed(vec![], vec![uncertified("dd-a")]);
    let (carry, acts) = decide(&Carry::default(), &snap);
    assert_eq!(named(&acts), [vec![uncertified("dd-a")]], "{acts:?}");
    let (_, again) = decide(&carry, &snap);
    assert!(again.is_empty(), "the same set is not noted twice: {again:?}");
}

#[test_id(tec6msh)]
#[test]
fn a_member_the_selection_sets_aside_is_noted_with_its_cause() {
    let blocked = batcher::core::Member { blocked_by: vec!["dd-base".into()], ..read("dd-a", true) };
    let mut snap = observed(vec![blocked], vec![]);
    snap.repos.get_mut("deckdumpster").unwrap().blockers = vec!["dd-base".into()];
    let (_, acts) = decide(&Carry::default(), &snap);
    let causes: Vec<_> = named(&acts).concat().into_iter().map(|l| (l.id, l.cause)).collect();
    assert_eq!(causes, [("dd-a".to_string(), batcher_cut::io::Cause::Blocked { dep: "dd-base".into() })], "{acts:?}");
}

#[test_id(tuqc8le)]
#[test]
fn a_failed_note_is_asked_again_on_the_next_tick() {
    let snap = observed(vec![], vec![uncertified("dd-a")]);
    let (carry, acts) = decide(&Carry::default(), &snap);
    let leg = serde_json::from_str(&acts[0].input).unwrap();
    let (carry, _) = on_outcome(&carry, NOTE_LEFT_OUT, false, "store down", &leg, NOW);
    assert_eq!(named(&decide(&carry, &snap).1).len(), 1);
}

#[test_id(t82hhoi)]
#[test]
fn an_open_batch_notes_nothing() {
    let mut snap = observed(vec![], vec![uncertified("dd-a")]);
    snap.repos.get_mut("deckdumpster").unwrap().open_batch = Some(Batch { pr: 12, head: "h1".into(), members: vec![] });
    let (_, acts) = decide(&Carry::default(), &snap);
    assert!(named(&acts).is_empty(), "{acts:?}");
}

#[test_id(tz74dm7)]
#[test]
fn a_disabled_repo_pages_marked_work_that_waits() {
    let (_, acts) = decide(&Carry::default(), &Snapshot { enabled: false, ..observed(vec![read("dd-a", true)], vec![]) });
    assert!(acts.iter().any(|a| a.effect == INBOX_APPEND && a.input.contains("dd-a")), "{acts:?}");
}

#[test_id(tz0gzx7)]
#[test]
fn a_member_whose_round_is_held_is_noted_with_that_cause() {
    let snap = observed(vec![read("dd-a", true)], vec![]);
    let (carry, acts) = decide(&Carry::default(), &snap);
    let leg = serde_json::from_str(&acts[1].input).unwrap();
    let (carry, _) = on_outcome(&carry, ASSEMBLE, true, r#"{"merged":[]}"#, &leg, NOW);
    let (carry, acts) = decide(&carry, &snap);
    assert!(acts.iter().all(|a| a.effect != ASSEMBLE), "a held round is not cut again: {acts:?}");
    let noted: Vec<_> = named(&acts).concat().into_iter().map(|l| (l.id.clone(), l.tag())).collect();
    assert_eq!(noted, [("dd-a".to_string(), "round-held".to_string())], "{acts:?}");
    assert!(named(&decide(&carry, &snap).1).is_empty(), "once per change");
}

/// A carry waiting in `state` on a round whose effect the scheduler may or may not hold.
fn waiting_on_a_round(state: State) -> Carry {
    let rc = RepoCarry { state, round_key: Some("sp-gyucje:a69188d0".into()), ..RepoCarry::default() };
    Carry { repos: [("deckdumpster".to_string(), rc)].into() }
}

fn one_submitted_flying(in_flight: Option<Vec<String>>) -> Snapshot {
    let mut s = observed(vec![read("sp-0iavy8.1", true)], vec![]);
    s.repos.get_mut("deckdumpster").unwrap().in_flight = in_flight;
    s
}

#[test_id(tdyir61)]
#[test]
fn an_assembling_or_opening_carry_with_its_effect_not_in_flight_is_cut_again() {
    for state in [State::Assembling, State::Opening] {
        let (carry, acts) = decide(&waiting_on_a_round(state), &one_submitted_flying(Some(vec![])));
        assert_eq!(acts.iter().map(|a| a.effect.as_str()).collect::<Vec<_>>(), [FETCH, ASSEMBLE], "{state:?}: {acts:?}");
        assert_eq!(carry.repos["deckdumpster"].state, State::Assembling, "{state:?}");
        assert_ne!(carry.repos["deckdumpster"].round_key.as_deref(), Some("sp-gyucje:a69188d0"), "{state:?}");
    }
}

#[test_id(tpsmo3q)]
#[test]
fn an_assemble_or_batch_open_in_flight_is_left_alone() {
    for (state, effect) in [(State::Assembling, ASSEMBLE), (State::Opening, BATCH_OPEN)] {
        for in_flight in [Some(vec![effect.to_string()]), None] {
            let before = waiting_on_a_round(state);
            let (carry, acts) = decide(&before, &one_submitted_flying(in_flight.clone()));
            assert!(acts.is_empty(), "{state:?} {in_flight:?}: {acts:?}");
            assert_eq!(carry.repos["deckdumpster"].state, state, "{in_flight:?}");
            assert_eq!(carry.repos["deckdumpster"].round_key, before.repos["deckdumpster"].round_key, "{state:?} {in_flight:?}");
        }
    }
}

#[test_id(ti2rq8d)]
#[test]
fn a_carried_assembling_round_with_nothing_in_flight_is_cut_through_the_handler() {
    use spira_runtime::tasks::a::batcher_local::{Assembled, Leg};
    let snapshot = serde_json::to_string(&one_submitted_flying(None)).unwrap();
    let assembled = serde_json::to_string(&Assembled { merged: vec![("sp-0iavy8.1".into(), "t1".into())], suites: false, ..Assembled::default() }).unwrap();
    let mut r = TaskRig::new(1, BatcherForge::new(BTreeMap::new()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(snapshot.clone()))
        .double(ASSEMBLE, move |_| Ok(assembled.clone()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(STATE, &serde_json::to_string(&waiting_on_a_round(State::Assembling)).unwrap())).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS * 2).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let names: Vec<&str> = r.requested().iter().map(|q| q.name.as_str()).collect();
    let cut = names.iter().position(|n| *n == ASSEMBLE).unwrap_or_else(|| panic!("no assemble was requested: {names:?}"));
    let open = r.requested()[cut..].iter().find(|q| q.name == BATCH_OPEN).unwrap_or_else(|| panic!("no batch_open followed: {names:?}"));
    let leg: Leg = serde_json::from_str(&open.input).unwrap();
    assert_eq!(leg.members, [("sp-0iavy8.1".to_string(), "t1".to_string())]);
}

/// The PR opens without its screenshots; they are asked for once, after it exists.
#[test_id(t4swm4o)]
#[test]
fn an_opened_batch_pr_asks_for_its_shots_once() {
    use spira_runtime::tasks::a::batcher_local::{Assembled, Leg};
    let mut world = one_submitted_flying(None);
    let before = serde_json::to_string(&world).unwrap();
    world.repos.get_mut("deckdumpster").unwrap().open_batch = Some(Batch { pr: 378, head: "h-round".into(), members: vec!["sp-0iavy8.1".into()] });
    let after = serde_json::to_string(&world).unwrap();
    let opened = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let saw = opened.clone();
    let assembled = serde_json::to_string(&Assembled { merged: vec![("sp-0iavy8.1".into(), "t1".into())], base: "b1".into(), suites: false, ..Assembled::default() }).unwrap();
    let mut r = TaskRig::new(1, BatcherForge::new(BTreeMap::new()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(if saw.load(std::sync::atomic::Ordering::SeqCst) { after.clone() } else { before.clone() }))
        .double(ASSEMBLE, move |_| Ok(assembled.clone()))
        .double(BATCH_OPEN, move |_| {
            opened.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(r#"{"pr":"378","head":"h-round"}"#.into())
        });
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE, BATCH_OPEN].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(STATE, &serde_json::to_string(&waiting_on_a_round(State::Assembling)).unwrap())).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS * 2).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let names: Vec<&str> = r.requested().iter().map(|q| q.name.as_str()).collect();
    let open = names.iter().position(|n| *n == BATCH_OPEN).unwrap_or_else(|| panic!("no batch_open was requested: {names:?}"));
    let shots: Vec<usize> = (0..names.len()).filter(|i| names[*i] == SHOTS).collect();
    assert!(matches!(shots[..], [at] if at > open), "one shots request follows the open: {names:?}");
    let input = &r.requested()[shots[0]].input;
    let leg: Leg = serde_json::from_str(input).unwrap();
    assert_eq!((leg.repo.as_str(), leg.base.as_str(), leg.head.as_str(), leg.key.as_str()), ("deckdumpster", "b1", "h-round", "deckdumpster:b1:h-round"));
    assert_eq!(serde_json::from_str::<serde_json::Value>(input).unwrap()["pr"], "378", "{input}");
}

/// The rig after three cadences with `hung` requested and never answered.
fn waiting_out_a_hung(hung: &str) -> TaskRig {
    let assembled = serde_json::to_string(&spira_runtime::tasks::a::batcher_local::Assembled { merged: vec![("dd-a".into(), "t1".into())], ..Default::default() }).unwrap();
    let mut r = TaskRig::new(1, BatcherForge::new(BTreeMap::new()).registration())
        .unwrap()
        .hanging(hung)
        .double(OBSERVE, |_| Ok(serde_json::to_string(&busy()).unwrap()))
        .double(ASSEMBLE, move |_| Ok(assembled.clone()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS * 3).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    r
}

/// How often the cut's answered fetch, its assemble and its open were asked. The engine refuses
/// a second ask of a hung effect, so only the fetch shows a re-cut under it.
fn cut_asks(r: &TaskRig) -> [usize; 3] {
    [FETCH, ASSEMBLE, BATCH_OPEN].map(|name| r.requested().iter().filter(|q| q.name == name).count())
}

#[test_id(t77erau)]
#[test]
fn a_hanging_assemble_is_asked_once() {
    assert_eq!(cut_asks(&waiting_out_a_hung(ASSEMBLE)), [1, 1, 0]);
}

#[test_id(tl8l85j)]
#[test]
fn a_hanging_batch_open_is_asked_once() {
    assert_eq!(cut_asks(&waiting_out_a_hung(BATCH_OPEN)), [1, 1, 1]);
}

#[test_id(t842jp4)]
#[test]
fn a_forge_assemble_held_by_a_halt_is_waited_for_and_released_once() {
    let assembled = serde_json::to_string(&spira_runtime::tasks::a::batcher_local::Assembled { merged: vec![("dd-a".into(), "t1".into())], ..Default::default() }).unwrap();
    let mut r = TaskRig::new(1, BatcherForge::new(BTreeMap::new()).registration())
        .unwrap()
        .double(OBSERVE, |_| Ok(serde_json::to_string(&busy()).unwrap()))
        .double(ASSEMBLE, move |_| Ok(assembled.clone()));
    for e in EFFECTS.iter().filter(|e| ![OBSERVE, ASSEMBLE].contains(e)) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.deliver(runtime::event::HALT_STREAM, EventKind::new(runtime::event::SCHEDULER_HALTED, "test")).unwrap();
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    r.advance(CADENCE_MS * 3).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    let held = r.journaled().iter().filter(|e| e.kind.tag == runtime::event::EFFECT_HELD).filter(|e| serde_json::from_str::<serde_json::Value>(&e.kind.body).unwrap()["spec"]["name"] == ASSEMBLE).count();
    let carry: Carry = r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).expect("a recorded carry");
    assert_eq!((held, carry.repos["deckdumpster"].state), (1, State::Assembling));
    r.deliver(runtime::event::HALT_STREAM, EventKind::new(runtime::event::SCHEDULER_RESUMED, "")).unwrap();
    assert_eq!(cut_asks(&r), [1, 1, 1]);
}

#[test_id(t5n898r)]
#[test]
fn a_forgotten_round_is_not_cut_again_while_the_forge_is_disabled() {
    let (carry, acts) = decide(&waiting_on_a_round(State::Assembling), &Snapshot { enabled: false, ..one_submitted_flying(Some(vec![])) });
    assert!(acts.iter().all(|a| a.effect != FETCH && a.effect != ASSEMBLE), "{acts:?}");
    assert_eq!(carry.repos["deckdumpster"].state, State::Disabled);
}
