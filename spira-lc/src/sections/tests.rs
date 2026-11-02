use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use runtime::ports::{Clock, VirtualClock};
use serde_json::{json, Value};
use spira_desired_state::compose::{compose, publish_specs};
use spira_desired_state::producer::parse_fragment;
use spira_otel::metrics::MetricsSink;
use state_store::gc::Controller;
use state_store::{
    events_key, Backend, Config, Contract, LogEntry, MemBackend, Object, Ports, Request, Rev,
    Txn, Version, Write,
};

use super::{run, Call, REFUSALS, WRITES};

mod assemble;
mod goodput_pass;

const FLOOR_MS: u64 = 1_000;
/// 2026-10-31T19:40:00Z.
const T0: u64 = 1_793_475_600_000;
const NOW: &str = "2026-10-31T19:40:00Z";
const MIN: u64 = 60_000;
const HOUR: u64 = 60 * MIN;

struct Rig<B: Backend> {
    store: Contract<B>,
    clock: VirtualClock,
    n: Cell<u32>,
    sink: MetricsSink,
}

fn rig_on<B: Backend>(b: B) -> Rig<B> {
    let clock = VirtualClock::new(T0);
    let c = Contract::open(b, Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    Rig {
        store: c,
        clock,
        n: Cell::new(0),
        sink: MetricsSink::new(None, "test", std::time::Duration::from_millis(1)),
    }
}

fn rig() -> Rig<MemBackend> {
    rig_on(MemBackend::new())
}

impl<B: Backend> Rig<B> {
    fn call_with(&self, sink: &MetricsSink, argv: &[&str], body: Option<&Value>) -> (i32, Value) {
        self.n.set(self.n.get() + 1);
        let request = format!("t/{}", self.n.get());
        let mut args: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        if let Some(b) = body {
            args.push("--json".into());
            args.push(b.to_string());
        }
        let call = Call {
            now_ms: self.clock.now_ms(),
            request: &request,
            metrics: sink,
        };
        let (code, out) = run(&self.store, &args, &call);
        let v = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{argv:?} answered non-JSON {out:?}: {e}"));
        (code, v)
    }

    fn call(&self, argv: &[&str], body: Option<&Value>) -> (i32, Value) {
        self.call_with(&self.sink, argv, body)
    }

    fn ok(&self, argv: &[&str], body: Option<&Value>) -> Value {
        let (code, v) = self.call(argv, body);
        assert_eq!(code, 0, "{argv:?}: {v}");
        v
    }

    fn refused(&self, argv: &[&str], body: Option<&Value>) -> String {
        let (code, v) = self.call(argv, body);
        assert_eq!(code, 3, "{argv:?} was not refused: {v}");
        v["refused"]["field"].as_str().unwrap().to_string()
    }

    fn head(&self) -> u64 {
        self.store.head().unwrap()
    }

    fn rev_of(&self, r: &str) -> String {
        let (code, v) = self.call(&["section", "get", r], None);
        if code == 1 {
            return "0".into();
        }
        v["section"]["metadata"]["resourceVersion"].as_str().unwrap().to_string()
    }

    fn publish(&self, body: &Value) -> Value {
        let rev = self.rev_of(&format!("Probe/{}", body["metadata"]["name"].as_str().unwrap()));
        self.ok(&["section", "publish-status", "--actor", "probe", "--expect-rev", &rev], Some(body))
    }

    fn apply_spec(&self, doc: &Value) -> Value {
        self.ok(&["section", "apply-spec", "--actor", "compose"], Some(doc))
    }

    fn lock(&self) -> String {
        let v = self.ok(&["lock", "acquire", "reconciler", "--ttl", "600", "--holder", "goodput-pass"], None);
        v["token"].as_str().unwrap().to_string()
    }

    fn put_knob(&self, key: &str, v: Value) {
        let t = Txn::new().put(key, Object::new(v));
        assert!(self.store.txn(&t, &Request::new(&format!("knob/{key}/{}", self.head()), "test", "t")).unwrap().succeeded);
    }

    fn knob(&self, key: &str) -> Value {
        self.store.get(key).unwrap().unwrap().value.data
    }
}

/// A Probe section the schema admits, observed at `at`.
fn status(name: &str, observed_generation: u64, at: &str) -> Value {
    json!({
        "apiVersion": "spira/v1",
        "kind": "Probe",
        "metadata": { "name": name, "owner": "probe", "observedAt": at, "target_latency": "5m", "max_age": "15m" },
        "status": {
            "observedGeneration": observed_generation,
            "depth": { "value": 3.0, "window": "level" },
            "wedged": { "value": 1.0, "window": "1h" },
            "depth_24h": { "value": 40.0, "window": "24h" },
            "conditions": [{ "type": "Deep", "status": "True", "reason": "3 deep", "lastTransitionTime": NOW }],
        },
        "flow": { "consumes": ["vm-pool/slots"], "produces": ["lifecycle/landed"] },
        "distance": { "fn": "max(0, depth) / 5 + wedged", "open": 1.0, "close": 0.5 },
        "actions": [{
            "name": "drain", "class": "local", "step": "one item", "moves": "depth",
            "addresses": ["Deep"], "expected_effect": -1.0, "horizon": "10m",
        }],
    })
}

fn spec(name: &str, depth_max: f64) -> Value {
    json!({
        "apiVersion": "spira/v1", "kind": "Probe", "metadata": { "name": name },
        "spec": { "depth_max": { "value": depth_max, "statute": "law-x" } },
    })
}

/// `v` with the value at `ptr` set; an object's missing last segment is added.
fn with(mut v: Value, ptr: &str, x: Value) -> Value {
    let (parent, last) = ptr.rsplit_once('/').unwrap_or_else(|| panic!("{ptr}"));
    match v.pointer_mut(parent).unwrap_or_else(|| panic!("{ptr}")) {
        Value::Object(o) => {
            o.insert(last.to_string(), x);
        }
        Value::Array(a) => a[last.parse::<usize>().unwrap_or_else(|_| panic!("{ptr}"))] = x,
        _ => panic!("{ptr}"),
    }
    v
}

fn intent(id: &str, token: &str, from: Value, to: Value) -> Value {
    json!({
        "id": id, "action": "drain", "target": "knobs/Probe/a/concurrency", "from": from, "to": to,
        "moves": "depth", "baseline": 3.0, "expected_effect": -1.0, "horizon": "10m",
        "addresses": ["Deep"], "taken_at": NOW, "by": { "pass": "p-1", "lock_token": token },
    })
}

#[test]
fn admission_refuses_each_malformed_write_naming_its_field_and_the_store_does_not_move() {
    let r = rig();
    let publish = |body: Value| r.refused(&["section", "publish-status", "--actor", "probe", "--expect-rev", "0"], Some(&body));
    let good = || status("a", 0, NOW);
    let cases: Vec<(&str, Value, &str)> = vec![
        ("unknown field", { let mut v = good(); v["metadata"]["bogus"] = json!(1); v }, "metadata"),
        ("unknown status field", { let mut v = good(); v["status"]["bogus"] = json!(1); v }, "status"),
        ("unknown kind", with(good(), "/kind", json!("Warp")), "kind"),
        ("apiVersion", with(good(), "/apiVersion", json!("spira/v2")), "apiVersion"),
        ("24h in an action-bearing distance", with(good(), "/distance/fn", json!("depth + depth_24h")), "distance.fn"),
        ("5m horizon on a 1h field", with(with(good(), "/actions/0/moves", json!("wedged")), "/actions/0/horizon", json!("5m")), "actions[0].horizon"),
        ("set to 4", with(good(), "/actions/0/step", json!("set to 4")), "actions[0].step"),
        ("rebuild", with(good(), "/actions/0/step", json!("rebuild")), "actions[0].step"),
        ("+30", with(good(), "/actions/0/step", json!("+30")), "actions[0].step"),
        ("+N", with(good(), "/actions/0/step", json!("+N")), "actions[0].step"),
        ("stopping", with(good(), "/actions/0/stopping", json!(true)), "actions[0].stopping"),
        ("undeclared condition", with(good(), "/status/conditions/0/type", json!("Lost")), "status.conditions[0].type"),
        ("Andon", with(good(), "/kind", json!("Andon")), "kind"),
    ];
    let head = r.head();
    for (what, body, field) in cases {
        let got = publish(body);
        assert!(got.starts_with(field), "{what}: refused on {got:?}, not {field:?}");
        assert_eq!(r.head(), head, "{what}: the store moved");
    }
    let andon = with(spec("a", 1.0), "/kind", json!("Andon"));
    assert_eq!(r.refused(&["section", "apply-spec", "--actor", "compose"], Some(&andon)), "kind");
    assert_eq!(r.head(), head);
    r.publish(&good());
    assert!(r.head() > head, "the good section is written");
}

#[test]
fn only_the_owner_writes_status_and_only_compose_writes_spec() {
    let r = rig();
    let head = r.head();
    assert_eq!(r.refused(&["section", "publish-status", "--actor", "intruder", "--expect-rev", "0"], Some(&status("a", 0, NOW))), "actor");
    assert_eq!(r.refused(&["section", "apply-spec", "--actor", "probe"], Some(&spec("a", 4.0))), "actor");
    assert_eq!(r.head(), head);

    r.apply_spec(&spec("a", 4.0));
    let mut owner_spec = status("a", 1, NOW);
    owner_spec["spec"] = json!({ "depth_max": { "value": 9.0, "statute": "law-x" } });
    let rev = r.rev_of("Probe/a");
    assert_eq!(r.refused(&["section", "publish-status", "--actor", "probe", "--expect-rev", &rev], Some(&owner_spec)), "spec");
    assert_eq!(r.refused(&["section", "publish-status", "--actor", "probe", "--expect-rev", "999"], Some(&status("a", 1, NOW))), "--expect-rev");
    r.publish(&status("a", 1, NOW));
    let got = r.ok(&["section", "get", "Probe/a"], None);
    assert_eq!(got["section"]["spec"]["depth_max"]["value"], 4.0, "the owner's publish keeps compose's spec");
}

#[test]
fn intents_are_fenced_on_the_reconciler_lock_and_acked_and_judged_once_by_their_owners() {
    let r = rig();
    let token = r.lock();
    let key = "intents/Probe/a/1";
    let put = |key: &str, actor: &str, tok: &str, body: &Value| r.call(&["intent", "put", key, "--token", tok, "--actor", actor], Some(body));

    let (code, v) = put(key, "reconciler", "999.0", &intent("1", "999.0", json!(1), json!(2)));
    assert_eq!((code, v["refused"]["field"].as_str()), (3, Some("--token")), "{v}");
    let (code, _) = put(key, "probe", &token, &intent("1", &token, json!(1), json!(2)));
    assert_eq!(code, 3, "only the reconciler writes an intent");

    let (code, v) = put(key, "reconciler", &token, &intent("1", &token, json!(1), json!(2)));
    assert_eq!(code, 0, "{v}");
    let snap = r.ok(&["section", "snapshot"], None);
    assert_eq!(snap["intents"][0]["key"], key);
    assert_eq!(snap["intents"][0]["intent"]["to"], 2);
    let head = r.head();
    let (code, _) = put(key, "reconciler", &token, &intent("1", &token, json!(1), json!(2)));
    assert_eq!(code, 3, "an intent is written once");
    assert_eq!(r.head(), head, "a second put is refused before its txn");

    let judge = |actor: &str| r.call(&["intent", "judge", key, "--token", &token, "--actor", actor, "--result", "failed", "--observed", "3"], None);
    assert_eq!(judge("reconciler").0, 3, "an unacknowledged intent is not judged");
    assert_eq!(r.refused(&["intent", "ack", key, "--actor", "intruder", "--result", "applied"], None), "actor");
    r.ok(&["intent", "ack", key, "--actor", "probe", "--result", "refused", "--reason", "held"], None);
    assert_eq!(r.refused(&["intent", "ack", key, "--actor", "probe", "--result", "applied"], None), "ack");
    assert_eq!(judge("probe").0, 3, "the owner does not judge");
    let (code, v) = judge("reconciler");
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["judgement"]["result"], "failed");
    assert_eq!(judge("reconciler").0, 3, "an intent is judged once");

    r.ok(&["lock", "release", "reconciler", "--token", &token], None);
    let (code, v) = put("intents/Probe/a/2", "reconciler", &token, &intent("2", &token, json!(1), json!(2)));
    assert_eq!((code, v["refused"]["field"].as_str()), (3, Some("--token")), "a released token fences nothing: {v}");
}

