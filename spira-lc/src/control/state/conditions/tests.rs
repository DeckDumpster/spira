use std::cell::Cell;

use runtime::ports::VirtualClock;
use serde_json::Value;
use state_store::{Config, Contract, MemBackend, Ports};

use crate::control::state::{self, Call};
use crate::control::store::Store;

const AT: i64 = 1_767_225_600;

struct Rig {
    s: Store<MemBackend>,
    n: Cell<u64>,
    now: Cell<i64>,
}

impl Rig {
    fn new() -> Self {
        let c = Contract::open(
            MemBackend::new(),
            Ports::virtual_clock(&VirtualClock::new(AT as u64 * 1_000)),
            Config { min_lock_ttl_ms: 1_000 },
        )
        .unwrap();
        Rig { s: Store::new(c, 1_000), n: Cell::new(0), now: Cell::new(AT) }
    }

    fn lc(&self, argv: &[&str]) -> (i32, String) {
        self.n.set(self.n.get() + 1);
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let req = format!("r{}", self.n.get());
        state::run(&self.s, &args, &Call { now: self.now.get(), request: &req })
    }

    fn ok(&self, argv: &[&str]) {
        let (rc, out) = self.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
    }

    fn listed(&self, extra: &[&str]) -> Vec<Value> {
        let mut argv = vec!["condition", "list"];
        argv.extend_from_slice(extra);
        let (rc, out) = self.lc(&argv);
        assert_eq!(rc, 0, "{out}");
        let v: Value = serde_json::from_str(&out).unwrap();
        v["conditions"].as_array().unwrap().clone()
    }

    fn one(&self, key: &str) -> Value {
        self.listed(&[]).into_iter().find(|c| c["key"] == key).unwrap_or(Value::Null)
    }

    fn raw(&self, key: &str) -> Option<state_store::Item> {
        self.s.contract().get(&crate::control::keys::condition("heimdall", key)).unwrap()
    }
}

fn raise(r: &Rig, key: &str, msg: &str) {
    r.ok(&["condition", "set", key, "--type", "Alert", "--reason", "Firing", "--message", msg]);
}

#[test]
fn a_raise_then_a_clear_moves_the_condition_true_then_false_at_its_own_key() {
    let r = Rig::new();
    raise(&r, "db-down", "the db is down");
    let c = r.one("db-down");
    assert_eq!((c["status"].as_str(), c["type"].as_str(), c["flaps"].as_u64()), (Some("True"), Some("Alert"), Some(1)));
    assert_eq!(c["first_seen"].as_i64(), Some(AT));
    let raised_at = r.raw("db-down").unwrap().mod_rev.main;
    assert_eq!(c["last_transition_rev"].as_u64(), Some(raised_at));

    r.now.set(AT + 60);
    r.ok(&["condition", "clear", "db-down", "--reason", "Cleared"]);
    let c = r.one("db-down");
    assert_eq!((c["status"].as_str(), c["reason"].as_str()), (Some("False"), Some("Cleared")));
    assert_eq!(c["message"].as_str(), Some("the db is down"), "a clear without a message keeps the last one");
    let cleared_at = r.raw("db-down").unwrap().mod_rev.main;
    assert!(cleared_at > raised_at);
    assert_eq!(c["last_transition_rev"].as_u64(), Some(cleared_at));
}

#[test]
fn a_refresh_keeps_the_transition_and_a_reraise_counts_a_flap_from_the_first_sighting() {
    let r = Rig::new();
    raise(&r, "k", "one");
    let first = r.one("k")["last_transition_rev"].as_u64();
    r.now.set(AT + 10);
    raise(&r, "k", "two");
    let c = r.one("k");
    assert_eq!((c["message"].as_str(), c["flaps"].as_u64()), (Some("two"), Some(1)));
    assert_eq!(c["last_transition_rev"].as_u64(), first, "a message refresh is not a transition");

    r.ok(&["condition", "ack", "k"]);
    assert_eq!(r.one("k")["acked"], true);
    r.ok(&["condition", "clear", "k", "--reason", "Cleared"]);
    r.now.set(AT + 100);
    raise(&r, "k", "three");
    let c = r.one("k");
    assert_eq!((c["flaps"].as_u64(), c["first_seen"].as_i64()), (Some(2), Some(AT)));
    assert_eq!(c["acked"], false, "a new firing is unseen");
}

#[test]
fn a_clear_of_nothing_writes_nothing_and_list_filters_by_status_and_type() {
    let r = Rig::new();
    r.ok(&["condition", "clear", "never", "--reason", "Cleared"]);
    assert!(r.raw("never").is_none());
    raise(&r, "a", "x");
    r.ok(&["condition", "set", "probe", "--type", "WriteProbe", "--reason", "Probe", "--message", "1", "--status", "False"]);
    let firing: Vec<String> = r.listed(&["--status", "True"]).iter().map(|c| c["key"].as_str().unwrap().to_string()).collect();
    assert_eq!(firing, ["a"]);
    let alerts = r.listed(&["--type", "Alert"]);
    assert_eq!(alerts.len(), 1);
    assert_eq!(r.one("probe")["flaps"].as_u64(), Some(0), "a False write is no firing");
}

#[test]
fn silence_sets_and_lifts_a_deadline_and_a_missing_condition_is_refused() {
    let r = Rig::new();
    raise(&r, "k", "x");
    r.ok(&["condition", "silence", "k", "--until", "1767229200"]);
    assert_eq!(r.one("k")["silent_until"].as_i64(), Some(1_767_229_200));
    r.ok(&["condition", "silence", "k", "--lift"]);
    assert!(r.one("k").get("silent_until").is_none());
    assert_eq!(r.lc(&["condition", "ack", "nope"]).0, crate::REFUSED);
    assert_eq!(r.lc(&["condition", "set", "--bad"]).0, crate::CANNOT_TELL);
}
