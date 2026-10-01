//! aeon — the runner summoned to claim one bead, build its worktree, render its brief, run
//! the model session and account for the outcome. See DESIGN.md.

pub mod bd;
pub mod brief;
pub mod capacity;
pub mod capacity_cli;
pub mod claim;
pub mod conf;
pub mod decide;
pub mod escape;
pub mod ledger;
pub mod naming;
pub mod ports;
pub mod restrict;
pub mod run;
pub mod seam;
pub mod session;
pub mod stack;
pub mod sweep;
pub mod teardown;
pub mod trace;
pub mod util;
pub mod verdict;
pub mod worktree;

#[cfg(test)]
mod tests;
