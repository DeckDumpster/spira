//! groomer — graph hygiene operations for the Spira DAG (DESIGN.md).
//!
//! [`litter`] is pure: no I/O, no clock, table-tested directly.
//! [`bd`] and [`seam`] are the impure boundary — the real bead store and the stranded-work
//! detectors (`strand::detectors`) that this crate does not own and must not re-derive.
//! [`cmds`] and [`unpoison`] compose the two.

pub mod bd;
pub mod cmds;
pub mod deadlocked;
pub mod litter;
pub mod seam;
pub mod unpoison;
