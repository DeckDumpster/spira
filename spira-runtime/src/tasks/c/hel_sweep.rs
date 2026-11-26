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
//!      `spira-store/src/lc_state.rs:195`), plus `state` for LANDED (today `timeout 5 spira-lc
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
//! - **Carry:** `{last_fired, pass: Option<{started, repos_left, cursor}>, tally:{sent, failed, refused}, refused}`.
//! - **Refused:** a failed send, archive or free is not planned again while the branch's tip, row
//!   state and land ref sha stand; each skip counts in `tally.refused`. Orphans are not held back.
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
/// Milliseconds of rest after each send leg starts.
pub const PACE_KEY: &str = "SPIRA_HEL_PACE_MS";
pub const KEYS: &[&str] = &[CADENCE_KEY, JITTER_KEY, BUDGET_KEY, "SPIRA_RUN", "SPIRA_REAPLOG", "SPIRA_SUBMITTED_LABEL", "SPIRA_GH", "SPIRA_GH_TIMEOUT", "SPIRA_RELEASE", PACE_KEY];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 120_000, cadence_key: CADENCE_KEY, calendar: true, effects: EFFECTS, keys: KEYS, probe_input_key: None, jitter_key: Some(JITTER_KEY) };

/// How long one observe may keep asking git for facts: half its deadline, so the snapshot is
/// built and answered inside it. Branches not reached are judged first on the next observe.
pub const OBSERVE_SLICE: std::time::Duration = std::time::Duration::from_millis(SPEC.probe_deadline_ms / 2);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Base {
    pub landref: String,
    pub landrefs: Vec<String>,
    pub remote: String,
    pub sha: String,
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
    /// The branch this observe resumed after, empty from the start.
    pub resumed_after: String,
    /// The last branch judged by an observe its time slice cut, empty when it walked to the end.
    pub cut_at: String,
    /// The repository's branches could not be read.
    pub unread: bool,
}

impl Snapshot {
    /// What a scan resumed after `cursor`, and cut after its `last` branch or not, listed.
    pub fn set_window(&mut self, cursor: Option<&str>, cut: bool, last: Option<&str>) {
        self.resumed_after = cursor.unwrap_or_default().to_string();
        self.cut_at = last.filter(|_| cut).unwrap_or_default().to_string();
    }

