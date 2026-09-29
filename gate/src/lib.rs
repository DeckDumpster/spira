//! gate — the certification gate (DESIGN.md). The branch is judged merged onto its landing
//! ref; a branch that does not merge is stale (NO_VERDICT reason=conflict), not red.

pub mod cert;
pub mod compose;
pub mod engine;
pub mod fence;
pub mod key;
pub mod parse;
pub mod ports;
pub mod real;
pub mod telemetry;

#[cfg(test)]
mod tests;
