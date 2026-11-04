//! `intake.gh`: GitHub issues become work beads or untrusted records. Replaces
//! `spira-gh-intake-prod` (ExecStartPre `world guard`; ExecStart `bin/gh-intake`). Pattern P1.
//! Contract: `wiki/projects/spira/designs/tasks/intake-gh.md`. Not ported yet: [`decide`] and
//! [`step`] are `unimplemented!`, and the row waits in `tasks::f::PENDING`.
//!
//! # Tick
//!
//! Every 15 min (`SPIRA_RUNTIME_INTAKE_GH_SECS`), single-flight; nothing else wakes it. While the
//! world is halted the snapshot carries `halted` and `decide` returns no effects (a per-task read
//! standing in for the S14 halt gate).
//!
//! # Observe
//!
//! `intake.gh.observe` returns a [`Snapshot`]: the targets (`SPIRA_GH_INTAKE_REPO` plus every
//! registered repo with a forge remote, its slug from the `spira-config` registry), the store's
//! ingested refs (the `content` verb under S7), every open-issue page (at most 20, 30 s timeout),
//! and the trust answers GitHub gave inside the probe (`spira:accept` actor, org membership,
//! collaborator permission, the hidden-membership 404), so `decide` makes no calls.
//!
//! # Decide
//!
//! `decide(carry, snap) -> (carry', acts)`, lifted from `logic::run`/`ingest_repo` with every rule
//! verbatim: refuse a zero-bead store; refuse an unresolvable bead repo; the first read of a slug
//! records its open issues as seen and files nothing; a ref already tracked gets a note; trusted
//! authors, own-bot acceptance or `spira:accept` from someone with access become work; everything
//! else gets an untrusted record; one digest mail per slug; a rate-limit deferral alarms after
//! 3 h. Carried state is `intake.gh.state { seen, last_ok_hour }`. Fixtures port
//! `gh-intake/src/tests.rs` (13 cases) and `spira/test-gh-intake.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `intake.gh.observe` (probe) | forge | 120 s | tick id | a run with no App token |
//! | `intake.gh.file_work` | store | 120 s | `github:<slug>#<n>` | a ref already in the store |
//! | `intake.gh.file_untrusted` | store | 120 s | `github-untrusted:<slug>#<n>` | an existing untrusted ref |
//! | `intake.gh.note_seen` | store | 30 s | `<bead-id>/<ref>` | nothing |
//! | `intake.gh.close_promoted` | store | 30 s | `<untrusted-id>` | a non-open bead; closes through `lifecycle_row::close` |
//! | `intake.gh.digest` | store (operator mail) | 30 s | `<slug>/<sorted new numbers>` | an empty list |
//! | `intake.gh.unverifiable` | store (operator mail) | 30 s | `<slug>/<login>` | a repeat login in one pass |
//! | `inbox.append` (shared) | host | 10 s | `<tick>/<slug>` | only the refusal lines, `<iso> [watch:intake.gh] gh-intake: <msg>` |
//!
//! The post-check (`work bead(s) carry no <scope> label`) is the next tick's observe.
//!
//! # Silence
//!
//! An idle tick (no new issues, every number seen) emits only the probe, and no
//! `intake.gh.state` when the seen sets and `last_ok` (carried at hour granularity) are unchanged.
//!
//! # Replaces
//!
//! `spira-gh-intake-prod.service` and `.timer` and their `world guard`; per tick `broker token`,
//! `bdq list/create/note`, `git remote` ×2 per repo, `mail send operator` and the `spira-lc`
//! close. Pinned config: [`KEYS`]. `closeout`, `unlanded-scan` and `backfill` stay library calls.
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; short of that, Observe mode shadows every write.
//! The carried seen set is a superset of the old seen file, so seeding the file from it goes back.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{commands, Cycle, Transition, INBOX};
use crate::watchers::carried;

