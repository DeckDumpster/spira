//! Replay tests over the pure core. No containers, no git, no wall clock — every case is
//! driven with recorded or synthetic inputs and checked against a recorded or reasoned
//! outcome.
//!
//! The observed rounds of 2026-09-24/25 (rounds 1-15) live in the brain wiki, which this
//! worktree cannot read (see the bead's own guardrails). The shapes below are instead built
//! from what the bead's own description records about those rounds — the three E double-reds
//! (sp-ui46l, sp-29g55, sp-q4swv) and the five stale members of 2026-09-25 — plus synthetic
//! cases for everything the scope names that no incident happened to exercise: an
//! all-conflict round, a flip on re-run, an express member, a stale stacked base, and a
//! stale bisect record.

use super::*;

fn m(id: &str, rank: u8, express: bool, at: u64) -> Member {
    Member { id: id.into(), tip: format!("{id}-tip"), title: format!("{id}: does a thing"), priority: Some(rank), express, certified_at: at }
}

fn suite(name: &str, outcome: SuiteOutcome, asserts: &[&str]) -> SuiteRun {
    SuiteRun { name: name.into(), outcome, failing_assertions: asserts.iter().map(|s| s.to_string()).collect() }
}

// -- clean_title --------------------------------------------------------------------------

#[test]
fn clean_title_drops_a_foreign_bead_prefix_never_mid_word() {
    assert_eq!(clean_title("sp-ui46l: census.sh orphaned remedy"), "census.sh orphaned remedy");
    assert_eq!(clean_title("sp-s088v.5: x"), "x");
    assert_eq!(clean_title("plain title: with colon"), "plain title: with colon");
    assert_eq!(clean_title("sp-: not an id"), "sp-: not an id");
    assert_eq!(clean_title("  sp-abc: padded  "), "padded");
}

// -- A. Adaptive trigger --------------------------------------------------------------------

#[test]
fn adaptive_n_tracks_certify_rate_times_round_duration() {
    assert_eq!(adaptive_n(PoolHistory { certify_rate_per_min: 0.5, round_duration_mins: 20.0 }), 10);
}

#[test]
fn adaptive_n_clamps_to_four_and_thirty() {
    assert_eq!(adaptive_n(PoolHistory { certify_rate_per_min: 0.01, round_duration_mins: 1.0 }), 4);
    assert_eq!(adaptive_n(PoolHistory { certify_rate_per_min: 10.0, round_duration_mins: 20.0 }), 30);
}

#[test]
fn pool_full_cuts_a_round() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-b", 2, false, 10), m("sp-c", 2, false, 20), m("sp-d", 2, false, 30)];
    let t = TriggerInputs { pool: &pool, now: 40, last_arrival: Some(30), n: 4, q_minutes: 30, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::PoolFull(4)));
}

#[test]
fn idle_with_nothing_new_cuts_a_round_even_under_n() {
    let pool = vec![m("sp-a", 2, false, 0)];
    let t = TriggerInputs { pool: &pool, now: 1800, last_arrival: Some(0), n: 10, q_minutes: 30, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::Idle { waited_mins: 30 }));
}

#[test]
fn under_n_and_not_yet_idle_does_not_cut() {
    let pool = vec![m("sp-a", 2, false, 0)];
    let t = TriggerInputs { pool: &pool, now: 60, last_arrival: Some(0), n: 10, q_minutes: 30, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), None);
}

#[test]
fn empty_pool_never_cuts_on_idle() {
    let t = TriggerInputs { pool: &[], now: 10_000, last_arrival: None, n: 4, q_minutes: 30, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), None);
}

#[test]
fn express_member_cuts_at_once_regardless_of_pool_size() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-x", 1, true, 5)];
    let t = TriggerInputs { pool: &pool, now: 6, last_arrival: Some(5), n: 30, q_minutes: 60, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::Express("sp-x".into())));
}

