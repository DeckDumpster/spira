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
//! - Also woken by `bead.handed_on` and `pr.merged`/`pr.closed`.
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
//!   content-store `show` verb (S7a). The lc state comes from `spira_store::lc_state::list_with`
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
pub const KEYS: &[&str] = &["SPIRA_BATCHER_FORGE_ENABLE", "SPIRA_ASK_LABEL", "SPIRA_GIT_NAME", "SPIRA_GIT_EMAIL", "SPIRA_REPO_MAP", "SPIRA_PR_REFRESH_MAX"];
/// `BD_TIMEOUT` is gone: every store read is `LcClient` under its own deadline.
pub const UNREGISTERED: &[&str] = &[];

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
    /// An open pull request whose branch no longer contains the base: it is refreshed.
    #[serde(default)]
    pub behind: bool,
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
#[serde(default)]
pub struct Carry {
    pub branches: BTreeMap<String, BranchCarry>,
}

fn word(state: State) -> String {
    serde_json::to_string(&state).unwrap_or_default().trim_matches('"').to_string()
}

pub fn step(state: State, event: Trigger) -> Result<State, String> {
    TABLE.iter().find(|(s, e, _)| *s == state && *e == event).map(|(_, _, to)| *to).ok_or_else(|| format!("{NAME}: {event:?} is not legal in state {}", word(state)))
}

/// The exit code a transition into `done` carries.
pub fn exit_code(state: State, event: Trigger) -> Option<u8> {
    EXIT.iter().find(|(s, e, _)| *s == state && *e == event).map(|(_, _, c)| *c)
}

/// Everything a branch's steps need, carried in each step's input so its outcome can take the
/// next step without the snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leg {
    pub repo: String,
    pub path: String,
    pub landref: String,
    pub base_fq: String,
    pub base_sha: String,
    pub br: String,
    pub id: String,
    pub tip: String,
    pub pr_state: PrState,
    pub version: u64,
    /// The bead's title, for the pull request's.
    #[serde(default)]
    pub cause: String,
}

/// `SPIRA_PR_REFRESH_MAX`'s registered default: a pull request refreshed this many times is
/// asked about rather than rebased again.
pub const PR_REFRESH_MAX: u32 = 5;

fn act(effect: &str, key: String, leg: &Leg) -> Act {
    Act::new(effect, key).with_input(serde_json::to_string(leg).unwrap_or_default())
}

fn rebase(leg: &Leg) -> Act {
    act(REBASE, format!("{}/{}", leg.br, leg.base_sha), leg)
}

/// The filter of `pr.rs:111-170` in (priority, closed_at) order, then the branch machine's
/// first step for each branch it starts. A branch mid-machine waits for its step's outcome
/// (`on_outcome`); a branch that left the snapshot leaves the carry.
pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<Act>) {
    decide_with(carry, snap, PR_REFRESH_MAX)
}

