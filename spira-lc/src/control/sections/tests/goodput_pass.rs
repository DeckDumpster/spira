//! The goodput pass end to end: `goodput_pass::run` against the control store's in-memory
//! backend through the same verbs a deployed pass calls, on the rig's virtual clock.

use std::cell::RefCell;

use reconciler_engine::goodput_pass::{run as pass, Lc, Outcome, PassConfig, Passed};
use reconciler_engine::pass_record::{Mode, PassRecord, PASS_KEY, STANDBY_KEY};
use reconciler_engine::select::backoff_secs;
use spira_desired_state::intent::JudgementResult;

use super::*;

impl<B: Backend> Rig<B> {
    fn raw(&self, args: &[String]) -> (i32, Value) {
        self.n.set(self.n.get() + 1);
        let request = format!("t/{}", self.n.get());
        let call = Call { now_ms: self.clock.now_ms(), request: &request, metrics: &self.sink };
        let (code, out) = run(&self.store, args, &call);
        (code, serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?} answered non-JSON {out:?}: {e}")))
    }

    fn stamp_now(&self) -> String {
        spira_desired_state::store::format_rfc3339(self.clock.now_ms() / 1000)
    }

    fn intents(&self) -> Vec<(String, Value)> {
        let (items, _) = self.store.contract().list(&["intents/"], &state_store::Selector::Everything).unwrap();
        items.into_iter().map(|i| (i.key, i.value.data)).collect()
    }

    fn passed(&self, cfg: &PassConfig) -> Passed {
        match pass(&Through(self), &self.clock, cfg).unwrap() {
            Outcome::Passed(p) => *p,
            other => panic!("the pass stood by: {other:?}"),
        }
    }

    fn record(&self) -> PassRecord {
        serde_json::from_value(self.store.contract().get(PASS_KEY).unwrap().expect("a pass record").value.data).unwrap()
    }

    fn owner(&self, argv: &[&str]) -> Value {
        self.ok(argv, None)
    }
}

/// The deployed shape: every call goes through the verbs' dispatch.
struct Through<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> Lc for Through<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        self.0.raw(args)
    }
}

fn cfg() -> PassConfig {
    PassConfig { holder: "inv-1".into(), timeout_secs: 50, shadow: vec![], halted: false }
}

fn action(name: &str, class: &str, step: &str, addresses: &[&str]) -> Value {
    json!({
        "name": name, "class": class, "step": step, "moves": "depth",
        "addresses": addresses, "expected_effect": -1.0, "horizon": "10m",
    })
}

fn local(name: &str) -> Value {
    action(name, "local", "one item", &["Deep"])
}

fn raise(name: &str) -> Value {
    action(name, "capacity-raise", "+1 slot", &["Deep"])
}

struct Sec {
    name: &'static str,
    depth: f64,
    conditions: Vec<&'static str>,
    consumes: Vec<&'static str>,
    produces: Vec<&'static str>,
    actions: Vec<Value>,
}

fn sec(name: &'static str, actions: Vec<Value>) -> Sec {
    Sec { name, depth: 3.0, conditions: vec!["Deep"], consumes: vec!["vm-pool/slots"], produces: vec!["lifecycle/landed"], actions }
}

/// A source for `consumes`: fresh and inside its spec, so it is no gap.
fn pool() -> Sec {
    Sec { name: "pool", depth: 0.0, conditions: vec![], consumes: vec![], produces: vec!["vm-pool/slots"], actions: vec![] }
}

impl Sec {
    fn body(&self, at: &str) -> Value {
        let conditions: Vec<Value> = self
            .conditions
            .iter()
            .map(|c| json!({ "type": c, "status": "True", "reason": "test", "lastTransitionTime": NOW }))
            .collect();
        let wedged = if self.conditions.is_empty() { 0.0 } else { 1.0 };
        let mut v = json!({
            "apiVersion": "spira/v1",
            "kind": "Probe",
            "metadata": { "name": self.name, "owner": "probe", "observedAt": at, "target_latency": "5m", "max_age": "15m" },
            "status": {
                "observedGeneration": 0,
                "depth": { "value": self.depth, "window": "level" },
                "wedged": { "value": wedged, "window": "1h" },
                "conditions": conditions,
            },
            "flow": { "consumes": self.consumes, "produces": self.produces },
            "distance": { "fn": "max(0, depth) / 5 + wedged", "open": 1.0, "close": 0.5 },
        });
        if !self.actions.is_empty() {
            v["actions"] = Value::Array(self.actions.clone());
        }
        v
    }
}

