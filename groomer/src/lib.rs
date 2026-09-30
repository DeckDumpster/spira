//! groomer — graph hygiene operations for the Spira DAG (DESIGN.md).
//!
//! [`litter`] and [`sweep::parse`] are pure: no I/O, no clock, table-tested directly.
//! [`bd`] and [`seam`] are the impure boundary — the real bead store and the lib.sh
//! detectors (`detect_livelocked` and its STATE-scan siblings) that this crate does not
//! own and must not re-derive (law: leave lib.sh alone; sentinel reaches the same
//! functions the same way). [`sweep`], [`cmds`] and [`unpoison`] compose the two.

pub mod bd;
pub mod cmds;
pub mod litter;
pub mod seam;
pub mod sweep;
pub mod unpoison;
