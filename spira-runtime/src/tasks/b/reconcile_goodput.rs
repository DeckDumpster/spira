//! # reconcile.goodput — contract
//!
//! Family B of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/reconcile-goodput.md`;
//! its Acceptance section is the test file `tests/task_b_reconcile_goodput.rs`.
//!
//! ## Tick
//!
//! - Cadence 60 s (timer `OnUnitActiveSec=60s`, `OnBootSec=2min`, `AccuracySec=5s`). Boot tick at +120 s.
//! - Woken also by `section.published` for any section (unverified: no such event exists today; the
//!   store's `watch` long-poll of S7b would supply it). Without it, cadence alone.
//! - Single-flight: one pass in flight per task. The store-side lock `locks/reconciler`
//!   (`desired-state/src/intent.rs:17`) stays as the fencing token every write carries; in-process
//!   single-flight does not replace it, because round-controller's intents also fence on it
//!   (unverified: `round-controller/src/intents/tests.rs` uses the engine).
//! - No `world guard`: the unit runs on the observability plane (`systemd/spira-reconciler-goodput.service`
//!   `[X-Spira] Plane=observability`). A halt is an input, not a skip (see Observe).
//!
//! ## Observe
//!
//! Probe `reconcile.goodput.observe` returns:
//!
//! ```text
//! Snapshot { halted: bool, section_snapshot: Value /* spira-lc section snapshot */, lock: LockAnswer }
//! ```
//!
//! - `section snapshot` — store verb (`reconciler-engine/src/goodput_pass.rs:223`), parsed by
//!   `assemble::Snapshot::parse` (`reconciler-engine/src/assemble.rs:52`). Holds sections, freshness,
//!   previous pass record, intents. No re-derivation: the ranking reads only section distances.
//! - `world.halted` — today `run_dir/world.halted` existence (`reconciler/src/goodput_pass.rs:58`),
//!   same stamp `world guard` reads (`spira-world/src/guard.rs:11`). A halt makes the pass
//!   `Mode::Halted`: it records and judges nothing, it does not skip (`goodput_pass.rs:147,242`).
//! - Lock acquisition is **not** an observe: it is the first act (below). The probe reads; it never
//!   takes the lock.
//!
//! ## Decide
//!
//! `decide(carry, snapshot, cfg) -> (Carry, Plan)`: `assemble` (`assemble.rs:181`) → `interim_ranking`
//! (`goodput_pass.rs:69`) → `select` (`reconciler-engine/src/select.rs:567`). Already pure; it reads
//! time only from `now_ms` passed in. Output: judgements, steps, would-steps, refusals, the
//! `PassRecord` (`goodput_pass.rs:250-268`).
//!
//! - Carry: the last pass record's identity `{n, list_rev, gaps, steps, judged, refused}` digest, so
//!   an unchanged pass is recognised (see Silence). Mode and binding come from the snapshot.
//! - Config (pinned, S10): `SPIRA_RECONCILER_PASS_TIMEOUT_SECS` (default 50,
//!   `spira/conf.d/SPIRA_RECONCILER_PASS_TIMEOUT_SECS`; lock TTL and pass deadline,
//!   `reconciler/src/goodput_pass.rs:56`), `SPIRA_RECONCILER_SHADOW_KINDS` (default
//!   `RoundController Lifecycle Alarms`, `goodput_pass.rs:57`; already in `spira-runtime/src/settings.rs:113`).
//! - Fixtures: `reconciler-engine/src/select/tests.rs` (9 tests), `assemble/tests.rs`,
//!   `spira-lc/src/sections/tests/goodput_pass.rs` (the engine on a rig store).
//!
//! ## Act
//!
//! The pass is a short machine on one lock: acquire → judge* → (put → get)* → record → release. Each
//! leg is an effect; the next leg is decided on the previous outcome, so no effect blocks the loop.
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `reconcile.goodput.lock` (`lock acquire reconciler --ttl T --holder H`, `goodput_pass.rs:197`) | store | 5 s | holder = task invocation id (`reconciler/src/goodput_pass.rs:44`) | `locked` → `reconcile.goodput.standby` |
//! | `reconcile.goodput.standby` (`pass standby --holder H`, `:202`) | store | 5 s | holder | store error |
//! | `reconcile.goodput.judge` (`intent judge <key> --token`, `:155`) | store | 5 s | intent key + token | halted, shadowed kind, deadline passed (`:147`) |
//! | `reconcile.goodput.put` (`intent put <key> --token --json`, `:177`) then read-back `intent get` (`:183`) | store | 5 s | intent key + intent id | deadline (`:175`); store refusal code; `no-read-back` (`:186`) |
//! | `reconcile.goodput.record` (`pass record --token --json`, `:271`) | store | 5 s | pass n + token | deadline passed (`:270`) |
//! | `reconcile.goodput.release` (`lock release --token --holder`, `:211`) | store | 5 s | token | none; always attempted once the token was read |
//!
//! - Pass deadline: `TIMEOUT_SECS` from the lock grant, on the injected clock (`goodput_pass.rs:136`).
//! - Metrics (`reconciler/src/goodput_pass.rs:62-86`) become wide-event fields on the record outcome
//!   (`ctx.event().set`), not an effect.
//!
//! ## Silence
//!
//! Idle means: no judgement due, no step planned, no refusal, and the gaps equal the carried record.
//! Today every pass writes a lock pair and a pass record each minute, idle or not. Proposed: an idle
//! tick emits the probe and nothing else. **Open question**: alarms' dead-man reads the pass
//! record's age (`incident/src/alarms_pass.rs:379`, `SPIRA_RECONCILER_DEADMAN_SECS`=180); an idle
//! pass that stops recording would trip it. Either the record is kept as a heartbeat at
//! ≤ deadman/2 (every 90 s idle), or the dead-man reads `scheduler status` (S13) instead. Default:
//! record at most every 90 s when idle; record at once on any change.
//!
//! ## Replaces
//!
//! - `spira-reconciler-goodput-prod.service` + `.timer`; `bin/reconciler goodput-pass`
//!   (`reconciler/src/main.rs:17`). `reconciler arm-check` stays a CLI.
//! - Log `run/reconciler-goodput.log` (service template `StandardOutput=append:`).
//! - Children: `spira-lc` × (4 + judgements + 2·steps) per pass via `bounded`
//!   (`reconciler/src/goodput_pass.rs:28`) → `LcClient` (S7a).
//! - Keys pinned: `SPIRA_RECONCILER_PASS_TIMEOUT_SECS`, `SPIRA_RECONCILER_SHADOW_KINDS`,
//!   `SPIRA_LC_PASSWORD_FILE` (already in `spira-runtime/src/main.rs:42`), `INVOCATION_ID` (dropped:
//!   the holder becomes `reconcile.goodput/<tick event id>`).
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Without a release: set the task to `Observe` (S4); its
//! writes are journaled as `effect.shadowed`. Re-enabling the old timer while the task is in Act is
//! safe only because the store lock fences both (the loser stands by).
//!

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "reconcile.goodput";
pub const STREAM: &str = "task.reconcile.goodput";
pub const TICK: EventKindTag = "reconcile.goodput.tick";
pub const STATE: EventKindTag = "reconcile.goodput.state";

