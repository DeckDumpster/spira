//! watchtower as a library: the sweep's gather, render and escalations and the sentinel's
//! detectors, so the scheduler's `watch.tower` task calls them in-process. `main.rs` is argv
//! dispatch and environment resolution over these modules.

pub mod conditions;
pub mod cpu_throttle;
pub mod czar_outcome;
pub mod deadline;
pub mod deploy_fault;
pub mod disk_mem;
pub mod drift;
pub mod env;
pub mod ctrl_gate;
pub mod failed_units;
pub mod fsstat;
pub mod gate_wait;
pub mod git;
pub mod incident;
pub mod lc;
pub mod lapsed;
pub mod load_fence;
pub mod lock_holders;
pub mod log;
pub mod pr_stall;
pub mod probes;
pub mod release_skew;
pub mod sccache_wedge;
pub mod seams;
pub mod dolt_drop;
pub mod slow_query;
pub mod sweep;
pub mod throttle;
pub mod process;
