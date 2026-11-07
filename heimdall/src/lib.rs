//! heimdall — the watchdog over the loop. See DESIGN.md.

pub mod alerts;
pub mod conditions;
pub mod classify;
pub mod judge;
pub mod fallback;
pub mod gather;
pub mod heartbeat;
pub mod hosted;
pub mod pass;
pub mod reconcile;
pub mod restart;
pub mod state;
pub mod util;

#[cfg(test)]
pub(crate) mod testing;
