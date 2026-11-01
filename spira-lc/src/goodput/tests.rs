use std::collections::BTreeMap;

use super::*;

const H: u64 = 3600;
const MIN: u64 = 60;
/// 2026-10-31T07:00:00Z: midnight Pacific, the start of the 10-31 day.
const DAY0: u64 = 1_793_430_000;

fn landed(bead: &str, repo: &str, at: u64) -> LoopEvent {
    LoopEvent::Landed { bead: bead.into(), repo: repo.into(), at, reverts: None, subject: String::new() }
}

fn revert(bead: &str, of: &str, repo: &str, at: u64) -> LoopEvent {
    LoopEvent::Landed { bead: bead.into(), repo: repo.into(), at, reverts: Some(of.into()), subject: String::new() }
}

fn stop(kind: StopKind, bead: Option<&str>, at: u64) -> LoopEvent {
    LoopEvent::Stop { kind, repo: Some("harness".into()), bead: bead.map(str::to_string), at, reason: String::new() }
}

fn world(kind: StopKind, at: u64) -> LoopEvent {
    LoopEvent::Stop { kind, repo: None, bead: None, at, reason: String::new() }
}

fn red(bead: &str, batch: &str, innocent: &[&str], at: u64, wall_secs: u64) -> LoopEvent {
    LoopEvent::RedAttributed { bead: bead.into(), batch: batch.into(), innocent: innocent.iter().map(|s| s.to_string()).collect(), at, wall_secs }
}

fn eject(bead: &str, batch: &str, at: u64, repeat_n: u32) -> LoopEvent {
    LoopEvent::Eject { bead: bead.into(), batch: batch.into(), at, repeat_n }
}

/// `n` landings in `repo`, one every `every` seconds ending just before `end`.
fn spread(n: usize, repo: &str, end: u64, every: u64) -> Vec<LoopEvent> {
    (0..n).map(|i| landed(&format!("{repo}-{i}"), repo, end - 1 - i as u64 * every)).collect()
}

fn day(events: &[LoopEvent], now: u64) -> Goodput {
    goodput(events, &Window::new(events, now, DAY_SECS), Scope::All, &BTreeMap::new())
}

fn value(m: &Measure) -> f64 {
    match m {
        Measure::Value(v) => *v,
        other => panic!("not a value: {other:?}"),
    }
}

#[test]
fn a_landing_stop_naming_a_bead_charges_its_whole_stop_window_until_its_revert_lands() {
    let end = DAY0 + DAY_SECS;
    let mut events = spread(80, "harness", end, 17 * MIN);
    events.push(landed("w2fute", "harness", DAY0 + 4 * H));
    let stopped = DAY0 + 4 * H + 43 * MIN;
    let reverted = DAY0 + 15 * H + 30 * MIN;
    let clean = value(&day(&events, end).ratio);
    events.push(stop(StopKind::Pause, Some("w2fute"), stopped));
    events.push(revert("revert-w2fute", "w2fute", "harness", reverted));

    let g = day(&events, end);
    let harm = g.harmful.iter().find(|h| h.bead == "w2fute").expect("w2fute is harmful");
    assert_eq!(harm.cost_secs, 647 * MIN, "{harm:?}");
    assert!(harm.evidence.iter().any(|e| e.signal == Signal::LandingStop));
    let fell = clean - value(&g.ratio);
    assert!(fell >= 647.0 / 1440.0, "the day's ratio fell by {fell}, less than its stop window");
}

#[test]
fn one_innocent_eject_alone_moves_the_ratio_by_less_than_a_hundredth() {
    let end = DAY0 + DAY_SECS;
    let base = spread(80, "harness", end, 17 * MIN);
    let clean = value(&day(&base, end).ratio);

    let mut first = base.clone();
    first.push(eject("guilty", "round-1", DAY0 + 10 * H, 1));
    first.push(red("guilty", "round-1", &[], DAY0 + 10 * H, 40 * MIN));
    assert_eq!(value(&day(&first, end).ratio), clean, "a first eject with no innocent member charges nothing");

    let mut innocent = base;
    innocent.push(red("guilty", "round-1", &["bystander"], DAY0 + 10 * H, 12 * MIN));
    let moved = clean - value(&day(&innocent, end).ratio);
    assert!(moved > 0.0 && moved < 0.01, "moved {moved}");
}

