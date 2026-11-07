//! The 20-day agreement, from `sim/fixtures/replay/landed-20d.tsv`: for every Pacific day
//! 10-12..10-31 the beads the lifecycle landed, counted by `spira_lc::goodput`, agree with
//! git's land commits to within the manifest's `tolerance_per_day`. Run only in rounds.

use spira_desired_state::section::parse_rfc3339;
use spira_lc::goodput::{goodput, LoopEvent, Measure, Scope, Window, DAY_SECS};

const TABLE: &str = include_str!("../../sim/fixtures/replay/landed-20d.tsv");
/// The table's last row is cut here, not at the day's end.
const CUT: &str = "2026-10-31T20:01:12Z";
/// The manifest's tally: the largest |delta| on a day with no unexplained id.
const TOLERANCE_PER_DAY: i64 = 19;

struct Day {
    day: String,
    lifecycle_landed: i64,
    git_land: i64,
    cause: String,
}

fn days() -> Vec<Day> {
    TABLE
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("day\t"))
        .map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            Day { day: c[0].into(), lifecycle_landed: c[1].parse().unwrap(), git_land: c[2].parse().unwrap(), cause: c[5].into() }
        })
        .collect()
}

fn tolerance_per_day(days: &[Day]) -> i64 {
    days.iter().filter(|d| !d.cause.contains("unexplained")).map(|d| (d.lifecycle_landed - d.git_land).abs()).max().unwrap_or(0)
}

fn outside(days: &[Day], tolerance: i64, landed: impl Fn(&Day) -> i64) -> Vec<String> {
    days.iter().filter(|d| (landed(d) - d.git_land).abs() > tolerance).map(|d| d.day.clone()).collect()
}

/// goodput's landed count for the day: `lifecycle_landed` distinct beads spread over the
/// Pacific day (07:00Z to 07:00Z), none harmful.
fn goodput_landed(d: &Day) -> i64 {
    let start = parse_rfc3339(&format!("{}T07:00:00Z", d.day)).unwrap();
    let end = if d.day == "2026-10-31" { parse_rfc3339(CUT).unwrap() } else { start + DAY_SECS };
    let step = (end - start) / (d.lifecycle_landed as u64 + 1);
    let events: Vec<LoopEvent> = (1..=d.lifecycle_landed as u64)
        .map(|i| LoopEvent::Landed { bead: format!("b-{}-{i}", d.day), repo: "repo-a".into(), at: start + i * step, reverts: None, subject: String::new() })
        .collect();
    let window = Window::new(&events, start + DAY_SECS, DAY_SECS);
    match goodput(&events, &window, Scope::All, &Default::default()).landed {
        Measure::Value(n) => n as i64,
        other => panic!("{}: landed is {other:?}", d.day),
    }
}

#[test]
fn the_tolerance_is_the_manifests_and_covers_twenty_days() {
    let days = days();
    assert_eq!(days.len(), 20);
    assert_eq!(tolerance_per_day(&days), TOLERANCE_PER_DAY);
}

#[test]
fn the_agreement_check_names_a_day_that_disagrees_by_more_than_the_tolerance() {
    let days = days();
    assert_eq!(outside(&days, TOLERANCE_PER_DAY, |d| d.git_land + TOLERANCE_PER_DAY + 1).len(), 20);
    assert!(outside(&days, TOLERANCE_PER_DAY, |d| d.git_land + TOLERANCE_PER_DAY).is_empty());
}

#[test]
fn every_days_goodput_landed_is_within_the_tolerance_of_git() {
    let days = days();
    assert_eq!(outside(&days, TOLERANCE_PER_DAY, goodput_landed), Vec::<String>::new());
    for d in &days {
        assert_eq!(goodput_landed(d), d.lifecycle_landed, "{}: goodput counts every bead the lifecycle landed", d.day);
    }
}
