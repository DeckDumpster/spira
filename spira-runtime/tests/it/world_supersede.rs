//! The supersede duty's two world doors, `World::supersede_snapshot` and `World::supersede_act`,
//! against a spira-lc double on a socket that speaks `lc_call`'s protocol: the verbs each sends,
//! the refusals each answers, and that a write repeated is settled rather than failed.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use runtime::RuntimeError;
use spira_runtime::watchers::supersede_duty::{self, Act, Held, Reading, Snapshot, Verb};
use spira_runtime::world::World;

/// The store as the double holds it, and every argv it was sent.
#[derive(Default)]
struct Store {
    held: Vec<(String, String)>,
    states: BTreeMap<String, String>,
    history: BTreeMap<String, String>,
    /// The `manual_hold` each held bead's listed row carries.
    raised: BTreeMap<String, serde_json::Value>,
    /// Verbs the store refuses (exit 3).
    refuse: BTreeSet<String>,
    /// Verbs the store faults on (exit 1).
    fault: BTreeSet<String>,
    sent: Vec<Vec<String>>,
}

impl Store {
    fn respond(&mut self, argv: &[String]) -> runtime::Result<String> {
        self.sent.push(argv.to_vec());
        let verb = argv[0].as_str();
        if self.refuse.contains(verb) {
            return Err(RuntimeError::refused(format!("{verb} refused")));
        }
        if self.fault.contains(verb) {
            return Err(RuntimeError::new(format!("{verb} faulted")));
        }
        let state = |s: &Store, id: &str| s.states.get(id).cloned().unwrap_or_default();
        match verb {
            "list" if argv.get(1).map(String::as_str) == Some("--ids") => {
                let rows: Vec<_> = argv[2].split(',').filter(|id| self.states.contains_key(*id)).map(|id| serde_json::json!({ "bead_id": id, "state": state(self, id) })).collect();
                Ok(serde_json::to_string(&rows).unwrap())
            }
            "list" => Ok(serde_json::to_string(
                &self.held.iter().map(|(id, v)| serde_json::json!({ "bead_id": id, "version": v, "state": state(self, id), "manual_hold": self.raised.get(id) })).collect::<Vec<_>>(),
            )
            .unwrap()),
            "history" => self.history.get(&argv[1]).cloned().ok_or_else(|| RuntimeError::new("no history")),
            "state" => Ok(format!("{}\n", state(self, &argv[1]))),
            "supersede" if supersede_duty::terminal(&state(self, &argv[1])) => Err(RuntimeError::refused(format!("{} is already terminal", argv[1]))),
            "supersede" => {
                self.states.insert(argv[1].clone(), "SUPERSEDED".into());
                Ok("ok".into())
            }
            "unhold" | "bd-supersede" | "bd-note" => Ok("ok".into()),
            _ => Err(RuntimeError::new(format!("the double has no {verb}"))),
        }
    }
}

struct Double {
    _dir: testkit::TempDir,
    world: World,
    store: Arc<Mutex<Store>>,
}

