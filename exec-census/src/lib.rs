//! Harness launches by systemd unit. Poll `ns_last_pid`, probe each PID allocated since the
//! last poll for comm, argv[0..2] and cgroup unit, and take `/proc/stat` `processes` as the
//! exact fork count: what the probes could not reach is reported as coverage, not dropped.
//! Read only — no ptrace, `environ` is never opened, argv is cut after two entries.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::time::SystemTime;

use spira_otel::metrics::Series;

pub const EXECS: &str = "spira_exec_census_execs_per_minute";
pub const FORKS: &str = "spira_exec_census_forks_per_minute";
pub const COVERAGE: &str = "spira_exec_census_coverage_ratio";

const ARGV_KEPT: usize = 2;
const READ_CAP: u64 = 4096;
const FIRST_PID: u32 = 2;
pub const NO_UNIT: &str = "none";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub comm: String,
    pub argv: Vec<String>,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    Process(Proc),
    Thread,
    Gone,
}

pub struct ProcFs {
    root: PathBuf,
}

fn read_capped(p: PathBuf) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    std::fs::File::open(p).ok()?.take(READ_CAP).read_to_end(&mut buf).ok()?;
    Some(buf)
}

impl ProcFs {
    pub fn new(root: impl Into<PathBuf>) -> ProcFs {
        ProcFs { root: root.into() }
    }

    fn text(&self, rel: &str) -> Option<String> {
        read_capped(self.root.join(rel)).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    fn number<T: std::str::FromStr>(&self, rel: &str) -> Option<T> {
        self.text(rel)?.trim().parse().ok()
    }

    pub fn ns_last_pid(&self) -> Option<u32> {
        self.number("sys/kernel/ns_last_pid")
    }

    pub fn pid_max(&self) -> Option<u32> {
        self.number("sys/kernel/pid_max")
    }

    pub fn forks(&self) -> Option<u64> {
        self.text("stat")?.lines().find_map(|l| l.strip_prefix("processes ")).and_then(|v| v.trim().parse().ok())
    }

    pub fn probe(&self, pid: u32) -> Probe {
        let Some(status) = self.text(&format!("{pid}/status")) else { return Probe::Gone };
        let tgid = status.lines().find_map(|l| l.strip_prefix("Tgid:")).and_then(|v| v.trim().parse::<u32>().ok());
        if tgid.is_some_and(|t| t != pid) {
            return Probe::Thread;
        }
        let Some(comm) = self.text(&format!("{pid}/comm")) else { return Probe::Gone };
        let argv = read_capped(self.root.join(format!("{pid}/cmdline")))
            .map(|b| b.split(|&c| c == 0).filter(|a| !a.is_empty()).take(ARGV_KEPT).map(|a| String::from_utf8_lossy(a).into_owned()).collect())
            .unwrap_or_default();
        let unit = self.text(&format!("{pid}/cgroup")).map_or_else(|| NO_UNIT.to_string(), |t| unit_of_cgroup(&t));
        Probe::Process(Proc { pid, comm: comm.trim().to_string(), argv, unit })
    }
}

/// The innermost service or scope in a cgroup v2 path. A scope's name carries an id that
/// differs per launch, so it is cut at the first segment holding a digit to bound the labels.
pub fn unit_of_cgroup(text: &str) -> String {
    let path = text.lines().find_map(|l| l.strip_prefix("0::")).or_else(|| text.lines().next().and_then(|l| l.splitn(3, ':').nth(2))).unwrap_or("");
    for seg in path.rsplit('/') {
        if seg.ends_with(".service") {
            return seg.to_string();
        }
        if let Some(stem) = seg.strip_suffix(".scope") {
            let kept: Vec<&str> = stem.split('-').take_while(|s| !s.chars().any(|c| c.is_ascii_digit())).collect();
            return format!("{}.scope", kept.join("-"));
        }
    }
    NO_UNIT.to_string()
}

#[derive(Debug, Default)]
pub struct Tick {
    pub procs: Vec<Proc>,
    pub threads: u64,
    pub gone: u64,
    pub forks: u64,
}

pub struct Sampler {
    last_pid: Option<u32>,
    last_forks: Option<u64>,
}

impl Sampler {
    pub fn new() -> Sampler {
        Sampler { last_pid: None, last_forks: None }
    }

    /// The PIDs handed out since the previous tick, including a wrap past `pid_max`. The
    /// first tick only records a baseline.
    pub fn tick(&mut self, fs: &ProcFs) -> Tick {
        let (Some(now), Some(forks)) = (fs.ns_last_pid(), fs.forks()) else { return Tick::default() };
        let (prev, prev_forks) = (self.last_pid.replace(now), self.last_forks.replace(forks));
        let (Some(prev), Some(prev_forks)) = (prev, prev_forks) else { return Tick::default() };
        let range: Vec<u32> = if now >= prev {
            (prev + 1..=now).collect()
        } else {
            (prev + 1..=fs.pid_max().unwrap_or(prev)).chain(FIRST_PID..=now).collect()
        };
        let mut t = Tick { forks: forks.saturating_sub(prev_forks), ..Tick::default() };
        for pid in range {
            match fs.probe(pid) {
                Probe::Process(p) => t.procs.push(p),
                Probe::Thread => t.threads += 1,
                Probe::Gone => t.gone += 1,
            }
        }
        t
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Sampler::new()
    }
}

/// Everything seen between two pushes.
#[derive(Debug, Default)]
pub struct Window {
    pub by_unit: BTreeMap<String, u64>,
    pub by_command: BTreeMap<String, u64>,
    pub threads: u64,
    pub gone: u64,
    pub forks: u64,
}

impl Window {
    pub fn add(&mut self, t: Tick) {
        for p in t.procs {
            *self.by_unit.entry(p.unit.clone()).or_default() += 1;
            *self.by_command.entry(command_of(&p)).or_default() += 1;
        }
        self.threads += t.threads;
        self.gone += t.gone;
        self.forks += t.forks;
    }

    pub fn execs(&self) -> u64 {
        self.by_unit.values().sum()
    }

    /// Launches attributed (processes and threads that were still there to probe) over the
    /// exact fork count; 1 when nothing forked.
    pub fn coverage(&self) -> f64 {
        if self.forks == 0 {
            return 1.0;
        }
        ((self.execs() + self.threads) as f64 / self.forks as f64).min(1.0)
    }

    pub fn series(&self, secs: f64, at: SystemTime) -> Vec<Series> {
        let per_min = |n: u64| n as f64 * 60.0 / secs.max(1e-3);
        let s = |name: &str, value: f64, labels: Vec<(String, String)>| Series { name: name.to_string(), value, labels, at };
        let mut out: Vec<Series> = self.by_unit.iter().map(|(u, &n)| s(EXECS, per_min(n), vec![("unit".into(), u.clone())])).collect();
        out.push(s(FORKS, per_min(self.forks), vec![]));
        out.push(s(COVERAGE, self.coverage(), vec![]));
        out
    }

    pub fn top_commands(&self, n: usize) -> Vec<String> {
        let mut v: Vec<(&String, &u64)> = self.by_command.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        v.into_iter().take(n).map(|(c, k)| format!("{c}={k}")).collect()
    }
}

fn command_of(p: &Proc) -> String {
    match p.argv.first() {
        Some(a) => a.rsplit('/').next().unwrap_or(a).to_string(),
        None => p.comm.clone(),
    }
}
