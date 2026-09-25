//! Fixture replays. Each case is a sequence of snapshots the queue actually produced on
//! 2026-09-24, and the exact event kinds a reader must see for it.

use super::*;

fn m(id: &str) -> Member {
    Member { id: id.into(), tip: format!("{id}-tip") }
}

fn batch(pr: &str, head: &str, ids: &[&str]) -> Batch {
    Batch { pr: pr.into(), head: head.into(), members: ids.iter().map(|i| m(i)).collect() }
}

fn beads(spec: &[(&str, u8, &str)]) -> BTreeMap<String, Bead> {
    spec.iter()
        .map(|(id, p, t)| (id.to_string(), Bead { priority: Some(*p), title: t.to_string(), express: false }))
        .collect()
}

fn snap(now: u64) -> Snapshot {
    Snapshot { now, ..Default::default() }
}

fn kinds(evs: &[Event]) -> Vec<&'static str> {
    evs.iter().map(|e| e.kind).collect()
}

/// Run a sequence from a fresh state; return every event after the baseline.
fn replay(snaps: &[Snapshot]) -> Vec<Event> {
    let mut st = RepoState::default();
    let mut all = Vec::new();
    for (i, s) in snaps.iter().enumerate() {
        let (n, evs) = step(&st, s, Limits::default());
        st = n;
        if i > 0 {
            all.extend(evs);
        }
    }
    all
}

#[test]
fn clean_title_drops_a_foreign_bead_prefix() {
    assert_eq!(clean_title("sp-c3q60: census.sh orphaned remedy"), "census.sh orphaned remedy");
    assert_eq!(clean_title("sp-s088v.5: x"), "x");
    assert_eq!(clean_title("plain title: with colon"), "plain title: with colon");
    assert_eq!(clean_title("sp-: not an id"), "sp-: not an id");
}

#[test]
fn first_snapshot_is_a_baseline_not_an_opening() {
    let mut s = snap(0);
    s.batch = Some(batch("323", "h1", &["sp-a"]));
    let (_, evs) = step(&RepoState::default(), &s, Limits::default());
    assert_eq!(kinds(&evs), vec!["watching"]);
}

#[test]
fn open_red_eject_repush_land() {
    let b = beads(&[("sp-a", 0, "a"), ("sp-b", 3, "b")]);
    let mut s0 = snap(0);
    s0.beads = b.clone();
    let mut s1 = snap(30);
    s1.beads = b.clone();
    s1.batch = Some(batch("324", "h1", &["sp-a", "sp-b"]));
    s1.ci = Some(Ci::Pending);
    let mut s2 = s1.clone();
    s2.now = 60;
    s2.ci = Some(Ci::Red);
    let mut s3 = s2.clone();
    s3.now = 90;
    s3.batch = Some(batch("324", "h2", &["sp-a"]));
    s3.ci = Some(Ci::Pending);
    let mut s4 = snap(120);
    s4.beads = b.clone();
    s4.outcome = Some(Outcome {
        pr: "324".into(),
        state: PrState::Merged,
        on_base: vec![("sp-a".into(), Some(true))],
    });
    let evs = replay(&[s0, s1, s2, s3, s4]);
    assert_eq!(kinds(&evs), vec!["opened", "ci", "ci", "ejected", "repushed", "ci", "landed"]);
    assert!(evs[3].text.contains("sp-b (P3) b ejected from PR 324 after a red run"), "{}", evs[3].text);
    assert!(evs[6].text.contains("verified on base"), "{}", evs[6].text);
}

#[test]
fn a_merge_whose_members_are_not_on_base_is_not_a_landing() {
    let mut s0 = snap(0);
    s0.batch = Some(batch("9", "h", &["sp-a", "sp-b"]));
    let mut s1 = snap(30);
    s1.outcome = Some(Outcome {
        pr: "9".into(),
        state: PrState::Merged,
        on_base: vec![("sp-a".into(), Some(true)), ("sp-b".into(), Some(false))],
    });
    let evs = replay(&[s0, s1]);
    assert_eq!(kinds(&evs), vec!["landed-unverified"]);
    assert!(evs[0].text.contains("sp-b"));
}