    /// Whether this observe would have listed `br` had it existed: only then is its absence "gone".
    fn covers(&self, br: &str) -> bool {
        !self.unread && br > self.resumed_after.as_str() && (self.cut_at.is_empty() || br <= self.cut_at.as_str())
    }
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
    /// The repository whose observe is in flight: a restart loses the probe, and the boot asks it again.
    pub observing: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tally {
    pub sent: u32,
    pub failed: u32,
    pub refused: u64,
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
    /// `repo:br` to the [`Job::inputs`] of that branch's last failed act.
    pub refused: BTreeMap<String, String>,
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
    /// `tip|row state|land ref sha` as planned.
    pub inputs: String,
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
        self.s.rows.get(&self.b.id).is_some_and(|r| spira_store::lc_state::is_terminal(r))
    }
    fn submitted(&self) -> bool {
        self.s.beads.get(&self.b.id).is_some_and(|b| b.labels.iter().any(|l| l == self.submitted_label)) || self.s.rows.get(&self.b.id).is_some_and(|r| spira_store::lc_state::past_builder(r))
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

fn job_key(kind: &str, repo: &str, id: &str) -> String {
    format!("{kind}:{repo}:{id}")
}

/// A failed runner's text names its job, so the failure is never charged to another branch.
pub fn naming_job(err: String, key: &str) -> String {
    format!("{err} [{key}]")
}

fn refusal_key(repo: &str, br: &str) -> String {
    format!("{repo}:{br}")
}

/// What an act on a branch is judged from: a retry needs one of these to have moved.
fn inputs_of(s: &Snapshot, id: &str, tip: &str) -> String {
    format!("{tip}|{}|{}", s.rows.get(id).map_or("-", String::as_str), s.base.sha)
}

fn record_failure(refused: &mut BTreeMap<String, String>, j: &Job) {
    if j.effect != ORPHAN_WORKTREE {
        refused.insert(refusal_key(&j.repo, &j.br), j.inputs.clone());
    }
}

fn send_leg(jobs: &mut BTreeMap<String, Job>, s: &Snapshot, b: &Branch, caller: &str, proof: bool, close: bool, inputs: String) -> Result<Act, String> {
    jobs.insert(leg_key(&s.repo, &b.br, &b.tip), Job { effect: SEND.into(), repo: s.repo.clone(), id: b.id.clone(), br: b.br.clone(), tip: b.tip.clone(), close, inputs });
    let why = if caller == "archived" { "archived".to_string() } else { format!("landed in {}", s.base.landref) };
    effect(SEND, format!("{}:{}:{}", s.repo, b.br, b.tip), &Send { repo: s.repo.clone(), id: b.id.clone(), br: b.br.clone(), tip: b.tip.clone(), why, caller: caller.into(), proof }, 120_000)
}

fn job(jobs: &mut BTreeMap<String, Job>, kind: &str, t: &Target, inputs: String) -> Result<Act, String> {
    jobs.insert(job_key(kind, &t.repo, &t.id), Job { effect: kind.into(), repo: t.repo.clone(), id: t.id.clone(), br: t.br.clone(), tip: t.tip.clone(), close: false, inputs });
    effect(kind, format!("{}:{}:{}:{}:{}:{kind}", t.repo, t.id, t.br, t.tip, t.path), t, if kind == ARCHIVE { 30_000 } else { 120_000 })
}

/// One repository's acts: `hel::sweep::judge` per branch (round branches and held beads
/// skipped), then the orphaned worktrees. Also answers how many branches `refused` held back.
fn plan_labelled(label: &str, s: &Snapshot, refused: &BTreeMap<String, String>, jobs: &mut BTreeMap<String, Job>) -> Result<(Vec<Act>, u64), String> {
    use hel::sweep::Disp;
    let mut acts = vec![];
    let mut held_back = 0;
    for b in &s.branches {
        if b.id.starts_with("round-") || s.holders.contains_key(&b.id) {
            continue;
        }
        let inputs = inputs_of(s, &b.id, &b.tip);
        if refused.get(&refusal_key(&s.repo, &b.br)) == Some(&inputs) {
            held_back += 1;
            continue;
        }
        let target = Target { repo: s.repo.clone(), id: b.id.clone(), br: b.br.clone(), tip: b.tip.clone(), path: b.worktree.clone().unwrap_or_default() };
        let terminal = s.rows.get(&b.id).is_some_and(|r| spira_store::lc_state::is_terminal(r));
        let landed = s.rows.get(&b.id).is_some_and(|r| r == "LANDED");
        match hel::sweep::judge(&Snap { s, b, submitted_label: label }) {
            Disp::KeepUnlanded => {}
            Disp::KeepSupersededUnsafe => {
                if b.worktree.is_some() {
                    acts.push(job(jobs, FREE_WORKTREE, &target, inputs)?);
                }
            }
            Disp::OrphanNoBead | Disp::ArchiveTerminal => acts.push(job(jobs, ARCHIVE, &target, inputs)?),
            Disp::SendContentLanded => {
                let ahead = u64::from(b.ahead);
                if !landed && hel::sweep::needs_evidence(ahead, || b.has_own_commit, || b.only_stacked_base, || !b.base_names.is_empty()) {
                    continue;
                }
                acts.push(send_leg(jobs, s, b, "hel", ahead > 0 && !terminal, true, inputs)?);
            }
            Disp::SendOtherPr | Disp::ReapSquashMerged => acts.push(send_leg(jobs, s, b, "hel", false, true, inputs)?),
            Disp::ReapSupersededSafe => acts.push(send_leg(jobs, s, b, "hel", false, false, inputs)?),
        }
    }
    for o in &s.orphans {
        if s.holders.contains_key(&o.id) {
            continue;
        }
        acts.push(job(jobs, ORPHAN_WORKTREE, &Target { repo: s.repo.clone(), id: o.id.clone(), br: o.br.clone(), tip: String::new(), path: o.path.clone() }, String::new())?);
    }
    Ok((acts, held_back))
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
            p.observing = r.clone();
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

/// The job an outcome of `effect` answers, and whether that is certain: the one `body` names, or
/// the only one in flight. Otherwise the first, which ends a wait but is recorded against nothing.
fn answered_job(jobs: &BTreeMap<String, Job>, effect: &str, body: &str) -> Option<(String, bool)> {
    let of_effect = || jobs.iter().filter(|(_, j)| j.effect == effect).map(|(k, _)| k);
    if let Some(k) = of_effect().find(|k| body.trim() == k.as_str() || body.contains(&format!("[{k}]"))) {
        return Some((k.clone(), true));
    }
    let first = of_effect().next()?;
    Some((first.clone(), of_effect().nth(1).is_none()))
}

/// An answer to a pass past its budget: it issues and tallies nothing, but a failure is still recorded.
fn drain(carry: &mut Carry, input: &Input<Snapshot>) {
    let (key, failed) = match input {
        Input::Ran(r) => (r.key_for(NAME), !matches!(r.exit, runtime::units::Exit::Code(0 | 2))),
        // A send's good outcome is only its leg starting.
        Input::Outcome { effect, ok, body } if !(*ok && effect == SEND) => match answered_job(&carry.jobs, effect, body) {
            Some((k, certain)) => (Some(k), certain && !ok),
            None => (None, false),
        },
        _ => (None, false),
    };
    if let Some(j) = key.and_then(|k| carry.jobs.remove(&k)).filter(|_| failed) {
        record_failure(&mut carry.refused, &j);
    }
}

/// `disposition` and `act` over the snapshot rather than `World`.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    let in_pass = matches!(carry.phase, Phase::Acting | Phase::Observing) && carry.pass.is_some();
    if in_pass && !matches!(input, Input::Fire) {
        let started = carry.pass.as_ref().map_or(now_ms, |p| p.started);
        let budget = budget_ms(cfg)?;
        if now_ms >= started + budget {
            // A pass far past its budget whose answers never came is forgotten, not waited on.
            let mut spent = Carry { phase: Phase::BudgetSpent, pass: None, ..carry.clone() };
            drain(&mut spent, input);
            let next = if now_ms >= started + 2 * budget { Carry { last_fired: spent.last_fired, tally: spent.tally, refused: spent.refused, ..Carry::default() } } else { spent };
            if matches!(input, Input::Observed(_)) && next.phase == Phase::Idle {
                return decide(cfg, &next, input, now_ms);
            }
            return Ok((next, vec![]));
        }
    }
    if carry.phase == Phase::BudgetSpent && matches!(input, Input::Ran(_) | Input::Outcome { .. }) {
        let mut next = carry.clone();
        drain(&mut next, input);
        if next != *carry {
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
            if !continuing {
                next.jobs.clear();
            }
            next.refused.retain(|key, inputs| match key.split_once(':').filter(|(repo, _)| *repo == s.repo) {
                Some((_, br)) => s.branches.iter().find(|b| b.br == br).map_or(!s.covers(br), |b| *inputs == inputs_of(s, &b.id, &b.tip)),
                None => true,
            });
            let mut jobs = BTreeMap::new();
            let (acts, held_back) = plan_labelled(cfg.get("SPIRA_SUBMITTED_LABEL").map_or("", String::as_str), s, &next.refused, &mut jobs)?;
            next.tally.refused += held_back;
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
                next.pass = Some(Pass { started: now_ms, repos_left: s.repos.clone(), cursor: 0, observing: s.repo.clone() });
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
                    next.refused.remove(&refusal_key(&j.repo, &j.br));
                    if j.close {
                        acts.push(effect(CLOSE_ON_LAND, format!("{}:{}", j.id, j.tip), &Target { repo: j.repo.clone(), id: j.id.clone(), br: j.br.clone(), tip: j.tip.clone(), path: String::new() }, 5_000)?);
                    }
                }
                // Queued behind a certification: neither sent nor failed.
                runtime::units::Exit::Code(2) => {}
                _ => {
                    next.tally.failed += 1;
                    record_failure(&mut next.refused, &j);
                }
            }
            settle(next, &j.repo, acts)
        }
        // A send's outcome is its leg starting; the leg answers by `unit.ran`.
        Input::Outcome { effect: e, ok: true, .. } if e == SEND => Ok((carry.clone(), vec![])),
        Input::Outcome { effect: e, ok: false, body } if e == SEND => {
            let Some((key, certain)) = answered_job(&carry.jobs, SEND, body) else { return Ok((carry.clone(), vec![])) };
            let mut next = carry.clone();
            let j = next.jobs.remove(&key).unwrap_or_default();
            next.tally.failed += 1;
            if certain {
                record_failure(&mut next.refused, &j);
            }
            settle(next, &j.repo, vec![])
        }
        Input::Outcome { effect: e, ok, body } if [ARCHIVE, FREE_WORKTREE, ORPHAN_WORKTREE].contains(&e.as_str()) => {
            let Some((key, certain)) = answered_job(&carry.jobs, e, body) else {
                return match carry.phase {
                    Phase::Acting => Err(format!("{NAME}: in state acting no {e} is in flight")),
                    _ => Ok((carry.clone(), vec![])),
                };
            };
            let mut next = carry.clone();
            let j = next.jobs.remove(&key).unwrap_or_default();
            let mut acts = vec![];
            if *ok {
                next.refused.remove(&refusal_key(&j.repo, &j.br));
            }
            match (*ok, e.as_str()) {
                (true, ARCHIVE) => {
                    // Parked and read back: only now is the branch reaped.
                    let b = Branch { id: j.id.clone(), br: j.br.clone(), tip: j.tip.clone(), ..Branch::default() };
                    let s = Snapshot { repo: j.repo.clone(), ..Snapshot::default() };
                    acts.push(send_leg(&mut next.jobs, &s, &b, "archived", false, false, j.inputs.clone())?);
                }
                (true, _) => next.tally.sent += 1,
                (false, _) => {
                    next.tally.failed += 1;
                    if certain {
                        record_failure(&mut next.refused, &j);
                    }
                }
            }
            settle(next, &j.repo, acts)
        }
        Input::Outcome { effect: e, .. } if e == CLOSE_ON_LAND || e == PRUNE || e == CONTENT_ON_BASE || e == PR_TIP => Ok((carry.clone(), vec![])),
        Input::Outcome { effect: e, .. } => Err(format!("{NAME}: in state {} an outcome of {e} is not expected", phase_name(carry.phase))),
    }
}

