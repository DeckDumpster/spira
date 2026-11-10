//! T1–T6 for `watch.pass_rate` (contract `wiki/projects/spira/designs/tasks/watch-pass-rate.md`).
//! The behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_pass_rate::*;

fn config() -> Config {
    Config { ledger: "/r/batch-timing.tsv".into(), window_s: 86_400, max_pct: 150 }
}

fn rate(passes: u64, landed: u64) -> Snapshot {
    let pct = (passes * 100).checked_div(landed).unwrap_or(if passes == 0 { 0 } else { 999_999 });
    Snapshot { readable: true, passes, landed, pct, state: if pct > 150 { Rate::Over } else { Rate::Ok } }
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchPassRate::boot();
    let mut r = TaskRig::new(seed, WatchPassRate::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(MAIL, |_| Ok("msg-1".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn mails(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == MAIL).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_fifteen_minute_tick() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), CADENCE_MS);
    assert_eq!(Config::pinned().map(|_| ()), Ok(()), "every key resolves through the config door");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(CADENCE_MS));
    assert_eq!(r.requested().iter().map(|q| q.name.as_str()).collect::<Vec<_>>(), [OBSERVE]);
}

#[test]
fn t2_a_ledger_under_the_limit_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(rate(12, 10))));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_ok_to_over_mails_over_over_to_ok_mails_clear_and_blind_is_no_transition() {
    let w = Arc::new(Mutex::new(rate(40, 10)));
    let mut r = rig(1, w.clone());
    assert_eq!((carry(&r), mails(&r).len()), (Rate::Over, 1), "Ok → Over");
    r.advance(2 * CADENCE_MS).unwrap();
    assert_eq!(mails(&r).len(), 1, "Over → Over records nothing");
    *w.lock().unwrap() = rate(12, 10);
    r.advance(CADENCE_MS).unwrap();
    assert_eq!((carry(&r), mails(&r).len()), (Rate::Ok, 2), "Over → Ok");
    let before = r.journaled().iter().filter(|e| e.kind.tag == STATE).count();
    *w.lock().unwrap() = Snapshot { readable: false, ..rate(0, 0) };
    r.advance(2 * CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(r.journaled().iter().filter(|e| e.kind.tag == STATE).count(), before, "blind is not a state transition");
    assert_eq!(mails(&r).len(), 4, "but it says BLIND every tick, as today");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_pass_rate.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.pass_rate.bogus") && !sim.contains(&"watch.pass_rate.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// The lines `spira/pass-rate.sh:195-201` mailed to the concierge mailbox.
#[test]
fn t5_the_lines_are_the_ones_pass_rate_mailed() {
    let w = Arc::new(Mutex::new(rate(40, 10)));
    let mut r = rig(1, w.clone());
    *w.lock().unwrap() = rate(12, 10);
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let m = mails(&r);
    assert!(m.len() == 2 && m[0].contains("pass-rate: OVER: 40 gate pass(es) for 10 landing(s) in 86400s (limit 150%) — rounds are not landing on their first green pass") && m[0].contains(FROM), "{m:?}");
    assert!(m[1].contains("CLEAR: 12 gate pass(es) for 10 landing(s), back under 150%"), "{m:?}");
    let r = rig(1, Arc::new(Mutex::new(Snapshot { readable: false, ..rate(0, 0) })));
    assert!(mails(&r).iter().any(|m| m.contains("BLIND: the pass-rate watcher cannot read the ledger window")));
}

fn registration() -> runtime::event::Registration {
    WatchPassRate::new(config()).registration()
}

fn over() -> TaskRig {
    rig(1, Arc::new(Mutex::new(rate(40, 10))))
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&over(), NAME);
    assert_eq!(rows, ["state"]);
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(over, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}
