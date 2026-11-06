//! The contract: events, commands, handlers.

use serde::{Deserialize, Serialize};

use crate::ports::Rng;
use crate::ports::SeededRng;
use crate::wide::Wide;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StreamKey(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CorrelationId(pub String);

/// The widest id the journal stores (`runtime_event.event_id`).
pub const EVENT_ID_MAX: usize = 255;

/// One short of the column, so no derived id is exactly `EVENT_ID_MAX` chars: that width
/// marks a row the store truncated (migrations/0032).
const KEPT: usize = EVENT_ID_MAX - 18;

/// FNV-1a over `plain`, the digest `derive_id` appends.
pub fn fnv1a(plain: &str) -> u64 {
    plain.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// The id scheme's definition: an id that fits is itself, a longer one is its first 237
/// chars, `#`, and 16 hex digits of the hash of the whole, so a chain of derivations stays
/// fixed-width and an id already written derives to itself.
pub fn derive_id(plain: String) -> String {
    if plain.chars().count() <= EVENT_ID_MAX {
        return plain;
    }
    let head: String = plain.chars().take(KEPT).collect();
    format!("{head}#{:016x}", fnv1a(&plain))
}

/// Whether `id` is one a handler wrote while handling `parent`: an id derived from the parent's
/// (`{parent}/{handler}/{i}`, kept to its first `KEPT` chars when long) and not its outcome's.
pub(crate) fn written_by_handler_of(parent: &EventId, id: &EventId) -> bool {
    let p = &parent.0;
    let head = if p.chars().count() < KEPT { format!("{p}/") } else { p.chars().take(KEPT).collect() };
    id.0.starts_with(&head) && id.0 != derive_id(format!("{p}/outcome"))
}

pub type EventKindTag = &'static str;

pub const EFFECT_REQUESTED: EventKindTag = "effect.requested";
pub const EFFECT_SUCCEEDED: EventKindTag = "effect.succeeded";
pub const EFFECT_FAILED: EventKindTag = "effect.failed";
pub const EFFECT_TIMED_OUT: EventKindTag = "effect.timed_out";
/// A write an `Observe` task asked for and the engine recorded instead of running. It is never
/// an open effect: no restart, and no later switch to `Act`, runs it.
pub const EFFECT_SHADOWED: EventKindTag = "effect.shadowed";
/// A cross-stream emit an `Observe` task asked for, recorded on its own stream instead of
/// landing on the target; its body is a [`ShadowedEmit`]. Nothing ever delivers it.
pub const EMIT_SHADOWED: EventKindTag = "emit.shadowed";
/// A write an `Act` task asked for while the scheduler was halted; resuming requests it.
pub const EFFECT_HELD: EventKindTag = "effect.held";

/// The stream that carries the scheduler's halt state and the writes held under it.
pub const HALT_STREAM: &str = "scheduler.halt";
pub const SCHEDULER_HALTED: EventKindTag = "scheduler.halted";
pub const SCHEDULER_RESUMED: EventKindTag = "scheduler.resumed";

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

/// The body of an [`EMIT_SHADOWED`] event: where the emit would have gone, and what.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowedEmit {
    pub stream: StreamKey,
    pub kind: EventKind,
}

/// `Emit`, `EmitTo`, `Effect` and `Schedule` are journaled and survive a restart. `Note`, `Probe` and
/// `Timer` live in the engine's memory only: a restart forgets them, so they are for work the
/// next poll repeats anyway, and a handler that must not lose a fact emits it.
pub enum Command {
    Emit(EventKind),
    /// An event on another stream: how one task wakes another. Its id is the causing event's,
    /// the handler, the command's index and the target stream, so a replayed handler appends
    /// nothing twice. A halt does not hold it (it is not an effect); under `Observe` it is
    /// journaled as [`EMIT_SHADOWED`] on the handler's own stream and never delivered.
    EmitTo { stream: StreamKey, kind: EventKind },
    Effect(EffectSpec),
    Schedule { at: u64, kind: EventKind },
    Note(EventKind),
    Probe(EffectSpec),
    /// One pending timer per (stream, tag): arming it again replaces the one waiting.
    Timer { at: u64, kind: EventKind },
}

/// What a handler may read besides the event: time and randomness, both pinned to the
/// (event, handler) pair so a redelivery or a replay sees the same values.
pub struct Ctx {
    now: u64,
    offset: u64,
    rng: std::cell::RefCell<SeededRng>,
    wide: Wide,
}

impl Ctx {
    pub fn new(now: u64, seed: u64) -> Self {
        Self::with(now, seed, 0, Wide::default())
    }

    pub fn with(now: u64, seed: u64, offset: u64, wide: Wide) -> Self {
        Ctx { now, offset, rng: std::cell::RefCell::new(SeededRng::new(seed)), wide }
    }

    /// The journal offset of the event being handled.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// The wide event of this unit of work: add a field with `set`, nothing else.
    pub fn event(&self) -> &Wide {
        &self.wide
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
    pushed: std::collections::BTreeMap<StreamKey, usize>,
    local: std::collections::BTreeSet<EventId>,
}

/// Events of one stream a handler is guaranteed to see: the newest `WINDOW`, however long the
/// stream. The view holds up to twice that, trimmed in one move so a push stays cheap.
pub const WINDOW: usize = 1000;

impl Projection {
    pub fn push(&mut self, ev: &Event) {
        *self.pushed.entry(ev.stream.clone()).or_default() += 1;
        let seen = self.streams.entry(ev.stream.clone()).or_default();
        seen.push(ev.clone());
        if seen.len() >= 2 * WINDOW {
            for gone in seen.drain(..seen.len() - WINDOW) {
                self.local.remove(&gone.id);
            }
        }
    }

    /// Push an event the journal does not hold, then drop the stream's oldest such events past
    /// `keep`, except the newest of each kind (a handler's carried state) and those `protect` names.
    pub fn push_local(&mut self, ev: &Event, keep: usize, protect: impl Fn(&EventId) -> bool) {
        self.push(ev);
        self.local.insert(ev.id.clone());
        let held = self.streams.entry(ev.stream.clone()).or_default();
        let mut over = held.iter().filter(|e| self.local.contains(&e.id)).count().saturating_sub(keep);
        if over == 0 {
            return;
        }
        let mut newest = std::collections::BTreeSet::new();
        let latest: std::collections::BTreeSet<EventId> = held.iter().rev().filter(|e| self.local.contains(&e.id) && newest.insert(e.kind.tag.clone())).map(|e| e.id.clone()).collect();
        let local = &mut self.local;
        held.retain(|e| {
            if over == 0 || !local.contains(&e.id) || latest.contains(&e.id) || protect(&e.id) {
                return true;
            }
            over -= 1;
            local.remove(&e.id);
            false
        });
    }

    pub fn events(&self, stream: &StreamKey) -> &[Event] {
        self.streams.get(stream).map_or(&[], |v| v.as_slice())
    }

    /// Events pushed over all streams; trimming a stream never lowers it.
    pub fn total(&self) -> usize {
        self.pushed.values().sum()
    }

    /// Events pushed on one stream; trimming never lowers it.
    pub fn pushed(&self, stream: &StreamKey) -> usize {
        self.pushed.get(stream).copied().unwrap_or(0)
    }

    pub fn count(&self, stream: &StreamKey, tag: &str) -> usize {
        self.events(stream).iter().filter(|e| e.kind.tag == tag).count()
    }
}

pub trait Handler: Send + Sync {
    fn interests(&self) -> &[EventKindTag];
    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command>;
}

/// `Observe` runs a task's probes and records its writes as `effect.shadowed` without running
/// them; `Act` runs both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Observe,
    Act,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Observe => "observe",
            Mode::Act => "act",
        }
    }
}

pub struct Registration {
    pub name: String,
    pub handler: Box<dyn Handler>,
    pub mode: Mode,
}

impl Registration {
    /// In `Act`; a service takes the mode of every task from its configured table.
    pub fn new(name: &str, handler: impl Handler + 'static) -> Self {
        Registration { name: name.into(), handler: Box::new(handler), mode: Mode::Act }
    }

    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }
}
