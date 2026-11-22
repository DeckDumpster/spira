//! Keeps exactly one scheduled code review in a non-closed state: the morning after it closes, the next is
//! staged. Each tick asks the world for the reviews and the epoch the next one is due; `decide` over
//! snapshot and carried state only compares epochs and does no IO.

use std::collections::BTreeSet;

use cr::record::{Kind, State};
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answered_effect, answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.cr-cadence";
pub const TICK: EventKindTag = "cr-cadence.tick";
pub const STATE_TAG: EventKindTag = "cr-cadence.state";
pub const OBSERVE: &str = "cr-cadence.observe";
pub const OPEN: &str = "cr-cadence.open";
pub const NAME: &str = "cr-cadence";

const OBSERVE_DEADLINE_MS: u64 = 60_000;
pub const OPEN_CALL_SECS: u64 = 100;
const OPEN_DEADLINE_MS: u64 = (OPEN_CALL_SECS + 20) * 1000;
const DEADLINE_MS: u64 = 10_000;
const DAY: u64 = 86_400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrCadence {
    pub interval_ms: u64,
    pub stale_days: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewObs {
    pub number: u32,
    pub kind: Kind,
    pub state: State,
    /// Milliseconds since the epoch, as the review record holds them.
    pub staged_at: u64,
    pub closed_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub reviews: Vec<ReviewObs>,
    pub due_at: u64,
}

/// Carried between ticks as the body of the latest state event, and sent to each observe: `anchor` is
/// when the row first looked, from which the first review is due when none has ever existed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub anchor: Option<u64>,
    pub requested: Option<u64>,
    pub reported: BTreeSet<u32>,
}

fn live(r: &ReviewObs) -> bool {
    r.state != State::Closed
}

/// The state to carry, whether to open the next scheduled review, and the inbox lines to say.
pub fn decide(cfg: &CrCadence, carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, bool, Vec<String>) {
    let mut next = carry.clone();
    next.anchor.get_or_insert(now);
    let scheduled_live = snap.reviews.iter().any(|r| r.kind == Kind::Scheduled && live(r));
    let open = !scheduled_live && now >= snap.due_at && carry.requested != Some(snap.due_at);
    if open {
        next.requested = Some(snap.due_at);
    }
    let mut said = vec![];
    for r in snap.reviews.iter().filter(|r| live(r)) {
        let age = now.saturating_sub(r.staged_at / 1000);
        if age > cfg.stale_days * DAY && next.reported.insert(r.number) {
            said.push(format!("CR {:03} open {} days", r.number, age / DAY));
        }
    }
    (next, open, said)
}

/// `cr open`'s first line is `<NNN> <unit>`.
fn staged_line(out: &str) -> String {
    let number = out.split_whitespace().next().unwrap_or("???");
    format!("CR {number} staged (scheduled): seal with cr seal --review {number}")
}

impl CrCadence {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("cr-cadence/boot", STREAM, EventKind::new(TICK, ""))
    }
}

fn say(now: u64, m: &str) -> Command {
    Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:inbox:{now}:{:016x}", runtime::event::fnv1a(m)), format!("{} [watch:{NAME}] {m}", iso8601(now)), DEADLINE_MS))
}

fn state(c: &Carry) -> Command {
    Command::Emit(EventKind::new(STATE_TAG, &serde_json::to_string(c).unwrap_or_default()))
}