pub const OBSERVE: &str = "reconcile.goodput.observe";
pub const LOCK: &str = "reconcile.goodput.lock";
pub const STANDBY: &str = "reconcile.goodput.standby";
pub const JUDGE: &str = "reconcile.goodput.judge";
pub const PUT: &str = "reconcile.goodput.put";
pub const RECORD: &str = "reconcile.goodput.record";
pub const RELEASE: &str = "reconcile.goodput.release";
pub const EFFECTS: &[&str] = &[OBSERVE, LOCK, STANDBY, JUDGE, PUT, RECORD, RELEASE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_RECONCILE_GOODPUT_SECS";
pub const PASS_TIMEOUT_KEY: &str = "SPIRA_RECONCILER_PASS_TIMEOUT_SECS";
pub const SHADOW_KINDS_KEY: &str = "SPIRA_RECONCILER_SHADOW_KINDS";
/// The idle heartbeat record is kept at most every deadman/2 (Silence, default).
pub const DEADMAN_KEY: &str = "SPIRA_RECONCILER_DEADMAN_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, PASS_TIMEOUT_KEY, SHADOW_KINDS_KEY, DEADMAN_KEY, "SPIRA_LC_PASSWORD_FILE"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 5_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

/// The store's lock answer as the probe read it; the probe never takes the lock.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockAnswer {
    pub held_by: Option<String>,
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub halted: bool,
    /// The `spira-lc section snapshot`, parsed by `assemble::Snapshot::parse`.
    pub section_snapshot: serde_json::Value,
    pub lock: LockAnswer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Locking,
    Standby,
    Judging,
    Stepping,
    Recording,
    Releasing,
}

/// `state × event → state` (T3). An effect asked in `stepping` once the pass deadline has passed
/// is refused naming `deadline`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Idle, "observed:work", Phase::Locking),
    (Phase::Locking, "lock:granted", Phase::Judging),
    (Phase::Locking, "lock:locked", Phase::Standby),
    (Phase::Standby, "standby:done", Phase::Idle),
    (Phase::Judging, "judge:done", Phase::Stepping),
    (Phase::Stepping, "put:done", Phase::Recording),
    (Phase::Recording, "record:done", Phase::Releasing),
    (Phase::Releasing, "release:done", Phase::Idle),
];

/// The identity of the last pass record, so an unchanged pass is recognised.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PassDigest {
    pub n: u64,
    pub list_rev: u64,
    pub gaps: Vec<String>,
    pub steps: Vec<String>,
    pub judged: Vec<String>,
    pub refused: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub last: PassDigest,
    pub recorded_at: Option<u64>,
    /// The lock grant's token and the pass deadline it set, while a pass is in flight.
    pub token: Option<String>,
    pub deadline_at: Option<u64>,
}

pub struct ReconcileGoodput {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `assemble` → `interim_ranking` → `select`, one leg per outcome.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("reconcile.goodput: not ported yet")
}

super::hosted!(ReconcileGoodput);
