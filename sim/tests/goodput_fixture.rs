//! Goodput over the captured 10-31 day: the numbers come from `fixtures/replay/1031`, the same
//! typed records the replay applies, run through `spira_lc::goodput`. The landing-stop bead is
//! the manifest's `b-1226` (the bead `sp-w2fute` names); the join check substitutes the
//! lifecycle's attribution rows for the TSD `round-attribution` family, which carries no batch id.

use std::collections::BTreeSet;

use sim::replay::{fixtures_root, load, Attribution, CertPass, Record, ReplayWorld, Slice};
use spira_lc::goodput::{goodput, Goodput, LoopEvent, Measure, Scope, Signal, StopKind, Window, DAY_SECS};

const LANDING_STOP_BEAD: &str = "b-1226";

fn oct31_world() -> ReplayWorld {
    let mut world = ReplayWorld::default();
    for (at, r) in load(&fixtures_root().join(Slice::Oct31.dir())).unwrap() {
        world.apply(at, &r);
    }
    world
}

fn oct31_events() -> Vec<LoopEvent> {
    oct31_world().loop_events
}

/// The landing-stop bead's stop, in seconds: from its pause to the landing of the revert that
/// names it.
fn stop_secs(events: &[LoopEvent]) -> u64 {
    let paused = events.iter().find_map(|e| match e {
        LoopEvent::Stop { kind: StopKind::Pause, bead: Some(b), at, .. } if b == LANDING_STOP_BEAD => Some(*at),
        _ => None,
    });
    let reverted = events.iter().find_map(|e| match e {
        LoopEvent::Landed { reverts: Some(b), at, .. } if b == LANDING_STOP_BEAD => Some(*at),
        _ => None,
    });
    reverted.unwrap() - paused.unwrap()
}

fn day(events: &[LoopEvent]) -> (Window, Goodput) {
    let (_, end) = Slice::Oct31.window();
    let w = Window::new(events, end, DAY_SECS);
    let g = goodput(events, &w, Scope::All, &Default::default());
    (w, g)
}

fn ratio(g: &Goodput) -> f64 {
    match g.ratio {
        Measure::Value(v) => v,
        ref other => panic!("ratio is {other:?}"),
    }
}

#[test]
fn the_landing_stop_beads_cost_is_its_typed_stop_window_and_it_pulls_the_days_ratio_down_by_at_least_that() {
    let events = oct31_events();
    let (w, g) = day(&events);
    let harm = g.harmful.iter().find(|h| h.bead == LANDING_STOP_BEAD).expect("the landing-stop bead is harmful");
    let stop: u64 = harm.evidence.iter().filter(|e| e.signal == Signal::LandingStop).map(|e| e.secs).sum();
    assert_eq!(stop, stop_secs(&events), "the charge is the stop's window");
    assert!((249 * 60..250 * 60).contains(&stop), "the fixture's stop is about 249 min, not the design prose's 647: {stop} s");
    assert!(harm.cost_secs >= stop, "{harm:?}");

    let without: Vec<LoopEvent> = events
        .iter()
        .filter(|e| !matches!(e, LoopEvent::Stop { bead: Some(b), .. } if b == LANDING_STOP_BEAD))
        .cloned()
        .collect();
    let (_, g_without) = day(&without);
    let drop = ratio(&g_without) - ratio(&g);
    assert!(drop + 1e-9 >= stop as f64 / w.secs as f64, "ratio fell by {drop}, the stop is {stop}s of {}s", w.secs);
}

#[test]
fn one_recorded_red_that_cost_an_innocent_member_moves_the_days_ratio_by_under_a_tenth_of_what_the_landing_stop_does() {
    let landings: Vec<LoopEvent> = oct31_events()
        .into_iter()
        .filter_map(|e| match e {
            LoopEvent::Landed { reverts: _, bead, repo, at, subject } => Some(LoopEvent::Landed { bead, repo, at, reverts: None, subject }),
            _ => None,
        })
        .collect();
    let (_, clean) = day(&landings);
    assert_eq!(ratio(&clean), 1.0, "landings alone harm nothing");

    let red = oct31_events()
        .into_iter()
        .find(|e| matches!(e, LoopEvent::RedAttributed { innocent, .. } if !innocent.is_empty()))
        .expect("the day holds a red that took an innocent member down");
    let mut with_red = landings;
    with_red.push(red);
    let (_, g) = day(&with_red);
    let moved = 1.0 - ratio(&g);
    assert!(moved > 0.0 && moved < 0.02, "one red moved the ratio by {moved}");
    assert!(moved < stop_secs(&oct31_events()) as f64 / DAY_SECS as f64 / 10.0, "one red moved the ratio by {moved}");
    assert_eq!(g.harmful.len(), 1, "the owner is charged: {:?}", g.harmful);
}

