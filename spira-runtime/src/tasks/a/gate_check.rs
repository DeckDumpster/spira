//! # gate-check — contract
//!
//! Task id `gate.check`, pattern P1, family A of [[scheduler-consolidation-2026-11-03]]. Each tick
//! it does five things:
//!
//! - resolves `gh:run` gates whose CI escalated
//! - files flaky-suite beads
//! - files red-twice-suite beads
//! - ingests recent CI runs into TSD
//! - runs bd-gate discovery for pr-mode repos
//!
//! Paths are relative to `spira-releases/current`.
//!
//! **Code differs from the catalogue.** The catalogue lists its children as "lib.sh, forge". The
//! code forks `bdq` (`gate-check/src/real.rs:37-146`) and calls `gh` directly, not `forge`
//! (`real.rs:167-229`). It also forks `bead.sh` (`real.rs:150`), `tsd-ingest.sh` (`real.rs:252`),
//! `git` (`real.rs:247`) and `sh -c command -v gh` (`real.rs:163`), and runs the lib.sh seam
//! through `bash -c` (`real.rs:48-68`).
//!
//! ## Tick
//!
//! - Cadence 2 min (`spira-gate-check-prod.timer`: OnBootSec 90 s, OnUnitActiveSec 2 min).
//!   Single-flight.
//! - No event wake is needed. Optionally it could be woken by `pr.ci_completed` from the
//!   `repo_events` hosted handler (unverified that that event exists).
//! - Today the unit has **no `world guard` ExecStartPre**, unlike its siblings. The task keeps
//!   that: it runs while the world is halted.
//!
//! ## Observe
//!
//! The snapshot is
//! `{ gate_list, gate_check_out, lc_open, beads_by_title, ci: { recent_runs, failed_main_runs, last_green_sha, annotations_by_job, fail_lines } }`.
//!
//! - **bd gates.** `bd gate list --json` (`real.rs:88-92`) and `bd gate check --type=gh:run`
//!   (`real.rs:100-108`, stdout and stderr merged), plus `bd show <gate> --json` per ESCALATE line
//!   (`real.rs:110-114`). *No store verb exists for gates today (unverified that lc-serve exposes
//!   them). bd is banned for new code, so this needs a `gates` verb on the content store or the
//!   move of gates into the lifecycle. Until then this probe is the one `bd` reader left.* Also
//!   `gate check` may itself mutate gate state (unverified). If it does, it is an effect, not an
//!   observe.
//! - **Lifecycle rows.** `spira_config::lc_state::list()` (`real.rs:132-134`). This becomes
//!   LcClient `list` (S7a), with the 5 s deadline.
//! - **Bead titles for dedup.** `bd list --all --json` (`real.rs:126-130`), re-read inside the
//!   loop (`main.rs:138,164`). This becomes one content-store `list` read per tick, or the
//!   open-title projection.
//! - **CI facts.** `forge` class probes, one per gh call:
//!   - `gh run list` (`real.rs:167,187,198`)
//!   - `gh api …/jobs` (`real.rs:177`)
//!   - `gh api …/annotations` (`real.rs:182`)
//!   - `gh run view --log-failed` (`real.rs:229`)
//!   - `git log --oneline A..B` (`real.rs:247`), on the git class
//! - **Repo map** through `spira_config::repos::Registry` (`real.rs:33,72-86`). It is already
//!   in-process.
//! - **Config**: `SPIRA_FLAKY_GH_REPO` (`gate-check/src/main.rs:44`,
//!   `spira/conf.d/SPIRA_FLAKY_GH_REPO`, default empty). Empty skips steps 3–5 (`main.rs:78`).
//!
//! ## Decide
//!
//! Everything except the IO is already pure in `gate-check/src/engine.rs`:
//!
//! - `discover_branches` (`engine.rs:11`) and `stuck_count` (`engine.rs:41`)
//! - `escalate_lines` (`engine.rs:46`), `gate_id_in` (`engine.rs:52`) and `blocked_bead`
//!   (`engine.rs:85`)
//! - `flaky_suites` and `red_twice_suites` (`engine.rs:162,184`)
//! - `has_open_bead` and `find_open_bead` (`engine.rs:130,136`)
//! - the titles and bodies (`engine.rs:193-205`)
//!
//! `decide(carry, snap)` composes the walk of `main.rs:63-190` over the snapshot.
//!
//! - **Carried state.** `ingested: BTreeSet<run_id>`, so a run is ingested once rather than all
//!   20 every tick (`main.rs:186-190`). *This is a change: today every tick re-runs `tsd-ingest.sh`
//!   for the 20 latest runs.*
//! - **Fixtures.** `gate-check/src/tests.rs` (215 lines, a fake `World`).
//!
//! ## Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `gate.check.discover` (`bd gate discover --branch B` in repo, `real.rs:94-98`) | store | 10 s | `repo/branch` per tick | a non-pr repo (`main.rs:94`) |
//! | `gate.check.resolve` (`bd gate resolve`, `real.rs:116-120`) | store | 10 s | `gate_id` | a gate with no blocked bead (`main.rs:112`) |
//! | `gate.check.ci_failed` (`spira_event ci.failed` via lib.sh, `real.rs:122-124`; becomes an LcClient `event`) | store | 5 s | `ci.failed/<blocked>/<gate_id>` | none |
//! | `gate.check.file_bead` (`bead.sh file … --for builder`, `real.rs:148-159`) | store | 30 s | the title (`engine.rs:193,197`) | a title with an open bead (`main.rs:138,165`). It files nothing when the lc rows are unreadable (`main.rs:119-127`) |
//! | `gate.check.raise_priority` (`bd priority <id> 1`, `real.rs:136`) | store | 10 s | `id/1` | priority already ≤ 1 (`main.rs:166`) |
//! | `gate.check.note` (`bd note`, `real.rs:142`) | store | 10 s | `id/run_id` | none |
//! | `gate.check.tsd_ingest` (`tsd-ingest.sh repo run`, `real.rs:251-253`) | host | 60 s (unverified) | `repo/run_id` | a run already in `ingested` |
//!
//! ## Silence
//!
//! An idle tick has:
//!
//! - no gates listed, or no ESCALATE lines
//! - no new flaky or red-twice suite without an open bead
//! - no new run ids
//!
//! It emits the observe probe and nothing else. *Today an idle tick is not silent.* It forks
//! `tsd-ingest.sh` ×20 and `bd gate discover` per branch × pr repo every 2 min.
//!
//! ## Replaces
//!
//! - `spira-gate-check-prod.{service,timer}` and the shim `spira/gate-check.sh`, which sources
//!   `conf.sh` and execs `gate-check --home`.
//! - The children listed above.
//! - Pinned keys: `SPIRA_FLAKY_GH_REPO`, `SPIRA_HOME`. The lib.sh repository-map seam is already gone
//!   (`real.rs:72-86`).
//!
//! ## Reverse
//!
//! Release rollback restores the timer and the shim. Per task, `Observe` mode shadows every
//! `gate.check.*`. Its writes are deduped by title and gate id, so running old and new together
//! for one tick at worst double-ingests TSD.
//!

