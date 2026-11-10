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
//! Every 2 min, first at boot+180 s (`spira-sentinel-prod.timer`; the port arms the first pass at
//! boot+2 min, since a task row carries one cadence and no first-boot delay); single-flight (today oneshot,
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
//! ## The port
//!
//! Every check of the pass, in today's order, over the sentinel library's pure entries:
//! CHECK 1 (`sentinel::pilgrimage`, `pilgrimage.sh check` ported: the label-scoped epics of the
//! content rows, a live unlanded push-mode child branch read in the probe defers, the marker is
//! the carry's `announced`, dropped when the epic grows or its close fails); CHECK 2's reaper
//! (`sentinel::checks::stale_leases`: a live holder is stopped and reclaimed on a later pass, a
//! dead one reclaimed now); 2b (strand's classification in the probe, `strand check` in-process
//! as the act when anything is stranded); 2c (`checks::inconsistent`, detect only: the carry's
//! `inconsistent`, journaled when it changes); 2d (`pass_checks::stale_ask_holds`, the
//! `AskWithdrawn` event and the ask label off; `stale_conflicts` and `starvation` as
//! `queue.starved` events); 3 (`should_recompute`); 6 (the landing leg as a unit under
//! `landing_props` — `RuntimeMaxSec=SPIRA_LAND_MAXSEC`, output appended to `landing.log`, as
//! `systemd-run` started it — matched back by `UnitRan::key_for`; the twelve watchtower
//! detectors run in the probe through `sentinel::tower`, since each reads and files in one call
//! and a pass must stay silent when nothing changed, so the probe's deadline is the pass budget,
//! 60 s); 3b (queue waiters); 3c (`open_children` over the claimable rows in scope plus the
//! claim's ready answer, `landed_red_trackers` closed); 7's hand-off; and 8
//! (`should_judge` with the carry's cooldown, the leg `sentinel --reflect <ids>` — `reflect.sh`
//! ported into `sentinel::reflect` — started as a unit appending to `reflect.log`).
//!
//! The label, note and close writes go through the store's content door; `recompute-blocked`
//! has no door yet, so CHECK 3's act refuses naming the verb rather than fork bd. CHECK 1's
//! subscribers are `SPIRA_DEFAULT_WATCHERS` (else the operator): the per-epic `spira.watch.*`
//! keys lived in bd's kv, which the store does not serve, so an epic silenced there is not
//! silenced here.
//!
//! ## Reverse
//!
//! Release rollback restores the unit; or `Observe` mode: every effect above is journaled
//! `effect.shadowed`, CHECK 2's reap included, and the old timer is re-enabled by hand.
//!

use std::collections::{BTreeMap, HashSet};

use runtime::event::*;
use runtime::units::{self, UnitRan, UNIT_RAN};
use serde::{Deserialize, Serialize};
use spira_config::lc_call::LcClient;

use super::kit::{self, Act, Cadence, Shape};
use crate::Effects;

pub const NAME: &str = "sentinel-pass";
pub const STREAM: &str = "task.sentinel-pass";
pub const TICK: EventKindTag = "sentinel-pass.tick";
pub const STATE: EventKindTag = "sentinel-pass.state";
/// CHECK 7's hand-off to [`super::sentinel_call`], emitted instead of running the call.
pub const CALL_WANTED: EventKindTag = super::sentinel_call::CALL_WANTED;
/// The landing leg CHECK 6 starts as a transient unit: its key under the naming rule.
pub const LANDING_UNIT: &str = "spira-landing";
pub const LANDING_KEY: &str = "landing";
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

/// `OnUnitActiveSec=2min`. The task row has no first-boot delay, so the old timer's
/// `OnBootSec=180s` is not modelled: the boot tick arms the first pass one cadence later.
pub const CADENCE_MS: u64 = 120_000;
/// The pass budget (`SPIRA_SENTINEL_PASS_TARGET_SECS`, 60 s) is the probe deadline: the probe also runs
/// CHECK 6's twelve watchtower detectors and CHECK 2b's strand classification.
pub const OBSERVE_DEADLINE_MS: u64 = 60_000;

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
    "SPIRA_OPERATOR_ACTOR",
];
pub const UNREGISTERED: &[&str] = &["SPIRA_LAND_UNIT", "SPIRA_LAND_STALE", "SPIRA_LAND_ESCALATE_EVERY", "SPIRA_INFERENCE_EVERY", "SPIRA_SENTINEL_PASS_TARGET_SECS", "SPIRA_SKIP_RECLAIM", "SPIRA_EPIC_LABELS", "SPIRA_DEFAULT_WATCHERS"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// One bead's content row: labels, `blocks` dependencies, title, its type and its parents.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub labels: Vec<String>,
    pub deps: Vec<String>,
    pub status: String,
    /// `issue_type`: an `epic` is CHECK 1's, an `epic`/`event` is never plan work.
    #[serde(default)]
    pub kind: String,
    /// Its parents, by the row's `parent` and its `parent-child` dependencies (CHECK 1, 3c).
    #[serde(default)]
    pub parents: Vec<String>,
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
    /// The hold's reason, its last update and the recorded tip (CHECK 2d, 3c).
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub updated_at_ms: Option<u64>,
    #[serde(default)]
    pub tip: Option<String>,
}

/// The landing leg's last status, as its `unit.ran` body or the `landing` task's carry reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingStatus {
    pub status: String,
    pub progress_at_ms: Option<u64>,
}

/// The labels the checks read, as pinned (`SPIRA_QUEUE_WAIT_LABEL`). No default spelling: an
/// unpinned label is empty, and a check whose label is empty does not run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Labels {
    pub queue_wait: String,
    /// `SPIRA_ASK_LABEL` (CHECK 2d), `SPIRA_SCOPE_LABEL` (the plan and the scoped ready set),
    /// `SPIRA_OPEN_CHILDREN_LABEL` and `SPIRA_RED_TRACKER_LABEL` (CHECK 3c).
    pub ask: String,
    pub scope: String,
    pub open_children: String,
    pub red_tracker: String,
    /// `SPIRA_EPIC_LABELS`, AND-scoped: CHECK 1's partition.
    pub epics: Vec<String>,
}

