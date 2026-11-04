//! `persona.archivist`: the archivist's sweep over live Concierge transcripts. Replaces
//! `spira-archivist-prod.service` (`archivist sweep`, timer every 5 min). Pattern P2: each
//! archivist claude session runs as a transient unit, started by `unit.start` and finished by
//! `unit.ran`. Contract: `wiki/projects/spira/designs/tasks/persona-archivist.md`. Not ported yet:
//! [`decide`] and [`step`] are `unimplemented!`, and the row waits in `tasks::f::STAGED`.
//!
//! # Tick
//!
//! Every 5 min (`SPIRA_RUNTIME_PERSONA_ARCHIVIST_SECS`); no other wake source. Single-flight: a
//! tick in any state but [`State::Idle`] is refused. `archivist.lock` stays only as an
//! effect-level lock inside `persona.archivist.brief`, because `archivist now` takes it from the
//! CLI.
//!
//! # Observe
//!
//! `persona.archivist.observe` returns a [`Snapshot`]: `paused` (the dwarf capacity pause), the
//! candidates (live `*.jsonl` under `SPIRA_TOKEN_PROJECTS` younger than `SPIRA_ARCHIVIST_IDLE`,
//! with the context meter's `ctx`, `turns` and drift, and the per-session bookkeeping
//! `<sid>.state`, `.covered`, `retry_at`) and `stale` (every `<sid>.state` whose transcript is
//! gone). The meter is a script fork today and should become a library read (unverified).
//!
//! # Decide
//!
//! The pure part is `candidates::would_archive`, `sort_by_drift_desc` and the per-pass budget.
//! Carry is `{ state, run_id, queue, current: {sid, at, tc}, archived }`; the table is
//! [`TRANSITIONS`]. Fixtures come from `run.rs`'s `Seam` double: paused, drift below or above
//! `every`, failed with and without `retry_at`, rc 0/124/capacity/other, a dirty checkout.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `persona.archivist.observe` | host | 30 s | (probe) | an unreadable meter: that session is skipped |
//! | `persona.archivist.brief` | host | 30 s | `archivist/<run_id>/<sid>/brief` | either lock held (75); `runes/archivist.md` missing |
//! | `unit.start` (shared) | unit | `SPIRA_ARCHIVIST_TIMEOUT × (tc+1)`, capped at 1800 s | `archivist/<run_id>/<sid>` as the unit key | a unit of that name already active |
//! | `unit.stop` (shared) | unit | 30 s | `archivist/<run_id>/<sid>/stop` | a unit not running |
//! | `persona.archivist.commit` | git | 150 s | `archivist/<sid>/<at_turn>/commit` | paths outside the wiki; a dirty checkout |
//! | `persona.archivist.settle` | host | 5 s | `archivist/<sid>/<at_turn>/settle` | nothing |
//! | `persona.archivist.digest_send` | host | 30 s | `archivist/digest/<date in SPIRA_TZ>` | an empty `digest.pending`; already sent today |
//! | `persona.archivist.mark` | host | 5 s | `archivist/<run_id>/mark` | nothing |
//! | `persona.archivist.forget` | host | 10 s | `archivist/forget/<sid>` | a sid whose transcript exists |
//!
//! The `unit.start` input carries the claude argv (`--model <SPIRA_ARCHIVIST_MODEL>`, the
//! allowed tools, `--add-dir <wiki>`), the cwd `archivist/cwd`, `SPIRA_MAIL_FROM`,
//! `SPIRA_WORK_ACTOR=archivist` and `SPIRA_WIKI` with `SPIRA_RUNE` unset, stdin from the task file
//! and the log `archivist-<sid>.log`. The effect deadline is the real bound on a session: today
//! `SPIRA_ARCHIVIST_TIMEOUT_RETRIES=3` can scale past the unit's 1800 s (unverified that it fires).
//!
//! # Silence
//!
//! An idle tick (not paused, no candidate at or above `every`, nothing stale) emits the probe only.
//!
//! # Replaces
//!
//! `spira-archivist-prod.service` and `.timer` and their manifest rows; children `bash lib.sh`,
//! `ctx-meter.sh`, `archive.sh lineage`, `mail`, `timeout git` ×4 and `timeout claude`; only `git`
//! and `claude` (inside the unit) remain. Pinned config: [`KEYS`].
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; Observe mode shadows the effects, no session
//! starts and nothing is committed to brain.

