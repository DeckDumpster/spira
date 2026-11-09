//! The DwarfFleet section end to end: `sentinel::section::run` against the control store's
//! in-memory backend through the verbs a deployed call pass calls, on the rig's virtual clock,
//! with the ceiling held by a fake `spira-config` and the ledger a fixture in the real grammar.

use std::cell::{Cell, RefCell};

use sentinel::model::LcRow;
use sentinel::section::{self as fs, Config as CeilingConfig, Ports, Report, Run};
use spira_desired_state::sections::dwarf_fleet as fleet;
use spira_desired_state::store::format_rfc3339;

use super::*;

struct Dispatch<'a, B: Backend> {
    rig: &'a Rig<B>,
    /// Fails the next `intent ack`: the pass died between the set and the ack.
    lose_ack: Cell<bool>,
}

impl<B: Backend> Lc for Dispatch<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        if args.starts_with(&["intent".into(), "ack".into()]) && self.lose_ack.replace(false) {
            return (2, json!({ "cannot_tell": "the pass died before the ack" }));
        }
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.rig.call(&argv, None)
    }
}

use reconciler_engine::goodput_pass::Lc;

struct Ceiling {
    live: Cell<u64>,
    max: u64,
    sets: RefCell<Vec<u64>>,
}

impl CeilingConfig for Ceiling {
    fn get(&self, key: &str) -> Result<u64, String> {
        match key {
            fleet::CEILING_KEY => Ok(self.live.get()),
            fleet::CEILING_MAX_KEY => Ok(self.max),
            other => Err(format!("{other} is no key this fake holds")),
        }
    }

    fn set(&self, key: &str, value: u64) -> Result<(), String> {
        assert_eq!(key, fleet::CEILING_KEY, "only the live ceiling is ever set");
        self.sets.borrow_mut().push(value);
        self.live.set(value);
        Ok(())
    }
}

fn secs(r: &Rig<MemBackend>) -> u64 {
    r.clock.now_ms() / 1000
}

fn stamp(epoch: u64) -> String {
    format_rfc3339(epoch)
}

/// A ledger in the real grammar: each run is a `born` and a `done`, sorted by time.
fn ledger(now: u64, runs: &[(u64, u64, &str)]) -> String {
    let mut lines: Vec<(u64, String)> = Vec::new();
    for (i, (ended_ago, minutes, bead)) in runs.iter().enumerate() {
        let end = now - ended_ago * 60;
        let start = end - minutes * 60;
        lines.push((start, format!("{} born builder {}", stamp(start), 1000 + i)));
        lines.push((end, format!("{} done builder {bead} rc=0 status=open wall_s={} api_s=?", stamp(end), minutes * 60)));
    }
    lines.sort_by_key(|(t, _)| *t);
    lines.into_iter().map(|(_, l)| l + "\n").collect()
}

fn row(bead: &str, state: &str, now: u64, landed_ago_min: u64) -> LcRow {
    LcRow {
        bead_id: bead.into(),
        state: state.into(),
        updated_at: Some((now - landed_ago_min * 60) as i64),
        ..LcRow::default()
    }
}

/// 2026-10-29..10-31: ten beads landed in the day behind `now`, twelve minutes-of-work runs on
/// them, one submitted and not yet landed, and two runs that never submitted.
fn day() -> (Vec<(u64, u64, &'static str)>, Vec<(&'static str, &'static str, u64)>) {
    let landed = ["b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8", "b9", "b10"];
    let mut runs: Vec<(u64, u64, &str)> = landed.iter().enumerate().map(|(i, b)| (120 + 60 * i as u64, 45, *b)).collect();
    runs.push((200, 18, "b11"));
    runs.push((300, 66, "b12"));
    runs.push((400, 66, "b13"));
    let mut rows: Vec<(&str, &str, u64)> = landed.iter().enumerate().map(|(i, b)| (*b, "LANDED", 100 + 60 * i as u64)).collect();
    rows.extend([("b11", "SUBMITTED", 190), ("b12", "WORKING", 290), ("b13", "READY", 390)]);
    (runs, rows)
}

fn spec_seeded(r: &Rig<MemBackend>) {
    let fragment = parse_fragment(include_str!("../../../../desired-state/examples/dwarf-fleet.toml")).unwrap();
    let composite = compose(&[fragment]).unwrap();
    for s in spira_desired_state::compose::section_specs(&composite) {
        r.apply_spec(&s);
    }
}

fn world(config: Ceiling) -> (Rig<MemBackend>, Ceiling) {
    let r = rig();
    spec_seeded(&r);
    (r, config)
}

struct Pass<'a> {
    r: &'a Rig<MemBackend>,
    config: &'a Ceiling,
    lose_ack: Cell<bool>,
    steps: RefCell<Vec<String>>,
}