/// A slice's cert-pass rows whose batch no round row names, and attributed reds inside the
/// window (`from` on) whose batch has cert-pass rows but none red at that pass; the lead-in
/// holds earlier events of its batches but none of the passes, and a few passes were never
/// measured. Also how many reds joined.
fn unjoined(records: &[(u64, Record)], from: u64) -> (Vec<String>, Vec<String>, usize) {
    let rounds: BTreeSet<&str> = records.iter().filter_map(|(_, r)| if let Record::Round(x) = r { Some(x.batch.as_str()) } else { None }).collect();
    let passes: Vec<&CertPass> = records.iter().filter_map(|(_, r)| if let Record::CertPass(x) = r { Some(x) } else { None }).collect();
    let orphan_passes = passes.iter().filter(|p| !rounds.contains(p.batch.as_str())).map(|p| p.batch.clone()).collect();
    let measured: BTreeSet<&str> = passes.iter().map(|p| p.batch.as_str()).collect();
    let reds: BTreeSet<(&str, u32)> = passes.iter().filter(|p| p.cause == "red-attributed").filter_map(|p| Some((p.batch.as_str(), p.pass?))).collect();
    let attributed = records
        .iter()
        .filter_map(|(at, r)| if let Record::Attribution(a) = r { Some((at, a)) } else { None })
        .filter(|(at, a)| **at >= from && measured.contains(a.batch.as_str()));
    let (joined, orphan): (Vec<_>, Vec<_>) = attributed.partition(|(_, a)| reds.contains(&(a.batch.as_str(), a.pass)));
    (orphan_passes, orphan.iter().map(|(_, a)| format!("{} pass {}", a.batch, a.pass)).collect(), joined.len())
}

fn slice_records(slice: Slice) -> Vec<(u64, Record)> {
    load(&fixtures_root().join(slice.dir())).unwrap()
}

#[test]
fn the_join_check_names_a_cert_pass_whose_batch_no_round_holds() {
    let mut records = slice_records(Slice::Oct31);
    let Some(Record::CertPass(p)) = records.iter().find_map(|(_, r)| matches!(r, Record::CertPass(_)).then(|| r.clone())) else { panic!("no cert-pass row") };
    records.push((0, Record::CertPass(CertPass { batch: "repo-a-19700101T000000Z".into(), ..p })));
    let (orphans, ..) = unjoined(&records, 0);
    assert_eq!(orphans, ["repo-a-19700101T000000Z"]);
}

#[test]
fn every_cert_pass_batch_is_the_batch_id_the_rounds_and_attributions_carry() {
    for slice in [Slice::Oct31, Slice::HeldOut] {
        let (passes, reds, joined) = unjoined(&slice_records(slice), slice.window().0);
        assert!(joined > 0, "{slice:?}: no attributed red joined a cert-pass row, so the check proves nothing");
        assert!(passes.is_empty(), "{slice:?}: cert-pass batches no round holds: {passes:?}");
        assert!(reds.is_empty(), "{slice:?}: attributed reds no red cert-pass carries: {reds:?}");
    }
}

#[test]
fn the_join_check_names_an_attributed_red_whose_pass_no_red_cert_pass_carries() {
    let mut records = slice_records(Slice::Oct31);
    let from = Slice::Oct31.window().0;
    let a = records
        .iter()
        .find_map(|(at, r)| match r {
            Record::Attribution(a) if *at >= from => Some(a.clone()),
            _ => None,
        })
        .unwrap();
    let pass = a.pass + 90;
    let batch = a.batch.clone();
    records.push((from + 1, Record::Attribution(Attribution { pass, ..a })));
    let (_, orphans, _) = unjoined(&records, from);
    assert_eq!(orphans, [format!("{batch} pass {pass}")]);
}

#[test]
fn a_recorded_red_is_charged_to_its_owner_in_that_owners_repo_scope() {
    let world = oct31_world();
    let owners: BTreeSet<&str> = world.loop_events.iter().filter_map(|e| if let LoopEvent::RedAttributed { bead, .. } = e { Some(bead.as_str()) } else { None }).collect();
    assert!(!owners.is_empty());
    for o in &owners {
        assert!(world.bead_repo.contains_key(*o), "{o} has no repo, so a repo scope would charge it nothing");
    }
    let (_, end) = Slice::Oct31.window();
    let w = Window::new(&world.loop_events, end, DAY_SECS);
    let g = goodput(&world.loop_events, &w, Scope::Repo("repo-a"), &world.bead_repo);
    let harm = g.harmful.iter().find(|h| h.bead == LANDING_STOP_BEAD).expect("the landing-stop bead is charged in its own repo");
    assert!(harm.evidence.iter().any(|e| e.signal == Signal::RedWithInnocents || e.signal == Signal::RepeatEject), "{harm:?}");
}
