//! T1–T6 for `watch.spill_key` (contract `wiki/projects/spira/designs/tasks/watch-spill-key.md`).
//! The behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_spill_key::*;
use spira_runtime::watchers::INBOX_APPEND;

const KEY: &str = "/h/.config/spira/ts-key";
const DAY_S: u64 = 86_400;

fn config() -> Config {
    Config { spill: "ec2".into(), state_dir: "/r/round-vm".into(), key_file: KEY.into(), warn_days: 14, launch_window_s: 1800, probe_deadline_s: 60, inbox: "/q/inbox".into() }
}

fn off() -> Snapshot {
    Snapshot { spill: String::new(), template: Some(Template::Ok), launch: None, key_file: None, expires: None, now: 0 }
}

/// Spill on, a recent good launch, a key `days_left` from expiry.
fn key(days_left: i64) -> Snapshot {
    Snapshot { spill: "ec2".into(), template: Some(Template::Ok), launch: Some(Launch::RecentOk), key_file: Some(KEY.into()), expires: Some((30 * DAY_S as i64 + days_left * DAY_S as i64) as u64), now: 30 * DAY_S }
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>) -> TaskRig {
    let (_, stream, kind) = WatchSpillKey::boot();
    let mut r = TaskRig::new(seed, WatchSpillKey::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(INBOX_APPEND, |l| Ok(l.into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn said(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

fn carry(r: &TaskRig) -> Carry {
    r.journaled().iter().rev().find(|e| e.kind.tag == STATE).map(|e| serde_json::from_str(&e.kind.body).unwrap()).unwrap_or_default()
}

#[test]
fn t1_the_row_is_registered_and_its_boot_arms_the_hourly_tick() {
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
fn t2_spill_off_with_the_template_ok_journals_nothing() {
    let mut r = rig(1, Arc::new(Mutex::new(off())));
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn t3_each_key_is_said_once_probe_and_template_keys_clear_and_expiry_keys_never_do() {
    let w = Arc::new(Mutex::new(key(10)));
    let mut r = rig(1, w.clone());
    assert_eq!(said(&r).len(), 1, "warn-<exp>: absent → said");
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(said(&r).len(), 1, "said → said records nothing");
    *w.lock().unwrap() = Snapshot { launch: Some(Launch::RecentFail { reason: "InsufficientInstanceCapacity".into() }), ..key(10) };
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(said(&r).len(), 2, "probe-<cksum>: absent → said");
    *w.lock().unwrap() = key(10);
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let keys = carry(&r).said;
    assert!(!keys.iter().any(|k| k.starts_with("probe-")), "a recent good launch clears probe-*: {keys:?}");
    assert!(keys.iter().any(|k| k.starts_with("warn-")), "warn-* never clears: {keys:?}");
    let tw = Arc::new(Mutex::new(Snapshot { template: Some(Template::Missing { tail2: "no template 9000".into() }), ..off() }));
    let mut t = rig(1, tw.clone());
    assert!(carry(&t).said.iter().any(|k| k.starts_with("template-")), "template-<cksum>: absent → said");
    *tw.lock().unwrap() = off();
    t.advance(CADENCE_MS).unwrap();
    assert!(!carry(&t).said.iter().any(|k| k.starts_with("template-")), "a template that checks ok clears template-*");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_spill_key.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.spill_key.bogus") && !sim.contains(&"watch.spill_key.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `spira/round-spill-key.sh:28` and the `cmd_pass` / `_rsk_probe` texts, exactly.
#[test]
fn t5_the_lines_are_the_ones_round_spill_key_wrote() {
    let r = rig(1, Arc::new(Mutex::new(key(10))));
    assert_eq!(said(&r), [format!("1970-01-01T00:00:00Z [watch:round-spill-key] ROUND SPILL KEY: the tailscale auth key in {KEY} expires in 10 days; replace it and record the new expiry in {KEY}.expires")]);
    let r = rig(1, Arc::new(Mutex::new(key(-1))));
    assert_eq!(said(&r), [format!("1970-01-01T00:00:00Z [watch:round-spill-key] ROUND SPILL KEY: the tailscale auth key in {KEY} EXPIRED; spilled passes cannot join the tailnet until it is replaced")]);
    let timeout = Snapshot { launch: Some(Launch::Probed { rc: 124, out_tail3: String::new(), took_s: 60 }), ..key(100) };
    let r = rig(1, Arc::new(Mutex::new(timeout)));
    assert_eq!(said(&r), ["1970-01-01T00:00:00Z [watch:round-spill-key] ROUND SPILL LAUNCH: probe could not decide: round-vm ec2-probe --dry-run gave no answer in 60s and no real EC2 launch is recorded in the last 1800s"]);
}

fn registration() -> runtime::event::Registration {
    WatchSpillKey::new(config()).registration()
}

/// A key inside its warning window, said once, behind a probe that took 100 s.
fn warned_after_a_slow_probe() -> TaskRig {
    rig(1, Arc::new(Mutex::new(Snapshot { launch: Some(Launch::Probed { rc: 0, out_tail3: String::new(), took_s: 100 }), ..key(10) })))
}

#[test]
fn the_carry_is_kept_as_rows_and_not_as_one_whole_row() {
    let rows = spira_runtime::tasks::a::kit::assert_kept_as_rows(&warned_after_a_slow_probe(), NAME);
    assert_eq!(rows, ["probe_latency_s", "said"]);
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(warned_after_a_slow_probe, registration, STREAM, runtime::event::EventKind::new(TICK, ""));
}
