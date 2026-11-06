//! `asks.ideas`: when Spira has been idle, ask the operator whether to start a parked idea.
//! Replaces `spira-idea-prompt-prod` (`bin/idea-prompt pass --state run/idea-prompt.state
//! --idle-secs 1800 --wake-from 7 --wake-until 22 --tz America/Los_Angeles --min-gap-secs 86400`).
//! Pattern P1. Contract: `wiki/projects/spira/designs/tasks/asks-ideas.md`.
//!
//! # Tick
//!
//! Every 5 min (`SPIRA_RUNTIME_ASKS_IDEAS_SECS`), single-flight. No `world guard`: a halted world
//! has no busy rows, so the prompt can fire during a halt, as today.
//!
//! # Observe
//!
//! `asks.ideas.observe` returns a [`Snapshot`]: `busy` (any unheld row in READY, REWORK, WORKING,
//! SUBMITTED, CERTIFIED or IN_DELIVERY, from the S7b projection; the code is followed, not the
//! lib's doc comment), `ideas` (the `content` verb's idea query, newest first, read only when the
//! carry says a prompt is possible) and `local_hour` (computed in-process from `now` and the
//! pinned timezone).
//!
//! # Decide
//!
//! The pure core is `idea_prompt::pass` with its ports as snapshot fields, plus
//! `in_waking_hours` and `render_body`. Outcomes, in order: [`Outcome`]. The carry is re-keyed to
//! change only on a transition, `{ idle_since, last_prompt, prompted }`: `idle_since` is set on
//! the first idle tick and cleared on the first busy one, so `now - last_busy` becomes
//! `now - idle_since` to within one tick. Fixtures port `idea-prompt/src/tests.rs` (6 cases) and
//! `spira/test-idea-prompt-units.sh`.
//!
//! # Act
//!
//! | Effect | Class | Deadline | Idempotency key | Refuses |
//! |---|---|---|---|---|
//! | `asks.ideas.observe` (probe) | store | 30 s | tick id | — |
//! | `asks.ideas.ask` | store (operator mail) | 300 s | `ideas/<idle_since>` | an empty idea list; a carry already `prompted` |
//!
//! The ask is the one `mail send operator` sends today: from `Spira Ideas <ideas@spira>`, subject
//! `<n> parked idea(s) — start any?`, `--kind question --class design --default "not now"`, and
//! the numbered body. The runtime has no question door yet (S2 needs a `--kind question` runner).
//! `asks.ideas.state` is emitted only after the ask succeeds; a failed send leaves the carry and
//! the next tick retries.
//!
//! # Silence
//!
//! A tick that leaves the idle spell unchanged emits the probe only; the task emits once at each
//! busy→idle and idle→busy edge.
//!
//! # Replaces
//!
//! `spira-idea-prompt-prod.service` and `.timer`; children `spira-lc list`, `bead.sh ideas`,
//! `mail send operator`, `date`. The unit's flags become the config keys in [`KEYS`];
//! `IDEA_PROMPT_BEAD` and `IDEA_PROMPT_MAIL` retire (the sim double takes their place).
//!
//! # Reverse
//!
//! Release rollback restores the unit; the carry renders `last_busy=<idle_since>`, `last_prompt`
//! and `prompted` in the old state-file format (`render_state`). Observe mode shadows the ask.

use std::collections::BTreeMap;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::trigger_groom::walk;
use super::{commands, Cycle, Transition};
use crate::watchers::carried;

pub const NAME: &str = "asks.ideas";
pub const STREAM: &str = "task.asks.ideas";
pub const TICK: EventKindTag = "asks.ideas.tick";
pub const STATE: EventKindTag = "asks.ideas.state";

pub const OBSERVE: &str = "asks.ideas.observe";
pub const ASK: &str = "asks.ideas.ask";
pub const EFFECTS: &[&str] = &[OBSERVE, ASK];

pub const CADENCE_KEY: &str = "SPIRA_RUNTIME_ASKS_IDEAS_SECS";
pub const KEYS: &[&str] = &[CADENCE_KEY, "SPIRA_IDEAS_IDLE_SECS", "SPIRA_IDEAS_WAKE_FROM", "SPIRA_IDEAS_WAKE_UNTIL", "SPIRA_IDEAS_TZ", "SPIRA_IDEAS_MIN_GAP_SECS"];

