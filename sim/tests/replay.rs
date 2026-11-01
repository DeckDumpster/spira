//! The captured days on the simulator: what the round-controller sections a reader would have
//! seen say about them, read back through the store, and what the replay guarantees of itself.
//! Every expectation is taken from the fixtures; per-replay wall time is reported against the
//! 100 ms goal, never asserted. A whole-slice replay costs seconds, so those tests are ignored
//! by the unit tier; `sim-replay` runs their bodies (sim::replay::checks), optimised, from
//! spira/test-sim-replay.sh.

use sim::replay::{self, checks, Replay, Slice};

const SEED: u64 = 31;

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

/// A slice of one line per file, each taken from the 10-31 slice, with `file`'s line replaced.
fn planted(tag: &str, file: &str, line: &str) -> testkit::TempDir {
    let dir = testkit::TempDir::new(tag);
    let real = replay::fixtures_root().join(Slice::Oct31.dir());
    for (f, _) in replay::FILES {
        let first = std::fs::read_to_string(real.join(f)).unwrap().lines().next().unwrap_or("").to_string();
        let text = if f == file { line.to_string() } else { first };
        std::fs::write(dir.path().join(f), if text.is_empty() { text } else { text + "\n" }).unwrap();
    }
    dir
}

#[test]
fn a_slice_line_that_is_not_its_record_is_refused_naming_file_and_line() {
    let window = Slice::Oct31.window();
    let dir = planted("replay-bad-line", "land.ndjson", r#"{"at":"2026-10-31T00:43:53Z","repo":"repo-b","bead":"b-1232","note":"x"}"#);
    let err = Replay::from_dir(SEED, dir.path(), window).err().unwrap();
    assert!(err.starts_with("land.ndjson:1: unknown field `note`"), "{err}");

    let dir = planted("replay-bad-stamp", "land.ndjson", r#"{"at":"2026-10-31 00:43","repo":"repo-b","bead":"b-1232"}"#);
    let err = Replay::from_dir(SEED, dir.path(), window).err().unwrap();
    assert_eq!(err, r#"land.ndjson:1: "2026-10-31 00:43" is not a UTC stamp"#);

    std::fs::remove_file(dir.path().join("round.ndjson")).unwrap();
    let err = Replay::from_dir(SEED, dir.path(), window).err().unwrap();
    assert!(err.contains("round.ndjson"), "a missing file is named: {err}");

    let err = Replay::from_dir(SEED, dir.path(), (window.1, window.0)).err().unwrap();
    assert!(err.contains("is empty"), "{err}");
}

#[test]
fn a_planted_slice_replays_every_line_it_holds() {
    let dir = planted("replay-small", "stop.ndjson", r#"{"at":"2026-10-31T11:23:43Z","kind":"paused","repo":"repo-a","lanes":["rounds"],"bead":"b-1226"}"#);
    let (start, _) = Slice::Oct31.window();
    let mut r = Replay::from_dir(SEED, dir.path(), (start, start + 600)).unwrap();
    r.run();
    assert_eq!(r.lines(), 7);
    assert_eq!(r.world().applied, 7);
    assert_eq!(r.pass_runs(), 10);
    assert_eq!(r.world().paused["repo-a"].as_deref(), Some("b-1226"));
}