#[test]
fn intent_apply_moves_a_knob_from_to_once_and_acks_in_the_same_txn() {
    let r = rig();
    let token = r.lock();
    let knob = "knobs/Probe/a/concurrency";
    let put = |id: &str, from: Value, to: Value| {
        let key = format!("intents/Probe/a/{id}");
        r.ok(&["intent", "put", &key, "--token", &token, "--actor", "reconciler"], Some(&intent(id, &token, from, to)));
        key
    };
    let apply = |key: &str| r.ok(&["intent", "apply", key, "--knob", knob, "--actor", "probe"], None);

    r.put_knob(knob, json!(2));
    let k1 = put("1", json!(2), json!(3));
    assert_eq!(apply(&k1)["ack"]["result"], "applied");
    assert_eq!(r.knob(knob), json!(3));
    assert_eq!(r.refused(&["intent", "apply", &k1, "--knob", knob, "--actor", "probe"], None), "ack");
    assert_eq!(r.knob(knob), json!(3), "a second apply moves nothing");

    let k2 = put("2", json!(3), json!(4));
    r.put_knob(knob, json!(4));
    let rev = r.store.get(knob).unwrap().unwrap().mod_rev;
    assert_eq!(apply(&k2)["ack"]["result"], "already-applied", "the re-run after a crash between apply and ack");
    let after = r.store.get(knob).unwrap().unwrap();
    assert_eq!((after.value.data, after.mod_rev), (json!(4), rev), "the knob moved once");

    r.put_knob(knob, json!(7));
    let k3 = put("3", json!(4), json!(5));
    let v = apply(&k3);
    assert_eq!(v["ack"]["result"], json!({ "refused": { "reason": "stale", "stale": 7 } }));
    assert_eq!(r.knob(knob), json!(7));

    assert_eq!(r.refused(&["intent", "apply", &k3, "--knob", "lifecycle/sp-x", "--actor", "probe"], None), "--knob");
    let k4 = put("4", json!(7), json!(8));
    assert_eq!(r.refused(&["intent", "apply", &k4, "--knob", "knobs/Probe/a/other", "--actor", "probe"], None), "--knob", "only the intent's own target moves");
    assert_eq!(r.knob(knob), json!(7));
}

