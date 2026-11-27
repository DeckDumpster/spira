//! dwarf — the runner called to claim one bead, build its worktree, render its brief, run
//! the model session and account for the outcome. See DESIGN.md.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod bash_stdin;
pub mod bd;
pub mod brief;
pub mod capacity;
pub mod capacity_cli;
pub mod checkpoint;
pub mod claim;
pub mod conf;
pub mod decide;
pub mod escape;
pub mod fast_tier;
pub mod search_fence;
pub mod rune_keys;
pub mod ledger;
pub mod mailbox;
pub mod naming;
pub mod ops_sweep;
pub mod pilgrim_world;
pub mod ports;
pub mod restrict;
pub mod run;
pub mod seam;
pub mod session;
pub mod session_feed;
pub mod stack;
pub mod stop;
pub mod direct;
pub mod sweep;
pub mod teardown;
pub mod trace;
pub mod util;
pub mod verdict;
pub mod worktree;

#[cfg(test)]
mod mimir_brief_tests;
#[cfg(test)]
mod persona_tools_tests;

#[cfg(test)]
mod tests;
