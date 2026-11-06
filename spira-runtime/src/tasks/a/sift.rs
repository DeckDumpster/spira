//! # sift — contract
//!
//! The pre-round screen, moving from `round-controller sifts` into the scheduler. Pattern P1, family
//! A of [[scheduler-consolidation-2026-11-03]]. Paths are relative to `spira-releases/current`
//! (0ef4f0f93); `rs` = `round-controller/src/sifts.rs`, `sl` = `sift/src/lib.rs`, `sg` =
//! `sift/src/git.rs`.
//!
//! ## Tick
//!
//! - Interval 60 s (`spira-sift-prod.timer`: `OnBootSec=150s`, `OnUnitActiveSec=60s`; the service
//!   carries `CPUQuota=50%`, `TimeoutStartSec=900`).
//! - Also woken by a bead entering `SUBMITTED` or `CERTIFIED`, or a tip moving (store `watch`
//!   events via S7), so a submission is screened without waiting a minute (proposed).
//! - Single-flight per repository. The binary has no lock of its own; overlap is prevented only by
//!   systemd's oneshot (unverified that two passes were never concurrent).
//! - Scope: every registry repo whose land is `queue` or `queue.local` (`rs:64-68`). Code and
//!   catalogue differ: `queue.forge` repos are **not** sifted by this unit.
//!
//! ## Observe
//!
//! Two steps, because the screen judges against a refreshed base:
//!
//! 1. `sift.fetch_base` (git, 60 s): `git fetch -q <remote>` for a non-local repo (`rs:557-574`).
//!    A failed fetch refuses the repo's pass: "an unrefreshed base is the wrong base".
//! 2. `sift.observe` (probe, git class, 120 s) per repo, returning:
//!
//! | Field | Today | From (after the move) |
//! |---|---|---|
//! | `rows` — SUBMITTED + CERTIFIED with `tip`, `holds`, `ejected_red_tip`, `sifted_tip` | `spira-lc list --state SUBMITTED,CERTIFIED --snapshot` fork (`rs:252-258`) | LcClient `list --state SUBMITTED,CERTIFIED --snapshot` |
//! | `outside` rows of stacked-on beads not in the pool | `spira-lc list --ids` (`rs:260-266`) | LcClient `list --ids` |
//! | `stack[id]`, `prereq_state[id]` | `spira-lc show <id>` per member and per prerequisite (`rs:394-438`) | LcClient `show` (batched) |
//! | `items[id]` — description + notes (target verdict) | `spira-lc plan items --full --ids` (`rs:481-513`) | LcClient `plan items --full` |
//! | `open_round` members `id:tip` | `queue batch show --repo` fork (`rs:517-553`) | the S7b round/batch projection |
//! | `branch_tips` | `git for-each-ref refs/heads/spira/*` (`rs:321-343`) | git (edge, unchanged) |
//! | per candidate: `base`, `merge` (clean/conflict files), `patch_id`, `stacked_on`, `identity_diff` | `git rev-parse`, `merge-tree --write-tree`, `commit-tree`, `diff`, `patch-id --stable`, `log` (`sg:38-97`) | git (edge), computed in the probe so `decide` does no IO |
//! | `identity_pages` | brain wiki pages under `SPIRA_WIKI/SPIRA_IDENTITY_PROJECTS` (`sg:95`, `sift/src/identity.rs`) | file read in the probe (unchanged) |
//!
//! `git merge-tree --write-tree` and `commit-tree` write loose objects (`sg:42-55`): harmless, but the
//! observe probe is not strictly read-only. The per-(id, tip, base) verdict cache in
//! `run/sift/verdicts/` (`sift/src/store.rs:18`) keeps the git work to new tips; it moves into the
//! probe as a memory cache.
//!
//! ## Decide
//!
//! `decide(carry, snap) -> (carry', acts)` is `sift::screen` (`sl:133-262`) run over a snapshot-backed
//! `Probe` and a recording `Acts`. It is already pure behind its three traits; the port is two
//! adapters, not a rewrite.
//!
//! - **Pool** (`rs:292-305`, `353-390`): unheld pool rows at their live branch tip, with a content
//!   row, not ejected at the tip, not stacked (transitively) on a barred bead (`rs:462-478`).
//! - **Verdict per candidate**: duplicate patch-id → supersede (keeper prefers an open-round member,
//!   `sl:158-170`); conflict, a stacked-on bead in REWORK, a target gap, an identity gap → findings
//!   (`sl:85-100`, `203-217`); none → pass. An identity check that could not run holds the bead
//!   out (`sl:211-215`).
//! - **Carried state**: per bead the `Sifted` machine (today `run/sift/events/<id>.json` and
//!   `send-backs/<id>`, `sift/src/store.rs:18-28`), states `screened`, `sent_back(n)`, `capped`,
//!   `mending` (`sift/src/machine.rs:18-31`); cap `SEND_BACK_CAP = 3` (`sl:16`). A conflict is never
//!   counted (`sl:229-230`, `249`).
//! - **Config pinned** (S10): `SPIRA_RUN`, `SPIRA_LC_TIMEOUT`, `SPIRA_WIKI`, `SPIRA_IDENTITY_PROJECTS`,
//!   `SPIRA_CONCIERGE_INBOX` (`rs:33-49`); the repo registry (`rs:56`).
//! - **Fixtures**: `sift/src/tests.rs` (29 tests), `sift/src/identity.rs` (6), `rs:677-738` (4).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `sift.fetch_base` | git | 60 s | `<repo>:<tick at>` | a `queue.local` repo (no remote, `rs:558`) |
//! | `sift.note` | store | 10 s | `<id>:<tip>:note` | nothing new; today `bash bead.sh amend --note` (`rs:664-671`), after the move LcClient `content` amend. Precedes `sift.send_back`; if it fails the bead is held, not sent back (`sl:242-246`) |
//! | `sift.send_back` | store | 10 s | `<id>:<tip>:GateRed` | a bead no longer SUBMITTED/CERTIFIED or whose tip moved (`rs:626-639`); a stale `--version` (store CAS) |
//! | `sift.record_pass` | store | 10 s | `<id>:<tip>:Sifted` | skipped when `sifted_tip == tip` (`rs:189-193`); a bead out of the pool states records none (`rs:643-652`); a moved tip (store) |
//! | `sift.supersede` | store | 10 s | `<id>:superseded-by:<keeper>` | the store's close refusals; today `spira-lc close --superseded-by` via `spira-config/src/lifecycle_row.rs:293-318` |
//! | `sift.record` | — (carry) | — | `<id>:<event>:<tip>` | a move the `Sifted` machine refuses (`sift/src/machine.rs:89-100`) — an `Emit`, not an effect |
//! | `inbox.append` | host | 10 s | line text | — the `SIFT superseded / sent / REPEAT` lines, today appended straight to the inbox file (`rs:195-212`) |
//!
//! Store writes go through LcClient `event bead <id> --expect <state> --version <v> --actor sift`
//! (`rs:603-622`), which is the refusal that matters: a moved bead fails the CAS.
//!
//! ## Silence
//!
//! An idle tick (empty pool for every repo, `rs:138-140`) emits the probe and nothing else; the
//! fetch is skipped when the pool is empty (move the fetch after the pool read, as `rs:141` already
//! orders it). A pool whose verdicts are all cached passes already recorded emits nothing.
//!
//! ## Replaces
//!
//! `spira-sift-prod.{service,timer}`; children per pass: `timeout spira-lc` list/show/plan/event/close
//! (`rs:238-244`, ~2 + 2 per member), `timeout queue batch show` (`rs:517-523`), `bash bead.sh amend`
//! (`rs:666`), git `for-each-ref`/`fetch`/`rev-parse`/`merge-tree`/`commit-tree`/`diff`/`patch-id`/`log`.
//! The catalogue omits `bead.sh` and the direct inbox write. Retires `run/sift/{events,send-backs}`
//! into carried state.
//!
//! ## Reverse
//!
//! Release rollback restores the unit. `Observe` mode (S4) journals the four store writes as
//! `effect.shadowed`; the screen still computes, so parity can be read off the shadowed specs.
//!

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "sift";
pub const STREAM: &str = "task.sift";
pub const TICK: EventKindTag = "sift.tick";
pub const STATE: EventKindTag = "sift.state";
/// A move of one bead's `Sifted` machine: an Emit, not an effect.
pub const RECORD: EventKindTag = "sift.record";
pub const OBSERVE: &str = "sift.observe";
pub const FETCH_BASE: &str = "sift.fetch_base";
pub const NOTE: &str = "sift.note";
pub const SEND_BACK: &str = "sift.send_back";
pub const RECORD_PASS: &str = "sift.record_pass";
pub const SUPERSEDE: &str = "sift.supersede";
pub const INBOX_APPEND: &str = crate::watchers::INBOX_APPEND;