#[test]
fn main_red_cuts_at_once_and_outranks_express() {
    let pool = vec![m("sp-x", 1, true, 0)];
    let t = TriggerInputs { pool: &pool, now: 1, last_arrival: Some(0), n: 30, q_minutes: 60, main_red: true, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::MainRed));
}

/// The only back pressure is an open batch PR: while one is open, an ordinary pool/idle
/// trigger must not fire (law-queue-back-pressure-is-an-open-pr) — the pool that fills
/// during this round is the next round, cut once the open PR closes.
#[test]
fn open_batch_pr_suppresses_ordinary_triggers() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-b", 2, false, 10), m("sp-c", 2, false, 20), m("sp-d", 2, false, 30)];
    let t = TriggerInputs { pool: &pool, now: 10_000, last_arrival: Some(30), n: 4, q_minutes: 1, main_red: false, batch_open: true };
    assert_eq!(should_cut(&t), None);
}

/// An express or main-red fix pipelines on top of the open PR's head even while it is
/// still open (A: "while a batch PR is in CI, the next round builds and validates on top
/// of that PR's head").
#[test]
fn express_still_cuts_while_a_batch_pr_is_open() {
    let pool = vec![m("sp-x", 1, true, 0)];
    let t = TriggerInputs { pool: &pool, now: 1, last_arrival: Some(0), n: 4, q_minutes: 30, main_red: false, batch_open: true };
    assert_eq!(should_cut(&t), Some(TriggerReason::Express("sp-x".into())));
}

// -- B. Combine: merge, order, set-aside -----------------------------------------------------

#[test]
fn combine_orders_express_first_then_rank_then_arrival() {
    let pool = vec![m("sp-late", 1, false, 100), m("sp-early", 1, false, 10), m("sp-low", 3, false, 5), m("sp-x", 5, true, 50)];
    let merges: BTreeMap<Id, MergeResult> = pool.iter().map(|p| (p.id.clone(), MergeResult::Ok)).collect();
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced });
    let ids: Vec<&str> = combined.merged.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["sp-x", "sp-early", "sp-late", "sp-low"]);
    assert!(combined.set_aside.is_empty());
}

#[test]
fn a_conflicting_member_is_set_aside_never_merged() {
    let pool = vec![m("sp-a", 1, false, 0), m("sp-b", 1, false, 0)];
    let mut merges = BTreeMap::new();
    merges.insert("sp-a".to_string(), MergeResult::Ok);
    merges.insert("sp-b".to_string(), MergeResult::Conflict);
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced });
    assert_eq!(combined.merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["sp-a"]);
    assert_eq!(combined.set_aside, vec![SetAside { id: "sp-b".into(), reason: SetAsideReason::Conflict { deleted_suites: vec![] } }]);
}

/// Synthetic: an all-conflict round — every member set aside, nothing merged, and the
/// round still produces a defined (empty) membership rather than panicking or defaulting
/// to "merge everyone".
#[test]
fn all_conflict_round_merges_nothing() {
    let pool = vec![m("sp-a", 1, false, 0), m("sp-b", 2, false, 0), m("sp-c", 3, false, 0)];
    let merges: BTreeMap<Id, MergeResult> = pool.iter().map(|p| (p.id.clone(), MergeResult::Conflict)).collect();
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced });
    assert!(combined.merged.is_empty());
    assert_eq!(combined.set_aside.len(), 3);
}

/// A member absent from the merge-results map (never attempted) is treated as a conflict,
/// not silently merged — a missing lookup fails closed.
#[test]
fn a_member_missing_from_merge_results_fails_closed_to_set_aside() {
    let pool = vec![m("sp-a", 1, false, 0)];
    let merges: BTreeMap<Id, MergeResult> = BTreeMap::new();
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced });
    assert!(combined.merged.is_empty());
    assert_eq!(combined.set_aside[0].id, "sp-a");
}

