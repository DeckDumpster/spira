//! Family F, fleet and intake, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`.
//!
//! Every row of this family is written and waits in [`PENDING`] until its task is ported. A row
//! in [`TASKS`] is built at boot, so it must be named in `SPIRA_TASK_MODES` and its keys must be
//! registered config, and both of those live outside this family's files. Moving a row from
//! `PENDING` to `TASKS`, with its `decide` written, is the step that ports the task and turns its
//! T1 green (`tests/task_f_<task>.rs`).

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::process::{cfg, cfg_parse};

use super::Task;
use crate::watchers::{answers_observe, INBOX_APPEND};

pub mod asks_ideas;
pub mod intake_filings;
pub mod intake_gh;
pub mod intake_kit_bump;
pub mod persona_archivist;
pub mod persona_ops;
pub mod trigger_groom;
pub mod trigger_mimir;
pub mod trigger_warden;

pub static TASKS: &[Task] = &[];

/// The rows of this family not yet ported, in the design's catalogue order. `inbox.append` and
/// the `unit.*` effects both persona rows emit are served by `tasks::SHARED`.
pub static PENDING: &[Task] = &[
    Task {
        name: intake_gh::NAME,
        boot: intake_gh::IntakeGh::boot,
        cadence_ms: |_| cadence(intake_gh::CADENCE_KEY),
        effects: intake_gh::EFFECTS,
        runners: &[
            (intake_gh::OBSERVE, |_, _, _| unimplemented!("intake.gh.observe: not ported yet")),
            (intake_gh::FILE_WORK, |_, _, _| unimplemented!("intake.gh.file_work: not ported yet")),
            (intake_gh::FILE_UNTRUSTED, |_, _, _| unimplemented!("intake.gh.file_untrusted: not ported yet")),
            (intake_gh::NOTE_SEEN, |_, _, _| unimplemented!("intake.gh.note_seen: not ported yet")),
            (intake_gh::CLOSE_PROMOTED, |_, _, _| unimplemented!("intake.gh.close_promoted: not ported yet")),
            (intake_gh::DIGEST, |_, _, _| unimplemented!("intake.gh.digest: not ported yet")),
            (intake_gh::UNVERIFIABLE, |_, _, _| unimplemented!("intake.gh.unverifiable: not ported yet")),
        ],
        keys: &[intake_gh::KEYS],
        build: |_| refuse(intake_gh::NAME, intake_gh::IntakeGh::from_cfg()).registration(),
    },
    Task {
        name: trigger_groom::NAME,
        boot: trigger_groom::TriggerGroom::boot,
        cadence_ms: |_| cadence(trigger_groom::CADENCE_KEY),
        effects: trigger_groom::EFFECTS,
        runners: &[
            (trigger_groom::OBSERVE, |_, _, _| unimplemented!("trigger.groom.observe: not ported yet")),
            (trigger_groom::FILE, |_, _, _| unimplemented!("trigger.groom.file: not ported yet")),
        ],
        keys: &[trigger_groom::KEYS],
        build: |_| refuse(trigger_groom::NAME, trigger_groom::TriggerGroom::from_cfg()).registration(),
    },
    Task {
        name: trigger_warden::NAME,
        boot: trigger_warden::TriggerWarden::boot,
        cadence_ms: |_| cadence(trigger_warden::CADENCE_KEY),
        effects: trigger_warden::EFFECTS,
        runners: &[
            (trigger_warden::OBSERVE, |_, _, _| unimplemented!("trigger.warden.observe: not ported yet")),
            (trigger_warden::FILE, |_, _, _| unimplemented!("trigger.warden.file: not ported yet")),
        ],
        keys: &[trigger_warden::KEYS],
        build: |_| refuse(trigger_warden::NAME, trigger_warden::TriggerWarden::from_cfg()).registration(),
    },
    Task {
        name: trigger_mimir::NAME,
        boot: trigger_mimir::TriggerMimir::boot,
        cadence_ms: |_| cadence(trigger_mimir::CADENCE_KEY),
        effects: trigger_mimir::EFFECTS,
        runners: &[
            (trigger_mimir::OBSERVE, |_, _, _| unimplemented!("trigger.mimir.observe: not ported yet")),
            (trigger_mimir::FILE, |_, _, _| unimplemented!("trigger.mimir.file: not ported yet")),
        ],
        keys: &[trigger_mimir::KEYS],
        build: |_| refuse(trigger_mimir::NAME, trigger_mimir::TriggerMimir::from_cfg()).registration(),
    },
    Task {
        name: persona_ops::NAME,
        boot: persona_ops::PersonaOps::boot,
        cadence_ms: |_| cadence(persona_ops::CADENCE_KEY),
        effects: persona_ops::EFFECTS,
        runners: &[
            (persona_ops::OBSERVE, |_, _, _| unimplemented!("persona.ops.observe: not ported yet")),
            (persona_ops::INTAKE_INSTALL, |_, _, _| unimplemented!("persona.ops.intake_install: not ported yet")),
            (persona_ops::INCIDENT_DRAIN, |_, _, _| unimplemented!("persona.ops.incident_drain: not ported yet")),
            (persona_ops::BRIEF, |_, _, _| unimplemented!("persona.ops.brief: not ported yet")),
            (persona_ops::LEDGER, |_, _, _| unimplemented!("persona.ops.ledger: not ported yet")),
            (persona_ops::CLEANUP, |_, _, _| unimplemented!("persona.ops.cleanup: not ported yet")),
        ],
        keys: &[persona_ops::KEYS],
        build: |_| refuse(persona_ops::NAME, persona_ops::PersonaOps::from_cfg()).registration(),
    },
    Task {
        name: persona_archivist::NAME,
        boot: persona_archivist::PersonaArchivist::boot,
        cadence_ms: |_| cadence(persona_archivist::CADENCE_KEY),
        effects: persona_archivist::EFFECTS,
        runners: &[
            (persona_archivist::OBSERVE, |_, _, _| unimplemented!("persona.archivist.observe: not ported yet")),
            (persona_archivist::BRIEF, |_, _, _| unimplemented!("persona.archivist.brief: not ported yet")),
            (persona_archivist::COMMIT, |_, _, _| unimplemented!("persona.archivist.commit: not ported yet")),
            (persona_archivist::SETTLE, |_, _, _| unimplemented!("persona.archivist.settle: not ported yet")),
            (persona_archivist::DIGEST_SEND, |_, _, _| unimplemented!("persona.archivist.digest_send: not ported yet")),
            (persona_archivist::MARK, |_, _, _| unimplemented!("persona.archivist.mark: not ported yet")),
            (persona_archivist::FORGET, |_, _, _| unimplemented!("persona.archivist.forget: not ported yet")),
        ],
        keys: &[persona_archivist::KEYS],
        build: |_| refuse(persona_archivist::NAME, persona_archivist::PersonaArchivist::from_cfg()).registration(),
    },
    Task {
        name: asks_ideas::NAME,
        boot: asks_ideas::AsksIdeas::boot,
        cadence_ms: |_| cadence(asks_ideas::CADENCE_KEY),
        effects: asks_ideas::EFFECTS,
        runners: &[
            (asks_ideas::OBSERVE, |_, _, _| unimplemented!("asks.ideas.observe: not ported yet")),
            (asks_ideas::ASK, |_, _, _| unimplemented!("asks.ideas.ask: not ported yet")),
        ],
        keys: &[asks_ideas::KEYS],
        build: |_| refuse(asks_ideas::NAME, asks_ideas::AsksIdeas::from_cfg()).registration(),
    },
    Task {
        name: intake_filings::NAME,
        boot: intake_filings::IntakeFilings::boot,
        cadence_ms: |_| cadence(intake_filings::CADENCE_KEY),
        effects: intake_filings::EFFECTS,
        runners: &[
            (intake_filings::OBSERVE, |_, _, _| unimplemented!("intake.filings.observe: not ported yet")),
            (intake_filings::FILE, |_, _, _| unimplemented!("intake.filings.file: not ported yet")),
            (intake_filings::APPEND, |_, _, _| unimplemented!("intake.filings.append: not ported yet")),
            (intake_filings::ACK, |_, _, _| unimplemented!("intake.filings.ack: not ported yet")),
        ],
        keys: &[intake_filings::KEYS],
        build: |_| refuse(intake_filings::NAME, intake_filings::IntakeFilings::from_cfg()).registration(),
    },
    Task {
        name: intake_kit_bump::NAME,
        boot: intake_kit_bump::IntakeKitBump::boot,
        cadence_ms: |_| cadence(intake_kit_bump::CADENCE_KEY),
        effects: intake_kit_bump::EFFECTS,
        runners: &[
            (intake_kit_bump::OBSERVE, |_, _, _| unimplemented!("intake.kit_bump.observe: not ported yet")),
            (intake_kit_bump::FILE, |_, _, _| unimplemented!("intake.kit_bump.file: not ported yet")),
            (intake_kit_bump::AMEND, |_, _, _| unimplemented!("intake.kit_bump.amend: not ported yet")),
        ],
        keys: &[intake_kit_bump::KEYS],
        build: |_| refuse(intake_kit_bump::NAME, intake_kit_bump::IntakeKitBump::from_cfg()).registration(),
    },
];