/// Batch 325: an express eviction closed the PR and wrote no log line at all. The members
/// went back to CERTIFIED and 326 opened in the same pass.
#[test]
fn eviction_with_no_log_line_is_reported_from_state() {
    let mut s0 = snap(0);
    s0.batch = Some(batch("325", "h", &["sp-a", "sp-b", "sp-c"]));
    let mut s1 = snap(30);
    s1.certified = vec!["sp-b".into(), "sp-c".into(), "sp-a".into()];
    s1.batch = Some(batch("326", "h2", &["sp-x"]));
    s1.outcome = Some(Outcome { pr: "325".into(), state: PrState::Closed, on_base: vec![] });
    let evs = replay(&[s0, s1]);
    assert_eq!(kinds(&evs), vec!["closed-unlanded", "opened"]);
    assert!(evs[0].text.contains("evicted or abandoned"), "{}", evs[0].text);
}

/// Batch 327: three P3s forced in by a 13-hour-old bisect group while nine P0s waited.
#[test]
fn stale_bisect_cut_reports_forced_cut_and_priority_inversion() {
    let b = beads(&[
        ("sp-bf31a", 3, "x"),
        ("sp-rf4xe", 3, "y"),
        ("sp-ui46l", 3, "z"),
        ("sp-p0a", 0, "urgent a"),
        ("sp-p0b", 0, "urgent b"),
    ]);
    let mut s0 = snap(0);
    s0.beads = b.clone();
    let mut s1 = snap(47_000);
    s1.beads = b;
    s1.batch = Some(batch("327", "h", &["sp-bf31a", "sp-rf4xe", "sp-ui46l"]));
    s1.bisect = Some((vec!["sp-ui46l".into(), "sp-bf31a".into(), "sp-rf4xe".into()], 200));
    s1.certified = vec!["sp-p0a".into(), "sp-p0b".into()];
    let evs = replay(&[s0, s1]);
    assert_eq!(kinds(&evs), vec!["opened", "forced-cut", "priority-inversion"]);
    assert!(evs[1].text.contains("780m ago"), "{}", evs[1].text);
    assert!(evs[2].text.contains("2 more urgent"), "{}", evs[2].text);
}

#[test]
fn unknown_priorities_never_manufacture_an_inversion() {
    let mut s1 = snap(30);
    s1.batch = Some(batch("1", "h", &["sp-a"]));
    s1.certified = vec!["sp-b".into()];
    let evs = replay(&[snap(0), s1]);
    assert_eq!(kinds(&evs), vec!["opened"]);
}

#[test]
fn idle_with_certified_work_stalls_once() {
    let mk = |t| {
        let mut s = snap(t);
        s.certified = vec!["sp-a".into()];
        s
    };
    let evs = replay(&[mk(0), mk(300), mk(700), mk(1300)]);
    assert_eq!(kinds(&evs), vec!["stall"]);
}

#[test]
fn a_head_that_never_moves_stalls_once() {
    let mk = |t| {
        let mut s = snap(t);
        s.batch = Some(batch("5", "h", &["sp-a"]));
        s.ci = Some(Ci::Pending);
        s
    };
    let evs = replay(&[mk(0), mk(1000), mk(2800), mk(4000)]);
    assert_eq!(kinds(&evs), vec!["ci", "stall"]);
}

/// A poll that could not read the forge must never read as a quiet queue.
#[test]
fn unreadable_is_blind_not_quiet_and_reported_once() {
    let bad = |t| {
        let mut s = snap(t);
        s.errors = vec!["forge unreachable".into()];
        s
    };
    let evs = replay(&[snap(0), bad(30), bad(60), snap(90)]);
    assert_eq!(kinds(&evs), vec!["blind", "recovered"]);
}

/// A blind poll must not be read as "the batch went away".
#[test]
fn blind_poll_does_not_close_the_open_batch() {
    let mut s0 = snap(0);
    s0.batch = Some(batch("7", "h", &["sp-a"]));
    let mut bad = snap(30);
    bad.errors = vec!["open file unreadable".into()];
    let mut s2 = s0.clone();
    s2.now = 60;
    let evs = replay(&[s0, bad, s2]);
    assert_eq!(kinds(&evs), vec!["blind", "recovered"]);
}