pub const CADENCE_MS: u64 = 60_000;
pub const OBSERVE_DEADLINE_MS: u64 = 120_000;
/// `SEND_BACK_CAP` (`sift/src/lib.rs:16`).
pub const SEND_BACK_CAP: u8 = 3;

pub const ACTS: &[(&str, u64)] = &[(FETCH_BASE, 60_000), (NOTE, 10_000), (SEND_BACK, 10_000), (RECORD_PASS, 10_000), (SUPERSEDE, 10_000), (INBOX_APPEND, 10_000)];
pub const EMITS: &[EventKindTag] = &[RECORD];
pub const EFFECTS: &[&str] = &[OBSERVE, FETCH_BASE, NOTE, SEND_BACK, RECORD_PASS, SUPERSEDE, INBOX_APPEND];
pub const KEYS: &[&str] = &["SPIRA_RUN", "SPIRA_LC_TIMEOUT", "SPIRA_WIKI", "SPIRA_IDENTITY_PROJECTS", "SPIRA_CONCIERGE_INBOX", "SPIRA_REPO_MAP"];
pub const UNREGISTERED: &[&str] = &[];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

/// One SUBMITTED or CERTIFIED row.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolRow {
    pub id: String,
    pub state: String,
    pub version: u64,
    pub tip: String,
    pub holds: Vec<String>,
    pub ejected_red_tip: Option<String>,
    pub sifted_tip: Option<String>,
}