impl<'a> Pass<'a> {
    fn new(r: &'a Rig<MemBackend>, config: &'a Ceiling) -> Pass<'a> {
        Pass { r, config, lose_ack: Cell::new(false), steps: RefCell::new(vec![]) }
    }

    fn run(&self, runs: &[Run], rows: &[LcRow], live: u64, ready: u64) -> Result<Report, String> {
        let d = Dispatch { rig: self.r, lose_ack: Cell::new(self.lose_ack.replace(false)) };
        let record = |l: &str| self.steps.borrow_mut().push(l.to_string());
        fs::run(&Ports { lc: &d, config: self.config, record: &record }, secs(self.r), runs, rows, live, ready)
    }
}

fn runs_of(now: u64, shape: &[(u64, u64, &str)]) -> Vec<Run> {
    fs::runs(&ledger(now, shape), now, &|_| false)
}

fn live_runs(now: u64) -> Vec<Run> {
    let text = format!(
        "{}\n{}\n{}\n{}\n",
        format_args!("{} born builder 101", stamp(now - 4 * 3600)),
        format_args!("{} born builder 102", stamp(now - 4 * 3600)),
        format_args!("{} born builder 103", stamp(now - 4 * 3600)),
        format_args!("{} born builder 104", stamp(now - 1800)),
    );
    fs::runs(&text, now, &|pid| (101..=104).contains(&pid))
}

fn section_of(r: &Rig<MemBackend>) -> Value {
    r.ok(&["section", "get", "DwarfFleet/fleet"], None)["section"].clone()
}

fn intent_for(id: &str, token: &str, action: &str, from: u64, to: u64) -> Value {
    json!({
        "id": id, "action": action, "target": format!("knobs/DwarfFleet/fleet/{action}"), "from": from, "to": to,
        "moves": "utilisation_1h", "baseline": 0.95, "expected_effect": -0.1, "horizon": "65m",
        "addresses": ["Saturated"], "taken_at": stamp(0), "by": { "pass": "p-1", "lock_token": token },
    })
}

fn put_intent(r: &Rig<MemBackend>, token: &str, id: &str, action: &str, from: u64, to: u64) -> String {
    let key = format!("intents/DwarfFleet/fleet/{id}");
    r.ok(&["intent", "put", &key, "--token", token, "--actor", "reconciler"], Some(&intent_for(id, token, action, from, to)));
    key
}

fn ack_of(r: &Rig<MemBackend>, key: &str) -> Value {
    r.ok(&["intent", "get", key], None)["intent"]["ack"].clone()
}

fn config(live: u64, max: u64) -> Ceiling {
    Ceiling { live: Cell::new(live), max, sets: RefCell::new(vec![]) }
}

