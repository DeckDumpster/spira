//! The VmPool section end to end: `round_vm::section::run` against the control store's in-memory
//! backend through the verbs the deployed publisher calls, on the rig's virtual clock, over the
//! 10-31 replay fixture's `cert-pass` rows.

use reconciler_engine::assemble::{assemble, Snapshot};
use reconciler_engine::causal_map::CausalMap;
use reconciler_engine::goodput_pass::Lc;
use reconciler_engine::rank::rank;
use reconciler_engine::select::{select, Inputs};
use round_vm::section::{self, Reading};
use spira_desired_state::sections::vm_pool as kind;
use spira_desired_state::store::format_rfc3339;

use runtime::ports::VirtualClock;
use super::*;

struct Dispatch<'a, B: Backend>(&'a Rig<B>);

impl<B: Backend> Lc for Dispatch<'_, B> {
    fn call(&self, args: &[String]) -> (i32, Value) {
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        self.0.call(&argv, None)
    }
}

/// The fixture's passes as the `cert-pass` family stamps them: `ts` and `cause` are what the
/// section reads.
fn fixture_rows() -> Vec<Value> {
    include_str!("../../../../sim/fixtures/replay/1031/cert_pass.ndjson")
        .lines()
        .map(|l| {
            let r: Value = serde_json::from_str(l).unwrap();
            json!({ "ts": r["at"], "family": "cert-pass", "cause": r["cause"] })
        })
        .collect()
}

/// 2026-10-31T05:00:00Z: the hour 04:00..05:00 holds 12 passes, one infra and two over-cap.
const FIVE: u64 = 1_793_422_800;

fn at_five() -> Rig<MemBackend> {
    let clock = VirtualClock::new(FIVE * 1000);
    let store = Contract::open(MemBackend::new(), Ports::virtual_clock(&clock), Config { min_lock_ttl_ms: FLOOR_MS }).unwrap();
    Rig { store, clock, n: Cell::new(0), sink: MetricsSink::new(None, "test", std::time::Duration::from_millis(1)) }
}

fn slots() -> Vec<(String, u64)> {
    vec![("local".into(), 2), ("ec2".into(), 0)]
}

fn publish(r: &Rig<MemBackend>, rows: &[Value]) -> Result<Reading, String> {
    section::run(&Dispatch(r), r.clock.now_ms() / 1000, rows, slots())
}

fn published(r: &Rig<MemBackend>) -> Value {
    r.ok(&["section", "get", "VmPool/pool"], None)["section"].clone()
}

#[test]
fn the_hours_shares_are_the_fixtures_infra_and_over_cap_passes_over_its_passes() {
    let r = at_five();
    let reading = publish(&r, &fixture_rows()).unwrap();
    assert_eq!(reading.passes, 12);
    assert!((reading.infra_incomplete_share - 1.0 / 12.0).abs() < 1e-9);
    assert!((reading.over_cap_share - 2.0 / 12.0).abs() < 1e-9);
    let s = published(&r);
    assert_eq!(s["status"][kind::PASSES]["value"], 12.0);
    assert_eq!(s["status"][kind::SLOTS_IN_USE], json!([{ "pool": "local", "in_use": 2 }, { "pool": "ec2", "in_use": 0 }]));
    assert_eq!(s["metadata"]["owner"], kind::OWNER);
}

#[test]
fn a_vm_lost_pass_counts_as_infra_incomplete_and_a_row_that_is_no_pass_counts_as_nothing() {
    let r = at_five();
    let ts = format_rfc3339(FIVE - 60);
    let rows = vec![
        json!({ "ts": ts, "cause": "vm-lost" }),
        json!({ "ts": ts, "cause": "green" }),
        json!({ "ts": ts, "cause": "no-such-cause" }),
        json!({ "cause": "infra" }),
        json!({ "ts": format_rfc3339(FIVE - 3600), "cause": "infra" }),
        json!({ "ts": format_rfc3339(FIVE + 1), "cause": "infra" }),
    ];
    let reading = publish(&r, &rows).unwrap();
    assert_eq!((reading.passes, reading.infra_incomplete_share, reading.over_cap_share), (2, 0.5, 0.0));
}

