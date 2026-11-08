//! T1–T6 for `watch.runtime` (contract `wiki/projects/spira/designs/tasks/watch-runtime.md`).
//! The behaviour tests are red until the task is ported: the stub's `decide` panics, the rig
//! quarantines the stream, and `assert_healthy` turns that into the failure.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::d::watch_runtime::*;

fn config() -> Config {
    Config { metrics_endpoint: "127.0.0.1:9".into(), run: "/r".into(), persona: PERSONA.into(), max_files: MAX_FILES }
}

/// One repo pinning rust `pin`, with upstream stable `stable`; no node or python pin.
fn pinned(pin: &str, stable: &str) -> Snapshot {
    let files: BTreeMap<String, Option<String>> = [("rust-toolchain.toml".to_string(), Some(format!("[toolchain]\nchannel = \"{pin}\"\n"))), ("package.json".to_string(), None), (".python-version".to_string(), None)].into();
    Snapshot {
        today: "2026-11-03".into(),
        repos: vec![Repo { name: "spira".into(), path: "/h/spira".into(), ref_: "local/main".into(), files, naming: BTreeMap::new() }],
        rust: Ok(format!("[pkg.rust]\nversion = \"{stable} (abc 2026-10-30)\"\n")),
        node: Ok("[]".into()),
        python: Ok("[]".into()),
        open_bumps: BTreeMap::new(),
    }
}

type World = Arc<Mutex<Snapshot>>;

fn rig(seed: u64, world: World) -> TaskRig {
    let (_, stream, kind) = WatchRuntime::boot();
    let mut r = TaskRig::new(seed, WatchRuntime::new(config()).registration())
        .unwrap()
        .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
        .double(FILE_BUMP, |_| Ok("sp-bump1".into()))
        .double(METRICS, |_| Ok("{}".into()))
        .double(LOG, |l| Ok(l.into()));
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
fn t1_the_row_is_registered_and_its_boot_arms_the_six_hour_tick() {
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

/// The contract's silence: the probe and one metrics push; no state Emit, no log line.
#[test]
fn t2_a_supported_pin_pushes_metrics_and_nothing_else() {
    let mut r = rig(1, Arc::new(Mutex::new(pinned("1.91", "1.91.0"))));
    let (journal, asks) = (r.journaled().len(), r.requested().len());
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let names: Vec<&str> = r.requested()[asks..].iter().map(|q| q.name.as_str()).collect();
    assert_eq!(names, [OBSERVE, METRICS]);
    assert!(r.journaled()[journal..].iter().all(|e| e.kind.tag != STATE));
}

#[test]
fn t3_a_pin_goes_stale_is_filed_once_and_files_again_after_its_bump_closes() {
    let w = Arc::new(Mutex::new(pinned("1.91", "1.93.0")));
    let mut r = rig(1, w.clone());
    assert_eq!(asked(&r, FILE_BUMP).len(), 1, "supported → stale-unfiled → filed");
    assert_eq!(carry(&r).ledger.get("spira\trust").map(String::as_str), Some("sp-bump1"), "filed(id)");
    w.lock().unwrap().open_bumps = [("spira\trust".to_string(), "sp-bump1".to_string())].into();
    r.advance(CADENCE_MS).unwrap();
    assert_eq!(asked(&r, FILE_BUMP).len(), 1, "filing while a live bump exists is refused");
    w.lock().unwrap().open_bumps.clear();
    r.advance(CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(asked(&r, FILE_BUMP).len(), 2, "closed → stale-unfiled → filed again");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("d/watch_runtime.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"watch.runtime.bogus") && !sim.contains(&"watch.runtime.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `pass`'s title (`runtime-watch/src/lib.rs:348`) and the `cannot check` line kept in the log.
#[test]
fn t5_the_bump_title_and_the_cannot_check_line_are_runtime_watchs() {
    let r = rig(1, Arc::new(Mutex::new(pinned("1.91", "1.93.0"))));
    let f = asked(&r, FILE_BUMP);
    assert!(f.len() == 1 && f[0].contains("bump rust 1.91 to 1.93"), "{f:?}");
    let mut blind = pinned("1.91", "1.93.0");
    blind.rust = Err("curl: (6) Could not resolve host: static.rust-lang.org".into());
    let r = rig(1, Arc::new(Mutex::new(blind)));
    assert!(asked(&r, FILE_BUMP).is_empty(), "unreachable upstream is cannot-check, not a bump");
    let logged = asked(&r, LOG);
    assert!(logged.len() == 1 && logged[0].contains("cannot check"), "{logged:?}");
}

#[test]
fn t6_two_days_replay_identically_inside_the_budget() {
    let r = assert_deterministic(|seed| rig(seed, Arc::new(Mutex::new(pinned("1.91", "1.91.0")))), 7, 2);
    acc::assert_healthy(&r, STREAM);
    assert_journal_budget(&r, 1);
}
