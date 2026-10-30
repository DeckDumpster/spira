//! NEW ASK and NEW ASK HOLD for the Concierge inbox. Each tick asks the world which asks are
//! open and which ask holds exist, with a line already composed for the holds not yet seen;
//! the decision over that and the seen set is `decide`, which does no IO.

use std::collections::BTreeSet;

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.asks";
pub const TICK: EventKindTag = "asks.tick";
pub const STATE: EventKindTag = "asks.state";
pub const OBSERVE: &str = "asks.observe";
pub const NAME: &str = "asks";

const DEADLINE_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asks {
    pub interval_ms: u64,
}

/// `open` is (id, title) of every open ask bead; `held` is (id, announcement) of every ask hold
/// whose id the tick was not told was already seen.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub open: Vec<(String, String)>,
    pub held: Vec<(String, String)>,
}

/// Carried between ticks. The first answered tick is the baseline and announces nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub seeded: bool,
    pub seen: BTreeSet<String>,
}

pub fn decide(carry: &Carry, snap: &Snapshot) -> (Carry, Vec<String>) {
    let mut c = carry.clone();
    let mut say = vec![];
    for (id, title) in &snap.open {
        if c.seen.insert(id.clone()) && c.seeded {
            say.push(format!("NEW ASK {id}: {}", title.chars().take(169usize.saturating_sub(id.chars().count())).collect::<String>()));
        }
    }
    for (id, line) in &snap.held {
        if c.seen.insert(id.clone()) && c.seeded {
            say.push(line.clone());
        }
    }
    c.seeded = true;
    (c, say)
}

impl Asks {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("asks/boot", STREAM, EventKind::new(TICK, ""))
    }
}

impl Handler for Asks {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let input = serde_json::to_string(&carried::<Carry>(view, &ev.stream, STATE).seen).unwrap_or_default();
            return vec![
                Command::Probe(EffectSpec { name: OBSERVE.into(), input, deadline_ms: DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else { return vec![] };
        let carry = carried::<Carry>(view, &ev.stream, STATE);
        let (next, say) = decide(&carry, &snap);
        ctx.event().set("asks.said", say.len() as u64);
        let now = ev.at / 1000;
        let mut cmds = vec![];
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        for m in say {
            cmds.push(Command::Effect(EffectSpec { name: INBOX_APPEND.into(), input: format!("{} [watch:{NAME}] {m}", iso8601(now)), deadline_ms: DEADLINE_MS }));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(ids: &[(&str, &str)]) -> Snapshot {
        Snapshot { open: ids.iter().map(|(i, t)| (i.to_string(), t.to_string())).collect(), held: vec![] }
    }

    #[test]
    fn the_first_answer_is_the_baseline_and_a_later_ask_is_announced_once() {
        let (c, say) = decide(&Carry::default(), &open(&[("sp-old", "already open")]));
        assert!(say.is_empty());
        let (c, say) = decide(&c, &open(&[("sp-old", "already open"), ("sp-new", "decide X")]));
        assert_eq!(say, ["NEW ASK sp-new: decide X"]);
        assert!(decide(&c, &open(&[("sp-new", "decide X")])).1.is_empty());
    }

    #[test]
    fn a_hold_is_announced_by_the_line_the_world_composed_and_not_twice_when_also_open() {
        let (c, _) = decide(&Carry::default(), &open(&[]));
        let snap = Snapshot { open: vec![("sp-a".into(), "t".into())], held: vec![("sp-a".into(), "NEW ASK HOLD sp-a".into()), ("sp-h".into(), "NEW ASK HOLD sp-h".into())] };
        assert_eq!(decide(&c, &snap).1, ["NEW ASK sp-a: t", "NEW ASK HOLD sp-h"]);
    }

    #[test]
    fn the_id_and_title_together_are_cut_to_170_characters() {
        let (c, _) = decide(&Carry::default(), &open(&[]));
        let long = "é".repeat(200);
        let say = decide(&c, &open(&[("sp-a", &long)])).1;
        assert_eq!(say[0].chars().count(), "NEW ASK sp-a: ".len() + 170 - "sp-a\t".len());
    }
}
