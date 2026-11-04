//! # landing — contract
//!
//! Task id `landing`, family A of [[scheduler-consolidation-2026-11-03]]. This is the **pr-mode**
//! landing pass, `landing-pass --pass`. For every pr-mode repo it takes every `spira/*` branch
//! whose bead is handed on and does the following:
//!
//! - rebase it
//! - confine it
//! - force-push it
//! - open or refresh its pull request
//! - record merged or closed
//!
//! Paths are relative to `spira-releases/current`.
//!
//! **Code differs from the catalogue in four places:**
//! - `--pass` is pr mode only (`landing-pass/src/cli.rs:47`, `landing-pass/src/main.rs:50,80-161`).
//!   The push, queue and queue.local landings are `landing-pass land` (`main.rs:163-244`), which
//!   is not this unit. They belong with the CHECK 6 caller (`queue.step` or `sentinel`).
//! - `hel` is a library call, not a child (`landing-pass/src/real.rs:1259-1261`).
//! - The children the catalogue misses are `bash` lib.sh (`real.rs:33`), `forge`
//!   (`real.rs:1184-1237`), `confine.sh` (`real.rs:1137`) and `mail` (`real.rs:289`).
//! - The per-branch leg (`landing-pass/src/pr_branch.rs:33`) is a sequential IO chain, so the
//!   catalogue's "P1" understates it. It is P1 per tick with a short per-branch sub-machine.
//!
//! ## Tick
//!
//! - Cadence 90 s (`spira-landing-pass-prod.timer`: OnBootSec 120 s, OnUnitActiveSec 90 s).
//! - Also woken by `bead.handed_on` and `pr.merged`/`pr.closed`, both from `repo_events`
//!   (unverified that it emits them).
//! - Single-flight across the whole task. Today that is `run/landing-pass.lock`
//!   (`main.rs:101-111`).
//! - It is skipped while `world.halted` (`main.rs:97-100`). This becomes the scheduler's world
//!   state, not a file probe.
//!
//! ## Observe
//!
//! The snapshot is
//! `{ forge_enabled, repos: [{ name, path, landref, base_fq, base_sha, branches: [{ br, tip, bead, lc_state, pr_state, holder_alive, content_on_base }] }] }`.
//!
//! - **Repo rows** come from `spira_config::repos::Registry` (`real.rs:103-133,147-157`). This is
//!   already in-process. The `repo_map_ok` check (`main.rs:119-125`) is a refusal, not an idle
//!   state.
//! - **Branches.** `git fetch <base_remote>` (`landing-pass/src/pr.rs:91-93`) is a git effect,
//!   then `spira_refs` (`pr.rs:94`) and `rev_parse`/`content_on_base` (`pr.rs:118,141`). All of
//!   these are git class probes.
//! - **Bead rows.** `bdq show <ids> --json` (`real.rs:644-668`, `pr.rs:99-100`) becomes the
//!   content-store `show` verb (S7a). The lc state comes from `spira_config::lc_state::list_with`
//!   or `row_with` (`real.rs:630-641`), which becomes LcClient `list`/`show`. The per-branch
//!   `bead_lc_state` (`pr_branch.rs:45`) becomes the same projection.
//! - **PR state.** `forge pr-state | pr-mergeability | pr-red | pr-list-open`
//!   (`real.rs:1183-1233`, `pr.rs:117`, `pr_branch.rs:38,66,71,199`). These are forge class
//!   probes.
//! - **Holder liveness** is `hel::reap::holder_alive(run, id)` (`real.rs:1259`). It becomes the
//!   `unit.facts` projection for the dwarf's transient unit (S6).
//! - **Config**, pinned once (S10). Today the lib.sh context seam delivers it
//!   (`real.rs:86-93`, `landing-pass/src/seam.rs:100-133`):
//!   - `SPIRA_BATCHER_FORGE_ENABLE` (`pr.rs:56`, `seam.rs:126`)
//!   - `SPIRA_PR_REFRESH_MAX` (`seam.rs:124`)
//!   - `BD_TIMEOUT` (`seam.rs:101`)
//!   - `SPIRA_ASK_LABEL` (`seam.rs:127`)
//!   - `SPIRA_GIT_NAME` and `SPIRA_GIT_EMAIL` (`seam.rs:115-116`)
//!   - `SPIRA_REPO_MAP` (unit env, `seam.rs:133`)
//!
//! ## Decide
//!
//! - `decide(snap) -> Vec<BranchPlan>` is the filter of `pr.rs:111-170`:
//!   - not in the bead db: skip
//!   - PR merged while not handed on: `deliver_pr_merged`
//!   - not handed on: skip
//!   - repo mismatch: skip
//!   - superseded: skip
//!   - content already on base: `deliver_by_content`
//!   - holder alive: defer
//!   - otherwise: run the branch machine
//! - The order is (priority, closed_at) (`pr.rs:101-108`).
//! - The branch machine (`pr_branch.rs`) has exit codes 0–9, documented at `pr_branch.rs:8-12`.
//!   Its states are `start → merged|closed|rebasing → confining → pushing → pr_open → done`:
//!
//! | state × event | → state |
//! |---|---|
//! | start × pr_state=merged | done (7): `landing.deliver_pr_merged`, else `landing.reopen` (`pr_branch.rs:38-56`) |
//! | start × pr_state=closed | done (8): `landing.deliver_pr_closed` (`pr_branch.rs:57-60`) |
//! | start × open/none | rebasing: `landing.rebase` (`pr_branch.rs:83`) |
//! | rebasing × conflict, or the refresh cap is hit | done (3): `landing.reopen` or `landing.ask` (`pr_branch.rs:91-110`) |
//! | rebasing × ok | confining: `landing.confine` (`pr_branch.rs:121`) |
//! | confining × violation or inconclusive | done (2/5) |
//! | confining × ok | pushing: `landing.force_push` (`pr_branch.rs:166`) |
//! | pushing × ok | pr_open: `landing.pr_create` if absent, then `landing.pr_automerge` (`pr_branch.rs:185-199`) |
//!
//! - Carried state per branch is `{ state, refreshes }`. `requeues_of` (`pr_branch.rs:102`) moves
//!   from the lib.sh seam into this carry.
//! - Fixtures are `landing-pass/src/tests.rs` (2,060 lines, fake `Beads`/`Git`/`Lib`/`Tools`).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `landing.fetch` (`pr.rs:92`) | git | 60 s | `repo/remote` per tick | none |
//! | `landing.deliver_pr_merged` (lib.sh `spira-lc deliver pr-merged`, `real.rs:578-579`) | store | 5 s | `id/merge_sha` | a bead whose content is not on base. spira-lc declines with rc 1 |
//! | `landing.deliver_pr_closed` (`real.rs:581-582`) | store | 5 s | `id/pr-closed` | none |
//! | `landing.deliver_by_content` (`spira-lc show`, then `event delivery --expect PR_OPEN --version`, `pr.rs:181-213`) | store | 5 s | `id/version` | a version mismatch, or a row not PR_OPEN (quiet) |
//! | `landing.reopen` (`real.rs:319-320`) | store | 5 s | `id/cause` | none |
//! | `landing.event` (`real.rs:322-323`) | store | 5 s | `kind/id/tip` | none |
//! | `landing.note` (`real.rs:515-516`) | store | 5 s | `id/hash(text)` | none |
//! | `landing.bump_requeue` (`real.rs:492`) | store | 5 s | `id/tip` | none |
//! | `landing.ask` (rebase-refused/loop/refresh-loop → `mail send … --kind question`, `real.rs:288-300`) | store | 10 s | the ask subject | an open ask with that subject (`real.rs:244-246`) |
//! | `landing.rebase` (lib.sh `Op::Rebase`, `real.rs:477`) | git | 120 s (unverified) | `branch/onto_sha` | a live holder |
//! | `landing.confine` (`confine.sh`, `real.rs:1136-1141`) | git | 60 s (unverified) | `id/tip` | none |
//! | `landing.force_push` (`--force-with-lease`, `real.rs:584-585`) | git | 60 s | `branch/tip` | a lease mismatch |
//! | `landing.pr_create` (`real.rs:1189-1203`) | forge | 30 s | `repo/head` | an open PR for the head (`pr-list-open`) |
//! | `landing.pr_automerge` (`real.rs:1234-1237`) | forge | 30 s | `repo/head` | none |
//!
//! ## Silence
//!
//! An idle tick has one of these:
//!
//! - forge disabled (`pr.rs:56-59`)
//! - no pr-mode repos
//! - no `spira/*` refs
//! - every branch not handed on
//!
//! It emits probes and no effect. The per-branch "not landed — its bead is X" log lines
//! (`pr.rs:126`) become `ctx.event()` fields, not journal writes.
//!
//! ## Replaces
//!
//! - `spira-landing-pass-prod.{service,timer}` and its `world guard` ExecStartPre.
//! - The `landing-pass.lock` flock.
//! - Children: `bash` lib.sh ×N, `timeout bdq`, `spira-lc` (`pr.rs:183-206` plus the lc_state
//!   forks), `forge` ×6 verbs, `confine.sh`, `mail`, and `git`. Git stays a child.
//! - Pinned keys: those listed under Observe.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. Per task, `Observe` mode shadows every `landing.*` write.
//! The probes still run, so `effect.shadowed` gives the per-branch plan for parity.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "landing";
pub const STREAM: &str = "task.landing";
pub const TICK: EventKindTag = "landing.tick";
pub const STATE: EventKindTag = "landing.state";
pub const OBSERVE: &str = "landing.observe";
/// `git fetch <base_remote>` each tick. An idle tick must stay probes only (T2), so the port
/// asks for it as a probe; it is in the Act table for its deadline.
pub const FETCH: &str = "landing.fetch";
pub const DELIVER_PR_MERGED: &str = "landing.deliver_pr_merged";
pub const DELIVER_PR_CLOSED: &str = "landing.deliver_pr_closed";
pub const DELIVER_BY_CONTENT: &str = "landing.deliver_by_content";
pub const REOPEN: &str = "landing.reopen";
pub const EVENT: &str = "landing.event";
pub const NOTE: &str = "landing.note";
pub const BUMP_REQUEUE: &str = "landing.bump_requeue";
pub const ASK: &str = "landing.ask";
pub const REBASE: &str = "landing.rebase";
pub const CONFINE: &str = "landing.confine";
pub const FORCE_PUSH: &str = "landing.force_push";
pub const PR_CREATE: &str = "landing.pr_create";
pub const PR_AUTOMERGE: &str = "landing.pr_automerge";

