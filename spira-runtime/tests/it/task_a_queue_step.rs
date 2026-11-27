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
    test_ids: [ #[test_id(t9yc0vd)] #[test_id(t4o5nj9)] #[test_id(t0teobg)] #[test_id(t1fqr6r)] #[test_id(tepm8q0)] ],
}

fn local() -> Repo {
    Repo { mode: "queue.local".into(), lc_ok: true, base_sha: "b1".into(), ..Repo::default() }
}

fn batch() -> OpenBatch {
    OpenBatch { id: "batch-7".into(), version: 3, pr: Some(88), branch: "spira/batch-7".into(), sealed_head: "h7".into(), members: vec!["sp-a".into()], batcher_owned: true, owner: "batcher".into(), flip: Flip::No }
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

#[test_id(tda4ox0)]
#[test]
fn queue_step_declares_no_flake_effect() {
    assert!(EFFECTS.iter().all(|e| !e.contains("flake")), "{EFFECTS:?}");
}

#[test_id(ts5o087)]
#[test]
fn t3_every_row_of_the_state_table_is_taken() {
    for (from, on, to) in TABLE {
        assert_eq!(step(*from, *on), Ok(*to), "{from:?} × {on:?}");
    }
}

#[test_id(tj5fcuv)]
#[test]
fn t3_landing_x_unit_ran_is_refused_naming_landing() {
    let refused = step(State::Landing, Trigger::UnitRan).expect_err("landing × unit.ran is illegal");
    assert!(refused.contains("Landing"), "{refused}");
}

#[test_id(tcvw4q7)]
#[test]
fn t3_the_recorded_carry_is_the_carry_read() {
    let mut r = rig(1, busy());
    r.advance(CADENCE_MS).unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    kit::assert_read_equals_record::<Carry>(&r, STATE, OBSERVE);
}

/// `tests.rs` step cases: green at the sealed head on an unmoved base fast-forwards.
#[test_id(t2exzfd)]
#[test]
fn t5_green_pushes_the_sealed_head() {
    let (carry, acts) = decide(&Carry::default(), &busy());
    assert_eq!(effects(&acts), [PUSH], "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Pushing);
}

/// Red on a batcher-owned batch goes to judgement, a unit leg.
#[test_id(tuzo6r4)]
#[test]
fn t5_red_goes_to_judgement() {
    let red = with(Repo { check: Check::Red, ..busy().repos["spira"].clone() });
    let (carry, acts) = decide(&Carry::default(), &red);
    assert!(effects(&acts).contains(&JUDGEMENT), "{acts:?}");
    assert_eq!(carry.repos["spira"].state, State::Judging);
}

const NAMED_TEST_RED: &str = "red\nfailed-step: gate / Run tests\nstep-log:         FAIL [   0.412s] (1/1) demo::it a_test\n";

fn flipping(check: Check, pr_state: PrState, flip: Flip) -> Snapshot {
    let batch = OpenBatch { flip, ..batch() };
    with(Repo { mode: "queue".into(), open_batch: Some(batch), pr_state, check, ..busy().repos["spira"].clone() })
}

#[test_id(tr24rm2)]
#[test]
fn a_batch_the_flip_rerun_speaks_for_goes_to_the_verdict_leg_each_tick_neither_pushed_nor_judged() {
    for (check, pr) in [(Check::Red, PrState::Open), (Check::Green, PrState::Open), (Check::None, PrState::Merged)] {
        let snap = flipping(check, pr, Flip::VerdictPass);
        let (carry, first) = decide_with(&Policy::default(), &Carry::default(), &snap, 1);
        let (_, again) = decide_with(&Policy::default(), &carry, &snap, 2);
        for acts in [&first, &again] {
            assert_eq!(effects(acts), [RECONCILE], "{check:?} {pr:?}");
        }
        assert_ne!(first[0].key, again[0].key, "each tick asks anew: {check:?}");
    }
}

#[test_id(thh4c7d)]
#[test]
fn a_red_the_verdict_leg_already_judged_is_not_judged_again() {
    let (_, acts) = decide(&Carry::default(), &flipping(Check::Red, PrState::Open, Flip::Judged));
    assert_eq!(acts, Vec::<Act>::new());
}

#[test_id(tusd816)]
#[test]
fn the_flip_speaks_for_a_named_test_red_and_for_the_green_or_merge_that_owes_a_fix_bead() {
    let hold = |rerun, owed, judged, merged, out: &str| flip_hold(&FlipRecord { rerun, owed, judged }, merged, out);
    assert_eq!(hold(false, false, false, false, NAMED_TEST_RED), Flip::VerdictPass);
    assert_eq!(hold(true, true, false, false, NAMED_TEST_RED), Flip::VerdictPass);
    assert_eq!(hold(true, true, false, false, "green\n"), Flip::VerdictPass);
    assert_eq!(hold(true, true, false, true, ""), Flip::VerdictPass);
    assert_eq!(hold(true, false, true, false, NAMED_TEST_RED), Flip::Judged);
    assert_eq!(hold(false, false, true, false, NAMED_TEST_RED), Flip::Judged);
    assert_eq!(hold(true, true, false, false, "pending\n"), Flip::No);
    assert_eq!(hold(true, false, false, false, "green\n"), Flip::No);
    assert_eq!(hold(false, false, false, false, "red\nred-suite: test-a.sh\n"), Flip::No);
}

/// `classify_fault_rerun`: a harness fault reruns once.
#[test_id(tjp6f18)]
#[test]
fn t5_a_harness_fault_reruns() {
    let fault = with(Repo { check: Check::HarnessFault, ..busy().repos["spira"].clone() });
    let (_, acts) = decide(&Carry::default(), &fault);
    assert!(effects(&acts).contains(&RERUN) || effects(&acts).contains(&NOTIFY), "{acts:?}");
}

/// `ops/reconcile.rs:22`: a closed or merged PR is reconciled.
#[test_id(t68v4uw)]
#[test]
fn t5_a_closed_pr_is_reconciled() {
    let closed = with(Repo { pr_state: PrState::Closed, check: Check::None, ..busy().repos["spira"].clone() });
    let (_, acts) = decide(&Carry::default(), &closed);
    assert!(effects(&acts).contains(&RECONCILE), "{acts:?}");
}

fn operator_batch(owner: &str, pr_state: PrState, check: Check) -> Snapshot {
    let batch = OpenBatch { owner: owner.into(), batcher_owned: false, ..batch() };
    with(Repo { open_batch: Some(batch), pr_state, check, ..busy().repos["spira"].clone() })
}

fn reconcile_keys(acts: &[Act]) -> Vec<&str> {
    acts.iter().filter(|a| a.effect == RECONCILE).map(|a| a.key.as_str()).collect()
}

#[test_id(tj2q56i)]
#[test]
fn t5_a_merged_pr_of_an_operator_batch_is_reconciled() {
    let (_, acts) = decide(&Carry::default(), &operator_batch("operator", PrState::Merged, Check::None));
    assert_eq!(reconcile_keys(&acts), ["batch-7"], "{acts:?}");
}

#[test_id(taztjqz)]
#[test]
fn t5_a_closed_pr_of_an_operator_batch_is_reconciled() {
    let (_, acts) = decide(&Carry::default(), &operator_batch("operator", PrState::Closed, Check::None));
    assert_eq!(reconcile_keys(&acts), ["batch-7"], "{acts:?}");
}

#[test_id(tzn3uv5)]
#[test]
fn t5_a_merged_pr_of_a_concierge_batch_is_left_alone() {
    let (_, acts) = decide(&Carry::default(), &operator_batch("concierge", PrState::Merged, Check::None));
    assert_eq!(acts, Vec::<Act>::new());
}

#[test_id(tfqk7xu)]
#[test]
fn t5_an_open_pr_of_an_operator_batch_is_neither_pushed_nor_judged() {
    for (check, base) in [(Check::Green, "b1"), (Check::Green, "b2"), (Check::Red, "b1"), (Check::HarnessFault, "b1")] {
        let mut snap = operator_batch("operator", PrState::Open, check);
        snap.repos.get_mut("spira").unwrap().base_sha = base.into();
        let (carry, acts) = decide(&Carry::default(), &snap);
        assert_eq!(acts, Vec::<Act>::new(), "{check:?} on {base}");
        assert_eq!(carry.repos.get("spira").and_then(|r| r.batch.as_deref()), None, "{check:?} on {base}");
    }
}

/// The contract's States table sends `Observing × no batch, no publish rec` to `Cutting` with a
/// `cut_wanted` Emit, while its T2 makes that same snapshot silent. Silence wins (the soak
/// lesson): the idle repo settles once and then asks for nothing, so a cut is wanted only on a change.
#[test_id(t2fsscz)]
#[test]
fn t5_an_idle_repo_settles_once_then_wants_nothing() {
    let (carry, _) = decide(&Carry::default(), &idle());
    assert_eq!(decide(&carry, &idle()).1, Vec::<Act>::new());
}

/// The forge's view of an open batch is the batch record's: a pending check writes no
/// `forge-state` file, on the first pass or the next.
#[test_id(tk6dalr)]
#[test]
fn t5_a_pending_check_records_no_forge_state_file() {
    let pending = with(Repo { check: Check::Pending, ..busy().repos["spira"].clone() });
    let (carry, first) = decide(&Carry::default(), &pending);
    assert!(first.iter().all(|a| !(a.effect == RECORD && a.input.contains(queue::ops::forge_batch::FORGE_STATE))), "{first:?}");
    let (_, again) = decide(&carry, &pending);
    assert!(!effects(&again).contains(&RECORD), "{again:?}");
}

/// The judgement leg's `unit.ran`, on this task's own stream, moves the judging repo at once: a
/// green one is recorded and wants the cut on the base it was judged on; another task's run, or
/// a leg this repo is not judging, changes nothing.
#[test_id(tb2h82a)]
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

#[test_id(t6arui9)]
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

#[test_id(tbwtwol)]
#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    kit::assert_kept_as_rows(&at_work(), NAME);
}

#[test_id(t4r5kwh)]
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    kit::assert_boot_matches_an_unstopped_run(at_work, registration, STREAM, EventKind::new(TICK, ""));
}

