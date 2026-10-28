//! The contract: events, commands, handlers.

use serde::{Deserialize, Serialize};

use crate::ports::Rng;
use crate::ports::SeededRng;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StreamKey(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CorrelationId(pub String);

pub type EventKindTag = &'static str;

pub const EFFECT_REQUESTED: EventKindTag = "effect.requested";
pub const EFFECT_SUCCEEDED: EventKindTag = "effect.succeeded";
pub const EFFECT_FAILED: EventKindTag = "effect.failed";
pub const EFFECT_TIMED_OUT: EventKindTag = "effect.timed_out";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventKind {
    pub tag: String,
    pub body: String,
}

impl EventKind {
    pub fn new(tag: &str, body: &str) -> Self {
        EventKind { tag: tag.into(), body: body.into() }
    }
}

/// An append-only fact owned by one stream. `at` is event time in milliseconds: a handler
/// that reads it on replay reads the same value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub stream: StreamKey,
    pub kind: EventKind,
    pub at: u64,
    pub cause: Option<EventId>,
    pub correlation: CorrelationId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectSpec {
    pub name: String,
    pub input: String,
    pub deadline_ms: u64,
}

pub enum Command {
    Emit(EventKind),
    Effect(EffectSpec),
    Schedule { at: u64, kind: EventKind },
}

/// What a handler may read besides the event: time and randomness, both pinned to the
/// (event, handler) pair so a redelivery or a replay sees the same values.
pub struct Ctx {
    now: u64,
    rng: std::cell::RefCell<SeededRng>,
}

impl Ctx {
    pub fn new(now: u64, seed: u64) -> Self {
        Ctx { now, rng: std::cell::RefCell::new(SeededRng::new(seed)) }
    }

    pub fn now(&self) -> u64 {
        self.now
    }

    pub fn rand_u64(&self) -> u64 {
        self.rng.borrow_mut().next_u64()
    }
}

/// Every event the handlers have seen, per stream, in journal order — up to and including
/// the one being handled.
#[derive(Default, Clone, Debug)]
pub struct Projection {
    streams: std::collections::BTreeMap<StreamKey, Vec<Event>>,
}

impl Projection {
    pub fn push(&mut self, ev: &Event) {
        self.streams.entry(ev.stream.clone()).or_default().push(ev.clone());
    }

    pub fn events(&self, stream: &StreamKey) -> &[Event] {
        self.streams.get(stream).map_or(&[], |v| v.as_slice())
    }

    pub fn count(&self, stream: &StreamKey, tag: &str) -> usize {
        self.events(stream).iter().filter(|e| e.kind.tag == tag).count()
    }
}

pub trait Handler: Send + Sync {
    fn interests(&self) -> &[EventKindTag];
    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command>;
}

pub struct Registration {
    pub name: String,
    pub handler: Box<dyn Handler>,
}

impl Registration {
    pub fn new(name: &str, handler: impl Handler + 'static) -> Self {
        Registration { name: name.into(), handler: Box::new(handler) }
    }
}
