//! Tests for `watch.unit_usage`: the timer that reads every unit's cgroup counters and pushes
//! them to the metrics store as a time series per unit class. The unit-name folding and the
//! counter read are tested in `runtime::usage`; here the row's registration, its tick, what
//! the running totals do as units come and go, and the shipped dashboard.

#[macro_use]
extern crate spira_test_id;
use std::cell::RefCell;
use std::time::SystemTime;

use runtime::pools::EffectClass;
use runtime::taskrig::{assert_silent_tick, effects_runnable, TaskRig};
use runtime::usage::{UnitCounters, UnitDisk};
use spira_otel::metrics::Series;
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_unit_usage::*;

fn rig(seed: u64) -> TaskRig {
    let (_, stream, kind) = WatchUnitUsage::boot();
    let mut r = TaskRig::new(seed, WatchUnitUsage.registration()).unwrap().double(SAMPLE, |_| Ok("3 unit(s): 9 series pushed".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn gate(path: &str, cpu_usec: u64, write_bytes: u64) -> UnitCounters {
    UnitCounters {
        class: "gate-worker".into(),
        unit: "gate-worker".into(),
        cgroup: path.into(),
        cpu_usec,
        io: vec![UnitDisk { device: "sdc".into(), role: "ssd-b".into(), write_bytes, write_ios: write_bytes / 10 }],
        pids_current: 2,
        pids_peak: if path.ends_with('a') { 5 } else { 9 },
    }
}

#[derive(Default)]
struct Recorder {
    pushed: RefCell<Vec<Vec<Series>>>,
    refuse: bool,
}

impl Exporter for Recorder {
    fn export(&self, series: &[Series]) -> Result<(), String> {
        if self.refuse {
            return Err("metrics push: status 503".into());
        }
        self.pushed.borrow_mut().push(series.to_vec());
        Ok(())
    }
}

fn label<'a>(s: &'a Series, k: &str) -> &'a str {
    s.labels.iter().find(|(n, _)| n == k).map_or("", |(_, v)| v.as_str())
}

fn last<'a>(rec: &'a Recorder, name: &str) -> Series {
    rec.pushed.borrow().last().unwrap().iter().find(|s| s.name == name && label(s, "class") == "gate-worker").unwrap_or_else(|| panic!("{name}")).clone()
}

#[test_id(t482a3n)]
#[test]
fn the_row_is_registered_with_its_class_its_mode_and_a_minute_cadence() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    assert_eq!(spira_runtime::classes::class_of(SAMPLE), Some(EffectClass::Host));
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), 60_000);
    assert_eq!(s.task_modes.get(NAME), Some(&runtime::Mode::Act), "SPIRA_TASK_MODES names the task");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(60_000));
    assert_eq!(r.requested().iter().map(|q| (q.name.as_str(), q.journaled)).collect::<Vec<_>>(), [(SAMPLE, false)], "a tick is one unjournaled probe");
}

#[test_id(tixf9k4)]
#[test]
fn every_tick_samples_and_journals_nothing() {
    let mut r = rig(1);
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test_id(t32l1gl)]
#[test]
fn the_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_unit_usage.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test_id(tjpxij3)]
#[test]
fn a_class_totals_the_increases_of_units_that_come_and_go_and_never_decreases() {
    let mut t = Tracker::default();
    let rec = Recorder::default();
    let at = SystemTime::now();
    push(&mut t, &[gate("g/a", 10_000_000, 1000), gate("g/b", 4_000_000, 500)], &rec, at).unwrap();
    assert_eq!((last(&rec, CPU_SECONDS).value, last(&rec, IO_WRITE_BYTES).value), (0.0, 0.0), "the first sample is a baseline");
    // a grows by 3 s and 100 B; b is gone; c is new and carries 2 s and 40 B it did before it was seen.
    push(&mut t, &[gate("g/a", 13_000_000, 1100), gate("g/c", 2_000_000, 40)], &rec, at).unwrap();
    assert_eq!((last(&rec, CPU_SECONDS).value, last(&rec, IO_WRITE_BYTES).value, last(&rec, IO_WRITE_REQUESTS).value), (5.0, 140.0, 14.0));
    // a disappears, c grows by 1 s; a reused path reads lower and contributes its current value.
    push(&mut t, &[gate("g/c", 3_000_000, 90), gate("g/a", 500_000, 10)], &rec, at).unwrap();
    assert_eq!((last(&rec, CPU_SECONDS).value, last(&rec, IO_WRITE_BYTES).value), (6.0 + 0.5 + 0.0, 140.0 + 50.0 + 10.0));
    let cpu: Vec<f64> = rec.pushed.borrow().iter().map(|p| p.iter().find(|s| s.name == CPU_SECONDS).unwrap().value).collect();
    assert!(cpu.windows(2).all(|w| w[0] <= w[1]), "{cpu:?}");
    push(&mut t, &[], &rec, at).unwrap();
    assert_eq!(rec.pushed.borrow().len(), 3, "no unit is no push");
}