/// A restart loses the timer holding the slot and the probe of a pass between repositories: the
/// boot observes at once when the slot after the last pass has gone by, or a repository's observe was in flight.
pub fn catch_up(carry: &Carry, now_ms: u64, cadence: &Cadence) -> Option<String> {
    if let (Phase::Observing, Some(p)) = (carry.phase, &carry.pass) {
        return Some(p.observing.clone());
    }
    carry.last_fired.filter(|last| cadence.next(*last) <= now_ms).map(|_| String::new())
}

/// The failure of a pass whose repository's bead read the store did not answer.
pub fn store_failure(repo: &str, store_text: &str) -> String {
    format!("{NAME}: {repo}: the store did not answer the branch read: {store_text}")
}

/// The pinned values the runners read.
pub struct Pinned {
    pub submitted_label: String,
    pub gh: String,
    pub gh_timeout: String,
    pub pace: std::time::Duration,
}

impl Pinned {
    pub fn read(get: &dyn Fn(&str) -> Result<String, String>) -> Result<Pinned, String> {
        Ok(Pinned { submitted_label: get("SPIRA_SUBMITTED_LABEL")?, gh: get("SPIRA_GH")?, gh_timeout: get("SPIRA_GH_TIMEOUT")?, pace: std::time::Duration::from_millis(get(PACE_KEY)?.trim().parse::<u64>().map_err(|e| format!("{PACE_KEY} must be a whole number of milliseconds: {e}"))?) })
    }
}

