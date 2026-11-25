//! The re-arm wake for the Concierge pane. Each tick asks the world whether watchd's inbox reader
//! lock is held and how many inbox lines lie past its cursor; `decide` says whether the pane gets
//! one wake line, and does no IO.

use runtime::event::*;
use runtime::rows::Part;
use serde::{Deserialize, Serialize};

use super::{iso8601, CONCIERGE_WAKE};

pub const STREAM: &str = "watch.inbox-keeper";
pub const TICK: EventKindTag = "inbox-keeper.tick";
pub const STATE: EventKindTag = "inbox-keeper.state";
pub const OBSERVE: &str = "inbox-keeper.observe";
pub const NAME: &str = "inbox-keeper";

const ROW_PARTS: &[Part] = &[Part::Field("last_seen"), Part::Field("last_wake"), Part::Field("persisted_seen"), Part::Field("wake_failed"), Part::Field("undelivered"), Part::Field("not_taking_wakes")];

const DEADLINE_MS: u64 = 10_000;
pub const WAKE_DEADLINE_MS: u64 = 30_000;
/// The wake child is killed this long before its effect times out, so the failure is reported as one.
pub const WAKE_KILL_MS: u64 = WAKE_DEADLINE_MS - 5_000;
/// `concierge.sh wake` settles for this long once (its `CONCIERGE_WAKE_SETTLE_SECS` default), and does nothing else slow.
pub const WAKE_SETTLE_MS: u64 = 1_000;
const _: () = assert!(WAKE_KILL_MS >= 10 * WAKE_SETTLE_MS, "the kill must leave the script's one settle room to finish");
const _: () = assert!(WAKE_KILL_MS < 60_000, "a wake is one look, never a hold");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxKeeper {
    pub interval_ms: u64,
    pub stall_secs: u64,
    pub backoff_secs: u64,
    pub tries: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub waiter_alive: bool,
    pub unread: u64,
    pub cursor_mtime: u64,
}

/// The `Err` of an observe that held a readable snapshot but reports failed because the last wake did not deliver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeUndelivered {
    pub error: String,
    pub snapshot: Snapshot,
}

/// Carried between ticks, as the body of the latest `inbox-keeper.state` event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub last_seen: Option<u64>,
    pub last_wake: u64,
    #[serde(default)]
    pub persisted_seen: u64,
    #[serde(default)]
    pub wake_failed: bool,
    #[serde(default)]
    pub undelivered: u32,
    #[serde(default)]
    pub not_taking_wakes: bool,
}

/// `last_seen` advances every tick but is journaled only this often per stall window, so a restart
/// finds it at most a quarter-stall stale.
const SEEN_SAVES_PER_STALL: u64 = 4;

pub const WAKE_LINE: &str = "[inbox-keeper] inbox lines are waiting and no inbox waiter is running — re-arm: watchd next concierge-inbox (Bash run_in_background)";

/// The wake the snapshot calls for, and the state to carry into the next tick.
pub fn decide(cfg: &InboxKeeper, carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, Option<String>) {
    let mut c = *carry;
    let seen = c.last_seen.unwrap_or(now);
    if snap.waiter_alive || snap.unread == 0 {
        c.wake_failed = false;
        c.undelivered = 0;
        c.not_taking_wakes = false;
    }
    if snap.waiter_alive {
        c.last_seen = Some(now);
        return (c, None);
    }
    c.last_seen = Some(seen);
    let stalled = now.saturating_sub(seen.max(snap.cursor_mtime)) >= cfg.stall_secs;
    if stalled && snap.unread > 0 && !c.not_taking_wakes && now.saturating_sub(c.last_wake) >= cfg.next_try_after_secs(c.undelivered) {
        c.last_wake = now;
        return (c, Some(format!("{} no inbox waiter for {}m with {} lines unread — re-arm wake", iso8601(now), now.saturating_sub(seen) / 60, snap.unread)));
    }
    (c, None)
}

