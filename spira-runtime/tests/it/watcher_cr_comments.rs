use std::sync::{Arc, Mutex};

use cr::record::{self, Kind, Review, State};
use cr::thread::Comment;
use runtime::taskrig::TaskRig;
use spira_runtime::watchers::cr_comments::*;
use spira_runtime::watchers::INBOX_APPEND;
use spira_runtime::world::{cr_snapshot_from, World};

const HOT: u64 = 30_000;
const COLD: u64 = 300_000;
const MIN: u64 = 60_000;

fn config() -> CrComments {
    CrComments { hot_ms: HOT, cold_ms: COLD, window_secs: 900 }
}

fn review(number: u32, pr: Option<u64>, state: State) -> Review {
    let unit = cr::units::Unit { id: "u".into(), kind: cr::units::Kind::Module, files: vec![], title: "t".into() };
    Review { number, kind: Kind::OnDemand, state, source_repo: "src".into(), source_sha: "c0ffee".into(), branch: "b".into(), pr, pr_url: None, unit, sealed_sha256: None, staged_at: 0, opened_at: None, published_at: None, closed_at: None }
}

fn comment(id: u64) -> Comment {
    Comment { id, author: "reviewer".into(), path: Some("a.rs".into()), line: Some(2), body: "why".into(), ..Comment::default() }
}

struct Forge {
    comments: Vec<Comment>,
    asked: usize,
}

/// A tick's observe, as the world makes it, over the reviews on disk and a forge double.
fn rig(open: Vec<Review>, forge: Arc<Mutex<Forge>>) -> TaskRig {
    let (_, stream, kind) = CrComments::boot();
    let mut r = TaskRig::new(1, config().registration())
        .unwrap()
        .double(OBSERVE, move |_| {
            let snap = cr_snapshot_from(&open, &|pr| {
                let mut f = forge.lock().unwrap();
                f.asked += 1;
                ReviewObs { pr, comments: Some(f.comments.clone()), conversation: Some(vec![]), reviews: Some(vec![]), pull: Some(Pull { state: "open".into(), closed_at: None }), ..ReviewObs::default() }
            });
            Ok(serde_json::to_string(&snap).unwrap())
        })
        .double(INBOX_APPEND, |l| Ok(l.into()))
        .double(RECORD, |r| Ok(r.into()));
    r.tick(stream, kind).unwrap();
    r
}

fn said(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

fn ticks_at(r: &TaskRig) -> Vec<u64> {
    r.requested().iter().filter(|q| q.name == OBSERVE).map(|q| q.at).collect()
}

#[test_id(t80ofbg)]
#[test]
fn the_next_tick_is_thirty_seconds_after_a_tick_that_saw_a_new_comment() {
    let forge = Arc::new(Mutex::new(Forge { comments: vec![comment(1)], asked: 0 }));
    let r = rig(vec![review(1, Some(7), State::Open)], forge);
    assert_eq!(said(&r).len(), 1, "{:?}", said(&r));
    assert_eq!(r.engine().next_timer_at(), Some(HOT));
}

#[test_id(tq6u906)]
#[test]
fn it_stays_thirty_seconds_at_fourteen_minutes_and_is_five_minutes_at_sixteen() {
    let forge = Arc::new(Mutex::new(Forge { comments: vec![comment(1)], asked: 0 }));
    let mut r = rig(vec![review(1, Some(7), State::Open)], forge);
    let wait_after_last_tick = |r: &TaskRig| r.engine().next_timer_at().unwrap() - *ticks_at(r).last().unwrap();
    r.advance(14 * MIN).unwrap();
    assert_eq!(wait_after_last_tick(&r), HOT);
    r.advance(2 * MIN).unwrap();
    assert_eq!(wait_after_last_tick(&r), COLD);
}

#[test_id(t0xygzr)]
#[test]
fn a_repeated_comment_is_said_once() {
    let forge = Arc::new(Mutex::new(Forge { comments: vec![comment(1)], asked: 0 }));
    let mut r = rig(vec![review(1, Some(7), State::Open)], forge);
    r.advance(5 * MIN).unwrap();
    assert_eq!(said(&r).len(), 1);
}

#[test_id(tjkhxo8)]
#[test]
fn with_no_review_open_the_forge_is_never_asked_and_the_wait_is_cold() {
    let forge = Arc::new(Mutex::new(Forge { comments: vec![comment(1)], asked: 0 }));
    let closed = review(2, Some(8), State::Closed);
    let staged = review(3, None, State::Staged);
    let r = rig(vec![closed, staged], forge.clone());
    assert_eq!(forge.lock().unwrap().asked, 0);
    assert_eq!(r.engine().next_timer_at(), Some(COLD));
    assert!(said(&r).is_empty());
}

#[test_id(t7cno30)]
#[test]
fn an_open_review_without_a_pull_request_is_not_asked_about() {
    let forge = Arc::new(Mutex::new(Forge { comments: vec![], asked: 0 }));
    rig(vec![review(1, None, State::Open)], forge.clone());
    assert_eq!(forge.lock().unwrap().asked, 0);
}

#[test_id(tny099u)]
#[test]
fn the_record_effect_keeps_the_comments_for_cr_thread_and_moves_the_review_state() {
    let t = testkit::TempDir::new("cr-comments-record");
    record::allocate(t.path()).unwrap();
    record::save(t.path(), &review(1, Some(7), State::Open)).unwrap();
    let world = World { run_dir: t.path().into(), ..World::default() };
    let records = Records { changes: vec![Change { number: 1, state: State::Published, at: 900 }], threads: vec![(1, vec![comment(4)])] };
    world.cr_record(&serde_json::to_string(&records).unwrap()).unwrap();
    let r = record::load(t.path(), 1).unwrap();
    assert_eq!((r.state, r.published_at), (State::Published, Some(900)));
    let kept: Vec<Comment> = serde_json::from_str(&std::fs::read_to_string(cr::thread::comments_path(t.path(), 1)).unwrap()).unwrap();
    assert_eq!(kept, [comment(4)]);
}

#[test_id(teugnxc)]
#[test]
fn a_closed_change_whose_ledger_close_fails_leaves_the_review_state_unchanged() {
    let t = testkit::TempDir::new("cr-comments-close-fails");
    record::allocate(t.path()).unwrap();
    record::save(t.path(), &review(1, Some(7), State::Open)).unwrap();
    let release = std::env::var(spira_config::RELEASE_ENV).ok();
    assert!(release.is_none(), "this test needs a process with no release to resolve cr from");
    let world = World { run_dir: t.path().into(), ..World::default() };
    let records = Records { changes: vec![Change { number: 1, state: State::Closed, at: 900 }], threads: vec![] };
    let e = world.cr_record(&serde_json::to_string(&records).unwrap()).unwrap_err();
    assert!(e.contains("cr"), "{e}");
    assert_eq!(record::load(t.path(), 1).unwrap().state, State::Open);
}

#[test]
fn the_row_is_registered_with_its_cadence_and_effects() {
    let t = spira_runtime::tasks::d::acceptance::row(NAME);
    let s = spira_runtime::tasks::d::acceptance::pinned_settings(&[]).expect("settings from the complete fixture");
    assert_eq!((t.stream(), t.effects), (STREAM, &[OBSERVE, RECORD, INBOX_APPEND][..]));
    assert_eq!((t.cadence_ms)(&s), COLD);
}
