//! T1–T6 for `hel.reap_terminal`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/hel-reap-terminal.md`. Red until the task is ported.

use runtime::taskrig::DAY_MS;
use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::hel_reap_terminal::{self as task, Carry, Entry, HelReapTerminal as H, Phase, Snapshot, Unsaved};

const SPEC: &str = "*-*-* *:20:00";
const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, SPEC), (task::JITTER_KEY, "120"), ("SPIRA_SCRATCH_MAX_AGE_HOURS", "24")];
const AT_20: u64 = 20 * 60_000;

fn entry(name: &str, is_git: bool, age_secs: u64, held: Option<&str>) -> Entry {
    Entry { name: name.into(), is_dir: true, is_git, repo: is_git.then(|| "spira".to_string()), unsaved: Unsaved::No, age_secs, held: held.map(str::to_string) }
}

/// A live-bead checkout, a `.spira-keep` dir and a young scratch dir.
fn idle() -> Snapshot {
    Snapshot {
        rows_ok: true,
        rows: [("sp-live".to_string(), "WORKING".to_string())].into(),
        entries: vec![entry("sp-live", true, 600, None), entry("kept", false, 999_999, Some(".spira-keep")), entry("scratch-young", false, 3_600, None)],
        ..Snapshot::default()
    }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_the_next_20() {
    t::t1_registered_and_boots::<H>(SET, AT_20);
}

#[test]
fn t2_live_kept_and_young_entries_are_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&idle()));
}

#[test]
fn t3_a_pass_observes_reaps_and_ends_an_empty_list_refuses_and_a_tick_while_reaping_is_refused() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let landed = Snapshot { rows: [("sp-done".to_string(), "LANDED".to_string())].into(), entries: vec![entry("sp-done", true, 600, None)], ..idle() };
    let (c, acts) = task::decide(&h.cfg, &observing, &Input::Observed(landed), AT_20).unwrap();
    assert_eq!((c.phase, names(&acts)), (Phase::Reaping, vec![task::DESTROY_WORKTREE]));
    let refused = task::decide(&h.cfg, &c, &Input::Fire, AT_20 + 3_600_000).unwrap_err();
    assert!(refused.contains("reaping"), "{refused}");
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::DESTROY_WORKTREE.into(), ok: true, body: "sp-done".into() }, AT_20 + 1_000).unwrap();
    assert_ne!(c.phase, Phase::Reaping, "the last removal answered ends the pass");
    let (c, acts) = task::decide(&h.cfg, &observing, &Input::Observed(Snapshot { rows_ok: false, ..Snapshot::default() }), AT_20).unwrap();
    assert_eq!((c.phase, acts.len()), (Phase::Refused, 0));
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../src/tasks/c/hel_reap_terminal.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_terminal_checkouts_and_old_scratch_go_and_unsaved_work_stays() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let mut unsaved = entry("sp-dropped", true, 600, None);
    unsaved.unsaved = Unsaved::Yes;
    let snap = Snapshot {
        rows_ok: true,
        rows: [("sp-done".to_string(), "LANDED".to_string()), ("sp-dropped".to_string(), "DROPPED".to_string())].into(),
        entries: vec![entry("sp-done", true, 600, None), entry("scratch-old", false, 2 * 86_400, None), unsaved, entry(".harness", false, 9_999_999, None)],
        ..Snapshot::default()
    };
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(snap), AT_20).unwrap();
    assert_eq!(names(&acts), [task::DESTROY_WORKTREE, task::REMOVE_SCRATCH], "WOULD remove sp-done and scratch-old; KEEP the unsaved checkout and the dot-name");
}

#[test]
fn t6_two_virtual_days_fire_at_every_20_deterministically_within_2_events_an_hour() {
    let rig = t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&idle())), 2, 2);
    assert_eq!(t::fired_at::<H>(&rig), t::elapses(SPEC, 0, 2 * DAY_MS));
}
