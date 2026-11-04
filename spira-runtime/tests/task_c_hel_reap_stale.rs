//! T1–T6 for `hel.reap_stale`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/hel-reap-stale.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::hel_reap_stale::{self as task, Carry, HelReapStale as H, Phase, Repo, Snapshot, Worktree};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "3600"), (task::IDLE_HOURS_KEY, "48")];
const HOUR: u64 = 3_600_000;

fn tree(id: &str, state: Option<&str>, merged: bool, idle_secs: u64, live_pid: Option<u32>) -> Worktree {
    Worktree { id: id.into(), path: format!("/r/worktree/{id}"), branch: format!("spira/{id}"), live_pid, bead_state: state.map(str::to_string), merged_into: merged.then(|| "local/main".to_string()), idle_secs }
}

fn repo(trees: Vec<Worktree>) -> Snapshot {
    Snapshot { halted: false, repos: vec![Repo { name: "spira".into(), root: "/r/spira".into(), landrefs: vec!["local/main".into()], worktrees: trees }] }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_an_hourly_tick() {
    t::t1_registered_and_boots::<H>(SET, HOUR);
}

#[test]
fn t2_a_2h_old_open_unmerged_worktree_is_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&repo(vec![tree("sp-a", Some("WORKING"), false, 7_200, None)])));
}

#[test]
fn t3_a_pass_observes_reaps_and_ends_a_halt_idles_and_a_tick_while_reaping_is_refused() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing };
    let (c, acts) = task::decide(&h.cfg, &observing, &Input::Observed(repo(vec![tree("sp-a", Some("LANDED"), false, 60, None)])), HOUR).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Reaping, vec![task::DESTROY_WORKTREE]));
    let refused = task::decide(&h.cfg, &c, &Input::Fire, 2 * HOUR).unwrap_err();
    assert!(refused.contains("reaping"), "{refused}");
    let (_, acts) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::DESTROY_WORKTREE.into(), ok: true, body: "sp-a".into() }, HOUR + 1_000).unwrap();
    assert_eq!(names(&acts), [task::PRUNE], "prune only after a removal succeeded");
    let (c, acts) = task::decide(&h.cfg, &observing, &Input::Observed(Snapshot { halted: true, repos: vec![] }), HOUR).unwrap();
    assert_eq!((c.phase, acts.len()), (Phase::Halted, 0));
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_neither_bash_nor_spira_lc() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../src/tasks/c/hel_reap_stale.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_finished_merged_and_idle_trees_go_and_the_two_listed_divergences_hold() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing };
    let snap = repo(vec![
        tree("sp-landed", Some("LANDED"), false, 60, None),
        tree("sp-merged", Some("SUBMITTED"), true, 2 * 3_600, None),
        tree("sp-idle", Some("WORKING"), false, 49 * 3_600, None),
        tree("sp-live", Some("LANDED"), false, 60, Some(4242)),
        tree("sp-bdclosed", Some("WORKING"), false, 60, None),
    ]);
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(snap), HOUR).unwrap();
    assert_eq!(names(&acts), [task::DESTROY_WORKTREE; 3], "landed, merged-and-idle-1h, idle-48h go; a live cwd and a live lifecycle row stay");
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(repo(vec![tree("sp-a", Some("WORKING"), false, 60, None)])), HOUR).unwrap();
    assert!(acts.is_empty(), "no prune on an idle pass (divergence from today): {acts:?}");
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_1_event_an_hour() {
    t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&repo(vec![tree("sp-a", Some("WORKING"), false, 7_200, None)]))), 2, 1);
}