#[test_id(tzv1tg4)]
#[test]
fn pids_are_summed_and_peaked_over_live_units_and_the_disk_is_labelled() {
    let mut t = Tracker::default();
    let rec = Recorder::default();
    push(&mut t, &[gate("g/a", 1, 1), gate("g/b", 1, 1)], &rec, SystemTime::now()).unwrap();
    assert_eq!((last(&rec, PIDS).value, last(&rec, PIDS_PEAK).value), (4.0, 9.0));
    let w = last(&rec, IO_WRITE_BYTES);
    assert_eq!((label(&w, "device"), label(&w, "role"), label(&w, "unit")), ("sdc", "ssd-b", "gate-worker"));
    push(&mut t, &[gate("g/a", 1, 1)], &rec, SystemTime::now()).unwrap();
    assert_eq!((last(&rec, PIDS).value, last(&rec, PIDS_PEAK).value), (2.0, 5.0));
    push(&mut t, &[UnitCounters { class: "lc-serve".into(), unit: "lc-serve.service".into(), cgroup: "l".into(), ..UnitCounters::default() }], &rec, SystemTime::now()).unwrap();
    assert_eq!(last(&rec, PIDS).value, 0.0, "a class with no live unit is zero processes, not its last count");
}

#[test_id(tm9yb3e)]
#[test]
fn the_samplers_own_cpu_is_a_series_labelled_as_the_total_so_the_share_divides() {
    let mut t = Tracker::default();
    t.add_own_cpu_ns(1_500_000_000);
    t.add_own_cpu_ns(500_000_000);
    let rec = Recorder::default();
    let total = UnitCounters { class: "_total".into(), unit: "_total".into(), cgroup: "m".into(), cpu_usec: 1, ..UnitCounters::default() };
    push(&mut t, &[total], &rec, SystemTime::now()).unwrap();
    let pushed = rec.pushed.borrow();
    let own = pushed[0].iter().find(|s| s.name == SAMPLE_CPU_SECONDS).unwrap();
    let tot = pushed[0].iter().find(|s| s.name == CPU_SECONDS).unwrap();
    assert_eq!(own.value, 2.0);
    assert_eq!((label(own, "class"), label(own, "unit")), (label(tot, "class"), label(tot, "unit")));
}

#[test_id(t4clsqs)]
#[test]
fn a_refused_push_is_the_effects_error() {
    let refusing = Recorder { refuse: true, ..Recorder::default() };
    let mut t = Tracker::default();
    assert_eq!(push(&mut t, &[gate("g/a", 1, 1)], &refusing, SystemTime::now()), Err("metrics push: status 503".into()));
    let unconfigured = spira_otel::metrics::MetricsSink::new(None, "spira-runtime", std::time::Duration::from_secs(1));
    assert!(push(&mut t, &[gate("g/a", 1, 1)], &unconfigured, SystemTime::now()).is_err(), "no store configured is said, not swallowed");
}

#[test_id(t2rcovl)]
#[test]
fn each_dashboard_panel_runs_its_pinned_expression() {
    let dash: serde_json::Value = serde_json::from_str(include_str!("../../spira/metrics/grafana/dashboards/harness-cost.json")).unwrap();
    let panels = dash["panels"].as_array().unwrap();
    let expr = |title: &str, i: usize| panels.iter().find(|p| p["title"] == title).unwrap_or_else(|| panic!("{title}"))["targets"][i]["expr"].as_str().unwrap().to_string();
    assert_eq!(dash["title"], "harness cost");
    assert_eq!(expr("CPU by unit class", 0), PANEL_CPU);
    assert_eq!(expr("CPU by unit class", 1), PANEL_CPU_TOTAL);
    assert_eq!(expr("bytes written by unit class", 0), PANEL_WRITE_BYTES);
    assert_eq!(expr("write requests by disk", 0), PANEL_WRITE_REQUESTS);
    assert_eq!(expr("processes by unit class", 0), PANEL_PIDS);
    assert_eq!(expr("collector share", 0), PANEL_COLLECTOR_SHARE);
}