/// [`decide`] under the pinned `SPIRA_PR_REFRESH_MAX`.
pub fn decide_with(carry: &Carry, snap: &Snapshot, refresh_max: u32) -> (Carry, Vec<Act>) {
    let mut next = Carry { branches: BTreeMap::new() };
    let mut acts = vec![];
    if !snap.forge_enabled {
        return (carry.clone(), acts);
    }
    for r in &snap.repos {
        let mut branches: Vec<&Branch> = r.branches.iter().collect();
        branches.sort_by_key(|b| b.bead.as_ref().map_or((u8::MAX, u64::MAX), |x| (x.priority, x.closed_at_ms.unwrap_or(u64::MAX))));
        for b in branches {
            let Some(bead) = &b.bead else { continue };
            let leg = Leg { repo: r.name.clone(), path: r.path.clone(), landref: r.landref.clone(), base_fq: r.base_fq.clone(), base_sha: r.base_sha.clone(), br: b.br.clone(), id: bead.id.clone(), tip: b.tip.clone(), pr_state: b.pr_state, version: bead.version, cause: String::new() };
            let merge_sha = || b.merge_sha.clone().unwrap_or_else(|| r.base_sha.clone());
            if !bead.handed_on {
                if b.pr_state == PrState::Merged {
                    acts.push(act(DELIVER_PR_MERGED, format!("{}/{}", bead.id, merge_sha()), &Leg { tip: merge_sha(), ..leg }));
                }
                continue;
            }
            if bead.repo != r.name || bead.superseded {
                continue;
            }
            if b.content_on_base {
                acts.push(act(DELIVER_BY_CONTENT, format!("{}/{}", bead.id, bead.version), &leg));
                continue;
            }
            if b.holder_alive {
                if let Some(c) = carry.branches.get(&b.br) {
                    next.branches.insert(b.br.clone(), c.clone());
                }
                continue;
            }
            let mut c = carry.branches.get(&b.br).cloned().unwrap_or_default();
            if !matches!(c.state, State::Start | State::Done) {
                next.branches.insert(b.br.clone(), c);
                continue;
            }
            match b.pr_state {
                PrState::Merged => {
                    acts.push(act(DELIVER_PR_MERGED, format!("{}/{}", bead.id, merge_sha()), &Leg { tip: merge_sha(), ..leg }));
                    c.state = State::Done;
                }
                PrState::Closed => {
                    acts.push(act(DELIVER_PR_CLOSED, format!("{}/pr-closed", bead.id), &leg));
                    c.state = State::Done;
                }
                PrState::None => {
                    acts.push(rebase(&leg));
                    c.state = State::Rebasing;
                }
                PrState::Open if b.behind && c.refreshes >= refresh_max => {
                    let subject = format!("{}: {} refreshed {} times", bead.id, b.br, c.refreshes);
                    acts.push(act(ASK, subject.clone(), &Leg { cause: subject, ..leg }));
                    c.state = State::Done;
                }
                PrState::Open if b.behind => {
                    acts.push(rebase(&leg));
                    c.refreshes += 1;
                    c.state = State::Rebasing;
                }
                // Open and current: already submitted, nothing to do (exit 6).
                PrState::Open => {}
            }
            if c != BranchCarry::default() {
                next.branches.insert(b.br.clone(), c);
            }
        }
    }
    (next, acts)
}

/// A rebase step's answer, as its effect body (law-lifecycle-state-is-not-text): `rebase-stale
/// rebase-branch` exit 0 is `Ok`, exit 1 (a named failure) is `Conflict`. Any other exit, or no
/// exit at all, is the effect failing — no verdict on the branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RebaseOutcome {
    Ok,
    Conflict { detail: String },
}

/// A confine step's answer: `confine.sh`'s exit status and its last line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfineOutcome {
    pub status: i32,
    pub detail: String,
}