#[test]
fn a_bead_landed_and_reverted_with_no_stop_counts_once_at_zero_cost() {
    let end = DAY0 + DAY_SECS;
    let mut events = spread(9, "harness", end, H);
    events.push(landed("bad", "harness", DAY0 + H));
    events.push(revert("undo", "bad", "harness", DAY0 + 2 * H));
    let g = day(&events, end);
    assert_eq!(g.harmful.len(), 1);
    assert_eq!((g.harmful[0].bead.as_str(), g.harmful[0].cost_secs), ("bad", 0));
    assert_eq!(g.harmful[0].evidence[0].signal, Signal::Revert);
    assert!((value(&g.ratio) - 10.0 / 11.0).abs() < 1e-9, "{:?}", g.ratio);
    assert!((value(&g.rate) - 11.0 * 10.0 / 11.0).abs() < 1e-9, "{:?}", g.rate);
}

#[test]
fn an_eject_of_an_innocent_member_charges_the_guilty_bead() {
    let end = DAY0 + DAY_SECS;
    let mut events = spread(10, "harness", end, H);
    events.push(red("guilty", "round-7", &["harness-1"], DAY0 + 5 * H, 30 * MIN));
    let g = day(&events, end);
    assert_eq!(g.harmful.len(), 1, "{:?}", g.harmful);
    let h = &g.harmful[0];
    assert_eq!((h.bead.as_str(), h.cost_secs), ("guilty", 30 * MIN));
    assert_eq!(h.evidence[0].signal, Signal::RedWithInnocents);
    assert_eq!(h.evidence[0].batch.as_deref(), Some("round-7"));
    assert!((value(&g.ratio) - (1.0 - 30.0 / 1440.0)).abs() < 1e-9, "a guilty bead that never landed costs only its minutes");
}

#[test]
fn a_second_eject_charges_its_round_and_a_first_does_not() {
    let end = DAY0 + DAY_SECS;
    let mut events = spread(10, "harness", end, H);
    events.push(red("g", "round-1", &[], DAY0 + 2 * H, 20 * MIN));
    events.push(eject("g", "round-1", DAY0 + 2 * H, 1));
    events.push(red("g", "round-2", &[], DAY0 + 4 * H, 25 * MIN));
    events.push(eject("g", "round-2", DAY0 + 4 * H, 2));
    let g = day(&events, end);
    let h = &g.harmful[0];
    assert_eq!(h.cost_secs, 25 * MIN);
    assert_eq!((h.evidence.len(), h.evidence[0].signal), (1, Signal::RepeatEject));
}

#[test]
fn over_cap_passes_between_a_landing_and_its_revert_are_charged_and_unreverted_ones_to_no_one() {
    let end = DAY0 + DAY_SECS;
    let mut events = spread(10, "harness", end, H);
    events.push(landed("slow", "harness", DAY0 + 2 * H));
    let pass = |at, wall_secs| LoopEvent::OverCapPass { repo: "harness".into(), at, wall_secs };
    events.push(pass(DAY0 + 3 * H, 20 * MIN));
    events.push(pass(DAY0 + 2 * H + 5 * MIN, 20 * MIN));
    events.push(LoopEvent::OverCapPass { repo: "other".into(), at: DAY0 + 3 * H, wall_secs: 20 * MIN });
    let unreverted = day(&events, end);
    assert!(unreverted.harmful.is_empty(), "no revert names anyone: {:?}", unreverted.harmful);

    events.push(revert("undo", "slow", "harness", DAY0 + 4 * H));
    events.push(pass(DAY0 + 5 * H, 20 * MIN));
    let g = day(&events, end);
    let h = g.harmful.iter().find(|h| h.bead == "slow").unwrap();
    let over_cap: Vec<u64> = h.evidence.iter().filter(|e| e.signal == Signal::OverCap).map(|e| e.secs).collect();
    assert_eq!(over_cap, vec![20 * MIN, 5 * MIN], "only this repo's passes, clipped to after the landing");
    assert_eq!(h.cost_secs, 25 * MIN);
}