use std::collections::{BTreeMap, BTreeSet};

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::kit::{self, Act, Cadence, Shape};

pub const NAME: &str = "gate.check";
pub const STREAM: &str = "task.gate.check";
pub const TICK: EventKindTag = "gate.check.tick";
pub const STATE: EventKindTag = "gate.check.state";
pub const OBSERVE: &str = "gate.check.observe";
pub const DISCOVER: &str = "gate.check.discover";
pub const RESOLVE: &str = "gate.check.resolve";
pub const CI_FAILED: &str = "gate.check.ci_failed";
pub const FILE_BEAD: &str = "gate.check.file_bead";
pub const RAISE_PRIORITY: &str = "gate.check.raise_priority";
pub const NOTE: &str = "gate.check.note";
pub const TSD_INGEST: &str = "gate.check.tsd_ingest";

pub const CADENCE_MS: u64 = 120_000;
/// The contract gives the probe no deadline of its own; its slowest read is the 5 s LcClient `list`.
pub const OBSERVE_DEADLINE_MS: u64 = 30_000;

pub const ACTS: &[(&str, u64)] = &[(DISCOVER, 10_000), (RESOLVE, 10_000), (CI_FAILED, 5_000), (FILE_BEAD, 30_000), (RAISE_PRIORITY, 10_000), (NOTE, 10_000), (TSD_INGEST, 60_000)];
pub const EMITS: &[EventKindTag] = &[];
pub const EFFECTS: &[&str] = &[OBSERVE, DISCOVER, RESOLVE, CI_FAILED, FILE_BEAD, RAISE_PRIORITY, NOTE, TSD_INGEST];
pub const KEYS: &[&str] = &["SPIRA_FLAKY_GH_REPO"];
/// `SPIRA_HOME` is the process's own home, not a `conf.d` key.
pub const UNREGISTERED: &[&str] = &["SPIRA_HOME"];

