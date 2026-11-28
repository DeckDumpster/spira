//! CLAIMS lines for the Concierge inbox: a live item reaching its 4th builder claim, and every
//! fourth after, with how each kept earlier builder session ended. The watcher only reports; the
//! count and the end reasons are the lifecycle row's, read through the store's live list. Each
//! live item at or past that first count is also held on `watch/claim-count/<id>`.

use std::collections::BTreeMap;

use runtime::event::*;
use runtime::rows::Part;
use serde::{Deserialize, Serialize};

use super::{carried, iso8601, INBOX_APPEND};
use crate::watch_topics::{self, Change, Published};

pub const STREAM: &str = "watch.claim-count";
pub const TICK: EventKindTag = "claim-count.tick";
pub const STATE: EventKindTag = "claim-count.state";
pub const OBSERVE: &str = "claim-count.observe";
pub const NAME: &str = "claim-count";
const ROW_PARTS: &[Part] = &[Part::Field("seeded"), Part::Map { name: "paged", drop: &[] }, Part::Map { name: "published", drop: &[] }];
const OBSERVE_DEADLINE_MS: u64 = 30_000;
const APPEND_DEADLINE_MS: u64 = 10_000;
/// How many session ends a row keeps (`lifecycle::bead::SESSION_ENDS_KEPT`).
const ENDS_KEPT: u32 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimCount {
    pub interval_ms: u64,
    pub every: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    pub id: String,
    pub n: u32,
    pub ends: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub rows: Vec<Claims>,
}

/// Carried between ticks: whether the first pass has run, each live item's claim count as of
/// the last tick that saw it, and what is held on the broker.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Carry {
    pub seeded: bool,
    pub paged: BTreeMap<String, u32>,
    pub published: Published,
}

fn end_text(e: &serde_json::Value) -> String {
    let text = |k: &str| e[k].as_str().map(str::to_string).or_else(|| e[k].as_u64().map(|n| n.to_string()));
    format!("#{} {} -> {}", text("claim").unwrap_or_else(|| "?".into()), text("left").unwrap_or_else(|| "?".into()), text("back").unwrap_or_else(|| "-".into()))
}

fn line(c: &Claims) -> String {
    let ends: Vec<String> = c.ends.iter().map(end_text).collect();
    let cut = if c.n.saturating_sub(1) > ENDS_KEPT { " (newest 8 kept)" } else { "" };
    format!("CLAIMS {}: claimed by a builder {} times and not landed; earlier sessions: {}{cut}", c.id, c.n, ends.join("; "))
}

/// The lines a snapshot calls for, and the carry into the next tick.
pub fn decide(carry: &Carry, snap: &Snapshot, every: u32) -> (Carry, Vec<Claims>) {
    let seen: BTreeMap<String, u32> = snap.rows.iter().map(|r| (r.id.clone(), r.n)).collect();
    if !carry.seeded {
        return (Carry { seeded: true, paged: seen, published: carry.published.clone() }, vec![]);
    }
    let every = every.max(1);
    let mut paged = BTreeMap::new();
    let mut out = vec![];
    for r in &snap.rows {
        let before = carry.paged.get(&r.id).copied().unwrap_or(0);
        if r.n >= every && r.n / every > before / every {
            out.push(r.clone());
            paged.insert(r.id.clone(), r.n);
        } else if let Some(b) = carry.paged.get(&r.id) {
            paged.insert(r.id.clone(), *b);
        }
    }
    (Carry { seeded: true, paged, published: carry.published.clone() }, out)
}

/// The pass's topics: every live item with at least `every` claims holds its page's line.
pub fn topics(published: &Published, snap: &Snapshot, every: u32, now_ms: u64) -> (Published, Option<Change>) {
    let over = snap.rows.iter().filter(|r| r.n >= every.max(1));
    watch_topics::settle(NAME, published, over.map(|r| (r.id.clone(), line(r))).collect(), now_ms)
}

impl ClaimCount {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("claim-count/boot", STREAM, EventKind::new(TICK, ""))
    }

}

