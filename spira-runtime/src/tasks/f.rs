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

use super::{Reads, Task};
use crate::watchers::{answers_observe, INBOX_APPEND};

pub mod incident_file;
pub mod intake_filings;
pub mod intake_gh;
pub mod intake_gh_closeout;
pub mod intake_kit_bump;
pub mod persona_archivist;
pub mod persona_ops;
pub mod trigger_groom;
pub mod trigger_mimir;
pub mod trigger_warden;

pub static TASKS: &[Task] = &[
    Task {
        name: intake_gh::NAME,
        reads: Reads::Polled(&["planning/"]),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: intake_gh::IntakeGh::boot,
        cadence_ms: |s| cadence(s, intake_gh::CADENCE_KEY),
        effects: intake_gh::EFFECTS,
        runners: &[
            (intake_gh::OBSERVE, intake_gh::run_observe),
            (intake_gh::FILE_WORK, intake_gh::run_file_work),
            (intake_gh::FILE_UNTRUSTED, intake_gh::run_file_untrusted),
            (intake_gh::NOTE_SEEN, intake_gh::run_note_seen),
            (intake_gh::CLOSE_PROMOTED, intake_gh::run_close_promoted),
            (intake_gh::DIGEST, intake_gh::run_digest),
            (intake_gh::UNVERIFIABLE, intake_gh::run_unverifiable),
        ],
        keys: &[intake_gh::KEYS],
        build: |s| refuse(intake_gh::NAME, intake_gh::IntakeGh::from_settings(s)).registration(),
    },
    Task {
        name: intake_gh_closeout::NAME,
        reads: Reads::Polled(&["gh-closeout/"]),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: intake_gh_closeout::IntakeGhCloseout::boot,
        cadence_ms: |s| cadence(s, intake_gh_closeout::CADENCE_KEY),
        effects: intake_gh_closeout::EFFECTS,
        runners: &[(intake_gh_closeout::OBSERVE, intake_gh_closeout::run_observe), (intake_gh_closeout::DRAIN, intake_gh_closeout::run_drain)],
        keys: &[intake_gh_closeout::KEYS],
        build: |s| refuse(intake_gh_closeout::NAME, intake_gh_closeout::IntakeGhCloseout::from_settings(s)).registration(),
    },
    Task {
        name: incident_file::NAME,
        reads: Reads::Only(&["incidents/"]),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: incident_file::IncidentFile::boot,
        cadence_ms: |_| incident_file::CADENCE_MS,
        effects: incident_file::EFFECTS,
        runners: &[(incident_file::OBSERVE, incident_file::run_observe), (incident_file::RUN, incident_file::run_file)],
        keys: &[incident_file::KEYS],
        build: |s| refuse(incident_file::NAME, incident_file::IncidentFile::from_settings(s)).registration(),
    },
    Task {
        name: persona_ops::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: persona_ops::PersonaOps::boot,
        cadence_ms: |s| cadence(s, persona_ops::CADENCE_KEY),
        effects: persona_ops::EFFECTS,
        runners: &[
            (persona_ops::OBSERVE, persona_ops::run_observe),
            (persona_ops::INTAKE_INSTALL, persona_ops::run_intake_install),
            (persona_ops::BRIEF, persona_ops::run_brief),
            (persona_ops::LEDGER, persona_ops::run_ledger),
            (persona_ops::CLEANUP, persona_ops::run_cleanup),
        ],
        keys: &[persona_ops::KEYS],
        build: |s| refuse(persona_ops::NAME, persona_ops::PersonaOps::from_settings(s)).registration(),
    },
    Task {
        name: trigger_groom::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: trigger_groom::TriggerGroom::boot,
        cadence_ms: |s| cadence(s, trigger_groom::CADENCE_KEY),
        effects: trigger_groom::EFFECTS,
        runners: &[
            (trigger_groom::OBSERVE, trigger_groom::run_observe),
            (trigger_groom::FILE, trigger_groom::run_file),
        ],
        keys: &[trigger_groom::KEYS],
        build: |s| refuse(trigger_groom::NAME, trigger_groom::TriggerGroom::from_settings(s)).registration(),
    },
    Task {
        name: trigger_warden::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: trigger_warden::TriggerWarden::boot,
        cadence_ms: |s| cadence(s, trigger_warden::CADENCE_KEY),
        effects: trigger_warden::EFFECTS,
        runners: &[
            (trigger_warden::OBSERVE, trigger_warden::run_observe),
            (trigger_warden::FILE, trigger_warden::run_file),
        ],
        keys: &[trigger_warden::KEYS],
        build: |s| refuse(trigger_warden::NAME, trigger_warden::TriggerWarden::from_settings(s)).registration(),
    },
    Task {
        name: trigger_mimir::NAME,
        reads: Reads::Polled(&["planning/"]),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: trigger_mimir::TriggerMimir::boot,
        cadence_ms: |s| cadence(s, trigger_mimir::CADENCE_KEY),
        effects: trigger_mimir::EFFECTS,
        runners: &[
            (trigger_mimir::OBSERVE, trigger_mimir::run_observe),
            (trigger_mimir::FILE, trigger_mimir::run_file),
        ],
        keys: &[trigger_mimir::KEYS],
        build: |s| refuse(trigger_mimir::NAME, trigger_mimir::TriggerMimir::from_settings(s)).registration(),
    },
    Task {
        name: intake_filings::NAME,
        reads: Reads::Polled(&["lifecycle/"]),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: intake_filings::IntakeFilings::boot,
        cadence_ms: |s| cadence(s, intake_filings::CADENCE_KEY),
        effects: intake_filings::EFFECTS,
        runners: &[
            (intake_filings::OBSERVE, intake_filings::run_observe),
            (intake_filings::FILE, intake_filings::run_file),
            (intake_filings::APPEND, intake_filings::run_append),
            (intake_filings::ACK, intake_filings::run_ack),
        ],
        keys: &[intake_filings::KEYS],
        build: |s| refuse(intake_filings::NAME, intake_filings::IntakeFilings::from_settings(s)).registration(),
    },
    Task {
        name: intake_kit_bump::NAME,
        reads: Reads::Polled(&["lifecycle/"]),
        plane: runtime::Plane::Work,
        signal: &[],
        boot: intake_kit_bump::IntakeKitBump::boot,
        cadence_ms: |s| cadence(s, intake_kit_bump::CADENCE_KEY),
        effects: intake_kit_bump::EFFECTS,
        runners: &[
            (intake_kit_bump::OBSERVE, intake_kit_bump::run_observe),
            (intake_kit_bump::FILE, intake_kit_bump::run_file),
            (intake_kit_bump::AMEND, intake_kit_bump::run_amend),
        ],
        keys: &[intake_kit_bump::KEYS],
        build: |s| refuse(intake_kit_bump::NAME, intake_kit_bump::IntakeKitBump::from_settings(s)).registration(),
    },
    Task {
        name: persona_archivist::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: persona_archivist::PersonaArchivist::boot,
        cadence_ms: |s| cadence(s, persona_archivist::CADENCE_KEY),
        effects: persona_archivist::EFFECTS,
        runners: &[
            (persona_archivist::OBSERVE, persona_archivist::run_observe),
            (persona_archivist::BRIEF, persona_archivist::run_brief),
            (persona_archivist::COMMIT, persona_archivist::run_commit),
            (persona_archivist::SETTLE, persona_archivist::run_settle),
            (persona_archivist::DIGEST_SEND, persona_archivist::run_digest_send),
            (persona_archivist::MARK, persona_archivist::run_mark),
            (persona_archivist::FORGET, persona_archivist::run_forget),
        ],
        keys: &[persona_archivist::KEYS],
        build: |s| refuse(persona_archivist::NAME, persona_archivist::PersonaArchivist::from_settings(s)).registration(),
    },
];

