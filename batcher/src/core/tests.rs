//! Replay tests over the pure core. No containers, no git, no wall clock — every case is
//! driven with recorded or synthetic inputs and checked against a recorded or reasoned
//! outcome.
//!
//! The observed rounds of 2026-09-24/25 live in the brain wiki
//! (`wiki/projects/spira/concierge-as-batcher-2026-09-24.md`), reachable from this worktree
//! (sp-z3p8z). Only round 1 is reconstructed below, against that page's own account of it —
//! the design's test strategy names it specifically ("given round 1's three double-reds, its
//! actions must match what was done"). Rounds 2-15 remain unreconstructed: the page is a
//! prose narrative log, not structured data, and each round's facts are told with a
//! different level of detail rather than in a fixture-ready shape. A future aeon with the
//! time to extract them should read that page round by round rather than trust a summary
//! here. The rest of the cases below are synthetic: an all-conflict round, a flip on
//! re-run, an express member, a stale stacked base, and a stale bisect record.

use super::*;

fn m(id: &str, rank: u8, express: bool, at: u64) -> Member {
    Member {
        id: id.into(),
        tip: format!("{id}-tip"),
        title: format!("{id}: does a thing"),
        priority: Some(rank),
        express,
        base_fix: false,
        certified_at: at,
        stack: BTreeMap::new(),
    }
}

/// A member stacked on `prereqs` — `(prereq_id, prereq_id's tip in this pool)` pairs, matching
/// the default tip `m()` gives that prereq (`"{id}-tip"`) unless the test wants a stale one.
fn stacked(id: &str, rank: u8, at: u64, prereqs: &[(&str, &str)]) -> Member {
    Member { stack: prereqs.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect(), ..m(id, rank, false, at) }
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

// -- A. Trigger and selection ---------------------------------------------------------------

// ACCEPTANCE (sp-1oiokx): any non-empty pool with no batch open cuts at once — no count to
// reach, no idle wait to sit out.
#[test]
fn a_pool_of_one_cuts_at_once() {
    let pool = vec![m("sp-a", 2, false, 0)];
    let t = TriggerInputs { pool: &pool, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::PoolFull(1)));
}

fn ids(v: &[Member]) -> Vec<&str> {
    v.iter().map(|m| m.id.as_str()).collect()
}

#[test]
fn no_shared_epic_is_a_catch_all_of_the_whole_pool() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-b.1", 2, false, 1), m("sp-c.1", 2, false, 2)];
    let (round, kind) = select_round(pool);
    assert_eq!(kind, RoundKind::CatchAll);
    assert_eq!(ids(&round), ["sp-a", "sp-b.1", "sp-c.1"]);
}

#[test]
fn two_members_of_one_epic_cut_that_feature_alone() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-f.1", 2, false, 1), m("sp-f.2", 2, false, 2), m("sp-z", 2, false, 3)];
    let (round, kind) = select_round(pool);
    assert_eq!(kind, RoundKind::Feature("sp-f".into()));
    assert_eq!(ids(&round), ["sp-f.1", "sp-f.2"]);
}

#[test]
fn the_epic_parent_itself_belongs_to_its_feature() {
    let pool = vec![m("sp-f", 2, false, 0), m("sp-f.1", 2, false, 1), m("sp-o", 2, false, 2)];
    let (round, kind) = select_round(pool);
    assert_eq!(kind, RoundKind::Feature("sp-f".into()));
    assert_eq!(ids(&round), ["sp-f", "sp-f.1"]);
}

#[test]
fn the_larger_feature_wins_and_a_tie_goes_to_the_lower_root() {
    let pool = vec![m("sp-b.1", 2, false, 0), m("sp-b.2", 2, false, 1), m("sp-a.1", 2, false, 2), m("sp-a.2", 2, false, 3)];
    assert_eq!(select_round(pool).1, RoundKind::Feature("sp-a".into()));
    let pool = vec![m("sp-b.1", 2, false, 0), m("sp-b.2", 2, false, 1), m("sp-b.3", 2, false, 2), m("sp-a.1", 2, false, 3), m("sp-a.2", 2, false, 4)];
    assert_eq!(select_round(pool).1, RoundKind::Feature("sp-b".into()));
}