/// The judgement leg ended while the runtime was down, so its `unit.ran` was never heard: the
/// next tick reads the unit through `unit.facts` and the machine moves on what it says.
#[test_id(tvxbbky)]
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

fn green_publish() -> Repo {
    Repo { publish_rec: Some(PublishRec { pr: 1123, head: "f89d".into(), branch: "spira/publish/1".into() }), publish_check: Check::Green, ..local() }
}

fn wakes(snap: Snapshot) -> usize {
    let (_, acts) = decide_with(&Policy::default(), &Carry::default(), &snap, 5);
    acts.iter().filter(|a| a.effect == PUBLISH_WANTED).count()
}

#[test_id(tn8vn5p)]
#[test]
fn a_green_standing_publish_record_wakes_publish_once() {
    assert_eq!(wakes(with(green_publish())), 1);
}

#[test_id(tv5m2ev)]
#[test]
fn a_publish_record_that_is_not_green_wakes_nothing() {
    for check in [Check::Pending, Check::None, Check::Red] {
        assert_eq!(wakes(with(Repo { publish_check: check, ..green_publish() })), 0, "{check:?}");
    }
}

#[test_id(tuak86d)]
#[test]
fn no_publish_record_a_queue_mode_repo_or_a_dead_lc_wakes_nothing() {
    assert_eq!(wakes(with(Repo { publish_rec: None, ..green_publish() })), 0);
    assert_eq!(wakes(with(Repo { mode: "queue".into(), ..green_publish() })), 0);
    assert_eq!(wakes(with(Repo { lc_ok: false, ..green_publish() })), 0);
}