/// The observe snapshot. `beads` or `rows` that could not be read are `None`: no lifecycle
/// decision rests on them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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
    /// `SPIRA_RECLAIM_GRACE_SECS`, as pinned.
    pub reclaim_grace_secs: u64,
    /// `SPIRA_SKIP_RECLAIM=1`: the reaper is off.
    pub skip_reclaim: bool,
    /// `SPIRA_LAND_MAXSEC`, as pinned: the landing leg's `RuntimeMaxSec`.
    pub land_maxsec: u64,
    pub labels: Labels,
    /// CHECK 1: each epic's children whose push-mode branch is live and not LANDED in the
    /// lifecycle record (`spira/<child>` in `<repo>`), for the complete epics only.
    pub unlanded: BTreeMap<String, Vec<String>>,
    /// CHECK 1: who a completion is announced to (`SPIRA_DEFAULT_WATCHERS`, else the operator).
    pub watchers: Vec<String>,
    /// CHECK 2d: each repository an unmapped-repo ask hold waits on, and whether it resolves now.
    pub repo_resolves: BTreeMap<String, bool>,
    /// CHECK 2d: each prerequisite's stack-conflict marker, (recorded tip, age in seconds).
    pub conflict_markers: BTreeMap<String, (String, u64)>,
    /// CHECK 6: each watchtower check this pass ran, with what it said.
    pub tower: Vec<String>,
    /// CHECK 8: `SPIRA_INFERENCE_EVERY`, the judgement cooldown in seconds.
    pub inference_every_secs: u64,
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
#[serde(default)]
pub struct Carry {
    pub state: State,
    pub inference_last_ms: Option<u64>,
    pub land_escalated_at_ms: Option<u64>,
    pub roster_warned: Vec<String>,
    /// CHECK 1's completion markers (`spira.landed.<epic>` in the script's kv): an epic is
    /// announced once, and a marker is dropped when the epic grows again or its close failed.
    pub announced: Vec<String>,
    /// CHECK 2c's last finding: rows whose holder and state disagree, recorded when it changes.
    pub inconsistent: Vec<String>,
}

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    TABLE.iter().find(|(s, e, _)| *s == state && *e == event).map(|(_, _, to)| *to).ok_or_else(|| format!("{NAME}: {event:?} is not a move from state {state:?}"))
}

/// A row as the sentinel's checks read it, in the machine's own words (`WORKING` is the leased state).
fn lc_row(r: &Row) -> sentinel::model::LcRow {
    sentinel::model::LcRow {
        bead_id: r.id.clone(),
        state: r.state.clone(),
        holder: r.holder.clone(),
        lease_until: r.lease_until_ms.map(|ms| (ms / 1000) as i64),
        holds: r.holds.clone(),
        stack: r.stack.clone(),
        version: Some(r.version.to_string()),
        tip: r.tip.clone(),
        reason: r.reason.clone(),
        updated_at: r.updated_at_ms.map(|ms| (ms / 1000) as i64),
    }
}

fn model_bead(b: &Bead) -> sentinel::model::Bead {
    let dep = |t: &String, kind: &str| sentinel::model::Dep { depends_on_id: Some(t.clone()), r#type: Some(kind.into()), ..sentinel::model::Dep::default() };
    sentinel::model::Bead {
        id: b.id.clone(),
        title: Some(b.title.clone()),
        status: b.status.clone(),
        issue_type: (!b.kind.is_empty()).then(|| b.kind.clone()),
        labels: b.labels.clone(),
        dependency_count: b.deps.len() as u64,
        dependencies: b.deps.iter().map(|d| dep(d, "blocks")).chain(b.parents.iter().map(|p| dep(p, "parent-child"))).collect(),
        ..sentinel::model::Bead::default()
    }
}

/// The plan labels (`<scope,>plan`) CHECK 3, 7 and 8 count.
fn plan_labels(labels: &Labels) -> Vec<String> {
    let mut out: Vec<String> = labels.scope.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
    out.push("plan".into());
    out
}

/// What a store write's runner is handed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Write {
    pub id: String,
    pub expect: String,
    pub version: u64,
    pub kind: String,
    pub cause: String,
    pub label: String,
    pub op: String,
    pub text: String,
    /// The landing leg's `RuntimeMaxSec`, in seconds.
    #[serde(default)]
    pub max_secs: u64,
    /// CHECK 1's notice and CHECK 2d's event title.
    #[serde(default)]
    pub subject: String,
    /// CHECK 1: the addresses a completion is delivered to.
    #[serde(default)]
    pub subscribers: Vec<String>,
    /// CHECK 1: the epic's child count.
    #[serde(default)]
    pub total: usize,
    /// CHECK 8: the open plan beads judgement is shown.
    #[serde(default)]
    pub ids: Vec<String>,
}

fn act(effect: &str, key: impl Into<String>, w: Write) -> Act {
    Act::new(effect, key).with_input(serde_json::to_string(&w).unwrap_or_default())
}

/// The acts that are progress for CHECK 8: a reclaim, a withdrawn hold, a close, strand's fix. A
/// recompute is not: the old pass counted it only once it freed a bead, which decide cannot know.
fn progress(acts: &[Act]) -> u32 {
    acts.iter().filter(|a| matches!(a.effect.as_str(), LC_EVENT | PILGRIMAGE_CLOSE | STRAND | LC_CLOSE | CONTENT_CLOSE)).count() as u32
}

