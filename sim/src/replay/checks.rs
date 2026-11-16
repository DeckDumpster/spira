//! The whole-slice replay check: the Lifecycle section over the captured 10-31 day, read back
//! through the store. The expectation is taken from the fixtures. A check panics on a failure; it
//! costs seconds, so `sim-replay` runs it and the unit tier's copy in sim/tests/it/replay.rs is
//! ignored.

use std::sync::OnceLock;

use spira_desired_state::section::parse_rfc3339;

use super::{Record, Replay, Slice};
use crate::replay;
const SEED: u64 = 31;

mod lifecycle;
pub use lifecycle::the_lifecycle_sections_one_hour_rate_falls_below_its_day_rate_while_the_stacked_pointer_stands;

static FIXTURES: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Reads the slices from `dir` instead of the source tree's: an installed binary was built
/// elsewhere. Set it before the first check runs.
pub fn use_fixtures(dir: std::path::PathBuf) {
    assert!(FIXTURES.set(dir).is_ok(), "the fixtures root is set once, before any check");
}

fn fixtures() -> std::path::PathBuf {
    FIXTURES.get().cloned().unwrap_or_else(replay::fixtures_root)
}

fn open(slice: Slice) -> Replay {
    Replay::from_dir(SEED, &fixtures().join(slice.dir()), slice.window()).unwrap()
}
const STACKED_REPO: &str = "repo-b";

fn stamp(s: &str) -> u64 {
    parse_rfc3339(s).unwrap()
}

/// The slice's round lines in replay order.
fn rounds(slice: Slice) -> Vec<replay::Round> {
    replay::load(&fixtures().join(slice.dir()))
        .unwrap()
        .into_iter()
        .filter_map(|(_, r)| match r {
            Record::Round(r) => Some(r),
            _ => None,
        })
        .collect()
}

struct Stack {
    from: u64,
    until: u64,
}

/// From the stacked repo's first refused round until the parent it is stacked on ends.
fn stack() -> Stack {
    let rounds = rounds(Slice::Oct31);
    let (start, _) = Slice::Oct31.window();
    let first = rounds
        .iter()
        .find(|r| r.repo == STACKED_REPO && stamp(&r.at) >= start && r.verdict.as_deref() == Some("refused") && r.parent.is_some())
        .expect("a refused round stacked on a parent");
    let parent = first.parent.as_deref().unwrap();
    let ended = rounds.iter().find(|r| r.batch == parent && r.state_to.terminal()).expect("the parent ends in the slice");
    Stack { from: stamp(&first.at), until: stamp(&ended.at) }
}

/// Every check, by the name its unit-tier test carries.
pub const CHECKS: [(&str, fn()); 1] = [
    ("the_lifecycle_sections_one_hour_rate_falls_below_its_day_rate_while_the_stacked_pointer_stands", the_lifecycle_sections_one_hour_rate_falls_below_its_day_rate_while_the_stacked_pointer_stands),
];
