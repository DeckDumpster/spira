//! T1–T6 for `sweep.archive`, from the Acceptance section of
//! `wiki/projects/spira/designs/tasks/sweep-archive.md`. Red until the task is ported.

use std::collections::BTreeMap;

use spira_runtime::tasks::b::kit::{names, testing as t, Input};
use spira_runtime::tasks::c::sweep_archive::{self as task, Carry, Index, Phase, Row, Snapshot, Source, SweepArchive as H};

const SET: &[(&str, &str)] = &[(task::CADENCE_KEY, "1200")];
const TICK: u64 = 1_200_000;

fn src(rel: &str, size: u64, mtime: u64) -> Source {
    Source { rel: rel.into(), size, mtime }
}

/// Three transcripts, the index already matching each size and mtime.
fn matching() -> Snapshot {
    let sources = vec![src("p/a.jsonl", 10, 1), src("p/b.jsonl", 20, 2), src("q/c.jsonl", 30, 3)];
    let index = sources.iter().map(|s| (s.rel.clone(), Row { bytes: s.size, mtime: s.mtime, body_exists: true })).collect::<BTreeMap<_, _>>();
    Snapshot { sources, index, projects_dir_exists: true, projects_dir_configured: false }
}

fn json(s: &Snapshot) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn t1_registered_boots_and_arms_a_20min_tick() {
    t::t1_registered_and_boots::<H>(SET, TICK);
}

#[test]
fn t2_an_index_that_matches_every_transcript_journals_nothing() {
    t::t2_silent::<H>(SET, &json(&matching()));
}

#[test]
fn t3_a_changed_rel_is_stored_the_index_goes_current_and_an_index_while_storing_is_refused() {
    let h = t::handler::<H>(SET);
    let mut grown = matching();
    grown.sources[0].size = 15;
    let (c, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(grown), TICK).unwrap();
    assert_eq!((c.rels.get("p/a.jsonl").copied(), names(&acts)), (Some(Phase::Storing), vec![task::STORE]));
    let refused = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::INDEX.into(), ok: true, body: String::new() }, TICK).unwrap_err();
    assert!(refused.contains("storing"), "{refused}");
    let (c, acts) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::STORE.into(), ok: true, body: "p/a.jsonl".into() }, TICK).unwrap();
    assert_eq!((c.rels.get("p/a.jsonl").copied(), c.index, names(&acts)), (Some(Phase::Stored), Index::Stale, vec![task::INDEX]));
    let (c, _) = task::decide(&h.cfg, &c, &Input::Outcome { effect: task::INDEX.into(), ok: true, body: String::new() }, TICK).unwrap();
    assert_eq!(c.index, Index::Current);
}

#[test]
fn t4_its_effects_are_runnable_and_the_module_spawns_neither_python3_nor_zstd() {
    let sim = sim::doubles::service().names();
    t::t4_runnable::<H>(include_str!("../../src/tasks/c/sweep_archive.rs"), &sim.iter().map(String::as_str).collect::<Vec<_>>());
}

#[test]
fn t5_the_fast_path_stores_new_changed_and_bodiless_rels_and_keeps_vanished_rows() {
    let h = t::handler::<H>(SET);
    let mut snap = matching();
    snap.sources.push(src("r/new.jsonl", 5, 9));
    snap.index.get_mut("p/b.jsonl").unwrap().body_exists = false;
    snap.index.insert("gone/old.jsonl".into(), Row { bytes: 1, mtime: 1, body_exists: true });
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(snap), TICK).unwrap();
    assert_eq!(names(&acts), [task::STORE, task::STORE]);
    assert!(acts.iter().any(|a| a.input.contains("r/new.jsonl")) && acts.iter().any(|a| a.input.contains("p/b.jsonl")), "{acts:?}");
    let (_, acts) = task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { projects_dir_exists: false, ..Snapshot::default() }), TICK).unwrap();
    assert!(acts.is_empty(), "a missing default projects dir stores nothing");
    assert!(task::decide(&h.cfg, &Carry::default(), &Input::Observed(Snapshot { projects_dir_exists: false, projects_dir_configured: true, ..Snapshot::default() }), TICK).is_err(), "a configured one that is missing refuses");
}

#[test]
fn t6_two_virtual_days_are_deterministic_and_within_2_events_an_hour() {
    t::t6_deterministic::<H>(|seed| t::rig::<H>(seed, SET, &json(&matching())), 2, 2);
}
