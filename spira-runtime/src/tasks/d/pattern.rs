//! The two shapes families D and E are written in (design §3), once, so each task module holds
//! only its contract: its names, its snapshot and carry, and its `decide`.
//!
//! - **P1 / P4 tick → observe → decide → act.** A tick asks one memory-only `Probe` and arms the
//!   next tick (a fixed cadence for P1, the next `OnCalendar` elapse for P4). The probe's answer
//!   is the snapshot; `decide` over it and the carried state returns the writes, and the carry is
//!   emitted only when it changed, so an idle tick journals nothing.
//! - **P2 machine.** The same tick, plus `unit.ran` for a unit the task started; the carry names
//!   the phase and every move is checked against the task's transition table.

use runtime::calendar::Calendar;
use runtime::event::*;
use runtime::units::UnitRan;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::watchers::{answers_observe, carried, iso8601};

/// The tick's commands: the observe probe, then the next tick at `next_at`.
pub fn tick(ev: &Event, observe: &str, input: String, deadline_ms: u64, next_at: u64) -> Vec<Command> {
    vec![Command::Probe(EffectSpec { name: observe.into(), input, deadline_ms }), Command::Timer { at: next_at, kind: EventKind::new(&ev.kind.tag, "") }]
}

/// The snapshot `ev` carries when it is the succeeded answer to this stream's `observe` probe.
pub fn answer<S: DeserializeOwned>(view: &Projection, ev: &Event, observe: &str) -> Option<S> {
    if ev.kind.tag != EFFECT_SUCCEEDED || !answers_observe(view, ev, observe) {
        return None;
    }
    serde_json::from_str(&ev.kind.body).ok()
}

/// The run `ev` reports when it is a `unit.ran` for a unit `task` started, with its key.
pub fn unit_ran(ev: &Event, task: &str) -> Option<(String, UnitRan)> {
    let ran = UnitRan::from_event_kind(&ev.kind)?;
    ran.key_for(task).map(|k| (k, ran))
}

/// One `Emit` of `after` under `tag` when it differs from `before`, then `acts` as effects.
pub fn commands<C: Serialize + PartialEq>(tag: EventKindTag, before: &C, after: &C, acts: Vec<EffectSpec>) -> Vec<Command> {
    let mut out = vec![];
    if before != after {
        out.push(Command::Emit(EventKind::new(tag, &serde_json::to_string(after).unwrap_or_default())));
    }
    out.extend(acts.into_iter().map(Command::Effect));
    out
}

/// The P1 answer path: when `ev` answers `observe`, `decide` over the carry under `state` and the
/// snapshot, with the pinned config; a config that did not resolve refuses, naming the key on the
/// wide event, and acts on nothing.
pub fn on_answer<S, C, K>(ctx: &Ctx, view: &Projection, ev: &Event, observe: &str, state: EventKindTag, config: &Result<K, String>, decide: impl FnOnce(&C, &S, u64, &K) -> (C, Vec<EffectSpec>)) -> Vec<Command>
where
    S: DeserializeOwned,
    C: DeserializeOwned + Serialize + Default + PartialEq,
{
    let Some(snap) = answer::<S>(view, ev, observe) else { return vec![] };
    let config = match config {
        Ok(c) => c,
        Err(e) => {
            ctx.event().set("config.refused", e.as_str());
            return vec![];
        }
    };
    let carry = carried::<C>(view, &ev.stream, state);
    let (next, acts) = decide(&carry, &snap, ev.at, config);
    commands(state, &carry, &next, acts)
}

/// A calendar spec the module names as a literal; a literal that does not parse is a build bug.
pub fn calendar(spec: &str) -> Calendar {
    Calendar::parse(spec).unwrap_or_else(|e| panic!("calendar literal {spec:?}: {e}"))
}

/// `<UTC ISO> [watch:<tag>] <line>`, the house inbox line every watchd row wrote.
pub fn inbox_line(now_ms: u64, tag: &str, line: &str) -> String {
    format!("{} [watch:{tag}] {line}", iso8601(now_ms / 1000))
}

/// A P2 transition table: `(from, on, to)` rows, every legal move and nothing else.
pub type Table<P> = &'static [(P, &'static str, P)];

/// `to` when `table` has the row `(from, on, to)`; otherwise a refusal naming the state.
pub fn transition<P: Copy + PartialEq + std::fmt::Debug>(table: Table<P>, task: &str, from: P, on: &str) -> Result<P, String> {
    table.iter().find(|(f, o, _)| *f == from && *o == on).map(|(_, _, to)| *to).ok_or_else(|| format!("{task}: {on} refused in state {from:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum P {
        Idle,
        Running,
    }

    #[test]
    fn a_table_move_lands_and_a_move_off_the_table_is_refused_naming_the_state() {
        static T: Table<P> = &[(P::Idle, "tick", P::Running), (P::Running, "unit.ran", P::Idle)];
        assert_eq!(transition(T, "t", P::Idle, "tick"), Ok(P::Running));
        assert_eq!(transition(T, "t", P::Idle, "unit.ran"), Err("t: unit.ran refused in state Idle".into()));
    }

    #[test]
    fn the_carry_is_emitted_only_when_it_changed() {
        assert!(commands("s", &1, &1, vec![]).is_empty());
        assert!(matches!(commands("s", &1, &2, vec![]).as_slice(), [Command::Emit(k)] if k.body == "2"));
    }

    #[test]
    fn the_inbox_line_is_the_house_format() {
        assert_eq!(inbox_line(1_793_166_343_000, "warm-full", "x"), "2026-10-28T05:45:43Z [watch:warm-full] x");
    }
}
