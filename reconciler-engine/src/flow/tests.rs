use serde_json::json;

use super::*;
use crate::assemble::{assemble, Snapshot};

const NOW: &str = "2026-10-31T19:40:00Z";
/// 2026-10-31T19:40:00Z.
const T0: u64 = 1_793_475_600_000;

fn section(kind: &str, name: &str, consumes: &[&str], produces: &[&str]) -> serde_json::Value {
    json!({ "section": {
        "apiVersion": "spira/v1", "kind": kind,
        "metadata": { "name": name, "owner": "o", "observedAt": NOW, "target_latency": "5m", "max_age": "15m" },
        "status": { "observedGeneration": 0 },
        "flow": { "consumes": consumes, "produces": produces },
    }})
}

fn flow(sections: Vec<serde_json::Value>) -> Flow {
    let snap = Snapshot::parse(&json!({ "list_rev": 1, "sections": sections, "intents": [], "pass": null, "knobs": {}, "undecodable": [] })).unwrap();
    Flow::of(&assemble(&snap, T0))
}

#[test_id(tdd1u0w)]
#[test]
fn a_producer_of_landed_reaches_goodput_in_one_edge_and_a_section_that_produces_nothing_never_does() {
    let f = flow(vec![
        section("DwarfFleet", "a", &["vm-pool/slots"], &["lifecycle/landed"]),
        section("Alarms", "all", &[], &[]),
    ]);
    assert_eq!(f.path_to_goodput("DwarfFleet/a"), ["DwarfFleet/a", "lifecycle/landed"]);
    assert!(f.path_to_goodput("Alarms/all").is_empty());
    assert!(f.path_to_goodput("Nonesuch/x").is_empty());
}

#[test_id(t8ldkan)]
#[test]
fn a_path_follows_produces_through_the_sections_that_consume_each_resource() {
    let f = flow(vec![
        section("Probe", "feed", &[], &["host/scratch"]),
        section("Probe", "mid", &["host/scratch"], &["gate/verdicts"]),
        section("DwarfFleet", "a", &["gate/verdicts"], &["lifecycle/landed"]),
        section("Probe", "loop", &["gate/verdicts"], &["host/scratch"]),
    ]);
    assert_eq!(
        f.path_to_goodput("Probe/feed"),
        ["Probe/feed", "host/scratch", "Probe/mid", "gate/verdicts", "DwarfFleet/a", "lifecycle/landed"]
    );
}

#[test_id(tfg6c8o)]
#[test]
fn upstream_names_the_sections_that_produce_a_resource() {
    let f = flow(vec![section("Probe", "feed", &[], &["host/scratch"]), section("Probe", "other", &[], &["host/scratch"])]);
    assert_eq!(f.upstream("host/scratch"), ["Probe/feed", "Probe/other"]);
    assert!(f.upstream("host/none").is_empty());
}

fn two_controllers() -> Flow {
    flow(vec![
        section("DwarfFleet", "a", &["vm-pool/slots", "host/scratch"], &["lifecycle/landed"]),
        section("DwarfFleet", "b", &["vm-pool/slots"], &["lifecycle/landed"]),
        section("Probe", "pool", &[], &["vm-pool/slots"]),
    ])
}

#[test_id(tlds0h3)]
#[test]
fn two_controllers_consuming_one_pool_share_a_capacity_neighbourhood_and_their_holds_do_not() {
    let f = two_controllers();
    let a = f.neighbourhood("DwarfFleet/a", ActionClass::CapacityRaise, "knobs/DwarfFleet/a/concurrency");
    let b = f.neighbourhood("DwarfFleet/b", ActionClass::CapacityRaise, "knobs/DwarfFleet/b/concurrency");
    assert!(a.members.intersection(&b.members).any(|m| m == "resource:vm-pool/slots"));
    assert!(a.members.contains("section:Probe/pool"));
    let ha = f.neighbourhood("DwarfFleet/a", ActionClass::Local, "knobs/hold/a");
    let hb = f.neighbourhood("DwarfFleet/b", ActionClass::Local, "knobs/hold/b");
    assert!(ha.members.is_disjoint(&hb.members));
    assert_eq!(ha.members.len(), 1, "a local step's neighbourhood is its target alone");
}

#[test_id(tee8r3m)]
#[test]
fn a_consumed_resource_no_section_produces_is_reported_absent_upstream() {
    let f = two_controllers();
    let a = f.neighbourhood("DwarfFleet/a", ActionClass::CapacityRaise, "k");
    assert_eq!(a.absent_upstream, ["host/scratch"]);
    assert!(f.neighbourhood("DwarfFleet/b", ActionClass::CapacityLower, "k").absent_upstream.is_empty());
    assert!(f.neighbourhood("DwarfFleet/a", ActionClass::Local, "k").absent_upstream.is_empty());
}

#[test_id(tahovj5)]
#[test]
fn only_a_capacity_step_depends_on_upstream_sections() {
    let f = two_controllers();
    assert_eq!(f.upstream_sections("DwarfFleet/b", ActionClass::CapacityRaise), ["Probe/pool"]);
    assert!(f.upstream_sections("DwarfFleet/b", ActionClass::Destructive).is_empty());
}
