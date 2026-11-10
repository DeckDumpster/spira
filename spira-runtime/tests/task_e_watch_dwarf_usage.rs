//! Tests for `watch.dwarf_usage` (sp-364e5g): the timer that samples every live dwarf's cgroup
//! and pushes the samples to the metrics store. The sampler's parse is tested in
//! `runtime::usage`; here the row's registration, its tick, and what it pushes.

use std::cell::RefCell;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use runtime::pools::EffectClass;
use runtime::taskrig::{assert_deterministic, assert_journal_budget, assert_silent_tick, effects_runnable, TaskRig};
use runtime::usage::{DeviceIo, Kind, Sample};
use spira_otel::metrics::{MetricsSink, Series};
use spira_runtime::tasks::d::acceptance as acc;
use spira_runtime::tasks::e::watch_dwarf_usage::*;

fn rig(seed: u64) -> TaskRig {
    let (_, stream, kind) = WatchDwarfUsage::boot();
    let mut r = TaskRig::new(seed, WatchDwarfUsage { interval_ms: 60_000 }.registration()).unwrap().double(SAMPLE, |_| Ok("1 dwarf(s) and 0 hand scope(s): 8 series pushed".into()));
    r.tick(stream, kind).unwrap();
    acc::assert_healthy(&r, STREAM);
    r
}

fn dwarf() -> Sample {
    Sample {
        kind: Kind::Dwarf,
        unit: "spira-run-sentinel_2dcall-builder_2d1762500000000_2d0.service".into(),
        cgroup: "user.slice/x".into(),
        rune: "builder".into(),
        name: "fjalar".into(),
        bead: "sp-abc".into(),
        repo: "spira".into(),
        started_ms: Some(1_762_500_000_000),
        cpu_usec: 12_500_000,
        io: vec![DeviceIo { device: "sdc".into(), mount: "/srv/ssd-b".into(), role: "ssd-b".into(), read_bytes: 4096, write_bytes: 3_000_000_000 }],
        memory_current: 300,
        memory_peak: 900,
        worktree: "/run/worktree/sp-abc".into(),
        worktree_bytes: Some(10_000),
        build_dir: "/warm/slot.1".into(),
        build_dir_bytes: Some(20_000),
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

#[test]
fn the_row_is_registered_with_its_class_its_mode_and_its_cadence_key() {
    let t = acc::row(NAME);
    assert_eq!((t.stream(), t.effects, t.keys.concat()), (STREAM, EFFECTS, KEYS.to_vec()));
    assert!(KEYS.contains(&CADENCE_KEY));
    assert_eq!(spira_runtime::classes::class_of(SAMPLE), Some(EffectClass::Host), "every registered effect names a class");
    let s = acc::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.cadence_ms)(&s), 60_000, "the shipped pin is a minute");
    assert_eq!(s.task_modes.get(NAME), Some(&runtime::Mode::Act), "SPIRA_TASK_MODES names the task");
    let mut r = TaskRig::new(1, (t.build)(&s)).unwrap();
    let (_, stream, kind) = (t.boot)();
    r.tick(stream, kind).unwrap();
    assert_eq!(r.engine().next_timer_at(), Some(60_000));
    assert_eq!(r.requested().iter().map(|q| (q.name.as_str(), q.journaled)).collect::<Vec<_>>(), [(SAMPLE, false)], "a tick is one unjournaled probe");
}

#[test]
fn every_tick_samples_and_journals_nothing() {
    let mut r = rig(1);
    assert_silent_tick(&mut r, STREAM);
    acc::assert_healthy(&r, STREAM);
}

#[test]
fn the_effect_has_a_runner_and_a_double_and_the_module_spawns_nothing() {
    assert_eq!(acc::launcher(&acc::module_source("e/watch_dwarf_usage.rs")), None);
    let real: Vec<&str> = spira_runtime::tasks::routes().keys().copied().collect();
    let names = sim::doubles::service().names();
    let sim: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(effects_runnable(EFFECTS, &real, &sim), Ok(()));
}

