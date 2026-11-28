//! heimdall — the watchdog over the loop. See DESIGN.md.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod alarm;
pub mod alerts;
pub mod broker;
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
pub mod streak;
pub mod util;

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod classify_cells;