#[test_id(tkjg7qf)]
#[test]
fn a_repo_judging_a_batch_still_wakes_publish_on_a_green_record() {
    let rc = RepoCarry { state: State::Judging, leg: Some("spira.j1".into()), base: "b2".into(), mode: "queue.local".into(), batch: Some("b-1:3".into()), leg_at_ms: 5, ..RepoCarry::default() };
    let carry = Carry { repos: [("spira".to_string(), rc)].into() };
    let (_, acts) = decide_with(&Policy::default(), &carry, &with(green_publish()), 5);
    assert_eq!(acts.iter().filter(|a| a.effect == PUBLISH_WANTED).count(), 1);
}

/// The rig with `hung` in flight at a restart, then reopened and settled.
fn restarted_with(hung: &str, snap: Snapshot) -> TaskRig {
    let snap = serde_json::to_string(&snap).unwrap();
    let mut r = TaskRig::new(1, QueueStep::new(BTreeMap::new()).registration()).unwrap().hanging(hung).double(OBSERVE, move |_| Ok(snap.clone()));
    for e in EFFECTS.iter().filter(|e| **e != OBSERVE) {
        r = r.double(e, |_| Ok("{}".into()));
    }
    r.tick(STREAM, EventKind::new(TICK, "")).unwrap();
    assert_eq!(inputs(&r, hung).len(), 1, "{hung} is in flight at the restart: {:?}", r.requested());
    r.reopen(QueueStep::new(BTreeMap::new()).registration()).unwrap();
    r.run_until_idle().unwrap();
    kit::assert_unpoisoned(&r, STREAM);
    r
}

fn inputs<'a>(r: &'a TaskRig, effect: &str) -> Vec<&'a str> {
    r.requested().iter().filter(|q| q.name == effect).map(|q| q.input.as_str()).collect()
}

fn asked_again_and_nothing_refused(effect: &str) {
    let r = restarted_with(effect, busy());
    let asked = inputs(&r, effect);
    assert!(matches!(asked[..], [a, b] if a == b), "{effect} is asked twice with one input: {asked:?}");
    assert_eq!(inputs(&r, NOTIFY), Vec::<&str>::new());
}

