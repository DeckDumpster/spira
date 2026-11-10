//! # hel.sweep — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/hel-sweep.md`;
//! its Acceptance section is the test file `tests/it/task_c_hel_sweep.rs`.
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

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

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
    /// The merged PR's head, asked by the observe only where the rules reach the forge (the
    /// `hel.sweep.pr_tip` read, folded into the per-repo probe).
    pub pr_merged_tip: Option<String>,
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
    /// The repositories still to observe after this one, in registry order: the next probe's
    /// input is the first of them.
    pub repos: Vec<String>,
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
    /// The pass's acts still without an answer, by key: a send leg by `repo:br:tip` (answered
    /// by its unit's `unit.ran`), every other act by `<kind>:<repo>:<id>` (answered by its outcome).
    pub jobs: BTreeMap<String, Job>,
}

/// One act of a pass, kept until it answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    /// The effect that answers it: [`SEND`] (by `unit.ran`), [`ARCHIVE`], [`FREE_WORKTREE`], [`ORPHAN_WORKTREE`].
    pub effect: String,
    pub repo: String,
    pub id: String,
    pub br: String,
    pub tip: String,
    /// Close the bead at the land ref's sha once the send leg ends clean.
    pub close: bool,
}

/// The [`SEND`] effect's input: one branch's send leg.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Send {
    pub repo: String,
    pub id: String,
    pub br: String,
    pub tip: String,
    pub why: String,
    pub caller: String,
    /// Record `content-on-base` (evidence before deleting) before the leg starts.
    pub proof: bool,
}

/// The input of every other act: one branch or worktree of one repository.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Target {
    pub repo: String,
    pub id: String,
    pub br: String,
    pub tip: String,
    pub path: String,
}

pub struct HelSweep {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Observing => "observing",
        Phase::Acting => "acting",
        Phase::BudgetSpent => "budget_spent",
    }
}

fn budget_ms(cfg: &Cfg) -> Result<u64, String> {
    let raw = cfg.get(BUDGET_KEY).ok_or_else(|| format!("{BUDGET_KEY} is read but not declared"))?;
    raw.trim().parse::<u64>().ok().filter(|s| *s > 0).map(|s| s * 1000).ok_or_else(|| format!("{BUDGET_KEY} must be a positive whole number of seconds, got {raw:?}"))
}

/// The snapshot's facts of one branch, as `hel::sweep::judge` reads them.
struct Snap<'a> {
    s: &'a Snapshot,
    b: &'a Branch,
    submitted_label: &'a str,
}

impl hel::sweep::Facts for Snap<'_> {
    fn content_on_base(&self) -> bool {
        self.b.content_on_base
    }
    fn has_bead(&self) -> bool {
        self.s.beads.contains_key(&self.b.id) || self.s.rows.contains_key(&self.b.id)
    }
    fn superseded(&self) -> bool {
        self.s.beads.get(&self.b.id).is_some_and(|b| !b.supersedes.is_empty())
    }
    fn ahead(&self) -> Option<u64> {
        Some(u64::from(self.b.ahead))
    }
    fn merges_clean(&self) -> bool {
        self.b.merges_clean
    }
    fn terminal(&self) -> bool {
        self.s.rows.get(&self.b.id).is_some_and(|r| spira_config::lc_state::is_terminal(r))
    }
    fn submitted(&self) -> bool {
        self.s.beads.get(&self.b.id).is_some_and(|b| b.labels.iter().any(|l| l == self.submitted_label)) || self.s.rows.get(&self.b.id).is_some_and(|r| spira_config::lc_state::past_builder(r))
    }
    fn tip(&self) -> Option<String> {
        Some(self.b.tip.clone()).filter(|t| !t.is_empty())
    }
    fn pr_merged_tip(&self) -> Option<String> {
        self.b.pr_merged_tip.clone()
    }
    fn landed(&self) -> bool {
        self.s.rows.get(&self.b.id).is_some_and(|r| r == "LANDED")
    }
    fn cherry_unapplied(&self) -> bool {
        self.b.cherry_unapplied > 0
    }
    fn landed_by_subject(&self) -> bool {
        self.b.landed_by_subject
    }
}