/// A registered repository's checkout by name.
fn root_of(real: &hel::real::Real, repo: &str) -> Result<std::path::PathBuf, String> {
    real.repos().into_iter().find(|r| r.name == repo).and_then(|r| r.root).ok_or_else(|| format!("repository {repo:?} has no configured checkout — refusing"))
}

/// The hel world `hel.sweep` reads and acts through: [`super::hel_reap_stale::world`] with the
/// pinned forge program and timeout.
pub fn world(run: &std::path::Path, release: &str, lc: &spira_store::lc_call::LcClient, p: &Pinned) -> Result<hel::real::Real, String> {
    Ok(super::hel_reap_stale::world(run, release, lc)?.with_gh(&p.gh, &p.gh_timeout))
}

/// The runner of [`OBSERVE`]: one repository's reads (`hel::sweep::scan_repo`, git and the
/// fetch in-process through hel; the store over the socket), the first registered one when
/// `repo` is empty, and the names of the repositories after it.
pub fn observe(real: &hel::real::Real, lc: &spira_store::lc_call::LcClient, p: &Pinned, repo: &str) -> Result<String, String> {
    use hel::ports::World;
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
    let Some(root) = r.root.as_deref() else {
        snap.unread = true;
        return serde_json::to_string(&snap).map_err(|e| e.to_string());
    };
    // A repository the pass cannot read is skipped, as today: its snapshot carries nothing.
    let cursor = real.resume_after(&r.name);
    let hel::sweep::Scan { base, branches, orphans, cut } = match hel::sweep::scan_repo(real, root, &p.submitted_label, true, cursor.as_deref(), OBSERVE_SLICE) {
        Ok(scan) => scan,
        Err(hel::sweep::ScanError::Skip(_)) => {
            snap.unread = true;
            return serde_json::to_string(&snap).map_err(|e| e.to_string());
        }
        Err(hel::sweep::ScanError::Store(e)) => return Err(store_failure(&r.name, &e)),
    };
    park_cursor(real, &r.name, cut, branches.last().map(|b| b.br.as_str()));
    snap.base = observed_base(root, base);
    snap.set_window(cursor.as_deref(), cut, branches.last().map(|b| b.br.as_str()));
    let ids: Vec<String> = branches.iter().map(|b| b.id.clone()).chain(orphans.iter().map(|o| o.id.clone())).collect();
    snap.rows = spira_store::lc_state::list_ids_on(lc, &ids).map(|rs| rs.into_iter().map(|r| (r.bead_id, r.state)).collect()).unwrap_or_default();
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
    if cursor.is_some() && !cut {
        clear_cursor_when_idle(real, &p.submitted_label, &snap)?;
    }
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// The scan's base with the commit its land ref names now.
pub fn observed_base(root: &std::path::Path, base: hel::ports::Base) -> Base {
    let sha = spira_config::git_refs::sha_of(root, &base.landref).or_else(|| hel::git::Git(root).rev_parse(&base.landref)).unwrap_or_default();
    Base { landref: base.landref, landrefs: base.landrefs, remote: base.remote.unwrap_or_default(), sha }
}

/// An observe cut by its time slice resumes after the last branch it judged. One that walked to
/// the end moves an existing cursor onto that last branch, so the prune finds nothing after it and clears it.
pub fn park_cursor(real: &hel::real::Real, repo: &str, cut: bool, last_judged: Option<&str>) {
    use hel::ports::World;
    if let Some(last) = last_judged.filter(|_| cut || real.resume_after(repo).is_some()) {
        real.set_resume_after(repo, Some(last));
    }
}

/// The prune ends a repository's pass: its cursor is cleared unless a branch sorts after it, which
/// only a cut observe left unjudged (an observe that walked to the end has none to leave).
pub fn clear_cursor_when_walked(real: &hel::real::Real, repo: &str, root: &std::path::Path) {
    use hel::ports::World;
    let unwalked = real.resume_after(repo).is_some_and(|c| hel::git::Git(root).spira_branches().iter().any(|b| b.as_str() > c.as_str()));
    if !unwalked {
        real.set_resume_after(repo, None);
    }
}

/// Branches sort by refname, so a cursor is a position: a branch sent since is gone, the cursor still holds.
pub fn past_cursor(br: &str, cursor: Option<&str>) -> bool {
    cursor.is_none_or(|c| br > c)
}

/// A resumed repository with nothing left to act on will issue no prune, so the cursor is cleared here.
pub fn clear_cursor_when_idle(real: &hel::real::Real, label: &str, snap: &Snapshot) -> Result<(), String> {
    use hel::ports::World;
    // The observe holds no carry: a repository with only refused acts left clears on its next observe.
    if plan_labelled(label, snap, &BTreeMap::new(), &mut BTreeMap::new())?.0.is_empty() {
        real.set_resume_after(&snap.repo, None);
    }
    Ok(())
}

/// Record that the pass handled `br`; the cursor only moves forward, so an act that answers late cannot pull it back.
pub fn mark_handled(real: &hel::real::Real, repo: &str, br: &str) {
    use hel::ports::World;
    if real.resume_after(repo).is_none_or(|c| c.as_str() < br) {
        real.set_resume_after(repo, Some(br));
    }
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
pub fn send(real: &hel::real::Real, lc: &spira_store::lc_call::LcClient, units: Option<&dyn runtime::units::Units>, p: &Pinned, release: &str, input: &str) -> Result<String, String> {
    let s: Send = serde_json::from_str(input).map_err(|e| e.to_string())?;
    run_send(real, lc, units, p, release, &s).map_err(|e| naming_job(e, &leg_key(&s.repo, &s.br, &s.tip)))
}

fn run_send(real: &hel::real::Real, lc: &spira_store::lc_call::LcClient, units: Option<&dyn runtime::units::Units>, p: &Pinned, release: &str, s: &Send) -> Result<String, String> {
    use hel::ports::World;
    let units = units.ok_or("no unit manager configured")?;
    let root = root_of(real, &s.repo)?;
    if let Some(held) = real.witness(&s.id) {
        return Err(format!("{}: {held} (mid-send)", s.id));
    }
    if s.proof {
        let base = real.base(&root).ok_or_else(|| format!("{}: the land ref does not resolve", s.repo))?;
        let sha = hel::git::Git(&root).rev_parse(&base.landref).unwrap_or_default();
        record_content_on_base(lc, &s.id, &sha)?;
    }
    let started = start_leg(units, release, &real.run(), &root, s)?;
    mark_handled(real, &s.repo, &s.br);
    real.rest(p.pace);
    Ok(started)
}

/// The leg's unit: `hel` is the release's own binary by absolute path (a transient unit has no
/// `PATH` to find it on), under the environment `hel reap-landed-branch` resolves its world from.
fn start_leg(units: &dyn runtime::units::Units, release: &str, run: &std::path::Path, root: &std::path::Path, s: &Send) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;
    let hel = spira_config::release_env::release_root(Some(release))?.join("bin").join("hel");
    if !hel.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0) {
        return Err(format!("{}: {} is not an executable file — refusing to send", s.id, hel.display()));
    }
    let home = std::env::var("HOME").map_err(|_| "HOME is not set — hel reads its pinned tool environment from it; there is no fallback".to_string())?;
    let mut env = spira_config::release_env::child_path_env(Some(release), Some(&home), &spira_config::process::cfg("SPIRA_PATH")?)?;
    env.extend([("SPIRA_RUN".to_string(), run.display().to_string()), ("HOME".to_string(), home)]);
    if let Ok(spec) = spira_config::process::spec() {
        env.push((spira_config::process::TOML_ENV.to_string(), spec));
    }
    let argv = vec![hel.display().to_string(), "reap-landed-branch".into(), s.id.clone(), s.br.clone(), root.display().to_string(), s.why.clone(), s.caller.clone()];
    let unit = runtime::units::UnitSpec { task: NAME.into(), key: leg_key(&s.repo, &s.br, &s.tip), argv, env, cwd: Some(root.to_path_buf()) };
    runtime::units::effect(units, runtime::units::UNIT_START, &serde_json::to_string(&unit).map_err(|e| e.to_string())?)
}

