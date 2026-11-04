//! # sweep.export_beads — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-export-beads.md`;
//! its Acceptance section is the test file `tests/task_c_sweep_export_beads.rs`.
//!
//! ## Tick
//!
//! - **Today:** `OnActiveSec=10min`, `OnUnitActiveSec=6h`, `AccuracySec=5min`, `Persistent=true`
//!   (`~/.config/systemd/user/spira-export-beads-prod.timer`). `Persistent=` applies only to
//!   `OnCalendar=`, so it is **inert** here: after downtime the timer does not catch up, it fires
//!   10 min after activation and every 6 h after the last run. (The brief said only
//!   `OnUnitActiveSec`; there is also an `OnActiveSec` anchor.)
//! - **Contract:** interval 6 h, first tick 10 min after boot; no catch-up rule (monotonic). If a
//!   missed window matters, heimdall's `mirror-stale` alarm already says so
//!   (`heimdall/src/judge.rs:313-326`).
//! - **Single-flight:** yes. No `world guard`.
//!
//! ## Observe
//!
//! `sweep.export_beads.observe` (probe, store, deadline 300 s — a full-store dump) returns:
//!
//! ```text
//! Snapshot { content: Vec<Bead>, lifecycle: BTreeMap<id, state>, memories: BTreeMap<k, v>,
//!            watermarks: String, mirrored_count: usize, mirror_bytes_sha: String }
//! ```
//!
//! - `content` = store verb `content list --all --json` (`:28`, 300 s); `lifecycle` = `list-all`
//!   (`:29`, 120 s); `memories` = `content memories --json` (`:30`, 60 s, absent → `{}`);
//!   `watermarks` = `plane-watermarks --manifest` (`:32`, 5 s). All four through the in-process
//!   `LcClient` (S7a), not the `spira-lc` CLI.
//! - `mirrored_count` / sha: the current `$SPIRA_WIKI/raw/spira-beads/spira.jsonl` (`:35`).
//! - `SPIRA_WIKI` empty → the probe reports `disabled` (`:16-19`).
//!
//! ## Decide
//!
//! `decide(snap) -> Unchanged | Write(jsonl, manifest) | Refuse(reason)`, pure, ported from the
//! embedded Python (`:37-55`). **Load-bearing properties, verified in the script:**
//! 1. **sorted by id** — `sorted(issues, key=id)` (`:50`), `sort_keys` per line (`:52`); memories follow, sorted by key (`:53-54`). ✔
//! 2. **commit only when the .jsonl changes** — an identical render exits before the manifest is
//!    rewritten or git is touched (`:57-61`); the manifest timestamp alone never commits. ✔
//! 3. **refuse without an `sp-` bead** — `if not issues: sys.exit(...)` (`:41-42`). ✔
//! 4. Not in CLAUDE.md but also load-bearing: **refuse to shrink below half** the mirrored count (`:43-44`).
//!
//! **Fixtures:** store dumps of 0 beads, 1 bead, half-shrink, unchanged, changed.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.export_beads.write` | host | 30 s | `export/<sha of new jsonl>` | a render `decide` refused; a `SPIRA_WIKI` that is not a directory (`:20`) |
//! | `sweep.export_beads.commit` | git | 30 s (3 tries on `index.lock`, `:82-94`) | `export/commit/<sha>` | a wiki that is not a git checkout (skipped, `:96`) |
//! | `sweep.export_beads.publish` | git | 60 s pull + 60 s push, 2 attempts (`:101-115`) | `export/push/<sha>` | a failed `pull --rebase` (aborts the rebase, `:105-107`) |
//! | `sweep.export_beads.touch` | host | 5 s | `export/touch/<tick>` | — |
//!
//! `write` writes `spira.jsonl` atomically (`:62`) and `manifest.md` (`:68-80`); `commit` stages and
//! commits both (`:97-99`); `publish` pull-rebases and pushes the brain remote. `touch` exists
//! because an unchanged pass still bumps the mirror's mtime (`:58`), which is what heimdall's
//! `mirror-stale` reads (`heimdall/src/main.rs:161`).
//!
//! ## Silence
//!
//! An unchanged pass emits only `touch`. **Proposed:** heimdall reads this task's last successful
//! observe from `scheduler status` (S13) instead of the file's mtime; then `touch` goes and an idle
//! tick is fully silent. Until then T2 allows exactly one `touch` per tick (4 per day).
//!
//! ## Replaces
//!
//! - Unit `spira-export-beads-prod.service` / `.timer`; output `run/export-beads.log`.
//! - Children: `bash lib.sh` (`:14`), `spira-lc` ×4, `python3` ×2, `git` (stays), `grep`, `cmp`, `date`, `mktemp`.
//! - Config keys pinned: `SPIRA_WIKI` (default empty, `spira/conf.d.defaults.generated.sh:1228`),
//!   `SPIRA_EXPORT_GIT_RETRY_WAIT` (default 10, `:88`).
//! - Commit message `chore: spira beads JSONL mirror <date> (<n> beads)` (`:98`) kept verbatim.
//!
//! ## Reverse
//!
//! Release rollback restores unit and timer. `Observe` shadows `write`/`commit`/`publish`; heimdall's
//! `mirror-stale` fires after its threshold, which is the intended signal.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "sweep.export_beads";
pub const STREAM: &str = "task.sweep.export_beads";
pub const TICK: EventKindTag = "sweep.export_beads.tick";
pub const STATE: EventKindTag = "sweep.export_beads.state";

pub const OBSERVE: &str = "sweep.export_beads.observe";
pub const WRITE: &str = "sweep.export_beads.write";
pub const COMMIT: &str = "sweep.export_beads.commit";
pub const PUBLISH: &str = "sweep.export_beads.publish";
pub const TOUCH: &str = "sweep.export_beads.touch";
pub const EFFECTS: &[&str] = &[OBSERVE, WRITE, COMMIT, PUBLISH, TOUCH];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_EXPORT_BEADS_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_WIKI", "SPIRA_EXPORT_GIT_RETRY_WAIT"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 300_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    /// `content list --all --json`, one value per bead (each has an `id`).
    pub content: Vec<serde_json::Value>,
    pub lifecycle: BTreeMap<String, String>,
    pub memories: BTreeMap<String, String>,
    pub watermarks: String,
    pub mirrored_count: usize,
    pub mirror_bytes_sha: String,
    /// `SPIRA_WIKI` empty.
    pub disabled: bool,
}

/// T3.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Rendered,
    Written,
    Committed,
    Published,
    Refused,
}

/// `state × event → state`. `publish` before `committed` is refused naming `written`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Rendered, "write:ok", Phase::Written), (Phase::Written, "commit:ok", Phase::Committed), (Phase::Committed, "publish:ok", Phase::Published), (Phase::Rendered, "refuse", Phase::Refused), (Phase::Published, "observed:changed", Phase::Rendered), (Phase::Refused, "observed:changed", Phase::Rendered)];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub phase: Phase,
    /// The sha of the render being carried through write → commit → publish.
    pub sha: Option<String>,
    /// `no-sp` or `shrink`.
    pub refused: Option<String>,
}

pub struct SweepExportBeads {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// `Unchanged | Write(jsonl, manifest) | Refuse(reason)`, sorted by id, refusing without an
/// `sp-` bead or below half the mirrored count.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("sweep.export_beads: not ported yet")
}

crate::tasks::b::hosted!(SweepExportBeads);
