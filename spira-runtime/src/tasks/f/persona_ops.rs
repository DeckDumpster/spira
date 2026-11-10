//! `persona.ops`: the Ops persona's sweep. Replaces `spira-ops-prod.service`
//! (`dwarf --home <prod> ops --sweep --prompt -`, stdin from `run/ops-sweep-prompt.txt`), timer
//! 5 min. Pattern P2: a multi-state machine whose long leg, the Ops claude session, is a transient
//! unit started by `unit.start` and finished by the `unit.ran` the units source submits.
//! Contract: `wiki/projects/spira/designs/tasks/persona-ops.md`. The sweep's steps are
//! `dwarf::ops_sweep`'s, called as a library; the session is a transient unit.
//!
//! Two shapes the runtime forces, both visible in [`PersonaOps::handle`]: `Observing` is never
//! journaled (an idle tick must journal nothing, so the observe's answer is decided from a
//! persisted `Idle`); and a `unit.ran` arrives on the `units` stream, where every command it
//! yields is journaled, so the carry is read as the latest `persona.ops.state` on either
//! stream ([`carry_of`]). A unit that never reports its end leaves the machine in `Running`
//! (then `Stopping` at the deadline) until it does.
//!
//! # Tick
//!
//! Every 5 min (`SPIRA_RUNTIME_PERSONA_OPS_SECS`), re-armed on the last pass's completion. Also
//! woken by `watch.tower` emitting its snapshot. Single-flight: a tick in any state but
//! [`State::Idle`] is refused naming the state. Incident claims come through `sentinel.call`.
//!
//! # Observe
//!
//! `persona.ops.observe` returns a [`Snapshot`]: `halted` (the world's halt), `rune_present`
//! (`runes/ops.rune`), `prompt_head` and `prompt` (read from `watch.tower`'s last emitted
//! snapshot, not the file), `live` and `max` (`live` from the scheduler's own unit table, S6
//! `unit.facts`; `RUNE_MAX_CONCURRENT`), `draining` (`world.draining`) and `pause` (the capacity
//! pause: open, paused, or unknown, which fails closed).
//!
//! # Decide
//!
//! `decide(carry, input) -> Result<(carry', acts), refusal>` is pure over the table
//! [`TRANSITIONS`]. Carry is `{ state, run_id, name, started_at }`. Outcome rule kept from
//! `sweep.rs`: a trace segment whose outcome is `unlanded` or `killed` counts as rc 0; every other
//! result keeps its rc. Fixtures (T5) come from `dwarf/src/sweep.rs` over its `Seam`: capacity,
//! draining, paused, statute-missing, normal, unlanded→0.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `persona.ops.observe` | host | 10 s | (probe) | nothing; an unreadable pause reads `Unknown` |
//! | `persona.ops.intake_install` | host | 60 s (unverified) | `ops/<run_id>/intake` | `SPIRA_ALERT_GLOB` unset: a no-op |
//! | `persona.ops.incident_drain` | store | 60 s (unverified) | `ops/<run_id>/drain` | no `SPIRA_DB`; a spooled file it cannot file |
//! | `persona.ops.brief` | host | 30 s | `ops/<run_id>/brief` | a missing core statute; an unresolvable persona model |
//! | `unit.start` (shared) | unit | 2000 s; the session's wall is `RUNE_TIMEOUT_SECONDS` | `ops/<run_id>` as the unit key | a unit of that name already active |
//! | `unit.stop` (shared) | unit | 30 s | `ops/<run_id>/stop` | a unit not running (done) |
//! | `persona.ops.ledger` | host | 5 s | `ops/<run_id>/<awake or done>` | nothing |
//! | `persona.ops.cleanup` | host | 5 s | `ops/<run_id>/cleanup` | nothing; a missing file is success |
//!
//! The `unit.start` input carries the claude argv, stdin from the task file, the log
//! `sweep-ops-<id>.log`, and `SPIRA_DWARF`, `BEADS_ACTOR` and `GIT_*`. The lease side thread goes;
//! liveness comes from the unit. `unit.ran` arrives on the `units` stream and is recognised by
//! `UnitRan::key_for(NAME)`; carrying the state back on [`STREAM`] is part of the port.
//!
//! # Silence
//!
//! The idle tick is Observing → Idle on `SWEEP:NOMINAL`, halted, or rune absent: the probe only,
//! no ledger line, no inbox line, no Emit. Today's nominal `ops.log` line is dropped.
//!
//! # Replaces
//!
//! `spira-ops-prod.service` and `.timer` and their manifest rows; children `world guard`,
//! `release intake install`, `incident.sh drain`, `bash lib.sh _dwarf_snapshot`,
//! `strand dwarf-count`, `bdq memories`, the capacity probe; only `claude` survives, inside the
//! unit. Pinned config: [`KEYS`]; `persona.ops.model` and the `RUNE_*` keys come from the toml
//! persona table and the rune file (unverified that `cfg` reads them).
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; Observe mode shadows every effect and no session
//! starts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runtime::event::*;
use runtime::sources::UNITS_STREAM;
use runtime::units::{Exit, UnitHandle, UnitRan, UnitSpec, UnitState, UNIT_RAN, UNIT_START, UNIT_STOP};
use serde::{Deserialize, Serialize};

