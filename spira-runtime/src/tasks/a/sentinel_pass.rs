//! # sentinel-pass — contract
//!
//! Pattern P2 of [[scheduler-consolidation-2026-11-03]] (§1 row A `sentinel`). Today: `sentinel` with no
//! flag, the full pass `Sentinel::full` (`sentinel/src/pass.rs:445-532`). Paths are relative to
//! `spira-releases/current/`. CHECK 7 is not this task's: it hands off to [[sentinel-call]].
//!
//! **Code vs catalogue.** No `dwarf-launch` child exists (the call is [[sentinel-call]]'s
//! `systemd-run`); the pass *does* start a long leg the catalogue omits: CHECK 6 dispatches
//! `landing-pass land` as transient unit `spira-landing` (`dispatch.rs:238-300`), distinct from
//! the `landing-pass --pass` timer ([[landing]]). Also forks `strand check` (`pass.rs:575`),
//! `reflect.sh` (`judge.rs:82`) and `bash -c` lib.sh seams (`main.rs:62`, `holds.rs:113-119`).
//!
//! ## Tick
//!
//! Every 2 min, first at boot+180 s (`spira-sentinel-prod.timer`); single-flight (today oneshot,
//! `TimeoutStartSec=900`). Also woken by `unit.ran(spira-landing)` (CHECK 6's leg finished).
//! Budget warning at `SPIRA_SENTINEL_PASS_TARGET_SECS` (`pass.rs:541-549`) becomes the probe deadline.
//!
//! ## Observe — `sentinel-pass.observe` (probe, 15 s)
//!
//! | Field | Today (fork) | Contract source |
//! |---|---|---|
//! | `beads` (content: labels, deps, titles) | `bd list --all --limit 0` cached by `dolt_hashof_db()` fingerprint (`store.rs:119,261-279`) | `content list` over the S7 `LcClient` |
//! | `ready_raw` | `bd ready …` (`store.rs:75-97`) | S7b ready projection |
//! | `rows` (state, holds, holder, lease, stack) | `spira-lc list`, once per pass (`lifecycle.rs:156-190`) | `LcClient::list` |
//! | `plan_ready` | `spira-claim ready-count <scope,plan>` (`pass.rs:613-621`) | spira-claim library over the ready projection |
//! | `live` | `systemctl --user list-units spira-dwarf-*` (`pass.rs:624-655`) | `unit.facts` (S6) |
//! | `holder_alive[id]` | `hel holder-alive <id>` (`detect.rs:345-349`) | hel library `holder_alive` |
//! | `session_gone[holder]` | `spira_config::session::session_gone` (`lifecycle.rs:336`) | library, unchanged |
//! | `landing_status` | `$SPIRA_RUN/landing.status`, `landing.progress` (`dispatch.rs:177-236`) | the `landing` task's carried state / `unit.ran` body (unverified shape) |
//! | `landing_active` | `systemctl is-active spira-landing` (`dispatch.rs:58-71`) | `unit.facts` |
//! | `pilgrimages` | `pilgrimage.sh check` (`pass.rs:554-570`) | pilgrimage library verb (unverified: script today) |
//! | `strand` | `strand check` (`pass.rs:573-588`) | strand library `check` |
//! | `skew`, `world` | `release_skew` (`call.rs:467-485`) | library |
//! | `inference_last` | `$SPIRA_RUN/inference.cooldown` (`judge.rs:54-59`) | carried state |
//!
//! Before any CAS write the row is re-read live: `spira-lc show` (`lifecycle.rs:223-235`) → `LcClient::show`.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (carry, Vec<Act>)`, the checks in today's order (`pass.rs:486-523`):
//! CHECK 1 pilgrimages; 2 wait-hold + stale-lease reap (`wait_decisions`, `stale_leases`,
//! `lifecycle.rs:288-361`); 2b strand; 2c inconsistent rows, detect only (`lifecycle.rs:386-400`);
//! 2d stale ask holds, stack conflicts, starvation (`holds.rs:122-154`); 3 recompute-blocked when
//! starved (`pass.rs:591-607`); 6 landing dispatch + 12 watchtower checks (`dispatch.rs:177-300`);
//! 3b queue waiters (`waiters.rs:120-215`); 3c open children (`open_children.rs:116`), red trackers
//! (`red_trackers.rs:34-52`); 7 → `call.wanted`; 8 judgement (`judge.rs:47-95`, `should_judge`).
//! Carry: `inference_last`, `land_escalated_at` (`dispatch.rs:141-145`), `roster_warned`
//! (`pass.rs:658-684`). Fixtures: `sentinel/src/tests.rs` (1,310 lines) and each module's tests.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sentinel-pass.lc_event` (`spira-lc event bead <id> --expect <state> --version <v>`, `lifecycle.rs:238-285`): HolderDead, Hold/Unhold wait, AskWithdrawn | store | 5 s | `(id, version, kind)` | CAS mismatch (rc 3, normal race); row moved since snapshot (`lifecycle.rs:327`) |
//! | `sentinel-pass.lc_hold` / `.lc_unhold` (`waiters.rs:88-118`) | store | 5 s | `(id, kind)` | — (fire-and-forget today) |
//! | `sentinel-pass.lc_fact` (`spira-lc fact … reclaimed`, `lifecycle.rs:368-383`) | store | 5 s | `(id, kind, cause, pass)` | — |
//! | `sentinel-pass.lc_close` (`lifecycle_row::close_with`, `red_trackers.rs:46`) | store | 5 s | id | non-terminal fixes |
//! | `sentinel-pass.content_label` / `.content_note` / `.content_close` (bd `label add/remove`, `note`, `close`: `holds.rs:137`, `open_children.rs:161-172`, `waiters.rs:153-215`, `lifecycle.rs:355`) | store | 5 s | `(id, label, op)` / `(id, note-hash)` | — |
//! | `sentinel-pass.recompute_blocked` (`bd recompute-blocked`, `pass.rs:600`) | store | 30 s (unverified) | pass id | not starved |
//! | `sentinel-pass.stop_holder` (`hel stop-holder`, `lifecycle.rs:364-366`) | unit (`unit.stop`) | 30 s | bead id | holder still alive after stop → no reclaim |
//! | `sentinel-pass.landing_start` (`unit.start` of `spira-landing` → `landing-pass land`, `RuntimeMaxSec=SPIRA_LAND_MAXSEC`, `dispatch.rs:241-290`) | unit | 10 s | unit name `SPIRA_LAND_UNIT` | already active; skew |
//! | `sentinel-pass.watchtower` ×12 checks (`dispatch.rs:180-203`) | host → library | 10 s each (unverified) | `(check, pass)` | — (moves with `watch.tower`) |
//! | `sentinel-pass.strand` (`pass.rs:575`) | store | 30 s (unverified) | pass id | — |
//! | `sentinel-pass.pilgrimage_close` (`pass.rs:555`) | store | 30 s (unverified) | pass id | — |
//! | `sentinel-pass.ask` (`mail send concierge --kind question`, `pass.rs:258-293`; land escalation `dispatch.rs:135-163`) | store | 5 s | subject; refused while an open ask carries it (`pass.rs:300-327`) | cooldown `SPIRA_LAND_ESCALATE_EVERY` |
//! | `sentinel-pass.incident_event` (lib.sh `event` seam: `queue.starved`, `holds.rs:113-119`) | store | 5 s | `(kind, target, title)` | — |
//! | `sentinel-pass.reflect` (`unit.start` of `reflect.sh <ids>`, `judge.rs:76-92`) | unit | 10 s | cooldown window | in cooldown; progressed > 0 |
//! | `sentinel-pass.phase_metric` (`tsd-write --family sentinel-phase`, `pass.rs:177-207`) | host (in-process otel) | 1 s | `(pass, check)` | — |
//! | emits `call.wanted` to [[sentinel-call]] | — (Emit) | — | pass id | — |
//!
//! ## States
//!
//! `idle → observing → deciding → acting → [awaiting_landing] → idle`.
//!
//! | State × event | → |
//! |---|---|
//! | idle × tick / `unit.ran(spira-landing)` | observing |
//! | observing × probe ok | deciding |
//! | observing × probe failed (bd unreadable, `pass.rs:421-426`) | idle, loud once; `rows` unreadable → no lifecycle decision (`lifecycle.rs:192-199`) |
//! | deciding × acts=∅ | idle |
//! | deciding × acts | acting |
//! | acting × all outcomes, no landing started | idle |
//! | acting × `landing_start` ok | awaiting_landing |
//! | awaiting_landing × `unit.ran(spira-landing)` | observing (reads its status) |
//! | awaiting_landing × tick | observing (landing stays in flight; not re-started) |
//! | any × tick while observing/acting | unchanged (single-flight) |
//!
//! ## Silence
//!
//! No starvation, no stale lease, no label change, landing already in flight: the probe only.
//! The `state: open=… plan_ready=…` line (`pass.rs:463-472`) becomes a status field (S13), not a journal row.
//!
//! ## Replaces
//!
//! `spira-sentinel-prod.service`/`.timer`, `world guard`; ~280 children/pass: `bd` (list, ready, sql,
//! label, note, close, recompute-blocked), `spira-lc` (list, show, event, fact, hold, unhold),
//! `spira-claim ready-count`, `hel holder-alive/stop-holder`, `systemctl`, `systemd-run`, `tsd-write`,
//! `pilgrimage.sh`, `strand`, `watchtower` ×12, `reflect.sh`, `mail`, `bash -c lib.sh` (probe +
//! seams). Keys (`cfg.rs:224-247,425-476`): `SPIRA_RUN`, `SPIRA_DB`, `SPIRA_BD`, `SPIRA_SCOPE_LABEL`,
//! `SPIRA_ASK_LABEL`, `SPIRA_NO_LOOP_LABEL`, `SPIRA_QUEUE_WAIT_LABEL`, `SPIRA_OPEN_CHILDREN_LABEL`,
//! `SPIRA_RED_TRACKER_LABEL`, `SPIRA_WORK_CLOSE_TYPES`, `SPIRA_RECLAIM_GRACE_SECS`, `SPIRA_LAND_MAXSEC`,
//! `SPIRA_LAND_UNIT`, `SPIRA_LAND_STALE`, `SPIRA_LAND_ESCALATE_EVERY`, `SPIRA_INFERENCE_EVERY`,
//! `SPIRA_SENTINEL_PASS_TARGET_SECS`, `SPIRA_SKIP_RECLAIM`, `SPIRA_REPO_MAP`, `SPIRA_GH`, `SPIRA_BATCH_MAXPAR`.
//!
//! ## Reverse
//!
//! Release rollback restores the unit; or `Observe` mode: every effect above is journaled
//! `effect.shadowed`, CHECK 2's reap included, and the old timer is re-enabled by hand.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "sentinel-pass";
pub const STREAM: &str = "task.sentinel-pass";
pub const TICK: EventKindTag = "sentinel-pass.tick";
pub const STATE: EventKindTag = "sentinel-pass.state";
/// CHECK 7's hand-off to [`super::sentinel_call`], emitted instead of running the call.
pub const CALL_WANTED: EventKindTag = super::sentinel_call::CALL_WANTED;
/// The landing leg CHECK 6 starts as a transient unit.
pub const LANDING_UNIT: &str = "spira-landing";
pub const OBSERVE: &str = "sentinel-pass.observe";
pub const LC_EVENT: &str = "sentinel-pass.lc_event";
pub const LC_HOLD: &str = "sentinel-pass.lc_hold";
pub const LC_UNHOLD: &str = "sentinel-pass.lc_unhold";
pub const LC_FACT: &str = "sentinel-pass.lc_fact";
pub const LC_CLOSE: &str = "sentinel-pass.lc_close";
pub const CONTENT_LABEL: &str = "sentinel-pass.content_label";
pub const CONTENT_NOTE: &str = "sentinel-pass.content_note";
pub const CONTENT_CLOSE: &str = "sentinel-pass.content_close";
pub const RECOMPUTE_BLOCKED: &str = "sentinel-pass.recompute_blocked";
pub const STOP_HOLDER: &str = "sentinel-pass.stop_holder";
pub const LANDING_START: &str = "sentinel-pass.landing_start";
pub const WATCHTOWER: &str = "sentinel-pass.watchtower";
pub const STRAND: &str = "sentinel-pass.strand";
pub const PILGRIMAGE_CLOSE: &str = "sentinel-pass.pilgrimage_close";
pub const ASK: &str = "sentinel-pass.ask";
pub const INCIDENT_EVENT: &str = "sentinel-pass.incident_event";
pub const REFLECT: &str = "sentinel-pass.reflect";
pub const PHASE_METRIC: &str = "sentinel-pass.phase_metric";

