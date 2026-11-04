//! `persona.ops`: the Ops persona's sweep. Replaces `spira-ops-prod.service`
//! (`dwarf --home <prod> ops --sweep --prompt -`, stdin from `run/ops-sweep-prompt.txt`), timer
//! 5 min. Pattern P2: a multi-state machine whose long leg, the Ops claude session, is a transient
//! unit started by `unit.start` and finished by the `unit.ran` the units source submits.
//! Contract: `wiki/projects/spira/designs/tasks/persona-ops.md`. Not ported yet: [`decide`] and
//! [`step`] are `unimplemented!`, and the row waits in `tasks::f::PENDING`.
//!
//! # Tick
//!
//! Every 5 min (`SPIRA_RUNTIME_PERSONA_OPS_SECS`), re-armed on the last pass's completion. Also
//! woken by `watch.tower` emitting its snapshot. Single-flight: a tick in any state but
//! [`State::Idle`] is refused naming the state. Incident claims come through `sentinel.call`.
//!
//! # Observe
//!
//! `persona.ops.observe` returns a [`Snapshot`]: `halted` (the world's halt), `rune_present`
//! (`runes/ops.rune`), `prompt_head` and `prompt` (read from `watch.tower`'s last emitted
//! snapshot, not the file), `live` and `max` (`live` from the scheduler's own unit table, S6
//! `unit.facts`; `RUNE_MAX_CONCURRENT`), `draining` (`world.draining`) and `pause` (the capacity
//! pause: open, paused, or unknown, which fails closed).
//!
//! # Decide
//!
//! `decide(carry, input) -> Result<(carry', acts), refusal>` is pure over the table
//! [`TRANSITIONS`]. Carry is `{ state, run_id, name, started_at }`. Outcome rule kept from
//! `sweep.rs`: a trace segment whose outcome is `unlanded` or `killed` counts as rc 0; every other
//! result keeps its rc. Fixtures (T5) come from `dwarf/src/sweep.rs` over its `Seam`: capacity,
//! draining, paused, statute-missing, normal, unlanded→0.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `persona.ops.observe` | host | 10 s | (probe) | nothing; an unreadable pause reads `Unknown` |
//! | `persona.ops.intake_install` | host | 60 s (unverified) | `ops/<run_id>/intake` | `SPIRA_ALERT_GLOB` unset: a no-op |
//! | `persona.ops.incident_drain` | store | 60 s (unverified) | `ops/<run_id>/drain` | no `SPIRA_DB`; a spooled file it cannot file |
//! | `persona.ops.brief` | host | 30 s | `ops/<run_id>/brief` | a missing core statute; an unresolvable persona model |
//! | `unit.start` (shared) | unit | 2000 s; the session's wall is `RUNE_TIMEOUT_SECONDS` | `ops/<run_id>` as the unit key | a unit of that name already active |
//! | `unit.stop` (shared) | unit | 30 s | `ops/<run_id>/stop` | a unit not running (done) |
//! | `persona.ops.ledger` | host | 5 s | `ops/<run_id>/<awake or done>` | nothing |
//! | `persona.ops.cleanup` | host | 5 s | `ops/<run_id>/cleanup` | nothing; a missing file is success |
//!
//! The `unit.start` input carries the claude argv, stdin from the task file, the log
//! `sweep-ops-<id>.log`, and `SPIRA_DWARF`, `BEADS_ACTOR` and `GIT_*`. The lease side thread goes;
//! liveness comes from the unit. `unit.ran` arrives on the `units` stream and is recognised by
//! `UnitRan::key_for(NAME)`; carrying the state back on [`STREAM`] is part of the port.
//!
//! # Silence
//!
//! The idle tick is Observing → Idle on `SWEEP:NOMINAL`, halted, or rune absent: the probe only,
//! no ledger line, no inbox line, no Emit. Today's nominal `ops.log` line is dropped.
//!
//! # Replaces
//!
//! `spira-ops-prod.service` and `.timer` and their manifest rows; children `world guard`,
//! `release intake install`, `incident.sh drain`, `bash lib.sh _dwarf_snapshot`,
//! `strand dwarf-count`, `bdq memories`, the capacity probe; only `claude` survives, inside the
//! unit. Pinned config: [`KEYS`]; `persona.ops.model` and the `RUNE_*` keys come from the toml
//! persona table and the rune file (unverified that `cfg` reads them).
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; Observe mode shadows every effect and no session
//! starts.

use std::collections::BTreeMap;

use runtime::event::*;
use runtime::units::{UnitRan, UNIT_RAN, UNIT_START, UNIT_STOP};
use serde::{Deserialize, Serialize};

use super::{commands, Transition};
use crate::watchers::{answered_effect, carried};

pub const NAME: &str = "persona.ops";
pub const STREAM: &str = "task.persona.ops";
pub const TICK: EventKindTag = "ops.tick";
pub const DEADLINE: EventKindTag = "ops.deadline";
pub const STATE: EventKindTag = "persona.ops.state";

pub const OBSERVE: &str = "persona.ops.observe";
pub const INTAKE_INSTALL: &str = "persona.ops.intake_install";
pub const INCIDENT_DRAIN: &str = "persona.ops.incident_drain";
pub const BRIEF: &str = "persona.ops.brief";
pub const LEDGER: &str = "persona.ops.ledger";
pub const CLEANUP: &str = "persona.ops.cleanup";
pub const EFFECTS: &[&str] = &[OBSERVE, INTAKE_INSTALL, INCIDENT_DRAIN, BRIEF, UNIT_START, UNIT_STOP, LEDGER, CLEANUP];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_PERSONA_OPS_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_AGENT", "SPIRA_MEMORIES_CACHE", "SPIRA_MEMORIES_CACHE_AGE", "SPIRA_STATUTE_CORE", "SPIRA_STATUTE_CORE_LOCAL", "SPIRA_REPO", "BD_TIMEOUT", "SPIRA_BDQ_CONN_RETRIES", "SPIRA_ALERT_GLOB", "SPIRA_RUN"];