use super::{commands, Transition};
use crate::watchers::answered_effect;
use crate::Effects;

pub const NAME: &str = "persona.ops";
pub const STREAM: &str = "task.persona.ops";
pub const TICK: EventKindTag = "ops.tick";
pub const DEADLINE: EventKindTag = "ops.deadline";
pub const STATE: EventKindTag = "persona.ops.state";

pub const OBSERVE: &str = "persona.ops.observe";
pub const INTAKE_INSTALL: &str = "persona.ops.intake_install";
pub const INCIDENT_DRAIN: &str = "persona.ops.incident_drain";
pub const BRIEF: &str = "persona.ops.brief";
pub const LEDGER: &str = "persona.ops.ledger";
pub const CLEANUP: &str = "persona.ops.cleanup";
pub const EFFECTS: &[&str] = &[OBSERVE, INTAKE_INSTALL, INCIDENT_DRAIN, BRIEF, UNIT_START, UNIT_STOP, LEDGER, CLEANUP];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_PERSONA_OPS_SECS";
/// `BD_TIMEOUT` is dropped: no step of the sweep calls bd with a deadline any more (statutes
/// come over the store's socket, the drain's bd calls carry their own).
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_AGENT", "SPIRA_MEMORIES_CACHE", "SPIRA_MEMORIES_CACHE_AGE", "SPIRA_STATUTE_CORE", "SPIRA_STATUTE_CORE_LOCAL", "SPIRA_BDQ_CONN_RETRIES", "SPIRA_ALERT_GLOB", "SPIRA_RUN"];

/// The first line of a prompt that means there is nothing to sweep.
pub const NOMINAL: &str = "SWEEP:NOMINAL";
/// The persona's rune.
pub const RUNE: &str = "ops";

const OBSERVE_DEADLINE_MS: u64 = 10_000;
const PREPARE_DEADLINE_MS: u64 = 60_000;
const BRIEF_DEADLINE_MS: u64 = 30_000;
/// The unit's `TimeoutStartSec`: the session is stopped if it has not ended by then.
const RUN_DEADLINE_MS: u64 = 2_000_000;
const STOP_DEADLINE_MS: u64 = 30_000;
const HOST_DEADLINE_MS: u64 = 5_000;
const TRACE_MARK: &str = "=== spira attempt";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pause {
    Open,
    Paused { left_s: u64 },
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub halted: bool,
    pub rune_present: bool,
    pub prompt_head: String,
    pub prompt: String,
    pub live: u32,
    pub max: u32,
    pub draining: bool,
    pub pause: Pause,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Observing,
    Preparing,
    Briefing,
    Starting,
    Running,
    Stopping,
    Recording,
}

/// `persona.ops.state`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub state: State,
    pub run_id: Option<String>,
    pub name: Option<String>,
    pub started_at: Option<u64>,
}

