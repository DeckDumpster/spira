//! The attribution state machine against synthetic rounds: every job's result is decided by
//! a table (suite, removal set) → green/red, so each case is a replay with no IO.

use super::*;

fn ids(v: &[&str]) -> Vec<Id> {
    v.iter().map(|s| s.to_string()).collect()
}

fn flat(members: &[&str]) -> Shape {
    Shape::new(&ids(members), &BTreeMap::new())
}

/// Drives an Attributor to completion: the main run's results arrive at `main` times, every
/// job takes `job_secs`, and `verdict(job)` decides its result.
fn replay(
    budget: Budget,
    shape: Shape,
    main: &[(&str, bool, u64)],
    main_end: u64,
    job_secs: u64,
    verdict: impl Fn(&Job) -> JobResult,
) -> (Attributor, Vec<(u64, u32)>) {
    let mut a = Attributor::new(budget, shape.clone(), main.len());
    let mut in_flight: Vec<(u64, u64)> = vec![]; // (job id, finishes at)
    let mut used: Vec<(u64, u32)> = vec![]; // (t, main + attribution slots in use)
    let mut t = 0;
    while t < 10_000 {
        for (s, green, at) in main {
            if *at == t {
                a.on_main_result(s, *green, &shape.members, t);
            }
        }
        if t == main_end {
            a.on_main_done(t);
        }
        let done: Vec<u64> = in_flight.iter().filter(|(_, f)| *f == t).map(|(id, _)| *id).collect();
        in_flight.retain(|(_, f)| *f != t);
        for id in done {
            let job = a.launched.iter().find(|j| j.id == id).unwrap().clone();
            a.on_job_result(id, verdict(&job), t);
        }
        for j in a.next_jobs() {
            in_flight.push((j.id, t + job_secs));
        }
        used.push((t, a.main_in_flight() + a.running() as u32));
        if a.settled() {
            break;
        }
        t += 1;
    }
    (a, used)
}

/// Green iff `culprit` is in the removal set (and the plain rerun, with everyone, is red).
fn owned_by(culprit: &'static str) -> impl Fn(&Job) -> JobResult {
    move |j: &Job| if j.removal.iter().any(|m| m == culprit) { JobResult::Green } else { JobResult::Red }
}

#[test]
fn case_glob_matches_like_a_bash_case_pattern() {
    assert!(case_glob("batcher-cut/src/*.rs", "batcher-cut/src/io.rs"));
    assert!(case_glob("queue/src/*", "queue/src/ops/land.rs"), "* crosses / in a case pattern");
    assert!(case_glob("spira/test-[ab].sh", "spira/test-a.sh"));
    assert!(!case_glob("spira/test-[!ab].sh", "spira/test-a.sh"));
    assert!(case_glob("a?c", "abc") && !case_glob("a?c", "ac"));
    assert!(!case_glob("spira/lib.sh", "spira/lib.shx"));
}

#[test]
fn suspects_that_touch_the_covered_paths_come_first_in_round_order() {
    let members = ids(&["m1", "m2", "m3"]);
    let mut changed = BTreeMap::new();
    changed.insert("m1".to_string(), ids(&["doc/x.md"]));
    changed.insert("m2".to_string(), ids(&["spira/lib.sh#bead_reopen"]));
    changed.insert("m3".to_string(), ids(&["queue/src/ops/land.rs"]));
    let covers = ids(&["queue/src/*", "spira/lib.sh#bead_reopen"]);
    // m2's path carries a '#' only to prove the glob side strips it, not the path side.
    assert_eq!(suspect_order("test-q.sh", Some(&covers), &members, &changed), ids(&["m3", "m1", "m2"]));
    changed.insert("m2".to_string(), ids(&["spira/lib.sh"]));
    assert_eq!(suspect_order("test-q.sh", Some(&covers), &members, &changed), ids(&["m2", "m3", "m1"]));
    // A member that edits the suite itself touches it; no # covers: covers everything.
    changed.insert("m1".to_string(), ids(&["spira/test-q.sh"]));
    assert_eq!(suspect_order("test-q.sh", Some(&covers), &members, &changed), ids(&["m1", "m2", "m3"]));
    assert_eq!(suspect_order("test-q.sh", None, &members, &changed), members);
}

#[test]
fn stacked_dependents_leave_with_their_prerequisite() {
    let mut pre = BTreeMap::new();
    pre.insert("b".to_string(), ids(&["a"]));
    pre.insert("c".to_string(), ids(&["b"]));
    let s = Shape::new(&ids(&["a", "b", "c", "d"]), &pre);
    assert_eq!(s.closure["a"], ids(&["a", "b", "c"]));
    assert_eq!(s.closure["b"], ids(&["b", "c"]));
    assert_eq!(s.closure["d"], ids(&["d"]));
}

