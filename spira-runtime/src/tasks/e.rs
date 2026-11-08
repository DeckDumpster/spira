//! Family E, watchd daemons, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`; each task is one module under `e/`, its doc comment the
//! task's contract (`wiki/projects/spira/designs/tasks/<task>.md`), written in the shapes of
//! `tasks::d::pattern` and tested through `tasks::d::acceptance`.
//!
//! Every row is ported and lives in [`TASKS`]; [`PENDING`] is empty. `inbox.append` and the
//! `unit.*` effects are served by `tasks::SHARED`, so none appears on a row here.

use super::Task;

pub mod alert;
pub mod inbox_notify;
pub mod incident_probe;
pub mod watch_ami_stale;
pub mod watch_backup_age;
pub mod watch_ci_spill;
pub mod watch_dwarf_usage;
pub mod watch_pass_rate;
pub mod watch_spill_key;
pub mod watch_testenv_cap;
pub mod watch_throughput;
pub mod watch_transitions;
pub mod watch_warm_full;

pub static TASKS: &[Task] = &[
    Task { name: inbox_notify::NAME, plane: runtime::Plane::Work, signal: &[inbox_notify::ASK, inbox_notify::CONCIERGE_NOTE], boot: inbox_notify::InboxNotify::boot, cadence_ms: |_| inbox_notify::CADENCE_MS, effects: inbox_notify::EFFECTS, runners: &[(inbox_notify::OBSERVE, |e, _, _| inbox_notify::observe(e.units.as_deref())), (inbox_notify::ASK, |_, _, s| inbox_notify::ask(&s.input)), (inbox_notify::SOP_MATCH, |_, _, s| inbox_notify::sop_match(&s.input)), (inbox_notify::SOP_APPLIED, |_, _, s| inbox_notify::sop_applied(&s.input)), (inbox_notify::CONCIERGE_NOTE, |_, _, s| inbox_notify::concierge_note(&s.input)), (inbox_notify::MAIL_HEALTH, |_, _, _| inbox_notify::mail_health())], keys: &[inbox_notify::KEYS], build: |_| inbox_notify::InboxNotify::pinned().registration() },
    Task { name: watch_ami_stale::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_ami_stale::WatchAmiStale::boot, cadence_ms: |_| watch_ami_stale::CADENCE_MS, effects: watch_ami_stale::EFFECTS, runners: &[(watch_ami_stale::OBSERVE, |_, _, _| watch_ami_stale::observe())], keys: &[watch_ami_stale::KEYS], build: |_| watch_ami_stale::WatchAmiStale::pinned().registration() },
    Task { name: incident_probe::NAME, plane: runtime::Plane::Signal, signal: &[], boot: incident_probe::IncidentProbe::boot, cadence_ms: |_| incident_probe::CADENCE_MS, effects: incident_probe::EFFECTS, runners: &[(incident_probe::OBSERVE, |_, _, _| incident_probe::observe()), (incident_probe::SEND, |_, _, s| incident_probe::send(&s.input))], keys: &[incident_probe::KEYS], build: |_| incident_probe::IncidentProbe::pinned().registration() },
    Task { name: watch_backup_age::NAME, plane: runtime::Plane::Work, signal: &[watch_backup_age::MAIL], boot: watch_backup_age::WatchBackupAge::boot, cadence_ms: |_| watch_backup_age::CADENCE_MS, effects: watch_backup_age::EFFECTS, runners: &[(watch_backup_age::OBSERVE, |_, _, _| watch_backup_age::observe()), (watch_backup_age::MAIL, |e, _, s| alert::send(e, &s.input))], keys: &[watch_backup_age::KEYS], build: |_| watch_backup_age::WatchBackupAge::pinned().registration() },
    Task { name: watch_dwarf_usage::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_dwarf_usage::WatchDwarfUsage::boot, cadence_ms: |s| super::f::cadence(s, watch_dwarf_usage::CADENCE_KEY), effects: watch_dwarf_usage::EFFECTS, runners: &[(watch_dwarf_usage::SAMPLE, watch_dwarf_usage::run_sample)], keys: &[watch_dwarf_usage::KEYS], build: |s| super::f::refuse(watch_dwarf_usage::NAME, watch_dwarf_usage::WatchDwarfUsage::from_settings(s)).registration() },
    Task { name: watch_ci_spill::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_ci_spill::WatchCiSpill::boot, cadence_ms: |_| watch_ci_spill::CADENCE_MS, effects: watch_ci_spill::EFFECTS, runners: &[(watch_ci_spill::OBSERVE, |e, _, s| watch_ci_spill::observe(e, &s.input))], keys: &[watch_ci_spill::KEYS], build: |_| watch_ci_spill::WatchCiSpill::pinned().registration() },
    Task { name: watch_pass_rate::NAME, plane: runtime::Plane::Work, signal: &[watch_pass_rate::MAIL], boot: watch_pass_rate::WatchPassRate::boot, cadence_ms: |_| watch_pass_rate::CADENCE_MS, effects: watch_pass_rate::EFFECTS, runners: &[(watch_pass_rate::OBSERVE, |_, _, _| watch_pass_rate::observe()), (watch_pass_rate::MAIL, |e, _, s| alert::send(e, &s.input))], keys: &[watch_pass_rate::KEYS], build: |_| watch_pass_rate::WatchPassRate::pinned().registration() },
    Task { name: watch_spill_key::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_spill_key::WatchSpillKey::boot, cadence_ms: |_| watch_spill_key::CADENCE_MS, effects: watch_spill_key::EFFECTS, runners: &[(watch_spill_key::OBSERVE, |_, _, s| watch_spill_key::observe(&s.input))], keys: &[watch_spill_key::KEYS], build: |_| watch_spill_key::WatchSpillKey::pinned().registration() },
    Task { name: watch_testenv_cap::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_testenv_cap::WatchTestenvCap::boot, cadence_ms: |_| watch_testenv_cap::CADENCE_MS, effects: watch_testenv_cap::EFFECTS, runners: &[(watch_testenv_cap::OBSERVE, |_, _, _| watch_testenv_cap::observe()), (watch_testenv_cap::KILL, |_, _, s| watch_testenv_cap::kill(&s.input)), (watch_testenv_cap::METRIC, |_, _, s| watch_testenv_cap::metric(&s.input))], keys: &[watch_testenv_cap::KEYS], build: |_| watch_testenv_cap::WatchTestenvCap::pinned().registration() },
    Task { name: watch_throughput::NAME, plane: runtime::Plane::Work, signal: &[watch_throughput::STAMP], boot: watch_throughput::WatchThroughput::boot, cadence_ms: |_| watch_throughput::CADENCE_MS, effects: watch_throughput::EFFECTS, runners: &[(watch_throughput::OBSERVE, |e, _, s| e.world.throughput_snapshot(&s.input)), (watch_throughput::STAMP, |e, _, s| e.world.halt_stamp(&s.input))], keys: &[watch_throughput::KEYS], build: |_| watch_throughput::WatchThroughput::pinned().registration() },
    Task { name: watch_transitions::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_transitions::WatchTransitions::boot, cadence_ms: |_| watch_transitions::cadence_ms(), effects: watch_transitions::EFFECTS, runners: &[(watch_transitions::OBSERVE, |e, _, s| e.world.transitions_snapshot(&s.input))], keys: &[watch_transitions::KEYS], build: |_| watch_transitions::WatchTransitions::pinned().registration() },
    Task { name: watch_warm_full::NAME, plane: runtime::Plane::Work, signal: &[], boot: watch_warm_full::WatchWarmFull::boot, cadence_ms: |_| watch_warm_full::CADENCE_MS, effects: watch_warm_full::EFFECTS, runners: &[(watch_warm_full::OBSERVE, |_, _, _| watch_warm_full::observe()), (watch_warm_full::REAP, |_, _, s| watch_warm_full::reap(&s.input))], keys: &[watch_warm_full::KEYS], build: |_| watch_warm_full::WatchWarmFull::pinned().registration() },
];

/// Every row of the family is ported; see `tasks`.
pub static PENDING: &[Task] = &[];
