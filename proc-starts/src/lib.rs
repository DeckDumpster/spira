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

#[cfg(test)]
mod object_tests {
    use sha2::{Digest, Sha256};

    #[test_id(ttxdm1t)]
    #[test]
    fn committed_bpf_object_matches_its_source() {
        let source = include_bytes!("../bpf/proc_starts.bpf.c");
        let recorded = include_str!("../bpf/proc_starts.bpf.c.sha256").trim();
        let actual: String = Sha256::digest(source).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(actual, recorded, "bpf/proc_starts.bpf.c changed without its object: run proc-starts/bpf/regen.sh and commit both");
    }

    #[test_id(ttw13gi)]
    #[test]
    fn committed_bpf_object_carries_the_programs_and_maps() {
        let object = include_bytes!("../bpf/proc_starts.bpf.o");
        for name in ["tp_btf/sched_process_fork", "tp_btf/sched_process_exec", "tp_btf/sched_process_exit", "tp_btf/cgroup_mkdir", "pending", "starts", "threads", "names", "cost", "dropped"] {
            assert!(object.windows(name.len()).any(|w| w == name.as_bytes()), "{name} missing from the committed object: run proc-starts/bpf/regen.sh");
        }
    }
}