/// CHECK 1 (`pilgrimage.sh check`): each complete epic of the partition announced and closed
/// once; a marker dropped when its epic grew again.
fn check1(next: &mut Carry, beads: &[sentinel::model::Bead], snap: &Snapshot, acts: &mut Vec<Act>) {
    use sentinel::pilgrimage::{self, Step};
    let state = |id: &str| snap.rows.iter().flatten().find(|r| r.id == id).map(|r| r.state.clone());
    let epics = pilgrimage::epics(beads, &snap.labels.epics, &state);
    let announced: std::collections::BTreeSet<String> = next.announced.iter().cloned().collect();
    for step in pilgrimage::check(&epics, &announced, &snap.unlanded, &|_| snap.watchers.clone()) {
        match step {
            Step::Reopened { id } => next.announced.retain(|a| *a != id),
            Step::Deferred { .. } => {}
            Step::Complete { id, subject, body, total, subscribers } => {
                next.announced.push(id.clone());
                acts.push(act(PILGRIMAGE_CLOSE, &id, Write { id: id.clone(), subject, text: body, total, subscribers, ..Write::default() }));
            }
        }
    }
}

/// CHECK 2: a live holder is stopped first, and never reclaimed in the pass that stops it.
fn check2(rows: &[Row], lc: &[sentinel::model::LcRow], snap: &Snapshot, now: i64, acts: &mut Vec<Act>) {
    let alive = |id: &str| snap.holder_alive.get(id).copied().unwrap_or(false);
    let gone = |h: &str| snap.session_gone.get(h).copied().unwrap_or(false);
    for (id, ago) in sentinel::checks::stale_leases(lc, now, snap.reclaim_grace_secs as i64, alive, gone) {
        if alive(&id) {
            acts.push(act(STOP_HOLDER, &id, Write { id: id.clone(), ..Write::default() }));
            continue;
        }
        let Some(row) = rows.iter().find(|r| r.id == id) else { continue };
        let expect = lc_row(row).state;
        acts.push(act(LC_EVENT, format!("{id}:{}:HolderDead", row.version), Write { id: id.clone(), expect, version: row.version, kind: sentinel::lifecycle_events::HOLDER_DEAD.into(), ..Write::default() }));
        acts.push(act(LC_FACT, format!("{id}:reclaimed:stale-lease:{now}"), Write { id: id.clone(), kind: "reclaimed".into(), cause: "stale-lease".into(), ..Write::default() }));
        let text = if ago > 0 { format!("Reclaimed by CHECK 2: in_progress with a lease that expired {}m ago and was never released.", ago / 60) } else { "Reclaimed by CHECK 2: the holder's session is gone and never released the claim.".to_string() };
        acts.push(act(CONTENT_NOTE, format!("{id}:{}", text.len()), Write { id, text, ..Write::default() }));
    }
}

/// `lib.sh spira_event`'s act: one cooled-down incident event line.
fn event(kind: &str, target: &str, title: &str, detail: &str) -> Act {
    act(INCIDENT_EVENT, format!("{kind}:{target}:{title}"), Write { kind: kind.into(), id: target.into(), subject: title.into(), text: detail.into(), ..Write::default() })
}

/// CHECK 2d (`holds.rs`): stale ask holds withdrawn, stale stack conflicts and a starved
/// queue announced.
fn check2d(rows: &[Row], v: &sentinel::pass_checks::View, snap: &Snapshot, now: i64, acts: &mut Vec<Act>) {
    use sentinel::pass_checks as pc;
    let resolves = |name: &str| snap.repo_resolves.get(name).copied().unwrap_or(false);
    for (id, why) in pc::stale_ask_holds(v, &snap.labels.ask, now, snap.reclaim_grace_secs as i64, &resolves) {
        let Some(row) = rows.iter().find(|r| r.id == id) else { continue };
        acts.push(act(LC_EVENT, format!("{id}:{}:AskWithdrawn", row.version), Write { id: id.clone(), expect: row.state.clone(), version: row.version, kind: "\"AskWithdrawn\"".into(), text: why, ..Write::default() }));
        if v.get(&id).is_some_and(|b| b.has(&snap.labels.ask)) {
            acts.push(act(CONTENT_LABEL, format!("{id}:{}:remove", snap.labels.ask), Write { id, label: snap.labels.ask.clone(), op: "remove".into(), ..Write::default() }));
        }
    }
    let marker = |id: &str| snap.conflict_markers.get(id).cloned();
    for (bead, prereq, age) in pc::stale_conflicts(v, &marker, spira_config::stack_conflict::REPORT_AGE.as_secs()) {
        let title = format!("{bead} is excluded from the ready set by a stack-conflict marker on {prereq}, {age}s old");
        acts.push(event("queue.starved", &bead, &title, "the marker lapses on its own; the next claim attempt re-records it if the conflict stands"));
    }
    let (held, ready, starved) = pc::starvation(v.rows);
    if starved {
        let title = format!("{held} of {ready} READY beads are held — the builder queue is starved");
        acts.push(event("queue.starved", "-", &title, "withdraw holds whose cause is gone: spira-lc list --hold ask"));
    }
}