#[test]
fn what_the_feature_stacks_on_and_what_stacks_on_it_ride_along() {
    let mut dep = m("sp-d", 2, false, 4);
    dep.stack.insert("sp-f.1".into(), "sp-f.1-tip".into());
    let mut second = m("sp-e", 2, false, 5);
    second.stack.insert("sp-d".into(), "sp-d-tip".into());
    let mut f2 = m("sp-f.2", 2, false, 2);
    f2.stack.insert("sp-p".into(), "sp-p-tip".into());
    let pool = vec![m("sp-f.1", 2, false, 1), f2, m("sp-p", 2, false, 0), dep, second, m("sp-z", 2, false, 6)];
    let (round, kind) = select_round(pool);
    assert_eq!(kind, RoundKind::Feature("sp-f".into()));
    assert_eq!(ids(&round), ["sp-f.1", "sp-f.2", "sp-p", "sp-d", "sp-e"]);
}

#[test]
fn an_express_member_rides_in_a_feature_round() {
    let pool = vec![m("sp-f.1", 2, false, 0), m("sp-f.2", 2, false, 1), m("sp-x", 1, true, 2), m("sp-z", 2, false, 3)];
    let (round, _) = select_round(pool);
    assert_eq!(ids(&round), ["sp-f.1", "sp-f.2", "sp-x"]);
}

#[test]
fn pool_full_cuts_a_round() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-b", 2, false, 10), m("sp-c", 2, false, 20), m("sp-d", 2, false, 30)];
    let t = TriggerInputs { pool: &pool, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::PoolFull(4)));
}

#[test]
fn empty_pool_never_cuts_on_idle() {
    let t = TriggerInputs { pool: &[], main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), None);
}

#[test]
fn express_member_cuts_at_once_regardless_of_pool_size() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-x", 1, true, 5)];
    let t = TriggerInputs { pool: &pool, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::Express("sp-x".into())));
}

#[test]
fn main_red_cuts_at_once_and_outranks_express() {
    let pool = vec![m("sp-x", 1, true, 0)];
    let t = TriggerInputs { pool: &pool, main_red: true, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::MainRed));
}

/// While a batch PR is open any non-empty pool prepares the next round on its head, so a
/// round is ready the moment the PR lands (law-queue-back-pressure-is-an-open-pr).
#[test]
fn open_batch_pr_prepares_rather_than_cuts() {
    let pool = vec![m("sp-a", 2, false, 0), m("sp-b", 2, false, 10), m("sp-c", 2, false, 20), m("sp-d", 2, false, 30)];
    let t = TriggerInputs { pool: &pool, main_red: false, batch_open: true };
    assert_eq!(should_cut(&t), Some(TriggerReason::Prepare(4)));
    let t = TriggerInputs { pool: &[], ..t };
    assert_eq!(should_cut(&t), None);
}

/// An express member while a batch is open still triggers, as a round of its own to build,
/// never as a push into the open PR.
#[test]
fn express_triggers_while_a_batch_pr_is_open() {
    let pool = vec![m("sp-x", 1, true, 0)];
    let t = TriggerInputs { pool: &pool, main_red: false, batch_open: true };
    assert_eq!(should_cut(&t), Some(TriggerReason::Express("sp-x".into())));
}

// -- B. Combine: merge, order, set-aside -----------------------------------------------------

#[test]
fn combine_orders_express_first_then_rank_then_arrival() {
    let pool = vec![m("sp-late", 1, false, 100), m("sp-early", 1, false, 10), m("sp-low", 3, false, 5), m("sp-x", 5, true, 50)];
    let merges: BTreeMap<Id, MergeResult> = pool.iter().map(|p| (p.id.clone(), MergeResult::Ok)).collect();
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
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
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
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
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
    assert!(combined.merged.is_empty());
    assert_eq!(combined.set_aside.len(), 3);
}