pub const CADENCE_MS: u64 = 90_000;
pub const OBSERVE_DEADLINE_MS: u64 = 60_000;

pub const ACTS: &[(&str, u64)] = &[
    (FETCH, 60_000),
    (DELIVER_PR_MERGED, 5_000),
    (DELIVER_PR_CLOSED, 5_000),
    (DELIVER_BY_CONTENT, 5_000),
    (REOPEN, 5_000),
    (EVENT, 5_000),
    (NOTE, 5_000),
    (BUMP_REQUEUE, 5_000),
    (ASK, 10_000),
    (REBASE, 120_000),
    (CONFINE, 60_000),
    (FORCE_PUSH, 60_000),
    (PR_CREATE, 30_000),
    (PR_AUTOMERGE, 30_000),
];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, FETCH, DELIVER_PR_MERGED, DELIVER_PR_CLOSED, DELIVER_BY_CONTENT, REOPEN, EVENT, NOTE, BUMP_REQUEUE, ASK, REBASE, CONFINE, FORCE_PUSH, PR_CREATE, PR_AUTOMERGE];
pub const KEYS: &[&str] = &["SPIRA_BATCHER_FORGE_ENABLE", "SPIRA_ASK_LABEL", "SPIRA_GIT_NAME", "SPIRA_GIT_EMAIL", "SPIRA_REPO_MAP"];
pub const UNREGISTERED: &[&str] = &["SPIRA_PR_REFRESH_MAX", "BD_TIMEOUT"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// A branch's bead, as the content store and the lifecycle row report it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeadRow {
    pub id: String,
    pub repo: String,
    pub handed_on: bool,
    pub superseded: bool,
    pub priority: u8,
    pub closed_at_ms: Option<u64>,
    pub version: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    #[default]
    None,
    Open,
    Merged,
    Closed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub br: String,
    pub tip: String,
    /// `None`: not in the bead db.
    pub bead: Option<BeadRow>,
    pub lc_state: Option<String>,
    pub pr_state: PrState,
    pub merge_sha: Option<String>,
    pub holder_alive: bool,
    pub content_on_base: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub name: String,
    pub path: String,
    pub landref: String,
    pub base_fq: String,
    pub base_sha: String,
    pub branches: Vec<Branch>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub forge_enabled: bool,
    pub repos: Vec<Repo>,
}

