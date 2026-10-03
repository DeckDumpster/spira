//! gate — the certification gate (DESIGN.md). The branch is judged merged onto its landing
//! ref; a branch that does not merge is stale (NO_VERDICT reason=conflict), not red.

pub mod basecache;
pub mod cert;
pub mod cgroup;
pub mod compose;
pub mod def;
pub mod engine;
pub mod fence;
pub mod key;
pub mod parse;
pub mod ports;
pub mod real;
pub mod target;
pub mod telemetry;
pub mod wait;

#[cfg(test)]
mod tests;