use std::collections::BTreeMap;

use runtime::event::*;
use runtime::units::{UnitRan, UNIT_RAN, UNIT_START, UNIT_STOP};
use serde::{Deserialize, Serialize};

use super::{commands, Transition};
use crate::watchers::{answered_effect, carried};

pub const NAME: &str = "persona.archivist";
pub const STREAM: &str = "task.persona.archivist";
pub const TICK: EventKindTag = "archivist.tick";
pub const DEADLINE: EventKindTag = "archivist.deadline";
pub const STATE: EventKindTag = "persona.archivist.state";

pub const OBSERVE: &str = "persona.archivist.observe";
pub const BRIEF: &str = "persona.archivist.brief";
pub const COMMIT: &str = "persona.archivist.commit";
pub const SETTLE: &str = "persona.archivist.settle";
pub const DIGEST_SEND: &str = "persona.archivist.digest_send";
pub const MARK: &str = "persona.archivist.mark";
pub const FORGET: &str = "persona.archivist.forget";
pub const EFFECTS: &[&str] = &[OBSERVE, BRIEF, UNIT_START, UNIT_STOP, COMMIT, SETTLE, DIGEST_SEND, MARK, FORGET];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_PERSONA_ARCHIVIST_SECS";
pub const KEYS: &[&str] = &[
    CADENCE_KEY,
    "SPIRA_DB",
    "SPIRA_RUN",
    "SPIRA_RUNES_DIR",
    "SPIRA_TOKEN_PROJECTS",
    "SPIRA_WIKI",
    "SPIRA_AGENT",
    "SPIRA_TZ",
    "SPIRA_ARCHIVIST_EVERY",
    "SPIRA_ARCHIVIST_IDLE",
    "SPIRA_ARCHIVIST_MODEL",
    "SPIRA_ARCHIVIST_TIMEOUT",
    "SPIRA_ARCHIVIST_PER_PASS",
    "SPIRA_ARCHIVIST_TIMEOUT_RETRIES",
    "SPIRA_ARCHIVIST_FAIL_RETRIES",
    "SPIRA_ARCHIVIST_FAIL_BACKOFF",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub sid: String,
    pub tp: String,
    pub ctx: u64,
    pub turns: u64,
    pub drift: u64,
    pub band: String,
    pub prev_state: String,
    pub retry_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub paused: bool,
    pub candidates: Vec<Candidate>,
    pub stale: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Observing,
    Briefing,
    Starting,
    Running,
    Committing,
    Settling,
    Forgetting,
}

/// The session being archived: its id, the turn it is archived at, and its timeout count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Current {
    pub sid: String,
    pub at: u64,
    pub tc: u32,
}

/// `persona.archivist.state`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub state: State,
    pub run_id: Option<String>,
    pub queue: Vec<String>,
    pub current: Option<Current>,
    pub archived: u32,
}

/// What the machine is handed: its tick, the observe answer, an effect's outcome by name, its
/// unit's end, or its deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Tick,
    Observed(Snapshot),
    Succeeded { effect: String, body: String },
    Failed { effect: String, error: String },
    UnitRan(UnitRan),
    Deadline,
}

