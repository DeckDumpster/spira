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

/// The real sources (S9) are `crate::sources`; their event shapes are this family's.
pub use crate::sources;
use super::Task;

pub mod cockpit_tier_5s;
pub mod cockpit_tier_600s;
pub mod cockpit_tier_60s;
pub mod mail_arrived;
pub mod mail_wake;
pub mod submission;
pub mod unit_ran;

/// The ported cockpit tiers. Each tier's runners are the shared ones in [`cockpit`].
pub static TASKS: &[Task] = &[
    Task {
        name: cockpit_tier_5s::NAME,
        boot: cockpit_tier_5s::CockpitTier5s::boot,
        cadence_ms: |s| super::f::cadence(s, cockpit_tier_5s::CADENCE_KEY),
        effects: cockpit_tier_5s::EFFECTS,
        runners: &[(cockpit_tier_5s::OBSERVE, cockpit::run_observe), (cockpit_tier_5s::PUBLISH, cockpit::run_publish)],
        keys: &[cockpit_tier_5s::KEYS],
        build: |s| refuse(cockpit_tier_5s::NAME, cockpit_tier_5s::CockpitTier5s::from_settings(s)).registration(),
    },
    Task {
        name: cockpit_tier_60s::NAME,
        boot: cockpit_tier_60s::CockpitTier60s::boot,
        cadence_ms: |s| super::f::cadence(s, cockpit_tier_60s::CADENCE_KEY),
        effects: cockpit_tier_60s::EFFECTS,
        runners: &[(cockpit_tier_60s::OBSERVE, cockpit::run_observe), (cockpit_tier_60s::PUBLISH, cockpit::run_publish), (cockpit_tier_60s::SLOTS_SAMPLE, cockpit::run_slots_sample)],
        keys: &[cockpit_tier_60s::KEYS],
        build: |s| refuse(cockpit_tier_60s::NAME, cockpit_tier_60s::CockpitTier60s::from_settings(s)).registration(),
    },
    Task {
        name: cockpit_tier_600s::NAME,
        boot: cockpit_tier_600s::CockpitTier600s::boot,
        cadence_ms: |s| super::f::cadence(s, cockpit_tier_600s::CADENCE_KEY),
        effects: cockpit_tier_600s::EFFECTS,
        runners: &[(cockpit_tier_600s::OBSERVE, cockpit::run_observe), (cockpit_tier_600s::PUBLISH, cockpit::run_publish)],
        keys: &[cockpit_tier_600s::KEYS],
        build: |s| refuse(cockpit_tier_600s::NAME, cockpit_tier_600s::CockpitTier600s::from_settings(s)).registration(),
    },
    mail_wake::ROW,
];

/// Every cockpit tier is ported; the sources are not rows.
pub static PENDING: &[Task] = &[];

