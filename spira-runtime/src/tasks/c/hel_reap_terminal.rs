//! # hel.reap_terminal — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/hel-reap-terminal.md`;
//! its Acceptance section is the test file `tests/it/task_c_hel_reap_terminal.rs`.
//!
//! ## Tick
//!
//! - **Calendar:** `OnCalendar=*-*-* *:20:00`, `RandomizedDelaySec=120`, `AccuracySec=2min`,
//!   `Persistent=true` (unit file). The jitter comes from `Ctx::rand_u64`, which is seeded.
//! - **Catch-up:** an overdue tick fires **once**. `last_fired` is carried in an emitted
//!   `hel.reap_terminal.state` event, because `Timer` does not survive a restart
//!   (`runtime/src/event.rs:290-300`).
//! - **Single-flight:** one pass at a time. A pass is one observe followed by at most N removals,
//!   each its own effect, so no effect runs as long as the unit's `TimeoutStartSec=900`.
//! - **Wake:** none besides the calendar. A `lifecycle.terminal` event could wake it later; that is
//!   not proposed here.
//!
//! ## Observe
//!
//! `hel.reap_terminal.observe` (class `host`, deadline 120 s; the 120 s matches the store read
//! today, `hel/src/main.rs:143-144`). Snapshot:
//! `{halted, rows:{id→state}, entries:[{name, is_dir, is_git, repo, unsaved:No|Yes|Unknown, age_secs, held:Option<why>}]}`.
//! - `halted`: `run/world.halted`, which replaces `ExecStartPre=world guard`
//!   (`spira-world/src/guard.rs:11`). The in-process precedent is `spira-runtime/src/world.rs:524`.
//! - `rows`: **S7 `LcClient` `list`**. Today this is `spira_config::lc_state::list_within(lc, 120)`
//!   forking `spira-lc list` (`main.rs:144`, `spira-config/src/lc_state.rs:209`). **An empty or
//!   failed list refuses the whole pass** (`main.rs:145-147`). The snapshot carries `rows_ok:false`,
//!   and decide acts on nothing.
//! - `entries`: a read of `run/worktree` (`terminal.rs:123-129`), with these fields:
//!   - `repo_of` from git-common-dir (`terminal.rs:72-76`, `hel/src/git.rs:237`);
//!   - `unsaved` from `rev-list HEAD --not <landrefs> --branches --glob=refs/archive/*`
//!     (`terminal.rs:60-69`, landrefs via `spira_config::repos`, `real.rs:182-194`);
//!   - `age` as the newest mtime, with a walk capped at 200 000 entries; a tree too big counts as
//!     fresh (`terminal.rs:78`, `terminal.rs:80-100`);
//!   - `held` as a `.spira-keep` marker or a live `/proc/*/cwd` inside (`terminal.rs:103-119`).
//!
//! ## Decide
//!
//! `hel::terminal::decide(name, state, is_git, unsaved, age, max_age, held)` (`terminal.rs:35-56`).
//! It is already pure, and is reused as is. The rules, in order:
//! 1. a dot-name is the harness tree;
//! 2. held means keep;
//! 3. a terminal state (LANDED, SUPERSEDED, DROPPED, DONE; `spira-config/src/lc_state.rs:85-87`)
//!    means remove as `Terminal`;
//! 4. a live row means keep;
//! 5. no row and younger than `max_age` means keep;
//! 6. otherwise it is `Scratch`.
//!
//! A git checkout with unsaved or unknown commits is kept. **Carry:** `{last_fired}` only. **Fixtures:**
//! `terminal.rs` unit tests, plus `hel/src/tests.rs`.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `hel.reap_terminal.destroy_worktree` | git | 120 s | `name:path` | it goes through `reap::destroy_worktree` (`hel/src/reap.rs:331`), which refuses a path outside `run/worktree` (`reap.rs:342-346`), a live holder or a WORKING row (`reap.rs:178-196`, and a store that does not answer counts as held), and a failed salvage (`reap.rs:362-364`; that salvage forks `tar` at `reap.rs:260`) |
//! | `hel.reap_terminal.remove_scratch` | host | 60 s | `name` | an entry not directly under the root; a symlink is unlinked, never followed (`terminal.rs:184-191`); a git entry with no resolvable repo (`terminal.rs:160`) |
//! | `hel.reap_terminal.prune` | git | 30 s | `repo:pass` | — (`reap.rs:282`), once per repo that lost a worktree (`terminal.rs:173-175`) |
//!
//! The witness inside `destroy_worktree` today asks `spira-lc show` per bead (the `ClaimProbe for
//! Real`, `real.rs:325-330`). In the task it reads the snapshot's `rows`, and the effect
//! re-checks through `LcClient`.
//!
//! ## Silence
//!
//! On an idle tick (everything KEEP, a halted world, or a refused list), the only output is the
//! observe probe. The `KEEP` lines (`terminal.rs:145`) become wide-event counts.
//!
//! ## Replaces
//!
//! - `spira-reap-terminal-prod.{service,timer}` and `ExecStartPre=world guard`.
//! - Children removed: `spira-lc list` and `spira-lc show`. `git` and `tar` stay.
//! - Pinned keys: `SPIRA_SCRATCH_MAX_AGE_HOURS` (default 24, `main.rs:139`,
//!   `spira/conf.d/SPIRA_SCRATCH_MAX_AGE_HOURS:8`), `SPIRA_RUN` (`real.rs:346`), `SPIRA_REAPLOG`
//!   (`real.rs:336`), `SPIRA_HOME`/`SPIRA_RELEASE` (`main.rs:79`), `SPIRA_METRICS_ENDPOINT`.
//! - **Catalogue note:** the catalogue lists `spira-lc list/state`. The code forks `list` and
//!   `show`, not `state`, on this path (`main.rs:144`, `real.rs:328-329`).
//!
//! ## Reverse
//!
//! Roll back the release, or use `Observe` mode, in which every removal is `effect.shadowed`. A
//! removed worktree is recoverable from the salvage patch and tar (`reap.rs:198`).
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "hel.reap_terminal";
pub const STREAM: &str = "task.hel.reap_terminal";
pub const TICK: EventKindTag = "hel.reap_terminal.tick";
pub const STATE: EventKindTag = "hel.reap_terminal.state";

