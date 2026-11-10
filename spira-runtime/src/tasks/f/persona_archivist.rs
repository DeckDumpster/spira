//! `persona.archivist`: the archivist's sweep over live Concierge transcripts. Replaces
//! `spira-archivist-prod.service` (`archivist sweep`, timer every 5 min). Pattern P2: each
//! archivist claude session runs as a transient unit, started by `unit.start` and finished by
//! `unit.ran`. Contract: `wiki/projects/spira/designs/tasks/persona-archivist.md`. The steps are
//! `archivist::run`'s (`prepare`, `commit_after`, `settle`, `forget_stale_sessions`) called as
//! a library; `archive` composes the same four for the CLI.
//!
//! As in [`super::persona_ops`]: `Observing` is never journaled, and the carry is the latest
//! `persona.archivist.state` on the task's stream or on `units` ([`super::persona_ops::carry_of`]'s
//! rule). The probe drops sessions still inside their retry wait (it has the clock), so
//! `decide` applies the drift threshold and the budget. A capacity refusal is read from the
//! session's log by the settle step, whose answer `capacity` defers the rest of the pass. The
//! brief's locks are held for the brief only, as the contract asks.
//!
//! # Tick
//!
//! Every 5 min (`SPIRA_RUNTIME_PERSONA_ARCHIVIST_SECS`); no other wake source. Single-flight: a
//! tick in any state but [`State::Idle`] is refused. `archivist.lock` stays only as an
//! effect-level lock inside `persona.archivist.brief`, because `archivist now` takes it from the
//! CLI.
//!
//! # Observe
//!
//! `persona.archivist.observe` returns a [`Snapshot`]: `paused` (the dwarf capacity pause), the
//! candidates (live `*.jsonl` under `SPIRA_TOKEN_PROJECTS` younger than `SPIRA_ARCHIVIST_IDLE`,
//! with the context meter's `ctx`, `turns` and drift, and the per-session bookkeeping
//! `<sid>.state`, `.covered`, `retry_at`) and `stale` (every `<sid>.state` whose transcript is
//! gone). The meter is a script fork today and should become a library read (unverified).
//!
//! # Decide
//!
//! The pure part is `candidates::would_archive`, `sort_by_drift_desc` and the per-pass budget.
//! Carry is `{ state, run_id, queue, current: {sid, at, tc}, archived }`; the table is
//! [`TRANSITIONS`]. Fixtures come from `run.rs`'s `Seam` double: paused, drift below or above
//! `every`, failed with and without `retry_at`, rc 0/124/capacity/other, a dirty checkout.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `persona.archivist.observe` | host | 30 s | (probe) | an unreadable meter: that session is skipped |
//! | `persona.archivist.brief` | host | 30 s | `archivist/<run_id>/<sid>/brief` | either lock held (75); `runes/archivist.md` missing |
//! | `unit.start` (shared) | unit | `SPIRA_ARCHIVIST_TIMEOUT × (tc+1)`, capped at 1800 s | `archivist/<run_id>/<sid>` as the unit key | a unit of that name already active |
//! | `unit.stop` (shared) | unit | 30 s | `archivist/<run_id>/<sid>/stop` | a unit not running |
//! | `persona.archivist.commit` | git | 150 s | `archivist/<sid>/<at_turn>/commit` | paths outside the wiki; a dirty checkout |
//! | `persona.archivist.settle` | host | 5 s | `archivist/<sid>/<at_turn>/settle` | nothing |
//! | `persona.archivist.digest_send` | host | 30 s | `archivist/digest/<date in SPIRA_TZ>` | an empty `digest.pending`; already sent today |
//! | `persona.archivist.mark` | host | 5 s | `archivist/<run_id>/mark` | nothing |
//! | `persona.archivist.forget` | host | 10 s | `archivist/forget/<sid>` | a sid whose transcript exists |
//!
//! The `unit.start` input carries the claude argv (`--model <SPIRA_ARCHIVIST_MODEL>`, the
//! allowed tools, `--add-dir <wiki>`), the cwd `archivist/cwd`, `SPIRA_MAIL_FROM`,
//! `SPIRA_WORK_ACTOR=archivist` and `SPIRA_WIKI` with `SPIRA_RUNE` unset, stdin from the task file
//! and the log `archivist-<sid>.log`. The effect deadline is the real bound on a session: today
//! `SPIRA_ARCHIVIST_TIMEOUT_RETRIES=3` can scale past the unit's 1800 s (unverified that it fires).
//!
//! # Silence
//!
//! An idle tick (not paused, no candidate at or above `every`, nothing stale) emits the probe only.
//!
//! # Replaces
//!
//! `spira-archivist-prod.service` and `.timer` and their manifest rows; children `bash lib.sh`,
//! `ctx-meter.sh`, `archive.sh lineage`, `mail`, `timeout git` ×4 and `timeout claude`; only `git`
//! and `claude` (inside the unit) remain. Pinned config: [`KEYS`].
//!
//! # Reverse
//!
//! Release rollback restores the unit and timer; Observe mode shadows the effects, no session
//! starts and nothing is committed to brain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use runtime::event::*;
use runtime::sources::UNITS_STREAM;
use runtime::units::{Exit, UnitHandle, UnitRan, UnitSpec, UNIT_RAN, UNIT_START, UNIT_STOP};
use serde::{Deserialize, Serialize};

