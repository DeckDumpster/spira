//! The captured days on the simulator: what the round-controller sections a reader would have
//! seen say about them, read back through the store, and what the replay guarantees of itself.
//! Every expectation is taken from the fixtures; per-replay wall time is reported against the
//! 100 ms goal, never asserted. A whole-slice replay costs seconds, so those tests are ignored
//! by the unit tier; `sim-replay` runs their bodies (sim::replay::checks), optimised, from
//! spira/test-sim-replay.sh.

use sim::replay::checks;

macro_rules! slice_sized {
    ($($name:ident),* $(,)?) => {$(
        #[test]
        #[ignore = "slice-sized: run by sim-replay (spira/test-sim-replay.sh)"]
        fn $name() {
            checks::$name();
        }
    )*};
}

slice_sized!(
    the_publisher_sees_the_stacked_parent_from_its_first_refused_child,
    the_publisher_sees_the_repeat_red_member_re_cut_at_one_tip,
    the_held_out_lanes_sections_show_their_recorded_rounds_against_landings,
    an_applied_ack_changes_nothing_in_the_replayed_world_and_no_record_is_dropped,
    two_runs_with_one_seed_trace_the_same,
    hooks_run_at_their_periods_across_the_whole_day_and_the_pass_reads_what_was_published,
);

#[test]
fn every_slice_check_is_named_for_its_test() {
    let names: Vec<_> = checks::CHECKS.iter().map(|(n, _)| *n).collect();
    let src = include_str!("replay.rs");
    for n in &names {
        assert!(src.contains(&format!("    {n},\n")), "{n} has no ignored test here");
    }
    assert_eq!(names.len(), 6);
}
