use std::sync::{Arc, Mutex};

use cr::record::{Kind, State};
use runtime::taskrig::TaskRig;
use spira_runtime::watchers::cr_cadence::*;
use spira_runtime::watchers::INBOX_APPEND;

const HOUR_MS: u64 = 3_600_000;
const OFFSET: u64 = 9 * 3600; // the zone is UTC+9 all year, so the virtual epoch 0 is 09:00 local
const OPEN_HOUR: u64 = 7;

fn config() -> CrCadence {
    CrCadence { interval_ms: 300_000, stale_days: 7 }
}

fn review(number: u32, kind: Kind, state: State, closed_at: Option<u64>) -> ReviewObs {
    ReviewObs { number, kind, state, staged_at: 0, closed_at }
}

/// What the world's observe makes of the reviews on disk, for a zone that is a fixed offset from UTC.
fn due_at(reviews: &[ReviewObs], anchor: u64) -> u64 {
    let base = reviews.iter().filter(|r| r.kind == Kind::Scheduled).max_by_key(|r| r.number).and_then(|r| r.closed_at);
    let day_after = |at: u64| ((at + OFFSET) / 86_400 + 1) * 86_400 + OPEN_HOUR * 3600 - OFFSET;
    match base {
        Some(closed) => day_after(closed),
        None => day_after(anchor),
    }
}

struct Rig {
    rig: TaskRig,
    reviews: Arc<Mutex<Vec<ReviewObs>>>,
}

fn rig() -> Rig {
    let reviews = Arc::new(Mutex::new(vec![review(1, Kind::Scheduled, State::Open, None)]));
    let seen = reviews.clone();
    let opened = reviews.clone();
    let (_, stream, kind) = CrCadence::boot();
    let mut rig = TaskRig::new(1, config().registration())
        .unwrap()
        .double(OBSERVE, move |_| {
            let rs = seen.lock().unwrap().clone();
            Ok(serde_json::to_string(&Snapshot { due_at: due_at(&rs, 0), reviews: rs }).unwrap())
        })
        .double(OPEN, move |_| {
            let mut rs = opened.lock().unwrap();
            let n = rs.len() as u32 + 1;
            rs.push(review(n, Kind::Scheduled, State::Staged, None));
            Ok(format!("{n:03} unit\nseal with: cr seal --review {n:03} < comments.json\n"))
        })
        .double(INBOX_APPEND, |l| Ok(l.into()));
    rig.tick(stream, kind).unwrap();
    Rig { rig, reviews }
}

impl Rig {
    fn hours(&mut self, n: u64) {
        for _ in 0..n {
            self.rig.advance(HOUR_MS).unwrap();
        }
    }

    fn now_secs(&self) -> u64 {
        self.rig.clock.now_ms() / 1000
    }

    fn opened_at(&self) -> Vec<u64> {
        self.rig.requested().iter().filter(|q| q.name == OPEN).map(|q| q.at).collect()
    }

    fn said(&self) -> Vec<String> {
        self.rig.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
    }
}

#[test]
fn nothing_opens_while_the_scheduled_review_stays_open_for_three_days() {
    let mut r = rig();
    r.hours(72);
    assert_eq!(r.opened_at(), Vec::<u64>::new());
}

#[test]
fn a_close_at_three_in_the_afternoon_opens_the_next_review_once_at_seven_the_next_morning() {
    let mut r = rig();
    r.hours(72 + 6);
    let closed = r.now_secs();
    r.reviews.lock().unwrap()[0] = review(1, Kind::Scheduled, State::Closed, Some(closed));
    r.hours(30);
    assert_eq!(r.opened_at(), [(closed + 16 * 3600) * 1000]);
    assert_eq!(r.said().len(), 1, "{:?}", r.said());
    assert!(r.said()[0].ends_with("CR 002 staged (scheduled): seal with cr seal --review 002"), "{:?}", r.said());
}

#[test]
fn on_demand_reviews_opened_in_between_change_nothing() {
    let mut r = rig();
    r.hours(72 + 6);
    let closed = r.now_secs();
    r.reviews.lock().unwrap()[0] = review(1, Kind::Scheduled, State::Closed, Some(closed));
    r.hours(5);
    r.reviews.lock().unwrap().push(review(2, Kind::OnDemand, State::Open, None));
    r.hours(5);
    r.reviews.lock().unwrap().push(review(3, Kind::OnDemand, State::Open, None));
    r.hours(20);
    assert_eq!(r.opened_at(), [(closed + 16 * 3600) * 1000]);
}

#[test]
fn a_failed_open_is_said_with_cr_stderr_and_tried_again_at_the_next_tick() {
    let reviews = Arc::new(Mutex::new(vec![review(1, Kind::Scheduled, State::Closed, Some(0))]));
    let seen = reviews.clone();
    let (_, stream, kind) = CrCadence::boot();
    let mut rig = TaskRig::new(1, config().registration())
        .unwrap()
        .double(OBSERVE, move |_| {
            let rs = seen.lock().unwrap().clone();
            Ok(serde_json::to_string(&Snapshot { due_at: due_at(&rs, 0), reviews: rs }).unwrap())
        })
        .double(OPEN, |_| Err("cr exited 1: only 0 of 1 units are drawable".into()))
        .double(INBOX_APPEND, |l| Ok(l.into()));
    rig.tick(stream, kind).unwrap();
    rig.advance(24 * HOUR_MS).unwrap();
    let tries = rig.requested().iter().filter(|q| q.name == OPEN).count();
    assert!(tries >= 2, "{tries}");
    assert!(rig.requested().iter().any(|q| q.name == INBOX_APPEND && q.input.contains("only 0 of 1 units are drawable")));
}

#[test]
fn the_row_is_registered_with_its_cadence_and_effects() {
    let t = spira_runtime::tasks::d::acceptance::row(NAME);
    let s = spira_runtime::tasks::d::acceptance::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.stream(), t.effects), (STREAM, &[OBSERVE, OPEN, INBOX_APPEND][..]));
    assert_eq!((t.cadence_ms)(&s), 5000.max(s.cr_cadence.interval_ms));
    assert_eq!((s.cr_cadence.stale_days, s.world.cr_tz.as_str(), s.world.cr_open_hour.as_str()), (7, "Pacific/Auckland", "6"));
}