#[test]
fn an_hour_with_no_pass_publishes_a_share_of_zero_so_its_distance_is_defined() {
    let r = at_five();
    let reading = publish(&r, &[]).unwrap();
    assert_eq!((reading.passes, reading.infra_incomplete_share, reading.over_cap_share), (0, 0.0, 0.0));
}

#[test]
fn the_publisher_declares_no_action_and_the_selector_records_the_section_as_a_term() {
    let r = at_five();
    spec_for_pool(&r);
    publish(&r, &fixture_rows()).unwrap();
    assert_eq!(published(&r)["actions"].as_array().map_or(0, Vec::len), 0);

    let (_, snap) = r.call(&["section", "snapshot"], None);
    let a = assemble(&Snapshot::parse(&snap).unwrap(), r.clock.now_ms());
    let gaps = rank(&a, &CausalMap::seed().unwrap()).gaps;
    let gap = gaps.iter().find(|g| g.section == "VmPool/pool").expect("the section ranks");
    assert!(gap.open && gap.weight > 0.0, "{gap:?}");
    let plan = select(&Inputs { assembled: &a, gaps: &gaps, shadow: &[], halted: false, now_ms: r.clock.now_ms(), token: "" });
    assert_eq!(plan.terms, ["VmPool/pool"]);
    assert!(plan.steps.is_empty() && plan.would_step.is_empty());
}

#[test]
fn the_section_turns_stale_when_the_publisher_stops_for_longer_than_its_max_age() {
    let r = at_five();
    publish(&r, &fixture_rows()).unwrap();
    let cannot_see = |r: &Rig<MemBackend>| {
        let (_, snap) = r.call(&["section", "snapshot"], None);
        let a = assemble(&Snapshot::parse(&snap).unwrap(), r.clock.now_ms());
        rank(&a, &CausalMap::seed().unwrap()).gaps.iter().find(|g| g.section == "VmPool/pool").and_then(|g| g.cannot_see())
    };
    r.clock.advance(29 * MIN);
    assert_eq!(cannot_see(&r), None, "inside its 30m max_age");
    r.clock.advance(2 * MIN);
    assert!(cannot_see(&r).is_some_and(|m| m.starts_with("cannot see VmPool/pool")));
    publish(&r, &fixture_rows()).unwrap();
    assert_eq!(cannot_see(&r), None, "the next publish makes it fresh");
}

#[test]
fn a_second_publish_replaces_the_first_on_its_revision() {
    let r = at_five();
    publish(&r, &fixture_rows()).unwrap();
    r.clock.advance(5 * MIN);
    publish(&r, &fixture_rows()).unwrap();
    let s = published(&r);
    assert_eq!(s["metadata"]["observedAt"], format_rfc3339(FIVE + 300));
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
    let e = section::run(&Refuses, FIVE, &[], slots()).unwrap_err();
    assert!(e.contains("publish VmPool/pool") && e.contains("exit 3"), "{e}");
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
    assert!(section::run(&Down, FIVE, &[], slots()).unwrap_err().contains("section get VmPool/pool"));
}

#[test]
fn leases_are_counted_per_pool_and_every_pool_is_named_at_zero() {
    use round_vm::schema::{Lease, Vm};
    let lease = |handle: &str| Lease { vm: Vm { handle: handle.into(), addr: String::new() }, owner: None, since: 0, holder: None, held: false, kept_at: 0, pulling: None, run: None, pulled: false };
    assert_eq!(section::slots_by_pool(&[]), [("local".to_string(), 0), ("ec2".to_string(), 0)]);
    let mixed = section::slots_by_pool(&[lease("9001"), lease("ec2-0abc123"), lease("9002")]);
    assert_eq!(mixed, [("local".to_string(), 2), ("ec2".to_string(), 1)]);
}

fn spec_for_pool(r: &Rig<MemBackend>) {
    let fragment = parse_fragment(include_str!("../../../../desired-state/examples/vm-pool.toml")).unwrap();
    for s in spira_desired_state::compose::section_specs(&compose(&[fragment]).unwrap()) {
        r.apply_spec(&s);
    }
}
