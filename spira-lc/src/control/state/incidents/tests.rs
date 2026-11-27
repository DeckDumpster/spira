use std::cell::Cell;

use runtime::ports::VirtualClock;
use serde_json::{json, Value};
use state_store::{Config, Contract, MemBackend, Ports};

use crate::control::state::{self, Call};
use crate::control::store::Store;

const AT: i64 = 1_767_225_600;

struct Rig {
    s: Store<MemBackend>,
    n: Cell<u64>,
}

impl Rig {
    fn new() -> Self {
        let c = Contract::open(MemBackend::new(), Ports::virtual_clock(&VirtualClock::new(AT as u64 * 1_000)), Config { min_lock_ttl_ms: 1_000 }).unwrap();
        Rig { s: Store::new(c, 1_000), n: Cell::new(0) }
    }

    fn lc(&self, argv: &[&str]) -> (i32, String) {
        self.n.set(self.n.get() + 1);
        let args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let req = format!("r{}", self.n.get());
        state::run(&self.s, &args, &Call { now: AT, request: &req, inputs: &crate::control::claimable::FIXED })
    }

    fn ok(&self, argv: &[&str]) -> Value {
        let (rc, out) = self.lc(argv);
        assert_eq!(rc, 0, "{argv:?}: {out}");
        serde_json::from_str(&out).unwrap()
    }

    fn raise(&self, body: &str) -> Value {
        let payload = json!({ "reference": "incident:a/b", "title": "t", "body": body }).to_string();
        self.ok(&["incidents", "raise", &payload])
    }

    fn listed(&self) -> Vec<Value> {
        self.ok(&["incidents", "list"]).as_array().unwrap().clone()
    }
}

#[test_id(t3rqy6h)]
#[test]
fn two_raises_of_one_reference_are_one_row_counting_two_events() {
    let r = Rig::new();
    assert_eq!(r.raise("one")["key"], "incident:a%2Fb");
    assert_eq!(r.raise("two")["events"], 2);
    let rows = r.listed();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0]["events"].clone(), rows[0]["body"].clone(), rows[0]["state"].clone()), (json!(2), json!("two"), json!("pending")));
}

#[test_id(tel9v92)]
#[test]
fn of_two_claims_one_succeeds_and_one_is_refused_naming_the_state() {
    let r = Rig::new();
    r.raise("p");
    let claim = ["incidents", "claim", "incident:a%2Fb", "--by", "a", "--for", "600"];
    assert_eq!(r.ok(&claim)["state"], "filing");
    let (rc, out) = r.lc(&claim);
    assert_eq!(rc, crate::REFUSED, "{out}");
    assert!(out.contains("state filing"), "{out}");
}

#[test_id(td59011)]
#[test]
fn a_filed_record_deletes_the_row_unless_a_newer_event_arrived_under_the_claim() {
    let r = Rig::new();
    r.raise("one");
    r.ok(&["incidents", "claim", "incident:a%2Fb", "--by", "a", "--for", "600"]);
    r.raise("two");
    r.ok(&["incidents", "record", "incident:a%2Fb", "filed", "sp-1"]);
    let rows = r.listed();
    assert_eq!((rows[0]["state"].clone(), rows[0]["bead"].clone()), (json!("pending"), json!("sp-1")));

    r.ok(&["incidents", "claim", "incident:a%2Fb", "--by", "a", "--for", "600"]);
    r.ok(&["incidents", "record", "incident:a%2Fb", "filed", "sp-1"]);
    assert!(r.listed().is_empty());
}

#[test_id(tfjq84w)]
#[test]
fn a_refusal_stays_on_the_row_until_a_retry_makes_it_pending() {
    let r = Rig::new();
    r.raise("p");
    r.ok(&["incidents", "record", "incident:a%2Fb", "refused", "--reason", "walled", "--lanes", "plan"]);
    r.raise("again");
    let rows = r.listed();
    assert_eq!((rows[0]["state"].clone(), rows[0]["reason"].clone(), rows[0]["lanes"].clone()), (json!("refused"), json!("walled"), json!("plan")));
    assert_eq!(r.ok(&["incidents", "list", "--state", "pending"]), json!([]));
    assert_eq!(r.ok(&["incidents", "retry"])["retried"], 1);
    assert_eq!(r.listed()[0]["state"], "pending");
    assert_eq!(r.lc(&["incidents", "record", "nope", "filed", "sp-1"]).0, crate::REFUSED);
}
