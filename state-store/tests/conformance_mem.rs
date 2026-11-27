//! The conformance suite on the in-memory reference backend, one test per scenario.

#[macro_use]
extern crate spira_test_id;
use state_store::conformance::{self as suite, Rig, SCENARIOS};
use state_store::MemBackend;

struct Mem;

impl Rig for Mem {
    type B = MemBackend;
    fn fresh(&self) -> MemBackend {
        MemBackend::new()
    }
    fn reopen(&self, b: &MemBackend) -> MemBackend {
        b.clone()
    }
}

macro_rules! scenarios {
    ($( #[test_id($id:ident)] $name:ident ),* $(,)?) => {
        $( #[test_id($id)] #[test] fn $name() { suite::$name(&Mem); } )*
        const COVERED: &[&str] = &[$(stringify!($name)),*];
    };
}

scenarios!(
    #[test_id(t19t7rj)] cas_conflict_on_a_stale_mod_rev,
    #[test_id(tk7pfrj)] a_refused_txn_writes_nothing_but_its_events_key_carrying_the_refusal,
    #[test_id(tz9lzli)] one_main_per_txn_with_main_sub_per_key,
    #[test_id(tfkxact)] the_next_main_stays_greater_after_deleting_the_newest_keys_and_reopening,
    #[test_id(tx1ryhd)] create_with_an_equal_value_succeeds_and_a_different_value_conflicts,
    #[test_id(t5ktd40)] list_pages_and_list_rev_come_from_one_snapshot_while_a_commit_lands_between_pages,
    #[test_id(tkva3dc)] watch_resumes_from_a_revision,
    #[test_id(t804h97)] a_watch_on_a_quiet_prefix_returns_progress_rev,
    #[test_id(tpyla1m)] closing_the_watches_answers_a_held_watch_and_every_later_one_with_progress,
    #[test_id(tl8j933)] closing_the_watches_still_delivers_events_already_committed,
    #[test_id(tfn75s4)] watch_returns_too_old_after_compaction,
    #[test_id(twzjroa)] a_value_overwritten_or_deleted_inside_the_ring_reads_as_of_a_revision_between_the_writes,
    #[test_id(tjrte71)] get_many_answers_what_get_answers_per_key_at_the_head_and_below_it,
    #[test_id(tylgyjz)] a_lock_expires_on_the_virtual_clock,
    #[test_id(t4sz2ye)] a_paused_holders_fenced_write_is_refused_after_expiry_and_reacquisition,
    #[test_id(tl2iz35)] reopen_extends_every_lock,
    #[test_id(t3333ek)] a_lock_ttl_below_the_minimum_is_refused,
    #[test_id(ti3x8a1)] a_finalizer_blocks_delete_until_the_write_clearing_it_removes_the_key,
    #[test_id(toihckq)] an_owner_reference_orphan_is_collected,
    #[test_id(tz0rxbj)] an_owner_deleted_with_more_dependents_than_a_pass_is_drained_over_passes,
    #[test_id(tyagdal)] a_put_naming_an_owner_the_batch_writes_or_one_that_lives_queues_no_marker,
    #[test_id(tpmduig)] an_owner_recreated_before_collection_keeps_its_dependents,
    #[test_id(tr1qpt0)] owned_by_names_a_keys_dependents_until_they_drop_the_reference,
    #[test_id(t4pxz6r)] a_request_retry_after_an_ambiguous_commit_reports_already_applied,
    #[test_id(tb4lx07)] retries_of_lock_create_delete_and_a_refusal_report_the_original_outcome,
    #[test_id(tulkgau)] the_gc_never_expires_a_receipt,
    #[test_id(taed5y1)] a_txn_touching_only_telemetry_writes_no_events_key_and_its_refusal_stores_nothing,
    #[test_id(t6fil6v)] a_txn_touching_telemetry_and_intents_writes_one_events_key_with_both_sub_events,
    #[test_id(tgefl8x)] a_put_under_intents_alone_writes_one_events_key,
    #[test_id(tn3ahlk)] a_put_under_telemetry_or_intents_without_ttl_secs_is_refused,
    #[test_id(tco494r)] ttl_keys_are_gone_after_their_ttl_and_present_one_tick_before,
    #[test_id(tl6tcdc)] a_rewrite_restarts_a_keys_ttl,
    #[test_id(te07k20)] a_reopen_renews_no_lifetime,
    #[test_id(t0m4pub)] a_gc_expiry_writes_no_events_key,
    #[test_id(trsasth)] a_retained_prefix_put_may_carry_a_ttl_and_expires_only_with_one,
    #[test_id(tats6r0)] a_multi_prefix_list_reads_one_snapshot_while_a_commit_lands_between_prefixes,
    #[test_id(tnfoyas)] txn_ops_at_the_limit_pass_and_one_over_is_refused,
    #[test_id(t507ztl)] txn_compares_at_the_limit_pass_and_one_over_is_refused,
    #[test_id(tldfsmo)] a_cut_at_the_member_cap_passes_the_limits_and_one_more_is_refused,
    #[test_id(tkhpsu4)] a_key_twice_in_one_txn_is_refused,
    #[test_id(troh2dd)] caller_writes_to_the_contracts_own_prefixes_are_refused,
    #[test_id(tl3jbnm)] a_txn_that_acquires_a_lock_and_writes_a_fenced_key_applies_whole_or_not_at_all,
    #[test_id(tquetgx)] a_txn_acquiring_a_lock_held_by_another_holder_fails_whole,
    #[test_id(trlshji)] lock_check_fails_the_txn_after_the_lock_expired_or_was_retaken,
    #[test_id(tkntxmf)] get_and_list_return_a_locks_expiry_and_renew_moves_it_forward,
    #[test_id(tj64bie)] a_renew_under_a_new_ttl_keeps_the_token_and_moves_the_expiry,
    #[test_id(tuy0ahn)] a_lock_renew_op_keeps_the_token_restarts_the_ttl_and_runs_only_while_held,
    #[test_id(tuzsdez)] a_state_filtered_range_reads_the_keys_in_that_state_at_the_head_and_below_it,
    #[test_id(t1w3y3l)] a_kind_since_range_reads_the_keys_of_that_kind_after_that_time_at_the_head_and_below_it,
    #[test_id(tda3gwh)] a_seq_range_reads_the_keys_above_a_seq_in_seq_order_at_the_head_and_below_it,
    #[test_id(t8o1m88)] gc_range_skips_the_history_prefix,
    #[test_id(t36xanc)] committed_at_is_the_wall_time_of_the_first_audited_commit_at_or_after_a_main,
);

/// Scenarios with no unit test here: each runs over 100 ms (sp-bym7j8) and stays in the suite
/// for the live run alone.
const OVER_BUDGET: &[&str] = &["request_bytes_at_the_limit_pass_and_one_over_is_refused"];

#[test_id(t7tyx2g)]
#[test]
fn every_scenario_of_the_suite_is_its_own_test_here() {
    let want: Vec<&str> = SCENARIOS.iter().copied().filter(|s| !OVER_BUDGET.contains(s)).collect();
    assert_eq!(COVERED, want.as_slice());
}