/// The runner of [`CONTENT_ON_BASE`]: the evidence verb on its own, for one branch.
pub fn content_on_base(real: &hel::real::Real, lc: &spira_store::lc_call::LcClient, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let root = root_of(real, &t.repo)?;
    let base = real.base(&root).ok_or_else(|| format!("{}: the land ref does not resolve", t.repo))?;
    let sha = hel::git::Git(&root).rev_parse(&base.landref).unwrap_or_default();
    record_content_on_base(lc, &t.id, &sha).map(|()| String::new())
}

fn record_content_on_base(lc: &spira_store::lc_call::LcClient, id: &str, land_sha: &str) -> Result<(), String> {
    spira_store::lifecycle_row::content_on_base_on(lc, id, &format!("merge-tree:{land_sha}"), "hel")
}

/// The runner of [`CLOSE_ON_LAND`]: close a submitted bead at the land ref's sha.
pub fn close_on_land(real: &hel::real::Real, lc: &spira_store::lc_call::LcClient, input: &str) -> Result<String, String> {
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
    let key = job_key(ARCHIVE, &t.repo, &t.id);
    root_of(real, &t.repo).and_then(|root| hel::sweep::archive_tip(&root, &t.br, &t.tip)).map_err(|e| naming_job(e, &key))?;
    Ok(key)
}

