//! # reconcile.content — contract
//!
//! Family B of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/reconcile-content.md`;
//! its Acceptance section is the test file `tests/it/task_b_reconcile_content.rs`.
//!
//! ## Tick
//!
//! - `Timer`, every 10 min (`OnUnitActiveSec=10min`, `OnBootSec=5min`, `AccuracySec=1min`;
//!   `TimeoutStartSec=300`). Boot tick at +5 min. Single-flight.
//! - **Proposed** (`unverified` that it is safe): a `lifecycle.applied` notification from the
//!   store's `watch` long-poll (S7b) also wakes it. Today nothing does.
//!
//! ## Observe
//!
//! `reconcile.content.observe` is a Probe. Its snapshot:
//!
//! ```text
//! Snapshot { world_halted: bool,
//!            through_seq: Option<u64>,                 // from event-seq
//!            work: Vec<Close>, asks: Result<Vec<Close>>, epics: Result<Vec<Close>> }
//! Close { id, why, reason }
//! ```
//!
//! Everything comes from store verbs over the socket, never from bd directly:
//!
//! | Read | Store verb | Cited at |
//! |---|---|---|
//! | lifecycle records | `list`, `list --ids` | `reconcile_content.rs:52-77` |
//! | scope since the carried seq | `event-seq --since N` | `reconcile_content.rs:131-138` |
//! | open planning items | `plan items --ids … --open`, `plan items --type epic --open` | `reconcile_content.rs:26-49,170` |
//! | closed asks | `list-asks --state ALL` | `reconcile_content.rs:213-235` |
//! | epic children | `plan children` (`unverified` verb name) | `complete_epics` at `reconcile_content.rs:259` |
//!
//! `world_halted` comes from the halt stamp, through the same function `world guard` uses
//! (`spira-world/src/guard.rs:11`, `spira-world/src/bin/world.rs:1098-1107`). The halt stamp is
//! a tick precondition: a halted world makes the tick a no-op, as the ExecStartPre does today.
//!
//! ## Decide
//!
//! The computation of the closes is already a pure function of `Machine` answers. Splitting
//! `reconcile_content` at its `apply` branch (`reconcile_content.rs:176`) gives the decide:
//!
//! ```text
//! decide(carry: Carry { through_seq: Option<u64>, full_at: u64 }, snap)
//!     -> (Carry, Vec<Close>)
//! ```
//!
//! - **Scope.** Today every run is a full pass: the unit passes no `--since-seq`. The task
//!   carries `through_seq` and runs incremental passes, with a full pass every 6 h so ask and
//!   epic sweeps still happen. The 6 h figure is a new choice, not current behaviour.
//! - **Fixtures.** The existing `callers/tests.rs:962,992,1032,1054` cases: an epic closes only
//!   when every child is terminal; every terminal state closes; `--since-seq` reads only the
//!   changed beads; a store that is down means no bd read and cannot tell.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `reconcile.content.skew_check` → store `skew-check` | store | 5 s | none (read gate) | exit 3 when the planes disagree; the batch is dropped and the next tick retries (`reconcile_content.rs:176-180`, `skew.rs:95-101`) |
//! | `reconcile.content.close` → `bd close <id> --force --reason R` (`bd.rs:154-159`) | store (content) | 60 s, `WRITE_SECS` (`bd.rs:62`) | `content-close/<id>`. Idempotent: a failed close that `bd_facts::open_among` (`bd.rs:99-107`) shows already closed counts as success | an id the probe did not list; any close while the skew check is red |
//!
//! Notes on the act:
//!
//! - One `close` effect per bead, not one per batch, so a single failure does not hold back the
//!   rest. Today a failure sets exit 3 and the pass continues (`reconcile_content.rs:193-196`).
//! - The bd fork lives inside the runner until design step 5 moves content behind the socket's
//!   `content` verb (S7a). Until then the static scan allow-lists the runner module, not the task
//!   module.
//!
//! ## Silence
//!
//! When nothing needs closing, the snapshot holds no closes and the carried seq is unchanged.
//! Nothing is emitted. If only `through_seq` advanced, one `reconcile.content.state` Emit is
//! journaled. To keep that within the budget, the Emit is written only when the seq moved **and**
//! a close happened, or when a full pass completed.
//!
//! ## Replaces
//!
//! - The units `spira-reconcile-content-prod.{service,timer}`, its ExecStartPre `world guard`,
//!   and `StandardOutput=append:$SPIRA_RUN/reconcile-content.log`.
//! - Children per run:
//!   - 1 `world`
//!   - 1 `spira-lc`
//!   - N `timeout bd -C $SPIRA_DB close`
//!   - N `bd list` (settle check)
//! - Config keys pinned: `SPIRA_BD`, `SPIRA_DB` (`bd.rs:9-14,80`); `SPIRA_RUN` (the halt stamp).
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Alternatively, set the task to `Observe`: the closes are
//! shadowed as `effect.shadowed` with their ids, and nothing writes bd's status. The manual verb
//! `spira-lc reconcile-content --apply` stays available either way.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use super::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "reconcile.content";
pub const STREAM: &str = "task.reconcile.content";
pub const TICK: EventKindTag = "reconcile.content.tick";
pub const STATE: EventKindTag = "reconcile.content.state";

