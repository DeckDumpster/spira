//! `trigger.groom`: files the groomer's scheduled hygiene bead. Replaces `spira-groom-prod`
//! (ExecStartPre `world guard`; ExecStart `spira/groom-trigger.sh`). Pattern P1, stateless.
//! Contract: `wiki/projects/spira/designs/tasks/trigger-groom.md`.
//!
//! # Tick
//!
//! Every 6 h (`SPIRA_RUNTIME_TRIGGER_GROOM_SECS`). Single-flight replaces the `flock` on
//! `groom-trigger.lock`. While the world is halted the snapshot carries `halted` and nothing acts.
//!
//! # Observe
//!
//! `trigger.groom.observe` returns a [`Snapshot`]: `open_triggers` (beads labelled
//! `<scope>,<groom label>` whose lifecycle row is neither past the builder nor poisoned: the
//! `content` query plus the `LcClient` list), `lane_admitted` (`lanes::lane_admitted`),
//! `lastpass` (the persona's own `groom.lastpass` file), `live_owing` (READY, WORKING and REWORK
//! rows, from the S7b projection) and `landings_since_lastpass` (distinct bead ids in `git log`
//! on the home repo, parsed by `mimir_trigger::engine::parse_landing_ids`; git stays a child).
//!
//! # Decide
//!
//! `decide(snap, threshold) -> Decision`, pure and stateless, in the script's order: an
//! unreadable count files nothing and says so; an open trigger skips; an unadmitted lane skips;
//! `live_owing + landings < SPIRA_GROOM_THRESHOLD` skips (an unreadable `live_owing` counts as 0);
//! otherwise file. Fixtures port `spira/test-groom-trigger.sh` and
//! `spira/test-groomer-litter-predicate.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `trigger.groom.observe` (probe) | store | 60 s | tick id | — |
//! | `trigger.groom.file` | store | 60 s | `groom-trigger/<6h period index>` | an open-trigger re-count above 0 ([`may_file`]) |
//!
//! The bead: title `Groomer pass — scheduled graph hygiene`, for `groomer`, repo the home repo,
//! priority 3, labels `delivers:note:<SPIRA_RUN>/groom.log` and `groom-trigger`, the body
//! `groom-trigger.sh` writes; bead and lifecycle row in one call. No inbox line: the skip reason
//! and the score are wide-event fields (`groom.skip`, `groom.score`).
//!
//! # Silence
//!
//! An idle tick (a trigger already open, or the score below threshold) emits only the probe. No
//! carried state, so never an Emit.
//!
//! # Replaces
//!
//! `spira-groom-prod.service` and `.timer` and their `world guard`; children `mimir-trigger` ×2,
//! `spira-lc list`, `python3`, `git log`, `bead.sh file`, `bdq update`, `flock`, `mktemp`, the
//! `lib.sh` bootstrap. Pinned config: [`KEYS`].
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; Observe mode shadows `trigger.groom.file`. No
//! state to migrate back.

use std::collections::BTreeMap;

use mimir_trigger::observe as obs;
use runtime::event::*;
use serde::{Deserialize, Serialize};
use spira_config::lc_call::LcClient;

use super::{Bead, Cycle, Decision, Transition};

pub const NAME: &str = "trigger.groom";
pub const STREAM: &str = "task.trigger.groom";
pub const TICK: EventKindTag = "trigger.groom.tick";

pub const OBSERVE: &str = "trigger.groom.observe";
pub const FILE: &str = "trigger.groom.file";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_TRIGGER_GROOM_SECS";
pub const THRESHOLD_KEY: &str = "SPIRA_GROOM_THRESHOLD";
/// The lane check expands every repo's lanes column, so it reads all six lane labels.
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_SCOPE_LABEL", "SPIRA_GROOMER_LABEL", THRESHOLD_KEY, "SPIRA_RUN", "SPIRA_DB", "SPIRA_HOME_REPO", "SPIRA_REPO_MAP", "SPIRA_PLAN_LABEL", "SPIRA_INCIDENT_LABEL", "SPIRA_MIMIR_LABEL", "SPIRA_SPIKE_LABEL", "SPIRA_CZAR_LABEL"];

