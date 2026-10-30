//! The re-arm wake for the Concierge pane. Each tick asks the world whether an inbox-triage
//! process is alive and when the inbox last changed; `decide` says whether the pane gets one
//! wake line, and does no IO.

use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{iso8601, CONCIERGE_WAKE};

pub const STREAM: &str = "watch.inbox-keeper";
pub const TICK: EventKindTag = "inbox-keeper.tick";
pub const STATE: EventKindTag = "inbox-keeper.state";
pub const OBSERVE: &str = "inbox-keeper.observe";
pub const NAME: &str = "inbox-keeper";

const DEADLINE_MS: u64 = 10_000;
pub const WAKE_DEADLINE_MS: u64 = 700_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxKeeper {
    pub interval_ms: u64,
    pub stall_secs: u64,
    pub backoff_secs: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub triage_alive: bool,
    pub inbox_mtime: u64,
}

/// Carried between ticks, as the body of the latest `inbox-keeper.state` event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub last_seen: Option<u64>,
    pub last_wake: u64,
    #[serde(default)]
    pub persisted_seen: u64,
}

/// `last_seen` advances every tick but is journaled only this often per stall window, so a restart
/// finds it at most a quarter-stall stale.
const SEEN_SAVES_PER_STALL: u64 = 4;

pub const WAKE_LINE: &str = "[inbox-keeper] events are waiting and no inbox monitor is running — re-arm: Monitor inbox-triage";

/// The wake the snapshot calls for, and the state to carry into the next tick.
pub fn decide(cfg: &InboxKeeper, carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, Option<String>) {
    let mut c = *carry;
    let seen = c.last_seen.unwrap_or(now);
    if snap.triage_alive {
        c.last_seen = Some(now);
        return (c, None);
    }
    c.last_seen = Some(seen);
    let stalled = now.saturating_sub(seen) >= cfg.stall_secs;
    if stalled && snap.inbox_mtime > seen && now.saturating_sub(c.last_wake) >= cfg.backoff_secs {
        c.last_wake = now;
        return (c, Some(format!("{} no triage monitor for {}m with events waiting — re-arm wake", iso8601(now), now.saturating_sub(seen) / 60)));
    }
    (c, None)
}

impl InboxKeeper {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("inbox-keeper/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn carry(view: &Projection, stream: &StreamKey) -> Carry {
        view.events(stream).iter().rev().find(|e| e.kind.tag == STATE).and_then(|e| serde_json::from_str(&e.kind.body).ok()).unwrap_or_default()
    }

    fn answers_observe(view: &Projection, ev: &Event) -> bool {
        let Some(cause) = &ev.cause else { return false };
        view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED).and_then(|e| serde_json::from_str::<serde_json::Value>(&e.kind.body).ok()).is_some_and(|b| b["spec"]["name"] == OBSERVE)
    }
}

impl Handler for InboxKeeper {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            return vec![
                Command::Probe(EffectSpec { name: OBSERVE.into(), input: String::new(), deadline_ms: DEADLINE_MS }),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if !Self::answers_observe(view, ev) {
            return vec![];
        }
        let carry = Self::carry(view, &ev.stream);
        let now = ev.at / 1000;
        let Ok(snap) = (match ev.kind.tag.as_str() {
            EFFECT_SUCCEEDED => serde_json::from_str::<Snapshot>(&ev.kind.body),
            _ => return vec![],
        }) else {
            return vec![];
        };
        let (next, say) = decide(self, &carry, &snap, now);
        ctx.event().set("inbox_keeper.woke", u64::from(say.is_some()));
        let mut cmds = vec![];
        if say.is_some() {
            cmds.push(Command::Effect(EffectSpec { name: CONCIERGE_WAKE.into(), input: WAKE_LINE.into(), deadline_ms: WAKE_DEADLINE_MS }));
        }
        let mut next = Carry { persisted_seen: carry.persisted_seen, ..next };
        let behind = next.last_seen.unwrap_or(0).saturating_sub(carry.persisted_seen) >= self.stall_secs / SEEN_SAVES_PER_STALL;
        if next.last_wake != carry.last_wake || behind {
            next.persisted_seen = next.last_seen.unwrap_or(0);
            cmds.push(Command::Emit(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        } else if next != carry {
            cmds.push(Command::Note(EventKind::new(STATE, &serde_json::to_string(&next).unwrap_or_default())));
        }
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> InboxKeeper {
        InboxKeeper { interval_ms: 60_000, stall_secs: 600, backoff_secs: 3600 }
    }

    fn waiting(mtime: u64) -> Snapshot {
        Snapshot { triage_alive: false, inbox_mtime: mtime }
    }

    const T: u64 = 1_800_000_000;

    #[test]
    fn a_stalled_monitor_with_events_waiting_wakes_once_per_backoff() {
        let (c, _) = decide(&cfg(), &Carry::default(), &waiting(T + 500), T);
        let (c, say) = decide(&cfg(), &c, &waiting(T + 500), T + 600);
        assert!(say.is_some());
        assert!(decide(&cfg(), &c, &waiting(T + 700), T + 2000).1.is_none());
        assert!(decide(&cfg(), &c, &waiting(T + 3000), T + 600 + 3600).1.is_some());
    }

    #[test]
    fn a_live_monitor_or_an_old_inbox_never_wakes() {
        let alive = Snapshot { triage_alive: true, inbox_mtime: T + 9999 };
        let (c, say) = decide(&cfg(), &Carry::default(), &alive, T);
        assert!(say.is_none());
        assert_eq!(c.last_seen, Some(T));
        assert!(decide(&cfg(), &c, &waiting(T - 100), T + 5000).1.is_none());
    }
}
