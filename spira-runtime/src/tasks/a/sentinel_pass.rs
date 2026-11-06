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
//! Ported: CHECK 2's reaper (`sentinel::checks::stale_leases`: a live holder is stopped and
//! reclaimed on a later pass, a dead one reclaimed now), CHECK 3b's queue waiters
//! (`sentinel::checks::decide` over the active blockers, and a labeled bead whose own row
//! LANDED closed), CHECK 6's dispatch of the landing leg (`unit.start` of
//! `spira-run-sentinel-pass-landing`, matched back by `UnitRan::key_for`) and CHECK 7's
//! hand-off. Not ported, and so never asked for by decide: CHECK 1 (pilgrimage.sh), 2b
//! (strand), 2c and 2d, 3 and 3c, the twelve watchtower checks and CHECK 8's judgement; their
//! runners refuse naming what is missing. The content door serves list and show only, so
//! the label, close and recompute writes refuse rather than fork bd.
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

/// The labels the checks read, as pinned; the default is the spellings the contract's fixtures use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Labels {
    pub queue_wait: String,
}

impl Default for Labels {
    fn default() -> Self {
        Labels { queue_wait: "queue-wait".into() }
    }
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
    pub labels: Labels,
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
}

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    TABLE.iter().find(|(s, e, _)| *s == state && *e == event).map(|(_, _, to)| *to).ok_or_else(|| format!("{NAME}: {event:?} is not a move from state {state:?}"))
}

/// A row as the sentinel's checks read it. The contract's fixtures name the leased state
/// `CLAIMED`; the machine's word is `WORKING`, and both are read as the leased state.
fn lc_row(r: &Row) -> sentinel::model::LcRow {
    sentinel::model::LcRow {
        bead_id: r.id.clone(),
        state: if r.state == "CLAIMED" { "WORKING".into() } else { r.state.clone() },
        holder: r.holder.clone(),
        lease_until: r.lease_until_ms.map(|ms| (ms / 1000) as i64),
        holds: r.holds.clone(),
        stack: r.stack.clone(),
        version: Some(r.version.to_string()),
        ..sentinel::model::LcRow::default()
    }
}