#[test]
fn the_ledger_and_landings_read_as_utilisation_minutes_per_bead_and_the_never_submitted_share() {
    let (r, c) = world(config(4, 6));
    let now = secs(&r);
    let (shape, rows) = day();
    let mut runs = runs_of(now, &shape);
    runs.extend(live_runs(now));
    let rows: Vec<LcRow> = rows.iter().map(|(b, s, ago)| row(b, s, now, *ago)).collect();
    let p = Pass::new(&r, &c);
    assert!(p.run(&runs, &rows, 4, 7).unwrap().published);

    let s = section_of(&r);
    let status = &s["status"];
    assert_eq!(status[fleet::UTILISATION], json!({ "value": 0.875, "window": "1h" }));
    assert_eq!(status[fleet::MINUTES_PER_LANDED], json!({ "value": 60.0, "window": "24h" }));
    let share = status[fleet::NEVER_SUBMITTED]["value"].as_f64().unwrap();
    assert!((share - 0.22).abs() < 1e-9, "{share}");
    assert_eq!((status["live"]["value"].clone(), status["ceiling"]["value"].clone(), status["claimable_ready"]["value"].clone()), (json!(4.0), json!(4.0), json!(7.0)));

    let parsed = spira_desired_state::section::parse_section(&s).unwrap();
    let kind = spira_desired_state::sections::lookup(fleet::NAME).unwrap();
    assert!((parsed.distance_value(kind).unwrap().unwrap() - 0.025).abs() < 1e-9, "the distance is utilisation over its target alone");
    assert_eq!(parsed.status.conditions.iter().find(|c| c.type_ == fleet::SATURATED).unwrap().status, spira_desired_state::section::ConditionStatus::True);
}

#[test]
fn no_work_waiting_reads_zero_utilisation_and_no_landing_leaves_minutes_per_bead_unobservable() {
    let (r, c) = world(config(4, 6));
    let now = secs(&r);
    let runs = live_runs(now);
    Pass::new(&r, &c).run(&runs, &[], 4, 0).unwrap();
    let s = section_of(&r);
    assert_eq!(s["status"][fleet::UTILISATION]["value"], 0.0);
    assert!(s["status"][fleet::MINUTES_PER_LANDED]["unobservable"].is_string());
    assert!(s["status"][fleet::NEVER_SUBMITTED]["unobservable"].is_string());
    let saturated = s["status"]["conditions"].as_array().unwrap().iter().find(|c| c["type"] == fleet::SATURATED).unwrap().clone();
    assert_eq!(saturated["status"], "False");

    let zero = fs::measure(&fs::Reading { now, runs: &runs, rows: &[], live: 0, ceiling: 0, claimable_ready: 5 });
    assert_eq!(zero.utilisation_1h, 0.0, "a zero ceiling has no capacity to be busy in");
}

#[test]
fn an_unchanged_section_is_published_again_only_at_its_keepalive() {
    let (r, c) = world(config(4, 6));
    let p = Pass::new(&r, &c);
    assert!(p.run(&[], &[], 4, 0).unwrap().published);
    r.clock.advance(MIN);
    assert!(!p.run(&[], &[], 4, 0).unwrap().published, "the same status inside the keepalive");
    r.clock.advance(5 * MIN);
    assert!(p.run(&[], &[], 4, 0).unwrap().published, "the keepalive");
    r.clock.advance(MIN);
    assert!(p.run(&[], &[], 5, 0).unwrap().published, "a changed status at once");
}

#[test]
fn a_raise_steps_the_ceiling_by_one_and_a_rerun_after_a_crash_between_set_and_ack_is_already_applied() {
    let (r, c) = world(config(4, 6));
    let p = Pass::new(&r, &c);
    p.run(&[], &[], 4, 7).unwrap();
    let token = r.lock();
    let key = put_intent(&r, &token, "1", fleet::RAISE, 4, 5);

    p.lose_ack.set(true);
    assert!(p.run(&[], &[], 4, 7).is_err(), "the pass died before it acked");
    assert_eq!(c.sets.borrow().as_slice(), [5], "the ceiling was set");
    assert_eq!(ack_of(&r, &key), Value::Null);

    let rep = p.run(&[], &[], 4, 7).unwrap();
    assert_eq!(rep.acked, [(key.clone(), "already-applied".to_string())]);
    assert_eq!(c.sets.borrow().as_slice(), [5], "the rerun set nothing again");
    assert_eq!(ack_of(&r, &key)["result"], "already-applied");

    let steps = p.steps.borrow();
    assert_eq!(steps.len(), 1);
    assert!(steps[0].contains("utilisation_1h=") && steps[0].contains("claimable_ready=7"), "{steps:?}");
    for action in [fleet::RAISE, fleet::LOWER] {
        assert_eq!(r.knob(&format!("knobs/DwarfFleet/fleet/{action}")), json!(5), "both knobs mirror the ceiling");
    }
}

