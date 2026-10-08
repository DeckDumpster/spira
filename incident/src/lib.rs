//! incident — turn a production event into a bead Ops can claim, deduplicated on
//! `external_ref`, with a write-ahead spool so an event that arrives while the store is
//! down is never lost. DESIGN.md has the contract, the scar and what was retired.

pub mod alarm;
pub mod decide;
pub mod ports;
pub mod real;
pub mod spool;
pub mod run;