pub const OBSERVE: &str = "hel.reap_terminal.observe";
pub const DESTROY_WORKTREE: &str = "hel.reap_terminal.destroy_worktree";
pub const REMOVE_SCRATCH: &str = "hel.reap_terminal.remove_scratch";
pub const PRUNE: &str = "hel.reap_terminal.prune";
pub const EFFECTS: &[&str] = &[OBSERVE, DESTROY_WORKTREE, REMOVE_SCRATCH, PRUNE];

/// `OnCalendar=*-*-* *:20:00`.
pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_HEL_REAP_TERMINAL_CALENDAR";
/// `RandomizedDelaySec=120`.
pub const JITTER_KEY: &str = "SPIRA_RUNTIME_HEL_REAP_TERMINAL_JITTER_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, JITTER_KEY, "SPIRA_SCRATCH_MAX_AGE_HOURS", "SPIRA_RUN", "SPIRA_REAPLOG", "SPIRA_RELEASE"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 120_000, cadence_key: CADENCE_KEY, calendar: true, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unsaved {
    No,
    Yes,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_git: bool,
    pub repo: Option<String>,
    pub unsaved: Unsaved,
    pub age_secs: u64,
    pub held: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub halted: bool,
    /// An empty or failed lifecycle list refuses the whole pass.
    pub rows_ok: bool,
    pub rows: BTreeMap<String, String>,
    pub entries: Vec<Entry>,
}

/// The pass machine (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Observing,
    Reaping,
    Refused,
}

/// `state × event → state`. A tick while `reaping` is refused, naming `reaping`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "tick", Phase::Observing), (Phase::Observing, "observed:work", Phase::Reaping), (Phase::Observing, "observed:idle", Phase::Idle), (Phase::Observing, "rows:empty", Phase::Refused), (Phase::Refused, "tick", Phase::Observing), (Phase::Reaping, "outcome", Phase::Reaping), (Phase::Reaping, "done", Phase::Idle)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    pub last_fired: Option<u64>,
    /// Removals asked and not yet answered, by entry name.
    pub open: Vec<String>,
    /// Each open checkout's repository, so a removal that succeeds prunes it.
    pub roots: BTreeMap<String, String>,
    /// Repositories that lost a worktree this pass: pruned once the last removal answers.
    pub prune: Vec<String>,
}

