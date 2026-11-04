//! # incident.alarms — contract
//!
//! Family B of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/incident-alarms.md`;
//! its Acceptance section is the test file `tests/task_b_incident_alarms.rs`.
//!
//! ## Tick
//!
//! - Cadence 60 s (`OnUnitActiveSec=60s`, `OnBootSec=90s`, `AccuracySec=10s`). Boot tick at +90 s.
//! - Also woken by `reconcile.goodput`'s record outcome (in-process, same journal), so a page follows
//!   the pass that caused it without waiting a minute.
//! - Single-flight. No `world guard`: observability plane. Its own halt is
//!   `run/world.halted.observability` (`alarms_pass.rs:41,79`), which only sets the dead-man to Waiting.
//!
//! ## Observe
//!
//! Probe `incident.alarms.observe` returns:
//!
//! ```text
//! Snapshot { section_snapshot: Value, pass: Option<PassRecord>, observability_halted: bool,
//!            goodput_armed: bool, attempts: Vec<Attempt>, floor_proposal: Option<String> }
//! ```
//!
//! - `section snapshot` — store verb (`alarms_pass.rs:643`): sections with freshness, the latest
//!   pass record (`:649`), intents (`:506`). Raised conditions are read from sections, never
//!   recomputed (`:350-370`).
//! - `goodput_armed` — today `systemctl is-enabled` on the goodput **timer** (`main.rs:700-703`).
//!   After the move there is no timer: it becomes "task `reconcile.goodput` registered and in Act
//!   mode" from `scheduler status` (S13). This is a semantic change, not a port.
//! - `attempts` — `run/tsd/alarm-attempt.jsonl` (`tsd/src/alarm.rs:108-118`); read for retry and
//!   for the Undelivered condition (`attempts::undelivered`, `alarms_pass.rs:517,565`).
//! - `floor_proposal` — `[floor] proposal` of `reconciler/causal-map.toml` (`main.rs:679,686`),
//!   pinned at boot or read per tick (file is Concierge-edited; per tick, unverified that it matters).
//!
//! ## Decide
//!
//! `decide(carry, snap, cfg) -> (Carry, Vec<Act>)`, composed of pieces that are already pure:
//!
//! - `deadman` (`alarms_pass.rs:372`): Waiting | Fresh | Stale over `SPIRA_RECONCILER_DEADMAN_SECS`
//!   (default 180, `main.rs:682`).
//! - candidates `from_pass` (no-lever, exhausted; `:318`), `raised` (page-on-raise; `:350`),
//!   `deadman_candidate` (`:385`); each through `page::decide` (`incident/src/page.rs:174`) → Send |
//!   Note | Quiet against the page rows.
//! - floor: one standing policy ask while Lifecycle `BelowFloor` is open, per binding condition
//!   (`alarms_pass.rs:454-492`); rows cleared when the condition clears (`:459-462`).
//! - retry-delivery intents: un-acked `Alarms` intents with action `RetryDelivery` (`:505-539`).
//! - the Alarms section status, and whether it is unchanged within 300 s (`:606-619`).
//! - Carry: the page rows (today `run/alarms-pages.tsv`, `:35,423`) and floor rows
//!   (`run/alarms-floor.tsv`, `:36,455`) move into carried state. Only `incident` reads them
//!   (grep of the release).
//! - Fixtures: `spira-lc/src/sections/tests/alarms_pass.rs` (rig with fake Primary/Http), `page.rs` tests.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `incident.alarms.page` (primary `mail send`, then maildir fallback; `alarms_pass.rs:203-233`) | store (primary), host (fallback) | 15 s | `page:<section>/<condition>:<identity>` | invalid mailbox (`:96`); forced mode skips fallback (`:219`) |
//! | `incident.alarms.note` (`mail::bead::note` on the open ask, `:139-144,439`) | store | 10 s | ask id + evidence hash | ask id empty or not a bead id (`:140`) |
//! | `incident.alarms.floor_ask` (policy ask, MOOT-WHEN `incident floor-cleared`, `:487-488`) | store | 15 s | `floor:<binding>` | binding already asked (`:466`) |
//! | `incident.alarms.retry` (re-deliver, then `intent apply` or `intent ack --result`, `:527-533`) | store | 15 s | intent key | intent already acked (`:513`); no recorded message → ack `refused` (`:525`) |
//! | `incident.alarms.attempt` (append to `tsd/alarm-attempt.jsonl`, `alarm.rs:109`) | host | 2 s | (at, caller, kind, reference) | none; failure is logged (`:206`) |
//! | `incident.alarms.knob_init` (`intent knob-init`, `:560`) | store | 5 s | knob key | already initialised (store no-op) |
//! | `incident.alarms.publish` (`section publish-status --expect-rev`, `:632`) | store | 5 s | section resource version | stale rev; unchanged within 300 s is not emitted (`:617`) |
//! | `incident.alarms.heartbeat` (HTTP GET of `SPIRA_RECONCILER_HEARTBEAT_URL`, `:494-503`) | host | 5 s (`:43`) | tick id | URL empty → Unconfigured, no effect; only when dead-man Fresh (`:663`) |
//!
//! `attempt` is recorded by the `page`/`retry`/`floor_ask` runners themselves, so a page and its
//! attempt row never diverge.
//!
//! ## Silence
//!
//! Idle: no candidate Sends or Notes, no retry intent, no floor change, section unchanged and
//! observed < 300 s ago. Then only the probe. Two standing exceptions today: `knob_init` runs every
//! pass (`:560`) — move to boot; and the heartbeat GET runs every Fresh minute — that is its job
//! (off-box dead-man), so it is an effect by design, not journaled as an event beyond the
//! section's `heartbeat_last` field. `publish` fires at least every 300 s to keep the section fresh
//! (`target_latency 5m`, `:623`); that is the floor of the journal budget.
//!
//! **Overlap.** `spira-runtime` already pings `SPIRA_RUNTIME_HEARTBEAT_URL` from its heartbeat
//! handler (`spira-runtime/src/lib.rs:203-204`, `settings.rs:265`). Once both live in one process,
//! two off-box heartbeats measure the same liveness; keep both until Ryan decides (they may point
//! at different monitors; unverified).
//!
//! ## Replaces
//!
//! - `spira-alarms-prod.service` + `.timer`. `incident page --probe` (watchd `alarms-probe`) and
//!   `incident floor-cleared` remain CLI/other tasks (`incident.probe`).
//! - Files `run/alarms-pages.tsv`, `run/alarms-floor.tsv` (carried state). The attempt log stays.
//! - Children per tick: `spira-lc` ≥ 3 (`snapshot`, `knob-init`, `publish-status`) via `bounded`
//!   (`main.rs:648`); `systemctl` 2–3 (`main.rs:701-702`); `bdq` per page (`mail/src/bead.rs:77`).
//! - Keys pinned: `SPIRA_RECONCILER_DEADMAN_SECS`, `SPIRA_RECONCILER_HEARTBEAT_URL`, mail `Env`
//!   (`mail::env::Env::load`, `main.rs:694`), `SPIRA_RUN`, hostname.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. `Observe` mode shadows pages, asks, acks and publishes —
//! **but then nobody is paged**, so Observe on this task must be paired with the old unit running.
//! Running both in Act double-pages only inside the page window, since page rows are not shared
//! (today's file vs carried state): avoid; reverse by rollback, not overlap.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "incident.alarms";
pub const STREAM: &str = "task.incident.alarms";
pub const TICK: EventKindTag = "incident.alarms.tick";
pub const STATE: EventKindTag = "incident.alarms.state";

