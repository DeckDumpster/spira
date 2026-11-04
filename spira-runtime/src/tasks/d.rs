//! Family D, watcher and alarm timers, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`; each task is one module under `d/`, its doc comment the
//! task's contract (`wiki/projects/spira/designs/tasks/<task>.md`).
//!
//! Every row's runners are stubs until the task is ported, so every row waits in [`PENDING`];
//! porting one moves it to [`TASKS`]. `inbox.append` is served by
//! `tasks::SHARED`, and `unit.start` / `unit.stop` by family F's row, so neither appears here.
//!
//! A calendar row's `cadence_ms` is its first elapse on a clock that starts at 0, which is when
//! its boot tick arms the next one; the row table has no calendar cadence of its own yet.

use super::Task;

pub mod acceptance;
pub mod cert_full;
pub mod cert_sample;
pub mod pattern;
pub mod watch_mend;
pub mod watch_perf;
pub mod watch_perf_happy;
pub mod watch_refusals;
pub mod watch_runtime;
pub mod watch_skew;
pub mod watch_sop_lint;
pub mod watch_statute_audit;
pub mod watch_tower;

/// A runner not yet ported: the effect name is the panic message.
macro_rules! stub {
    ($name:expr) => {
        ($name, |_, _, spec| unimplemented!("{}: not ported yet", spec.name))
    };
}
pub(crate) use stub;

pub static TASKS: &[Task] = &[];

/// The family's rows, written from their contracts and not yet ported; see `tasks`.
pub static PENDING: &[Task] = &[
    Task { name: watch_perf::NAME, boot: watch_perf::WatchPerf::boot, cadence_ms: |_| watch_perf::CADENCE_MS, effects: watch_perf::EFFECTS, runners: &[stub!(watch_perf::OBSERVE), stub!(watch_perf::LOG), stub!(watch_perf::SLOW_SAMPLE), stub!(watch_perf::METRICS)], keys: &[watch_perf::KEYS], build: |_| watch_perf::WatchPerf::pinned().registration() },
    Task { name: watch_perf_happy::NAME, boot: watch_perf_happy::WatchPerfHappy::boot, cadence_ms: |_| watch_perf_happy::CADENCE_MS, effects: watch_perf_happy::EFFECTS, runners: &[stub!(watch_perf_happy::OBSERVE), stub!(watch_perf_happy::CHECKOUT), stub!(watch_perf_happy::TEARDOWN), stub!(watch_perf_happy::LOG), stub!(watch_perf_happy::HISTORY)], keys: &[watch_perf_happy::KEYS], build: |_| watch_perf_happy::WatchPerfHappy::pinned().registration() },
    Task { name: watch_skew::NAME, boot: watch_skew::WatchSkew::boot, cadence_ms: |_| watch_skew::CADENCE_MS, effects: watch_skew::EFFECTS, runners: &[stub!(watch_skew::OBSERVE), stub!(watch_skew::ESCALATE), stub!(watch_skew::LOG), stub!(watch_skew::REFRESH)], keys: &[watch_skew::KEYS], build: |_| watch_skew::WatchSkew::pinned().registration() },
    Task { name: watch_refusals::NAME, boot: watch_refusals::WatchRefusals::boot, cadence_ms: |_| watch_refusals::CADENCE_MS, effects: watch_refusals::EFFECTS, runners: &[stub!(watch_refusals::OBSERVE), stub!(watch_refusals::FILE)], keys: &[watch_refusals::KEYS], build: |_| watch_refusals::WatchRefusals::pinned().registration() },
    Task { name: watch_runtime::NAME, boot: watch_runtime::WatchRuntime::boot, cadence_ms: |_| watch_runtime::CADENCE_MS, effects: watch_runtime::EFFECTS, runners: &[stub!(watch_runtime::OBSERVE), stub!(watch_runtime::FILE_BUMP), stub!(watch_runtime::METRICS), stub!(watch_runtime::LOG)], keys: &[watch_runtime::KEYS], build: |_| watch_runtime::WatchRuntime::pinned().registration() },
    Task { name: watch_tower::NAME, boot: watch_tower::WatchTower::boot, cadence_ms: |_| watch_tower::CADENCE_MS, effects: watch_tower::EFFECTS, runners: &[stub!(watch_tower::OBSERVE), stub!(watch_tower::PROMPT), stub!(watch_tower::ALARM), stub!(watch_tower::DRAIN_CHECK), stub!(watch_tower::LOG)], keys: &[watch_tower::KEYS], build: |_| watch_tower::WatchTower::pinned().registration() },
    Task { name: watch_mend::NAME, boot: watch_mend::WatchMend::boot, cadence_ms: |_| watch_mend::CADENCE_MS, effects: watch_mend::EFFECTS, runners: &[stub!(watch_mend::OBSERVE), stub!(watch_mend::LOG_APPEND), stub!(watch_mend::LIFT_STOP)], keys: &[watch_mend::KEYS], build: |_| watch_mend::WatchMend::pinned().registration() },
    Task { name: watch_sop_lint::NAME, boot: watch_sop_lint::WatchSopLint::boot, cadence_ms: |_| watch_sop_lint::CADENCE_MS, effects: watch_sop_lint::EFFECTS, runners: &[stub!(watch_sop_lint::OBSERVE), stub!(watch_sop_lint::FILE_INCIDENT)], keys: &[watch_sop_lint::KEYS], build: |_| watch_sop_lint::WatchSopLint::pinned().registration() },
    Task { name: watch_statute_audit::NAME, boot: watch_statute_audit::WatchStatuteAudit::boot, cadence_ms: |_| watch_statute_audit::next_tick(0), effects: watch_statute_audit::EFFECTS, runners: &[stub!(watch_statute_audit::OBSERVE), stub!(watch_statute_audit::FILE_BEAD)], keys: &[watch_statute_audit::KEYS], build: |_| watch_statute_audit::WatchStatuteAudit::pinned().registration() },
    Task { name: cert_full::NAME, boot: cert_full::CertFull::boot, cadence_ms: |_| cert_full::next_tick(0), effects: cert_full::EFFECTS, runners: &[stub!(cert_full::OBSERVE), stub!(cert_full::CHECKOUT), stub!(cert_full::FILE_BEAD), stub!(cert_full::RECORD)], keys: &[cert_full::KEYS], build: |_| cert_full::CertFull::pinned().registration() },
    Task { name: cert_sample::NAME, boot: cert_sample::CertSample::boot, cadence_ms: |_| cert_sample::next_tick(0), effects: cert_sample::EFFECTS, runners: &[stub!(cert_sample::OBSERVE), stub!(cert_sample::BRANCH), stub!(cert_sample::FILE_BEAD), stub!(cert_sample::RECORD)], keys: &[cert_sample::KEYS], build: |_| cert_sample::CertSample::pinned().registration() },
];
