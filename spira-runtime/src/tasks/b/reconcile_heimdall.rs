//! # reconcile.heimdall — contract
//!
//! Family B of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/reconcile-heimdall.md`;
//! its Acceptance section is the test file `tests/task_b_reconcile_heimdall.rs`.
//!
//! **Scope (rulings of 2026-11-03, S15).** Only heimdall's in-scheduler checks move here. The
//! outside watch, the one that sees the scheduler itself wedged or absent, is the watchdog
//! watcher on the platform's timer, not this task. So the task reads the sentinel through
//! `scheduler status` and never forks `systemctl`; it does not judge its own scheduler.
//!
//! ## Tick
//!
//! - `Timer`, every 2 min. The unit uses `OnUnitActiveSec=2min`, `OnBootSec=420`, `AccuracySec=30s` and `TimeoutStartSec=60`.
//! - The first tick is at boot + 420 s. No event wakes it. It runs single-flight.
//!
//! ## Observe
//!
//! `reconcile.heimdall.observe` is a Probe. Its snapshot is the existing `classify::Observation` (`heimdall/src/classify.rs`; built at `main.rs:227-248`). Each field must come from the component that owns it:
//!
//! | Field | Today (`path:line`) | Hosted source |
//! |---|---|---|
//! | `sentinel_log` tail, mtime | file read, 256 KiB (`main.rs:185`, `gather.rs:22`) | the `sentinel.pass` task's own status (S13), not its log |
//! | `sentinel_timer` | `systemctl --user is-enabled/is-active` (`gather.rs:59-66,133`) | `scheduler status` → `sentinel.pass` armed/last-fired (S13) |
//! | `restart_alerts` | `systemctl list-units` + `show NRestarts` (`gather.rs:68-96`, `main.rs:192-203`) | `unit.facts` (S6) for the P6 units that remain, plus per-task effect-failure counts from S13 |
//! | `db_reachable`, `alert_rows` | `spira-lc condition list --owner heimdall`, timeout 10 s (`conditions.rs:70-92`, `main.rs:207-217`) | store verb `condition list` |
//! | `mirror` | stat of `raw/spira-beads/spira.jsonl` (`gather.rs:155`, `main.rs:161,188`) | the `sweep.export_beads` task's last outcome |
//! | `world_halted/draining` | run-dir stamps, with `date -d` forked to parse them (`gather.rs:180-205`) | the `world` status read, with time parsed in-process |
//! | `strands` | `$SPIRA_RUN/strands.json` (`main.rs:190`) | unchanged file read (`unverified` who writes it) |
//!
//! The write probe (`alerts::maintain_probe`, `alerts.rs:66-71`) calls `condition set heimdall-probe … --status False` on every pass. It stays part of observe: "the alert query is also the read probe" (`main.rs:205`).
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (Carry, Vec<Act>)` composes two functions that are already pure:
//!
//! - `judge::classify(&obs)` (`judge.rs:171`)
//! - `reconcile::step_keys(now, confirm=2, clear=2, refresh=3600, db_reachable, firing, prior)` (`reconcile.rs:93`)
//!
//! **Carry** is `heimdall::state::State`: `first_run`, per-key `KeyState{seen, unseen, state, first, flaps, refreshed, bead}`, restart baselines and `probe_id`. Today it is the file `$SPIRA_RUN/heimdall.state` (`main.rs:156,178-182,357`). It becomes a journaled `reconcile.heimdall.state` Emit, written only when it changes.
//!
//! **Fixtures** are the existing tests in `reconcile.rs`, `judge.rs`, `restart.rs` and `spira/test-heimdall.sh`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `reconcile.heimdall.raise` → `condition set <key> --type alert --status True --owner heimdall` (`alerts.rs:42-45`) | store | 10 s (`SPIRA_HEIMDALL_BD_TIMEOUT`, `main.rs:95`) | `heimdall/<key>/raise/<flaps>`, because the store counts flaps (`alerts.rs:40-41`) | any write while `db_reachable=false` (`reconcile.rs:108`) |
//! | `reconcile.heimdall.clear` → `condition clear <key>` (`alerts.rs:48-51`) | store | 10 s | `heimdall/<key>/clear/<flaps>` | a key that is not firing (a no-op success, `alerts.rs:49`) |
//! | `reconcile.heimdall.refresh` → `condition set` on a firing key after 3600 s | store | 10 s | `heimdall/<key>/refresh/<refreshed>` | a key closed by hand (`main.rs:318-320`) |
//! | `reconcile.heimdall.fallback` → write or remove `$SPIRA_RUN/heimdall.alerts.json` (`fallback.rs`, `main.rs:342-354`) | host | 2 s | the content hash of the doc | nothing |
//! | `reconcile.heimdall.heartbeat` → `$SPIRA_RUN/heimdall.status` (`heartbeat.rs`, `main.rs:363-364`) | host | 2 s | `at` | nothing |
//!
//! Today a timeout (exit 124) is classed as *saturated*, not unreachable (`conditions.rs:61-67`). An effect `timed_out` keeps that meaning.
//!
//! ## Silence
//!
//! A tick where nothing is firing and every counter is unchanged emits no store write.
//!
//! That fails two ways today:
//!
//! 1. The write probe writes on every pass, by design.
//! 2. `heimdall.status` is rewritten every pass and is read by `cockpit-collect/src/probes/mod.rs` (`unverified` line).
//!
//! The ruling: the probe stays an unjournaled Probe. The heartbeat moves into `scheduler status` (S13), and the file is dropped once the pane reads S13. Until then the heartbeat is the one allowed per-tick host write, and it is not journaled.
//!
//! ## Replaces
//!
//! **Units and logs:** `spira-heimdall-prod.{service,timer}`, `StandardOutput=append:$SPIRA_RUN/heimdall.log`.
//!
//! **Children per pass** (catalogue: "(param runner)"):
//!
//! - `systemctl` ×4+: is-enabled, is-active, list-units, show
//! - `spira-lc condition` ×2+: list, the probe set, plus one per raise or clear
//! - `date` ×1–N (`gather.rs:183`, `util.rs:147`)
//!
//! **Keys pinned:**
//!
//! - registered: `SPIRA_RUN`, `SPIRA_DB`, `SPIRA_WIKI`, `SPIRA_EXPORTER`, `SPIRA_INSTANCE`, `SPIRA_TZ`, `SPIRA_HEIMDALL_RESTARTS`, `SPIRA_HEIMDALL_RESTART_WINDOW`
//! - ad hoc: `SPIRA_HEIMDALL_{BD_TIMEOUT,FALLBACK,SENTINEL_LOG,STRANDS,MIRROR,TAIL_BYTES,CONFIRM,CLEAR,REFRESH,PASS_STALE,STARVE_PASSES,MIRROR_STALE,GHOST_STALE}`, `SPIRA_SYSTEMCTL` (`main.rs:95-175`)
//!
//! All of these join `Settings::KEYS` (S10).
//!
//! ## Reverse
//!
//! Release rollback restores the unit. With the task in `Observe` mode, raise and clear are shadowed. The probe and heartbeat keep running because they are observe. Conditions that are already raised stay raised until the task acts again.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "reconcile.heimdall";
pub const STREAM: &str = "task.reconcile.heimdall";
pub const TICK: EventKindTag = "reconcile.heimdall.tick";
pub const STATE: EventKindTag = "reconcile.heimdall.state";

pub const OBSERVE: &str = "reconcile.heimdall.observe";
pub const RAISE: &str = "reconcile.heimdall.raise";
pub const CLEAR: &str = "reconcile.heimdall.clear";
pub const REFRESH: &str = "reconcile.heimdall.refresh";
pub const FALLBACK: &str = "reconcile.heimdall.fallback";
pub const HEARTBEAT: &str = "reconcile.heimdall.heartbeat";
pub const EFFECTS: &[&str] = &[OBSERVE, RAISE, CLEAR, REFRESH, FALLBACK, HEARTBEAT];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_RECONCILE_HEIMDALL_SECS";
/// The in-scheduler checks' keys only. `SPIRA_SYSTEMCTL`, `SPIRA_HEIMDALL_SENTINEL_LOG` and
/// `SPIRA_HEIMDALL_TAIL_BYTES` are not read: the sentinel is a sibling task read through
/// `scheduler status` (S13), and the scheduler itself is watched from outside by the watchdog
/// (S15), not by this task.
pub const KEYS: &[&str] = &[
    CADENCE_KEY,
    "SPIRA_RUN",
    "SPIRA_DB",
    "SPIRA_WIKI",
    "SPIRA_EXPORTER",
    "SPIRA_INSTANCE",
    "SPIRA_TZ",
    "SPIRA_HEIMDALL_RESTARTS",
    "SPIRA_HEIMDALL_RESTART_WINDOW",
    "SPIRA_HEIMDALL_BD_TIMEOUT",
    "SPIRA_HEIMDALL_FALLBACK",
    "SPIRA_HEIMDALL_STRANDS",
    "SPIRA_HEIMDALL_MIRROR",
    "SPIRA_HEIMDALL_CONFIRM",
    "SPIRA_HEIMDALL_CLEAR",
    "SPIRA_HEIMDALL_REFRESH",
    "SPIRA_HEIMDALL_PASS_STALE",
    "SPIRA_HEIMDALL_STARVE_PASSES",
    "SPIRA_HEIMDALL_MIRROR_STALE",
    "SPIRA_HEIMDALL_GHOST_STALE",
];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 10_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

/// A sibling task as `scheduler status` (S13) reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskStatus {
    pub armed: bool,
    pub last_fired_ms: Option<u64>,
    pub effect_failures: u64,
}

/// `classify::Observation`, each field from the component that owns it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// `sentinel.pass` from `scheduler status`, replacing its log tail and its timer.
    pub sentinel: TaskStatus,
    /// (unit, restarts in the window) from `unit.facts` for the P6 units that remain.
    pub restart_alerts: Vec<(String, u64)>,
    pub db_reachable: bool,
    /// Keys of the conditions the store holds raised with owner heimdall.
    pub alert_rows: Vec<String>,
    /// `sweep.export_beads`' last outcome age, replacing the mirror file's mtime.
    pub mirror_age_secs: Option<u64>,
    pub world_halted: bool,
    pub world_draining: bool,
    pub strands: Vec<String>,
}

/// Per key (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Quiet,
    Seen,
    Firing,
    Unseen,
    Cleared,
    Refreshed,
}

/// `state × event → state` per key. A raise while the store is unreachable is refused, naming
/// `unreachable`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Quiet, "seen", Phase::Seen),
    (Phase::Seen, "seen:confirm", Phase::Firing),
    (Phase::Seen, "unseen", Phase::Quiet),
    (Phase::Firing, "unseen", Phase::Unseen),
    (Phase::Firing, "refresh:due", Phase::Refreshed),
    (Phase::Refreshed, "seen", Phase::Firing),
    (Phase::Unseen, "seen", Phase::Firing),
    (Phase::Unseen, "unseen:clear", Phase::Cleared),
    (Phase::Cleared, "seen", Phase::Seen),
];

/// `heimdall::state::KeyState`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyState {
    pub phase: Phase,
    pub seen: u32,
    pub unseen: u32,
    pub first: Option<u64>,
    pub flaps: u32,
    pub refreshed: Option<u64>,
    pub bead: Option<String>,
}

/// `heimdall::state::State`, journaled only when it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub first_run: bool,
    pub keys: BTreeMap<String, KeyState>,
    pub restart_baselines: BTreeMap<String, u64>,
    pub probe_id: Option<String>,
}

pub struct ReconcileHeimdall {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `judge::classify` then `reconcile::step_keys(now, confirm, clear, refresh, …)`.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("reconcile.heimdall: not ported yet")
}

super::hosted!(ReconcileHeimdall);
