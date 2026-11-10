//! Declared operator overrides: each tick runs `overrides.sh apply`, which holds every
//! unlanded override in place and retires the ones whose bead reached the landing ref. A pass
//! that fails is said to the Concierge once per distinct failure, not once per tick.

use runtime::event::*;
use runtime::rows::Part;
use serde::{Deserialize, Serialize};

use super::{answered_effect, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.overrides";
pub const TICK: EventKindTag = "overrides.tick";
pub const STATE: EventKindTag = "overrides.state";
pub const APPLY: &str = "overrides.apply";
pub const NAME: &str = "overrides";

const ROW_PARTS: &[Part] = &[Part::Field("said")];

const DEADLINE_MS: u64 = 120_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overrides {
    pub interval_ms: u64,
}

/// The failure already said; empty while passes succeed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub said: String,
}

impl Overrides {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("overrides/boot", STREAM, EventKind::new(TICK, ""))
    }
}

pub fn decide(carry: &Carry, failure: Option<&str>) -> (Carry, Option<String>) {
    match failure {
        None => (Carry::default(), None),
        Some(f) if f == carry.said => (carry.clone(), None),
        Some(f) => (Carry { said: f.to_string() }, Some(format!("overrides: apply pass failed: {f}"))),
    }
}

impl Handler for Overrides {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return vec![
                Command::Effect(EffectSpec::keyed(APPLY, format!("{NAME}:apply:{}", ev.at), String::new(), DEADLINE_MS)),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if answered_effect(view, ev).as_deref() != Some(APPLY) {
            return vec![];
        }
        let failure = if ev.kind.tag == EFFECT_SUCCEEDED {
            None
        } else if ev.kind.tag == EFFECT_FAILED {
            Some(ev.kind.body.trim().to_string())
        } else {
            Some("timed out".to_string())
        };
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, say) = decide(&carry, failure.as_deref());
        ctx.event().set("overrides.failed", u64::from(failure.is_some()));
        let mut cmds = vec![];
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        if let Some(m) = say {
            cmds.push(Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:inbox:{}:{:016x}", ev.at / 1000, runtime::event::fnv1a(&m)), format!("{} [watch:{NAME}] {m}", iso8601(ev.at / 1000)), 10_000)));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_is_said_once_until_it_changes_or_clears() {
        let (c, say) = decide(&Carry::default(), Some("bead sp-x is not in the store"));
        assert!(say.is_some_and(|m| m.contains("sp-x")));
        assert!(decide(&c, Some("bead sp-x is not in the store")).1.is_none());
        assert!(decide(&c, Some("another")).1.is_some());
        let (cleared, none) = decide(&c, None);
        assert_eq!((cleared.clone(), none), (Carry::default(), None));
        assert!(decide(&cleared, Some("bead sp-x is not in the store")).1.is_some(), "a failure that returns after success is said again");
    }

    #[test]
    fn every_tick_asks_for_a_pass_and_schedules_the_next() {
        use runtime::ports::VirtualClock;
        use runtime::{Engine, Executor, MemJournal};
        let mut e = Engine::open(MemJournal::new(), std::sync::Arc::new(VirtualClock::new(1_800_000_000_000)), vec![Overrides { interval_ms: 1000 }.registration()], 0).unwrap();
        let (id, stream, kind) = Overrides::boot();
        e.submit(id, stream, kind).unwrap();
        e.run_until_idle().unwrap();
        assert_eq!(e.pending_effects().iter().filter(|p| p.spec.name == APPLY).count(), 1);
    }

    #[test]
    fn a_failed_pass_reaches_the_inbox_once_across_ticks() {
        use runtime::ports::VirtualClock;
        use runtime::{Engine, Executor, MemJournal, Outcome};
        let clock = VirtualClock::new(1_800_000_000_000);
        let mut e = Engine::open(MemJournal::new(), std::sync::Arc::new(clock.clone()), vec![Overrides { interval_ms: 1000 }.registration()], 0).unwrap();
        let (id, stream, kind) = Overrides::boot();
        e.submit(id, stream, kind).unwrap();
        e.run_until_idle().unwrap();
        for _ in 0..2 {
            clock.advance(1000);
            e.run_until_idle().unwrap();
            let apply = e.pending_effects().into_iter().find(|p| p.spec.name == APPLY).unwrap();
            e.complete(&apply.key, Outcome::Failed("overrides: ghost: bead sp-x is not in the store".into())).unwrap();
            e.run_until_idle().unwrap();
        }
        let said: Vec<_> = e.pending_effects().into_iter().filter(|p| p.spec.name == INBOX_APPEND).collect();
        assert_eq!(said.len(), 1, "the same failure on the next pass is not said again");
        assert!(said[0].spec.input.contains("bead sp-x is not in the store"));
    }
}
