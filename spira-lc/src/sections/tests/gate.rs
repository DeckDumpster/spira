//! The Gate section end to end: `gate::section::run` against the control store's in-memory
//! backend through the verbs the deployed publisher calls, on the rig's virtual clock, over a
//! one-hour fixture of `gate-run` rows and the manifest that states what it holds.

use ::gate::section::{self, Lc};
use spira_desired_state::store::format_rfc3339;
use reconciler_engine::assemble::{assemble, Snapshot};
use reconciler_engine::causal_map::CausalMap;
use reconciler_engine::flow::Flow;
use reconciler_engine::rank::rank;
use reconciler_engine::select::{select, Inputs};
use spira_desired_state::sections::gate as kind;

use runtime::ports::VirtualClock;
use super::*;

struct Dispatch<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> Lc for Dispatch<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.0.call(&argv, None)
    }
}

fn fixture_rows() -> Vec<Value> {
    include_str!("../../../../gate/fixtures/gate-run-1h.ndjson").lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn manifest() -> Value {
    serde_json::from_str(include_str!("../../../../gate/fixtures/gate-run-1h.manifest.json")).unwrap()
}

/// 2026-10-31T05:00:00Z: the manifest's `now`.
const FIVE: u64 = 1_793_422_800;

fn at_five() -> Rig<MemBackend> {
    let clock = VirtualClock::new(FIVE * 1000);
    let store = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    Rig { store, clock, n: Cell::new(0), sink: MetricsSink::new(None, "test", std::time::Duration::from_millis(1)) }
}

fn publish(r: &Rig<MemBackend>, rows: &[Value]) -> Result<section::Reading, String> {
    section::run(&Dispatch(r), r.clock.now_ms() / 1000, rows)
}

fn published(r: &Rig<MemBackend>) -> Value {
    r.ok(&["section", "get", "Gate/gate"], None)["section"].clone()
}

fn snapshot(r: &Rig<MemBackend>) -> reconciler_engine::assemble::Assembled {
    let (_, snap) = r.call(&["section", "snapshot"], None);
    assemble(&Snapshot::parse(&snap).unwrap(), r.clock.now_ms())
}

#[test]
fn the_hours_share_and_cause_counts_are_the_manifests() {
    let m = manifest();
    let r = at_five();
    let reading = publish(&r, &fixture_rows()).unwrap();
    let (gates, lost) = (m["gates"].as_f64().unwrap(), m["no_verdict"].as_f64().unwrap());
    assert_eq!(reading.gates, 20);
    assert!((reading.no_verdict_share - lost / gates).abs() < 1e-12);
    assert!((reading.no_verdict_hours_share - m["no_verdict_run_secs"].as_f64().unwrap() / m["total_run_secs"].as_f64().unwrap()).abs() < 1e-12);
    let causes: Vec<(String, u64)> = m["by_cause"].as_array().unwrap().iter().map(|c| (c[0].as_str().unwrap().into(), c[1].as_u64().unwrap())).collect();
    assert_eq!(reading.by_cause, causes);
    let s = published(&r);
    assert_eq!(s["status"][kind::NO_VERDICT_SHARE]["value"], 0.3);
    assert_eq!(s["status"][kind::GATES]["value"], 20.0);
    assert_eq!(s["status"][kind::NO_VERDICT_BY_CAUSE], json!([{ "cause": "died-twice", "count": 3 }, { "cause": "scratch-short", "count": 2 }, { "cause": "harness-fault", "count": 1 }]));
    assert_eq!(s["metadata"]["owner"], kind::OWNER);
}

#[test]
fn a_row_that_is_no_gate_or_outside_the_hour_counts_for_nothing_and_a_blank_reason_is_unspecified() {
    let r = at_five();
    let ts = format_rfc3339(FIVE - 60);
    let rows = vec![
        json!({ "ts": ts, "status": "NO_VERDICT", "ran_secs": 10 }),
        json!({ "ts": ts, "status": "NO_VERDICT", "reason": "", "ran_secs": "30" }),
        json!({ "ts": ts, "status": "PASS", "reason": "", "ran_secs": 40 }),
        json!({ "ts": ts, "status": "SKIPPED", "ran_secs": 999 }),
        json!({ "status": "NO_VERDICT", "ran_secs": 999 }),
    ];
    let reading = publish(&r, &rows).unwrap();
    assert_eq!((reading.gates, reading.no_verdict_share, reading.no_verdict_hours_share), (3, 2.0 / 3.0, 0.5));
    assert_eq!(reading.by_cause, [("unspecified".to_string(), 2)]);
}

#[test]
fn an_hour_with_no_gate_or_no_run_time_publishes_zero_so_its_distance_is_defined() {
    let r = at_five();
    let reading = publish(&r, &[]).unwrap();
    assert_eq!((reading.gates, reading.no_verdict_share, reading.no_verdict_hours_share, reading.by_cause.len()), (0, 0.0, 0.0, 0));
    let instant = vec![json!({ "ts": format_rfc3339(FIVE - 1), "status": "NO_VERDICT", "reason": "died-twice" })];
    let reading = publish(&r, &instant).unwrap();
    assert_eq!((reading.no_verdict_share, reading.no_verdict_hours_share), (1.0, 0.0));
}

#[test]
fn a_scratch_short_no_verdict_traces_to_the_host_section_over_its_spec() {
    let r = at_five();
    let mut host = status("host", 0, &format_rfc3339(FIVE));
    host["flow"] = json!({ "consumes": [], "produces": ["host/scratch", "host/io"] });
    r.publish(&host);
    publish(&r, &fixture_rows()).unwrap();

    let a = snapshot(&r);
    let gaps = rank(&a, &CausalMap::seed().unwrap()).gaps;
    assert!(gaps.iter().find(|g| g.section == "Probe/host").is_some_and(|g| g.open), "host is over its spec");
    let flow = Flow::of(&a);
    let fed_by: Vec<&str> = flow.consumes("Gate/gate").iter().flat_map(|res| flow.upstream(res)).collect();
    assert_eq!(fed_by, ["Probe/host", "Probe/host"]);
    assert_eq!(flow.upstream("gate/verdicts"), ["Gate/gate"]);
}

#[test]
fn the_publisher_declares_no_action_and_the_selector_records_the_section_as_a_term() {
    let r = at_five();
    let fragment = parse_fragment(include_str!("../../../../desired-state/examples/gate.toml")).unwrap();
    for s in spira_desired_state::compose::section_specs(&compose(&[fragment]).unwrap()) {
        r.apply_spec(&s);
    }
    publish(&r, &fixture_rows()).unwrap();
    assert_eq!(published(&r)["actions"].as_array().map_or(0, Vec::len), 0);

    let a = snapshot(&r);
    let gaps = rank(&a, &CausalMap::seed().unwrap()).gaps;
    let gap = gaps.iter().find(|g| g.section == "Gate/gate").expect("the section ranks");
    assert!(gap.open && gap.weight > 0.0, "{gap:?}");
    assert!((gap.distance - 0.25).abs() < 1e-9, "{gap:?}");
    let plan = select(&Inputs { assembled: &a, gaps: &gaps, shadow: &[], halted: false, now_ms: r.clock.now_ms(), token: "" });
    assert_eq!(plan.terms, ["Gate/gate"]);
    assert!(plan.steps.is_empty() && plan.would_step.is_empty() && plan.exhausted.is_empty() && plan.no_lever.is_empty(), "{plan:?}");
}

#[test]
fn the_section_turns_stale_when_the_publisher_stops_for_longer_than_its_max_age() {
    let r = at_five();
    publish(&r, &fixture_rows()).unwrap();
    let cannot_see = |r: &Rig<MemBackend>| {
        let a = snapshot(r);
        rank(&a, &CausalMap::seed().unwrap()).gaps.iter().find(|g| g.section == "Gate/gate").and_then(|g| g.cannot_see())
    };
    r.clock.advance(59 * MIN);
    assert_eq!(cannot_see(&r), None, "inside its 1h max_age");
    r.clock.advance(2 * MIN);
    assert!(cannot_see(&r).is_some_and(|m| m.starts_with("cannot see Gate/gate")));
    publish(&r, &fixture_rows()).unwrap();
    assert_eq!(cannot_see(&r), None, "the next publish makes it fresh");
}

#[test]
fn a_store_that_refuses_the_write_is_an_error_naming_the_verb() {
    struct Refuses;
    impl Lc for Refuses {
        fn call(&self, args: &[String]) -> (i32, Value) {
            if args[1] == "get" {
                (1, json!({}))
            } else {
                (3, json!({ "refused": { "field": "status" } }))
            }
        }
    }
    let e = section::run(&Refuses, FIVE, &[]).unwrap_err();
    assert!(e.contains("publish Gate/gate") && e.contains("exit 3"), "{e}");
}

#[test]
fn a_store_that_cannot_answer_the_read_stops_the_publish_before_any_write() {
    struct Down;
    impl Lc for Down {
        fn call(&self, args: &[String]) -> (i32, Value) {
            assert_eq!(args[1], "get", "nothing is written when the read failed");
            (-1, json!({ "cannot_tell": "down" }))
        }
    }
    assert!(section::run(&Down, FIVE, &[]).unwrap_err().contains("section get Gate/gate"));
}

#[test]
fn a_family_with_no_file_reads_as_no_gates_and_any_other_failure_stops_the_publish() {
    use tsd::query::{Failure, ERR_NO_FAMILY};
    assert_eq!(section::gate_rows(Ok(vec![json!({ "status": "PASS" })])).unwrap().len(), 1);
    assert_eq!(section::gate_rows(Err(Failure { code: ERR_NO_FAMILY, message: "no rows yet".into() })).unwrap(), Vec::<Value>::new());
    assert_eq!(section::gate_rows(Err(Failure { code: 1, message: "unreadable".into() })).unwrap_err(), "unreadable");
}
