use testkit::TempDir;


use crate::machine::{Event, Pilgrimage, State, STATES};
use crate::*;


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
fn a_bead_id_cannot_name_a_path() {
    let tmp = TempDir::new("pilgrimage-id");
    let store = FileStore::new(tmp.path().to_path_buf());
    assert!(store.record("../x", Event::Opened, 1).is_err());
}
