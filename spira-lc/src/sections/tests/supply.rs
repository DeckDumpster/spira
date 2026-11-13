//! The Supply section end to end: `groomer::section::run` against the control store's in-memory
//! backend through the verbs the deployed publisher calls, on the rig's virtual clock, over the
//! 10-31 replay fixture's lifecycle transitions.

use groomer::section::{self, Reading, Transition};
use reconciler_engine::assemble::{assemble, Snapshot};
use reconciler_engine::causal_map::CausalMap;
use reconciler_engine::goodput_pass::Lc;
use reconciler_engine::rank::rank;
use reconciler_engine::select::{select, Inputs};
use spira_desired_state::sections::supply as kind;
use spira_desired_state::section::parse_rfc3339;

use runtime::ports::VirtualClock;
use super::*;

struct Dispatch<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> Lc for Dispatch<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.0.call(&argv, None)
    }
}

/// The groom interval the section publishes under.
const INTERVAL: u64 = 6 * 3_600;

/// 2026-11-01T00:00:00Z: the end of the fixture's UTC day.
const MIDNIGHT: u64 = 1_793_491_200;

fn fixture_transitions() -> Vec<Transition> {
    include_str!("../../../../sim/fixtures/replay/1031/transition.ndjson")
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r["kind"] == "bead")
        .map(|r| Transition {
            at: parse_rfc3339(r["at"].as_str().unwrap()).unwrap(),
            id: r["id"].as_str().unwrap().into(),
            from: r["from"].as_str().unwrap().into(),
            to: r["to"].as_str().unwrap().into(),
        })
        .collect()
}

fn at_midnight() -> Rig<MemBackend> {
    let clock = VirtualClock::new(MIDNIGHT * 1000);
    let store = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    Rig { store, clock, n: Cell::new(0), sink: MetricsSink::new(None, "test", std::time::Duration::from_millis(1)) }
}

fn publish(r: &Rig<MemBackend>, rows: &[Transition]) -> Result<Reading, String> {
    section::run(&Dispatch(r), r.clock.now_ms() / 1000, INTERVAL, rows, 3)
}

fn published(r: &Rig<MemBackend>) -> Value {
    r.ok(&["section", "get", "Supply/supply"], None)["section"].clone()
}

fn t(at: u64, id: &str, from: &str, to: &str) -> Transition {
    Transition { at, id: id.into(), from: from.into(), to: to.into() }
}

fn spec_for_supply(r: &Rig<MemBackend>) {
    let fragment = parse_fragment(include_str!("../../../../desired-state/examples/supply.toml")).unwrap();
    for s in spira_desired_state::compose::section_specs(&compose(&[fragment]).unwrap()) {
        r.apply_spec(&s);
    }
}

#[test]
fn the_day_reads_the_manifests_landable_and_dropped_figures_from_the_fixtures_transitions() {
    let r = at_midnight();
    let reading = publish(&r, &fixture_transitions()).unwrap();
    assert_eq!((reading.filed, reading.landable), (201, 38));
    assert!((reading.dropped_share - 163.0 / 201.0).abs() < 1e-9);
    let s = published(&r);
    assert_eq!(s["status"][kind::LANDABLE]["value"], 38.0);
    assert_eq!(s["status"][kind::FILED]["value"], 201.0);
    assert_eq!(s["status"][kind::CLAIMABLE_READY]["value"], 3.0);
    assert_eq!(s["metadata"]["owner"], kind::OWNER);
    assert_eq!(s["metadata"]["target_latency"], "21600s");
    assert_eq!(s["metadata"]["max_age"], "86400s");
}