fn effect(name: &str, key: String, input: &impl Serialize, deadline_ms: u64) -> Result<Act, String> {
    Ok(Act { name: name.into(), key, input: serde_json::to_string(input).map_err(|e| e.to_string())?, deadline_ms, probe: false })
}

fn leg_key(repo: &str, br: &str, tip: &str) -> String {
    format!("{repo}:{br}:{tip}")
}

fn send_leg(jobs: &mut BTreeMap<String, Job>, s: &Snapshot, b: &Branch, caller: &str, proof: bool, close: bool) -> Result<Act, String> {
    jobs.insert(leg_key(&s.repo, &b.br, &b.tip), Job { effect: SEND.into(), repo: s.repo.clone(), id: b.id.clone(), br: b.br.clone(), tip: b.tip.clone(), close });
    let why = if caller == "archived" { "archived".to_string() } else { format!("landed in {}", s.base.landref) };
    effect(SEND, format!("{}:{}:{}", s.repo, b.br, b.tip), &Send { repo: s.repo.clone(), id: b.id.clone(), br: b.br.clone(), tip: b.tip.clone(), why, caller: caller.into(), proof }, 120_000)
}

fn job(jobs: &mut BTreeMap<String, Job>, kind: &str, t: &Target) -> Result<Act, String> {
    jobs.insert(format!("{kind}:{}:{}", t.repo, t.id), Job { effect: kind.into(), repo: t.repo.clone(), id: t.id.clone(), br: t.br.clone(), tip: t.tip.clone(), close: false });
    effect(kind, format!("{}:{}:{}:{}:{}:{kind}", t.repo, t.id, t.br, t.tip, t.path), t, if kind == ARCHIVE { 30_000 } else { 120_000 })
}

/// One repository's acts: `hel::sweep::judge` per branch (round branches and held beads
/// skipped), then the orphaned worktrees.
pub fn plan(cfg: &Cfg, s: &Snapshot, jobs: &mut BTreeMap<String, Job>) -> Result<Vec<Act>, String> {
    use hel::sweep::Disp;
    let label = cfg.get("SPIRA_SUBMITTED_LABEL").cloned().unwrap_or_default();
    let mut acts = vec![];
    for b in &s.branches {
        if b.id.starts_with("round-") || s.holders.contains_key(&b.id) {
            continue;
        }
        let target = Target { repo: s.repo.clone(), id: b.id.clone(), br: b.br.clone(), tip: b.tip.clone(), path: b.worktree.clone().unwrap_or_default() };
        let terminal = s.rows.get(&b.id).is_some_and(|r| spira_config::lc_state::is_terminal(r));
        match hel::sweep::judge(&Snap { s, b, submitted_label: &label }) {
            Disp::KeepUnlanded => {}
            Disp::KeepSupersededUnsafe => {
                if b.worktree.is_some() {
                    acts.push(job(jobs, FREE_WORKTREE, &target)?);
                }
            }
            Disp::OrphanNoBead | Disp::ArchiveTerminal => acts.push(job(jobs, ARCHIVE, &target)?),
            Disp::SendContentLanded => {
                let ahead = u64::from(b.ahead);
                if hel::sweep::needs_evidence(ahead, || b.has_own_commit, || b.only_stacked_base, || !b.base_names.is_empty()) {
                    continue;
                }
                acts.push(send_leg(jobs, s, b, "hel", ahead > 0 && !terminal, true)?);
            }
            Disp::SendOtherPr | Disp::ReapSquashMerged => acts.push(send_leg(jobs, s, b, "hel", false, true)?),
            Disp::ReapSupersededSafe => acts.push(send_leg(jobs, s, b, "hel", false, false)?),
        }
    }
    for o in &s.orphans {
        if s.holders.contains_key(&o.id) {
            continue;
        }
        acts.push(job(jobs, ORPHAN_WORKTREE, &Target { repo: s.repo.clone(), id: o.id.clone(), br: o.br.clone(), tip: String::new(), path: o.path.clone() })?);
    }
    Ok(acts)
}

fn observe_next(repo: &str) -> Act {
    Act { name: OBSERVE.into(), key: OBSERVE.into(), input: repo.into(), deadline_ms: SPEC.probe_deadline_ms, probe: true }
}