/// CHECK 3b: queue waiters.
fn check3b(beads: &[Bead], rows: &[Row], lc: &[sentinel::model::LcRow], snap: &Snapshot, acts: &mut Vec<Act>) {
    let label = &snap.labels.queue_wait;
    if label.is_empty() {
        return;
    }
    let labeled: Vec<&Bead> = beads.iter().filter(|b| b.labels.contains(label)).collect();
    let active = sentinel::checks::active_blockers_lc(lc);
    // The waiters are the labeled beads whose row is claimable; one with no lifecycle
    // row is left alone (`waiters.rs`, `labeled_set_is_claimable_by_the_lifecycle_row`).
    let waiting: HashSet<String> = sentinel::checks::labeled_claimable(lc, labeled.iter().map(|b| model_bead(b)).collect());
    let ready: Vec<sentinel::model::Bead> = if active.is_empty() { vec![] } else { beads.iter().filter(|b| snap.ready_raw.contains(&b.id)).map(model_bead).collect() };
    for (add, id) in sentinel::checks::decide(&active, &waiting, &ready) {
        let (op, hold) = if add { ("add", LC_HOLD) } else { ("remove", LC_UNHOLD) };
        acts.push(act(CONTENT_LABEL, format!("{id}:{label}:{op}"), Write { id: id.clone(), label: label.clone(), op: op.into(), ..Write::default() }));
        acts.push(act(hold, format!("{id}:wait"), Write { id, kind: "wait".into(), text: "blocker certified or batched, not yet landed".into(), ..Write::default() }));
    }
    for b in labeled.iter().filter(|b| rows.iter().any(|r| r.id == b.id && r.state == "LANDED")) {
        acts.push(act(CONTENT_LABEL, format!("{}:{label}:remove", b.id), Write { id: b.id.clone(), label: label.clone(), op: "remove".into(), ..Write::default() }));
        acts.push(act(LC_UNHOLD, format!("{}:wait", b.id), Write { id: b.id.clone(), kind: "wait".into(), ..Write::default() }));
        acts.push(act(CONTENT_CLOSE, &b.id, Write { id: b.id.clone(), text: "Content already on main (LANDED); no branch remained to land.".into(), ..Write::default() }));
    }
}

/// CHECK 3c (`open_children.rs`, `red_trackers.rs`): the open-children label over the scoped
/// ready set (the claimable rows in scope, and the claim's ready answer), and the red trackers
/// whose every fix landed closed.
fn check3c(v: &sentinel::pass_checks::View, snap: &Snapshot, acts: &mut Vec<Act>) {
    use sentinel::pass_checks::{self as pc, Change};
    let scope = &snap.labels.scope;
    let in_scope = |id: &str| scope.is_empty() || v.get(id).is_some_and(|b| b.has(scope));
    let mut ready: Vec<String> = snap.ready_raw.iter().filter(|id| in_scope(id)).cloned().collect();
    ready.extend(v.rows.iter().filter(|r| spira_config::lc_state::is_claimable(&r.state) && in_scope(&r.bead_id) && !snap.ready_raw.contains(&r.bead_id)).map(|r| r.bead_id.clone()));
    let label = &snap.labels.open_children;
    for c in pc::open_children(v, &ready, label) {
        let (id, op) = match c {
            Change::Add(id) => (id, "add"),
            Change::Remove(id) => (id, "remove"),
        };
        acts.push(act(CONTENT_LABEL, format!("{id}:{label}:{op}"), Write { id, label: label.clone(), op: op.into(), ..Write::default() }));
    }
    for (id, fixes) in pc::landed_red_trackers(v, &snap.labels.red_tracker) {
        acts.push(act(LC_CLOSE, &id, Write { id: id.clone(), text: pc::red_tracker_reason(&fixes), ..Write::default() }));
    }
}

/// The checks in today's order (1, 2, 2b, 2c, 2d, 3, 6, 3b, 3c, 7, 8), from the snapshot alone.
/// CHECK 6's twelve watchtower detectors ran in the probe (they read and file in one call),
/// so their answer is in `snap.tower` and nothing here re-asks for them.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    use sentinel::pass_checks::{self as pc, Judge};
    let mut acts = vec![];
    let mut next = carry.clone();
    let now = (snap.now_ms / 1000) as i64;
    let mut landing = false;
    let beads: Vec<sentinel::model::Bead> = snap.beads.iter().flatten().map(model_bead).collect();
    // CHECK 1.
    if snap.beads.is_some() {
        check1(&mut next, &beads, snap, &mut acts);
    }
    // CHECK 2, 2b, 2c, 2d over the lifecycle rows; unreadable rows decide nothing about them.
    let lc: Vec<sentinel::model::LcRow> = snap.rows.iter().flatten().map(lc_row).collect();
    let view = pc::View::new(&beads, &lc);
    if let Some(rows) = &snap.rows {
        if !snap.skip_reclaim {
            check2(rows, &lc, snap, now, &mut acts);
        }
    }
    if !snap.strand.is_empty() {
        acts.push(Act::new(STRAND, format!("strand:{now}")).with_input(serde_json::json!({ "stranded": snap.strand }).to_string()));
    }
    if let Some(rows) = &snap.rows {
        next.inconsistent = sentinel::checks::inconsistent(&lc);
        if snap.beads.is_some() {
            check2d(rows, &view, snap, now, &mut acts);
        }
    }
    // CHECK 3: the starved plan's stale blocked flags.
    let plan = plan_labels(&snap.labels);
    let open_plan = if snap.skip_reclaim { vec![] } else { pc::plan_open(&view, &plan) };
    let plan_inprog = if snap.skip_reclaim { 0 } else { pc::plan_inprog(&view, &plan) };
    let plan_ready = snap.plan_ready.map(|n| n as usize);
    if snap.rows.is_some() && pc::should_recompute(snap.skip_reclaim, plan_ready, plan_inprog, open_plan.len()) {
        acts.push(Act::new(RECOMPUTE_BLOCKED, format!("recompute:{now}")));
    }
    // CHECK 6: the landing leg, unless one is in flight or this is not the current release.
    if snap.world_running && snap.skew.is_none() && !snap.landing_active {
        acts.push(act(LANDING_START, LANDING_UNIT, Write { max_secs: snap.land_maxsec, ..Write::default() }));
        landing = true;
    }
    // CHECK 3b and 3c.
    if let (Some(rows), Some(content)) = (&snap.rows, &snap.beads) {
        check3b(content, rows, &lc, snap, &mut acts);
        check3c(&view, snap, &mut acts);
    }
    // CHECK 7: plan beads ready hand off to the call.
    if snap.plan_ready.is_some_and(|n| n > 0) {
        acts.push(Act::new(CALL_WANTED, format!("plan_ready:{now}")).with_input(serde_json::json!({"plan_ready": snap.plan_ready}).to_string()));
    }
    // CHECK 8: judgement, when the plan is starved and nothing moved, once per cooldown.
    let last = carry.inference_last_ms.map_or(0, |ms| (ms / 1000) as i64);
    if snap.rows.is_some() && pc::should_judge(plan_ready, plan_inprog, open_plan.len(), progress(&acts), last, now, snap.inference_every_secs as i64) == Judge::Yes {
        next.inference_last_ms = Some(snap.now_ms);
        let ids: Vec<String> = open_plan.iter().take(pc::REFLECT_IDS).cloned().collect();
        acts.push(act(REFLECT, format!("reflect:{now}"), Write { ids, ..Write::default() }));
    }
    next.state = if landing { State::AwaitingLanding } else if acts.is_empty() { State::Idle } else { State::Acting };
    (next, acts)
}

