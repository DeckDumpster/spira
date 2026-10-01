//! testenv — the harness's test runner (sp-5odic). See DESIGN.md for the contract.

pub mod batch;
pub mod bdmeter;
pub mod build;
pub mod cli;
pub mod container;
pub mod fixture;
pub mod landed;
pub mod plan;
pub mod prebuilt;
pub mod reap;
pub mod record;
pub mod run;
pub mod runtime;
pub mod schedule;
pub mod selection;
pub mod settings;
pub mod skipgate;
pub mod suite;
pub mod suites;
pub mod testdb;
pub mod tap;
pub mod timing;
pub mod util;
pub mod verdict;
pub mod warm;
pub mod worktree;