/// The rows of this family not yet ported, in the design's catalogue order. `inbox.append` and
/// the `unit.*` effects both persona rows emit are served by `tasks::SHARED`.
pub static PENDING: &[Task] = &[];

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

/// Every key in `keys`, each read from the `Settings` the row is built with (`task_config`, as
/// `Settings::from_lookup` read it), never the process's own config door. The first key the
/// settings do not hold refuses, naming it; there is no default.
pub fn pinned(settings: &crate::Settings, keys: &[&str]) -> Result<BTreeMap<String, String>, String> {
    keys.iter().map(|k| setting(settings, k).map(|v| (k.to_string(), v))).collect()
}

fn setting(settings: &crate::Settings, key: &str) -> Result<String, String> {
    settings.task_config.get(key).cloned().ok_or_else(|| format!("{key} is not in the settings (no row of tasks::all() declares it) — refusing"))
}

/// A cadence key in seconds, as milliseconds, read from `settings`. A value that is not a
/// positive integer refuses, naming the key.
pub fn cadence_ms(settings: &crate::Settings, key: &str) -> Result<u64, String> {
    let raw = setting(settings, key)?;
    match raw.trim().parse::<u64>().map_err(|_| format!("{key} must be a positive number of seconds, got {raw:?} — refusing"))? {
        0 => Err(format!("{key} must be a positive number of seconds — refusing")),
        s => Ok(s * 1000),
    }
}

/// [`cadence_ms`] for a row's `cadence_ms`, which cannot return an error: `check-config` has
/// already refused an unresolvable key at boot, so reaching the panic is a boot refusal.
pub fn cadence(settings: &crate::Settings, key: &str) -> u64 {
    cadence_ms(settings, key).unwrap_or_else(|e| panic!("{e}"))
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
            return vec![Command::Probe(EffectSpec::probe(self.observe, input(), self.deadline_ms)), Command::Timer { at: ev.at + every_ms, kind: EventKind::new(self.tick, "") }];
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

#[cfg(test)]
mod tests {
    /// Every family-F and family-G row builds from the `Settings` it is handed, with no config
    /// door resolved: a planted value there is the cadence pinned, and a key the settings lack
    /// refuses.
    #[test_id(tg5srec)]
    #[test]
    fn the_helpers_read_the_settings_they_are_given_not_the_process_config() {
        let mut s = crate::settings::fixture();
        s.task_config.insert(super::super::intake_filings::CADENCE_KEY.into(), "42".into());
        assert_eq!(super::super::cadence(&s, super::super::intake_filings::CADENCE_KEY), 42_000);
        assert_eq!(super::super::intake_filings::IntakeFilings::from_settings(&s).map(|t| t.interval_ms), Ok(42_000), "the row pins what the settings hold");
        for row in super::super::TASKS.iter().chain(super::super::super::g::TASKS) {
            let _ = (row.build)(&s);
            let _ = (row.cadence_ms)(&s);
        }
        assert!(super::super::pinned(&s, &["SPIRA_PLANTED_NOT_A_KEY"]).is_err(), "a key the settings lack refuses");
        s.task_config.insert(super::super::intake_filings::CADENCE_KEY.into(), "0".into());
        assert!(super::super::cadence_ms(&s, super::super::intake_filings::CADENCE_KEY).is_err());
    }
}