/// A branch's next step on its current step's outcome: rebase → confine → force-push → the
/// pull request created (when absent) and armed → done; a conflict, a violation or a refused
/// push ends the machine, a conflict and a violation sending the bead back to rework.
pub fn on_outcome(carry: &Carry, effect: &str, input: &str, ok: bool, body: &str) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let mut acts = vec![];
    let Ok(leg) = serde_json::from_str::<Leg>(input) else { return (next, acts) };
    let Some(c) = next.branches.get_mut(&leg.br) else { return (next, acts) };
    let trigger = match (c.state, effect, ok) {
        (State::Rebasing, REBASE, true) => match serde_json::from_str::<RebaseOutcome>(body) {
            Ok(RebaseOutcome::Ok) => Trigger::RebaseOk,
            Ok(RebaseOutcome::Conflict { .. }) => Trigger::Conflict,
            // An unreadable answer is no verdict: end this pass; the next tick starts again.
            Err(_) => {
                c.state = State::Done;
                return (next, acts);
            }
        },
        // The rebase never answered (a timeout, a usage or config exit, a tool that did not
        // run): not a conflict, so the bead is not sent back to rework.
        (State::Rebasing, REBASE, false) => {
            c.state = State::Done;
            return (next, acts);
        }
        (State::Confining, CONFINE, true) => match serde_json::from_str::<ConfineOutcome>(body) {
            Ok(ConfineOutcome { status: 0, .. }) => Trigger::ConfineOk,
            Ok(ConfineOutcome { status: 2, .. }) => Trigger::Violation,
            _ => Trigger::Inconclusive,
        },
        (State::Confining, CONFINE, false) => Trigger::Inconclusive,
        (State::Pushing, FORCE_PUSH, true) => Trigger::PushOk,
        // A refused push ends the machine for this pass (exit 1); the next tick starts again.
        (State::Pushing, FORCE_PUSH, false) => {
            c.state = State::Done;
            return (next, acts);
        }
        (State::PrOpen, PR_CREATE, _) => {
            acts.push(act(PR_AUTOMERGE, format!("{}/{}", leg.repo, leg.br), &leg));
            return (next, acts);
        }
        (State::PrOpen, PR_AUTOMERGE, _) => Trigger::PrArmed,
        _ => return (next, acts),
    };
    let Ok(to) = step(c.state, trigger) else { return (next, acts) };
    match trigger {
        Trigger::RebaseOk => acts.push(act(CONFINE, format!("{}/{}", leg.id, leg.tip), &leg)),
        Trigger::Conflict => {
            let detail = match serde_json::from_str::<RebaseOutcome>(body) {
                Ok(RebaseOutcome::Conflict { detail }) => detail,
                _ => String::new(),
            };
            acts.push(act(REOPEN, format!("{}/rebase-conflict", leg.id), &Leg { cause: format!("rebase-conflict: conflict: {detail}"), ..leg.clone() }))
        }
        Trigger::Violation => {
            let detail = serde_json::from_str::<ConfineOutcome>(body).map(|o| o.detail).unwrap_or_default();
            acts.push(act(REOPEN, format!("{}/confine-violation", leg.id), &Leg { cause: format!("confine-violation: 2 {detail}"), ..leg.clone() }))
        }
        Trigger::ConfineOk => acts.push(act(FORCE_PUSH, format!("{}/{}", leg.br, leg.tip), &leg)),
        Trigger::PushOk if leg.pr_state == PrState::None => acts.push(act(PR_CREATE, format!("{}/{}", leg.repo, leg.br), &leg)),
        Trigger::PushOk => acts.push(act(PR_AUTOMERGE, format!("{}/{}", leg.repo, leg.br), &leg)),
        _ => {}
    }
    c.state = to;
    (next, acts)
}

// ---------------------------------------------------------------------------------------------
// Runners. The repository map is `spira_config::repos`, the store is `LcClient`, the forge is
// `forge::client` in this process (its one child is `gh`), and git is a `git` child.

fn lc() -> Result<spira_store::lc_call::LcClient, String> {
    Ok(spira_store::lc_call::LcClient::configured()?.within(std::time::Duration::from_secs(5)))
}

fn registry() -> Result<spira_config::repos::Registry, String> {
    let home = spira_config::resolve::locate_home_for_process()?;
    Ok(spira_config::repos::Registry::from_env(std::env::vars().collect(), &home))
}

fn git(path: &str, args: &[&str], secs: u64) -> Option<String> {
    super::gate_check::git(std::path::Path::new(path), args, secs).map(|o| o.trim().to_string())
}

fn forge(verb: &str, path: &str, args: &[&str], input: Option<&[u8]>) -> forge::client::Reply {
    forge::client::call(verb, std::path::Path::new(path), args, input)
}

fn leg_of(input: &str, effect: &str) -> Result<Leg, String> {
    serde_json::from_str(input).map_err(|e| format!("{effect}: bad input: {e}"))
}

