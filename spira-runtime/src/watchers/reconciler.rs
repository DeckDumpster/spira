//! The structural pass that was `reconciler --pass` on a timer. Each tick asks the world for one
//! observation (the invariants' checks, the suspensions due a reminder, the hold sweep);
//! `reconciler::decide` steps every check through the shared hysteresis and says what to do;
//! remedies, mail and telemetry leave as effects. What the pass remembers is journaled only when
//! it changes, so a tick that finds everything satisfied writes nothing.

use std::collections::BTreeMap;

use reconciler::{checks_of, decide, judge_holds, prune, Act, Book, EventRow, Mail, Observation, Rules};
use reconciler_engine::io::StatusRecord;
use runtime::event::*;
use serde::{Deserialize, Serialize};

pub const STREAM: &str = "watch.reconciler";
pub const TICK: EventKindTag = "reconciler.tick";
pub const STATE: EventKindTag = "reconciler.state";
pub const OBSERVE: &str = "reconciler.observe";
pub const RECORD: &str = "reconciler.record";
pub const REMEDY: &str = "reconciler.remedy";
pub const MAIL: &str = "reconciler.mail";
pub const NAME: &str = "reconciler";

/// A pass runs the fences of a whole checkout of the landing ref; the old unit allowed it five minutes.
pub const OBSERVE_DEADLINE_MS: u64 = 280_000;
const EFFECT_DEADLINE_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reconciler {
    pub interval_ms: u64,
    pub rules: Rules,
    /// The registered keys the observers read, as they stood when the service started.
    pub values: BTreeMap<String, String>,
    /// Where the harness is installed: a fact about the process, not a key.
    pub home: String,
}

/// What the observe effect is asked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveIn {
    pub at: u64,
    pub reminded: std::collections::BTreeSet<String>,
}

/// Telemetry for the metrics store: a status row per check every pass, and the remedy events.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub at: u64,
    pub status: Vec<StatusRecord>,
    pub events: Vec<EventRow>,
    pub log: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemedyJob {
    pub key: String,
    pub steps: Vec<(String, Vec<String>)>,
}

/// One mail, and what to forget if it does not go: the escalation, or the reminder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailJob {
    pub mail: Mail,
    pub unalert: Option<String>,
    pub remind: Option<String>,
    pub what: String,
}

fn spec<T: Serialize>(name: &str, input: &T, deadline_ms: u64) -> EffectSpec {
    EffectSpec { name: name.into(), input: serde_json::to_string(input).unwrap_or_default(), deadline_ms }
}

impl Reconciler {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("reconciler/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn book(view: &Projection, stream: &StreamKey) -> Book {
        super::carried(view, stream, STATE)
    }

    fn state(book: &Book) -> Command {
        Command::Emit(EventKind::new(STATE, &serde_json::to_string(book).unwrap_or_default()))
    }

    fn apply(&self, ctx: &Ctx, view: &Projection, ev: &Event, obs: Observation) -> Vec<Command> {
        if obs.halted {
            return vec![];
        }
        let carried = Self::book(view, &ev.stream);
        let mut book = carried.clone();
        let rules = Rules { now_secs: obs.now_secs, ..self.rules.clone() };
        prune(&mut book.state, &obs.groups);
        let checks = checks_of(obs.groups);
        ctx.event().set("reconciler.checks", checks.len() as u64);
        let mut acts: Vec<Act> = checks.into_iter().flat_map(|c| decide(&rules, &mut book.state, &mut book.pending, &mut book.alerted, c)).collect();
        acts.extend(judge_holds(&rules, &mut book.pending, &obs.holds));

        book.reminded.retain(|m| obs.reminders.live.contains(m));
        let mut mails: Vec<MailJob> = vec![];
        for r in obs.reminders.due {
            book.reminded.insert(r.marker.clone());
            mails.push(MailJob { mail: r.mail, unalert: None, remind: Some(r.marker), what: format!("reminded — {}: {}", r.subject, r.why) });
        }

        let mut record = Record { at: ev.at, ..Record::default() };
        let mut cmds = vec![];
        let mut remedies = vec![];
        for act in acts {
            match act {
                Act::Status(row) => record.status.push(row),
                Act::Event(e) => record.events.push(e),
                Act::Log(line) => record.log.push(line),
                Act::Remedy { key, steps } => remedies.push(RemedyJob { key, steps }),
                Act::Mail { mail, unalert, what } => mails.push(MailJob { mail, unalert, remind: None, what }),
            }
        }
        if book != carried {
            cmds.push(Self::state(&book));
        }
        ctx.event().set("reconciler.acted", (remedies.len() + mails.len()) as u64);
        if !record.events.is_empty() || !record.log.is_empty() {
            let journaled = Record { status: vec![], ..record.clone() };
            cmds.push(Command::Effect(spec(RECORD, &journaled, EFFECT_DEADLINE_MS)));
        }
        if !record.status.is_empty() {
            let rows = Record { events: vec![], log: vec![], ..record };
            cmds.push(Command::Probe(spec(RECORD, &rows, EFFECT_DEADLINE_MS)));
        }
        cmds.extend(remedies.iter().map(|r| Command::Effect(spec(REMEDY, r, OBSERVE_DEADLINE_MS))));
        cmds.extend(mails.iter().map(|m| Command::Effect(spec(MAIL, m, EFFECT_DEADLINE_MS))));
        cmds
    }

    /// A mail that did not go: forget that it was sent, so the next pass sends it again.
    fn unsent(&self, view: &Projection, ev: &Event) -> Vec<Command> {
        let Some(job) = super::answered_spec(view, ev).and_then(|s| serde_json::from_str::<MailJob>(&s.input).ok()) else { return vec![] };
        let carried = Self::book(view, &ev.stream);
        let mut book = carried.clone();
        if let Some(key) = &job.unalert {
            book.alerted.remove(key);
        }
        if let Some(marker) = &job.remind {
            book.reminded.remove(marker);
        }
        if book == carried { vec![] } else { vec![Self::state(&book)] }
    }
}

impl Handler for Reconciler {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let input = ObserveIn { at: ev.at, reminded: Self::book(view, &ev.stream).reminded };
            return vec![
                Command::Probe(spec(OBSERVE, &input, OBSERVE_DEADLINE_MS)),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        match (super::answered_effect(view, ev).as_deref(), ev.kind.tag.as_str()) {
            (Some(OBSERVE), EFFECT_SUCCEEDED) => match serde_json::from_str::<Observation>(&ev.kind.body) {
                Ok(obs) => self.apply(ctx, view, ev, obs),
                Err(_) => vec![],
            },
            (Some(MAIL), EFFECT_FAILED | EFFECT_TIMED_OUT) => self.unsent(view, ev),
            _ => vec![],
        }
    }
}
