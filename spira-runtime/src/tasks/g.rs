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
use super::{Reads, Task};

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
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: cockpit_tier_5s::CockpitTier5s::boot,
        cadence_ms: |s| super::f::cadence(s, cockpit_tier_5s::CADENCE_KEY),
        effects: cockpit_tier_5s::EFFECTS,
        runners: &[(cockpit_tier_5s::OBSERVE, cockpit::run_observe), (cockpit_tier_5s::PUBLISH, cockpit::run_publish)],
        keys: &[cockpit_tier_5s::KEYS],
        build: |s| refuse(cockpit_tier_5s::NAME, cockpit_tier_5s::CockpitTier5s::from_settings(s)).registration(),
    },
    Task {
        name: cockpit_tier_60s::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
        boot: cockpit_tier_60s::CockpitTier60s::boot,
        cadence_ms: |s| super::f::cadence(s, cockpit_tier_60s::CADENCE_KEY),
        effects: cockpit_tier_60s::EFFECTS,
        runners: &[(cockpit_tier_60s::OBSERVE, cockpit::run_observe), (cockpit_tier_60s::PUBLISH, cockpit::run_publish), (cockpit_tier_60s::SLOTS_SAMPLE, cockpit::run_slots_sample)],
        keys: &[cockpit_tier_60s::KEYS],
        build: |s| refuse(cockpit_tier_60s::NAME, cockpit_tier_60s::CockpitTier60s::from_settings(s)).registration(),
    },
    Task {
        name: cockpit_tier_600s::NAME,
        reads: Reads::None,
        plane: runtime::Plane::Work,
        signal: &[],
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
    #[serde(default)]
    pub struct FragmentCarry {
        pub at: u64,
        pub status: FragmentState,
        pub killed: bool,
    }

    /// `<tier>.state`: each probe's fragment status and the digest of the last published `Kv`.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(default)]
    pub struct Carry {
        pub fragments: BTreeMap<String, FragmentCarry>,
        pub digest: Option<String>,
    }

    /// What every tier keeps: the digest that stops a republish, and each fragment's time and status.
    pub const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("digest"), runtime::rows::Part::Map { name: "fragments", drop: &[] }];

    pub type Decide = fn(&Carry, &Snapshot) -> Result<(Carry, Vec<EffectSpec>), String>;

    /// The pinned key that names which scheduler instance is the cockpit snapshot's one
    /// writer.
    pub const WRITE_KEY: &str = "SPIRA_RUNTIME_COCKPIT_TIER_WRITE";

    /// Keys whose value moves with the clock alone: an age, a countdown, an elapsed time.
    pub const CLOCK_ONLY: &[&str] = &[
        "SP_SENTINEL_AGE", "SP_OPS_AGE", "SP_HEIMDALL_AGE", "SP_LC_STALE_S", "SP_CAPACITY_LEFT",
        "SP_AWAITING_AGE", "SP_QUEUE_BATCH_AGE", "SP_RATELIM_AGE", "SP_RATELIM_5H_MIN", "SP_RATELIM_7D_MIN",
        "SP_FUNNEL_CERTIFY_AGE", "SP_FUNNEL_RED_AGE", "SP_FUNNEL_CERT_AGE", "SP_FUNNEL_DONE_AGE",
        "SP_MAIL_OLDEST_AGE", "SP_SWEEP_AGE", "SP_SENT_FAILED_AGE_M", "SP_UNSENT_OLDEST_H", "SP_CLOSED_STRANDED_OLDEST_H",
    ];

    /// The per-dwarf suffixes of the same kind, spelled `SP_DWARF<i>_<suffix>`.
    pub const CLOCK_ONLY_DWARF: &[&str] = &["MIN", "WALL", "FUSE", "LEASE", "QUIET"];

    fn clock_only(key: &str) -> bool {
        CLOCK_ONLY.contains(&key)
            || key.strip_prefix("SP_DWARF").and_then(|r| r.split_once('_')).is_some_and(|(i, suffix)| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()) && CLOCK_ONLY_DWARF.contains(&suffix))
    }

    /// Keys that never make a snapshot new on their own: the pass's own time, each probe's read
    /// time, and every clock-only value. The file keeps them current through the refresh path.
    pub fn volatile(key: &str) -> bool {
        key == "SP_AT" || key == "SP_PASS_SECS" || key.starts_with("_PROBE_AT_") || clock_only(key)
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

    fn indexed(key: &str, prefix: &str) -> bool {
        key.strip_prefix(prefix).is_some_and(|i| !i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))
    }

    /// The part of a value that is not its leading age: past the first word of an
    /// `SP_INFLOW<i>` or `SP_EVENT<i>` row, past the first `|` of an `SP_MAIL<i>` row.
    pub fn stable_value<'a>(key: &str, value: &'a str) -> &'a str {
        if indexed(key, "SP_INFLOW") || indexed(key, "SP_EVENT") {
            value.split_once(char::is_whitespace).map_or("", |(_, rest)| rest.trim_start())
        } else if indexed(key, "SP_MAIL") {
            value.split_once('|').map_or("", |(_, rest)| rest)
        } else {
            value
        }
    }

    /// The digest of the keys that make a snapshot new.
    pub fn digest(kv: &[(String, String)]) -> String {
        let stable: String = kv.iter().filter(|(k, _)| !volatile(k)).map(|(k, v)| format!("{k}={}\n", stable_value(k, v))).collect();
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
    /// the digest unchanged, refresh it when a newer pass carries the pane's clock (`SP_AT`) or a
    /// row with a leading age (`stable_value` differs from the value) so the snapshot never reads stale. [`answer`] runs a refresh as a probe, off the journal.
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
        let refresh = !changed && fresher && kv.iter().any(|(k, v)| k == "SP_AT" || stable_value(k, v) != v);
        let acts = if changed || refresh { vec![EffectSpec::keyed(publish, format!("{publish}/{d}"), render(&kv), 1_000)] } else { vec![] };
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
    pub fn run_observe(e: &crate::Effects, _: &str, s: &EffectSpec) -> Result<String, String> {
        let input: ObserveInput = serde_json::from_str(&s.input).map_err(|e| format!("{}: input: {e}", s.name))?;
        let probes: Vec<(String, std::time::Duration)> = input.probes.iter().map(|p| (registry_name(&p.name).to_string(), std::time::Duration::from_millis(p.timeout_ms))).collect();
        let supervised = input.config.get(WRITE_KEY).is_some_and(|v| matches!(v.as_str(), "1" | "true"));
        view::set_context(e, supervised);
        let got = cockpit_collect::tiers::observe_with(&probes, view::probe);
        let fragments = input
            .probes
            .iter()
            .zip(got)
            .map(|(p, f)| (p.name.clone(), Fragment { status: FragmentState::of_word(f.status).unwrap_or(FragmentState::Error), at: f.at.max(0) as u64, killed: f.killed, kv: f.kv.into_iter().collect() }))
            .collect();
        let snap = Snapshot { supervised, fragments };
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
        let views = tier_of(&s.name) == super::cockpit_tier_5s::NAME;
        if views && e.world.cockpit_view_dir.as_os_str().is_empty() {
            return Err(view::NO_RUNTIME_DIR.into());
        }
        let published = cockpit_collect::tiers::publish(&e.world.run_dir, &e.world.ctx_status_dir, tier_of(&s.name), &s.input)?;
        if views {
            view::publish(&e.world.cockpit_view_dir)?;
        }
        Ok(published)
    }

    /// The cockpit ops document: the two groups of reads the scheduler keeps in process, written
    /// to the memory-backed runtime directory only when what they show changes.
    pub mod view {
        use std::path::{Path, PathBuf};
        use std::sync::{Arc, Mutex, MutexGuard};

        use cockpit_ops::lcgather::{self, Held};
        use spira_store::lc_call::{LcClient, LcError};

        pub const FAST: &str = "view";
        pub const SLOW: &str = "view_slow";
        pub const NO_RUNTIME_DIR: &str = "XDG_RUNTIME_DIR is unset: the cockpit document has no runtime directory to be written to";

        /// Whether `name` (a registry name) is one of the view's groups, which [`probe`] answers
        /// and the probe library does not dispatch.
        pub fn is_view_probe(name: &str) -> bool {
            name == FAST || name == SLOW
        }

        struct Context {
            supervised: bool,
            run_dir: PathBuf,
            lc: Option<LcClient>,
            units: Option<Arc<dyn runtime::units::Units>>,
            pool: Option<Arc<round_vm::pool::Pool>>,
        }

        static CONTEXT: Mutex<Option<Context>> = Mutex::new(None);
        static FAST_HELD: Mutex<Option<Held>> = Mutex::new(None);
        static SLOW_HELD: Mutex<Option<Held>> = Mutex::new(None);

        fn held(m: &'static Mutex<Option<Held>>) -> MutexGuard<'static, Option<Held>> {
            m.lock().unwrap_or_else(|p| p.into_inner())
        }

        /// What the next probe reads through. A different run directory starts the groups afresh.
        pub fn set_context(e: &crate::Effects, supervised: bool) {
            let mut ctx = CONTEXT.lock().unwrap_or_else(|p| p.into_inner());
            if ctx.as_ref().is_some_and(|c| c.run_dir != e.world.run_dir) {
                *held(&FAST_HELD) = None;
                *held(&SLOW_HELD) = None;
            }
            *ctx = Some(Context {
                supervised,
                run_dir: e.world.run_dir.clone(),
                lc: (!e.world.lc_socket.as_os_str().is_empty()).then(|| LcClient::socket(e.world.lc_socket.clone())),
                units: e.units.clone(),
                pool: e.vm_pool.clone(),
            });
        }

        fn now() -> i64 {
            spira_config::vtime::now_epoch() as i64
        }

        /// The probe function of the two view groups; any other name is the probe library's.
        pub fn probe(name: &str) -> Option<cockpit_collect::probes::Kv> {
            let (slot, fast) = match name {
                FAST => (&FAST_HELD, true),
                SLOW => (&SLOW_HELD, false),
                _ => return cockpit_collect::probes::run(name, &cockpit_collect::probes::Cfg::load().ok()?),
            };
            let (run_dir, lc, units, pool) = {
                let ctx = CONTEXT.lock().unwrap_or_else(|p| p.into_inner());
                let c = ctx.as_ref()?;
                if !c.supervised {
                    return Some(Vec::new());
                }
                (c.run_dir.clone(), c.lc.clone(), c.units.clone(), c.pool.clone())
            };
            let read = |args: &[&str]| -> Result<String, LcError> {
                match store_view::held_read(args) {
                    Some(held) => held,
                    None => cockpit_ops::lcio::answer(lc.as_ref().ok_or_else(|| LcError::Fault("no store socket is configured".into()))?, args),
                }
            };
            let status = || pool.as_ref().ok_or_else(|| "round-vm status: no pool configured".to_string())?.status_json();
            let env = lcgather::Env { run_dir: &run_dir, lc: &read, units: units.as_deref(), vm_status: Some(&status) };
            let mut next = held(slot).clone().unwrap_or_default();
            if fast {
                lcgather::gather_fast(&env, now(), &mut next);
            } else {
                lcgather::gather_slow(&env, now(), &mut next);
            }
            *held(slot) = Some(next);
            Some(Vec::new())
        }

        /// Writes the document when it shows something other than what the file does, else touches
        /// it so its age is the age of the last pass. Nothing is written until both groups have read.
        pub fn publish(dir: &Path) -> Result<(), String> {
            if dir.as_os_str().is_empty() {
                return Err(NO_RUNTIME_DIR.into());
            }
            let (fast, slow) = (held(&FAST_HELD), held(&SLOW_HELD));
            let (Some(fast), Some(slow)) = (fast.as_ref(), slow.as_ref()) else { return Ok(()) };
            let Some(doc) = Held::document(fast, slow) else { return Ok(()) };
            let path = cockpit_ops::lcview::document_path(dir);
            let old = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<cockpit_ops::lcview::Snapshot>(&b).ok());
            if old.as_ref().is_some_and(|o| lcgather::same_but_clock(o, &doc)) {
                ctx_meter::status::touch(&path);
            } else {
                ctx_meter::status::put(&path, &serde_json::to_string(&doc).map_err(|e| e.to_string())?);
            }
            Ok(())
        }
    }

    /// The slots sample runner: one `slots` tsd row under the run dir.
    pub fn run_slots_sample(e: &crate::Effects, _: &str, s: &EffectSpec) -> Result<String, String> {
        if e.world.run_dir.as_os_str().is_empty() {
            return Err("no run dir configured".into());
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let claim = cockpit_ops::lcview::read_fresh_claim(&e.world.cockpit_view_dir, now);
        cockpit_collect::tiers::slots_sample(&e.world.run_dir, &with_ready(&s.input, claim.as_ref()))
    }

    /// `input` plus the `ready` field as the claim view counts it; no claim view leaves it unwritten, which the row reads as `?`.
    fn with_ready(input: &str, claim: Option<&cockpit_ops::lcview::Snapshot>) -> String {
        let ready = claim.and_then(|c| cockpit_ops::lcview::claim_keys(c).into_iter().find(|(k, _)| k == "SP_READY")).map(|(_, v)| v);
        match ready {
            Some(n) => format!("{input}ready={n}\n"),
            None => input.to_string(),
        }
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

        #[test_id(t0arhoy)]
        #[test]
        fn only_an_indexed_row_loses_its_leading_age_and_every_other_key_is_whole() {
            assert_eq!(stable_value("SP_INFLOW0", "4m   task a"), "task a");
            assert_eq!(stable_value("SP_EVENT12", "9m x y"), "x y");
            assert_eq!(stable_value("SP_EVENT1", "10m  x y"), stable_value("SP_EVENT1", "9m   x y"));
            assert_eq!(stable_value("SP_MAIL3", "20218|READ|hi"), "READ|hi");
            for k in ["SP_MAIL_N", "SP_INFLOW", "SP_MAIL", "SP_EVENTX", "SP_INFLOW0_REPO", "SP_MAIL0_X"] {
                assert_eq!(stable_value(k, "4m   a|b"), "4m   a|b", "{k}");
            }
        }

        #[test_id(teop35r)]
        #[test]
        fn the_slots_sample_counts_ready_from_the_claim_view_and_leaves_it_unknown_without_one() {
            let claim = cockpit_ops::lcview::Snapshot { claimable: Some(vec!["sp-a".into(), "sp-b".into()]), rows: ["sp-a", "sp-b"].iter().map(|id| cockpit_ops::lcview::Row { id: id.to_string(), state: "READY".into(), claim: Some(cockpit_ops::lcview::ClaimView { verdict: cockpit_ops::lcview::Kind::Claimable, blocker: None, runes: vec!["builder".into()] }), ..Default::default() }).collect(), ..Default::default() };
            assert_eq!(with_ready("live=1\n", Some(&claim)), "live=1\nready=2\n");
            assert_eq!(with_ready("live=1\n", None), "live=1\n");
        }

        #[test_id(tnx3py5)]
        #[test]
        fn the_digest_ignores_clock_only_keys_and_sees_every_other() {
            let kv = |extra: &[(&str, &str)]| -> Vec<(String, String)> { extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
            let base = kv(&[("SP_DWARVES", "1"), ("SP_SENTINEL_AGE", "3"), ("SP_DWARF0_MIN", "4"), ("SP_DWARF12_LEASE", "9"), ("SP_MAIL_OLDEST_AGE", "5"), ("SP_DWARF3_QUIET", "2")]);
            let aged = kv(&[("SP_DWARVES", "1"), ("SP_SENTINEL_AGE", "8"), ("SP_DWARF0_MIN", "9"), ("SP_DWARF12_LEASE", "2"), ("SP_MAIL_OLDEST_AGE", "50"), ("SP_DWARF3_QUIET", "40")]);
            assert_eq!(digest(&base), digest(&aged));
            assert_ne!(digest(&base), digest(&kv(&[("SP_DWARVES", "2"), ("SP_SENTINEL_AGE", "3")])));
            assert!(!volatile("SP_DWARF_N") && !volatile("SP_DWARFX_MIN") && !volatile("SP_DWARF0_BEAD"));
        }

        // covers: UC-cockpit-observability-01
        #[test_id(twee3q0)]
        #[test]
        fn an_instance_that_is_not_the_supervised_writer_refuses_to_publish() {
            let mut unsupervised = snap(10, "r1");
            unsupervised.supervised = false;
            let refusal = decide_tier(&Carry::default(), &unsupervised, "t.publish").unwrap_err();
            assert!(refusal.contains(WRITE_KEY), "{refusal}");
            assert!(decide_tier(&Carry::default(), &snap(10, "r1"), "t.publish").is_ok());
        }

        #[test_id(ttng1sv)]
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

        #[test_id(tdmjsl2)]
        #[test]
        fn a_status_moves_only_along_the_table_and_its_at_is_the_move() {
            let c = Carry { fragments: [("p".to_string(), FragmentCarry { at: 5, status: FragmentState::Error, killed: true })].into(), digest: None };
            let s = Snapshot { supervised: true, fragments: [("p".to_string(), frag(FragmentState::Timeout, 9, &[]))].into() };
            assert_eq!(carry_fragments(&c, &s)["p"].status, FragmentState::Error, "error to timeout is not a move the table has");
            let s = Snapshot { supervised: true, fragments: [("p".to_string(), frag(FragmentState::Ok, 9, &[]))].into() };
            assert_eq!(carry_fragments(&c, &s)["p"], FragmentCarry { at: 9, status: FragmentState::Ok, killed: false });
            assert_eq!(merged(&Snapshot { supervised: true, fragments: [("cockpit.x".to_string(), frag(FragmentState::Error, 0, &[("SP_X", "0")]))].into() }).last(), Some(&("SP_X".to_string(), "?".to_string())));
        }

        #[test_id(tm3q3f1)]
        #[test]
        fn an_idle_five_second_tier_starts_no_child_process() {
            use crate::tasks::g::cockpit_tier_5s;
            let dir = testkit::TempDir::new("idle-tier");
            let run = testkit::TempDir::new("idle-tier-run");
            let _env = testkit::env(&spira_config::process::fixture_world_env(dir.path(), run.path(), &["spira"]).iter().map(|(k, v)| (*k, v.as_deref())).collect::<Vec<_>>());
            let fx = std::sync::Arc::new(crate::Effects { world: crate::world::World { run_dir: run.path().to_path_buf(), ..Default::default() }, ..Default::default() });
            let task = crate::tasks::all().find(|t| t.name == cockpit_tier_5s::NAME).unwrap();
            let runner = |name: &'static str| task.runners.iter().find(|(n, _)| *n == name).unwrap().1;
            let double = |name: &'static str| {
                let (fx, run) = (fx.clone(), runner(name));
                move |input: &str| run(&fx, "k", &EffectSpec::keyed(name, name, input, 1))
            };
            let mut rig = runtime::taskrig::TaskRig::new(1, (task.build)(&crate::fixture::fixture())).unwrap().double(cockpit_tier_5s::OBSERVE, double(cockpit_tier_5s::OBSERVE)).double(cockpit_tier_5s::PUBLISH, double(cockpit_tier_5s::PUBLISH));
            let (_, stream, kind) = (task.boot)();
            let before = spira_config::bounded::started();
            rig.tick(stream, kind).unwrap();
            let count = |rig: &runtime::taskrig::TaskRig, name: &str| rig.requested().iter().filter(|r| r.name == name).count();
            assert!(count(&rig, cockpit_tier_5s::PUBLISH) > 0, "the first pass publishes");
            rig.advance(60_000).unwrap();
            assert_eq!(count(&rig, cockpit_tier_5s::OBSERVE), 13, "twelve idle ticks follow the first pass");
            assert_eq!(spira_config::bounded::started(), before, "an idle tick starts no child");
        }

        #[test_id(txfnl3s)]
        #[test]
        fn the_observe_input_names_every_probe_and_the_writer_key() {
            let cfg: BTreeMap<String, String> = [(WRITE_KEY.to_string(), "1".to_string())].into();
            let input: ObserveInput = serde_json::from_str(&probe_input(&[("cockpit.now", 30_000)], &cfg)).unwrap();
            assert_eq!((input.probes[0].name.as_str(), input.probes[0].timeout_ms, input.config[WRITE_KEY].as_str()), ("cockpit.now", 30_000, "1"));
            assert_eq!(tier_of("cockpit-tier-60s.slots_sample"), "cockpit-tier-60s");
        }
    }
}
