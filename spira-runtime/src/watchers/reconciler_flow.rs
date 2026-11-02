//! The reconciler's flow pass: backlog trend, stage velocity and its floor, stage dwell and its
//! regression, round health, idle capacity, sentinel overrun and rework rate, each held to the
//! same hysteresis as the structural invariants. Each tick asks the world what the series read as;
//! `judge` does no IO. A confirmed gap leaves as one mail to the Concierge, and every verdict is
//! recorded as a `reconciler-status` row.

use std::collections::BTreeMap;

use reconciler_engine::alert::{compose_alert, should_alert};
use reconciler_engine::core::{step, HysteresisState, RawStatus, Verdict};
use reconciler_engine::io::{status_record, AlertedSinceMap, StateMap};
use reconciler_flow::core::{
    backlog_trend_raw, dwell_raw, dwell_regression_raw, idle_capacity_raw, round_health_raw, rework_raw, sentinel_overrun_raw, velocity_raw, BacklogObserved, DwellObserved, DwellRegressionObserved,
    ReworkObserved, RoundHealthObserved, SlotsSample, VelocityObserved, DWELL_REGRESSION_STATES,
};
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::answers_observe;

pub const STREAM: &str = "watch.reconciler-flow";
pub const TICK: EventKindTag = "reconciler-flow.tick";
pub const STATE: EventKindTag = "reconciler-flow.state";
pub const OBSERVE: &str = "reconciler-flow.observe";
pub const MAIL: &str = "reconciler-flow.mail";
pub const STATUS: &str = "reconciler-flow.status";
pub const NAME: &str = "reconciler-flow";

const OBSERVE_DEADLINE_MS: u64 = 120_000;
const ACT_DEADLINE_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq)]
pub struct ReconcilerFlow {
    pub interval_ms: u64,
    pub window_hours: f64,
    pub baseline_hours: f64,
    pub grace_secs: u64,
    pub unobservable_grace_secs: u64,
    pub rework_window_hours: f64,
    pub sentinel_timer_secs: u64,
}

/// What the world is asked to read: the instant and the windows the series are cut by.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub now: u64,
    pub window_hours: f64,
    pub baseline_hours: f64,
    pub rework_window_hours: f64,
    pub sentinel_timer_secs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slots {
    pub live: u64,
    pub ceiling: u64,
    pub ready: u64,
    pub capacity_paused: bool,
}

impl From<SlotsSample> for Slots {
    fn from(s: SlotsSample) -> Slots {
        Slots { live: s.live, ceiling: s.ceiling, ready: s.ready, capacity_paused: s.capacity_paused }
    }
}

impl From<Slots> for SlotsSample {
    fn from(s: Slots) -> SlotsSample {
        SlotsSample { live: s.live, ceiling: s.ceiling, ready: s.ready, capacity_paused: s.capacity_paused }
    }
}

/// What the `bead-stage` family read as, each metric already cut to its window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub waiting: u64,
    pub velocity: (f64, f64),
    pub dwell: (f64, u64, f64, u64),
    pub round_health: (u64, u64, u64, u64),
    pub rework: (u64, u64, f64),
    pub regression: Vec<(String, f64, u64, f64, u64)>,
}

/// Every reading is its own `Result`: one that could not be taken is that invariant's
/// `Unobservable`, never a pass. `now` is the instant the series were cut at, which the verdicts
/// are judged at too, as the binary judged one pass at one instant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowSnapshot {
    pub now: u64,
    pub halted: bool,
    pub export: Result<(), String>,
    pub velocity_floor: Option<f64>,
    pub dwell_limit: Option<u64>,
    pub backlog: Result<u64, String>,
    pub baseline: Result<f64, String>,
    pub stage: Result<Stage, String>,
    pub slots: Result<(Slots, Slots), String>,
    pub sentinel: Result<u64, String>,
}

impl FlowSnapshot {
    pub fn halted(now: u64) -> FlowSnapshot {
        let halted = || "world is halted".to_string();
        FlowSnapshot { now, halted: true, export: Err(halted()), velocity_floor: None, dwell_limit: None, backlog: Err(halted()), baseline: Err(halted()), stage: Err(halted()), slots: Err(halted()), sentinel: Err(halted()) }
    }
}