const DEADLINE_MS: u64 = 30_000;
const ASK_DEADLINE_MS: u64 = 300_000;
const CYCLE: Cycle = Cycle { stream: STREAM, tick: TICK, observe: OBSERVE, deadline_ms: DEADLINE_MS };

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Idea {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub now: u64,
    pub local_hour: u32,
    pub busy: Result<bool, String>,
    pub ideas: Result<Vec<Idea>, String>,
}

/// `asks.ideas.state`: changes only on a busy/idle edge or a prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub idle_since: Option<u64>,
    pub last_prompt: Option<u64>,
    pub prompted: bool,
}

/// `idea_prompt::Outcome`, in its order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Busy,
    NotIdleLongEnough,
    AlreadyPrompted,
    TooSoon,
    OutsideWakingHours,
    NoIdeas,
    Prompted(usize),
}

/// The idle spell the world is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spell {
    Busy,
    Idle,
    Prompted,
}

pub const TRANSITIONS: &[Transition<Spell>] = &[
    Transition { from: Spell::Busy, on: "idle", to: Spell::Idle, effects: &[] },
    Transition { from: Spell::Idle, on: "busy", to: Spell::Busy, effects: &[] },
    Transition { from: Spell::Idle, on: "prompt", to: Spell::Prompted, effects: &[ASK] },
    Transition { from: Spell::Prompted, on: "busy", to: Spell::Busy, effects: &[] },
];

/// The spell's next state on `on`, or a refusal naming the state it is in.
pub fn step(from: Spell, on: &str) -> Result<Spell, String> {
    walk(TRANSITIONS, from, on)
}

/// `idea_prompt::pass`'s config from the pinned [`KEYS`]; `None` when one does not parse.
fn prompt_config(config: &BTreeMap<String, String>) -> Option<idea_prompt::Config> {
    let n = |k: &str| config.get(k).and_then(|v| v.trim().parse::<u64>().ok());
    Some(idea_prompt::Config { idle_secs: n("SPIRA_IDEAS_IDLE_SECS")?, wake_from: n("SPIRA_IDEAS_WAKE_FROM")? as u32, wake_until: n("SPIRA_IDEAS_WAKE_UNTIL")? as u32, min_gap_secs: n("SPIRA_IDEAS_MIN_GAP_SECS")? })
}

/// The ask `asks.ideas.ask` sends: `mail send operator --kind question --class design`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub subject: String,
    pub default: String,
    pub body: String,
    pub tz: String,
}

/// `idea_prompt::pass`'s ports, answered from one snapshot; the send is recorded, not made.
struct Seen<'a> {
    snap: &'a Snapshot,
    sent: std::cell::RefCell<Option<(String, String, String)>>,
}

impl idea_prompt::Ports for Seen<'_> {
    fn now(&self) -> u64 {
        self.snap.now
    }

    fn local_hour(&self, _now: u64) -> Result<u32, String> {
        Ok(self.snap.local_hour)
    }

    fn busy(&self) -> Result<bool, String> {
        self.snap.busy.clone()
    }

    fn ideas(&self) -> Result<Vec<idea_prompt::Idea>, String> {
        self.snap.ideas.clone().map(|v| v.into_iter().map(|i| idea_prompt::Idea { id: i.id, title: i.title }).collect())
    }

    fn send(&self, subject: &str, default: &str, body: &str) -> Result<(), String> {
        *self.sent.borrow_mut() = Some((subject.into(), default.into(), body.into()));
        Ok(())
    }
}

fn outcome_of(o: idea_prompt::Outcome) -> Outcome {
    match o {
        idea_prompt::Outcome::Busy => Outcome::Busy,
        idea_prompt::Outcome::NotIdleLongEnough => Outcome::NotIdleLongEnough,
        idea_prompt::Outcome::AlreadyPrompted => Outcome::AlreadyPrompted,
        idea_prompt::Outcome::TooSoon => Outcome::TooSoon,
        idea_prompt::Outcome::OutsideWakingHours => Outcome::OutsideWakingHours,
        idea_prompt::Outcome::NoIdeas => Outcome::NoIdeas,
        idea_prompt::Outcome::Prompted(n) => Outcome::Prompted(n),
    }
}

