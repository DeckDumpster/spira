//! rebase-stale — rebases a submitted branch that went stale against its land ref
//! mechanically, so a bead reaches an aeon only for a real content conflict or a red gate.
//! See DESIGN.md for the contract this implements.

pub mod engine;
pub mod git;
pub mod holder;
pub mod record;
pub mod resolve;
pub mod seam;

#[cfg(test)]
mod tests;