pub struct HelReapTerminal {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Observing => "observing",
        Phase::Reaping => "reaping",
        Phase::Refused => "refused",
    }
}

fn max_age(cfg: &Cfg) -> Result<std::time::Duration, String> {
    let k = "SPIRA_SCRATCH_MAX_AGE_HOURS";
    let raw = cfg.get(k).ok_or_else(|| format!("{k} is read but not declared"))?;
    raw.trim().parse::<u64>().map(|h| std::time::Duration::from_secs(h * 3_600)).map_err(|_| format!("{k} must be a whole number of hours, got {raw:?}"))
}

fn unsaved_of(u: Unsaved) -> hel::terminal::Unsaved {
    match u {
        Unsaved::No => hel::terminal::Unsaved::No,
        Unsaved::Yes => hel::terminal::Unsaved::Yes,
        Unsaved::Unknown => hel::terminal::Unsaved::Unknown,
    }
}

fn act(name: &str, input: String, deadline_ms: u64) -> Act {
    Act { name: name.into(), input, deadline_ms, probe: false }
}

/// One answered removal: the entry leaves `open`; a checkout that went marks its repository
/// for the pass's prune; the last answer ends the pass and prunes.
fn answered(carry: &Carry, body: &str, ok: bool) -> (Carry, Vec<Act>) {
    let mut next = carry.clone();
    let name = next.open.iter().position(|n| n == body.trim()).or(if next.open.is_empty() { None } else { Some(0) }).map(|i| next.open.remove(i));
    if let Some(root) = name.and_then(|n| next.roots.remove(&n)) {
        if ok && !next.prune.contains(&root) {
            next.prune.push(root);
        }
    }
    if !next.open.is_empty() {
        return (next, vec![]);
    }
    let acts = next.prune.iter().map(|r| act(PRUNE, r.clone(), 30_000)).collect();
    (Carry { last_fired: next.last_fired, ..Carry::default() }, acts)
}

/// `hel::terminal::decide` per entry, reused as is.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire if carry.phase == Phase::Reaping => Err(format!("{NAME}: a tick in state reaping: {} removal(s) have not answered", carry.open.len())),
        Input::Fire => Ok((Carry { phase: Phase::Observing, ..carry.clone() }, vec![])),
        // A pass whose removals never all answered ends here: the next tick decides afresh.
        Input::Observed(_) if carry.phase == Phase::Reaping => Ok((Carry { last_fired: carry.last_fired, ..Carry::default() }, vec![])),
        Input::Observed(s) if s.halted => Ok((carry.clone(), vec![])),
        Input::Observed(s) if !s.rows_ok => Ok((Carry { phase: Phase::Refused, last_fired: carry.last_fired, ..Carry::default() }, vec![])),
        Input::Observed(s) => {
            let max_age = max_age(cfg)?;
            let mut next = Carry { last_fired: carry.last_fired, ..Carry::default() };
            let mut acts = vec![];
            for e in &s.entries {
                let held = e.held.as_ref().map(|_| "held");
                let d = hel::terminal::decide(&e.name, s.rows.get(&e.name).map(String::as_str), e.is_git, if e.is_git { unsaved_of(e.unsaved) } else { hel::terminal::Unsaved::No }, std::time::Duration::from_secs(e.age_secs), max_age, held);
                let hel::terminal::Decision::Remove(why) = d else { continue };
                let tag = if why == hel::terminal::Why::Terminal { "terminal" } else { "scratch" };
                match &e.repo {
                    Some(root) => {
                        let d = super::hel_reap_stale::Destroy { id: e.name.clone(), path: String::new(), root: root.clone(), why: format!("reap-terminal: {tag}") };
                        acts.push(act(DESTROY_WORKTREE, serde_json::to_string(&d).map_err(|e| e.to_string())?, 120_000));
                        next.roots.insert(e.name.clone(), root.clone());
                    }
                    // A checkout whose repository does not resolve cannot go through the chokepoint.
                    None if e.is_git => continue,
                    None => acts.push(act(REMOVE_SCRATCH, e.name.clone(), 60_000)),
                }
                next.open.push(e.name.clone());
            }
            if acts.is_empty() {
                return Ok((Carry { phase: Phase::Idle, ..next }, vec![]));
            }
            next.phase = Phase::Reaping;
            next.last_fired = Some(now_ms);
            Ok((next, acts))
        }
        Input::Outcome { effect, ok, body } if (effect == DESTROY_WORKTREE || effect == REMOVE_SCRATCH) && carry.phase == Phase::Reaping => Ok(answered(carry, body, *ok)),
        // A prune answers after its pass ended, as does a removal that outlived its pass.
        Input::Outcome { effect, .. } if effect == PRUNE || effect == DESTROY_WORKTREE || effect == REMOVE_SCRATCH => Ok((carry.clone(), vec![])),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state {} an outcome of {effect} is not expected", phase_name(carry.phase))),
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {:?}", phase_name(carry.phase), r.handle)),
    }
}

