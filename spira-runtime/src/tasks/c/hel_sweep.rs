//! # hel.sweep — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/hel-sweep.md`;
//! its Acceptance section is the test file `tests/task_c_hel_sweep.rs`.
//!
//! **Pattern (corrections from the contracts).** A P2 machine, not a P1/P4 tick: the state
//! table is `TRANSITIONS`, and a long leg run as a transient unit ends on its `unit.ran`,
//! recognised by `UnitRan::key_for(NAME)`.
//!
//! ## Tick
//!
//! - **Calendar:** `OnCalendar=*-*-* *:35:00`, `RandomizedDelaySec=300`, `AccuracySec=5min`,
//!   `Persistent=true` (unit file). The jitter is drawn from `Ctx::rand_u64` (`runtime/src/event.rs:335`), so it is
//!   deterministic by seed.
//! - **Catch-up:** an overdue tick fires **once**, not once per missed slot. `Command::Timer` is
//!   memory-only (`runtime/src/event.rs:290-300`), so `last_fired` is carried in an emitted
//!   `hel.sweep.state` event; at boot, `next_after(last_fired) <= now` fires one tick immediately.
//! - **Single-flight:** a tick that finds a pass in flight (carry `pass: Some`) re-arms and does nothing.
//! - **Why P2, not one long effect.** Today the pass is one process for up to 900 s
//!   (`hel/src/lib.rs:42-45`, `hel/src/sweep.rs:169-197`), under `TimeoutStartSec=1200`. One
//!   effect holding a `git` permit for 15 minutes breaks law-an-event-loop-never-blocks and
//!   starves the S3 `git` class. So the pass is a **machine**: one bounded observe per repository,
//!   then one bounded effect per branch. The 900 s budget becomes the pass's **deadline in
//!   carried state**: when `now >= started + 900 s` the machine stops issuing effects and ends the
//!   pass. The remainder waits for the next tick (as `sweep.rs:188-197` does today).
//! - **Scope:** every repository (`Scope::All`). The unit passes no `--queue-only`, although
//!   `hel/src/sweep.rs:18` calls this "the daily straggler sweep: queue-mode repos only".
//!   **Code over doc:** the contract keeps `All`.
//!
//! ## Observe
//!
//! Two probes per repository. No raw re-derivation: claims, states and beads come from the
//! components that own them.
//!
//! 1. `hel.sweep.observe` (per repo, class `git`). Snapshot:
//!    `{repo, halted, base:{landref,landrefs,remote}, branches:[{id, br, tip, ahead, content_on_base, merges_clean, cherry_unapplied, landed_by_subject, has_own_commit, only_stacked_base, base_names, worktree}], orphans:[{id, path, br}], rows:{id→state}, beads:{id→{labels, supersedes}}, holders:{id→why}}`.
//!    - `halted`: `run/world.halted` exists. This replaces `ExecStartPre=world guard`
//!      (`spira-world/src/guard.rs:11`, `spira-world/src/bin/world.rs:1098`). The in-process
//!      precedent is `spira-runtime/src/world.rs:524`. Halted ⇒ the snapshot carries nothing else.
//!    - `base`: `spira_config::repos::landrefs` / `ref_remote` (`hel/src/real.rs:182-194`).
//!    - git facts: `hel/src/git.rs:29-137` (spira_branches, ahead, content_on_base, merges_clean,
//!      cherry, landed_by_subject, worktrees). The `fetch` of the base's remote
//!      (`sweep.rs:221-225`, `git.rs:156`) runs inside this probe, and only when the repo has branches.
//!    - `rows`: **S7 `LcClient` `list`**, which replaces `spira-lc list` (`real.rs:205`,
//!      `spira-config/src/lc_state.rs:195`), plus `state` for LANDED (today `timeout 5 spira-lc
//!      state`, `real.rs:270-279`).
//!    - `beads`: the store's `content` verb. Today this is `bdjson list --all --limit 0` through the
//!      lib.sh seam (`real.rs:199`, `hel/src/seam.rs:74`), and the seam fork itself is `real.rs:153`.
//!    - `holders`: `reap::holder_alive` (pidfiles, `hel/src/reap.rs:135`) plus the row's `working()`
//!      (`reap.rs:178-196`). A store that did not answer means **held**: that is the positive control.
//! 2. `hel.sweep.pr_tip` (class `forge`, deadline 10 s). This is `gh pr view <br> --json
//!    state,headRefOid` (`real.rs:288-300`). It is asked only for a submitted or past-builder bead
//!    after every local check has said no (`sweep.rs:114-121`).
//!
//! ## Decide
//!
//! `hel::sweep::disposition` (`sweep.rs:89-144`) and `act` (`sweep.rs:298-372`), refactored to read
//! the snapshot rather than call `World`. Pure: `(Carry, Snapshot) -> (Carry, Vec<Effect>)`.
//! - **Carry:** `{last_fired, pass: Option<{started, repos_left, cursor}>, tally:{sent, failed}}`.
//! - **Dispositions:** SendContentLanded, SendOtherPr, ReapSupersededSafe, ReapSquashMerged,
//!   KeepSupersededUnsafe, KeepUnlanded, OrphanNoBead, ArchiveTerminal (`sweep.rs:37-48`). Round
//!   branches and held beads are skipped (`sweep.rs:244-251`).
//! - **Fixtures:** the existing `hel/src/tests.rs` world doubles, and one fixture per disposition
//!   `code()` (`sweep.rs:53-64`).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `hel.sweep.send` | git | 120 s | `repo:br:tip` | **re-checks the witness mid-send** (`real.rs:230-246`); a path outside `run/worktree` (`reap.rs:342-346`); a failed salvage (`reap.rs:362-364`); CERTIFIED/IN_DELIVERY → `Queued` (`reap.rs:404-406`, another `spira-lc state`); the content fence (`reap.rs:397`). Removes the worktree, the branch, the remote branch (`git.rs:231`) and the `branch:` label (`real.rs:122`, today `bdq` through the seam `seam.rs:84`) |
//! | `hel.sweep.content_on_base` | store | 5 s | `id:merge-tree:<landsha>` | a terminal row (`sweep.rs:344-347`); today `spira-lc content-on-base` (`real.rs:280-287`) |
//! | `hel.sweep.close_on_land` | store | 5 s | `id:sha` | a row that is not submitted (store's rule); today `spira-lc close-on-land` via bash (`seam.rs:83`, `real.rs:247`) |
//! | `hel.sweep.archive` | git | 30 s | `repo:refs/archive/<br>:tip` | a read-back that differs from the tip (`sweep.rs:407-428`); on success it chains `hel.sweep.send` with caller `archived` |
//! | `hel.sweep.free_worktree` | git | 120 s | `id:path` | the same witness, fence and salvage rules as `send` (`sweep.rs:313-319`, `reap.rs:331`) |
//! | `hel.sweep.orphan_worktree` | git | 120 s | `id:path` | a branch that still exists, or a held bead (`sweep.rs:256-292`) |
//! | `hel.sweep.prune` | git | 30 s | `repo:pass` | — (`reap.rs:282`; it repairs rather than prunes a live directory) |
//!
//! The tar salvage (`reap.rs:260`) runs inside `send`, `free_worktree` and `orphan_worktree`; it is
//! an allow-listed child. The reap log (`reap.rs:87`) remains those effects' own record.
//!
//! ## Silence
//!
//! An idle tick produces one `observe` probe per repository and nothing else. That covers a halted
//! world, a repository with no branches, and branches that are all KEEP, SKIP or HELD. Today's
//! `KEEP`/`SKIP` lines (`sweep.rs:199`) go to the wide event as counts, not to the journal. No
//! `hel.sweep.state` is emitted unless the carry changed.
//!
//! ## Replaces
//!
//! - `spira-straggler-sweep-prod.{service,timer}` and its `ExecStartPre=world guard`.
//! - Its children: `spira-lc` list, show, state, content-on-base and close-on-land; `bash -s
//!   hel.seam.sh` with lib.sh, `bdjson` and `bdq`; `timeout`; `gh`. git and tar stay as children.
//! - Pinned keys (S10): `SPIRA_RUN` (`real.rs:346`), `SPIRA_REAPLOG` (`real.rs:336`),
//!   `SPIRA_SUBMITTED_LABEL` and `SPIRA_GH` (`real.rs:71-72`), `GH_TIMEOUT` (an unregistered key, on
//!   the seam: `seam.rs:64`), `SPIRA_HOME`/`SPIRA_RELEASE` (`main.rs:376`), and
//!   `SPIRA_METRICS_ENDPOINT` (`hel/src/telemetry.rs:9`). The budget `hel.sweep.budget_secs = 900`
//!   is a new key.
//!
//! ## Reverse
//!
//! A release rollback restores the unit and timer. Per task, use `Observe` mode (S4): every act is
//! journaled as `effect.shadowed`, which suits this destructive task. A deleted branch is
//! recoverable from `refs/archive/*` and the salvage patch and tar under `run/`.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "hel.sweep";
pub const STREAM: &str = "task.hel.sweep";
pub const TICK: EventKindTag = "hel.sweep.tick";
pub const STATE: EventKindTag = "hel.sweep.state";

