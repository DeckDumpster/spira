//! `cockpit-tier-600s`: the pane's slow tier, probes `livelock` and `drift`. Replaces those
//! registry rows of `cockpit-collect` and their re-exec and `timeout` chain. Pattern P1.
//! Contract: `wiki/projects/spira/designs/tasks/cockpit-tier-600s.md`; siblings
//! [`super::cockpit_tier_5s`] and [`super::cockpit_tier_60s`]. The decision, the merge and the
//! runners are shared in [`super::cockpit`]; the probes run in-process (`cockpit_collect::tiers`).
//!
//! # Tick
//!
//! Every 600 s (`SPIRA_RUNTIME_COCKPIT_TIER_600S_SECS`). Both probes ride one observe
//! ([`OBSERVE`], input naming [`PROBES`]), timeout 300 s each. Today's
//! `SPIRA_COCKPIT_SLOW_CONCURRENT` cap becomes the effector class cap (S3): never more than one
//! slow probe at a time.
//!
//! # Observe
//!
//! `livelock`: `strand::detectors::detect_livelocked` and `detect_invalid_closed` with bead lists
//! from the content verb over `LcClient` and states from the lifecycle projection, `git rev-list`
//! per ref (git stays a class B child), the allow file `run/invalid-closed.allow`, and the
//! emptiness positive control, in-process. `drift`: `drift.sh` ported into the task, a git status
//! of the prod checkout and a compare of the user unit directory against the install manifest
//! (unverified that `install` exposes one).
//!
//! # Decide
//!
//! Pure cores: `drift_value(code, out, dirty_word, prefix)` and the LIVELOCK / INVALID-CLOSED /
//! UNFILED-FOLLOW / ALLOWED-IC row classification (LIVELOCK capped at 20 rows). A detector
//! answering empty while the store does not answer is `?`, never 0. Carry: fragment status per
//! probe ([`TRANSITIONS`]). Fixtures: the `drift_value` tests and the strand detector tests.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `cockpit-tier-600s.publish` | host | 1 s | digest of the merged `Kv` | not the supervised instance |
//!
//! Nothing else: neither probe writes. Whether a drift finding should page is a separate decision.
//!
//! # Silence
//!
//! No change in the drift words or the livelock and invalid-closed rows: no publish, no journal.
//!
//! # Replaces
//!
//! Registry rows `livelock`, `drift` and their chain; per 10 min `bash drift.sh` ×2 and its
//! children, `spira-lc content` ×2–4, the `bd`-via-strand list reads, `git rev-list` per ref.
//! Pinned config: [`KEYS`]; the strand `Config` resolves from `strand::config::Live`.
//!
//! # Reverse
//!
//! Release rollback restores the collector; Observe mode shadows `publish`.

use std::collections::BTreeMap;

use runtime::event::*;

use super::cockpit::{decide_tier, Carry, FragmentState, Snapshot, FRAGMENT_TRANSITIONS, WRITE_KEY};
use super::Transition;
use crate::tasks::f::Cycle;

pub const NAME: &str = "cockpit-tier-600s";
pub const STREAM: &str = "task.cockpit-tier-600s";
pub const TICK: EventKindTag = "cockpit-tier-600s.tick";
pub const STATE: EventKindTag = "cockpit-tier-600s.state";

pub const OBSERVE: &str = "cockpit-tier-600s.observe";
pub const PUBLISH: &str = "cockpit-tier-600s.publish";
pub const EFFECTS: &[&str] = &[OBSERVE, PUBLISH];

/// The sub-probes [`OBSERVE`] runs, each with its own timeout.
pub const PROBES: &[(&str, u64)] = &[("cockpit.livelock", 300_000), ("cockpit.drift", 300_000)];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_COCKPIT_TIER_600S_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, WRITE_KEY, "SPIRA_LOCAL_UNITS"];

const OBSERVE_DEADLINE_MS: u64 = 600_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: OBSERVE_DEADLINE_MS };

pub const TRANSITIONS: &[Transition<FragmentState>] = FRAGMENT_TRANSITIONS;

/// A fragment's next status on `on`, or a refusal naming the status it is in.
pub fn step(from: FragmentState, on: &str) -> Result<FragmentState, String> {
    super::cockpit::step(from, on)
}

/// The pure decision: the next carry and the acts (the publish when the merged `Kv` changed); a
/// refusal naming `may_write` when this is not the supervised instance.
pub fn decide(carry: &Carry, snap: &Snapshot) -> Result<(Carry, Vec<EffectSpec>), String> {
    decide_tier(carry, snap, PUBLISH)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CockpitTier600s {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl CockpitTier600s {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(CockpitTier600s { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("cockpit-tier-600s/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for CockpitTier600s {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, || super::cockpit::probe_input(PROBES, &self.config), |body| super::cockpit::answer(ctx, view, ev, STATE, body, decide))
    }
}