/// What the machine is handed: its tick, the observe answer, an effect's outcome by name, its
/// unit's end (with the trace segment's outcome, for the unlanded/killed→0 rule), or its deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Tick,
    Observed(Snapshot),
    Succeeded { effect: String, body: String },
    Failed { effect: String, error: String },
    UnitRan { ran: UnitRan, outcome: Option<String> },
    Deadline,
}

/// The Decide table of the contract, one row per legal (state, on) pair.
pub const TRANSITIONS: &[Transition<State>] = &[
    Transition { from: State::Idle, on: "tick", to: State::Observing, effects: &[OBSERVE] },
    Transition { from: State::Observing, on: "skip", to: State::Idle, effects: &[] },
    Transition { from: State::Observing, on: "busy", to: State::Idle, effects: &[LEDGER] },
    Transition { from: State::Observing, on: "go", to: State::Preparing, effects: &[INTAKE_INSTALL, INCIDENT_DRAIN] },
    Transition { from: State::Preparing, on: "prepared", to: State::Briefing, effects: &[BRIEF] },
    Transition { from: State::Briefing, on: "statute-missing", to: State::Idle, effects: &[LEDGER] },
    Transition { from: State::Briefing, on: "no-argv", to: State::Idle, effects: &[LEDGER] },
    Transition { from: State::Briefing, on: "briefed", to: State::Starting, effects: &[UNIT_START] },
    Transition { from: State::Starting, on: "started", to: State::Running, effects: &[LEDGER] },
    Transition { from: State::Starting, on: "start-failed", to: State::Idle, effects: &[LEDGER] },
    Transition { from: State::Running, on: "unit.ran", to: State::Recording, effects: &[LEDGER, CLEANUP] },
    Transition { from: State::Running, on: "deadline", to: State::Stopping, effects: &[UNIT_STOP] },
    Transition { from: State::Stopping, on: "unit.ran", to: State::Recording, effects: &[LEDGER, CLEANUP] },
    Transition { from: State::Recording, on: "recorded", to: State::Idle, effects: &[] },
];

/// The machine's next state on `on`, or a refusal naming the state it is in.
pub fn step(from: State, on: &str) -> Result<State, String> {
    TRANSITIONS.iter().find(|t| t.from == from && t.on == on).map(|t| t.to).ok_or_else(|| format!("{NAME}: no transition from {from:?} on {on}"))
}

/// One `dwarf-ledger.log` line: `awake ops <detail>`, or `done ops sweep rc=<rc> status=<detail>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerLine {
    pub what: String,
    pub detail: String,
    pub rc: Option<i32>,
    pub run_id: Option<String>,
}

/// `persona.ops.brief`'s input: the run it is for, and the prompt when not the watch tower's
/// (`run/ops-sweep-prompt.txt`, read by the runner when this is empty).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefAsk {
    #[serde(default)]
    pub prompt: String,
    pub run_id: String,
}

fn spec<T: Serialize>(name: &str, key: String, body: &T, deadline_ms: u64) -> EffectSpec {
    EffectSpec::keyed(name, key, serde_json::to_string(body).unwrap_or_default(), deadline_ms)
}

fn run_of(carry: &Carry) -> &str {
    carry.run_id.as_deref().unwrap_or_default()
}

fn ledger(carry: &Carry, what: &str, detail: &str, rc: Option<i32>) -> EffectSpec {
    spec(LEDGER, format!("ops/{}/{what}", run_of(carry)), &LedgerLine { what: what.into(), detail: detail.into(), rc, run_id: carry.run_id.clone() }, HOST_DEADLINE_MS)
}

fn with(carry: &Carry, state: State) -> Carry {
    Carry { state, ..carry.clone() }
}

/// The rc a finished session is recorded with: an unlanded or killed segment is 0
/// (`dwarf/src/sweep.rs`), a stopped session 124, otherwise its exit.
fn rc_of(ran: &UnitRan, outcome: Option<&str>, stopped: bool) -> i32 {
    if matches!(outcome, Some("unlanded" | "killed")) {
        return 0;
    }
    if stopped {
        return 124;
    }
    match ran.exit {
        Exit::Code(c) => c,
        Exit::Signal(s) => 128 + s,
        Exit::Unknown => 1,
    }
}

