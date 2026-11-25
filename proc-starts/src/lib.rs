//! proc-starts: every process start on the host counted inside the kernel, by unit class, unit,
//! program and starter, drained every [`run::INTERVAL`] and pushed as cumulative series.
//! Holds CAP_BPF and CAP_PERFMON only while it loads and attaches.

#[cfg(test)]
#[macro_use]
extern crate spira_test_id;

pub mod bpf;
pub mod caps;
pub mod classify;
pub mod run;
pub mod totals;

use spira_otel::metrics::MetricsSink;

pub fn sink() -> MetricsSink {
    spira_config::process::metrics_sink("proc-starts")
}