/// The observe probe's input: the carry (which T3 reads back) and the pinned config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ObserveIn {
    #[serde(flatten)]
    pub carry: Carry,
    #[serde(default)]
    pub pinned: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentinelPass {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl SentinelPass {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        SentinelPass { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    /// The boot tick only arms the first pass, one cadence later (the row's `cadence_ms`).
    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("sentinel-pass/boot", STREAM, EventKind::new(TICK, "boot"))
    }

    fn observe(&self, carry: &Carry, next_at: u64) -> Vec<Command> {
        let input = ObserveIn { carry: carry.clone(), pinned: self.pinned.clone() };
        kit::on_tick(&SHAPE, serde_json::to_string(&input).unwrap_or_default(), next_at)
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("state"), runtime::rows::Part::Field("inference_last_ms"), runtime::rows::Part::Field("land_escalated_at_ms"), runtime::rows::Part::Field("roster_warned"), runtime::rows::Part::Field("announced"), runtime::rows::Part::Field("inconsistent")];

impl Handler for SentinelPass {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, UNIT_RAN, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        // Commands land on the event's own stream: a `unit.ran` of the landing leg routed here
        // wakes the pass; elsewhere the next tick reads the leg's state from the unit manager.
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK && ev.kind.body == "boot" {
            return vec![Command::Timer { at: self.cadence.next_after(ev.at), kind: EventKind::new(TICK, "") }];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return self.observe(&carry, self.cadence.next_after(ev.at));
        }
        if let Some(ran) = UnitRan::from_event_kind(&ev.kind) {
            if ran.key_for(NAME).as_deref() != Some(LANDING_KEY) {
                return vec![];
            }
            return self.observe(&carry, self.cadence.next_after(ev.at));
        }
        // CHECK 1's close failed (a subscriber unreached, the store refusing): its marker is
        // dropped so the next pass retries, as the script left the epic open unmarked.
        if ev.kind.tag != EFFECT_SUCCEEDED {
            let Some(spec) = crate::watchers::answered_spec(view, ev).filter(|s| s.name == PILGRIMAGE_CLOSE) else { return vec![] };
            let Ok(w) = serde_json::from_str::<Write>(&spec.input) else { return vec![] };
            let mut next = carry.clone();
            next.announced.retain(|a| *a != w.id);
            return kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, vec![]);
        }
        let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
        let (next, acts) = decide(&carry, &snap);
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}

// ---------------------------------------------------------------------------------------------
// The runners.

fn client(e: &Effects, deadline_ms: u64) -> LcClient {
    LcClient::socket(&e.world.lc_socket).within(std::time::Duration::from_millis(deadline_ms))
}

fn answer(lc: &LcClient, args: &[&str]) -> Result<String, String> {
    let r = lc.call(args).map_err(|e| e.to_string())?;
    if r.code == 0 { Ok(r.out) } else { Err(format!("spira-lc {} exited {}: {}", args.first().copied().unwrap_or(""), r.code, r.why())) }
}

fn write_of(input: &str, what: &str) -> Result<Write, String> {
    serde_json::from_str(input).map_err(|e| format!("{what}: {e}"))
}

/// The landing leg's unit under the naming rule.
fn landing_handle() -> Result<units::UnitHandle, String> {
    units::UnitHandle::of(NAME, LANDING_KEY).map_err(|e| e.to_string())
}

/// `sentinel-pass.observe`: the content and lifecycle rows over `LcClient`, the plan pool's
/// ready count from spira-claim in-process, the holders from hel, the landing leg from the
/// unit manager, and the world and release gates.
pub fn observe(e: &Effects, input: &str) -> Result<String, String> {
    let req: ObserveIn = serde_json::from_str(input).unwrap_or_default();
    let pin = |k: &str| req.pinned.get(k).cloned().unwrap_or_default();
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let list = |s: &str| s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect::<Vec<_>>();
    let lc = client(e, 5_000);
    let run = e.world.run_dir.clone();
    let mut snap = Snapshot {
        now_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
        world_running: !run.join("world.halted").is_file() && !run.join("world.draining").is_file(),
        reclaim_grace_secs: pin("SPIRA_RECLAIM_GRACE_SECS").trim().parse().unwrap_or(10_800),
        skip_reclaim: env("SPIRA_SKIP_RECLAIM").as_deref() == Some("1"),
        land_maxsec: pin("SPIRA_LAND_MAXSEC").trim().parse().unwrap_or(0),
        labels: Labels {
            queue_wait: pin("SPIRA_QUEUE_WAIT_LABEL"),
            ask: pin("SPIRA_ASK_LABEL"),
            scope: pin("SPIRA_SCOPE_LABEL"),
            open_children: pin("SPIRA_OPEN_CHILDREN_LABEL"),
            red_tracker: pin("SPIRA_RED_TRACKER_LABEL"),
            epics: list(&env("SPIRA_EPIC_LABELS").unwrap_or_else(|| "spira".into())),
        },
        watchers: list(&env("SPIRA_DEFAULT_WATCHERS").unwrap_or_else(|| pin("SPIRA_OPERATOR_ACTOR"))),
        inference_every_secs: env("SPIRA_INFERENCE_EVERY").and_then(|v| v.trim().parse().ok()).unwrap_or(3_600),
        ..Snapshot::default()
    };
    snap.skew = std::env::var(spira_config::RELEASE_ENV).ok().filter(|r| !r.is_empty()).and_then(|r| spira_config::release_skew::skew(std::path::Path::new(&r))).map(|s| s.describe());
    let content = answer(&lc, &["content", "list", "--all", "--limit", "0", "--json"]).ok().and_then(|t| sentinel::model::parse_beads(&t).ok());
    snap.beads = content.as_ref().map(|v| {
        v.iter()
            .map(|b| {
                let mut parents: Vec<String> = b.parent.iter().filter(|p| !p.is_empty()).cloned().collect();
                parents.extend(b.dependencies.iter().filter(|d| d.r#type.as_deref() == Some("parent-child")).filter_map(|d| d.target().map(str::to_string)));
                Bead { deps: b.dependencies.iter().filter(|d| d.r#type.as_deref() == Some("blocks")).filter_map(|d| d.target().map(str::to_string)).collect(), title: b.title.clone().unwrap_or_default(), status: b.status.clone(), labels: b.labels.clone(), id: b.id.clone(), kind: b.typ().to_string(), parents }
            })
            .collect()
    });
    let mut rows = answer(&lc, &["list", "--live"]).ok().and_then(|t| sentinel::model::parse_lc_rows(&t).ok());
    if let (Some(live), Some(beads)) = (rows.as_mut(), content.as_ref()) {
        let known: std::collections::HashSet<String> = live.iter().map(|r| r.bead_id.clone()).collect();
        let mut ids = sentinel::pass_checks::unread_blockers(beads, &snap.labels.red_tracker, &|id| known.contains(id));
        ids.extend(sentinel::pilgrimage::open_epic_children(beads, &snap.labels.epics).into_iter().filter(|id| !known.contains(id)));
        ids.sort();
        ids.dedup();
        for chunk in ids.chunks(100) {
            if let Some(older) = answer(&lc, &["list", "--ids", &chunk.join(",")]).ok().and_then(|t| sentinel::model::parse_lc_rows(&t).ok()) {
                live.extend(older);
            }
        }
    }
    if let Some(rows) = &rows {
        for r in rows.iter().filter(|r| r.state == "WORKING") {
            snap.holder_alive.insert(r.bead_id.clone(), sentinel::hel_verbs::holder_alive(&r.bead_id).unwrap_or(false));
            if let Some(h) = &r.holder {
                snap.session_gone.insert(h.clone(), spira_config::session::session_gone(h, &spira_config::admission::RealProcs));
            }
        }
        // CHECK 2d's outside facts: whether each unmapped repo resolves now, and each prerequisite's marker.
        let home = spira_config::resolve::locate_home_for_process().ok();
        let reg = home.as_ref().map(|h| spira_config::repos::Registry::from_env(std::env::vars().chain(req.pinned.clone()).collect(), h));
        for name in sentinel::pass_checks::unmapped_repos(rows) {
            let ok = reg.as_ref().and_then(|r| r.root(&name)).is_some_and(|p| std::path::Path::new(&p).join(".git").exists());
            snap.repo_resolves.insert(name, ok);
        }
        for p in sentinel::pass_checks::conflict_prereqs(rows) {
            if let Some((tip, age)) = spira_config::stack_conflict::recorded(&run, &p.bead_id) {
                snap.conflict_markers.insert(p.bead_id.clone(), (tip, age.as_secs()));
            }
        }
        // CHECK 1's assertion: a complete epic's child with a live push-mode branch must be LANDED.
        if let (Some(content), Some(reg)) = (&content, &reg) {
            let state = |id: &str| rows.iter().find(|r| r.bead_id == id).map(|r| r.state.as_str());
            let row_state = |id: &str| state(id).map(str::to_string);
            for epic in sentinel::pilgrimage::epics(content, &snap.labels.epics, &row_state).into_iter().filter(|e| e.complete()) {
                let mut live = vec![];
                for child in &epic.children {
                    for name in reg.names().into_iter().filter(|n| reg.land(n) == "push") {
                        let Some(root) = reg.root(&name) else { continue };
                        let present = super::gate_check::git_out(std::path::Path::new(&root), &["show-ref", "--verify", "--quiet", &format!("refs/heads/spira/{child}")], 10).is_ok_and(|(code, _, _)| code == Some(0));
                        if present && state(child) != Some("LANDED") {
                            live.push(format!("spira/{child} in {name}"));
                        }
                    }
                }
                if !live.is_empty() {
                    snap.unlanded.insert(epic.id, live);
                }
            }
        }
    }
    snap.rows = rows.map(|v| {
        v.into_iter()
            .map(|r| Row { version: r.version.as_deref().and_then(|v| v.parse().ok()).unwrap_or(0), lease_until_ms: r.lease_until.map(|s| s.max(0) as u64 * 1000), updated_at_ms: r.updated_at.map(|s| s.max(0) as u64 * 1000), id: r.bead_id, state: r.state, holds: r.holds, holder: r.holder, stack: r.stack, reason: r.reason, tip: r.tip })
            .collect()
    });
    let plan = plan_labels(&snap.labels).join(",");
    let lookup = |k: &str| req.pinned.get(k).cloned().or_else(|| spira_config::process::cfg(k).ok()).or_else(|| std::env::var(k).ok());
    let o = sentinel::claim::dispatch_in(&["ready-count".to_string(), plan], &mut std::io::empty(), &lookup);
    snap.plan_ready = (o.code == 0).then(|| o.out.trim().parse().ok()).flatten();
    // CHECK 2b's read: what strand would act on or escalate; unreadable is nothing to act on.
    snap.strand = sentinel::edge::strand_observe().unwrap_or_default();
    // CHECK 6's twelve watchtower detectors: each reads and files in one call, so they run here.
    snap.tower = sentinel::tower::CHECKS.iter().map(|c| match sentinel::tower::run(c) {
        Ok(m) => m,
        Err(e) => format!("watchtower: {c} refused — {e}"),
    }).collect();
    let unitp = e.units.as_deref().ok_or("no unit manager configured")?;
    snap.landing_active = matches!(unitp.facts(&landing_handle()?).map(|f| f.state), Ok(units::UnitState::Active | units::UnitState::Activating));
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `sentinel-pass.lc_event`: the row re-read live, then the CAS write; a row that moved since
/// the snapshot is refused.
pub fn lc_event(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, LC_EVENT)?;
    let lc = client(e, spec.deadline_ms);
    let shown: serde_json::Value = serde_json::from_str(&answer(&lc, &["show", &w.id])?).map_err(|e| format!("show {}: {e}", w.id))?;
    let state = shown["bead"]["state"].as_str().unwrap_or_default().to_string();
    if state != w.expect {
        return Err(format!("{} moved to {state} since the sweep read it", w.id));
    }
    let version = w.version.to_string();
    answer(&lc, &["event", "bead", &w.id, "--expect", &w.expect, "--version", &version, "--actor", "sentinel", "--kind", &w.kind])
}

pub fn lc_hold(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, LC_HOLD)?;
    answer(&client(e, spec.deadline_ms), &["hold", &w.id, &w.kind, &w.text, "sentinel"])
}

pub fn lc_unhold(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, LC_UNHOLD)?;
    answer(&client(e, spec.deadline_ms), &["unhold", &w.id, &w.kind, "sentinel"])
}

pub fn lc_fact(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, LC_FACT)?;
    answer(&client(e, spec.deadline_ms), &["fact", &w.id, "--kind", &w.kind, "--actor", "harness", "--cause", &w.cause])
}

pub fn lc_close(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, LC_CLOSE)?;
    let r = client(e, spec.deadline_ms).call_stdin(&["close", &w.id, "--reason-file", "-", "--actor", "sentinel"], &w.text).map_err(|e| e.to_string())?;
    if r.code == 0 { Ok(r.out) } else { Err(format!("spira-lc close {} exited {}: {}", w.id, r.code, r.why())) }
}

/// `sentinel-pass.content_note`: the bead's notes, through the store's `work` door.
pub fn content_note(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, CONTENT_NOTE)?;
    let r = client(e, spec.deadline_ms).call_stdin(&["work", &w.id, "note", "-"], &w.text).map_err(|e| e.to_string())?;
    if r.code == 0 { Ok(r.out) } else { Err(format!("spira-lc work {} note exited {}: {}", w.id, r.code, r.why())) }
}