#[test]
fn an_absent_knob_is_null_so_a_step_from_null_creates_it_once() {
    let r = rig();
    let token = r.lock();
    let knob = "knobs/Probe/a/concurrency";
    let put = |id: &str, from: Value, to: Value| {
        let key = format!("intents/Probe/a/{id}");
        r.ok(&["intent", "put", &key, "--token", &token, "--actor", "reconciler"], Some(&intent(id, &token, from, to)));
        key
    };
    let apply = |key: &str| r.ok(&["intent", "apply", key, "--knob", knob, "--actor", "probe"], None);

    let stale = put("1", json!(2), json!(3));
    assert_eq!(apply(&stale)["ack"]["result"], json!({ "refused": { "reason": "stale", "stale": null } }));
    assert!(r.store.get(knob).unwrap().is_none(), "a refused step creates nothing");

    let held = json!({ "lifted_by": "reconciler" });
    let create = put("2", Value::Null, held.clone());
    assert_eq!(apply(&create)["ack"]["result"], "applied");
    assert_eq!(r.knob(knob), held);
    let again = put("3", Value::Null, held.clone());
    assert_eq!(apply(&again)["ack"]["result"], "already-applied");
}

#[test]
fn knob_init_sets_an_owners_knob_once_and_answers_an_existing_one_as_it_stands() {
    let r = rig();
    let knob = "knobs/Probe/a/concurrency";
    let init = |v: Value| r.ok(&["intent", "knob-init", knob, "--actor", "probe", "--json", &v.to_string()], None);
    let first = init(json!(2));
    assert_eq!((first["initialized"].clone(), first["value"].clone()), (json!(true), json!(2)));
    let rev = r.store.get(knob).unwrap().unwrap().mod_rev;
    let second = init(json!(5));
    assert_eq!((second["initialized"].clone(), second["value"].clone()), (json!(false), json!(2)));
    assert_eq!(r.store.get(knob).unwrap().unwrap().mod_rev, rev, "an existing knob is not written");

    let body = json!(1).to_string();
    let refused = |args: &[&str]| r.refused(args, None);
    assert_eq!(refused(&["intent", "knob-init", knob, "--actor", "reconciler", "--json", &body]), "actor");
    assert_eq!(refused(&["intent", "knob-init", "knobs/Probe/a", "--actor", "probe", "--json", &body]), "knob");
    assert_eq!(refused(&["intent", "knob-init", "lifecycle/sp-a/x", "--actor", "probe", "--json", &body]), "knob");
    assert_eq!(refused(&["intent", "knob-init", "knobs/Warp/a/x", "--actor", "probe", "--json", &body]), "kind");
    assert_eq!(refused(&["intent", "knob-init", "knobs/Probe/b/x", "--actor", "probe", "--json", "{"]), "body");
    assert!(r.store.get("knobs/Probe/b/x").unwrap().is_none());
}

