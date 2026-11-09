//! T1–T6 for `inbox.notify` (contract `wiki/projects/spira/designs/tasks/inbox-notify.md`). The
//! behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::units::UNIT_START;
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::inbox_notify::*;

const MIN: u64 = 60_000;

fn config() -> Config {
    Config { actionable: "FAIL|DEGRADED".into(), health_timeout_s: 10, notify_age_s: 1800, instance: "prod".into(), run: "/r".into(), watchers: "/h/spira/watchers".into(), watchers_overlay: String::new(), mail_readers: "concierge".into(), mail_unread_age_s: 1800, mail: "/m".into() }
}

fn row(name: &str, lines: u64, cursor: u64, unread: &[&str]) -> RowView {
    RowView { name: name.into(), kind: "log".into(), lines, cursor, unread: unread.iter().map(|s| s.to_string()).collect(), pending: None, unhealthy: None }
}

/// Every cursor at its line count, the extern unit active and healthy.
fn caught_up() -> Snapshot {
    Snapshot {
        now: 0,
        halted: false,
        rows: vec![row("gate", 40, 40, &[]), row("mail-deliver", 3, 3, &[])],
        units: [("spira-mail-deliver-prod.service".to_string(), ("active".to_string(), 0))].into(),
        health: [("mail-deliver".to_string(), Health::Ok)].into(),
        mail_health: Some(0),
    }
}

fn stale() -> Snapshot {
    let mut s = caught_up();
    s.rows[0] = row("gate", 41, 40, &["[2026-11-03T00:00:00Z] gate FAIL test-x.sh"]);
    s
}

fn degraded() -> Snapshot {
    let mut s = caught_up();
    s.health.insert("mail-deliver".into(), Health::Degraded("no delivery in 900s".into()));
    s
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = InboxNotify::boot();
    let mut r = TaskRig::new(seed, InboxNotify::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(ASK, |_| Ok("msg-1".into()))
        .double(SOP_MATCH, |_| Ok("none".into()))
        .double(UNIT_START, |_| Ok("{}".into()))
        .double(SOP_APPLIED, |_| Ok("{}".into()))
        .double(CONCIERGE_NOTE, |_| Ok("{}".into()))
        .double(MAIL_HEALTH, |_| Ok("0".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn asks(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == ASK).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_five_minute_tick() {
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
fn t2_every_row_caught_up_and_healthy_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(caught_up())));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_the_pending_and_unhealthy_clocks_set_ask_once_and_clear_and_a_halted_world_asks_no_health() {
    let w = Arc::new(Mutex::new(stale()));
    let mut r = rig(1, w.clone());
    assert!(carry(&r).pending.contains_key("gate"), "pending none → set");
    assert!(asks(&r).is_empty(), "younger than SPIRA_NOTIFY_AGE asks nothing");
    r.advance(35 * MIN).unwrap();
    assert_eq!(asks(&r).len(), 1, "stale past the age → one ask");
    assert!(carry(&r).stamp_backlog.is_some(), "stamp none → fp");
    r.advance(10 * MIN).unwrap();
    assert_eq!(asks(&r).len(), 1, "the same fingerprint is not asked again");
    *w.lock().unwrap() = caught_up();
    r.advance(CADENCE_MS).unwrap();
    assert!(carry(&r).pending.is_empty() && carry(&r).stamp_backlog.is_none(), "pending and stamp cleared when nothing is stale");
    *w.lock().unwrap() = degraded();
    r.advance(CADENCE_MS).unwrap();
    assert!(carry(&r).unhealthy.contains_key("mail-deliver"), "unhealthy none → set");
    *w.lock().unwrap() = Snapshot { halted: true, ..degraded() };
    r.advance(40 * MIN).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(asks(&r).len(), 1, "a health ask while halted is refused");
    assert!(carry(&r).unhealthy.is_empty(), "a halted world clears the unhealthy clock");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/inbox_notify.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"inbox.notify.bogus") && !sim.contains(&"inbox.notify.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// The two asks `watchd notify` mailed (`notify.rs:146,286`, `ops.rs:252-259`).
#[test]
fn t5_the_asks_are_the_ones_watchd_notify_mailed() {
    let mut r = rig(1, Arc::new(Mutex::new(stale())));
    r.advance(35 * MIN).unwrap();
    acc::assert_healthy(&r, STREAM);
    let a = asks(&r);
    assert!(a.len() == 1 && a[0].contains(BACKLOG_SUBJECT) && a[0].contains("gate — 1 actionable event(s) with no reader, the oldest for") && a[0].contains("Watchd <watchd@spira>"), "{a:?}");
    let mut r = rig(1, Arc::new(Mutex::new(degraded())));
    r.advance(35 * MIN).unwrap();
    let a = asks(&r);
    assert!(a.len() == 1 && a[0].contains(HEALTH_SUBJECT) && a[0].contains("mail-deliver — DEGRADED for") && a[0].contains("no delivery in 900s"), "{a:?}");
}