pub const TITLE: &str = "Groomer pass — scheduled graph hygiene";
pub const BODY: &str = "Scheduled trigger: the groomer persona will claim this bead, run a hygiene pass over the open bead graph (splitting unsplittable beads, merging duplicates, closing stale premises, correcting mislabelled lanes), and close this bead when finished. See spira/runes/groomer.md for the pass procedure.";

const DEADLINE_MS: u64 = 60_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub open_triggers: Result<u64, String>,
    pub lane_admitted: bool,
    pub lastpass: i64,
    pub live_owing: Result<u64, String>,
    pub landings_since_lastpass: u64,
    pub home_repo: String,
}

/// One tick of a stateless trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Step {
    Observe,
    Skip,
    Filed,
}

pub const TRANSITIONS: &[Transition<Step>] = &[
    Transition { from: Step::Observe, on: "unreadable", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "open", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "lane", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "score", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "file", to: Step::Filed, effects: &[FILE] },
];

/// The edge of `table` from `from` on `on`, or a refusal naming the state. Shared by the
/// family's stateless triggers.
pub fn walk<S: Copy + PartialEq + std::fmt::Debug>(table: &[Transition<S>], from: S, on: &str) -> Result<S, String> {
    table.iter().find(|t| t.from == from && t.on == on).map(|t| t.to).ok_or_else(|| format!("no step on {on} from {from:?}"))
}

/// The tick's next step on `on`, or a refusal naming the step it is in.
pub fn step(from: Step, on: &str) -> Result<Step, String> {
    walk(TRANSITIONS, from, on)
}

/// A trigger's file guard over a fresh re-count of its open beads: only a readable 0 files.
pub fn guard(recount: &Result<u64, String>) -> Result<(), String> {
    match recount {
        Ok(0) => Ok(()),
        Ok(n) => Err(format!("{n} trigger bead(s) already open — not filing")),
        Err(e) => Err(format!("cannot count open triggers ({e}) — not filing")),
    }
}

/// The file effect's own guard: the open-trigger count it re-reads before writing.
pub fn may_file(recount: &Result<u64, String>) -> Result<(), String> {
    guard(recount)
}

/// `<scope>,<label>`, or `<label>` with no scope: the labels a trigger is deduplicated on.
pub fn dedup_labels(config: &BTreeMap<String, String>, label_key: &str) -> String {
    let get = |k: &str| config.get(k).cloned().unwrap_or_default();
    mimir_trigger::engine::trigger_labels(&get("SPIRA_SCOPE_LABEL"), &get(label_key))
}

/// The pure decision over one snapshot, with `config` the pinned [`KEYS`].
pub fn decide(snap: &Snapshot, config: &BTreeMap<String, String>) -> Decision {
    if snap.halted {
        return Decision::Skip("halted".into());
    }
    match snap.open_triggers {
        Err(_) => return Decision::Skip("unreadable".into()),
        Ok(n) if n > 0 => return Decision::Skip("open".into()),
        Ok(_) => {}
    }
    if !snap.lane_admitted {
        return Decision::Skip("lane".into());
    }
    let Some(threshold) = config.get(THRESHOLD_KEY).and_then(|t| t.trim().parse::<u64>().ok()) else { return Decision::Skip("unreadable".into()) };
    let score = snap.live_owing.as_ref().copied().unwrap_or(0) + snap.landings_since_lastpass;
    if score < threshold {
        return Decision::Skip("score".into());
    }
    let run = config.get("SPIRA_RUN").cloned().unwrap_or_default();
    let mut labels: Vec<String> = dedup_labels(config, "SPIRA_GROOMER_LABEL").split(',').filter(|l| !l.is_empty()).map(str::to_string).collect();
    labels.extend([format!("delivers:note:{run}/groom.log"), "groom-trigger".into()]);
    Decision::File(Bead { title: TITLE.into(), kind: "task".into(), priority: 3, labels, body: BODY.into(), persona: Some("groomer".into()), repo: Some(snap.home_repo.clone()).filter(|r| !r.is_empty()) })
}