pub const SHAPE: Shape = Shape { name: NAME, stream: STREAM, tick: TICK, state: STATE, observe: OBSERVE, observe_deadline_ms: OBSERVE_DEADLINE_MS };

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bead {
    pub id: String,
    pub priority: u8,
    pub open: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub repo: String,
    pub branch: String,
    pub conclusion: String,
    pub head_sha: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ci {
    pub recent_runs: Vec<Run>,
    pub failed_main_runs: Vec<Run>,
    pub last_green_sha: Option<String>,
    /// Annotations per job id, from which `flaky_suites` and `red_twice_suites` read.
    pub annotations_by_job: BTreeMap<String, Vec<String>>,
    pub fail_lines: BTreeMap<String, Vec<String>>,
}

/// A pr-mode repository and its branches, for bd-gate discovery.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRepo {
    pub name: String,
    pub branches: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub gate_list: Vec<String>,
    /// `bd gate check --type=gh:run`, stdout and stderr merged.
    pub gate_check_out: String,
    /// The bead each ESCALATE line's gate blocks (`bd show <gate>`), `None` for none.
    pub gate_blocked: BTreeMap<String, Option<String>>,
    /// Open lifecycle rows; `None` when unreadable, and then nothing is filed.
    pub lc_open: Option<Vec<String>>,
    pub beads_by_title: BTreeMap<String, Bead>,
    pub ci: Ci,
    pub pr_repos: Vec<PrRepo>,
    /// `SPIRA_FLAKY_GH_REPO` as pinned; empty skips steps 3–5.
    pub flaky_gh_repo: String,
}

/// Carried between ticks: every run id already ingested into TSD.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub ingested: BTreeSet<String>,
}

/// Record `write` over `recorded`, refusing a stale write: one that drops an id already recorded.
pub fn record_ingested(_recorded: &Carry, _write: &Carry) -> Result<Carry, String> {
    crate::not_ported!(NAME)
}

/// The walk of `gate-check/src/main.rs:63-190` over the snapshot.
pub fn decide(_carry: &Carry, _snap: &Snapshot) -> (Carry, Vec<Act>) {
    crate::not_ported!(NAME)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateCheck {
    pub cadence: Cadence,
    pub pinned: BTreeMap<String, String>,
}

impl GateCheck {
    pub fn new(pinned: BTreeMap<String, String>) -> Self {
        GateCheck { cadence: Cadence::Every(CADENCE_MS), pinned }
    }

    pub fn from_cfg() -> Self {
        Self::new(kit::pin(NAME, KEYS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("gate.check/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for GateCheck {
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