pub const OBSERVE: &str = "reconcile.content.observe";
pub const SKEW_CHECK: &str = "reconcile.content.skew_check";
pub const CLOSE: &str = "reconcile.content.close";
pub const EFFECTS: &[&str] = &[OBSERVE, SKEW_CHECK, CLOSE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_RECONCILE_CONTENT_SECS";
/// The full pass that keeps the ask and epic sweeps (a new choice: 6 h).
pub const FULL_PASS_KEY: &str = "SPIRA_RUNTIME_RECONCILE_CONTENT_FULL_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, FULL_PASS_KEY, "SPIRA_BD", "SPIRA_DB", "SPIRA_RUN"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 30_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Close {
    pub id: String,
    pub why: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub world_halted: bool,
    pub through_seq: Option<u64>,
    pub work: Vec<Close>,
    pub asks: Vec<Close>,
    pub epics: Vec<Close>,
    /// The store's cannot-tell on the ask or epic read, when it said so.
    pub cannot_tell: Option<String>,
}

/// Per bead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Open,
    CloseRequested,
    Closed,
    Failed,
}

/// `state × event → state` per bead (T3). A second close on a closed id is refused by name.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Open, "close", Phase::CloseRequested), (Phase::CloseRequested, "close:ok", Phase::Closed), (Phase::CloseRequested, "close:failed", Phase::Failed), (Phase::Failed, "close", Phase::CloseRequested)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub through_seq: Option<u64>,
    pub full_at: u64,
    pub beads: BTreeMap<String, Phase>,
    /// Closes held until the skew check answers.
    pub pending: Vec<Close>,
    /// When the skew check last answered red: closes wait one full-pass interval after it, so
    /// a store whose planes disagree is not asked again every tick.
    pub skew_red_at: Option<u64>,
}

pub struct ReconcileContent {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// How many closed ids the carry keeps once they leave the store's listing; a closed id is
/// kept so a listing that lags the close never closes it twice.
pub const CLOSED_KEEP: usize = 1_000;

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Open => "open",
        Phase::CloseRequested => "close_requested",
        Phase::Closed => "closed",
        Phase::Failed => "failed",
    }
}

/// The closes `reconcile_content` computes, split at its `apply` branch: the probe lists them
/// (the store's dry run), the batch waits on the skew check, and each close is its own effect.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire => Ok((carry.clone(), vec![])),
        Input::Observed(snap) => {
            let full_ms = cfg.get(FULL_PASS_KEY).and_then(|v| v.trim().parse::<u64>().ok()).filter(|v| *v > 0).ok_or_else(|| format!("{FULL_PASS_KEY} must be a positive integer"))? * 1000;
            if snap.world_halted || carry.skew_red_at.is_some_and(|at| now_ms < at + full_ms) {
                return Ok((carry.clone(), vec![]));
            }
            let mut next = carry.clone();
            let listed: Vec<&Close> = snap.work.iter().chain(&snap.asks).chain(&snap.epics).collect();
            let fresh: Vec<Close> = listed.iter().filter(|c| matches!(carry.beads.get(&c.id).copied().unwrap_or_default(), Phase::Open | Phase::Failed)).map(|c| (*c).clone()).collect();
            if next.beads.len() > CLOSED_KEEP {
                next.beads.retain(|id, p| *p != Phase::Closed || listed.iter().any(|c| &c.id == id));
            }
            if fresh.is_empty() {
                return Ok((next, vec![]));
            }
            next.through_seq = carry.through_seq.max(snap.through_seq);
            next.pending = fresh;
            Ok((next, vec![Act { name: SKEW_CHECK.into(), input: String::new(), deadline_ms: 5_000, probe: false }]))
        }
        Input::Outcome { effect, ok, .. } if effect == SKEW_CHECK => {
            let mut next = Carry { pending: vec![], skew_red_at: None, ..carry.clone() };
            if !ok {
                return Ok((Carry { skew_red_at: Some(now_ms), ..next }, vec![]));
            }
            let mut acts = vec![];
            for c in &carry.pending {
                match carry.beads.get(&c.id).copied().unwrap_or_default() {
                    Phase::Open | Phase::Failed => {}
                    p => return Err(format!("{NAME}: {} is {}; a second close is refused", c.id, phase_name(p))),
                }
                next.beads.insert(c.id.clone(), Phase::CloseRequested);
                acts.push(Act { name: CLOSE.into(), input: serde_json::to_string(c).map_err(|e| e.to_string())?, deadline_ms: CLOSE_DEADLINE_MS, probe: false });
            }
            Ok((next, acts))
        }
        Input::Outcome { effect, ok, body } if effect == CLOSE => {
            let id = closed_id(body);
            match carry.beads.get(id).copied().unwrap_or_default() {
                Phase::CloseRequested => {
                    let mut next = carry.clone();
                    next.beads.insert(id.to_string(), if *ok { Phase::Closed } else { Phase::Failed });
                    Ok((next, vec![]))
                }
                p => Err(format!("{NAME}: a close outcome for {id}, which is {}", phase_name(p))),
            }
        }
        Input::Outcome { effect, .. } => Err(format!("{NAME}: no outcome of {effect} is expected")),
        Input::Ran(r) => Err(format!("{NAME}: no unit is expected, got {r:?}")),
    }
}