/// The shared effect every family-F task that writes the Concierge inbox emits.
pub const INBOX: &str = INBOX_APPEND;

/// One legal edge of a task's state machine: in `from`, on `on`, the task moves to `to` and asks
/// for `effects`. A task's `TRANSITIONS` is the whole table; any (state, on) pair not in it is
/// refused naming the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition<S: 'static> {
    pub from: S,
    pub on: &'static str,
    pub to: S,
    pub effects: &'static [&'static str],
}

/// A bead a trigger task files: the fields `bead.sh file` / `bdq create` write today.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub title: String,
    pub kind: String,
    pub priority: u8,
    pub labels: Vec<String>,
    pub body: String,
    pub persona: Option<String>,
    pub repo: Option<String>,
}

/// A stateless trigger's decision over one snapshot: skip with a reason (a wide-event field,
/// never an inbox line), or file one bead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Skip(String),
    File(Bead),
}

/// Every key in `keys`, each read through the one config door (pinned at boot). The first key
/// that does not resolve refuses, naming it; there is no default.
pub fn pinned(keys: &[&str]) -> Result<BTreeMap<String, String>, String> {
    keys.iter().map(|k| cfg(k).map(|v| (k.to_string(), v))).collect()
}

/// A cadence key in seconds, as milliseconds, read through the one config door. A value that is
/// not a positive integer refuses, naming the key.
pub fn cadence_ms(key: &str) -> Result<u64, String> {
    match cfg_parse::<u64>(key)? {
        0 => Err(format!("{key} must be a positive number of seconds — refusing")),
        s => Ok(s * 1000),
    }
}