/// What the observe runner reports for `snap` given the carry: the snapshot when the last wake is not
/// known to have failed, an `Err` carrying it while one did and the condition still holds.
pub fn observed(carry: &Carry, snap: Snapshot) -> Result<String, String> {
    let json = |r: serde_json::Result<String>| r.map_err(|e| e.to_string());
    if carry.wake_failed && !snap.waiter_alive && snap.unread > 0 {
        let err = WakeUndelivered { error: "the last re-arm wake did not deliver and the inbox waiter is still not running".into(), snapshot: snap };
        return Err(json(serde_json::to_string(&err)).unwrap_or_else(|e| e));
    }
    json(serde_json::to_string(&snap))
}

impl InboxKeeper {
    /// Seconds after the last wake the next is due: the backoff after a first or a delivered wake, and
    /// after `undelivered` refusals one tick doubled per refusal, never past the backoff.
    fn next_try_after_secs(&self, undelivered: u32) -> u64 {
        if undelivered == 0 {
            return self.backoff_secs;
        }
        (self.interval_ms / 1000).saturating_mul(1u64 << (undelivered - 1).min(32)).min(self.backoff_secs)
    }

    fn not_taking_wakes_note(&self, carry: &Carry, refusal: &str) -> Command {
        let note = super::pr_notify::Note {
            subject: "The Concierge is not taking wakes".into(),
            body: format!("{} wakes in a row were not delivered; the keeper has stopped waking until the inbox waiter returns or nothing is unread. Last refusal: {}. Run: concierge.sh attach", self.tries, refusal.trim()),
        };
        Command::Effect(EffectSpec::keyed(super::pr_notify::OPERATOR_NOTE, format!("{NAME}:not-taking-wakes:{}", carry.last_seen.unwrap_or(0)), serde_json::to_string(&note).unwrap_or_default(), DEADLINE_MS))
    }

    pub fn registration(self) -> Registration {
        Registration::new(NAME, self).with_rows(runtime::row_codec!(STREAM, STATE, ROW_PARTS))
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("inbox-keeper/boot", STREAM, EventKind::new(TICK, ""))
    }

    fn carry(view: &Projection, stream: &StreamKey) -> Carry {
        view.events(stream).iter().rev().find(|e| e.kind.tag == STATE).and_then(|e| serde_json::from_str(&e.kind.body).ok()).unwrap_or_default()
    }

    fn answers(view: &Projection, ev: &Event, name: &str) -> bool {
        let Some(cause) = &ev.cause else { return false };
        view.events(&ev.stream).iter().rev().find(|e| &e.id == cause && e.kind.tag == EFFECT_REQUESTED).and_then(|e| serde_json::from_str::<serde_json::Value>(&e.kind.body).ok()).is_some_and(|b| b["spec"]["name"] == name)
    }