#[test]
fn freshness_is_age_against_max_age_and_generation_lag_reads_apart() {
    let r = rig();
    r.publish(&status("a", 0, NOW));
    let got = r.ok(&["section", "get", "Probe/a"], None);
    assert_eq!(got["freshness"]["state"], "Fresh");
    r.clock.advance(16 * MIN);
    let got = r.ok(&["section", "get", "Probe/a"], None);
    assert_eq!(got["freshness"]["state"], "Stale");
    assert!(got["freshness"]["reason"].as_str().unwrap().contains("max_age"), "{got}");

    let fresh_at = spira_desired_state::store::format_rfc3339(r.clock.now_ms() / 1000);
    assert_eq!(r.apply_spec(&spec("a", 4.0))["generation"], 1, "a section published before any spec is at generation 0");
    r.publish(&status("a", 1, &fresh_at));
    assert_eq!(r.apply_spec(&spec("a", 4.0))["applied"], false, "an unchanged spec writes nothing");
    assert_eq!(r.apply_spec(&spec("a", 5.0))["generation"], 2, "a spec change bumps generation");
    let v = r.publish(&status("a", 1, &fresh_at));
    assert_eq!(v["generation"], 2, "a status write does not");
    let got = r.ok(&["section", "get", "Probe/a"], None);
    assert_eq!((got["freshness"]["state"].as_str(), got["caught_up"].as_bool()), (Some("Fresh"), Some(false)));
    let rev = r.rev_of("Probe/a");
    assert_eq!(r.refused(&["section", "publish-status", "--actor", "probe", "--expect-rev", &rev], Some(&status("a", 3, &fresh_at))), "status.observedGeneration");
    let ahead = spira_desired_state::store::format_rfc3339(r.clock.now_ms() / 1000 + 120);
    assert_eq!(r.refused(&["section", "publish-status", "--actor", "probe", "--expect-rev", &rev], Some(&status("a", 1, &ahead))), "metadata.observedAt", "a future stamp would read fresh until the clock caught up");
}

