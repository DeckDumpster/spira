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

/// The intake's real client, answered by this verb.
struct Served(std::sync::Arc<Rig2>);

struct Rig2 {
    s: Store<MemBackend>,
    n: std::sync::atomic::AtomicU64,
}

impl spira_store::lc_call::Transport for Served {
    fn exchange(&self, argv: &[String], _: Option<&str>, _: std::time::Duration) -> Result<spira_store::lc_call::Reply, String> {
        let req = format!("c{}", self.0.n.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
        let (code, text) = state::run(&self.0.s, argv, &Call { now: AT, request: &req, inputs: &crate::control::claimable::FIXED });
        let (out, err) = if code == 0 { (text, String::new()) } else { (String::new(), text) };
        Ok(spira_store::lc_call::Reply { code, out, err, read_ended: false })
    }
}

#[test_id(tenqx1e)]
#[test]
fn the_intakes_real_rows_client_speaks_every_subcommand_the_verb_serves() {
    use incident::ports::Bd as _;
    use spira_store::incident::{Payload, Recorded, State};
    let store = std::sync::Arc::new(Rig2 { s: Rig::new().s, n: Default::default() });
    let bd = incident::real::RealBd { lc: Some(spira_store::lc_call::LcClient::over(std::sync::Arc::new(Served(store)), true)) };
    let rows = bd.incident_rows();
    let payload = Payload { reference: "incident:a/b".into(), title: "-t with spaces".into(), body: "--reason\nbody".into(), labels: "spira,incident".into(), express: true, actor: Some("czar".into()), lookback_days: Some(9), ..Payload::default() };
    let key = rows.raise(&payload).unwrap();
    assert_eq!(key, spira_store::incident::key("incident:a/b"));
    let claimed = rows.claim(&key, "me", 600).unwrap();
    assert_eq!((claimed.state, claimed.payload), (State::Filing, payload.clone()));
    let why = rows.claim(&key, "you", 600).unwrap_err();
    assert!(why.contains("state filing"), "{why}");

    rows.record(&key, &Recorded::Refused { reason: "--walled".into(), lanes: Some("plan".into()) }).unwrap();
    let held = rows.list().unwrap();
    assert_eq!((held[0].0.as_str(), held[0].1.state, held[0].1.reason.as_deref(), held[0].1.lanes.as_deref()), (key.as_str(), State::Refused, Some("--walled"), Some("plan")));
    assert_eq!(rows.retry(Some(&key)).unwrap(), 1);
    rows.claim(&key, "me", 600).unwrap();
    rows.record(&key, &Recorded::Unread { reason: "down".into(), retry_at: AT + 60 }).unwrap();
    let held = rows.list().unwrap();
    assert_eq!((held[0].1.state, held[0].1.retry_at, held[0].1.tries), (State::Unread, Some(AT + 60), 1));
    assert_eq!(rows.retry(None).unwrap(), 1);
    rows.claim(&key, "me", 600).unwrap();
    rows.raise(&payload).unwrap();
    rows.record(&key, &Recorded::Filed { bead: Some("sp-1".into()) }).unwrap();
    assert_eq!(rows.list().unwrap()[0].1.bead.as_deref(), Some("sp-1"));
    rows.claim(&key, "me", 600).unwrap();
    rows.record(&key, &Recorded::Filed { bead: None }).unwrap();
    assert!(rows.list().unwrap().is_empty());
}
