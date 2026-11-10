//! # hel.reap_stale — contract
//!
//! Family C of the scheduler consolidation, ported. The sections below are the contract page `wiki/projects/spira/designs/tasks/hel-reap-stale.md`;
//! its Acceptance section is the test file `tests/it/task_c_hel_reap_stale.rs`.
//!
//! ## Tick
//!
//! - **Cadence:** 1 h (`OnUnitActiveSec=1h`, `AccuracySec=5min`), and the first tick is 10 min
//!   after boot (`OnBootSec=10min`). No calendar, and no `Persistent`.
//! - **Single-flight.** Each removal is its own effect. One pass today can run up to
//!   `TimeoutStartSec=1800`, which no single effect may do.
//!
//! ## Observe
//!
//! `hel.reap_stale.observe` (class `git`, deadline 120 s). The snapshot covers every registered
//! repository:
//! `{halted, repos:[{name, root, landrefs, worktrees:[{id, path, branch, live_pid, bead_state, merged_into, idle_secs}]}]}`.
//! - `halted` comes from `run/world.halted` and replaces `ExecStartPre=world guard`
//!   (`spira-world/src/guard.rs:11`; in-process precedent `spira-runtime/src/world.rs:524`).
//! - `repos` is the repository registry `spira_config::repos::Registry`, read in-process
//!   (`hel/src/real.rs:58-67`). `Real::new` still forks the lib.sh context seam first
//!   (`real.rs:47`, `hel/src/seam.rs:62-66`), only for `reaplog` and `gh_timeout`, which this pass
//!   never uses. **The task drops it.**
//! - `worktrees` is `git worktree list` per root (`hel/src/git.rs:137`), filtered to
//!   `run/worktree/<id>`. Dot-names are skipped (`stale.rs:97-102`).
//! - `live_pid` is the first `/proc/*/cwd` inside the worktree (`stale.rs:69-79`).
//! - `bead_state` today is **bd's** `status` from `bdjson list` through the seam (`stale.rs:109-110`,
//!   `real.rs:199`), overridden by `spira-lc state` when that reads LANDED or DROPPED
//!   (`stale.rs:111`, `hel/src/reap.rs:163-171`). This re-derives state from raw bd.
//!   **Law-a-watcher-asks-the-component-that-decides puts the lifecycle row in charge:** the task
//!   reads `LcClient` `list` (S7) and maps a terminal row to finished. bd `closed` is not
//!   consulted. That is a behaviour change, and T5 must show it (a bd-closed bead with a live row
//!   is no longer reaped on that ground).
//! - `merged_into` is the first landref of which the branch is an ancestor (`stale.rs:113`,
//!   `git.rs:38`).
//! - `idle_secs` is the newest mtime of the tree directory and of the admin dir's `HEAD` and
//!   `index` (`stale.rs:52-66`).
//!
//! ## Decide
//!
//! The decision is `hel::stale::decide(bead_state, merged_into, idle, idle_secs)` (`stale.rs:34-49`).
//! It is already pure. The rules, in order:
//! 1. A state of `closed`, `LANDED` or `DROPPED` means BeadFinished.
//! 2. A merged branch that has been idle for at least 1 h (`MERGED_GRACE_SECS`, `stale.rs:17`)
//!    means BranchMerged.
//! 3. Idle for at least the window means Idle.
//! 4. Otherwise keep.
//!
//! The window is 48 h (`DEFAULT_IDLE_SECS`, `stale.rs:12`). For a tree with no bead it is the
//! smaller of that and 4 h (`UNBEADED_IDLE_SECS`, `stale.rs:14`, `stale.rs:115`). A live cwd always
//! keeps the tree (`stale.rs:104-107`).
//! - **Carry:** none. The task is stateless, and no `state` event is ever emitted.
//! - **Fixtures:** the tests in `stale.rs:143-172`, plus one fixture per verdict that carries a
//!   live pid.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `hel.reap_stale.destroy_worktree` | git | 120 s | `id:path` | It goes through `reap::destroy_worktree` (`reap.rs:331`). It refuses a path outside `run/worktree` (`reap.rs:342-346`) and a live holder or a WORKING row (`reap.rs:178-196`; the claim probe forks `spira-lc show` today, `real.rs:325-330`). A salvage failure also refuses (`reap.rs:362-364`; tar at `reap.rs:260`) |
//! | `hel.reap_stale.prune` | git | 30 s | `repo:pass` | Nothing. One per repository per non-dry pass (`stale.rs:136-138`, `reap.rs:282`) |
//!
//! ## Silence
//!
//! An idle tick, where every worktree is kept or the world is halted, issues the probe and
//! nothing more. The summary line `reap-stale: N removed, K kept, F failed` (`hel/src/main.rs:195`)
//! becomes wide-event fields, not a journal line.
//!
//! **Gap:** today `prune` runs every pass, even when nothing was removed (`stale.rs:136-138`).
//! The task issues `prune` only when a removal succeeded, which keeps the idle tick silent. T5
//! records the difference.
//!
//! ## Replaces
//!
//! - `spira-worktree-reaper-prod.{service,timer}`, together with `ExecStartPre=world guard`.
//! - Children removed: the `bash -s hel.seam.sh` context seam and the beads seam (lib.sh and
//!   `bdjson`), `spira-lc list` (prefetch, `real.rs:205`), `spira-lc state` per worktree
//!   (`reap.rs:164`) and `spira-lc show` (the witness). The children that stay are `git` and
//!   `tar`.
//! - Pinned keys: `SPIRA_RUN` (`real.rs:346`), `SPIRA_REAPLOG` (`real.rs:336`), `SPIRA_HOME` and
//!   `SPIRA_RELEASE` (`main.rs:187`), and `SPIRA_METRICS_ENDPOINT`. One new key,
//!   `hel.reap_stale.idle_hours`, defaults to 48. Today the idle window is fixed at 48 h, because
//!   the unit passes no `--idle-hours` (`main.rs:177-183`).
//! - **Catalogue note:** the catalogue lists only `git worktree` as a child. The code also forks
//!   lib.sh and bd through the seam, `spira-lc list`, `spira-lc state` and `spira-lc show`, and
//!   `tar`.
//!
//! ## Reverse
//!
//! A release rollback restores the unit and its timer. Alternatively, use `Observe` mode. Removed
//! trees are recoverable from the salvage patch and tar in `run/` (`reap.rs:198`).
//!

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Act, Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "hel.reap_stale";
pub const STREAM: &str = "task.hel.reap_stale";
pub const TICK: EventKindTag = "hel.reap_stale.tick";
pub const STATE: EventKindTag = "hel.reap_stale.state";