/// What a trigger's file effect is handed: the bead, and the labels its guard re-counts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filing {
    pub bead: Bead,
    pub dedup: String,
}

/// The file effect a trigger asks for when `decide` files.
pub fn file_effect(name: &str, key: String, bead: Bead, dedup: String, deadline_ms: u64) -> Command {
    Command::Effect(EffectSpec::keyed(name, key, serde_json::to_string(&Filing { bead, dedup }).unwrap_or_default(), deadline_ms))
}

/// The file runner every trigger shares: re-count the open triggers, refuse through `guard`
/// unless none is open, then write the bead and its lifecycle row. Answers the new id.
pub fn run_file_with(e: &crate::Effects, s: &EffectSpec, guard: fn(&Result<u64, String>) -> Result<(), String>) -> Result<String, String> {
    let f: Filing = serde_json::from_str(&s.input).map_err(|e| format!("file input: {e}"))?;
    let lc = LcClient::socket(&e.world.lc_socket);
    guard(&obs::open_trigger_count_on(&lc, &f.dedup))?;
    let b = &f.bead;
    groomer::trigger::file(&lc, &groomer::trigger::Filing { title: &b.title, kind: &b.kind, priority: b.priority, labels: &b.labels, body: &b.body, persona: b.persona.as_deref(), repo: b.repo.as_deref() })
}

/// Whether the world is halted: the one file `world guard` read.
pub fn halted(e: &crate::Effects) -> bool {
    e.world.run_dir.join("world.halted").exists()
}

/// The observe's input: the handler's pinned config.
pub fn config_input(config: &BTreeMap<String, String>) -> String {
    serde_json::to_string(config).unwrap_or_default()
}

/// The pinned config an observe was handed.
pub fn config_of(s: &EffectSpec) -> Result<BTreeMap<String, String>, String> {
    serde_json::from_str(&s.input).map_err(|e| format!("observe input: {e}"))
}

/// `trigger.groom.observe`: the snapshot `decide` reads.
pub fn run_observe(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let config = config_of(s)?;
    let lc = LcClient::socket(&e.world.lc_socket);
    let reg = obs::registry(&config)?;
    let labels = obs::lane_labels(&config)?;
    let lane = obs::pinned(&config, "SPIRA_GROOMER_LABEL")?;
    let run = std::path::PathBuf::from(obs::pinned(&config, "SPIRA_RUN")?);
    let lastpass = obs::read_epoch(&run.join("groom.lastpass"));
    let live_owing = lc
        .call(&["list", "--live"])
        .map_err(String::from)
        .and_then(|r| if r.code == 0 { spira_config::lc_state::parse_rows(&r.out) } else { Err(r.why()) })
        .map(|rows| rows.iter().filter(|r| matches!(r.state.as_str(), "READY" | "WORKING" | "REWORK")).count() as u64);
    let home_repo = reg.home_repo().to_string();
    let snap = Snapshot {
        halted: halted(e),
        open_triggers: obs::open_trigger_count_on(&lc, &dedup_labels(&config, "SPIRA_GROOMER_LABEL")),
        lane_admitted: obs::lane_admitted(&reg, lane, &labels),
        lastpass,
        live_owing,
        landings_since_lastpass: obs::landings_since(&reg, &home_repo, lastpass),
        home_repo,
    };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `trigger.groom.file`.
pub fn run_file(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    run_file_with(e, s, may_file)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerGroom {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl TriggerGroom {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(TriggerGroom { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("trigger.groom/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for TriggerGroom {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, || config_input(&self.config), |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            ctx.event().set("groom.score", (snap.live_owing.as_ref().copied().unwrap_or(0) + snap.landings_since_lastpass) as i64);
            match decide(&snap, &self.config) {
                Decision::Skip(why) => {
                    ctx.event().set("groom.skip", why.as_str());
                    vec![]
                }
                Decision::File(bead) => vec![file_effect(FILE, format!("groom-trigger/{}", ev.at / self.interval_ms.max(1)), bead, dedup_labels(&self.config, "SPIRA_GROOMER_LABEL"), DEADLINE_MS)],
            }
        })
    }
}
