//! # hel.reap_terminal — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/hel-reap-terminal.md`;
//! its Acceptance section is the test file `tests/task_c_hel_reap_terminal.rs`.
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

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

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
}

pub struct HelReapTerminal {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `hel::terminal::decide` per entry, reused as is.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("hel.reap_terminal: not ported yet")
}

crate::tasks::b::hosted!(HelReapTerminal);