/// A backend that lands one commit the moment the snapshot reaches the sections prefix.
struct Interleave {
    inner: MemBackend,
    fired: AtomicBool,
}

impl Backend for Interleave {
    fn head(&self) -> state_store::Result<u64> {
        self.inner.head()
    }
    fn get(&self, key: &str, at: u64) -> state_store::Result<Option<Version>> {
        self.inner.get(key, at)
    }
    fn range(&self, prefix: &str, after: Option<&str>, limit: usize, at: u64) -> state_store::Result<Vec<(String, Version)>> {
        if prefix == spira_desired_state::section::SECTIONS_PREFIX && !self.fired.swap(true, Ordering::SeqCst) {
            let head = self.inner.head()?;
            let late = |key: &str, v: Value, sub: u32| Write::Put {
                key: key.into(),
                value: Object::new(v).with_ttl(60).encode(),
                create_rev: Rev::new(head + 1, sub),
                prev: None,
            };
            self.inner.commit(
                head,
                &[
                    late("intents/Probe/late/1", json!({}), 0),
                    late("telemetry/sections/Probe/late", status("late", 0, NOW), 1),
                ],
            )?;
        }
        self.inner.range(prefix, after, limit, at)
    }
    fn commit(&self, expect: u64, batch: &[Write]) -> state_store::Result<u64> {
        self.inner.commit(expect, batch)
    }
    fn log(&self, from: u64, max_mains: usize) -> state_store::Result<Vec<LogEntry>> {
        self.inner.log(from, max_mains)
    }
    fn compact(&self, rev: u64) -> state_store::Result<()> {
        self.inner.compact(rev)
    }
}