#[test]
fn a_return_to_ready_is_no_filing_and_a_drop_after_ready_leaves_it_unlandable() {
    let r = at_midnight();
    let n = MIDNIGHT - 60;
    let rows = vec![
        t(n, "a", "OPEN", "READY"),
        t(n, "b", "READY", "READY"),
        t(n, "c", "WORKING", "READY"),
        t(n, "c", "READY", "DROPPED"),
        t(n, "d", "READY", "DROPPED"),
        t(n, "e", "READY", "WORKING"),
        t(MIDNIGHT - 86_400, "f", "OPEN", "READY"),
        t(MIDNIGHT + 1, "g", "OPEN", "READY"),
    ];
    let reading = publish(&r, &rows).unwrap();
    assert_eq!((reading.filed, reading.landable), (3, 1));
    assert!((reading.dropped_share - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn a_day_with_no_filing_publishes_a_share_of_zero_so_its_distance_is_defined() {
    let r = at_midnight();
    let reading = publish(&r, &[]).unwrap();
    assert_eq!((reading.filed, reading.landable, reading.dropped_share), (0, 0, 0.0));
}

#[test]
fn the_publisher_declares_no_action_and_the_selector_records_the_section_as_a_term() {
    let r = at_midnight();
    spec_for_supply(&r);
    publish(&r, &fixture_transitions()).unwrap();
    assert_eq!(published(&r)["actions"].as_array().map_or(0, Vec::len), 0);

    let (_, snap) = r.call(&["section", "snapshot"], None);
    let a = assemble(&Snapshot::parse(&snap).unwrap(), r.clock.now_ms());
    let gaps = rank(&a, &CausalMap::seed().unwrap()).gaps;
    let gap = gaps.iter().find(|g| g.section == "Supply/supply").expect("the section ranks");
    assert!(gap.open && gap.weight > 0.0, "{gap:?}");
    let plan = select(&Inputs { assembled: &a, gaps: &gaps, shadow: &[], halted: false, now_ms: r.clock.now_ms(), token: "" });
    assert_eq!(plan.terms, ["Supply/supply"]);
    assert!(plan.steps.is_empty() && plan.would_step.is_empty());
}

#[test]
fn the_section_turns_stale_when_the_publisher_stops_for_longer_than_its_max_age() {
    let r = at_midnight();
    publish(&r, &fixture_transitions()).unwrap();
    let cannot_see = |r: &Rig<MemBackend>| {
        let (_, snap) = r.call(&["section", "snapshot"], None);
        let a = assemble(&Snapshot::parse(&snap).unwrap(), r.clock.now_ms());
        rank(&a, &CausalMap::seed().unwrap()).gaps.iter().find(|g| g.section == "Supply/supply").and_then(|g| g.cannot_see())
    };
    r.clock.advance(23 * HOUR);
    assert_eq!(cannot_see(&r), None, "inside its max_age of four intervals");
    r.clock.advance(2 * HOUR);
    assert!(cannot_see(&r).is_some_and(|m| m.starts_with("cannot see Supply/supply")));
    publish(&r, &fixture_transitions()).unwrap();
    assert_eq!(cannot_see(&r), None, "the next publish makes it fresh");
}

#[test]
fn a_second_publish_replaces_the_first_on_its_revision() {
    let r = at_midnight();
    publish(&r, &fixture_transitions()).unwrap();
    r.clock.advance(HOUR);
    publish(&r, &fixture_transitions()).unwrap();
    assert_eq!(published(&r)["metadata"]["observedAt"], spira_desired_state::store::format_rfc3339(MIDNIGHT + 3_600));
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
    let e = section::run(&Refuses, MIDNIGHT, INTERVAL, &[], 0).unwrap_err();
    assert!(e.contains("publish Supply/supply") && e.contains("exit 3"), "{e}");
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
    assert!(section::run(&Down, MIDNIGHT, INTERVAL, &[], 0).unwrap_err().contains("section get Supply/supply"));
}

#[test]
fn the_groom_pass_reads_the_listed_rows_and_the_ready_sets_claimable_count() {
    struct Store;
    impl Lc for Store {
        fn call(&self, args: &[String]) -> (i32, Value) {
            match args[0].as_str() {
                "list" => (0, json!([
                    { "bead_id": "a", "state": "READY", "updated_at": (MIDNIGHT - 60).to_string() },
                    { "bead_id": "b", "state": "DROPPED", "updated_at": MIDNIGHT - 90 },
                    { "bead_id": "c", "state": "READY", "updated_at": null },
                    { "state": "READY", "updated_at": 1 },
                ])),
                "ready-set" => (0, json!({ "lifecycle": [{}, {}], "items": [] })),
                "section" if args[1] == "get" => (1, json!({})),
                "section" => (0, json!({})),
                other => panic!("unexpected verb {other}"),
            }
        }
    }
    let reading = section::publish(&Store, MIDNIGHT, INTERVAL).unwrap();
    assert_eq!(reading, Reading { filed: 2, landable: 1, dropped_share: 0.5, claimable_ready: 2 });
    assert_eq!(section::claimable_ready(&Store).unwrap(), 2);
}

#[test]
fn a_list_or_ready_set_that_fails_stops_the_pass_before_any_write() {
    struct Down;
    impl Lc for Down {
        fn call(&self, args: &[String]) -> (i32, Value) {
            assert!(args[0] == "list" || args[0] == "ready-set", "nothing is written: {args:?}");
            (2, json!({ "cannot_tell": "down" }))
        }
    }
    assert!(section::publish(&Down, MIDNIGHT, INTERVAL).unwrap_err().starts_with("list: exit 2"));
    assert!(section::claimable_ready(&Down).unwrap_err().starts_with("ready-set: exit 2"));
}

#[test]
fn rows_that_are_not_a_list_are_no_transitions() {
    assert!(section::transitions_of_rows(&json!({ "error": "x" })).is_empty());
}

#[test]
fn a_section_the_store_holds_but_the_schema_cannot_read_stops_the_publish() {
    struct Garbled;
    impl Lc for Garbled {
        fn call(&self, args: &[String]) -> (i32, Value) {
            assert_eq!(args[1], "get", "nothing is written over a section that cannot be read");
            (0, json!({ "section": { "kind": 3 } }))
        }
    }
    assert!(section::run(&Garbled, MIDNIGHT, INTERVAL, &[], 0).unwrap_err().contains("Supply/supply is unreadable"));
}
