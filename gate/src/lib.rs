//! gate — the certification gate (DESIGN.md). The branch is judged merged onto its landing
//! ref; a branch that does not merge is stale (NO_VERDICT reason=conflict), not red.

pub mod compose;
pub mod engine;
pub mod key;
pub mod parse;
pub mod ports;
pub mod real;

#[cfg(test)]
mod tests;