/// After an answer: while jobs remain, wait; once the repository's are all in, prune it and
/// observe the next repository, or end the pass.
fn settle(mut next: Carry, repo: &str, mut acts: Vec<Act>) -> Result<(Carry, Vec<Act>), String> {
    if !next.jobs.is_empty() {
        return Ok((next, acts));
    }
    if !repo.is_empty() {
        acts.push(effect(PRUNE, format!("{repo}:prune"), &Target { repo: repo.into(), ..Target::default() }, 30_000)?);
    }
    match next.pass.as_mut().filter(|p| !p.repos_left.is_empty()) {
        Some(p) => {
            let r = p.repos_left.remove(0);
            p.cursor += 1;
            next.phase = Phase::Observing;
            acts.push(observe_next(&r));
        }
        None => {
            next.phase = Phase::Idle;
            next.pass = None;
        }
    }
    Ok((next, acts))
}

/// The job an outcome answers: the one its body names, else the first of that effect.
fn answered_job(carry: &Carry, effect: &str, body: &str) -> Option<String> {
    let body = body.trim();
    carry.jobs.get(body).filter(|j| j.effect == effect).map(|_| body.to_string()).or_else(|| carry.jobs.iter().find(|(_, j)| j.effect == effect).map(|(k, _)| k.clone()))
}