/// A member whose tip is already an ancestor of the round head (reset to an old base, or no
/// commits of its own) is set aside as Empty, never merged, and never mixed in with a real
/// conflict's reason — sp-5xki9's batcher parity fix.
#[test]
fn an_empty_member_is_set_aside_never_merged() {
    let pool = vec![m("sp-a", 1, false, 0), m("sp-b", 1, false, 0)];
    let mut merges = BTreeMap::new();
    merges.insert("sp-a".to_string(), MergeResult::Ok);
    merges.insert("sp-b".to_string(), MergeResult::Empty);
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
    assert_eq!(combined.merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["sp-a"]);
    assert_eq!(combined.set_aside, vec![SetAside { id: "sp-b".into(), reason: SetAsideReason::Empty }]);
}

// -- Stacked dependents: closure and topological order (design stacked-dependents-2026-09-28) --

/// The acceptance scenario itself, at the pure-core layer: A, B stacked on A, C stacked on
/// B, handed to topo_order in the wrong order (C, A, B) — SEEN RED first against `order_key`
/// alone, which sorts by (express, priority, certified_at) and has no idea B depends on A.
#[test]
fn topo_order_sorts_a_stack_prerequisite_first_regardless_of_pool_order() {
    let a = m("sp-a", 1, false, 0);
    let b = stacked("sp-b", 1, 0, &[("sp-a", "sp-a-tip")]);
    let c = stacked("sp-c", 1, 0, &[("sp-b", "sp-b-tip")]);
    let pool = vec![c.clone(), a.clone(), b.clone()];
    let ordered = topo_order(&pool);
    assert_eq!(ordered.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["sp-a", "sp-b", "sp-c"]);
}

#[test]
fn topo_order_with_no_stacking_reduces_to_order_key() {
    let pool = vec![m("sp-late", 1, false, 100), m("sp-early", 1, false, 10), m("sp-low", 3, false, 5), m("sp-x", 5, true, 50)];
    let ordered = topo_order(&pool);
    assert_eq!(ordered.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["sp-x", "sp-early", "sp-late", "sp-low"]);
}

#[test]
fn stack_sequencing_is_empty_when_every_stacked_prereq_is_in_the_pool_at_its_recorded_tip() {
    let a = m("sp-a", 1, false, 0);
    let b = stacked("sp-b", 1, 0, &[("sp-a", "sp-a-tip")]);
    let sequenced = stack_sequencing(&[a, b]);
    assert!(sequenced.is_empty());
}

/// The tip invariant, extended to round assembly (design §1, "Tip invariant, extended"): a
/// member whose stack names a prerequisite tip that the pool does not currently certify at
/// that exact tip is refused — member_added's stale-stack refusal.
#[test]
fn stack_sequencing_refuses_a_member_whose_prereq_tip_does_not_match_the_pool() {
    let a = m("sp-a", 1, false, 0);
    let b = stacked("sp-b", 1, 0, &[("sp-a", "some-other-sha")]);
    let sequenced = stack_sequencing(&[a, b]);
    assert_eq!(sequenced.get("sp-b"), Some(&"sp-a".to_string()));
}

/// A prerequisite absent from the pool entirely (reworked, ejected, or simply landed and
/// already dropped from `stack` upstream) is the same refusal — no row to confirm the tip
/// against.
#[test]
fn stack_sequencing_refuses_a_member_whose_prereq_is_not_in_the_pool_at_all() {
    let b = stacked("sp-b", 1, 0, &[("sp-a", "sp-a-tip")]);
    let sequenced = stack_sequencing(&[b]);
    assert_eq!(sequenced.get("sp-b"), Some(&"sp-a".to_string()));
}

/// Bullet 3 of the bead: a member whose tip is already an ancestor of the round head because
/// a dependent carrying it merged first is still a member — recorded, never dropped as
/// Empty — while a genuinely-empty member unrelated to any stack keeps today's behaviour.
#[test]
fn combine_keeps_a_stacked_prerequisite_whose_tip_merged_empty() {
    let a = m("sp-a", 1, false, 0);
    let b = stacked("sp-b", 1, 0, &[("sp-a", "sp-a-tip")]);
    let unrelated = m("sp-z", 1, false, 0);
    let pool = vec![a, b, unrelated];
    let mut merges = BTreeMap::new();
    merges.insert("sp-a".to_string(), MergeResult::Empty); // already carried in by sp-b's own merge
    merges.insert("sp-b".to_string(), MergeResult::Ok);
    merges.insert("sp-z".to_string(), MergeResult::Empty); // genuinely no commits of its own
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
    assert_eq!(combined.merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["sp-a", "sp-b"]);
    assert_eq!(combined.set_aside, vec![SetAside { id: "sp-z".into(), reason: SetAsideReason::Empty }]);
}

