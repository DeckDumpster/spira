//! gate-check's pure decision surface as a library: the scheduler's `gate.check` task composes
//! the walk of `main.rs` over a snapshot with these functions, so the binary and the task
//! decide alike.

pub mod engine;