#[test]
fn snapshot_reads_sections_intents_and_the_pass_record_at_one_list_rev() {
    let r = rig_on(Interleave { inner: MemBackend::new(), fired: AtomicBool::new(true) });
    r.publish(&status("a", 0, NOW));
    let token = r.lock();
    r.ok(&["intent", "put", "intents/Probe/a/1", "--token", &token, "--actor", "reconciler"], Some(&intent("1", &token, json!(1), json!(2))));
    let pass = Txn::new().put("telemetry/reconciler/pass", Object::new(json!({ "pass": "p-1" })).with_ttl(600));
    assert!(r.store.txn(&pass, &Request::new("pass", "reconciler", "t")).unwrap().succeeded);

    let before = r.head();
    r.store.backend().fired.store(false, Ordering::SeqCst);
    let snap = r.ok(&["section", "snapshot"], None);
    assert!(r.head() > before, "a commit landed while the snapshot was listing");
    assert_eq!(snap["list_rev"], before);
    let names: Vec<_> = snap["sections"].as_array().unwrap().iter().map(|s| s["section"]["metadata"]["name"].clone()).collect();
    assert_eq!(names, [json!("a")], "the late section is past the snapshot");
    assert_eq!(snap["intents"].as_array().unwrap().len(), 1, "the late intent is past the snapshot");
    assert_eq!(snap["pass"]["pass"], "p-1");

    let later = r.ok(&["section", "snapshot"], None);
    assert_eq!(later["sections"].as_array().unwrap().len(), 2);
    assert_eq!(later["intents"].as_array().unwrap().len(), 2);
}

#[test]
fn a_status_publish_is_unaudited_telemetry_an_intent_is_audited_and_both_expire() {
    let r = rig();
    let v = r.publish(&status("a", 0, NOW));
    let rev: u64 = v["resourceVersion"].as_str().unwrap().parse().unwrap();
    assert!(r.store.get(&events_key(rev)).unwrap().is_none(), "a publish writes no events/ key");
    let token = r.lock();
    let v = r.ok(&["intent", "put", "intents/Probe/a/1", "--token", &token, "--actor", "reconciler"], Some(&intent("1", &token, json!(1), json!(2))));
    let rev = v["rev"].as_u64().unwrap();
    assert!(r.store.get(&events_key(rev)).unwrap().is_some(), "an intent is audited");

    let gc = |at: u64| {
        r.clock.advance_to(at);
        Controller::new(&r.store).tick(r.clock.now_ms()).unwrap();
    };
    let present = |key: &str| r.store.get(key).unwrap().is_some();
    gc(T0 + 48 * HOUR - MIN);
    assert!(present("intents/Probe/a/1"));
    gc(T0 + 48 * HOUR + MIN);
    assert!(!present("intents/Probe/a/1"), "an intent is gone after 48 h");
    assert!(present("telemetry/sections/Probe/a"));
    assert_eq!(r.ok(&["section", "get", "Probe/a"], None)["freshness"]["state"], "Stale", "a stale section stays readable");
    gc(T0 + 7 * 24 * HOUR + MIN);
    assert!(!present("telemetry/sections/Probe/a"), "a section is gone after 7 days");
}

const FRAGMENT: &str = r#"
[producer]
producer = "operator"
version = "1"
time = "2026-10-31T00:00:00Z"

[[resources]]
apiVersion = "spira/v1"
kind = "Probe"
[resources.metadata]
name = "a"
[resources.spec]
depth_max = { value = 4.0, statute = "law-x" }
"#;