use super::{commands, Transition};
use crate::watchers::answered_effect;
use crate::Effects;

pub const NAME: &str = "persona.archivist";
pub const STREAM: &str = "task.persona.archivist";
pub const TICK: EventKindTag = "archivist.tick";
pub const DEADLINE: EventKindTag = "archivist.deadline";
pub const STATE: EventKindTag = "persona.archivist.state";

pub const OBSERVE: &str = "persona.archivist.observe";
pub const BRIEF: &str = "persona.archivist.brief";
pub const COMMIT: &str = "persona.archivist.commit";
pub const SETTLE: &str = "persona.archivist.settle";
pub const DIGEST_SEND: &str = "persona.archivist.digest_send";
pub const MARK: &str = "persona.archivist.mark";
pub const FORGET: &str = "persona.archivist.forget";
pub const EFFECTS: &[&str] = &[OBSERVE, BRIEF, UNIT_START, UNIT_STOP, COMMIT, SETTLE, DIGEST_SEND, MARK, FORGET];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_PERSONA_ARCHIVIST_SECS";
pub const KEYS: &[&str] = &[
    CADENCE_KEY,
    "SPIRA_DB",
    "SPIRA_RUN",
    "SPIRA_RUNES_DIR",
    "SPIRA_TOKEN_PROJECTS",
    "SPIRA_WIKI",
    "SPIRA_AGENT",
    "SPIRA_TZ",
    "SPIRA_ARCHIVIST_EVERY",
    "SPIRA_ARCHIVIST_IDLE",
    "SPIRA_ARCHIVIST_MODEL",
    "SPIRA_ARCHIVIST_TIMEOUT",
    "SPIRA_ARCHIVIST_PER_PASS",
    "SPIRA_ARCHIVIST_TIMEOUT_RETRIES",
    "SPIRA_ARCHIVIST_FAIL_RETRIES",
    "SPIRA_ARCHIVIST_FAIL_BACKOFF",
];

const OBSERVE_DEADLINE_MS: u64 = 30_000;
const BRIEF_DEADLINE_MS: u64 = 30_000;
const COMMIT_DEADLINE_MS: u64 = 150_000;
const HOST_DEADLINE_MS: u64 = 5_000;
const FORGET_DEADLINE_MS: u64 = 10_000;
const DIGEST_DEADLINE_MS: u64 = 30_000;
const STOP_DEADLINE_MS: u64 = 30_000;
/// The unit's own bound: the session's `timeout` kills it first; the stop is the backstop.
const START_DEADLINE_MS: u64 = 1_800_000;
const STOP_GRACE_S: u64 = 120;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub sid: String,
    pub tp: String,
    pub ctx: u64,
    pub turns: u64,
    pub drift: u64,
    pub band: String,
    pub prev_state: String,
    pub retry_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub paused: bool,
    pub candidates: Vec<Candidate>,
    pub stale: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Observing,
    Briefing,
    Starting,
    Running,
    Committing,
    Settling,
    Forgetting,
}

/// The session being archived: its id, the turn it is archived at, and its timeout count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Current {
    pub sid: String,
    pub at: u64,
    pub tc: u32,
}

/// `persona.archivist.state`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub state: State,
    pub run_id: Option<String>,
    pub queue: Vec<String>,
    pub current: Option<Current>,
    pub archived: u32,
}