#[test]
fn a_cost_is_clipped_to_the_window_and_the_ratio_to_zero() {
    let end = DAY0 + DAY_SECS;
    let mut events = vec![landed("w", "harness", DAY0 + H)];
    events.push(stop(StopKind::Pause, Some("w"), DAY0 - 3 * DAY_SECS));
    let g = day(&events, end);
    assert_eq!(g.harmful[0].cost_secs, DAY_SECS, "an unreverted stop runs to now, inside the window only");
    assert_eq!(value(&g.ratio), 0.0);
    assert_eq!(value(&g.rate), 0.0);
}

#[test]
fn a_warm_window_with_no_landings_is_unobservable_never_one_or_zero() {
    let g = day(&[], DAY0);
    assert_eq!(g.ratio, Measure::Unobservable(NO_LANDINGS));
    assert_eq!((g.landed.clone(), g.rate.clone()), (Measure::Value(0.0), Measure::Value(0.0)));
    assert_eq!(g.warming_until, None);
}

#[test]
fn a_halt_in_the_window_is_skipped_so_the_rate_is_the_rate_of_running_time() {
    let end = DAY0 + 30 * H;
    let halt = (DAY0 + 12 * H, DAY0 + 18 * H);
    let mut halted: Vec<LoopEvent> = (0..96u64)
        .map(|i| {
            let t = DAY0 + 1 + i * 15 * MIN;
            landed(&format!("b{i}"), "harness", if t > halt.0 { t + 6 * H } else { t })
        })
        .collect();
    halted.push(world(StopKind::Halt, halt.0));
    halted.push(world(StopKind::Release, halt.1));
    let straight: Vec<LoopEvent> = (0..96u64).map(|i| landed(&format!("b{i}"), "harness", DAY0 + 6 * H + 1 + i * 15 * MIN)).collect();

    let a = day(&halted, end);
    let b = day(&straight, end);
    assert_eq!(value(&a.landed), 96.0);
    assert_eq!(a.rate, b.rate);
    assert_eq!(a.warming_until, None, "a 6 h halt is shorter than the window");
    assert!(!Window::new(&halted, end, DAY_SECS).contains(halt.0 + H), "halted time is outside the window");
}

#[test]
fn after_a_release_from_a_halt_as_long_as_the_window_every_field_warms_up_for_one_window() {
    let released = DAY0 + 30 * H;
    let events = vec![landed("a", "harness", DAY0 - H), world(StopKind::Halt, DAY0), world(StopKind::Release, released)];
    let g = day(&events, released + 2 * H);
    assert_eq!((g.landed, g.ratio, g.rate), (Measure::WarmingUp, Measure::WarmingUp, Measure::WarmingUp));
    assert_eq!(g.warming_until, Some(released + DAY_SECS));
    let w = Window::new(&events, released + 2 * H, DAY_SECS);
    assert_eq!(dwell_p95(&[(released + H, 60)], &w), Measure::WarmingUp);

    let hour = Window::new(&events, released + 2 * H, H);
    assert_eq!(hour.warming_until, None, "the 1 h window is already past the release");
    let warm = day(&events, released + DAY_SECS);
    assert_eq!(warm.warming_until, None);
    assert_eq!(warm.ratio, Measure::Unobservable(NO_LANDINGS));
}

#[test]
fn a_halt_still_open_reaches_the_window_back_past_it() {
    let events = vec![landed("a", "harness", DAY0 + H), world(StopKind::Halt, DAY0 + 2 * H)];
    let now = DAY0 + 40 * H;
    let w = Window::new(&events, now, DAY_SECS);
    assert_eq!(w.start, DAY0 + 2 * H - DAY_SECS);
    assert!(w.contains(DAY0 + H));
    assert_eq!(value(&day(&events, now).landed), 1.0);
    assert_eq!(halts(&events, now), vec![(DAY0 + 2 * H, now, false)]);
}