/// Carried between ticks: each invariant's streak, the streak last alerted for, and the word each
/// last read as, so a pass whose verdicts did not change leaves the journal alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub state: StateMap,
    pub alerted: AlertedSinceMap,
    pub words: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    pub subject: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Statuses {
    pub ts: String,
    pub rows: Vec<serde_json::Value>,
}

pub struct Pass {
    pub carry: Carry,
    pub statuses: Vec<serde_json::Value>,
    pub mails: Vec<Mail>,
}

fn status_word(v: &Verdict) -> &'static str {
    match &v.status {
        RawStatus::Satisfied => "satisfied",
        RawStatus::Gap { .. } => "gap",
        RawStatus::Unobservable { .. } => "unobservable",
        RawStatus::Deliberate { .. } => "deliberate",
    }
}

fn unobservable(reason: String) -> RawStatus {
    RawStatus::Unobservable { reason }
}

/// An invariant read from `bead-stage` is only as fresh as the last export.
fn fresh(export: &Result<(), String>, raw: RawStatus) -> RawStatus {
    match export {
        Ok(()) => raw,
        Err(e) => unobservable(format!("bead-stage export failed: {e}")),
    }
}

struct Judging<'a> {
    cfg: &'a ReconcilerFlow,
    now: u64,
    pass: Pass,
}

impl Judging<'_> {
    fn evaluate(&mut self, key: &str, raw: RawStatus, alert: bool) {
        let prev = self.pass.carry.state.remove(key).unwrap_or_default();
        let grace = match raw {
            RawStatus::Unobservable { .. } => self.cfg.unobservable_grace_secs,
            _ => self.cfg.grace_secs,
        };
        let (verdict, next) = step(self.now, raw, grace, prev);
        if next != HysteresisState::default() {
            self.pass.carry.state.insert(key.to_string(), next);
        }
        self.pass.statuses.push(serde_json::to_value(status_record(key, &verdict)).unwrap_or_default());
        if alert {
            self.alert(key, &verdict);
        }
        self.pass.carry.words.insert(key.to_string(), status_word(&verdict).to_string());
    }

    fn alert(&mut self, key: &str, verdict: &Verdict) {
        let (fire, next) = should_alert(verdict, self.pass.carry.alerted.get(key).copied());
        match next {
            Some(since) => {
                self.pass.carry.alerted.insert(key.to_string(), since);
            }
            None => {
                self.pass.carry.alerted.remove(key);
            }
        }
        if fire {
            let evidence = compose_alert(key, self.now, verdict, None);
            self.pass.mails.push(Mail { subject: format!("RECONCILER: flow gap — {key}"), body: format!("{evidence}\nThis has no deterministic remedy — it needs judgement, not a retry.\n") });
        }
    }
}