#[test]
fn attribution_starts_on_the_first_red_before_the_main_run_ends() {
    let b = Budget { slots: 6, maxpar: 4 };
    let main: Vec<(&str, bool, u64)> = (0..20).map(|i| (["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "t7", "t8", "t9"][i], i != 1, 2 + i as u64 * 5)).collect();
    let (a, _) = replay(b, flat(&["m1", "m2", "m3"]), &main, 200, 3, owned_by("m2"));
    let first = &a.launched[0];
    assert_eq!((first.suite.as_str(), &first.purpose), ("s1", &Purpose::Plain));
    let d = a.decision();
    assert_eq!(d.owners.get("m2"), Some(&ids(&["s1"])));
    let r = &d.records[0];
    assert!(r.settled_at.unwrap() < 200, "settled at {:?}, main ended at 200", r.settled_at);
    assert!(r.settled_before_main_end);
    assert_eq!(r.red_at, 7);
}

#[test]
fn attribution_never_takes_a_slot_from_the_main_run() {
    let b = Budget { slots: 5, maxpar: 4 };
    let main: Vec<(&str, bool, u64)> = vec![("a", false, 1), ("b", false, 2), ("c", true, 30), ("d", true, 31), ("e", true, 32), ("f", true, 60)];
    let (a, used) = replay(b, flat(&["m1", "m2", "m3", "m4"]), &main, 61, 4, owned_by("m4"));
    for (t, n) in &used {
        assert!(*n <= 5, "t={t}: {n} slots in use, budget 5");
    }
    // main_in_flight is what the corpus is owed; main + attribution never exceeding the
    // budget is exactly "attribution only ever took a spare slot".
    assert!(used.iter().any(|(_, n)| *n == 5), "attribution did use the one spare slot");
    assert!(a.settled());
    assert_eq!(a.decision().owners.get("m4"), Some(&ids(&["a", "b"])));
}

#[test]
fn a_zero_slot_budget_waits_for_the_corpus_tail() {
    let b = Budget { slots: 2, maxpar: 2 };
    let mut a = Attributor::new(b, flat(&["m1"]), 3);
    a.on_main_result("a", false, &ids(&["m1"]), 1);
    assert!(a.next_jobs().is_empty(), "2 suites left at maxpar 2: no spare slot");
    a.on_main_result("b", true, &[], 10);
    assert_eq!(a.next_jobs().len(), 1, "one corpus slot freed in the tail");
}

#[test]
fn a_plain_green_is_flaky_and_nothing_else_runs() {
    let (a, _) = replay(Budget { slots: 8, maxpar: 2 }, flat(&["m1", "m2"]), &[("s", false, 1), ("t", true, 2)], 3, 2, |j| {
        if j.purpose == Purpose::Plain { JobResult::Green } else { JobResult::Red }
    });
    let d = a.decision();
    assert_eq!(d.flaky, ids(&["s"]));
    assert!(d.owners.is_empty() && d.base.is_empty());
    assert_eq!(a.launched.len(), 1);
    assert_eq!(d.records[0].reruns, 1);
}

#[test]
fn a_red_with_every_member_removed_is_the_base_s() {
    let (a, _) = replay(Budget { slots: 8, maxpar: 2 }, flat(&["m1", "m2", "m3"]), &[("s", false, 1)], 2, 2, |_| JobResult::Red);
    let d = a.decision();
    assert_eq!(d.base, ids(&["s"]));
    assert!(d.owners.is_empty());
    assert_eq!(d.records[0].outcome, Some(Outcome::Base));
}

#[test]
fn a_one_member_round_needs_no_separate_base_run() {
    let (a, _) = replay(Budget { slots: 8, maxpar: 2 }, flat(&["m1"]), &[("s", false, 1)], 2, 2, owned_by("m1"));
    assert_eq!(a.decision().owners.get("m1"), Some(&ids(&["s"])));
    assert!(a.launched.iter().all(|j| j.purpose != Purpose::Base));
    let (a, _) = replay(Budget { slots: 8, maxpar: 2 }, flat(&["m1"]), &[("s", false, 1)], 2, 2, |_| JobResult::Red);
    assert_eq!(a.decision().base, ids(&["s"]));
}

#[test]
fn every_owner_is_named_with_its_own_suites() {
    let main = vec![("s1", false, 1), ("s2", false, 2), ("s3", false, 3), ("ok", true, 4)];
    let verdict = |j: &Job| {
        let culprit = match j.suite.as_str() {
            "s1" | "s3" => "m1",
            _ => "m3",
        };
        if j.removal.iter().any(|m| m == culprit) { JobResult::Green } else { JobResult::Red }
    };
    let (a, _) = replay(Budget { slots: 3, maxpar: 2 }, flat(&["m1", "m2", "m3"]), &main, 5, 2, verdict);
    let d = a.decision();
    assert_eq!(d.owners.get("m1"), Some(&ids(&["s1", "s3"])));
    assert_eq!(d.owners.get("m3"), Some(&ids(&["s2"])));
    assert_eq!(d.owned_suites(), ids(&["s1", "s2", "s3"]));
}

#[test]
fn two_members_each_breaking_a_suite_alone_is_unattributed() {
    let verdict = |j: &Job| {
        let r = &j.removal;
        let without_both = r.contains(&"m1".to_string()) && r.contains(&"m2".to_string());
        if without_both { JobResult::Green } else { JobResult::Red }
    };
    let (a, _) = replay(Budget { slots: 8, maxpar: 2 }, flat(&["m1", "m2", "m3"]), &[("s", false, 1)], 2, 2, verdict);
    assert_eq!(a.decision().unattributed, ids(&["s"]));
}

#[test]
fn the_touching_suspect_runs_first_and_its_green_cancels_the_rest() {
    let mut a = Attributor::new(Budget { slots: 3, maxpar: 1 }, flat(&["m1", "m2", "m3", "m4"]), 1);
    a.on_main_result("s", false, &ids(&["m3", "m1", "m2", "m4"]), 0);
    a.on_main_done(0);
    let plain = a.next_jobs();
    assert_eq!(plain.len(), 1);
    a.on_job_result(plain[0].id, JobResult::Red, 1);
    let wave = a.next_jobs();
    let purposes: Vec<Purpose> = wave.iter().map(|j| j.purpose.clone()).collect();
    assert_eq!(purposes, vec![Purpose::Base, Purpose::Without("m3".into()), Purpose::Without("m1".into())]);
    a.on_job_result(wave[1].id, JobResult::Green, 2);
    assert!(a.settled());
    assert!(a.next_jobs().is_empty(), "queued runs for m2, m4 cancelled");
    assert_eq!(a.decision().owners.get("m3"), Some(&ids(&["s"])));
}

#[test]
fn an_in_flight_green_is_a_co_owner_but_a_prerequisite_is_not_blamed_for_its_dependent() {
    let mut pre = BTreeMap::new();
    pre.insert("dep".to_string(), ids(&["pre"]));
    let shape = Shape::new(&ids(&["pre", "dep", "x", "y"]), &pre);
    let mut a = Attributor::new(Budget { slots: 10, maxpar: 1 }, shape, 1);
    a.on_main_result("s", false, &ids(&["pre", "dep", "x", "y"]), 0);
    a.on_main_done(0);
    let p = a.next_jobs();
    a.on_job_result(p[0].id, JobResult::Red, 1);
    let wave = a.next_jobs();
    let by = |who: &str| wave.iter().find(|j| j.purpose == Purpose::Without(who.into())).unwrap().id;
    a.on_job_result(by("pre"), JobResult::Green, 2); // removes pre+dep
    a.on_job_result(by("dep"), JobResult::Green, 3); // removes dep only: explains it better
    a.on_job_result(by("x"), JobResult::Green, 4); // an interaction: co-owner
    let d = a.decision();
    assert_eq!(d.owners.keys().cloned().collect::<Vec<_>>(), ids(&["dep", "x"]));
    assert_eq!(d.records[0].settled_at, Some(2), "the first owner settled it");
}

#[test]
fn a_fault_is_retried_once_and_never_read_as_a_result() {
    let mut a = Attributor::new(Budget { slots: 4, maxpar: 1 }, flat(&["m1", "m2"]), 1);
    a.on_main_result("s", false, &ids(&["m1", "m2"]), 0);
    a.on_main_done(0);
    let p = a.next_jobs();
    a.on_job_result(p[0].id, JobResult::Fault, 1);
    let retry = a.next_jobs();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].purpose, Purpose::Plain);
    a.on_job_result(retry[0].id, JobResult::Fault, 2);
    assert!(a.settled());
    assert_eq!(a.decision().unattributed, ids(&["s"]));
}

#[test]
fn nothing_is_owned_when_every_red_is_a_flake_or_the_base_s() {
    let main = vec![("flake", false, 1), ("basered", false, 2), ("ok", true, 3)];
    let verdict = |j: &Job| match (j.suite.as_str(), &j.purpose) {
        ("flake", Purpose::Plain) => JobResult::Green,
        _ => JobResult::Red,
    };
    let (a, _) = replay(Budget { slots: 6, maxpar: 2 }, flat(&["m1", "m2"]), &main, 4, 2, verdict);
    let d = a.decision();
    assert!(d.owners.is_empty());
    assert_eq!((d.flaky.clone(), d.base.clone()), (ids(&["flake"]), ids(&["basered"])));
    assert!(d.unattributed.is_empty());
}

#[test]
fn a_green_main_run_settles_with_no_reruns() {
    let mut a = Attributor::new(Budget::with_default(16, None), flat(&["m1"]), 2);
    a.on_main_result("a", true, &[], 1);
    a.on_main_result("b", true, &[], 2);
    assert!(!a.settled(), "not before the main run says it ended");
    a.on_main_done(3);
    assert!(a.settled());
    assert_eq!(a.decision(), Decision::default());
    assert_eq!(Budget::with_default(16, None).slots, 20);
}

#[test]
fn the_round_attribution_row_names_owners_and_leaves_unsettled_time_empty() {
    let owned = RedRecord {
        suite: "test-a.sh".into(),
        outcome: Some(Outcome::Owner(vec!["sp-1".into(), "sp-2".into()])),
        red_at: 100,
        settled_at: Some(340),
        reruns: 3,
        settled_before_main_end: true,
    };
    let f: std::collections::HashMap<_, _> = owned.tsd_fields("spira", "r7", 2).into_iter().collect();
    assert_eq!(f["outcome"], "owner");
    assert_eq!(f["owner"], "sp-1,sp-2");
    assert_eq!(f["attribution_secs"], "240");
    assert_eq!(f["iteration"], "2");
    assert_eq!(f["settled_before_corpus_end"], "true");
    let open = RedRecord { outcome: None, settled_at: None, ..owned };
    let f: std::collections::HashMap<_, _> = open.tsd_fields("spira", "r7", 1).into_iter().collect();
    assert_eq!(f["outcome"], "unsettled");
    assert_eq!(f["owner"], "");
    assert_eq!(f["attribution_secs"], "", "never 0 for a red that never settled");
}

#[test]
fn an_install_fault_names_the_planted_member_and_costs_a_logarithmic_ladder() {
    let members = ["m1", "m2", "m3", "m4", "m5", "m6", "m7"];
    for bad in members {
        let shape = flat(&members);
        let mut probes = 0;
        let got = attribute_install_fault(&shape, |removal| {
            probes += 1;
            if members.iter().any(|m| *m == bad && !removal.iter().any(|r| r == m)) { JobResult::Red } else { JobResult::Green }
        });
        assert_eq!(got, InstallFault::Owner(bad.to_string()), "planted {bad}");
        assert!(probes <= 6, "planted {bad}: {probes} probes");
    }
}

#[test]
fn an_install_fault_that_survives_removing_everything_is_the_bases() {
    assert_eq!(attribute_install_fault(&flat(&["m1", "m2"]), |_| JobResult::Red), InstallFault::Base);
}

#[test]
fn a_faulting_install_probe_is_retried_once_and_never_exonerates() {
    let shape = flat(&["m1", "m2"]);
    let mut n = 0;
    let got = attribute_install_fault(&shape, |removal| {
        n += 1;
        if n == 1 { JobResult::Fault } else if removal.iter().any(|r| r == "m2") { JobResult::Green } else { JobResult::Red }
    });
    assert_eq!(got, InstallFault::Owner("m2".into()));
    assert_eq!(attribute_install_fault(&shape, |_| JobResult::Fault), InstallFault::Unattributed);
}

#[test]
fn two_members_that_each_break_install_are_not_pinned_on_one() {
    let shape = flat(&["m1", "m2", "m3"]);
    let got = attribute_install_fault(&shape, |removal| {
        if removal.len() < 2 { JobResult::Red } else { JobResult::Green }
    });
    assert_eq!(got, InstallFault::Unattributed);
}

#[test]
fn a_dependent_s_install_fault_is_confirmed_by_removing_its_prerequisite_set() {
    let prereqs = BTreeMap::from([("m2".to_string(), vec!["m1".to_string()])]);
    let shape = Shape::new(&ids(&["m1", "m2", "m3"]), &prereqs);
    let got = attribute_install_fault(&shape, |removal| {
        if removal.iter().any(|r| r == "m1") { JobResult::Green } else { JobResult::Red }
    });
    assert_eq!(got, InstallFault::Owner("m1".into()));
}
