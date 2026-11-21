//! A code review pull request's new comments, submitted reviews and close, for the Concierge's inbox.
//! Each tick asks the world for one snapshot of every open review; `decide` over snapshot and carried
//! state does no IO. The next tick is soon while a comment arrived lately and late otherwise.

use std::collections::{BTreeMap, BTreeSet};

use cr::record::State;
use cr::thread::Comment;
use runtime::event::*;
use serde::{Deserialize, Serialize};

use super::{answers_observe, carried, iso8601, INBOX_APPEND};

pub const STREAM: &str = "watch.cr-comments";
pub const TICK: EventKindTag = "cr-comments.tick";
pub const STATE_TAG: EventKindTag = "cr-comments.state";
pub const OBSERVE: &str = "cr-comments.observe";
pub const RECORD: &str = "cr-comments.record";
pub const NAME: &str = "cr-comments";

const OBSERVE_DEADLINE_MS: u64 = 120_000;
const DEADLINE_MS: u64 = 10_000;
const BODY_CHARS: usize = 300;
const PUBLISHING: [&str; 3] = ["COMMENTED", "APPROVED", "CHANGES_REQUESTED"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrComments {
    pub hot_ms: u64,
    pub cold_ms: u64,
    pub window_secs: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvent {
    pub id: u64,
    pub author: String,
    pub bot: bool,
    pub state: String,
    pub submitted_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pull {
    pub state: String,
    pub closed_at: Option<u64>,
}

/// One review as the forge showed it. A field is `None` when the forge did not answer it, which is
/// never the same as an empty list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewObs {
    pub number: u32,
    pub pr: u64,
    pub state: Option<State>,
    pub comments: Option<Vec<Comment>>,
    pub conversation: Option<Vec<Comment>>,
    pub reviews: Option<Vec<ReviewEvent>>,
    pub pull: Option<Pull>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub reviews: Vec<ReviewObs>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCarry {
    pub comments: BTreeSet<u64>,
    pub conversation: BTreeSet<u64>,
    pub reviews: BTreeSet<u64>,
    pub closed: bool,
    pub last_comment_at: Option<u64>,
}

/// Carried between ticks as the body of the latest state event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    pub reviews: BTreeMap<u32, ReviewCarry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub number: u32,
    pub state: State,
    pub at: u64,
}

/// What the record effect applies: review states to change, and the comments `cr thread` reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Records {
    pub changes: Vec<Change>,
    pub threads: Vec<(u32, Vec<Comment>)>,
}

impl Records {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.threads.is_empty()
    }
}

fn one_line(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(BODY_CHARS) {
        Some((cut, _)) => flat[..cut].to_string(),
        None => flat,
    }
}

fn comment_line(number: u32, pr: u64, c: &Comment) -> String {
    let at = match (&c.path, c.line) {
        (Some(p), Some(l)) => format!(" at {p}:{l}"),
        (Some(p), None) => format!(" at {p}"),
        _ => String::new(),
    };
    format!("CR {number:03} #{pr} comment {} by {}{at}: {} — cr thread --review {number:03} --comment {}", c.id, c.author, one_line(&c.body), c.id)
}

/// The lines a snapshot calls for, the record changes and comments to keep, and the state to carry.
/// A review the forge did not answer for in part changes nothing for that part.
pub fn decide(carry: &Carry, snap: &Snapshot, now: u64) -> (Carry, Vec<String>, Records) {
    let mut c = carry.clone();
    let mut said = vec![];
    let mut records = Records::default();
    for obs in &snap.reviews {
        let rc = c.reviews.entry(obs.number).or_default();
        let mut fresh = false;
        for (list, seen) in [(&obs.comments, &mut rc.comments), (&obs.conversation, &mut rc.conversation)] {
            for cm in list.iter().flatten() {
                if !seen.insert(cm.id) {
                    continue;
                }
                fresh = true;
                if !cm.bot {
                    said.push(comment_line(obs.number, obs.pr, cm));
                }
            }
        }
        if fresh {
            rc.last_comment_at = Some(now);
            let kept: Vec<Comment> = obs.comments.iter().chain(&obs.conversation).flatten().cloned().collect();
            records.threads.push((obs.number, kept));
        }
        for r in obs.reviews.iter().flatten() {
            if !rc.reviews.insert(r.id) {
                continue;
            }
            if !r.bot && PUBLISHING.contains(&r.state.as_str()) {
                said.push(format!("CR {:03} published (review {} by {})", obs.number, r.id, r.author));
                if obs.state == Some(State::Open) {
                    records.changes.push(Change { number: obs.number, state: State::Published, at: r.submitted_at });
                }
            }
        }
        if let Some(p) = obs.pull.as_ref().filter(|p| p.state == "closed") {
            if !rc.closed {
                rc.closed = true;
                said.push(format!("CR {:03} closed", obs.number));
            }
            records.changes.push(Change { number: obs.number, state: State::Closed, at: p.closed_at.unwrap_or(now) });
        }
    }
    (c, said, records)
}

