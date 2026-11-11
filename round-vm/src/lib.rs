//! round-vm — a pool of one ephemeral round VM from the hypervisor. See DESIGN.md.

pub mod alarm;
pub mod ci_yield;
pub mod cli;
pub mod config;
pub mod ec2;
pub mod host;
pub mod load;
pub mod lpt;
pub mod machine;
pub mod pool;
pub mod procs;
pub mod progress;
pub mod provider;
pub mod recipe;
pub mod pve;
pub mod route;
pub mod run;
pub mod schema;
pub mod section;
pub mod spill;
pub mod spool;
pub mod telemetry;
pub mod template;
pub mod vm_fault;

#[cfg(test)]
pub mod testutil;