/// The snapshot: every pr-mode repository's `spira/*` branches, each with its bead (content
/// over the store's content door, state from the lifecycle rows), its pull request's state,
/// whether its content is already on the base, and whether a live dwarf still holds it.
pub fn observe(e: &crate::Effects) -> Result<String, String> {
    let enabled = spira_config::process::cfg("SPIRA_BATCHER_FORGE_ENABLE")?;
    let forge_enabled = matches!(enabled.trim(), "1" | "true" | "yes");
    if !forge_enabled {
        return serde_json::to_string(&Snapshot::default()).map_err(|e| e.to_string());
    }
    let reg = registry()?;
    let lc = lc()?;
    let mut repos = vec![];
    for name in reg.names().into_iter().filter(|n| reg.land(n) == "pr") {
        let Some(path) = reg.root(&name).filter(|p| std::path::Path::new(p).join(".git").exists()) else { continue };
        let Some(landref) = spira_config::repos::landref(&reg, &path) else { continue };
        let base_fq = spira_config::repos::qualify_base_ref(&landref, &path);
        // `landing.fetch` (`pr.rs:91-93`) runs inside this probe, so an idle tick stays probes only.
        let _ = fetch(&serde_json::to_string(&Leg { path: path.clone(), landref: landref.clone(), ..Leg::default() }).unwrap_or_default());
        let base_sha = git(&path, &["rev-parse", "--verify", "--quiet", &base_fq], 10).unwrap_or_default();
        let refs = git(&path, &["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads/spira/"], 10).unwrap_or_default();
        let refs: Vec<(String, String)> = refs.lines().filter_map(|l| l.split_once(' ').map(|(b, t)| (b.to_string(), t.to_string()))).collect();
        if refs.is_empty() {
            continue;
        }
        let ids: Vec<String> = refs.iter().map(|(b, _)| b.trim_start_matches("spira/").to_string()).collect();
        let rows = spira_store::lc_state::index(spira_store::lc_state::list_ids_on(&lc, &ids)?);
        let mut argv = vec!["content".to_string(), "show".to_string()];
        argv.extend(ids.iter().cloned());
        argv.push("--json".into());
        let shown = lc.answer(&argv).unwrap_or_default();
        let items: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(shown.find(['[', '{']).map_or("[]", |i| &shown[i..])) {
            Ok(serde_json::Value::Array(a)) => a,
            Ok(o @ serde_json::Value::Object(_)) => vec![o],
            _ => vec![],
        };
        let beads: BTreeMap<String, landing_pass::model::BeadRow> = items.iter().filter_map(|i| landing_pass::model::BeadRow::from_json(i, reg.home_repo())).map(|b| (b.id.clone(), b)).collect();
        let mut branches = vec![];
        for (br, tip) in refs {
            let id = br.trim_start_matches("spira/").to_string();
            let row = rows.get(&id);
            let bead = beads.get(&id).map(|b| {
                let state = row.map_or("-".to_string(), |r| r.state.clone());
                BeadRow { id: id.clone(), repo: b.repo.clone(), handed_on: landing_pass::model::handed_on(&state), superseded: b.superseded, priority: b.priority.clamp(0, i64::from(u8::MAX)) as u8, closed_at_ms: None, version: 0 }
            });
            let pr = forge("pr-state", &path, &[&br], None).stdout().map(|s| s.trim().to_string());
            let pr_state = match pr.as_deref() {
                Some("open") => PrState::Open,
                Some("merged") => PrState::Merged,
                Some("closed") => PrState::Closed,
                _ => PrState::None,
            };
            let handed = bead.as_ref().is_some_and(|b| b.handed_on);
            let content_on_base = handed && git(&path, &["cherry", &base_fq, &br], 10).is_some_and(|o| !o.lines().any(|l| l.starts_with('+')));
            let behind = pr_state == PrState::Open && super::gate_check::git_out(std::path::Path::new(&path), &["merge-base", "--is-ancestor", &base_fq, &br], 10).is_ok_and(|(c, _, _)| c == Some(1));
            let holder_alive = handed && hel::reap::holder_alive(&e.world.run_dir, &id);
            branches.push(Branch { br, tip, bead, lc_state: row.map(|r| r.state.clone()), pr_state, merge_sha: None, holder_alive, content_on_base, behind });
        }
        repos.push(Repo { name, path, landref, base_fq, base_sha, branches });
    }
    serde_json::to_string(&Snapshot { forge_enabled, repos }).map_err(|e| e.to_string())
}

