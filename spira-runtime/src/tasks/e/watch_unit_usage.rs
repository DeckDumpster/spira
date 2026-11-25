//! # watch.unit_usage — contract
//!
//! What every systemd unit of the harness user costs, as a time series per unit class, read from
//! the kernel's control groups once a minute and pushed to the metrics store. Measurement only:
//! it starts no timer, no child process and no loop of its own.
//!
//! ## Tick
//!
//! - Every [`CADENCE_MS`] (60 s). Single-flight; the sample is one read of the counter files of
//!   each unit directory under the user manager's slices (`runtime::usage::units`), no walk.
//!
//! ## Observe
//!
//! `watch.unit_usage.sample` (host class) reads every unit's counters, adds each cgroup's increase
//! since the previous tick to a running total per class, and pushes the totals straight to the
//! metrics store (law-components-push-metrics-directly). A tick journals nothing.
//!
//! The previous tick's counters live in this process. A cgroup absent at the previous tick
//! contributes its whole count, except on the first tick after start, which records a baseline and
//! pushes zero increase. A counter that went down (a reused path) contributes its current value.
//! A transient cgroup is removed when its unit ends, so what it did after its last sample is not
//! seen per class; the `_total` row is the user manager's own cgroup, which keeps the counts of
//! dead descendants, so the gap shows as the difference between it and the sum of the classes.
//!
//! Every series carries `class` and `unit` (see `runtime::usage::unit_class`; transient units fold
//! to their class so the label set stays bounded):
//!
//! | Series | Value |
//! |---|---|
//! | `spira_unit_cpu_seconds` | cumulative CPU |
//! | `spira_unit_io_write_bytes` | cumulative bytes written, also labelled `device`, `role` |
//! | `spira_unit_io_write_requests` | cumulative write requests, labelled the same |
//! | `spira_unit_pids` | sum of `pids.current` across the class's live units |
//! | `spira_unit_pids_peak` | max of `pids.peak` across the class's live units |
//! | `spira_unit_usage_sample_cpu_seconds` | cumulative on-CPU time of the sampling thread, labelled as the `_total` row |
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.unit_usage.sample` | host (cgroup read, metrics push) | 30 s | (probe) | a push the store refuses, or no metrics endpoint configured |
//!
//! ## Silence
//!
//! No unit directory and no manager cgroup: the probe pushes nothing.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::SystemTime;

use runtime::event::*;
use runtime::usage::{self, UnitCounters, TOTAL_CLASS};
use spira_otel::metrics::{MetricsSink, Series};

use crate::tasks::f::Cycle;

pub const NAME: &str = "watch.unit_usage";
pub const STREAM: &str = "watch.unit_usage";
pub const TICK: EventKindTag = "watch.unit_usage.tick";
pub const SAMPLE: &str = "watch.unit_usage.sample";
pub const EFFECTS: &[&str] = &[SAMPLE];
pub const KEYS: &[&str] = &["SPIRA_METRICS_ENDPOINT"];
pub const CADENCE_MS: u64 = 60_000;
const DEADLINE_MS: u64 = 30_000;

pub const CPU_SECONDS: &str = "spira_unit_cpu_seconds";
pub const IO_WRITE_BYTES: &str = "spira_unit_io_write_bytes";
pub const IO_WRITE_REQUESTS: &str = "spira_unit_io_write_requests";
pub const PIDS: &str = "spira_unit_pids";
pub const PIDS_PEAK: &str = "spira_unit_pids_peak";
pub const SAMPLE_CPU_SECONDS: &str = "spira_unit_usage_sample_cpu_seconds";

pub const PANEL_CPU: &str = r#"sum by (class) (rate(spira_unit_cpu_seconds{class!="_total"}[5m]))"#;
pub const PANEL_CPU_TOTAL: &str = r#"rate(spira_unit_cpu_seconds{class="_total"}[5m])"#;
pub const PANEL_WRITE_BYTES: &str = r#"sum by (class) (rate(spira_unit_io_write_bytes{class!="_total"}[5m]))"#;
pub const PANEL_WRITE_REQUESTS: &str = r#"sum by (role) (rate(spira_unit_io_write_requests{class!="_total"}[5m]))"#;
pub const PANEL_PIDS: &str = "spira_unit_pids";
pub const PANEL_COLLECTOR_SHARE: &str = r#"rate(spira_unit_usage_sample_cpu_seconds[1h]) / rate(spira_unit_cpu_seconds{class="_total"}[1h])"#;

const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: SAMPLE, deadline_ms: DEADLINE_MS };

/// Where the samples go: the metrics store in production, a recorder in a test.
pub trait Exporter {
    fn export(&self, series: &[Series]) -> Result<(), String>;
}

impl Exporter for MetricsSink {
    fn export(&self, series: &[Series]) -> Result<(), String> {
        self.try_push_series(series).map_err(|e| format!("metrics push: {e}"))
    }
}

type Unit = (String, String);
type Disk = (String, String, String, String);

#[derive(Clone, Debug, Default)]
struct Seen {
    cpu_usec: u64,
    io: BTreeMap<String, (u64, u64)>,
}

fn increase(now: u64, before: u64) -> u64 {
    if now < before {
        now
    } else {
        now - before
    }
}

/// The running totals of the counters seen so far, and what the previous tick saw.
#[derive(Debug, Default)]
pub struct Tracker {
    primed: bool,
    seen: BTreeMap<String, Seen>,
    cpu_usec: BTreeMap<Unit, u64>,
    io: BTreeMap<Disk, (u64, u64)>,
    pids: BTreeMap<Unit, (u64, u64)>,
    own_cpu_ns: u64,
}

impl Tracker {
    /// Takes one sample of every unit's counters.
    pub fn observe(&mut self, rows: &[UnitCounters]) {
        let first = !self.primed;
        self.primed = true;
        for live in self.pids.values_mut() {
            *live = (0, 0);
        }
        let mut seen = BTreeMap::new();
        for r in rows {
            let unit: Unit = (r.class.clone(), r.unit.clone());
            let before = self.seen.get(&r.cgroup);
            let cpu = self.cpu_usec.entry(unit.clone()).or_default();
            if !first {
                *cpu += increase(r.cpu_usec, before.map_or(0, |b| b.cpu_usec));
            }
            let mut io = BTreeMap::new();
            for d in &r.io {
                let key = (r.class.clone(), r.unit.clone(), d.device.clone(), d.role.clone());
                let total = self.io.entry(key).or_default();
                if !first {
                    let (b, n) = before.and_then(|b| b.io.get(&d.device)).copied().unwrap_or((0, 0));
                    total.0 += increase(d.write_bytes, b);
                    total.1 += increase(d.write_ios, n);
                }
                io.insert(d.device.clone(), (d.write_bytes, d.write_ios));
            }
            let live = self.pids.entry(unit.clone()).or_default();
            live.0 += r.pids_current;
            live.1 = live.1.max(r.pids_peak);
            seen.insert(r.cgroup.clone(), Seen { cpu_usec: r.cpu_usec, io });
        }
        self.seen = seen;
    }

    pub fn add_own_cpu_ns(&mut self, ns: u64) {
        self.own_cpu_ns += ns;
    }

    /// Every series the totals so far make, each stamped `at`.
    pub fn series(&self, at: SystemTime) -> Vec<Series> {
        let mk = |name: &str, value: f64, labels: &[(&str, &str)]| Series { name: name.into(), value, labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), at };
        let mut out = vec![];
        for ((class, unit), usec) in &self.cpu_usec {
            out.push(mk(CPU_SECONDS, *usec as f64 / 1e6, &[("class", class), ("unit", unit)]));
        }
        for ((class, unit, device, role), (bytes, ios)) in &self.io {
            let l = [("class", class.as_str()), ("unit", unit.as_str()), ("device", device.as_str()), ("role", role.as_str())];
            out.push(mk(IO_WRITE_BYTES, *bytes as f64, &l));
            out.push(mk(IO_WRITE_REQUESTS, *ios as f64, &l));
        }
        for ((class, unit), (current, peak)) in &self.pids {
            out.push(mk(PIDS, *current as f64, &[("class", class), ("unit", unit)]));
            out.push(mk(PIDS_PEAK, *peak as f64, &[("class", class), ("unit", unit)]));
        }
        out.push(mk(SAMPLE_CPU_SECONDS, self.own_cpu_ns as f64 / 1e9, &[("class", TOTAL_CLASS), ("unit", TOTAL_CLASS)]));
        out
    }
}

/// One sample: `rows` into `tracker`, the totals through `exporter`; says how many series went.
pub fn push(tracker: &mut Tracker, rows: &[UnitCounters], exporter: &dyn Exporter, at: SystemTime) -> Result<String, String> {
    if rows.is_empty() {
        return Ok("no unit cgroup".into());
    }
    tracker.observe(rows);
    let s = tracker.series(at);
    exporter.export(&s)?;
    Ok(format!("{} unit(s): {} series pushed", rows.len(), s.len()))
}

static TRACKER: Mutex<Option<Tracker>> = Mutex::new(None);

/// The first field of a `schedstat`: nanoseconds the thread has spent on a CPU.
pub fn schedstat_cpu_ns(text: &str) -> u64 {
    text.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn thread_cpu_ns() -> u64 {
    schedstat_cpu_ns(&std::fs::read_to_string(format!("{}/thread-self/schedstat", system_paths::PROC)).unwrap_or_default())
}

/// `watch.unit_usage.sample`: the live host's units, pushed to the configured store. The
/// thread's cost of this sample, the push included, is added to the totals the next one pushes.
pub fn run_sample(e: &crate::Effects, _key: &str, _s: &EffectSpec) -> Result<String, String> {
    let sink = MetricsSink::for_component(e.world.metrics_endpoint.as_deref(), "spira-runtime");
    let before = thread_cpu_ns();
    let rows = usage::units(&usage::Roots::live(&e.world.run_dir));
    let mut guard = TRACKER.lock().unwrap_or_else(|p| p.into_inner());
    let tracker = guard.get_or_insert_with(Tracker::default);
    let pushed = push(tracker, &rows, &sink, SystemTime::now());
    tracker.add_own_cpu_ns(thread_cpu_ns().saturating_sub(before));
    pushed
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchUnitUsage;

impl WatchUnitUsage {
    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.unit_usage/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for WatchUnitUsage {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(CADENCE_MS, view, ev, String::new, |body| {
            ctx.event().set("unit_usage.pushed", body);
            vec![]
        })
    }
}