pub const OBSERVE: &str = "incident.alarms.observe";
pub const PAGE: &str = "incident.alarms.page";
pub const NOTE: &str = "incident.alarms.note";
pub const FLOOR_ASK: &str = "incident.alarms.floor_ask";
pub const RETRY: &str = "incident.alarms.retry";
pub const ATTEMPT: &str = "incident.alarms.attempt";
pub const KNOB_INIT: &str = "incident.alarms.knob_init";
pub const PUBLISH: &str = "incident.alarms.publish";
pub const HEARTBEAT: &str = "incident.alarms.heartbeat";
pub const EFFECTS: &[&str] = &[OBSERVE, PAGE, NOTE, FLOOR_ASK, RETRY, ATTEMPT, KNOB_INIT, PUBLISH, HEARTBEAT];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INCIDENT_ALARMS_SECS";
pub const DEADMAN_KEY: &str = "SPIRA_RECONCILER_DEADMAN_SECS";
pub const HEARTBEAT_URL_KEY: &str = "SPIRA_RECONCILER_HEARTBEAT_URL";
pub const KEYS: &[&str] = &[CADENCE_KEY, DEADMAN_KEY, HEARTBEAT_URL_KEY, "SPIRA_RUN", "SPIRA_MAIL"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 5_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

/// One row of `tsd/alarm-attempt.jsonl`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attempt {
    pub at: u64,
    pub caller: String,
    pub kind: String,
    pub reference: String,
    pub delivered: bool,
}

