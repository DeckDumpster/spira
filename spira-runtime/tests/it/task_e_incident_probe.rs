//! T1–T6 for `incident.probe` (contract `wiki/projects/spira/designs/tasks/incident-probe.md`).
//! Every tick sends by design, so its silence is "one send and nothing else". The behaviour tests
//! are red until the task is ported: the stub's `decide` panics, the rig quarantines the stream,
//! and `assert_healthy` turns that into the failure.

use std::sync::{Arc, Mutex};

use runtime::taskrig::{assert_deterministic, assert_journal_budget, effects_runnable, TaskRig, DAY_MS};
use runtime::{EFFECT_FAILED, EFFECT_REQUESTED, EFFECT_SUCCEEDED};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::incident_probe::*;

fn config() -> Config {
    Config { deadman_secs: 180, heartbeat_url: String::new(), run: "/r".into() }
}

fn rig(seed: u64, world: Arc<Mutex<Snapshot>>, primary: fn(&str) -> Result<String, String>) -> TaskRig {
    let (_, stream, kind) = IncidentProbe::boot();
    let mut r = TaskRig::new(seed, IncidentProbe::new(config()).registration()).unwrap().double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap())).double(SEND, primary);
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn delivered(_: &str) -> Result<String, String> {
    Ok("msg-1".into())
}

fn none() -> Arc<Mutex<Snapshot>> {
    Arc::new(Mutex::new(Snapshot { last_probe: None }))
}

fn sends(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == SEND).map(|q| q.input.clone()).collect()
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
fn t2_a_tick_sends_once_and_emits_nothing_else_and_between_ticks_nothing_happens() {
    let mut r = rig(1, none(), delivered);
    let tags: Vec<String> = r.journaled().iter().map(|e| e.kind.tag.clone()).collect();
    assert_eq!(tags, [EFFECT_REQUESTED, EFFECT_SUCCEEDED], "one send, no Emit");
    let before = r.journaled().len();
    r.advance(CADENCE_MS / 2).unwrap();
    acc::assert_healthy(&r, STREAM);
    assert_eq!(r.journaled().len(), before, "between ticks nothing");
}

#[test]
fn t3_each_tick_sends_under_its_own_key_and_a_failure_never_falls_back() {
    let mut r = rig(1, none(), delivered);
    r.advance(2 * CADENCE_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    let s = sends(&r);
    assert_eq!(s.len(), 3, "idle → sending → delivered, once per tick");
    assert!(s.iter().all(|i| i.contains("incident_probe/")), "keyed by the tick: {s:?}");
    let mut keys = s.clone();
    keys.dedup();
    assert_eq!(keys.len(), 3, "a second send for the same tick would share its key");
    let mut down = rig(1, none(), |_| Err("primary down".into()));
    down.advance(CADENCE_MS).unwrap();
    assert_eq!(sends(&down).len(), 2, "a failed probe is not retried inside the hour and has no fallback");
}

#[test]
fn t4_every_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/incident_probe.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(!real.contains(&"incident.probe.bogus") && !sim.contains(&"incident.probe.bogus"));
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

/// `alarms_pass::probe` (`incident/src/alarms_pass.rs:689-699`): the mailbox, subject and body.
#[test]
fn t5_the_probe_is_the_note_the_alarms_component_sent() {
    let r = rig(1, none(), delivered);
    let s = sends(&r);
    assert!(s.len() == 1 && s[0].contains("alarms-probe") && s[0].contains("alarms probe of the primary delivery path"), "{s:?}");
    assert!(s[0].contains("The alarms component's hourly probe: the primary path delivered this."), "{s:?}");
    let last = Snapshot { last_probe: Some(Probe { at: 0, outcome: Outcome::Failed { reason: "primary down".into() } }) };
    let r = rig(1, Arc::new(Mutex::new(last)), delivered);
    assert_eq!(sends(&r).len(), 1, "the last outcome does not gate the send");
}

/// The contract's T6: seven virtual days with the primary down for three hours once. Each send
/// journals its request and its outcome, and the boot tick sends too: 2 × (7 × 24 + 1) events in
/// 168 h, two over 2/h, so the exact count is pinned and the ceiling is 3/h.
#[test]
fn t6_seven_days_with_a_three_hour_outage_replay_identically_at_one_send_an_hour() {
    const DAYS: u64 = 7;
    let make = |seed| {
        let n = Arc::new(Mutex::new(0u64));
        let world = none();
        let (_, stream, kind) = IncidentProbe::boot();
        let mut r = TaskRig::new(seed, IncidentProbe::new(config()).registration())
            .unwrap()
            .double(OBSERVE, move |_| Ok(serde_json::to_string(&*world.lock().unwrap()).unwrap()))
            .double(SEND, move |_| {
                let mut n = n.lock().unwrap();
                *n += 1;
                if (50..53).contains(&*n) { Err("primary down".into()) } else { Ok(format!("msg-{n}")) }
            });
        r.tick(stream, kind).unwrap();
        r
    };
    let r = assert_deterministic(make, 7, DAYS);
    acc::assert_healthy(&r, STREAM);
    let per_hour = DAYS * DAY_MS / CADENCE_MS + 1;
    assert_eq!(sends(&r).len() as u64, per_hour, "one send an hour, the outage's hours included");
    let failed = r.trace().iter().filter(|e| e.2 == EFFECT_FAILED).count();
    assert_eq!(failed, 3, "the three hours down fail, and no retry or fallback adds a send");
    assert_eq!(r.trace().len() as u64, 2 * per_hour);
    assert_journal_budget(&r, 3);
}

fn registration() -> runtime::event::Registration {
    IncidentProbe::new(config()).registration()
}

#[test]
fn a_probe_that_carries_nothing_keeps_no_row_after_running() {
    let mut r = rig(1, none(), delivered);
    r.advance(CADENCE_MS).unwrap();
    let kept: Vec<_> = r.journal.state().into_iter().filter(|((t, _), v)| t == NAME && !v.is_empty()).collect();
    assert!(kept.is_empty(), "{kept:?}");
}

/// The send is keyed by the tick event that asked, whose sequence number a restart renumbers, so the
/// tick after a boot is compared by what it asks for with that key set aside.
#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    let asked = |restart: bool| {
        let mut r = rig(1, none(), delivered);
        if restart {
            r.reopen(registration()).expect("restart");
        }
        let mark = r.requested().len();
        r.tick(STREAM, runtime::event::EventKind::new(TICK, "")).expect("first tick");
        r.requested()[mark..].iter().map(|q| (q.name.clone(), q.input.replace(|c: char| c.is_ascii_digit(), ""))).collect::<Vec<_>>()
    };
    let unstopped = asked(false);
    assert_eq!(unstopped.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), [OBSERVE, SEND]);
    assert_eq!(asked(true), unstopped);
}