#[test]
fn compose_publishes_a_section_kinds_spec_and_nothing_without_one() {
    let r = rig();
    let mut apply = |body: &Value| {
        let (code, v) = r.call(&["section", "apply-spec", "--actor", "compose"], Some(body));
        if code == 0 { Ok(()) } else { Err(v.to_string()) }
    };
    let composite = compose(&[parse_fragment(FRAGMENT).unwrap()]).unwrap();
    assert_eq!(publish_specs(&composite, &mut apply).unwrap(), 1);
    let got = r.ok(&["section", "get", "Probe/a"], None);
    assert_eq!(got["section"]["spec"]["depth_max"], json!({ "value": 4.0, "statute": "law-x" }));
    assert_eq!(got["section"]["metadata"]["owner"], "probe");
    assert_eq!(got["freshness"]["state"], "Stale", "no status is published yet");

    let head = r.head();
    let default = parse_fragment(include_str!("../../../desired-state/examples/default.toml")).unwrap();
    let composite = compose(&[default]).unwrap();
    assert_eq!(publish_specs(&composite, &mut apply).unwrap(), 0);
    assert_eq!(r.head(), head, "with no section kind, compose writes nothing");
}

#[test]
fn every_answer_is_json_and_list_filters_by_kind() {
    let r = rig();
    for argv in [&["section", "get", "nope"][..], &["section", "frob"], &["intent", "put"], &["lock", "acquire", "reconciler"]] {
        let (code, v) = r.call(argv, None);
        assert_eq!(code, 2, "{argv:?}: {v}");
        assert!(v["cannot_tell"].is_string(), "{argv:?}: {v}");
    }
    assert_eq!(r.call(&["section", "get", "Probe/none"], None).0, 1);
    assert_eq!(r.call(&["section", "get", "Andon/cord"], None).0, 1, "an unregistered kind reads as absent");
    assert_eq!(r.refused(&["lock", "acquire", &crate::keys::claim_resource("sp-a"), "--ttl", "60", "--holder", "h"], None), "resource");
    r.publish(&status("a", 0, NOW));
    assert_eq!(r.ok(&["section", "list", "--kind", "Probe"], None)["sections"].as_array().unwrap().len(), 1);
    assert_eq!(r.ok(&["section", "list", "--kind", "Other"], None)["sections"].as_array().unwrap().len(), 0);
    let bad = Txn::new().put("telemetry/sections/Probe/bad", Object::new(json!({ "kind": "Probe" })).with_ttl(60));
    assert!(r.store.txn(&bad, &Request::new("bad", "test", "t")).unwrap().succeeded);
    let listed = r.ok(&["section", "list"], None);
    assert_eq!(listed["sections"].as_array().unwrap().len(), 1, "one bad record does not blind the rest: {listed}");
    assert_eq!(listed["undecodable"], json!(["telemetry/sections/Probe/bad"]));
}

#[test]
fn a_lock_renews_under_its_token_and_refuses_another_holder() {
    let r = rig();
    let token = r.lock();
    assert_eq!(r.refused(&["lock", "acquire", "reconciler", "--ttl", "600", "--holder", "other"], None), "resource");
    r.ok(&["lock", "renew", "reconciler", "--token", &token, "--ttl", "900"], None);
    assert_eq!(r.refused(&["lock", "renew", "reconciler", "--token", "999.0"], None), "--token");
    r.clock.advance(901_000);
    assert_eq!(r.refused(&["lock", "renew", "reconciler", "--token", &token], None), "--token", "an expired lock renews nothing");
    let again = r.lock();
    assert_ne!(again, token, "a retaken lock has a new fencing token");
}

#[test]
fn writes_and_refusals_are_counted_by_kind_and_reason() {
    let r = rig();
    let ((), got) = spira_otel::metrics::fixture::capture(|sink| {
        let body = status("a", 0, NOW);
        assert_eq!(r.call_with(sink, &["section", "publish-status", "--actor", "probe", "--expect-rev", "0"], Some(&body)).0, 0);
        let bad = with(status("a", 0, NOW), "/actions/0/step", json!("rebuild"));
        assert_eq!(r.call_with(sink, &["section", "publish-status", "--actor", "probe", "--expect-rev", "0"], Some(&bad)).0, 3);
    });
    let find = |name: &str, reason: &str| {
        got.iter().any(|s| s.name == name && s.label("kind") == Some("Probe") && s.label("reason") == Some(reason) && s.label("verb") == Some("section publish-status"))
    };
    assert!(find(WRITES, "ok"), "{got:?}");
    assert!(find(REFUSALS, "step"), "{got:?}");
}