#[test]
fn evicted_event_for_an_empty_member_names_it_not_a_member() {
    let sa = SetAside { id: "sp-b".into(), reason: SetAsideReason::Empty };
    let e = evicted_event(&sa);
    assert_eq!(e.kind, "evicted");
    assert!(e.text.contains("sp-b"));
    assert!(e.text.contains("not a member"));
}

/// A member absent from the merge-results map (never attempted) is treated as a conflict,
/// not silently merged — a missing lookup fails closed.
#[test]
fn a_member_missing_from_merge_results_fails_closed_to_set_aside() {
    let pool = vec![m("sp-a", 1, false, 0)];
    let merges: BTreeMap<Id, MergeResult> = BTreeMap::new();
    let deleted = BTreeMap::new();
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
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
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
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

/// The design's Section E paragraph names three E-shaped double-reds of 2026-09-24/25 — a
/// bead landing a test (or lint) asserting behaviour another, unlanded bead provides:
/// sp-ui46l, sp-29g55, sp-q4swv. Only sp-q4swv gives a clean positive case here. Round 3's
/// `test-testlib-migrated` recurrence was sp-q4swv, sequenced behind sp-29g55
/// (concierge-as-batcher-2026-09-24.md, "Rounds 3 and after"). sp-ui46l is excluded: its
/// real bead (`bd show sp-ui46l`) is a P3 fix for a `SPIRA_INCIDENT_LABELS` partition-label
/// prefix, closed per standing operator instruction after bouncing from batch 327 — nothing
/// like a test landing ahead of its code. sp-29g55's own real dependency (sp-qvjzb, round
/// 1's occurrence of the same suite) is covered separately below as a negative case, not
/// here — see `waits_on_finds_a_real_dependency_id_with_no_digit_in_its_suffix`.
#[test]
fn a_recorded_test_ahead_of_code_shape_sequences_behind_its_dependency() {
    let assertion = "test-testlib-migrated: a lint ahead of the migration it checks, sequenced behind sp-29g55";
    let own = vec![suite("test-own", SuiteOutcome::Red, &[assertion])];
    let sa = test_ahead_of_code(&"sp-q4swv".to_string(), &own).expect("should sequence behind the named dependency");
    assert_eq!(sa.reason, SetAsideReason::TestAheadOfCode { waits_on: "sp-29g55".into() });
}

/// sp-29g55's real round-1 dependency was sp-qvjzb (concierge-as-batcher-2026-09-24.md,
/// "What the first two rounds actually found": "`sp-29g55` adds a lint that cannot pass
/// until `sp-qvjzb` lands"). The suffix is all letters, so `waits_on` accepts a five-letter
/// suffix as well as one containing a digit.
#[test]
fn waits_on_finds_a_real_dependency_id_with_no_digit_in_its_suffix() {
    assert_eq!(waits_on("adds a lint that cannot pass until sp-qvjzb lands"), Some("sp-qvjzb".to_string()));
    assert_eq!(waits_on("see sp-ish thing"), None);
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
        Member { id: "sp-a".into(), tip: "ta".into(), title: "sp-a: a fairly long descriptive title that must not be cut".into(), priority: Some(2), express: false, base_fix: false, certified_at: 0, stack: BTreeMap::new() },
        Member { id: "sp-x".into(), tip: "tx".into(), title: "an express fix".into(), priority: Some(9), express: true, base_fix: false, certified_at: 0, stack: BTreeMap::new() },
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
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
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
    assert!(e.text.contains("4 waiting, no round running"));
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

// -- Local attribution: ejection and base-fail (sp-hqoap) -----------------------------------

#[test]
fn ejected_event_names_the_member_and_every_suite() {
    let e = ejected_event(&Ejection { id: "sp-a".into(), suites: vec!["test-x.sh".into(), "test-y.sh".into()] });
    assert_eq!(e.kind, "ejected");
    assert_eq!(e.ids, vec!["sp-a".to_string()]);
    assert!(e.text.contains("test-x.sh"));
    assert!(e.text.contains("test-y.sh"));
}

#[test]
fn base_fail_event_names_no_member_and_every_suite() {
    let e = base_fail_event("spira", &["test-x.sh".to_string(), "test-y.sh".to_string()]);
    assert_eq!(e.kind, "basefail");
    assert!(e.ids.is_empty());
    assert!(e.text.contains("test-x.sh"));
    assert!(e.text.contains("test-y.sh"));
}

#[test]
fn base_fail_body_carries_repo_branch_suites_and_evidence() {
    let body = base_fail_body("spira", "spira/queue/123", &["test-x.sh".to_string()], "/run/batch-results/spira-123");
    assert!(body.contains("Repo: spira"));
    assert!(body.contains("spira/queue/123"));
    assert!(body.contains("test-x.sh"));
    assert!(body.contains("/run/batch-results/spira-123"));
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
    let trig = TriggerInputs { pool: &pool, main_red: false, batch_open: false };
    let reason = should_cut(&trig).expect("the express member forces an immediate cut");
    assert_eq!(reason, TriggerReason::Express("sp-x".into()));

    let mut merges = BTreeMap::new();
    merges.insert("sp-a".to_string(), MergeResult::Ok);
    merges.insert("sp-b".to_string(), MergeResult::Conflict);
    merges.insert("sp-x".to_string(), MergeResult::Ok);
    let mut deleted = BTreeMap::new();
    deleted.insert("sp-b".to_string(), vec!["test-retired.sh".to_string()]);
    let sequenced = BTreeMap::new();
    let combined = combine(&CombineInput { pool: &pool, merges: &merges, deleted_suites: &deleted, sequenced: &sequenced, siblings: &BTreeMap::new() });
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

/// The recorded round 1 of 2026-09-24 (concierge-as-batcher-2026-09-24.md, "What the first
/// two rounds actually found"): 481 suites against 17 certified branches, 21 minutes at
/// width 8, **3 red, all red again on the re-run, so none were flip-floppers**:
///
/// - `test-skew-check-release` — `origin/main` itself was red (`sp-fghps` had fixed
///   `run_skew` but left `run_skew_artifact` copying the bare template). No member's own
///   suite named a dependency; what was actually done was a direct fix, `sp-4brh5`, never a
///   sequencing.
/// - `test-script-exec` — `sp-04yh0` left `gate-lib.sh` and `tap-jsonl.sh` with "sourced
///   only" headers the lint could not recognise. Again no named dependency; fixed directly
///   on `sp-04yh0`'s own branch.
/// - `test-testlib-migrated` — `sp-29g55` adds a lint that cannot pass until `sp-qvjzb`
///   lands. Reopened and made to depend on `sp-qvjzb` by hand.
///
/// `test_ahead_of_code` (design Section E) did not exist at the time — all three were
/// resolved by a person. Replayed today, `judgement_for` still flags all three by name, and
/// only the third names a dependency, which `test_ahead_of_code` now sequences behind.
#[test]
fn round_1s_three_double_reds_are_flagged_for_judgement_and_resolve_like_what_was_done() {
    let round1 = [
        ("test-skew-check-release", "origin/main itself is red: run_skew_artifact still copies the bare template, no activated release"),
        ("test-script-exec", "gate-lib.sh and tap-jsonl.sh sourced-only headers not recognised by the lint"),
        ("test-testlib-migrated", "adds a lint that cannot pass until sp-qvjzb lands"),
    ];
    let first: Vec<SuiteRun> = round1.iter().map(|(suite_name, assertion)| suite(suite_name, SuiteOutcome::Red, &[assertion])).collect();
    let verdicts = classify(&first, &first); // re-run reproduces the same failure: still red both times
    for (suite_name, _) in round1 {
        assert_eq!(verdicts.iter().find(|v| v.name == suite_name).unwrap().classification, Classification::DoubleRed);
    }

    let j = judgement_for(&verdicts).expect("round 1 has three surviving double-reds");
    assert_eq!(j.source, RedSource::Local);
    assert_eq!(j.suites.len(), 3);

    for (suite_name, assertion) in round1 {
        let own = vec![suite(suite_name, SuiteOutcome::Red, &[assertion])];
        let expected = (suite_name == "test-testlib-migrated")
            .then(|| SetAsideReason::TestAheadOfCode { waits_on: "sp-qvjzb".into() });
        assert_eq!(test_ahead_of_code(&"member".to_string(), &own).map(|sa| sa.reason), expected, "{suite_name}");
    }
}

// -- terminal_ready (sp-828tp): the one gate both queue.forge and queue.local finish through --

#[test]
fn terminal_ready_passes_when_named_and_binned() {
    let members = vec![m("sp-a", 2, false, 0), m("sp-b", 2, false, 10)];
    let named = vec!["sp-a".to_string(), "sp-b".to_string()];
    assert_eq!(terminal_ready(&members, &named, true), Ok(()));
}

// POSITIVE CONTROL for the whole suite of refusals below: the same inputs, ok=true, prove
// the fixture itself is capable of passing before each case flips exactly one input bad.

#[test]
fn terminal_ready_refuses_a_member_no_commit_names() {
    let members = vec![m("sp-a", 2, false, 0), m("sp-b", 2, false, 10)];
    let named = vec!["sp-a".to_string()]; // sp-b's own commit never merged, or its tip changed
    assert_eq!(terminal_ready(&members, &named, true), Err(Refusal::MemberNotNamed { id: "sp-b".into() }));
}

#[test]
fn terminal_ready_refuses_missing_with_bins_artifacts() {
    let members = vec![m("sp-a", 2, false, 0)];
    let named = vec!["sp-a".to_string()];
    assert_eq!(terminal_ready(&members, &named, false), Err(Refusal::BinsMissing));
}

#[test]
fn terminal_ready_checks_membership_before_bins() {
    let members = vec![m("sp-a", 2, false, 0)];
    // Every input is wrong at once; the membership refusal must be the one reported.
    assert_eq!(terminal_ready(&members, &[], false), Err(Refusal::MemberNotNamed { id: "sp-a".into() }));
}

#[test]
fn base_fix_member_triggers_ahead_of_express_and_lands_alone() {
    let fix = Member { base_fix: true, priority: Some(0), ..m("sp-f", 0, false, 9) };
    let pool = base_fix_lane(vec![m("sp-a", 1, false, 0), m("sp-x", 1, true, 5), fix]);
    assert_eq!(pool.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["sp-f"]);
    let t = TriggerInputs { pool: &pool, main_red: false, batch_open: false };
    assert_eq!(should_cut(&t), Some(TriggerReason::BaseFix("sp-f".into())));
}

#[test]
fn base_fix_lane_leaves_a_pool_without_a_fix_untouched() {
    let pool = vec![m("sp-a", 1, false, 0), m("sp-x", 1, true, 5)];
    assert_eq!(base_fix_lane(pool.clone()), pool);
}

#[test]
fn a_sibling_loser_names_the_p0_it_lost_to_not_the_base() {
    let mut p0 = m("sp-p0", 0, false, 200);
    p0.certified_at = 200;
    let mut p2 = m("sp-p2", 2, false, 0);
    p2.certified_at = 100;
    let sorted = topo_order(&[p2.clone(), p0.clone()]);
    assert_eq!(sorted[0].id, "sp-p0");
    let mut merges = BTreeMap::new();
    merges.insert("sp-p0".to_string(), MergeResult::Ok);
    merges.insert("sp-p2".to_string(), MergeResult::Conflict);
    let mut siblings = BTreeMap::new();
    siblings.insert("sp-p2".to_string(), ("sp-p0".to_string(), vec!["a.rs".to_string()]));
    let combined = combine(&CombineInput { pool: &sorted, merges: &merges, deleted_suites: &BTreeMap::new(), sequenced: &BTreeMap::new(), siblings: &siblings });
    assert_eq!(combined.merged.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), vec!["sp-p0"]);
    let text = evicted_event(&combined.set_aside[0]).text;
    assert!(text.contains("sibling sp-p0") && text.contains("a.rs") && !text.contains("with base"), "{text}");
}
