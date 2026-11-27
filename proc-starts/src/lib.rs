//! proc-starts: every process start on the host counted inside the kernel, by unit class, unit,
//! program and starter, drained every [`run::INTERVAL`] and pushed as cumulative series.
//! Holds CAP_BPF and CAP_PERFMON only while it loads and attaches.
//!
//! The kernel side is the committed `bpf/proc_starts.bpf.o`, built from `bpf/proc_starts.bpf.c`.
//! After any edit to the source, run `proc-starts/bpf/regen.sh` with the flake's clang and commit
//! the object and `bpf/proc_starts.bpf.manifest` together.
//!
//! What each count is ([`totals`] names the series):
//! - `spira_proc_starts_total`: forked processes, counted at first exec (or at exit if none) and
//!   labelled by the exec'd program.
//! - `spira_proc_thread_starts_total`: forked threads. Starts plus thread starts are the
//!   `sched_process_fork` hits, which `spira_proc_starts_bpf_runs_total{prog="fork"}` counts.
//! - `spira_proc_execs_total`: every `sched_process_exec`, including an exec with no fork.
//! - `spira_proc_host_forks_total`: `/proc/stat processes`, which also counts `copy_process`
//!   callers that fire no fork tracepoint.

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
        let manifest = include_str!("../bpf/proc_starts.bpf.manifest");
        let mut fields = std::collections::BTreeMap::new();
        for line in manifest.lines() {
            let (key, value) = line.split_once('=').unwrap_or_else(|| panic!("bpf/proc_starts.bpf.manifest: line without '=': {line}"));
            assert!(["source_sha256", "object_sha256", "clang", "flags"].contains(&key), "bpf/proc_starts.bpf.manifest: unknown key {key}");
            assert!(fields.insert(key, value).is_none(), "bpf/proc_starts.bpf.manifest: duplicate key {key}");
        }
        let recorded = |key: &str| *fields.get(key).unwrap_or_else(|| panic!("{key} missing from bpf/proc_starts.bpf.manifest: run proc-starts/bpf/regen.sh"));
        for key in ["clang", "flags"] {
            assert!(!recorded(key).is_empty(), "{key} empty in bpf/proc_starts.bpf.manifest: run proc-starts/bpf/regen.sh");
        }
        let sha256 = |bytes: &[u8]| Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(sha256(include_bytes!("../bpf/proc_starts.bpf.c")), recorded("source_sha256"), "bpf/proc_starts.bpf.c changed without its object: run proc-starts/bpf/regen.sh and commit both");
        assert_eq!(sha256(include_bytes!("../bpf/proc_starts.bpf.o")), recorded("object_sha256"), "bpf/proc_starts.bpf.o was changed outside proc-starts/bpf/regen.sh: run proc-starts/bpf/regen.sh and commit the object with its manifest");
    }

    #[test_id(ttw13gi)]
    #[test]
    fn committed_bpf_object_carries_the_programs_and_maps() {
        let object = include_bytes!("../bpf/proc_starts.bpf.o");
        for name in ["tp_btf/sched_process_fork", "tp_btf/sched_process_exec", "tp_btf/sched_process_exit", "tp_btf/cgroup_mkdir", "pending", "starts", "threads", "execs", "names", "cost", "dropped"] {
            assert!(object.windows(name.len()).any(|w| w == name.as_bytes()), "{name} missing from the committed object: run proc-starts/bpf/regen.sh");
        }
    }
}
