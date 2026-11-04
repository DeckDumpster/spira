//! `intake.filings`: filings product hosts drop in their outbox become builder beads. Replaces
//! `spira-filings-collect-prod` (`bin/filings-collect pass --state run/filings-collect.state`).
//! Pattern P1, with a three-step act per filing. Contract:
//! `wiki/projects/spira/designs/tasks/intake-filings.md`. Not ported yet: [`decide`] and [`step`]
//! are `unimplemented!`, and the row waits in `tasks::f::PENDING`.
//!
//! # Tick
//!
//! Every 10 min (`SPIRA_RUNTIME_INTAKE_FILINGS_SECS`), single-flight, no `world guard`. When
//! `SPIRA_FILINGS_HOSTS` names no host every tick is a no-op.
//!
//! # Observe
//!
//! `intake.filings.observe` returns a [`Snapshot`]: `outbox` (each host's `<home>/filings/outbox`
//! listed over `ssh -o BatchMode=yes -o ConnectTimeout=10`, each unapplied `*.json` parsed by
//! `parse_filing`; ssh stays a class D child) and `open` (for each carried bead, whether its
//! lifecycle row is outside LANDED, SUPERSEDED, DROPPED and DONE, an `LcClient` show).
//!
//! # Decide
//!
//! The pure core is `filings_collect::pass` with `apply`, `new_bead_body` and `append_body`, split
//! into a decision over the snapshot plus a per-filing machine `seen → applied → acked`
//! ([`TRANSITIONS`]). A filing already `applied` in the carry skips the apply and is only
//! acknowledged, so an unacknowledged filing is never applied twice. `apply` appends when the
//! carried bead for the host and class is open and files a new bead otherwise. Carried state is
//! `intake.filings.state { beads, applied }`. Fixtures port `filings-collect/src/tests.rs`
//! (5 cases) and `spira/test-filings-collect-units.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `intake.filings.observe` (probe) | host (ssh) | 60 s per host | tick id | an unparsable `SPIRA_FILINGS_HOSTS` |
//! | `intake.filings.file` | store | 120 s | `<host>/<name>` | a filing already in `applied` |
//! | `intake.filings.append` | store | 120 s | `<host>/<name>` | the same, or a bead no longer open |
//! | `intake.filings.ack` | host (ssh) | 60 s | `<host>/<name>` | a filing not in `applied`; runs only after the Emit that records the apply |
//! | `inbox.append` (shared) | host | 10 s | `<host>/<name>/<reason>` | — |
//!
//! file: `bead.sh file "<title>" --for builder --repo <repo>`, the body plus a `## Build` section
//! when it lacks one, then `Collected from <host>: <name>`. append: `bead.sh amend <id>
//! --append-body` with `## Filing <name> from <host>`. ack: move the file from `outbox/` to
//! `acked/`. inbox: `<iso> [watch:intake.filings] filings-collect: ALARM <host>[/<name>]: <reason>`.
//! The order is file or append → Emit (`applied += name`) → ack → Emit (`applied -= name`).
//!
//! # Silence
//!
//! An idle tick (empty outboxes) emits the probe only, and no Emit when the carry is unchanged. A
//! filing that keeps failing to parse alarms once per `<host>/<name>`, not once per tick.
//!
//! # Replaces
//!
//! `spira-filings-collect-prod.service` and `.timer`; children `ssh` ×(1 + filings) per host,
//! `bead.sh file/amend`, `spira-lc show`. Pinned config: [`KEYS`]; the persona `builder` is a
//! constant.
//!
//! # Reverse
//!
//! Release rollback restores the unit; the carry renders back to the old state file with
//! `State::render`, so the old unit resumes without a double apply. Observe mode shadows `file`,
//! `append` and `ack`, leaving filings in the outbox.

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{commands, Cycle, Transition, INBOX};
use crate::watchers::carried;

pub const NAME: &str = "intake.filings";
pub const STREAM: &str = "task.intake.filings";
pub const TICK: EventKindTag = "intake.filings.tick";
pub const STATE: EventKindTag = "intake.filings.state";

pub const OBSERVE: &str = "intake.filings.observe";
pub const FILE: &str = "intake.filings.file";
pub const APPEND: &str = "intake.filings.append";
pub const ACK: &str = "intake.filings.ack";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE, APPEND, ACK, INBOX];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INTAKE_FILINGS_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_FILINGS_HOSTS", "SPIRA_METRICS_ENDPOINT"];

/// The persona every filed bead is for.
pub const PERSONA: &str = "builder";

const DEADLINE_MS: u64 = 60_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub ssh: String,
    pub home: String,
}

/// `filings_collect::Filing`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub class: String,
    pub action: String,
    pub repo: String,
    pub title: String,
    pub body: String,
}

/// One outbox file: its name and its parse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outboxed {
    pub name: String,
    pub filing: Result<Filing, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub hosts: Vec<Host>,
    pub outbox: BTreeMap<String, Result<Vec<Outboxed>, String>>,
    pub open: BTreeMap<String, Result<bool, String>>,
}

/// `intake.filings.state`: the bead per `host/class`, the filings applied and not yet
/// acknowledged (`host/name`), and the `host/name` alarms already raised.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub beads: BTreeMap<String, String>,
    pub applied: BTreeSet<String>,
    pub alarmed: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilingState {
    Seen,
    Applied,
    Acked,
}

pub const TRANSITIONS: &[Transition<FilingState>] = &[
    Transition { from: FilingState::Seen, on: "file", to: FilingState::Applied, effects: &[FILE] },
    Transition { from: FilingState::Seen, on: "append", to: FilingState::Applied, effects: &[APPEND] },
    Transition { from: FilingState::Applied, on: "ack", to: FilingState::Acked, effects: &[ACK] },
];

/// The filing's next state on `on`, or a refusal naming the state it is in.
pub fn step(_from: FilingState, _on: &str) -> Result<FilingState, String> {
    unimplemented!("intake.filings: not ported yet")
}

/// The pure decision over one snapshot: the next carry and the acts.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("intake.filings: not ported yet")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeFilings {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl IntakeFilings {
    pub fn from_cfg() -> Result<Self, String> {
        Ok(IntakeFilings { interval_ms: super::cadence_ms(CADENCE_KEY)?, config: super::pinned(KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("intake.filings/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for IntakeFilings {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, String::new, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            let (next, acts) = decide(&carry, &snap);
            commands(&carry, &next, STATE, acts)
        })
    }
}