/// `git fetch <remote>` in the repository.
pub fn fetch(input: &str) -> Result<String, String> {
    let leg = leg_of(input, FETCH)?;
    let remote = spira_config::repos::ref_remote(&leg.landref, Some(&leg.path)).unwrap_or_else(|| "origin".into());
    git(&leg.path, &["fetch", "--quiet", &remote], 55).map(|_| format!("fetched {remote}")).ok_or_else(|| format!("{FETCH}: git fetch {remote} failed in {}", leg.path))
}

fn deliver(effect: &str, argv: &[&str]) -> Result<String, String> {
    let mut all = vec!["deliver"];
    all.extend_from_slice(argv);
    lc()?.answer(&all).map_err(|e| format!("{effect}: {e}"))
}

/// `spira-lc deliver pr-merged <repo> <id> <br> <merge-sha>`; a decline is the store's answer.
pub fn deliver_pr_merged(input: &str) -> Result<String, String> {
    let leg = leg_of(input, DELIVER_PR_MERGED)?;
    deliver(DELIVER_PR_MERGED, &["pr-merged", &leg.path, &leg.id, &leg.br, &leg.tip])
}

/// `spira-lc deliver pr-closed <id> <reason>`.
pub fn deliver_pr_closed(input: &str) -> Result<String, String> {
    let leg = leg_of(input, DELIVER_PR_CLOSED)?;
    deliver(DELIVER_PR_CLOSED, &["pr-closed", &leg.id, "pull request closed unmerged"])
}

/// `pr.rs:181-213`: the Delivered event on a PR_OPEN delivery row, at its version; no row is
/// `deliver pr-merged` on content proof; a row in any other state is the quiet case.
pub fn deliver_by_content(input: &str) -> Result<String, String> {
    let leg = leg_of(input, DELIVER_BY_CONTENT)?;
    let lc = lc()?;
    let shown = lc.answer(&["show", &leg.id]).map_err(|e| format!("{DELIVER_BY_CONTENT}: show: {e}"))?;
    let Some((state, version)) = landing_pass::pr::delivery_of(shown.as_bytes()) else {
        return deliver(DELIVER_BY_CONTENT, &["pr-merged", &leg.path, &leg.id, &leg.br, &leg.base_sha]);
    };
    if state != "PR_OPEN" {
        return Ok(format!("{}: delivery is {state} — nothing to record", leg.id));
    }
    let kind = serde_json::json!({"Delivered": {"merge_sha": leg.base_sha, "proof": "merge-tree"}}).to_string();
    lc.answer(&["event", "delivery", &leg.id, "--expect", "PR_OPEN", "--version", &version, "--actor", "landing-pass", "--kind", &kind]).map_err(|e| format!("{DELIVER_BY_CONTENT}: {e}"))
}

/// A Spira tool the store has no verb for yet, run as this effect's child.
// FORK-EDGE: spira-claim reopen, rebase-stale rebase-branch, confine.sh and queue-helpers
// git-push are Spira programs; each needs its crate's library (spira-claim, rebase-stale,
// confine, queue-helpers) to become an in-process call.
fn tool(effect: &str, prog: &str, args: &[&str], secs: u64) -> Result<(Option<i32>, String), String> {
    use runtime::ports::Proc;
    let spec = runtime::ports::ProcSpec { prog: prog.into(), args: args.iter().map(|a| a.to_string()).collect(), stdin: vec![], deadline: std::time::Duration::from_secs(secs) };
    let out = runtime::ports::SysProc.run(&spec).map_err(|e| format!("{effect}: {e}"))?;
    Ok((out.code, format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))))
}

