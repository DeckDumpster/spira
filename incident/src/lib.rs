//! incident — turn a production event into a bead Ops can claim, deduplicated on
//! `external_ref`, through one control-store row per unfiled reference. DESIGN.md has the
//! contract, the scar and what was retired.

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
pub mod supersede;
pub mod stub_bd;
pub mod run;