/// `sentinel-pass.content_label`: a label added or removed through the store's content door.
pub fn content_label(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, CONTENT_LABEL)?;
    let op = if w.op == "add" { "add" } else { "remove" };
    answer(&client(e, spec.deadline_ms), &["content", "label", op, &w.id, &w.label])
}

/// `sentinel-pass.content_close`: a bead's content closed, with its reason, through the content door.
pub fn content_close(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, CONTENT_CLOSE)?;
    answer(&client(e, spec.deadline_ms), &["content", "close", &w.id, "--reason", &w.text])
}

/// `sentinel-pass.recompute_blocked` (bd `recompute-blocked`): the store's content door does
/// not serve the verb, so it is refused naming it, never a bd child.
pub fn recompute_blocked(spec: &EffectSpec) -> Result<String, String> {
    Err(format!("{}: unserved — the store's content door has no `recompute-blocked` verb", spec.name))
}

/// `sentinel-pass.watchtower`: one of CHECK 6's detectors by name (`input` is its name), in
/// process. The pass runs all twelve in its probe; this runs one on its own.
pub fn watchtower(input: &str) -> Result<String, String> {
    let check: String = serde_json::from_str(input).unwrap_or_else(|_| input.trim().to_string());
    sentinel::tower::run(&check)
}

