//! `trigger.mimir`: files the Mimir persona's retrospective bead. Replaces `spira-mimir-prod`
//! (ExecStartPre `world guard`; ExecStart `spira/mimir-trigger.sh`, which execs `mimir-trigger`).
//! Pattern P1, stateless. Contract: `wiki/projects/spira/designs/tasks/trigger-mimir.md`.
//!
//! # Tick
//!
//! Every 15 min (`SPIRA_RUNTIME_TRIGGER_MIMIR_SECS`). Single-flight replaces the `flock` on
//! `mimir-trigger.lock`. While the world is halted nothing acts.
//!
//! # Observe
//!
//! `trigger.mimir.observe` returns a [`Snapshot`]: `open_triggers` (`Real::open_trigger_count`
//! as the `content` query plus the `LcClient` list), `lane_admitted`, `lastpass` and `watermark`
//! (the persona's own files), `landings_since_watermark` (`engine::parse_landing_ids` over
//! `git log` on the home repo and every `SPIRA_REPO_MAP` repo) and `invalid_closed_raw`
//! (`strand::detectors::detect_invalid_closed`, in-process).
//!
//! # Decide
//!
//! `mimir_trigger::engine` already holds the pure core (`time_trigger`, `landing_trigger`,
//! `invalid_closed_rows`, `trigger_labels`, `reason_text`, `description`). In order: a count error
//! or an open trigger skips; an unadmitted lane skips; fire when `now - lastpass >=
//! SPIRA_MIMIR_MAX_GAP_SECONDS`, or `landings >= SPIRA_MIMIR_LANDING_INTERVAL`, or there is any
//! invalid-closed row. No carried state. Fixtures: the 17 engine tests, the 5 `main.rs` tests,
//! `real.rs`'s open-trigger test and `spira/test-mimir-trigger.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `trigger.mimir.observe` (probe) | store | 120 s | tick id | — |
//! | `trigger.mimir.file` | store | 120 s | `mimir-sweep/<15m period index>` | an open-trigger re-count above 0 ([`may_file`]) |
//!
//! The bead: title `Mimir pass — <reason>` (`<n>s elapsed`, `<n> landings` and
//! `<n> invalid-closed row(s)` joined with `, `), type `task`, priority 3, labels
//! `<scope>,<mimir label>,delivers:note:<SPIRA_RUN>/mimir.log`, the body of
//! `engine::description`; bead and lifecycle row together. No inbox line.
//!
//! # Silence
//!
//! An idle tick (a trigger already open, or no trigger condition) emits only the probe.
//!
//! # Replaces
//!
//! `spira-mimir-prod.service` and `.timer`, the `world guard` and the shim; children `bdq list`,
//! `bdq create`, `git log` per repo, `date`, the `spira-lc` list. The `mimir-trigger repo-lanes |
//! lane-admitted | open-trigger-count` doors retire with `trigger.groom`. Pinned config: [`KEYS`].
//!
//! # Reverse
//!
//! Release rollback restores the unit, the timer and the shim; Observe mode shadows the file.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use mimir_trigger::engine;
use mimir_trigger::observe as obs;
use spira_config::lc_call::LcClient;

use super::trigger_groom::{config_input, config_of, dedup_labels, file_effect, guard, halted, run_file_with, walk};
use super::{Bead, Cycle, Decision, Transition};

pub const NAME: &str = "trigger.mimir";
pub const STREAM: &str = "task.trigger.mimir";
pub const TICK: EventKindTag = "trigger.mimir.tick";

pub const OBSERVE: &str = "trigger.mimir.observe";
pub const FILE: &str = "trigger.mimir.file";
pub const EFFECTS: &[&str] = &[OBSERVE, FILE];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_TRIGGER_MIMIR_SECS";
pub const KEYS: &[&str] = &[
    CADENCE_KEY,
    "SPIRA_RUN",
    "SPIRA_REPO_MAP",
    "SPIRA_PLAN_LABEL",
    "SPIRA_INCIDENT_LABEL",
    "SPIRA_GROOMER_LABEL",
    "SPIRA_MIMIR_LABEL",
    "SPIRA_SPIKE_LABEL",
    "SPIRA_CZAR_LABEL",
    "SPIRA_SCOPE_LABEL",
    "SPIRA_MIMIR_MAX_GAP_SECONDS",
    "SPIRA_MIMIR_LANDING_INTERVAL",
    "SPIRA_MIMIR_MAX_BEADS",
    "SPIRA_METRICS_ENDPOINT",
];

const DEADLINE_MS: u64 = 120_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub open_triggers: Result<u64, String>,
    pub lane_admitted: bool,
    pub now: i64,
    pub lastpass: i64,
    pub watermark: i64,
    pub landings_since_watermark: u64,
    pub invalid_closed_raw: String,
}

pub use super::trigger_groom::Step;

pub const TRANSITIONS: &[Transition<Step>] = &[
    Transition { from: Step::Observe, on: "unreadable", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "open", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "lane", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "no-trigger", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "recent", to: Step::Skip, effects: &[] },
    Transition { from: Step::Observe, on: "time", to: Step::Filed, effects: &[FILE] },
    Transition { from: Step::Observe, on: "landing", to: Step::Filed, effects: &[FILE] },
    Transition { from: Step::Observe, on: "invalid-closed", to: Step::Filed, effects: &[FILE] },
];

