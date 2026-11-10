//! T1–T6 for `watch.warm_full` (contract `wiki/projects/spira/designs/tasks/watch-warm-full.md`),
//! a once-then-clears alarm with a destructive reap. The behaviour tests are red until the task
//! is ported: the stub's `decide` panics, the rig quarantines the stream, and `assert_healthy`
//! turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_warm_full::*;
use spira_runtime::watchers::INBOX_APPEND;

fn config() -> Config {
    Config { base: "/w".into(), cap_mib: 6144, inbox: "/q/inbox".into() }
}

fn slot(n: u32, mib: u64, held_by: Option<&str>) -> Slot {
    Slot { name: format!("slot.{n}"), mib, held_by: held_by.map(str::to_string) }
}

fn fill(pct: u8, slots: Vec<Slot>) -> Snapshot {
    Snapshot { base: Some("/w".into()), pct: Some(pct), slots }
}

/// Full, with the only data in a slot a session holds: nothing can be reaped.
fn full() -> Snapshot {
    fill(90, vec![slot(1, 4096, Some("dwarf-ab")), slot(2, 0, None)])
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchWarmFull::boot();
    let mut r = TaskRig::new(seed, WatchWarmFull::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(REAP, |_| Ok("emptied".into()))
        .double(INBOX_APPEND, |l| Ok(l.into()));
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
fn t1_the_row_is_registered_and_its_boot_arms_the_two_minute_tick() {
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
fn t2_a_base_at_forty_percent_with_no_idle_slot_over_cap_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(fill(40, vec![slot(1, 2048, None), slot(2, 1024, Some("dwarf-ab"))]))));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

/// The latch: said once on the crossing, cleared on falling back under, said again on re-crossing.
#[test]
fn t3_the_crossing_is_said_once_cleared_under_the_limit_and_said_again() {
    let w = Arc::new(Mutex::new(full()));
    let mut r = rig(1, w.clone());
    assert_eq!((asked(&r, INBOX_APPEND).len(), carry(&r).said), (1, true), "quiet → full(said)");
    r.advance(2 * CADENCE_MS).unwrap();
    assert_eq!(asked(&r, INBOX_APPEND).len(), 1, "full → full records nothing");
    *w.lock().unwrap() = fill(40, vec![]);
    r.advance(CADENCE_MS).unwrap();
    assert!(!carry(&r).said, "full → quiet");
    let before = r.journaled().len();
    r.advance(2 * CADENCE_MS).unwrap();
    assert_eq!(r.journaled().len(), before, "quiet → quiet records nothing");
    *w.lock().unwrap() = full();
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(asked(&r, INBOX_APPEND).len(), 2, "a re-crossing is said again");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_warm_full.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.warm_full.bogus") && !sim.contains(&"watch.warm_full.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// The script's cases (`spira/warm-full.sh:56-70`): the crossing line, a held slot never reaped,
/// an idle slot over cap reaped below the limit.
#[test]
fn t5_the_reap_set_and_the_line_are_the_ones_warm_full_made() {
    let r = rig(1, Arc::new(Mutex::new(full())));
    assert!(asked(&r, REAP).is_empty(), "a held slot is never reaped");
    assert_eq!(asked(&r, INBOX_APPEND), ["1970-01-01T00:00:00Z [watch:warm-full] dwarf warm tmpfs /w is 90% full (limit 85%): slot.1=4096MiB slot.2=0MiB ; the slots a session holds cannot be emptied (held by: slot.1=dwarf-ab)"]);
    let r = rig(1, Arc::new(Mutex::new(fill(50, vec![slot(3, 7000, None), slot(4, 100, None)]))));
    acc::assert_healthy(&r, STREAM);
    let reaped = asked(&r, REAP);
    assert!(reaped.len() == 1 && reaped[0].contains("slot.3"), "an idle slot over cap is reaped below the limit: {reaped:?}");
    assert!(asked(&r, INBOX_APPEND).is_empty());
}

fn registration() -> runtime::event::Registration {
    WatchWarmFull::new(config()).registration()
}

fn said_once() -> TaskRig {
    rig(1, Arc::new(Mutex::new(full())))
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&said_once(), NAME);
    assert_eq!(rows, ["said"]);
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(said_once, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}