/// The runner of [`FREE_WORKTREE`] and [`ORPHAN_WORKTREE`]: `reap::destroy_worktree` through
/// the hel world (fence, witnesses, salvage); answers with the job key.
pub fn destroy_worktree(real: &hel::real::Real, kind: &str, input: &str) -> Result<String, String> {
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let key = job_key(kind, &t.repo, &t.id);
    run_destroy(real, kind, &t).map(|()| key.clone()).map_err(|e| naming_job(e, &key))
}

fn run_destroy(real: &hel::real::Real, kind: &str, t: &Target) -> Result<(), String> {
    use hel::ports::World;
    let root = root_of(real, &t.repo)?;
    if kind == ORPHAN_WORKTREE && hel::git::Git(&root).branch_exists(&t.br) {
        return Err(format!("{}: branch {} exists again — not an orphan", t.id, t.br));
    }
    let why = if kind == ORPHAN_WORKTREE { format!("orphan: branch {} is gone", t.br) } else { format!("branch {} kept as superseded; worktree freed", t.id) };
    if real.destroy_worktree(&t.id, std::path::Path::new(&t.path), &root, &why) {
        if kind == FREE_WORKTREE {
            mark_handled(real, &t.repo, &t.br);
        }
        Ok(())
    } else {
        Err(format!("{}: worktree {} was not removed — see {}", t.id, t.path, real.reaplog_path().display()))
    }
}

