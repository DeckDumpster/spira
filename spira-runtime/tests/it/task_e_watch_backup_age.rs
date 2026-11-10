//! T1–T6 for `watch.backup_age` (contract `wiki/projects/spira/designs/tasks/watch-backup-age.md`).
//! The behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_backup_age::*;

fn config() -> Config {
    Config { url: "s3://b/lc".into(), db: "spira_lifecycle".into(), max_age_s: 25_200, run: "/r".into() }
}

fn dbs(state: Db) -> Snapshot {
    Snapshot::Dbs(vec![("spira_lifecycle".into(), state)])
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchBackupAge::boot();
    let mut r = TaskRig::new(seed, WatchBackupAge::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(MAIL, |_| Ok("msg-1".into()));
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
fn t2_every_stamp_fresh_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(dbs(Db::Ok { age: 60 }))));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_ok_to_red_mails_red_red_to_ok_mails_clear_and_a_standing_state_records_nothing() {
    let w = Arc::new(Mutex::new(dbs(Db::Stale { age: 30_000 })));
    let mut r = rig(1, w.clone());
    assert_eq!((carry(&r), mails(&r).len()), (Carry::Red, 1), "Ok → Red");
    r.advance(2 * CADENCE_MS).unwrap();
    assert_eq!(mails(&r).len(), 1, "Red → Red records nothing");
    *w.lock().unwrap() = dbs(Db::Ok { age: 60 });
    r.advance(CADENCE_MS).unwrap();
    assert_eq!((carry(&r), mails(&r).len()), (Carry::Ok, 2), "Red → Ok");
    let before = r.journaled().len();
    r.advance(2 * CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(r.journaled().len(), before, "Ok → Ok records nothing");
    for red in [Snapshot::NoDestination, Snapshot::NoDatabase, dbs(Db::Never), dbs(Db::Unreadable)] {
        assert_eq!(carry(&rig(1, Arc::new(Mutex::new(red)))), Carry::Red);
    }
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_backup_age.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.backup_age.bogus") && !sim.contains(&"watch.backup_age.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `_lcb_tick` and `_lcb_report` (`spira/lc-backup.sh:328-348`): the alert's line, subject and From.
#[test]
fn t5_the_alerts_are_the_ones_lc_backup_mailed() {
    let w = Arc::new(Mutex::new(dbs(Db::Stale { age: 30_000 })));
    let mut r = rig(1, w.clone());
    *w.lock().unwrap() = dbs(Db::Ok { age: 60 });
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let m = mails(&r);
    assert_eq!(m.len(), 2);
    let red = "RED: a store has no good backup within 25200s (spira_lifecycle: stale 30000) — losing its disk now loses its data and history";
    assert!(m[0].contains(&format!("lc-backup: {red}")) && m[0].contains("## Alert") && m[0].matches(red).count() >= 2 && m[0].contains(FROM), "{m:?}");
    assert!(m[1].contains("CLEAR: every store has a fresh backup again (spira_lifecycle: ok 60)"), "{m:?}");
}