    fn state(carry: &Carry) -> Command {
        Command::Emit(EventKind::new(STATE, &serde_json::to_string(carry).unwrap_or_default()))
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
        let carry = Self::carry(view, &ev.stream);
        if ev.kind.tag == TICK {
            return vec![
                Command::Probe(EffectSpec::probe(OBSERVE, serde_json::to_string(&carry).unwrap_or_default(), DEADLINE_MS)),
                Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") },
            ];
        }
        if Self::answers(view, ev, CONCIERGE_WAKE) {
            let failed = ev.kind.tag != EFFECT_SUCCEEDED;
            ctx.event().set("inbox_keeper.wake_failed", u64::from(failed));
            let mut next = Carry { wake_failed: failed, undelivered: if failed { carry.undelivered + 1 } else { 0 }, ..carry };
            let mut cmds = vec![];
            if failed && next.undelivered >= self.tries && !next.not_taking_wakes {
                next.not_taking_wakes = true;
                cmds.push(self.not_taking_wakes_note(&next, &ev.kind.body));
            }
            if next != carry {
                cmds.push(Self::state(&next));
            }
            return cmds;
        }
        if !Self::answers(view, ev, OBSERVE) {
            return vec![];
        }
        let now = ev.at / 1000;
        let snap = match ev.kind.tag.as_str() {
            EFFECT_SUCCEEDED => serde_json::from_str::<Snapshot>(&ev.kind.body).ok(),
            _ => {
                ctx.event().set("inbox_keeper.observe_failed", 1);
                serde_json::from_str::<WakeUndelivered>(&ev.kind.body).ok().map(|u| u.snapshot)
            }
        };
        let Some(snap) = snap else { return vec![] };
        let (next, say) = decide(self, &carry, &snap, now);
        ctx.event().set("inbox_keeper.woke", u64::from(say.is_some()));
        let mut cmds = vec![];
        if say.is_some() {
            cmds.push(Command::Effect(EffectSpec::keyed(CONCIERGE_WAKE, format!("{NAME}:wake:{now}"), WAKE_LINE, WAKE_DEADLINE_MS)));
        }
        let mut next = Carry { persisted_seen: carry.persisted_seen, ..next };
        let behind = next.last_seen.unwrap_or(0).saturating_sub(carry.persisted_seen) >= self.stall_secs / SEEN_SAVES_PER_STALL;
        if next.last_wake != carry.last_wake || next.wake_failed != carry.wake_failed || next.undelivered != carry.undelivered || next.not_taking_wakes != carry.not_taking_wakes || behind {
            next.persisted_seen = next.last_seen.unwrap_or(0);
            cmds.push(Self::state(&next));
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
        InboxKeeper { interval_ms: 60_000, stall_secs: 600, backoff_secs: 3600, tries: 5 }
    }

    fn waiting(cursor_mtime: u64) -> Snapshot {
        Snapshot { waiter_alive: false, unread: 2, cursor_mtime }
    }

    const T: u64 = 1_800_000_000;

    #[test_id(tdubg5w)]
    #[test]
    fn a_stalled_waiter_with_lines_unread_wakes_once_per_backoff() {
        let (c, _) = decide(&cfg(), &Carry::default(), &waiting(0), T);
        let (c, say) = decide(&cfg(), &c, &waiting(0), T + 600);
        assert!(say.is_some());
        assert!(decide(&cfg(), &c, &waiting(0), T + 2000).1.is_none());
        assert!(decide(&cfg(), &c, &waiting(0), T + 600 + 3600).1.is_some());
    }

    #[test_id(tv3ndo7)]
    #[test]
    fn a_live_waiter_or_a_read_inbox_never_wakes() {
        let alive = Snapshot { waiter_alive: true, unread: 5, cursor_mtime: 0 };
        let (c, say) = decide(&cfg(), &Carry::default(), &alive, T);
        assert!(say.is_none());
        assert_eq!(c.last_seen, Some(T));
        let read = Snapshot { waiter_alive: false, unread: 0, cursor_mtime: 0 };
        assert!(decide(&cfg(), &c, &read, T + 5000).1.is_none());
    }

    #[test_id(tubkfw6)]
    #[test]
    fn a_recent_cursor_advance_defers_the_wake_by_a_stall() {
        let (c, _) = decide(&cfg(), &Carry::default(), &waiting(0), T);
        assert!(decide(&cfg(), &c, &waiting(T + 500), T + 700).1.is_none());
        assert!(decide(&cfg(), &c, &waiting(T + 500), T + 1100).1.is_some());
    }

    #[test_id(trzgj6r)]
    #[test]
    fn an_observe_reports_failed_only_while_a_failed_wake_and_the_condition_hold() {
        let failed = Carry { wake_failed: true, ..Carry::default() };
        assert!(observed(&failed, waiting(0)).is_err());
        assert!(observed(&failed, Snapshot { waiter_alive: true, ..waiting(0) }).is_ok());
        assert!(observed(&Carry::default(), waiting(0)).is_ok());
    }
}