/// A member already sequenced behind a dependency by an earlier round stays set aside
/// even when it now merges cleanly — the dependency, not the merge, decides.
#[test]
fn a_sequenced_member_stays_set_aside_even_if_it_now_merges() {
    let pool = vec![m("sp-a", 1, false, 0)];
    let mut merges = BTreeMap::new();
    merges.insert("sp-a".to_string(), MergeResult::Ok);
    let deleted = BTreeMap::new();
    let mut sequenced = BTreeMap::new();
    sequenced.insert("sp-a".to_string(), "sp-dep".to_string());
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced });
    assert!(combined.merged.is_empty());
    assert_eq!(combined.set_aside, vec![SetAside { id: "sp-a".into(), reason: SetAsideReason::Dependency { waits_on: "sp-dep".into() } }]);
}

// -- F. Staleness: compare against when the base ref moved, not the base commit's time ------

/// The 2026-09-25 incident: a batch merge commit is created before the batch lands, so the
/// base head's own commit time never looks newer than a member certified moments earlier,
/// and the retry never fires. Comparing against the fetch that observed the ref move fires
/// it immediately instead.
#[test]
fn stale_retry_fires_on_ref_move_even_when_commit_time_predates_certification() {
    let member_certified_at = 1_000;
    let base_commit_time = 900; // the batch merge commit's own (earlier) timestamp
    let base_ref_moved_at = 1_100; // when the fetch actually observed the base move
    assert!(!stale_retry_due(member_certified_at, base_commit_time), "commit-time comparison misses it, as it did on 2026-09-25");
    assert!(stale_retry_due(member_certified_at, base_ref_moved_at));
}

#[test]
fn no_retry_when_the_base_has_not_moved_since_certification() {
    assert!(!stale_retry_due(1_000, 1_000));
    assert!(!stale_retry_due(1_000, 500));
}

/// Synthetic: a stale stacked base — the next round was building on an open batch PR's
/// head, but that PR landed (or was superseded) meanwhile, so the stacked head is itself
/// stale and must be treated as moved, same as any other base-ref move.
#[test]
fn a_stacked_base_that_lands_counts_as_the_base_moving() {
    let member_certified_at = 1_000;
    let stacked_head_landed_at = 1_050;
    assert!(stale_retry_due(member_certified_at, stacked_head_landed_at));
}

// -- Suite classification: green / flip / double-red -----------------------------------------

#[test]
fn a_green_suite_stays_green() {
    let first = vec![suite("test-a", SuiteOutcome::Green, &[])];
    let v = classify(&first, &[]);
    assert_eq!(v[0].classification, Classification::Green);
}

/// Synthetic: a flip on re-run — red once, green on the immediate re-run, so it is not
/// attributed to any member.
#[test]
fn red_then_green_on_rerun_flips() {
    let first = vec![suite("test-flaky", SuiteOutcome::Red, &["assertion X failed"])];
    let rerun = vec![suite("test-flaky", SuiteOutcome::Green, &[])];
    let v = classify(&first, &rerun);
    assert_eq!(v[0].classification, Classification::Flip);
}

#[test]
fn red_on_both_runs_is_double_red() {
    let first = vec![suite("test-real", SuiteOutcome::Red, &["assertion Y failed"])];
    let rerun = vec![suite("test-real", SuiteOutcome::Red, &["assertion Y failed"])];
    let v = classify(&first, &rerun);
    assert_eq!(v[0].classification, Classification::DoubleRed);
    assert_eq!(v[0].failing_assertions, vec!["assertion Y failed"]);
}

/// A suite red on the first run but absent from the re-run set (e.g. the re-run was never
/// reached, or its result could not be read) fails closed to double-red rather than being
/// assumed fixed.
#[test]
fn a_red_suite_missing_from_the_rerun_fails_closed_to_double_red() {
    let first = vec![suite("test-real", SuiteOutcome::Red, &["assertion Y failed"])];
    let v = classify(&first, &[]);
    assert_eq!(v[0].classification, Classification::DoubleRed);
}

// -- E. A test ahead of its code (the commonest double-red shape) ---------------------------