/// `disposition` and `act` over the snapshot rather than `World`.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    let in_pass = matches!(carry.phase, Phase::Acting | Phase::Observing) && carry.pass.is_some();
    if in_pass && !matches!(input, Input::Fire) {
        let started = carry.pass.as_ref().map_or(now_ms, |p| p.started);
        let budget = budget_ms(cfg)?;
        if now_ms >= started + budget {
            // A pass far past its budget whose answers never came is forgotten, not waited on.
            let next = if now_ms >= started + 2 * budget { Carry { last_fired: carry.last_fired, tally: carry.tally.clone(), ..Carry::default() } } else { Carry { phase: Phase::BudgetSpent, pass: None, jobs: BTreeMap::new(), ..carry.clone() } };
            if matches!(input, Input::Observed(_)) && next.phase == Phase::Idle {
                return decide(cfg, &next, input, now_ms);
            }
            return Ok((next, vec![]));
        }
    }
    match input {
        Input::Fire if carry.phase == Phase::Acting => Err(format!("{NAME}: a tick in state acting: {} act(s) of the pass have not answered", carry.jobs.len())),
        Input::Fire => Ok((Carry { phase: Phase::Observing, ..carry.clone() }, vec![])),
        // Single-flight: the tick's observation while a pass acts changes nothing.
        Input::Observed(_) if carry.phase == Phase::Acting => Ok((carry.clone(), vec![])),
        Input::Observed(s) if s.halted => Ok((carry.clone(), vec![])),
        Input::Observed(s) => {
            let mut next = carry.clone();
            if carry.phase == Phase::BudgetSpent {
                next.phase = Phase::Idle;
            }
            let continuing = next.phase == Phase::Observing && next.pass.is_some();
            let mut jobs = BTreeMap::new();
            let acts = plan(cfg, s, &mut jobs)?;
            if acts.is_empty() {
                if continuing {
                    return settle(next, "", vec![]);
                }
                // Nothing here; the remaining repositories are observed in turn, unjournaled.
                return Ok(match s.repos.first() {
                    Some(r) => (if next.phase == Phase::Observing { Carry { phase: Phase::Idle, ..next } } else { next }, vec![observe_next(r)]),
                    None => (if next.phase == Phase::Observing { Carry { phase: Phase::Idle, ..next } } else { next }, vec![]),
                });
            }
            if !continuing {
                next.pass = Some(Pass { started: now_ms, repos_left: s.repos.clone(), cursor: 0 });
                next.last_fired = Some(now_ms);
            }
            next.phase = Phase::Acting;
            next.jobs = jobs;
            Ok((next, acts))
        }
        Input::Ran(r) => {
            let key = r.key_for(NAME).ok_or_else(|| format!("{NAME}: a unit that is not this task's: {:?}", r.handle))?;
            let Some(j) = carry.jobs.get(&key).cloned() else {
                return match carry.phase {
                    Phase::Acting => Err(format!("{NAME}: in state acting no leg {key} is in flight")),
                    _ => Ok((carry.clone(), vec![])),
                };
            };
            let mut next = carry.clone();
            next.jobs.remove(&key);
            let mut acts = vec![];
            match r.exit {
                runtime::units::Exit::Code(0) => {
                    next.tally.sent += 1;
                    if j.close {
                        acts.push(effect(CLOSE_ON_LAND, format!("{}:{}", j.id, j.tip), &Target { repo: j.repo.clone(), id: j.id.clone(), br: j.br.clone(), tip: j.tip.clone(), path: String::new() }, 5_000)?);
                    }
                }
                // Queued behind a certification: neither sent nor failed.
                runtime::units::Exit::Code(2) => {}
                _ => next.tally.failed += 1,
            }
            settle(next, &j.repo, acts)
        }
        // A send's outcome is its leg starting; the leg answers by `unit.ran`.
        Input::Outcome { effect: e, ok: true, .. } if e == SEND => Ok((carry.clone(), vec![])),
        Input::Outcome { effect: e, ok: false, body } if e == SEND => {
            let key = carry.jobs.iter().find(|(k, j)| j.effect == SEND && body.contains(k.as_str())).or_else(|| carry.jobs.iter().find(|(_, j)| j.effect == SEND)).map(|(k, _)| k.clone());
            let Some(key) = key else { return Ok((carry.clone(), vec![])) };
            let mut next = carry.clone();
            let repo = next.jobs.remove(&key).map(|j| j.repo).unwrap_or_default();
            next.tally.failed += 1;
            settle(next, &repo, vec![])
        }
        Input::Outcome { effect: e, ok, body } if [ARCHIVE, FREE_WORKTREE, ORPHAN_WORKTREE].contains(&e.as_str()) => {
            let Some(key) = answered_job(carry, e, body) else {
                return match carry.phase {
                    Phase::Acting => Err(format!("{NAME}: in state acting no {e} is in flight")),
                    _ => Ok((carry.clone(), vec![])),
                };
            };
            let mut next = carry.clone();
            let j = next.jobs.remove(&key).unwrap_or_default();
            let mut acts = vec![];
            match (*ok, e.as_str()) {
                (true, ARCHIVE) => {
                    // Parked and read back: only now is the branch reaped.
                    let b = Branch { id: j.id.clone(), br: j.br.clone(), tip: j.tip.clone(), ..Branch::default() };
                    let s = Snapshot { repo: j.repo.clone(), ..Snapshot::default() };
                    acts.push(send_leg(&mut next.jobs, &s, &b, "archived", false, false)?);
                }
                (true, _) => next.tally.sent += 1,
                (false, _) => next.tally.failed += 1,
            }
            settle(next, &j.repo, acts)
        }
        Input::Outcome { effect: e, .. } if e == CLOSE_ON_LAND || e == PRUNE || e == CONTENT_ON_BASE || e == PR_TIP => Ok((carry.clone(), vec![])),
        Input::Outcome { effect: e, .. } => Err(format!("{NAME}: in state {} an outcome of {e} is not expected", phase_name(carry.phase))),
    }
}

/// The pinned values the runners read.
pub struct Pinned {
    pub submitted_label: String,
    pub gh: String,
    pub gh_timeout: String,
}

