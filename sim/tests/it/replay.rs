//! The captured days on the simulator: what the Lifecycle section a reader would have
//! seen says about it, read back through the store, and what the replay guarantees of itself.
//! Every expectation is taken from the fixtures; per-replay wall time is reported against the
//! 100 ms goal, never asserted. A whole-slice replay costs seconds, so those tests are ignored
//! by the unit tier; `sim-replay` runs their bodies (sim::replay::checks), optimised, from
//! the sim-replay binary.

use sim::replay::checks;

macro_rules! slice_sized {
    ($( #[test_id($id:ident)] $name:ident ),* $(,)?) => {$(
        #[test_id($id)]
        #[test]
        #[ignore = "slice-sized: run by sim-replay"]
        fn $name() {
            checks::$name();
        }
    )*};
}

slice_sized!(
    #[test_id(tmf6jxj)] the_lifecycle_sections_one_hour_rate_falls_below_its_day_rate_while_the_stacked_pointer_stands,
);

#[test_id(tm12f9u)]
#[test]
fn every_slice_check_is_named_for_its_test() {
    let names: Vec<_> = checks::CHECKS.iter().map(|(n, _)| *n).collect();
    let src = include_str!("replay.rs");
    for n in &names {
        assert!(src.contains(&format!("    {n},\n")), "{n} has no ignored test here");
    }
    assert_eq!(names.len(), 1);
}
