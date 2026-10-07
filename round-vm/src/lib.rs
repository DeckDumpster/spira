//! round-vm — a pool of one ephemeral round VM from the hypervisor. See DESIGN.md.

pub mod alarm;
pub mod ci_yield;
pub mod cli;
pub mod config;
pub mod pool;
pub mod procs;
pub mod provider;
pub mod pve;
pub mod run;
pub mod schema;
pub mod spool;
pub mod template;

#[cfg(test)]
pub mod testutil;