/// `bead_reopen <id> <cause> <note>` (`spira-claim reopen`).
pub fn reopen(input: &str) -> Result<String, String> {
    let leg = leg_of(input, REOPEN)?;
    let (cause, note) = leg.cause.split_once(": ").unwrap_or((leg.cause.as_str(), ""));
    let note = format!("{}: {note}", leg.br);
    match tool(REOPEN, "spira-claim", &["reopen", &leg.id, cause, &note], 5)? {
        (Some(0), out) => Ok(out),
        (code, out) => Err(format!("{REOPEN}: spira-claim reopen exited {code:?}: {}", out.trim())),
    }
}

/// `spira_event <kind> <id> <title> <detail>`: `bead::event::emit`, in this process.
pub fn event(e: &crate::Effects, input: &str) -> Result<String, String> {
    let leg = leg_of(input, EVENT)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    bead::event::emit(&e.world.run_dir, 3_600, now, &leg.cause, &leg.id, &format!("{} {}", leg.br, leg.tip), "").map_err(|_| format!("{EVENT}: not written"))?;
    Ok(leg.cause)
}

/// `bd-note <id> <text>` over the store.
pub fn note(input: &str) -> Result<String, String> {
    let leg = leg_of(input, NOTE)?;
    lc()?.answer(&["bd-note", &leg.id, &leg.cause]).map_err(|e| format!("{NOTE}: {e}"))
}

/// `bump_requeue` has no port outside lib.sh yet; the branch's requeue count is the carry's.
pub fn bump_requeue(input: &str) -> Result<String, String> {
    let leg = leg_of(input, BUMP_REQUEUE)?;
    Err(format!("{BUMP_REQUEUE}: {} — lib.sh's bump_requeue has no library to call; the requeue count lives in the landing carry", leg.id))
}

/// The refresh-loop ask, through mail's question kind, unless one with the subject is open.
pub fn ask(e: &crate::Effects, input: &str) -> Result<String, String> {
    let leg = leg_of(input, ASK)?;
    let env = e.concierge_mail.as_deref().or(e.mail.as_deref()).ok_or("no mail environment configured")?;
    let args = mail::cmds::SendArgs { mailbox: "operator", from: Some("landing <landing@spira>"), subject: &leg.cause, kind: "question", default: "", class: "", bead: &leg.id, urgent: false, digest: false, dry_run: false, requested_by: "" };
    let bd = mail::bead::BdCli { bdq: String::new(), conn_retries: 1 };
    let body = format!("## Question\n{}. Its pull request keeps falling behind {} without landing. Rebase it by hand, or let it go?\n", leg.cause, leg.base_fq);
    mail::cmds::send(&bd, env, &args, body).map(|o| o.delivered_path.display().to_string())
}

/// `rebase_branch <br> <onto> <repo> <name>` (`rebase-stale rebase-branch`): a [`RebaseOutcome`]
/// body; any exit but 0 or 1 is the effect failing.
pub fn rebase_branch(input: &str) -> Result<String, String> {
    let leg = leg_of(input, REBASE)?;
    let fmt = registry()?.format(&leg.repo).unwrap_or_default();
    let outcome = match tool(REBASE, "rebase-stale", &["rebase-branch", &leg.br, &leg.base_fq, &leg.path, &leg.repo, &fmt], 115)? {
        (Some(0), _) => RebaseOutcome::Ok,
        (Some(1), out) => RebaseOutcome::Conflict { detail: out.lines().last().unwrap_or("").trim().to_string() },
        (code, out) => return Err(format!("{REBASE}: rebase-stale rebase-branch exited {code:?}: {}", out.trim())),
    };
    serde_json::to_string(&outcome).map_err(|e| format!("{REBASE}: {e}"))
}