/// The pure decision: the next carry and the acts, or a refusal naming the state.
pub fn decide(carry: &Carry, input: &Input) -> Result<(Carry, Vec<EffectSpec>), String> {
    let s = carry.state;
    let go = |on: &str| step(s, on);
    let out = match input {
        Input::Tick => (with(carry, go("tick")?), vec![EffectSpec::probe(OBSERVE, String::new(), OBSERVE_DEADLINE_MS)]),
        Input::Observed(snap) => {
            if snap.halted || !snap.rune_present || snap.prompt_head.trim() == NOMINAL {
                (Carry { state: go("skip")?, ..Carry::default() }, vec![])
            } else if snap.live >= snap.max || snap.draining || snap.pause != Pause::Open {
                let why = if snap.live >= snap.max {
                    "capacity"
                } else if snap.draining {
                    "draining"
                } else {
                    "paused"
                };
                (Carry { state: go("busy")?, ..Carry::default() }, vec![ledger(carry, "awake", why, None)])
            } else {
                let next = with(carry, go("go")?);
                (next, vec![EffectSpec::keyed(INTAKE_INSTALL, format!("ops/{}/intake", run_of(carry)), String::new(), PREPARE_DEADLINE_MS), EffectSpec::keyed(INCIDENT_DRAIN, format!("ops/{}/drain", run_of(carry)), String::new(), PREPARE_DEADLINE_MS)])
            }
        }
        Input::Succeeded { effect, .. } | Input::Failed { effect, .. } if effect == LEDGER || effect == INTAKE_INSTALL || effect == UNIT_STOP => (carry.clone(), vec![]),
        Input::Succeeded { effect, .. } | Input::Failed { effect, .. } if effect == INCIDENT_DRAIN => {
            let ask = BriefAsk { prompt: String::new(), run_id: carry.run_id.clone().unwrap_or_default() };
            (with(carry, go("prepared")?), vec![spec(BRIEF, format!("ops/{}/brief", run_of(carry)), &ask, BRIEF_DEADLINE_MS)])
        }
        Input::Succeeded { effect, body } if effect == BRIEF => {
            let next = go("briefed")?;
            let b: dwarf::ops_sweep::SweepBrief = serde_json::from_str(body).map_err(|e| format!("{NAME}: the brief's answer is unreadable: {e}"))?;
            if b.argv.is_empty() {
                return Ok((Carry { state: go("no-argv")?, ..Carry::default() }, vec![ledger(carry, "done", "harness-red", Some(1))]));
            }
            let key = carry.run_id.clone().ok_or_else(|| format!("{NAME}: {s:?} has no run id"))?;
            let unit = UnitSpec { task: NAME.into(), key, argv: b.argv, env: b.env, cwd: b.sys_file.parent().map(Path::to_path_buf) };
            (Carry { state: next, name: Some(b.name), ..carry.clone() }, vec![spec(UNIT_START, format!("ops/{}", run_of(carry)), &unit, RUN_DEADLINE_MS)])
        }
        Input::Failed { effect, error } if effect == BRIEF => {
            if error.contains(dwarf::ops_sweep::STATUTE_MISSING) {
                (Carry { state: go("statute-missing")?, ..Carry::default() }, vec![ledger(carry, "awake", "statute-core-missing", None)])
            } else {
                (Carry { state: go("no-argv")?, ..Carry::default() }, vec![ledger(carry, "done", "harness-red", Some(1))])
            }
        }
        Input::Succeeded { effect, .. } if effect == UNIT_START => (with(carry, go("started")?), vec![ledger(carry, "awake", "sweep", None)]),
        Input::Failed { effect, .. } if effect == UNIT_START => (Carry { state: go("start-failed")?, ..Carry::default() }, vec![ledger(carry, "done", "harness-red", Some(1))]),
        Input::Succeeded { effect, .. } | Input::Failed { effect, .. } if effect == CLEANUP => (Carry { state: go("recorded")?, ..Carry::default() }, vec![]),
        Input::Succeeded { effect, .. } | Input::Failed { effect, .. } => return Err(format!("{NAME}: {s:?} expects no outcome of {effect}")),
        Input::UnitRan { ran, outcome } => {
            let stopped = s == State::Stopping;
            let next = go("unit.ran")?;
            let rc = rc_of(ran, outcome.as_deref(), stopped);
            (with(carry, next), vec![ledger(carry, "done", "sweep", Some(rc)), spec(CLEANUP, format!("ops/{}/cleanup", run_of(carry)), &carry.run_id, HOST_DEADLINE_MS)])
        }
        Input::Deadline => {
            let next = go("deadline")?;
            let key = carry.run_id.clone().ok_or_else(|| format!("{NAME}: {s:?} has no run id"))?;
            let h = UnitHandle::of(NAME, &key).map_err(|e| e.to_string())?;
            (with(carry, next), vec![spec(UNIT_STOP, format!("ops/{}/stop", run_of(carry)), &h, STOP_DEADLINE_MS)])
        }
    };
    Ok(out)
}

