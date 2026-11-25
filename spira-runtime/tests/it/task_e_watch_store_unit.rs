//! `watch.store_unit`: the row, its boot delay, and a tick through the real observe over a
//! scripted unit backend.

use std::sync::Arc;

use runtime::units::{ScriptedUnits, UnitFacts, UnitState};
use runtime::taskrig::{effects_runnable, TaskRig};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_store_unit::*;

const UNIT: &str = "dolt-beads.service";

fn scripted(state: UnitState) -> Arc<ScriptedUnits> {
    let units = Arc::new(ScriptedUnits::new(Arc::new(runtime::ports::VirtualClock::new(0))));
    units.install(UNIT, UnitFacts { state, ..UnitFacts::absent() });
    units
}

fn rig(units: &Arc<ScriptedUnits>) -> TaskRig {
    let config = Config { unit: UNIT.into(), installed: true };
    let u = units.clone();
    let (_, stream, kind) = WatchStoreUnit::boot();
    let mut r = TaskRig::new(1, WatchStoreUnit::pinned().registration()).unwrap().double(OBSERVE, move |_| serde_json::to_string(&observe_with(&config, Some(&*u))).map_err(|e| e.to_string())).double(MAIL, |_| Ok("msg-1".into())).double("inbox.append", |l| Ok(l.into()));
    r.tick(stream, kind).unwrap();
    r.advance(BOOT_DELAY_MS).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn asked(r: &TaskRig, name: &str) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == name).map(|q| q.input.clone()).collect()
}

#[test_id(tvszgj9)]
#[test]
fn the_row_is_registered_and_its_boot_waits_two_minutes_for_the_first_read() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), BOOT_DELAY_MS, "the row states when its first read falls");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!((r.engine().next_timer_at(), r.requested().len()), (Some(BOOT_DELAY_MS), 0), "boot only arms the first read");
}

#[test_id(tq328z2)]
#[test]
fn a_tick_over_an_inactive_unit_appends_one_inbox_line_asks_one_alert_and_starts_no_program() {
    let units = scripted(UnitState::Inactive);
    let mut r = rig(&units);
    assert_eq!(asked(&r, "inbox.append").len(), 1);
    let mail = asked(&r, MAIL);
    assert_eq!(mail.len(), 1);
    assert!(mail[0].contains("is 'inactive': the bead store is down. Run: systemctl --user start dolt-beads.service") && mail[0].contains("\"urgent\":true"), "{mail:?}");
    r.advance(2 * CADENCE_MS).unwrap();
    assert_eq!((asked(&r, "inbox.append").len(), asked(&r, MAIL).len()), (1, 1), "a standing outage alarms once");
    assert!(units.calls().is_empty(), "the task only reads: {:?}", units.calls());
    assert_eq!(acc::launcher(&acc::module_source("e/watch_store_unit.rs")), None);
}

#[test_id(txxfbt7)]
#[test]
fn an_active_unit_leaves_the_journal_and_the_effects_untouched() {
    let r = rig(&scripted(UnitState::Active));
    assert_eq!((asked(&r, "inbox.append").len(), asked(&r, MAIL).len()), (0, 0));
}

#[test_id(tbgvkj2)]
#[test]
fn every_effect_has_a_runner_and_a_double() {
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}
