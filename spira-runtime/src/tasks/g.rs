//! Family G, sources and collector, of the scheduler consolidation design
//! (`wiki/projects/spira/designs/scheduler-consolidation-2026-11-03.md`). Its rows join the
//! service through `tasks::FAMILIES`.
//!
//! Two kinds of thing live here. The cockpit's three tier tasks are P1 rows; like family F's they
//! wait in [`PENDING`] until each is ported (a row in [`TASKS`] must be named in
//! `SPIRA_TASK_MODES` and its keys registered, both outside this family's files). The three
//! sources (`mail.arrived`, `unit.ran`, `submission`) are not rows: each is a
//! `runtime::sources::Source` the host registers, declared in its own module with the event it
//! submits and the backend it reads.

pub use super::f::{cadence_ms, pinned, refuse, Transition};
use super::Task;

pub mod cockpit_tier_5s;
pub mod cockpit_tier_600s;
pub mod cockpit_tier_60s;
pub mod mail_arrived;
pub mod submission;
pub mod unit_ran;

pub static TASKS: &[Task] = &[];

/// The cockpit tier rows not yet ported.
pub static PENDING: &[Task] = &[
    Task {
        name: cockpit_tier_5s::NAME,
        boot: cockpit_tier_5s::CockpitTier5s::boot,
        cadence_ms: |_| super::f::cadence(cockpit_tier_5s::CADENCE_KEY),
        effects: cockpit_tier_5s::EFFECTS,
        runners: &[
            (cockpit_tier_5s::OBSERVE, |_, _, _| unimplemented!("cockpit-tier-5s.observe: not ported yet")),
            (cockpit_tier_5s::PUBLISH, |_, _, _| unimplemented!("cockpit-tier-5s.publish: not ported yet")),
        ],
        keys: &[cockpit_tier_5s::KEYS],
        build: |_| refuse(cockpit_tier_5s::NAME, cockpit_tier_5s::CockpitTier5s::from_cfg()).registration(),
    },
    Task {
        name: cockpit_tier_60s::NAME,
        boot: cockpit_tier_60s::CockpitTier60s::boot,
        cadence_ms: |_| super::f::cadence(cockpit_tier_60s::CADENCE_KEY),
        effects: cockpit_tier_60s::EFFECTS,
        runners: &[
            (cockpit_tier_60s::OBSERVE, |_, _, _| unimplemented!("cockpit-tier-60s.observe: not ported yet")),
            (cockpit_tier_60s::PUBLISH, |_, _, _| unimplemented!("cockpit-tier-60s.publish: not ported yet")),
            (cockpit_tier_60s::SLOTS_SAMPLE, |_, _, _| unimplemented!("cockpit-tier-60s.slots_sample: not ported yet")),
        ],
        keys: &[cockpit_tier_60s::KEYS],
        build: |_| refuse(cockpit_tier_60s::NAME, cockpit_tier_60s::CockpitTier60s::from_cfg()).registration(),
    },
    Task {
        name: cockpit_tier_600s::NAME,
        boot: cockpit_tier_600s::CockpitTier600s::boot,
        cadence_ms: |_| super::f::cadence(cockpit_tier_600s::CADENCE_KEY),
        effects: cockpit_tier_600s::EFFECTS,
        runners: &[
            (cockpit_tier_600s::OBSERVE, |_, _, _| unimplemented!("cockpit-tier-600s.observe: not ported yet")),
            (cockpit_tier_600s::PUBLISH, |_, _, _| unimplemented!("cockpit-tier-600s.publish: not ported yet")),
        ],
        keys: &[cockpit_tier_600s::KEYS],
        build: |_| refuse(cockpit_tier_600s::NAME, cockpit_tier_600s::CockpitTier600s::from_cfg()).registration(),
    },
];

/// What the three cockpit tiers share: the fragment lifecycle the supervisor carries per probe,
/// the snapshot shape, and the answer step.
pub mod cockpit {
    use std::collections::BTreeMap;

    use runtime::event::*;
    use serde::{Deserialize, Serialize};

    use super::Transition;
    use crate::tasks::f::commands;
    use crate::watchers::carried;

    /// The supervisor's fragment status (`never/ok/stale/error/timeout`).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub enum FragmentState {
        #[default]
        Never,
        Ok,
        Stale,
        Error,
        Timeout,
    }

    pub const FRAGMENT_TRANSITIONS: &[Transition<FragmentState>] = &[
        Transition { from: FragmentState::Never, on: "ok", to: FragmentState::Ok, effects: &[] },
        Transition { from: FragmentState::Never, on: "error", to: FragmentState::Error, effects: &[] },
        Transition { from: FragmentState::Never, on: "timeout", to: FragmentState::Timeout, effects: &[] },
        Transition { from: FragmentState::Ok, on: "ok", to: FragmentState::Ok, effects: &[] },
        Transition { from: FragmentState::Ok, on: "stale", to: FragmentState::Stale, effects: &[] },
        Transition { from: FragmentState::Ok, on: "error", to: FragmentState::Error, effects: &[] },
        Transition { from: FragmentState::Ok, on: "timeout", to: FragmentState::Timeout, effects: &[] },
        Transition { from: FragmentState::Stale, on: "ok", to: FragmentState::Ok, effects: &[] },
        Transition { from: FragmentState::Error, on: "ok", to: FragmentState::Ok, effects: &[] },
        Transition { from: FragmentState::Timeout, on: "ok", to: FragmentState::Ok, effects: &[] },
    ];

    /// One sub-probe's answer: its status, when it was read, whether it was killed, its keys.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Fragment {
        pub status: FragmentState,
        pub at: u64,
        pub killed: bool,
        pub kv: BTreeMap<String, String>,
    }

    /// A tier's observe answer: whether this is the supervised instance (`may_write`) and each
    /// sub-probe's fragment by probe name.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Snapshot {
        pub supervised: bool,
        pub fragments: BTreeMap<String, Fragment>,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct FragmentCarry {
        pub at: u64,
        pub status: FragmentState,
        pub killed: bool,
    }

    /// `<tier>.state`: each probe's fragment status and the digest of the last published `Kv`.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Carry {
        pub fragments: BTreeMap<String, FragmentCarry>,
        pub digest: Option<String>,
    }

    pub type Decide = fn(&Carry, &Snapshot) -> Result<(Carry, Vec<EffectSpec>), String>;

    /// The observe's input: each sub-probe with its timeout, as JSON.
    pub fn probe_input(probes: &[(&str, u64)]) -> String {
        serde_json::to_string(&probes.iter().map(|(n, t)| serde_json::json!({ "name": n, "timeout_ms": t })).collect::<Vec<_>>()).unwrap_or_default()
    }

    /// The observe's answer through `decide`: a refusal is a wide-event field, never a write.
    pub fn answer(ctx: &Ctx, view: &Projection, ev: &Event, state: EventKindTag, body: &str, decide: Decide) -> Vec<Command> {
        let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
        let carry = carried::<Carry>(view, &ev.stream, state);
        match decide(&carry, &snap) {
            Ok((next, acts)) => commands(&carry, &next, state, acts),
            Err(refusal) => {
                ctx.event().set("cockpit.refused", refusal.as_str());
                vec![]
            }
        }
    }
}