/// What the three cockpit tiers share: the fragment lifecycle the supervisor carries per probe,
/// the snapshot shape, the decision that merges a tier's fragments into one `Kv`, the answer
/// step, and the runners each tier's observe, publish and slots sample run on.
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

    impl FragmentState {
        /// The status word the supervisor wrote (`_PROBE_STATUS`), which is also the step's `on`.
        pub fn word(self) -> &'static str {
            match self {
                FragmentState::Never => "never",
                FragmentState::Ok => "ok",
                FragmentState::Stale => "stale",
                FragmentState::Error => "error",
                FragmentState::Timeout => "timeout",
            }
        }

        pub fn of_word(w: &str) -> Option<FragmentState> {
            [FragmentState::Never, FragmentState::Ok, FragmentState::Stale, FragmentState::Error, FragmentState::Timeout].into_iter().find(|s| s.word() == w)
        }

        /// Whether a fragment in this status contributes its values (the merge's rule).
        pub fn has_values(self) -> bool {
            matches!(self, FragmentState::Ok | FragmentState::Stale)
        }
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

    /// A fragment's next status on `on`, or a refusal naming the status it is in.
    pub fn step(from: FragmentState, on: &str) -> Result<FragmentState, String> {
        FRAGMENT_TRANSITIONS.iter().find(|t| t.from == from && t.on == on).map(|t| t.to).ok_or_else(|| format!("a fragment in {from:?} cannot take {on:?}"))
    }

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

    /// The pinned key that makes this scheduler the cockpit snapshot's one writer: the
    /// supervisor's `may_write`, which was `INVOCATION_ID` or `SPIRA_COCKPIT_FORCE`.
    pub const WRITE_KEY: &str = "SPIRA_RUNTIME_COCKPIT_TIER_WRITE";

    /// Keys that move on every read and so never make a snapshot new on their own: the pass's
    /// own time, and each probe's read time.
    pub fn volatile(key: &str) -> bool {
        key == "SP_AT" || key == "SP_PASS_SECS" || key.starts_with("_PROBE_AT_")
    }

    /// The probe's name as the registry and the pane's meta keys spell it: `cockpit.now` is `now`.
    pub fn registry_name(probe: &str) -> &str {
        probe.strip_prefix("cockpit.").unwrap_or(probe)
    }

    /// The tier's merged `Kv`: per probe in name order its meta keys, then every value key, the
    /// first probe's value winning. A fragment with no values (`never`, `error`, `timeout`)
    /// reads `?` for each key it names, never 0.
    pub fn merged(snap: &Snapshot) -> Vec<(String, String)> {
        let mut out = vec![];
        for (name, f) in &snap.fragments {
            let n = registry_name(name);
            out.push((format!("_PROBE_AT_{n}"), f.at.to_string()));
            out.push((format!("_PROBE_STATUS_{n}"), f.status.word().to_string()));
            out.push((format!("SP_PROBE_KILLED_{n}"), u8::from(f.killed).to_string()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in snap.fragments.values() {
            for (k, v) in &f.kv {
                if seen.insert(k.clone()) {
                    out.push((k.clone(), if f.status.has_values() { v.clone() } else { "?".into() }));
                }
            }
        }
        out
    }

    /// `KEY=value` lines, unquoted: the publish writer quotes.
    pub fn render(kv: &[(String, String)]) -> String {
        kv.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
    }

    /// The digest of the keys that make a snapshot new.
    pub fn digest(kv: &[(String, String)]) -> String {
        let stable: String = kv.iter().filter(|(k, _)| !volatile(k)).map(|(k, v)| format!("{k}={v}\n")).collect();
        let d = ring::digest::digest(&ring::digest::SHA256, stable.as_bytes());
        d.as_ref().iter().take(16).map(|b| format!("{b:02x}")).collect()
    }

    /// Each probe's carried status after this read. A status moves only along
    /// [`FRAGMENT_TRANSITIONS`]; its `at` is the time it moved, so an idle read changes
    /// nothing. A move the table does not have keeps the carried status.
    pub fn carry_fragments(carry: &Carry, snap: &Snapshot) -> BTreeMap<String, FragmentCarry> {
        snap.fragments
            .iter()
            .map(|(name, f)| {
                let was = carry.fragments.get(name).cloned().unwrap_or_default();
                let next = if was.status == f.status {
                    FragmentCarry { killed: f.killed, ..was }
                } else {
                    match step(was.status, f.status.word()) {
                        Ok(to) => FragmentCarry { at: f.at, status: to, killed: f.killed },
                        Err(_) => was,
                    }
                };
                (name.clone(), next)
            })
            .collect()
    }

    /// The tier decision: refuse naming `may_write` off the supervised instance; carry each
    /// fragment's status; publish the merged `Kv` as `publish` when its digest moved, or, with
    /// the digest unchanged, refresh it when a newer pass carries the pane's clock (`SP_AT`) so
    /// the snapshot never reads stale. [`answer`] runs a refresh as a probe, off the journal.
    pub fn decide_tier(carry: &Carry, snap: &Snapshot, publish: &str) -> Result<(Carry, Vec<EffectSpec>), String> {
        if !snap.supervised {
            return Err(format!("may_write: not the supervised instance; {WRITE_KEY} names the one writer"));
        }
        let kv = merged(snap);
        let d = digest(&kv);
        let newest = |at: &mut dyn Iterator<Item = u64>| at.max().unwrap_or(0);
        let fresher = newest(&mut snap.fragments.values().map(|f| f.at)) > newest(&mut carry.fragments.values().map(|f| f.at));
        let next = Carry { fragments: carry_fragments(carry, snap), digest: Some(d.clone()) };
        let changed = carry.digest.as_deref() != Some(d.as_str());
        let refresh = !changed && fresher && kv.iter().any(|(k, _)| k == "SP_AT");
        let acts = if changed || refresh { vec![EffectSpec { name: publish.into(), input: render(&kv), deadline_ms: 1_000 }] } else { vec![] };
        Ok((next, acts))
    }

    /// The observe's input: each sub-probe with its timeout, and the pinned config, as JSON.
    pub fn probe_input(probes: &[(&str, u64)], config: &BTreeMap<String, String>) -> String {
        serde_json::json!({ "probes": probes.iter().map(|(n, t)| serde_json::json!({ "name": n, "timeout_ms": t })).collect::<Vec<_>>(), "config": config }).to_string()
    }

    /// Whether `act` stays off the journal: a sample, or a publish whose digest did not move (a
    /// refresh of the volatile keys only).
    fn is_probe(act: &EffectSpec, digest_moved: bool) -> bool {
        act.name.ends_with(".slots_sample") || (act.name.ends_with(".publish") && !digest_moved)
    }

    /// The observe's answer through `decide`: a refusal is a wide-event field, never a write.
    pub fn answer(ctx: &Ctx, view: &Projection, ev: &Event, state: EventKindTag, body: &str, decide: Decide) -> Vec<Command> {
        let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
        let carry = carried::<Carry>(view, &ev.stream, state);
        match decide(&carry, &snap) {
            Ok((next, acts)) => {
                let moved = carry.digest != next.digest;
                let (probes, writes): (Vec<EffectSpec>, Vec<EffectSpec>) = acts.into_iter().partition(|a| is_probe(a, moved));
                let mut out = commands(&carry, &next, state, writes);
                out.extend(probes.into_iter().map(Command::Probe));
                out
            }
            Err(refusal) => {
                ctx.event().set("cockpit.refused", refusal.as_str());
                vec![]
            }
        }
    }

    #[derive(Deserialize)]
    struct ProbeArg {
        name: String,
        timeout_ms: u64,
    }

    #[derive(Deserialize)]
    struct ObserveInput {
        probes: Vec<ProbeArg>,
        #[serde(default)]
        config: BTreeMap<String, String>,
    }

    /// The observe runner: the tier's probes in-process through `cockpit_collect::tiers`, each
    /// under its own timeout, as a [`Snapshot`].
    pub fn run_observe(_: &crate::Effects, _: &str, s: &EffectSpec) -> Result<String, String> {
        let input: ObserveInput = serde_json::from_str(&s.input).map_err(|e| format!("{}: input: {e}", s.name))?;
        let probes: Vec<(String, std::time::Duration)> = input.probes.iter().map(|p| (registry_name(&p.name).to_string(), std::time::Duration::from_millis(p.timeout_ms))).collect();
        let got = cockpit_collect::tiers::observe(&probes);
        let fragments = input
            .probes
            .iter()
            .zip(got)
            .map(|(p, f)| (p.name.clone(), Fragment { status: FragmentState::of_word(f.status).unwrap_or(FragmentState::Error), at: f.at.max(0) as u64, killed: f.killed, kv: f.kv.into_iter().collect() }))
            .collect();
        let snap = Snapshot { supervised: input.config.get(WRITE_KEY).is_some_and(|v| matches!(v.as_str(), "1" | "true")), fragments };
        serde_json::to_string(&snap).map_err(|e| e.to_string())
    }

    /// The tier a `<tier>.publish` or `<tier>.slots_sample` effect belongs to.
    fn tier_of(effect: &str) -> &str {
        effect.rsplit_once('.').map_or(effect, |(t, _)| t)
    }

    /// The publish runner: the tier's merged `Kv` into `run/cockpit.env` with every other tier's.
    pub fn run_publish(e: &crate::Effects, _: &str, s: &EffectSpec) -> Result<String, String> {
        if e.world.run_dir.as_os_str().is_empty() {
            return Err("no run dir configured".into());
        }
        cockpit_collect::tiers::publish(&e.world.run_dir, tier_of(&s.name), &s.input)
    }

    /// The slots sample runner: one `slots` tsd row under the run dir.
    pub fn run_slots_sample(e: &crate::Effects, _: &str, s: &EffectSpec) -> Result<String, String> {
        if e.world.run_dir.as_os_str().is_empty() {
            return Err("no run dir configured".into());
        }
        cockpit_collect::tiers::slots_sample(&e.world.run_dir, &s.input)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn frag(status: FragmentState, at: u64, kv: &[(&str, &str)]) -> Fragment {
            Fragment { status, at, killed: false, kv: kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }
        }

        fn snap(at: u64, round: &str) -> Snapshot {
            Snapshot { supervised: true, fragments: [("cockpit.now".to_string(), frag(FragmentState::Ok, at, &[("SP_AT", &at.to_string())])), ("cockpit.round".to_string(), frag(FragmentState::Ok, at, &[("SP_ROUND", round)]))].into() }
        }

        #[test]
        fn a_newer_pass_with_the_same_values_refreshes_off_the_journal_and_moves_no_carry() {
            let (first, acts) = decide_tier(&Carry::default(), &snap(10, "r1"), "t.publish").unwrap();
            assert_eq!(acts.len(), 1);
            let (second, acts) = decide_tier(&first, &snap(15, "r1"), "t.publish").unwrap();
            assert_eq!(second, first, "an idle read carries nothing new");
            assert_eq!(acts.len(), 1, "the clock is refreshed");
            assert!(is_probe(&acts[0], false));
            let (third, acts) = decide_tier(&second, &snap(20, "r2"), "t.publish").unwrap();
            assert_ne!(third.digest, second.digest);
            assert!(!is_probe(&acts[0], true), "a moved digest is a journaled publish");
        }

        #[test]
        fn a_status_moves_only_along_the_table_and_its_at_is_the_move() {
            let c = Carry { fragments: [("p".to_string(), FragmentCarry { at: 5, status: FragmentState::Error, killed: true })].into(), digest: None };
            let s = Snapshot { supervised: true, fragments: [("p".to_string(), frag(FragmentState::Timeout, 9, &[]))].into() };
            assert_eq!(carry_fragments(&c, &s)["p"].status, FragmentState::Error, "error to timeout is not a move the table has");
            let s = Snapshot { supervised: true, fragments: [("p".to_string(), frag(FragmentState::Ok, 9, &[]))].into() };
            assert_eq!(carry_fragments(&c, &s)["p"], FragmentCarry { at: 9, status: FragmentState::Ok, killed: false });
            assert_eq!(merged(&Snapshot { supervised: true, fragments: [("cockpit.x".to_string(), frag(FragmentState::Error, 0, &[("SP_X", "0")]))].into() }).last(), Some(&("SP_X".to_string(), "?".to_string())));
        }

        #[test]
        fn the_observe_input_names_every_probe_and_the_writer_key() {
            let cfg: BTreeMap<String, String> = [(WRITE_KEY.to_string(), "1".to_string())].into();
            let input: ObserveInput = serde_json::from_str(&probe_input(&[("cockpit.now", 30_000)], &cfg)).unwrap();
            assert_eq!((input.probes[0].name.as_str(), input.probes[0].timeout_ms, input.config[WRITE_KEY].as_str()), ("cockpit.now", 30_000, "1"));
            assert_eq!(tier_of("cockpit-tier-60s.slots_sample"), "cockpit-tier-60s");
        }
    }
}
