//! T1–T6 for `roundvm.ami_prune`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/roundvm-ami-prune.md`. Red until the task is ported.

use std::collections::BTreeMap;

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::roundvm_ami_prune::{self as task, Carry, ImageStore, Phase, RoundvmAmiPrune as H, Snapshot};

const SPEC: &str = "*-*-* 06:30:00";
const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, SPEC), (task::JITTER_KEY, "600")];
const AT_0630: u64 = (6 * 60 + 30) * 60_000;

fn image(n: u32, recipe: &str) -> ImageStore {
    ImageStore { image_id: format!("ami-{n}"), name: format!("spira-round-{n}"), created: format!("2026-10-{:02}T00:00:00Z", 10 + n), recipe: recipe.into(), snapshots: vec![format!("snap-ami-{n}")], gib: 100 }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_the_next_0630() {
    t::t1_registered_and_boots::<H>(SET, AT_0630);
}

#[test]
fn t2_two_round_images_within_keep_are_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&Snapshot { stores: vec![image(1, "r1"), image(2, "r2")], current_recipe: "r2".into(), ..Snapshot::default() }));
}

#[test]
fn t3_an_image_is_deregistered_then_its_snapshots_deleted_and_the_reverse_is_refused() {
    let h = t::handler::<H>(SET);
    let planned = Carry { images: BTreeMap::from([("ami-0".to_string(), Phase::Planned)]), ..Carry::default() };
    let (c, acts) = task::decide(&h.cfg, &planned, &Input::Outcome { effect: task::DEREGISTER.into(), ok: true, body: "ami-0".into() }, AT_0630).unwrap();
    assert_eq!((c.images.get("ami-0").copied(), names(&acts)), (Some(Phase::Deregistered), vec![task::DELETE_SNAPSHOT]));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::DELETE_SNAPSHOT.into(), ok: true, body: "snap-ami-0".into() }, AT_0630).unwrap();
    assert_eq!(c.images.get("ami-0"), Some(&Phase::SnapshotsDeleted));
    let refused = task::decide(&h.cfg, &planned, &Input::Outcome { effect: task::DELETE_SNAPSHOT.into(), ok: true, body: "snap-ami-0".into() }, AT_0630).unwrap_err();
    assert!(refused.contains("planned"), "{refused}");
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/roundvm_ami_prune.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_the_fake_ec2_fixture_plans_ami_0_and_deregisters_it_first() {
    let h = t::handler::<H>(SET);
    let snap = Snapshot { stores: vec![image(0, "r0"), image(1, "r1"), image(2, "r2")], in_use: vec![], current_recipe: "r2".into(), ..Snapshot::default() };
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), AT_0630).unwrap();
    assert_eq!(names(&acts), [task::DEREGISTER]);
    assert!(acts[0].input.contains("ami-0"));
    assert_eq!(c.images.get("ami-0"), Some(&Phase::Planned));
    let in_use = Snapshot { stores: vec![image(0, "r0"), image(1, "r1"), image(2, "r2")], in_use: vec!["ami-0".into()], current_recipe: "r2".into(), ..Snapshot::default() };
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(in_use), AT_0630).unwrap();
    assert!(acts.is_empty(), "an image in use is never pruned: {acts:?}");
}