/// [`cadence_ms`] for a row's `cadence_ms`, which cannot return an error: `check-config` has
/// already refused an unresolvable key at boot, so reaching the panic is a boot refusal.
pub fn cadence(key: &str) -> u64 {
    cadence_ms(key).unwrap_or_else(|e| panic!("{e}"))
}

/// A row's `build`, which cannot return an error: the refusal is raised at boot, naming the task.
pub fn refuse<T>(task: &str, built: Result<T, String>) -> T {
    built.unwrap_or_else(|e| panic!("{task}: {e}"))
}

/// What a P1 task's tick probes: on `tick` on `stream`, one memory-only `observe` probe with
/// `deadline_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cycle {
    pub stream: &'static str,
    pub tick: EventKindTag,
    pub observe: &'static str,
    pub deadline_ms: u64,
}

impl Cycle {
    /// The P1 tick: on `tick`, the observe probe with `input` and the next tick `every_ms` on;
    /// on the probe's answer, `answer(body)`. Anything on another stream, and any other outcome,
    /// is ignored.
    pub fn handle(&self, every_ms: u64, view: &Projection, ev: &Event, input: impl FnOnce() -> String, answer: impl FnOnce(&str) -> Vec<Command>) -> Vec<Command> {
        if ev.stream.0 != self.stream {
            return vec![];
        }
        if ev.kind.tag == self.tick {
            return vec![Command::Probe(EffectSpec { name: self.observe.into(), input: input(), deadline_ms: self.deadline_ms }), Command::Timer { at: ev.at + every_ms, kind: EventKind::new(self.tick, "") }];
        }
        if ev.kind.tag != EFFECT_SUCCEEDED || !answers_observe(view, ev, self.observe) {
            return vec![];
        }
        answer(&ev.kind.body)
    }
}

/// The commands for one decision: an Emit of the carry under `tag` only when it changed, then
/// each act as a journaled effect.
pub fn commands<C: Serialize + PartialEq>(before: &C, after: &C, tag: EventKindTag, acts: Vec<EffectSpec>) -> Vec<Command> {
    let mut out = vec![];
    if before != after {
        out.push(Command::Emit(EventKind::new(tag, &serde_json::to_string(after).unwrap_or_default())));
    }
    out.extend(acts.into_iter().map(Command::Effect));
    out
}