/// `confine.sh <id> <br> <repo> <base> <labels>`: a [`ConfineOutcome`] body — status `0` clean,
/// `2` a violation, `5` inconclusive.
pub fn confine(input: &str) -> Result<String, String> {
    let leg = leg_of(input, CONFINE)?;
    let (code, out) = tool(CONFINE, "confine.sh", &[&leg.id, &leg.br, &leg.path, &leg.base_fq, ""], 55)?;
    let outcome = ConfineOutcome { status: code.unwrap_or(5), detail: out.lines().last().unwrap_or("").trim().to_string() };
    serde_json::to_string(&outcome).map_err(|e| format!("{CONFINE}: {e}"))
}

/// `spira_git_push --force-with-lease -u <remote> <br>` (`queue-helpers git-push`).
pub fn force_push(input: &str) -> Result<String, String> {
    let leg = leg_of(input, FORCE_PUSH)?;
    let remote = spira_config::repos::ref_remote(&leg.landref, Some(&leg.path)).unwrap_or_else(|| "origin".into());
    match tool(FORCE_PUSH, "queue-helpers", &["git-push", &leg.path, "-q", "--force-with-lease", "-u", &remote, &leg.br], 55)? {
        (Some(0), _) => Ok(format!("pushed {}", leg.br)),
        (_, out) => Err(format!("{FORCE_PUSH}: {}", out.trim())),
    }
}

/// `forge pr-create <repo> <head> <base> <title>`, refused when the head already has an open PR.
pub fn pr_create(input: &str) -> Result<String, String> {
    let leg = leg_of(input, PR_CREATE)?;
    if forge("pr-list-open", &leg.path, &[], None).stdout().is_some_and(|o| o.lines().any(|l| l.split_once(' ').is_some_and(|(_, b)| b.trim() == leg.br))) {
        return Err(format!("{PR_CREATE}: {} already has an open pull request — refused", leg.br));
    }
    let base = spira_config::repos::ref_branch(&leg.landref);
    let title = format!("{}: {}", leg.id, if leg.cause.is_empty() { "Spira" } else { leg.cause.as_str() });
    let r = forge("pr-create", &leg.path, &[&leg.br, &base, &title], Some(b""));
    r.stdout().ok_or_else(|| format!("{PR_CREATE}: {}", r.err.trim()))
}

/// `forge pr-automerge <repo> <head> <method>`.
pub fn pr_automerge(input: &str) -> Result<String, String> {
    let leg = leg_of(input, PR_AUTOMERGE)?;
    let r = forge("pr-automerge", &leg.path, &[&leg.br, ""], None);
    r.stdout().ok_or_else(|| format!("{PR_AUTOMERGE}: {}", r.err.trim()))
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

    pub fn from_settings(settings: &crate::Settings) -> Self {
        Self::new(kit::pin(settings, NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("landing/boot", STREAM, EventKind::new(TICK, ""))
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Map { name: "branches", drop: &[] }];

impl Handler for Landing {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, _ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry: Carry = kit::carry(&SHAPE, view);
        if ev.kind.tag == TICK {
            return kit::on_tick(&SHAPE, serde_json::to_string(&carry).unwrap_or_default(), self.cadence.next_after(ev.at));
        }
        let Some(spec) = crate::watchers::answered_spec(view, ev) else { return vec![] };
        let (next, acts) = if spec.name == OBSERVE {
            let Some(snap) = kit::observed::<Snapshot>(&SHAPE, view, ev) else { return vec![] };
            decide_with(&carry, &snap, self.pinned.get("SPIRA_PR_REFRESH_MAX").and_then(|v| v.trim().parse().ok()).unwrap_or(PR_REFRESH_MAX))
        } else {
            on_outcome(&carry, &spec.name, &spec.input, ev.kind.tag == EFFECT_SUCCEEDED, &ev.kind.body)
        };
        kit::commands(&SHAPE, ACTS, EMITS, &carry, &next, acts)
    }
}
