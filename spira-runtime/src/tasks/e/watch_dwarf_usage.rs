//! # watch.dwarf_usage — contract
//!
//! What every live dwarf uses, sampled on a timer and pushed to the metrics store, so the dwarf
//! count can be set by hand from numbers (sp-364e5g). Measurement only: nothing here admits,
//! throttles or stops a dwarf.
//!
//! ## Tick
//!
//! - Every `SPIRA_RUNTIME_DWARF_USAGE_SECS` (60 s shipped). Single-flight; the sample is one
//!   read of each dwarf unit's cgroup files plus a walk of its worktree and build directory.
//!
//! ## Observe
//!
//! `watch.dwarf_usage.sample` (host class) reads every live dwarf unit's cgroup and every hand
//! build's session scope (`runtime::usage::sample`) and pushes one gauge sample per series
//! straight to the metrics store (law-components-push-metrics-directly), then answers how many
//! rows it pushed. It is a probe, so a tick journals nothing: the store is the record. The
//! push is telemetry, as the runtime's own OTLP logs are, so it runs in Observe mode too.
//!
//! Series, each labelled `kind` (`dwarf` or `hand`), `rune`, `name`, `bead`, `repo`, `unit`:
//!
//! | Series | Value |
//! |---|---|
//! | `spira_dwarf_cpu_seconds` | cumulative CPU over the unit's life (`cpu.stat` `usage_usec`) |
//! | `spira_dwarf_io_read_bytes` | cumulative bytes read, one series per disk, labelled `device`, `mount`, `role` |
//! | `spira_dwarf_io_write_bytes` | cumulative bytes written, labelled the same |
//! | `spira_dwarf_memory_bytes` | `memory.current` |
//! | `spira_dwarf_memory_peak_bytes` | `memory.peak` |
//! | `spira_dwarf_worktree_bytes` | bytes allocated under the worktree |
//! | `spira_dwarf_build_dir_bytes` | bytes allocated under the build directory (`CARGO_TARGET_DIR`) |
//! | `spira_dwarf_wall_seconds` | seconds since the unit started (a `sentinel-call` unit only) |
//!
//! A session is one `unit`; a bead's total is the sum over its units of each counter's last
//! value (`dwarves.sh usage bead <id>`, or `sum by (bead) (max_over_time(...[30d]))`).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `watch.dwarf_usage.sample` | host (cgroup read, metrics push) | 30 s | (probe) | a push the store refuses, or no metrics endpoint configured |
//!
//! ## Silence
//!
//! No live dwarf and no hand build: the probe pushes nothing. Every tick is a probe only.

use std::time::SystemTime;

use runtime::event::*;
use runtime::usage::{self, Sample};
use spira_otel::metrics::{MetricsSink, Series};

use crate::tasks::f::{cadence_ms, Cycle};

pub const NAME: &str = "watch.dwarf_usage";
pub const STREAM: &str = "watch.dwarf_usage";
pub const TICK: EventKindTag = "watch.dwarf_usage.tick";
pub const SAMPLE: &str = "watch.dwarf_usage.sample";
pub const EFFECTS: &[&str] = &[SAMPLE];
pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_DWARF_USAGE_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_METRICS_ENDPOINT"];
const DEADLINE_MS: u64 = 30_000;

pub use runtime::usage::metric::{BUILD_DIR_BYTES, CPU_SECONDS, IO_READ_BYTES, IO_WRITE_BYTES, MEMORY_BYTES, MEMORY_PEAK_BYTES, WALL_SECONDS, WORKTREE_BYTES};

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

/// Every series one sample set makes, each stamped `at`.
pub fn series(samples: &[Sample], at: SystemTime) -> Vec<Series> {
    let mut out = vec![];
    for s in samples {
        let base: Vec<(String, String)> = [("kind", s.kind.word()), ("rune", &s.rune), ("name", &s.name), ("bead", &s.bead), ("repo", &s.repo), ("unit", &s.unit)].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut one = |name: &str, value: f64, extra: &[(&str, &str)]| {
            let mut labels = base.clone();
            labels.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
            out.push(Series { name: name.into(), value, labels, at });
        };
        one(CPU_SECONDS, s.cpu_seconds(), &[]);
        for io in &s.io {
            let disk = [("device", io.device.as_str()), ("mount", io.mount.as_str()), ("role", io.role.as_str())];
            one(IO_READ_BYTES, io.read_bytes as f64, &disk);
            one(IO_WRITE_BYTES, io.write_bytes as f64, &disk);
        }
        one(MEMORY_BYTES, s.memory_current as f64, &[]);
        one(MEMORY_PEAK_BYTES, s.memory_peak as f64, &[]);
        if let Some(b) = s.worktree_bytes {
            one(WORKTREE_BYTES, b as f64, &[]);
        }
        if let Some(b) = s.build_dir_bytes {
            one(BUILD_DIR_BYTES, b as f64, &[]);
        }
        if let Some(started) = s.started_ms {
            let now = at.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
            one(WALL_SECONDS, now.saturating_sub(started) as f64 / 1000.0, &[]);
        }
    }
    out
}

/// Pushes `samples` through `exporter` and says how many rows went; nothing to push is no push.
pub fn push(samples: &[Sample], exporter: &dyn Exporter, at: SystemTime) -> Result<String, String> {
    if samples.is_empty() {
        return Ok("no live dwarf and no hand build".into());
    }
    let s = series(samples, at);
    exporter.export(&s)?;
    let hands = samples.iter().filter(|s| s.kind == usage::Kind::Hand).count();
    Ok(format!("{} dwarf(s) and {hands} hand scope(s): {} series pushed", samples.len() - hands, s.len()))
}

/// `watch.dwarf_usage.sample`: the live host, pushed to the configured store.
pub fn run_sample(e: &crate::Effects, _key: &str, _s: &EffectSpec) -> Result<String, String> {
    let sink = MetricsSink::for_component(e.world.metrics_endpoint.as_deref(), "spira-runtime");
    push(&usage::sample(&usage::Roots::live(&e.world.run_dir)), &sink, SystemTime::now())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchDwarfUsage {
    pub interval_ms: u64,
}

impl WatchDwarfUsage {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(WatchDwarfUsage { interval_ms: cadence_ms(settings, CADENCE_KEY)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("watch.dwarf_usage/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }
}

const STATE: EventKindTag = "watch.dwarf_usage.state";
const ROW_PARTS: &[runtime::rows::Part] = &[];

impl Handler for WatchDwarfUsage {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, String::new, |body| {
            ctx.event().set("dwarf_usage.pushed", body);
            vec![]
        })
    }
}