fn model_bead(b: &Bead) -> sentinel::model::Bead {
    sentinel::model::Bead {
        id: b.id.clone(),
        title: Some(b.title.clone()),
        status: b.status.clone(),
        labels: b.labels.clone(),
        dependency_count: b.deps.len() as u64,
        dependencies: b.deps.iter().map(|d| sentinel::model::Dep { depends_on_id: Some(d.clone()), r#type: Some("blocks".into()), ..sentinel::model::Dep::default() }).collect(),
        ..sentinel::model::Bead::default()
    }
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
}

fn act(effect: &str, key: impl Into<String>, w: Write) -> Act {
    Act::new(effect, key).with_input(serde_json::to_string(&w).unwrap_or_default())
}

/// The checks in today's order (2, 3b, 6, 7), from the snapshot alone.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    let mut acts = vec![];
    let now = (snap.now_ms / 1000) as i64;
    let mut landing = false;
    if let Some(rows) = &snap.rows {
        let lc: Vec<sentinel::model::LcRow> = rows.iter().map(lc_row).collect();
        // CHECK 2: a live holder is stopped first, and never reclaimed in the pass that stops it.
        if !snap.skip_reclaim {
            let alive = |id: &str| snap.holder_alive.get(id).copied().unwrap_or(false);
            let gone = |h: &str| snap.session_gone.get(h).copied().unwrap_or(false);
            for (id, ago) in sentinel::checks::stale_leases(&lc, now, snap.reclaim_grace_secs as i64, alive, gone) {
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
        // CHECK 3b: queue waiters.
        if let (Some(beads), label) = (&snap.beads, &snap.labels.queue_wait) {
            if !label.is_empty() {
                let labeled: Vec<&Bead> = beads.iter().filter(|b| b.labels.contains(label)).collect();
                let active = sentinel::checks::active_blockers_lc(&lc);
                let mut waiting: HashSet<String> = sentinel::checks::labeled_claimable(&lc, labeled.iter().map(|b| model_bead(b)).collect());
                // A waiter with no lifecycle row yet is read by its content status, as bd's
                // `--status open` was read before the rows existed.
                waiting.extend(labeled.iter().filter(|b| b.status == "open" && !rows.iter().any(|r| r.id == b.id)).map(|b| b.id.clone()));
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
        }
    }
    // CHECK 6: the landing leg, unless one is in flight or this is not the current release.
    if snap.world_running && snap.skew.is_none() && !snap.landing_active {
        acts.push(act(LANDING_START, LANDING_UNIT, Write::default()));
        landing = true;
    }
    // CHECK 7: plan beads ready hand off to the call.
    if snap.plan_ready.is_some_and(|n| n > 0) {
        acts.push(Act::new(CALL_WANTED, format!("plan_ready:{now}")).with_input(serde_json::json!({"plan_ready": snap.plan_ready}).to_string()));
    }
    let mut next = carry.clone();
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
        Registration::new(NAME, self)
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

impl Handler for SentinelPass {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, UNIT_RAN, EFFECT_SUCCEEDED]
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
    let lc = client(e, 5_000);
    let run = e.world.run_dir.clone();
    let mut snap = Snapshot {
        now_ms: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
        world_running: !run.join("world.halted").is_file() && !run.join("world.draining").is_file(),
        reclaim_grace_secs: pin("SPIRA_RECLAIM_GRACE_SECS").trim().parse().unwrap_or(10_800),
        labels: Labels { queue_wait: pin("SPIRA_QUEUE_WAIT_LABEL") },
        ..Snapshot::default()
    };
    snap.skew = std::env::var(spira_config::RELEASE_ENV).ok().filter(|r| !r.is_empty()).and_then(|r| spira_config::release_skew::skew(std::path::Path::new(&r))).map(|s| s.describe());
    snap.beads = answer(&lc, &["content", "list", "--all", "--limit", "0", "--json"]).ok().and_then(|t| sentinel::model::parse_beads(&t).ok()).map(|v| {
        v.into_iter()
            .map(|b| Bead { deps: b.dependencies.iter().filter(|d| d.r#type.as_deref() == Some("blocks")).filter_map(|d| d.target().map(str::to_string)).collect(), title: b.title.clone().unwrap_or_default(), status: b.status.clone(), labels: b.labels.clone(), id: b.id })
            .collect()
    });
    let rows = answer(&lc, &["list"]).ok().and_then(|t| sentinel::model::parse_lc_rows(&t).ok());
    if let Some(rows) = &rows {
        for r in rows.iter().filter(|r| r.state == "WORKING") {
            snap.holder_alive.insert(r.bead_id.clone(), sentinel::hel_verbs::holder_alive(&r.bead_id).unwrap_or(false));
            if let Some(h) = &r.holder {
                snap.session_gone.insert(h.clone(), spira_config::session::session_gone(h, &spira_config::admission::RealProcs));
            }
        }
    }
    snap.rows = rows.map(|v| {
        v.into_iter()
            .map(|r| Row { version: r.version.as_deref().and_then(|v| v.parse().ok()).unwrap_or(0), lease_until_ms: r.lease_until.map(|s| s.max(0) as u64 * 1000), id: r.bead_id, state: r.state, holds: r.holds, holder: r.holder, stack: r.stack })
            .collect()
    });
    let scope = pin("SPIRA_SCOPE_LABEL");
    let plan = if scope.is_empty() { "plan".to_string() } else { format!("{scope},plan") };
    let lookup = |k: &str| req.pinned.get(k).cloned().or_else(|| spira_config::process::cfg(k).ok()).or_else(|| std::env::var(k).ok());
    let o = sentinel::claim::dispatch_in(&["ready-count".to_string(), plan], &mut std::io::empty(), &lookup);
    snap.plan_ready = (o.code == 0).then(|| o.out.trim().parse().ok()).flatten();
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

/// The content writes the store's `content` door does not serve (it answers list and show):
/// refused, never a bd child.
pub fn unserved(spec: &EffectSpec) -> Result<String, String> {
    Err(format!("{}: unserved — the store's content door serves list and show only", spec.name))
}

/// The checks this task does not port yet: refused, naming what is missing.
pub fn not_ported(spec: &EffectSpec) -> Result<String, String> {
    Err(format!("{}: not ported — its check needs a library entry (pilgrimage, strand, watchtower, reflect, mail, event)", spec.name))
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
    let input = serde_json::to_string(&unit).map_err(|e| e.to_string())?;
    e.unit(&EffectSpec { name: units::UNIT_START.into(), input, deadline_ms: spec.deadline_ms })
}

/// `sentinel-pass.phase_metric`: one `sentinel-phase` row in the metrics store.
pub fn phase_metric(e: &Effects, input: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(input).map_err(|e| format!("{PHASE_METRIC}: {e}"))?;
    let fields: Vec<(String, serde_json::Value)> = v.as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    tsd::store::Pusher::new(e.world.metrics_endpoint.clone()).push("sentinel-phase", &tsd::now_iso(), &fields).map_err(|e| e.to_string())?;
    Ok("sentinel-phase".into())
}
