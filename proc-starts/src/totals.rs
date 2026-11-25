//! In-process cumulative totals folded from what the kernel maps held at each drain.

use std::collections::BTreeMap;
use std::time::SystemTime;

use spira_otel::metrics::Series;

use crate::classify::{classify, fold_program, UNRESOLVED};

pub const STARTS: &str = "spira_proc_starts_total";
pub const THREAD_STARTS: &str = "spira_proc_thread_starts_total";
pub const DROPPED: &str = "spira_proc_starts_dropped_total";
pub const BPF_NS: &str = "spira_proc_starts_bpf_ns_total";
pub const BPF_RUNS: &str = "spira_proc_starts_bpf_runs_total";
pub const SELF_CPU: &str = "spira_proc_starts_self_cpu_seconds";

pub const DROP_REASONS: [&str; 2] = ["pending_full", "counts_full"];
pub const PROGS: [&str; 4] = ["fork", "exec", "exit", "mkdir"];

/// One `starts` entry the kernel held: a count for a cgroup id, the program's comm and the starter's comm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartCount {
    pub cgroup: u64,
    pub program: String,
    pub starter: String,
    pub n: u64,
}

/// Everything one drain took out of the kernel. `dropped` and `cost` are the kernel's own running
/// totals (arrays that are never emptied), so they replace rather than add.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drain {
    pub starts: Vec<StartCount>,
    pub threads: Vec<(u64, u64)>,
    pub dropped: [u64; 2],
    pub cost: [(u64, u64); 4],
}

type StartKey = (String, String, String, String);

#[derive(Debug, Default)]
pub struct Totals {
    starts: BTreeMap<StartKey, u64>,
    threads: BTreeMap<String, u64>,
    dropped: [u64; 2],
    cost: [(u64, u64); 4],
}

fn class_of(cgroup: u64, resolve: &mut dyn FnMut(u64) -> Option<String>) -> (String, String) {
    match resolve(cgroup) {
        Some(path) => classify(&path),
        None => (UNRESOLVED.to_string(), UNRESOLVED.to_string()),
    }
}

impl Totals {
    pub fn fold(&mut self, d: Drain, resolve: &mut dyn FnMut(u64) -> Option<String>) {
        for s in d.starts {
            let (class, unit) = class_of(s.cgroup, resolve);
            *self.starts.entry((class, unit, fold_program(&s.program), fold_program(&s.starter))).or_default() += s.n;
        }
        for (cgroup, n) in d.threads {
            *self.threads.entry(class_of(cgroup, resolve).0).or_default() += n;
        }
        self.dropped = d.dropped;
        self.cost = d.cost;
    }

    pub fn series(&self, self_cpu_seconds: f64, at: SystemTime) -> Vec<Series> {
        let s = |name: &str, value: f64, labels: &[(&str, &str)]| Series {
            name: name.to_string(),
            value,
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            at,
        };
        let mut out = Vec::new();
        for ((class, unit, program, starter), &n) in &self.starts {
            out.push(s(STARTS, n as f64, &[("unit_class", class), ("unit", unit), ("program", program), ("starter", starter)]));
        }
        for (class, &n) in &self.threads {
            out.push(s(THREAD_STARTS, n as f64, &[("unit_class", class)]));
        }
        for (reason, &n) in DROP_REASONS.iter().zip(&self.dropped) {
            out.push(s(DROPPED, n as f64, &[("reason", reason)]));
        }
        for (prog, &(ns, runs)) in PROGS.iter().zip(&self.cost) {
            out.push(s(BPF_NS, ns as f64, &[("prog", prog)]));
            out.push(s(BPF_RUNS, runs as f64, &[("prog", prog)]));
        }
        out.push(s(SELF_CPU, self_cpu_seconds, &[]));
        out
    }
}