pub const OBSERVE: &str = "hel.reap_stale.observe";
pub const DESTROY_WORKTREE: &str = "hel.reap_stale.destroy_worktree";
pub const PRUNE: &str = "hel.reap_stale.prune";
pub const EFFECTS: &[&str] = &[OBSERVE, DESTROY_WORKTREE, PRUNE];

/// `MERGED_GRACE_SECS` and `UNBEADED_IDLE_SECS` (`stale.rs`).
pub const MERGED_GRACE_SECS: u64 = 3_600;
pub const UNBEADED_IDLE_SECS: u64 = 4 * 3_600;

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_HEL_REAP_STALE_SECS";
/// The idle window (48 h today, fixed); a new key.
pub const IDLE_HOURS_KEY: &str = "SPIRA_RUNTIME_HEL_REAP_STALE_IDLE_HOURS";
pub const KEYS: &[&str] = &[CADENCE_KEY, IDLE_HOURS_KEY, "SPIRA_RUN", "SPIRA_REAPLOG", "SPIRA_RELEASE"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 120_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Worktree {
    pub id: String,
    pub path: String,
    pub branch: String,
    pub live_pid: Option<u32>,
    /// The lifecycle row's state (`LcClient list`), not bd's status.
    pub bead_state: Option<String>,
    pub merged_into: Option<String>,
    pub idle_secs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Repo {
    pub name: String,
    pub root: String,
    pub landrefs: Vec<String>,
    pub worktrees: Vec<Worktree>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub halted: bool,
    pub repos: Vec<Repo>,
}

/// The pass machine (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Idle,
    Observing,
    Reaping,
    Halted,
}

/// `state × event → state`. A tick while `reaping` is refused, naming `reaping`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Idle, "tick", Phase::Observing), (Phase::Observing, "observed:work", Phase::Reaping), (Phase::Observing, "observed:idle", Phase::Idle), (Phase::Observing, "halted", Phase::Halted), (Phase::Halted, "tick", Phase::Observing), (Phase::Reaping, "outcome", Phase::Reaping), (Phase::Reaping, "done", Phase::Idle)];

/// None: the task is stateless between passes; `phase` is in-pass only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
}