#[test]
fn a_dwarf_sample_is_one_series_per_counter_each_labelled_with_who_and_where() {
    let at = UNIX_EPOCH + Duration::from_millis(1_762_500_090_000);
    let rec = Recorder::default();
    let said = push(&[dwarf()], &rec, at).unwrap();
    assert_eq!(said, "1 dwarf(s) and 0 hand scope(s): 8 series pushed");
    let pushed = rec.pushed.borrow();
    assert_eq!(pushed.len(), 1, "one push per sample set");
    let got: Vec<(&str, f64)> = pushed[0].iter().map(|s| (s.name.as_str(), s.value)).collect();
    assert_eq!(
        got,
        [
            (CPU_SECONDS, 12.5),
            (IO_READ_BYTES, 4096.0),
            (IO_WRITE_BYTES, 3e9),
            (MEMORY_BYTES, 300.0),
            (MEMORY_PEAK_BYTES, 900.0),
            (WORKTREE_BYTES, 10_000.0),
            (BUILD_DIR_BYTES, 20_000.0),
            (WALL_SECONDS, 90.0),
        ]
    );
    for s in pushed[0].iter() {
        assert_eq!((label(s, "kind"), label(s, "rune"), label(s, "name"), label(s, "bead"), label(s, "repo"), label(s, "unit")), ("dwarf", "builder", "fjalar", "sp-abc", "spira", dwarf().unit.as_str()), "{s:?}");
        assert_eq!(s.at, at);
    }
    let w = pushed[0].iter().find(|s| s.name == IO_WRITE_BYTES).unwrap();
    assert_eq!((label(w, "device"), label(w, "mount"), label(w, "role")), ("sdc", "/srv/ssd-b", "ssd-b"));
    let c = pushed[0].iter().find(|s| s.name == CPU_SECONDS).unwrap();
    assert_eq!(label(c, "device"), "", "only the IO series carry the disk");
}

#[test]
fn the_series_encode_as_otlp_gauges_for_the_metrics_store() {
    let at = UNIX_EPOCH + Duration::from_secs(1_762_500_090);
    let req = MetricsSink::new(Some("127.0.0.1:9"), "spira-runtime", Duration::from_secs(1)).encode_series(&series(&[dwarf()], at));
    let names: Vec<String> = req.resource_metrics[0].scope_metrics[0].metrics.iter().map(|m| m.name.clone()).collect();
    assert_eq!(names, [CPU_SECONDS, IO_READ_BYTES, IO_WRITE_BYTES, MEMORY_BYTES, MEMORY_PEAK_BYTES, WORKTREE_BYTES, BUILD_DIR_BYTES, WALL_SECONDS]);
}

#[test]
fn nothing_live_pushes_nothing_and_a_refused_push_is_an_error() {
    let rec = Recorder::default();
    assert_eq!(push(&[], &rec, SystemTime::now()), Ok("no live dwarf and no hand build".into()));
    assert!(rec.pushed.borrow().is_empty(), "no samples is no push");
    let mut hand = dwarf();
    hand.kind = Kind::Hand;
    hand.started_ms = None;
    hand.worktree_bytes = None;
    let rec = Recorder::default();
    assert_eq!(push(&[dwarf(), hand], &rec, SystemTime::now()), Ok("1 dwarf(s) and 1 hand scope(s): 14 series pushed".into()));
    let refusing = Recorder { refuse: true, ..Recorder::default() };
    assert_eq!(push(&[dwarf()], &refusing, SystemTime::now()), Err("metrics push: status 503".into()));
    let unconfigured = MetricsSink::new(None, "spira-runtime", Duration::from_secs(1));
    assert!(push(&[dwarf()], &unconfigured, SystemTime::now()).is_err(), "no store configured is said, not swallowed");
}

#[test]
fn two_days_of_minute_ticks_replay_identically_inside_the_budget() {
    let r = assert_deterministic(rig, 7, 2);
    acc::assert_healthy(&r, STREAM);
    assert_journal_budget(&r, 1);
}

#[test]
fn a_sampler_that_carries_nothing_keeps_no_row_after_running() {
    let mut r = rig(1);
    r.advance(120_000).unwrap();
    let kept: Vec<_> = r.journal.state().into_iter().filter(|((t, _), v)| t == NAME && !v.is_empty()).collect();
    assert!(kept.is_empty(), "{kept:?}");
}

#[test]
fn a_boot_repeats_no_side_effect_and_misses_none() {
    spira_runtime::tasks::a::kit::assert_boot_matches_an_unstopped_run(|| rig(1), || WatchDwarfUsage { interval_ms: 60_000 }.registration(), STREAM, runtime::event::EventKind::new(TICK, ""));
}