impl Double {
    fn new(store: Store) -> Double {
        let dir = testkit::TempDir::new("world-supersede");
        let path = dir.path().join("lc.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let store = Arc::new(Mutex::new(store));
        let served = store.clone();
        std::thread::spawn(move || runtime::socket::answer(&listener, &|argv| served.lock().unwrap().respond(argv)));
        Double { _dir: dir, world: World { lc_socket: path, ..World::default() }, store }
    }

    /// The verbs sent since the last call, each as its argv joined.
    fn sent(&self) -> Vec<String> {
        std::mem::take(&mut self.store.lock().unwrap().sent).into_iter().map(|a| a.join(" ")).collect()
    }
}

fn hold_history(cause: &str, detail: &str) -> String {
    let evidence = serde_json::json!({ "Hold": { "kind": "manual", "cause": cause, "detail": detail } }).to_string();
    serde_json::json!([{ "applied": "1", "evidence": evidence }]).to_string()
}

fn raise(cause: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({ "at": 100, "cause": cause, "detail": detail })
}

fn act(verb: Verb) -> String {
    serde_json::to_string(&Act { id: "sp-old".into(), succ: "sp-new".into(), verb, note: "superseded by sp-new".into(), done: String::new(), fail: String::new() }).unwrap()
}

fn store() -> Store {
    Store {
        held: vec![("sp-old".into(), "4".into()), ("sp-other".into(), "2".into())],
        states: [("sp-old".to_string(), "CERTIFIED".to_string()), ("sp-new".to_string(), "SUBMITTED".to_string()), ("sp-other".to_string(), "READY".to_string())].into(),
        history: [("sp-old".to_string(), hold_history("supersede-request", "sp-new")), ("sp-other".to_string(), hold_history("parked", "waiting"))].into(),
        raised: [("sp-old".to_string(), raise("supersede-request", "sp-new")), ("sp-other".to_string(), raise("manual-hold", "waiting"))].into(),
        ..Store::default()
    }
}

#[test_id(t0fklrr)]
#[test]
fn the_snapshot_lists_the_manual_holds_and_reads_each_unknown_one() {
    let d = Double::new(store());
    let snap: Snapshot = serde_json::from_str(&d.world.supersede_snapshot("").unwrap()).unwrap();
    assert_eq!(
        snap.held,
        [
            Held { id: "sp-old".into(), version: "4".into(), reading: Reading::Request { succ: "sp-new".into(), own: "CERTIFIED".into(), succ_state: "SUBMITTED".into() } },
            Held { id: "sp-other".into(), version: "2".into(), reading: Reading::NotARequest },
        ]
    );
    assert_eq!(d.sent(), ["list --live --hold manual", "list --ids sp-new"]);
}

#[test_id(tpusfw0)]
#[test]
fn the_snapshot_skips_a_version_already_read_and_reads_it_again_once_it_moves() {
    let d = Double::new(store());
    let known = serde_json::to_string(&BTreeMap::from([("sp-other", "2"), ("sp-old", "3")])).unwrap();
    let snap: Snapshot = serde_json::from_str(&d.world.supersede_snapshot(&known).unwrap()).unwrap();
    assert_eq!(snap.held[1].reading, Reading::Skipped, "{snap:?}");
    assert!(matches!(snap.held[0].reading, Reading::Request { .. }), "a moved version is read again: {snap:?}");
    assert!(!d.sent().iter().any(|v| v.starts_with("history")), "a known version sends no history read");
}

#[test_id(tx11j87)]
#[test]
fn the_snapshot_fails_when_the_list_is_refused_and_when_a_held_row_has_no_manual_hold() {
    let d = Double::new(Store { refuse: ["list".to_string()].into(), ..store() });
    assert!(d.world.supersede_snapshot("").is_err(), "no list, no snapshot");
    let d = Double::new(Store { raised: BTreeMap::new(), held: vec![("sp-old".into(), "4".into())], ..store() });
    let err = d.world.supersede_snapshot("").unwrap_err();
    assert!(err.contains("manual_hold") && err.contains("migration"), "{err}");
    assert!(!d.sent().iter().any(|v| v.starts_with("history")), "never a history read");
}

#[test_id(t1i35zn)]
#[test]
fn a_held_fleet_is_read_from_its_rows_without_a_history_or_state_call_per_bead() {
    let states = ["LANDED", "SUBMITTED", "READY", "SUPERSEDED"];
    let mut st = Store::default();
    for i in 0..30 {
        let id = format!("sp-h{i:02}");
        st.held.push((id.clone(), "1".into()));
        st.states.insert(id.clone(), if i % 7 == 6 { "LANDED".into() } else { "CERTIFIED".into() });
        if i % 3 == 0 {
            let succ = format!("sp-s{i:02}");
            if i % 9 != 0 {
                st.states.insert(succ.clone(), states[i % 4].into());
            }
            st.history.insert(id.clone(), hold_history("supersede-request", &succ));
            st.raised.insert(id, raise("supersede-request", &succ));
        } else {
            st.history.insert(id.clone(), hold_history("manual-hold", "parked"));
            st.raised.insert(id, raise("manual-hold", "parked"));
        }
    }
    let d = Double::new(st);
    let snap: Snapshot = serde_json::from_str(&d.world.supersede_snapshot("").unwrap()).unwrap();
    let sent = d.sent();
    assert!(sent.iter().all(|v| !v.starts_with("history") && !v.starts_with("state")), "{sent:?}");
    assert!(sent.iter().filter(|v| v.starts_with("list --ids")).count() <= 1, "{sent:?}");
    let reading = |i: usize| snap.held[i].reading.clone();
    assert_eq!(reading(1), Reading::NotARequest);
    assert_eq!(reading(0), Reading::Request { succ: "sp-s00".into(), own: "CERTIFIED".into(), succ_state: String::new() }, "no row for the successor");
    assert_eq!(reading(3), Reading::Request { succ: "sp-s03".into(), own: "CERTIFIED".into(), succ_state: "SUPERSEDED".into() });
    assert_eq!(reading(6), Reading::Request { succ: "sp-s06".into(), own: "LANDED".into(), succ_state: String::new() }, "a terminal bead's successor is not looked up");
    assert_eq!(reading(15), Reading::Request { succ: "sp-s15".into(), own: "CERTIFIED".into(), succ_state: "SUPERSEDED".into() });
    assert_eq!(reading(24), Reading::Request { succ: "sp-s24".into(), own: "CERTIFIED".into(), succ_state: "LANDED".into() });
    assert_eq!(snap.held.iter().filter(|h| matches!(h.reading, Reading::Request { .. })).count(), 10);
}

#[test_id(tkbaepv)]
#[test]
fn a_supersede_writes_the_row_then_the_bd_side_then_the_note() {
    let d = Double::new(store());
    assert_eq!(d.world.supersede_act(&act(Verb::Supersede)), Ok("done".into()));
    assert_eq!(d.sent(), [format!("supersede sp-old sp-new {}", supersede_duty::ACTOR), "bd-supersede sp-old sp-new".into(), "bd-note sp-old superseded by sp-new".into()]);
}

#[test_id(tby0jdr)]
#[test]
fn an_unhold_writes_the_release_and_the_note_and_no_bd_supersede() {
    let d = Double::new(store());
    assert_eq!(d.world.supersede_act(&act(Verb::Unhold)), Ok("done".into()));
    assert_eq!(d.sent(), [format!("unhold sp-old manual {}", supersede_duty::ACTOR), "bd-note sp-old superseded by sp-new".into()]);
}

#[test_id(tr21nx8)]
#[test]
fn a_supersede_repeated_is_settled_and_writes_nothing_more() {
    let d = Double::new(store());
    assert_eq!(d.world.supersede_act(&act(Verb::Supersede)), Ok("done".into()));
    d.sent();
    assert_eq!(d.world.supersede_act(&act(Verb::Supersede)), Ok(supersede_duty::SETTLED.into()), "the bead is terminal under the repeat");
    assert_eq!(d.sent(), [format!("supersede sp-old sp-new {}", supersede_duty::ACTOR), "state sp-old".into()], "no second bd-supersede or note");
}

#[test_id(t165ra8)]
#[test]
fn a_refused_write_on_a_live_bead_fails_and_leaves_no_note() {
    for verb in [Verb::Supersede, Verb::Unhold] {
        let name = if verb == Verb::Supersede { "supersede" } else { "unhold" };
        let d = Double::new(Store { refuse: [name.to_string()].into(), ..store() });
        assert!(d.world.supersede_act(&act(verb)).is_err(), "{name} refused on a live bead is a failure");
        assert!(!d.sent().iter().any(|v| v.starts_with("bd-")), "{name}: nothing written after a refusal");
    }
}

#[test_id(t0d970r)]
#[test]
fn a_bd_side_or_note_that_fails_after_the_row_took_is_flagged_not_failed() {
    let d = Double::new(Store { fault: ["bd-supersede".to_string(), "bd-note".to_string()].into(), ..store() });
    assert_eq!(d.world.supersede_act(&act(Verb::Supersede)), Ok(format!("{} {}", supersede_duty::BD_FAILED, supersede_duty::NOTE_FAILED)));
    let d = Double::new(Store { fault: ["bd-note".to_string()].into(), ..store() });
    assert_eq!(d.world.supersede_act(&act(Verb::Unhold)), Ok(supersede_duty::NOTE_FAILED.into()));
}