pub const OBSERVE: &str = "hel.sweep.observe";
pub const PR_TIP: &str = "hel.sweep.pr_tip";
pub const SEND: &str = "hel.sweep.send";
pub const CONTENT_ON_BASE: &str = "hel.sweep.content_on_base";
pub const CLOSE_ON_LAND: &str = "hel.sweep.close_on_land";
pub const ARCHIVE: &str = "hel.sweep.archive";
pub const FREE_WORKTREE: &str = "hel.sweep.free_worktree";
pub const ORPHAN_WORKTREE: &str = "hel.sweep.orphan_worktree";
pub const PRUNE: &str = "hel.sweep.prune";
pub const EFFECTS: &[&str] = &[OBSERVE, PR_TIP, SEND, CONTENT_ON_BASE, CLOSE_ON_LAND, ARCHIVE, FREE_WORKTREE, ORPHAN_WORKTREE, PRUNE];

/// `OnCalendar=*-*-* *:35:00`.
pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_HEL_SWEEP_CALENDAR";
/// `RandomizedDelaySec=300`, drawn from `Ctx::rand_u64`.
pub const JITTER_KEY: &str = "SPIRA_RUNTIME_HEL_SWEEP_JITTER_SECS";
/// The pass deadline in carried state (900 s); a new key.
pub const BUDGET_KEY: &str = "SPIRA_RUNTIME_HEL_SWEEP_BUDGET_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, JITTER_KEY, BUDGET_KEY, "SPIRA_RUN", "SPIRA_REAPLOG", "SPIRA_SUBMITTED_LABEL", "SPIRA_GH", "SPIRA_GH_TIMEOUT", "SPIRA_RELEASE"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 120_000, cadence_key: CADENCE_KEY, calendar: true, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Base {
    pub landref: String,
    pub landrefs: Vec<String>,
    pub remote: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Branch {
    pub id: String,
    pub br: String,
    pub tip: String,
    pub ahead: u32,
    pub content_on_base: bool,
    pub merges_clean: bool,
    pub cherry_unapplied: u32,
    pub landed_by_subject: bool,
    pub has_own_commit: bool,
    pub only_stacked_base: bool,
    pub base_names: Vec<String>,
    pub worktree: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Orphan {
    pub id: String,
    pub path: String,
    pub br: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bead {
    pub labels: Vec<String>,
    pub supersedes: Vec<String>,
}

/// One repository's observe. Halted carries nothing else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub repo: String,
    pub halted: bool,
    pub base: Base,
    pub branches: Vec<Branch>,
    pub orphans: Vec<Orphan>,
    pub rows: BTreeMap<String, String>,
    pub beads: BTreeMap<String, Bead>,
    /// A bead whose store did not answer is held (the positive control).
    pub holders: BTreeMap<String, String>,
}

/// `sweep.rs:37-48`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    SendContentLanded,
    SendOtherPr,
    ReapSupersededSafe,
    ReapSquashMerged,
    KeepSupersededUnsafe,
    KeepUnlanded,
    OrphanNoBead,
    ArchiveTerminal,
}

/// The P2 pass machine (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Observing,
    Acting,
    BudgetSpent,
}

/// `state × event → state`, as data. A tick while `acting` is refused, naming `acting`. A long
/// leg run as a transient unit ends on its `unit.ran` (`UnitRan::key_for(NAME)`).
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[
    (Phase::Idle, "tick", Phase::Observing),
    (Phase::Observing, "observed:work", Phase::Acting),
    (Phase::Observing, "observed:idle", Phase::Idle),
    (Phase::Acting, "outcome", Phase::Acting),
    (Phase::Acting, "unit.ran", Phase::Acting),
    (Phase::Acting, "done", Phase::Idle),
    (Phase::Acting, "budget", Phase::BudgetSpent),
    (Phase::BudgetSpent, "drained", Phase::Idle),
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pass {
    pub started: u64,
    pub repos_left: Vec<String>,
    pub cursor: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tally {
    pub sent: u32,
    pub failed: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub last_fired: Option<u64>,
    pub pass: Option<Pass>,
    pub tally: Tally,
}

pub struct HelSweep {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `disposition` and `act` over the snapshot rather than `World`.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("hel.sweep: not ported yet")
}

crate::tasks::b::hosted!(HelSweep);