pub const NAME: &str = "intake.gh";
pub const STREAM: &str = "task.intake.gh";
pub const TICK: EventKindTag = "intake.gh.tick";
pub const STATE: EventKindTag = "intake.gh.state";

pub const OBSERVE: &str = "intake.gh.observe";
pub const FILE_WORK: &str = "intake.gh.file_work";
pub const FILE_UNTRUSTED: &str = "intake.gh.file_untrusted";
pub const NOTE_SEEN: &str = "intake.gh.note_seen";
pub const CLOSE_PROMOTED: &str = "intake.gh.close_promoted";
pub const DIGEST: &str = "intake.gh.digest";
pub const UNVERIFIABLE: &str = "intake.gh.unverifiable";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE_WORK, FILE_UNTRUSTED, NOTE_SEEN, CLOSE_PROMOTED, DIGEST, UNVERIFIABLE, INBOX];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_INTAKE_GH_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_GH_INTAKE_REPO", "SPIRA_GH_INTAKE_BEAD_REPO", "SPIRA_GH_INTAKE_PRIORITY", "SPIRA_GH_INTAKE_API", "SPIRA_SCOPE_LABEL", "SPIRA_PLAN_LABEL", "SPIRA_MAIL_BIN", "SPIRA_METRICS_ENDPOINT"];

const OBSERVE_DEADLINE_MS: u64 = 120_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: OBSERVE_DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub slug: String,
    pub bead_repo: String,
    pub root_ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownRef {
    pub id: String,
    pub ext_ref: String,
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GhIssue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: String,
    pub is_pr: bool,
}

/// One slug's issue pages as the probe read them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pages {
    Issues(Vec<GhIssue>),
    RateLimited { reset: u64 },
    Error(String),
}

/// GitHub's trust answer for one issue, settled inside the probe.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accept {
    pub slug: String,
    pub number: u64,
    pub actor: Option<String>,
    pub has_access: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub targets: Vec<Target>,
    pub store: Vec<KnownRef>,
    pub store_total: u64,
    pub now: u64,
    pub pages: BTreeMap<String, Pages>,
    pub accept: Vec<Accept>,
    /// (slug, login, hidden) for each contributor whose membership GitHub hides.
    pub contributor_hidden: Vec<(String, String, bool)>,
}

/// `intake.gh.state`: the open issue numbers seen per slug and the last good fetch, by hour.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub seen: BTreeMap<String, Vec<u64>>,
    pub last_ok_hour: Option<u64>,
}

/// One issue's life at intake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IssueState {
    Unseen,
    SeenBaseline,
    WorkFiled,
    UntrustedRecorded,
    Promoted,
}

pub const TRANSITIONS: &[Transition<IssueState>] = &[
    Transition { from: IssueState::Unseen, on: "baseline", to: IssueState::SeenBaseline, effects: &[] },
    Transition { from: IssueState::Unseen, on: "trusted", to: IssueState::WorkFiled, effects: &[FILE_WORK] },
    Transition { from: IssueState::Unseen, on: "untrusted", to: IssueState::UntrustedRecorded, effects: &[FILE_UNTRUSTED, DIGEST] },
    Transition { from: IssueState::UntrustedRecorded, on: "accepted", to: IssueState::Promoted, effects: &[FILE_WORK, CLOSE_PROMOTED] },
];

/// The issue's next state on `on`, or a refusal naming the state it is in.
pub fn step(_from: IssueState, _on: &str) -> Result<IssueState, String> {
    unimplemented!("intake.gh: not ported yet")
}

/// The pure decision over one snapshot: the next carry and the acts, each an effect spec.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<EffectSpec>) {
    unimplemented!("intake.gh: not ported yet")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntakeGh {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl IntakeGh {
    pub fn from_cfg() -> Result<Self, String> {
        Ok(IntakeGh { interval_ms: super::cadence_ms(CADENCE_KEY)?, config: super::pinned(KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("intake.gh/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for IntakeGh {
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