#[test_id(twfoj1n)]
#[test]
fn a_push_lost_at_a_restart_is_asked_again_and_no_refusal_is_told() {
    asked_again_and_nothing_refused(PUSH);
}

#[test_id(tbzpcbz)]
#[test]
fn a_land_lost_at_a_restart_is_asked_again_and_no_refusal_is_told() {
    asked_again_and_nothing_refused(LAND);
}

#[test_id(t0dv7p3)]
#[test]
fn a_judgement_lost_at_a_restart_stays_judging_and_tells_no_refusal() {
    let red = with(Repo { check: Check::Red, ..busy().repos["spira"].clone() });
    let r = restarted_with(JUDGEMENT, red);
    assert_eq!(inputs(&r, NOTIFY), Vec::<&str>::new());
    let carry: Carry = r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).expect("a recorded carry");
    assert_eq!(carry.repos["spira"].state, State::Judging);
}

/// The carry and act input of a repo whose `state` waits on an effect for batch-7.
fn waiting_in(state: State) -> (Carry, Step) {
    let rc = RepoCarry { state, batch: Some("batch-7:3".into()), leg: Some("spira.j1".into()), ..RepoCarry::default() };
    let step = Step { repo: "spira".into(), mode: "queue.local".into(), batch: "batch-7".into(), version: 3, head: "h7".into(), ..Step::default() };
    (Carry { repos: [("spira".to_string(), rc)].into() }, step)
}

fn notified(acts: &[Act]) -> Vec<Step> {
    acts.iter().filter(|a| a.effect == NOTIFY).map(|a| serde_json::from_str(&a.input).unwrap()).collect()
}

#[test_id(tgwh3z0)]
#[test]
fn a_push_or_land_that_failed_for_a_reason_still_tells_its_refusal() {
    for (state, effect, subject) in [(State::Pushing, PUSH, "batch batch-7 not landed — the fast-forward was refused"), (State::Landing, LAND, "batch batch-7 pushed but not landed on the store")] {
        let (carry, step) = waiting_in(state);
        let (next, acts) = on_outcome(&carry, effect, false, "refused: not an ancestor", &step, 1);
        assert_eq!(notified(&acts).iter().map(|s| s.subject.as_str()).collect::<Vec<_>>(), [subject]);
        assert_eq!(next.repos.get("spira").map_or(State::Idle, |r| r.state), State::Idle, "{effect}");
        assert_eq!(next.repos.get("spira").and_then(|r| r.batch.clone()), None, "{effect}");
    }
}

#[test_id(tndqfxh)]
#[test]
fn a_judgement_that_failed_for_a_reason_is_a_failed_judgement() {
    let (carry, step) = waiting_in(State::Judging);
    let (next, acts) = on_outcome(&carry, JUDGEMENT, false, "SPIRA_BATCHER_ENABLE=0: no judgement", &step, 1);
    let told = notified(&acts);
    assert_eq!(told.iter().map(|s| (s.subject.as_str(), s.body.as_str())).collect::<Vec<_>>(), [("judgement for batch-7 failed", "the judgement did not start: SPIRA_BATCHER_ENABLE=0: no judgement")]);
    assert_eq!(next.repos["spira"].state, State::Idle);
}

/// A store that refuses `land` and answers `show-batch` with the batch in `state`.
struct RefusingStore(&'static str);

impl spira_store::lc_call::Transport for RefusingStore {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
        let (code, out, err) = match argv.first().map(String::as_str) {
            Some("land") => (3, String::new(), "refused: expectation mismatch".to_string()),
            Some("show-batch") => (0, format!(r#"{{"state":"{}","version":4}}"#, self.0), String::new()),
            _ => (2, String::new(), format!("unexpected verb {argv:?}")),
        };
        Ok(spira_store::lc_call::Reply { code, out, err, read_ended: false })
    }
}

fn land_against(state: &'static str) -> Result<String, String> {
    let client = spira_store::lc_call::LcClient::over(std::sync::Arc::new(RefusingStore(state)), true);
    let lc = queue::real::RealLc { bin: None, client: Some(client) };
    edge::land_on(&lc, &waiting_in(State::Landing).1)
}

#[test_id(tq371jv)]
#[test]
fn a_land_refused_because_the_batch_already_landed_succeeds() {
    assert_eq!(land_against("LANDED"), Ok("{}".to_string()));
}

#[test_id(tu8ceq6)]
#[test]
fn a_land_refused_while_the_batch_is_not_landed_is_still_refused() {
    let refused = land_against("GREEN").expect_err("a GREEN batch was not landed");
    assert!(refused.starts_with("spira-lc land refused for batch-7 (rc=3)"), "{refused}");
}
