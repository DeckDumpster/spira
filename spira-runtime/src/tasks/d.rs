//! Family D, watcher and alarm timers, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`; each task is one module under `d/`, its doc comment the
//! task's contract (`wiki/projects/spira/designs/tasks/<task>.md`).
//!
//! A row's runners were stubs until its task was ported and it moved from [`PENDING`] to
//! [`TASKS`]; every row of this family is ported. `inbox.append` and the `unit.*` effects are served by
//! `tasks::SHARED`, so none appears on a row here.
//!
//! A calendar row's `cadence_ms` is its first elapse on a clock that starts at 0, which is when
//! its boot tick arms the next one; the row table has no calendar cadence of its own yet.

use super::{Reads, Task};

pub mod acceptance;
pub mod broker_exec;
pub mod pattern;
pub mod watch_mend;
pub mod watch_perf;
pub mod watch_refusals;
pub mod watch_runtime;
pub mod watch_skew;
pub mod watch_sop_lint;
pub mod watch_tower;

pub static TASKS: &[Task] = &[
    Task {
        name: broker_exec::NAME,
        reads: Reads::Only(&["work-requests/", "work-request-index/"]),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: broker_exec::BrokerExec::boot,
        cadence_ms: |_| broker_exec::CADENCE_MS,
        effects: broker_exec::EFFECTS,
        runners: &[
            (broker_exec::OBSERVE, broker_exec::observe),
            (broker_exec::RUN, broker_exec::run),
        ],
        keys: &[broker_exec::KEYS],
        build: |_| broker_exec::BrokerExec.registration(),
    },
    Task {
        name: watch_skew::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: watch_skew::WatchSkew::boot,
        cadence_ms: |_| watch_skew::CADENCE_MS,
        effects: watch_skew::EFFECTS,
        runners: &[
            (watch_skew::OBSERVE, watch_skew::observe),
            (watch_skew::ESCALATE, watch_skew::escalate),
            (watch_skew::LOG, watch_skew::log),
            (watch_skew::REFRESH, watch_skew::refresh),
        ],
        keys: &[watch_skew::KEYS],
        build: |_| watch_skew::WatchSkew::pinned().registration(),
    },
    Task {
        name: watch_perf::NAME,
        reads: Reads::Undeclarable("it measures the store's own latency, so it must keep polling"),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: watch_perf::WatchPerf::boot,
        cadence_ms: |_| watch_perf::CADENCE_MS,
        effects: watch_perf::EFFECTS,
        runners: &[
            (watch_perf::OBSERVE, watch_perf::observe),
            (watch_perf::LOG, watch_perf::log),
            (watch_perf::SLOW_SAMPLE, watch_perf::slow_sample),
            (watch_perf::METRICS, watch_perf::metrics),
        ],
        keys: &[watch_perf::KEYS],
        build: |_| watch_perf::WatchPerf::pinned().registration(),
    },
    Task {
        name: watch_sop_lint::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: watch_sop_lint::WatchSopLint::boot,
        cadence_ms: |_| watch_sop_lint::CADENCE_MS,
        effects: watch_sop_lint::EFFECTS,
        runners: &[
            (watch_sop_lint::OBSERVE, watch_sop_lint::observe),
            (watch_sop_lint::FILE_INCIDENT, watch_sop_lint::file_incident),
        ],
        keys: &[watch_sop_lint::KEYS],
        build: |_| watch_sop_lint::WatchSopLint::pinned().registration(),
    },
    Task {
        name: watch_mend::NAME,
        reads: Reads::Polled(&["mendings/", "history/"]),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: watch_mend::WatchMend::boot,
        cadence_ms: |_| watch_mend::CADENCE_MS,
        effects: watch_mend::EFFECTS,
        runners: &[
            (watch_mend::OBSERVE, |_, _, _| watch_mend::run_observe()),
            (watch_mend::LOG_APPEND, |_, _, s| {
                watch_mend::log_append(&watch_mend::Config::pinned()?, &s.input)
            }),
            (watch_mend::LIFT_STOP, |_, _, s| {
                watch_mend::lift_stop(&watch_mend::Config::pinned()?, &s.input)
            }),
        ],
        keys: &[watch_mend::KEYS],
        build: |_| watch_mend::WatchMend::pinned().registration(),
    },
    Task {
        name: watch_refusals::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: watch_refusals::WatchRefusals::boot,
        cadence_ms: |_| watch_refusals::CADENCE_MS,
        effects: watch_refusals::EFFECTS,
        runners: &[
            (watch_refusals::OBSERVE, |_, _, _| {
                watch_refusals::observe(&watch_refusals::Config::pinned()?)
            }),
            (watch_refusals::FILE, |_, _, s| {
                watch_refusals::file(&s.input)
            }),
        ],
        keys: &[watch_refusals::KEYS],
        build: |_| watch_refusals::WatchRefusals::pinned().registration(),
    },
    Task {
        name: watch_runtime::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: watch_runtime::WatchRuntime::boot,
        cadence_ms: |_| watch_runtime::CADENCE_MS,
        effects: watch_runtime::EFFECTS,
        runners: &[
            (watch_runtime::OBSERVE, |_, _, s| {
                watch_runtime::observe(&s.input)
            }),
            (watch_runtime::FILE_BUMP, |_, _, s| {
                watch_runtime::file_bump(&s.input)
            }),
            (watch_runtime::METRICS, |_, _, s| {
                watch_runtime::metrics(&watch_runtime::Config::pinned()?, &s.input)
            }),
            (watch_runtime::LOG, |_, _, s| {
                watch_runtime::log(&watch_runtime::Config::pinned()?, &s.input)
            }),
        ],
        keys: &[watch_runtime::KEYS],
        build: |_| watch_runtime::WatchRuntime::pinned().registration(),
    },
    Task {
        name: watch_tower::NAME,
        reads: Reads::Undeclarable(
            "it reads audits over a newest-first time window, which a change to a key cannot say",
        ),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: watch_tower::WatchTower::boot,
        cadence_ms: |_| watch_tower::CADENCE_MS,
        effects: watch_tower::EFFECTS,
        runners: &[
            (watch_tower::OBSERVE, |_, _, _| watch_tower::observe()),
            (watch_tower::PROMPT, |_, _, s| watch_tower::prompt(&s.input)),
            (watch_tower::ALARM, |_, _, s| watch_tower::alarm(&s.input)),
            (watch_tower::DRAIN_CHECK, |_, _, _| {
                watch_tower::drain_check()
            }),
            (watch_tower::LOG, |_, _, s| watch_tower::log(&s.input)),
        ],
        keys: &[watch_tower::KEYS],
        build: |_| watch_tower::WatchTower::pinned().registration(),
    },
];

/// The family's rows, written from their contracts and not yet ported; see `tasks`.
pub static PENDING: &[Task] = &[];