pub struct HelReapStale {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Idle => "idle",
        Phase::Observing => "observing",
        Phase::Reaping => "reaping",
        Phase::Halted => "halted",
    }
}

/// The `destroy_worktree` effect's input.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Destroy {
    pub id: String,
    pub path: String,
    pub root: String,
    pub why: String,
}

/// A lifecycle state as `hel::stale::decide` reads it: every terminal row is finished.
fn finished_as(state: &str) -> &str {
    match state {
        "SUPERSEDED" | "DONE" => "DROPPED",
        s => s,
    }
}

fn idle_secs(cfg: &Cfg) -> Result<u64, String> {
    let raw = cfg.get(IDLE_HOURS_KEY).ok_or_else(|| format!("{IDLE_HOURS_KEY} is read but not declared"))?;
    raw.trim().parse::<u64>().ok().filter(|h| *h > 0).map(|h| h * 3_600).ok_or_else(|| format!("{IDLE_HOURS_KEY} must be a positive whole number of hours, got {raw:?}"))
}

/// `hel::stale::decide` per worktree.
pub fn decide(cfg: &Cfg, carry: &Carry, input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    match input {
        Input::Fire if carry.phase == Phase::Reaping => Err(format!("{NAME}: a tick in state reaping: the pass in flight has not ended")),
        Input::Fire => Ok((Carry { phase: Phase::Observing }, vec![])),
        // A pass whose outcomes never all came back ends here: the next tick decides afresh.
        Input::Observed(_) if carry.phase == Phase::Reaping => Ok((Carry::default(), vec![])),
        Input::Observed(s) if s.halted => Ok((Carry { phase: Phase::Halted }, vec![])),
        Input::Observed(s) => {
            let window = idle_secs(cfg)?;
            let mut acts = vec![];
            for r in &s.repos {
                for t in &r.worktrees {
                    let state = t.bead_state.as_deref().map(finished_as);
                    let Some(why) = hel::stale::reason(&hel::stale::verdict(state, t.merged_into.as_deref(), t.idle_secs, t.live_pid.is_some(), window)) else { continue };
                    let d = Destroy { id: t.id.clone(), path: t.path.clone(), root: r.root.clone(), why: format!("reaper: {why}") };
                    acts.push(Act { name: DESTROY_WORKTREE.into(), key: format!("{}:{}", t.id, t.path), input: serde_json::to_string(&d).map_err(|e| e.to_string())?, deadline_ms: 120_000, probe: false });
                }
            }
            Ok((Carry { phase: if acts.is_empty() { Phase::Idle } else { Phase::Reaping } }, acts))
        }
        // Prune only after a removal succeeded (today it runs every pass).
        Input::Outcome { effect, ok: true, body } if effect == DESTROY_WORKTREE => Ok((carry.clone(), vec![Act { name: PRUNE.into(), key: format!("prune:{body}"), input: body.clone(), deadline_ms: 30_000, probe: false }])),
        Input::Outcome { effect, ok: false, .. } if effect == DESTROY_WORKTREE => Ok((carry.clone(), vec![])),
        Input::Outcome { effect, .. } if effect == PRUNE => Ok((Carry::default(), vec![])),
        Input::Outcome { effect, .. } => Err(format!("{NAME}: in state {} an outcome of {effect} is not expected", phase_name(carry.phase))),
        Input::Ran(r) => Err(format!("{NAME}: in state {} no unit is expected, got {:?}", phase_name(carry.phase), r.handle)),
    }
}