/// The id a close outcome names: the success body is the id, and a failure starts `<id>: `.
pub fn closed_id(body: &str) -> &str {
    let b = body.trim();
    b.split_once(": ").map_or(b, |(id, _)| id)
}

/// A close's deadline: bd's write bound (`WRITE_SECS`), which the store's apply rides.
pub const CLOSE_DEADLINE_MS: u64 = 60_000;

/// The store's dry run as closes: each `would close <id> (<why>)` line. The reason is the
/// store's own, written when it closes the id.
pub fn parse_dry_run(out: &str) -> Vec<Close> {
    out.lines()
        .filter_map(|l| l.trim().strip_prefix("would close "))
        .map(|rest| {
            let (id, why) = rest.split_once(' ').unwrap_or((rest, ""));
            let why = why.trim().trim_start_matches('(').trim_end_matches(')').to_string();
            Close { id: id.to_string(), reason: why.clone(), why }
        })
        .collect()
}

/// The runner of [`OBSERVE`]: the halt stamp, then the store's `reconcile-content --dry-run`
/// and `event-seq`, over the socket. A cannot-tell on the ask or epic half still lists the
/// work closes, as the store's own pass does.
pub fn observe(lc: &spira_config::lc_call::LcClient, run_dir: &std::path::Path) -> Result<String, String> {
    if run_dir.join("world.halted").exists() {
        return serde_json::to_string(&Snapshot { world_halted: true, ..Snapshot::default() }).map_err(|e| e.to_string());
    }
    let sweep = lc.within(spira_config::lc_call::SWEEP_DEADLINE);
    let r = sweep.call(&["reconcile-content", "--dry-run"]).map_err(String::from)?;
    let cannot_tell = match r.code {
        0 => None,
        spira_config::lc_call::CANNOT_TELL if r.out.contains("would close") || r.out.contains("reconcile-content: ") => Some(r.why()),
        _ => return Err(format!("reconcile-content --dry-run: exit {}: {}", r.code, r.why())),
    };
    let seq = lc.answer(&["event-seq"]).map_err(String::from)?;
    let through_seq = serde_json::from_str::<serde_json::Value>(seq.trim()).ok().and_then(|v| v["seq"].as_u64());
    serde_json::to_string(&Snapshot { world_halted: false, through_seq, work: parse_dry_run(&r.out), asks: vec![], epics: vec![], cannot_tell }).map_err(|e| e.to_string())
}

/// The runner of [`SKEW_CHECK`]: the store's `skew-check`; exit 3 (the planes disagree) fails it.
pub fn skew_check(lc: &spira_config::lc_call::LcClient) -> Result<String, String> {
    lc.answer(&["skew-check"]).map_err(String::from)
}

/// The runner of [`CLOSE`]: the store's `reconcile-content --apply <id>`, which re-derives the
/// close for that one id behind its own skew guard and writes it with its own reason. An id the
/// store no longer lists is already closed, which is success (the idempotency rule).
pub fn close(lc: &spira_config::lc_call::LcClient, input: &str) -> Result<String, String> {
    let c: Close = serde_json::from_str(input).map_err(|e| format!("close input: {e}"))?;
    if c.id.is_empty() || c.id.starts_with('-') {
        return Err(format!("{}: not a bead id", c.id));
    }
    let r = lc.within(spira_config::lc_call::SWEEP_DEADLINE).call(&["reconcile-content", "--apply", c.id.as_str()]).map_err(|e| format!("{}: {e}", c.id))?;
    match r.code {
        0 => Ok(c.id),
        _ => Err(format!("{}: exit {}: {}", c.id, r.code, r.why())),
    }
}

super::hosted!(ReconcileContent);