/// The runner of [`PRUNE`]: `git worktree prune` on one repository.
pub fn prune(real: &hel::real::Real, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let t: Target = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let root = root_of(real, &t.repo)?;
    real.prune(&root);
    clear_cursor_when_walked(real, &t.repo, &root);
    Ok(format!("pruned {}", t.repo))
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("phase"), runtime::rows::Part::Field("last_fired"), runtime::rows::Part::Field("pass"), runtime::rows::Part::Field("tally"), runtime::rows::Part::Map { name: "jobs", drop: &[] }, runtime::rows::Part::Map { name: "refused", drop: &[] }];

crate::tasks::b::hosted!(HelSweep, catch_up = catch_up);

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tqon8fj)]
    #[test]
    fn a_cut_observe_parks_the_cursor_on_the_last_branch_it_judged_and_a_walk_to_the_end_moves_it_to_the_last_branch() {
        use hel::ports::World;
        let dir = testkit::TempDir::new("hel-sweep-park");
        let real = hel::real::Real::minimal(dir.path().to_path_buf(), dir.path().to_path_buf(), None);
        park_cursor(&real, "spira", false, Some("spira/sp-b"));
        assert_eq!(real.resume_after("spira"), None);
        park_cursor(&real, "spira", true, Some("spira/sp-c"));
        assert_eq!(real.resume_after("spira").as_deref(), Some("spira/sp-c"));
        park_cursor(&real, "spira", false, Some("spira/sp-z"));
        assert_eq!(real.resume_after("spira").as_deref(), Some("spira/sp-z"));
    }

    #[test_id(t8oa3r8)]
    #[test]
    fn a_prune_keeps_the_cursor_while_a_branch_sorts_after_it_and_clears_it_at_the_end() {
        use hel::ports::World;
        let dir = testkit::TempDir::new("hel-sweep-prune-cursor");
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let o = std::process::Command::new("git").arg("-C").arg(&root).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
            assert!(o.status.success(), "{args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "x"]);
        git(&["branch", "spira/sp-a"]);
        git(&["branch", "spira/sp-c"]);
        let real = hel::real::Real::minimal(dir.path().to_path_buf(), dir.path().to_path_buf(), None);
        real.set_resume_after("spira", Some("spira/sp-a"));
        clear_cursor_when_walked(&real, "spira", &root);
        assert_eq!(real.resume_after("spira").as_deref(), Some("spira/sp-a"));
        real.set_resume_after("spira", Some("spira/sp-c"));
        clear_cursor_when_walked(&real, "spira", &root);
        assert_eq!(real.resume_after("spira"), None);
    }

    #[test_id(t35gqtj)]
    #[test]
    fn a_landed_rows_branch_with_its_diff_on_the_base_and_no_naming_commit_is_planned_for_sending_and_a_live_rows_is_kept() {
        let branch = |id: &str| Branch { id: id.into(), br: format!("spira/{id}"), tip: "t".into(), ahead: 2, content_on_base: true, merges_clean: true, ..Default::default() };
        let s = Snapshot {
            repo: "spira".into(),
            branches: vec![branch("sp-landed"), branch("sp-live")],
            rows: [("sp-landed".to_string(), "LANDED".to_string()), ("sp-live".to_string(), "CERTIFIED".to_string())].into(),
            ..Default::default()
        };
        let (acts, _) = plan_labelled("submitted", &s, &BTreeMap::new(), &mut BTreeMap::new()).unwrap();
        assert_eq!(acts.len(), 1, "{acts:?}");
        assert!(format!("{acts:?}").contains("sp-landed"), "{acts:?}");
    }

    struct Store(std::sync::Mutex<Vec<String>>);

    impl spira_store::lc_call::Transport for Store {
        fn exchange(&self, argv: &[String], _stdin: Option<&str>, _d: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
            self.0.lock().unwrap().push(argv.join(" "));
            let (code, out) = match argv[0].as_str() {
                "content-on-base" => (2, "usage: spira-lc show ...".to_string()),
                "show" => (0, r#"{"bead":{"state":"SUBMITTED","version":3}}"#.to_string()),
                _ => (0, String::new()),
            };
            Ok(spira_store::lc_call::Reply { code, out, err: String::new(), read_ended: false })
        }
    }

    #[derive(Default)]
    struct Recorder(std::sync::Mutex<Vec<runtime::units::UnitSpec>>);

    impl runtime::units::Units for Recorder {
        fn start(&self, spec: &runtime::units::UnitSpec) -> runtime::Result<runtime::units::UnitHandle> {
            self.0.lock().unwrap().push(spec.clone());
            Ok(runtime::units::UnitHandle { name: "u".into() })
        }
        fn stop(&self, _: &runtime::units::UnitHandle) -> runtime::Result<()> {
            Ok(())
        }
        fn restart(&self, _: &runtime::units::UnitHandle) -> runtime::Result<()> {
            Ok(())
        }
        fn facts(&self, _: &runtime::units::UnitHandle) -> runtime::Result<runtime::units::UnitFacts> {
            Err(runtime::RuntimeError("no facts".into(), runtime::Class::Fault))
        }
    }

    fn one() -> Send {
        Send { repo: "spira".into(), id: "sp-a".into(), br: "spira/sp-a".into(), tip: "abc".into(), why: "landed".into(), caller: "hel.sweep".into(), proof: false }
    }

    #[test_id(t799xm9)]
    #[test]
    fn the_leg_execs_hel_from_the_release_by_absolute_path_with_the_env_hel_reads() {
        let release = testkit::TempDir::new("hel-sweep-release");
        let bin = release.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        testkit::write_exe(bin.join("hel"), "exit 0\n");
        let rec = Recorder::default();
        let rel = release.path().display().to_string();
        start_leg(&rec, &rel, std::path::Path::new("run/x"), std::path::Path::new("repo"), &one()).unwrap();
        let started = rec.0.lock().unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].argv[0], bin.join("hel").display().to_string());
        assert_eq!(started[0].argv[1], "reap-landed-branch");
        let has = |k: &str, v: &str| started[0].env.iter().any(|(a, b)| a == k && b == v);
        assert!(has("SPIRA_RELEASE", &rel));
        assert!(has("SPIRA_RUN", "run/x"));
        assert!(started[0].env.iter().any(|(k, v)| k == "PATH" && v.starts_with(&format!("{rel}/bin:"))));
        assert!(started[0].env.iter().any(|(k, _)| k == "HOME"));
    }

    #[test_id(t46wqil)]
    #[test]
    fn a_release_without_hel_refuses_naming_the_path_and_starts_no_unit() {
        let release = testkit::TempDir::new("hel-sweep-no-hel");
        let rec = Recorder::default();
        let rel = release.path().display().to_string();
        let err = start_leg(&rec, &rel, std::path::Path::new("run/x"), std::path::Path::new("repo"), &one()).unwrap_err();
        assert!(err.contains(&release.path().join("bin").join("hel").display().to_string()), "{err}");
        assert!(rec.0.lock().unwrap().is_empty());
    }

    #[test_id(t9vjqa9)]
    #[test]
    fn content_on_base_is_sent_as_a_bead_event_the_socket_serves() {
        let store = std::sync::Arc::new(Store(Default::default()));
        let lc = spira_store::lc_call::LcClient::over(store.clone(), true);
        record_content_on_base(&lc, "sp-a", "abc").unwrap();
        assert_eq!(
            *store.0.lock().unwrap(),
            vec![
                "show sp-a".to_string(),
                r#"event bead sp-a --expect SUBMITTED --version 3 --actor hel --kind {"ContentOnBase":{"proof":"merge-tree:abc"}}"#.to_string()
            ]
        );
    }
}