/// What the machine is handed: its tick, the observe answer, an effect's outcome by name, its
/// unit's end, or its deadline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Tick,
    Observed(Snapshot),
    Succeeded { effect: String, body: String },
    Failed { effect: String, error: String },
    UnitRan(UnitRan),
    Deadline,
}

/// The Decide table of the contract, one row per legal (state, on) pair. `deadline` in Running
/// (stop the unit, then settle on its `unit.ran` as a timeout) is the table's reading of "the
/// deadline passes and `unit.stop` runs". `start-failed` is this port's: a unit that would not
/// start settles as a failure.
pub const TRANSITIONS: &[Transition<State>] = &[
    Transition { from: State::Idle, on: "tick", to: State::Observing, effects: &[OBSERVE] },
    Transition { from: State::Observing, on: "paused", to: State::Idle, effects: &[MARK] },
    Transition { from: State::Observing, on: "nothing", to: State::Idle, effects: &[] },
    Transition { from: State::Observing, on: "stale-only", to: State::Forgetting, effects: &[FORGET] },
    Transition { from: State::Observing, on: "candidates", to: State::Briefing, effects: &[BRIEF] },
    Transition { from: State::Briefing, on: "skip-next", to: State::Briefing, effects: &[BRIEF] },
    Transition { from: State::Briefing, on: "skip-last", to: State::Forgetting, effects: &[FORGET] },
    Transition { from: State::Briefing, on: "briefed", to: State::Starting, effects: &[UNIT_START] },
    Transition { from: State::Starting, on: "started", to: State::Running, effects: &[] },
    Transition { from: State::Starting, on: "start-failed", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Running, on: "ran-ok-wiki", to: State::Committing, effects: &[COMMIT] },
    Transition { from: State::Running, on: "ran-ok", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Running, on: "deadline", to: State::Running, effects: &[UNIT_STOP] },
    Transition { from: State::Running, on: "timeout", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Running, on: "capacity", to: State::Forgetting, effects: &[SETTLE] },
    Transition { from: State::Running, on: "ran-failed", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Committing, on: "committed", to: State::Settling, effects: &[SETTLE, DIGEST_SEND] },
    Transition { from: State::Committing, on: "dirty", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Committing, on: "commit-failed", to: State::Settling, effects: &[SETTLE] },
    Transition { from: State::Settling, on: "next", to: State::Briefing, effects: &[BRIEF] },
    Transition { from: State::Settling, on: "done", to: State::Forgetting, effects: &[FORGET] },
    Transition { from: State::Forgetting, on: "forgotten", to: State::Idle, effects: &[] },
];

/// The machine's next state on `on`, or a refusal naming the state it is in.
pub fn step(from: State, on: &str) -> Result<State, String> {
    TRANSITIONS.iter().find(|t| t.from == from && t.on == on).map(|t| t.to).ok_or_else(|| format!("{NAME}: no transition from {from:?} on {on}"))
}

/// `persona.archivist.brief`'s input: the session, and what the observe read of it (absent
/// for a session taken from the queue, which the runner reads again).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefAsk {
    pub sid: String,
    #[serde(default)]
    pub tp: Option<String>,
    #[serde(default)]
    pub at: Option<u64>,
    #[serde(default)]
    pub ctx: Option<u64>,
    #[serde(default)]
    pub drift: Option<u64>,
}

/// The brief's answer: the two files, where the wiki's before-snapshot is kept, and the unit's
/// argv, environment and working directory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Briefed {
    pub sysfile: String,
    pub taskfile: String,
    pub wiki_before: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub at: Option<u64>,
    #[serde(default)]
    pub tc: Option<u32>,
}

/// `persona.archivist.settle`'s input: `outcome` is `safe`, `retry`, `commit-failed`, `timeout`
/// or `failed` (with the session's `rc`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settle {
    pub sid: String,
    pub at: u64,
    pub outcome: String,
    pub rc: Option<i32>,
}

fn spec<T: Serialize>(name: &str, key: String, body: &T, deadline_ms: u64) -> EffectSpec {
    EffectSpec::keyed(name, key, serde_json::to_string(body).unwrap_or_default(), deadline_ms)
}

fn run_of(carry: &Carry) -> &str {
    carry.run_id.as_deref().unwrap_or_default()
}