/// The goodput pass record the dead-man reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PassRecord {
    pub n: u64,
    pub at_ms: u64,
    pub mode: String,
    pub no_lever: Vec<String>,
    pub exhausted: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// Sections with freshness, the latest pass record and intents.
    pub section_snapshot: serde_json::Value,
    pub pass: Option<PassRecord>,
    pub observability_halted: bool,
    /// `reconcile.goodput` registered and in Act mode, from `scheduler status` (S13); no longer
    /// the goodput timer.
    pub goodput_armed: bool,
    pub attempts: Vec<Attempt>,
    pub floor_proposal: Option<String>,
}

/// The dead-man, the page rows and the floor rows each run a machine (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deadman {
    #[default]
    Waiting,
    Fresh,
    Stale,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Page {
    #[default]
    Absent,
    Sent,
    Noted,
    Quiet,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Floor {
    #[default]
    None,
    Asked,
    Cleared,
}

pub const DEADMAN: &[(Deadman, &str, Deadman)] = &[(Deadman::Waiting, "pass:fresh", Deadman::Fresh), (Deadman::Fresh, "pass:stale", Deadman::Stale), (Deadman::Stale, "pass:fresh", Deadman::Fresh), (Deadman::Fresh, "halted", Deadman::Waiting), (Deadman::Stale, "halted", Deadman::Waiting)];
pub const PAGES: &[(Page, &str, Page)] = &[(Page::Absent, "send", Page::Sent), (Page::Sent, "note", Page::Noted), (Page::Noted, "quiet", Page::Quiet), (Page::Quiet, "window:expired", Page::Sent)];
/// A second floor ask for an asked binding is refused, naming `asked`.
pub const FLOORS: &[(Floor, &str, Floor)] = &[(Floor::None, "below_floor", Floor::Asked), (Floor::Asked, "cleared", Floor::Cleared), (Floor::Cleared, "below_floor", Floor::Asked)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PageRow {
    pub state: Page,
    pub sent_at: Option<u64>,
    pub identity: String,
}

/// The page rows (`run/alarms-pages.tsv`) and floor rows (`run/alarms-floor.tsv`), carried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub deadman: Deadman,
    pub pages: BTreeMap<String, PageRow>,
    pub floors: BTreeMap<String, Floor>,
    pub published_at: Option<u64>,
    pub status: String,
}

pub struct IncidentAlarms {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `deadman`, the candidates through `page::decide`, the floor, retries and the section status.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("incident.alarms: not ported yet")
}

super::hosted!(IncidentAlarms);
