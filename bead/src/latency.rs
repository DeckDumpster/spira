//! Per-minute p50/p99 rollup of `$SPIRA_RUN/bdq/latency.log`, whose lines read
//! `<ISO-8601 UTC> rc=<n> tries=<n> ms=<n> verb=<v> caller=<program>`; `caller=` is absent from
//! older lines.

use std::collections::BTreeMap;

#[derive(Debug, PartialEq, Eq)]
pub struct Minute {
    pub minute: String,
    pub calls: usize,
    pub failed: usize,
    pub p50: u64,
    pub p99: u64,
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
}

const WRAPPERS: [&str; 9] = ["timeout", "env", "nice", "ionice", "nohup", "setsid", "stdbuf", "flock", "chrt"];
const MAX_ANCESTORS: usize = 8;

/// The first ancestor from `start` up that is not a wrapper, by its `comm`; `-` when none can
/// be read.
pub fn caller_name(start: i32, comm_of: impl Fn(i32) -> Option<String>, parent_of: impl Fn(i32) -> Option<i32>) -> String {
    let mut at = start;
    for _ in 0..MAX_ANCESTORS {
        let Some(comm) = comm_of(at).map(|c| c.trim().to_string()).filter(|c| !c.is_empty()) else { break };
        if !WRAPPERS.contains(&comm.as_str()) {
            return comm.split_whitespace().collect::<Vec<_>>().join("_");
        }
        match parent_of(at).filter(|p| *p > 1) {
            Some(p) => at = p,
            None => break,
        }
    }
    "-".to_string()
}

/// The program that ran this process, from `/proc/<pid>/comm` and `/proc/<pid>/stat` only.
pub fn parent_name() -> String {
    let comm = |pid: i32| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok();
    let parent = |pid: i32| {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
    };
    caller_name(std::os::unix::process::parent_id() as i32, comm, parent)
}

/// Nearest-rank percentile over an ascending, non-empty slice.
fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// Lines that do not parse are skipped, so a torn append never poisons the rollup.
pub fn rollup(log: &str) -> Vec<Minute> {
    let mut by_minute: BTreeMap<&str, (Vec<u64>, usize)> = BTreeMap::new();
    for line in log.lines() {
        let Some(ts) = line.split_whitespace().next().and_then(|t| t.get(..16)) else { continue };
        let Some(ms) = field(line, "ms").and_then(|v| v.parse::<u64>().ok()) else { continue };
        let failed = field(line, "rc").map(|v| v != "0").unwrap_or(false);
        let e = by_minute.entry(ts).or_default();
        e.0.push(ms);
        e.1 += usize::from(failed);
    }
    by_minute
        .into_iter()
        .map(|(minute, (mut ms, failed))| {
            ms.sort_unstable();
            Minute { minute: minute.to_string(), calls: ms.len(), failed, p50: percentile(&ms, 50), p99: percentile(&ms, 99) }
        })
        .collect()
}

pub fn render(rows: &[Minute]) -> String {
    rows.iter()
        .map(|r| format!("{} calls={} failed={} p50_ms={} p99_ms={}\n", r.minute, r.calls, r.failed, r.p50, r.p99))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ts: &str, rc: i32, ms: u64) -> String {
        format!("{ts}.123456789Z rc={rc} tries=1 ms={ms} verb=show\n")
    }

    #[test]
    fn caller_name_skips_wrappers_to_the_first_real_program() {
        let tree = |pid: i32| match pid {
            30 => Some(("timeout", 20)),
            20 => Some(("env", 10)),
            10 => Some(("my prog", 1)),
            _ => None,
        };
        let name = |start| caller_name(start, |p| tree(p).map(|t| t.0.to_string()), |p| tree(p).map(|t| t.1));
        assert_eq!(name(30), "my_prog");
        assert_eq!(name(99), "-", "an unreadable pid");
    }

    #[test]
    fn a_caller_field_does_not_change_the_rollup() {
        let plain = "2026-11-11T10:00:01.1Z rc=0 tries=1 ms=7 verb=show\n";
        let named = "2026-11-11T10:00:01.1Z rc=0 tries=1 ms=7 verb=show caller=perf-watch\n";
        assert_eq!(rollup(plain), rollup(named));
        assert_eq!(rollup(named).len(), 1);
    }

    #[test]
    fn groups_by_minute_with_nearest_rank_percentiles() {
        let mut log = String::new();
        for ms in 1..=100 {
            log += &line("2026-10-02T14:05", 0, ms);
        }
        log += &line("2026-10-02T14:06", 1, 900);
        let rows = rollup(&log);
        assert_eq!(
            rows,
            vec![
                Minute { minute: "2026-10-02T14:05".into(), calls: 100, failed: 0, p50: 50, p99: 99 },
                Minute { minute: "2026-10-02T14:06".into(), calls: 1, failed: 1, p50: 900, p99: 900 },
            ]
        );
    }

    #[test]
    fn torn_and_foreign_lines_are_skipped() {
        let log = format!("garbage\n2026-10-02T14:05:0\n{}", line("2026-10-02T14:05", 0, 7));
        assert_eq!(rollup(&log).len(), 1);
    }

    #[test]
    fn empty_log_renders_nothing() {
        assert_eq!(render(&rollup("")), "");
    }
}
