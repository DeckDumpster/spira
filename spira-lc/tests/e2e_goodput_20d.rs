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

fn outside(days: &[Day], tolerance: i64, count: impl Fn(&Day) -> i64) -> Vec<String> {
    days.iter().filter(|d| (count(d) - d.git_land).abs() > tolerance).map(|d| d.day.clone()).collect()
}

fn day_start(d: &Day) -> u64 {
    parse_rfc3339(&format!("{}T07:00:00Z", d.day)).unwrap()
}

/// A counting check, not a replay: the table keeps counts and no ids or reds, so this proves
/// goodput's landed count and window arithmetic agree with git, not its harm handling.
/// One stream of all twenty days' landings, `lifecycle_landed` distinct beads spread over each
/// Pacific day (07:00Z to 07:00Z, the last cut at `CUT`): the table keeps counts, not bead ids.
fn landings(days: &[Day]) -> Vec<LoopEvent> {
    days.iter()
        .flat_map(|d| {
            let start = day_start(d);
            let end = if d.day == "2026-10-31" { parse_rfc3339(CUT).unwrap() } else { start + DAY_SECS };
            let step = (end - start) / (d.lifecycle_landed as u64 + 1);
            (1..=d.lifecycle_landed as u64).map(move |i| LoopEvent::Landed {
                bead: format!("b-{}-{i}", d.day),
                repo: "repo-a".into(),
                at: start + i * step,
                reverts: None,
                subject: String::new(),
            })
        })
        .collect()
}

/// What goodput counts as landed in the 24 h ending when `d`'s Pacific day does, over the whole
/// stream.
fn goodput_landed(events: &[LoopEvent], d: &Day) -> i64 {
    let window = Window::new(events, day_start(d) + DAY_SECS, DAY_SECS);
    match goodput(events, &window, Scope::All, &Default::default()).landed {
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
    let events = landings(&days);
    let counted = |d: &Day| goodput_landed(&events, d);
    assert_eq!(outside(&days, TOLERANCE_PER_DAY, counted), Vec::<String>::new());
    for d in &days {
        assert_eq!(counted(d), d.lifecycle_landed, "{}: goodput counts every bead the lifecycle landed that day", d.day);
    }
}
