//! T1–T6 for `sweep.reclaim`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-reclaim.md`. Red until the task is ported.

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_reclaim::{self as task, CacheFile, Candidate, Carry, Phase, Snapshot, SweepReclaim as H, Verdict};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "3600"), (task::MIN_FREE_KEY, "70"), (task::TARGET_FREE_KEY, "100"), (task::PODMAN_CAP_KEY, "40"), (task::CACHE_CAP_KEY, "40"), ("SPIRA_RECLAIM_IDLE_DAYS", "2")];
const HOUR: u64 = 3_600_000;
const GIB: u64 = 1 << 30;

/// 90 GiB free, podman at 10 GiB, the cache at 1 GiB.
fn idle() -> Snapshot {
    Snapshot { require_ok: vec![("/r/worktree".into(), true)], free_mib: 90 * 1024, candidates: vec![], podman_bytes: 10 * GIB, cache: vec![CacheFile { mtime: 1, bytes: GIB, path: "/r/sccache-dav/a".into() }] }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_an_hourly_tick() {
    t::t1_registered_and_boots::<H>(SET, HOUR);
}

#[test]
fn t2_plenty_free_and_everything_under_its_cap_is_the_probe_alone() {
    t::t2_silent::<H>(SET, &json(&idle()));
}

#[test]
fn t3_a_pass_reclaims_or_refuses_and_a_tick_while_reclaiming_is_refused() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let low = Snapshot { free_mib: 60 * 1024, candidates: vec![Candidate { path: "/r/worktree/a/target".into(), bytes: 50 * GIB, age: 9, verdict: Verdict::Reclaim }], ..idle() };
    let (c, _) = task::decide(&h.cfg, &observing, &Input::Observed(low), HOUR).unwrap();
    assert_eq!(c.phase, Phase::Reclaiming);
    let refused = task::decide(&h.cfg, &c, &Input::Fire, 2 * HOUR).unwrap_err();
    assert!(refused.contains("reclaiming"), "{refused}");
    let (c, acts) = task::decide(&h.cfg, &observing, &Input::Observed(Snapshot { require_ok: vec![("/r/worktree".into(), false)], ..idle() }), HOUR).unwrap();
    assert_eq!((c.phase, acts.len()), (Phase::Refused, 0));
    assert!(c.refused_cause.is_some_and(|w| w.contains("/r/worktree")));
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_nothing() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_reclaim.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_low_space_prunes_podman_then_the_oldest_targets_and_a_fat_podman_alone_prunes() {
    let h = t::handler::<H>(SET);
    let observing = Carry { phase: Phase::Observing, ..Carry::default() };
    let cand = |p: &str, gib: u64, v| Candidate { path: p.into(), bytes: gib * GIB, age: 9, verdict: v };
    let low = Snapshot { free_mib: 60 * 1024, candidates: vec![cand("/old", 30, Verdict::Reclaim), cand("/held", 30, Verdict::Held), cand("/newer", 30, Verdict::Reclaim), cand("/newest", 30, Verdict::Reclaim)], ..idle() };
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(low), HOUR).unwrap();
    assert_eq!(names(&acts), [task::PODMAN_PRUNE, task::REMOVE_TARGET, task::REMOVE_TARGET], "reclaimed /old, /newer (dry); kept /held; target 100 GiB reached");
    let (_, acts) = task::decide(&h.cfg, &observing, &Input::Observed(Snapshot { podman_bytes: 50 * GIB, ..idle() }), HOUR).unwrap();
    assert_eq!(names(&acts), [task::PODMAN_PRUNE], "podman: system prune -f would run");
}