/// The first line of a prompt that means there is nothing to sweep.
pub const NOMINAL: &str = "SWEEP:NOMINAL";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pause {
    Open,
    Paused { left_s: u64 },
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub rune_present: bool,
    pub prompt_head: String,
    pub prompt: String,
    pub live: u32,
    pub max: u32,
    pub draining: bool,
    pub pause: Pause,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Observing,
    Preparing,
    Briefing,
    Starting,
    Running,
    Stopping,
    Recording,
}

/// `persona.ops.state`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub state: State,
    pub run_id: Option<String>,
    pub name: Option<String>,
    pub started_at: Option<u64>,
}

/// What the machine is handed: its tick, the observe answer, an effect's outcome by name, its
/// unit's end (with the trace segment's outcome, for the unlanded/killed→0 rule), or its deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Tick,
    Observed(Snapshot),
    Succeeded { effect: String, body: String },
    Failed { effect: String, error: String },
    UnitRan { ran: UnitRan, outcome: Option<String> },
    Deadline,
}

/// The Decide table of the contract, one row per legal (state, on) pair.
pub const TRANSITIONS: &[Transition<State>] = &[
    Transition { from: State::Idle, on: "tick", to: State::Observing, effects: &[OBSERVE] },
    Transition { from: State::Observing, on: "skip", to: State::Idle, effects: &[] },
    Transition { from: State::Observing, on: "busy", to: State::Idle, effects: &[LEDGER] },
    Transition { from: State::Observing, on: "go", to: State::Preparing, effects: &[INTAKE_INSTALL, INCIDENT_DRAIN] },
    Transition { from: State::Preparing, on: "prepared", to: State::Briefing, effects: &[BRIEF] },
    Transition { from: State::Briefing, on: "statute-missing", to: State::Idle, effects: &[LEDGER] },
    Transition { from: State::Briefing, on: "no-argv", to: State::Idle, effects: &[LEDGER] },
    Transition { from: State::Briefing, on: "briefed", to: State::Starting, effects: &[UNIT_START] },
    Transition { from: State::Starting, on: "started", to: State::Running, effects: &[LEDGER] },
    Transition { from: State::Starting, on: "start-failed", to: State::Idle, effects: &[LEDGER] },
    Transition { from: State::Running, on: "unit.ran", to: State::Recording, effects: &[LEDGER, CLEANUP] },
    Transition { from: State::Running, on: "deadline", to: State::Stopping, effects: &[UNIT_STOP] },
    Transition { from: State::Stopping, on: "unit.ran", to: State::Recording, effects: &[LEDGER, CLEANUP] },
    Transition { from: State::Recording, on: "recorded", to: State::Idle, effects: &[] },
];

/// The machine's next state on `on`, or a refusal naming the state it is in.
pub fn step(_from: State, _on: &str) -> Result<State, String> {
    unimplemented!("persona.ops: not ported yet")
}

/// The pure decision: the next carry and the acts, or a refusal naming the state.
pub fn decide(_carry: &Carry, _input: &Input) -> Result<(Carry, Vec<EffectSpec>), String> {
    unimplemented!("persona.ops: not ported yet")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonaOps {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl PersonaOps {
    pub fn from_cfg() -> Result<Self, String> {
        Ok(PersonaOps { interval_ms: super::cadence_ms(CADENCE_KEY)?, config: super::pinned(KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("persona.ops/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    fn input(view: &Projection, ev: &Event) -> Option<Input> {
        match ev.kind.tag.as_str() {
            TICK if ev.stream.0 == STREAM => Some(Input::Tick),
            DEADLINE if ev.stream.0 == STREAM => Some(Input::Deadline),
            UNIT_RAN => UnitRan::from_event_kind(&ev.kind).filter(|r| r.key_for(NAME).is_some()).map(|ran| Input::UnitRan { ran, outcome: None }),
            EFFECT_SUCCEEDED if ev.stream.0 == STREAM => match answered_effect(view, ev)? {
                n if n == OBSERVE => serde_json::from_str(&ev.kind.body).ok().map(Input::Observed),
                effect => Some(Input::Succeeded { effect, body: ev.kind.body.clone() }),
            },
            EFFECT_FAILED if ev.stream.0 == STREAM => answered_effect(view, ev).map(|effect| Input::Failed { effect, error: ev.kind.body.clone() }),
            _ => None,
        }
    }
}

impl Handler for PersonaOps {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, DEADLINE, UNIT_RAN, EFFECT_SUCCEEDED, EFFECT_FAILED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let Some(input) = Self::input(view, ev) else { return vec![] };
        let carry = carried::<Carry>(view, &StreamKey(STREAM.into()), STATE);
        let mut out = match decide(&carry, &input) {
            Ok((next, acts)) => {
                let (probes, writes): (Vec<EffectSpec>, Vec<EffectSpec>) = acts.into_iter().partition(|a| a.name == OBSERVE);
                let mut out = commands(&carry, &next, STATE, writes);
                out.extend(probes.into_iter().map(Command::Probe));
                out
            }
            Err(refusal) => {
                ctx.event().set("ops.refused", refusal.as_str());
                vec![]
            }
        };
        if input == Input::Tick {
            out.push(Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") });
        }
        out
    }
}