/// The runner of [`OBSERVE`]: one lifecycle `list` over the socket (an empty or failed list
/// refuses the pass), `hel::terminal::entries` in-process, and the world's halt stamp.
pub fn observe(real: &hel::real::Real, lc: &spira_config::lc_call::LcClient) -> Result<String, String> {
    if real.run().join("world.halted").exists() {
        return serde_json::to_string(&Snapshot { halted: true, ..Snapshot::default() }).map_err(|e| e.to_string());
    }
    let rows: BTreeMap<String, String> = spira_config::lc_state::list_on(lc).map(|rs| rs.into_iter().map(|r| (r.bead_id, r.state)).collect()).unwrap_or_default();
    if rows.is_empty() {
        return serde_json::to_string(&Snapshot { rows_ok: false, ..Snapshot::default() }).map_err(|e| e.to_string());
    }
    let entries = hel::terminal::entries(real)?
        .into_iter()
        .map(|e| Entry {
            name: e.name,
            is_dir: e.is_dir,
            is_git: e.is_git,
            repo: e.repo.map(|r| r.display().to_string()),
            unsaved: match e.unsaved {
                hel::terminal::Unsaved::No => Unsaved::No,
                hel::terminal::Unsaved::Yes => Unsaved::Yes,
                hel::terminal::Unsaved::Unknown => Unsaved::Unknown,
            },
            age_secs: e.age.as_secs(),
            held: e.held.map(str::to_string),
        })
        .collect();
    serde_json::to_string(&Snapshot { halted: false, rows_ok: true, rows, entries }).map_err(|e| e.to_string())
}

/// The runner of [`DESTROY_WORKTREE`]: `reap::destroy_worktree` on `<run>/worktree/<name>`.
pub fn destroy(real: &hel::real::Real, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let mut d: super::hel_reap_stale::Destroy = serde_json::from_str(input).map_err(|e| e.to_string())?;
    if d.id.is_empty() || d.id.contains('/') || d.id.starts_with('.') {
        return Err(format!("{:?} is not an entry name — refusing", d.id));
    }
    d.path = real.worktrees().join(&d.id).display().to_string();
    super::hel_reap_stale::destroy(real, &serde_json::to_string(&d).map_err(|e| e.to_string())?)
}

/// The runner of [`REMOVE_SCRATCH`]: a non-git entry directly under the worktree root.
pub fn remove_scratch(real: &hel::real::Real, name: &str) -> Result<String, String> {
    use hel::ports::World;
    let name = name.trim();
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(format!("{name:?} is not an entry name — refusing"));
    }
    let root = real.worktrees();
    let path = root.join(name);
    if path.join(".git").exists() {
        return Err(format!("{name} is a git checkout — refusing to remove it as scratch"));
    }
    if hel::terminal::remove_scratch(&root, &path) {
        Ok(name.to_string())
    } else {
        Err(format!("{name}: scratch removal did not complete"))
    }
}

/// The runner of [`PRUNE`]: `git worktree prune` on one repository.
pub fn prune(real: &hel::real::Real, root: &str) -> Result<String, String> {
    use hel::ports::World;
    real.prune(std::path::Path::new(root.trim()));
    Ok(format!("pruned {root}"))
}

crate::tasks::b::hosted!(HelReapTerminal);
