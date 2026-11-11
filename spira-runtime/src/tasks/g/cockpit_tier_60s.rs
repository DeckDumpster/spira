//! `cockpit-tier-60s`: the pane's minute tier, probes `slots`, `reachable` and `tsd`. Replaces
//! those registry rows of `cockpit-collect` and their re-exec and `timeout` chain. Pattern P1.
//! Contract: `wiki/projects/spira/designs/tasks/cockpit-tier-60s.md`; siblings
//! [`super::cockpit_tier_5s`] (which lists the rows that become projections) and
//! [`super::cockpit_tier_600s`]. The decision, the merge and the runners are shared in
//! [`super::cockpit`]; the probes run in-process (`cockpit_collect::tiers`).
//!
//! # Tick
//!
//! Every 60 s (`SPIRA_RUNTIME_COCKPIT_TIER_60S_SECS`). The three probes ride one observe
//! ([`OBSERVE`], input naming [`PROBES`]); each keeps its own timeout (`slots` 20 s, `tsd` 30 s,
//! `reachable` 120 s) and the registry's skip-if-running when it overruns the cadence.
//!
//! # Observe
//!
//! `slots`: live dwarves and lanes from the `sentinel` task's carry, the ready count from the
//! `spira-claim` library (not the pane's own last output, which is a self-loop today), holds and
//! states from the lifecycle projection (S7). `reachable`: the content verb over the in-process
//! `LcClient`, runes from pinned settings, the ctrl file read stays a host read. `tsd`: the metrics
//! store's query API in-process (unverified that `tsd` exposes `where`/`rework` as library calls).
//!
//! # Decide
//!
//! Pure cores reused: `reachable_bfs`, `tsd_keys_with` (`where_line`/`rework_line`) and the slots
//! arithmetic (ceiling, free, pool). Carry: per-probe fragment status ([`TRANSITIONS`]). Fixtures:
//! the `reachable.rs` and `tsd.rs` unit tests.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `cockpit-tier-60s.publish` | host | 1 s | digest of the merged `Kv` | not the supervised instance |
//! | `cockpit-tier-60s.slots_sample` | store | 2 s | `slots/<minute>` | — |
//!
//! `publish` merges into the one `cockpit.env` writer shared with the other tiers. `slots_sample`
//! replaces the `tsd-write --family slots` fork after every ok `slots` fragment with the
//! in-process metrics push.
//!
//! # Silence
//!
//! Unchanged snapshot: no `publish`. `slots_sample` is the time series and fires every tick by
//! design; the contract counts it as zero journal rows, which a journaled effect is not (see the
//! T2 note in `tests/it/task_g_cockpit_tier_60s.rs`).
//!
//! # Replaces
//!
//! Registry rows `slots`, `reachable`, `tsd` and their chain; per minute `bash lib.sh`
//! ×(2 + 1 + runes), `spira-lc` ×4 or more, `tsd-query.sh` ×2, `tsd-write` ×1. Pinned config:
//! [`KEYS`]; the lanes cap and the ctrl path have no key of their own today.
//!
//! # Reverse
//!
//! Observe mode shadows both effects.

use std::collections::BTreeMap;

use runtime::event::*;

use super::cockpit::{decide_tier, Carry, FragmentState, Snapshot, FRAGMENT_TRANSITIONS, WRITE_KEY};
use super::Transition;
use crate::tasks::f::Cycle;

pub const NAME: &str = "cockpit-tier-60s";
pub const STREAM: &str = "task.cockpit-tier-60s";
pub const TICK: EventKindTag = "cockpit-tier-60s.tick";
pub const STATE: EventKindTag = "cockpit-tier-60s.state";

pub const OBSERVE: &str = "cockpit-tier-60s.observe";
pub const PUBLISH: &str = "cockpit-tier-60s.publish";
pub const SLOTS_SAMPLE: &str = "cockpit-tier-60s.slots_sample";
pub const EFFECTS: &[&str] = &[OBSERVE, PUBLISH, SLOTS_SAMPLE];

/// The sub-probes [`OBSERVE`] runs, each with its own timeout.
pub const PROBES: &[(&str, u64)] = &[("cockpit.slots", 20_000), ("cockpit.reachable", 120_000), ("cockpit.tsd", 30_000)];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_COCKPIT_TIER_60S_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, WRITE_KEY, "SPIRA_LC_TIMEOUT", "SPIRA_BDQ_CONN_RETRIES"];

const OBSERVE_DEADLINE_MS: u64 = 120_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: OBSERVE_DEADLINE_MS };

pub const TRANSITIONS: &[Transition<FragmentState>] = FRAGMENT_TRANSITIONS;

/// A fragment's next status on `on`, or a refusal naming the status it is in.
pub fn step(from: FragmentState, on: &str) -> Result<FragmentState, String> {
    super::cockpit::step(from, on)
}

/// The pure decision: the next carry and the acts (the publish when the merged `Kv` changed, the
/// slots sample after an ok `slots` fragment); a refusal naming `may_write` when this is not the
/// supervised instance.
pub fn decide(carry: &Carry, snap: &Snapshot) -> Result<(Carry, Vec<EffectSpec>), String> {
    decide_with_sample(carry, snap)
}

/// The `slots` tsd fields an ok `slots` fragment carries, by the field names `tsd-write`
/// wrote (`live`, `ceiling`, `lanes_live`, `ready`, `capacity_paused`).
const SAMPLED: &[(&str, &str)] = &[("live", "SP_SLOTS_LIVE"), ("ceiling", "SP_SLOTS_CEILING"), ("lanes_live", "SP_SLOTS_LANES_LIVE"), ("ready", "SP_SLOTS_READY"), ("capacity_paused", "SP_SLOTS_CAPACITY_PAUSED")];

/// The tier decision plus the supervisor's `slots` tsd sample after an ok `slots` read that moved
/// the snapshot (an idle tick asks for nothing beyond its observe), run by `cockpit::answer` as a
/// probe, off the journal. The series records each change, not each minute.
fn decide_with_sample(carry: &Carry, snap: &Snapshot) -> Result<(Carry, Vec<EffectSpec>), String> {
    let (next, mut acts) = decide_tier(carry, snap, PUBLISH)?;
    let moved = carry.digest != next.digest;
    if let Some(slots) = snap.fragments.get("cockpit.slots").filter(|f| moved && f.status == FragmentState::Ok) {
        let input: String = SAMPLED.iter().map(|(field, key)| format!("{field}={}\n", slots.kv.get(*key).map_or("?", String::as_str))).collect();
        acts.push(EffectSpec::keyed(SLOTS_SAMPLE, format!("slots/{}", slots.at), input, 5_000));
    }
    Ok((next, acts))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CockpitTier60s {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl CockpitTier60s {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(CockpitTier60s { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("cockpit-tier-60s/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, super::cockpit::ROW_PARTS))
    }
}

impl Handler for CockpitTier60s {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, || super::cockpit::probe_input(PROBES, &self.config), |body| super::cockpit::answer(ctx, view, ev, STATE, body, decide))
    }
}