#[test]
fn a_release_with_no_halt_and_a_second_halt_while_halted_are_ignored() {
    let events = vec![world(StopKind::Release, DAY0), world(StopKind::Halt, DAY0 + H), world(StopKind::Halt, DAY0 + 2 * H), world(StopKind::Release, DAY0 + 3 * H), world(StopKind::Halt, DAY0 + 9 * H)];
    assert_eq!(halts(&events, DAY0 + 5 * H), vec![(DAY0 + H, DAY0 + 3 * H, true)], "a halt after now is not yet");
}

#[test]
fn prose_is_not_a_signal() {
    let end = DAY0 + DAY_SECS;
    let mut events = spread(10, "harness", end, H);
    events.push(LoopEvent::Stop { kind: StopKind::Pause, repo: Some("harness".into()), bead: None, at: DAY0 + H, reason: "harness-1 broke every round".into() });
    events.push(LoopEvent::Landed { bead: "r".into(), repo: "harness".into(), at: DAY0 + 2 * H, reverts: None, subject: "Revert harness-1: it broke landing".into() });
    let g = day(&events, end);
    assert!(g.harmful.is_empty(), "{:?}", g.harmful);
    assert_eq!(value(&g.ratio), 1.0);
}

#[test]
fn a_repo_scope_counts_only_that_repos_landings_and_harm() {
    let end = DAY0 + DAY_SECS;
    let mut events = spread(4, "harness", end, H);
    events.extend(spread(2, "site", end, H));
    events.push(red("g", "round-1", &["x"], DAY0 + 3 * H, 60 * MIN));
    let repo_of = BTreeMap::from([("g".to_string(), "site".to_string())]);
    let w = Window::new(&events, end, DAY_SECS);
    let harness = goodput(&events, &w, Scope::Repo("harness"), &repo_of);
    let site = goodput(&events, &w, Scope::Repo("site"), &repo_of);
    assert_eq!((value(&harness.landed), value(&harness.ratio)), (4.0, 1.0));
    assert_eq!(value(&site.landed), 2.0);
    assert_eq!(site.harmful.len(), 1);
    assert_eq!(goodput(&events, &w, Scope::Repo("none"), &repo_of).ratio, Measure::Unobservable(NO_LANDINGS));
}

#[test]
fn an_hour_window_reports_a_daily_rate() {
    let end = DAY0 + DAY_SECS;
    let events = spread(6, "harness", end, 5 * MIN);
    let w = Window::new(&events, end, H);
    let g = goodput(&events, &w, Scope::All, &BTreeMap::new());
    assert_eq!(value(&g.rate), 6.0 * 24.0);
}

#[test]
fn the_dwell_p95_reads_stays_that_ended_in_the_window() {
    let end = DAY0 + DAY_SECS;
    let w = Window::new(&[], end, DAY_SECS);
    let stays: Vec<(u64, u64)> = (1..=20).map(|i| (end - i * H, i * 100)).chain([(DAY0 - H, 99_999)]).collect();
    assert!((value(&dwell_p95(&stays, &w)) - 1905.0).abs() < 1e-9);
    assert_eq!(dwell_p95(&[(DAY0 - H, 5)], &w), Measure::Unobservable(NO_CERTIFIED_EXITS));
}

#[test]
fn a_stop_after_its_beads_revert_landed_runs_to_now() {
    let end = DAY0 + DAY_SECS;
    let mut events = spread(4, "harness", end, H);
    events.push(landed("w", "harness", DAY0 + H));
    events.push(revert("undo", "w", "harness", DAY0 + 2 * H));
    events.push(stop(StopKind::Pause, Some("w"), DAY0 + 20 * H));
    let g = day(&events, end);
    let h = g.harmful.iter().find(|h| h.bead == "w").unwrap();
    assert!(h.evidence.iter().any(|e| e.signal == Signal::LandingStop && e.secs == 4 * H), "{h:?}");
}