/// The verdicts, mails and carried state one snapshot calls for.
pub fn judge(cfg: &ReconcilerFlow, now: u64, carry: &Carry, snap: &FlowSnapshot) -> Pass {
    let mut j = Judging { cfg, now, pass: Pass { carry: Carry { words: BTreeMap::new(), ..carry.clone() }, statuses: vec![], mails: vec![] } };

    let backlog_raw = match (&snap.backlog, &snap.baseline) {
        (Ok(cur), Ok(base)) => backlog_trend_raw(&BacklogObserved { current: *cur, baseline: *base }),
        (Err(e), _) | (_, Err(e)) => unobservable(e.clone()),
    };
    j.evaluate("flow:backlog", backlog_raw, true);

    let velocity_raw_status = match &snap.stage {
        Ok(s) => velocity_raw(&VelocityObserved { current_per_hour: s.velocity.0, baseline_per_hour: s.velocity.1, waiting: s.waiting }, snap.velocity_floor),
        Err(e) => unobservable(e.clone()),
    };
    j.evaluate("flow:velocity:queue", fresh(&snap.export, velocity_raw_status), true);

    let dwell_raw_status = match &snap.stage {
        Ok(Stage { dwell: (cur_p95, cur_n, base_p95, base_n), .. }) => dwell_raw(&DwellObserved { p95_seconds: *cur_p95, n: *cur_n }, snap.dwell_limit, if *base_n > 0 { Some(*base_p95) } else { None }),
        Err(e) => unobservable(e.clone()),
    };
    j.evaluate("flow:dwell:review", fresh(&snap.export, dwell_raw_status), true);

    let round_health_raw_status = match &snap.stage {
        Ok(Stage { round_health: (cur_flips, cur_transitions, base_flips, base_transitions), .. }) => {
            let baseline_flip_rate = if *base_transitions > 0 { *base_flips as f64 / *base_transitions as f64 } else { 0.0 };
            round_health_raw(&RoundHealthObserved { flips: *cur_flips, transitions: *cur_transitions, baseline_flip_rate })
        }
        Err(e) => unobservable(e.clone()),
    };
    j.evaluate("flow:round-health", fresh(&snap.export, round_health_raw_status), true);

    let idle_raw_status = match &snap.slots {
        Ok((prev, cur)) => idle_capacity_raw(&(*prev).into(), &(*cur).into()),
        Err(e) => unobservable(e.clone()),
    };
    j.evaluate("flow:idle-capacity", idle_raw_status, true);

    let sentinel_raw_status = match &snap.sentinel {
        Ok(secs) => sentinel_overrun_raw(*secs, cfg.sentinel_timer_secs),
        Err(e) => unobservable(e.clone()),
    };
    j.evaluate("flow:sentinel-overrun", sentinel_raw_status, true);

    match &snap.stage {
        Ok(Stage { rework: (reopens, landed, history_hours), .. }) => {
            let raw = fresh(&snap.export, rework_raw(&ReworkObserved { reopens: *reopens, landed: *landed }));
            j.evaluate("flow:rework", raw, *history_hours >= cfg.baseline_hours);
        }
        Err(e) => j.evaluate("flow:rework", unobservable(e.clone()), true),
    }

    for state_name in DWELL_REGRESSION_STATES {
        let key = format!("flow:dwell-regression:{}", state_name.to_ascii_lowercase());
        let raw = match &snap.stage {
            Ok(s) => {
                let observed = match s.regression.iter().find(|(name, ..)| name == state_name) {
                    Some((_, p90, n, base_p90, base_n)) => DwellRegressionObserved { p90_seconds: *p90, n: *n, baseline_p90_seconds: *base_p90, baseline_n: *base_n },
                    None => DwellRegressionObserved { p90_seconds: 0.0, n: 0, baseline_p90_seconds: 0.0, baseline_n: 0 },
                };
                fresh(&snap.export, dwell_regression_raw(&observed))
            }
            Err(e) => unobservable(e.clone()),
        };
        j.evaluate(&key, raw, true);
    }
    j.pass
}

impl ReconcilerFlow {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("reconciler-flow/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn params(&self, now: u64) -> Params {
        Params { now, window_hours: self.window_hours, baseline_hours: self.baseline_hours, rework_window_hours: self.rework_window_hours, sentinel_timer_secs: self.sentinel_timer_secs }
    }
}