/// The Decide table of the contract, one row per legal (state, on) pair. `deadline` in Running
/// (stop the unit, then settle on its `unit.ran` as a timeout) is the table's reading of "the
/// deadline passes and `unit.stop` runs".
pub const TRANSITIONS: &[Transition<State>] = &[
    Transition { from: State::Idle, on: "tick", to: State::Observing, effects: &[OBSERVE] },
    Transition { from: State::Observing, on: "paused", to: State::Idle, effects: &[MARK] },
    Transition { from: State::Observing, on: "nothing", to: State::Idle, effects: &[] },
    Transition { from: State::Observing, on: "stale-only", to: State::Forgetting, effects: &[FORGET] },
    Transition { from: State::Observing, on: "candidates", to: State::Briefing, effects: &[BRIEF] },
    Transition { from: State::Briefing, on: "skip-next", to: State::Briefing, effects: &[BRIEF] },
    Transition { from: State::Briefing, on: "skip-last", to: State::Forgetting, effects: &[FORGET] },
    Transition { from: State::Briefing, on: "briefed", to: State::Starting, effects: &[UNIT_START] },
    Transition { from: State::Starting, on: "started", to: State::Running, effects: &[] },
    Transition { from: State::Running, on: "ran-ok-wiki", to: State::Committing, effects: &[COMMIT] },
    Transition { from: State::Running, on: "ran-ok", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Running, on: "deadline", to: State::Running, effects: &[UNIT_STOP] },
    Transition { from: State::Running, on: "timeout", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Running, on: "capacity", to: State::Forgetting, effects: &[SETTLE] },
    Transition { from: State::Running, on: "ran-failed", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Committing, on: "committed", to: State::Settling, effects: &[SETTLE, DIGEST_SEND] },
    Transition { from: State::Committing, on: "dirty", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Committing, on: "commit-failed", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Settling, on: "next", to: State::Briefing, effects: &[BRIEF] },
    Transition { from: State::Settling, on: "done", to: State::Forgetting, effects: &[FORGET] },
    Transition { from: State::Forgetting, on: "forgotten", to: State::Idle, effects: &[] },
];

/// The machine's next state on `on`, or a refusal naming the state it is in.
pub fn step(_from: State, _on: &str) -> Result<State, String> {
    unimplemented!("persona.archivist: not ported yet")
}

/// The pure decision, with `config` the pinned [`KEYS`] (`SPIRA_WIKI` empty means no wiki): the
/// next carry and the acts, or a refusal naming the state.
pub fn decide(_carry: &Carry, _input: &Input, _config: &BTreeMap<String, String>) -> Result<(Carry, Vec<EffectSpec>), String> {
    unimplemented!("persona.archivist: not ported yet")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonaArchivist {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl PersonaArchivist {
    pub fn from_cfg() -> Result<Self, String> {
        Ok(PersonaArchivist { interval_ms: super::cadence_ms(CADENCE_KEY)?, config: super::pinned(KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("persona.archivist/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    fn input(view: &Projection, ev: &Event) -> Option<Input> {
        match ev.kind.tag.as_str() {
            TICK if ev.stream.0 == STREAM => Some(Input::Tick),
            DEADLINE if ev.stream.0 == STREAM => Some(Input::Deadline),
            UNIT_RAN => UnitRan::from_event_kind(&ev.kind).filter(|r| r.key_for(NAME).is_some()).map(Input::UnitRan),
            EFFECT_SUCCEEDED if ev.stream.0 == STREAM => match answered_effect(view, ev)? {
                n if n == OBSERVE => serde_json::from_str(&ev.kind.body).ok().map(Input::Observed),
                effect => Some(Input::Succeeded { effect, body: ev.kind.body.clone() }),
            },
            EFFECT_FAILED if ev.stream.0 == STREAM => answered_effect(view, ev).map(|effect| Input::Failed { effect, error: ev.kind.body.clone() }),
            _ => None,
        }
    }
}

impl Handler for PersonaArchivist {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, DEADLINE, UNIT_RAN, EFFECT_SUCCEEDED, EFFECT_FAILED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let Some(input) = Self::input(view, ev) else { return vec![] };
        let carry = carried::<Carry>(view, &StreamKey(STREAM.into()), STATE);
        let mut out = match decide(&carry, &input, &self.config) {
            Ok((next, acts)) => {
                let (probes, writes): (Vec<EffectSpec>, Vec<EffectSpec>) = acts.into_iter().partition(|a| a.name == OBSERVE);
                let mut out = commands(&carry, &next, STATE, writes);
                out.extend(probes.into_iter().map(Command::Probe));
                out
            }
            Err(refusal) => {
                ctx.event().set("archivist.refused", refusal.as_str());
                vec![]
            }
        };
        if input == Input::Tick {
            out.push(Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") });
        }
        out
    }
}