/// The tick's next step on `on`, or a refusal naming the step it is in.
pub fn step(from: Step, on: &str) -> Result<Step, String> {
    walk(TRANSITIONS, from, on)
}

/// The file effect's own guard: the open-trigger count it re-reads before writing.
pub fn may_file(recount: &Result<u64, String>) -> Result<(), String> {
    guard(recount)
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
    let num = |k: &str| config.get(k).and_then(|v| v.trim().parse::<i64>().ok());
    let (Some(max_gap), Some(interval), Some(max_beads)) = (num("SPIRA_MIMIR_MAX_GAP_SECONDS"), num("SPIRA_MIMIR_LANDING_INTERVAL"), num("SPIRA_MIMIR_MAX_BEADS")) else { return Decision::Skip("unreadable".into()) };
    let elapsed = snap.now - snap.lastpass;
    let rows = engine::invalid_closed_rows(&snap.invalid_closed_raw);
    let reason = engine::TriggerReason { time: engine::time_trigger(elapsed, max_gap), landing: engine::landing_trigger(snap.landings_since_watermark, interval.max(0) as u64), invalid_closed: !rows.is_empty() };
    if !reason.any() {
        return Decision::Skip("no-trigger".into());
    }
    let text = engine::reason_text(&reason, elapsed, snap.landings_since_watermark, rows.len());
    let joined = rows.join("\n");
    let body = engine::description(max_beads.max(0) as u64, &text, snap.lastpass, snap.watermark, reason.invalid_closed.then_some(joined.as_str()));
    let run = config.get("SPIRA_RUN").cloned().unwrap_or_default();
    let mut labels: Vec<String> = dedup_labels(config, "SPIRA_MIMIR_LABEL").split(',').filter(|l| !l.is_empty()).map(str::to_string).collect();
    labels.push(format!("delivers:note:{run}/mimir.log"));
    Decision::File(Bead { title: format!("Mimir pass — {text}"), kind: "task".into(), priority: 3, labels, body, persona: None, repo: None })
}

/// When this stream last filed a trigger: the newest `trigger.mimir.file` that succeeded.
pub fn last_filed_at(view: &Projection, stream: &StreamKey) -> Option<u64> {
    view.events(stream).iter().rev().find(|e| e.kind.tag == EFFECT_SUCCEEDED && crate::watchers::answered_effect(view, e).as_deref() == Some(FILE)).map(|e| e.at)
}

/// Whether a trigger filed at `filed_at` still covers `now` (both ms): at most one Mimir pass is
/// filed per `SPIRA_MIMIR_MAX_GAP_SECONDS`. The open-trigger count is the dedupe once the store
/// shows the bead; this covers the bead the store does not show yet, or one closed before its
/// pass ran, which the script would have filed again on the next quarter hour.
pub fn recently_filed(filed_at: Option<u64>, now: u64, config: &BTreeMap<String, String>) -> bool {
    let gap_ms = config.get("SPIRA_MIMIR_MAX_GAP_SECONDS").and_then(|g| g.trim().parse::<u64>().ok()).unwrap_or(0) * 1000;
    filed_at.is_some_and(|at| now.saturating_sub(at) < gap_ms)
}

/// `trigger.mimir.observe`: the snapshot `decide` reads.
pub fn run_observe(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let config = config_of(s)?;
    let lc = LcClient::socket(&e.world.lc_socket);
    let reg = obs::registry(&config)?;
    let labels = obs::lane_labels(&config)?;
    let run = std::path::PathBuf::from(obs::pinned(&config, "SPIRA_RUN")?);
    let watermark = obs::read_epoch(&run.join("mimir.watermark"));
    let map_text = std::fs::read_to_string(obs::pinned(&config, "SPIRA_REPO_MAP")?).unwrap_or_default();
    let snap = Snapshot {
        halted: halted(e),
        open_triggers: obs::open_trigger_count_on(&lc, &dedup_labels(&config, "SPIRA_MIMIR_LABEL")),
        lane_admitted: obs::lane_admitted(&reg, &labels.mimir, &labels),
        now: spira_config::vtime::now_epoch() as i64,
        lastpass: obs::read_epoch(&run.join("mimir.lastpass")),
        watermark,
        landings_since_watermark: obs::landings_everywhere(&reg, &map_text, watermark),
        invalid_closed_raw: obs::invalid_closed_raw()?,
    };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `trigger.mimir.file`.
pub fn run_file(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    run_file_with(e, s, may_file)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerMimir {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl TriggerMimir {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(TriggerMimir { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("trigger.mimir/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for TriggerMimir {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        CYCLE.handle(self.interval_ms, view, ev, || config_input(&self.config), |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            match decide(&snap, &self.config) {
                Decision::Skip(why) => {
                    ctx.event().set("mimir.skip", why.as_str());
                    vec![]
                }
                Decision::File(_) if recently_filed(last_filed_at(view, &ev.stream), ev.at, &self.config) => {
                    ctx.event().set("mimir.skip", "recent");
                    vec![]
                }
                Decision::File(bead) => vec![file_effect(FILE, format!("mimir-sweep/{}", ev.at / self.interval_ms.max(1)), bead, dedup_labels(&self.config, "SPIRA_MIMIR_LABEL"), DEADLINE_MS)],
            }
        })
    }
}