/// The three recorded double-reds of 2026-09-24/25 were this shape: a bead landing a test
/// (or lint) asserting behaviour another, unlanded bead provides.
#[test]
fn each_recorded_test_ahead_of_code_shape_sequences_behind_its_dependency() {
    for (member, dep, assertion) in [
        ("sp-ui46l", "sp-zc2a", "expected census.sh orphaned remedy from sp-zc2a, got nothing"),
        ("sp-29g55", "sp-kc9v4", "probe defect fixed by sp-kc9v4 not yet on main"),
        ("sp-q4swv", "sp-e19x2", "lint expects the guard landed in sp-e19x2"),
    ] {
        let own = vec![suite("test-own", SuiteOutcome::Red, &[assertion])];
        let sa = test_ahead_of_code(&member.to_string(), &own).expect("should sequence behind the named dependency");
        assert_eq!(sa.reason, SetAsideReason::TestAheadOfCode { waits_on: dep.into() });
    }
}

#[test]
fn a_double_red_naming_no_bead_is_not_test_ahead_of_code() {
    let own = vec![suite("test-own", SuiteOutcome::Red, &["some unrelated assertion failed"])];
    assert_eq!(test_ahead_of_code(&"sp-a".to_string(), &own), None);
}

#[test]
fn a_green_own_suite_is_never_test_ahead_of_code() {
    let own = vec![suite("test-own", SuiteOutcome::Green, &[])];
    assert_eq!(test_ahead_of_code(&"sp-a".to_string(), &own), None);
}

#[test]
fn waits_on_ignores_a_bare_sp_dash_with_no_digits() {
    assert_eq!(waits_on("see sp- for details"), None);
    assert_eq!(waits_on("no bead named here"), None);
}

#[test]
fn waits_on_strips_trailing_punctuation() {
    assert_eq!(waits_on("blocked on sp-abc12."), Some("sp-abc12".to_string()));
}

// -- Bisect: bound to tips, expired on any tip change or any batch landing -------------------

fn tips(pairs: &[(&str, &str)]) -> BTreeMap<Id, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn bisect_state_is_not_expired_while_nothing_changed() {
    let state = BisectState { members: vec!["sp-a".into(), "sp-b".into()], tips: tips(&[("sp-a", "t1"), ("sp-b", "t2")]) };
    let current = tips(&[("sp-a", "t1"), ("sp-b", "t2")]);
    assert!(!bisect_expired(&state, &current, false));
}

/// Synthetic: a stale bisect record — a member's tip moved (it was re-certified after a
/// fix) since the bisect group was recorded, so the record is no longer evidence about the
/// current branch and must be expired rather than reused.
#[test]
fn bisect_state_expires_when_a_members_tip_changes() {
    let state = BisectState { members: vec!["sp-a".into(), "sp-b".into()], tips: tips(&[("sp-a", "t1"), ("sp-b", "t2")]) };
    let current = tips(&[("sp-a", "t1-new"), ("sp-b", "t2")]);
    assert!(bisect_expired(&state, &current, false));
}

#[test]
fn bisect_state_expires_when_any_batch_lands_even_with_unchanged_tips() {
    let state = BisectState { members: vec!["sp-a".into()], tips: tips(&[("sp-a", "t1")]) };
    let current = tips(&[("sp-a", "t1")]);
    assert!(bisect_expired(&state, &current, true));
}

#[test]
fn bisect_next_halves_toward_the_bad_group_until_pinned() {
    let tips_all = tips(&[("sp-a", "ta"), ("sp-b", "tb"), ("sp-c", "tc"), ("sp-d", "td")]);
    let state = BisectState { members: vec!["sp-a".into(), "sp-b".into(), "sp-c".into(), "sp-d".into()], tips: tips_all.clone() };
    let bad = vec!["sp-a".to_string(), "sp-b".to_string()];
    let next = bisect_next(&state, &bad).expect("two members still need splitting");
    assert_eq!(next.members, vec!["sp-a".to_string()]);
    let pinned = bisect_next(&next, &["sp-a".to_string()]);
    assert_eq!(pinned, None, "a single member is pinned, not split further");
}

