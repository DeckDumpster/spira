//! gate — the certification gate (DESIGN.md). The branch is judged merged onto its landing
//! ref; a branch that does not merge is red (FAIL reason=no-rebase), never NO_VERDICT.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod basecache;
pub mod buildtiming;
pub mod cert;
pub mod cgroup;
pub mod compose;
pub mod def;
pub mod engine;
pub mod fence;
pub mod fixture;
pub mod flip;
pub mod forced;
pub mod fencecache;
pub mod fences;
pub mod redcache;
pub mod stall;
pub mod key;
pub mod machine;
pub mod own;
pub mod parse;
pub mod publish;
pub mod stopped;
pub mod section;
pub mod ports;
pub mod real;
pub mod target;
pub mod tier;
pub mod unitrecord;
pub mod telemetry;
pub mod testevents;
pub mod toolkey;
pub mod wait;
pub mod warm;

#[cfg(test)]
mod tests;
