//! `cockpit-tier-5s`: the pane's fast tier, probes `now`, `round` and `inflight`. Replaces the
//! three 5 s rows of `cockpit-collect`'s registry run by its supervisor
//! (`spira-cockpit-prod.service`: `spira-supervise cockpit-collect collect`). Pattern P1.
//! Contract: `wiki/projects/spira/designs/tasks/cockpit-tier-5s.md`; siblings
//! [`super::cockpit_tier_60s`] and [`super::cockpit_tier_600s`]. The decision, the merge and the
//! runners are shared in [`super::cockpit`]; the probes run in-process (`cockpit_collect::tiers`).
//!
//! # Tick
//!
//! Every 5 s (`SPIRA_RUNTIME_COCKPIT_TIER_5S_SECS`; the collector's `SPIRA_COCKPIT_TICK` is not registry config). Also woken by `unit.ran` for a gate, round or dwarf unit, so
//! WORK IN FLIGHT moves on the event. The three probes ride one observe ([`OBSERVE`], its input
//! naming [`PROBES`]), so an idle tick asks for one probe; each sub-probe keeps its own timeout
//! (`now` 30 s, `round` 60 s, `inflight` 60 s) and its skip-if-running inside the runner.
//!
//! # Observe
//!
//! The snapshot is each sub-probe's `Kv` fragment with its status. After the move: `now` reads the
//! `sentinel` task's carried dwarves, timer liveness from `scheduler status` (S13), the `release`
//! library, the hosted `overrides` carry, the `gate.worker` state and `dwarf::capacity`; `round`
//! and `inflight` read the round/batch and funnel projections (S7b), with no `queue` exec. The
//! shared read cache keyed by `SPIRA_PANE_READ_TTL` and the last-good fallback go.
//!
//! # Decide
//!
//! The probes' pure cores (`round_keys_from`, `inflight::compose`/`segment`) over the snapshot,
//! plus the supervisor's fragment lifecycle (`never/ok/stale/error/timeout`) carried per probe as
//! `{at, status, killed}` ([`TRANSITIONS`]). Fixtures: the `round.rs` and `inflight.rs` tests.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `cockpit-tier-5s.publish` | host | 1 s | digest of the merged `Kv` | not the supervised instance (`may_write`) |
//!
//! One effect: the merged snapshot written atomically to `run/cockpit.env` (and its history row),
//! the pane's read contract, unchanged. The `run/cockpit.d/` fragments go: the three tiers publish
//! into one `Kv` and one writer merges. `publish` is skipped when the digest is unchanged.
//!
//! # Silence
//!
//! Snapshot unchanged since the last publish: no effect, no journal. The observe is a probe, never
//! journaled.
//!
//! # Replaces
//!
//! The 5 s rows of `PROBES` and their child chain (supervisor re-exec, `timeout`, `cockpit-collect
//! probe`); per tick `spira-lc` ×2–3, `queue batch show` per repo, `systemctl` ×3, `release`,
//! `overrides.sh`, `git` per dwarf, `date`. Pinned config: [`KEYS`]. The other fifteen registry
//! rows become projections the pane reads at render time.
//!
//! # Reverse
//!
//! Release rollback restores the service and supervisor; Observe mode shadows `publish`, and the
//! old collector can run alongside for parity with `SPIRA_RUNTIME_COCKPIT_TIER_WRITE=1` on one side only (`SPIRA_COCKPIT_FORCE` on the other).

use std::collections::BTreeMap;

use runtime::event::*;

use super::cockpit::{decide_tier, Carry, FragmentState, Snapshot, FRAGMENT_TRANSITIONS, WRITE_KEY};
use super::Transition;
use crate::tasks::f::Cycle;

pub const NAME: &str = "cockpit-tier-5s";
pub const STREAM: &str = "task.cockpit-tier-5s";
pub const TICK: EventKindTag = "cockpit-tier-5s.tick";
pub const STATE: EventKindTag = "cockpit-tier-5s.state";

pub const OBSERVE: &str = "cockpit-tier-5s.observe";
pub const PUBLISH: &str = "cockpit-tier-5s.publish";
pub const EFFECTS: &[&str] = &[OBSERVE, PUBLISH];

/// The sub-probes [`OBSERVE`] runs, each with its own timeout.
pub const PROBES: &[(&str, u64)] = &[("cockpit.now", 30_000), ("cockpit.round", 60_000), ("cockpit.inflight", 60_000)];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_COCKPIT_TIER_5S_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, WRITE_KEY, "SPIRA_SNAP_STALE_S", "SPIRA_PANE_READ_TTL", "SPIRA_LC_TIMEOUT", "SPIRA_TRACE_MARK"];

const OBSERVE_DEADLINE_MS: u64 = 60_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: OBSERVE_DEADLINE_MS };

pub const TRANSITIONS: &[Transition<FragmentState>] = FRAGMENT_TRANSITIONS;

/// A fragment's next status on `on`, or a refusal naming the status it is in.
pub fn step(from: FragmentState, on: &str) -> Result<FragmentState, String> {
    super::cockpit::step(from, on)
}

/// The pure decision: the next carry and the acts (the publish, when the merged `Kv` changed); a refusal
/// naming `may_write` when this is not the supervised instance.
pub fn decide(carry: &Carry, snap: &Snapshot) -> Result<(Carry, Vec<EffectSpec>), String> {
    decide_tier(carry, snap, PUBLISH)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CockpitTier5s {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl CockpitTier5s {
    pub fn from_cfg() -> Result<Self, String> {
        Ok(CockpitTier5s { interval_ms: super::cadence_ms(CADENCE_KEY)?, config: super::pinned(KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("cockpit-tier-5s/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for CockpitTier5s {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, || super::cockpit::probe_input(PROBES, &self.config), |body| super::cockpit::answer(ctx, view, ev, STATE, body, decide))
    }
}