/// The git facts of one candidate, computed in the probe so decide does no IO.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub base: String,
    /// The conflicting files of `merge-tree --write-tree`; empty is a clean merge.
    pub conflicts: Vec<String>,
    pub patch_id: Option<String>,
    pub stacked_on: Vec<String>,
    pub identity_diff: Vec<String>,
}

/// One repository's snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub rows: Vec<PoolRow>,
    pub outside: Vec<PoolRow>,
    pub stack: BTreeMap<String, Vec<String>>,
    pub prereq_state: BTreeMap<String, String>,
    /// Description and notes of each bead, for the target verdict.
    pub items: BTreeMap<String, String>,
    /// `id:tip` of the open round's members.
    pub open_round: Vec<String>,
    pub branch_tips: BTreeMap<String, String>,
    pub candidates: BTreeMap<String, Candidate>,
    /// `None` when the identity pages could not be read: the check holds the bead out.
    pub identity_pages: Option<Vec<String>>,
    /// Whether this repo's base was fetched; `false` refuses its pass.
    pub fetched: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub repos: BTreeMap<String, Repo>,
}

/// One bead's `Sifted` machine (`sift/src/machine.rs:18-31`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sifted {
    #[default]
    Unrecorded,
    Screened,
    SentBack(u8),
    Capped,
    Mending,
}

/// The legal moves: screened; sent back one at a time up to the cap; capped at the cap; any
/// state to mending.
pub static MOVES: &[(Sifted, Sifted)] = &[
    (Sifted::Unrecorded, Sifted::Screened),
    (Sifted::Screened, Sifted::SentBack(1)),
    (Sifted::SentBack(1), Sifted::SentBack(2)),
    (Sifted::SentBack(2), Sifted::SentBack(3)),
    (Sifted::SentBack(3), Sifted::Capped),
    (Sifted::Unrecorded, Sifted::Mending),
    (Sifted::Screened, Sifted::Mending),
    (Sifted::SentBack(1), Sifted::Mending),
    (Sifted::SentBack(2), Sifted::Mending),
    (Sifted::SentBack(3), Sifted::Mending),
    (Sifted::Capped, Sifted::Mending),
];

/// Carried between ticks: each bead's `Sifted` state and the tip it was judged at.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub beads: BTreeMap<String, (Sifted, String)>,
}

/// Move bead `id` from `from` to `to`, or refuse naming the state it is in.
pub fn record(_id: &str, _from: Sifted, _to: Sifted) -> Result<Sifted, String> {
    crate::not_ported!(NAME)
}

/// `sift::screen` over the snapshot: supersede, send back, record a pass, or hold out.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sift {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl Sift {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        Sift { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("sift/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for Sift {
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