pub const CADENCE_MS: u64 = 120_000;
/// The first tick after boot (`OnBootSec=180s`).
pub const BOOT_DELAY_MS: u64 = 180_000;
pub const OBSERVE_DEADLINE_MS: u64 = 15_000;

pub const ACTS: &[(&str, u64)] = &[
    (LC_EVENT, 5_000),
    (LC_HOLD, 5_000),
    (LC_UNHOLD, 5_000),
    (LC_FACT, 5_000),
    (LC_CLOSE, 5_000),
    (CONTENT_LABEL, 5_000),
    (CONTENT_NOTE, 5_000),
    (CONTENT_CLOSE, 5_000),
    (RECOMPUTE_BLOCKED, 30_000),
    (STOP_HOLDER, 30_000),
    (LANDING_START, 10_000),
    (WATCHTOWER, 10_000),
    (STRAND, 30_000),
    (PILGRIMAGE_CLOSE, 30_000),
    (ASK, 5_000),
    (INCIDENT_EVENT, 5_000),
    (REFLECT, 10_000),
    (PHASE_METRIC, 1_000),
];
/// Hand-offs decide returns as acts that are emitted, not run: CHECK 7 wants a call.
pub const EMITS: &[EventKindTag] = &[CALL_WANTED];
pub const EFFECTS: &[&str] = &[OBSERVE, LC_EVENT, LC_HOLD, LC_UNHOLD, LC_FACT, LC_CLOSE, CONTENT_LABEL, CONTENT_NOTE, CONTENT_CLOSE, RECOMPUTE_BLOCKED, STOP_HOLDER, LANDING_START, WATCHTOWER, STRAND, PILGRIMAGE_CLOSE, ASK, INCIDENT_EVENT, REFLECT, PHASE_METRIC];
pub const KEYS: &[&str] = &[
    "SPIRA_RUN",
    "SPIRA_DB",
    "SPIRA_BD",
    "SPIRA_SCOPE_LABEL",
    "SPIRA_ASK_LABEL",
    "SPIRA_NO_LOOP_LABEL",
    "SPIRA_QUEUE_WAIT_LABEL",
    "SPIRA_OPEN_CHILDREN_LABEL",
    "SPIRA_RED_TRACKER_LABEL",
    "SPIRA_WORK_CLOSE_TYPES",
    "SPIRA_RECLAIM_GRACE_SECS",
    "SPIRA_LAND_MAXSEC",
    "SPIRA_REPO_MAP",
    "SPIRA_GH",
    "SPIRA_BATCH_MAXPAR",
];
pub const UNREGISTERED: &[&str] = &["SPIRA_LAND_UNIT", "SPIRA_LAND_STALE", "SPIRA_LAND_ESCALATE_EVERY", "SPIRA_INFERENCE_EVERY", "SPIRA_SENTINEL_PASS_TARGET_SECS", "SPIRA_SKIP_RECLAIM"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// One bead's content row: labels, dependencies, title.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub labels: Vec<String>,
    pub deps: Vec<String>,
    pub status: String,
}

