use std::cell::RefCell;
use testkit::TempDir;

use spira_otel::metrics::fixture::capture;

use crate::machine::{Event, Pilgrimage, State, STATES};
use crate::*;

struct Stub(RefCell<Vec<(String, String)>>, bool);

impl Concierge for Stub {
    fn ask(&self, id: &str, subject: &str, body: &str) -> Result<String, String> {
        if self.1 {
            return Err("mail down".into());
        }
        self.0.borrow_mut().push((subject.into(), body.into()));
        Ok(format!("ask-for-{id}"))
    }
}


fn walk(p: &mut Pilgrimage, events: Vec<Event>) {
    for (i, e) in events.into_iter().enumerate() {
        p.record(e, 100 + i as u64).unwrap();
    }
}

#[test]
fn the_full_path_visits_every_state_in_order() {
    let mut p = Pilgrimage::new("sp-x");
    let mut seen = vec![];
    for e in [
        Event::Opened,
        Event::WalkStarted { n: 1, landed_at: 50 },
        Event::DefectsFiled { count: 2 },
        Event::Waiting,
        Event::WalkStarted { n: 2, landed_at: 50 },
        Event::Clean,
        Event::ConciergeWalkAsked { ask: "a".into() },
        Event::Done,
    ] {
        p.record(e, 1).unwrap();
        seen.push(p.state().unwrap());
    }
    for s in STATES {
        assert!(seen.contains(&s), "{} never visited", s.name());
    }
    assert_eq!((p.walks(), p.defects()), (2, 2));
}

#[test]
fn an_illegal_move_is_refused_naming_the_state_and_writes_nothing() {
    let mut p = Pilgrimage::new("sp-x");
    walk(&mut p, vec![Event::Opened]);
    let before = p.clone();
    for bad in [Event::Clean, Event::Done, Event::DefectsFiled { count: 1 }, Event::Opened, Event::WalkStarted { n: 2, landed_at: 0 }] {
        let e = p.record(bad, 9).unwrap_err();
        assert!(e.contains("waiting"), "{e}");
    }
    assert_eq!(p, before);
    walk(&mut p, vec![Event::WalkStarted { n: 1, landed_at: 0 }]);
    assert!(p.record(Event::DefectsFiled { count: 0 }, 9).is_err());
    assert_eq!(p.state(), Some(State::Walking));
}

#[test]
fn records_round_trip_through_json() {
    let mut p = Pilgrimage::new("sp-x");
    walk(&mut p, vec![Event::Opened, Event::WalkStarted { n: 1, landed_at: 7 }, Event::Clean, Event::ConciergeWalkAsked { ask: "m1".into() }]);
    assert_eq!(Pilgrimage::from_json(&p.to_json().to_string()).unwrap(), p);
    assert!(Pilgrimage::from_json("{\"id\":\"x\",\"events\":[{\"event\":\"bogus\"}]}").is_err());
}

#[test]
fn a_clean_close_files_the_concierge_walk_ask_once_and_pushes_the_points() {
    let tmp = TempDir::new("pilgrimage-clean");
    let store = FileStore::new(tmp.path().to_path_buf());
    let stub = Stub(RefCell::default(), false);
    let (res, samples) = capture(|sink| {
        record(&store, sink, "sp-x", Event::Opened, 100).unwrap();
        record(&store, sink, "sp-x", Event::WalkStarted { n: 1, landed_at: 110 }, 120).unwrap();
        record(&store, sink, "sp-x", Event::DefectsFiled { count: 3 }, 130).unwrap();
        record(&store, sink, "sp-x", Event::Waiting, 140).unwrap();
        record(&store, sink, "sp-x", Event::WalkStarted { n: 2, landed_at: 110 }, 150).unwrap();
        let ask = close_clean(&store, sink, &stub, "sp-x", 200);
        let again = close_clean(&store, sink, &stub, "sp-x", 210);
        (ask, again)
    });
    assert_eq!(res.0.unwrap(), "ask-for-sp-x");
    assert!(res.1.is_err());
    assert_eq!(stub.0.borrow().len(), 1);
    assert!(stub.0.borrow()[0].0.contains("sp-x") && stub.0.borrow()[0].1.contains("2 walk(s) and 3 defect(s)"));
    assert_eq!(store.load("sp-x").unwrap().state(), Some(State::ConciergeWalk));

    let get = |name: &str| samples.iter().filter(|s| s.name == name).collect::<Vec<_>>();
    assert_eq!(get(STARTED).len(), 1, "started counts the first walk only");
    assert_eq!(get(CLOSED).len(), 1);
    assert_eq!(get(DEFECTS_FILED)[0].value, 3.0);
    assert_eq!(get(CLEAN_SECONDS)[0].value, 90.0);
    assert!(get(CLEAN_SECONDS)[0].label("pilgrimage") == Some("sp-x"));
    let last_walks = get(WALKS).into_iter().map(|s| s.value).fold(0.0, f64::max);
    assert_eq!(last_walks, 2.0);
    let states: Vec<_> = get(TRANSITIONS).iter().filter_map(|s| s.label("state").map(str::to_string)).collect();
    for s in ["waiting", "walking", "defects-filed", "clean", "concierge-walk"] {
        assert!(states.iter().any(|x| x == s), "no transition point for {s}: {states:?}");
    }
}

#[test]
fn a_failed_ask_leaves_the_pilgrimage_clean_and_handoff_retries_it() {
    let tmp = TempDir::new("pilgrimage-retry");
    let store = FileStore::new(tmp.path().to_path_buf());
    let down = Stub(RefCell::default(), true);
    let up = Stub(RefCell::default(), false);
    let ((), _) = capture(|sink| {
        record(&store, sink, "sp-y", Event::Opened, 1).unwrap();
        record(&store, sink, "sp-y", Event::WalkStarted { n: 1, landed_at: 1 }, 2).unwrap();
        let e = close_clean(&store, sink, &down, "sp-y", 3).unwrap_err();
        assert!(e.contains("pilgrimage handoff sp-y"), "{e}");
        assert_eq!(store.load("sp-y").unwrap().state(), Some(State::Clean));
        hand_off(&store, sink, &up, "sp-y", 4).unwrap();
    });
    assert_eq!(store.load("sp-y").unwrap().ask(), Some("ask-for-sp-y"));
}

#[test]
fn a_bead_id_cannot_name_a_path() {
    let tmp = TempDir::new("pilgrimage-id");
    let store = FileStore::new(tmp.path().to_path_buf());
    assert!(store.record("../x", Event::Opened, 1).is_err());
}