fn brief(carry: &Carry, ask: BriefAsk) -> EffectSpec {
    spec(BRIEF, format!("archivist/{}/{}/brief", run_of(carry), ask.sid), &ask, BRIEF_DEADLINE_MS)
}

fn forget(carry: &Carry) -> EffectSpec {
    spec(FORGET, format!("archivist/forget/{}", run_of(carry)), &serde_json::json!({}), FORGET_DEADLINE_MS)
}

fn settle(carry: &Carry, outcome: &str, rc: Option<i32>) -> EffectSpec {
    let c = carry.current.clone().unwrap_or(Current { sid: String::new(), at: 0, tc: 0 });
    spec(SETTLE, format!("archivist/{}/{}/settle", c.sid, c.at), &Settle { sid: c.sid, at: c.at, outcome: outcome.into(), rc }, HOST_DEADLINE_MS)
}

fn num(config: &BTreeMap<String, String>, k: &str, default: u64) -> u64 {
    config.get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

/// The next session from the queue, or the end of the pass.
fn next_or_done(carry: &Carry, archived: u32, config: &BTreeMap<String, String>, on_next: &str, on_done: &str) -> Result<(Carry, Vec<EffectSpec>), String> {
    let per_pass = num(config, "SPIRA_ARCHIVIST_PER_PASS", 1) as u32;
    let s = carry.state;
    match carry.queue.split_first() {
        Some((sid, rest)) if archived < per_pass => {
            let next = Carry { state: step(s, on_next)?, queue: rest.to_vec(), current: Some(Current { sid: sid.clone(), at: 0, tc: 0 }), archived, ..carry.clone() };
            Ok((next, vec![brief(carry, BriefAsk { sid: sid.clone(), ..BriefAsk::default() })]))
        }
        _ => Ok((Carry { state: step(s, on_done)?, queue: vec![], current: None, archived, ..carry.clone() }, vec![forget(carry)])),
    }
}

/// The pure decision, with `config` the pinned [`KEYS`] (`SPIRA_WIKI` empty means no wiki): the
/// next carry and the acts, or a refusal naming the state.
pub fn decide(carry: &Carry, input: &Input, config: &BTreeMap<String, String>) -> Result<(Carry, Vec<EffectSpec>), String> {
    let s = carry.state;
    let go = |on: &str| step(s, on);
    let wiki = config.get("SPIRA_WIKI").is_some_and(|w| !w.trim().is_empty());
    let unexpected = |effect: &str| Err(format!("{NAME}: {s:?} expects no outcome of {effect}"));
    match input {
        Input::Tick => Ok((Carry { state: go("tick")?, ..carry.clone() }, vec![EffectSpec::probe(OBSERVE, String::new(), OBSERVE_DEADLINE_MS)])),
        Input::Observed(snap) => {
            if snap.paused {
                return Ok((Carry { state: go("paused")?, ..Carry::default() }, vec![spec(MARK, format!("archivist/{}/mark", run_of(carry)), &serde_json::json!({}), HOST_DEADLINE_MS)]));
            }
            let every = num(config, "SPIRA_ARCHIVIST_EVERY", 40);
            let mut due: Vec<&Candidate> = snap.candidates.iter().filter(|c| archivist::candidates::would_archive(c.drift as i64, every, None, None, 0)).collect();
            due.sort_by_key(|c| std::cmp::Reverse(c.drift));
            match due.split_first() {
                None if snap.stale.is_empty() => Ok((Carry { state: go("nothing")?, ..Carry::default() }, vec![])),
                None => Ok((Carry { state: go("stale-only")?, ..Carry::default() }, vec![forget(carry)])),
                Some((first, rest)) => {
                    let next = Carry { state: go("candidates")?, run_id: carry.run_id.clone(), queue: rest.iter().map(|c| c.sid.clone()).collect(), current: Some(Current { sid: first.sid.clone(), at: first.turns, tc: 0 }), archived: 0 };
                    Ok((next, vec![brief(carry, BriefAsk { sid: first.sid.clone(), tp: Some(first.tp.clone()), at: Some(first.turns), ctx: Some(first.ctx), drift: Some(first.drift) })]))
                }
            }
        }
        Input::Succeeded { effect, .. } | Input::Failed { effect, .. } if effect == MARK || effect == DIGEST_SEND || effect == UNIT_STOP => Ok((carry.clone(), vec![])),
        Input::Succeeded { effect, body } if effect == BRIEF => {
            let state = go("briefed")?;
            let b: Briefed = serde_json::from_str(body).map_err(|e| format!("{NAME}: the brief's answer is unreadable: {e}"))?;
            let mut current = carry.current.clone().ok_or_else(|| format!("{NAME}: {s:?} has no session"))?;
            current.at = b.at.unwrap_or(current.at);
            current.tc = b.tc.unwrap_or(current.tc);
            let run = carry.run_id.clone().ok_or_else(|| format!("{NAME}: {s:?} has no run id"))?;
            let unit = UnitSpec { task: NAME.into(), key: format!("{run}-{}", current.sid), argv: b.argv, env: b.env, cwd: b.cwd };
            Ok((Carry { state, current: Some(current), ..carry.clone() }, vec![spec(UNIT_START, format!("archivist/{run}/{}", carry.current.as_ref().map(|c| c.sid.as_str()).unwrap_or_default()), &unit, START_DEADLINE_MS)]))
        }
        Input::Failed { effect, .. } if effect == BRIEF => next_or_done(carry, carry.archived, config, "skip-next", "skip-last"),
        Input::Succeeded { effect, .. } if effect == UNIT_START => Ok((Carry { state: go("started")?, ..carry.clone() }, vec![])),
        Input::Failed { effect, .. } if effect == UNIT_START => Ok((Carry { state: go("start-failed")?, ..carry.clone() }, vec![settle(carry, "failed", Some(1))])),
        Input::UnitRan(ran) => match ran.exit {
            Exit::Code(0) if wiki => {
                let sid = carry.current.as_ref().map(|c| c.sid.clone()).unwrap_or_default();
                Ok((Carry { state: go("ran-ok-wiki")?, ..carry.clone() }, vec![spec(COMMIT, format!("archivist/{sid}/{}/commit", carry.current.as_ref().map_or(0, |c| c.at)), &serde_json::json!({ "sid": sid }), COMMIT_DEADLINE_MS)]))
            }
            Exit::Code(0) => Ok((Carry { state: go("ran-ok")?, ..carry.clone() }, vec![settle(carry, "safe", Some(0))])),
            Exit::Code(124) | Exit::Signal(_) => Ok((Carry { state: go("timeout")?, ..carry.clone() }, vec![settle(carry, "timeout", Some(124))])),
            Exit::Code(rc) => Ok((Carry { state: go("ran-failed")?, ..carry.clone() }, vec![settle(carry, "failed", Some(rc))])),
            Exit::Unknown => Ok((Carry { state: go("ran-failed")?, ..carry.clone() }, vec![settle(carry, "failed", Some(1))])),
        },
        Input::Deadline => {
            let state = go("deadline")?;
            let run = carry.run_id.clone().ok_or_else(|| format!("{NAME}: {s:?} has no run id"))?;
            let sid = carry.current.as_ref().map(|c| c.sid.clone()).unwrap_or_default();
            let h = UnitHandle::of(NAME, &format!("{run}-{sid}")).map_err(|e| e.to_string())?;
            Ok((Carry { state, ..carry.clone() }, vec![spec(UNIT_STOP, format!("archivist/{run}/{sid}/stop"), &h, STOP_DEADLINE_MS)]))
        }
        Input::Succeeded { effect, .. } if effect == COMMIT => {
            let state = go("committed")?;
            Ok((Carry { state, ..carry.clone() }, vec![settle(carry, "safe", Some(0)), spec(DIGEST_SEND, format!("archivist/digest/{}", run_of(carry)), &serde_json::json!({}), DIGEST_DEADLINE_MS)]))
        }
        Input::Failed { effect, error } if effect == COMMIT => {
            if archivist::brain::is_dirty_checkout_refusal(error) || error.contains("dirty") {
                Ok((Carry { state: go("dirty")?, ..carry.clone() }, vec![settle(carry, "retry", Some(0))]))
            } else {
                Ok((Carry { state: go("commit-failed")?, ..carry.clone() }, vec![settle(carry, "commit-failed", Some(0))]))
            }
        }
        Input::Succeeded { effect, body } if effect == SETTLE => {
            if s != State::Settling {
                return unexpected(effect);
            }
            if body.trim() == "capacity" {
                return Ok((Carry { state: go("done")?, queue: vec![], current: None, ..carry.clone() }, vec![forget(carry)]));
            }
            let archived = carry.archived + u32::from(body.trim() == "safe");
            next_or_done(carry, archived, config, "next", "done")
        }
        Input::Failed { effect, .. } if effect == SETTLE => next_or_done(carry, carry.archived, config, "next", "done"),
        Input::Succeeded { effect, .. } | Input::Failed { effect, .. } if effect == FORGET => Ok((Carry { state: go("forgotten")?, ..Carry::default() }, vec![])),
        Input::Succeeded { effect, .. } | Input::Failed { effect, .. } => unexpected(effect),
    }
}

/// The latest `persona.archivist.state` on the task's own stream or on `units`.
pub fn carry_of(view: &Projection) -> Carry {
    let latest = |stream: &str| view.events(&StreamKey(stream.into())).iter().rev().find(|e| e.kind.tag == STATE).map(|e| (e.at, e.kind.body.clone()));
    let body = match (latest(STREAM), latest(UNITS_STREAM)) {
        (Some(o), Some(u)) => Some(if u.0 > o.0 { u.1 } else { o.1 }),
        (o, u) => o.or(u).map(|x| x.1),
    };
    body.and_then(|b| serde_json::from_str(&b).ok()).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonaArchivist {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl PersonaArchivist {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(PersonaArchivist { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("persona.archivist/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    fn ours(effect: &str) -> bool {
        effect.starts_with("persona.archivist.") || effect == UNIT_START || effect == UNIT_STOP
    }

    fn input(view: &Projection, ev: &Event) -> Option<Input> {
        let here = ev.stream.0 == STREAM || ev.stream.0 == UNITS_STREAM;
        match ev.kind.tag.as_str() {
            TICK if ev.stream.0 == STREAM => Some(Input::Tick),
            DEADLINE if ev.stream.0 == STREAM => Some(Input::Deadline),
            UNIT_RAN => UnitRan::from_event_kind(&ev.kind).filter(|r| r.key_for(NAME).is_some()).map(Input::UnitRan),
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

impl Handler for PersonaArchivist {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, DEADLINE, UNIT_RAN, EFFECT_SUCCEEDED, EFFECT_FAILED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let Some(input) = Self::input(view, ev) else { return vec![] };
        let persisted = carry_of(view);
        // `Observing` is never journaled: the observe's answer is decided as if it were.
        let carry = match (&input, persisted.state) {
            (Input::Observed(_), State::Idle) => Carry { state: State::Observing, ..persisted.clone() },
            _ => persisted.clone(),
        };
        let mut out = match decide(&carry, &input, &self.config) {
            Ok((mut next, acts)) => {
                if carry.state == State::Observing && next.state == State::Briefing {
                    next.run_id = Some(format!("r{}", ev.at));
                }
                let recorded = if next.state == State::Observing { persisted.clone() } else { next.clone() };
                let (probes, writes): (Vec<EffectSpec>, Vec<EffectSpec>) = acts.into_iter().partition(|a| a.name == OBSERVE);
                let mut out = commands(&persisted, &recorded, STATE, writes.into_iter().map(|a| self.pinned(a)).collect());
                out.extend(probes.into_iter().map(|p| Command::Probe(EffectSpec { input: serde_json::json!({ "cfg": self.config }).to_string(), ..p })));
                if carry.state == State::Starting && next.state == State::Running {
                    let tc = next.current.as_ref().map_or(0, |c| c.tc) as u64;
                    let bound_s = num(&self.config, "SPIRA_ARCHIVIST_TIMEOUT", 900) * (tc + 1) + STOP_GRACE_S;
                    out.push(Command::Timer { at: ev.at + bound_s * 1000, kind: EventKind::new(DEADLINE, "") });
                }
                out
            }
            Err(refusal) => {
                ctx.event().set("archivist.refused", refusal.as_str());
                vec![]
            }
        };
        if input == Input::Tick {
            out.push(Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") });
        }
        out
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

/// The archivist's config from the pinned map, its state directory and its seam.
fn setup(cfg: &BTreeMap<String, String>) -> Result<(archivist::config::Env, PathBuf, archivist::seam::RealSeam), String> {
    let env = archivist::config::from_lookup(&|k| cfg.get(k).cloned().ok_or_else(|| format!("{k} is not pinned")))?;
    let arc = Path::new(&env.run).join("archivist");
    let home = spira_config::resolve::locate_home_for_process()?;
    Ok((env, arc, archivist::seam::RealSeam { lib_sh: home.join("lib.sh") }))
}

fn now_secs() -> i64 {
    archivist::run::now_epoch()
}

/// `persona.archivist.observe`: the capacity pause, every live session past its retry wait
/// with its drift, and every session whose transcript is gone.
pub fn run_observe(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use archivist::seam::Seam;
    #[derive(Deserialize)]
    struct In {
        cfg: BTreeMap<String, String>,
    }
    let cfg = serde_json::from_str::<In>(&s.input).map_err(|e| format!("{NAME}: unreadable input: {e}"))?.cfg;
    let (env, arc, seam) = setup(&cfg)?;
    let paused = seam.capacity_paused().is_some();
    let now = now_secs();
    let candidates = archivist::run::build_candidates(&seam, &env, &arc, now)
        .into_iter()
        .filter_map(|c| {
            let retry_at: Option<i64> = archivist::state::read_state_key(&arc, &c.sid, "retry_at").and_then(|t| t.parse().ok());
            archivist::candidates::would_archive(i64::MAX, 0, c.prev_state.as_deref(), retry_at, now).then(|| Candidate {
                sid: c.sid,
                tp: c.tp,
                ctx: c.ctx.parse().unwrap_or(0),
                turns: c.turns,
                drift: c.drift.max(0) as u64,
                band: c.band.to_string(),
                prev_state: c.prev_state.unwrap_or_default(),
                retry_at: retry_at.map(|t| t.max(0) as u64),
            })
        })
        .collect();
    let stale = archivist::run::stale_sessions(&env, &arc);
    serde_json::to_string(&Snapshot { paused, candidates, stale }).map_err(|e| e.to_string())
}

fn before_file(arc: &Path, sid: &str) -> PathBuf {
    arc.join(format!("{sid}.wiki-before.json"))
}

/// `persona.archivist.brief`: both locks for the brief, the state `sweeping`, the prompt, the
/// two files, the wiki's snapshot (kept beside the session's state for the commit) and the
/// argv: `/bin/sh -c 'exec timeout <secs> <agent> <args…> <task >log 2>&1'`, as
/// `RealSeam::run_agent` ran it.
pub fn run_brief(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let p: Pinned<BriefAsk> = parse(&s.input)?;
    let (env, arc, seam) = setup(&p.cfg)?;
    let ask = p.act;
    let (tp, at, ctx, drift) = match (&ask.tp, ask.at) {
        (Some(tp), Some(at)) => (tp.clone(), at, ask.ctx.unwrap_or(0), ask.drift.unwrap_or(0)),
        _ => {
            let c = archivist::run::build_candidates(&seam, &env, &arc, now_secs()).into_iter().find(|c| c.sid == ask.sid).ok_or_else(|| format!("{}: its transcript is no longer live", ask.sid))?;
            (c.tp.clone(), c.turns, c.ctx.parse().unwrap_or(0), c.drift.max(0) as u64)
        }
    };
    let why = format!("turns since last sweep ({drift}) >= {}", env.every);
    let prepared = archivist::run::prepare(&seam, &env, &arc, &ask.sid, Path::new(&tp), at, &ctx.to_string(), &why, false).map_err(|rc| format!("{}: the brief was refused (rc {rc})", ask.sid))?;
    let before = match &prepared.before {
        Some(Ok(snap)) => serde_json::to_string(snap).map_err(|e| e.to_string())?,
        Some(Err(e)) => serde_json::json!({ "error": e }).to_string(),
        None => String::new(),
    };
    let wiki_before = before_file(&arc, &ask.sid);
    if before.is_empty() {
        let _ = std::fs::remove_file(&wiki_before);
    } else {
        std::fs::write(&wiki_before, before).map_err(|e| format!("{}: {e}", wiki_before.display()))?;
    }
    let mut argv = vec!["/bin/sh".to_string(), "-c".to_string(), "exec timeout \"$SPIRA_ARCHIVIST_SECS\" \"$0\" \"$@\" <\"$SPIRA_ARCHIVIST_TASK\" >\"$SPIRA_ARCHIVIST_LOG\" 2>&1".to_string(), env.agent.clone()];
    argv.extend(archivist::run::agent_args(&prepared.sysfile, &env.model, env.wiki.as_deref()));
    let mut unit_env = vec![
        ("SPIRA_MAIL_FROM".to_string(), archivist::run::MAIL_FROM.to_string()),
        ("SPIRA_WORK_ACTOR".to_string(), "archivist".to_string()),
        ("SPIRA_ARCHIVIST_SECS".to_string(), prepared.timeout_secs.to_string()),
        ("SPIRA_ARCHIVIST_TASK".to_string(), prepared.taskfile.display().to_string()),
        ("SPIRA_ARCHIVIST_LOG".to_string(), prepared.logf.display().to_string()),
    ];
    if let Some(w) = env.wiki.as_deref() {
        unit_env.push(("SPIRA_WIKI".to_string(), w.to_string()));
    }
    let b = Briefed {
        sysfile: prepared.sysfile.display().to_string(),
        taskfile: prepared.taskfile.display().to_string(),
        wiki_before: wiki_before.display().to_string(),
        argv,
        env: unit_env,
        cwd: Some(arc.join("cwd")),
        at: Some(at),
        tc: Some(prepared.timeout_count),
    };
    serde_json::to_string(&b).map_err(|e| e.to_string())
}

/// `persona.archivist.commit`: the pages the session touched, committed and pushed on the wiki.
pub fn run_commit(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        sid: String,
    }
    let p: Pinned<Ask> = parse(&s.input)?;
    let (env, arc, _) = setup(&p.cfg)?;
    let file = before_file(&arc, &p.act.sid);
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: no wiki snapshot from the brief: {e}", file.display()))?;
    let before: Result<archivist::brain::Snapshot, String> = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) if v.get("error").is_some() => Err(v["error"].as_str().unwrap_or_default().to_string()),
        Ok(v) => serde_json::from_value(v).map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    let committed = archivist::run::commit_after(&env, &p.act.sid, Some(before));
    let _ = std::fs::remove_file(&file);
    match committed {
        None => Err("no wiki is configured".into()),
        Some(Ok(n)) => Ok(format!("{n} page(s)")),
        Some(Err(e)) => Err(e),
    }
}

/// `persona.archivist.settle`: the session's state files after its run. Answers `safe`,
/// `capacity` (the account refused the session; the rest of the pass is deferred), `timeout`
/// or `failed`.
pub fn run_settle(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use archivist::run::Outcome;
    let p: Pinned<Settle> = parse(&s.input)?;
    let (env, arc, seam) = setup(&p.cfg)?;
    let a = p.act;
    let outcome = match a.outcome.as_str() {
        "safe" => Outcome::Safe,
        "retry" => Outcome::Retry,
        "commit-failed" => Outcome::CommitFailed,
        "timeout" => Outcome::Timeout,
        _ => Outcome::Ran(a.rc.filter(|rc| *rc != 0 && *rc != 124).unwrap_or(1)),
    };
    let logf = Path::new(&env.run).join(format!("archivist-{}.log", a.sid));
    let rc = archivist::run::settle(&seam, &env, &arc, &a.sid, a.at, outcome, &logf, false);
    Ok(match rc {
        0 => "safe",
        124 => "timeout",
        rc if rc == archivist::state::ARC_RC_CAPACITY => "capacity",
        _ => "failed",
    }
    .to_string())
}

/// `persona.archivist.digest_send`: today's digest, at most once a day.
pub fn run_digest_send(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let p: Pinned<serde_json::Value> = parse(&s.input)?;
    let (env, arc, seam) = setup(&p.cfg)?;
    archivist::run::digest_send(&seam, &arc, &env.tz).map(|()| "digest".to_string())
}

/// `persona.archivist.mark`: `sweep.state` says this pass was skipped for capacity.
pub fn run_mark(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let p: Pinned<serde_json::Value> = parse(&s.input)?;
    let (_, arc, _) = setup(&p.cfg)?;
    archivist::run::mark_skipped(&arc);
    Ok("skipped".into())
}

/// `persona.archivist.forget`: every stale session's bookkeeping and log, removed.
pub fn run_forget(_: &Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    let p: Pinned<serde_json::Value> = parse(&s.input)?;
    let (env, arc, _) = setup(&p.cfg)?;
    let mut lines = vec![];
    let _ = std::fs::remove_file(arc.join("sweep.state"));
    archivist::run::forget_stale_sessions(&env, &arc, &mut lines);
    Ok(format!("forgot {}", lines.len()))
}