impl CrComments {
    pub fn registration(self) -> Registration {
        Registration::new(NAME, self)
    }

    pub fn boot() -> (&'static str, &'static str, EventKind) {
        ("cr-comments/boot", STREAM, EventKind::new(TICK, ""))
    }

    /// The wait before the next tick: short while any watched review had a comment within the window.
    pub fn next_in_ms(&self, carry: &Carry, now: u64) -> u64 {
        let lately = carry.reviews.values().filter_map(|r| r.last_comment_at).any(|at| now.saturating_sub(at) <= self.window_secs);
        if lately {
            self.hot_ms
        } else {
            self.cold_ms
        }
    }
}

impl Handler for CrComments {
    fn interests(&self) -> &[EventKindTag] {
        &[TICK, EFFECT_SUCCEEDED, EFFECT_FAILED, EFFECT_TIMED_OUT]
    }

    fn handle(&self, ctx: &Ctx, view: &Projection, ev: &Event) -> Vec<Command> {
        if ev.stream.0 != STREAM {
            return vec![];
        }
        if ev.kind.tag == TICK {
            let input = serde_json::to_string(&carried::<Carry>(view, &ev.stream, STATE_TAG)).unwrap_or_default();
            return vec![Command::Probe(EffectSpec::probe(OBSERVE, input, OBSERVE_DEADLINE_MS))];
        }
        if !answers_observe(view, ev, OBSERVE) {
            return vec![];
        }
        let now = ev.at / 1000;
        let carry = carried::<Carry>(view, &ev.stream, STATE_TAG);
        let again = |c: &Carry| Command::Timer { at: ev.at + self.next_in_ms(c, now), kind: EventKind::new(TICK, "") };
        let Some(snap) = (ev.kind.tag == EFFECT_SUCCEEDED).then(|| serde_json::from_str::<Snapshot>(&ev.kind.body).ok()).flatten() else { return vec![again(&carry)] };
        let (next, said, records) = decide(&carry, &snap, now);
        ctx.event().set("cr_comments.said", said.len() as u64);
        let mut cmds = vec![];
        if next != carry {
            cmds.push(Command::Emit(EventKind::new(STATE_TAG, &serde_json::to_string(&next).unwrap_or_default())));
        }
        if !records.is_empty() {
            let input = serde_json::to_string(&records).unwrap_or_default();
            cmds.push(Command::Effect(EffectSpec::keyed(RECORD, format!("{NAME}:record:{now}:{:016x}", runtime::event::fnv1a(&input)), input, DEADLINE_MS)));
        }
        for m in said {
            cmds.push(Command::Effect(EffectSpec::keyed(INBOX_APPEND, format!("{NAME}:inbox:{now}:{:016x}", runtime::event::fnv1a(&m)), format!("{} [watch:{NAME}] {m}", iso8601(now)), DEADLINE_MS)));
        }
        cmds.push(again(&next));
        cmds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(id: u64, body: &str) -> Comment {
        Comment { id, author: "reviewer".into(), path: Some("a/b.rs".into()), line: Some(12), body: body.into(), ..Comment::default() }
    }

    fn review(comments: Vec<Comment>) -> ReviewObs {
        ReviewObs { number: 7, pr: 3, state: Some(State::Open), comments: Some(comments), conversation: Some(vec![]), reviews: Some(vec![]), pull: Some(Pull { state: "open".into(), closed_at: None }) }
    }

    fn snap(r: ReviewObs) -> Snapshot {
        Snapshot { reviews: vec![r] }
    }

    #[test_id(tpo4c4l)]
    #[test]
    fn a_new_person_comment_is_one_line_with_its_place_and_its_thread_command() {
        let (_, said, records) = decide(&Carry::default(), &snap(review(vec![person(41, "why\n  this\tway?")])), 1000);
        assert_eq!(said, ["CR 007 #3 comment 41 by reviewer at a/b.rs:12: why this way? — cr thread --review 007 --comment 41"]);
        assert_eq!(records.threads.len(), 1, "the comment is kept for cr thread");
    }

    #[test_id(ttktu9e)]
    #[test]
    fn a_long_body_is_cut_at_three_hundred_characters() {
        let (_, said, _) = decide(&Carry::default(), &snap(review(vec![person(1, &"é".repeat(400))])), 1000);
        assert!(said[0].contains(&format!(": {} — cr thread", "é".repeat(300))), "{}", said[0]);
    }

    #[test_id(ttehvwu)]
    #[test]
    fn a_conversation_comment_has_no_place() {
        let mut r = review(vec![]);
        r.conversation = Some(vec![Comment { path: None, line: None, ..person(9, "publish it") }]);
        let (_, said, _) = decide(&Carry::default(), &snap(r), 1000);
        assert_eq!(said, ["CR 007 #3 comment 9 by reviewer: publish it — cr thread --review 007 --comment 9"]);
    }

    #[test_id(tdqfne9)]
    #[test]
    fn a_comment_seen_again_gives_nothing() {
        let s = snap(review(vec![person(41, "why")]));
        let (c, _, _) = decide(&Carry::default(), &s, 1000);
        let (after, said, records) = decide(&c, &s, 1030);
        assert_eq!((said, records.is_empty(), after), (vec![], true, c));
    }

    #[test_id(tdppqc9)]
    #[test]
    fn a_bot_comment_gives_nothing_and_is_marked_seen() {
        let bot = Comment { bot: true, ..person(5, "automated") };
        let (c, said, _) = decide(&Carry::default(), &snap(review(vec![bot])), 1000);
        assert!(said.is_empty());
        assert!(c.reviews[&7].comments.contains(&5));
    }

    fn submitted(bot: bool, state: &str) -> ReviewEvent {
        ReviewEvent { id: 70, author: "reviewer".into(), bot, state: state.into(), submitted_at: 900 }
    }

    #[test_id(t9su5dj)]
    #[test]
    fn a_submitted_review_publishes_the_review_once() {
        let mut r = review(vec![]);
        r.reviews = Some(vec![submitted(false, "COMMENTED")]);
        let (c, said, records) = decide(&Carry::default(), &snap(r.clone()), 1000);
        assert_eq!(said, ["CR 007 published (review 70 by reviewer)"]);
        assert_eq!(records.changes, [Change { number: 7, state: State::Published, at: 900 }]);
        let (_, said, records) = decide(&c, &snap(r), 1030);
        assert_eq!((said, records.changes), (vec![], vec![]));
    }

    #[test_id(ty34tjm)]
    #[test]
    fn a_pending_or_bot_review_publishes_nothing() {
        for ev in [submitted(false, "PENDING"), submitted(false, "DISMISSED"), submitted(true, "COMMENTED")] {
            let mut r = review(vec![]);
            r.reviews = Some(vec![ev]);
            let (_, said, records) = decide(&Carry::default(), &snap(r), 1000);
            assert_eq!((said, records.changes), (vec![], vec![]));
        }
    }

    #[test_id(t61mhqg)]
    #[test]
    fn a_review_after_the_operator_already_published_by_comment_changes_no_record() {
        let mut r = review(vec![]);
        r.state = Some(State::Published);
        r.reviews = Some(vec![submitted(false, "APPROVED")]);
        assert_eq!(decide(&Carry::default(), &snap(r), 1000).2.changes, []);
    }

    #[test_id(t5uhdsk)]
    #[test]
    fn a_closed_pull_request_is_closed_and_its_record_says_so() {
        let mut r = review(vec![]);
        r.pull = Some(Pull { state: "closed".into(), closed_at: Some(950) });
        let (c, said, records) = decide(&Carry::default(), &snap(r.clone()), 1000);
        assert_eq!(said, ["CR 007 closed"]);
        assert_eq!(records.changes, [Change { number: 7, state: State::Closed, at: 950 }]);
        assert!(decide(&c, &snap(r), 1030).1.is_empty(), "the line is said once");
    }

    #[test_id(tru75t0)]
    #[test]
    fn a_review_published_and_closed_in_one_tick_is_published_before_it_is_closed() {
        let mut r = review(vec![]);
        r.reviews = Some(vec![submitted(false, "COMMENTED")]);
        r.pull = Some(Pull { state: "closed".into(), closed_at: Some(950) });
        let (_, _, records) = decide(&Carry::default(), &snap(r), 1000);
        assert_eq!(
            records.changes,
            [Change { number: 7, state: State::Published, at: 900 }, Change { number: 7, state: State::Closed, at: 950 }]
        );
    }

    #[test_id(tf629vc)]
    #[test]
    fn an_unanswered_forge_leaves_the_carry_unchanged() {
        let (c, _, _) = decide(&Carry::default(), &snap(review(vec![person(41, "why")])), 1000);
        let silent = ReviewObs { number: 7, pr: 3, state: Some(State::Open), ..ReviewObs::default() };
        let (after, said, records) = decide(&c, &snap(silent), 1030);
        assert_eq!((after, said, records.is_empty()), (c, vec![], true));
    }

    #[test_id(tgdofib)]
    #[test]
    fn the_wait_is_hot_within_the_window_of_the_last_comment_and_cold_after() {
        let cfg = CrComments { hot_ms: 30_000, cold_ms: 300_000, window_secs: 900 };
        let (c, _, _) = decide(&Carry::default(), &snap(review(vec![person(41, "why")])), 1000);
        assert_eq!((cfg.next_in_ms(&c, 1000), cfg.next_in_ms(&c, 1900), cfg.next_in_ms(&c, 1901)), (30_000, 30_000, 300_000));
        assert_eq!(cfg.next_in_ms(&Carry::default(), 1000), 300_000);
    }
}