/// One lifecycle row: state, holds, holder, lease, stack, and the CAS version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub state: String,
    pub version: u64,
    pub holds: Vec<String>,
    pub holder: Option<String>,
    pub lease_until_ms: Option<u64>,
    pub stack: Vec<String>,
}

/// The landing leg's last status, as its `unit.ran` body or the `landing` task's carry reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingStatus {
    pub status: String,
    pub progress_at_ms: Option<u64>,
}

/// The observe snapshot. `beads` or `rows` that could not be read are `None`: no lifecycle
/// decision rests on them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub beads: Option<Vec<Bead>>,
    pub ready_raw: Vec<String>,
    pub rows: Option<Vec<Row>>,
    pub plan_ready: Option<u32>,
    pub live: Vec<String>,
    pub holder_alive: BTreeMap<String, bool>,
    pub session_gone: BTreeMap<String, bool>,
    pub landing_status: Option<LandingStatus>,
    pub landing_active: bool,
    pub pilgrimages: Vec<String>,
    pub strand: Vec<String>,
    pub skew: Option<String>,
    pub world_running: bool,
    pub now_ms: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Idle,
    Observing,
    Deciding,
    Acting,
    AwaitingLanding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Tick,
    /// `unit.ran(spira-landing)`.
    LandingRan,
    /// `unit.ran` for any other unit.
    OtherUnitRan,
    ProbeOk,
    ProbeFailed,
    NoActs,
    Acts,
    AllDoneNoLanding,
    LandingStarted,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Idle, Trigger::Tick, State::Observing),
    (State::Idle, Trigger::LandingRan, State::Observing),
    (State::Observing, Trigger::ProbeOk, State::Deciding),
    (State::Observing, Trigger::ProbeFailed, State::Idle),
    (State::Observing, Trigger::Tick, State::Observing),
    (State::Deciding, Trigger::NoActs, State::Idle),
    (State::Deciding, Trigger::Acts, State::Acting),
    (State::Acting, Trigger::AllDoneNoLanding, State::Idle),
    (State::Acting, Trigger::LandingStarted, State::AwaitingLanding),
    (State::AwaitingLanding, Trigger::LandingRan, State::Observing),
    (State::AwaitingLanding, Trigger::Tick, State::Observing),
    (State::AwaitingLanding, Trigger::OtherUnitRan, State::AwaitingLanding),
];

/// Carried between passes: the machine, the judgement cooldown, the land escalation, and the
/// roster warnings already given.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub state: State,
    pub inference_last_ms: Option<u64>,
    pub land_escalated_at_ms: Option<u64>,
    pub roster_warned: Vec<String>,
}

pub fn step(_state: State, _event: Trigger) -> Result<State, String> {
    crate::not_ported!(NAME)
}

/// The checks in today's order (1, 2, 2b, 2c, 2d, 3, 6, 3b, 3c, 7, 8), from the snapshot alone.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentinelPass {
    pub cadence: Cadence,
    pub boot_delay_ms: u64,
    pub pinned: BTreeMap<String, String>,
}

impl SentinelPass {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        SentinelPass { cadence: Cadence::Every(CADENCE_MS), boot_delay_ms: BOOT_DELAY_MS, pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    /// The boot tick only arms the first pass, `BOOT_DELAY_MS` later.
    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("sentinel-pass/boot", STREAM, EventKind::new(TICK, "boot"))
    }
}

impl Handler for SentinelPass {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK && ev.kind.body == "boot" {
            return vec![Command::Timer { at: ev.at + self.boot_delay_ms, kind: EventKind::new(TICK, "") }];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
