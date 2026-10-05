//! landing-pass — the one landing pass for every land mode. See DESIGN.md for the contract
//! this implements.

pub mod ask;
pub mod budget;
pub mod cli;
pub mod gateq;
pub mod halt;
pub mod land_verify;
pub mod lifecycle;
pub mod model;
pub mod order;
pub mod pass;
pub mod ports;
pub mod pr;
pub mod pr_branch;
pub mod push;
pub mod real;
pub mod records;
pub mod report;
pub mod seam;
pub mod signals;
pub mod util;

#[cfg(test)]
mod testutil;
#[cfg(test)]
mod tests;
