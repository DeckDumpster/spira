//! T1–T6 for `sweep.export_beads`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-export-beads.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_export_beads::{self as task, Carry, Phase, Snapshot, SweepExportBeads as H};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "21600"), ("SPIRA_EXPORT_GIT_RETRY_WAIT", "10")];
const TICK: u64 = 21_600_000;

fn dump(ids: &[&str], mirrored_count: usize, mirror_sha: &str) -> Snapshot {
    Snapshot { content: ids.iter().map(|id| serde_json::json!({"id": id, "title": format!("bead {id}")})).collect(), mirrored_count, mirror_bytes_sha: mirror_sha.into(), ..Snapshot::default() }
}

/// A dump whose render is byte-equal to what is mirrored now: the probe reports the sha256 of the
/// mirror's bytes, so an unchanged mirror carries the sha of this very render.
fn current(ids: &[&str], mirrored_count: usize) -> Snapshot {
    let mut s = dump(ids, mirrored_count, "");
    s.mirror_bytes_sha = task::sha_hex(task::render(&s).unwrap().as_bytes());
    s
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_6h_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_a_render_identical_to_the_mirror_touches_and_commits_nothing() {
    let h = t::handler::<H>(SET);
    let unchanged = current(&["sp-a", "sp-b"], 2);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(unchanged.clone()), TICK).unwrap();
    assert_eq!(names(&acts), [task::TOUCH], "an unchanged pass bumps the mirror's mtime only");
    let mut rig = t::rig::<H>(1, SET, &json(&unchanged));
    t::assert_ported(&rig, task::STREAM);
    rig.advance(TICK).unwrap();
    assert!(rig.requested().iter().all(|q| q.name == task::OBSERVE || q.name == task::TOUCH), "{:?}", rig.requested());
}

#[test]
fn t3_rendered_written_committed_published_and_publish_before_committed_is_refused() {
    let h = t::handler::<H>(SET);
    let at = |phase| Carry { phase, sha: Some("s1".into()), refused: None };
    let ok = |e: &str| Input::Outcome { effect: e.into(), ok: true, body: "s1".into() };
    let (c, acts) = task::decide(&h.cfg, &at(Phase::Rendered), &ok(task::WRITE), TICK).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Written, vec![task::COMMIT]));
    let (c, acts) = task::decide(&h.cfg, &c, &ok(task::COMMIT), TICK).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Committed, vec![task::PUBLISH]));
    let (c, _) = task::decide(&h.cfg, &c, &ok(task::PUBLISH), TICK).unwrap();
    assert_eq!(c.phase, Phase::Published);
    let refused = task::decide(&h.cfg, &at(Phase::Written), &ok(task::PUBLISH), TICK).unwrap_err();
    assert!(refused.contains("written"), "{refused}");
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(dump(&[], 10, "x")), TICK).unwrap();
    assert_eq!((c.phase, c.refused.as_deref(), acts.len()), (Phase::Refused, Some("no-sp"), 0));
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_no_spira_lc_or_python3() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_export_beads.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_a_changed_dump_writes_sorted_by_id_and_a_half_shrink_refuses() {
    let h = t::handler::<H>(SET);
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(dump(&["sp-b", "sp-a", "sp-c"], 2, "old")), TICK).unwrap();
    assert_eq!(names(&acts), [task::WRITE]);
    let lines: Vec<&str> = acts[0].input.lines().filter(|l| l.contains("\"id\"")).collect();
    assert!(lines.len() == 3 && lines[0].contains("sp-a") && lines[2].contains("sp-c"), "sorted by id: {lines:?}");
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(dump(&["sp-a"], 10, "old")), TICK).unwrap();
    assert_eq!((c.refused.as_deref(), acts.len()), (Some("shrink"), 0));
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_1_event_an_hour() {
    t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&current(&["sp-a"], 1))), 2, 1);
}