/// `sentinel-pass.strand`: CHECK 2b's act, strand's own check in-process.
pub fn strand(_: &EffectSpec) -> Result<String, String> {
    sentinel::edge::strand_act()
}

/// `sentinel-pass.pilgrimage_close`: CHECK 1's delivery, then the close. Every subscriber is
/// told first; one that cannot be reached leaves the epic open with a note and fails the
/// effect, so the marker drops and the next pass retries.
pub fn pilgrimage_close(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    use sentinel::pilgrimage::{self, Address};
    let w = write_of(&spec.input, PILGRIMAGE_CLOSE)?;
    let lc = client(e, spec.deadline_ms);
    let operator = spira_config::process::cfg("SPIRA_OPERATOR_ACTOR").unwrap_or_default();
    let mut failed = vec![];
    for addr in &w.subscribers {
        let delivered = match pilgrimage::address(addr, &operator) {
            Address::Operator => {
                let body = pilgrimage::note_body(&w.subject, &w.id, &w.text);
                sentinel::ask::send(&sentinel::ask::Mail { mailbox: "operator", from: "Pilgrimage <pilgrimage@spira>", subject: &w.subject, kind: "note", default: "", body: &body, bead: "", own_dedup: false })
            }
            Address::Bead(id) => lc.call_stdin(&["content", "note", &id, "--stdin"], &format!("{}\n{}", w.subject, w.text)).map_err(|e| e.to_string()).and_then(|r| if r.code == 0 { Ok(()) } else { Err(r.why()) }),
            Address::Log => Ok(()),
            Address::Unknown(a) => Err(format!("unknown subscriber address '{a}'")),
        };
        if let Err(why) = delivered {
            failed.push(format!("{addr}: {why}"));
        }
    }
    if !failed.is_empty() {
        let _ = lc.call_stdin(&["content", "note", &w.id, "--stdin"], pilgrimage::UNDELIVERED_NOTE);
        return Err(format!("{}: DELIVERY FAILED — {}", w.id, failed.join("; ")));
    }
    answer(&lc, &["close-epic", &w.id, &pilgrimage::close_reason(w.total, &w.subscribers)]).map(|_| w.subject.clone())
}