/// The hel world for this run, its store calls over `lc`.
pub fn world(run: &std::path::Path, release: &str, lc: &spira_config::lc_call::LcClient) -> Result<hel::real::Real, String> {
    let home = hel::locate_home(None, Some(release)).ok_or("SPIRA_RELEASE does not resolve to a release — refusing")?;
    Ok(hel::real::Real::minimal(home, run.to_path_buf(), None).with_lc(lc.clone()))
}

/// The runner of [`OBSERVE`]: `hel::stale::scan` in-process, with each tree's lifecycle row
/// (one `list` over the socket) as its state, and the world's halt stamp.
pub fn observe(real: &hel::real::Real, lc: &spira_config::lc_call::LcClient, proc_root: &std::path::Path) -> Result<String, String> {
    let run = real.run();
    if run.join("world.halted").exists() {
        return serde_json::to_string(&Snapshot { halted: true, repos: vec![] }).map_err(|e| e.to_string());
    }
    let mut rows = spira_config::lc_state::index(spira_config::lc_state::list_live_on(lc)?);
    let unread = std::cell::Cell::new(None::<String>);
    let memo = std::cell::RefCell::new(&mut rows);
    let asked = std::cell::RefCell::new(std::collections::HashSet::<String>::new());
    let state_of = |id: &str| {
        if !memo.borrow().contains_key(id) && asked.borrow_mut().insert(id.to_string()) {
            match spira_config::lc_state::list_ids_on(lc, &[id.to_string()]) {
                Ok(found) => memo.borrow_mut().extend(spira_config::lc_state::index(found)),
                Err(e) => unread.set(Some(e)),
            }
        }
        memo.borrow().get(id).map(|r| r.state.clone())
    };
    let scanned = hel::stale::scan(real, &real.repos(), proc_root, &state_of);
    if let Some(e) = unread.take() {
        return Err(format!("lifecycle state unreadable: {e}"));
    }
    let repos = scanned
        .into_iter()
        .map(|r| Repo {
            name: r.name,
            root: r.root.display().to_string(),
            landrefs: r.landrefs,
            worktrees: r.trees.into_iter().map(|t| Worktree { id: t.id, path: t.path.display().to_string(), branch: t.branch.unwrap_or_default(), live_pid: t.live_pid, bead_state: t.state, merged_into: t.merged_into, idle_secs: t.idle_secs }).collect(),
        })
        .collect();
    serde_json::to_string(&Snapshot { halted: false, repos }).map_err(|e| e.to_string())
}

/// The runner of [`DESTROY_WORKTREE`]: `reap::destroy_worktree` through the hel world (the
/// path fence, the holder witnesses and the salvage apply); answers with the id.
pub fn destroy(real: &hel::real::Real, input: &str) -> Result<String, String> {
    use hel::ports::World;
    let d: Destroy = serde_json::from_str(input).map_err(|e| e.to_string())?;
    if real.destroy_worktree(&d.id, std::path::Path::new(&d.path), std::path::Path::new(&d.root), &d.why) {
        Ok(d.id)
    } else {
        Err(format!("{} not removed, see {}", d.id, real.reaplog_path().display()))
    }
}

/// The runner of [`PRUNE`]: `git worktree prune` on every registered repository.
pub fn prune(real: &hel::real::Real) -> Result<String, String> {
    use hel::ports::World;
    let roots: Vec<std::path::PathBuf> = real.repos().into_iter().filter_map(|r| r.root).collect();
    for root in &roots {
        real.prune(root);
    }
    Ok(format!("pruned {} repositor(ies)", roots.len()))
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("phase")];

crate::tasks::b::hosted!(HelReapStale);