impl Handler for ReconcilerFlow {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let now = ev.at / 1000;
            return vec![
                Command::Probe(EffectSpec { name: OBSERVE.into(), input: serde_json::to_string(&self.params(now)).unwrap_or_default(), deadline_ms: OBSERVE_DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<FlowSnapshot>(&ev.kind.body) else {
            return vec![];
        };
        if snap.halted {
            return vec![];
        }
        let now = snap.now;
        let carry: Carry = super::carried(view, &ev.stream, STATE);
        let pass = judge(self, now, &carry, &snap);
        ctx.event().set("reconciler_flow.mails", pass.mails.len() as u64);
        let mut cmds = vec![];
        if pass.carry != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&pass.carry).unwrap_or_default())));
        }
        for m in &pass.mails {
            cmds.push(Command::Effect(EffectSpec { name: MAIL.into(), input: serde_json::to_string(m).unwrap_or_default(), deadline_ms: ACT_DEADLINE_MS }));
        }
        let statuses = Statuses { ts: super::iso8601(now), rows: pass.statuses };
        cmds.push(Command::Probe(EffectSpec { name: STATUS.into(), input: serde_json::to_string(&statuses).unwrap_or_default(), deadline_ms: ACT_DEADLINE_MS }));
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ReconcilerFlow {
        ReconcilerFlow { interval_ms: 1_800_000, window_hours: 0.5, baseline_hours: 24.0, grace_secs: 1800, unobservable_grace_secs: 3600, rework_window_hours: 6.0, sentinel_timer_secs: 900 }
    }

    fn quiet() -> FlowSnapshot {
        FlowSnapshot {
            now: 1_000,
            halted: false,
            export: Ok(()),
            velocity_floor: None,
            dwell_limit: None,
            backlog: Ok(10),
            baseline: Ok(10.0),
            stage: Ok(Stage { waiting: 0, velocity: (1.0, 1.0), dwell: (10.0, 5, 10.0, 50), round_health: (0, 4, 0, 40), rework: (0, 5, 30.0), regression: vec![] }),
            slots: Ok((Slots { live: 3, ceiling: 3, ready: 0, capacity_paused: false }, Slots { live: 3, ceiling: 3, ready: 0, capacity_paused: false })),
            sentinel: Ok(10),
        }
    }

    #[test]
    fn a_quiet_world_alerts_nobody_and_reads_the_same_twice() {
        let first = judge(&cfg(), 1_000, &Carry::default(), &quiet());
        assert!(first.mails.is_empty());
        assert_eq!(first.statuses.len(), 12);
        let second = judge(&cfg(), 2_800, &first.carry, &quiet());
        assert_eq!(second.carry, first.carry, "a pass whose verdicts did not change carries the same state");
    }

    #[test]
    fn a_gap_alerts_once_after_its_grace_and_not_again_while_it_lasts() {
        let mut snap = quiet();
        snap.backlog = Ok(500);
        let seen = judge(&cfg(), 1_000, &Carry::default(), &snap);
        assert!(seen.mails.is_empty(), "inside grace");
        let confirmed = judge(&cfg(), 2_900, &seen.carry, &snap);
        assert_eq!(confirmed.mails.len(), 1);
        assert_eq!(confirmed.mails[0].subject, "RECONCILER: flow gap — flow:backlog");
        let after = judge(&cfg(), 4_700, &confirmed.carry, &snap);
        assert!(after.mails.is_empty(), "one alert per gap streak");
    }

    #[test]
    fn a_reading_that_could_not_be_taken_is_unobservable_not_satisfied() {
        let mut snap = quiet();
        snap.stage = Err("metrics-store-unreachable".into());
        let p = judge(&cfg(), 1_000, &Carry::default(), &snap);
        assert_eq!(p.carry.words["flow:velocity:queue"], "unobservable");
        assert_eq!(p.carry.words["flow:dwell-regression:ready"], "unobservable");
    }

    #[test]
    fn a_failed_export_blinds_every_stage_invariant() {
        let mut snap = quiet();
        snap.export = Err("exit 1".into());
        let p = judge(&cfg(), 1_000, &Carry::default(), &snap);
        for key in ["flow:velocity:queue", "flow:dwell:review", "flow:round-health", "flow:rework", "flow:dwell-regression:working"] {
            assert_eq!(p.carry.words[key], "unobservable", "{key}");
        }
        assert_eq!(p.carry.words["flow:backlog"], "satisfied");
    }

    #[test]
    fn rework_is_report_only_until_the_history_covers_the_baseline() {
        let mut snap = quiet();
        snap.stage = Ok(Stage { rework: (50, 5, 2.0), ..match quiet().stage { Ok(s) => s, Err(_) => unreachable!() } });
        let cfg = ReconcilerFlow { grace_secs: 0, ..cfg() };
        let p = judge(&cfg, 1_000, &Carry::default(), &snap);
        assert_eq!(p.carry.words["flow:rework"], "gap");
        assert!(p.mails.is_empty(), "report-only while 2h of 24h exist");
    }
}