impl Pinned {
    pub fn read(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Pinned, String> {
        Ok(Pinned { submitted_label: get("SPIRA_SUBMITTED_LABEL")?, gh: get("SPIRA_GH")?, gh_timeout: get("SPIRA_GH_TIMEOUT")? })
    }
}

/// A registered repository's checkout by name.
fn root_of(real: &hel::real::Real, repo: &str) -> Result<std::path::PathBuf, String> {
    real.repos().into_iter().find(|r| r.name == repo).and_then(|r| r.root).ok_or_else(|| format!("repository {repo:?} has no configured checkout — refusing"))
}

/// The hel world `hel.sweep` reads and acts through: [`super::hel_reap_stale::world`] with the
/// pinned forge program and timeout.
pub fn world(run: &std::path::Path, release: &str, lc: &spira_config::lc_call::LcClient, p: &Pinned) -> Result<hel::real::Real, String> {
    Ok(super::hel_reap_stale::world(run, release, lc)?.with_gh(&p.gh, &p.gh_timeout))
}

/// The runner of [`OBSERVE`]: one repository's reads (`hel::sweep::scan_repo`, git and the
/// fetch in-process through hel; the store over the socket), the first registered one when
/// `repo` is empty, and the names of the repositories after it.
pub fn observe(real: &hel::real::Real, lc: &spira_config::lc_call::LcClient, p: &Pinned, repo: &str) -> Result<String, String> {
    let mut snap = Snapshot::default();
    if real.run().join("world.halted").exists() {
        snap.halted = true;
        return serde_json::to_string(&snap).map_err(|e| e.to_string());
    }
    let names: Vec<hel::ports::Repo> = real.repos();
    let at = if repo.trim().is_empty() { 0 } else { names.iter().position(|r| r.name == repo.trim()).ok_or_else(|| format!("repository {repo:?} is not registered — refusing"))? };
    let Some(r) = names.get(at) else { return serde_json::to_string(&snap).map_err(|e| e.to_string()) };
    snap.repo = r.name.clone();
    snap.repos = names.iter().skip(at + 1).map(|r| r.name.clone()).collect();
    let Some(root) = r.root.as_deref() else { return serde_json::to_string(&snap).map_err(|e| e.to_string()) };
    // A repository the pass cannot read is skipped, as today: its snapshot carries nothing.
    let Ok((base, branches, orphans)) = hel::sweep::scan_repo(real, root, &p.submitted_label, true) else { return serde_json::to_string(&snap).map_err(|e| e.to_string()) };
    snap.base = Base { landref: base.landref, landrefs: base.landrefs, remote: base.remote.unwrap_or_default() };
    let ids: Vec<String> = branches.iter().map(|b| b.id.clone()).chain(orphans.iter().map(|o| o.id.clone())).collect();
    snap.rows = spira_config::lc_state::list_ids_on(lc, &ids).map(|rs| rs.into_iter().map(|r| (r.bead_id, r.state)).collect()).unwrap_or_default();
    for b in branches {
        if let Some(why) = &b.held {
            snap.holders.insert(b.id.clone(), why.clone());
        }
        if !b.labels.is_empty() || b.superseded {
            snap.beads.insert(b.id.clone(), Bead { labels: b.labels.clone(), supersedes: if b.superseded { vec!["supersedes".into()] } else { vec![] } });
        }
        snap.branches.push(Branch {
            ahead: b.ahead.unwrap_or(u64::from(u32::MAX)).min(u64::from(u32::MAX)) as u32,
            content_on_base: b.content_on_base,
            merges_clean: b.merges_clean,
            cherry_unapplied: u32::from(b.cherry_unapplied),
            landed_by_subject: b.landed_by_subject,
            has_own_commit: b.has_own_commit,
            only_stacked_base: b.only_stacked_base,
            base_names: if b.base_names { vec![b.id.clone()] } else { vec![] },
            worktree: b.worktree.map(|w| w.display().to_string()),
            pr_merged_tip: b.pr_merged_tip,
            id: b.id,
            br: b.br,
            tip: b.tip,
        });
    }
    for o in orphans {
        if let Some(why) = &o.held {
            snap.holders.insert(o.id.clone(), why.clone());
        }
        snap.orphans.push(Orphan { id: o.id, path: o.path.display().to_string(), br: o.br });
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The runner of [`PR_TIP`]: the merged PR's head for one branch (`gh pr view`, hel's one
/// forge child), for an operator or the simulator; the observe folds the same read in.
pub fn pr_tip(real: &hel::real::Real, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    Ok(real.pr_merged_tip(&root_of(real, &t.repo)?, &t.br).unwrap_or_default())
}

/// The runner of [`SEND`]: the witness re-checked, the content-on-base evidence recorded
/// first when asked, then the leg (`hel reap-landed-branch`, the verified deletion) started
/// as a transient unit named for `repo:br:tip`; it ends on that unit's `unit.ran`.
pub fn send(real: &hel::real::Real, lc: &spira_config::lc_call::LcClient, units: Option<&dyn runtime::units::Units>, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let units = units.ok_or("no unit manager configured")?;
    let s: Send = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let root = root_of(real, &s.repo)?;
    if let Some(held) = real.witness(&s.id) {
        return Err(format!("{}: {held} (mid-send) — {}", s.id, leg_key(&s.repo, &s.br, &s.tip)));
    }
    if s.proof {
        let base = real.base(&root).ok_or_else(|| format!("{}: the land ref does not resolve", s.repo))?;
        let sha = hel::git::Git(&root).rev_parse(&base.landref).unwrap_or_default();
        lc.answer(&["content-on-base", s.id.as_str(), &format!("merge-tree:{sha}"), "hel"]).map_err(String::from)?;
    }
    let mut env = vec![];
    if let Ok(spec) = spira_config::process::spec() {
        env.push((spira_config::process::TOML_ENV.to_string(), spec));
    }
    let unit = runtime::units::UnitSpec { task: NAME.into(), key: leg_key(&s.repo, &s.br, &s.tip), argv: vec!["hel".into(), "reap-landed-branch".into(), s.id.clone(), s.br.clone(), root.display().to_string(), s.why.clone(), s.caller.clone()], env, cwd: Some(root) };
    runtime::units::effect(units, runtime::units::UNIT_START, &serde_json::to_string(&unit).map_err(|e| e.to_string())?)
}

/// The runner of [`CONTENT_ON_BASE`]: the evidence verb on its own, for one branch.
pub fn content_on_base(real: &hel::real::Real, lc: &spira_config::lc_call::LcClient, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let root = root_of(real, &t.repo)?;
    let base = real.base(&root).ok_or_else(|| format!("{}: the land ref does not resolve", t.repo))?;
    let sha = hel::git::Git(&root).rev_parse(&base.landref).unwrap_or_default();
    lc.answer(&["content-on-base", t.id.as_str(), &format!("merge-tree:{sha}"), "hel"]).map_err(String::from)
}

/// The runner of [`CLOSE_ON_LAND`]: close a submitted bead at the land ref's sha.
pub fn close_on_land(real: &hel::real::Real, lc: &spira_config::lc_call::LcClient, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let root = root_of(real, &t.repo)?;
    let base = real.base(&root).ok_or_else(|| format!("{}: the land ref does not resolve", t.repo))?;
    let sha = hel::git::Git(&root).rev_parse(&base.landref).unwrap_or(base.landref);
    lc.answer(&["close-on-land", t.id.as_str(), sha.as_str()]).map_err(String::from)
}

/// The runner of [`ARCHIVE`]: park the tip at `refs/archive/<br>` and read it back; answers
/// with the job key so the send leg chains.
pub fn archive(real: &hel::real::Real, input: &str) -> Result<String, String> {
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    hel::sweep::archive_tip(&root_of(real, &t.repo)?, &t.br, &t.tip)?;
    Ok(format!("{ARCHIVE}:{}:{}", t.repo, t.id))
}

/// The runner of [`FREE_WORKTREE`] and [`ORPHAN_WORKTREE`]: `reap::destroy_worktree` through
/// the hel world (fence, witnesses, salvage); answers with the job key.
pub fn destroy_worktree(real: &hel::real::Real, kind: &str, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let root = root_of(real, &t.repo)?;
    if kind == ORPHAN_WORKTREE && hel::git::Git(&root).branch_exists(&t.br) {
        return Err(format!("{}: branch {} exists again — not an orphan", t.id, t.br));
    }
    let why = if kind == ORPHAN_WORKTREE { format!("orphan: branch {} is gone", t.br) } else { format!("branch {} kept as superseded; worktree freed", t.id) };
    if real.destroy_worktree(&t.id, std::path::Path::new(&t.path), &root, &why) {
        Ok(format!("{kind}:{}:{}", t.repo, t.id))
    } else {
        Err(format!("{}: worktree {} was not removed — see {}", t.id, t.path, real.reaplog_path().display()))
    }
}

/// The runner of [`PRUNE`]: `git worktree prune` on one repository.
pub fn prune(real: &hel::real::Real, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    real.prune(&root_of(real, &t.repo)?);
    Ok(format!("pruned {}", t.repo))
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("phase"), runtime::rows::Part::Field("last_fired"), runtime::rows::Part::Field("pass"), runtime::rows::Part::Field("tally"), runtime::rows::Part::Map { name: "jobs", drop: &[] }];

crate::tasks::b::hosted!(HelSweep);