impl<B: Backend> Rig<B> {
    /// Publishes each section observed now, and the knob of each of its actions at 0 unless set.
    fn world(&self, secs: &[&Sec]) {
        let at = self.stamp_now();
        for s in secs {
            self.publish(&s.body(&at));
            for a in &s.actions {
                let knob = format!("knobs/Probe/{}/{}", s.name, a["name"].as_str().unwrap());
                if self.store.contract().get(&knob).unwrap().is_none() {
                    self.put_knob(&knob, json!(0));
                }
            }
        }
    }
}

fn codes(rec: &PassRecord) -> Vec<(String, String)> {
    rec.refused.iter().map(|r| (r.section.clone(), r.code.clone())).collect()
}

fn stepped(rec: &PassRecord) -> Vec<String> {
    rec.steps.iter().map(|s| s.section.clone()).collect()
}

#[test]
fn a_step_is_written_read_back_and_holds_its_neighbourhood_until_it_is_judged() {
    let r = rig();
    let a = sec("a", vec![local("drain")]);
    r.world(&[&a]);
    let p = r.passed(&cfg());
    assert!(p.recorded && p.released, "{p:?}");
    let rec = r.record();
    assert_eq!((rec.n, rec.mode), (1, Mode::Acting));
    assert_eq!(rec.binding.as_deref(), Some("Probe/a"));
    assert_eq!(stepped(&rec), ["Probe/a"]);
    let key = rec.steps[0].intent.clone().unwrap();
    let intents = r.intents();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].0, key);
    assert_eq!((intents[0].1["from"].clone(), intents[0].1["to"].clone()), (json!(0), json!(1)));
    assert!(r.store.contract().get("telemetry/reconciler/passes/1").unwrap().is_some(), "the day of history holds pass 1");

    // Unacknowledged, inside its horizon: no new step in its neighbourhood.
    r.clock.advance(MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    let rec = r.record();
    assert_eq!(rec.n, 2);
    assert!(rec.steps.is_empty());
    assert_eq!(codes(&rec), [("Probe/a".to_string(), "in-flight".to_string())]);

    // Acknowledged, inside its horizon: still in flight.
    r.owner(&["intent", "apply", &key, "--knob", "knobs/Probe/a/drain", "--actor", "probe"]);
    r.clock.advance(MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    assert_eq!(codes(&r.record()), [("Probe/a".to_string(), "in-flight".to_string())]);
    assert!(r.record().judged.is_empty());
}

#[test]
fn an_acked_intent_past_its_horizon_is_judged_on_the_field_it_moves() {
    let r = rig();
    let mut a = sec("a", vec![local("drain")]);
    r.world(&[&a]);
    r.passed(&cfg());
    let key = r.record().steps[0].intent.clone().unwrap();
    r.owner(&["intent", "apply", &key, "--knob", "knobs/Probe/a/drain", "--actor", "probe"]);
    a.depth = 2.0;
    r.clock.advance(11 * MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    let rec = r.record();
    assert_eq!(rec.judged.len(), 1, "{rec:?}");
    assert_eq!((rec.judged[0].result, rec.judged[0].baseline, rec.judged[0].observed), (JudgementResult::Fulfilled, 3.0, 2.0));
    let stored = r.store.contract().get(&key).unwrap().unwrap().value.data;
    assert_eq!(stored["judgement"]["result"], "fulfilled", "the judgement is written to the intent");
    assert_eq!(stepped(&rec), ["Probe/a"], "a fulfilled step frees its neighbourhood, and Deep is still open");
    assert_eq!(rec.steps[0].from, json!(1), "the next step starts from the knob the owner moved");
}

#[test]
fn a_failed_step_backs_off_doubling_to_a_day_and_its_only_candidate_is_exhausted() {
    assert_eq!(backoff_secs(600, 1), 1200);
    assert_eq!(backoff_secs(600, 2), 2400);
    assert_eq!(backoff_secs(600, 7), 24 * 3600, "the cap");
    assert_eq!(backoff_secs(600, 200), 24 * 3600, "no overflow past the cap");

    let r = rig();
    let a = sec("a", vec![local("drain")]);
    let fail_one = |r: &Rig<MemBackend>| {
        let key = r.record().steps[0].intent.clone().unwrap();
        r.owner(&["intent", "apply", &key, "--knob", "knobs/Probe/a/drain", "--actor", "probe"]);
        r.clock.advance(11 * MIN);
        r.world(&[&a]);
        r.passed(&cfg());
        let rec = r.record();
        assert_eq!(rec.judged.iter().map(|j| j.result).collect::<Vec<_>>(), [JudgementResult::Failed], "no effect is a failure");
        rec
    };
    r.world(&[&a]);
    r.passed(&cfg());
    let rec = fail_one(&r);
    assert_eq!(rec.exhausted, ["Probe/a"], "{rec:?}");
    assert!(rec.steps.is_empty() && rec.refused.is_empty(), "Exhausted is the gap's one outcome");

    r.clock.advance(19 * MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    assert_eq!(r.record().exhausted, ["Probe/a"], "inside horizon × 2");
    r.clock.advance(2 * MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    assert_eq!(stepped(&r.record()), ["Probe/a"], "past horizon × 2 the step is taken again");

    fail_one(&r);
    r.clock.advance(39 * MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    assert_eq!(r.record().exhausted, ["Probe/a"], "two failures in a row: horizon × 4");
    r.clock.advance(2 * MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    assert_eq!(stepped(&r.record()), ["Probe/a"]);
}

#[test]
fn a_refused_ack_is_recorded_and_replanned_from_the_fresh_from() {
    let r = rig();
    let a = sec("a", vec![local("drain")]);
    r.world(&[&a]);
    r.passed(&cfg());
    let key = r.record().steps[0].intent.clone().unwrap();
    r.clock.advance(MIN);
    r.put_knob("knobs/Probe/a/drain", json!(5));
    let v = r.owner(&["intent", "apply", &key, "--knob", "knobs/Probe/a/drain", "--actor", "probe"]);
    assert_eq!(v["ack"]["result"]["refused"]["stale"], 5);
    r.clock.advance(MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    let rec = r.record();
    assert_eq!(rec.acks_refused.iter().map(|x| x.intent.as_str()).collect::<Vec<_>>(), [key.as_str()]);
    assert!(rec.judged.is_empty() && rec.exhausted.is_empty(), "a refusal is not a failure");
    assert_eq!((rec.steps[0].from.clone(), rec.steps[0].to.clone()), (json!(5), json!(6)));
    r.clock.advance(MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    assert!(r.record().acks_refused.is_empty(), "recorded once, by the pass after it");
}

#[test]
fn an_unacknowledged_intent_past_its_horizon_is_the_owners_gap_and_is_never_judged() {
    let r = rig();
    let a = sec("a", vec![local("drain")]);
    r.world(&[&a]);
    r.passed(&cfg());
    r.clock.advance(11 * MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    let rec = r.record();
    assert!(rec.judged.is_empty() && rec.steps.is_empty(), "{rec:?}");
    assert_eq!(rec.no_lever.len(), 1);
    assert_eq!((rec.no_lever[0].section.as_str(), rec.no_lever[0].conditions.clone()), ("Probe/a", vec!["IntentsUnacknowledged".to_string()]));
    assert_eq!(r.intents().len(), 1, "no more intents pile on an owner that does not ack");
}

#[test]
fn an_open_condition_no_action_addresses_is_no_lever_recorded_once_beside_the_gaps_one_step() {
    let r = rig();
    let mut a = sec("a", vec![local("drain")]);
    a.conditions = vec!["Deep", "Wedged"];
    r.world(&[&a]);
    r.passed(&cfg());
    let first = r.record();
    assert_eq!(stepped(&first), ["Probe/a"], "the addressed condition still gets its one step");
    assert_eq!(first.no_lever.len(), 1);
    assert_eq!((first.no_lever[0].conditions.clone(), first.no_lever[0].since), (vec!["Wedged".to_string()], 1));
    r.clock.advance(MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    let second = r.record();
    assert!(second.steps.is_empty(), "one step per gap: the first is in flight");
    assert_eq!(second.no_lever[0].since, 1, "a standing NoLever keeps the pass that first recorded it");

    let r = rig();
    a.conditions = vec!["Wedged"];
    r.world(&[&a]);
    r.passed(&cfg());
    let only = r.record();
    assert!(only.steps.is_empty() && only.refused.is_empty() && r.intents().is_empty(), "{only:?}");
    assert_eq!(only.no_lever[0].conditions, ["Wedged"]);
}

#[test]
fn a_section_with_no_actions_is_a_term_and_records_nothing_beyond_its_rank() {
    let r = rig();
    let t = sec("t", vec![]);
    r.world(&[&t]);
    r.passed(&cfg());
    let rec = r.record();
    assert_eq!(rec.binding.as_deref(), Some("Probe/t"));
    assert!(rec.steps.is_empty() && rec.refused.is_empty() && rec.exhausted.is_empty() && rec.no_lever.is_empty(), "{rec:?}");
}

#[test]
fn capacity_steps_sharing_a_resource_are_never_both_in_flight_and_local_steps_on_two_targets_may_be() {
    let r = rig();
    let (a, b, p) = (sec("a", vec![raise("concurrency")]), sec("b", vec![raise("concurrency")]), pool());
    r.world(&[&a, &b, &p]);
    r.passed(&cfg());
    let rec = r.record();
    assert_eq!(stepped(&rec), ["Probe/a"], "{rec:?}");
    assert_eq!(codes(&rec), [("Probe/b".to_string(), "in-flight".to_string())]);
    r.clock.advance(MIN);
    r.world(&[&a, &b, &p]);
    r.passed(&cfg());
    assert!(r.record().steps.is_empty(), "the first is still in flight on vm-pool/slots");

    let r = rig();
    let (a, b) = (sec("a", vec![local("hold-member")]), sec("b", vec![local("hold-member")]));
    r.world(&[&a, &b]);
    r.passed(&cfg());
    assert_eq!(stepped(&r.record()), ["Probe/a", "Probe/b"]);
}

#[test]
fn a_step_fails_closed_on_an_absent_or_stale_upstream_a_stale_target_and_a_missing_andon() {
    // No section produces what the raise consumes.
    let r = rig();
    let mut a = sec("a", vec![raise("concurrency")]);
    a.consumes = vec!["host/scratch"];
    r.world(&[&a]);
    r.passed(&cfg());
    let rec = r.record();
    assert_eq!(codes(&rec), [("Probe/a".to_string(), "upstream-absent".to_string())]);
    assert!(rec.refused[0].reason.contains("host/scratch"), "{}", rec.refused[0].reason);

    // The target itself is stale.
    let r = rig();
    let a = sec("a", vec![raise("concurrency")]);
    r.world(&[&a, &pool()]);
    r.clock.advance(16 * MIN);
    r.world(&[&pool()]);
    r.passed(&cfg());
    assert_eq!(codes(&r.record()), [("Probe/a".to_string(), "stale-input".to_string())]);

    // A stale section upstream blocks the step, whatever the step's own target.
    let r = rig();
    let a = sec("a", vec![raise("concurrency")]);
    r.world(&[&a, &pool()]);
    r.clock.advance(16 * MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    let rec = r.record();
    assert_eq!(codes(&rec), [("Probe/a".to_string(), "upstream-stale".to_string())]);
    assert!(rec.refused[0].reason.contains("Probe/pool"), "{}", rec.refused[0].reason);
    assert!(rec.gaps.iter().any(|g| g.section == "Probe/pool"), "a stale section ranks as its own gap");

    // An upstream with its own gap.
    let r = rig();
    let mut gapped = pool();
    gapped.depth = 9.0;
    r.world(&[&sec("a", vec![raise("concurrency")]), &gapped]);
    r.passed(&cfg());
    assert_eq!(codes(&r.record()), [("Probe/a".to_string(), "upstream-gapped".to_string())]);

    // A destructive step while no andon section exists.
    let r = rig();
    r.world(&[&sec("a", vec![action("reap", "destructive", "10% fewer", &["Deep"])])]);
    r.put_knob("knobs/Probe/a/reap", json!(10));
    r.passed(&cfg());
    assert_eq!(codes(&r.record()), [("Probe/a".to_string(), "no-andon".to_string())]);
    assert!(r.intents().is_empty());
}

#[test]
fn a_knob_the_step_cannot_read_refuses_the_step() {
    let r = rig();
    let a = sec("a", vec![local("drain")]);
    r.world(&[&a]);
    r.store.contract().txn(&Txn::new().delete("knobs/Probe/a/drain"), &Request::new("rm", "test", "t")).unwrap();
    r.passed(&cfg());
    assert_eq!(codes(&r.record()), [("Probe/a".to_string(), "knob-absent".to_string())]);
}

#[test]
fn a_second_pass_finding_the_lock_held_stands_by_naming_the_holder_and_writes_no_pass_record() {
    let r = rig();
    r.world(&[&sec("a", vec![local("drain")])]);
    let held = r.ok(&["lock", "acquire", "reconciler", "--ttl", "600", "--holder", "inv-0"], None);
    let mut other = cfg();
    other.holder = "inv-2".into();
    let got = pass(&Through(&r), &r.clock, &other).unwrap();
    assert_eq!(got, Outcome::Standby { held_by: json!("inv-0") });
    assert!(r.store.contract().get(PASS_KEY).unwrap().is_none(), "a standby pass writes no pass record");
    assert!(r.intents().is_empty());
    let standby = r.store.contract().get(STANDBY_KEY).unwrap().unwrap().value.data;
    assert_eq!((standby["holder"].clone(), standby["held_by"].clone()), (json!("inv-2"), json!("inv-0")));
    r.ok(&["lock", "release", "reconciler", "--token", held["token"].as_str().unwrap()], None);
}

#[test]
fn the_lock_is_free_after_every_pass_and_a_write_under_a_previous_token_is_refused() {
    let r = rig();
    r.world(&[&sec("a", vec![local("drain")])]);
    r.passed(&cfg());
    let old = r.intents()[0].1["by"]["lock_token"].as_str().unwrap().to_string();
    let now = r.ok(&["lock", "acquire", "reconciler", "--ttl", "600", "--holder", "inv-3"], None);
    assert_ne!(now["token"].as_str().unwrap(), old, "the lock was free, and its new token is new");
    let (code, v) = r.call(&["pass", "record", "--token", &old, "--actor", "reconciler"], Some(&json!({ "n": 9 })));
    assert_eq!((code, v["refused"]["code"].clone()), (3, json!("token")), "{v}");
    let key = r.intents()[0].0.clone();
    r.owner(&["intent", "apply", &key, "--knob", "knobs/Probe/a/drain", "--actor", "probe"]);
    let (code, v) = r.call(&["intent", "judge", &key, "--token", &old, "--actor", "reconciler", "--result", "failed", "--observed", "3"], None);
    assert_eq!((code, v["refused"]["code"].clone()), (3, json!("token")), "{v}");
    assert_eq!(r.record().n, 1, "the fenced write left the record as it was");
}

#[test]
fn under_a_halt_and_for_a_shadowed_kind_the_record_holds_the_would_be_step_and_intents_are_unchanged() {
    for (halted, shadow, held, mode) in [
        (true, vec![], "world halted", Mode::Halted),
        (false, vec!["Probe".to_string()], "shadow: Probe", Mode::Shadow),
        (true, vec!["Probe".to_string()], "world halted", Mode::Halted),
        (false, vec!["*".to_string()], "shadow: Probe", Mode::Shadow),
    ] {
        let r = rig();
        r.world(&[&sec("a", vec![local("drain")])]);
        r.passed(&PassConfig { holder: "inv-1".into(), timeout_secs: 50, shadow: shadow.clone(), halted });
        let rec = r.record();
        assert!(rec.steps.is_empty(), "{rec:?}");
        assert_eq!(rec.would_step.len(), 1);
        assert_eq!((rec.would_step[0].held.as_deref(), rec.would_step[0].intent.as_ref()), (Some(held), None));
        assert_eq!((rec.would_step[0].from.clone(), rec.would_step[0].to.clone()), (json!(0), json!(1)));
        assert_eq!(rec.mode, mode);
        assert_eq!(rec.shadow_kinds, shadow);
        assert!(r.intents().is_empty(), "nothing is written to intents/");
    }
}

#[test]
fn under_a_halt_or_a_shadow_an_intent_due_for_judgement_is_left_unjudged() {
    for (halted, shadow) in [(true, vec![]), (false, vec!["Probe".to_string()]), (false, vec!["*".to_string()])] {
        let r = rig();
        let a = sec("a", vec![local("drain")]);
        r.world(&[&a]);
        r.passed(&cfg());
        let key = r.record().steps[0].intent.clone().unwrap();
        r.owner(&["intent", "apply", &key, "--knob", "knobs/Probe/a/drain", "--actor", "probe"]);
        r.clock.advance(11 * MIN);
        r.world(&[&a]);
        r.passed(&PassConfig { holder: "inv-1".into(), timeout_secs: 50, shadow, halted });
        let rec = r.record();
        assert!(rec.judged.is_empty() && rec.steps.is_empty(), "{rec:?}");
        let stored = r.store.contract().get(&key).unwrap().unwrap().value.data;
        assert!(stored.get("judgement").is_none_or(Value::is_null), "intents/ is not written: {stored}");
    }
}

#[test]
fn a_target_behind_its_spec_is_refused_as_not_caught_up() {
    let r = rig();
    r.apply_spec(&spec("a", 1.0));
    r.world(&[&sec("a", vec![local("drain")])]);
    r.passed(&cfg());
    assert_eq!(codes(&r.record()), [("Probe/a".to_string(), "not-caught-up".to_string())]);
    assert!(r.intents().is_empty());
}

#[test]
fn a_capacity_step_with_nothing_consumed_still_holds_its_own_target() {
    let r = rig();
    let mut a = sec("a", vec![action("shed", "capacity-lower", "-1 slot", &["Deep"])]);
    a.consumes = vec![];
    r.world(&[&a]);
    r.put_knob("knobs/Probe/a/shed", json!(4));
    r.passed(&cfg());
    assert_eq!(stepped(&r.record()), ["Probe/a"]);
    r.clock.advance(MIN);
    r.world(&[&a]);
    r.passed(&cfg());
    assert_eq!(codes(&r.record()), [("Probe/a".to_string(), "in-flight".to_string())]);
}

/// Answers every call, and moves the clock on by `step_ms` after each.
struct Slow<'a, B: Backend> {
    rig: &'a Rig<B>,
    step_ms: u64,
    calls: RefCell<Vec<String>>,
}

impl<B: Backend> Lc for Slow<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        self.calls.borrow_mut().push(args[..2].join(" "));
        let out = self.rig.raw(args);
        self.rig.clock.advance(self.step_ms);
        out
    }
}

#[test]
fn the_pass_stops_writing_at_its_own_deadline_and_still_releases_the_lock() {
    let r = rig();
    r.world(&[&sec("a", vec![local("drain")])]);
    let slow = Slow { rig: &r, step_ms: 30_000, calls: RefCell::new(Vec::new()) };
    let Outcome::Passed(p) = pass(&slow, &r.clock, &cfg()).unwrap() else { panic!("stood by") };
    assert!(p.deadline_hit && !p.recorded && p.released, "{p:?}");
    assert_eq!(*slow.calls.borrow(), ["lock acquire", "section snapshot", "lock release"], "past 50 s on its clock no write is made");
    assert!(r.intents().is_empty());
    assert!(r.store.contract().get(PASS_KEY).unwrap().is_none());

    let r = rig();
    r.world(&[&sec("a", vec![local("drain")])]);
    let slow = Slow { rig: &r, step_ms: 15_000, calls: RefCell::new(Vec::new()) };
    let Outcome::Passed(p) = pass(&slow, &r.clock, &cfg()).unwrap() else { panic!("stood by") };
    assert!(p.deadline_hit && !p.recorded && p.released, "{p:?}");
    assert_eq!(*slow.calls.borrow(), ["lock acquire", "section snapshot", "intent put", "intent get", "lock release"], "the step lands, the record after the deadline does not");
    assert_eq!(r.intents().len(), 1);
    assert!(r.store.contract().get(PASS_KEY).unwrap().is_none());
}

/// Every call answered, except that an intent never reads back.
struct Forgetful<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> Lc for Forgetful<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        if args[..2] == ["intent", "get"] {
            return (1, json!({ "not_found": args[2] }));
        }
        self.0.raw(args)
    }
}

#[test]
fn a_step_counts_only_once_its_intent_reads_back() {
    let r = rig();
    r.world(&[&sec("a", vec![local("drain")])]);
    let Outcome::Passed(p) = pass(&Forgetful(&r), &r.clock, &cfg()).unwrap() else { panic!("stood by") };
    assert!(p.record.steps.is_empty(), "{:?}", p.record);
    assert_eq!(codes(&p.record), [("Probe/a".to_string(), "no-read-back".to_string())]);
}