#[test]
fn a_raise_is_applied_once_and_acked_applied() {
    let (r, c) = world(config(4, 6));
    let p = Pass::new(&r, &c);
    p.run(&[], &[], 4, 7).unwrap();
    let token = r.lock();
    let key = put_intent(&r, &token, "1", fleet::RAISE, 4, 5);
    let rep = p.run(&[], &[], 4, 7).unwrap();
    assert_eq!(rep.acked, [(key.clone(), "applied".to_string())]);
    assert_eq!((c.live.get(), c.sets.borrow().len()), (5, 1));
    assert_eq!(ack_of(&r, &key)["result"], "applied");
}

#[test]
fn a_raise_at_the_ceiling_the_config_allows_is_refused_and_moves_nothing() {
    let (r, c) = world(config(6, 6));
    let p = Pass::new(&r, &c);
    p.run(&[], &[], 6, 7).unwrap();
    let token = r.lock();
    let key = put_intent(&r, &token, "1", fleet::RAISE, 6, 7);
    let rep = p.run(&[], &[], 6, 7).unwrap();
    assert_eq!(rep.acked, [(key.clone(), "refused".to_string())]);
    assert!(c.sets.borrow().is_empty());
    let ack = ack_of(&r, &key);
    assert!(ack["result"]["refused"]["reason"].as_str().unwrap().contains("SPIRA_MAX_DWARVES"), "{ack}");
}

#[test]
fn a_step_below_one_a_stale_from_and_a_step_that_is_not_one_are_refused() {
    let (r, c) = world(config(1, 6));
    let p = Pass::new(&r, &c);
    p.run(&[], &[], 1, 7).unwrap();
    let token = r.lock();
    let below = put_intent(&r, &token, "1", fleet::LOWER, 1, 0);
    let stale = put_intent(&r, &token, "2", fleet::RAISE, 3, 4);
    let wrong_way = put_intent(&r, &token, "3", fleet::RAISE, 1, 0);
    let rep = p.run(&[], &[], 1, 7).unwrap();
    assert!(rep.acked.iter().all(|(_, w)| w == "refused"), "{rep:?}");
    let reason = |k: &str| ack_of(&r, k)["result"]["refused"]["reason"].as_str().unwrap().to_string();
    assert!(reason(&below).contains("1..=6"), "{}", reason(&below));
    assert!(reason(&stale).starts_with("stale"), "{}", reason(&stale));
    assert!(reason(&wrong_way).contains("one step"), "{}", reason(&wrong_way));
    assert!(c.sets.borrow().is_empty());
}

#[test]
fn an_intent_for_an_action_the_kind_does_not_declare_or_with_no_whole_ceiling_is_refused() {
    let i = |action: &str, from: Value, to: Value| {
        let mut v = intent_for("1", "1.0", action, 1, 2);
        v["from"] = from;
        v["to"] = to;
        spira_desired_state::intent::parse_intent(&v).unwrap()
    };
    assert!(matches!(fs::decide(&i("drain", json!(1), json!(2)), 1, 6), fs::Step::Refuse(m) if m.contains("drain")));
    assert!(matches!(fs::decide(&i(fleet::RAISE, json!("1"), json!(2)), 1, 6), fs::Step::Refuse(m) if m.contains("whole")));
    assert_eq!(fs::decide(&i(fleet::LOWER, json!(3), json!(2)), 3, 6), fs::Step::Set(2));
}

#[test]
fn a_ceiling_the_config_cannot_state_stops_the_pass_before_it_acks_anything() {
    struct Unreadable;
    impl CeilingConfig for Unreadable {
        fn get(&self, key: &str) -> Result<u64, String> {
            Err(format!("{key} is unreadable"))
        }
        fn set(&self, _: &str, _: u64) -> Result<(), String> {
            panic!("nothing is set against a ceiling that could not be read")
        }
    }
    let r = rig();
    let d = Dispatch { rig: &r, lose_ack: Cell::new(false) };
    let record = |_: &str| {};
    let e = fs::run(&Ports { lc: &d, config: &Unreadable, record: &record }, secs(&r), &[], &[], 0, 0).unwrap_err();
    assert!(e.contains("unreadable"), "{e}");
}