/// The latest `persona.ops.state` on the task's own stream or on `units`, where the commands a
/// `unit.ran` yields are journaled.
pub fn carry_of(view: &Projection) -> Carry {
    let latest = |stream: &str| view.events(&StreamKey(stream.into())).iter().rev().find(|e| e.kind.tag == STATE).map(|e| (e.at, e.kind.body.clone()));
    let (own, units) = (latest(STREAM), latest(UNITS_STREAM));
    let body = match (own, units) {
        (Some(o), Some(u)) => {
            if u.0 > o.0 {
                Some(u.1)
            } else {
                Some(o.1)
            }
        }
        (o, u) => o.or(u).map(|x| x.1),
    };
    body.and_then(|b| serde_json::from_str(&b).ok()).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonaOps {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl PersonaOps {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(PersonaOps { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("persona.ops/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::event::RowCodec { also: &[UNITS_STREAM], ..runtime::row_codec!(STREAM, STATE, ROW_PARTS)})
    }

    fn ours(effect: &str) -> bool {
        effect.starts_with("persona.ops.") || effect == UNIT_START || effect == UNIT_STOP
    }

    fn input(view: &Projection, ev: &Event) -> Option<Input> {
        let here = ev.stream.0 == STREAM || ev.stream.0 == UNITS_STREAM;
        match ev.kind.tag.as_str() {
            TICK if ev.stream.0 == STREAM => Some(Input::Tick),
            DEADLINE if ev.stream.0 == STREAM => Some(Input::Deadline),
            UNIT_RAN => UnitRan::from_event_kind(&ev.kind).filter(|r| r.key_for(NAME).is_some()).map(|ran| Input::UnitRan { ran, outcome: None }),
            EFFECT_SUCCEEDED if here => match answered_effect(view, ev).filter(|n| Self::ours(n))? {
                n if n == OBSERVE => serde_json::from_str(&ev.kind.body).ok().map(Input::Observed),
                effect => Some(Input::Succeeded { effect, body: ev.kind.body.clone() }),
            },
            EFFECT_FAILED if here => answered_effect(view, ev).filter(|n| Self::ours(n)).map(|effect| Input::Failed { effect, error: ev.kind.body.clone() }),
            _ => None,
        }
    }

    /// A write's input with the pinned config beside the act, so its runner reads no config.
    fn pinned(&self, a: EffectSpec) -> EffectSpec {
        if a.name == UNIT_START || a.name == UNIT_STOP {
            return a;
        }
        let act: serde_json::Value = serde_json::from_str(&a.input).unwrap_or(serde_json::Value::String(a.input.clone()));
        EffectSpec { input: serde_json::json!({ "cfg": self.config, "act": act }).to_string(), ..a }
    }
}

const ROW_PARTS: &[runtime::rows::Part] = &[runtime::rows::Part::Field("state"), runtime::rows::Part::Field("run_id"), runtime::rows::Part::Field("name"), runtime::rows::Part::Field("started_at")];

impl Handler for PersonaOps {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, DEADLINE, UNIT_RAN, EFFECT_SUCCEEDED, EFFECT_FAILED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let Some(input) = Self::input(view, ev) else { return vec![] };
        let persisted = carry_of(view);
        // `Observing` is never journaled: the observe's answer is decided as if it were.
        let carry = match (&input, persisted.state) {
            (Input::Observed(_), State::Idle) => with(&persisted, State::Observing),
            _ => persisted.clone(),
        };
        let mut out = match decide(&carry, &input) {
            Ok((mut next, acts)) => {
                if next.state == State::Preparing && next.run_id.is_none() {
                    next.run_id = Some(format!("r{}", ev.at));
                }
                if next.state == State::Running && carry.state == State::Starting {
                    next.started_at = Some(ev.at);
                }
                let acts: Vec<EffectSpec> = acts.into_iter().map(|a| self.with_run(a, &next)).collect();
                let (probes, writes): (Vec<EffectSpec>, Vec<EffectSpec>) = acts.into_iter().partition(|a| a.name == OBSERVE);
                let recorded = if next.state == State::Observing { persisted.clone() } else { next.clone() };
                let mut out = commands(&persisted, &recorded, STATE, writes.into_iter().map(|a| self.pinned(a)).collect());
                out.extend(probes.into_iter().map(|p| Command::Probe(EffectSpec { input: serde_json::json!({ "cfg": self.config }).to_string(), ..p })));
                if next.state == State::Running && carry.state == State::Starting {
                    out.push(Command::Timer { at: ev.at + RUN_DEADLINE_MS, kind: EventKind::new(DEADLINE, "") });
                }
                out
            }
            Err(refusal) => {
                ctx.event().set("ops.refused", refusal.as_str());
                vec![]
            }
        };
        if input == Input::Tick {
            out.push(Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") });
        }
        out
    }
}

impl PersonaOps {
    /// The run id the handler assigned on entering `Preparing`, written into the acts that name it.
    fn with_run(&self, a: EffectSpec, next: &Carry) -> EffectSpec {
        let Some(id) = &next.run_id else { return a };
        let a = match a.name.as_str() {
            BRIEF => match serde_json::from_str::<BriefAsk>(&a.input) {
                Ok(ask) if ask.run_id.is_empty() => spec(BRIEF, a.key.clone(), &BriefAsk { run_id: id.clone(), ..ask }, a.deadline_ms),
                _ => a,
            },
            LEDGER => match serde_json::from_str::<LedgerLine>(&a.input) {
                Ok(l) if l.run_id.is_none() => spec(LEDGER, a.key.clone(), &LedgerLine { run_id: Some(id.clone()), ..l }, a.deadline_ms),
                _ => a,
            },
            _ => a,
        };
        EffectSpec { key: a.key.replacen("ops//", &format!("ops/{id}/"), 1), ..a }
    }
}

// ── runners ─────────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Pinned<T> {
    cfg: BTreeMap<String, String>,
    act: T,
}

fn parse<T: serde::de::DeserializeOwned>(input: &str) -> Result<Pinned<T>, String> {
    serde_json::from_str(input).map_err(|e| format!("{NAME}: unreadable input: {e}"))
}

fn get<'a>(cfg: &'a BTreeMap<String, String>, k: &str) -> &'a str {
    cfg.get(k).map(String::as_str).unwrap_or("")
}

fn run_dir(e: &Effects, cfg: &BTreeMap<String, String>) -> PathBuf {
    Some(get(cfg, "SPIRA_RUN")).filter(|r| !r.is_empty()).map(PathBuf::from).unwrap_or_else(|| e.world.run_dir.clone())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) // clock-safe: a ledger stamp and a pause comparison, read once per effect
}

