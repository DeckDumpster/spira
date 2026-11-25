//! The counters the kernel keeps per control group, parsed from the files of one cgroup directory.

use std::path::Path;

/// `usage_usec` from a `cpu.stat`.
pub fn cpu_usage_usec(cpu_stat: &str) -> Option<u64> {
    cpu_stat.lines().find_map(|l| l.strip_prefix("usage_usec ")).and_then(|v| v.trim().parse().ok())
}

/// Each `io.stat` line as (`MAJ:MIN`, rbytes, wbytes).
pub fn io_stat(text: &str) -> Vec<(String, u64, u64)> {
    io_lines(text).into_iter().map(|d| (d.majmin, d.read_bytes, d.write_bytes)).collect()
}

/// A one-number cgroup file (`memory.current`, `pids.peak`).
pub fn scalar(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// One `io.stat` line: a disk's cumulative bytes and write requests for the cgroup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskIo {
    pub majmin: String,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub write_ios: u64,
}

fn io_lines(text: &str) -> Vec<DiskIo> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let mut d = DiskIo { majmin: f.next()?.to_string(), ..DiskIo::default() };
            for kv in f {
                match kv.split_once('=') {
                    Some(("rbytes", v)) => d.read_bytes = v.parse().unwrap_or(0),
                    Some(("wbytes", v)) => d.write_bytes = v.parse().unwrap_or(0),
                    Some(("wios", v)) => d.write_ios = v.parse().unwrap_or(0),
                    _ => {}
                }
            }
            Some(d)
        })
        .collect()
}

/// What one cgroup directory holds; a file that is absent reads as zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub cpu_usec: u64,
    pub io: Vec<DiskIo>,
    pub pids_current: u64,
    pub pids_peak: u64,
}

pub fn read(dir: &Path) -> Counters {
    let text = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap_or_default();
    Counters {
        cpu_usec: cpu_usage_usec(&text("cpu.stat")).unwrap_or(0),
        io: io_lines(&text("io.stat")),
        pids_current: scalar(&text("pids.current")).unwrap_or(0),
        pids_peak: scalar(&text("pids.peak")).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(tr0o6f1)]
    #[test]
    fn a_planted_cgroup_directory_reads_as_its_four_counter_groups() {
        let d = testkit::TempDir::new("otel-cgroup");
        std::fs::write(d.join("cpu.stat"), "usage_usec 2500000\nuser_usec 1\n").unwrap();
        std::fs::write(d.join("io.stat"), "8:32 rbytes=4096 wbytes=3000 rios=1 wios=9 dbytes=0 dios=0\n8:16 rbytes=0 wbytes=7 rios=0 wios=2\n").unwrap();
        std::fs::write(d.join("pids.current"), "4\n").unwrap();
        std::fs::write(d.join("pids.peak"), "11\n").unwrap();
        assert_eq!(
            read(&d),
            Counters {
                cpu_usec: 2_500_000,
                io: vec![
                    DiskIo { majmin: "8:32".into(), read_bytes: 4096, write_bytes: 3000, write_ios: 9 },
                    DiskIo { majmin: "8:16".into(), read_bytes: 0, write_bytes: 7, write_ios: 2 },
                ],
                pids_current: 4,
                pids_peak: 11,
            }
        );
        assert_eq!(read(&d.join("absent")), Counters::default(), "a cgroup that is gone reads as zero");
    }
}