#[test]
fn a_section_no_pass_republishes_reads_stale() {
    let (r, c) = world(config(4, 6));
    Pass::new(&r, &c).run(&[], &[], 4, 0).unwrap();
    let freshness = |r: &Rig<MemBackend>| {
        let snap = r.ok(&["section", "snapshot"], None);
        let item = snap["sections"].as_array().unwrap().iter().find(|i| i["section"]["kind"] == fleet::NAME).unwrap().clone();
        item["freshness"]["state"].clone()
    };
    assert_eq!(freshness(&r), json!("Fresh"));
    r.clock.advance(16 * MIN);
    assert_eq!(freshness(&r), json!("Stale"));
}

fn probe_source(r: &Rig<MemBackend>, name: &str, produces: &str, depth: f64) {
    let body = json!({
        "apiVersion": "spira/v1", "kind": "Probe",
        "metadata": { "name": name, "owner": "probe", "observedAt": stamp(secs(r)), "target_latency": "5m", "max_age": "15m" },
        "status": { "observedGeneration": 0, "depth": { "value": depth, "window": "level" }, "wedged": { "value": 0.0, "window": "1h" }, "conditions": [] },
        "flow": { "consumes": [], "produces": [produces] },
        "distance": { "fn": "max(0, depth) / 5 + wedged", "open": 1.0, "close": 0.5 },
    });
    r.publish(&body);
}

#[test]
fn the_selector_refuses_the_raise_while_host_is_absent_or_gapped_and_takes_it_once_host_is_whole() {
    use reconciler_engine::goodput_pass::{run as pass, Outcome, PassConfig};
    let (r, c) = world(config(4, 6));
    let now = secs(&r);
    let (shape, rows) = day();
    let mut runs = runs_of(now, &shape);
    let saturated: String = (101..=104).map(|pid| format!("{} born builder {pid}\n", stamp(now - 2 * 3600))).collect();
    runs.extend(fs::runs(&saturated, now, &|pid| (101..=104).contains(&pid)));
    let rows: Vec<LcRow> = rows.iter().map(|(b, s, ago)| row(b, s, now, *ago)).collect();
    struct Through<'a>(&'a Rig<MemBackend>);
    impl Lc for Through<'_> {
        fn call(&self, args: &[String]) -> (i32, Value) {
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            self.0.call(&argv, None)
        }
    }
    let pass_once = || {
        let cfg = PassConfig { holder: "inv-1".into(), timeout_secs: 50, shadow: vec![], halted: false };
        match pass(&Through(&r), &r.clock, &cfg).unwrap() {
            Outcome::Passed(p) => *p,
            other => panic!("the pass stood by: {other:?}"),
        }
    };
    let record = |r: &Rig<MemBackend>| -> reconciler_engine::pass_record::PassRecord {
        serde_json::from_value(r.store.get(reconciler_engine::pass_record::PASS_KEY).unwrap().unwrap().value.data).unwrap()
    };
    let code_for = |rec: &reconciler_engine::pass_record::PassRecord| rec.refused.iter().find(|x| x.section == "DwarfFleet/fleet").map(|x| x.code.clone());

    let publish = |r: &Rig<MemBackend>| {
        Pass::new(r, &c).run(&runs, &rows, 4, 7).unwrap();
    };
    publish(&r);
    probe_source(&r, "supply", "supply/landable", 0.0);
    pass_once();
    assert_eq!(code_for(&record(&r)).as_deref(), Some("upstream-absent"), "host absent");

    r.clock.advance(MIN);
    publish(&r);
    probe_source(&r, "supply", "supply/landable", 0.0);
    probe_source(&r, "host", "host/scratch", 9.0);
    pass_once();
    assert_eq!(code_for(&record(&r)).as_deref(), Some("upstream-gapped"), "host gapped");

    r.clock.advance(MIN);
    publish(&r);
    probe_source(&r, "supply", "supply/landable", 0.0);
    probe_source(&r, "host", "host/scratch", 0.0);
    pass_once();
    let rec = record(&r);
    assert_eq!(code_for(&rec), None, "host whole: {:?}", rec.refused);
    assert!(rec.steps.iter().any(|s| s.section == "DwarfFleet/fleet"), "{:?}", rec.steps);
}
