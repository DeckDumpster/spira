//! Family E, watchd daemons, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`; each task is one module under `e/`, its doc comment the
//! task's contract (`wiki/projects/spira/designs/tasks/<task>.md`), written in the shapes of
//! `tasks::d::pattern` and tested through `tasks::d::acceptance`.
//!
//! Every row's runners are stubs until the task is ported, so every row waits in [`PENDING`];
//! porting one moves it to [`TASKS`]. `inbox.append` and the `unit.*` effects are served by
//! `tasks::SHARED`, so none appears on a row here.

use super::d::stub;
use super::Task;

pub mod inbox_notify;
pub mod incident_probe;
pub mod watch_ami_stale;
pub mod watch_backup_age;
pub mod watch_ci_spill;
pub mod watch_pass_rate;
pub mod watch_spill_key;
pub mod watch_testenv_cap;
pub mod watch_warm_full;

pub static TASKS: &[Task] = &[];

/// The family's rows, written from their contracts and not yet ported; see `tasks`.
pub static PENDING: &[Task] = &[
    Task { name: inbox_notify::NAME, boot: inbox_notify::InboxNotify::boot, cadence_ms: |_| inbox_notify::CADENCE_MS, effects: inbox_notify::EFFECTS, runners: &[stub!(inbox_notify::OBSERVE), stub!(inbox_notify::ASK), stub!(inbox_notify::SOP_MATCH), stub!(inbox_notify::SOP_APPLIED), stub!(inbox_notify::CONCIERGE_NOTE), stub!(inbox_notify::MAIL_HEALTH)], keys: &[inbox_notify::KEYS], build: |_| inbox_notify::InboxNotify::pinned().registration() },
    Task { name: incident_probe::NAME, boot: incident_probe::IncidentProbe::boot, cadence_ms: |_| incident_probe::CADENCE_MS, effects: incident_probe::EFFECTS, runners: &[stub!(incident_probe::OBSERVE), stub!(incident_probe::SEND)], keys: &[incident_probe::KEYS], build: |_| incident_probe::IncidentProbe::pinned().registration() },
    Task { name: watch_ci_spill::NAME, boot: watch_ci_spill::WatchCiSpill::boot, cadence_ms: |_| watch_ci_spill::CADENCE_MS, effects: watch_ci_spill::EFFECTS, runners: &[stub!(watch_ci_spill::OBSERVE)], keys: &[watch_ci_spill::KEYS], build: |_| watch_ci_spill::WatchCiSpill::pinned().registration() },
    Task { name: watch_backup_age::NAME, boot: watch_backup_age::WatchBackupAge::boot, cadence_ms: |_| watch_backup_age::CADENCE_MS, effects: watch_backup_age::EFFECTS, runners: &[stub!(watch_backup_age::OBSERVE), stub!(watch_backup_age::MAIL)], keys: &[watch_backup_age::KEYS], build: |_| watch_backup_age::WatchBackupAge::pinned().registration() },
    Task { name: watch_pass_rate::NAME, boot: watch_pass_rate::WatchPassRate::boot, cadence_ms: |_| watch_pass_rate::CADENCE_MS, effects: watch_pass_rate::EFFECTS, runners: &[stub!(watch_pass_rate::OBSERVE), stub!(watch_pass_rate::MAIL)], keys: &[watch_pass_rate::KEYS], build: |_| watch_pass_rate::WatchPassRate::pinned().registration() },
    Task { name: watch_ami_stale::NAME, boot: watch_ami_stale::WatchAmiStale::boot, cadence_ms: |_| watch_ami_stale::CADENCE_MS, effects: watch_ami_stale::EFFECTS, runners: &[stub!(watch_ami_stale::OBSERVE)], keys: &[watch_ami_stale::KEYS], build: |_| watch_ami_stale::WatchAmiStale::pinned().registration() },
    Task { name: watch_spill_key::NAME, boot: watch_spill_key::WatchSpillKey::boot, cadence_ms: |_| watch_spill_key::CADENCE_MS, effects: watch_spill_key::EFFECTS, runners: &[stub!(watch_spill_key::OBSERVE)], keys: &[watch_spill_key::KEYS], build: |_| watch_spill_key::WatchSpillKey::pinned().registration() },
    Task { name: watch_testenv_cap::NAME, boot: watch_testenv_cap::WatchTestenvCap::boot, cadence_ms: |_| watch_testenv_cap::CADENCE_MS, effects: watch_testenv_cap::EFFECTS, runners: &[stub!(watch_testenv_cap::OBSERVE), stub!(watch_testenv_cap::KILL), stub!(watch_testenv_cap::METRIC)], keys: &[watch_testenv_cap::KEYS], build: |_| watch_testenv_cap::WatchTestenvCap::pinned().registration() },
    Task { name: watch_warm_full::NAME, boot: watch_warm_full::WatchWarmFull::boot, cadence_ms: |_| watch_warm_full::CADENCE_MS, effects: watch_warm_full::EFFECTS, runners: &[stub!(watch_warm_full::OBSERVE), stub!(watch_warm_full::REAP)], keys: &[watch_warm_full::KEYS], build: |_| watch_warm_full::WatchWarmFull::pinned().registration() },
];