/// `sentinel-pass.ask`: one question to the Concierge, in-process over the mail library.
/// `subject`, `text` (the body), `cause` (the default) and `id` (the bead it is about).
pub fn ask(spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, ASK)?;
    sentinel::ask::send(&sentinel::ask::Mail { mailbox: "concierge", from: "Sentinel <sentinel@spira>", subject: &w.subject, kind: "question", default: &w.cause, body: &w.text, bead: &w.id, own_dedup: true }).map(|()| w.subject.clone())
}

/// `sentinel-pass.incident_event`: lib.sh `spira_event`, in-process.
pub fn incident_event(spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, INCIDENT_EVENT)?;
    sentinel::edge::incident_event(&w.kind, &w.id, &w.subject, &w.text)
}

/// `sentinel-pass.reflect`: CHECK 8's inference leg as a unit (`sentinel --reflect <ids>`, the
/// ported `reflect.sh`), appending to `$SPIRA_RUN/reflect.log`; one model call runs for minutes.
pub fn reflect(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, REFLECT)?;
    let sentinel_bin = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).map(|d| d.join("sentinel")).find(|p| p.is_file()).ok_or("sentinel not found on PATH")?;
    let pass_env = ["PATH", "SPIRA_RELEASE", "SPIRA_TOML", "HOME", "SPIRA_HOME", "SPIRA_RUN"];
    let env = pass_env.iter().filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v))).collect();
    let mut argv = vec![sentinel_bin.to_string_lossy().into_owned(), "--reflect".into()];
    argv.extend(w.ids.iter().cloned());
    let unit = units::UnitSpec { task: NAME.into(), key: format!("reflect-{:016x}", runtime::event::fnv1a(&spec.input)), argv, env, cwd: None };
    let props = units::UnitProps { output_append: Some(e.world.run_dir.join("reflect.log")), runtime_max_ms: Some((sentinel::reflect::AGENT_TIMEOUT_SECS + 60) * 1000), ..units::UnitProps::default() };
    let input = serde_json::to_string(&units::StartSpec { unit, props }).map_err(|e| e.to_string())?;
    e.unit(&EffectSpec::keyed(units::UNIT_START, spec.key.clone(), input, spec.deadline_ms))
}

/// `sentinel-pass.stop_holder`: hel stops whatever still works in the bead's worktree.
pub fn stop_holder(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let w = write_of(&spec.input, STOP_HOLDER)?;
    let units = e.units.as_deref().ok_or("no unit manager configured")?;
    sentinel::hel_verbs::stop_holder(&w.id, units).map(|()| format!("stopped the holder of {}", w.id))
}

/// `sentinel-pass.landing_start`: the landing leg as a unit under the naming rule.
pub fn landing_start(e: &Effects, spec: &EffectSpec) -> Result<String, String> {
    let units = e.units.as_deref().ok_or("no unit manager configured")?;
    if matches!(units.facts(&landing_handle()?).map(|f| f.state), Ok(units::UnitState::Active | units::UnitState::Activating)) {
        return Err("a landing is already in flight".into());
    }
    let landing = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).map(|d| d.join("landing-pass")).find(|p| p.is_file()).ok_or("landing-pass not found on PATH")?;
    let pass_env = ["PATH", "SPIRA_RELEASE", "SPIRA_TOML", "HOME", "SPIRA_HOME", "SPIRA_RUN", "SPIRA_DB", "SPIRA_REPO", "SPIRA_REPO_MAP"];
    let env = pass_env.iter().filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v))).collect();
    let unit = units::UnitSpec { task: NAME.into(), key: LANDING_KEY.into(), argv: vec![landing.to_string_lossy().into_owned(), "land".into()], env, cwd: None };
    let props = landing_props(&write_of(&spec.input, LANDING_START)?, &e.world.run_dir);
    let input = serde_json::to_string(&units::StartSpec { unit, props }).map_err(|e| e.to_string())?;
    e.unit(&EffectSpec::keyed(units::UNIT_START, spec.key.clone(), input, spec.deadline_ms))
}

/// The properties CHECK 6's `systemd-run` gave the landing leg (`sentinel/src/dispatch.rs`):
/// `RuntimeMaxSec=SPIRA_LAND_MAXSEC` (none when it is 0) and both streams appended to
/// `$SPIRA_RUN/landing.log`, the tail the land escalation quotes.
pub fn landing_props(w: &Write, run_dir: &std::path::Path) -> units::UnitProps {
    units::UnitProps { runtime_max_ms: (w.max_secs > 0).then_some(w.max_secs * 1000), output_append: Some(run_dir.join("landing.log")), ..units::UnitProps::default() }
}

/// `sentinel-pass.phase_metric`: one `sentinel-phase` row in the metrics store.
pub fn phase_metric(e: &Effects, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{PHASE_METRIC}: {e}"))?;
    let fields: Vec<(String, serde_json::Value)> = v.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    tsd::store::Pusher::new(e.world.metrics_endpoint.clone()).push("sentinel-phase", &tsd::now_iso(), &fields).map_err(|e| e.to_string())?;
    Ok("sentinel-phase".into())
}