impl Handler for CrCadence {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        let now = ev.at / 1000;
        let carry = carried::<Carry>(view, &ev.stream, STATE_TAG);
        if ev.kind.tag == TICK {
            let input = serde_json::to_string(&carry).unwrap_or_default();
            return vec![Command::Probe(EffectSpec::probe(OBSERVE, input, OBSERVE_DEADLINE_MS)), Command::Timer { at: ev.at + self.interval_ms, kind: EventKind::new(TICK, "") }];
        }
        if answers_observe(view, ev, OBSERVE) {
            let Some(snap) = (ev.kind.tag == EFFECT_SUCCEEDED).then(|| serde_json::from_str::<Snapshot>(&ev.kind.body).ok()).flatten() else { return vec![] };
            let (next, open, said) = decide(self, &carry, &snap, now);
            ctx.event().set("cr_cadence.said", said.len() as u64);
            let mut cmds = vec![];
            if next != carry {
                cmds.push(state(&next));
            }
            if open {
                cmds.push(Command::Effect(EffectSpec::keyed(OPEN, format!("{NAME}:open:{}", snap.due_at), String::new(), OPEN_DEADLINE_MS)));
            }
            cmds.extend(said.iter().map(|m| say(now, m)));
            return cmds;
        }
        if answered_effect(view, ev).as_deref() != Some(OPEN) {
            return vec![];
        }
        if ev.kind.tag == EFFECT_SUCCEEDED {
            return vec![say(now, &staged_line(&ev.kind.body))];
        }
        let why = if ev.kind.tag == EFFECT_FAILED { ev.kind.body.trim() } else { "timed out" };
        let retry = Carry { requested: None, ..carry };
        vec![state(&retry), say(now, &format!("cr open --count 1 --scheduled failed: {why}"))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUE: u64 = 10 * DAY;

    fn cfg() -> CrCadence {
        CrCadence { interval_ms: 300_000, stale_days: 7 }
    }

    fn rev(number: u32, kind: Kind, state: State) -> ReviewObs {
        ReviewObs { number, kind, state, staged_at: (DUE - DAY) * 1000, closed_at: (state == State::Closed).then_some((DUE - 3600) * 1000) }
    }

    fn snap(reviews: Vec<ReviewObs>) -> Snapshot {
        Snapshot { reviews, due_at: DUE }
    }

    fn opens(carry: &Carry, s: &Snapshot, now: u64) -> bool {
        decide(&cfg(), carry, s, now).1
    }

    #[test_id(tc3yjth)]
    #[test]
    fn an_open_scheduled_review_is_never_followed_by_another() {
        let s = snap(vec![rev(1, Kind::Scheduled, State::Open)]);
        for now in [0, DUE, DUE + 30 * DAY] {
            assert!(!opens(&Carry::default(), &s, now), "now={now}");
        }
        for st in [State::Staged, State::Published] {
            assert!(!opens(&Carry::default(), &snap(vec![rev(1, Kind::Scheduled, st)]), DUE));
        }
    }

    #[test_id(tcx3t16)]
    #[test]
    fn a_closed_scheduled_review_is_followed_at_the_due_epoch_and_once() {
        let s = snap(vec![rev(1, Kind::Scheduled, State::Closed)]);
        assert!(!opens(&Carry::default(), &s, DUE - 1));
        let (carry, open, _) = decide(&cfg(), &Carry::default(), &s, DUE);
        assert!(open);
        assert!(!opens(&carry, &s, DUE + 300), "the same snapshot at the next tick opens nothing");
    }

    #[test_id(tc9ash9)]
    #[test]
    fn an_on_demand_review_neither_counts_toward_the_one_nor_delays_it() {
        let s = snap(vec![rev(1, Kind::OnDemand, State::Open)]);
        assert!(opens(&Carry::default(), &s, DUE));
        let s = snap(vec![rev(1, Kind::Scheduled, State::Closed), rev(2, Kind::OnDemand, State::Open)]);
        assert!(opens(&Carry::default(), &s, DUE));
    }

    #[test_id(t40pwn3)]
    #[test]
    fn a_review_past_the_stale_days_is_reported_once() {
        let mut r = rev(4, Kind::OnDemand, State::Open);
        r.staged_at = (100 * DAY - 8 * DAY - 5) * 1000;
        let s = snap(vec![r]);
        let (carry, _, said) = decide(&cfg(), &Carry::default(), &s, 100 * DAY);
        assert_eq!(said, ["CR 004 open 8 days"]);
        assert!(decide(&cfg(), &carry, &s, 100 * DAY + 300).2.is_empty());
    }

    #[test_id(t8tbtgn)]
    #[test]
    fn a_review_staged_in_milliseconds_eight_days_before_the_tick_is_reported_once() {
        let tick_ms: u64 = 100 * DAY * 1000;
        let mut r = rev(4, Kind::OnDemand, State::Open);
        r.staged_at = tick_ms - 8 * DAY * 1000;
        let s = snap(vec![r]);
        let now = tick_ms / 1000;
        let (carry, _, said) = decide(&cfg(), &Carry::default(), &s, now);
        assert_eq!(said, ["CR 004 open 8 days"]);
        assert!(decide(&cfg(), &carry, &s, now + 300).2.is_empty());
    }

    #[test_id(tx0mq28)]
    #[test]
    fn a_closed_or_young_review_is_not_reported() {
        let mut old_closed = rev(1, Kind::OnDemand, State::Closed);
        old_closed.staged_at = 0;
        let mut young = rev(2, Kind::OnDemand, State::Open);
        young.staged_at = (100 * DAY - 7 * DAY) * 1000;
        assert!(decide(&cfg(), &Carry::default(), &snap(vec![old_closed, young]), 100 * DAY).2.is_empty());
    }

    #[test_id(t337tcl)]
    #[test]
    fn the_staged_line_names_the_review_cr_printed() {
        assert_eq!(staged_line("007 mod:cr\nseal with: cr seal --review 007 < comments.json\n"), "CR 007 staged (scheduled): seal with cr seal --review 007");
    }
}