#[test]
fn bisect_split_divides_evenly_rounding_up_the_first_half() {
    let members = vec!["sp-a".to_string(), "sp-b".to_string(), "sp-c".to_string()];
    let t = tips(&[("sp-a", "ta"), ("sp-b", "tb"), ("sp-c", "tc")]);
    let (a, b) = bisect_split(&members, &t);
    assert_eq!(a.members, vec!["sp-a".to_string(), "sp-b".to_string()]);
    assert_eq!(b.members, vec!["sp-c".to_string()]);
}

// -- D. PR record: full titles, never cut mid-word, foreign prefix dropped, express first ----

#[test]
fn pr_record_lists_full_titles_with_foreign_prefixes_dropped_and_express_first() {
    let members = vec![
        Member { id: "sp-a".into(), tip: "ta".into(), title: "sp-a: a fairly long descriptive title that must not be cut".into(), priority: Some(2), express: false, certified_at: 0 },
        Member { id: "sp-x".into(), tip: "tx".into(), title: "an express fix".into(), priority: Some(9), express: true, certified_at: 0 },
    ];
    let pr = pr_record(&members);
    assert_eq!(pr.members, vec!["sp-a".to_string(), "sp-x".to_string()]);
    assert!(pr.body.contains("a fairly long descriptive title that must not be cut"));
    assert!(!pr.body.contains("sp-a: a fairly"));
    assert!(pr.body.contains("express"));
}

#[test]
fn combine_then_pr_record_puts_express_first_regardless_of_pool_order() {
    let pool = vec![m("sp-a", 1, false, 0), m("sp-x", 9, true, 5)];
    let merges: BTreeMap<Id, MergeResult> = pool.iter().map(|p| (p.id.clone(), MergeResult::Ok)).collect();
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced });
    let pr = pr_record(&combined.merged);
    assert_eq!(pr.members, vec!["sp-x".to_string(), "sp-a".to_string()]);
}

// -- Structured events: one per transition ---------------------------------------------------

#[test]
fn cut_event_names_the_trigger_and_membership() {
    let combined = Combined { merged: vec![m("sp-a", 1, false, 0)], set_aside: vec![] };
    let e = cut_event(&TriggerReason::PoolFull(4), &combined);
    assert_eq!(e.kind, "cut");
    assert_eq!(e.ids, vec!["sp-a".to_string()]);
    assert!(e.text.contains("pool reached 4"));
}

#[test]
fn skipped_event_carries_its_reason() {
    let e = skipped_event("under N and not yet idle");
    assert_eq!(e.kind, "skipped");
    assert!(e.text.contains("under N and not yet idle"));
}

#[test]
fn evicted_event_for_a_conflict_names_deleted_suites() {
    let sa = SetAside { id: "sp-a".into(), reason: SetAsideReason::Conflict { deleted_suites: vec!["test-old.sh".into()] } };
    let e = evicted_event(&sa);
    assert_eq!(e.kind, "evicted");
    assert!(e.text.contains("test-old.sh"));
}

#[test]
fn opened_event_names_every_member() {
    let pr = pr_record(&[m("sp-a", 1, false, 0), m("sp-b", 2, false, 0)]);
    let e = opened_event(&pr);
    assert_eq!(e.kind, "opened");
    assert_eq!(e.ids, vec!["sp-a".to_string(), "sp-b".to_string()]);
}

#[test]
fn abandoned_event_names_the_member_and_why() {
    let e = abandoned_event(&"sp-a".to_string(), "judgement found main itself red; reverted on main instead");
    assert_eq!(e.kind, "abandoned");
    assert!(e.text.contains("sp-a"));
}

// -- End-to-end shaped replay: a full round on synthetic pool/merge/suite/bisect inputs ------

