//! The Margin feed pages only when the reader stays unreachable (sp-sxmmoo), on the rig's virtual time.

use runtime::event::*;
use runtime::taskrig::TaskRig;
use spira_runtime::sources::margin_changed::Unreachable;
use spira_runtime::watchers::margin_comments::*;
use spira_runtime::watchers::INBOX_APPEND;

const MIN: u64 = 60_000;

fn rig() -> TaskRig {
    let mut r = TaskRig::new(1, MarginComments { sweep_ms: 3_600_000 }.registration()).unwrap().double(INBOX_APPEND, |l| Ok(l.into())).double(DELIVER, |_| Ok("{}".into()));
    reopened(&mut r);
    r
}

fn reopened(r: &mut TaskRig) {
    let (_, stream, kind) = MarginComments::boot();
    r.tick(stream, kind).unwrap();
}

fn down(r: &mut TaskRig, error: &str) {
    let body = serde_json::to_string(&Unreachable { since_ms: r.now(), error: error.into() }).unwrap();
    r.deliver(MARGIN_STREAM, EventKind::new(MARGIN_UNREACHABLE, &body)).unwrap();
}

fn up(r: &mut TaskRig, body: &str) {
    r.deliver(MARGIN_STREAM, EventKind::new(MARGIN_CHANGED, body)).unwrap();
}

fn lines(r: &TaskRig) -> Vec<String> {
    r.requested().iter().filter(|q| q.name == INBOX_APPEND).map(|q| q.input.clone()).collect()
}

const EMPTY: &str = r#"{"through":9,"changes":[]}"#;

#[test_id(t8qry4o)]
#[test]
fn a_sixty_second_outage_writes_no_line() {
    let mut r = rig();
    down(&mut r, "timed out");
    r.advance(MIN).unwrap();
    up(&mut r, EMPTY);
    r.advance(10 * MIN).unwrap();
    assert_eq!(lines(&r), Vec::<String>::new());
}

#[test_id(t05s6bz)]
#[test]
fn a_four_minute_outage_writes_one_line_of_each_kind_and_a_second_outage_pages_again() {
    let mut r = rig();
    for _ in 0..2 {
        down(&mut r, "Unexpected EOF");
        r.advance(4 * MIN).unwrap();
        down(&mut r, "Unexpected EOF");
        up(&mut r, EMPTY);
    }
    let l = lines(&r);
    assert_eq!(l.len(), 4, "{l:?}");
    assert!(l[0].contains("margin feed unreachable since") && l[0].contains("Unexpected EOF"), "{}", l[0]);
    assert!(l[1].contains("margin feed reachable again after 240s"), "{}", l[1]);
    assert!(l[2].contains("unreachable since") && l[3].contains("reachable again"), "{l:?}");
}

#[test_id(tva8o2l)]
#[test]
fn a_comment_in_the_recovering_answer_requests_one_delivery() {
    let mut r = rig();
    down(&mut r, "x");
    r.advance(4 * MIN).unwrap();
    let c = r#"{"through":9,"changes":[{"seq":3,"kind":"comment_created","collection_id":"c","thread_id":"t","comment_id":"c-1","author":"reader"}]}"#;
    up(&mut r, c);
    assert_eq!(r.requested().iter().filter(|q| q.name == DELIVER).count(), 1);
}

#[test_id(tvwaco2)]
#[test]
fn after_a_restart_mid_outage_a_paged_run_is_not_said_twice_and_its_recovery_is() {
    let mut r = rig();
    down(&mut r, "x");
    r.advance(4 * MIN).unwrap();
    assert_eq!(lines(&r).len(), 1);
    r.reopen(MarginComments { sweep_ms: 3_600_000 }.registration()).unwrap();
    reopened(&mut r);
    down(&mut r, "x");
    r.advance(10 * MIN).unwrap();
    assert_eq!(lines(&r).len(), 1, "{:?}", lines(&r));
    up(&mut r, EMPTY);
    let l = lines(&r);
    assert!(l.len() == 2 && l[1].contains("reachable again"), "{l:?}");
}
