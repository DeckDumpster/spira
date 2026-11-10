//! T1–T6 for `watch.skew` (contract `wiki/projects/spira/designs/tasks/watch-skew.md`). The
//! behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig, DAY_MS};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::watch_skew::*;

fn config() -> Config {
    Config { releases: "/rel".into(), release_repo: "o/spira".into(), gh_intake_repo: "o/spira".into(), gh: "gh".into(), run: "/r".into(), queue_dir: "/q".into() }
}

/// `queue.local`, running the tip that landed, verify ok, no stamps.
fn in_effect() -> Snapshot {
    Snapshot {
        repo: "/h/spira".into(),
        mode: Mode::Local,
        activated: Some("20261103T000000Z".into()),
        manifest_commit: Some("a".repeat(40)),
        base: Some("local/main".into()),
        tip: Some("a".repeat(40)),
        verify: Verify { ok: true, out: String::new() },
        ..Snapshot::default()
    }
}

fn behind() -> Snapshot {
    Snapshot { tip: Some("b".repeat(40)), ..in_effect() }
}

fn declined() -> Snapshot {
    Snapshot { mode: Mode::Checkout, tip: Some("b".repeat(40)), queue_lock_held: true, ..in_effect() }
}

type World = Arc<Mutex<Snapshot>>;

fn rig(seed: u64, world: World) -> TaskRig {
    let (_, stream, kind) = WatchSkew::boot();
    let mut r = TaskRig::new(seed, WatchSkew::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(ESCALATE, |_| Ok("{}".into()))
        .double(LOG, |l| Ok(l.into()))
        .double(REFRESH, |_| Ok("{}".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn asked(r: &TaskRig, name: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == name).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_hourly_tick() {
    let row = acc::row(NAME);
    assert_eq!((row.stream(), row.effects, row.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((row.cadence_ms)(&s), CADENCE_MS);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (row.build)(&s)).unwrap();
    let (_, stream, kind) = (row.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_the_tip_in_effect_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(in_effect())));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_a_condition_escalates_once_and_a_decline_streak_alarms_at_three() {
    let world = Arc::new(Mutex::new(behind()));
    let mut r = rig(1, world.clone());
    assert_eq!(asked(&r, ESCALATE).len(), 1, "clear → escalated(key)");
    assert_eq!(carry(&r).escalated.len(), 1);
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(asked(&r, ESCALATE).len(), 1, "escalated → the same key is a no-op");
    *world.lock().unwrap() = declined();
    r.advance(2 * CADENCE_MS).unwrap();
    assert!(asked(&r, LOG).is_empty(), "streaks 1 and 2 say nothing");
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(asked(&r, LOG).len(), 1, "the third declined pass alarms");
    assert_eq!(carry(&r).declined.get("/h/spira"), Some(&3));
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_skew.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.skew.bogus") && !sim.contains(&"watch.skew.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `escalate`'s operator question and `decline_streak_bump`'s log line (`skew/src/lib.rs:594-597,845-848`).
#[test]
fn t5_the_question_and_the_log_line_are_the_ones_skew_wrote() {
    let r = rig(1, Arc::new(Mutex::new(behind())));
    let q = asked(&r, ESCALATE);
    assert!(q.len() == 1 && q[0].contains("The Spira copy in force is not the code that landed") && q[0].contains("Skew check <skew@spira>"), "{q:?}");
    let mut r = rig(1, Arc::new(Mutex::new(declined())));
    r.advance(2 * CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let logged = asked(&r, LOG);
    assert!(logged.len() == 1 && logged[0].starts_with("skew: refresh of /h/spira has declined 3 consecutive passes"), "{logged:?}");
}

#[test]
fn t6_seven_days_with_a_tip_advance_replay_identically_inside_the_budget() {
    let r = assert_deterministic(
        |seed| {
            let world = Arc::new(Mutex::new(in_effect()));
            let mut r = rig(seed, world.clone());
            r.advance(3 * DAY_MS).unwrap();
            *world.lock().unwrap() = behind();
            r
        },
        7,
        4,
    );
    acc::assert_healthy(&r, STREAM);
    assert_eq!(r.requested().iter().filter(|q| q.name == ESCALATE).count(), 1);
    assert_journal_budget(&r, 2);
}

fn escalated_condition() -> TaskRig {
    rig(1, Arc::new(Mutex::new(behind())))
}

fn two_declined_passes() -> TaskRig {
    let mut r = rig(1, Arc::new(Mutex::new(declined())));
    r.advance(CADENCE_MS).unwrap();
    r
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&escalated_condition(), NAME);
    assert!(rows.iter().any(|k| k.starts_with("escalated/")), "{rows:?}");
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&two_declined_passes(), NAME);
    assert!(rows.iter().any(|k| k.starts_with("declined/")), "{rows:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(escalated_condition, || WatchSkew::new(config()).registration(), STREAM, WatchSkew::boot().2);
}

#[test]
fn a_boot_keeps_the_decline_streak_that_the_next_pass_alarms_on() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(two_declined_passes, || WatchSkew::new(config()).registration(), STREAM, WatchSkew::boot().2);
}
