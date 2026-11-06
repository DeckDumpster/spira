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

use super::Task;

pub mod acceptance;
pub mod cert_full;
pub mod cert_sample;
pub mod pattern;
pub mod sweep;
pub mod watch_mend;
pub mod watch_perf;
pub mod watch_perf_happy;
pub mod watch_refusals;
pub mod watch_runtime;
pub mod watch_skew;
pub mod watch_sop_lint;
pub mod watch_statute_audit;
pub mod watch_tower;

pub static TASKS: &[Task] = &[
    Task { name: cert_sample::NAME, plane: runtime::Plane::Work, signal: &[], boot: cert_sample::CertSample::boot, cadence_ms: |_| cert_sample::next_tick(0), effects: cert_sample::EFFECTS, runners: &[(cert_sample::OBSERVE, cert_sample::observe), (cert_sample::BRANCH, cert_sample::branch), (cert_sample::FILE_BEAD, cert_sample::file_bead), (cert_sample::RECORD, cert_sample::record)], keys: &[cert_sample::KEYS], build: |_| cert_sample::CertSample::pinned().registration() },
    Task { name: cert_full::NAME, plane: runtime::Plane::Work, signal: &[], boot: cert_full::CertFull::boot, cadence_ms: |_| cert_full::next_tick(0), effects: cert_full::EFFECTS, runners: &[(cert_full::OBSERVE, cert_full::observe), (cert_full::CHECKOUT, cert_full::checkout), (cert_full::FILE_BEAD, cert_full::file_bead), (cert_full::RECORD, cert_full::record)], keys: &[cert_full::KEYS], build: |_| cert_full::CertFull::pinned().registration() },
    Task { name: watch_skew::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_skew::WatchSkew::boot, cadence_ms: |_| watch_skew::CADENCE_MS, effects: watch_skew::EFFECTS, runners: &[(watch_skew::OBSERVE, watch_skew::observe), (watch_skew::ESCALATE, watch_skew::escalate), (watch_skew::LOG, watch_skew::log), (watch_skew::REFRESH, watch_skew::refresh)], keys: &[watch_skew::KEYS], build: |_| watch_skew::WatchSkew::pinned().registration() },
    Task { name: watch_statute_audit::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_statute_audit::WatchStatuteAudit::boot, cadence_ms: |_| watch_statute_audit::next_tick(0), effects: watch_statute_audit::EFFECTS, runners: &[(watch_statute_audit::OBSERVE, watch_statute_audit::observe), (watch_statute_audit::FILE_BEAD, watch_statute_audit::file_bead)], keys: &[watch_statute_audit::KEYS], build: |_| watch_statute_audit::WatchStatuteAudit::pinned().registration() },
    Task { name: watch_perf_happy::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_perf_happy::WatchPerfHappy::boot, cadence_ms: |_| watch_perf_happy::CADENCE_MS, effects: watch_perf_happy::EFFECTS, runners: &[(watch_perf_happy::OBSERVE, watch_perf_happy::observe), (watch_perf_happy::CHECKOUT, watch_perf_happy::checkout), (watch_perf_happy::TEARDOWN, watch_perf_happy::teardown), (watch_perf_happy::LOG, watch_perf_happy::log), (watch_perf_happy::HISTORY, watch_perf_happy::history)], keys: &[watch_perf_happy::KEYS], build: |_| watch_perf_happy::WatchPerfHappy::pinned().registration() },
    Task { name: watch_perf::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_perf::WatchPerf::boot, cadence_ms: |_| watch_perf::CADENCE_MS, effects: watch_perf::EFFECTS, runners: &[(watch_perf::OBSERVE, watch_perf::observe), (watch_perf::LOG, watch_perf::log), (watch_perf::SLOW_SAMPLE, watch_perf::slow_sample), (watch_perf::METRICS, watch_perf::metrics)], keys: &[watch_perf::KEYS], build: |_| watch_perf::WatchPerf::pinned().registration() },
    Task { name: watch_sop_lint::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_sop_lint::WatchSopLint::boot, cadence_ms: |_| watch_sop_lint::CADENCE_MS, effects: watch_sop_lint::EFFECTS, runners: &[(watch_sop_lint::OBSERVE, watch_sop_lint::observe), (watch_sop_lint::FILE_INCIDENT, watch_sop_lint::file_incident)], keys: &[watch_sop_lint::KEYS], build: |_| watch_sop_lint::WatchSopLint::pinned().registration() },
    Task { name: watch_mend::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_mend::WatchMend::boot, cadence_ms: |_| watch_mend::CADENCE_MS, effects: watch_mend::EFFECTS, runners: &[(watch_mend::OBSERVE, |_, _, _| watch_mend::run_observe()), (watch_mend::LOG_APPEND, |_, _, s| watch_mend::log_append(&watch_mend::Config::pinned()?, &s.input)), (watch_mend::LIFT_STOP, |_, _, s| watch_mend::lift_stop(&watch_mend::Config::pinned()?, &s.input))], keys: &[watch_mend::KEYS], build: |_| watch_mend::WatchMend::pinned().registration() },
    Task { name: watch_refusals::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_refusals::WatchRefusals::boot, cadence_ms: |_| watch_refusals::CADENCE_MS, effects: watch_refusals::EFFECTS, runners: &[(watch_refusals::OBSERVE, |_, _, _| watch_refusals::observe(&watch_refusals::Config::pinned()?)), (watch_refusals::FILE, |_, _, s| watch_refusals::file(&s.input))], keys: &[watch_refusals::KEYS], build: |_| watch_refusals::WatchRefusals::pinned().registration() },
    Task { name: watch_runtime::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_runtime::WatchRuntime::boot, cadence_ms: |_| watch_runtime::CADENCE_MS, effects: watch_runtime::EFFECTS, runners: &[(watch_runtime::OBSERVE, |_, _, s| watch_runtime::observe(&s.input)), (watch_runtime::FILE_BUMP, |_, _, s| watch_runtime::file_bump(&s.input)), (watch_runtime::METRICS, |_, _, s| watch_runtime::metrics(&watch_runtime::Config::pinned()?, &s.input)), (watch_runtime::LOG, |_, _, s| watch_runtime::log(&watch_runtime::Config::pinned()?, &s.input))], keys: &[watch_runtime::KEYS], build: |_| watch_runtime::WatchRuntime::pinned().registration() },
    Task { name: watch_tower::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_tower::WatchTower::boot, cadence_ms: |_| watch_tower::CADENCE_MS, effects: watch_tower::EFFECTS, runners: &[(watch_tower::OBSERVE, |_, _, _| watch_tower::observe()), (watch_tower::PROMPT, |_, _, s| watch_tower::prompt(&s.input)), (watch_tower::ALARM, |_, _, s| watch_tower::alarm(&s.input)), (watch_tower::DRAIN_CHECK, |_, _, _| watch_tower::drain_check()), (watch_tower::LOG, |_, _, s| watch_tower::log(&s.input))], keys: &[watch_tower::KEYS], build: |_| watch_tower::WatchTower::pinned().registration() },
];

/// The family's rows, written from their contracts and not yet ported; see `tasks`.
pub static PENDING: &[Task] = &[
];