fn lookup(cfg: &BTreeMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
    move |k: &str| cfg.get(k).cloned().filter(|v| !v.is_empty()).or_else(|| spira_config::process::cfg(k).ok().filter(|v| !v.is_empty()))
}

/// Every unit this task starts: its unit name with the key a glob.
fn unit_glob() -> String {
    let one = runtime::units::unit_name(NAME, "k").unwrap_or_default();
    format!("{}*", one.trim_end_matches("k.service"))
}

/// `persona.ops.observe`: the halt, the rune, the watch tower's prompt, the live sessions
/// (this task's own units), the drain and the account's pause.
pub fn run_observe(e: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    #[derive(Deserialize)]
    struct In {
        cfg: BTreeMap<String, String>,
    }
    let cfg = serde_json::from_str::<In>(&s.input).map(|i| i.cfg).unwrap_or_default();
    let run = run_dir(e, &cfg);
    let home = spira_config::resolve::locate_home_for_process()?;
    let rune_file = home.join("runes").join(format!("{RUNE}.rune"));
    let prompt = std::fs::read_to_string(run.join("ops-sweep-prompt.txt")).unwrap_or_default();
    let max = if rune_file.is_file() { dwarf::ops_sweep::rune(&home, RUNE, &lookup(&cfg)).map(|r| r.max_concurrent).unwrap_or(1) } else { 0 };
    let live = e
        .units
        .as_deref()
        .and_then(|u| u.list(&unit_glob()).ok())
        .map(|l| l.iter().filter(|d| matches!(d.state, UnitState::Active | UnitState::Activating)).count() as u32)
        .unwrap_or(0);
    let pause_file = Some(get(&cfg, "SPIRA_CAPACITY_PAUSE")).filter(|p| !p.is_empty()).map(PathBuf::from).unwrap_or_else(|| run.join("capacity-pause"));
    let pause = match dwarf::capacity::pause_state(&pause_file) {
        dwarf::capacity::PauseState::Open => Pause::Open,
        dwarf::capacity::PauseState::Paused { until, .. } if until <= now_secs() => Pause::Open,
        dwarf::capacity::PauseState::Paused { until, .. } => Pause::Paused { left_s: (until - now_secs()) as u64 },
        dwarf::capacity::PauseState::Unknown => Pause::Unknown,
    };
    let snap = Snapshot {
        halted: run.join("world.halted").exists(),
        rune_present: rune_file.is_file(),
        prompt_head: prompt.lines().next().unwrap_or("").to_string(),
        prompt: prompt.trim_end_matches('\n').to_string(),
        live,
        max,
        draining: run.join("world.draining").is_file(),
        pause,
    };
    serde_json::to_string(&snap).map_err(|e| e.to_string())
}