/// The per-branch machine of `pr_branch.rs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    #[default]
    Start,
    Rebasing,
    Confining,
    Pushing,
    PrOpen,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    PrMerged,
    PrClosed,
    PrOpenOrNone,
    Conflict,
    RefreshCap,
    RebaseOk,
    Violation,
    Inconclusive,
    ConfineOk,
    PushOk,
    /// `pr_automerge` succeeded (not in the contract's table, which gives `pr_open` no exit).
    PrArmed,
}

pub static TABLE: &[(State, Trigger, State)] = &[
    (State::Start, Trigger::PrMerged, State::Done),
    (State::Start, Trigger::PrClosed, State::Done),
    (State::Start, Trigger::PrOpenOrNone, State::Rebasing),
    (State::Rebasing, Trigger::Conflict, State::Done),
    (State::Rebasing, Trigger::RefreshCap, State::Done),
    (State::Rebasing, Trigger::RebaseOk, State::Confining),
    (State::Confining, Trigger::Violation, State::Done),
    (State::Confining, Trigger::Inconclusive, State::Done),
    (State::Confining, Trigger::ConfineOk, State::Pushing),
    (State::Pushing, Trigger::PushOk, State::PrOpen),
    (State::PrOpen, Trigger::PrArmed, State::Done),
];

/// The exit code `pr_branch.rs:8-12` gives each way into `done`, kept for parity.
pub static EXIT: &[(State, Trigger, u8)] = &[
    (State::Start, Trigger::PrMerged, 7),
    (State::Start, Trigger::PrClosed, 8),
    (State::Rebasing, Trigger::Conflict, 3),
    (State::Rebasing, Trigger::RefreshCap, 3),
    (State::Confining, Trigger::Violation, 2),
    (State::Confining, Trigger::Inconclusive, 5),
    (State::PrOpen, Trigger::PrArmed, 0),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchCarry {
    pub state: State,
    pub refreshes: u32,
    /// `requeues_of`, moved from the lib.sh seam.
    pub requeues: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub branches: BTreeMap<String, BranchCarry>,
}

pub fn step(_state: State, _event: Trigger) -> Result<State, String> {
    crate::not_ported!(NAME)
}

/// The exit code a transition into `done` carries.
pub fn exit_code(_state: State, _event: Trigger) -> Option<u8> {
    crate::not_ported!(NAME)
}

/// The filter of `pr.rs:111-170` in (priority, closed_at) order, then the branch machine.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landing {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl Landing {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        Landing { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("landing/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for Landing {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
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