/// The pure decision, with `config` the pinned [`KEYS`]: the next carry, the outcome, and the acts.
/// `idea_prompt::pass` runs over the snapshot with the carry as its state; the carry keeps
/// `idle_since` (the old `last_busy`) only while idle, so a busy tick leaves it unchanged. A read
/// the snapshot could not make, or a key that does not parse, prompts nothing and keeps the carry.
pub fn decide(carry: &Carry, snap: &Snapshot, config: &BTreeMap<String, String>) -> (Carry, Outcome, Vec<EffectSpec>) {
    let Some(cfg) = prompt_config(config) else { return (carry.clone(), Outcome::Busy, vec![]) };
    let mut state = idea_prompt::State { last_busy: carry.idle_since.unwrap_or(0), last_prompt: carry.last_prompt.unwrap_or(0), prompted: carry.prompted };
    let ports = Seen { snap, sent: std::cell::RefCell::new(None) };
    let Ok(outcome) = idea_prompt::pass(&ports, &cfg, &mut state) else { return (carry.clone(), Outcome::Busy, vec![]) };
    let outcome = outcome_of(outcome);
    let next = match outcome {
        Outcome::Busy => Carry { idle_since: None, last_prompt: carry.last_prompt, prompted: false },
        _ => Carry { idle_since: Some(state.last_busy), last_prompt: (state.last_prompt != 0).then_some(state.last_prompt), prompted: state.prompted },
    };
    let acts = ports
        .sent
        .into_inner()
        .map(|(subject, default, body)| EffectSpec { name: ASK.into(), input: serde_json::to_string(&Ask { subject, default, body, tz: config.get("SPIRA_IDEAS_TZ").cloned().unwrap_or_default() }).unwrap_or_default(), deadline_ms: ASK_DEADLINE_MS })
        .into_iter()
        .collect();
    (next, outcome, acts)
}

/// What the observe is handed: the pinned config, and whether the carry leaves a prompt
/// possible (only then are the ideas and the local hour read).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveInput {
    pub config: BTreeMap<String, String>,
    pub may_prompt: bool,
}

/// `asks.ideas.observe`: the busy read over the store's door; the ideas and local hour when a
/// prompt is possible.
pub fn run_observe(e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use idea_prompt::Ports;
    let input: ObserveInput = serde_json::from_str(&s.input).map_err(|e| format!("observe input: {e}"))?;
    let tz = input.config.get("SPIRA_IDEAS_TZ").cloned().ok_or("SPIRA_IDEAS_TZ is not pinned")?;
    let lc = spira_config::lc_call::LcClient::socket(&e.world.lc_socket);
    let real = idea_prompt::real::Real::with(tz, Ok(lc), Box::new(|_, _| Err("the observe sends nothing".into())));
    let now = spira_config::vtime::now_epoch();
    let (local_hour, ideas) = if input.may_prompt {
        let ideas = real.ideas().map(|v| v.into_iter().map(|i| Idea { id: i.id, title: i.title }).collect());
        (real.local_hour(now).unwrap_or(0), ideas)
    } else {
        (0, Ok(vec![]))
    };
    serde_json::to_string(&Snapshot { now, local_hour, busy: real.busy(), ideas }).map_err(|e| e.to_string())
}

/// `asks.ideas.ask`: the operator question, through the `mail` library.
pub fn run_ask(_e: &crate::Effects, _key: &str, s: &EffectSpec) -> Result<String, String> {
    use idea_prompt::Ports;
    let ask: Ask = serde_json::from_str(&s.input).map_err(|e| format!("ask input: {e}"))?;
    idea_prompt::real::Real::new(ask.tz).send(&ask.subject, &ask.default, &ask.body).map(|()| ask.subject)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AsksIdeas {
    pub interval_ms: u64,
    pub config: BTreeMap<String, String>,
}

impl AsksIdeas {
    pub fn from_settings(settings: &crate::Settings) -> Result<Self, String> {
        Ok(AsksIdeas { interval_ms: super::cadence_ms(settings, CADENCE_KEY)?, config: super::pinned(settings, KEYS)? })
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("asks.ideas/boot", STREAM, EventKind::new(TICK, ""))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }
}

impl Handler for AsksIdeas {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        let may_prompt = || {
            let c = carried::<Carry>(view, &ev.stream, STATE);
            serde_json::to_string(&ObserveInput { config: self.config.clone(), may_prompt: c.idle_since.is_some() && !c.prompted }).unwrap_or_default()
        };
        CYCLE.handle(self.interval_ms, view, ev, may_prompt, |body| {
            let Ok(snap) = serde_json::from_str::<Snapshot>(body) else { return vec![] };
            let carry = carried::<Carry>(view, &ev.stream, STATE);
            let (next, outcome, acts) = decide(&carry, &snap, &self.config);
            ctx.event().set("ideas.outcome", format!("{outcome:?}").as_str());
            commands(&carry, &next, STATE, acts)
        })
    }
}
