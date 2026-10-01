//! groomer — graph hygiene operations for the Spira DAG (DESIGN.md).
//!
//! [`litter`] and [`sweep::parse`] are pure: no I/O, no clock, table-tested directly.
//! [`bd`] and [`seam`] are the impure boundary — the real bead store and the stranded-work
//! detectors (`detect_livelocked` and its STATE-scan siblings, now `strand::detectors`,
//! wave 4.29 sp-8ofmt) that this crate does not own and must not re-derive. [`sweep`],
//! [`cmds`] and [`unpoison`] compose the two.

pub mod bd;
pub mod cmds;
pub mod deadlocked;
pub mod litter;
pub mod seam;
pub mod sweep;
pub mod unpoison;
