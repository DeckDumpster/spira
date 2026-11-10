//! Readiness of the records that wait on, or sit under, a record that changed. Each tick asks the
//! store which records had an event since the carried revision, and has their dependents judged
//! again; the revision moves only once that has been done, so a failed pass is asked for again.

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answered_effect, carried};

pub const STREAM: &str = "watch.readiness-dependents";
pub const TICK: EventKindTag = "readiness-dependents.tick";
pub const STATE: EventKindTag = "readiness-dependents.state";
pub const RUN: &str = "readiness-dependents.run";
pub const NAME: &str = "readiness-dependents";

const DEADLINE_MS: u64 = 120_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessDependents {
    pub interval_ms: u64,
}

/// The store revision the dependents are judged through; `None` before the first pass, which
/// takes the head as its baseline rather than judging every record the store holds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub through: Option<u64>,
}

/// What the effect is asked, and what it answers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pass {
    pub through: Option<u64>,
    pub changed: u64,
}

impl ReadinessDependents {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("readiness-dependents/boot", STREAM, EventKind::new(TICK, ""))
    }
}

/// The carry after a pass answered `body`; a body that is not a pass leaves it as it was.
pub fn decide(carry: &Carry, body: &str) -> Carry {
    match serde_json::from_str::<Pass>(body) {
        Ok(Pass { through: Some(t), .. }) if Some(t) >= carry.through => Carry { through: Some(t) },
        _ => carry.clone(),
    }
}

impl Handler for ReadinessDependents {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        if ev.kind.tag == TICK {
            let input = serde_json::to_string(&Pass { through: carry.through, changed: 0 }).unwrap_or_default();
            return vec![
                Command::Effect(EffectSpec::keyed(RUN, format!("{NAME}:run:{}", ev.at), input, DEADLINE_MS)),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if answered_effect(view, ev).as_deref() != Some(RUN) {
            return vec![];
        }
        ctx.event().set("readiness-dependents.failed", u64::from(ev.kind.tag != EFFECT_SUCCEEDED));
        if ev.kind.tag != EFFECT_SUCCEEDED {
            return vec![];
        }
        let next = decide(&carry, &ev.kind.body);
        ctx.event().set("readiness-dependents.changed", serde_json::from_str::<Pass>(&ev.kind.body).map_or(0, |p| p.changed));
        if next == carry {
            return vec![];
        }
        vec![Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default()))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::ports::VirtualClock;
    use runtime::{Engine, Executor, MemJournal, Outcome};

    fn engine() -> (Engine, VirtualClock) {
        let clock = VirtualClock::new(1_800_000_000_000);
        let mut e = Engine::open(MemJournal::new(), std::sync::Arc::new(clock.clone()), vec![ReadinessDependents { interval_ms: 1000 }.registration()], 0).unwrap();
        let (id, stream, kind) = ReadinessDependents::boot();
        e.submit(id, stream, kind).unwrap();
        e.run_until_idle().unwrap();
        (e, clock)
    }

    fn asked(e: &Engine) -> Vec<Pass> {
        e.pending_effects().iter().filter(|p| p.spec.name == RUN).map(|p| serde_json::from_str(&p.spec.input).unwrap()).collect()
    }

    fn answer(e: &mut Engine, body: &str) {
        let run = e.pending_effects().into_iter().find(|p| p.spec.name == RUN).unwrap();
        e.complete(&run.key, Outcome::Succeeded(body.into())).unwrap();
        e.run_until_idle().unwrap();
    }

    #[test]
    fn a_pass_that_reached_a_revision_is_not_asked_for_again_below_it() {
        let (mut e, clock) = engine();
        assert_eq!(asked(&e), [Pass { through: None, changed: 0 }]);
        answer(&mut e, r#"{"through":41,"changed":3}"#);
        clock.advance(1000);
        e.run_until_idle().unwrap();
        assert_eq!(asked(&e).last(), Some(&Pass { through: Some(41), changed: 0 }));
    }

    #[test]
    fn a_failed_pass_leaves_the_revision_so_the_same_changes_are_asked_for_again() {
        let (mut e, clock) = engine();
        answer(&mut e, r#"{"through":41,"changed":0}"#);
        clock.advance(1000);
        e.run_until_idle().unwrap();
        let run = e.pending_effects().into_iter().find(|p| p.spec.name == RUN).unwrap();
        e.complete(&run.key, Outcome::Failed("store down".into())).unwrap();
        e.run_until_idle().unwrap();
        clock.advance(1000);
        e.run_until_idle().unwrap();
        assert_eq!(asked(&e).last(), Some(&Pass { through: Some(41), changed: 0 }));
    }

    #[test]
    fn the_revision_never_moves_back() {
        let carry = Carry { through: Some(50) };
        assert_eq!(decide(&carry, r#"{"through":41,"changed":1}"#), carry);
        assert_eq!(decide(&carry, "not json"), carry);
        assert_eq!(decide(&Carry::default(), r#"{"through":7,"changed":0}"#), Carry { through: Some(7) });
    }
}