/// A full round through the pure core in sequence, shaped like the incidents the bead
/// names: three certified members, one express arrives mid-pool and forces an immediate
/// cut, one of the two non-express members conflicts (F: handed to the landing pass with
/// its deleted suites), and the remaining pair's combined run has one flip and one E-shaped
/// double-red that sequences behind its dependency rather than reaching judgement.
#[test]
fn a_full_round_from_trigger_through_pr_record() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-b", 3, false, 10), m("sp-x", 1, true, 20)];
    let trig = TriggerInputs { pool: &pool, now: 21, last_arrival: Some(20), n: 30, q_minutes: 60, main_red: false, batch_open: false };
    let reason = should_cut(&trig).expect("the express member forces an immediate cut");
    assert_eq!(reason, TriggerReason::Express("sp-x".into()));

    let mut merges = BTreeMap::new();
    merges.insert("sp-a".to_string(), MergeResult::Ok);
    merges.insert("sp-b".to_string(), MergeResult::Conflict);
    merges.insert("sp-x".to_string(), MergeResult::Ok);
    let mut deleted = BTreeMap::new();
    deleted.insert("sp-b".to_string(), vec!["test-retired.sh".to_string()]);
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced });
    assert_eq!(combined.merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["sp-x", "sp-a"]);
    assert_eq!(combined.set_aside.len(), 1);

    let first = vec![
        suite("test-flaky", SuiteOutcome::Red, &["timing assertion failed"]),
        suite("test-real", SuiteOutcome::Red, &["waits on sp-dep99"]),
        suite("test-fine", SuiteOutcome::Green, &[]),
    ];
    let rerun = vec![suite("test-flaky", SuiteOutcome::Green, &[]), suite("test-real", SuiteOutcome::Red, &["waits on sp-dep99"])];
    let verdicts = classify(&first, &rerun);
    let by_name: BTreeMap<&str, &SuiteVerdict> = verdicts.iter().map(|v| (v.name.as_str(), v)).collect();
    assert_eq!(by_name["test-flaky"].classification, Classification::Flip);
    assert_eq!(by_name["test-real"].classification, Classification::DoubleRed);
    assert_eq!(by_name["test-fine"].classification, Classification::Green);

    let double_red = &by_name["test-real"];
    let own = vec![SuiteRun { name: "test-real".into(), outcome: SuiteOutcome::Red, failing_assertions: double_red.failing_assertions.clone() }];
    let sa = test_ahead_of_code(&"sp-a".to_string(), &own).expect("the double-red names its dependency");
    assert_eq!(sa.reason, SetAsideReason::TestAheadOfCode { waits_on: "sp-dep99".into() });

    let pr = pr_record(&combined.merged);
    assert_eq!(pr.members, vec!["sp-x".to_string(), "sp-a".to_string()]);

    let events = [cut_event(&reason, &combined), evicted_event(&combined.set_aside[0]), opened_event(&pr)];
    assert_eq!(events.iter().map(|e| e.kind).collect::<Vec<_>>(), vec!["cut", "evicted", "opened"]);
}

// -- Judgement: when the crate cannot resolve a red mechanically, summon the persona --------

#[test]
fn judgement_for_is_none_when_nothing_survives_the_rerun_as_double_red() {
    let first = vec![suite("test-flaky", SuiteOutcome::Red, &[]), suite("test-fine", SuiteOutcome::Green, &[])];
    let rerun = vec![suite("test-flaky", SuiteOutcome::Green, &[])];
    let verdicts = classify(&first, &rerun);
    assert_eq!(judgement_for(&verdicts), None);
}

#[test]
fn judgement_for_names_every_surviving_double_red() {
    let first = vec![
        suite("test-a", SuiteOutcome::Red, &["a failed"]),
        suite("test-b", SuiteOutcome::Red, &["b failed"]),
        suite("test-c", SuiteOutcome::Green, &[]),
    ];
    let verdicts = classify(&first, &[]); // nothing re-run: fails closed to DoubleRed (B)
    let j = judgement_for(&verdicts).expect("two suites are still red after the re-run");
    assert_eq!(j.source, RedSource::Local);
    assert_eq!(j.suites, vec!["test-a".to_string(), "test-b".to_string()]);
}

