//! # sweep.archive — contract
//!
//! Family C of the scheduler consolidation, red-first: `decide` is not ported yet. The
//! sections below are the contract page `wiki/projects/spira/designs/tasks/sweep-archive.md`;
//! its Acceptance section is the test file `tests/task_c_sweep_archive.rs`.
//!
//! ## Tick
//!
//! - Interval 20 min (`OnUnitActiveSec=20min`), first 5 min after boot (`OnBootSec=5min`);
//!   `Persistent=true` is inert on a monotonic timer.
//! - **Wakes (proposed):** none. The session-end hook (`archive.sh hook`, `:298-313`) remains a
//!   separate writer outside the scheduler (Claude Code runs it), so it is not an event source.
//! - **Single-flight:** yes; and the whole-run `flock` on `$SPIRA_ARCHIVE/.lock` (`:69-81`) stays,
//!   because the hook is a second writer the scheduler does not own.
//! - No `world guard`.
//!
//! ## Observe
//!
//! `sweep.archive.observe` (probe, host, deadline 60 s) returns:
//!
//! ```text
//! Snapshot { sources: Vec<{rel, size, mtime}>, index: BTreeMap<rel, {bytes, mtime, body_exists}>, projects_dir_exists: bool }
//! ```
//!
//! - `sources`: every `*.jsonl` under `$SPIRA_TOKEN_PROJECTS` (`:268-274`), stat only.
//! - `index`: `$SPIRA_ARCHIVE/index.jsonl` (`:227-239`) — the archive is the component that owns
//!   this; the probe reads its index, not re-derived from bodies.
//! - No lifecycle-store reads. Today the unit forks no Spira binary.
//!
//! ## Decide
//!
//! `decide(snap) -> Vec<rel>`: the fast path at `:358-361` — a source is stored iff no index row,
//! or `bytes`/`mtime` differ, or the body is missing. Missing default projects dir → nothing
//! (`:327-329`); a configured one that is missing → refuse (`:330`). Rows whose source vanished are
//! **kept** (`:372-374`). No carry.
//! **Fixtures:** `spira/test-archive-no-sessions.sh` and the archive test scripts' trees, ported.
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sweep.archive.store` (one per changed rel) | host | 300 s per transcript | `archive/<rel>/<size>/<mtime>` | a source outside `$SPIRA_TOKEN_PROJECTS` (`:350-352`); an unreadable source (skipped, `:345-348`); the lock held > 900 s by the hook (`:75-77`) |
//! | `sweep.archive.index` | host | 60 s | `archive/index/<sha of row set>` | — ; replaces the index only when the bytes differ (`:241-266`) |
//!
//! `store` = snapshot the first `size` bytes (`:168-181`), scan+sha256 (`:129-158`), compress to
//! `bodies/<rel>.zst|.gz` atomically and remove the other extension (`:204-220`). Ported to Rust;
//! the compressor becomes in-process (a zstd crate) or stays a child — **`zstd`/`gzip` are not on
//! the W1 allow-list** (§6), so one of the two must change (unverified which crate is vendored).
//! Nothing ever deletes a body (`:35-37`).
//!
//! ## Silence
//!
//! An unchanged sweep stores nothing and leaves the index untouched (`:244-246`), so it journals
//! nothing beyond the probe. Today it prints a summary line every 20 min to `run/archive.log`
//! (`:378-380`); that goes.
//!
//! ## Replaces
//!
//! - Unit `spira-archive-prod.service` / `.timer`; output `run/archive.log`.
//! - Children: `bash conf.sh`, `python3`, `flock`, `zstd`|`gzip`, `date`.
//! - Config keys pinned: `SPIRA_ARCHIVE` (default `$SPIRA_RUN/archive`,
//!   `spira/conf.d.defaults.generated.sh:58`), `SPIRA_TOKEN_PROJECTS` (default
//!   `$HOME/.claude/projects`, `:1158`).
//! - `archive.sh query|lineage|restore|verify|where|hook` stay as the operator CLI and hook.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. `Observe` shadows `store`/`index`; the hook still archives
//! each session at its end, so a shadowed window loses only mid-session growth.
//!

use std::collections::BTreeMap;

use runtime::event::EventKindTag;
use serde::{Deserialize, Serialize};

use crate::tasks::b::kit::{Cadence, Cfg, Decision, Input, Spec};

pub const NAME: &str = "sweep.archive";
pub const STREAM: &str = "task.sweep.archive";
pub const TICK: EventKindTag = "sweep.archive.tick";
pub const STATE: EventKindTag = "sweep.archive.state";

pub const OBSERVE: &str = "sweep.archive.observe";
pub const STORE: &str = "sweep.archive.store";
pub const INDEX: &str = "sweep.archive.index";
pub const EFFECTS: &[&str] = &[OBSERVE, STORE, INDEX];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_SWEEP_ARCHIVE_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_ARCHIVE", "SPIRA_TOKEN_PROJECTS"];

pub const SPEC: Spec = Spec { name: NAME, stream: STREAM, tick: TICK, state: STATE, probe: OBSERVE, probe_deadline_ms: 60_000, cadence_key: CADENCE_KEY, calendar: false, effects: EFFECTS, keys: KEYS, probe_input_key: None };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    pub rel: String,
    pub size: u64,
    pub mtime: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    pub bytes: u64,
    pub mtime: u64,
    pub body_exists: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub sources: Vec<Source>,
    pub index: BTreeMap<String, Row>,
    pub projects_dir_exists: bool,
    /// A configured (not default) projects dir that is missing refuses.
    pub projects_dir_configured: bool,
}

/// Per rel and for the index (T3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Unseen,
    Changed,
    Storing,
    Stored,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Index {
    #[default]
    Current,
    Stale,
}

/// `state × event → state` per rel. An `index` while a `store` of the same pass is in flight
/// is refused naming `storing`.
pub const TRANSITIONS: &[(Phase, &str, Phase)] = &[(Phase::Unseen, "store", Phase::Storing), (Phase::Changed, "store", Phase::Storing), (Phase::Storing, "store:ok", Phase::Stored), (Phase::Storing, "store:failed", Phase::Changed), (Phase::Stored, "changed", Phase::Changed)];
pub const INDEX_TRANSITIONS: &[(Index, &str, Index)] = &[(Index::Current, "stored", Index::Stale), (Index::Stale, "index:ok", Index::Current)];

/// No carry beyond the pass in flight.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub rels: BTreeMap<String, Phase>,
    pub index: Index,
}

pub struct SweepArchive {
    pub cadence: Cadence,
    pub cfg: Cfg,
}

/// The fast path: store a source with no index row, a size or mtime change, or no body.
pub fn decide(_cfg: &Cfg, _carry: &Carry, _input: &Input<Snapshot>, _now_ms: u64) -> Decision<Carry> {
    unimplemented!("sweep.archive: not ported yet")
}

crate::tasks::b::hosted!(SweepArchive);