/// `persona.ops.intake_install`: wires the alert templates' incident drop-ins (`release`'s
/// intake as a library); an unset `SPIRA_ALERT_GLOB` is a no-op.
pub fn run_intake_install(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let p: Pinned<serde_json::Value> = parse(&s.input)?;
    let pattern = Some(get(&p.cfg, "SPIRA_ALERT_GLOB").to_string()).filter(|g| !g.is_empty());
    if pattern.is_none() {
        return Ok(release::intake::NO_GLOB.to_string());
    }
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let unit_dir = env("SPIRA_UNIT_DIR").map(PathBuf::from).or_else(|| env("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("systemd/user"))).or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config/systemd/user"))).ok_or("no systemd unit directory: set SPIRA_UNIT_DIR, XDG_CONFIG_HOME or HOME")?;
    let home = spira_config::resolve::locate_home_for_process()?;
    let o = release::intake::Opts { unit_dir, pattern, incident: home.join("incident.sh"), reload: true };
    match release::intake::install(&release::intake::RealTemplates, &release::systemctl::RealSystemctl::from_env(), &o)? {
        None => Ok(release::intake::NO_GLOB.to_string()),
        Some(r) if r.unverified.is_empty() => Ok(format!("{} template(s), {} wired", r.total, r.wired.len())),
        Some(r) => Err(format!("{} template(s), {} wired, {} unverified: {}", r.total, r.wired.len(), r.unverified.len(), r.unverified.join(" "))),
    }
}