#[test]
fn judgement_for_ci_is_none_on_an_empty_red_list() {
    assert_eq!(judgement_for_ci(&[]), None);
}

#[test]
fn judgement_for_ci_wraps_the_suites_ci_reported_red() {
    let j = judgement_for_ci(&["test-x".to_string()]).expect("ci reported a red suite");
    assert_eq!(j.source, RedSource::Ci);
    assert_eq!(j.suites, vec!["test-x".to_string()]);
}

#[test]
fn judgement_event_names_source_and_suites() {
    let j = Judgement { source: RedSource::Ci, suites: vec!["test-x".into(), "test-y".into()] };
    let e = judgement_event(&j, "spira");
    assert_eq!(e.kind, "judgement");
    assert!(e.text.contains("spira"));
    assert!(e.text.contains("CI"));
    assert!(e.text.contains("test-x,test-y"));
}

#[test]
fn judgement_body_carries_repo_suites_members_and_evidence() {
    let j = Judgement { source: RedSource::Local, suites: vec!["test-real".into()] };
    let body = judgement_body("spira", &j, &["sp-a".to_string(), "sp-x".to_string()], "/run/batch-results/spira-123");
    assert!(body.contains("Repo: spira"));
    assert!(body.contains("local corpus"));
    assert!(body.contains("test-real"));
    assert!(body.contains("sp-a, sp-x"));
    assert!(body.contains("/run/batch-results/spira-123"));
}

#[test]
fn judgement_body_renders_no_members_explicitly() {
    let j = Judgement { source: RedSource::Local, suites: vec!["test-real".into()] };
    let body = judgement_body("spira", &j, &[], "/evidence");
    assert!(body.contains("Round member(s): (none)"));
}

/// The recorded round 1 of 2026-09-24: three E-shaped double-reds (sp-ui46l, sp-29g55,
/// sp-q4swv — see `each_recorded_test_ahead_of_code_shape_sequences_behind_its_dependency`
/// above). What was actually done for each was sequencing behind its named dependency, never
/// a persona summon — so `judgement_for` must flag exactly these three by name (the crate's
/// half of the record), and `test_ahead_of_code` must still resolve each to the same
/// dependency a person resolved it to by hand (the persona's half, exercised without a live
/// aeon). Together they are round 1 replayed end to end through this bead's own additions.
#[test]
fn round_1s_three_double_reds_are_flagged_for_judgement_and_resolve_like_what_was_done() {
    let round1 = [
        ("test-census", "sp-ui46l", "sp-zc2a", "expected census.sh orphaned remedy from sp-zc2a, got nothing"),
        ("test-probe", "sp-29g55", "sp-kc9v4", "probe defect fixed by sp-kc9v4 not yet on main"),
        ("test-lint", "sp-q4swv", "sp-e19x2", "lint expects the guard landed in sp-e19x2"),
    ];
    let first: Vec<SuiteRun> = round1.iter().map(|(suite_name, _, _, assertion)| suite(suite_name, SuiteOutcome::Red, &[assertion])).collect();
    let verdicts = classify(&first, &first); // re-run reproduces the same failure: still red both times
    for (suite_name, ..) in round1 {
        assert_eq!(verdicts.iter().find(|v| v.name == suite_name).unwrap().classification, Classification::DoubleRed);
    }

    let j = judgement_for(&verdicts).expect("round 1 has three surviving double-reds");
    assert_eq!(j.source, RedSource::Local);
    assert_eq!(j.suites.len(), 3);

    for (suite_name, member, dep, assertion) in round1 {
        let own = vec![suite(suite_name, SuiteOutcome::Red, &[assertion])];
        let sa = test_ahead_of_code(&member.to_string(), &own).expect("round 1's shape always names a dependency");
        assert_eq!(sa.reason, SetAsideReason::TestAheadOfCode { waits_on: dep.into() }, "{member} must resolve exactly as it did on 2026-09-24");
    }
}
