//! incident — turn a production event into a bead Ops can claim, deduplicated on
//! `external_ref`, with a write-ahead spool so an event that arrives while the store is
//! down is never lost. DESIGN.md has the contract, the scar and what was retired.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;
pub mod alarm;
pub mod alarms_pass;
pub mod attempts;
pub mod decide;
#[cfg(test)]
mod fake;
pub mod intake;
pub mod page;
pub mod ports;
pub mod real;
pub mod settle;
pub mod settle_real;
pub mod spool;
pub mod supersede;
pub mod stub_bd;
pub mod run;
