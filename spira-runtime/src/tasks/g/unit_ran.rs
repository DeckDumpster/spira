//! Source `unit.ran`: a unit's run ended. Replaces `unit-run watch`
//! (`spira-unit-run-dbus-prod.service`, `Type=simple`). Pattern P3, and the completion channel
//! for every P2 task: each `unit.start` is finished by the `unit.ran` this source submits.
//! Contract: `wiki/projects/spira/designs/tasks/source-unit-ran.md`. The real read is
//! `tasks::g::sources::units`: the host registers `sources::units::over` on the same unit manager
//! its `unit.*` effects run on; [`UnitRanDbus`] is the standalone form with its own D-Bus
//! connection. Under virtual time a `ScriptedSource` (or `runtime::sources::UnitRanSource::scripted`)
//! stands in, and this source is refused.
//!
//! # Tick
//!
//! No timer. One D-Bus connection to the user manager with the `PropertiesChanged` and
//! `NameOwnerChanged` match rules plus `Subscribe`, re-subscribed when the manager re-execs; or,
//! off systemd, the child-process backend (`runtime::units::ProcessUnits`).
//!
//! # Observe
//!
//! Off the wire, folded per unit object; on `inactive|failed` the unit is read back so the record
//! carries the manager's final values. Startup catch-up from the latest `ended_at` already stored.
//!
//! # Decide
//!
//! One submit per invocation, deduped on its id. The event is the runtime's own:
//! [`UNIT_RAN`] with body [`UnitRan`] `{handle, exit, duration_ms}` on [`UNITS_STREAM`]; a P2 task
//! recognises its own run by `UnitRan::key_for(task)` over the unit name rule
//! `spira-run-<task>-<key>.service` (`runtime::units::unit_name`).
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | submit `unit.ran` | — | — | `unit.ran/<unit>/<at>` | a non-run unit; an invocation already seen |
//! | `unit-ran.store_row` | store | 2 s | invocation id | — ; a backlog of 20 000 while the store refuses |
//! | `unit-ran.metrics` | store | 2 s | — (counter) | — |
//! | `unit-ran.ensure_accounting` (boot only) | host | 10 s | drop-in content | io controller not delegated: a note |
//!
//! # Silence
//!
//! Signals that open or close nothing submit nothing; a manager with no run ending gives zero
//! events.
//!
//! # Replaces
//!
//! The unit and its ExecStartPre; children `systemctl --user show` per new unit and
//! `systemctl --user daemon-reexec` at boot. Pinned config: [`KEYS`].
//!
//! # Reverse
//!
//! Release rollback restores the unit; both can run at once during a parity check (dedupe on the
//! invocation id).

use runtime::host::Submit;
use runtime::sources::Source;
pub use runtime::sources::UNITS_STREAM;
pub use runtime::units::{UnitRan, UNIT_RAN};
use tokio::sync::mpsc;

/// The source's registered name.
pub const NAME: &str = "unit.ran";
/// What the real source reads.
pub const BACKEND: &str = "systemd-dbus";

pub const KEYS: &[&str] = &["SPIRA_INSTANCE", "SPIRA_METRICS_ENDPOINT"];

/// The real source: the user manager's D-Bus signals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnitRanDbus;

impl Source for UnitRanDbus {
    fn name(&self) -> &str {
        NAME
    }

    fn run(self: Box<Self>, tx: mpsc::Sender<Submit>) {
        super::sources::units::run_dbus(tx)
    }
}