/// utime + stime of a `/proc/<pid>/stat` text, in seconds; the fields follow the `)` closing comm.
pub fn cpu_seconds(stat: &str, ticks_per_second: f64) -> Option<f64> {
    let after = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = after.split_whitespace().collect();
    let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    Some(ticks as f64 / ticks_per_second)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(cgroup: u64, program: &str, n: u64) -> StartCount {
        StartCount { cgroup, program: program.into(), starter: "bash".into(), n }
    }

    fn resolver(id: u64) -> Option<String> {
        (id == 7).then(|| "/system.slice/dolt-beads.service".to_string()) // not a path: a cgroup path
    }

    fn value(series: &[Series], name: &str, label: (&str, &str)) -> Option<f64> {
        series.iter().find(|s| s.name == name && s.labels.iter().any(|(k, v)| (k.as_str(), v.as_str()) == label)).map(|s| s.value)
    }

    #[test_id(t8863ub)]
    #[test]
    fn two_drains_of_the_same_cgroup_program_and_starter_sum() {
        let mut t = Totals::default();
        t.fold(Drain { starts: vec![start(7, "git", 3)], ..Drain::default() }, &mut resolver);
        t.fold(Drain { starts: vec![start(7, "git", 4)], ..Drain::default() }, &mut resolver);
        let series = t.series(0.0, SystemTime::UNIX_EPOCH);
        assert_eq!(value(&series, STARTS, ("program", "git")), Some(7.0));
        assert_eq!(series.iter().filter(|s| s.name == STARTS).count(), 1);
    }

    #[test_id(tdydhvm)]
    #[test]
    fn a_cgroup_nothing_resolves_is_counted_as_unresolved() {
        let mut t = Totals::default();
        t.fold(Drain { starts: vec![start(99, "git", 2)], threads: vec![(99, 5)], ..Drain::default() }, &mut resolver);
        let series = t.series(0.0, SystemTime::UNIX_EPOCH);
        assert_eq!(value(&series, STARTS, ("unit_class", "unresolved")), Some(2.0));
        assert_eq!(value(&series, THREAD_STARTS, ("unit_class", "unresolved")), Some(5.0));
    }

    #[test_id(t0qmmo3)]
    #[test]
    fn a_test_binary_hash_does_not_make_a_new_series() {
        let mut t = Totals::default();
        t.fold(Drain { starts: vec![start(7, "round_vm-304631", 1), start(7, "round_vm-ffee0011", 1)], ..Drain::default() }, &mut resolver);
        assert_eq!(value(&t.series(0.0, SystemTime::UNIX_EPOCH), STARTS, ("program", "round_vm-*")), Some(2.0));
    }

    #[test_id(tbwjsl4)]
    #[test]
    fn the_series_carry_the_six_names_and_only_their_labels() {
        let mut t = Totals::default();
        let d = Drain { starts: vec![start(7, "git", 1)], threads: vec![(7, 1)], dropped: [1, 2], cost: [(10, 2); 4] };
        t.fold(d, &mut resolver);
        let series = t.series(1.5, SystemTime::UNIX_EPOCH);
        let allowed: [(&str, &[&str]); 6] = [
            (STARTS, &["unit_class", "unit", "program", "starter"]),
            (THREAD_STARTS, &["unit_class"]),
            (DROPPED, &["reason"]),
            (BPF_NS, &["prog"]),
            (BPF_RUNS, &["prog"]),
            (SELF_CPU, &[]),
        ];
        for (name, labels) in allowed {
            let of: Vec<&Series> = series.iter().filter(|s| s.name == name).collect();
            assert!(!of.is_empty(), "{name} is not pushed");
            for s in of {
                let mut got: Vec<&str> = s.labels.iter().map(|(k, _)| k.as_str()).collect();
                let mut want = labels.to_vec();
                got.sort_unstable();
                want.sort_unstable();
                assert_eq!(got, want, "{name}");
            }
        }
        assert_eq!(series.len(), 1 + 1 + 2 + 8 + 1);
        assert_eq!(value(&series, DROPPED, ("reason", "counts_full")), Some(2.0));
        assert_eq!(series.iter().find(|s| s.name == SELF_CPU).map(|s| s.value), Some(1.5));
    }

    #[test_id(t727cq0)]
    #[test]
    fn self_cpu_is_utime_plus_stime_past_a_comm_holding_parentheses() {
        let stat = "123 (proc (starts)) S 1 1 1 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 3 0 5 1000 100";
        assert_eq!(cpu_seconds(stat, 100.0), Some(3.0));
        assert_eq!(cpu_seconds("garbage", 100.0), None);
    }
}