impl Handler for ClaimCount {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let held = watch_topics::carried(NAME, &carried::<Carry>(view, &ev.stream, STATE).published);
            return vec![
                Command::Probe(EffectSpec::probe(OBSERVE, held, OBSERVE_DEADLINE_MS)),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if ev.kind.tag == EFFECT_TIMED_OUT {
            let carry: Carry = carried(view, &ev.stream, STATE);
            return watch_topics::again(view, ev, &carry.published).map(Command::Effect).into_iter().collect();
        }
        let observed = ev.cause.as_ref().is_some_and(|cause| {
            view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED).and_then(|e| serde_json::from_str::<serde_json::Value>(&e.kind.body).ok()).is_some_and(|b| b["spec"]["name"] == OBSERVE)
        });
        if !observed {
            return vec![];
        }
        let Ok(snap) = serde_json::from_str::<Snapshot>(&ev.kind.body) else {
            return vec![];
        };
        let carry: Carry = carried(view, &ev.stream, STATE);
        let (mut next, due) = decide(&carry, &snap, self.every);
        let (published, change) = topics(&carry.published, &snap, self.every, ev.at);
        next.published = published;
        ctx.event().set("claim_count.live", snap.rows.len() as u64);
        let now = ev.at / 1000;
        let mut cmds = vec![];
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        cmds.extend(due.iter().map(|c| {
            Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:{}:{}", c.id, c.n), format!("{} [watch:{NAME}] {}", iso8601(now), line(c)), APPEND_DEADLINE_MS))
        }));
        cmds.extend(change.map(|c| Command::Effect(watch_topics::effect(&c, ev.at))));
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, n: u32) -> Claims {
        Claims { id: id.into(), n, ends: vec![] }
    }

    fn snap(rows: &[(&str, u32)]) -> Snapshot {
        Snapshot { rows: rows.iter().map(|(i, n)| row(i, *n)).collect() }
    }

    #[test_id(t0chog6)]
    #[test]
    fn the_first_pass_pages_nothing_and_a_crossing_of_a_multiple_pages_once() {
        let (c, out) = decide(&Carry::default(), &snap(&[("a", 9)]), 4);
        assert!(out.is_empty() && c.seeded && c.paged["a"] == 9);
        let (c, out) = decide(&c, &snap(&[("a", 9), ("b", 5)]), 4);
        assert_eq!(out.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["b"]);
        let (c, out) = decide(&c, &snap(&[("a", 12), ("b", 5)]), 4);
        assert_eq!(out.len(), 1);
        let (c2, out) = decide(&c, &snap(&[("a", 12), ("b", 7)]), 4);
        assert!(out.is_empty() && c2 == c, "an idle tick changes nothing");
        let (c3, _) = decide(&c, &snap(&[("a", 13)]), 4);
        assert!(!c3.paged.contains_key("b"), "an item that left the live list leaves the carry");
    }

    fn set_and_clear(change: &Option<Change>) -> (Vec<&str>, Vec<&str>) {
        change.as_ref().map_or_else(Default::default, |c| (c.set.keys().map(String::as_str).collect(), c.clear.iter().map(String::as_str).collect()))
    }

    #[test_id(txumcad)]
    #[test]
    fn a_topic_is_set_when_an_item_reaches_the_count_left_alone_while_unchanged_and_cleared_when_it_leaves() {
        let (p, change) = topics(&Published::new(), &snap(&[("a", 4), ("under", 3)]), 4, 10_000);
        assert_eq!(set_and_clear(&change), (vec!["a"], vec![]), "appear");
        assert_eq!((change.unwrap().prefix.as_str(), p["a"].level.as_str(), p["a"].since, p["a"].at), ("watch/claim-count", "P2", 10, 10));
        let (held, change) = topics(&p, &snap(&[("a", 4), ("under", 3)]), 4, 70_000);
        assert_eq!((change, &held), (None, &p), "hold");
        let (p, change) = topics(&held, &snap(&[("a", 5)]), 4, 130_000);
        assert_eq!(set_and_clear(&change), (vec!["a"], vec![]), "change");
        assert!(p["a"].text.contains("5 times"), "{p:?}");
        assert_eq!((p["a"].since, p["a"].at), (10, 130), "since survives a change of text");
        let (p, change) = topics(&p, &snap(&[("under", 3)]), 4, 190_000);
        assert_eq!(set_and_clear(&change), (vec![], vec!["a"]), "clear");
        assert!(p.is_empty());
    }
}