/// `persona.ops.incident_drain`: files every spooled incident (`incident`'s intake as a library).
pub fn run_incident_drain(_: &Effects, _key: &str, _s: &EffectSpec) -> Result<String, String> {
    let env = incident::intake::Env::load();
    if env.db.is_none() {
        return Err("no SPIRA_DB: the incident spool cannot be drained".into());
    }
    let bd = incident::real::RealBd::from_env()?;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&env.spool_dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().map(|e| e != "bad").unwrap_or(true)).collect();
    paths.sort();
    let (mut n, mut stuck) = (0, 0);
    for p in paths {
        match incident::intake::drain_one(&env, &bd, &incident::real::RealMailer, &incident::real::RealClock, &p) {
            Ok(_) => n += 1,
            Err(()) => stuck += 1,
        }
    }
    if stuck == 0 {
        Ok(format!("drained {n}"))
    } else {
        Err(format!("drained {n}, still spooled {stuck}"))
    }
}

/// `persona.ops.brief`: the name, the statutes (memories over the store's socket), the two
/// brief files and the session's argv (`dwarf::ops_sweep::brief`).
pub fn run_brief(e: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let p: Pinned<BriefAsk> = parse(&s.input)?;
    let cfg = &p.cfg;
    let home = spira_config::resolve::locate_home_for_process()?;
    let run = run_dir(e, cfg);
    let rune = dwarf::ops_sweep::rune(&home, RUNE, &lookup(cfg))?;
    let bc = dwarf::ops_sweep::BriefCfg {
        agent: get(cfg, "SPIRA_AGENT").to_string(),
        memories_cache: get(cfg, "SPIRA_MEMORIES_CACHE").to_string(),
        memories_cache_age: get(cfg, "SPIRA_MEMORIES_CACHE_AGE").parse().unwrap_or(0),
        statute_core: get(cfg, "SPIRA_STATUTE_CORE").to_string(),
        statute_core_local: get(cfg, "SPIRA_STATUTE_CORE_LOCAL").to_string(),
        // The home repo's checkout from the pinned repo map, not `SPIRA_REPO`: that is a
        // per-copy fact the config door never resolves, so declaring it refused the settings whole.
        harness_repo: e.world.home_path().map(|p| p.display().to_string()).unwrap_or_default(),
        toml: spira_config::process::cfg("SPIRA_TOML").unwrap_or_default(),
    };
    let lc = spira_config::lc_call::LcClient::socket(&e.world.lc_socket);
    let memories = || lc.answer(&["content", "memories", "--json"]).map_err(String::from);
    let prompt = if p.act.prompt.is_empty() { std::fs::read_to_string(run.join("ops-sweep-prompt.txt")).unwrap_or_default() } else { p.act.prompt.clone() };
    let b = dwarf::ops_sweep::brief(&home, &run, &rune, prompt.trim_end_matches('\n'), &p.act.run_id, &bc, now_secs(), &memories)?;
    serde_json::to_string(&b).map_err(|e| e.to_string())
}

/// `persona.ops.ledger`: one line on `dwarf-ledger.log`; a done line carries the session's
/// spend from its log.
pub fn run_ledger(e: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let p: Pinned<LedgerLine> = parse(&s.input)?;
    let run = run_dir(e, &p.cfg);
    let log = p.act.run_id.as_ref().map(|id| run.join(format!("sweep-{RUNE}-{id}.log")));
    let done = (p.act.what == "done").then(|| (p.act.rc.unwrap_or(1), p.act.detail.as_str(), log.as_deref()));
    dwarf::ops_sweep::ledger_line(&run, now_secs(), RUNE, done, &p.act.detail, TRACE_MARK);
    Ok(format!("{} {RUNE} {}", p.act.what, p.act.detail))
}

/// `persona.ops.cleanup`: removes the run's brief and name files.
pub fn run_cleanup(e: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let p: Pinned<Option<String>> = parse(&s.input)?;
    if let Some(id) = &p.act {
        dwarf::ops_sweep::cleanup(&run_dir(e, &p.cfg), RUNE, id);
    }
    Ok("cleaned".into())
}
